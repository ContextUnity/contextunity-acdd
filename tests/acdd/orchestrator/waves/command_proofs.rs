use super::harness;
use super::root_fixture::RootGateHarness;
use super::run_cli;
use crate::acdd::harness::plans::PlanHarness;
use crate::acdd::harness::workflows::WorkflowHarness;
use contextunity_acdd::acdd_tasks::tasks::profile;
use std::fs;

#[test]
fn workflow_rejects_executed_command_that_violates_proof_catalog() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/ordered.yaml",
        include_str!("../../fixtures/workflows/catalog-invalid.yaml"),
    );
    let error = run_cli(root.root(), &["workflow", "run", "ordered"]).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
        "{error:#}"
    );
}

#[test]
fn plan_rejects_executed_command_that_violates_proof_catalog() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "profiles/plan.yaml",
        include_str!("../../fixtures/plans/catalog-invalid.yaml"),
    );
    let error = run_cli(root.root(), &["plan", "run", "linear", "--in-place"]).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
        "{error:#}"
    );
}

#[test]
fn milestone_rejects_executed_command_that_violates_proof_catalog() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = RootGateHarness::with_dependencies();
    let mut profile = h.read_profile();
    profile.proofs.insert("command".into(), serde_json::from_value(serde_json::json!({
        "schema": {"type": "object", "properties": {"tests_passed": {"type": "integer", "const": 2}}}
    })).unwrap());
    profile.commands.insert(
        "catalog-check".into(),
        serde_json::from_value(serde_json::json!({
            "command": "echo '1 passed; 0 failed'",
            "output": {"counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
        }))
        .unwrap(),
    );
    profile.gates.insert(
        0,
        serde_json::from_value(serde_json::json!({
            "id": "catalog_check", "command": "catalog-check", "proof": "command"
        }))
        .unwrap(),
    );
    h.commit_profile(&profile, "catalog proof fixture");
    let options = h.run_options();
    let error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
        "{error:#}"
    );
    let passes: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='catalog_check' AND action='pass'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(passes, 0);
}

#[test]
fn post_task_milestone_catalog_rejection_does_not_persist_pass() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile.proofs.insert("command".into(), serde_json::from_value(serde_json::json!({
        "schema": {"type": "object", "properties": {"command": {"type": "string", "const": "rust-check-and-test"}}}
    })).unwrap());
    profile.commands.insert(
        "catalog-check".into(),
        serde_json::from_value(serde_json::json!({
            "command": "echo '1 passed; 0 failed'",
            "output": {"counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
        }))
        .unwrap(),
    );
    profile.find_gate_mut("verify").unwrap().command = Some("catalog-check".into());
    h.commit_profile(&profile, "post-task catalog fixture");
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
        "{error:#}"
    );
    let traces: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_execution_traces WHERE subject='milestone' AND gate_path='verify' AND command_id='catalog-check' AND status='completed'", [], |row| row.get(0)
    ).unwrap();
    let passes: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='verify' AND action='pass'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(traces, 1);
    assert_eq!(passes, 0);
}

#[test]
fn milestone_command_catalog_checks_aggregate_counts() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = RootGateHarness::with_dependencies();
    let mut profile = h.read_profile();
    profile.proofs.insert("command".into(), serde_json::from_value(serde_json::json!({
        "schema": {"type": "object", "properties": {"tests_passed": {"type": "integer", "const": 2}}}
    })).unwrap());
    profile.commands.insert("aggregate".into(), serde_json::from_value(serde_json::json!({
        "by_pattern": {"src/lib.rs": "echo '1 passed; 0 failed' # first", "src/second.rs": "echo '1 passed; 0 failed' # second"},
        "output": {"counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
    })).unwrap());
    profile.commands.insert(
        "stop-root".into(),
        profile::CommandDef::Script("exit 1".into()),
    );
    profile.gates.insert(0, serde_json::from_value(serde_json::json!({"id": "aggregate", "command": "aggregate", "proof": "command", "max_attempts": 1})).unwrap());
    profile.gates.insert(1, serde_json::from_value(serde_json::json!({"id": "stop_root", "command": "stop-root", "proof": "none", "max_attempts": 1})).unwrap());
    h.commit_profile(&profile, "aggregate proof fixture");
    let options = h.run_options();
    let _ = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {});
    let passes: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='aggregate' AND action='pass'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(passes, 1);
}

