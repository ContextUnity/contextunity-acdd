#![cfg(target_os = "linux")]

use crate::acdd::{support::*, tasks::harness::TaskHarness};
use acdd_tasks::{
    engine::orchestrator::{
        spawner::{
            monitor_runner_process, render_role_prompt, spawn_role_runner, RunnerSessionPolicy,
        },
        task_runner::run_task_lifecycle,
        types::{OrchestratorEvent, RunnerInvocationContext},
    },
    tasks::profile::{GateDriver, ProofKind, RunnerConfig},
};
use std::{fs, process::Command, time::Duration};

mod harness;
use harness::{LifecycleHarness, MANIFEST};

#[test]
fn taskless_rejection_reuses_configured_role_sessions() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;

    let root = WorkflowHarness::agents().materialize();
    let mut profile: Value =
        serde_yaml::from_str(include_str!("../fixtures/workflows/agents.yaml")).unwrap();
    profile["gates"][0]["proof"] = profile["gates"][1]["proof"].clone();
    profile["gates"][0]["fail"] = json!("build");
    profile["gates"][0]["max_attempts"] = json!(4);
    profile["roles"]["builder"]["reuse_on_reject"] = json!(true);
    root.write(
        "docs/workflows/agents.yaml",
        &serde_yaml::to_string(&profile).unwrap(),
    );
    let runner = include_str!("../fixtures/workflows/runner.py")
        .replace("Path(\"review-rejected\")", "Path(gate + \"-rejected\")");
    root.write("workflow-runner.py", &runner);

    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
    assert!(result.is_ok(), "{result:#?}");
    let records: Vec<Value> = fs::read_to_string(root.path("workflow-role-sessions.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let builders: Vec<_> = records
        .iter()
        .filter(|record| record["gate"] == "build")
        .collect();
    let reviewers: Vec<_> = records
        .iter()
        .filter(|record| record["gate"] == "review")
        .collect();
    assert_eq!(builders.len(), 3, "{records:?}");
    assert_eq!(reviewers.len(), 2, "{records:?}");
    assert_eq!(builders[0]["session"], builders[1]["session"]);
    assert_eq!(builders[1]["session"], builders[2]["session"]);
    for reviewer in reviewers {
        assert_ne!(reviewer["session"], builders[1]["session"]);
    }
}

#[test]
fn workflow_reexecuted_rejected_producer_cannot_use_its_old_command_artifact() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;

    let root = WorkflowHarness::artifact_refresh().materialize();
    let source = include_str!("../fixtures/workflows/artifact-refresh.yaml")
        .replace(
            "{plan_name} {plan_slug} {artifact_path} {prerequisite}",
            "{artifact_path}",
        )
        .replace(
            "    proof: *artifact_proof\n",
            "    proof: *artifact_proof\n    command: check\n",
        )
        .replace("    fail: synthesize", "    fail: prerequisite");
    root.write("docs/workflows/artifact-refresh.yaml", &source);
    let runner = include_str!("../fixtures/workflows/artifact-runner.py")
        .replace("payload.update(prerequisite=\"prerequisite\")", "payload.update(prerequisite=\"prerequisite\")\n    if not Path(\"synthesized\").exists():\n        payload.update(artifact_path=\"bootstrap\")")
        .replace("payload.update(plan_slug=\"new-plan\", artifact_path=\"new/path\")", "payload.update(plan_slug=\"new-plan\")\n        decision = \"reject\"\n        Path(\"synth-rejected\").touch()");
    root.write("artifact-runner.py", &runner);
    let error = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap_err();
    assert!(root.path("synth-rejected").exists());
    assert!(
        format!("{error:#}").contains("WORKFLOW_COMMAND_UNRESOLVED"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(root.path("validated-arguments")).unwrap(),
        "bootstrap"
    );
}

#[test]
fn workflow_recently_accepted_producer_wins_collisions_in_execution_order() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;

    let root = WorkflowHarness::artifact_refresh().materialize();
    let source = include_str!("../fixtures/workflows/artifact-refresh.yaml")
        .replace("id: prerequisite", "id: z_prerequisite")
        .replace("id: synthesize", "id: a_synthesize")
        .replace("fail: synthesize", "fail: a_synthesize");
    root.write("docs/workflows/artifact-refresh.yaml", &source);
    let runner = include_str!("../fixtures/workflows/artifact-runner.py")
        .replace("gate == \"prerequisite\"", "gate == \"z_prerequisite\"")
        .replace("gate == \"synthesize\"", "gate == \"a_synthesize\"")
        .replace(
            "prerequisite=\"prerequisite\"",
            "prerequisite=\"prerequisite\", artifact_path=\"earlier/path\"",
        );
    root.write("artifact-runner.py", &runner);
    let result = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(
        fs::read_to_string(root.path("validated-arguments")).unwrap(),
        "new-plan new-plan new/path prerequisite"
    );
}

#[test]
fn workflow_repass_refreshes_artifacts_for_role_command() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::artifact_refresh().materialize();
    let result = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap();
    let gates: Vec<_> = result["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["gate"].as_str().unwrap())
        .collect();
    assert_eq!(
        gates,
        [
            "prerequisite",
            "synthesize",
            "review",
            "synthesize",
            "review",
            "preflight"
        ]
    );
    assert_eq!(
        fs::read_to_string(root.path("validated-arguments")).unwrap(),
        "new-plan new-plan new/path prerequisite"
    );
}

