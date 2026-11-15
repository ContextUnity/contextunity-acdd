use crate::acdd::{
    harness::{plans::PlanHarness, workflows::WorkflowHarness},
    support::{git_command, git_head},
};
use clap::Parser;
use contextunity_acdd::{acdd_tasks::tasks::profile, cli::Cli};
use serde_json::Value;
use std::fs;

mod harness;

#[test]
fn native_absorbed_polish_passes_immutable_review_and_handoff() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let mut role = profile.roles["wave_role"].clone();
    role.runner = Some(format!("python3 -c 'import runpy;open(\"docs/polish-note.md\",\"w\").write(\"polished\\n\");runpy.run_path(\"{}\")' '{{prompt}}'", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py")));
    profile.roles.insert("polishing_role".into(), role);
    let polish = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap();
    polish.role = Some("polishing_role".into());
    polish.persistence.post = Some(PersistenceAction::Snapshot);
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "milestone_review")
        .unwrap()
        .persistence
        .immutable = true;
    let handoff = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "handoff")
        .unwrap();
    handoff.persistence.immutable = true;
    handoff.absorb_working_tree = true;
    profile.polish_scope.push("docs/polish-note.md".into());
    h.commit_profile(&profile, "immutable handoff fixture");
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let output = git_command(h.root(), &["show", "HEAD:docs/polish-note.md"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "polished\n");
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
    assert!(!h.root().join("docs/milestones/011-wave.md").exists());
}

#[test]
fn native_root_snapshots_capture_pre_and_post_without_moving_head() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let polish = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap();
    polish.persistence.pre = Some(PersistenceAction::Snapshot);
    polish.persistence.post = Some(PersistenceAction::Snapshot);
    h.commit_profile(&profile, "root snapshots");
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let raw: String = h.task.store.connection.query_row(
        "SELECT proof_payload FROM gate_transitions WHERE subject='milestone' AND gate_path='polish' AND action='pass' ORDER BY id DESC LIMIT 1", [], |row| row.get(0)
    ).unwrap();
    let payload: Value = serde_json::from_str(&raw).unwrap();
    for phase in ["pre_snapshot", "post_snapshot"] {
        let snapshot = &payload[phase];
        assert_eq!(snapshot["head"], git_head(h.root()));
        assert_eq!(
            snapshot["candidate_fingerprint"],
            payload[if phase == "pre_snapshot" {
                "candidate_before"
            } else {
                "candidate_after"
            }]
        );
        let output = git_command(
            h.root(),
            &[
                "rev-parse",
                "--verify",
                snapshot["snapshot_ref"].as_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            snapshot["snapshot_commit"].as_str().unwrap()
        );
    }
}

#[test]
fn native_immutable_review_rejects_candidate_mutation() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .persistence
        .post = Some(PersistenceAction::Snapshot);
    let mut role = profile.roles["wave_role"].clone();
    role.runner = Some(format!("python3 -c 'import runpy;open(\"src/lib.rs\",\"a\").write(\"// mutation\\n\");runpy.run_path(\"{}\")' '{{prompt}}'", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/acdd/fixtures/workflows/wave-role.py")));
    profile.roles.insert("mutating_review".into(), role);
    let review = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "milestone_review")
        .unwrap();
    review.role = Some("mutating_review".into());
    review.persistence.immutable = true;
    h.commit_profile(&profile, "immutable review");
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("TASK_CANDIDATE_MISMATCH"),
        "{error:#}"
    );
    let count: i64 = h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='milestone_review' AND action='pass'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn native_root_replay_after_reopen_invalidates_changed_candidate_and_preserves_receipt() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .persistence
        .post = Some(PersistenceAction::Snapshot);
    h.commit_profile(&profile, "replay snapshots");
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let receipt = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(receipt.status, "completed");
    let receipt = serde_json::to_value(receipt.receipt).unwrap();
    assert!(!receipt.is_null());
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn changed_after_review() {}\n",
    )
    .unwrap();
    for args in [
        vec!["add", "src/lib.rs"],
        vec!["commit", "-m", "changed candidate"],
    ] {
        assert!(git_command(h.root(), &args).status().unwrap().success());
    }
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_RUNNER_FAILED"),
        "{error:#}"
    );
    let count: i64 = h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='polish' AND action='pass'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert_eq!(
        serde_json::to_value(h.task.store.inspect(&h.task.id).unwrap().receipt).unwrap(),
        receipt
    );
    let after: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    assert_eq!(claims, after);
}

#[test]
fn workflow_rejects_executed_command_that_violates_proof_catalog() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/ordered.yaml",
        include_str!("../fixtures/workflows/catalog-invalid.yaml"),
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
        include_str!("../fixtures/plans/catalog-invalid.yaml"),
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
    let mut h = harness::WaveHarness::new(true);
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
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
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
    let mut h = harness::WaveHarness::new(true);
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
    let _ = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
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
    let milestone = harness::WaveHarness::new(true);
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
                    let error = run_milestone_waves(
                        milestone.root(),
                        "m-wave",
                        &milestone.options,
                        &mut |_| {},
                    )
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
    let milestone = harness::WaveHarness::new(true);
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
                    let error = run_milestone_waves(
                        milestone.root(),
                        "m-wave",
                        &milestone.options,
                        &mut |_| {},
                    )
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

    let mut h = harness::WaveHarness::new(true);
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
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
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
fn milestone_replay_reconstructs_visits_and_failure_budget() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    for (name, command) in [
        (
            "check",
            "echo check >> .acdd/replay-visits; if test -f .acdd/reviewed; then exit 1; fi",
        ),
        (
            "review",
            "echo review >> .acdd/replay-visits; touch .acdd/reviewed; exit 1",
        ),
        ("repair", "echo repair >> .acdd/replay-visits"),
    ] {
        profile.commands.insert(
            name.into(),
            serde_json::from_value(
                serde_json::json!({"command": command, "output": {"require_counts": false}}),
            )
            .unwrap(),
        );
    }
    for (index, gate) in [
        serde_json::json!({"id":"check","command":"check","proof":"none","max_attempts":2,"pass":"root_retry_review","fail":"repair"}),
        serde_json::json!({"id":"root_retry_review","command":"review","proof":"none","max_attempts":2,"fail":"check"}),
        serde_json::json!({"id":"repair","command":"repair","proof":"none","pass":"check"}),
    ].into_iter().enumerate() {
        profile.gates.insert(index, serde_json::from_value(gate).unwrap());
    }
    h.commit_profile(&profile, "replay retry budgets");
    for _ in 0..2 {
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: check"),
            "{error:#}"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/replay-visits")).unwrap(),
            "check\nreview\ncheck\nrepair\ncheck\n"
        );
    }
    let mut query = h.task.store.connection.prepare("SELECT gate_path, action, json_extract(proof_payload, '$.attempt') FROM gate_transitions WHERE subject='milestone' AND proof_kind='orchestrator' ORDER BY id").unwrap();
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            ("check".into(), "pass".into(), 1),
            ("root_retry_review".into(), "fail".into(), 1),
            ("check".into(), "fail".into(), 2),
            ("repair".into(), "pass".into(), 1),
            ("check".into(), "fail".into(), 3)
        ]
    );
}

#[test]
fn milestone_corrupt_root_route_fails_closed_without_reexecuting_observations() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    h.task.store.connection.execute("UPDATE gate_transitions SET proof_payload=json_set(proof_payload, '$.previous_observation', id) WHERE id=(SELECT max(id) FROM gate_transitions WHERE proof_kind='orchestrator')", []).unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("MILESTONE_CHECKPOINT_INVALID"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seen"
    );
}

#[test]
fn milestone_partial_root_prefix_reuses_refreshed_suffix_on_unchanged_restart() {
    use acdd_tasks::engine::{candidate_fingerprint, orchestrator::waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.partial_pre_task_checks(false);
    let original = fs::read(h.root().join("src/lib.rs")).unwrap();
    let initial = candidate_fingerprint(h.root()).unwrap();
    let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        first
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{first:#}"
    );
    assert_ne!(candidate_fingerprint(h.root()).unwrap(), initial);
    fs::write(h.root().join("src/lib.rs"), &original).unwrap();
    assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
    for _ in 0..2 {
        let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            resumed
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{resumed:#}"
        );
        assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
            "seen"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
            "seenseen"
        );
    }
}

#[test]
fn milestone_partial_root_prefix_preserves_failed_retry_route_on_restart() {
    use acdd_tasks::engine::{candidate_fingerprint, orchestrator::waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.partial_pre_task_checks(true);
    let original = fs::read(h.root().join("src/lib.rs")).unwrap();
    let initial = candidate_fingerprint(h.root()).unwrap();
    let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        first
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{first:#}"
    );
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
        "seenseenseen"
    );
    fs::write(h.root().join("src/lib.rs"), &original).unwrap();
    fs::remove_file(h.root().join(".acdd/preflight-retry-done")).unwrap();
    for _ in 0..2 {
        let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            resumed
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{resumed:#}"
        );
        assert_eq!(candidate_fingerprint(h.root()).unwrap(), initial);
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
            "seen"
        );
        assert_eq!(
            fs::read_to_string(h.root().join(".acdd/second-preflight.log")).unwrap(),
            "seenseenseenseenseenseen"
        );
    }
}

#[test]
fn milestone_restart_fences_delivered_but_unintegrated_isolated_predecessor() {
    use acdd_tasks::engine::orchestrator::{types::OrchestratorEvent, waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(false);
    let mut delivered = None;
    let first = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
        if let OrchestratorEvent::TaskDelivered { receipt_id, .. } = event {
            if delivered.is_none() {
                delivered = Some(receipt_id);
                fs::write(h.root().join("concurrent.txt"), "independent root writer\n").unwrap();
                for args in [
                    vec!["add", "concurrent.txt"],
                    vec!["commit", "-m", "concurrent root writer"],
                ] {
                    assert!(git_command(h.root(), &args).status().unwrap().success());
                }
            }
        }
    })
    .unwrap_err();
    assert!(
        format!("{first:#}").contains("Not possible to fast-forward"),
        "{first:#}"
    );
    let delivered = delivered.unwrap();
    assert!(!git_command(
        h.root(),
        &["merge-base", "--is-ancestor", &delivered, "HEAD"]
    )
    .status()
    .unwrap()
    .success());
    let accepted = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(accepted.status, "completed");
    let receipt = serde_json::to_value(&accepted.receipt).unwrap();
    let owned = fs::read_dir(h.root().join(".worktrees"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(git_head(&owned), delivered);
    let head = git_head(h.root());
    for in_place in [false, true] {
        h.options.in_place = in_place;
        let mut claims = Vec::new();
        let restart = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
            if let OrchestratorEvent::TaskClaimed { task_id, .. } = event {
                claims.push(task_id);
            }
        });
        assert!(
            claims.is_empty(),
            "in_place={in_place}: unintegrated predecessor permitted claims: {claims:?}; {restart:#?}"
        );
        assert!(format!("{:#}", restart.unwrap_err()).contains("MILESTONE_DELIVERY_NOT_INTEGRATED"));
    }
    assert_eq!(git_head(h.root()), head);
    assert_eq!(git_head(&owned), delivered);
    assert_eq!(
        serde_json::to_value(h.task.store.inspect(&h.task.id).unwrap().receipt).unwrap(),
        receipt
    );
    let second = format!("{}:second", h.task.id.rsplit_once(':').unwrap().0);
    let count: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_claims WHERE task_id=?1",
            [&second],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn milestone_pre_task_observations_resume_unchanged_content() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    for _ in 0..2 {
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
            "{error:#}"
        );
    }
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seen"
    );
}