#[test]
fn milestone_role_contract_observes_runtime_red_and_rejects_invalid_results() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    profile.commands.insert(
        "stop-root".into(),
        profile::CommandDef::Script("exit 1".into()),
    );
    profile.gates.insert(0, serde_json::from_value(serde_json::json!({
        "id": "root_contract", "role": "wave_role", "command": "root-contract",
        "proof": "contract", "expected_failure": {"exit_codes": [101], "stderr_contains": "assertion failed"}, "max_attempts": 1
    })).unwrap());
    profile.gates.insert(
        1,
        serde_json::from_value(serde_json::json!({
            "id": "stop_root", "command": "stop-root", "proof": "none", "max_attempts": 1
        }))
        .unwrap(),
    );
    fs::write(
        h.root().join("root-proof.json"),
        serde_json::json!({"seam_test_ref":"tests/acdd.rs::red","red_exit_code":101}).to_string(),
    )
    .unwrap();
    for (command, accepted) in [
        ("exit 0 # seam:tests/acdd.rs::red", false),
        (
            "printf 'could not compile: assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            false,
        ),
        (
            "printf 'assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            true,
        ),
    ] {
        profile.commands.insert(
            "root-contract".into(),
            serde_json::from_value(serde_json::json!({
                "command": command,
                "output": {"require_counts": false, "seam_ref_pattern": "seam:(\\S+)"}
            }))
            .unwrap(),
        );
        h.commit_profile(&profile, "root contract fixture");
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        if !accepted {
            assert!(
                format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
                "{error:#}"
            );
            assert!(h.root().join(".acdd/wave-roles.jsonl").exists());
        }
        let passes: i64 = h.task.store.connection.query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='root_contract' AND action='pass'", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(passes, i64::from(accepted), "{error:#}");
    }
}

#[test]
fn command_catalog_validation_rejects_invalid_inputs_across_orchestrators() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let workflow = WorkflowHarness::ordered().materialize();
    let plan_harness = PlanHarness::linear();
    let plan = plan_harness.materialize(plan_harness.path);
    let milestone = RootGateHarness::with_dependencies();
    let options = milestone.run_options();
    let base_profile = milestone.read_profile();
    let cases = [
        (
            "{ command: '' }",
            serde_json::json!({"command": ""}),
            false,
            "commands registry names and commands must be non-empty",
        ),
        (
            "{ command: 'touch forbidden-command', output: { seam_ref_pattern: '[' } }",
            serde_json::json!({"command": "touch forbidden-command", "output": {"seam_ref_pattern": "["}}),
            false,
            "invalid output.seam_ref_pattern regex",
        ),
        (
            "'touch forbidden-command'",
            serde_json::json!("touch forbidden-command"),
            true,
            "expected_failure.exit_codes must contain non-zero values",
        ),
    ];
    for (command, json_command, invalid_exit, expected_error) in cases {
        for domain in ["workflow", "plan", "milestone"] {
            let error = match domain {
                "workflow" => {
                    let expected = if invalid_exit {
                        "    expected_failure: { exit_codes: [0] }\n"
                    } else {
                        ""
                    };
                    workflow.write("docs/workflows/ordered.yaml", &format!("version: 2\ninherit: none\nsave: none\ncommands:\n  check: {command}\ngates:\n  - id: check\n    command: check\n    proof: contract\n{expected}"));
                    let error =
                        run_cli(workflow.root(), &["workflow", "run", "ordered"]).unwrap_err();
                    assert!(!workflow.path("forbidden-command").exists());
                    error
                }
                "plan" => {
                    let expected = if invalid_exit {
                        "          expected_failure: { exit_codes: [0] }\n"
                    } else {
                        ""
                    };
                    plan.write("profiles/plan.yaml", &format!("version: 2\ninherit: none\nsave: snapshot\ncommands:\n  check: {command}\ngates:\n  - tasks:\n      gates:\n        - id: check\n          command: check\n          proof: contract\n{expected}        - id: finish\n          receipt: true\n          save: snapshot\n"));
                    let error =
                        run_cli(plan.root(), &["plan", "run", "linear", "--in-place"]).unwrap_err();
                    assert!(!plan.path("forbidden-command").exists());
                    error
                }
                "milestone" => {
                    let mut profile = base_profile.clone();
                    profile.commands.insert(
                        "check-invalid".into(),
                        serde_json::from_value(json_command.clone()).unwrap(),
                    );
                    let mut gate = serde_json::json!({"id": "check_invalid", "command": "check-invalid", "proof": "contract"});
                    if invalid_exit {
                        gate["expected_failure"] = serde_json::json!({"exit_codes": [0]});
                    }
                    profile
                        .gates
                        .insert(0, serde_json::from_value(gate).unwrap());
                    fs::write(
                        milestone.root().join(".acdd/profile.yaml"),
                        format!(
                            "inherit: none\n{}",
                            serde_yaml::to_string(&profile).unwrap()
                        ),
                    )
                    .unwrap();
                    let error =
                        run_milestone_waves(milestone.root(), "m-wave", &options, &mut |_| {})
                            .unwrap_err();
                    assert!(!milestone.root().join("forbidden-command").exists());
                    error
                }
                _ => unreachable!(),
            };
            assert!(
                format!("{error:#}").contains(expected_error),
                "{domain}: {error:#}"
            );
        }
    }
}

