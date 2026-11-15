use crate::acdd::{
    harness::plans::{PlanFixture, PlanHarness},
    support::*,
};
use acdd_tasks::{
    engine::orchestrator::plan::{run_plan_workflow, PlanDocument},
    tasks::profile,
};
use std::{fs, path::Path};

#[test]
fn plan_steps_persist_in_tasks_store_and_resume_without_command_replay() {
    let harness = PlanHarness::linear();
    let root = harness.materialize(harness.path);
    let profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    let head = git_head(root.root());
    let plan = PlanDocument::from_sections(
        Path::new(harness.path),
        fs::read_to_string(root.path(harness.path)).unwrap(),
        &harness.sections,
    )
    .unwrap();
    let first = run_plan_workflow(root.root(), plan, &profile, true).unwrap();
    assert_eq!(first["status"], "completed");
    let store = tasks::store(root.root()).unwrap();
    let rows: Vec<String> = store
        .connection
        .prepare("SELECT task_id FROM tasks WHERE milestone_ref=?1 ORDER BY task_id")
        .unwrap()
        .query_map([harness.path], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "Plan steps must use the canonical task registry: {rows:?}"
    );
    for id in &rows {
        assert_eq!(store.inspect(id).unwrap().status, "completed");
    }
    drop(store);
    let source = fs::read_to_string(root.path(harness.path)).unwrap();
    let sections =
        contextunity_acdd::engine::docs::extract_with_details(harness.path, &source, 0.0)
            .unwrap()
            .into_iter()
            .map(|(section, _)| section)
            .collect::<Vec<_>>();
    let plan = PlanDocument::from_sections(Path::new(harness.path), source, &sections).unwrap();
    assert_eq!(
        run_plan_workflow(root.root(), plan, &profile, true).unwrap()["status"],
        "completed"
    );
    assert_eq!(
        fs::read_to_string(root.path("plan-execution.log")).unwrap(),
        "step\nstep\n"
    );
    assert_eq!(git_head(root.root()), head);
    assert!(!root.path(".worktrees").exists());
}

#[test]
fn blocked_plan_delivery_retains_real_subtask_lifecycle_state() {
    let harness = PlanHarness::fixture(PlanFixture::PendingSubtask);
    let root = harness.materialize(harness.path);
    let profile =
        profile::validate_profile_document(include_str!("../fixtures/plans/snapshot.yaml"))
            .unwrap();
    let plan = PlanDocument::from_sections(
        Path::new(harness.path),
        fs::read_to_string(root.path(harness.path)).unwrap(),
        &harness.sections,
    )
    .unwrap();
    let result = run_plan_workflow(root.root(), plan, &profile, true);
    assert!(
        result.is_err(),
        "pending subtasks must withhold snapshot completion"
    );
    let store = tasks::store(root.root()).unwrap();
    let states: Vec<String> = store.connection.prepare(
        "SELECT s.status FROM task_subtasks s JOIN tasks t ON t.task_id=s.task_id WHERE t.milestone_ref=?1 AND s.subtask_ref='check'",
    ).unwrap().query_map([harness.path], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        states,
        ["ready"],
        "pending Plan subtasks must survive in TasksStore for claim/resume"
    );
    assert!(!fs::read_to_string(root.path(harness.path))
        .unwrap()
        .contains("**Status**: completed"));
}