#[test]
fn workflow_repass_removes_old_producer_fields() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::artifact_refresh().materialize();
    let source = include_str!("../fixtures/workflows/artifact-refresh.yaml")
        .replace("{artifact_path}", "{removed}");
    root.write("docs/workflows/artifact-refresh.yaml", &source);
    let error = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_COMMAND_UNRESOLVED"),
        "{error:#}"
    );
    assert!(!root.path("validated-arguments").exists());
}

#[test]
fn workflow_role_command_rejects_unsafe_artifact_values() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::artifact_refresh().materialize();
    let runner =
        include_str!("../fixtures/workflows/artifact-runner.py").replace("new/path", "../unsafe");
    root.write("artifact-runner.py", &runner);
    let error = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_COMMAND_INVALID"),
        "{error:#}"
    );
    assert!(!root.path("validated-arguments").exists());
}

#[test]
fn workflow_rejected_producer_fields_are_unavailable() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::artifact_refresh().materialize();
    let source = include_str!("../fixtures/workflows/artifact-refresh.yaml").replace(
        "    proof: *artifact_proof\n",
        "    proof: *artifact_proof\n    fail: preflight\n    max_attempts: 2\n",
    );
    root.write("docs/workflows/artifact-refresh.yaml", &source);
    let runner = include_str!("../fixtures/workflows/artifact-runner.py").replace(
        "elif gate == \"review\":",
        "    decision = \"reject\"\nelif gate == \"review\":",
    );
    root.write("artifact-runner.py", &runner);
    let error = run_standalone_workflow(
        root.root(),
        Path::new("docs/workflows/artifact-refresh.yaml"),
        None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_COMMAND_UNRESOLVED"),
        "{error:#}"
    );
    assert!(!root.path("validated-arguments").exists());
}

#[test]
fn workflow_role_rejects_counts_not_observed_by_count_free_command() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    root.write(
        "docs/workflows/agents.yaml",
        include_str!("../fixtures/workflows/count-free-role.yaml"),
    );
    root.write(
        "root-proof.json",
        &json!({"command":"check","exit_code":0,"tests_passed":99,"tests_failed":0}).to_string(),
    );
    let error = run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
        "{error:#}"
    );
}

#[test]
fn workflow_role_seam_checks_observed_references_and_counts() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let command = "printf 'test suite::passes ... ok\\n1 passed; 0 failed\\n'";
    root.write("docs/workflows/agents.yaml", &format!(
        "version: 2\ninherit: none\nsave: none\nroles:\n  builder: {{ runner: \"python3 workflow-runner.py '{{prompt}}'\" }}\ncommands:\n  check:\n    command: {}\n    output:\n      counts: {{ passed: '(\\d+) passed', failed: '(\\d+) failed' }}\n      tests: {{ passed: 'test (\\S+) \\.\\.\\. ok' }}\ngates:\n  - id: build\n    role: builder\n    command: check\n    proof: seam_test\n    max_attempts: 1\n",
        serde_json::to_string(command).unwrap()
    ));
    for (reference, count, accepted) in [
        ("suite::passes", 1, true),
        ("suite::other", 1, false),
        ("suite::passes", 2, false),
    ] {
        root.write("root-proof.json", &json!({"exit_code":0,"seam_test_refs":[reference],"tests_passed":count,"tests_failed":0}).to_string());
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        if accepted {
            assert!(result.is_ok(), "{result:#?}");
        } else {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
                "{error:#}"
            );
        }
        assert!(root.path("workflow-role-sessions.jsonl").exists());
        fs::remove_file(root.path("workflow-role-sessions.jsonl")).unwrap();
    }
}

#[test]
fn workflow_role_contract_requires_observed_runtime_red() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    for (command, accepted) in [
        (
            "printf 'assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            true,
        ),
        ("exit 0 # seam:tests/acdd.rs::red", false),
        (
            "printf 'error[E1234]: assertion failed' >&2; exit 101 # seam:tests/acdd.rs::red",
            false,
        ),
    ] {
        root.write("docs/workflows/agents.yaml", &format!(
            "version: 2\ninherit: none\nsave: none\nroles:\n  builder: {{ runner: \"python3 workflow-runner.py '{{prompt}}'\" }}\ncommands:\n  check:\n    command: {}\n    output: {{ require_counts: false, seam_ref_pattern: 'seam:(\\S+)' }}\ngates:\n  - id: build\n    role: builder\n    command: check\n    proof: contract\n    expected_failure: {{ exit_codes: [101], stderr_contains: 'assertion failed' }}\n    max_attempts: 1\n",
            serde_json::to_string(command).unwrap()
        ));
        root.write(
            "root-proof.json",
            &json!({"seam_test_ref":"tests/acdd.rs::red","red_exit_code":101}).to_string(),
        );
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        if accepted {
            assert!(result.is_ok(), "{result:#?}");
        } else {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
                "{error:#}"
            );
        }
        assert!(root.path("workflow-role-sessions.jsonl").exists());
        fs::remove_file(root.path("workflow-role-sessions.jsonl")).unwrap();
    }
}