#[test]
fn command_drivers_reject_unsupported_proofs_across_orchestrators() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let workflow = WorkflowHarness::ordered().materialize();
    let plan_harness = PlanHarness::linear();
    let plan = plan_harness.materialize(plan_harness.path);
    let milestone = RootGateHarness::with_dependencies();
    let options = milestone.run_options();
    let base_profile = milestone.read_profile();
    let cases = [
        ("review", serde_json::json!("review")),
        (
            "{ scheme: { type: object } }",
            serde_json::json!({"scheme": {"type": "object"}}),
        ),
    ];
    for (proof, json_proof) in cases {
        for domain in ["workflow", "plan", "milestone"] {
            match domain {
                "workflow" => {
                    workflow.write("docs/workflows/ordered.yaml", &format!("version: 2\ninherit: none\nsave: none\ncommands:\n  check: 'touch forbidden-command'\ngates:\n  - id: check\n    command: check\n    proof: {proof}\n"));
                    let result = run_cli(workflow.root(), &["workflow", "run", "ordered"]);
                    assert!(result.is_err(), "{proof}: {result:?}");
                    assert!(!workflow.path("forbidden-command").exists(), "{proof}");
                }
                "plan" => {
                    plan.write("profiles/plan.yaml", &format!("version: 2\ninherit: none\nsave: snapshot\ncommands:\n  check: 'touch forbidden-command'\ngates:\n  - tasks:\n      gates:\n        - id: check\n          command: check\n          proof: {proof}\n        - id: finish\n          receipt: true\n          save: snapshot\n"));
                    let result = run_cli(plan.root(), &["plan", "run", "linear", "--in-place"]);
                    assert!(result.is_err(), "{proof}: {result:?}");
                    assert!(!plan.path("forbidden-command").exists(), "{proof}");
                }
                "milestone" => {
                    let mut profile = base_profile.clone();
                    profile.commands.insert(
                        "unsupported".into(),
                        profile::CommandDef::Script("touch forbidden-command".into()),
                    );
                    profile.gates.insert(0, serde_json::from_value(serde_json::json!({"id": "unsupported", "command": "unsupported", "proof": json_proof})).unwrap());
                    fs::write(
                        milestone.root().join(".acdd/profile.yaml"),
                        format!(
                            "inherit: none\n{}",
                            serde_yaml::to_string(&profile).unwrap()
                        ),
                    )
                    .unwrap();
                    let error =
                        run_milestone_waves(milestone.root(), "m-wave", &options, &mut |_| {})
                            .unwrap_err();
                    assert!(
                        format!("{error:#}").contains(
                            "MILESTONE_PROOF_REQUIRED: command execution cannot supply this proof"
                        ),
                        "{proof}: {error:#}"
                    );
                    assert!(
                        !milestone.root().join("forbidden-command").exists(),
                        "{proof}"
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn milestone_root_command_without_required_counts_fails_without_retrying_forever() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = RootGateHarness::with_dependencies();
    let mut profile = h.read_profile();
    profile.commands.insert(
        "countless".into(),
        profile::CommandDef::Script("echo visited >> .acdd/countless-visits".into()),
    );
    profile.gates.insert(
        0,
        serde_json::from_value(serde_json::json!({
            "id": "countless", "command": "countless", "proof": "command", "max_attempts": 1
        }))
        .unwrap(),
    );
    h.commit_profile(&profile, "require root command counts");
    let options = h.run_options();
    assert!(run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).is_err());
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/countless-visits")).unwrap(),
        "visited\n"
    );
}

#[test]
fn workflow_command_without_required_counts_fails_without_retrying_forever() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/ordered.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  check: "echo visited >> visits"
gates:
  - id: check
    command: check
    proof: command
    max_attempts: 1
"#,
    );
    assert!(run_cli(root.root(), &["workflow", "run", "ordered"]).is_err());
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "visited\n"
    );
}

#[test]
fn workflow_command_can_explicitly_opt_out_of_counts() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/ordered.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  check:
    command: "echo visited >> visits"
    output:
      require_counts: false
gates:
  - id: check
    command: check
    proof: command
"#,
    );
    run_cli(root.root(), &["workflow", "run", "ordered"]).unwrap();
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "visited\n"
    );
}

#[test]
fn plan_command_without_required_counts_fails_without_retrying_forever() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "profiles/plan.yaml",
        r#"version: 2
inherit: none
save: snapshot
commands:
  check: "echo visited >> visits"
gates:
  - tasks:
      gates:
        - id: check
          command: check
          proof: command
          max_attempts: 1
        - id: finish
          receipt: true
          save: snapshot
"#,
    );
    assert!(run_cli(root.root(), &["plan", "run", "linear", "--in-place"]).is_err());
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "visited\n"
    );
}

#[test]
fn post_task_milestone_verification_accepts_absent_counts_when_optional() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile.commands.insert(
        "count-free".into(),
        serde_json::from_value(serde_json::json!({
            "command": "true", "output": {"require_counts": false}
        }))
        .unwrap(),
    );
    profile.find_gate_mut("verify").unwrap().command = Some("count-free".into());
    h.commit_profile(&profile, "count-free verification fixture");
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
}
