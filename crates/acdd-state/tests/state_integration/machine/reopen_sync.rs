use super::helpers::*;
use acdd_core::errors::ErrorCode;
use acdd_core::ids::NodeCursor;
use acdd_state::machine::graph::is_post_task_root_node;
use acdd_state::machine::schedule::task_dependencies_satisfied;
use acdd_state::types::{
    IsolationMode, NodeKind, NodeStatus, SubtaskStatus, TaskDependencyRecord,
};

fn plan_sync(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &SyncInput,
) -> Result<SyncPlan, acdd_core::errors::StateError> {
    let TransitionOutcome::Sync(plan) =
        evaluate_transition(snapshot, profile, obs, &TransitionInput::Sync(input.clone()))?
    else {
        unreachable!("Sync input must yield TransitionOutcome::Sync");
    };
    Ok(*plan)
}

fn plan_extend_scope(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    input: &ExtendScopeInput,
) -> Result<TransitionPlan, acdd_core::errors::StateError> {
    let TransitionOutcome::Plan(plan) = evaluate_transition(
        snapshot,
        profile,
        &ValidatedObservations::default(),
        &TransitionInput::ExtendScope(input.clone()),
    )?
    else {
        unreachable!("ExtendScope input must yield TransitionOutcome::Plan");
    };
    Ok(*plan)
}

fn plan_reopen(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
    input: &ReopenInput,
) -> Result<TransitionPlan, acdd_core::errors::StateError> {
    let (dummy_profile, _) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let TransitionOutcome::Plan(plan) = evaluate_transition(
        snapshot,
        &dummy_profile,
        obs,
        &TransitionInput::Reopen(input.clone()),
    )?
    else {
        unreachable!("Reopen input must yield TransitionOutcome::Plan");
    };
    Ok(*plan)
}

