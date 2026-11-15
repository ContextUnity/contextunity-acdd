use super::run_cli;
use crate::acdd::harness::plans::PlanHarness;
use crate::acdd::support::git_command;
use crate::acdd::support::git_head;
use clap::Parser;
use contextunity_acdd::acdd_tasks::tasks::profile;
use contextunity_acdd::cli::Cli;
use serde_json::Value;
use std::fs;

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
fn plan_delivery_reuses_subtask_completion_and_evidence_guard() {
    use crate::acdd::harness::plans::PlanFixture;
    let base = PlanHarness::fixture(PlanFixture::PendingSubtask);
    let root = base.materialize(base.path);
    for (fixture, source, expected) in [
        (
            PlanFixture::PendingSubtask,
            include_str!("../../fixtures/plans/pending-subtask.md"),
            Some("TASK_SUBTASKS_INCOMPLETE"),
        ),
        (
            PlanFixture::MissingSubtaskEvidence,
            include_str!("../../fixtures/plans/missing-subtask-evidence.md"),
            Some("TASK_SUBTASK_EVIDENCE_MISSING"),
        ),
        (
            PlanFixture::CompletedSubtask,
            include_str!("../../fixtures/plans/completed-subtask.md"),
            None,
        ),
    ] {
        let harness = PlanHarness::fixture(fixture);
        root.write(harness.path, source);
        let before = fs::read(root.path(harness.path)).unwrap();
        let result = run_cli(root.root(), &["plan", "run", "invalid"]);
        if let Some(expected) = expected {
            let error = result.unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(fs::read(root.path(harness.path)).unwrap(), before);
        } else {
            assert_eq!(result.unwrap()["status"], "completed");
        }
        root.remove("plan-execution.log");
    }
}

#[test]
fn plan_dependency_validation_precedes_execution() {
    use crate::acdd::harness::plans::PlanFixture;
    let base = PlanHarness::fixture(PlanFixture::UnknownDependency);
    let root = base.materialize(base.path);
    for (fixture, source, expected) in [
        (
            PlanFixture::UnknownDependency,
            include_str!("../../fixtures/plans/unknown-dependency.md"),
            "PLAN_DEPENDENCY_UNKNOWN",
        ),
        (
            PlanFixture::Cycle,
            include_str!("../../fixtures/plans/cycle.md"),
            "PLAN_DEPENDENCY_CYCLE",
        ),
        (
            PlanFixture::CancelledDependency,
            include_str!("../../fixtures/plans/cancelled-dependency.md"),
            "PLAN_DEPENDENCY_INCOMPLETE",
        ),
    ] {
        let harness = PlanHarness::fixture(fixture);
        root.write(harness.path, source);
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

#[test]
fn plan_run_executes_steps_in_place_without_committing() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let head = git_head(root.root());
    let snapshots = harness.snapshots(&root);
    let plan_profile =
        profile::validate_profile_document(include_str!("../../fixtures/plans/snapshot.yaml"));
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
