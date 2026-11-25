use super::helpers::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{NodeCursor, RunName};
use acdd_state::machine::verify::ExecutionDigestInput;
use acdd_state::types::{CheckResultRecord, IsolationMode, NodeKind, NodeStatus, TraceStatus};
use serde_json::json;

#[test]
fn graph_construction_node_ids_and_when_filtering() {
    let allowed = BTreeSet::from(["code".to_owned(), "docs".to_owned()]);
    assert_eq!(
        effective_task_tags(&[], &allowed).unwrap(),
        vec!["default".to_owned()]
    );
    assert_eq!(
        effective_task_tags(&["nope".to_owned()], &allowed)
            .unwrap_err()
            .code,
        ErrorCode::ProfileSchemaInvalid
    );
    let (mut profile, _) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, true);
    // Add `when: "code"` to `build` gate so `T3` (`docs`) skips `build`.
    if let MachineGraphItem::TasksBlock(ref mut block) = profile.gates[1] {
        block.gates[0].when = Some("code".to_owned());
    }
    let run_id = RunId::new("run-when").unwrap();
    let input = BuildGraphInput {
        run_id: run_id.clone(),
        run_name: RunName::new("AC-101").unwrap(),
        collection: None,
        workspace: "/repo".to_owned(),
        spec_path: None,
        is_ephemeral: true,
        target_base: None,
        target_base_commit: None,
        branch: None,
        worktree: None,
        trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
        created_at: "2026-11-17T09:00:00.000Z".to_owned(),
        spec: MachineSpecInput {
            tasks: vec![
                make_task("T1", &["code"], &["crates/a/**"], &[], &run_id),
                make_task("T3", &["docs"], &["docs/**"], &[], &run_id),
            ],
            spec_digest: "ephemeral".to_owned(),
        },
    };
    let snap = build_initial_run_graph(&profile, &input).unwrap();
    let ids: BTreeSet<&str> = snap.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert!(ids.contains("root:blueprint_synthesis"));
    assert!(ids.contains("root:blueprint_synthesis.synthesize"));
    assert!(ids.contains("root:blueprint_synthesis.review"));
    assert!(ids.contains("tasks#0"));
    assert!(ids.contains("task:T1"));
    assert!(ids.contains("task:T1:build"));
    assert!(ids.contains("task:T3"));
    assert!(!ids.contains("task:T3:build"));
    assert!(ids.contains("task:T3:review"));
    assert!(ids.contains("root:verify"));

    // Pre-task gate starts Ready; tasks start Pending behind pre-task barrier.
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "root:blueprint_synthesis.synthesize")
            .unwrap()
            .status,
        NodeStatus::Ready
    );
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T1")
            .unwrap()
            .status,
        NodeStatus::Pending
    );

    // Cycle in task_dependencies must fail closed.
    let mut bad_input = input.clone();
    bad_input.spec.tasks[0].depends_on.push(MachineTaskDep {
        depends_on_workspace: "/repo".to_owned(),
        depends_on_run_id: run_id.clone(),
        depends_on_ref: t("T3"),
    });
    bad_input.spec.tasks[1].depends_on.push(MachineTaskDep {
        depends_on_workspace: "/repo".to_owned(),
        depends_on_run_id: run_id,
        depends_on_ref: t("T1"),
    });
    assert_eq!(
        build_initial_run_graph(&profile, &bad_input)
            .unwrap_err()
            .code,
        ErrorCode::ProfileSchemaInvalid
    );
}