#[test]
fn task_claim_fails_closed_until_pre_task_root_gates_pass() {
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(false);

    let error = h.task.claim_next().unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{message}"
    );
    assert!(message.contains("preflight"), "{message}");
    assert!(message.contains("gate_path"), "{message}");
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
}

#[test]
fn task_next_surfaces_pre_task_root_gates_before_wave_tasks() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(false);
    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "preflight");
    assert_eq!(action["active_stage_id"], "preflight");
    assert_eq!(action["gate"]["command"], "preflight");
    let task = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(task.status, "ready");
    assert!(
        task.worker_id.is_none(),
        "root gate dispatch claimed a wave task"
    );
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_claims WHERE task_id=?1",
            [&task.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 0, "root gate dispatch persisted a wave claim");
}

#[test]
fn task_next_keeps_the_barrier_closed_after_a_pre_task_gate_exhausts() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    let run_error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{run_error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{run_error:#}"
    );

    let before = h.task.store.inspect(&h.task.id).unwrap();
    let error = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );

    let after = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(after.status, "ready");
    assert_eq!(after.claim_revision, before.claim_revision);
    assert!(after.worker_id.is_none());
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM task_claims WHERE task_id=?1",
            [&h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        claims, 0,
        "an exhausted root barrier persisted a task claim"
    );
}

#[test]
fn pre_task_root_transitions_are_persisted_for_the_milestone_subject() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(true);
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{error:#}"
    );

    h.task
        .store
        .connection
        .execute(
            "UPDATE gate_transitions SET proof_kind='blueprint' WHERE gate_path='preflight' AND action='pass'",
            [],
        )
        .unwrap();

    let claim_error = h.task.claim_next().unwrap_err();
    let claim_message = format!("{claim_error:#}");
    assert!(
        claim_message.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{claim_message}"
    );
    assert!(
        claim_message.contains("stop_before_tasks"),
        "{claim_message}"
    );

    let (milestone_id, subject): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT milestone_id, subject FROM gate_transitions WHERE gate_path='preflight' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!milestone_id.is_empty());
    assert_eq!(subject, "milestone");
}

#[test]
fn task_next_surfaces_post_task_root_gates_after_all_tasks_complete() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::with_single_task(true);
    h.task.advance_to_completion().unwrap();

    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "polish");
    assert_eq!(action["active_stage_id"], "polish");
    assert_eq!(action["role"], "wave_role");
    assert_eq!(action["workflow_guidance"]["role"], "wave_role");
    assert_eq!(
        action["workflow_guidance"]["role_guide"],
        "Exercise the real task gate protocol."
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
}

fn install_post_task_route_profile(h: &harness::WaveHarness) {
    let path = h.root().join(".acdd/profile.yaml");
    let mut profile =
        profile::validate_profile_document(&fs::read_to_string(&path).unwrap()).unwrap();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let mut verify = profile
        .gates
        .iter()
        .find(|gate| gate.id == "verify")
        .unwrap()
        .clone();
    verify.on_success = Some("documentation".into());
    verify.on_fail = Some("polish".into());
    verify.max_attempts = Some(2);
    let mut polish = profile
        .gates
        .iter()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .clone();
    polish.on_success = Some("milestone_review".into());
    let mut milestone_review = profile
        .gates
        .iter()
        .find(|gate| gate.id == "milestone_review")
        .unwrap()
        .clone();
    milestone_review.on_success = Some("verify".into());
    let documentation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "documentation",
        "role": "wave_role",
        "proof": "none",
        "pass": "handoff"
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![
        task_container,
        verify,
        polish,
        milestone_review,
        documentation,
        handoff,
    ];
    profile.validate().unwrap();
    fs::write(
        path,
        format!(
            "inherit: none\n{}",
            serde_yaml::to_string(&profile).unwrap()
        ),
    )
    .unwrap();
}

fn record_milestone_root_transition(
    h: &harness::WaveHarness,
    gate_path: &str,
    action: &str,
    attempt: u32,
) {
    let manifest = fs::read_to_string(h.root().join(harness::MANIFEST)).unwrap();
    let milestone =
        contextunity_acdd::acdd_tasks::tasks::Milestone::parse(&manifest, "forge-mcp").unwrap();
    let milestone_identity = format!(
        "{}/{}/{}",
        milestone.repository, milestone.project, milestone.id
    );
    let payload = serde_json::json!({"attempt":attempt,"findings":null});
    h.task
        .store
        .connection
        .execute(
            "INSERT INTO gate_transitions (
                milestone_id, subject, gate_path, action, proof_kind, proof_payload,
                git_tree_sha, worker_id
            ) VALUES (?1, 'milestone', ?2, ?3, 'orchestrator', ?4, ?5, 'root-route-test')",
            rusqlite::params![
                milestone_identity,
                gate_path,
                action,
                payload.to_string(),
                git_head(h.root())
            ],
        )
        .unwrap();
}

#[test]
fn task_next_follows_verify_pass_edge_to_documentation() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::with_single_task(true);
    h.task.advance_to_completion().unwrap();
    install_post_task_route_profile(&h);
    record_milestone_root_transition(&h, "verify", "pass", 1);

    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "documentation");
    assert_eq!(action["gate"]["id"], "documentation");
}

#[test]
fn task_next_follows_polish_pass_edge_after_failed_verification() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::with_single_task(true);
    h.task.advance_to_completion().unwrap();
    install_post_task_route_profile(&h);
    record_milestone_root_transition(&h, "verify", "fail", 1);
    record_milestone_root_transition(&h, "polish", "pass", 1);

    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "milestone_review");
    assert_eq!(action["gate"]["id"], "milestone_review");
}

#[test]
fn task_next_follows_the_latest_transition_after_a_repeated_polish_cycle() {
    use acdd_tasks::engine::{manage, Manage, ManageAction};

    let mut h = harness::WaveHarness::with_single_task(true);
    h.task.advance_to_completion().unwrap();
    install_post_task_route_profile(&h);
    record_milestone_root_transition(&h, "verify", "fail", 1);
    record_milestone_root_transition(&h, "polish", "pass", 1);
    record_milestone_root_transition(&h, "milestone_review", "fail", 1);
    record_milestone_root_transition(&h, "polish", "pass", 2);

    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "milestone_review");
    assert_eq!(action["gate"]["id"], "milestone_review");
}

#[test]
fn milestone_waves_select_the_applicable_conditional_task_container() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    let task_pos = profile
        .gates
        .iter()
        .position(|gate| gate.tasks.is_some())
        .unwrap();
    let mut code = profile.gates.remove(task_pos);
    code.id = "code".into();
    code.tasks.as_mut().unwrap().when = Some("code".into());
    let mut docs = code.clone();
    docs.id = "docs".into();
    docs.tasks.as_mut().unwrap().when = Some("doc".into());
    docs.tasks.as_mut().unwrap().gates = vec![serde_json::from_value(
        serde_json::json!({"id":"docs_only","role":"wave_role","proof":"none"}),
    )
    .unwrap()];
    profile.gates.insert(task_pos, docs);
    profile.gates.insert(task_pos, code);
    h.commit_profile(&profile, "conditional task tracks");
    h.pre_task_checks(true);
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{error:#}"
    );
}

#[test]
fn invalid_pre_task_task_map_is_rejected_before_root_pass_is_recorded() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let h = harness::WaveHarness::with_blueprint_gate(true);
    fs::write(h.root().join(".acdd/invalid-task-proof-map"), "invalid").unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{error:#}"
    );

    let (action, payload): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT action, proof_payload FROM gate_transitions
             WHERE subject='milestone' AND gate_path='preflight' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(action, "fail");
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["findings"]["code"], "MILESTONE_TASK_PROOF_INVALID");
    let accepted_passes: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions
             WHERE subject='milestone' AND gate_path='preflight' AND action='pass'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(accepted_passes, 0);
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    assert_eq!(claims, 0);
}

#[test]
fn pre_task_resynthesis_requires_a_fresh_review() {
    use acdd_tasks::store::GateTransition;

    let mut h = harness::WaveHarness::with_nested_blueprint(true);
    let milestone = h.task.id.split_once(':').unwrap().0.to_owned();
    let record = |gate: &str, action: &str| {
        h.task
            .store
            .record_gate_transition(&GateTransition {
                milestone_id: milestone.clone(),
                subject: "milestone".into(),
                gate_path: format!("blueprint_synthesis.{gate}"),
                action: action.into(),
                proof_kind: "orchestrator".into(),
                proof_payload: serde_json::json!({"findings": {"scope_findings": ["revise"]}}),
                git_tree_sha: h.options.target_base.clone(),
                worker_id: gate.into(),
            })
            .unwrap();
    };
    record("synthesize", "pass");
    record("review", "pass");
    record("review", "fail");
    record("synthesize", "pass");
    let error = h.task.claim_next().unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );
    let advances: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject=?1 AND action='advance'",
            [&h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(advances, 0);
}

#[test]
fn pre_task_missing_reachable_gate_keeps_claim_blocked() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_single_task(true);
    h.pre_task_checks(true);
    let outcome = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(outcome.is_err());
    let error = h.task.claim_next().unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );
}