#[test]
fn claimed_role_process_submits_through_real_task_cli() {
    let mut h = LifecycleHarness::new();
    let mut starts = 0;
    loop {
        let gate = h
            .task
            .store
            .resolve_gate_for_task(&h.task.id, h.root().to_str().unwrap(), None)
            .unwrap();
        if gate.driver() != Some(GateDriver::Agent)
            || gate.proof.kind() == Some(ProofKind::Delivery)
        {
            break;
        }
        let claimed = h.task.claim_next().unwrap();
        let context =
            TaskHarness::manage_task(h.root(), &h.task.id, tasks::ManageAction::Context).unwrap();
        let profile = h.task.profile().unwrap();
        let role_id = gate.role.as_ref().unwrap();
        let role = &profile.roles[role_id];
        let invocation = RunnerInvocationContext {
            task_id: h.task.id.clone(),
            stage: claimed.stage.clone(),
            role: role_id.clone(),
            model: role
                .models
                .first()
                .map(|model| model.model.clone())
                .unwrap_or_default(),
            reasoning: role
                .models
                .first()
                .and_then(|model| model.reasoning.clone())
                .unwrap_or_default(),
            session_id: TaskHarness::session_for(claimed.worker_id.as_deref().unwrap()),
            prompt: render_role_prompt(&context)
                .unwrap_or_else(|error| panic!("role context: {error}: {context}")),
            worktree: h.root().to_owned(),
        };
        let config = RunnerConfig::from_role(role).unwrap();
        let process = spawn_role_runner(&config, &invocation, RunnerSessionPolicy::Resume).unwrap();
        let mut output = Vec::new();
        let outcome = monitor_runner_process(process, Duration::from_secs(20), &mut |_, bytes| {
            output.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            outcome.exit_code,
            Some(0),
            "{}: {}",
            claimed.stage,
            String::from_utf8_lossy(&output)
        );
        assert!(!outcome.timed_out);
        let after = h.task.store.inspect(&h.task.id).unwrap();
        assert_ne!(after.stage, claimed.stage);
        assert_eq!(after.status, "ready");
        assert!(!h.task.persisted_gate_evidence(&claimed.stage)["proof"].is_null());
        starts += 1;
    }
    assert!(starts > 0);
    assert_eq!(h.records().len(), starts);
}

#[test]
fn task_run_advances_profile_gates_and_records_delivery() {
    let h = LifecycleHarness::new();
    let mut events = Vec::new();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        events.push(event)
    });
    assert!(
        result.is_ok(),
        "assertion failed: configured task lifecycle must reach delivery: {result:?}"
    );
    let result = result.unwrap();
    let accepted = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(accepted.status, "completed");
    assert_eq!(result["status"], "completed");
    let receipt = accepted.receipt.unwrap();
    let delivered = receipt.commit.unwrap();
    assert_eq!(git_head(h.root()), delivered);
    assert!(delivered.starts_with(result["receipt"]["commit"].as_str().unwrap()));
    let (candidate, _) = h
        .task
        .store
        .build_snapshot_candidate(&h.task.id)
        .unwrap()
        .unwrap();
    let source = |commit: &str| {
        git_command(h.root(), &["show", &format!("{commit}:src/lib.rs")])
            .output()
            .unwrap()
    };
    let candidate_source = source(&candidate);
    let delivered_source = source(&delivered);
    assert!(candidate_source.status.success() && delivered_source.status.success());
    assert_eq!(candidate_source.stdout, delivered_source.stdout);
    let manifest = Milestone::parse(
        &fs::read_to_string(h.root().join(MANIFEST)).unwrap(),
        "forge-mcp",
    )
    .unwrap();
    assert_eq!(manifest.tasks[0].status.as_deref(), Some("completed"));
    assert_eq!(manifest.tasks[0].receipt.as_ref().unwrap().decision, "pass");
    assert!(!h.task.persisted_gate_proof("runtime-check")["command_proof"].is_null());
    let profile = h.task.profile().unwrap();
    let records = h.records();
    assert!(records.iter().any(|record| {
        let gate = profile.by_stage(record["stage"].as_str().unwrap()).unwrap();
        gate.command.is_some() && gate.driver() == Some(GateDriver::Agent)
    }));
    for record in &records {
        let gate = profile.by_stage(record["stage"].as_str().unwrap()).unwrap();
        assert_ne!(gate.proof.kind(), Some(ProofKind::Delivery));
        assert_eq!(record["session"], record["environment_session"]);
        assert_eq!(
            h.task
                .persisted_claim_session(record["claim_revision"].as_u64().unwrap()),
            record["session"].as_str().unwrap()
        );
    }
    let builder = records
        .iter()
        .find(|record| record["stage"] == "build")
        .unwrap();
    let reviewer = records
        .iter()
        .find(|record| record["stage"] == "review")
        .unwrap();
    assert_ne!(builder["session"], reviewer["session"]);
    assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::TaskDelivered { task_id, .. } if task_id == &h.task.id)));
}

#[test]
fn task_run_resumes_persisted_cursor_without_replaying_accepted_gates() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("build").unwrap();
    let prior = h.task.persisted_gate_evidence("contract");
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(result.is_ok(), "persisted cursor must resume: {result:?}");
    assert_eq!(h.task.persisted_gate_evidence("contract"), prior);
    assert!(h
        .records()
        .iter()
        .all(|record| record["stage"] != "contract"));
    let completed = h.task.store.inspect(&h.task.id).unwrap();
    let head = git_head(h.root());
    let records = h.records();
    let mut events = Vec::new();
    let repeated = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        events.push(event)
    });
    assert!(
        repeated.is_ok(),
        "completed task must be idempotent: {repeated:?}"
    );
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().claim_revision,
        completed.claim_revision
    );
    assert_eq!(git_head(h.root()), head);
    assert_eq!(h.records(), records);
    assert!(events.is_empty());
}

