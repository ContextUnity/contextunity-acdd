use super::{
    spawner::{
        monitor_runner_process, prepare_runner_session, spawn_role_runner, RunnerSessionPolicy,
        RunnerStream,
    },
    types::RunnerInvocationContext,
    workflow::GateDecision,
};
use crate::{
    engine::gate_graph::GateCursor,
    tasks::profile::{GateProfile, RunnerConfig},
};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleDecision {
    gate: String,
    session_id: String,
    decision: String,
    proof: Value,
    #[serde(default)]
    findings: Option<Value>,
    #[serde(default)]
    integration_review: Option<crate::milestones::IntegrationReview>,
    #[serde(default)]
    binding: Option<crate::milestones::HandoffBinding>,
}

#[derive(Default)]
pub(super) struct RoleObservations {
    sessions: BTreeMap<String, String>,
    executed_gates: BTreeSet<String>,
    findings: Option<Value>,
    subject_context: Value,
    forbidden_sessions: Vec<String>,
    decisions: Vec<(String, GateDecision)>,
}

pub(super) fn sync_command_findings(
    stage: &str,
    decision: &mut GateDecision,
    observations: &mut RoleObservations,
) {
    decision.findings = if decision.passed {
        None
    } else {
        Some(json!({"code": "COMMAND_FAILED", "gate": stage, "diagnostics": decision.evidence}))
    };
    observations.restore(stage, decision);
}

impl RoleObservations {
    pub(super) fn note_executed(&mut self, stage: &str) {
        self.executed_gates.insert(stage.to_owned());
    }