#[test]
fn pre_task_bypass_route_survives_store_reopen_and_wave_replay() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_single_task(true);
    h.pre_task_checks(false);
    let mut profile = h.read_profile();
    profile.gates[0].on_success = Some("conditional".into());
    profile.gates.insert(
        1,
        serde_json::from_value(serde_json::json!({
            "id": "bypassed", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    profile.gates.insert(
        2,
        serde_json::from_value(serde_json::json!({
            "id": "conditional", "when": "doc", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    profile.gates.insert(
        3,
        serde_json::from_value(serde_json::json!({
            "id": "required", "command": "preflight", "proof": "none"
        }))
        .unwrap(),
    );
    h.commit_profile(&profile, "persisted bypass route");
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let before: Vec<(String, String)> = h.task.store.connection.prepare(
        "SELECT gate_path, action FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('preflight','bypassed','conditional','required') ORDER BY id"
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        before,
        vec![
            ("preflight".into(), "pass".into()),
            ("required".into(), "pass".into())
        ]
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pre_task_bypass_reopens_before_fresh_task_claim() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    for (index, gate) in [
        serde_json::json!({"id":"start","role":"wave_role","proof":"none","on_success":"conditional"}),
        serde_json::json!({"id":"bypassed","role":"wave_role","proof":"none"}),
        serde_json::json!({"id":"conditional","when":"doc","role":"wave_role","proof":"none"}),
        serde_json::json!({"id":"required","role":"wave_role","proof":"none"}),
    ].into_iter().enumerate() {
        profile.gates.insert(index, serde_json::from_value(gate).unwrap());
    }
    h.commit_profile(&profile, "fresh claim bypass fixture");
    let worktree = h.root().to_string_lossy().into_owned();
    for stage in ["start", "required"] {
        claim(
            h.root(),
            Claim {
                task_id: "m-wave".into(),
                stage: stage.into(),
                worker_id: "architect".into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
        submit(h.root(), Submit { task_id: "m-wave".into(), stage: stage.into(),
            evidence: serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"none_proof":{}}}),
            action: Action::Pass, findings: None, session_fingerprint: None }).unwrap();
    }
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let skipped: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path IN ('bypassed','conditional')",
        [], |row| row.get(0)).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(h.task.claim_next().unwrap().status, "in_progress");
}

fn pre_task_multi_blueprint_fixture() -> harness::WaveHarness {
    use crate::acdd::{support::MilestoneFixtureBuilder, tasks::harness::TaskHarness};
    let mut h = harness::WaveHarness::new(true);
    h.with_blueprint_synthesis_root_gates();
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].id = "review".into();
    let source = MilestoneFixtureBuilder::new("m-wave")
        .task("task_a", &["src/a.rs"])
        .task("task_b", &["src/b.rs"])
        .build();
    h.task = TaskHarness::disk_with_profile(&source, harness::MANIFEST, profile);
    h
}

fn pre_task_submit_blueprint(h: &harness::WaveHarness, tasks: Value) -> anyhow::Result<Value> {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let worktree = h.root().to_string_lossy().into_owned();
    claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            worker_id: "architect".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )?;
    submit(
        h.root(),
        Submit {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"scheme_proof":{"tasks":tasks}}}),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
}

#[test]
fn pre_task_nested_blueprint_failed_synthesis_blocks_all_task_proofs() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let mut h = pre_task_multi_blueprint_fixture();
    let worktree = h.root().to_string_lossy().into_owned();
    claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            worker_id: "architect".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(h.root(), Submit { task_id:"m-wave".into(), stage:"blueprint_synthesis.synthesize".into(),
        evidence:serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"scheme_proof":{"tasks":{"task_a":{"goal":"a"},"task_b":{"goal":"b"}}}}}),
        action:Action::Reject, findings:Some(serde_json::json!({"scope_findings":["revise"]})), session_fingerprint:None }).unwrap();
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let second = format!("{}:task_b", h.task.id.split_once(':').unwrap().0);
    let stage = h.task.store.inspect(&second).unwrap().stage;
    let error = claim(
        h.root(),
        Claim {
            task_id: second,
            stage,
            worker_id: "other".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let failures: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.synthesize' AND action='fail'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(failures, 1);
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND action='advance'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pre_task_nested_blueprint_invalid_task_maps_fail_before_fanout() {
    let mut h = pre_task_multi_blueprint_fixture();
    for (tasks, detail) in [
        (
            serde_json::json!({"task_a":{"goal":"a"}}),
            "missing non-cancelled task_ref 'task_b'",
        ),
        (
            serde_json::json!({"task_a":{"goal":"a"},"task_b":{"goal":"b"},"foreign":{"goal":"x"}}),
            "unknown task_ref 'foreign'",
        ),
    ] {
        let error = pre_task_submit_blueprint(&h, tasks).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("MILESTONE_TASK_PROOF_INVALID") && message.contains(detail),
            "{message}"
        );
        assert!(format!("{:#}", h.task.claim_next().unwrap_err())
            .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
        let count: i64 = h.task.store.connection.query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND action='advance'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let root_passes: i64 = h
            .task
            .store
            .connection
            .query_row(
                "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND action='pass'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(root_passes, 0);
        assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    }
}

#[test]
fn pre_task_nested_blueprint_independent_review_injects_only_each_task_slice() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let h = pre_task_multi_blueprint_fixture();
    pre_task_submit_blueprint(
        &h,
        serde_json::json!({"task_a":{"goal":"slice-a"},"task_b":{"goal":"slice-b"}}),
    )
    .unwrap();
    let worktree = h.root().to_string_lossy().into_owned();
    let review = |worker: &str| {
        claim(
            h.root(),
            Claim {
                task_id: "m-wave".into(),
                stage: "blueprint_synthesis.review".into(),
                worker_id: worker.into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
    };
    assert!(
        format!("{:#}", review("architect").unwrap_err()).contains("TASK_REVIEW_NOT_INDEPENDENT")
    );
    review("reviewer").unwrap();
    submit(h.root(), Submit { task_id:"m-wave".into(), stage:"blueprint_synthesis.review".into(),
        evidence:serde_json::json!({"worker_id":"reviewer","worktree":worktree,"proof":{"none_proof":{}}}),
        action:Action::Pass, findings:None, session_fingerprint:None }).unwrap();
    let milestone = h.task.id.split_once(':').unwrap().0;
    for (task_ref, expected, other) in [
        ("task_a", "slice-a", "slice-b"),
        ("task_b", "slice-b", "slice-a"),
    ] {
        let id = format!("{milestone}:{task_ref}");
        let stage = h.task.store.inspect(&id).unwrap().stage;
        let result = claim(
            h.root(),
            Claim {
                task_id: id,
                stage,
                worker_id: format!("worker-{task_ref}"),
                worktree: worktree.clone(),
                bundle: Some(true),
                session_fingerprint: None,
            },
        )
        .unwrap();
        let inherited = &result["context_bundle"]["inherited_proofs"];
        assert_eq!(inherited[0]["proof"]["goal"], expected, "{result}");
        assert!(!inherited.to_string().contains(other), "{result}");
    }
}

#[test]
fn pre_task_exhausted_native_run_is_replaced_by_current_observations() {
    use acdd_tasks::engine::{
        claim, manage, orchestrator::waves::run_milestone_waves, Claim, Manage, ManageAction,
    };

    let mut h = harness::WaveHarness::with_nested_blueprint(true);
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].max_attempts = Some(1);
    profile.gates[0].gates[0].proof = serde_json::from_value(serde_json::json!({
        "scheme": {"type": "object", "properties": {"tasks": {"type": "object"}}}
    }))
    .unwrap();
    h.commit_profile(&profile, "exhausted blueprint fixture");
    fs::write(h.root().join("root-proof.json"), r#"{"tasks":{"first":{"layer":"obsolete","summary":"old","blackboard_decision_id":"old"}}}"#).unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{error:#}"
    );
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));

    fs::write(
        h.root().join("root-proof.json"),
        r#"{"tasks":{"first":{"layer":"current","summary":"new","blackboard_decision_id":"new"}}}"#,
    )
    .unwrap();
    fs::write(h.root().join("src/lib.rs"), "// changed candidate\n").unwrap();
    let stop_review = h.root().join(".acdd/stop-blueprint-review");
    fs::write(&stop_review, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("next-worker".into()),
            worktree: Some(h.root().to_string_lossy().into_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(action["subject"], "milestone");
    assert_eq!(action["stage"], "blueprint_synthesis.review");
    fs::remove_file(stop_review).unwrap();
    // Stop before dispatch so the subsequent claim exercises a fresh store.
    h.options.task.runner_timeout = std::time::Duration::ZERO;
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    let reviews: Vec<String> = h.task.store.connection.prepare(
        "SELECT action FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.review' ORDER BY id"
    ).unwrap().query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(reviews, ["fail", "pass"]);
    let task = h.task.store.inspect(&h.task.id).unwrap();
    let result = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: task.stage,
            worker_id: "fresh-worker".into(),
            worktree: h.root().to_string_lossy().into_owned(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    let inherited = &result["context_bundle"]["inherited_proofs"];
    assert!(inherited.to_string().contains("current"), "{result}");
    assert!(!inherited.to_string().contains("obsolete"), "{result}");
}

#[test]
fn pre_task_nested_blueprint_rejected_review_does_not_fan_out() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_nested_blueprint(true);
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].max_attempts = Some(1);
    h.commit_profile(&profile, "rejected nested blueprint fixture");
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND gate_path='blueprint_synthesis.synthesize' AND action='advance'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(count, 0);
    let rejected: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.review' AND action='fail'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(rejected, 1);
}

#[test]
fn pre_task_blueprint_synthesis_container_barrier_and_per_task_proof_injection() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_nested_blueprint(true);

    // 1. Direct claim before the preflight gate passes fails closed
    let claim_err = h.task.claim_next().unwrap_err();
    assert!(
        format!("{claim_err:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{claim_err:#}"
    );

    // 2. Stop at milestone_review before handoff so state is not pruned
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    let outcome = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(outcome.is_err(), "{outcome:#?}");

    // 3. Verify gate_transitions contains the fanned-out task proof for subject = full_task_id
    let (action, proof_payload): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT action, proof_payload FROM gate_transitions
             WHERE subject=?1 AND gate_path='blueprint_synthesis.synthesize' AND action='advance'",
            [&h.task.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("per-task proof transition should be recorded");
    assert_eq!(action, "advance");
    let payload: Value = serde_json::from_str(&proof_payload).unwrap();
    assert_eq!(payload["layer"], "fixture seam");
    assert_eq!(payload["summary"], "Exercise the pre-task proof boundary.");
    assert_eq!(payload["blackboard_decision_id"], "wave-blueprint");

    let transitions: Vec<(String, String)> = h.task.store.connection.prepare(
        "SELECT gate_path, action FROM gate_transitions WHERE subject='milestone' AND gate_path LIKE 'blueprint_synthesis.%' ORDER BY id"
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        transitions,
        vec![
            ("blueprint_synthesis.synthesize".into(), "pass".into()),
            ("blueprint_synthesis.review".into(), "fail".into()),
            ("blueprint_synthesis.synthesize".into(), "pass".into()),
            ("blueprint_synthesis.review".into(), "pass".into()),
        ]
    );

    // 4. Verify that task completed successfully with proof receipt
    let inspected = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(inspected.status, "completed");
    assert!(inspected.receipt.is_some());
}

#[test]
fn milestone_pre_task_observations_invalidate_changed_implementation() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    for committed in [false, true] {
        let mut h = harness::WaveHarness::new(true);
        h.pre_task_checks(true);
        assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
        fs::write(
            h.root().join("src/lib.rs"),
            "pub fn changed_after_preflight() {}\n",
        )
        .unwrap();
        if committed {
            for args in [
                vec!["add", "src/lib.rs"],
                vec!["commit", "-m", "changed implementation"],
            ] {
                assert!(git_command(h.root(), &args).status().unwrap().success());
            }
        }
        for _ in 0..2 {
            let error =
                run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
                "{error:#}"
            );
            assert_eq!(
                fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
                "seenseen",
                "committed={committed}"
            );
        }
    }
}

#[test]
fn milestone_pre_task_refresh_after_delivery_resumes_without_new_task_claims() {
    use acdd_tasks::engine::orchestrator::{types::OrchestratorEvent, waves::run_milestone_waves};
    let mut h = harness::WaveHarness::new(true);
    h.pre_task_checks(false);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    fs::remove_file(stop).unwrap();
    let mut claims = Vec::new();
    let resumed = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
        if let OrchestratorEvent::TaskClaimed { task_id, .. } = event {
            claims.push(task_id);
        }
    });
    assert!(resumed.is_ok(), "{resumed:#?}");
    assert!(claims.is_empty(), "{claims:?}");
    assert_eq!(
        fs::read_to_string(h.root().join(".acdd/preflight.log")).unwrap(),
        "seenseen"
    );
}

#[test]
fn selected_server_worktree_rebases_config_profile_and_index_without_leaking_root() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let primary = plan.materialize(plan.path);
    let selected = plan.materialize(plan.path);
    selected.write("src/lib.rs", "pub fn worktree_selected() {}\n");
    let settings = harness::selected_settings(
        primary.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/selected-index.sqlite",
    );
    let mut client = settings.run(|| {
        McpTestHarness::with_root_and_db(primary.root(), &settings.code_db, "worktree-server")
            .unwrap()
    });
    let found = client.payload("code_map_search", serde_json::json!({"pattern":"worktree_selected","exact":true,"worktree":selected.root()})).unwrap();
    assert_eq!(found["nodes"]["total"], 1);
    assert!(selected.path(".acdd/selected-index.sqlite").exists());
    assert!(!settings.code_db.exists());
    client
        .payload("task_list", serde_json::json!({"worktree":selected.root()}))
        .unwrap();
    assert!(selected.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!primary.path(".acdd/custom-tasks.sqlite").exists());
    let primary_result = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"worktree_selected","exact":true}),
        )
        .unwrap();
    assert_eq!(primary_result["nodes"]["total"], 0);
    assert!(settings.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn milestone_preflight_enforces_selected_module_policy() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "docs/milestones/001-selected.md",
        &MilestoneFixtureBuilder::new("m-selected")
            .task("one", &["src/first.rs"])
            .build(),
    );
    root.write("configs/minimal.yaml", "modules: {profile: minimal}\n");
    let error = run_cli(
        root.root(),
        &[
            "--config",
            "configs/minimal.yaml",
            "--profile",
            "profiles/plan.yaml",
            "milestone",
            "init",
            "docs/milestones/001-selected.md",
            "--in-place",
        ],
    )
    .unwrap_err();
    assert!(error.to_string().contains("FEATURE_DISABLED"), "{error:#}");
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    let result = run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "milestone",
            "init",
            "docs/milestones/001-selected.md",
            "--in-place",
        ],
    )
    .unwrap();
    assert_eq!(result["milestone_id"], "m-selected");
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn selected_config_reaches_cli_and_mcp_guide_validation() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected_guide() {}\n");
    let result = run_cli(
        root.root(),
        &["--config", "configs/index.yaml", "guide", "validate"],
    )
    .unwrap();
    assert_eq!(
        result["roots"],
        serde_json::json!([root.path("selected").canonicalize().unwrap()])
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/index.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "guide-server").unwrap()
    });
    let mcp = client
        .payload("forge_guide", serde_json::json!({"topic":"validate"}))
        .unwrap();
    assert_eq!(mcp["roots"], result["roots"]);
    assert_eq!(mcp["files"], result["files"]);
    assert!(!selection.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn validation_attributes_the_explicit_selected_profile() {
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    let result = run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "validate",
        ],
    )
    .unwrap();
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "PROFILE_VALID"
            && diagnostic["path"] == "profiles/plan.yaml"));
}