#[test]
fn task_run_routes_structured_rejection_to_retained_stage_session() {
    for (kind, target) in [("scope", "build"), ("symbols", "contract")] {
        let h = LifecycleHarness::new();
        let findings = json!({"kind":kind,"details":[{"message":"retain structured feedback {model}","path":"src/lib.rs"}]});
        h.write_control("reject.json", &findings);
        let mut events = Vec::new();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            events.push(event)
        });
        assert!(
            result.is_ok(),
            "{kind} rejection must repair and deliver: {result:?}; role records: {:?}",
            h.records()
        );
        let records = h.records();
        let target_runs: Vec<_> = records
            .iter()
            .filter(|record| record["stage"] == target)
            .collect();
        assert_eq!(target_runs.len(), 2, "{kind}: {records:?}");
        assert_eq!(target_runs[0]["worker_id"], target_runs[1]["worker_id"]);
        assert_eq!(target_runs[0]["session"], target_runs[1]["session"]);
        assert_eq!(target_runs[1]["rejection"]["findings"], findings);
        assert_eq!(target_runs[1]["rejection"]["rejected_from"], "review");
        let review_runs: Vec<_> = records
            .iter()
            .filter(|record| record["stage"] == "review")
            .collect();
        assert_eq!(review_runs.len(), 2);
        assert_ne!(review_runs[0]["session"], target_runs[1]["session"]);
        assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::GateRejected { findings: observed, .. } if observed == &findings)));
        assert_eq!(
            h.task.store.inspect(&h.task.id).unwrap().status,
            "completed"
        );
    }
}

#[test]
fn zero_test_command_observation_routes_to_repair_then_passes() {
    let h = LifecycleHarness::with_profile(|profile| {
        profile.commands.insert("zero-check".into(), serde_json::from_value(json!({
            "command": "if test -e .acdd/lifecycle/fail-check; then echo 'test result: ok. 0 passed; 0 failed'; else .acdd/runner_fixture; fi",
            "output": {"require_counts": true, "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
        })).unwrap());
        let gate = profile.find_gate_mut("runtime-check").unwrap();
        gate.command = Some("zero-check".into());
        gate.on_fail = Some("build".into());
    });
    fs::write(h.root().join(".acdd/lifecycle/fail-check"), "").unwrap();
    let mut rejected = false;
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        if matches!(event, OrchestratorEvent::GateRejected { .. }) {
            rejected = true;
            fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
        }
    });
    assert!(result.is_ok(), "zero-test repair must complete: {result:?}");
    assert!(rejected);
    assert_eq!(h.check_exits(), vec![Some(0), Some(0)]);
}

#[test]
fn multi_command_contract_rejects_later_green_then_repairs() {
    let h = LifecycleHarness::with_profile_and_scope(&["src/lib.rs", "src/other.md"], |profile| {
        profile.commands.insert("multi-contract".into(), serde_json::from_value(json!({
            "by_pattern": {
                "src/*.rs": ": --seam-ref tests/acdd/tasks.rs && ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture",
                "src/*.md": ": --seam-ref tests/acdd/tasks.rs && if test -e .acdd/lifecycle/fail-check; then .acdd/runner_fixture; else ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture; fi"
            },
            "output": {"require_counts": false, "seam_ref_pattern": "--seam-ref (\\S+)"}
        })).unwrap());
        let gate = profile.find_gate_mut("runtime-check").unwrap();
        gate.command = Some("multi-contract".into());
        gate.proof = serde_json::from_value(json!("contract")).unwrap();
        gate.expected_failure = Some(serde_json::from_value(json!({"exit_codes": [101]})).unwrap());
        gate.on_fail = Some("build".into());
    });
    fs::write(h.root().join("src/other.md"), "fixture\n").unwrap();
    fs::write(h.root().join(".acdd/lifecycle/fail-check"), "").unwrap();
    let mut rejected = false;
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        if matches!(event, OrchestratorEvent::GateRejected { .. }) {
            rejected = true;
            fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
        }
    });
    assert!(
        result.is_ok(),
        "multi-command contract repair must complete: {result:?}"
    );
    assert!(rejected);
    assert_eq!(
        h.check_exits(),
        vec![Some(101), Some(0), Some(101), Some(101)]
    );
}

#[test]
fn aggregate_only_seam_observation_routes_to_repair_and_new_pass() {
    let h = LifecycleHarness::with_profile(|profile| {
        profile.commands.insert("seam-repair".into(), serde_json::from_value(json!({
            "command": ": target=tests/runner_fixture.rs && if test -e .acdd/lifecycle/fail-check; then echo 'test result: ok. 1 passed; 0 failed'; else .acdd/runner_fixture; fi",
            "output": {
                "require_counts": true,
                "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"},
                "tests": {"target": "target=(tests/runner_fixture\\.rs)", "passed": "test\\s+([A-Za-z0-9_:]+)\\s+\\.\\.\\.\\s+ok"}
            }
        })).unwrap());
        let gate = profile.find_gate_mut("runtime-check").unwrap();
        gate.command = Some("seam-repair".into());
        gate.proof = serde_json::from_value(json!("seam_test")).unwrap();
        gate.on_fail = Some("build".into());
    });
    fs::write(h.root().join(".acdd/lifecycle/fail-check"), "").unwrap();
    let mut rejected = false;
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
        if matches!(event, OrchestratorEvent::GateRejected { .. }) {
            rejected = true;
            fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
        }
    });
    assert!(result.is_ok(), "seam repair must complete: {result:?}");
    assert!(rejected);
    assert_eq!(h.check_exits(), vec![Some(0), Some(0)]);
}

#[test]
fn failed_observation_is_not_reused_for_same_claim_after_repair() {
    let mut h = LifecycleHarness::with_repairable_check();
    h.task.advance_to("runtime-check").unwrap();
    h.task.claim_next().unwrap();
    let failed = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(failed["evidence"].is_null());
    assert_eq!(h.check_exits(), vec![Some(1)]);
    fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
    let passed = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(!passed["evidence"].is_null(), "{passed}");
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
    let cached = tasks::verify_task(h.root(), &h.task.id, None).unwrap();
    assert!(!cached["evidence"].is_null());
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
}

