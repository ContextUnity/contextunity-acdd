use super::role::{sync_command_findings, RoleObservations};
use crate::{
    engine::gate_graph,
    milestones::command_gate::{observe_command_for_id, resolve_command_plan},
    tasks::profile::{
        resolve_effective_workflow_profile, GateDef, GateDriver, GateProfile, PersistenceAction,
        ProofDef, ProofKind,
    },
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Discover standalone workflow documents without executing gates or opening a store.
pub fn list_workflows(root: &Path) -> Result<Vec<PathBuf>> {
    let root = root.canonicalize()?;
    let directory = root.join("docs/workflows");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let directory = directory.canonicalize()?;
    ensure!(
        directory.starts_with(&root),
        "WORKFLOW_PATH_INVALID: workflow directory escapes root"
    );
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yaml" | "yml")
        ) {
            let canonical = path.canonicalize()?;
            ensure!(
                canonical.starts_with(&root),
                "WORKFLOW_PATH_INVALID: workflow escapes root"
            );
            if canonical.is_file() {
                paths.push(path.strip_prefix(&root)?.to_owned());
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Select one discovered workflow; ambiguous names and arbitrary paths fail closed.
pub fn resolve_workflow(root: &Path, selector: &str) -> Result<PathBuf> {
    let paths = list_workflows(root)?;
    let matches = paths
        .into_iter()
        .filter(|path| {
            path.to_str() == Some(selector)
                || path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|stem| {
                        stem == selector || stem.strip_suffix(".workflow") == Some(selector)
                    })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] if builtin_workflow(selector).is_some() => Ok(PathBuf::from(format!(
            "docs/workflows/{selector}.workflow.yaml"
        ))),
        [] => bail!("WORKFLOW_NOT_FOUND: {selector}"),
        _ => bail!("WORKFLOW_AMBIGUOUS: {selector}"),
    }
}

/// Read and validate the source through the canonical profile parser.
pub fn read_workflow(root: &Path, path: &Path) -> Result<GateProfile> {
    let source = if root.join(path).exists() {
        let canonical = root.join(path).canonicalize()?;
        ensure!(
            canonical.starts_with(root.canonicalize()?),
            "WORKFLOW_PATH_INVALID: workflow escapes root"
        );
        fs::read_to_string(canonical)?
    } else {
        let name = path
            .to_str()
            .and_then(|p| p.strip_prefix("docs/workflows/"))
            .and_then(|p| p.strip_suffix(".workflow.yaml"))
            .context("WORKFLOW_NOT_FOUND")?;
        let template = builtin_workflow(name).context("WORKFLOW_NOT_FOUND")?;
        let mut document: serde_yaml::Value = serde_yaml::from_str(template)?;
        document["inherit"] = serde_yaml::Value::String("workspace".into());
        if let Some(roles) = document
            .get_mut("roles")
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            for role in roles
                .values_mut()
                .filter_map(serde_yaml::Value::as_mapping_mut)
            {
                for field in ["runner", "prompt_template", "models"] {
                    role.remove(serde_yaml::Value::String(field.into()));
                }
            }
        }
        serde_yaml::to_string(&document)?
    };
    let profile = resolve_effective_workflow_profile(root, path, &source)?;
    ensure!(
        matches!(
            profile.persistence,
            Some(PersistenceAction::None | PersistenceAction::Logfile)
        ),
        "WORKFLOW_SAVE_INVALID: standalone workflows require save: none or log"
    );
    fn validate_taskless(gates: &[GateDef]) -> Result<()> {
        for gate in gates {
            ensure!(
                gate.tasks.is_none(),
                "WORKFLOW_TASKS_INVALID: standalone workflows cannot contain tasks"
            );
            ensure!(
                [gate.persistence.pre, gate.persistence.post]
                    .into_iter()
                    .all(|save| save.is_none_or(|save| matches!(
                        save,
                        PersistenceAction::None | PersistenceAction::Logfile
                    ))),
                "WORKFLOW_SAVE_INVALID: standalone workflow gates require save: none or log"
            );
            validate_taskless(&gate.gates)?;
        }
        Ok(())
    }
    validate_taskless(&profile.gates)?;
    for template in profile.gate_templates.values() {
        validate_taskless(&template.gates)?;
    }
    Ok(profile)
}

fn builtin_workflow(name: &str) -> Option<&'static str> {
    match name {
        "author-plan" => Some(include_str!(
            "../../../../../docs/workflows/author-plan.workflow.yaml"
        )),
        "author-milestone" => Some(include_str!(
            "../../../../../docs/workflows/author-milestone.workflow.yaml"
        )),
        "author-workflow" => Some(include_str!(
            "../../../../../docs/workflows/author-workflow.workflow.yaml"
        )),
        "code-quality-and-polish" => Some(include_str!(
            "../../../../../docs/workflows/code-quality-and-polish.workflow.yaml"
        )),
        "author-spec" => Some(
            r#"version: 2
inherit: workspace
save: none
roles:
  architect:
    guidance: "Specify the goal, non-goals, ambiguities, numbered AC-* acceptance criteria, C1 and C2 boundary impact, ADR requirements, and SAD updates."
  independent_reviewer:
    guidance: "Review the goal, non-goals, ambiguities, numbered AC-* acceptance criteria, C1 and C2 boundary impact, ADR requirements, and SAD updates."
gates:
  - id: specify
    role: architect
    max_attempts: 3
    proof:
      scheme:
        type: object
        required: [spec_path, summary, goal, non_goals, ambiguities, acceptance_criteria, c1_boundary_impact, c2_boundary_impact, adr_required, sad_updates]
        properties:
          spec_path: {type: string}
          summary: {type: string}
          goal: {type: string}
          non_goals: {type: array, items: {type: string}}
          ambiguities: {type: array, items: {type: string}}
          acceptance_criteria: {type: array, items: {type: object, required: [id, criterion], properties: {id: {type: string}, criterion: {type: string}}}}
          c1_boundary_impact: {type: array, items: {type: string}}
          c2_boundary_impact: {type: array, items: {type: string}}
          adr_required: {type: array, items: {type: string}}
          sad_updates: {type: array, items: {type: string}}
  - id: review
    role: independent_reviewer
    independent_from: specify
    fail: specify
    max_attempts: 3
    proof:
      scheme:
        type: object
        required: [decision, summary]
        properties:
          decision: {type: string, enum: [pass, reject]}
          summary: {type: string}
"#,
        ),
        _ => None,
    }
}