#[test]
fn selected_adapter_resolves_linked_sources_in_cli_and_mcp_ast_queries() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "configs/linked.yaml",
        "roots: [src]\nlinked_workspaces:\n  - name: extra\n    path: linked\n",
    );
    root.write("linked/lib.rs", "pub fn linked_selected() {}\n");
    let cli = run_cli(
        root.root(),
        &[
            "--db",
            ".acdd/linked-index.sqlite",
            "--config",
            "configs/linked.yaml",
            "ast",
            "grep",
            "pub fn $NAME() {}",
            "--language",
            "rust",
            "--path",
            "[extra]/lib.rs",
        ],
    )
    .unwrap();
    assert_eq!(
        cli["matches"]["items"][0]["path"], "[extra]/lib.rs",
        "{cli}"
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/linked.yaml",
        "profiles/plan.yaml",
        ".acdd/linked-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "linked-server").unwrap()
    });
    let mcp = client.payload("ast_grep_search", serde_json::json!({"pattern":"pub fn $NAME() {}","language":"rust","path":"[extra]/lib.rs"})).unwrap();
    assert_eq!(mcp["matches"]["items"][0]["path"], "[extra]/lib.rs");
    client.finish().unwrap();
}

#[test]
fn server_claim_uses_retained_explicit_profile_over_configured_profile() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "docs/milestones/001-selected.md",
        &MilestoneFixtureBuilder::new("m-selected")
            .task("one", &["src/first.rs"])
            .build(),
    );
    let selection = harness::selected_settings(
        root.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let mut client = selection.run(|| {
        McpTestHarness::with_root_and_db(root.root(), &selection.code_db, "profile-server").unwrap()
    });
    // Configuration points to rejected.yaml; the explicitly selected, already
    // validated profile remains bound even when its source changes afterward.
    root.write(
        "profiles/plan.yaml",
        include_str!("../fixtures/plans/rejected-profile.yaml"),
    );
    client
        .payload(
            "task_manage",
            serde_json::json!({"action":"sync","milestone_ref":"docs/milestones/001-selected.md"}),
        )
        .unwrap();
    let result = client.payload("task_claim", serde_json::json!({"task_id":"forge-mcp/m-selected:one","stage":"record","worker_id":"selected-profile-worker","worktree":root.root()})).unwrap();
    assert_eq!(result["stage"], "record");
    assert!(root.path(".acdd/custom-tasks.sqlite").is_file());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!selection.code_db.exists());
    client.finish().unwrap();
}

#[test]
fn server_selection_survives_transport_lifetime_and_refreshes() {
    use crate::common::{mcp_client::invalidate_mcp_connection_cache, mcp_harness::McpTestHarness};
    use acdd_tasks::tasks::configuration::current;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write(
        "configs/index.yaml",
        "roots: [selected]\ntasks_db: .acdd/server-tasks.sqlite\n",
    );
    root.write("selected/lib.rs", "pub fn before_refresh() {}\n");
    let selection = harness::selected_settings(
        root.root(),
        "configs/index.yaml",
        "profiles/plan.yaml",
        ".acdd/server-index.sqlite",
    );
    let db = root.path(".acdd/server-index.sqlite");
    let mut client =
        selection.run(|| McpTestHarness::spawn(root.root(), &db, "selected-server").unwrap());
    assert!(current(root.root()).is_none());
    let initialized = client.request_value("initialize", serde_json::json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"selection-proof","version":"1"}})).unwrap();
    assert!(initialized.get("error").is_none(), "{initialized}");
    client
        .notify("notifications/initialized", serde_json::json!({}))
        .unwrap();
    let before = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"before_refresh","exact":true}),
        )
        .unwrap();
    assert_eq!(before["nodes"]["total"], 1);
    root.write("selected/lib.rs", "pub fn after_refresh() {}\n");
    invalidate_mcp_connection_cache(&db).unwrap();
    let after = client
        .payload(
            "code_map_search",
            serde_json::json!({"pattern":"after_refresh","exact":true}),
        )
        .unwrap();
    assert_eq!(after["nodes"]["total"], 1);
    client.payload("task_list", serde_json::json!({})).unwrap();
    assert!(root.path(".acdd/server-tasks.sqlite").is_file());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(current(root.root()).is_none());
    client.finish().unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        Some(root.path("configs/index.yaml").canonicalize().unwrap())
    );
}

#[test]
fn simultaneous_servers_keep_selected_module_and_task_policies_independent() {
    use crate::common::mcp_harness::McpTestHarness;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/minimal.yaml", "modules: {profile: minimal}\n");
    let minimal = harness::selected_settings(
        root.root(),
        "configs/minimal.yaml",
        "profiles/plan.yaml",
        ".acdd/minimal-index.sqlite",
    );
    let full = harness::selected_settings(
        root.root(),
        "configs/custom.yaml",
        "profiles/plan.yaml",
        ".acdd/full-index.sqlite",
    );
    let parent = (
        std::env::var_os("ACDD_CONFIG"),
        std::env::var_os("ACDD_PROFILE"),
    );
    std::thread::scope(|threads| {
        let first = threads.spawn(|| {
            let mut client = minimal.run(|| {
                McpTestHarness::with_root_and_db(root.root(), &minimal.code_db, "minimal-server")
                    .unwrap()
            });
            let listed = client
                .request_value("tools/list", serde_json::json!({}))
                .unwrap();
            assert!(!listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "task_list"));
            let error = client
                .payload("task_list", serde_json::json!({}))
                .unwrap_err();
            assert!(error.to_string().contains("FEATURE_DISABLED"), "{error}");
            client.finish().unwrap();
        });
        let second = threads.spawn(|| {
            let mut client = full.run(|| {
                McpTestHarness::with_root_and_db(root.root(), &full.code_db, "full-server").unwrap()
            });
            let listed = client
                .request_value("tools/list", serde_json::json!({}))
                .unwrap();
            assert!(listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "task_list"));
            client.payload("task_list", serde_json::json!({})).unwrap();
            client.finish().unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();
    });
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert_eq!(
        (
            std::env::var_os("ACDD_CONFIG"),
            std::env::var_os("ACDD_PROFILE")
        ),
        parent
    );
}

#[test]
fn cli_query_rebuilds_existing_index_with_the_selected_policy() {
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected_query() {}\n");
    run_cli(root.root(), &["--db", ".acdd/query-index.sqlite", "build"]).unwrap();
    run_cli(
        root.root(),
        &[
            "--db",
            ".acdd/query-index.sqlite",
            "--config",
            "configs/index.yaml",
            "query",
            "overview",
        ],
    )
    .unwrap();
    let conn =
        contextunity_acdd::db::reader::open(&root.path(".acdd/query-index.sqlite"), root.root())
            .unwrap();
    let inventory = contextunity_acdd::db::reader::inventory_snapshot(&conn).unwrap();
    assert!(inventory
        .iter()
        .any(|entry| entry.path == "selected/lib.rs"));
    assert!(!inventory.iter().any(|entry| entry.path == "src/first.rs"));
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        Some(root.path("configs/index.yaml").canonicalize().unwrap())
    );
}

#[test]
fn cli_delta_selects_policy_and_preserves_legacy_persisted_selection() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write("configs/index.yaml", "roots: [selected]\n");
    root.write("selected/lib.rs", "pub fn selected() {}\n");
    let db = root.path(".acdd/selected-index.sqlite");
    run_cli(root.root(), &["--db", db.to_str().unwrap(), "build"]).unwrap();
    run_cli(
        root.root(),
        &[
            "--db",
            db.to_str().unwrap(),
            "--config",
            "configs/index.yaml",
            "delta",
            "--modified",
            "selected/lib.rs",
        ],
    )
    .unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    let selected = contextunity_acdd::core::config::read_policy(root.root(), &conn).unwrap();
    assert_eq!(
        selected.adapter_path.as_deref(),
        Some(
            root.path("configs/index.yaml")
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    assert_eq!(
        contextunity_acdd::db::reader::inventory_snapshot(&conn)
            .unwrap()
            .iter()
            .filter(|entry| !entry.is_doc)
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["selected/lib.rs"]
    );
    drop(conn);
    root.write("selected/lib.rs", "pub fn selected_after_delta() {}\n");
    run_cli(
        root.root(),
        &[
            "--db",
            db.to_str().unwrap(),
            "delta",
            "--modified",
            "selected/lib.rs",
        ],
    )
    .unwrap();
    let conn = contextunity_acdd::db::reader::open(&db, root.root()).unwrap();
    assert_eq!(
        contextunity_acdd::core::config::read_policy(root.root(), &conn)
            .unwrap()
            .adapter_path,
        selected.adapter_path
    );
    assert!(!root.path(".acdd/code-map.sqlite").exists());
}

#[test]
fn cli_missing_or_malformed_policy_never_changes_delta_generation() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let db = root.path(".acdd/selected-index.sqlite");
    run_cli(root.root(), &["--db", db.to_str().unwrap(), "build"]).unwrap();
    root.write("configs/broken.yaml", "roots: [\n");
    let before = fs::read(&db).unwrap();
    for config in ["configs/absent.yaml", "configs/broken.yaml"] {
        for command in ["serve", "delta"] {
            let mut args = vec!["--db", db.to_str().unwrap(), "--config", config, command];
            if command == "delta" {
                args.extend(["--modified", "src/first.rs"]);
            }
            assert!(run_cli(root.root(), &args).is_err());
            assert_eq!(fs::read(&db).unwrap(), before);
        }
    }
}

#[test]
fn milestone_cancelled_dependency_blocks_dispatch_without_completion_credit() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(true);
    let source = MilestoneFixtureBuilder::new("m-wave")
        .task("first", &["src/lib.rs"])
        .task_status("cancelled")
        .task_with_deps("second", &["src/second.rs"], &["first"])
        .build();
    fs::write(h.root().join(harness::MANIFEST), source).unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("TASK_DEPENDENCY_INCOMPLETE"),
        "{error:#}"
    );
    let cancelled = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.receipt.is_none());
    assert!(!h.root().join(".acdd/lifecycle/roles.jsonl").exists());
    assert!(h.root().join(harness::MANIFEST).exists());
}

