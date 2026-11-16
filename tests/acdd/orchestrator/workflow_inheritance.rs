use crate::acdd::{harness::workflows::WorkflowHarness, support::*};
use acdd_tasks::{
    engine::orchestrator::workflow::{read_workflow, resolve_workflow, run_standalone_workflow},
    tasks::{
        configuration::InvocationSettings,
        profile::{self, PersistenceAction},
    },
};
use std::{fs, path::Path, sync::Arc};

#[test]
fn cli_binary_resolves_builtin_and_workspace_inherited_profiles_with_confined_path_errors() {
    let root = WorkflowHarness::ordered().materialize();
    let mut workspace = fixture_profile();
    workspace.limits.max_command_timeout_seconds = 19;
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    let mut selected: Value = serde_yaml::from_str(&standalone_profile_yaml(&workspace)).unwrap();
    selected["inherit"] = json!("workspace");
    selected["gates"] = json!([{"tasks":{"gates":[{"id":"inspect","role":"builder","proof":"none"},{"id":"deliver","receipt":true}]}}]);
    for key in [
        "roles", "commands", "policies", "guidance", "limits", "proofs",
    ] {
        selected.as_object_mut().unwrap().remove(key);
    }
    root.write(
        "configs/inherited.yaml",
        &serde_yaml::to_string(&selected).unwrap(),
    );
    let mut workflow: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    workflow["inherit"] = json!("workspace");
    root.write(
        "docs/workflows/inherited.yaml",
        &serde_yaml::to_string(&workflow).unwrap(),
    );
    // Each row selects a distinct CLI/configuration layout, on one shared root.
    let cases = [
        (Some("plan"), None, None),
        (Some("configs/inherited.yaml"), None, None),
        (None, Some("plan"), None),
        (None, Some("configs/inherited.yaml"), None),
        (Some("configs/missing.yaml"), None, Some("CLI_PATH_MISSING")),
        (None, Some("configs/missing.yaml"), Some("CLI_PATH_MISSING")),
        (Some("configs"), None, Some("CLI_PATH_INVALID")),
        (Some("/etc/passwd"), None, Some("CLI_PATH_INVALID")),
    ];
    let mut failures = Vec::new();
    for (explicit, configured, diagnostic) in cases {
        root.write(
            "acdd.yaml",
            &serde_yaml::to_string(&json!({"acdd_profile":configured})).unwrap(),
        );
        if configured.is_none() {
            root.write("acdd.yaml", "{}\n");
        }
        // The in-process TaskHarness CLI adapter bypasses the binary boundary.
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_acdd"));
        command.args(["--root", root.root().to_str().unwrap(), "--format", "json"]);
        if let Some(profile) = explicit {
            command.args(["--profile", profile]);
        }
        let output = command
            .args(["workflow", "show", "inherited"])
            .output()
            .unwrap();
        let label = format!("--profile={explicit:?}, acdd_profile={configured:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(code) = diagnostic {
            assert!(!output.status.success(), "{label}: invalid path accepted");
            let codes: Vec<_> = stderr
                .split(|c: char| !c.is_ascii_uppercase() && c != '_')
                .filter(|word| word.starts_with("CLI_PATH_"))
                .collect();
            assert!(
                codes.contains(&code),
                "{label}: expected exact {code}, got {stderr}"
            );
            continue;
        }
        if !output.status.success() {
            failures.push(format!("{label}: {stderr}"));
            continue;
        }
        let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
        let resolved: profile::GateProfile =
            serde_json::from_value(shown["profile"].clone()).unwrap();
        assert_eq!(
            resolved.roles, workspace.roles,
            "{label}: inherit workspace roles"
        );
        assert_eq!(
            resolved.limits, workspace.limits,
            "{label}: inherit workspace limits"
        );
        assert_eq!(
            resolved.gates,
            WorkflowHarness::ordered().profile.gates,
            "{label}: retain workflow gates"
        );
        assert!(resolved.gates.iter().all(|gate| gate.tasks.is_none()));
        // Reuse the selected invocation to traverse the task CLI caller too.
        let mut tasks = std::process::Command::new(env!("CARGO_BIN_EXE_acdd"));
        tasks.args(["--root", root.root().to_str().unwrap(), "--format", "json"]);
        if let Some(profile) = explicit {
            tasks.args(["--profile", profile]);
        }
        let output = tasks.args(["task", "list"]).output().unwrap();
        assert!(
            output.status.success(),
            "{label}: task list: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(listed.is_object(), "{label}: structured task response");
    }
    assert!(!root.path("docs/workflows/plan.workflow.yaml").exists());
    assert!(
        failures.is_empty(),
        "CLI profile resolution contract: {failures:#?}"
    );
}

#[test]
fn builtin_author_spec_executes_structured_traceability_proof_and_emits_complete_guidance() {
    let root = WorkflowHarness::agents().materialize();
    let mut workspace = fixture_profile();
    let runner = WorkflowHarness::agents().profile.roles["builder"].clone();
    for role in workspace.roles.values_mut() {
        role.runner = runner.runner.clone();
        role.prompt_template = Some("{role_guidance}".into());
    }
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    let runner_source = include_str!("../fixtures/workflows/runner.py")
        .replacen(
            "{\"gate\": gate,",
            "{\"context\": context, \"prompt\": sys.argv[1], \"gate\": gate,",
            1,
        )
        .replace(
            "    payload.update(json.loads",
            "    if gate == \"specify\":\n        payload.clear()\n    payload.update(json.loads",
        );
    root.write("workflow-runner.py", &runner_source);
    root.write("review-rejected", "");
    let structured = json!({
        "spec_path":"docs/specs/profile.md", "summary":"Specify inherited execution",
        "goal":"Resolve shared workspace resources",
        "non_goals":["No task rows for workflows"],
        "ambiguities":["None unresolved"],
        "acceptance_criteria":[{"id":"AC-1","criterion":"Inherited runner executes without importing tasks"}],
        "c1_boundary_impact":["CLI user selects a workspace profile"],
        "c2_boundary_impact":["Workflow engine consumes shared profile resources"],
        "adr_required":["ADR 0021 resource/topology boundary applies"],
        "sad_updates":["Document profile resolution at the workflow boundary"]
    });
    root.write(
        "root-proof.json",
        &serde_json::to_string(&structured).unwrap(),
    );
    let path = resolve_workflow(root.root(), "author-spec").unwrap();
    assert!(!root.path(&path).exists(), "execute the embedded workflow");
    let result =
        run_standalone_workflow(root.root(), &path, Some("Specify inherited execution")).unwrap();
    assert_eq!(result["status"], "completed");
    let accepted = &result["outcomes"][0]["evidence"]["proof"]["scheme_proof"];
    let sessions: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let context = &sessions[0]["context"];
    let proof = &context["expected_response"]["proof"]["scheme_proof"];
    let schema = &context["gate_definition"]["proof"]["scheme"];
    let (guidance, _) = sessions[0]["prompt"]
        .as_str()
        .unwrap()
        .split_once("\n\nGate context:\n")
        .expect("production role prompt must separate guidance from Gate context JSON");
    let guidance = guidance.to_lowercase();
    let cases = [
        ("goal", "goal", "string"),
        ("non_goals", "non-goals", "array"),
        ("ambiguities", "ambiguit", "array"),
        ("acceptance_criteria", "ac-", "array"),
        ("c1_boundary_impact", "c1", "array"),
        ("c2_boundary_impact", "c2", "array"),
        ("adr_required", "adr", "array"),
        ("sad_updates", "sad", "array"),
    ];
    let mut failures = Vec::new();
    for (field, keyword, expected_type) in cases {
        assert_eq!(accepted[field], structured[field], "retain valid {field}");
        if proof.get(field).is_none() || schema["properties"].get(field).is_none() {
            failures.push(format!("structured spec schema/template omits {field}"));
        }
        if !guidance.contains(keyword) {
            failures.push(format!("specify role guidance omits {keyword}"));
        }
        // Mutate one field of the accepted proof at a time. The specify runner
        // clears its template first, so omissions cannot be defaulted.
        for omit in [true, false] {
            let mut invalid = structured.clone();
            let expected = if omit {
                invalid.as_object_mut().unwrap().remove(field);
                format!("TASK_EVIDENCE_INVALID: $.scheme_proof.{field} is required")
            } else {
                invalid[field] = Value::Null;
                format!("TASK_EVIDENCE_INVALID: $.scheme_proof.{field} must be {expected_type}")
            };
            root.write("root-proof.json", &serde_json::to_string(&invalid).unwrap());
            match run_standalone_workflow(root.root(), &path, Some("Specify inherited execution")) {
                Ok(_) => failures.push(format!("{field}, omit={omit}: runtime accepted invalid traceability; expected {expected}")),
                Err(error) => assert_eq!(error.root_cause().to_string(), expected,
                    "{field}, omit={omit}: reject at the offending schema property: {error:#}"),
            }
        }
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(
        failures.is_empty(),
        "INV-SPECIFY-DESIGN-TRACEABILITY: {failures:#?}"
    );
}

#[test]
fn workflow_gate_save_log_persists_observation_under_root_save_none() {
    let root = WorkflowHarness::ordered().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    source["gates"][0]["save"] = json!("log");
    root.write(
        "docs/workflows/gate-log.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/gate-log.yaml"), None)
            .unwrap();
    assert_eq!(result["status"], "completed");
    let log = fs::read_to_string(root.path(".acdd/workflow-observations.jsonl"));
    assert!(
        log.is_ok(),
        "gate-level save: log must persist its observation under root save: none: {log:?}"
    );
    let records: Vec<Value> = log
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let observed = records
        .iter()
        .find(|row| row["gate"] == "first")
        .expect("log the selected gate");
    assert_eq!(observed["passed"], true);
    assert_eq!(observed["attempt"], 1);
    assert_eq!(observed["evidence"], result["outcomes"][0]["evidence"]);
    assert_eq!(observed["evidence"][0]["observation"]["exit_code"], 0);
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn workflow_save_log_persists_terminal_failed_status_before_returning_exhaustion() {
    let root = WorkflowHarness::ordered().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    source["save"] = json!("log");
    source["commands"]["first"]["command"] = json!("exit 37");
    source["gates"] = json!([{"id":"first","command":"first","max_attempts":1,"fail":"first"}]);
    root.write(
        "docs/workflows/exhausted.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let error = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/exhausted.yaml"),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.root_cause().to_string(),
        "WORKFLOW_ATTEMPTS_EXHAUSTED: first",
        "{error:#}"
    );
    let records: Vec<Value> = fs::read_to_string(root.path(".acdd/workflow-observations.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0]["gate"], "first");
    assert_eq!(records[0]["passed"], false);
    assert_eq!(records[0]["evidence"][0]["observation"]["exit_code"], 37);
    assert_eq!(
        records.last().unwrap()["status"],
        "failed",
        "terminal failed record must be durable before returning: {records:?}"
    );
    assert_eq!(
        records.last().unwrap()["workflow"],
        "docs/workflows/exhausted.yaml"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn custom_catalog_proof_projects_optional_properties_without_requiring_them_at_runtime() {
    let root = WorkflowHarness::agents().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    source["proofs"] = json!({"design_record":{"schema":{
        "type":"object", "required":["summary"], "additionalProperties":false,
        "properties":{
            "summary":{"type":"string"}, "boundary_notes":{"type":"array","items":{"type":"string"}},
            "requires_adr":{"type":"boolean"}, "design":{"type":"object","required":["goal"],
                "properties":{"goal":{"type":"string"},"sad_updates":{"type":"array","items":{"type":"string"}}}}
        }
    }}});
    source["gates"] = json!([{"id":"design","role":"builder","proof":"design_record"}]);
    root.write(
        "docs/workflows/catalog.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let resolved = read_workflow(root.root(), Path::new("docs/workflows/catalog.yaml")).unwrap();
    let projected =
        acdd_tasks::tasks::gates::proof_template(&resolved.gates[0], &resolved).unwrap();
    // Submit exactly the chosen proof, independently of optional template fields.
    let runner_source = include_str!("../fixtures/workflows/runner.py").replace(
        "payload.update(json.loads",
        "payload.clear(); payload.update(json.loads",
    );
    root.write("workflow-runner.py", &runner_source);
    for supplied in [
        json!({"summary":"Required field only"}),
        json!({"summary":"Complete design","boundary_notes":["No new task engine"],"requires_adr":false,"design":{"goal":"Share resources","sad_updates":["Update architecture"]}}),
    ] {
        root.write(
            "root-proof.json",
            &serde_json::to_string(&supplied).unwrap(),
        );
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/catalog.yaml"), None)
                .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(
            result["outcomes"][0]["evidence"]["proof"]["scheme_proof"],
            supplied
        );
    }
    root.write(
        "root-proof.json",
        r#"{"summary":"Malformed","requires_adr":"not a boolean"}"#,
    );
    let error =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/catalog.yaml"), None)
            .unwrap_err();
    assert_eq!(
        error.root_cause().to_string(),
        "TASK_EVIDENCE_INVALID: $.scheme_proof.requires_adr must be boolean",
        "reject the malformed optional property at its exact schema path: {error:#}"
    );
    let template = &projected["scheme_proof"];
    assert!(template["summary"].is_string());
    for field in ["boundary_notes", "requires_adr", "design"] {
        assert!(template.get(field).is_none(), "optional {field}: {projected}");
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn explicit_inherit_targets_merge_resources_with_local_and_selected_profile_precedence() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    let mut workspace = fixture_profile();
    workspace.commands.extend(harness.profile.commands.clone());
    workspace
        .roles
        .extend(WorkflowHarness::agents().profile.roles);
    workspace.roles.get_mut("builder").unwrap().models =
        serde_json::from_value(json!([{"model":"workspace-model","reasoning":"medium"}])).unwrap();
    workspace.limits.max_command_timeout_seconds = 17;
    workspace
        .scope_args_by_pattern
        .insert("**/*.rs".into(), "--workspace".into());
    workspace.guidance.insert(
        "inheritance_note".into(),
        serde_json::from_value(json!("workspace guidance")).unwrap(),
    );
    workspace.proofs.insert(
        "resource_proof".into(),
        serde_json::from_value(json!({"schema":{"type":"string"}})).unwrap(),
    );
    let mut selected = workspace.clone();
    selected.roles.get_mut("builder").unwrap().runner = Some("selected-runner '{prompt}'".into());
    selected.limits.max_command_timeout_seconds = 23;
    let workspace_yaml = standalone_profile_yaml(&workspace);
    root.write("configs/resources.yaml", &workspace_yaml);
    root.write("configs/selected.yaml", &standalone_profile_yaml(&selected));
    let template: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    let cases = [
        ("none", false, false),
        ("default", false, false),
        ("workspace", false, false),
        ("configs/resources.yaml", false, false),
        ("workspace", true, false),
        ("configs/resources.yaml", true, false),
        ("workspace", false, true),
        ("workspace", true, true),
        ("configs/resources.yaml", false, true),
    ];
    let mut failures = Vec::new();
    for (target, local_override, profile_override) in cases {
        // `none` must not even parse a broken repository profile.
        root.write(
            ".acdd/profile.yaml",
            if target == "none" {
                "invalid: ["
            } else {
                &workspace_yaml
            },
        );
        let mut source = template.clone();
        source["inherit"] = json!(target);
        if target == "default" {
            // Default overlays retain the embedded milestone graph. Appending
            // workflow gates after its terminal receipt would be invalid.
            source.as_object_mut().unwrap().remove("gates");
        }
        if target != "none" && target != "default" {
            source.as_object_mut().unwrap().remove("commands");
        }
        if local_override {
            source["roles"] = json!({
                "builder":{"models":["local-model:high"],"guidance":"local role guidance"},
                "reviewer":{"runner":"local-reviewer '{prompt}'"}
            });
            source["commands"] =
                json!({"first":{"command":"printf local","output":{"require_counts":false}}});
            source["guidance"] = json!({"inheritance_note":"local guidance"});
            source["scope_args_by_pattern"] = json!({"**/*.rs":"--local"});
            source["proofs"] = json!({"resource_proof":{"schema":{"type":"integer"}}});
            source["limits"] = json!({"max_command_timeout_seconds":31});
            source["policies"] = json!({"proof":{"command":{"max_log_bytes":1234}}});
        }
        let yaml = serde_yaml::to_string(&source).unwrap();
        root.write("docs/workflows/inheritance.workflow.yaml", &yaml);
        let settings = InvocationSettings::new(
            root.root().to_owned(),
            None,
            Arc::new(selected.clone()),
            Some(root.path("configs/selected.yaml")),
            root.path(".acdd/code.sqlite"),
        );
        // Default remains the embedded profile overlay; its milestone topology is
        // intentional. Workspace/path resource inheritance must isolate topology.
        let result = if target == "default" {
            profile::validate_profile_document(&yaml)
        } else if profile_override {
            settings.run(|| {
                read_workflow(
                    root.root(),
                    Path::new("docs/workflows/inheritance.workflow.yaml"),
                )
            })
        } else {
            read_workflow(
                root.root(),
                Path::new("docs/workflows/inheritance.workflow.yaml"),
            )
        };
        let resolved = match result {
            Ok(profile) => profile,
            Err(error) => {
                failures.push(format!(
                    "{target}, local={local_override}, --profile={profile_override}: {error:#}"
                ));
                continue;
            }
        };
        resolved.validate().unwrap();
        if target == "none" {
            assert!(resolved.roles.is_empty());
            assert!(resolved.proofs.is_empty());
        } else if target == "default" {
            assert_eq!(resolved.roles, profile::compiled().roles);
            assert_eq!(resolved.proofs, profile::compiled().proofs);
            continue;
        } else {
            let base = if profile_override && target == "workspace" {
                &selected
            } else {
                &workspace
            };
            let role = &resolved.roles["builder"];
            let inherited = &base.roles["builder"];
            assert_eq!(
                role.runner, inherited.runner,
                "partial role override retains runner"
            );
            assert_eq!(role.prompt_template, inherited.prompt_template);
            assert_eq!(resolved.commands["second"], base.commands["second"]);
            if local_override {
                assert_eq!(
                    resolved.roles["reviewer"].runner.as_deref(),
                    Some("local-reviewer '{prompt}'")
                );
                assert_eq!(
                    resolved.roles["reviewer"].models,
                    base.roles["reviewer"].models
                );
                assert_eq!(role.models[0].model, "local-model");
                assert_eq!(role.models[0].reasoning.as_deref(), Some("high"));
                assert_eq!(role.guidance.as_deref(), Some("local role guidance"));
                assert_eq!(resolved.commands["first"].command(), "printf local");
                assert_eq!(
                    resolved.scope_args_for_path("src/lib.rs").as_deref(),
                    Some("--local")
                );
                assert_eq!(
                    serde_json::to_value(&resolved.guidance["inheritance_note"]).unwrap(),
                    json!("local guidance")
                );
                assert_eq!(
                    resolved.proofs["resource_proof"]
                        .schema
                        .as_ref()
                        .unwrap()
                        .schema_type,
                    "integer"
                );
                assert_eq!(resolved.max_log_bytes(), 1234);
                assert_eq!(resolved.limits.max_command_timeout_seconds, 31);
            } else {
                assert_eq!(resolved.roles, base.roles);
                assert_eq!(resolved.commands, base.commands);
                assert_eq!(resolved.policies, base.policies);
                assert_eq!(resolved.guidance, base.guidance);
                assert_eq!(resolved.proofs, base.proofs);
                assert_eq!(resolved.scope_args_by_pattern, base.scope_args_by_pattern);
                assert_eq!(resolved.limits, base.limits);
            }
        }
        assert_eq!(
            resolved.gates, harness.profile.gates,
            "{target} must retain local root gates"
        );
        assert_eq!(
            resolved.persistence,
            Some(PersistenceAction::None),
            "{target} must retain local save"
        );
        assert!(resolved
            .gates
            .iter()
            .all(|gate| gate.tasks.is_none() && !gate.receipt));
        if target == "workspace" && !local_override && !profile_override {
            let log_path = root.path("workflow-order.log");
            if log_path.exists() {
                fs::remove_file(&log_path).unwrap();
            }
            let executed = run_standalone_workflow(
                root.root(),
                Path::new("docs/workflows/inheritance.workflow.yaml"),
                None,
            )
            .unwrap();
            assert_eq!(executed["status"], "completed");
            assert_eq!(fs::read_to_string(log_path).unwrap(), "first\nsecond\n");
        }
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(
        failures.is_empty(),
        "explicit inheritance contract failures: {failures:#?}"
    );
}

#[test]
fn inheritance_rejects_root_self_reference_cycles_and_unconfined_paths() {
    let root = WorkflowHarness::ordered().materialize();
    let template: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    let mut a = template.clone();
    a["inherit"] = json!("configs/b.yaml");
    root.write("configs/a.yaml", &serde_yaml::to_string(&a).unwrap());
    let mut b = template.clone();
    b["inherit"] = json!("configs/a.yaml");
    root.write("configs/b.yaml", &serde_yaml::to_string(&b).unwrap());
    let cases = [
        ("inherit", json!("workspace"), true, "ACDD_PROFILE_INVALID"),
        (
            "inherit",
            json!("configs/a.yaml"),
            false,
            "ACDD_PROFILE_INHERIT_CYCLE",
        ),
        (
            "inherit",
            json!("docs/workflows/rejected.yaml"),
            false,
            "ACDD_PROFILE_INHERIT_CYCLE",
        ),
        (
            "inherit",
            json!("../outside.yaml"),
            false,
            "ACDD_PROFILE_PATH_INVALID",
        ),
        (
            "inherit",
            json!("/etc/passwd"),
            false,
            "ACDD_PROFILE_PATH_INVALID",
        ),
        ("inherit", json!(true), false, "ACDD_PROFILE_SCHEMA_INVALID"),
        (
            "inherit",
            json!(false),
            false,
            "ACDD_PROFILE_SCHEMA_INVALID",
        ),
        (
            "inherit_defaults",
            json!(true),
            false,
            "ACDD_PROFILE_SCHEMA_INVALID",
        ),
    ];
    let mut failures = Vec::new();
    for (field, value, root_profile, diagnostic) in cases {
        let mut source = template.clone();
        if field != "inherit" {
            source.as_object_mut().unwrap().remove("inherit");
        }
        source[field] = value.clone();
        let yaml = serde_yaml::to_string(&source).unwrap();
        let result = if root_profile {
            root.write(".acdd/profile.yaml", &yaml);
            profile::load_for_workspace(root.root()).map(|_| ())
        } else {
            root.write("docs/workflows/rejected.yaml", &yaml);
            read_workflow(root.root(), Path::new("docs/workflows/rejected.yaml")).map(|_| ())
        };
        match result {
            Ok(()) => failures.push(format!("{field}={value}: invalid inheritance was accepted")),
            Err(error) => {
                let message = format!("{error:#}");
                if !message.contains(diagnostic) {
                    failures.push(format!(
                        "{field}={value}: expected diagnostic code {diagnostic}, got {message}"
                    ));
                }
            }
        }
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(
        failures.is_empty(),
        "inheritance rejection contract: {failures:#?}"
    );
}

#[test]
fn builtin_authoring_workflows_fall_back_to_embedded_profiles_and_prefer_local_files() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    let mut workspace = fixture_profile();
    let runner = WorkflowHarness::agents().profile.roles["builder"].clone();
    for role in workspace.roles.values_mut() {
        role.runner = runner.runner.clone();
        role.prompt_template = runner.prompt_template.clone();
        role.models =
            serde_json::from_value(json!([{"model":"repository-model","reasoning":"high"}]))
                .unwrap();
    }
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    let cases = [
        "author-spec",
        "author-plan",
        "author-milestone",
        "author-workflow",
        "code-quality-and-polish",
    ];
    let mut failures = Vec::new();
    for name in cases {
        let local = format!("docs/workflows/{name}.workflow.yaml");
        let fallback =
            resolve_workflow(root.root(), name).and_then(|path| read_workflow(root.root(), &path));
        match fallback {
            Ok(profile) => {
                profile.validate().unwrap();
                assert!(!profile.gates.is_empty(), "{name}");
                assert!(
                    profile
                        .gates
                        .iter()
                        .all(|gate| gate.tasks.is_none() && !gate.receipt),
                    "{name}"
                );
                assert!(
                    matches!(
                        profile.persistence,
                        Some(PersistenceAction::None | PersistenceAction::Logfile)
                    ),
                    "{name}"
                );
                for gate in profile.executable_gates() {
                    if let Some(role_id) = &gate.role {
                        let role = &profile.roles[role_id];
                        assert_eq!(
                            role.runner, runner.runner,
                            "{name}: inherit the repository runner for {role_id}"
                        );
                        assert_eq!(role.models[0].model, "repository-model", "{name}");
                        assert_eq!(role.prompt_template, runner.prompt_template, "{name}");
                    }
                }
            }
            Err(error) => failures.push(format!("{name}: embedded fallback: {error:#}")),
        }
        assert!(
            !root.path(&local).exists(),
            "builtin lookup must not install files"
        );
        root.write(&local, include_str!("../fixtures/workflows/ordered.yaml"));
        let path = resolve_workflow(root.root(), name).unwrap();
        assert_eq!(
            path,
            Path::new(&local),
            "local {name} must shadow its builtin"
        );
        let local_profile = read_workflow(root.root(), &path).unwrap();
        assert_eq!(local_profile.gates, harness.profile.gates, "{name}");
        assert_eq!(local_profile.commands, harness.profile.commands, "{name}");
    }
    let result = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/author-spec.workflow.yaml"),
        None,
    )
    .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\n"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(
        failures.is_empty(),
        "builtin fallback contract: {failures:#?}"
    );
}

#[test]
fn builtin_plan_preset_resolves_without_an_installed_file_and_prefers_local_override() {
    let root = WorkflowHarness::ordered().materialize();
    let mut workspace = fixture_profile();
    workspace.limits.max_command_timeout_seconds = 19;
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    root.write("acdd.yaml", "acdd_profile: plan\n");
    let fallback = profile::load_for_workspace(root.root());
    let mut failures = Vec::new();
    match fallback {
        Ok(profile) => {
            profile.validate().unwrap();
            assert_eq!(profile.persistence, Some(PersistenceAction::Snapshot));
            assert!(
                profile.gates.iter().all(|gate| gate.tasks.is_some()),
                "Plan preset must not import milestone root gates"
            );
            assert_eq!(profile.roles, workspace.roles);
            assert_eq!(profile.limits, workspace.limits);
            for gate in profile.executable_gates() {
                assert_ne!(
                    gate.proof.kind(),
                    Some(profile::ProofKind::Contract),
                    "Plan has no mandatory red-contract ceremony"
                );
                assert_ne!(gate.persistence.post, Some(PersistenceAction::Commit));
            }
        }
        Err(error) => failures.push(format!("plan fallback: {error:#}")),
    }
    let path = "docs/workflows/plan.workflow.yaml";
    assert!(
        !root.path(path).exists(),
        "builtin lookup must not install files"
    );
    let mut local = workspace.clone();
    local.persistence = Some(PersistenceAction::Snapshot);
    local.gates.retain(|gate| gate.tasks.is_some());
    local.limits.max_command_timeout_seconds = 29;
    root.write(path, &standalone_profile_yaml(&local));
    match profile::load_for_workspace(root.root()) {
        Ok(profile) => {
            assert_eq!(profile.gates, local.gates);
            assert_eq!(
                profile.limits, local.limits,
                "local plan preset takes precedence"
            );
            assert_eq!(profile.persistence, local.persistence);
        }
        Err(error) => failures.push(format!("local plan override: {error:#}")),
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(failures.is_empty(), "plan preset contract: {failures:#?}");
}

#[test]
fn workflow_command_findings_reach_on_fail_roles_and_clear_after_success() {
    let root = WorkflowHarness::agents().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    source["commands"] = json!({
        "check": {"command":"if test -f workflow-role-sessions.jsonl; then true; else printf 'workflow command diagnostic\\n' >&2; exit 37; fi", "output":{"require_counts":false}},
        "fail": {"command":"printf 'second command diagnostic\\n' >&2; exit 41", "output":{"require_counts":false}},
        "recover": {"command":"true", "output":{"require_counts":false}}
    });
    source["gates"] = json!([
        {"id":"check","command":"check","proof":"none","fail":"repair","pass":"stale","max_attempts":2},
        {"id":"repair","role":"builder","proof":"none","pass":"check"},
        {"id":"stale","command":"fail","proof":"none","fail":"recover","max_attempts":2},
        {"id":"recover","command":"recover","proof":"none"},
        {"id":"inspect","role":"reviewer","proof":"none"}
    ]);
    root.write(
        "docs/workflows/findings.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/findings.yaml"), None)
            .unwrap();
    let outcomes = result["outcomes"].as_array().unwrap();
    assert_eq!(
        outcomes
            .iter()
            .map(|row| row["gate"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["check", "repair", "check", "stale", "recover", "inspect"]
    );
    assert_eq!(outcomes[0]["passed"], false);
    assert_eq!(outcomes[2]["passed"], true);
    assert_eq!(outcomes[3]["passed"], false);
    assert_eq!(outcomes[4]["passed"], true);
    let sessions: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["gate"], "repair");
    assert!(
        sessions[0]["findings"].is_object(),
        "on_fail repair must receive current command findings: {}",
        sessions[0]
    );
    let findings = &sessions[0]["findings"];
    assert_eq!(findings["code"], "COMMAND_FAILED");
    assert_eq!(findings["gate"], "check");
    assert_eq!(findings["diagnostics"], outcomes[0]["evidence"]);
    assert_eq!(findings["diagnostics"][0]["command_id"], "check");
    assert_eq!(findings["diagnostics"][0]["observation"]["exit_code"], 37);
    let diag_text = findings["diagnostics"][0]["observation"]["diagnostic"]
        .as_str()
        .unwrap_or_default();
    assert!(
        diag_text.contains("workflow command diagnostic"),
        "expected diagnostic stderr snippet, got {findings}"
    );
    assert_eq!(sessions[1]["gate"], "inspect");
    assert!(
        sessions[1]["findings"].is_null(),
        "a successful command must clear the preceding failure before the next role"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn blueprint_schema_validates_additive_design_fields_without_weakening_core_proof() {
    let profile = profile::compiled();
    let schema = profile.proofs["blueprint"].schema.as_ref().unwrap();
    let legacy = json!({
        "cited_invariants":["INV-WORKFLOW-PROFILE-INHERITANCE"],
        "cited_symbols":["fn:read_workflow"],
        "global_negative_boundaries":["No task rows for workflows"],
        "blackboard_decision_id":"201",
        "tasks":{"profiles":{
            "layer":"tasks", "summary":"Resolve workflow resources",
            "exported_interfaces":["fn:resolve_effective_workflow_profile"],
            "negative_boundaries":["No topology inheritance"],
            "incoming_seams":[], "outgoing_seams":[]
        }}
    });
    assert!(
        schema.validate_value(&legacy, "$.scheme_proof").is_ok(),
        "omitted optional design fields preserve legacy proofs"
    );
    let fields = [
        "ac_mapping",
        "c2_boundary_impact",
        "adr_required",
        "sad_updates",
    ];
    let root = WorkflowHarness::agents().materialize();
    let mut source: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    source["proofs"] = serde_json::to_value(&profile.proofs).unwrap();
    source["gates"] = json!([{"id":"design","role":"builder","proof":"blueprint"}]);
    let gate: profile::GateDef = serde_json::from_value(source["gates"][0].clone()).unwrap();
    let projected = acdd_tasks::tasks::gates::proof_template(&gate, &profile).unwrap();
    let mut expanded = legacy.clone();
    let mut failures = Vec::new();
    for field in fields {
        if !schema.properties.contains_key(field) {
            failures.push(format!("missing typed Specify/Design field {field}"));
        }
        if projected["scheme_proof"].get(field).is_some() {
            failures.push(format!("role proof template populated optional {field}"));
        }
        expanded[field] = match field {
            "ac_mapping" | "sad_updates" => json!(["example"]),
            "c2_boundary_impact" => json!("boundary"),
            "adr_required" => json!(false),
            _ => unreachable!(),
        };
        let mut malformed = legacy.clone();
        malformed[field] = Value::Null;
        if schema.validate_value(&malformed, "$.scheme_proof").is_ok() {
            failures.push(format!("{field}: untyped null accepted"));
        }
    }
    for field in [
        "cited_invariants",
        "cited_symbols",
        "global_negative_boundaries",
        "blackboard_decision_id",
        "tasks",
    ] {
        let mut missing = legacy.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            schema.validate_value(&missing, "$.scheme_proof").is_err(),
            "core {field} stays required"
        );
    }
    for field in [
        "layer",
        "summary",
        "exported_interfaces",
        "negative_boundaries",
        "incoming_seams",
        "outgoing_seams",
    ] {
        let mut missing = legacy.clone();
        missing["tasks"]["profiles"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            schema.validate_value(&missing, "$.scheme_proof").is_err(),
            "task-slice {field} stays required"
        );
    }
    let mut malformed = legacy.clone();
    malformed["tasks"]["profiles"]["incoming_seams"] = json!([{"source_task":"source","contract_type":"unknown","contract_ref":"seam","specification":"contract"}]);
    assert!(
        schema.validate_value(&malformed, "$.scheme_proof").is_err(),
        "design additions preserve closed seam contract types"
    );
    // The public role caller reaches validate_gate_proof with the embedded
    // catalog. Only the unavailable model process is replaced by the fixture.
    root.write(
        "root-proof.json",
        &serde_json::to_string(&expanded).unwrap(),
    );
    root.write(
        "docs/workflows/design.yaml",
        &serde_yaml::to_string(&source).unwrap(),
    );
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/design.yaml"), None)
            .unwrap();
    assert_eq!(result["status"], "completed");
    let accepted = &result["outcomes"][0]["evidence"]["proof"]["scheme_proof"];
    assert_eq!(accepted["tasks"], expanded["tasks"]);
    for field in fields {
        assert_eq!(
            accepted.get(field),
            expanded.get(field),
            "{field} must survive gate validation"
        );
    }
    assert!(
        failures.is_empty(),
        "blueprint validation contract: {failures:#?}"
    );
}

#[test]
fn workflow_save_log_appends_observations_without_task_rows() {
    let root = WorkflowHarness::ordered().materialize();
    let source =
        include_str!("../fixtures/workflows/ordered.yaml").replace("save: none", "save: log");
    root.write("docs/workflows/ordered.yaml", &source);
    let mut retained_logs: std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> =
        std::collections::BTreeMap::new();
    let mut prior_bytes = 0;
    for run in 0..2 {
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/ordered.yaml"), None);
        assert!(
            result.is_ok(),
            "save: log must execute a taskless workflow: {result:#?}"
        );
        assert_eq!(result.unwrap()["status"], "completed");
        for (path, prior) in &retained_logs {
            assert!(
                fs::read(path).unwrap().starts_with(prior),
                "run {run} discarded earlier observations"
            );
        }
        // Discover runtime JSONL traces without fixing a particular run ID or
        // filename. A later run must preserve every earlier record byte-for-byte.
        for entry in walkdir::WalkDir::new(root.path(".acdd")) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("jsonl")
            {
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap();
            retained_logs.insert(entry.path().to_owned(), bytes);
        }
        assert!(
            !retained_logs.is_empty(),
            "save: log must persist structured run observations"
        );
        let total_bytes: usize = retained_logs.values().map(Vec::len).sum();
        assert!(
            total_bytes > prior_bytes,
            "run {run} must append new observations"
        );
        prior_bytes = total_bytes;
        assert!(!root.path(".acdd/tasks.sqlite").exists());
        assert!(!root.path(".worktrees").exists());
    }
    let mut records = Vec::new();
    for bytes in retained_logs.values() {
        let text = std::str::from_utf8(bytes).unwrap();
        records.extend(
            text.lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()),
        );
    }
    assert!(
        records.len() >= 4,
        "both runs must retain structured gate observations: {records:?}"
    );
    for command in ["first", "second"] {
        let observations: Vec<_> = records
            .iter()
            .filter(|record| {
                record["gate"].as_str() == Some(command)
                    && record["evidence"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|item| item["command_id"].as_str())
                        == Some(command)
            })
            .collect();
        assert!(
            observations.len() >= 2,
            "both runs must log {command}: {records:?}"
        );
        for observation in observations {
            let entry = &observation["evidence"][0]["observation"];
            assert_eq!(observation["passed"], true, "{observation}");
            assert!(
                observation["attempt"]
                    .as_u64()
                    .is_some_and(|attempt| attempt >= 1),
                "persist valid attempt number: {observation}"
            );
            assert_eq!(entry["exit_code"], 0, "persist exit_code: {observation}");
            assert!(
                entry["duration_ms"].is_u64(),
                "persist command timing: {observation}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\nfirst\nsecond\n"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
    assert!(!root.path(".git").exists());
}