    pub(super) fn restore(&mut self, stage: &str, decision: &GateDecision) {
        self.note_executed(stage);
        if decision.passed || decision.evidence["reuse_on_reject"] == true {
            if let Some(session) = decision.evidence["session_id"].as_str() {
                self.sessions.insert(stage.to_owned(), session.to_owned());
            }
        }
        self.findings = if decision.passed {
            None
        } else {
            decision.findings.clone()
        };
        self.decisions.retain(|(gate, _)| gate != stage);
        self.decisions.push((stage.to_owned(), decision.clone()));
    }
    pub(super) fn artifact_tokens(&self) -> BTreeMap<String, String> {
        let mut tokens = BTreeMap::new();
        for (_, decision) in self
            .decisions
            .iter()
            .filter(|(_, decision)| decision.passed)
        {
            super::workflow::collect_artifact_tokens(&decision.evidence, &mut tokens);
        }
        tokens
    }
    pub(super) fn set_subject_context(&mut self, context: Value, forbidden_sessions: Vec<String>) {
        self.subject_context = context;
        self.forbidden_sessions = forbidden_sessions;
    }
    /// Run a taskless role, accepting only its validated structured decision.
    pub(super) fn execute(
        &mut self,
        root: &Path,
        subject: &str,
        goal: &str,
        scope: &[String],
        cursor: &GateCursor,
        profile: &GateProfile,
    ) -> Result<GateDecision> {
        self.decisions
            .retain(|(gate, _)| gate != &cursor.canonical_stage_id);
        let role_id = cursor
            .gate
            .role
            .as_deref()
            .context("WORKFLOW_RUNNER_REQUIRED: gate has no role")?;
        let role = profile
            .roles
            .get(role_id)
            .context("WORKFLOW_RUNNER_REQUIRED: role is missing")?;
        let config = RunnerConfig::from_role(role)
            .context("WORKFLOW_RUNNER_REQUIRED: role has no configured runner")?;
        let session = if role.effective_reuse_on_reject() {
            self.sessions.get(&cursor.canonical_stage_id).cloned()
        } else {
            None
        }
        .map(Ok)
        .unwrap_or_else(|| prepare_runner_session("", RunnerSessionPolicy::Fresh))?;
        ensure!(
            !self.forbidden_sessions.contains(&session),
            "TASK_REVIEW_NOT_INDEPENDENT: role session matches an accepted milestone builder"
        );
        if let Some(prior_ref) = &cursor.gate.independent_from {
            let prior_session = self.sessions.get(prior_ref).or_else(|| {
                self.sessions.iter().find_map(|(stage, session)| {
                    stage
                        .rsplit('.')
                        .next()
                        .filter(|leaf| *leaf == prior_ref)
                        .map(|_| session)
                })
            });
            let prior_executed = self.executed_gates.contains(prior_ref)
                || self.executed_gates.iter().any(|stage| {
                    stage
                        .rsplit('.')
                        .next()
                        .is_some_and(|leaf| leaf == prior_ref)
                });
            if let Some(prior) = prior_session {
                ensure!(
                    &session != prior,
                    "TASK_REVIEW_NOT_INDEPENDENT: role gate reused its predecessor session"
                );
            } else if prior_executed {
                anyhow::bail!(
                    "WORKFLOW_INDEPENDENCE_UNKNOWN: prior gate has no observed role session"
                );
            }
        }
        let model = role.models.first();
        let context = json!({
            "subject": subject, "goal":goal,"gate":cursor.canonical_stage_id,
            "session_id":session,"allowed_write_scope":scope,"profile":profile,
            "gate_definition":cursor.gate,"findings":self.findings,
            "subject_context": self.subject_context,
            "expected_response":{"gate":cursor.canonical_stage_id,"session_id":session,"decision":"pass or reject","proof":crate::tasks::gates::proof_template(&cursor.gate, profile)?,"findings":"structured rejection findings, if rejected","integration_review":"For an independent milestone integration review, return {worker_id: session_id, decision: pass or reject, summary: substantive review coverage, scope_findings: [string], contract_findings: [string]}. Omit for other roles.","binding":"With integration_review, return the exact subject_context.binding inspected by this role; otherwise omit."},
            "instructions":"Execute the assigned role in the admitted scope. Return exactly one JSON response on stdout matching expected_response. Send progress to stderr. A process exit code does not submit a decision. Use the supplied independent session identity; do not invoke task claim or task submit for this taskless subject."
        });
        let mut invocation = RunnerInvocationContext {
            task_id: subject.into(),
            stage: cursor.canonical_stage_id.clone(),
            role: role_id.into(),
            model: model.map(|model| model.model.clone()).unwrap_or_default(),
            reasoning: model
                .and_then(|model| model.reasoning.clone())
                .unwrap_or_default(),
            session_id: session.clone(),
            prompt: String::new(),
            worktree: root.canonicalize()?,
        };
        let rendered = invocation.render_prompt_template(
            config.prompt_template.as_deref().unwrap_or(
                "Execute {role} for {task_id} at {stage}. Goal: {goal}. {role_guidance}",
            ),
            goal,
            role.guidance.as_deref().unwrap_or_default(),
        );
        invocation.prompt = format!("{rendered}\n\nGate context:\n{context}");
        let process = spawn_role_runner(&config, &invocation, RunnerSessionPolicy::Resume)?;
        let mut stdout = Vec::new();
        let mut overflow = false;
        let outcome = monitor_runner_process(
            process,
            Duration::from_secs(profile.limits.max_command_timeout_seconds),
            &mut |stream, bytes| {
                if stream == RunnerStream::Stdout {
                    let available = (1024_usize * 1024).saturating_sub(stdout.len());
                    overflow |= bytes.len() > available;
                    stdout.extend_from_slice(&bytes[..available.min(bytes.len())]);
                    Ok(())
                } else {
                    std::io::stderr().write_all(bytes)
                }
            },
        )?;
        ensure!(!overflow, "WORKFLOW_DECISION_TOO_LARGE");
        ensure!(
            outcome.exit_code == Some(0) && !outcome.timed_out,
            "WORKFLOW_RUNNER_FAILED: role did not complete successfully"
        );
        let payload = decision_json(&stdout)?;
        ensure!(
            payload.starts_with('{'),
            "WORKFLOW_DECISION_REQUIRED: decision must be a JSON object"
        );
        let decision: RoleDecision = serde_json::from_str(payload).context(
            "WORKFLOW_DECISION_REQUIRED: runner must return one structured gate decision",
        )?;
        ensure!(
            decision.session_id == session && decision.gate == cursor.canonical_stage_id,
            "WORKFLOW_DECISION_IDENTITY_MISMATCH"
        );
        ensure!(
            matches!(decision.decision.as_str(), "pass" | "reject"),
            "WORKFLOW_DECISION_INVALID"
        );
        if let Some(proof_decision) = decision
            .proof
            .get("scheme_proof")
            .and_then(|proof| proof.get("decision"))
        {
            ensure!(
                proof_decision.as_str() == Some(decision.decision.as_str()),
                "WORKFLOW_DECISION_MISMATCH: scheme proof decision disagrees with outer decision"
            );
        }
        let passed = decision.decision == "pass";
        let findings = rejection_findings(&decision, cursor, profile)?;
        if !passed {
            ensure!(
                has_structured_findings(findings.as_ref()),
                "WORKFLOW_FINDINGS_REQUIRED: rejection requires structured findings"
            );
        }
        if let Some(review) = &decision.integration_review {
            ensure!(review.worker_id == session && review.decision == decision.decision, "HANDOFF_REVIEW_IDENTITY_MISMATCH: integration reviewer identity must be the executing role session");
            ensure!(
                decision
                    .binding
                    .as_ref()
                    .is_some_and(|binding| serde_json::to_value(binding).ok().as_ref()
                        == self.subject_context.get("binding")),
                "HANDOFF_EVIDENCE_STALE: review must return the exact supplied content binding"
            );
        }
        // Command proofs must agree with observations made by ACDD itself.
        if let Some(command_id) = cursor.gate.command.as_deref() {
            let command = profile
                .commands
                .get(command_id)
                .context("WORKFLOW_COMMAND_MISSING")?;
            let plan =
                crate::milestones::resolve_command_plan(root, command_id, command, scope, profile)?;
            ensure!(!plan.is_empty(), "WORKFLOW_COMMAND_MISSING");
            let mut observations = Vec::new();
            for (id, command_str, timeout) in plan {
                let command_str = super::workflow::expand_workflow_placeholders(
                    &command_str,
                    &self.artifact_tokens(),
                )?;
                let observation = crate::milestones::command_gate::observe_command_for_id(
                    root,
                    &id,
                    &command_str,
                    timeout,
                    command.output(),
                    cursor.gate.expected_failure.as_ref(),
                )?;
                super::workflow::ensure_command_completed(
                    &observation,
                    &cursor.canonical_stage_id,
                )?;
                observations.push((id, observation));
            }
            let observed_pass =
                crate::engine::evidence_compiler::evaluate_root_command_gate_decision(
                    &cursor.gate,
                    profile,
                    &observations,
                    cursor.gate.policy.as_deref().unwrap_or("seam-test-first"),
                    Some(&decision.proof),
                    passed,
                )?;
            ensure!(
                !passed || observed_pass,
                "WORKFLOW_PROOF_UNOBSERVED: decision disagrees with executed commands"
            );
        } else {
            ensure!(!matches!(cursor.gate.proof.kind(), Some(crate::tasks::profile::ProofKind::Command|crate::tasks::profile::ProofKind::Contract|crate::tasks::profile::ProofKind::SeamTest)), "WORKFLOW_PROOF_UNOBSERVED: executable proof requires a registered observed command");
        }
        crate::tasks::gates::validate_gate_proof(
            &decision.proof,
            &cursor.gate,
            profile,
            passed,
            cursor.gate.policy.as_deref().unwrap_or("seam-test-first"),
        )?;
        if passed || role.effective_reuse_on_reject() {
            self.sessions
                .insert(cursor.canonical_stage_id.clone(), session.clone());
        }
        self.findings = if passed { None } else { findings.clone() };
        let result = GateDecision {
            passed,
            evidence: json!({"session_id":session,"reuse_on_reject":role.effective_reuse_on_reject(),"proof":decision.proof,"integration_review":decision.integration_review,"binding":decision.binding}),
            findings,
        };
        self.decisions
            .push((cursor.canonical_stage_id.clone(), result.clone()));
        Ok(result)
    }
}