#[test]
fn command_and_contract_failures_route_to_repair_and_observe_new_passes() {
    let mut h = LifecycleHarness::with_repairable_check();
    let first = h.task.id.clone();
    let second = h.add_second_case();
    let cases = [
        (first, false, vec![Some(1), Some(0)]),
        (second, true, vec![Some(0), Some(101)]),
    ];
    for (task_id, contract, expected_exits) in cases {
        h.task.id = task_id;
        if contract {
            let path = h.root().join(".acdd/profile.yaml");
            let source = fs::read_to_string(&path).unwrap();
            let mut profile =
                acdd_tasks::tasks::profile::validate_profile_document(&source).unwrap();
            profile.commands.insert("repairable-check".into(), serde_json::from_value(json!({
                "command": ": --seam-ref tests/acdd/tasks.rs && if test -e .acdd/lifecycle/fail-check; then .acdd/runner_fixture; else ACDD_FIXTURE_RED=1 .acdd/runner_fixture --nocapture; fi",
                "output": {"require_counts": false, "seam_ref_pattern": "--seam-ref (\\S+)", "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"}}
            })).unwrap());
            let gate = profile.find_gate_mut("runtime-check").unwrap();
            gate.proof = serde_json::from_value(json!("contract")).unwrap();
            gate.expected_failure =
                Some(serde_json::from_value(json!({"exit_codes": [101]})).unwrap());
            fs::write(
                path,
                format!(
                    "inherit: none\n{}",
                    serde_yaml::to_string(&profile).unwrap()
                ),
            )
            .unwrap();
            for args in [
                &["add", "-f", ".acdd/profile.yaml"][..],
                &["commit", "-m", "contract fixture profile"][..],
            ] {
                assert!(git_command(h.root(), args).status().unwrap().success());
            }
            fs::write(h.root().join(".acdd/lifecycle/fail-check"), "").unwrap();
        }
        let previous = h.check_exits().len();
        let mut rejected = false;
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            if matches!(event, OrchestratorEvent::GateRejected { .. }) {
                rejected = true;
                fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
            }
        });
        assert!(
            result.is_ok(),
            "contract={contract}: repair must complete: {result:?}"
        );
        assert!(rejected, "contract={contract}");
        assert_eq!(
            &h.check_exits()[previous..],
            expected_exits,
            "contract={contract}"
        );
    }
}

#[test]
fn command_rejection_survives_store_reopen_before_repair() {
    let h = LifecycleHarness::with_repairable_check();
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            if matches!(event, OrchestratorEvent::GateRejected { .. }) {
                panic!("simulate coordinator restart after rejection");
            }
        });
    }));
    assert!(stopped.is_err());
    let reopened = tasks::store_for_task(h.root(), &h.task.id).unwrap();
    assert_eq!(reopened.inspect(&h.task.id).unwrap().status, "ready");
    assert_eq!(h.check_exits(), vec![Some(1)]);
    drop(reopened);
    fs::remove_file(h.root().join(".acdd/lifecycle/fail-check")).unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(
        result.is_ok(),
        "reopened rejection must recover: {result:?}"
    );
    assert_eq!(h.check_exits(), vec![Some(1), Some(0)]);
}

