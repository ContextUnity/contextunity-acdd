use super::harness;
use crate::acdd::support::git_command;
use crate::acdd::support::git_head;
use std::fs;

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
