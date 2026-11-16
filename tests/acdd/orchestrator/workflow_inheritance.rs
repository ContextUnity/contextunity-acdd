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
fn workflow_resolution_matrix() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    // explicit_inherit_targets_merge_resources_with_local_and_selected_profile_precedence

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
    let mut a = template.clone();
    a["inherit"] = json!("configs/b.yaml");
    root.write("configs/a.yaml", &serde_yaml::to_string(&a).unwrap());
    let mut b = template.clone();
    b["inherit"] = json!("configs/a.yaml");
    root.write("configs/b.yaml", &serde_yaml::to_string(&b).unwrap());
    struct Resolution {
        value: Value,
        local: bool,
        selected: bool,
        field: &'static str,
        root: bool,
        diagnostic: Option<&'static str>,
    }
    let mut cases = Vec::new();
    for (target, local, selected) in [
        ("none", false, false),
        ("default", false, false),
        ("workspace", false, false),
        ("configs/resources.yaml", false, false),
        ("workspace", true, false),
        ("configs/resources.yaml", true, false),
        ("workspace", false, true),
        ("workspace", true, true),
        ("configs/resources.yaml", false, true),
    ] {
        cases.push(Resolution {
            value: json!(target),
            local,
            selected,
            field: "inherit",
            root: false,
            diagnostic: None,
        });
    }
    for (field, value, root_profile, diagnostic) in [
        ("inherit", json!("workspace"), true, "ACDD_PROFILE_INVALID"),
        (
            "inherit",
            json!("configs/a.yaml"),
            false,
            "ACDD_PROFILE_INHERIT_CYCLE",
        ),
        (
            "inherit",
            json!("docs/workflows/inheritance.workflow.yaml"),
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
    ] {
        cases.push(Resolution {
            value,
            local: false,
            selected: false,
            field,
            root: root_profile,
            diagnostic: Some(diagnostic),
        });
    }
    let mut failures = Vec::new();
    for case in cases {
        let target = case.value.as_str().unwrap_or("invalid");
        let local_override = case.local;
        let profile_override = case.selected;
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
        if case.field != "inherit" {
            source.as_object_mut().unwrap().remove("inherit");
        }
        source[case.field] = case.value.clone();
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
        let result = if case.root {
            root.write(".acdd/profile.yaml", &yaml);
            profile::load_for_workspace(root.root()).map(|p| (*p).clone())
        } else if target == "default" {
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
        if let Some(expected) = case.diagnostic {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{}={}: {error:#}",
                case.field,
                case.value
            );
            continue;
        }
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

    // Embedded fallback and local shadowing use the same isolated repository.
    let mut workspace = fixture_profile();
    let runner = WorkflowHarness::agents().profile.roles["builder"].clone();
    for role in workspace.roles.values_mut() {
        role.runner = runner.runner.clone();
        role.prompt_template = runner.prompt_template.clone();
        role.models =
            serde_json::from_value(json!([{"model":"repository-model","reasoning":"high"}]))
                .unwrap();
    }
    workspace.limits.max_command_timeout_seconds = 19;
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
    for name in [
        "author-spec",
        "author-plan",
        "author-milestone",
        "author-workflow",
        "code-quality-and-polish",
        "plan",
    ] {
        let is_plan = name == "plan";
        root.write(
            "acdd.yaml",
            if is_plan {
                "acdd_profile: plan\n"
            } else {
                "{}\n"
            },
        );
        let local = format!("docs/workflows/{name}.workflow.yaml");
        let fallback = if is_plan {
            (*profile::load_for_workspace(root.root()).unwrap()).clone()
        } else {
            let path = resolve_workflow(root.root(), name).unwrap();
            read_workflow(root.root(), &path).unwrap()
        };
        fallback.validate().unwrap();
        assert!(!fallback.gates.is_empty(), "{name}");
        assert!(
            !root.path(&local).exists(),
            "builtin lookup must not install {name}"
        );
        if is_plan {
            assert_eq!(fallback.persistence, Some(PersistenceAction::Snapshot));
            assert!(fallback.gates.iter().all(|gate| gate.tasks.is_some()));
            assert_eq!(fallback.roles, workspace.roles);
            assert_eq!(fallback.limits, workspace.limits);
            for gate in fallback.executable_gates() {
                assert_ne!(gate.proof.kind(), Some(profile::ProofKind::Contract));
                assert_ne!(gate.persistence.post, Some(PersistenceAction::Commit));
            }
        } else {
            assert!(fallback
                .gates
                .iter()
                .all(|gate| gate.tasks.is_none() && !gate.receipt));
            assert!(matches!(
                fallback.persistence,
                Some(PersistenceAction::None | PersistenceAction::Logfile)
            ));
            for gate in fallback.executable_gates() {
                if let Some(role_id) = &gate.role {
                    let role = &fallback.roles[role_id];
                    assert_eq!(role.runner, runner.runner, "{name}");
                    assert_eq!(role.models[0].model, "repository-model", "{name}");
                    assert_eq!(role.prompt_template, runner.prompt_template, "{name}");
                }
            }
        }
        let expected = if is_plan {
            let mut local_profile = workspace.clone();
            local_profile.persistence = Some(PersistenceAction::Snapshot);
            local_profile.gates.retain(|gate| gate.tasks.is_some());
            local_profile.limits.max_command_timeout_seconds = 29;
            root.write(&local, &standalone_profile_yaml(&local_profile));
            local_profile
        } else {
            root.write(&local, include_str!("../fixtures/workflows/ordered.yaml"));
            harness.profile.clone()
        };
        let resolved = if is_plan {
            (*profile::load_for_workspace(root.root()).unwrap()).clone()
        } else {
            let path = resolve_workflow(root.root(), name).unwrap();
            assert_eq!(path, Path::new(&local));
            read_workflow(root.root(), &path).unwrap()
        };
        assert_eq!(resolved.gates, expected.gates, "{name}");
        assert_eq!(resolved.commands, expected.commands, "{name}");
        assert_eq!(resolved.persistence, expected.persistence, "{name}");
        if is_plan {
            assert_eq!(resolved.limits, expected.limits);
        }
    }
    root.write("acdd.yaml", "{}\n");
    root.remove("workflow-order.log");
    let executed = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/author-spec.workflow.yaml"),
        None,
    )
    .unwrap();
    assert_eq!(executed["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\n"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn proof_schema_matrix() {
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
    let root = WorkflowHarness::ordered().materialize();
    let specification = read_workflow(
        root.root(),
        Path::new("docs/workflows/author-spec.workflow.yaml"),
    )
    .unwrap();
    let schema = specification.gates[0]
        .proof
        .resolve_schema(&specification.proofs)
        .unwrap();
    for (field, expected_type) in [
        ("goal", "string"),
        ("non_goals", "array"),
        ("ambiguities", "array"),
        ("acceptance_criteria", "array"),
        ("c1_boundary_impact", "array"),
        ("c2_boundary_impact", "array"),
        ("adr_required", "array"),
        ("sad_updates", "array"),
    ] {
        for omit in [true, false] {
            let mut invalid = structured.clone();
            let expected = if omit {
                invalid.as_object_mut().unwrap().remove(field);
                format!("TASK_EVIDENCE_INVALID: $.scheme_proof.{field} is required")
            } else {
                invalid[field] = Value::Null;
                format!("TASK_EVIDENCE_INVALID: $.scheme_proof.{field} must be {expected_type}")
            };
            let error = schema
                .validate_value(&invalid, "$.scheme_proof")
                .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

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
    let gate: profile::GateDef =
        serde_json::from_value(json!({"id":"design","role":"builder","proof":"blueprint"}))
            .unwrap();
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
    assert!(
        failures.is_empty(),
        "blueprint validation contract: {failures:#?}"
    );
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
    let custom =
        profile::validate_profile_document(&serde_yaml::to_string(&source).unwrap()).unwrap();
    let gate = &custom.gates[0];
    let schema = gate.proof.resolve_schema(&custom.proofs).unwrap();
    let template = acdd_tasks::tasks::gates::proof_template(gate, &custom).unwrap();
    assert!(template["scheme_proof"]["summary"].is_string());
    for field in ["boundary_notes", "requires_adr", "design"] {
        assert!(template["scheme_proof"].get(field).is_none());
    }
    for supplied in [
        json!({"summary":"Required field only"}),
        json!({"summary":"Complete design","boundary_notes":["No task engine"],"requires_adr":false,"design":{"goal":"Share resources","sad_updates":["Update architecture"]}}),
    ] {
        schema.validate_value(&supplied, "$.scheme_proof").unwrap();
    }
    for (field, value, expected) in [
        (
            "requires_adr",
            json!("not a boolean"),
            "TASK_EVIDENCE_INVALID: $.scheme_proof.requires_adr must be boolean",
        ),
        (
            "boundary_notes",
            Value::Null,
            "TASK_EVIDENCE_INVALID: $.scheme_proof.boundary_notes must be array",
        ),
        (
            "design",
            json!({}),
            "TASK_EVIDENCE_INVALID: $.scheme_proof.design.goal is required",
        ),
    ] {
        let mut malformed = json!({"summary":"Malformed optional property"});
        malformed[field] = value;
        assert_eq!(
            schema
                .validate_value(&malformed, "$.scheme_proof")
                .unwrap_err()
                .to_string(),
            expected
        );
    }
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn role_proof_ingress_matrix() {
    let root = WorkflowHarness::agents().materialize();
    let mut workspace = fixture_profile();
    for role in workspace.roles.values_mut() {
        role.runner = Some("./workflow-role-fixture workflow-runner ingress '{prompt}'".into());
        role.prompt_template = Some("{role_guidance}".into());
    }
    root.write(".acdd/profile.yaml", &standalone_profile_yaml(&workspace));
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

    let custom = source;
    let profile = profile::compiled();
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

    let fields = [
        "ac_mapping",
        "c2_boundary_impact",
        "adr_required",
        "sad_updates",
    ];
    let mut expanded = legacy;
    for field in fields {
        expanded[field] = match field {
            "ac_mapping" | "sad_updates" => json!(["example"]),
            "c2_boundary_impact" => json!("boundary"),
            "adr_required" => json!(false),
            _ => unreachable!(),
        };
    }
    let mut blueprint: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    blueprint["proofs"] = serde_json::to_value(&profile.proofs).unwrap();
    blueprint["gates"] = json!([{"id":"design","role":"builder","proof":"blueprint"}]);
    let mut invalid_spec = structured.clone();
    invalid_spec["goal"] = Value::Null;
    struct Ingress {
        name: &'static str,
        source: Option<Value>,
        supplied: Value,
        diagnostic: Option<&'static str>,
    }
    let cases=[
        Ingress{name:"builtin_author_spec_executes_structured_traceability_proof_and_emits_complete_guidance",source:None,supplied:structured,diagnostic:None},
        Ingress{name:"specify_exact_schema_path",source:None,supplied:invalid_spec,diagnostic:Some("TASK_EVIDENCE_INVALID: $.scheme_proof.goal must be string")},
        Ingress{name:"custom_catalog_proof_projects_optional_properties_without_requiring_them_at_runtime/required",source:Some(custom.clone()),supplied:json!({"summary":"Required field only"}),diagnostic:None},
        Ingress{name:"custom_catalog_proof_projects_optional_properties_without_requiring_them_at_runtime/complete",source:Some(custom.clone()),supplied:json!({"summary":"Complete design","boundary_notes":["No new task engine"],"requires_adr":false,"design":{"goal":"Share resources","sad_updates":["Update architecture"]}}),diagnostic:None},
        Ingress{name:"custom_catalog_proof_projects_optional_properties_without_requiring_them_at_runtime/malformed",source:Some(custom),supplied:json!({"summary":"Malformed","requires_adr":"not a boolean"}),diagnostic:Some("TASK_EVIDENCE_INVALID: $.scheme_proof.requires_adr must be boolean")},
        Ingress{name:"blueprint_schema_validates_additive_design_fields_without_weakening_core_proof",source:Some(blueprint),supplied:expanded,diagnostic:None},
    ];
    for case in cases {
        root.write(
            "root-proof.json",
            &serde_json::to_string(&case.supplied).unwrap(),
        );
        root.write("workflow-role-sessions.jsonl", "");
        let embedded = case.source.is_none();
        let path = if let Some(source) = case.source {
            let path = Path::new("docs/workflows/ingress.yaml");
            root.write(path, &serde_yaml::to_string(&source).unwrap());
            path.to_owned()
        } else {
            let path = resolve_workflow(root.root(), "author-spec").unwrap();
            assert!(!root.path(&path).exists());
            path
        };
        let result =
            run_standalone_workflow(root.root(), &path, Some("Specify inherited execution"));
        if let Some(expected) = case.diagnostic {
            let error = result.unwrap_err();
            assert_eq!(
                error.root_cause().to_string(),
                expected,
                "{}: {error:#}",
                case.name
            );
        } else {
            let result = result.unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
            assert_eq!(result["status"], "completed");
            let accepted = &result["outcomes"][0]["evidence"]["proof"]["scheme_proof"];
            assert_eq!(accepted, &case.supplied, "{}", case.name);
            if embedded {
                let sessions: Vec<Value> =
                    fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
                        .unwrap()
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                let context = &sessions[0]["context"];
                let template = &context["expected_response"]["proof"]["scheme_proof"];
                let schema = &context["gate_definition"]["proof"]["scheme"];
                let (guidance, _) = sessions[0]["prompt"]
                    .as_str()
                    .unwrap()
                    .split_once("\n\nGate context:\n")
                    .unwrap();
                let guidance = guidance.to_lowercase();
                for (field, keyword) in [
                    ("goal", "goal"),
                    ("non_goals", "non-goals"),
                    ("ambiguities", "ambiguit"),
                    ("acceptance_criteria", "ac-"),
                    ("c1_boundary_impact", "c1"),
                    ("c2_boundary_impact", "c2"),
                    ("adr_required", "adr"),
                    ("sad_updates", "sad"),
                ] {
                    assert!(template.get(field).is_some(), "{field}");
                    assert!(schema["properties"].get(field).is_some(), "{field}");
                    assert!(guidance.contains(keyword), "{keyword}");
                }
            }
        }
        assert!(!root.path(".acdd/tasks.sqlite").exists());
        assert!(!root.path(".worktrees").exists());
    }
}

#[test]
fn workflow_log_matrix() {
    let root = WorkflowHarness::ordered().materialize();
    let template: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/ordered.yaml")).unwrap();
    struct Case {
        name: &'static str,
        gate_only: bool,
        failed: bool,
        runs: usize,
    }
    for case in [
        Case {
            name: "workflow_gate_save_log_persists_observation_under_root_save_none",
            gate_only: true,
            failed: false,
            runs: 1,
        },
        Case {
            name: "workflow_save_log_persists_terminal_failed_status_before_returning_exhaustion",
            gate_only: false,
            failed: true,
            runs: 1,
        },
        Case {
            name: "workflow_save_log_appends_observations_without_task_rows",
            gate_only: false,
            failed: false,
            runs: 2,
        },
    ] {
        let mut source = template.clone();
        if case.gate_only {
            source["gates"][0]["save"] = json!("log");
        } else {
            source["save"] = json!("log");
        }
        if case.failed {
            source["commands"]["first"]["command"] = json!("exit 37");
            source["gates"] =
                json!([{"id":"first","command":"first","max_attempts":1,"fail":"first"}]);
        }
        let path = Path::new("docs/workflows/log-matrix.yaml");
        root.write(path, &serde_yaml::to_string(&source).unwrap());
        if root.path("workflow-order.log").exists() {
            root.remove("workflow-order.log");
        }
        if root.path(".acdd/workflow-observations.jsonl").exists() {
            root.remove(".acdd/workflow-observations.jsonl");
        }
        let mut retained = std::collections::BTreeMap::<std::path::PathBuf, Vec<u8>>::new();
        let mut prior_bytes = 0;
        for run in 0..case.runs {
            let result = run_standalone_workflow(root.root(), path, None);
            if case.failed {
                let error = result.as_ref().unwrap_err();
                assert_eq!(
                    error.root_cause().to_string(),
                    "WORKFLOW_ATTEMPTS_EXHAUSTED: first",
                    "{}",
                    case.name
                );
            } else {
                assert_eq!(
                    result.as_ref().unwrap()["status"],
                    "completed",
                    "{}",
                    case.name
                );
            }
            for (path, bytes) in &retained {
                assert!(
                    fs::read(path).unwrap().starts_with(bytes),
                    "run {run} discarded observations"
                );
            }
            for entry in walkdir::WalkDir::new(root.path(".acdd")) {
                let entry = entry.unwrap();
                if entry.file_type().is_file()
                    && entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl")
                {
                    retained.insert(entry.path().to_owned(), fs::read(entry.path()).unwrap());
                }
            }
            assert!(!retained.is_empty());
            let total_bytes: usize = retained.values().map(Vec::len).sum();
            assert!(total_bytes > prior_bytes);
            prior_bytes = total_bytes;
            let records: Vec<Value> =
                fs::read_to_string(root.path(".acdd/workflow-observations.jsonl"))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
            let observations: Vec<_> = records
                .iter()
                .filter(|row| row["gate"] == "first")
                .collect();
            assert_eq!(observations.len(), run + 1);
            let observed = observations.last().unwrap();
            assert_eq!(observed["passed"], !case.failed);
            assert_eq!(observed["attempt"], 1);
            assert_eq!(
                observed["evidence"][0]["observation"]["exit_code"],
                if case.failed { 37 } else { 0 }
            );
            if !case.failed && !case.gate_only {
                for command in ["first", "second"] {
                    let rows: Vec<_> = records
                        .iter()
                        .filter(|row| {
                            row["gate"] == command && row["evidence"][0]["command_id"] == command
                        })
                        .collect();
                    assert_eq!(rows.len(), run + 1);
                    for row in rows {
                        assert_eq!(row["passed"], true);
                        assert!(row["attempt"].as_u64().is_some_and(|attempt| attempt >= 1));
                        assert_eq!(row["evidence"][0]["observation"]["exit_code"], 0);
                        assert!(row["evidence"][0]["observation"]["duration_ms"].is_u64());
                    }
                }
            }
            if case.failed {
                assert_eq!(records.last().unwrap()["status"], "failed");
                assert_eq!(records.last().unwrap()["workflow"], path.to_str().unwrap());
            } else {
                assert_eq!(
                    observed["evidence"],
                    result.as_ref().unwrap()["outcomes"][0]["evidence"]
                );
                assert_eq!(
                    fs::read_to_string(root.path("workflow-order.log")).unwrap(),
                    "first\nsecond\n".repeat(run + 1)
                );
                let timing = &observed["evidence"][0]["observation"];
                assert!(timing["duration_ms"].is_u64(), "{timing}");
            }
        }
        assert!(!root.path(".acdd/tasks.sqlite").exists());
        assert!(!root.path(".worktrees").exists());
        assert!(!root.path(".git").exists());
    }
}
