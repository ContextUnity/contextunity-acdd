use super::harness;
use crate::acdd::support::git_command;
use crate::acdd::support::git_head;
use contextunity_acdd::acdd_tasks::tasks::profile;
use std::fs;

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

#[test]
fn milestone_waves_reject_unobserved_or_stale_role_decisions() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let mut h = harness::WaveHarness::new(true);
    let stop = h.root().join(".acdd/no-wave-decision");
    fs::write(&stop, "").unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        error.to_string().contains("WORKFLOW_DECISION_REQUIRED"),
        "{error:#}"
    );
    assert_eq!(h.task.store.list(None, "completed", None).unwrap().len(), 2);
    fs::remove_file(stop).unwrap();
    let baseline = h.checkpoint();
    for (control, expected) in [
        ("no-wave-decision", "WORKFLOW_DECISION_REQUIRED"),
        ("stale-wave-review", "HANDOFF_EVIDENCE_STALE"),
    ] {
        h.restore(&baseline);
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
