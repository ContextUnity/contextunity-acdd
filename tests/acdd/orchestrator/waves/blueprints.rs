use super::harness;
use super::root_fixture::RootGateHarness;
use crate::acdd::support::git_head;
use serde_json::Value;
use std::fs;

#[test]
fn malformed_blueprint_guidance_cannot_be_repaired_by_role_fixture() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;
    let h = harness::WaveHarness::with_blueprint_gate(true);
    for marker in [
        "corrupt-blueprint-missing",
        "corrupt-blueprint-tasks",
        "corrupt-blueprint-nested",
    ] {
        fs::write(h.root().join(".acdd").join(marker), "").unwrap();
        let result = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
        assert!(result.is_err(), "{marker}: malformed guidance must fail");
        let passes: i64 = h.task.store.connection.query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='preflight' AND action='pass'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(passes, 0, "{marker}: no malformed blueprint may pass");
        fs::remove_file(h.root().join(".acdd").join(marker)).unwrap();
    }
}

#[test]
fn invalid_pre_task_task_map_is_rejected_before_root_pass_is_recorded() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let h = harness::WaveHarness::with_blueprint_gate(true);
    fs::write(h.root().join(".acdd/invalid-task-proof-map"), "invalid").unwrap();
    let error = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).unwrap_err();
    assert!(
        format!("{error:#}").contains("WORKFLOW_ATTEMPTS_EXHAUSTED"),
        "{error:#}"
    );

    let (action, payload): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT action, proof_payload FROM gate_transitions
             WHERE subject='milestone' AND gate_path='preflight' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(action, "fail");
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["findings"]["code"], "MILESTONE_TASK_PROOF_INVALID");
    let accepted_passes: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions
             WHERE subject='milestone' AND gate_path='preflight' AND action='pass'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(accepted_passes, 0);
    assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    let claims: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_claims", [], |row| row.get(0))
        .unwrap();
    assert_eq!(claims, 0);
}

#[test]
fn pre_task_resynthesis_requires_a_fresh_review() {
    use acdd_tasks::store::GateTransition;

    let mut h = RootGateHarness::new();
    let mut profile = harness::milestone_profile(true);
    profile.gates[0] = harness::blueprint_synthesis_gate();
    h.commit_profile(&profile, "resynthesis route fixture");
    let milestone = h.task.id.split_once(':').unwrap().0.to_owned();
    let record = |gate: &str, action: &str| {
        h.task
            .store
            .record_gate_transition(&GateTransition {
                milestone_id: milestone.clone(),
                subject: "milestone".into(),
                gate_path: format!("blueprint_synthesis.{gate}"),
                action: action.into(),
                proof_kind: "orchestrator".into(),
                proof_payload: serde_json::json!({"findings": {"scope_findings": ["revise"]}}),
                git_tree_sha: git_head(h.root()),
                worker_id: gate.into(),
            })
            .unwrap();
    };
    record("synthesize", "pass");
    record("review", "pass");
    record("review", "fail");
    record("synthesize", "pass");
    let error = h.task.claim_next().unwrap_err();
    assert!(
        format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{error:#}"
    );
    let advances: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject=?1 AND action='advance'",
            [&h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(advances, 0);
}

fn pre_task_multi_blueprint_fixture() -> RootGateHarness {
    use crate::acdd::{support::MilestoneFixtureBuilder, tasks::harness::TaskHarness};
    let mut h = RootGateHarness::new();
    h.with_blueprint_synthesis_root_gates();
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].id = "review".into();
    let source = MilestoneFixtureBuilder::new("m-wave")
        .task("task_a", &["src/a.rs"])
        .task("task_b", &["src/b.rs"])
        .build();
    h.task = TaskHarness::disk_with_profile(&source, harness::MANIFEST, profile);
    h
}

fn pre_task_submit_blueprint(h: &RootGateHarness, tasks: Value) -> anyhow::Result<Value> {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let worktree = h.root().to_string_lossy().into_owned();
    claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            worker_id: "architect".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )?;
    submit(
        h.root(),
        Submit {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"scheme_proof":{"tasks":tasks}}}),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
}