#[test]
fn extend_scope_reopen_cascade_and_sync_matrices() {
    let (profile, mut snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, true);
    let t_a = tree('a');
    let obs = obs_with_tree(t_a.clone());

    // Simulate blueprint_synthesis completion with a `tasks:` fan-out artifact.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "root:blueprint_synthesis.synthesize")
        .unwrap()
        .status = NodeStatus::Completed;
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "root:blueprint_synthesis.review")
        .unwrap()
        .status = NodeStatus::Completed;
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "root:blueprint_synthesis")
        .unwrap()
        .status = NodeStatus::Completed;
    snap.artifacts.push(acdd_state::types::GateArtifactRecord {
        run_id: snap.run.run_id.clone(),
        node_id: "root:blueprint_synthesis.synthesize".to_owned(),
        claim_revision: 1,
        kind: ArtifactKind::Proof,
        payload_json: r#"{"tasks":{"T1":{}}}"#.to_owned(),
        snapshot_commit: None,
        created_at: "2026-11-17T09:10:00.000Z".to_owned(),
    });
    for tid in ["task:T1", "task:T1:build", "task:T3", "task:T3:build"] {
        snap.nodes.iter_mut().find(|n| n.node_id == tid).unwrap().status = NodeStatus::Ready;
    }

    // 1. `sync` adding a new task T4 after pre-task `tasks:` fan-out fails with PreTaskArtifactStale.
    let mut spec_with_t4 = MachineSpecInput {
        tasks: vec![
            make_task("T1", &["code"], &["crates/a/**"], &[], &snap.run.run_id),
            make_task("T2", &["code"], &["crates/b/**"], &["T1"], &snap.run.run_id),
            make_task("T3", &["docs"], &["docs/guide/**"], &[], &snap.run.run_id),
            make_task("T4", &["docs"], &["docs/extra/**"], &[], &snap.run.run_id),
        ],
        spec_digest: "spec-v2".to_owned(),
    };
    assert_eq!(
        plan_sync(
            &snap,
            &profile,
            &obs,
            &SyncInput {
                spec: spec_with_t4.clone(),
                new_profile: None,
                rebind_profile: false,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::PreTaskArtifactStale
    );

    // 2. `extend_scope`:
    // Claim T1.build; T3 is still unclaimed (claim_revision == 0) so extending T1 into `docs/guide/**` is ALLOWED,
    // while extending into `Cargo.toml` fails with OUT_OF_SCOPE_CHANGES;
    // then claiming T3 and extending T1 into `docs/guide/foo.md` fails with TASK_SCOPE_CONFLICT!
    let c1 = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w("w1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: None,
            baseline_commit: None,
            claim_tree: t_a.clone(),
            span_id: "aaaaaaaaaaaaaaaa".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c1);

    assert_eq!(
        plan_extend_scope(
            &snap,
            &profile,
            &ExtendScopeInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
                paths: vec!["Cargo.toml".to_owned()],
                reason: "forbidden".to_owned(),
                worker_id: Some(w("w1")),
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::OutOfScopeChanges
    );

    let ext_ok = plan_extend_scope(
        &snap,
        &profile,
        &ExtendScopeInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            paths: vec!["crates/shared/**".to_owned()],
            reason: "shared helper".to_owned(),
            worker_id: Some(w("w1")),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &ext_ok);
    assert_eq!(
        snap.nodes
            .iter()
            .find(|n| n.node_id == "task:T1:build")
            .unwrap()
            .contract_revision,
        2
    );

    let c3 = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T3"), vec![g("build")]),
            worker_id: w("w3"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T3".to_owned(),
            branch: None,
            baseline_commit: None,
            claim_tree: t_a,
            span_id: "bbbbbbbbbbbbbbbb".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c3);

    assert_eq!(
        plan_extend_scope(
            &snap,
            &profile,
            &ExtendScopeInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
                paths: vec!["docs/guide/page.md".to_owned()],
                reason: "collides with claimed T3".to_owned(),
                worker_id: Some(w("w1")),
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskScopeConflict
    );

    // 2b. `#459`: `extend_scope` supports non-terminal `root:polish`, rejects other `RootGate`s and `protected_paths`, and rejects terminal `root:polish`.
    let mut polish_snap = snap.clone();
    if let Some(existing) = polish_snap.nodes.iter_mut().find(|n| n.node_id == "root:polish") {
        existing.status = NodeStatus::Ready;
        existing.scope = vec!["docs/**".to_owned(), "ROADMAP.md".to_owned(), "TESTS.md".to_owned()];
    } else {
        let mut polish_node = polish_snap.nodes.iter().find(|n| n.node_id == "root:handoff").unwrap().clone();
        polish_node.node_id = "root:polish".to_owned();
        polish_node.gate_id = "polish".to_owned();
        polish_node.status = NodeStatus::Ready;
        polish_node.scope = vec!["docs/**".to_owned(), "ROADMAP.md".to_owned(), "TESTS.md".to_owned()];
        polish_snap.nodes.push(polish_node);
    }

    let polish_ext = plan_extend_scope(
        &polish_snap,
        &profile,
        &ExtendScopeInput {
            node_cursor: NodeCursor::root(vec![g("polish")]).unwrap(),
            paths: vec!["crates/polish-extra.md".to_owned()],
            reason: "polish extra".to_owned(),
            worker_id: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut polish_snap, &polish_ext);
    let updated_polish = polish_snap.nodes.iter().find(|n| n.node_id == "root:polish").unwrap();
    assert_eq!(updated_polish.contract_revision, 2);
    assert!(acdd_state::machine::container_scope(&polish_snap, updated_polish).contains(&"crates/polish-extra.md".to_owned()));

    for (cursor, path, exp_code) in [
        (NodeCursor::root(vec![g("handoff")]).unwrap(), "docs/x.md", ErrorCode::ProfileSchemaInvalid),
        (NodeCursor::root(vec![g("polish")]).unwrap(), "Cargo.toml", ErrorCode::OutOfScopeChanges),
    ] {
        assert_eq!(
            plan_extend_scope(
                &polish_snap,
                &profile,
                &ExtendScopeInput {
                    node_cursor: cursor,
                    paths: vec![path.to_owned()],
                    reason: "negative check".to_owned(),
                    worker_id: None,
                },
            )
            .unwrap_err()
            .code,
            exp_code
        );
    }
    polish_snap.nodes.iter_mut().find(|n| n.node_id == "root:polish").unwrap().status = NodeStatus::Completed;
    assert_eq!(
        plan_extend_scope(
            &polish_snap,
            &profile,
            &ExtendScopeInput {
                node_cursor: NodeCursor::root(vec![g("polish")]).unwrap(),
                paths: vec!["docs/late.md".to_owned()],
                reason: "terminal polish".to_owned(),
                worker_id: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::GateVerificationFailed
    );

    // 3. `reopen` completed T1 when dependent T2 is `InProgress` requires `--cascade`.
    snap.nodes.iter_mut().find(|n| n.node_id == "task:T1").unwrap().status = NodeStatus::Completed;
    snap.dependencies[0].satisfied = true;
    snap.nodes.iter_mut().find(|n| n.node_id == "task:T2").unwrap().status = NodeStatus::InProgress;

    assert_eq!(
        plan_reopen(
            &snap,
            &obs,
            &ReopenInput {
                node_cursor: NodeCursor::task(t("T1"), vec![]),
                cascade: false,
                reason: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::DependentTaskCompleted
    );

    let cascade_plan = plan_reopen(
        &snap,
        &obs,
        &ReopenInput {
            node_cursor: NodeCursor::task(t("T1"), vec![]),
            cascade: true,
            reason: Some("fix T1".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &cascade_plan);
    assert!(!snap.dependencies[0].satisfied);
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T2").unwrap().status,
        NodeStatus::Stale
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T1").unwrap().status,
        NodeStatus::Ready
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T1").unwrap().claim_revision,
        2
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T1:build").unwrap().claim_revision,
        2
    );

    // 4. Reopen `blueprint_synthesis.synthesize` -> allows syncing new task T4 without resetting Completed T3!
    snap.nodes.iter_mut().find(|n| n.node_id == "task:T3").unwrap().status = NodeStatus::Completed;
    let reopen_pre = plan_reopen(
        &snap,
        &obs,
        &ReopenInput {
            node_cursor: NodeCursor::root(vec![g("blueprint_synthesis"), g("synthesize")]).unwrap(),
            cascade: false,
            reason: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &reopen_pre);
    assert_eq!(snap.run.root_cycle_count, 0);

    spec_with_t4.tasks[2].scope = vec!["docs/should-not-overwrite-completed/**".to_owned()];

    // Preserve runtime subtask status/evidence on T1, remove T2->T1 dependency to promote T2 Ready,
    // and test `sync --rebind-profile` instantiating new TaskGate `lint` + RootGate `audit`.
    snap.subtasks
        .iter_mut()
        .find(|s| s.node_id == "task:T1")
        .unwrap()
        .status = SubtaskStatus::InProgress;
    snap.subtasks
        .iter_mut()
        .find(|s| s.node_id == "task:T1")
        .unwrap()
        .evidence = Some("runtime wip".to_owned());
    spec_with_t4.tasks[0].subtasks[0].status = SubtaskStatus::Pending;
    spec_with_t4.tasks[0].subtasks[0].evidence = None;
    spec_with_t4.tasks[1].depends_on.clear(); // Remove T2 -> T1 edge (#17).

    let mut rebound_profile = profile.clone();
    if let MachineGraphItem::TasksBlock(ref mut blk) = rebound_profile.gates[1] {
        let lint_gate = MachineGateDef::minimal(g("lint"));
        blk.gates.insert(1, lint_gate);
    }
    let audit_gate = MachineGateDef::minimal(g("audit"));
    rebound_profile
        .gates
        .insert(3, MachineGraphItem::Gate(Box::new(audit_gate)));

    let sync_ok = plan_sync(
        &snap,
        &profile,
        &obs,
        &SyncInput {
            spec: spec_with_t4,
            new_profile: Some(rebound_profile.clone()),
            rebind_profile: true,
        },
    )
    .unwrap();
    assert!(sync_ok.added_nodes.iter().any(|n| n.node_id == "task:T4"));
    assert!(sync_ok.added_nodes.iter().any(|n| n.node_id == "task:T1:lint"));
    assert!(sync_ok.added_nodes.iter().any(|n| n.node_id == "task:T2:lint"));
    assert!(sync_ok.added_nodes.iter().any(|n| n.node_id == "task:T4:lint"));
    assert!(!sync_ok.added_nodes.iter().any(|n| n.node_id == "task:T3:lint"));
    assert!(sync_ok.added_nodes.iter().any(|n| n.node_id == "root:audit"));
    assert_eq!(sync_ok.removed_dependencies.len(), 1);
    assert_eq!(sync_ok.removed_dependencies[0].task_ref, t("T2"));
    // Completed T3 must NOT appear in `node_patches`!
    assert!(!sync_ok.transition.node_patches.iter().any(|p| p.node_id == "task:T3"));

    apply_sync_plan_in_memory(&mut snap, &sync_ok);
    // Runtime subtask status/evidence survived sync.
    let t1_sub = snap.subtasks.iter().find(|s| s.node_id == "task:T1").unwrap();
    assert_eq!(t1_sub.status, SubtaskStatus::InProgress);
    assert_eq!(t1_sub.evidence.as_deref(), Some("runtime wip"));
    // Re-complete pre-task barrier and place Pending T2 behind T2->T1 to verify dependency removal promotion.
    for nid in [
        "root:blueprint_synthesis.synthesize",
        "root:blueprint_synthesis.review",
        "root:blueprint_synthesis",
    ] {
        snap.nodes.iter_mut().find(|n| n.node_id == nid).unwrap().status = NodeStatus::Completed;
    }
    for nid in ["task:T2", "task:T2:build"] {
        snap.nodes.iter_mut().find(|n| n.node_id == nid).unwrap().status = NodeStatus::Pending;
    }
    snap.dependencies.push(TaskDependencyRecord {
        run_id: snap.run.run_id.clone(),
        task_ref: t("T2"),
        depends_on_workspace: "/repo".to_owned(),
        depends_on_run_id: snap.run.run_id.clone(),
        depends_on_ref: t("T1"),
        satisfied: false,
    });
    let mut spec_remove_dep = MachineSpecInput {
        tasks: vec![
            make_task("T1", &["code"], &["crates/a/**"], &[], &snap.run.run_id),
            make_task("T2", &["code"], &["crates/b/**"], &[], &snap.run.run_id),
            make_task("T3", &["docs"], &["docs/guide/**"], &[], &snap.run.run_id),
            make_task("T4", &["docs"], &["docs/extra/**"], &[], &snap.run.run_id),
        ],
        spec_digest: "spec-v3".to_owned(),
    };
    spec_remove_dep.tasks[0].subtasks[0].status = SubtaskStatus::Pending;
    spec_remove_dep.tasks[0].subtasks[0].evidence = None;
    let sync_promote = plan_sync(
        &snap,
        &rebound_profile,
        &obs,
        &SyncInput {
            spec: spec_remove_dep,
            new_profile: None,
            rebind_profile: false,
        },
    )
    .unwrap();
    apply_sync_plan_in_memory(&mut snap, &sync_promote);
    assert!(snap.dependencies.is_empty());
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T2").unwrap().status,
        NodeStatus::Ready
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T2:build").unwrap().status,
        NodeStatus::Ready
    );
    assert!(is_post_task_root_node(
        &snap,
        snap.nodes.iter().find(|n| n.node_id == "root:verify").unwrap()
    ));
    assert!(is_post_task_root_node(
        &snap,
        snap.nodes.iter().find(|n| n.node_id == "root:audit").unwrap()
    ));

    // 5. #19: Transitive A -> B -> C cascade reopen clears B -> C satisfaction and schedule blocks C until B completes.
    let (_, mut chain_snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let t4 = t("T4");
    chain_snap.nodes.push(GateNodeRecord {
        run_id: chain_snap.run.run_id.clone(),
        node_id: "task:T4".to_owned(),
        parent_id: Some("tasks#0".to_owned()),
        kind: NodeKind::Task,
        task_ref: Some(t4.clone()),
        gate_id: "__task__".to_owned(),
        seq_order: 50,
        status: NodeStatus::Completed,
        node_version: 1,
        isolation: IsolationMode::Worktree,
        attempt: 0,
        cycle_count: 0,
        max_attempts: 3,
        claim_revision: 0,
        contract_revision: 1,
        worker_id: None,
        session_fingerprint: None,
        branch: None,
        worktree: None,
        baseline_commit: None,
        claim_tree: None,
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["code".to_owned()],
        invariants: InvariantsState::default(),
        scope: vec!["crates/c/**".to_owned()],
        extended_scope: Vec::new(),
        active_span_id: None,
    });
    chain_snap.nodes.push(GateNodeRecord {
        run_id: chain_snap.run.run_id.clone(),
        node_id: "task:T4:build".to_owned(),
        parent_id: Some("task:T4".to_owned()),
        kind: NodeKind::TaskGate,
        task_ref: Some(t4.clone()),
        gate_id: "build".to_owned(),
        seq_order: 51,
        status: NodeStatus::Completed,
        node_version: 1,
        isolation: IsolationMode::Worktree,
        attempt: 0,
        cycle_count: 0,
        max_attempts: 3,
        claim_revision: 0,
        contract_revision: 1,
        worker_id: None,
        session_fingerprint: None,
        branch: None,
        worktree: None,
        baseline_commit: None,
        claim_tree: None,
        accepted_tree: None,
        accepted_definition_digest: None,
        tags: vec!["code".to_owned()],
        invariants: InvariantsState::default(),
        scope: vec!["crates/c/**".to_owned()],
        extended_scope: Vec::new(),
        active_span_id: None,
    });
    for tid in ["task:T1", "task:T2", "task:T3"] {
        chain_snap.nodes.iter_mut().find(|n| n.node_id == tid).unwrap().status = NodeStatus::Completed;
    }
    chain_snap.dependencies[0].satisfied = true; // T2 -> T1
    chain_snap.dependencies.push(TaskDependencyRecord {
        run_id: chain_snap.run.run_id.clone(),
        task_ref: t4.clone(),
        depends_on_workspace: "/repo".to_owned(),
        depends_on_run_id: chain_snap.run.run_id.clone(),
        depends_on_ref: t("T2"),
        satisfied: true,
    });
    let chain_cascade = plan_reopen(
        &chain_snap,
        &obs,
        &ReopenInput {
            node_cursor: NodeCursor::task(t("T1"), vec![]),
            cascade: true,
            reason: Some("reopen root".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut chain_snap, &chain_cascade);
    assert!(chain_snap.dependencies.iter().all(|d| !d.satisfied));

    // Reopening T4 while T2 is Stale must NOT make T4 claimable or scheduled as ready.
    let reopen_t4 = plan_reopen(
        &chain_snap,
        &obs,
        &ReopenInput {
            node_cursor: NodeCursor::task(t4.clone(), vec![]),
            cascade: false,
            reason: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut chain_snap, &reopen_t4);
    assert!(!task_dependencies_satisfied(&chain_snap, &t4, &obs));
    assert!(!compute_schedule(&chain_snap, &obs).unwrap().ready_tasks.contains(&t4));
    // Even if dep.satisfied were corrupted true while T2 is Stale, schedule still refuses T4.
    chain_snap.dependencies[1].satisfied = true;
    assert!(!task_dependencies_satisfied(&chain_snap, &t4, &obs));
    assert_eq!(
        admit_claim(
            &chain_snap,
            &profile,
            &obs,
            &ClaimInput {
                node_cursor: NodeCursor::task(t4, vec![g("build")]),
                worker_id: w("w4"),
                session_fingerprint: None,
                worktree: "/repo/.worktrees/T4".to_owned(),
                branch: None,
                baseline_commit: None,
                claim_tree: tree('a'),
                span_id: "cccccccccccccccc".to_owned(),
                force: false,
                steal_remote: false,
                previous_local_pid_alive: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::GateVerificationFailed
    );

    // 6. #9: `rebind_profile` inserting a new gate ahead of an unclaimed `Ready` sibling gate
    // demotes that unclaimed sibling back to `Pending` and activates the newly inserted gate first.
    let (p9_base_profile, mut p9_snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    if let Some(b) = p9_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build") {
        b.status = NodeStatus::Completed;
    }
    if let Some(r) = p9_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:review") {
        r.status = NodeStatus::Ready;
        r.claim_revision = 0;
    }
    let mut p9_rebound = p9_base_profile.clone();
    for item in &mut p9_rebound.gates {
        if let MachineGraphItem::TasksBlock(ref mut blk) = item {
            blk.gates.insert(1, MachineGateDef::minimal(g("check")));
            break;
        }
    }
    let p9_spec = MachineSpecInput {
        tasks: vec![make_task("T1", &[], &["crates/a/**"], &[], &p9_snap.run.run_id)],
        spec_digest: "p9".to_owned(),
    };
    p9_snap.nodes.retain(|n| {
        n.task_ref.as_ref().is_none_or(|tr| *tr == t("T1"))
    });
    p9_snap.subtasks.retain(|s| s.node_id == "task:T1");
    let p9_sync = plan_sync(
        &p9_snap,
        &p9_base_profile,
        &obs,
        &SyncInput {
            spec: p9_spec,
            new_profile: Some(p9_rebound),
            rebind_profile: true,
        },
    )
    .unwrap();
    apply_sync_plan_in_memory(&mut p9_snap, &p9_sync);
    assert_eq!(
        p9_snap.nodes.iter().find(|n| n.node_id == "task:T1:check").unwrap().status,
        NodeStatus::Ready,
        "newly inserted earlier gate must become Ready"
    );
    assert_eq!(
        p9_snap.nodes.iter().find(|n| n.node_id == "task:T1:review").unwrap().status,
        NodeStatus::Pending,
        "unclaimed later Ready sibling gate must demote to Pending"
    );

    // #5: Root gate demotion when `rebind_profile` inserts an earlier root sibling gate ahead of unclaimed `Ready` sibling.
    let (r_base, mut r_snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, true);
    let mut r_rebound = r_base.clone();
    if let Some(MachineGraphItem::Gate(ref mut bp)) = r_rebound.gates.first_mut() {
        bp.children.insert(0, MachineGateDef::minimal(g("early")));
    }
    let mut r_spec = MachineSpecInput {
        tasks: vec![make_task("T1", &[], &["crates/a/**"], &[], &r_snap.run.run_id)],
        spec_digest: "r5".to_owned(),
    };
    r_spec.tasks[0].depends_on.clear();
    r_snap.nodes.retain(|n| n.task_ref.as_ref().is_none_or(|tr| *tr == t("T1")));
    r_snap.subtasks.retain(|s| s.node_id == "task:T1");
    let r_sync = plan_sync(
        &r_snap,
        &r_base,
        &obs,
        &SyncInput {
            spec: r_spec,
            new_profile: Some(r_rebound),
            rebind_profile: true,
        },
    )
    .unwrap();
    apply_sync_plan_in_memory(&mut r_snap, &r_sync);
    assert_eq!(
        r_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:blueprint_synthesis.early")
            .unwrap()
            .status,
        NodeStatus::Ready,
        "newly inserted earlier root child gate must become Ready"
    );
    assert_eq!(
        r_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:blueprint_synthesis.synthesize")
            .unwrap()
            .status,
        NodeStatus::Pending,
        "unclaimed later Ready root sibling gate must demote to Pending"
    );

    // Prepending a top-level root gate before unclaimed `blueprint_synthesis` container must
    // demote BOTH the container and its unclaimed `Ready` child gate, making the prepended root gate Ready.
    let (rc_base, mut rc_snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, true);
    let mut rc_rebound = rc_base.clone();
    rc_rebound
        .gates
        .insert(0, MachineGraphItem::Gate(Box::new(MachineGateDef::minimal(g("pre_root")))));
    let mut rc_spec = MachineSpecInput {
        tasks: vec![make_task("T1", &[], &["crates/a/**"], &[], &rc_snap.run.run_id)],
        spec_digest: "rc5".to_owned(),
    };
    rc_spec.tasks[0].depends_on.clear();
    rc_snap.nodes.retain(|n| n.task_ref.as_ref().is_none_or(|tr| *tr == t("T1")));
    rc_snap.subtasks.retain(|s| s.node_id == "task:T1");
    let rc_sync = plan_sync(
        &rc_snap,
        &rc_base,
        &obs,
        &SyncInput {
            spec: rc_spec.clone(),
            new_profile: Some(rc_rebound),
            rebind_profile: true,
        },
    )
    .unwrap();
    apply_sync_plan_in_memory(&mut rc_snap, &rc_sync);
    assert_eq!(
        rc_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:pre_root")
            .unwrap()
            .status,
        NodeStatus::Ready,
        "prepended top-level root gate must become Ready"
    );
    assert_eq!(
        rc_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:blueprint_synthesis")
            .unwrap()
            .status,
        NodeStatus::Pending,
        "unclaimed root container following prepended root gate must demote to Pending"
    );
    assert_eq!(
        rc_snap
            .nodes
            .iter()
            .find(|n| n.node_id == "root:blueprint_synthesis.synthesize")
            .unwrap()
            .status,
        NodeStatus::Pending,
        "unclaimed descendant of demoted root container must also demote to Pending"
    );

    // Rebinding Container <-> RootGate fails closed (`ProfileSchemaInvalid`), and adding child gates
    // under a Completed/Cancelled ancestor never inserts orphaned children.
    let mut kind_flip = rc_base.clone();
    kind_flip.gates[0] = MachineGraphItem::Gate(Box::new(MachineGateDef::minimal(g("blueprint_synthesis"))));
    let flip_err = plan_sync(
        &rc_snap,
        &rc_base,
        &obs,
        &SyncInput {
            spec: rc_spec.clone(),
            new_profile: Some(kind_flip),
            rebind_profile: true,
        },
    )
    .unwrap_err();
    assert_eq!(flip_err.code, ErrorCode::ProfileSchemaInvalid);
    assert!(flip_err.message.contains("across node kind change"));

    for nid in [
        "root:blueprint_synthesis",
        "root:blueprint_synthesis.synthesize",
        "root:blueprint_synthesis.review",
    ] {
        rc_snap.nodes.iter_mut().find(|n| n.node_id == nid).unwrap().status =
            NodeStatus::Completed;
    }
    let mut add_under_done = rc_base.clone();
    if let MachineGraphItem::Gate(ref mut gate_box) = add_under_done.gates[0] {
        gate_box.children.push(MachineGateDef::minimal(g("late_extra")));
    }
    let done_sync = plan_sync(
        &rc_snap,
        &rc_base,
        &obs,
        &SyncInput {
            spec: rc_spec,
            new_profile: Some(add_under_done),
            rebind_profile: true,
        },
    )
    .unwrap();
    assert!(!done_sync
        .added_nodes
        .iter()
        .any(|n| n.node_id == "root:blueprint_synthesis.late_extra"));
}

#[test]
fn unified_evaluate_transition_routes_all_variants() {
    let (profile, snap) = RunHarness::build_machine_sample_graph(IsolationMode::Worktree, false);
    let obs = obs_with_tree(tree('a'));
    let out = evaluate_transition(
        &snap,
        &profile,
        &obs,
        &TransitionInput::Claim(ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: w("w1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: None,
            baseline_commit: None,
            claim_tree: tree('a'),
            span_id: "0123456789abcdef".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        }),
    )
    .unwrap();
    assert!(matches!(out, TransitionOutcome::Claim(b) if matches!(*b, ClaimDecision::Admit { .. })));
}