#[test]
fn schedule_waves_scope_overlap_and_in_place_constraints() {
    // 1. Pure pattern overlap matrix.
    let overlap_cases: &[(&str, &str, bool)] = &[
        ("crates/a/**", "crates/b/**", false),
        ("crates/a", "crates/a/src/lib.rs", true),
        ("crates/a", "crates/ab/src/lib.rs", false),
        ("crates/a/*.rs", "crates/a/*.md", false),
        ("crates/a/*.rs", "crates/a/foo.*", true),
        ("**", "Cargo.toml", true),
    ];
    for &(a, b, expected) in overlap_cases {
        assert_eq!(
            acdd_core::path_policy::patterns_overlap(a, b),
            expected,
            "patterns_overlap({a:?}, {b:?})"
        );
    }

    // 2. Worktree wave schedule vs overlapping tasks.
    let (_, mut wt_snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    // Make T3 overlap T1's scope so T1 and T3 split across wave 0 and wave 1!
    wt_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T3")
        .unwrap()
        .scope = vec!["crates/a/docs.md".to_owned()];
    let obs = obs_with_tree(tree('a'));
    let wt_sched = compute_schedule(&wt_snap, &obs).unwrap();
    assert!(wt_sched.pre_task_satisfied);
    assert_eq!(wt_sched.waves.len(), 2);
    assert_eq!(wt_sched.waves[0], vec![t("T1")]);
    assert_eq!(wt_sched.waves[1], vec![t("T2"), t("T3")]);

    // 3. In-place isolation caps every wave at 1 task.
    let (_, ip_snap) = RunHarness::build_machine_sample_graph(IsolationMode::InPlace, false);
    let ip_sched = compute_schedule(&ip_snap, &obs).unwrap();
    assert!(ip_sched.waves.iter().all(|w| w.len() == 1));
    assert_eq!(ip_sched.waves.len(), 3);

    // 4. Post-task integration barrier returns TASK_NOT_INTEGRATED when unintegrated_tasks non-empty.
    for n in &mut wt_snap.nodes {
        if n.kind == NodeKind::Task || n.kind == NodeKind::TaskGate {
            n.status = NodeStatus::Completed;
        }
    }
    let mut unint_obs = obs.clone();
    unint_obs.unintegrated_tasks = vec![t("T1")];
    let blocked_sched = compute_schedule(&wt_snap, &unint_obs).unwrap();
    assert_eq!(
        blocked_sched.post_task_blocked_reason,
        Some(ErrorCode::TaskNotIntegrated)
    );
    assert_eq!(
        check_post_task_integration_barrier(&wt_snap, &unint_obs)
            .unwrap_err()
            .code,
        ErrorCode::TaskNotIntegrated
    );
}

#[test]
fn claim_idempotency_force_takeover_in_place_and_scope_conflict_matrix() {
    let (profile, mut snap) =
        RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let obs = obs_with_tree(tree('a'));

    let claim_t1 = ClaimInput {
        node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
        worker_id: w("worker-1"),
        session_fingerprint: Some("host:100:1".to_owned()),
        worktree: "/repo/.worktrees/T1".to_owned(),
        branch: Some("task/T1".to_owned()),
        baseline_commit: Some(commit('1')),
        claim_tree: tree('a'),
        span_id: "0011223344556677".to_owned(),
        force: false,
        steal_remote: false,
        previous_local_pid_alive: None,
    };
    let d1 = admit_claim(&snap, &profile, &obs, &claim_t1).unwrap();
    apply_claim_in_memory(&mut snap, &d1);

    struct ClaimCase<'a> {
        label: &'a str,
        worker: &'a str,
        fp: Option<&'a str>,
        worktree: &'a str,
        force: bool,
        steal: bool,
        pid_alive: Option<bool>,
        expected: Result<bool, ErrorCode>,
    }

    let cases = [
        ClaimCase {
            label: "idempotent_same",
            worker: "worker-1",
            fp: Some("host:100:1"),
            worktree: "/repo/.worktrees/T1",
            force: false,
            steal: false,
            pid_alive: None,
            expected: Ok(true),
        },
        ClaimCase {
            label: "other_worker_no_force",
            worker: "worker-2",
            fp: Some("host:200:2"),
            worktree: "/repo/.worktrees/T1",
            force: false,
            steal: false,
            pid_alive: None,
            expected: Err(ErrorCode::TaskAlreadyClaimed),
        },
        ClaimCase {
            label: "other_worker_force_live_pid",
            worker: "worker-2",
            fp: Some("host:200:2"),
            worktree: "/repo/.worktrees/T1",
            force: true,
            steal: false,
            pid_alive: Some(true),
            expected: Err(ErrorCode::TaskAlreadyClaimed),
        },
        ClaimCase {
            label: "same_worker_force_new_fp",
            worker: "worker-1",
            fp: Some("host:101:2"),
            worktree: "/repo/.worktrees/T1",
            force: true,
            steal: false,
            pid_alive: Some(true),
            expected: Ok(false),
        },
        ClaimCase {
            label: "other_worker_dead_pid",
            worker: "worker-2",
            fp: Some("host:200:2"),
            worktree: "/repo/.worktrees/T1",
            force: true,
            steal: false,
            pid_alive: Some(false),
            expected: Ok(false),
        },
        ClaimCase {
            label: "other_worker_steal_remote",
            worker: "worker-3",
            fp: Some("remote:999:9"),
            worktree: "/repo/.worktrees/T1",
            force: true,
            steal: true,
            pid_alive: None,
            expected: Ok(false),
        },
    ];

    for case in cases {
        let req = ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w(case.worker),
            session_fingerprint: case.fp.map(str::to_owned),
            worktree: case.worktree.to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: tree('a'),
            span_id: "1122334455667788".to_owned(),
            force: case.force,
            steal_remote: case.steal,
            previous_local_pid_alive: case.pid_alive,
        };
        let res = admit_claim(&snap, &profile, &obs, &req);
        match case.expected {
            Ok(expect_idemp) => {
                let dec = res.unwrap_or_else(|e| panic!("{}: unexpected err {e}", case.label));
                assert_eq!(
                    matches!(dec, ClaimDecision::Idempotent(_)),
                    expect_idemp,
                    "{}",
                    case.label
                );
            }
            Err(code) => {
                assert_eq!(res.unwrap_err().code, code, "{}", case.label);
            }
        }
    }

    // Scope overlap against already-claimed unfinished task T1 fails with TASK_SCOPE_CONFLICT.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T3")
        .unwrap()
        .scope = vec!["crates/a/sub/**".to_owned()];
    let claim_t3 = ClaimInput {
        node_cursor: NodeCursor::task(t("T3"), vec![g("build")]),
        worker_id: w("worker-2"),
        session_fingerprint: None,
        worktree: "/repo/.worktrees/T3".to_owned(),
        branch: None,
        baseline_commit: None,
        claim_tree: tree('a'),
        span_id: "2233445566778899".to_owned(),
        force: false,
        steal_remote: false,
        previous_local_pid_alive: None,
    };
    assert_eq!(
        admit_claim(&snap, &profile, &obs, &claim_t3)
            .unwrap_err()
            .code,
        ErrorCode::TaskScopeConflict
    );

    // `in_place` single-active constraint rejects claiming T3 while T1 is InProgress.
    let (ip_profile, mut ip_snap) =
        RunHarness::build_machine_sample_graph(IsolationMode::InPlace, false);
    let ip_claim_t1 = ClaimInput {
        worktree: "/repo".to_owned(),
        branch: None,
        ..claim_t1
    };
    let d_ip = admit_claim(&ip_snap, &ip_profile, &obs, &ip_claim_t1).unwrap();
    apply_claim_in_memory(&mut ip_snap, &d_ip);
    let ip_claim_t3 = ClaimInput {
        worktree: "/repo".to_owned(),
        branch: None,
        ..claim_t3
    };
    assert_eq!(
        admit_claim(&ip_snap, &ip_profile, &obs, &ip_claim_t3)
            .unwrap_err()
            .code,
        ErrorCode::TaskAlreadyClaimed
    );

    // Pre-task barrier error names the exact pending pre-task gate cursor and suggests claiming it next.
    let (pre_profile, pre_snap) =
        RunHarness::build_machine_sample_graph(IsolationMode::Worktree, true);
    let pre_err = admit_claim(
        &pre_snap,
        &pre_profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w("worker-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: None,
            baseline_commit: None,
            claim_tree: tree('a'),
            span_id: "3344556677889900".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap_err();
    assert_eq!(pre_err.code, ErrorCode::GateVerificationFailed);
    assert!(
        pre_err.message.contains("blueprint_synthesis.synthesize"),
        "expected pending pre-task cursor in error: {}",
        pre_err.message
    );
    assert!(
        pre_err
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("blueprint_synthesis.synthesize")),
        "expected claim hint for pre-task cursor: {:?}",
        pre_err.hint
    );
}