#[test]
fn pre_task_nested_blueprint_failed_synthesis_blocks_all_task_proofs() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let mut h = pre_task_multi_blueprint_fixture();
    let worktree = h.root().to_string_lossy().into_owned();
    claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: "blueprint_synthesis.synthesize".into(),
            worker_id: "architect".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(h.root(), Submit { task_id:"m-wave".into(), stage:"blueprint_synthesis.synthesize".into(),
        evidence:serde_json::json!({"worker_id":"architect","worktree":worktree,"proof":{"scheme_proof":{"tasks":{"task_a":{"goal":"a"},"task_b":{"goal":"b"}}}}}),
        action:Action::Reject, findings:Some(serde_json::json!({"scope_findings":["revise"]})), session_fingerprint:None }).unwrap();
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let second = format!("{}:task_b", h.task.id.split_once(':').unwrap().0);
    let stage = h.task.store.inspect(&second).unwrap().stage;
    let error = claim(
        h.root(),
        Claim {
            task_id: second,
            stage,
            worker_id: "other".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let failures: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.synthesize' AND action='fail'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(failures, 1);
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND action='advance'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pre_task_nested_blueprint_invalid_task_maps_fail_before_fanout() {
    let mut h = pre_task_multi_blueprint_fixture();
    for (tasks, detail) in [
        (
            serde_json::json!({"task_a":{"goal":"a"}}),
            "missing non-cancelled task_ref 'task_b'",
        ),
        (
            serde_json::json!({"task_a":{"goal":"a"},"task_b":{"goal":"b"},"foreign":{"goal":"x"}}),
            "unknown task_ref 'foreign'",
        ),
    ] {
        let error = pre_task_submit_blueprint(&h, tasks).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("MILESTONE_TASK_PROOF_INVALID") && message.contains(detail),
            "{message}"
        );
        assert!(format!("{:#}", h.task.claim_next().unwrap_err())
            .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
        let count: i64 = h.task.store.connection.query_row(
            "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND action='advance'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let root_passes: i64 = h
            .task
            .store
            .connection
            .query_row(
                "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND action='pass'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(root_passes, 0);
        assert_eq!(h.task.store.inspect(&h.task.id).unwrap().status, "ready");
    }
}

#[test]
fn pre_task_nested_blueprint_independent_review_injects_only_each_task_slice() {
    use acdd_tasks::engine::{claim, submit, Action, Claim, Submit};
    let h = pre_task_multi_blueprint_fixture();
    pre_task_submit_blueprint(
        &h,
        serde_json::json!({"task_a":{"goal":"slice-a"},"task_b":{"goal":"slice-b"}}),
    )
    .unwrap();
    let worktree = h.root().to_string_lossy().into_owned();
    let review = |worker: &str| {
        claim(
            h.root(),
            Claim {
                task_id: "m-wave".into(),
                stage: "blueprint_synthesis.review".into(),
                worker_id: worker.into(),
                worktree: worktree.clone(),
                bundle: None,
                session_fingerprint: None,
            },
        )
    };
    assert!(
        format!("{:#}", review("architect").unwrap_err()).contains("TASK_REVIEW_NOT_INDEPENDENT")
    );
    review("reviewer").unwrap();
    submit(h.root(), Submit { task_id:"m-wave".into(), stage:"blueprint_synthesis.review".into(),
        evidence:serde_json::json!({"worker_id":"reviewer","worktree":worktree,"proof":{"none_proof":{}}}),
        action:Action::Pass, findings:None, session_fingerprint:None }).unwrap();
    let milestone = h.task.id.split_once(':').unwrap().0;
    for (task_ref, expected, other) in [
        ("task_a", "slice-a", "slice-b"),
        ("task_b", "slice-b", "slice-a"),
    ] {
        let id = format!("{milestone}:{task_ref}");
        let stage = h.task.store.inspect(&id).unwrap().stage;
        let result = claim(
            h.root(),
            Claim {
                task_id: id,
                stage,
                worker_id: format!("worker-{task_ref}"),
                worktree: worktree.clone(),
                bundle: Some(true),
                session_fingerprint: None,
            },
        )
        .unwrap();
        let inherited = &result["context_bundle"]["inherited_proofs"];
        assert_eq!(inherited[0]["proof"]["goal"], expected, "{result}");
        assert!(!inherited.to_string().contains(other), "{result}");
    }
}

#[test]
fn pre_task_nested_blueprint_rejected_review_does_not_fan_out() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_nested_blueprint(true);
    let mut profile = h.read_profile();
    profile.gates[0].gates[1].max_attempts = Some(1);
    h.commit_profile(&profile, "rejected nested blueprint fixture");
    assert!(run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {}).is_err());
    assert!(format!("{:#}", h.task.claim_next().unwrap_err())
        .contains("MILESTONE_PRE_TASK_GATE_PENDING"));
    let count: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject != 'milestone' AND gate_path='blueprint_synthesis.synthesize' AND action='advance'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(count, 0);
    let rejected: i64 = h.task.store.connection.query_row(
        "SELECT count(*) FROM gate_transitions WHERE subject='milestone' AND gate_path='blueprint_synthesis.review' AND action='fail'", [], |row| row.get(0)
    ).unwrap();
    assert_eq!(rejected, 1);
}

#[test]
fn pre_task_blueprint_synthesis_container_barrier_and_per_task_proof_injection() {
    use acdd_tasks::engine::orchestrator::waves::run_milestone_waves;

    let mut h = harness::WaveHarness::with_nested_blueprint(true);

    // 1. Direct claim before the preflight gate passes fails closed
    let claim_err = h.task.claim_next().unwrap_err();
    assert!(
        format!("{claim_err:#}").contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{claim_err:#}"
    );

    // 2. Stop at milestone_review before handoff so state is not pruned
    let stop = h.root().join(".acdd/stop-wave-review");
    fs::write(&stop, "").unwrap();
    let outcome = run_milestone_waves(h.root(), "m-wave", &h.options, &mut |_| {});
    assert!(outcome.is_err(), "{outcome:#?}");

    // 3. Verify gate_transitions contains the fanned-out task proof for subject = full_task_id
    let (action, proof_payload): (String, String) = h
        .task
        .store
        .connection
        .query_row(
            "SELECT action, proof_payload FROM gate_transitions
             WHERE subject=?1 AND gate_path='blueprint_synthesis.synthesize' AND action='advance'",
            [&h.task.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("per-task proof transition should be recorded");
    assert_eq!(action, "advance");
    let payload: Value = serde_json::from_str(&proof_payload).unwrap();
    assert_eq!(payload["layer"], "fixture seam");
    assert_eq!(payload["summary"], "Exercise the pre-task proof boundary.");
    assert_eq!(payload["blackboard_decision_id"], "wave-blueprint");

    let transitions: Vec<(String, String)> = h.task.store.connection.prepare(
        "SELECT gate_path, action FROM gate_transitions WHERE subject='milestone' AND gate_path LIKE 'blueprint_synthesis.%' ORDER BY id"
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        transitions,
        vec![
            ("blueprint_synthesis.synthesize".into(), "pass".into()),
            ("blueprint_synthesis.review".into(), "fail".into()),
            ("blueprint_synthesis.synthesize".into(), "pass".into()),
            ("blueprint_synthesis.review".into(), "pass".into()),
        ]
    );

    // 4. Verify that task completed successfully with proof receipt
    let inspected = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(inspected.status, "completed");
    assert!(inspected.receipt.is_some());
}

#[test]
fn milestone_root_gate_claim_and_submit_enforce_barriers_independence_and_fanout() {
    use contextunity_acdd::acdd_tasks::engine::{
        claim, manage, submit, Action, Claim, Manage, ManageAction, Submit,
    };

    let mut h = RootGateHarness::new();
    h.with_blueprint_synthesis_root_gates();

    let worktree = h.root().to_string_lossy().into_owned();
    let revisions_before_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_revisions", [], |row| row.get(0))
        .unwrap();
    let transitions_before_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let next_action = manage(
        h.root(),
        Manage {
            action: ManageAction::Next,
            worker_id: Some("architect-1".into()),
            worktree: Some(worktree.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(next_action["subject"], "milestone");
    assert_eq!(next_action["stage"], "blueprint_synthesis.synthesize");
    let revisions_after_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM task_revisions", [], |row| row.get(0))
        .unwrap();
    let transitions_after_next: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        revisions_after_next, revisions_before_next,
        "pre-task next dispatch must not enroll task revisions"
    );
    assert_eq!(
        transitions_after_next, transitions_before_next,
        "pre-task next dispatch must not mutate gate_transitions"
    );

    let pre_claim = claim(
        h.root(),
        Claim {
            task_id: "m-wave".into(),
            stage: String::new(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(pre_claim["subject"], "milestone");
    assert_eq!(pre_claim["stage"], "blueprint_synthesis.synthesize");
    let milestone_id = pre_claim["milestone_id"].as_str().unwrap().to_owned();

    // Post-task gate cannot be claimed while pre-task barrier or tasks are incomplete.
    let barrier_err = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        barrier_err.contains("MILESTONE_PRE_TASK_GATE_PENDING"),
        "{barrier_err}"
    );

    let task_ref = h.task.store.inspect(&h.task.id).unwrap().spec.task_ref;
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            task_ref: {
                                "goal": "root-fanout-ready"
                            }
                        }
                    }
                }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    // Independent reviewer cannot reuse architect-1 on either claim or submit.
    let claim_not_independent = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        claim_not_independent.contains("TASK_REVIEW_NOT_INDEPENDENT"),
        "{claim_not_independent}"
    );

    let not_independent = submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        not_independent.contains("TASK_REVIEW_NOT_INDEPENDENT"),
        "{not_independent}"
    );

    // Passing final pre-task gate eagerly fans out task slices before any context call.
    submit(
        h.root(),
        Submit {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-2",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    let fanned_out_rows: i64 = h
        .task
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM gate_transitions WHERE milestone_id=?1 AND subject=?2 AND action='advance'",
            rusqlite::params![milestone_id, h.task.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fanned_out_rows, 1);

    let total_before_context: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let ctx = manage(
        h.root(),
        Manage {
            action: ManageAction::Context,
            task_id: Some(h.task.id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let total_after_context: i64 = h
        .task
        .store
        .connection
        .query_row("SELECT count(*) FROM gate_transitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(total_before_context, total_after_context);
    assert_eq!(
        ctx["context_bundle"]["inherited_proofs"][0]["proof"]["goal"],
        "root-fanout-ready"
    );

    // Barrier 2 rejects post-task root claim until wave task completes, then allows it.
    let barrier2_err = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        barrier2_err.contains("MILESTONE_POST_TASK_BARRIER_OPEN"),
        "{barrier2_err}"
    );

    h.task.advance_to_completion().unwrap();
    let post_claim = claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "polish".into(),
            worker_id: "architect-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(post_claim["stage"], "polish");
    let post_submit = submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "polish".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {"none_proof": {}}
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    assert_eq!(post_submit["action"], "pass");
}

#[test]
fn claim_response_overflow_releases_claim_and_keeps_task_ready() {
    use contextunity_acdd::acdd_tasks::engine::{claim, submit, Action, Claim, Submit};

    let mut h = RootGateHarness::new();
    h.with_blueprint_synthesis_root_gates();
    let worktree = h.root().to_string_lossy().into_owned();
    let milestone_id = "m-wave".to_owned();

    claim(
        h.root(),
        Claim {
            task_id: milestone_id.clone(),
            stage: "blueprint_synthesis.synthesize".into(),
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
            stage: "blueprint_synthesis.synthesize".into(),
            evidence: serde_json::json!({
                "worker_id": "architect-1",
                "worktree": worktree,
                "proof": {
                    "scheme_proof": {
                        "tasks": {
                            "first": { "padding": "A".repeat(2048) }
                        }
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
            stage: "blueprint_synthesis.blueprint_review".into(),
            worker_id: "reviewer-1".into(),
            worktree: worktree.clone(),
            bundle: None,
            session_fingerprint: None,
        },
    )
    .unwrap();
    submit(
        h.root(),
        Submit {
            task_id: milestone_id,
            stage: "blueprint_synthesis.blueprint_review".into(),
            evidence: serde_json::json!({
                "worker_id": "reviewer-1",
                "worktree": worktree,
                "proof": { "none_proof": {} }
            }),
            action: Action::Pass,
            findings: None,
            session_fingerprint: None,
        },
    )
    .unwrap();

    // Lower configured response budget below the inherited proof size to force TASK_RESPONSE_OVERFLOW.
    let cfg_path = h.root().join("acdd.yaml");
    let mut cfg = fs::read_to_string(&cfg_path).unwrap();
    cfg.push_str("\nresponse:\n  max_output_bytes: 1024\n");
    fs::write(&cfg_path, cfg).unwrap();

    let first_err = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: "contract".into(),
            worker_id: "worker-1".into(),
            worktree: worktree.clone(),
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(first_err.contains("TASK_RESPONSE_OVERFLOW"), "{first_err}");

    let task = h.task.store.inspect(&h.task.id).unwrap();
    assert_eq!(task.status, "ready");
    assert!(task.worker_id.is_none());

    let second_err = claim(
        h.root(),
        Claim {
            task_id: h.task.id.clone(),
            stage: "contract".into(),
            worker_id: "worker-2".into(),
            worktree,
            bundle: Some(true),
            session_fingerprint: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        second_err.contains("TASK_RESPONSE_OVERFLOW"),
        "{second_err}"
    );
}