/// Execute root gates with ephemeral observations and the existing graph router.
pub fn run_standalone_workflow(root: &Path, path: &Path, goal: Option<&str>) -> Result<Value> {
    let goal_text = match goal {
        Some(raw) => {
            let trimmed = raw.trim();
            ensure!(
                !trimmed.is_empty(),
                "WORKFLOW_GOAL_INVALID: --goal cannot be blank"
            );
            trimmed.to_owned()
        }
        None => format!("Execute standalone workflow {}", path.display()),
    };
    let profile = read_workflow(root, path)?;
    let cursors = gate_graph::collect_root_cursors(&profile)?;
    ensure!(
        !cursors.is_empty(),
        "WORKFLOW_GATES_EMPTY: no executable root gates"
    );
    // Validate the entire execution capability before the first side effect.
    // Agent gates need accepted structured decisions and cannot be inferred from
    // a process exit code.
    for cursor in &cursors {
        match cursor.gate.driver() {
            Some(GateDriver::Agent) => {
                ensure!(cursor.gate.role.as_ref().and_then(|role| profile.roles.get(role)).and_then(crate::tasks::profile::RunnerConfig::from_role).is_some(), "WORKFLOW_RUNNER_REQUIRED: gate '{}' needs a configured runner", cursor.canonical_stage_id);
                ensure!(!cursor.gate.receipt, "WORKFLOW_PROOF_REQUIRED: taskless workflow cannot create a task receipt");
            }
            Some(GateDriver::Command) => ensure!(matches!(cursor.gate.proof.kind(), Some(ProofKind::None | ProofKind::Command | ProofKind::Contract | ProofKind::SeamTest)) && cursor.gate.independent_from.is_none(), "WORKFLOW_PROOF_REQUIRED: gate '{}' needs typed proof handling beyond command execution", cursor.canonical_stage_id),
            _ => anyhow::bail!("WORKFLOW_RUNNER_REQUIRED: gate '{}' has no executable driver", cursor.canonical_stage_id),
        }
        ensure!(
            [cursor.gate.persistence.pre, cursor.gate.persistence.post]
                .into_iter()
                .all(|save| save.is_none_or(|save| matches!(
                    save,
                    PersistenceAction::None | PersistenceAction::Logfile
                ))),
            "WORKFLOW_SAVE_INVALID: gate cannot persist standalone workflow state"
        );
    }
    let paths = standalone_workflow_scope(path, &profile);
    let mut roles = RoleObservations::default();
    let outcomes = run_gate_graph(root, &paths, &[], &cursors, |cursor, attempt| {
        roles.note_executed(&cursor.canonical_stage_id);
        if cursor.gate.driver() == Some(GateDriver::Agent) {
            let decision = roles.execute(
                root,
                &path.to_string_lossy(),
                &goal_text,
                &paths,
                cursor,
                &profile,
            )?;
            if profile.persistence == Some(PersistenceAction::Logfile)
                || [cursor.gate.persistence.pre, cursor.gate.persistence.post]
                    .contains(&Some(PersistenceAction::Logfile))
            {
                append_observation(root, path, &cursor.canonical_stage_id, attempt, &decision)?;
            }
            return Ok(decision);
        }
        let name = cursor
            .gate
            .command
            .as_deref()
            .context("WORKFLOW_COMMAND_MISSING")?;
        let definition = profile
            .commands
            .get(name)
            .context("WORKFLOW_COMMAND_MISSING")?;
        let plan = resolve_command_plan(root, name, definition, &paths, &profile)?;
        ensure!(
            !plan.is_empty(),
            "WORKFLOW_COMMAND_MISSING: no command matched gate '{name}'"
        );
        let mut observations = Vec::new();
        for (command_id, command, timeout) in plan {
            let command = expand_workflow_placeholders(&command, &roles.artifact_tokens())?;
            let observation = observe_command_for_id(
                root,
                &command_id,
                &command,
                timeout,
                definition.output(),
                cursor.gate.expected_failure.as_ref(),
            )?;
            ensure_command_completed(&observation, &cursor.canonical_stage_id)?;
            observations.push((command_id, observation));
        }
        let passed = crate::engine::evidence_compiler::evaluate_root_command_gate(
            &cursor.gate,
            &profile,
            &observations,
            cursor.gate.policy.as_deref().unwrap_or("seam-test-first"),
            None,
        )?;
        let mut decision = GateDecision {
            passed,
            evidence: json!(observations.iter().map(|(command_id, observation)| json!({"command_id": command_id, "observation": observation})).collect::<Vec<_>>()),
            findings: None,
        };
        sync_command_findings(&cursor.canonical_stage_id, &mut decision, &mut roles);
        if profile.persistence == Some(PersistenceAction::Logfile)
            || [cursor.gate.persistence.pre, cursor.gate.persistence.post]
                .contains(&Some(PersistenceAction::Logfile))
        {
            append_observation(root, path, &cursor.canonical_stage_id, attempt, &decision)?;
        }
        Ok(decision)
    });
    if profile.persistence == Some(PersistenceAction::Logfile) {
        append_log(
            root,
            &json!({"workflow": path, "status": if outcomes.is_ok() { "completed" } else { "failed" }}),
        )?;
    }
    let result = json!({"status":"completed", "path":path, "outcomes":outcomes?});
    Ok(result)
}

