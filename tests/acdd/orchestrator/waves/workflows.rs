use super::run_cli;
use crate::acdd::harness::workflows::WorkflowHarness;
use clap::Parser;
use contextunity_acdd::cli::Cli;
use serde_json::Value;
use std::fs;

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
        "contextunity-acdd",
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
        include_str!("../../fixtures/workflows/ordered.yaml")
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
                "contextunity-acdd",
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
