use super::harness;
use super::root_fixture::RootGateHarness;
use std::fs;

#[test]
fn milestone_waves_select_the_applicable_conditional_task_container() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = RootGateHarness::with_dependencies();
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
    let options = h.run_options();
    let error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED: stop_before_tasks"),
        "{error:#}"
    );
}

#[test]
fn milestone_cancelled_dependency_blocks_dispatch_without_completion_credit() {
    use crate::acdd::support::MilestoneFixtureBuilder;
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = RootGateHarness::with_dependencies();
    let source = MilestoneFixtureBuilder::new("m-wave")
        .task("first", &["src/lib.rs"])
        .task_status("cancelled")
        .task_with_deps("second", &["src/second.rs"], &["first"])
        .build();
    fs::write(h.root().join(harness::MANIFEST), source).unwrap();
    let options = h.run_options();
    let error = run_milestone_waves(h.root(), "m-wave", &options, &mut |_| {}).unwrap_err();
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