fn append_observation(
    root: &Path,
    path: &Path,
    gate: &str,
    attempt: u32,
    decision: &GateDecision,
) -> Result<()> {
    append_log(
        root,
        &json!({"workflow": path, "gate": gate, "attempt": attempt, "passed": decision.passed, "evidence": decision.evidence, "findings": decision.findings}),
    )
}

fn append_log(root: &Path, record: &Value) -> Result<()> {
    let directory = crate::tasks::confined_path(root, ".acdd")?;
    fs::create_dir_all(&directory)?;
    let log = crate::tasks::confined_path(root, ".acdd/workflow-observations.jsonl")?;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(log)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let mut record = record.clone();
    record["timestamp_ms"] = json!(timestamp);
    let mut bytes = serde_json::to_vec(&record)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    Ok(())
}

#[derive(Clone)]
pub(super) struct GateDecision {
    pub passed: bool,
    pub evidence: Value,
    pub findings: Option<Value>,
}

/// Treat timeouts and signal termination as execution errors, not gate failures.
pub(crate) fn ensure_command_completed(observation: &Value, gate: &str) -> Result<()> {
    ensure!(
        observation["timed_out"] != true && observation["status"] != "timeout",
        "WORKFLOW_COMMAND_TIMEOUT: {gate}"
    );
    ensure!(
        observation["exit_code"].as_i64().is_some(),
        "WORKFLOW_COMMAND_INTERRUPTED: {gate}"
    );
    Ok(())
}

