use super::helpers::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::NodeCursor;
use acdd_state::types::{
    ArtifactKind, BlackboardTopic, FindingItem, FindingSeverity, IsolationMode, NodeStatus,
    RejectionFindings, RejectionKind, ReviewContourResult, ReviewEvidence, SubmitAction,
};
use std::collections::BTreeMap;

fn evaluate_submit_plan(
    snapshot: &acdd_state::types::RunGraphSnapshot,
    profile: &acdd_state::machine::MachineProfileInput,
    obs: &acdd_state::types::ValidatedObservations,
    input: &SubmitInput,
) -> Result<acdd_state::types::TransitionPlan, acdd_core::errors::AcddError> {
    match evaluate_submit(snapshot, profile, obs, input)? {
        SubmitEvaluation::Apply(plan) => Ok(plan),
        SubmitEvaluation::MarkStaleAndReject { error, .. } => Err(error),
    }
}

#[test]
fn submit_independence_immutable_need_drift_reject_escalation_and_receipt_closure() {
    let (profile, mut snap) =
        RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let t_a = tree('a');
    let t_b = tree('b');
    let obs_a = obs_with_tree(t_a.clone());

    // Claim & verify T1.build by builder-1
    let c1 = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w("builder-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000001".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c1);

    // Missing trace fails with TASK_PROOF_UNOBSERVED.
    assert_eq!(
        evaluate_submit_plan(
            &snap,
            &profile,
            &obs_a,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
                action: SubmitAction::Pass,
                worker_id: w("builder-1"),
                claim_revision: Some(1),
                expected_execution_digest: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskProofUnobserved
    );

    let v1 = evaluate_verify(
        &snap,
        &profile,
        &obs_a,
        &VerifyInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: Some(w("builder-1")),
            claim_revision: Some(1),
            test_target: None,
            command_id: Some("build_check".to_owned()),
            command_line: Some("cargo test".to_owned()),
            resolved_commands: vec!["cargo test".to_owned()],
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/b1.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: Some(0),
                stderr_tail: String::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a.clone()),
            },
        },
    )
    .unwrap();
    record_verify_in_memory(&mut snap, &v1);

    let p1 = evaluate_submit_plan(
        &snap,
        &profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            action: SubmitAction::Pass,
            worker_id: w("builder-1"),
            claim_revision: Some(1),
            expected_execution_digest: Some(v1.execution_digest),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p1);

    // 1. Self-review on `review` triggers TASK_REVIEW_NOT_INDEPENDENT.
    let c_r_bad = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            worker_id: w("builder-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000002".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_r_bad);
    let rev_ev = ReviewEvidence {
        decision: SubmitAction::Pass,
        contours: BTreeMap::from([
            (
                "paths".to_owned(),
                ReviewContourResult {
                    applicable: true,
                    evidence: "ok".to_owned(),
                },
            ),
            (
                "claims".to_owned(),
                ReviewContourResult {
                    applicable: true,
                    evidence: "ok".to_owned(),
                },
            ),
        ]),
        summary: "ok".to_owned(),
        notes: None,
    };
    assert_eq!(
        evaluate_submit_plan(
            &snap,
            &profile,
            &obs_a,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
                action: SubmitAction::Pass,
                worker_id: w("builder-1"),
                claim_revision: Some(1),
                expected_execution_digest: None,
                review_evidence: Some(rev_ev.clone()),
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskReviewNotIndependent
    );

    // Takeover by independent reviewer
    let c_r_ok = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            worker_id: w("rev-2"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000003".to_owned(),
            force: true,
            steal_remote: true,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_r_ok);

    // 2. When both `immutable: true` and `need:` drift simultaneously, `evaluate_submit` emits
    // `MarkStaleAndReject` BEFORE failing on `immutable` so drifted `need:` predecessors don't stay Completed.
    let obs_b = obs_with_tree(t_b.clone());
    let both_drift = evaluate_submit(
        &snap,
        &profile,
        &obs_b,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-2"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: Some(rev_ev.clone()),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    match both_drift {
        SubmitEvaluation::MarkStaleAndReject {
            stale_node_ids,
            error,
            ..
        } => {
            assert_eq!(stale_node_ids, vec!["task:T1:build".to_owned()]);
            assert_eq!(error.code, ErrorCode::TaskCandidateMismatch);
        }
        other => {
            panic!("expected MarkStaleAndReject when both need: and immutable drift, got {other:?}")
        }
    }

    // When `need:` matches `t_b` while `claim_tree` is still `t_a`, `immutable: true` still rejects directly.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:build")
        .unwrap()
        .accepted_tree = Some(t_b.clone());
    assert_eq!(
        evaluate_submit_plan(
            &snap,
            &profile,
            &obs_b,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
                action: SubmitAction::Pass,
                worker_id: w("rev-2"),
                claim_revision: Some(2),
                expected_execution_digest: None,
                review_evidence: Some(rev_ev.clone()),
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskCandidateMismatch
    );
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:build")
        .unwrap()
        .accepted_tree = Some(t_a.clone());

    // 3. `need:` drift when claim_tree == current_tree == t_b while predecessor `build` had `accepted_tree == t_a`:
    // produces `MarkStaleAndReject { stale_node_ids: ["task:T1:build"], ... }`.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:review")
        .unwrap()
        .claim_tree = Some(t_b.clone());
    let need_drift = evaluate_submit(
        &snap,
        &profile,
        &obs_b,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-2"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: Some(rev_ev.clone()),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    match need_drift {
        SubmitEvaluation::MarkStaleAndReject {
            stale_node_ids,
            error,
            ..
        } => {
            assert_eq!(stale_node_ids, vec!["task:T1:build".to_owned()]);
            assert_eq!(error.code, ErrorCode::TaskCandidateMismatch);
        }
        other => panic!("expected MarkStaleAndReject, got {other:?}"),
    }

    // Restore claim_tree to t_a before 3x reject -> Escalated!
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:review")
        .unwrap()
        .claim_tree = Some(t_a.clone());

    // Review reject 3 times -> Escalated
    for attempt in 1..=3 {
        let c_r = admit_claim(
            &snap,
            &profile,
            &obs_a,
            &ClaimInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
                worker_id: w("rev-2"),
                session_fingerprint: None,
                worktree: "/repo/.worktrees/T1".to_owned(),
                branch: Some("task/T1".to_owned()),
                baseline_commit: Some(commit('1')),
                claim_tree: t_a.clone(),
                span_id: format!("100000000000010{attempt}"),
                force: false,
                steal_remote: false,
                previous_local_pid_alive: None,
            },
        )
        .unwrap();
        apply_claim_in_memory(&mut snap, &c_r);
        let rej = evaluate_submit_plan(
            &snap,
            &profile,
            &obs_a,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
                action: SubmitAction::Reject,
                worker_id: w("rev-2"),
                claim_revision: None,
                expected_execution_digest: None,
                review_evidence: None,
                rejection_findings: Some(RejectionFindings {
                    kind: RejectionKind::Defect,
                    summary: "needs fix".to_owned(),
                    blackboard_message_id: Some(7),
                    items: vec![FindingItem {
                        severity: FindingSeverity::High,
                        path: Some("crates/a/src/lib.rs".to_owned()),
                        message: "missing guard".to_owned(),
                        remediation: "add guard".to_owned(),
                    }],
                }),
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap();
        apply_plan_in_memory(&mut snap, &rej);
        if attempt < 3 {
            // Re-claim & re-pass build for next cycle
            snap.nodes
                .iter_mut()
                .find(|n| n.node_id == "task:T1:build")
                .unwrap()
                .status = NodeStatus::Completed;
            snap.nodes
                .iter_mut()
                .find(|n| n.node_id == "task:T1:build")
                .unwrap()
                .accepted_tree = Some(t_a.clone());
            snap.nodes
                .iter_mut()
                .find(|n| n.node_id == "task:T1:review")
                .unwrap()
                .status = NodeStatus::Ready;
        }
    }
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T1:review")
            .unwrap()
            .status,
        NodeStatus::Escalated
    );

    // Reopen review, pass review, then deliver (testing subtask missing evidence failure first!).
    let TransitionOutcome::Plan(reopen_rev) = evaluate_transition(
        &snap,
        &profile,
        &obs_a,
        &TransitionInput::Reopen(ReopenInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            cascade: false,
            reason: None,
        }),
    )
    .unwrap() else {
        panic!("expected transition plan");
    };
    apply_plan_in_memory(&mut snap, &reopen_rev);
    let c_r = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            worker_id: w("rev-2"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000099".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_r);
    let p_r = evaluate_submit_plan(
        &snap,
        &profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-2"),
            claim_revision: None,
            expected_execution_digest: None,
            review_evidence: Some(rev_ev),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p_r);

    // Claim deliver
    let c_d = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("deliver")]),
            worker_id: w("rev-2"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000100".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_d);

    // Subtask without evidence blocks deliver!
    snap.subtasks[0].evidence = Some("   ".to_owned());
    assert_eq!(
        evaluate_submit_plan(
            &snap,
            &profile,
            &obs_a,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("deliver")]),
                action: SubmitAction::Pass,
                worker_id: w("rev-2"),
                claim_revision: Some(1),
                expected_execution_digest: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: vec!["INV-1".to_owned()],
                snapshot_commit: Some(commit('9')),
                summary: Some("delivered".to_owned()),
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::GateVerificationFailed
    );

    // Restore evidence and deliver -> marks task T1 Completed, satisfied = true, unblocks T2!
    snap.subtasks[0].evidence = Some("verified".to_owned());
    let p_d = evaluate_submit_plan(
        &snap,
        &profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("deliver")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-2"),
            claim_revision: Some(1),
            expected_execution_digest: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: vec!["INV-1".to_owned()],
            snapshot_commit: Some(commit('9')),
            summary: Some("delivered".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p_d);

    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T1")
            .unwrap()
            .status,
        NodeStatus::Completed
    );
    assert!(
        snap.dependencies
            .iter()
            .find(|d| d.task_ref == t("T2"))
            .unwrap()
            .satisfied
    );
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T2")
            .unwrap()
            .status,
        NodeStatus::Ready
    );
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T2:build")
            .unwrap()
            .status,
        NodeStatus::Ready
    );
    assert!(snap
        .artifacts
        .iter()
        .any(|a| a.node_id == "task:T1:deliver" && a.kind == ArtifactKind::Receipt));

    // Verify blackboard cleanup helper (notes/draft -> Resolved, deferred -> CarriedOver).
    let bb_items = vec![
        BlackboardRecord {
            id: Some(1),
            run_id: snap.run.run_id.clone(),
            node_id: Some("task:T1:build".to_owned()),
            recipient_node_id: None,
            subtask_ref: None,
            gate: None,
            author: "b".to_owned(),
            topic: BlackboardTopic::Notes,
            status: BlackboardStatus::Open,
            payload: "note".to_owned(),
            created_at: "t".to_owned(),
        },
        BlackboardRecord {
            id: Some(2),
            run_id: snap.run.run_id.clone(),
            node_id: Some("task:T1".to_owned()),
            recipient_node_id: None,
            subtask_ref: None,
            gate: None,
            author: "b".to_owned(),
            topic: BlackboardTopic::Deferred,
            status: BlackboardStatus::Open,
            payload: "defer".to_owned(),
            created_at: "t".to_owned(),
        },
    ];
    let bb_patches = plan_container_blackboard_cleanup(&snap, "task:T1", &bb_items);
    assert_eq!(bb_patches.len(), 2);
    assert_eq!(bb_patches[0].status, BlackboardStatus::Resolved);
    assert_eq!(bb_patches[1].status, BlackboardStatus::CarriedOver);

    // Verify `architectural_drift -> rearchitect -> build` resets stale `build`/`review` and
    // unused `rearchitect` becomes `Cancelled` when `deliver` completes the task.
    let mut drift_profile = RunHarness::canonical_machine_profile(IsolationMode::Worktree);
    drift_profile.gates.remove(0);
    if let MachineGraphItem::TasksBlock(ref mut tb) = drift_profile.gates[0] {
        tb.gates[1].fail_drift = Some("rearchitect".to_owned());
        tb.gates.push(MachineGateDef {
            role: Some("architect".to_owned()),
            pass: Some("build".to_owned()),
            max_attempts: 3,
            ..MachineGateDef::minimal(g("rearchitect"))
        });
    }
    let drift_run = RunId::new("run-drift").unwrap();
    let drift_input = BuildGraphInput {
        run_id: drift_run.clone(),
        run_name: RunName::new("AC-102").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "/repo".to_owned(),
        spec_path: Some("docs/milestones/AC-102.md".to_owned()),
        is_ephemeral: false,
        target_base: Some("main".to_owned()),
        target_base_commit: Some(commit('1')),
        branch: Some("milestone/AC-102".to_owned()),
        worktree: Some("/repo/.worktrees/AC-102".to_owned()),
        trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
        created_at: "2026-11-17T09:00:00.000Z".to_owned(),
        spec: MachineSpecInput {
            tasks: vec![
                make_task("T1", &["code"], &["crates/a/**"], &[], &drift_run),
                make_task("T2", &["code"], &["crates/b/**"], &[], &drift_run),
            ],
            spec_digest: "drift-sha".to_owned(),
        },
    };
    let mut d_snap = build_initial_run_graph(&drift_profile, &drift_input).unwrap();
    // Simulate completed build & active review on T1, then reject with ArchitecturalDrift.
    {
        let build_n = d_snap
            .nodes
            .iter_mut()
            .find(|n| n.node_id == "task:T1:build")
            .unwrap();
        build_n.status = NodeStatus::Completed;
        build_n.accepted_tree = Some(t_a.clone());
        build_n.accepted_definition_digest = Some("def:build".to_owned());
        build_n.worker_id = Some(w("b1"));
        d_snap
            .nodes
            .iter_mut()
            .find(|n| n.node_id == "task:T1:review")
            .unwrap()
            .status = NodeStatus::Ready;
    }
    let c_dr = admit_claim(
        &d_snap,
        &drift_profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            worker_id: w("rev-drift"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000200".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut d_snap, &c_dr);
    let rej_drift = evaluate_submit_plan(
        &d_snap,
        &drift_profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            action: SubmitAction::Reject,
            worker_id: w("rev-drift"),
            claim_revision: Some(1),
            expected_execution_digest: None,
            review_evidence: None,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::ArchitecturalDrift,
                summary: "shadow resolver introduced".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::High,
                    path: Some("crates/a/src/lib.rs".to_owned()),
                    message: "bypassed canonical owner".to_owned(),
                    remediation: "rearchitect around canonical owner".to_owned(),
                }],
            }),
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &rej_drift);

    let findings_art = d_snap
        .artifacts
        .iter()
        .find(|a| a.node_id == "task:T1:review" && a.kind == ArtifactKind::Findings)
        .unwrap();
    let f_json: serde_json::Value = serde_json::from_str(&findings_art.payload_json).unwrap();
    assert_eq!(f_json["kind"], "architectural_drift");
    assert_eq!(f_json["reset_worker_context"], true);
    assert_eq!(f_json["reset_gate_id"], "build");
    assert_eq!(
        d_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task:T1:rearchitect")
            .unwrap()
            .status,
        NodeStatus::Ready
    );
    let reset_build = d_snap
        .nodes
        .iter()
        .find(|n| n.node_id == "task:T1:build")
        .unwrap();
    assert_eq!(reset_build.status, NodeStatus::Pending);
    assert_eq!(reset_build.accepted_tree, None);
    assert_eq!(reset_build.accepted_definition_digest, None);
    assert_eq!(reset_build.worker_id, None);
    assert_eq!(reset_build.session_fingerprint, None);
    assert_eq!(reset_build.active_span_id, None);
    assert_eq!(
        d_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task:T1:review")
            .unwrap()
            .status,
        NodeStatus::Pending
    );

    // Passing `rearchitect` advances `pass: build` to Ready.
    let c_arch = admit_claim(
        &d_snap,
        &drift_profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("rearchitect")]),
            worker_id: w("arch-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000201".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut d_snap, &c_arch);
    let pass_arch = evaluate_submit_plan(
        &d_snap,
        &drift_profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("rearchitect")]),
            action: SubmitAction::Pass,
            worker_id: w("arch-1"),
            claim_revision: Some(1),
            expected_execution_digest: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: Some("rearchitected".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &pass_arch);
    assert_eq!(
        d_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task:T1:build")
            .unwrap()
            .status,
        NodeStatus::Ready
    );

    // Happy-path deliver on T2 cancels unused Pending `rearchitect`.
    {
        for gid in ["build", "review"] {
            let n = d_snap
                .nodes
                .iter_mut()
                .find(|n| n.node_id == format!("task:T2:{gid}"))
                .unwrap();
            n.status = NodeStatus::Completed;
            n.accepted_tree = Some(t_a.clone());
            if gid == "build" {
                n.worker_id = Some(w("b2"));
            }
        }
        d_snap
            .nodes
            .iter_mut()
            .find(|n| n.node_id == "task:T2:deliver")
            .unwrap()
            .status = NodeStatus::Ready;
    }
    let c_d2 = admit_claim(
        &d_snap,
        &drift_profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T2"), vec![g("deliver")]),
            worker_id: w("rev-t2"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T2".to_owned(),
            branch: Some("task/T2".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "1000000000000202".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut d_snap, &c_d2);
    let pass_d2 = evaluate_submit_plan(
        &d_snap,
        &drift_profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T2"), vec![g("deliver")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-t2"),
            claim_revision: Some(1),
            expected_execution_digest: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: Some(commit('8')),
            summary: Some("delivered".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &pass_d2);
    assert_eq!(
        d_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task:T2:rearchitect")
            .unwrap()
            .status,
        NodeStatus::Cancelled
    );

    // ArchitecturalDrift when `fail_drift` sibling (`rearchitect`) was excluded by `when:` falls back to `fail` (`build`).
    d_snap.nodes.retain(|n| n.node_id != "task:T1:rearchitect");
    d_snap.claims.retain(|c| c.node_id != "task:T1:rearchitect");
    d_snap
        .artifacts
        .retain(|a| a.node_id != "task:T1:rearchitect");
    d_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:build")
        .unwrap()
        .status = NodeStatus::Completed;
    d_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:build")
        .unwrap()
        .accepted_tree = Some(t_a.clone());
    d_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:review")
        .unwrap()
        .status = NodeStatus::Ready;
    let c_rev_fallback = admit_claim(
        &d_snap,
        &drift_profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            worker_id: w("rev-drift-fb"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a,
            span_id: "1000000000000203".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut d_snap, &c_rev_fallback);
    let rej_fallback = evaluate_submit_plan(
        &d_snap,
        &drift_profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            action: SubmitAction::Reject,
            worker_id: w("rev-drift-fb"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: None,
            rejection_findings: Some(RejectionFindings {
                kind: RejectionKind::ArchitecturalDrift,
                summary: "drift without rearchitect sibling".to_owned(),
                blackboard_message_id: None,
                items: vec![FindingItem {
                    severity: FindingSeverity::High,
                    path: Some("crates/a/src/lib.rs".to_owned()),
                    message: "bypassed owner".to_owned(),
                    remediation: "reset and rebuild".to_owned(),
                }],
            }),
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &rej_fallback);
    assert_eq!(
        d_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task:T1:build")
            .unwrap()
            .status,
        NodeStatus::Ready
    );
    let fb_art = d_snap
        .artifacts
        .iter()
        .filter(|a| a.node_id == "task:T1:review" && a.kind == ArtifactKind::Findings)
        .max_by_key(|a| a.claim_revision)
        .unwrap();
    let fb_stored: serde_json::Value = serde_json::from_str(&fb_art.payload_json).unwrap();
    assert_eq!(fb_stored["reset_worker_context"], serde_json::json!(true));
    assert_eq!(fb_stored["reset_gate_id"], serde_json::json!("build"));
}