fn decision_json(stdout: &[u8]) -> Result<&str> {
    let text = std::str::from_utf8(stdout)
        .context("WORKFLOW_DECISION_REQUIRED: stdout is not UTF-8")?
        .trim();
    if !text.starts_with("```") {
        return Ok(text);
    }
    let (opening, body) = text
        .split_once('\n')
        .context("WORKFLOW_DECISION_REQUIRED: incomplete fence")?;
    ensure!(
        matches!(opening.trim_end_matches('\r'), "```" | "```json"),
        "WORKFLOW_DECISION_REQUIRED: invalid fence"
    );
    let (body, closing) = body
        .rsplit_once('\n')
        .context("WORKFLOW_DECISION_REQUIRED: incomplete fence")?;
    ensure!(
        closing.trim_end_matches('\r') == "```",
        "WORKFLOW_DECISION_REQUIRED: invalid closing fence"
    );
    Ok(body.trim())
}

fn rejection_findings(
    decision: &RoleDecision,
    cursor: &GateCursor,
    profile: &GateProfile,
) -> Result<Option<Value>> {
    let mut embedded: Option<Value> = None;
    if let (Some(schema), Some(proof)) = (
        cursor.gate.proof.resolve_schema(&profile.proofs),
        decision
            .proof
            .get("scheme_proof")
            .and_then(Value::as_object),
    ) {
        for key in schema
            .properties
            .keys()
            .filter(|key| *key == "findings" || key.ends_with("_findings"))
        {
            if let Some(value) = proof.get(key) {
                let candidate = if key == "findings" {
                    value.clone()
                } else {
                    let mut object = serde_json::Map::new();
                    object.insert(key.clone(), value.clone());
                    Value::Object(object)
                };
                if let Some(existing) = &mut embedded {
                    match (existing, candidate) {
                        (Value::Object(existing), Value::Object(candidate)) => {
                            for (key, value) in candidate {
                                if let Some(prior) = existing.get(&key) {
                                    ensure!(
                                        prior == &value,
                                        "WORKFLOW_FINDINGS_CONFLICT: duplicated findings disagree"
                                    );
                                } else {
                                    existing.insert(key, value);
                                }
                            }
                        }
                        (existing, candidate) => ensure!(
                            *existing == candidate,
                            "WORKFLOW_FINDINGS_CONFLICT: duplicated findings disagree"
                        ),
                    }
                } else {
                    embedded = Some(candidate);
                }
            }
        }
    }
    if let (Some(top), Some(proof_findings)) = (&decision.findings, &embedded) {
        ensure!(
            top == proof_findings,
            "WORKFLOW_FINDINGS_CONFLICT: duplicated findings disagree"
        );
    }
    Ok(decision.findings.clone().or(embedded))
}