fn standalone_workflow_scope(path: &Path, profile: &GateProfile) -> Vec<String> {
    let mut scopes = Vec::new();
    let mut push_unique = |entry: &str| {
        if !scopes.iter().any(|existing| existing == entry) {
            scopes.push(entry.to_owned());
        }
    };
    let has_field_or_token = |tokens: &[&str]| {
        profile.commands.values().any(|command| {
            tokens
                .iter()
                .any(|token| command.command().contains(&format!("{{{token}}}")))
        }) || profile.gates.iter().any(|gate| {
            if let ProofDef::Scheme { scheme } = &gate.proof {
                scheme
                    .properties
                    .keys()
                    .any(|key| tokens.contains(&key.as_str()))
            } else {
                false
            }
        })
    };
    if has_field_or_token(&["plan_name", "plan_slug", "plan_path"]) {
        push_unique("docs/plans/");
    }
    if has_field_or_token(&["manifest_path", "milestone_id"]) {
        push_unique("docs/milestones/");
    }
    if has_field_or_token(&["workflow_name", "workflow_path"]) {
        push_unique("docs/workflows/");
    }
    drop(push_unique);
    if scopes.is_empty() {
        scopes.push(path.to_string_lossy().into_owned());
    }
    scopes
}

/// Route subject-neutral observations through the canonical gate cursor graph.
pub(super) fn run_gate_graph(
    root: &Path,
    paths: &[String],
    tasks: &[crate::store::Task],
    cursors: &[gate_graph::GateCursor],
    mut execute: impl FnMut(&gate_graph::GateCursor, u32) -> Result<GateDecision>,
) -> Result<Vec<Value>> {
    let applicable = |cursor: &gate_graph::GateCursor| {
        gate_graph::gate_condition_matches(cursor.when.as_deref(), paths, tasks, None, Some(root))
    };
    let mut next = cursors.iter().position(applicable);
    let mut visits = BTreeMap::<usize, u32>::new();
    let mut failures = BTreeMap::<usize, u32>::new();
    let mut outcomes = Vec::new();
    while let Some(index) = next {
        let cursor = &cursors[index];
        let used = visits.entry(index).or_default();
        *used += 1;
        let decision = execute(cursor, *used)?;
        outcomes.push(json!({"gate":cursor.canonical_stage_id,"attempt":used,"passed":decision.passed,"evidence":decision.evidence}));
        next = if decision.passed {
            gate_graph::resolve_pass_target(cursors, index, applicable)?
        } else {
            let failed = failures.entry(index).or_default();
            *failed += 1;
            let route = gate_graph::resolve_fail_target(
                cursors,
                index,
                decision.findings.as_ref(),
                *failed,
                applicable,
            )?;
            ensure!(
                !route.exhausted,
                "WORKFLOW_ATTEMPTS_EXHAUSTED: {}",
                cursor.canonical_stage_id
            );
            let target = route
                .target
                .context("WORKFLOW_COMMAND_FAILED: no failure route")?;
            Some(
                cursors
                    .iter()
                    .position(|cursor| cursor.canonical_stage_id == target.canonical_stage_id)
                    .context("WORKFLOW_ROUTE_INVALID")?,
            )
        };
    }
    Ok(outcomes)
}

pub(super) fn collect_artifact_tokens(evidence: &Value, tokens: &mut BTreeMap<String, String>) {
    let source = evidence
        .pointer("/proof/scheme_proof")
        .or_else(|| evidence.get("proof"))
        .and_then(Value::as_object);
    let Some(map) = source else {
        return;
    };
    for (key, value) in map {
        if let Some(text) = value.as_str() {
            tokens.insert(key.clone(), text.to_owned());
        }
    }
    if let Some(slug) = map.get("plan_slug").and_then(Value::as_str) {
        if !map.contains_key("plan_name") {
            tokens.insert("plan_name".to_owned(), slug.to_owned());
        }
    }
    if let Some(name) = map.get("plan_name").and_then(Value::as_str) {
        if !map.contains_key("plan_slug") {
            tokens.insert("plan_slug".to_owned(), name.to_owned());
        }
    }
}

