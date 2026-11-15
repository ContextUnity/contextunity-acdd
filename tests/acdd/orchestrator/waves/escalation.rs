use super::run_cli;
use crate::acdd::harness::workflows::WorkflowHarness;
use serde_json::Value;
use std::fs;

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
fn workflow_failure_routes_retry_without_creating_task_state() {
    let root = WorkflowHarness::gate_profile(
        "docs/workflows/retry.yaml",
        include_str!("../../fixtures/workflows/retry.yaml"),
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
        include_str!("../../fixtures/workflows/failing.yaml"),
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
