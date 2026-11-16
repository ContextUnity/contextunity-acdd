use super::harness;
use super::root_fixture::RootGateHarness;
use super::run_cli;
use crate::acdd::support::git_command;
use crate::acdd::support::git_head;
use contextunity_acdd::acdd_tasks::tasks::profile;
use serde_json::Value;
use std::fs;

#[test]
fn native_absorbed_polish_passes_immutable_review_and_handoff() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    crate::acdd::snapshot::stage_role_helper(h.root());
    let mut profile = h.read_profile();
    let mut role = profile.roles["wave_role"].clone();
    role.runner = Some(r#"sh -c 'printf "polished\\n" > docs/polish-note.md && .acdd/role_fixture wave-role "$1"' sh '{prompt}'"#.into());
    profile.roles.insert("polishing_role".into(), role);
    let polish = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap();
    polish.role = Some("polishing_role".into());
    polish.persistence.post = Some(PersistenceAction::Snapshot);
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "milestone_review")
        .unwrap()
        .persistence
        .immutable = true;
    let handoff = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "handoff")
        .unwrap();
    handoff.persistence.immutable = true;
    handoff.absorb_working_tree = true;
    profile.polish_scope.push("docs/polish-note.md".into());
    h.commit_profile(&profile, "immutable handoff fixture");
    let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(result.is_ok(), "{result:#?}");
    let output = git_command(h.root(), &["show", "HEAD:docs/polish-note.md"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "polished\n");
    assert!(h
        .root()
        .join("docs/milestones/archive/011-wave.md")
        .exists());
    assert!(!h.root().join("docs/milestones/011-wave.md").exists());
}

#[test]
fn native_root_snapshots_capture_pre_and_post_without_moving_head() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    let polish = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap();
    polish.persistence.pre = Some(PersistenceAction::Snapshot);
    polish.persistence.post = Some(PersistenceAction::Snapshot);
    h.commit_profile(&profile, "root snapshots");
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let raw: String = h.task.store.connection.query_row(
        "SELECT proof_payload FROM gate_transitions WHERE subject='milestone' AND gate_path='polish' AND action='pass' ORDER BY id DESC LIMIT 1", [], |row| row.get(0)
    ).unwrap();
    let payload: Value = serde_json::from_str(&raw).unwrap();
    for phase in ["pre_snapshot", "post_snapshot"] {
        let snapshot = &payload[phase];
        assert_eq!(snapshot["head"], git_head(h.root()));
        assert_eq!(
            snapshot["candidate_fingerprint"],
            payload[if phase == "pre_snapshot" {
                "candidate_before"
            } else {
                "candidate_after"
            }]
        );
        let output = git_command(
            h.root(),
            &[
                "rev-parse",
                "--verify",
                snapshot["snapshot_ref"].as_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            snapshot["snapshot_commit"].as_str().unwrap()
        );
    }
}

#[test]
fn native_immutable_review_rejects_candidate_mutation() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    crate::acdd::snapshot::stage_role_helper(h.root());
    let mut profile = h.read_profile();
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .persistence
        .post = Some(PersistenceAction::Snapshot);
    let mut role = profile.roles["wave_role"].clone();
    role.runner = Some(r#"sh -c 'printf "// mutation\\n" >> src/lib.rs && .acdd/role_fixture wave-role "$1"' sh '{prompt}'"#.into());
    profile.roles.insert("mutating_review".into(), role);
    let review = profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "milestone_review")
        .unwrap();
    review.role = Some("mutating_review".into());
    review.persistence.immutable = true;
    h.commit_profile(&profile, "immutable review");
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("CANDIDATE_DRIFT_REBIND_REQUIRED"),
        "{error:#}"
    );
    let count: i64 = h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='milestone_review' AND action='pass'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn native_root_replay_after_reopen_invalidates_changed_candidate_and_preserves_receipt() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    use acdd_tasks::tasks::profile::PersistenceAction;
    let mut h = harness::WaveHarness::with_single_task(true);
    let mut profile = h.read_profile();
    profile
        .gates
        .iter_mut()
        .find(|gate| gate.id == "polish")
        .unwrap()
        .persistence
        .post = Some(PersistenceAction::Snapshot);
    h.commit_profile(&profile, "replay snapshots");
    fs::write(h.root().join(".acdd/stop-wave-review"), "").unwrap();
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    let receipt = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(receipt.status, "completed");
    let receipt = serde_json::to_value(receipt.receipt).unwrap();
    assert!(!receipt.is_null());
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    let temporary = acdd_tasks::store::TasksStore::open_in_memory("fixture", "fixture").unwrap();
    drop(std::mem::replace(&mut h.task.store, temporary));
    h.task.store = contextunity_acdd::engine::tasks::store_for_task(h.root(), &h.task.id).unwrap();
    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn changed_after_review() {}\n",
    )
    .unwrap();
    for args in [
        vec!["add", "src/lib.rs"],
        vec!["commit", "-m", "changed candidate"],
    ] {
        assert!(git_command(h.root(), &args).status().unwrap().success());
    }
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_RUNNER_FAILED"),
        "{error:#}"
    );
    let count: i64 = h.task.store.connection.query_row("SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='polish' AND action='pass'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        h.task.store.inspect(&h.task.id).unwrap().status,
        "completed"
    );
    assert_eq!(
        serde_json::to_value(h.task.store.inspect(&h.task.id).unwrap().receipt).unwrap(),
        receipt
    );
    let after: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    assert_eq!(claims, after);
}