#[test]
fn plan_immutable_gate_rejects_drift_without_completion_or_index_mutation() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let source = fs::read(root.path(harness.path)).unwrap();
    let head = git_head(root.root());
    let index = git_command(root.root(), &["ls-files", "--stage"])
        .output()
        .unwrap()
        .stdout;
    let error = run_cli(
        root.root(),
        &[
            "--profile",
            "profiles/immutable-drift.yaml",
            "plan",
            "run",
            "linear",
        ],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("TASK_CANDIDATE_MISMATCH"),
        "{error:#}"
    );
    assert_eq!(fs::read(root.path(harness.path)).unwrap(), source);
    assert_eq!(git_head(root.root()), head);
    assert_eq!(
        git_command(root.root(), &["ls-files", "--stage"])
            .output()
            .unwrap()
            .stdout,
        index
    );
    assert_eq!(
        fs::read_to_string(root.path("src/first.rs")).unwrap(),
        "changed\n"
    );
}

#[test]
fn explicit_missing_configuration_or_profile_fails_before_plan_execution() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    for flag in ["--config", "--profile"] {
        let error = run_cli(
            root.root(),
            &[flag, "missing.yaml", "plan", "run", "linear"],
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("CLI_PATH_MISSING"),
            "{error:#}"
        );
        assert!(!root.path("plan-execution.log").exists());
    }
}

#[test]
fn milestone_changed_content_invalidates_root_observation_reuse() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(true);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn example() { /* polish */ }\n",
    )
    .unwrap();
    assert!(git_command(h.root(), &["add", "src/lib.rs"])
        .status()
        .unwrap()
        .success());
    assert!(git_command(h.root(), &["commit", "-m", "fixture polish"])
        .status()
        .unwrap()
        .success());
    fs::remove_file(stop).unwrap();
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let contexts = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl")).unwrap();
    let polish = contexts
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|c| c["gate"] == "polish")
        .collect::<Vec<_>>();
    assert_eq!(polish.len(), 2);
    assert_ne!(
        polish[0]["subject_context"]["binding"],
        polish[1]["subject_context"]["binding"]
    );
}

#[test]
fn milestone_handoff_refuses_a_profile_route_without_independent_review() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let profile_path = h.root().join(".acdd/profile.yaml");
    let mut profile =
        profile::validate_profile_document(&fs::read_to_string(&profile_path).unwrap()).unwrap();
    profile.gates.retain(|gate| gate.id != "milestone_review");
    let profile_yaml = format!(
        "inherit: none\n{}",
        serde_yaml::to_string(&profile).unwrap()
    );
    fs::write(&profile_path, profile_yaml).unwrap();
    assert!(git_command(h.root(), &["add", "-f", ".acdd/profile.yaml"])
        .status()
        .unwrap()
        .success());
    assert!(
        git_command(h.root(), &["commit", "-m", "fixture gate profile"])
            .status()
            .unwrap()
            .success()
    );
    h.options.target_base = git_head(h.root());
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("HANDOFF_REVIEW_REQUIRED"),
        "{error:#}; task role records: {}",
        fs::read_to_string(h.root().join(".acdd/lifecycle/roles.jsonl")).unwrap_or_default()
    );
    assert!(h.root().join(harness::MANIFEST).exists());
}

#[test]
fn milestone_failed_verification_cannot_archive_passing_tasks() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let profile_path = h.root().join(".acdd/profile.yaml");
    let mut profile =
        profile::validate_profile_document(&fs::read_to_string(&profile_path).unwrap()).unwrap();
    profile.commands.insert(
        "check_full_repo".into(),
        profile::CommandDef::Script("exit 19".into()),
    );
    let profile_yaml = format!(
        "inherit: none\n{}",
        serde_yaml::to_string(&profile).unwrap()
    );
    fs::write(&profile_path, profile_yaml).unwrap();
    assert!(git_command(h.root(), &["add", "-f", ".acdd/profile.yaml"])
        .status()
        .unwrap()
        .success());
    assert!(
        git_command(h.root(), &["commit", "-m", "fixture gate profile"])
            .status()
            .unwrap()
            .success()
    );
    h.options.target_base = git_head(h.root());
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(error.to_string().contains("WORKFLOW_"), "{error:#}");
    let exit: i32 = h.task.store.connection.query_row("SELECT exit_code FROM gate_execution_traces WHERE subject='milestone' ORDER BY id DESC LIMIT 1", [], |row| row.get(0)).unwrap();
    assert_eq!(exit, 19);
    assert!(h.root().join(harness::MANIFEST).exists());
    assert!(!h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
}

fn configure_post_task_retry(h: &mut harness::WaveHarness, command: &str, limit: Option<u32>) {
    let mut profile = h.read_profile();
    profile.commands.insert(
        "check_full_repo".into(),
        profile::CommandDef::Script(command.into()),
    );
    let task = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let mut verify = profile.find_gate("verify").unwrap().clone();
    verify.on_fail = Some("verify".into());
    verify.on_success = None;
    verify.max_attempts = limit;
    let handoff = profile.find_gate("handoff").unwrap().clone();
    profile.gates = vec![task, verify, handoff];
    h.commit_profile(&profile, "post-task retry fixture");
}

#[cfg(unix)]
#[test]
fn milestone_post_task_signal_preserves_missing_exit_and_stops_retry() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    configure_post_task_retry(&mut h, "if test -f .acdd/post-task-signal; then printf 'test result: ok. 1 passed; 0 failed\\n'; exit 0; fi; touch .acdd/post-task-signal; kill -TERM $$", None);
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: verify"),
        "{error:#}"
    );
    let rows = h.task.store.connection.prepare("SELECT status, exit_code FROM gate_execution_traces WHERE subject='milestone' AND gate_path='verify' ORDER BY id").unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i32>>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows, [("completed".into(), None)]);
}

#[test]
fn milestone_post_task_nonzero_exit_consumes_failure_budget() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::with_single_task(true);
    configure_post_task_retry(&mut h, "exit 19", Some(2));
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: verify"),
        "{error:#}"
    );
    let rows = h.task.store.connection.prepare("SELECT status, exit_code FROM gate_execution_traces WHERE subject='milestone' AND gate_path='verify' ORDER BY id").unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i32>>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        rows,
        [
            ("completed".into(), Some(19)),
            ("completed".into(), Some(19))
        ]
    );
}

#[test]
fn milestone_restart_reuses_bound_root_observations_and_delivered_tasks() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(true);
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_RUNNER_FAILED"),
        "{error:#}"
    );
    let claims: u64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    fs::remove_file(stop).unwrap();
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let contexts = fs::read_to_string(h.root().join(".acdd/wave-roles.jsonl")).unwrap();
    let gates = contexts
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["gate"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(gates, ["polish", "milestone_review", "milestone_review"]);
    assert!(claims > 0);
    // Successful handoff prunes live claims only after accepting all existing receipts.
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
}

#[test]
fn milestone_waves_integrate_owned_isolated_worktrees_before_handoff() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(false);
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
    assert_eq!(
        fs::read_dir(h.root().join(".worktrees")).unwrap().count(),
        0
    );
}

#[test]
fn milestone_isolation_failure_preserves_owned_worktree_and_foreign_paths() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::new(false);
    fs::create_dir_all(h.root().join(".worktrees/foreign")).unwrap();
    fs::write(h.root().join(".worktrees/foreign/keep"), "user data").unwrap();
    fs::write(h.root().join(".acdd/fail-isolated-role"), "").unwrap();
    let head = git_head(h.root());
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("recoverable worktree"),
        "{error:#}"
    );
    assert_eq!(git_head(h.root()), head);
    assert_eq!(
        fs::read_to_string(h.root().join(".worktrees/foreign/keep")).unwrap(),
        "user data"
    );
    let entries = fs::read_dir(h.root().join(".worktrees"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert!(h.root().join(harness::MANIFEST).exists());
}

#[test]
fn milestone_waves_execute_dependencies_and_content_bound_handoff_in_place() {
    use acdd_tasks::engine::orchestrator::{types::OrchestratorEvent, waves::run_milestone_waves};
    let h = harness::WaveHarness::new(true);
    let mut events = Vec::new();
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |event| {
        events.push(event)
    });
    assert!(result.is_ok(), "{result:#?}");
    let starts = events
        .iter()
        .filter_map(|event| match event {
            OrchestratorEvent::WaveStarted { wave_index, .. } => Some(*wave_index),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![0, 1]);
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
    assert_eq!(
        fs::read_to_string(&h.verification_marker).unwrap(),
        "verified"
    );
    assert!(!h.root().join(".worktrees").exists());
}

#[test]
fn milestone_waves_reject_unobserved_or_stale_role_decisions() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    for (control, expected) in [
        ("no-wave-decision", "WORKFLOW_DECISION_REQUIRED"),
        ("stale-wave-review", "HANDOFF_EVIDENCE_STALE"),
    ] {
        let h = harness::WaveHarness::new(true);
        fs::write(h.root().join(".acdd").join(control), "").unwrap();
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert!(h.root().join(harness::MANIFEST).exists());
        assert!(!h
            .root()
            .join("docs/milestones/archive/011-wave.md")
            .exists());
    }
}

#[test]
fn plan_delivery_reuses_subtask_completion_and_evidence_guard() {
    use crate::acdd::harness::plans::PlanFixture;
    for (fixture, expected) in [
        (
            PlanFixture::PendingSubtask,
            Some("TASK_SUBTASKS_INCOMPLETE"),
        ),
        (
            PlanFixture::MissingSubtaskEvidence,
            Some("TASK_SUBTASK_EVIDENCE_MISSING"),
        ),
        (PlanFixture::CompletedSubtask, None),
    ] {
        let harness = PlanHarness::fixture(fixture);
        let root = harness.materialize(harness.path);
        let before = fs::read(root.path(harness.path)).unwrap();
        let result = run_cli(root.root(), &["plan", "run", "invalid"]);
        if let Some(expected) = expected {
            let error = result.unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(fs::read(root.path(harness.path)).unwrap(), before);
        } else {
            assert_eq!(result.unwrap()["status"], "completed");
        }
    }
}

#[test]
fn global_config_selects_existing_task_store_without_using_index_db() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--db",
            ".acdd/selected-index.sqlite",
            "task",
            "list",
        ],
    )
    .unwrap();
    assert!(root.path(".acdd/custom-tasks.sqlite").exists());
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".acdd/selected-index.sqlite").exists());
}

#[test]
fn global_settings_survive_interleaved_future_polls_without_leaking() {
    use contextunity_acdd::acdd_tasks::{
        tasks::configuration::{current, InvocationSettings},
        TaskSettings,
    };
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let profile = std::sync::Arc::new(
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap(),
    );
    let default = InvocationSettings::new(
        root.root().to_owned(),
        Some(root.path("acdd.yaml")),
        profile.clone(),
        None,
        root.path(".acdd/first-index.sqlite"),
    );
    let custom = InvocationSettings::new(
        root.root().to_owned(),
        Some(root.path("configs/custom.yaml")),
        profile,
        None,
        root.path(".acdd/second-index.sqlite"),
    );
    let workspace_root = root.root();
    let check = |expected: &'static str| async move {
        for _ in 0..3 {
            assert_eq!(
                TaskSettings::load(workspace_root)
                    .unwrap()
                    .tasks_db
                    .to_str(),
                Some(expected)
            );
            tokio::task::yield_now().await;
        }
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::join!(
                default.clone().scope(check(".acdd/tasks.sqlite")),
                custom.scope(check(".acdd/custom-tasks.sqlite"))
            );
        });
    assert!(current(root.root()).is_none());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        default.run(|| panic!("restore scoped settings"))
    }));
    assert!(current(root.root()).is_none());
    assert_eq!(
        TaskSettings::load(root.root()).unwrap().tasks_db.to_str(),
        Some(".acdd/tasks.sqlite")
    );
}