#[test]
fn task_run_recovers_first_self_rejection_for_polish_and_seam() {
    let mut h = LifecycleHarness::with_profile(|profile| {
        let executable = env!("CARGO_BIN_EXE_acdd").replace('\'', "'\\''");
        profile.roles.get_mut("builder").unwrap().runner = Some(format!(
            "python3 {{worktree}}/.acdd/lifecycle/seam_fixture.py '{executable}' {{session_id}} '{{prompt}}'"
        ));
        profile.commands.insert("seam-fixture".into(), serde_json::from_value(json!({
            "command": ": target=tests/runner_fixture.rs && .acdd/runner_fixture",
            "output": {
                "require_counts": true,
                "counts": {"passed": "(\\d+) passed", "failed": "(\\d+) failed"},
                "tests": {"target": "target=(tests/runner_fixture\\.rs)", "passed": "test\\s+([A-Za-z0-9_:]+)\\s+\\.\\.\\.\\s+ok"}
            }
        })).unwrap());
        let gates = &mut profile
            .gates
            .iter_mut()
            .find_map(|gate| gate.tasks.as_mut())
            .unwrap()
            .gates;
        let mut polish = gates
            .iter()
            .find(|gate| gate.id == "build")
            .unwrap()
            .clone();
        polish.id = "polish".into();
        polish.on_fail = Some("polish".into());
        let mut integrate = polish.clone();
        integrate.id = "integrate".into();
        integrate.on_fail = Some("integrate".into());
        integrate.proof = serde_json::from_value(json!("seam_test")).unwrap();
        integrate.command = Some("seam-fixture".into());
        gates.retain(|gate| gate.id != "seam");
        let review = gates.iter().position(|gate| gate.id == "review").unwrap();
        gates.insert(review, polish);
        gates.insert(
            review + 1,
            acdd_tasks::tasks::profile::GateDef {
                id: "seam".into(),
                gates: vec![integrate],
                ..Default::default()
            },
        );
    });
    let fixture =
        include_str!("lifecycle/role_fixture.py").replace("\"seamtest\"", "\"seam_test\"");
    fs::write(h.root().join(".acdd/lifecycle/seam_fixture.py"), fixture).unwrap();
    let first = h.task.id.clone();
    let second = h.add_second_case();
    let cases = [(first, "polish"), (second, "seam.integrate")];
    for (task_id, stage) in cases {
        h.task.id = task_id;
        let log = h.root().join(".acdd/lifecycle/roles.jsonl");
        if log.exists() {
            fs::remove_file(log).unwrap();
        }
        h.task.advance_to(stage).unwrap();
        let claim = h.task.claim_next().unwrap();
        let details = h.task.store.inspect_details(&h.task.id).unwrap();
        assert!(
            details["gates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|gate| gate["stage"] != stage || gate["state"] != "passed"),
            "{stage}: no prior pass is allowed"
        );
        let evidence = if stage == "seam.integrate" {
            let context =
                TaskHarness::manage_task(h.root(), &h.task.id, tasks::ManageAction::Context)
                    .unwrap();
            serde_json::from_value(
                context["workflow_guidance"]["expected_evidence_template"].clone(),
            )
            .unwrap()
        } else {
            h.task.evidence_for_gate(&claim).unwrap()
        };
        let session = TaskHarness::session_for(&evidence.worker_id);
        let findings = json!({"kind": "repair", "message": "first attempt"});
        h.task
            .submit_rejection_in_session(&evidence, Some(&session), Some(&findings))
            .unwrap();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
        assert!(result.is_ok(), "{stage} must recover: {result:?}");
        let records = h.records();
        let run = records
            .iter()
            .find(|record| record["stage"] == stage)
            .unwrap();
        assert_ne!(run["session"], session, "{stage}");
        assert_eq!(run["rejection"]["findings"], findings, "{stage}");
    }
}

#[test]
fn task_run_recovers_first_self_rejections_with_fresh_sessions() {
    let mut h = LifecycleHarness::with_profile(|profile| {
        profile.find_gate_mut("contract").unwrap().on_fail = Some("contract".into());
    });
    let claim = h.task.claim_next().unwrap();
    let evidence = h.task.evidence_for_gate(&claim).unwrap();
    let contract_session = TaskHarness::session_for(&evidence.worker_id);
    let findings = json!({"kind":"repair","message":"retry first attempt"});
    h.task
        .submit_rejection_in_session(&evidence, Some(&contract_session), Some(&findings))
        .unwrap();
    let root = h.root().to_owned();
    let task_id = h.task.id.clone();
    let options = h.options.clone();
    let mut build_session = None;
    let result = run_task_lifecycle(&root, &task_id, &options, &mut |event| {
        if matches!(event, OrchestratorEvent::RunnerExited { ref stage, .. } if stage == "contract_review")
        {
            let claim = h.task.claim_next().unwrap();
            let evidence = h.task.evidence_for_gate(&claim).unwrap();
            let session = TaskHarness::session_for(&evidence.worker_id);
            h.task
                .submit_rejection_in_session(&evidence, Some(&session), Some(&findings))
                .unwrap();
            build_session = Some(session);
        }
    });
    assert!(result.is_ok(), "self rejections must recover: {result:?}");
    let records = h.records();
    let cases = [
        ("contract", contract_session),
        ("build", build_session.unwrap()),
    ];
    for (stage, prior_session) in cases {
        let run = records
            .iter()
            .find(|record| record["stage"] == stage)
            .unwrap();
        assert_ne!(run["session"], prior_session, "{stage}");
        assert_eq!(run["rejection"]["findings"], findings, "{stage}");
    }
    assert_eq!(result.unwrap()["status"], "completed");
}

#[test]
fn rejected_predecessor_with_invalid_identity_fails_closed() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("review").unwrap();
    let claim = h.task.claim_next().unwrap();
    let mut evidence = h.task.evidence_for_gate(&claim).unwrap();
    evidence.proof = review_proof("reject");
    let reviewer_session = TaskHarness::session_for(&evidence.worker_id);
    h.task
        .submit_rejection_in_session(
            &evidence,
            Some(&reviewer_session),
            Some(&json!({"kind":"scope","message":"repair"})),
        )
        .unwrap();
    let revision = h.task.persisted_gate_evidence("build")["claim_revision"]
        .as_u64()
        .unwrap();
    let (worker, session): (String, Option<String>) = h.task.store.connection.query_row(
        "SELECT worker_id, session_fingerprint FROM task_claims WHERE task_id=?1 AND revision=?2",
        rusqlite::params![h.task.id, revision], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    let cases = [true, false];
    for missing_session in cases {
        if missing_session {
            h.task.store.connection.execute(
                "UPDATE task_claims SET session_fingerprint=NULL WHERE task_id=?1 AND revision=?2",
                rusqlite::params![h.task.id, revision],
            ).unwrap();
        } else {
            h.task.store.connection.execute(
                "UPDATE task_claims SET worker_id='inconsistent-worker' WHERE task_id=?1 AND revision=?2",
                rusqlite::params![h.task.id, revision],
            ).unwrap();
        }
        let error = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {}).unwrap_err();
        assert!(
            error.to_string().contains("TASK_RESUME_IDENTITY_MISSING"),
            "{error:#}"
        );
        h.task.store.connection.execute(
            "UPDATE task_claims SET worker_id=?1, session_fingerprint=?2 WHERE task_id=?3 AND revision=?4",
            rusqlite::params![worker, session, h.task.id, revision],
        ).unwrap();
    }
}

