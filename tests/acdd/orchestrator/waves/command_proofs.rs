use super::harness;
use super::root_fixture::RootGateHarness;
use super::run_cli;
use crate::acdd::harness::plans::PlanHarness;
use crate::acdd::harness::workflows::WorkflowHarness;
use contextunity_acdd::acdd_tasks::tasks::profile;
use std::fs;

#[test]
fn observed_command_catalog_matrix() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    let mut h = harness::WaveHarness::with_single_task(true);
    let baseline = h.checkpoint();
    struct Case {
        name: &'static str,
        domain: &'static str,
        aggregate: bool,
        post: bool,
    }
    let cases = [
        Case {
            name: "workflow_rejects_executed_command_that_violates_proof_catalog",
            domain: "workflow",
            aggregate: false,
            post: false,
        },
        Case {
            name: "plan_rejects_executed_command_that_violates_proof_catalog",
            domain: "plan",
            aggregate: false,
            post: false,
        },
        Case {
            name: "milestone_rejects_executed_command_that_violates_proof_catalog",
            domain: "milestone",
            aggregate: false,
            post: false,
        },
        Case {
            name: "post_task_milestone_catalog_rejection_does_not_persist_pass",
            domain: "milestone",
            aggregate: false,
            post: true,
        },
        Case {
            name: "milestone_command_catalog_checks_aggregate_counts",
            domain: "milestone",
            aggregate: true,
            post: false,
        },
    ];
    for case in cases {
        if case.domain != "milestone" {
            let (path, source, args) = if case.domain == "workflow" {
                (
                    "docs/workflows/ordered.yaml",
                    include_str!("../../fixtures/workflows/catalog-invalid.yaml"),
                    vec!["workflow", "run", "ordered"],
                )
            } else {
                (
                    "profiles/plan.yaml",
                    include_str!("../../fixtures/plans/catalog-invalid.yaml"),
                    vec!["plan", "run", "linear", "--in-place"],
                )
            };
            root.write(path, source);
            let error = run_cli(root.root(), &args).unwrap_err();
            assert!(
                format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
                "{}: {error:#}",
                case.name
            );
            continue;
        }
        h.restore(&baseline);
        let mut selected = h.read_profile();
        let schema = if case.post {
            serde_json::json!({"type":"object","properties":{"command":{"type":"string","const":"rust-check-and-test"}}})
        } else {
            serde_json::json!({"type":"object","properties":{"tests_passed":{"type":"integer","const":2}}})
        };
        selected.proofs.insert(
            "command".into(),
            serde_json::from_value(serde_json::json!({"schema":schema})).unwrap(),
        );
        if case.aggregate {
            fs::write(
                h.root().join("docs/milestones/011-wave.md"),
                crate::acdd::support::MilestoneFixtureBuilder::new("m-wave")
                    .task("first", &["src/lib.rs"])
                    .task_with_deps("second", &["src/second.rs"], &["first"])
                    .build(),
            )
            .unwrap();
        }
        let command = if case.aggregate {
            serde_json::json!({"by_pattern":{"src/lib.rs":"echo '1 passed; 0 failed' # first","src/second.rs":"echo '1 passed; 0 failed' # second"},"output":{"counts":{"passed":"(\\d+) passed","failed":"(\\d+) failed"}}})
        } else {
            serde_json::json!({"command":"echo '1 passed; 0 failed'","output":{"counts":{"passed":"(\\d+) passed","failed":"(\\d+) failed"}}})
        };
        selected.commands.insert(
            "catalog-check".into(),
            serde_json::from_value(command).unwrap(),
        );
        let stage = if case.post { "verify" } else { "catalog_check" };
        if case.post {
            selected.find_gate_mut(stage).unwrap().command = Some("catalog-check".into());
        } else {
            selected.gates.insert(0,serde_json::from_value(serde_json::json!({"id":stage,"command":"catalog-check","proof":"command","max_attempts":1})).unwrap());
            if case.aggregate {
                selected.commands.insert(
                    "stop-root".into(),
                    profile::CommandDef::Script("exit 1".into()),
                );
                selected.gates.insert(1,serde_json::from_value(serde_json::json!({"id":"stop_root","command":"stop-root","proof":"none","max_attempts":1})).unwrap());
            }
        }
        h.commit_profile(&selected, case.name);
        let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
        if !case.aggregate {
            assert!(
                format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
                "{error:#}"
            );
        }
        let passes:i64=h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path=?1 AND action='pass'",[stage],|row|row.get(0)).unwrap();
        assert_eq!(passes, i64::from(case.aggregate), "{}", case.name);
        let traces:i64=h.task.store.connection.query_row("SELECT count(*) FROM gate_execution_traces WHERE subject='milestone' AND gate_path=?1 AND command_id='catalog-check' AND status='completed'",[stage],|row|row.get(0)).unwrap();
        if case.post {
            assert_eq!(traces, if case.aggregate { 2 } else { 1 }, "{}", case.name);
        }
    }
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
fn command_preflight_rejection_matrix() {
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
            serde_json::json!("contract"),
            Some("commands registry names and commands must be non-empty"),
        ),
        (
            "{ command: 'touch forbidden-command', output: { seam_ref_pattern: '[' } }",
            serde_json::json!({"command": "touch forbidden-command", "output": {"seam_ref_pattern": "["}}),
            false,
            serde_json::json!("contract"),
            Some("invalid output.seam_ref_pattern regex"),
        ),
        (
            "'touch forbidden-command'",
            serde_json::json!("touch forbidden-command"),
            true,
            serde_json::json!("contract"),
            Some("expected_failure.exit_codes must contain non-zero values"),
        ),
        (
            "'touch forbidden-command'",
            serde_json::json!("touch forbidden-command"),
            false,
            serde_json::json!("review"),
            None,
        ),
        (
            "'touch forbidden-command'",
            serde_json::json!("touch forbidden-command"),
            false,
            serde_json::json!({"scheme":{"type":"object"}}),
            None,
        ),
    ];
    for (command, json_command, invalid_exit, proof, expected_error) in cases {
        let proof_yaml = serde_json::to_string(&proof).unwrap();
        let mut pure = serde_json::json!({"version":2,"inherit":"none","save":"none","commands":{"check":json_command},"gates":[{"id":"check","command":"check","proof":proof}]});
        if invalid_exit {
            pure["gates"][0]["expected_failure"] = serde_json::json!({"exit_codes":[0]});
        }
        if let Some(expected) = expected_error {
            let error = profile::validate_profile_document(&serde_yaml::to_string(&pure).unwrap())
                .unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
        for domain in ["workflow", "plan", "milestone"] {
            let error = match domain {
                "workflow" => {
                    let expected = if invalid_exit {
                        "    expected_failure: { exit_codes: [0] }\n"
                    } else {
                        ""
                    };
                    workflow.write("docs/workflows/ordered.yaml", &format!("version: 2\ninherit: none\nsave: none\ncommands:\n  check: {command}\ngates:\n  - id: check\n    command: check\n    proof: {proof_yaml}\n{expected}"));
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
                    plan.write("profiles/plan.yaml", &format!("version: 2\ninherit: none\nsave: snapshot\ncommands:\n  check: {command}\ngates:\n  - tasks:\n      gates:\n        - id: check\n          command: check\n          proof: {proof_yaml}\n{expected}        - id: finish\n          receipt: true\n          save: snapshot\n"));
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
                    let mut gate = serde_json::json!({"id": "check_invalid", "command": "check-invalid", "proof": proof});
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
            if let Some(expected) = expected_error {
                assert!(
                    format!("{error:#}").contains(expected),
                    "{domain}: {error:#}"
                );
            } else if domain == "milestone" {
                assert!(
                    format!("{error:#}").contains(
                        "MILESTONE_PROOF_REQUIRED: command execution cannot supply this proof"
                    ),
                    "{error:#}"
                );
            }
        }
    }
}

#[test]
fn observed_command_counts_matrix() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let plan = PlanHarness::linear();
    let root = plan.materialize(plan.path);
    let mut h = harness::WaveHarness::with_single_task(true);
    let baseline = h.checkpoint();
    struct Case {
        name: &'static str,
        domain: &'static str,
        optional: bool,
        post: bool,
    }
    for case in [
        Case {
            name: "milestone_root_command_without_required_counts_fails_without_retrying_forever",
            domain: "milestone",
            optional: false,
            post: false,
        },
        Case {
            name: "workflow_command_without_required_counts_fails_without_retrying_forever",
            domain: "workflow",
            optional: false,
            post: false,
        },
        Case {
            name: "workflow_command_can_explicitly_opt_out_of_counts",
            domain: "workflow",
            optional: true,
            post: false,
        },
        Case {
            name: "plan_command_without_required_counts_fails_without_retrying_forever",
            domain: "plan",
            optional: false,
            post: false,
        },
        Case {
            name: "post_task_milestone_verification_accepts_absent_counts_when_optional",
            domain: "milestone",
            optional: true,
            post: true,
        },
    ] {
        let command = serde_json::json!({"command":"echo visited >> .acdd/countless-visits","output":{"require_counts":!case.optional}});
        let gate = serde_json::json!({"id":"countless","command":"countless","proof":"command","max_attempts":1});
        let result = if case.domain == "milestone" {
            h.restore(&baseline);
            let mut selected = h.read_profile();
            selected
                .commands
                .insert("countless".into(), serde_json::from_value(command).unwrap());
            if case.post {
                selected.find_gate_mut("verify").unwrap().command = Some("countless".into());
            } else {
                selected
                    .gates
                    .insert(0, serde_json::from_value(gate).unwrap());
            }
            h.commit_profile(&selected, case.name);
            run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {})
        } else {
            if root.path(".acdd/countless-visits").exists() {
                root.remove(".acdd/countless-visits");
            }
            fs::create_dir_all(root.path(".acdd")).unwrap();
            let mut document = serde_json::json!({"version":2,"inherit":"none","save":"none","commands":{"countless":command},"gates":[gate.clone()]});
            let (path, args) = if case.domain == "workflow" {
                (
                    "docs/workflows/ordered.yaml",
                    vec!["workflow", "run", "ordered"],
                )
            } else {
                document["save"] = serde_json::json!("snapshot");
                document["gates"] = serde_json::json!([{"tasks":{"gates":[gate,{"id":"finish","receipt":true,"save":"snapshot"}]}}]);
                (
                    "profiles/plan.yaml",
                    vec!["plan", "run", "linear", "--in-place"],
                )
            };
            root.write(path, &serde_yaml::to_string(&document).unwrap());
            run_cli(root.root(), &args)
        };
        assert_eq!(result.is_ok(), case.optional, "{}: {result:#?}", case.name);
        let path = if case.domain == "milestone" {
            h.root().join(".acdd/countless-visits")
        } else {
            root.path(".acdd/countless-visits")
        };
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            if case.post {
                "visited\nvisited\n"
            } else {
                "visited\n"
            },
            "{}",
            case.name
        );
    }
}