#[test]
fn verify_command_scope_and_mechanical_cycle_guard_matrix() {
    let (profile, mut snap) =
        RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let t_a = tree('a');
    let obs = obs_with_tree(t_a.clone());

    // Claim T1.build
    let claim = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w("builder-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "aaaaaaaaaaaaaaaa".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &claim);

    struct CommandCase<'a> {
        label: &'a str,
        exit_code: Option<i32>,
        timed_out: bool,
        aborted: bool,
        changed: &'a [&'a str],
        exp_trace: TraceStatus,
        exp_err: Option<ErrorCode>,
    }

    let command_cases = [
        CommandCase {
            label: "valid_zero_exit",
            exit_code: Some(0),
            timed_out: false,
            aborted: false,
            changed: &[],
            exp_trace: TraceStatus::Passed,
            exp_err: None,
        },
        CommandCase {
            label: "nonzero_exit_fails",
            exit_code: Some(1),
            timed_out: false,
            aborted: false,
            changed: &[],
            exp_trace: TraceStatus::Failed,
            exp_err: Some(ErrorCode::GateVerificationFailed),
        },
        CommandCase {
            label: "missing_exit_fails",
            exit_code: None,
            timed_out: false,
            aborted: false,
            changed: &[],
            exp_trace: TraceStatus::Failed,
            exp_err: Some(ErrorCode::GateVerificationFailed),
        },
        CommandCase {
            label: "aborted",
            exit_code: None,
            timed_out: false,
            aborted: true,
            changed: &[],
            exp_trace: TraceStatus::Aborted,
            exp_err: Some(ErrorCode::GateVerificationFailed),
        },
        CommandCase {
            label: "timed_out",
            exit_code: None,
            timed_out: true,
            aborted: false,
            changed: &[],
            exp_trace: TraceStatus::TimedOut,
            exp_err: Some(ErrorCode::GateVerificationFailed),
        },
        CommandCase {
            label: "out_of_scope",
            exit_code: Some(0),
            timed_out: false,
            aborted: false,
            changed: &["Cargo.toml"],
            exp_trace: TraceStatus::ScopeViolation,
            exp_err: Some(ErrorCode::OutOfScopeChanges),
        },
    ];

    for case in command_cases {
        let mut case_obs = obs.clone();
        case_obs.changed_paths = case.changed.iter().map(|s| (*s).to_owned()).collect();
        let req = VerifyInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: Some(w("builder-1")),
            claim_revision: Some(1),
            test_target: Some("tests::seam".to_owned()),
            command_id: Some("build_check".to_owned()),
            command_line: Some("cargo test tests::seam".to_owned()),
            resolved_commands: vec!["cargo test tests::seam".to_owned()],
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/t1-build.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: case.timed_out,
                aborted: case.aborted,
                exit_code: case.exit_code,
                stderr_tail: String::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a.clone()),
            },
        };
        let eval = evaluate_verify(&snap, &profile, &case_obs, &req).unwrap();
        assert_eq!(eval.trace_finish.status, case.exp_trace, "{}", case.label);
        assert_eq!(
            eval.outcome.as_ref().err().map(|e| e.code),
            case.exp_err,
            "{}",
            case.label
        );
    }

    // Verify RFC-8785 execution_digest determinism regardless of JSON key order in call_with.
    let d1 = compute_execution_digest(&ExecutionDigestInput {
        contract_revision: 1,
        gate_def_digest: "d".to_owned(),
        admitted_scope: vec!["crates/a/**".to_owned()],
        resolved_commands: vec!["cargo test".to_owned()],
        test_target: Some("tests::seam".to_owned()),
        call_with: json!({"b": 2, "a": 1}),
        checks_params: json!([]),
        index_generation: Some("42".to_owned()),
        live_files_digest: Some("deadbeef".to_owned()),
    })
    .unwrap();
    let d2 = compute_execution_digest(&ExecutionDigestInput {
        contract_revision: 1,
        gate_def_digest: "d".to_owned(),
        admitted_scope: vec!["crates/a/**".to_owned()],
        resolved_commands: vec!["cargo test".to_owned()],
        test_target: Some("tests::seam".to_owned()),
        call_with: json!({"a": 1, "b": 2}),
        checks_params: json!([]),
        index_generation: Some("42".to_owned()),
        live_files_digest: Some("deadbeef".to_owned()),
    })
    .unwrap();
    assert_eq!(d1, d2);

    // Mechanical root gate `verify -> polish` increments `root_cycle_count` and escalates at `max_attempts = 3`.
    let mut root_snap = snap.clone();
    for n in &mut root_snap.nodes {
        if n.kind == NodeKind::Task || n.kind == NodeKind::TaskGate {
            n.status = NodeStatus::Completed;
        }
    }
    root_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "root:verify")
        .unwrap()
        .status = NodeStatus::Ready;
    for cycle in 1..=3u32 {
        let c_v = admit_claim(
            &root_snap,
            &profile,
            &obs,
            &ClaimInput {
                node_cursor: NodeCursor::root(vec![g("verify")]).unwrap(),
                worker_id: w("mech"),
                session_fingerprint: None,
                worktree: "/repo/.worktrees/AC-100".to_owned(),
                branch: Some("milestone/AC-100".to_owned()),
                baseline_commit: Some(commit('1')),
                claim_tree: t_a.clone(),
                span_id: format!("ddddddddddddddd{cycle}"),
                force: false,
                steal_remote: false,
                previous_local_pid_alive: None,
            },
        )
        .unwrap();
        apply_claim_in_memory(&mut root_snap, &c_v);
        let ev = evaluate_verify(
            &root_snap,
            &profile,
            &obs,
            &VerifyInput {
                node_cursor: NodeCursor::root(vec![g("verify")]).unwrap(),
                worker_id: None,
                claim_revision: None,
                test_target: None,
                command_id: Some("verify_all".to_owned()),
                command_line: Some("cargo test".to_owned()),
                resolved_commands: vec!["cargo test".to_owned()],
                action_id: None,
                index_generation: None,
                live_files_digest: None,
                log_relpath: "logs/verify.log".to_owned(),
                started_at: "2026-11-17T09:59:00.000Z".to_owned(),
                exec_obs: VerifyExecutionObservation {
                    timed_out: false,
                    aborted: false,
                    exit_code: Some(1),
                    stderr_tail: String::new(),
                    check_results: Vec::new(),
                    action_outcome: None,
                    candidate_tree_before: t_a.clone(),
                    candidate_tree_after: Some(t_a.clone()),
                },
            },
        )
        .unwrap();
        record_verify_in_memory(&mut root_snap, &ev);
        assert_eq!(root_snap.run.root_cycle_count, cycle);
        let v_node = root_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:verify")
            .unwrap();
        if cycle < 3 {
            assert_eq!(v_node.status, NodeStatus::Pending);
            assert_eq!(
                root_snap
                    .nodes
                    .iter()
                    .find(|n| n.node_id == "root:polish")
                    .unwrap()
                    .status,
                NodeStatus::Ready
            );
            // Reset polish -> verify Ready for next cycle test
            root_snap
                .nodes
                .iter_mut()
                .find(|n| n.node_id == "root:polish")
                .unwrap()
                .status = NodeStatus::Pending;
            root_snap
                .nodes
                .iter_mut()
                .find(|n| n.node_id == "root:verify")
                .unwrap()
                .status = NodeStatus::Ready;
        } else {
            assert_eq!(v_node.status, NodeStatus::Escalated);
        }
    }

    // MutatingActionUncertainOutcome check
    let mut call_profile = profile.clone();
    if let MachineGraphItem::Gate(ref mut vgate) = call_profile.gates[2] {
        vgate.proof = MachineProofKind::Call;
        vgate.call = Some("http.deploy".to_owned());
        vgate.command = None;
    }
    let mut call_snap = snap.clone();
    let v_node = call_snap
        .nodes
        .iter_mut()
        .find(|n| n.node_id == "root:verify")
        .unwrap();
    v_node.status = NodeStatus::InProgress;
    v_node.claim_revision = 1;
    v_node.worker_id = Some(w("deploy-bot"));
    let uncertain = evaluate_verify(
        &call_snap,
        &call_profile,
        &obs,
        &VerifyInput {
            node_cursor: NodeCursor::root(vec![g("verify")]).unwrap(),
            worker_id: Some(w("deploy-bot")),
            claim_revision: Some(1),
            test_target: None,
            command_id: None,
            command_line: None,
            resolved_commands: Vec::new(),
            action_id: Some("http.deploy".to_owned()),
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/deploy.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: None,
                stderr_tail: String::new(),
                check_results: vec![CheckResultRecord {
                    domain: "http".to_owned(),
                    check: "health".to_owned(),
                    passed: true,
                    summary: "ok".to_owned(),
                    diagnostics: Vec::new(),
                    execution_digest_salt: None,
                }],
                action_outcome: Some(ActionExecutionOutcome::UncertainCrash),
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a),
            },
        },
    )
    .unwrap();
    assert_eq!(uncertain.trace_finish.status, TraceStatus::Aborted);
    assert_eq!(
        uncertain.outcome.unwrap_err().code,
        ErrorCode::MutatingActionUncertainOutcome
    );
    assert!(uncertain.failure_transition.is_none());
}