#[test]
fn root_immutable_gate_rejects_modified_candidate_after_claim() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = RootGateHarness::new();
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let preparation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let preparation_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation_review",
        "role": "wave_role",
        "proof": "none",
        "independent_from": "preparation",
        "save": { "immutable": true }
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![preparation, preparation_review, task_container, handoff];
    h.commit_profile(&profile, "immutable root gate profile");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "preparation".into(),
            worker_id: "worker-a".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "preparation".into(),
            evidence: serde_json::json!({
                "worker_id": "worker-a",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "preparation_review".into(),
            worker_id: "reviewer-b".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn modified() -> bool { false }\n",
    )
    .unwrap();

    let response = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "preparation_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-b",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert!(
        response["candidate_drift_warning"]["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("src/lib.rs")),
        "{response}"
    );
}

#[test]
fn root_immutable_gate_enforces_prior_snapshot_across_intervening_ordinary_pass() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = RootGateHarness::new();
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let preparation: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let ordinary_gate: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "ordinary_gate",
        "role": "wave_role",
        "proof": "none"
    }))
    .unwrap();
    let preparation_review: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "preparation_review",
        "role": "wave_role",
        "proof": "none",
        "independent_from": "preparation",
        "save": { "immutable": true }
    }))
    .unwrap();
    let handoff = profile
        .gates
        .iter()
        .find(|gate| gate.id == "handoff")
        .unwrap()
        .clone();
    profile.gates = vec![
        preparation,
        ordinary_gate,
        preparation_review,
        task_container,
        handoff,
    ];
    h.commit_profile(&profile, "intervening pass before immutable root gate");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    for (stage, worker) in [("preparation", "worker-a"), ("ordinary_gate", "worker-b")] {
        claim(
            h.root(),
            Claim {
                task_id: milestone_id.clone(),
                stage: stage.into(),
                worker_id: worker.into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
        submit(
            h.root(),
            Submit {
                task_id: milestone_id.clone(),
                stage: stage.into(),
                evidence: serde_json::json!({
                    "worker_id": worker,
                    "worktree": worktree,
                    "proof": { "none_proof": {} }
                }),
                action: Action::Pass,
                findings: None,
                session_fingerprint: None,
            },
        )
        .unwrap();
    }

    fs::write(
        h.root().join("src/lib.rs"),
        "pub fn altered() -> bool { false }\n",
    )
    .unwrap();

    let response = claim(
        h.root(),
        Claim {
            task_id: milestone_id,
            stage: "preparation_review".into(),
            worker_id: "reviewer-c".into(),
            worktree,
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert!(
        response["candidate_drift_warning"]["changed_paths"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("src/lib.rs")),
        "{response}"
    );
}

#[test]
fn post_task_immutable_review_ignores_pre_task_snapshot_and_cli_verifies_root_gate() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = RootGateHarness::new();
    let mut profile = h.read_profile();
    let task_container = profile
        .gates
        .iter()
        .find(|gate| gate.tasks.is_some())
        .unwrap()
        .clone();
    let blueprint: profile::GateDef = serde_json::from_value(serde_json::json!({
        "id": "blueprint",
        "role": "wave_role",
        "proof": "none",
        "save": { "pre": "snapshot", "post": "snapshot" }
    }))
    .unwrap();
    let mut verify = profile.find_gate("verify").unwrap().clone();
    verify.on_success = Some("milestone_review".into());
    verify.on_fail = None;
    let mut milestone_review = profile.find_gate("milestone_review").unwrap().clone();
    milestone_review.independent_from = None;
    milestone_review.on_fail = None;
    milestone_review.persistence.immutable = true;
    let handoff = profile.find_gate("handoff").unwrap().clone();
    profile.gates = vec![blueprint, task_container, verify, milestone_review, handoff];
    h.commit_profile(&profile, "blueprint snapshot to post-task review profile");

    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();
    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    h.task.advance_to_completion().unwrap();

    let verified = run_cli(h.root(), &["task", "verify", "m-wave"]).unwrap();
    assert!(
        verified["milestone"].as_str().unwrap().ends_with("/m-wave"),
        "{verified:?}"
    );
    assert_eq!(verified["stage"], "verify");
    assert_eq!(verified["passed"], true);

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            worker_id: "coord-2".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "verify".into(),
            evidence: serde_json::json!({
                "worker_id": "coord-2",
                "worktree": worktree,
                "proof": {
                    "command_proof": {
                        "command": "check_full_repo",
                        "exit_code": 0,
                        "tests_passed": 1,
                        "tests_failed": 0
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "milestone_review".into(),
            worker_id: "reviewer-9".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    let review_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "milestone_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-9",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "decision": "pass",
                        "summary": "verified post-task candidate without pre-task snapshot collision"
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(review_submit["action"], "pass");
}
