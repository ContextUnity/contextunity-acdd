use super::harness::MANIFEST;
use crate::acdd::{support::MilestoneFixtureBuilder, tasks::harness::TaskHarness};
use acdd_tasks::{milestones::validate_wave_dag, tasks::Milestone};

#[test]
fn persisted_waves_and_dependency_readiness_agree_without_processes() {
    let source = MilestoneFixtureBuilder::new("m-wave")
        .task("first", &["src/lib.rs"])
        .task_with_deps("second", &["src/second.rs"], &["first"])
        .task("cancelled", &["src/cancelled.rs"])
        .task_status("cancelled")
        .task_with_deps("blocked", &["src/blocked.rs"], &["cancelled"])
        .build();
    let milestone = Milestone::parse(&source, "forge-mcp").unwrap();
    let mut h = TaskHarness::memory_with_manifest(&source, MANIFEST);
    let waves = validate_wave_dag(&milestone).unwrap();
    assert_eq!(
        waves,
        [vec!["first", "cancelled"], vec!["second", "blocked"]]
    );

    // One migrated in-memory store covers both dependency statuses. Failed claims
    // must preserve the ready dependent and never enroll a claim revision.
    let worktree = h.root().to_string_lossy().into_owned();
    for (task_ref, dependency, status) in [
        ("second", "first", "ready"),
        ("blocked", "cancelled", "cancelled"),
    ] {
        let id = format!("{}:{task_ref}", h.id.split_once(':').unwrap().0);
        let dependency_id = format!("{}:{dependency}", h.id.split_once(':').unwrap().0);
        let before = h.store.inspect(&id).unwrap();
        assert_eq!(before.wave_index, 1);
        let predecessor = h.store.inspect(&dependency_id).unwrap();
        assert_eq!(predecessor.wave_index, 0);
        assert_eq!(predecessor.status, status);
        assert!(predecessor.receipt.is_none());
        let error = h
            .store
            .claim(&id, "", "scheduler", &worktree, Some("scheduler-session"))
            .unwrap_err();
        assert!(error.to_string().contains("TASK_NOT_READY"), "{error:#}");
        assert!(error.to_string().contains(&dependency_id), "{error:#}");
        let after = h.store.inspect(&id).unwrap();
        assert_eq!(after.status, "ready");
        assert_eq!(after.claim_revision, before.claim_revision);
        assert!(after.worker_id.is_none());
    }
    let first = h.claim_next().unwrap();
    assert_eq!(first.task_id, h.id);
    assert_eq!(first.wave_index, 0);
    assert_eq!(first.status, "in_progress");
}