#[test]
fn global_selections_reach_role_children_without_parent_environment_mutation() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    WorkflowHarness::agents().write_to(&root);
    let original = (
        std::env::var_os("ACDD_CONFIG"),
        std::env::var_os("ACDD_PROFILE"),
        std::env::var_os("ACDD_DB"),
    );
    run_cli(
        root.root(),
        &[
            "--config",
            "configs/custom.yaml",
            "--profile",
            "profiles/plan.yaml",
            "--db",
            ".acdd/selected-index.sqlite",
            "workflow",
            "run",
            "agents",
        ],
    )
    .unwrap();
    let canonical_root = root.root().canonicalize().unwrap();
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    for record in records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
    {
        assert_eq!(
            record["config"],
            canonical_root.join("configs/custom.yaml").to_str().unwrap()
        );
        assert_eq!(
            record["profile"],
            canonical_root.join("profiles/plan.yaml").to_str().unwrap()
        );
        assert_eq!(
            record["db"],
            canonical_root
                .join(".acdd/selected-index.sqlite")
                .to_str()
                .unwrap()
        );
    }
    assert_eq!(
        (
            std::env::var_os("ACDD_CONFIG"),
            std::env::var_os("ACDD_PROFILE"),
            std::env::var_os("ACDD_DB")
        ),
        original
    );
}

#[test]
fn workflow_role_rejection_resumes_builder_and_preserves_independence() {
    let root = WorkflowHarness::agents().materialize();
    let result = run_cli(root.root(), &["workflow", "run", "agents"]).unwrap();
    assert_eq!(result["status"], "completed");
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["gate"], "build");
    assert_eq!(records[1]["gate"], "review");
    assert_eq!(records[0]["session"], records[2]["session"]);
    assert_ne!(records[0]["session"], records[1]["session"]);
    assert_eq!(
        records[2]["findings"]["scope_findings"][0],
        "Apply the required correction"
    );
    assert!(!root.path(".acdd").exists());
    assert!(!root.path(".git").exists());
}

#[test]
fn workflow_role_exit_success_without_decision_cannot_pass() {
    let root = WorkflowHarness::agents().materialize();
    fs::write(root.path("missing-decision"), "").unwrap();
    let error = run_cli(root.root(), &["workflow", "run", "agents"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_DECISION_REQUIRED"),
        "{error:#}"
    );
    assert!(!root.path(".acdd").exists());
}

#[test]
fn plan_dependency_validation_precedes_execution() {
    use crate::acdd::harness::plans::PlanFixture;
    for (fixture, expected) in [
        (PlanFixture::UnknownDependency, "PLAN_DEPENDENCY_UNKNOWN"),
        (PlanFixture::Cycle, "PLAN_DEPENDENCY_CYCLE"),
        (
            PlanFixture::CancelledDependency,
            "PLAN_DEPENDENCY_INCOMPLETE",
        ),
    ] {
        let harness = PlanHarness::fixture(fixture);
        let root = harness.materialize(harness.path);
        let before = fs::read(root.path(harness.path)).unwrap();
        let error = run_cli(root.root(), &["plan", "run", "invalid", "--in-place"]).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(!root.path("plan-execution.log").exists());
        assert_eq!(fs::read(root.path(harness.path)).unwrap(), before);
    }
}

#[test]
fn plan_scope_validation_precedes_execution() {
    use crate::acdd::harness::plans::PlanFixture;
    let harness = PlanHarness::fixture(PlanFixture::UnsafeScope);
    let root = harness.materialize(harness.path);
    let error = run_cli(root.root(), &["plan", "run", "invalid"]).unwrap_err();
    assert!(
        error.to_string().contains("PLAN_SCOPE_INVALID"),
        "{error:#}"
    );
    assert!(!root.path("plan-execution.log").exists());
}

#[test]
fn plan_completed_source_prevents_command_replay() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let first = run_cli(root.root(), &["plan", "run", "linear"]).unwrap();
    assert_eq!(first["snapshots"].as_array().unwrap().len(), 2);
    let source = fs::read(root.path(harness.path)).unwrap();
    let snapshots = harness.snapshots(&root);
    let second = run_cli(root.root(), &["plan", "run", "linear"]).unwrap();
    assert_eq!(second["status"], "completed");
    assert!(second["snapshots"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(root.path(harness.path)).unwrap(), source);
    assert_eq!(harness.snapshots(&root), snapshots);
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\nstep\n"
    );
}

#[test]
fn plan_explicit_commit_profile_is_rejected_before_effects() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let head = git_head(root.root());
    let error = run_cli(
        root.root(),
        &["--profile", "profiles/commit.yaml", "plan", "run", "linear"],
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("PLAN_PROFILE_COMMIT_FORBIDDEN"),
        "{error:#}"
    );
    assert!(!root.path("forbidden-plan-command").exists());
    assert_eq!(git_head(root.root()), head);
}

fn run_cli(root: &std::path::Path, arguments: &[&str]) -> anyhow::Result<Value> {
    let cli = Cli::try_parse_from(
        [
            "contextunity-forge-mcp",
            "--root",
            root.to_str().unwrap(),
            "--format",
            "json",
        ]
        .into_iter()
        .chain(arguments.iter().copied()),
    )?;
    let mut output = Vec::new();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(cli.run_with_writer(&mut output))?;
    Ok(serde_json::from_slice(&output)?)
}

#[test]
fn workflow_failure_routes_retry_without_creating_task_state() {
    let root = WorkflowHarness::gate_profile(
        "docs/workflows/retry.yaml",
        include_str!("../fixtures/workflows/retry.yaml"),
    )
    .materialize();
    let result = run_cli(root.root(), &["workflow", "run", "retry"]).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["outcomes"].as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(root.path("retry-result")).unwrap(),
        "retried\n"
    );
    assert!(!root.path(".acdd").exists());
}

#[test]
fn workflow_unbounded_failure_routes_through_repair_and_passes() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/unbounded.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  verify: { command: "echo verify >> visits; test -f repaired", output: { require_counts: false } }
  repair: { command: "echo repair >> visits; touch repaired", output: { require_counts: false } }
  done: { command: "true", output: { require_counts: false } }
gates:
  - id: verify
    command: verify
    pass: done
    fail: repair
  - id: repair
    command: repair
    pass: verify
  - id: done
    command: done
"#,
    );
    let result = run_cli(root.root(), &["workflow", "run", "unbounded"]).unwrap();
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "verify\nrepair\nverify\n"
    );
    assert_eq!(
        result["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (
                v["attempt"].as_u64().unwrap(),
                v["passed"].as_bool().unwrap()
            ))
            .collect::<Vec<_>>(),
        [(1, false), (1, true), (2, true), (1, true)]
    );
}

#[test]
fn workflow_finite_budget_exhausts_on_exact_failure_count() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/exhaust.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  verify: { command: "echo verify >> visits; exit 1", output: { require_counts: false } }
  repair: { command: "echo repair >> visits", output: { require_counts: false } }
gates:
  - id: verify
    command: verify
    max_attempts: 2
    fail: repair
  - id: repair
    command: repair
    pass: verify
"#,
    );
    let error = run_cli(root.root(), &["workflow", "run", "exhaust"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_ATTEMPTS_EXHAUSTED: verify"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "verify\nrepair\nverify\n"
    );
}

#[test]
fn workflow_successful_visit_does_not_consume_failure_budget() {
    let root = WorkflowHarness::ordered().materialize();
    root.write("docs/workflows/revisit.yaml", r#"version: 2
inherit: none
save: none
commands:
  verify: { command: "echo verify >> visits; if test -f review-rejected && ! test -f repaired; then exit 1; fi", output: { require_counts: false } }
  review: { command: "echo review >> visits; if test -f review-rejected; then exit 0; else touch review-rejected; exit 1; fi", output: { require_counts: false } }
  polish: { command: "echo polish >> visits", output: { require_counts: false } }
  repair: { command: "echo repair >> visits; touch repaired", output: { require_counts: false } }
  done: { command: "true", output: { require_counts: false } }
gates:
  - id: verify
    command: verify
    max_attempts: 2
    pass: review
    fail: repair
  - id: review
    command: review
    max_attempts: 2
    pass: done
    fail: polish
  - id: polish
    command: polish
    pass: verify
  - id: repair
    command: repair
    pass: verify
  - id: done
    command: done
"#);
    let result = run_cli(root.root(), &["workflow", "run", "revisit"]).unwrap();
    assert_eq!(
        fs::read_to_string(root.path("visits")).unwrap(),
        "verify\nreview\npolish\nverify\nrepair\nverify\nreview\n"
    );
    assert_eq!(
        result["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (
                v["gate"].as_str().unwrap(),
                v["attempt"].as_u64().unwrap(),
                v["passed"].as_bool().unwrap()
            ))
            .collect::<Vec<_>>(),
        [
            ("verify", 1, true),
            ("review", 1, false),
            ("polish", 1, true),
            ("verify", 2, false),
            ("repair", 1, true),
            ("verify", 3, true),
            ("review", 2, true),
            ("done", 1, true)
        ]
    );
}

#[test]
fn workflow_failure_exhaustion_prevents_successor_execution() {
    let root = WorkflowHarness::gate_profile(
        "docs/workflows/failing.yaml",
        include_str!("../fixtures/workflows/failing.yaml"),
    )
    .materialize();
    let error = run_cli(root.root(), &["workflow", "run", "failing"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{error:#}"
    );
    assert!(!root.path("must-not-run").exists());
    assert!(!root.path(".acdd").exists());
}

#[cfg(unix)]
#[test]
fn workflow_timeouts_reap_the_owned_process_group() {
    let root = WorkflowHarness::gate_profile(
        "docs/workflows/timeout.yaml",
        include_str!("../fixtures/workflows/timeout.yaml"),
    )
    .materialize();
    let source = fs::read_to_string(root.path("docs/workflows/timeout.yaml")).unwrap();
    let cases = [
        ("timeout", source.clone()),
        (
            "unbounded-timeout",
            source.replace("    max_attempts: 1\n", ""),
        ),
    ];
    for (name, profile) in cases {
        root.write(format!("docs/workflows/{name}.yaml"), &profile);
        let started = std::time::Instant::now();
        let error = run_cli(root.root(), &["workflow", "run", name]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WORKFLOW_COMMAND_TIMEOUT: timeout"),
            "{name}: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{name}"
        );
        let pid = fs::read_to_string(root.path("timeout.pid")).unwrap();
        let pid: libc::pid_t = pid.trim().parse().unwrap();
        assert!(pid > 0, "{name}");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "{name}: process {pid} is still alive"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "{name}"
        );
        assert!(!root.path(".acdd").exists());
        fs::remove_file(root.path("timeout.pid")).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn workflow_signal_interruption_stops_unbounded_route() {
    let root = WorkflowHarness::ordered().materialize();
    root.write(
        "docs/workflows/signal.yaml",
        r#"version: 2
inherit: none
save: none
commands:
  interrupted: "if test -f signal-visited; then exit 0; fi; touch signal-visited; kill -TERM $$"
gates:
  - id: interrupted
    command: interrupted
    fail: interrupted
"#,
    );
    let error = run_cli(root.root(), &["workflow", "run", "signal"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: interrupted"),
        "{error:#}"
    );
    assert!(root.path("signal-visited").exists());
}

#[cfg(unix)]
#[test]
fn milestone_signal_interruption_stops_unbounded_root_route() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::CommandDef;
    let mut h = harness::WaveHarness::new(true);
    let mut profile = h.read_profile();
    profile.commands.insert("signal_root".into(), CommandDef::Script("if test -f .acdd/signal-visited; then exit 0; fi; touch .acdd/signal-visited; kill -TERM $$".into()));
    profile.gates.insert(0, serde_json::from_value(serde_json::json!({"id":"signal_root","command":"signal_root","proof":"none","fail":"signal_root"})).unwrap());
    h.commit_profile(&profile, "signal root fixture");
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("WORKFLOW_COMMAND_INTERRUPTED: signal_root"),
        "{error:#}"
    );
    assert!(h.root().join(".acdd/signal-visited").exists());
}

#[test]
fn plan_unbounded_timeout_stops_without_retry() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    root.write(
        "profiles/plan.yaml",
        r#"version: 2
inherit: none
save: snapshot
commands:
  slow:
    command: "if test -f timeout-visited; then exit 0; fi; touch timeout-visited; sleep 30"
    timeout_seconds: 1
gates:
  - tasks:
      gates:
        - id: slow
          command: slow
          fail: slow
        - id: finish
          receipt: true
          save: snapshot
"#,
    );
    let started = std::time::Instant::now();
    let error = run_cli(root.root(), &["plan", "run", "linear", "--in-place"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_COMMAND_TIMEOUT: slow"),
        "{error:#}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(root.path("timeout-visited").exists());
}

#[test]
fn workflow_agent_gate_requires_a_structured_decision_runner() {
    let root = WorkflowHarness::rejection_routing().materialize();
    let error = run_cli(root.root(), &["workflow", "run", "rejection_routing"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_RUNNER_REQUIRED"),
        "{error:#}"
    );
    assert!(!root.path(".acdd").exists());
}

#[cfg(unix)]
#[test]
fn workflow_discovery_rejects_escaping_symlinks() {
    use std::os::unix::fs::symlink;
    let root = WorkflowHarness::ordered().materialize();
    let outside = WorkflowHarness::ordered().materialize();
    symlink(
        outside.path("docs/workflows/ordered.yaml"),
        root.path("docs/workflows/escape.yaml"),
    )
    .unwrap();
    let error = run_cli(root.root(), &["workflow", "list"]).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_PATH_INVALID"),
        "{error:#}"
    );
    assert!(!root.path("workflow-order.log").exists());
}

#[test]
fn workflow_run_executes_root_gates_without_task_state() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    let parsed = Cli::try_parse_from([
        "contextunity-forge-mcp",
        "--root",
        root.root().to_str().unwrap(),
        "--format",
        "json",
        "workflow",
        "run",
        "ordered",
    ]);
    assert!(
        parsed.is_ok(),
        "assertion failed: workflow run must enter production CLI dispatch: {:?}",
        parsed.as_ref().err()
    );
    let mut output = Vec::new();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(Cli::run_with_writer(parsed.unwrap(), &mut output));
    assert!(result.is_ok(), "taskless root execution: {result:?}");
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("workflow-order.log")).unwrap(),
        "first\nsecond\n"
    );
    assert!(!root.path(".git").exists());
    assert!(
        !root.path(".acdd").exists(),
        "save:none must remain ephemeral"
    );
    assert_eq!(
        fs::read_to_string(root.path("docs/workflows/ordered.yaml")).unwrap(),
        include_str!("../fixtures/workflows/ordered.yaml")
    );
}