#[test]
fn task_run_recovers_rejection_handoff_after_restart() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("review").unwrap();
    let builder = h.task.persisted_gate_evidence("build");
    let builder_session = h
        .task
        .persisted_claim_session(builder["claim_revision"].as_u64().unwrap());
    let review = h.task.claim_next().unwrap();
    let mut evidence = h.task.evidence_for_gate(&review).unwrap();
    evidence.proof = review_proof("reject");
    let findings = json!({"kind":"scope","scope_findings":[{"path":"src/lib.rs","message":"repair before delivery"}]});
    let reviewer_session = TaskHarness::session_for(&evidence.worker_id);
    h.task
        .submit_rejection_in_session(&evidence, Some(&reviewer_session), Some(&findings))
        .unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(
        result.is_ok(),
        "persisted rejection must resume: {result:?}"
    );
    let records = h.records();
    assert_eq!(records[0]["stage"], "build");
    assert_eq!(records[0]["worker_id"], builder["worker_id"]);
    assert_eq!(records[0]["session"], builder_session);
    assert_eq!(records[0]["rejection"]["findings"], findings);
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
}

#[test]
fn task_run_does_not_accept_process_success_without_gate_submission() {
    let h = LifecycleHarness::new();
    h.write_control("mode.json", &json!("no-submit"));
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    assert!(
        result.is_err(),
        "an exit code is not accepted gate evidence"
    );
    let task = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(task.status, "in_progress");
    assert_eq!(h.records().len(), 1);
    assert!(task.receipt.is_none());
    assert!(h.task.store.inspect_details(&h.task.id).unwrap()["gates"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn task_run_preserves_foreign_active_claim() {
    let mut h = LifecycleHarness::new();
    let foreign = h.task.claim_next().unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    let error = result.unwrap_err().to_string();
    assert!(error.contains("TASK_ALREADY_CLAIMED"), "{error}");
    let retained = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(retained.claim_revision, foreign.claim_revision);
    assert_eq!(retained.worker_id, foreign.worker_id);
    assert_eq!(retained.worktree, foreign.worktree);
    assert_eq!(retained.status, "in_progress");
    assert!(h.records().is_empty());
}

#[test]
fn task_run_stops_on_runner_failure_or_timeout_without_delivery() {
    for (mode, code, timed_out) in [("exit-17", Some(17), false), ("sleep", None, true)] {
        let mut h = LifecycleHarness::new();
        h.options.runner_timeout = Duration::from_secs(1);
        h.write_control("mode.json", &json!(mode));
        let mut events = Vec::new();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |event| {
            events.push(event)
        });
        assert!(result.is_err(), "{mode} must stop execution");
        assert!(events.iter().any(|event| matches!(event, OrchestratorEvent::RunnerExited { exit_code, timed_out: observed_timeout, .. } if *exit_code == code && *observed_timeout == timed_out)));
        assert_ne!(
            h.task.store.inspect(&h.task.id).unwrap().status,
            "completed"
        );
        assert!(h.task.store.inspect(&h.task.id).unwrap().receipt.is_none());
    }
}

#[test]
fn task_run_delivery_preserves_subtask_guard() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("deliver").unwrap();
    TaskHarness::claim_in_worktree(
        h.root(),
        &h.task.id,
        "",
        &h.options.worker_id,
        h.root(),
        Some(&h.options.session_fingerprint),
    )
    .unwrap();
    let head = git_head(h.root());
    let manifest = fs::read(h.root().join(MANIFEST)).unwrap();
    let candidate = h.task.store.build_snapshot_candidate(&h.task.id).unwrap();
    let revision = h.task.store.inspect(&h.task.id).unwrap().claim_revision;
    for (status, evidence, expected) in [
        ("ready", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("in_progress", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("blocked", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
        ("completed", None, "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("completed", Some(""), "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("completed", Some("   "), "TASK_SUBTASK_EVIDENCE_MISSING"),
        ("pending", Some(""), "TASK_SUBTASKS_INCOMPLETE"),
    ] {
        if status == "pending" {
            h.task.set_all_subtasks_legacy_pending().unwrap();
            h.task
                .store
                .subtask_update(&h.task.id, "optional", "cancelled", None)
                .unwrap();
        } else {
            h.task
                .store
                .subtask_update(&h.task.id, "verify", status, evidence)
                .unwrap();
        }
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
        let error = result.unwrap_err().to_string();
        assert!(error.contains(expected), "{status}/{evidence:?}: {error}");
        let task = h.task.store.inspect(&h.task.id).unwrap();
        assert_eq!(task.status, "in_progress");
        assert_eq!(task.claim_revision, revision);
        assert!(task.receipt.is_none());
        assert_eq!(git_head(h.root()), head);
        assert_eq!(fs::read(h.root().join(MANIFEST)).unwrap(), manifest);
        assert_eq!(
            h.task.store.build_snapshot_candidate(&h.task.id).unwrap(),
            candidate
        );
    }
}

#[test]
fn task_run_delivery_withholds_receipt_on_candidate_drift() {
    let mut h = LifecycleHarness::new();
    h.task.advance_to("deliver").unwrap();
    let head = git_head(h.root());
    let candidate = h.task.store.build_snapshot_candidate(&h.task.id).unwrap();
    let manifest = fs::read(h.root().join(MANIFEST)).unwrap();
    let changed = "pub fn example() -> bool { false }\n";
    fs::write(h.root().join("src/lib.rs"), changed).unwrap();
    let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
    let error = result.unwrap_err().to_string();
    assert!(error.contains("TASK_RECEIPT_MISSING"), "{error}");
    let pending = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(pending.stage, "build");
    assert!(pending.receipt.is_none());
    assert_eq!(git_head(h.root()), head);
    assert_eq!(
        h.task.persisted_gate_evidence("build")["commit"],
        candidate.unwrap().0
    );
    assert_eq!(fs::read(h.root().join(MANIFEST)).unwrap(), manifest);
    assert_eq!(
        fs::read_to_string(h.root().join("src/lib.rs")).unwrap(),
        changed
    );
}

#[test]
fn task_run_cli_accepts_in_place_and_uses_lifecycle() {
    let h = LifecycleHarness::new();
    let output = Command::new(env!("CARGO_BIN_EXE_acdd"))
        .args([
            "--root",
            h.root().to_str().unwrap(),
            "--format",
            "json",
            "task",
            "run",
            &h.task.id,
            "--in-place",
        ])
        .env("ACDD_AGENT_SESSION_ID", "fixture-cli-coordinator")
        .current_dir(h.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "task run --in-place: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "completed");
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert!(!h.records().is_empty());
}

#[test]
fn malformed_emitted_review_templates_are_rejected_by_task_submission() {
    let mut h = LifecycleHarness::new();
    let cases = [
        (
            "corrupt-template",
            "missing review decision must be rejected",
        ),
        ("wrong-wrapper", "wrong proof wrapper must be rejected"),
    ];
    for (marker, reason) in cases {
        let path = h.root().join(".acdd/lifecycle").join(marker);
        let log = h.root().join(".acdd/lifecycle/roles.jsonl");
        if log.exists() {
            fs::remove_file(log).unwrap();
        }
        fs::write(&path, "").unwrap();
        let result = run_task_lifecycle(h.root(), &h.task.id, &h.options, &mut |_| {});
        assert!(result.is_err(), "{reason}: {result:?}");
        let task = h.task.store.inspect(&h.task.id).unwrap();
        assert_ne!(task.status, "completed", "{marker}");
        assert_eq!(task.stage, "review", "{marker}");
        assert!(
            h.records().iter().any(|record| record["stage"] == "review"),
            "{marker}: review runner must execute"
        );
        fs::remove_file(path).unwrap();
        h.task.store.reset(&h.task.id).unwrap();
    }
}

#[test]
fn renamed_workflow_gate_preserves_role_proof_contract() {
    let root = crate::acdd::harness::workflows::WorkflowHarness::agents().materialize();
    let path = root.path("docs/workflows/agents.yaml");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("id: review", "id: inspection");
    fs::write(path, source).unwrap();
    let result = TaskHarness::run_cli_for_root(
        root.root(),
        &["--format", "json", "workflow", "run", "agents"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["status"], "completed");
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    assert!(records
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["gate"] == "inspection"));
}

#[test]
fn workflow_role_decision_framing_and_consistency() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    for (mode, prepass, succeeds) in [
        ("bare", true, true),
        ("fenced", true, true),
        ("array", true, false),
        ("fenced-array", true, false),
        ("duplicate", true, false),
        ("fenced-duplicate", true, false),
        ("nested-match", false, true),
        ("nested-conflict", false, false),
        ("nested-sibling-match", false, true),
        ("nested-sibling-conflict", false, false),
        ("embedded", false, true),
        ("fenced-embedded", false, true),
        ("mismatch", true, false),
        ("identity", true, false),
        ("missing", false, false),
        ("empty", false, false),
        ("conflict", false, false),
        ("trailing", true, false),
        ("incomplete", true, false),
        ("multiple", true, false),
    ] {
        for path in ["review-rejected", "workflow-role-sessions.jsonl"] {
            if root.path(path).exists() {
                fs::remove_file(root.path(path)).unwrap();
            }
        }
        fs::write(root.path("role-mode"), mode).unwrap();
        if prepass {
            fs::write(root.path("review-rejected"), "").unwrap();
        }
        let result =
            run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
        assert_eq!(result.is_ok(), succeeds, "{mode}: {result:?}");
        if matches!(
            mode,
            "embedded" | "fenced-embedded" | "nested-match" | "nested-sibling-match"
        ) {
            let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
            assert!(records
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|record| record["gate"] == "build"
                    && record["findings"]["step_findings"][0] == "Repair this step"));
        }
    }
}

#[test]
fn workflow_role_and_command_both_execute() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let path = root.path("docs/workflows/agents.yaml");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "roles:\n",
            "commands:\n  check:\n    command: 'touch command-observed'\n    output: { require_counts: false }\nroles:\n",
        )
        .replace(
            "  - id: build\n    role: builder",
            "  - id: build\n    role: builder\n    command: check\n    proof: none",
        );
    fs::write(path, source).unwrap();
    fs::write(root.path("review-rejected"), "").unwrap();
    let result =
        run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None);
    assert!(result.is_ok(), "{result:?}");
    assert!(root.path("command-observed").exists());
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    assert!(records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .any(|record| record["gate"] == "build"));
}

#[test]
fn workflow_role_pass_is_blocked_by_failed_registered_command() {
    use crate::acdd::harness::workflows::WorkflowHarness;
    use contextunity_acdd::acdd_tasks::engine::orchestrator::workflow::run_standalone_workflow;
    use std::path::Path;
    let root = WorkflowHarness::agents().materialize();
    let path = root.path("docs/workflows/agents.yaml");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "roles:\n",
            "commands:\n  check: 'touch command-observed; exit 1'\nroles:\n",
        )
        .replace(
            "  - id: build\n    role: builder",
            "  - id: build\n    role: builder\n    command: check\n    proof: none",
        );
    fs::write(path, source).unwrap();
    let error = run_standalone_workflow(root.root(), Path::new("docs/workflows/agents.yaml"), None)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_PROOF_UNOBSERVED"),
        "{error:#}"
    );
    assert!(root.path("command-observed").exists());
    let records = fs::read_to_string(root.path("workflow-role-sessions.jsonl")).unwrap();
    let gates: Vec<_> = records
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["gate"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(gates, ["build"]);
}