pub(crate) fn expand_workflow_placeholders(
    command: &str,
    tokens: &BTreeMap<String, String>,
) -> Result<String> {
    let mut output = String::with_capacity(command.len());
    let mut rest = command;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            output.push('{');
            rest = after_start;
            continue;
        };
        let key = &after_start[..end];
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            output.push_str(&rest[start..start + 1 + end + 1]);
            rest = &after_start[end + 1..];
            continue;
        }
        let value = tokens.get(key).with_context(|| {
            format!(
                "WORKFLOW_COMMAND_UNRESOLVED: placeholder '{{{key}}}' requires accepted prior gate field '{key}'"
            )
        })?;
        ensure!(
            !value.is_empty()
                && !value.starts_with('-')
                && !value.starts_with('/')
                && !value.contains("..")
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')),
            "WORKFLOW_COMMAND_INVALID: artifact placeholder '{key}' contains unsafe characters"
        );
        output.push_str(value);
        rest = &after_start[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_placeholders_substitute_authoring_artifacts_and_reject_unsafe_values() {
        let mut tokens = BTreeMap::new();
        collect_artifact_tokens(
            &json!({
                "proof": {
                    "scheme_proof": {
                        "manifest_path": "docs/milestones/001-demo.md",
                        "plan_slug": "step-plan",
                        "workflow_name": "author-milestone"
                    }
                }
            }),
            &mut tokens,
        );

        assert_eq!(
            expand_workflow_placeholders("acdd milestone init {manifest_path} --in-place", &tokens)
                .unwrap(),
            "acdd milestone init docs/milestones/001-demo.md --in-place"
        );
        assert_eq!(
            expand_workflow_placeholders("acdd plan show {plan_name}", &tokens).unwrap(),
            "acdd plan show step-plan"
        );
        assert_eq!(
            expand_workflow_placeholders("acdd workflow show {workflow_name}", &tokens).unwrap(),
            "acdd workflow show author-milestone"
        );

        // Re-synthesizing plan_slug refreshes derived plan_name alias cleanly.
        collect_artifact_tokens(
            &json!({
                "proof": {
                    "scheme_proof": {
                        "plan_slug": "updated-plan"
                    }
                }
            }),
            &mut tokens,
        );
        assert_eq!(
            expand_workflow_placeholders("acdd plan show {plan_name}", &tokens).unwrap(),
            "acdd plan show updated-plan"
        );

        let missing = expand_workflow_placeholders("acdd plan show {missing_field}", &tokens)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("WORKFLOW_COMMAND_UNRESOLVED"), "{missing}");

        for bad_value in [
            "docs/milestones/bad.md;rm -rf /",
            "--help",
            "-h",
            "/etc/passwd",
            "../secret",
        ] {
            tokens.insert("plan_name".into(), bad_value.into());
            let unsafe_err = expand_workflow_placeholders("acdd plan show {plan_name}", &tokens)
                .unwrap_err()
                .to_string();
            assert!(
                unsafe_err.contains("WORKFLOW_COMMAND_INVALID"),
                "expected rejection for {bad_value}, got: {unsafe_err}"
            );
        }

        // Actual workflow resolver matches stem without `.workflow` cleanly,
        // including when root traverses a symlink (e.g. /var -> /private/var).
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("docs/workflows")).unwrap();
        std::fs::write(
            real.join("docs/workflows/author-milestone.workflow.yaml"),
            "version: 2\n",
        )
        .unwrap();
        assert_eq!(
            resolve_workflow(&real, "author-milestone").unwrap(),
            PathBuf::from("docs/workflows/author-milestone.workflow.yaml")
        );
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&real, &alias).unwrap();
            assert_eq!(
                resolve_workflow(&alias, "author-milestone").unwrap(),
                PathBuf::from("docs/workflows/author-milestone.workflow.yaml")
            );
        }

        let plan_wf_path = Path::new("docs/workflows/author-plan.workflow.yaml");
        let plan_wf_yaml = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(plan_wf_path),
        )
        .unwrap();
        let plan_profile = crate::tasks::profile::validate_profile_document(&plan_wf_yaml).unwrap();
        assert_eq!(
            standalone_workflow_scope(plan_wf_path, &plan_profile),
            vec!["docs/plans/".to_owned()]
        );
    }
}