#[test]
fn workflow_run_forwards_explicit_goal_to_role_prompts_and_rejects_blank_goal() {
    let harness = WorkflowHarness::agents();
    let root = harness.materialize();

    let blank_err =
        run_cli(root.root(), &["workflow", "run", "agents", "--goal", "   "]).unwrap_err();
    assert!(
        blank_err.to_string().contains("WORKFLOW_GOAL_INVALID"),
        "{blank_err:#}"
    );
    assert!(!root.path("workflow-role-sessions.jsonl").exists());

    let explicit_goal = "Author rename milestone for contextunity-acdd";
    let value = run_cli(
        root.root(),
        &["workflow", "run", "agents", "--goal", explicit_goal],
    )
    .unwrap();
    assert_eq!(value["status"], "completed");

    let log = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    let entries: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(entries.len() >= 2);
    assert!(entries.iter().all(|entry| entry["goal"] == explicit_goal));
}

#[test]
fn workflow_discovery_does_not_execute_gates() {
    let harness = WorkflowHarness::ordered();
    let root = harness.materialize();
    for action in [
        vec!["workflow", "list"],
        vec!["workflow", "show", "ordered"],
    ] {
        let parsed = Cli::try_parse_from(
            [
                "contextunity-forge-mcp",
                "--root",
                root.root().to_str().unwrap(),
                "--format",
                "json",
            ]
            .into_iter()
            .chain(action),
        );
        assert!(
            parsed.is_ok(),
            "workflow discovery dispatch: {:?}",
            parsed.as_ref().err()
        );
        let mut output = Vec::new();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(Cli::run_with_writer(parsed.unwrap(), &mut output));
        assert!(result.is_ok(), "workflow discovery: {result:?}");
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert!(value.to_string().contains("ordered"));
        assert!(!root.path("workflow-order.log").exists());
        assert!(!root.path(".acdd").exists());
        assert!(!root.path(".git").exists());
    }
}

#[test]
fn plan_run_executes_steps_in_place_without_committing() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let head = git_head(root.root());
    let snapshots = harness.snapshots(&root);
    let plan_profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"));
    assert!(
        plan_profile.is_ok(),
        "snapshot-only plan profile: {plan_profile:?}"
    );
    let parsed = Cli::try_parse_from([
        "contextunity-forge-mcp",
        "--root",
        root.root().to_str().unwrap(),
        "--format",
        "json",
        "plan",
        "run",
        "linear",
        "--in-place",
    ]);
    assert!(
        parsed.is_ok(),
        "plan run dispatch: {:?}",
        parsed.as_ref().err()
    );
    let mut output = Vec::new();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(Cli::run_with_writer(parsed.unwrap(), &mut output));
    assert!(result.is_ok(), "in-place plan execution: {result:?}");
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\nstep\n"
    );
    assert_eq!(
        git_head(root.root()),
        head,
        "plan snapshots must not commit HEAD"
    );
    assert!(
        harness
            .snapshots(&root)
            .iter()
            .any(|snapshot| snapshot.starts_with("commit ") && !snapshots.contains(snapshot)),
        "plan execution must retain a new Git snapshot commit"
    );
    assert!(!root.path(".worktrees").exists());
    assert!(!root.path("rejected-profile.log").exists());
}

#[test]
fn plan_discovery_preserves_canonical_step_dependencies() {
    let harness = PlanHarness::dag();
    let root = harness.materialize(harness.path);
    let head = git_head(root.root());
    let source = fs::read(root.path(harness.path)).unwrap();
    for action in [vec!["plan", "list"], vec!["plan", "show", "dag"]] {
        let show = action[1] == "show";
        let parsed = Cli::try_parse_from(
            [
                "contextunity-forge-mcp",
                "--root",
                root.root().to_str().unwrap(),
                "--format",
                "json",
            ]
            .into_iter()
            .chain(action),
        );
        assert!(
            parsed.is_ok(),
            "plan discovery dispatch: {:?}",
            parsed.as_ref().err()
        );
        let mut output = Vec::new();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(Cli::run_with_writer(parsed.unwrap(), &mut output));
        assert!(result.is_ok(), "plan discovery: {result:?}");
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert!(value.to_string().contains("dag"));
        if show {
            let steps = value["steps"]
                .as_array()
                .expect("plan show exposes structured steps");
            for title in ["Step B: Extension Beta", "Step C: Extension Gamma"] {
                let step = steps
                    .iter()
                    .find(|step| {
                        step["title"]
                            .as_str()
                            .is_some_and(|name| name.contains(title))
                    })
                    .expect("canonical dependent step");
                assert!(
                    step["depends_on"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|dependency| dependency
                            .as_str()
                            .is_some_and(|name| name.contains("Step A: Independent Core"))),
                    "{title} must retain its dependency"
                );
            }
        }
        assert_eq!(fs::read(root.path(harness.path)).unwrap(), source);
        assert_eq!(git_head(root.root()), head);
        assert!(!root.path("plan-execution.log").exists());
    }
}

#[test]
fn plan_run_honors_explicit_config_and_profile_paths() {
    let harness = PlanHarness::linear();
    let root = harness.materialize("custom-plans/linear.md");
    let head = git_head(root.root());
    let snapshots = harness.snapshots(&root);
    let parsed = Cli::try_parse_from([
        "contextunity-forge-mcp",
        "--root",
        root.root().to_str().unwrap(),
        "--db",
        ".acdd/selected-index.sqlite",
        "--config",
        "configs/custom.yaml",
        "--profile",
        "profiles/plan.yaml",
        "--format",
        "json",
        "plan",
        "run",
        "linear",
        "--in-place",
    ]);
    assert!(
        parsed.is_ok(),
        "global path dispatch: {:?}",
        parsed.as_ref().err()
    );
    let mut output = Vec::new();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(Cli::run_with_writer(parsed.unwrap(), &mut output));
    assert!(
        result.is_ok(),
        "explicit config/profile execution: {result:?}"
    );
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "completed");
    assert_eq!(value["path"], "custom-plans/linear.md");
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\nstep\n"
    );
    assert!(
        !root.path("rejected-profile.log").exists(),
        "explicit profile must override selected config"
    );
    assert!(!root.path(".acdd/tasks.sqlite").exists());
    assert!(!root.path(".acdd/custom-tasks.sqlite").exists());
    assert_eq!(git_head(root.root()), head);
    assert!(harness
        .snapshots(&root)
        .iter()
        .any(|snapshot| snapshot.starts_with("commit ") && !snapshots.contains(snapshot)));
}

#[test]
fn milestone_root_gate_claim_and_submit_enforce_barriers_independence_and_fanout() {
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };

    let mut h = harness::WaveHarness::with_single_task(true);
    h.with_blueprint_synthesis_root_gates();

    let worktree = h.root().to_string_lossy().into_owned();
    let revisions_before_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_revisions", [], |row| row.get(0))
        .unwrap();
    let transitions_before_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let next_action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("architect-1".into()),
            worktree: Some(worktree.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(next_action["subject"], "milestone");
    assert_eq!(next_action["stage"], "blueprint_synthesis.synthesize");
    let revisions_after_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_revisions", [], |row| row.get(0))
        .unwrap();
    let transitions_after_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        revisions_after_next, revisions_before_next,
        "pre-task next dispatch must not enroll task revisions"
    );
    assert_eq!(
        transitions_after_next, transitions_before_next,
        "pre-task next dispatch must not mutate gate_transitions"
    );

    let pre_claim = claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: String::new(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(pre_claim["subject"], "milestone");
    assert_eq!(pre_claim["stage"], "blueprint_synthesis.synthesize");
    let milestone_id = pre_claim["milestone_id"].as_str().unwrap().to_owned();

    // Post-task gate cannot be claimed while pre-task barrier or tasks are incomplete.
    let barrier_err = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        barrier_err.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{barrier_err}"
    );

    let task_ref = h.task.store.inspect(&h.task.id).unwrap().spec.task_ref;
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            task_ref: {
                                "goal": "root-fanout-ready"
                            }
                        }
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    // Independent reviewer cannot reuse architect-1 on either claim or submit.
    let claim_not_independent = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        claim_not_independent.contains("TASK_REVIEW_NOT_INDEPENDENT"),
        "{claim_not_independent}"
    );

    let not_independent = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        not_independent.contains("TASK_REVIEW_NOT_INDEPENDENT"),
        "{not_independent}"
    );

    // Passing final pre-task gate eagerly fans out task slices before any context call.
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-2",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    let fanned_out_rows: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions WHERE milestone_id=?1 AND subject=?2 AND action='advance'",
            rusqlite::params![milestone_id, h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fanned_out_rows, 1);

    let total_before_context: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let ctx = manage(
        h.root(),
        Manage {
            action: ManageAction::Context,
            task_id: Some(h.task.id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let total_after_context: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(total_before_context, total_after_context);
    assert_eq!(
        ctx["context_bundle"]["inherited_proofs"][0]["proof"]["goal"],
        "root-fanout-ready"
    );

    // Barrier 2 rejects post-task root claim until wave task completes, then allows it.
    let barrier2_err = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        barrier2_err.contains("MILESTONE_POST_TASK_BARRIER_OPEN"),
        "{barrier2_err}"
    );

    h.task.advance_to_completion().unwrap();
    let post_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(post_claim["stage"], "polish");
    let post_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "polish".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(post_submit["action"], "pass");
}

#[test]
fn root_command_verify_blocks_unobserved_pass_and_bypassed_polish_allows_independent_milestone_review(
) {
    use crate::acdd::support::passing_review_proof;
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };

    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile.commands.insert(
        "root-verify-pass".into(),
        serde_json::from_value(serde_json::json!({
            "command": "true",
            "output": { "require_counts": false }
        }))
        .unwrap(),
    );
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let verify: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "verify",
        "command": "root-verify-pass",
        "proof": "command",
        "pass": "milestone_review",
        "fail": "polish"
    }))
    .unwrap();
    let polish: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "polish",
        "role": "wave_role",
        "proof": "none",
        "pass": "milestone_review"
    }))
    .unwrap();
    let milestone_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "milestone_review",
        "role": "wave_role",
        "proof": "review",
        "independent_from": "polish"
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![task_container, verify, polish, milestone_review, handoff];
    h.commit_profile(&profile, "verified review root gate profile");
    h.task.advance_to_completion().unwrap();
    let milestone_id = "m-wave".to_owned();
    let worktree = h.root().to_string_lossy().into_owned();

    let verify_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(verify_claim["stage"], "verify");

    let unobserved_err = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "command_proof": {
                        "command": "root-verify-pass",
                        "exit_code": 0
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        unobserved_err.contains("TASK_PROOF_UNOBSERVED"),
        "{unobserved_err}"
    );

    let verified = manage(
        h.root(),
        Manage {
            action: ManageAction::Verify,
            task_id: Some(milestone_id.clone()),
            worktree: Some(worktree.clone()),
            worker_id: Some("architect-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(verified["passed"], true);

    let verify_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": verified["proof"].clone()
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(verify_submit["action"], "pass");

    // Passing `verify` routes directly to `milestone_review`, bypassing `polish`,
    // so `independent_from: polish` must still allow claiming `milestone_review`.
    let review_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "milestone_review".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_claim["stage"], "milestone_review");

    let review_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "milestone_review".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": passing_review_proof()
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_submit["action"], "pass");
}