fn has_structured_findings(findings: Option<&Value>) -> bool {
    fn nonempty(value: &Value) -> bool {
        match value {
            Value::Object(object) => object.values().any(nonempty),
            Value::Array(items) => items.iter().any(nonempty),
            Value::String(text) => !text.trim().is_empty(),
            Value::Null => false,
            _ => true,
        }
    }
    findings.is_some_and(|value| (value.is_object() || value.is_array()) && nonempty(value))
}

#[cfg(test)]
mod tests {
    use super::has_structured_findings;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn embedded_task_cursor_context_and_role_templates_agree() {
        use super::RoleObservations;
        use crate::engine::{
            context::context_bundle, gate_graph::collect_cursors, workspaces::Registry,
        };
        use crate::tasks::{gates::validate_gate_proof, Milestone};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs/milestones")).unwrap();
        std::fs::write(root.join("acdd.yaml"), "tasks_db: .acdd/tasks.sqlite\n").unwrap();
        std::fs::create_dir_all(root.join(".acdd")).unwrap();
        std::fs::write(root.join(".acdd/profile.yaml"), "inherit: default\ncommands:\n  test: 'exit 101'\n  rust-check-and-test: \"printf 'test result: ok. 1 passed; 0 failed\\n'\"\ngates:\n  - tasks:\n      gates:\n        - id: contract\n          command: test\n        - id: build\n          command: rust-check-and-test\n        - id: seam\n          gates:\n            - id: integrate\n              command: rust-check-and-test\n").unwrap();
        let manifest = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/acdd/fixtures/milestones/lifecycle_task.md"
        ))
        .replace("{{ID}}", "m-parity")
        .replace("{{STATUS}}", "status: active\n")
        .replace("{{CLOSURE}}", "")
        .replace("{{TASK}}", "case")
        .replace("{{TARGET}}", "Compare guidance")
        .replace("{{REVISION}}", "1")
        .replace("{{SUBTASK}}", "part")
        .replace(
            "scope: [src/]",
            "scope: [src/]\nconcurrency: {mode: parallel}",
        );
        let path = "docs/milestones/010-parity.md";
        std::fs::write(root.join(path), &manifest).unwrap();
        std::fs::write(root.join("capture.py"), r#"import json, pathlib, sys
context = json.loads(sys.argv[1].split('\n\nGate context:\n', 1)[1])
proof = context['expected_response']['proof']
pathlib.Path('proof.json').write_text(json.dumps(proof))
if 'seam_test_proof' in proof:
    proof['seam_test_proof']['seam_test_refs'] = ['tests/example.rs::test_case']
if 'contract_proof' in proof:
    proof['contract_proof']['seam_test_ref'] = 'tests/example.rs::test_case'
if 'command_proof' in proof:
    proof['command_proof']['command'] = 'rust-check-and-test'
print(json.dumps({'gate': context['gate'], 'session_id': context['session_id'], 'decision': 'pass', 'proof': proof}))
"#).unwrap();
        let registry = Registry::load(root).unwrap();
        let workspace = registry.select(None).unwrap();
        let milestone = Milestone::parse(&manifest, &workspace.repository).unwrap();
        let id = milestone.task_id(&milestone.tasks[0]);
        let mut store = registry.open(workspace).unwrap();
        store.sync(&milestone, path, root).unwrap();
        let mut profile = (*workspace.gates).clone();
        for role in profile.roles.values_mut() {
            role.runner = Some("python3 capture.py '{prompt}'".into());
        }
        let cursors = collect_cursors(&profile).unwrap();
        assert_eq!(cursors.len(), 7);
        assert!(cursors
            .iter()
            .any(|cursor| cursor.stage_id == "contract_review"
                && cursor.gate.proof.kind() == Some(crate::tasks::profile::ProofKind::Review)));
        for cursor in cursors {
            let mut task = store.inspect(&id).unwrap();
            task.stage = cursor.stage_id.clone();
            crate::store::save(&store.connection, &task).unwrap();
            let claimed = store
                .claim(
                    &id,
                    &cursor.stage_id,
                    "parity-worker",
                    &root.to_string_lossy(),
                    None,
                )
                .unwrap();
            let details = workspace
                .guidance_envelope_with_store(&store, store.inspect_details(&id).unwrap())
                .unwrap();
            let bundle = context_bundle(root, workspace, &id, details).unwrap();
            let expected = &bundle["workflow_guidance"]["expected_evidence_template"]["proof"];
            let result = RoleObservations::default().execute(
                root,
                &id,
                "Compare guidance",
                &[],
                &cursor,
                &profile,
            );
            if matches!(
                cursor.gate.proof.kind(),
                Some(
                    crate::tasks::profile::ProofKind::Contract
                        | crate::tasks::profile::ProofKind::SeamTest
                )
            ) {
                assert!(result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("WORKFLOW_PROOF_UNOBSERVED"));
            } else {
                result.unwrap_or_else(|error| panic!("{}: {error:#}", cursor.stage_id));
            }
            let captured: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("proof.json")).unwrap()).unwrap();
            assert_eq!(*expected, captured, "{}", cursor.stage_id);
            let mut materialized = captured;
            if materialized.get("seam_test_proof").is_some() {
                materialized["seam_test_proof"]["seam_test_refs"] =
                    json!(["tests/example.rs::test_case"]);
            }
            if materialized.get("contract_proof").is_some() {
                materialized["contract_proof"]["seam_test_ref"] =
                    json!("tests/example.rs::test_case");
            }
            if materialized.get("command_proof").is_some() {
                materialized["command_proof"]["command"] = json!("rust-check-and-test");
            }
            validate_gate_proof(
                &materialized,
                &cursor.gate,
                &profile,
                true,
                "seam-test-first",
            )
            .unwrap();
            let mut invalid = materialized.clone();
            if invalid.get("review_proof").is_some() {
                assert!(invalid["review_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("decision")
                    .is_some());
            } else if invalid.get("contract_proof").is_some() {
                assert!(invalid["contract_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("seam_test_ref")
                    .is_some());
            } else if invalid.get("command_proof").is_some() {
                assert!(invalid["command_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("command")
                    .is_some());
            } else if invalid.get("seam_test_proof").is_some() {
                assert!(invalid["seam_test_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("seam_test_refs")
                    .is_some());
            } else {
                invalid["none_proof"] = json!({"unexpected": true});
            }
            assert!(
                validate_gate_proof(&invalid, &cursor.gate, &profile, true, "seam-test-first")
                    .is_err(),
                "{}",
                cursor.stage_id
            );
            store
                .release_failed_claim(&id, claimed.claim_revision)
                .unwrap();
            let evidence = json!({"task_id": id, "stage": cursor.stage_id, "claim_revision": claimed.claim_revision, "contract_revision": claimed.contract_revision, "worker_id": "fixture-worker", "worktree": root, "proof": materialized});
            store.connection.execute("INSERT INTO task_gates (task_id, gate, revision, state, evidence) VALUES (?1, ?2, ?3, 'passed', ?4)", rusqlite::params![id, cursor.stage_id, claimed.claim_revision, evidence.to_string()]).unwrap();
            let submission =
                json!({"evidence": evidence, "action": "pass", "findings": null, "task": claimed});
            store
                .connection
                .execute(
                    "INSERT INTO task_submissions VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, claimed.claim_revision, submission.to_string()],
                )
                .unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn default_root_roles_and_seam_review_emit_validator_accepted_proofs() {
        use super::RoleObservations;
        use crate::engine::gate_graph::{collect_cursors, collect_root_cursors};
        use crate::tasks::gates::validate_gate_proof;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("capture.py"), r#"import json, pathlib, sys
context = json.loads(sys.argv[1].split('\n\nGate context:\n', 1)[1])
proof = context['expected_response']['proof']
pathlib.Path('proof.json').write_text(json.dumps(proof))
print(json.dumps({'gate': context['gate'], 'session_id': context['session_id'], 'decision': 'pass', 'proof': proof}))
"#).unwrap();
        let mut profile = (*crate::tasks::profile::compiled()).clone();
        for role in profile.roles.values_mut() {
            role.runner = Some("python3 capture.py '{prompt}'".into());
        }
        let mut covered = Vec::new();
        let seam_review = collect_cursors(&profile)
            .unwrap()
            .into_iter()
            .filter(|cursor| cursor.canonical_stage_id.ends_with("seam.review"));
        for cursor in collect_root_cursors(&profile)
            .unwrap()
            .into_iter()
            .chain(seam_review)
        {
            if cursor.gate.role.is_none() {
                continue;
            }
            let decision = RoleObservations::default()
                .execute(
                    dir.path(),
                    "fixture",
                    "Inspect gate",
                    &[],
                    &cursor,
                    &profile,
                )
                .unwrap_or_else(|error| panic!("{}: {error:#}", cursor.canonical_stage_id));
            let captured: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("proof.json")).unwrap())
                    .unwrap();
            assert_eq!(captured, decision.evidence["proof"]);
            validate_gate_proof(&captured, &cursor.gate, &profile, true, "seam-test-first")
                .unwrap();
            let mut corrupt = captured.clone();
            corrupt.as_object_mut().unwrap().clear();
            assert!(
                validate_gate_proof(&corrupt, &cursor.gate, &profile, true, "seam-test-first")
                    .is_err()
            );
            let mut corrupt_inner = captured.clone();
            let required = if corrupt_inner.get("review_proof").is_some() {
                Some(("review_proof", "decision"))
            } else if corrupt_inner.get("scheme_proof").is_some() {
                Some(("scheme_proof", "cited_invariants"))
            } else {
                None
            };
            if let Some((envelope, field)) = required {
                assert!(corrupt_inner[envelope]
                    .as_object_mut()
                    .unwrap()
                    .remove(field)
                    .is_some());
                assert!(
                    validate_gate_proof(
                        &corrupt_inner,
                        &cursor.gate,
                        &profile,
                        true,
                        "seam-test-first"
                    )
                    .is_err(),
                    "{} accepted missing {field}",
                    cursor.canonical_stage_id
                );
            }
            covered.push(cursor.canonical_stage_id);
        }
        assert_eq!(
            covered.len(),
            5,
            "unexpected default role gates: {covered:?}"
        );
    }

    #[test]
    fn default_non_role_root_gates_project_validator_accepted_proofs() {
        use crate::engine::gate_graph::collect_root_cursors;
        use crate::tasks::gates::{proof_template, validate_gate_proof};
        use crate::tasks::profile::{compiled, ProofKind};

        let mut profile = (*compiled()).clone();
        profile.commands.insert(
            "fixture".into(),
            serde_yaml::from_str("cargo test").unwrap(),
        );
        let gates = collect_root_cursors(&profile)
            .unwrap()
            .into_iter()
            .filter(|cursor| cursor.gate.role.is_none());
        let mut count = 0;
        for cursor in gates {
            count += 1;
            let gate = &cursor.gate;
            let mut proof = proof_template(gate, &profile).unwrap();
            if gate.proof.kind() == Some(ProofKind::Command) {
                let command = gate
                    .command
                    .as_ref()
                    .unwrap_or_else(|| profile.commands.keys().next().expect("registered command"));
                proof["command_proof"]["command"] = json!(command);
            }
            if gate.proof.kind() == Some(ProofKind::Review) {
                proof["review_proof"]["notes"] = json!("Reviewed the configured contours");
            }
            validate_gate_proof(&proof, gate, &profile, true, "seam-test-first").unwrap_or_else(
                |error| panic!("{}: {error:#}; {proof}", cursor.canonical_stage_id),
            );

            let mut invalid = proof.clone();
            if gate.proof.kind() == Some(ProofKind::Review) {
                invalid["review_proof"]["contours"] = json!("invalid");
            } else {
                invalid.as_object_mut().unwrap().clear();
            }
            assert!(
                validate_gate_proof(&invalid, gate, &profile, true, "seam-test-first").is_err(),
                "{} accepted {invalid}",
                cursor.canonical_stage_id
            );
            if gate.proof.kind() == Some(ProofKind::Command) {
                let mut invalid_inner = proof.clone();
                assert!(invalid_inner["command_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("command")
                    .is_some());
                assert!(validate_gate_proof(
                    &invalid_inner,
                    gate,
                    &profile,
                    true,
                    "seam-test-first"
                )
                .is_err());
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn structured_findings_accept_non_empty_objects_and_arrays_only() {
        assert!(has_structured_findings(Some(
            &json!({"scope_findings": ["fix scope"]})
        )));
        assert!(has_structured_findings(Some(
            &json!([{"id": "AG-01", "code": "AUTHORITY_GAP"}])
        )));

        assert!(!has_structured_findings(None));
        assert!(!has_structured_findings(Some(&json!(null))));
        assert!(!has_structured_findings(Some(&json!({}))));
        assert!(!has_structured_findings(Some(&json!([]))));
        assert!(!has_structured_findings(Some(&json!("plain string"))));
        assert!(!has_structured_findings(Some(&json!(42))));
        assert!(!has_structured_findings(Some(&json!(true))));
    }
}