#[test]
fn root_immutable_gate_rejects_modified_candidate_after_claim() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let preparation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let preparation_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation_review",
        "role": "wave_role",
        "proof": "none",
        "independent_from": "preparation",
        "save": { "immutable": true }
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![preparation, preparation_review, task_container, handoff];
    h.commit_profile(&profile, "immutable root gate profile");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "preparation".into(),
            worker_id: "worker-a".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "preparation".into(),
            evidence: serde_json::json!({
                "worker_id": "worker-a",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "preparation_review".into(),
            worker_id: "reviewer-b".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn modified() -> bool { false }\n",
    )
    .unwrap();

    let err = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "preparation_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-b",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("TASK_CANDIDATE_MISMATCH"), "{err}");
}

#[test]
fn claim_response_overflow_releases_claim_and_keeps_task_ready() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = harness::WaveHarness::with_single_task(true);
    h.with_blueprint_synthesis_root_gates();
    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.synthesize".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            "first": { "padding": "A".repeat(2048) }
                        }
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            worker_id: "reviewer-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-1",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    // Lower configured response budget below the inherited proof size to force TASK_RESPONSE_OVERFLOW.
    let cfg_path = h.root().join("acdd.yaml");
    let mut cfg = fs::read_to_string(&cfg_path).unwrap();
    cfg.push_str("\nresponse:\n  max_output_bytes: 1024\n");
    fs::write(&cfg_path, cfg).unwrap();

    let first_err = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: "contract".into(),
            worker_id: "worker-1".into(),
            worktree: worktree.clone(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(first_err.contains("TASK_RESPONSE_OVERFLOW"), "{first_err}");

    let task = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(task.status, "ready");
    assert!(task.worker_id.is_none());

    let second_err = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: "contract".into(),
            worker_id: "worker-2".into(),
            worktree,
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        second_err.contains("TASK_RESPONSE_OVERFLOW"),
        "{second_err}"
    );
}

#[test]
fn root_immutable_gate_enforces_prior_snapshot_across_intervening_ordinary_pass() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let preparation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let ordinary_gate: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "ordinary_gate",
        "role": "wave_role",
        "proof": "none"
    }))
    .unwrap();
    let preparation_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation_review",
        "role": "wave_role",
        "proof": "none",
        "independent_from": "preparation",
        "save": { "immutable": true }
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![
        preparation,
        ordinary_gate,
        preparation_review,
        task_container,
        handoff,
    ];
    h.commit_profile(&profile, "intervening pass before immutable root gate");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    for (stage, worker) in [("preparation", "worker-a"), ("ordinary_gate", "worker-b")] {
        claim(
            h.root(),
            Claim {
                task_id: milestone_id.clone(),
                stage: stage.into(),
                worker_id: worker.into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
        submit(
            h.root(),
            Submit {
                task_id: milestone_id.clone(),
                stage: stage.into(),
                evidence: serde_json::json!({
                    "worker_id": worker,
                    "worktree": worktree,
                    "proof": { "none_proof": {} }
                }),
                action: Action::Pass,
                findings: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
    }

    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn altered() -> bool { false }\n",
    )
    .unwrap();

    let err = claim(
        h.root(),
        Claim {
            task_id: milestone_id,
            stage: "preparation_review".into(),
            worker_id: "reviewer-c".into(),
            worktree,
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("TASK_CANDIDATE_MISMATCH"), "{err}");
}

#[test]
fn task_pinned_inherit_none_profile_cannot_bypass_milestone_root_barriers() {
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };
    use sha2::Digest;

    let h = harness::WaveHarness::with_single_task(true);
    let milestone_yaml = "\
inherit: none
version: 2
roles:
  actor: {mode: subagent}
gates:
  - id: preparation
    role: actor
    proof:
      scheme:
        type: object
        required: [tasks]
        properties:
          tasks:
            type: object
  - tasks:
      gates:
        - id: build
          role: actor
          proof: none
        - id: deliver
          role: actor
          proof: delivery
          receipt: true
          auto_commit: false
  - id: handoff
    receipt: true
    proof: delivery
";
    let task_yaml = "\
inherit: none
version: 2
roles:
  actor: {mode: subagent}
gates:
  - tasks:
      gates:
        - id: build
          role: actor
          proof: none
        - id: deliver
          role: actor
          proof: delivery
          receipt: true
          auto_commit: false
";
    let pin = |path: &str, yaml: &str| {
        let digest = format!("{:x}", sha2::Sha256::digest(yaml.as_bytes()));
        format!("{path}:{}", &digest[..12])
    };
    let milestone_pin = pin("profiles/milestone.yaml", milestone_yaml);
    let task_pin = pin("profiles/task.yaml", task_yaml);
    fs::create_dir_all(h.root().join("profiles")).unwrap();
    fs::write(h.root().join("profiles/milestone.yaml"), milestone_yaml).unwrap();
    fs::write(h.root().join("profiles/task.yaml"), task_yaml).unwrap();

    let manifest_rel = "docs/milestones/001-pinned-barrier.md";
    let spec = format!(
        "---\nid: m-pinned-barrier\ntitle: Pinned Barrier\ndoc_type: contract\nstatus: active\nacdd_profile: \"{milestone_pin}\"\n---\n# Pinned Barrier\n```yaml\ntask_ref: t1\ntarget: Task with inherit none pin\nacdd_profile: \"{task_pin}\"\nproof_policy: seam-test-first\nscope: [src/]\n```\n"
    );
    fs::write(h.root().join(manifest_rel), spec).unwrap();
    fs::remove_file(h.root().join(harness::MANIFEST)).unwrap();
    fs::remove_file(h.root().join(".acdd/tasks.sqlite")).unwrap();
    let cfg = fs::read_to_string(h.root().join("acdd.yaml")).unwrap();
    let cleaned_cfg = cfg
        .lines()
        .filter(|line| !line.starts_with("acdd_profile:"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(h.root().join("acdd.yaml"), format!("{cleaned_cfg}\n")).unwrap();
    for args in [
        vec!["rm", harness::MANIFEST],
        vec!["add", "acdd.yaml", "profiles", manifest_rel],
        vec!["commit", "-m", "pinned milestone and task profiles"],
    ] {
        assert!(git_command(h.root(), &args).status().unwrap().success());
    }

    let synced = manage(
        h.root(),
        Manage {
            action: ManageAction::Sync,
            milestone_ref: Some(manifest_rel.into()),
            ..Default::default()
        },
    )
    .unwrap();
    let task_id = synced["tasks"][0]["task_id"].as_str().unwrap().to_owned();
    let worktree = h.root().to_string_lossy().into_owned();

    let next_before = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("coord-1".into()),
            worktree: Some(worktree.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(next_before["subject"], "milestone");
    assert_eq!(next_before["stage"], "preparation");

    let early_claim_err = claim(
        h.root(),
        Claim {
            task_id: task_id.clone(),
            stage: "build".into(),
            worker_id: "worker-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        early_claim_err.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{early_claim_err}"
    );

    claim(
        h.root(),
        Claim {
            task_id: "m-pinned-barrier".into(),
            stage: "preparation".into(),
            worker_id: "coord-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: "m-pinned-barrier".into(),
            stage: "preparation".into(),
            evidence: serde_json::json!({
                "worker_id": "coord-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            "t1": { "summary": "pinned blueprint slice" }
                        }
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    let allowed = claim(
        h.root(),
        Claim {
            task_id,
            stage: "build".into(),
            worker_id: "worker-1".into(),
            worktree,
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(allowed["stage"], "build");
    let inherited = allowed["context_bundle"]["inherited_proofs"]
        .as_array()
        .unwrap();
    assert_eq!(inherited.len(), 1, "{allowed:?}");
    assert_eq!(inherited[0]["gate_path"], "preparation");
    assert_eq!(inherited[0]["proof"]["summary"], "pinned blueprint slice");
}

#[test]
fn post_task_immutable_review_ignores_pre_task_snapshot_and_cli_verifies_root_gate() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let blueprint: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "blueprint",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let mut verify = profile.find_gate("verify").unwrap().clone();
    verify.on_success = Some("milestone_review".into());
    verify.on_fail = None;
    let mut milestone_review = profile.find_gate("milestone_review").unwrap().clone();
    milestone_review.independent_from = None;
    milestone_review.on_fail = None;
    milestone_review.persistence.immutable = true;
    let handoff = profile.find_gate("handoff").unwrap().clone();
    profile.gates = vec![blueprint, task_container, verify, milestone_review, handoff];
    h.commit_profile(&profile, "blueprint snapshot to post-task review profile");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    h.task.advance_to_completion().unwrap();

    let verified = run_cli(h.root(), &["task", "verify", "m-wave"]).unwrap();
    assert!(
        verified["milestone"].as_str().unwrap().ends_with("/m-wave"),
        "{verified:?}"
    );
    assert_eq!(verified["stage"], "verify");
    assert_eq!(verified["passed"], true);

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            worker_id: "coord-2".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "coord-2",
                "worktree": worktree,
                "proof": {
                    "command_proof": {
                        "command": "check_full_repo",
                        "exit_code": 0,
                        "tests_passed": 1,
                        "tests_failed": 0
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "milestone_review".into(),
            worker_id: "reviewer-9".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    let review_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "milestone_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-9",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "decision": "pass",
                        "summary": "verified post-task candidate without pre-task snapshot collision"
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_submit["action"], "pass");
}
