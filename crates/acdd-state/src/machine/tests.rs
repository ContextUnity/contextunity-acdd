use super::*;
use crate::machine::graph::is_post_task_root_node;
use crate::machine::schedule::task_dependencies_satisfied;
use crate::machine::verify::ExecutionDigestInput;
use crate::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, BlackboardTopic, CheckResultRecord,
    ClaimOutcome, FindingItem, FindingSeverity, GateClaimRecord, GateNodeRecord, GateTraceRecord,
    InvariantsState, IsolationMode, NodeKind, NodeStatus, RejectionFindings, RejectionKind,
    ReviewContourResult, ReviewEvidence, RunGraphSnapshot, SaveAction, SubmitAction, SubtaskStatus,
    TaskDependencyRecord, TraceStatus, TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::ErrorCode;
use acdd_core::ids::{
    CommitOid, GateId, GateSelector, NodeCursor, RunId, RunName, SubtaskRef, TaskRef, TreeOid,
    WorkerId,
};
use acdd_core::schema::SchemaDef;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn g(s: &str) -> GateId {
    GateId::new(s).unwrap()
}
fn t(s: &str) -> TaskRef {
    TaskRef::new(s).unwrap()
}
fn w(s: &str) -> WorkerId {
    WorkerId::new(s).unwrap()
}
fn tree(ch: char) -> TreeOid {
    TreeOid::new(ch.to_string().repeat(40)).unwrap()
}
fn commit(ch: char) -> CommitOid {
    CommitOid::new(ch.to_string().repeat(40)).unwrap()
}

fn obs_with_tree(t_oid: TreeOid) -> ValidatedObservations {
    ValidatedObservations {
        current_tree: Some(t_oid.clone()),
        container_tree: Some(t_oid),
        changed_paths: Vec::new(),
        out_of_scope_paths: Vec::new(),
        unintegrated_tasks: Vec::new(),
        external_satisfied_deps: Vec::new(),
        now_utc: "2026-11-17T10:00:00.000Z".to_owned(),
    }
}

fn apply_claim_in_memory(
    snap: &mut RunGraphSnapshot,
    decision: &ClaimDecision,
) {
    if let ClaimDecision::Admit { claim, transition } = decision {
        apply_plan_in_memory(snap, transition);
        let next_id = snap.claims.iter().filter_map(|c| c.claim_id).max().unwrap_or(0) + 1;
        snap.claims.push(GateClaimRecord {
            claim_id: Some(next_id),
            run_id: snap.run.run_id.clone(),
            node_id: claim.node_id.clone(),
            claim_revision: claim.claim_revision,
            worker_id: claim.worker_id.clone(),
            session_fingerprint: claim.session_fingerprint.clone(),
            worktree: claim.worktree.clone(),
            span_id: claim.span_id.clone(),
            claim_tree: claim.claim_tree.clone(),
            claimed_at: claim.claimed_at.clone(),
            ended_at: None,
            outcome: ClaimOutcome::Active,
        });
    }
}

fn record_verify_in_memory(snap: &mut RunGraphSnapshot, eval: &VerifyEvaluation) {
    let next_id = snap
        .traces
        .iter()
        .filter_map(|tr| tr.trace_row_id)
        .max()
        .unwrap_or(0)
        + 1;
    snap.traces.push(GateTraceRecord {
        trace_row_id: Some(next_id),
        run_id: eval.trace_start.run_id.clone(),
        node_id: eval.trace_start.node_id.clone(),
        claim_revision: eval.trace_start.claim_revision,
        contract_revision: eval.trace_start.contract_revision,
        execution_digest: eval.trace_start.execution_digest.clone(),
        idempotency_key: eval.trace_start.idempotency_key.clone(),
        command_id: eval.trace_start.command_id.clone(),
        command_line: eval.trace_start.command_line.clone(),
        action_id: eval.trace_start.action_id.clone(),
        test_target: eval.trace_start.test_target.clone(),
        status: eval.trace_finish.status,
        exit_code: eval.trace_finish.exit_code,
        tests_passed: eval.trace_finish.tests_passed,
        tests_failed: eval.trace_finish.tests_failed,
        tests_ignored: eval.trace_finish.tests_ignored,
        passed_tests: eval.trace_finish.passed_tests.clone(),
        failed_tests: eval.trace_finish.failed_tests.clone(),
        check_results: eval.trace_finish.check_results.clone(),
        candidate_tree_before: eval.trace_start.candidate_tree_before.clone(),
        candidate_tree_after: eval.trace_finish.candidate_tree_after.clone(),
        log_relpath: eval.trace_start.log_relpath.clone(),
        started_at: eval.trace_start.started_at.clone(),
        finished_at: Some(eval.trace_finish.finished_at.clone()),
    });
    if let Some(ref plan) = eval.failure_transition {
        apply_plan_in_memory(snap, plan);
    }
}

fn apply_plan_in_memory(snap: &mut RunGraphSnapshot, plan: &TransitionPlan) {
    assert_eq!(
        snap.run.generation, plan.preconditions.expected_run_generation,
        "CAS generation mismatch"
    );
    for exp in &plan.preconditions.expected_node_versions {
        let node = snap
            .nodes
            .iter()
            .find(|n| n.node_id == exp.node_id)
            .expect("precondition node exists");
        assert_eq!(
            node.node_version, exp.expected_version,
            "CAS node_version mismatch on {}",
            exp.node_id
        );
    }
    if plan.bump_run_generation {
        snap.run.generation += 1;
    }
    if let Some(st) = plan.new_run_status {
        snap.run.status = st;
    }
    if let Some(rc) = plan.new_root_cycle_count {
        snap.run.root_cycle_count = rc;
    }
    if let Some(ref ca) = plan.completed_at {
        snap.run.completed_at = Some(ca.clone());
    }
    for np in &plan.node_patches {
        let node = snap
            .nodes
            .iter_mut()
            .find(|n| n.node_id == np.node_id)
            .expect("patch target exists");
        node.status = np.new_status;
        node.node_version += 1;
        if let Some(v) = np.attempt {
            node.attempt = v;
        }
        if let Some(v) = np.cycle_count {
            node.cycle_count = v;
        }
        if let Some(v) = np.claim_revision {
            node.claim_revision = v;
        }
        if let Some(v) = np.contract_revision {
            node.contract_revision = v;
        }
        if let Some(ref v) = np.worker_id {
            node.worker_id = v.clone();
        }
        if let Some(ref v) = np.session_fingerprint {
            node.session_fingerprint = v.clone();
        }
        if let Some(ref v) = np.branch {
            node.branch = v.clone();
        }
        if let Some(ref v) = np.worktree {
            node.worktree = v.clone();
        }
        if let Some(ref v) = np.baseline_commit {
            node.baseline_commit = v.clone();
        }
        if let Some(ref v) = np.claim_tree {
            node.claim_tree = v.clone();
        }
        if let Some(ref v) = np.accepted_tree {
            node.accepted_tree = v.clone();
        }
        if let Some(ref v) = np.accepted_definition_digest {
            node.accepted_definition_digest = v.clone();
        }
        if let Some(ref v) = np.tags {
            node.tags = v.clone();
        }
        if let Some(ref v) = np.invariants {
            node.invariants = v.clone();
        }
        if let Some(ref v) = np.scope {
            node.scope = v.clone();
        }
        if let Some(ref v) = np.extended_scope {
            node.extended_scope = v.clone();
        }
        if let Some(ref v) = np.active_span_id {
            node.active_span_id = v.clone();
        }
    }
    for dp in &plan.dependency_patches {
        for dep in &mut snap.dependencies {
            if dep.depends_on_ref == dp.depends_on_ref
                && dp
                    .dependent_task_ref
                    .as_ref()
                    .is_none_or(|d| d == &dep.task_ref)
            {
                dep.satisfied = dp.satisfied;
            }
        }
    }
    for cp in &plan.claim_patches {
        if let Some(c) = snap
            .claims
            .iter_mut()
            .find(|c| c.node_id == cp.node_id && c.claim_revision == cp.claim_revision)
        {
            c.outcome = cp.outcome;
            c.ended_at = Some(cp.ended_at.clone());
        }
    }
    snap.artifacts.extend(plan.new_artifacts.clone());
    for sub in &plan.new_subtasks {
        if let Some(existing) = snap
            .subtasks
            .iter_mut()
            .find(|s| s.node_id == sub.node_id && s.subtask_ref == sub.subtask_ref)
        {
            *existing = sub.clone();
        } else {
            snap.subtasks.push(sub.clone());
        }
    }
}

fn apply_sync_plan_in_memory(snap: &mut RunGraphSnapshot, plan: &SyncPlan) {
    apply_plan_in_memory(snap, &plan.transition);
    snap.run.spec_digest = plan.spec_digest.clone();
    snap.nodes.extend(plan.added_nodes.clone());
    for (nid, seq) in &plan.seq_order_updates {
        if let Some(n) = snap.nodes.iter_mut().find(|n| &n.node_id == nid) {
            n.seq_order = *seq;
        }
    }
    snap.dependencies.retain(|d| {
        !plan.removed_dependencies.iter().any(|r| {
            r.task_ref == d.task_ref
                && r.depends_on_workspace == d.depends_on_workspace
                && r.depends_on_run_id == d.depends_on_run_id
                && r.depends_on_ref == d.depends_on_ref
        })
    });
    for dep in &plan.added_dependencies {
        if !snap.dependencies.iter().any(|d| {
            d.task_ref == dep.task_ref
                && d.depends_on_workspace == dep.depends_on_workspace
                && d.depends_on_run_id == dep.depends_on_run_id
                && d.depends_on_ref == dep.depends_on_ref
        }) {
            snap.dependencies.push(dep.clone());
        }
    }
}

fn canonical_profile(isolation: IsolationMode) -> MachineProfileInput {
    let mut bp_schema = SchemaDef::object();
    bp_schema
        .properties
        .insert("tasks".to_owned(), SchemaDef::object());
    bp_schema.required = vec!["tasks".to_owned()];

    let synthesize = MachineGateDef {
        role: Some("architect".to_owned()),
        proof: MachineProofKind::CustomSchema("blueprint".to_owned()),
        pass: Some("review".to_owned()),
        ..MachineGateDef::minimal(g("synthesize"))
    };
    let bp_review = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        proof: MachineProofKind::Review,
        contours: vec![
            "claims".to_owned(),
            "project_isolation".to_owned(),
            "administration".to_owned(),
        ],
        independent_from: vec![GateSelector::from_str_ok("synthesize")],
        fail: Some("synthesize".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("review"))
    };
    let blueprint_synthesis = MachineGateDef {
        children: vec![synthesize, bp_review],
        ..MachineGateDef::minimal(g("blueprint_synthesis"))
    };

    let contract = MachineGateDef {
        role: Some("contract_author".to_owned()),
        proof: MachineProofKind::Contract,
        command: Some("test".to_owned()),
        require_test_target: true,
        expected_failure: Some(MachineExpectedFailure {
            exit_codes: vec![1, 101],
            stderr_contains: None,
        }),
        pass: Some("contract_review".to_owned()),
        save: MachineSavePolicy {
            post: SaveAction::None,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("contract"))
    };
    let contract_review = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        proof: MachineProofKind::Review,
        contours: vec!["paths".to_owned(), "claims".to_owned()],
        independent_from: vec![GateSelector::from_str_ok("contract")],
        need: vec![GateSelector::from_str_ok("contract")],
        pass: Some("build".to_owned()),
        fail: Some("contract".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("contract_review"))
    };
    let build = MachineGateDef {
        role: Some("builder".to_owned()),
        proof: MachineProofKind::Command,
        command: Some("build_check".to_owned()),
        require_counts: true,
        pass: Some("review".to_owned()),
        save: MachineSavePolicy {
            post: SaveAction::None,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("build"))
    };
    let task_review = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        proof: MachineProofKind::Review,
        contours: vec!["paths".to_owned(), "claims".to_owned()],
        independent_from: vec![GateSelector::from_str_ok("build")],
        need: vec![GateSelector::from_str_ok("build")],
        pass: Some("deliver".to_owned()),
        fail: Some("build".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("review"))
    };
    let deliver = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        independent_from: vec![GateSelector::from_str_ok("build")],
        need: vec![
            GateSelector::from_str_ok("build"),
            GateSelector::from_str_ok("review"),
        ],
        receipt: true,
        fail: Some("build".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            post: if isolation == IsolationMode::InPlace {
                SaveAction::None
            } else {
                SaveAction::Commit
            },
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("deliver"))
    };

    let verify_root = MachineGateDef {
        proof: MachineProofKind::Command,
        command: Some("verify_all".to_owned()),
        pass: Some("integration_review".to_owned()),
        fail: Some("polish".to_owned()),
        max_attempts: 3,
        ..MachineGateDef::minimal(g("verify"))
    };
    let polish = MachineGateDef {
        role: Some("builder".to_owned()),
        proof: MachineProofKind::Command,
        command: Some("verify_all".to_owned()),
        scope: vec!["docs/**".to_owned(), ".acdd/profile.yaml".to_owned()],
        pass: Some("verify".to_owned()),
        max_attempts: 3,
        ..MachineGateDef::minimal(g("polish"))
    };
    let integration_review = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        proof: MachineProofKind::Review,
        contours: vec!["paths".to_owned(), "claims".to_owned()],
        independent_from: vec![
            GateSelector::from_str_ok("polish"),
            GateSelector::from_str_ok("tasks.build"),
        ],
        need: vec![GateSelector::from_str_ok("verify")],
        pass: Some("handoff".to_owned()),
        fail: Some("polish".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("integration_review"))
    };
    let handoff = MachineGateDef {
        role: Some("independent_reviewer".to_owned()),
        independent_from: vec![
            GateSelector::from_str_ok("polish"),
            GateSelector::from_str_ok("tasks.build"),
        ],
        need: vec![
            GateSelector::from_str_ok("verify"),
            GateSelector::from_str_ok("integration_review"),
        ],
        receipt: true,
        fail: Some("polish".to_owned()),
        save: MachineSavePolicy {
            immutable: true,
            archive: true,
            prune: true,
            post: if isolation == IsolationMode::InPlace {
                SaveAction::None
            } else {
                SaveAction::Commit
            },
            ..Default::default()
        },
        ..MachineGateDef::minimal(g("handoff"))
    };

    MachineProfileInput {
        profile_id: "default".to_owned(),
        profile_json: "{}".to_owned(),
        git: MachineGitPolicy {
            isolation,
            ..Default::default()
        },
        default_save: SaveAction::Commit,
        protected_paths: vec!["Cargo.toml".to_owned(), "build.rs".to_owned()],
        tags: BTreeSet::from(["code".to_owned(), "docs".to_owned(), "urgent".to_owned()]),
        proof_schemas: BTreeMap::from([("blueprint".to_owned(), bp_schema)]),
        gates: vec![
            MachineGraphItem::Gate(Box::new(blueprint_synthesis)),
            MachineGraphItem::TasksBlock(MachineTasksBlockDef {
                when: None,
                gates: vec![contract, contract_review, build, task_review, deliver],
            }),
            MachineGraphItem::Gate(Box::new(verify_root)),
            MachineGraphItem::Gate(Box::new(polish)),
            MachineGraphItem::Gate(Box::new(integration_review)),
            MachineGraphItem::Gate(Box::new(handoff)),
        ],
    }
}

trait SelectorExt {
    fn from_str_ok(s: &str) -> Self;
}
impl SelectorExt for GateSelector {
    fn from_str_ok(s: &str) -> Self {
        s.parse().unwrap()
    }
}

fn make_task(
    name: &str,
    tags: &[&str],
    scope: &[&str],
    deps: &[&str],
    run_id: &RunId,
) -> MachineTaskSpec {
    MachineTaskSpec {
        task_ref: t(name),
        tags: tags.iter().map(|s| (*s).to_owned()).collect(),
        scope: scope.iter().map(|s| (*s).to_owned()).collect(),
        extended_scope: Vec::new(),
        invariants: vec!["INV-1".to_owned()],
        depends_on: deps
            .iter()
            .map(|d| MachineTaskDep {
                depends_on_workspace: "/repo".to_owned(),
                depends_on_run_id: run_id.clone(),
                depends_on_ref: t(d),
            })
            .collect(),
        subtasks: vec![MachineSubtaskInput {
            subtask_ref: SubtaskRef::new("S1").unwrap(),
            title: "Implement core".to_owned(),
            status: SubtaskStatus::Completed,
            evidence: Some("implemented and tested".to_owned()),
        }],
    }
}

fn build_sample_graph(isolation: IsolationMode, with_pre_task: bool) -> (MachineProfileInput, RunGraphSnapshot) {
    let mut profile = canonical_profile(isolation);
    if !with_pre_task {
        profile.gates.remove(0);
    }
    let run_id = RunId::new("run-1").unwrap();
    let input = BuildGraphInput {
        run_id: run_id.clone(),
        run_name: RunName::new("AC-100").unwrap(),
        collection: Some("milestones".to_owned()),
        workspace: "/repo".to_owned(),
        spec_path: Some("docs/milestones/AC-100.md".to_owned()),
        is_ephemeral: false,
        target_base: Some("main".to_owned()),
        target_base_commit: Some(commit('1')),
        branch: Some("milestone/AC-100".to_owned()),
        worktree: Some("/repo/.worktrees/AC-100".to_owned()),
        trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
        created_at: "2026-11-17T09:00:00.000Z".to_owned(),
        spec: MachineSpecInput {
            tasks: vec![
                make_task("T1", &["code"], &["crates/a/**"], &[], &run_id),
                make_task("T2", &["code"], &["crates/b/**"], &["T1"], &run_id),
                make_task("T3", &["docs"], &["docs/guide/**"], &[], &run_id),
            ],
            spec_digest: "spec-sha".to_owned(),
        },
    };
    let snap = build_initial_run_graph(&profile, &input).unwrap();
    (profile, snap)
}

#[test]
fn graph_construction_node_ids_and_when_filtering() {
    let allowed = BTreeSet::from(["code".to_owned(), "docs".to_owned()]);
    assert_eq!(effective_task_tags(&[], &allowed).unwrap(), vec!["default".to_owned()]);
    assert_eq!(
        effective_task_tags(&["nope".to_owned()], &allowed).unwrap_err().code,
        ErrorCode::ProfileSchemaInvalid
    );
    let (mut profile, _) = build_sample_graph(IsolationMode::Worktree, true);
    // Add `when: "code"` to `contract` gate so `T3` (`docs`) skips `contract`.
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
    assert!(ids.contains("task:T1:contract"));
    assert!(ids.contains("task:T3"));
    assert!(!ids.contains("task:T3:contract"));
    assert!(ids.contains("task:T3:contract_review"));
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
            patterns_overlap(a, b),
            expected,
            "patterns_overlap({a:?}, {b:?})"
        );
    }

    // 2. Worktree wave schedule vs overlapping tasks.
    let (_, mut wt_snap) = build_sample_graph(IsolationMode::Worktree, false);
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
    let (_, ip_snap) = build_sample_graph(IsolationMode::InPlace, false);
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
    let (profile, mut snap) = build_sample_graph(IsolationMode::Worktree, false);
    let obs = obs_with_tree(tree('a'));

    let claim_t1 = ClaimInput {
        node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
        ClaimCase { label: "idempotent_same", worker: "worker-1", fp: Some("host:100:1"), worktree: "/repo/.worktrees/T1", force: false, steal: false, pid_alive: None, expected: Ok(true) },
        ClaimCase { label: "other_worker_no_force", worker: "worker-2", fp: Some("host:200:2"), worktree: "/repo/.worktrees/T1", force: false, steal: false, pid_alive: None, expected: Err(ErrorCode::TaskAlreadyClaimed) },
        ClaimCase { label: "other_worker_force_live_pid", worker: "worker-2", fp: Some("host:200:2"), worktree: "/repo/.worktrees/T1", force: true, steal: false, pid_alive: Some(true), expected: Err(ErrorCode::TaskAlreadyClaimed) },
        ClaimCase { label: "same_worker_force_new_fp", worker: "worker-1", fp: Some("host:101:2"), worktree: "/repo/.worktrees/T1", force: true, steal: false, pid_alive: Some(true), expected: Ok(false) },
        ClaimCase { label: "other_worker_dead_pid", worker: "worker-2", fp: Some("host:200:2"), worktree: "/repo/.worktrees/T1", force: true, steal: false, pid_alive: Some(false), expected: Ok(false) },
        ClaimCase { label: "other_worker_steal_remote", worker: "worker-3", fp: Some("remote:999:9"), worktree: "/repo/.worktrees/T1", force: true, steal: true, pid_alive: None, expected: Ok(false) },
    ];

    for case in cases {
        let req = ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
        node_cursor: NodeCursor::task(t("T3"), vec![g("contract")]),
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
    let (ip_profile, mut ip_snap) = build_sample_graph(IsolationMode::InPlace, false);
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
    let (pre_profile, pre_snap) = build_sample_graph(IsolationMode::Worktree, true);
    let pre_err = admit_claim(
        &pre_snap,
        &pre_profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
fn verify_contract_command_scope_and_mechanical_cycle_guard_matrix() {
    let (profile, mut snap) = build_sample_graph(IsolationMode::Worktree, false);
    let t_a = tree('a');
    let obs = obs_with_tree(t_a.clone());

    // Claim T1.contract
    let claim = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            worker_id: w("author-1"),
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

    struct ContractCase<'a> {
        label: &'a str,
        exit_code: Option<i32>,
        compile_errs: u32,
        timed_out: bool,
        aborted: bool,
        failed: &'a [&'a str],
        ignored: &'a [&'a str],
        changed: &'a [&'a str],
        exp_trace: TraceStatus,
        exp_err: Option<ErrorCode>,
    }

    let contract_cases = [
        ContractCase { label: "valid_red", exit_code: Some(101), compile_errs: 0, timed_out: false, aborted: false, failed: &["tests::red"], ignored: &[], changed: &[], exp_trace: TraceStatus::ExpectedFailure, exp_err: None },
        ContractCase { label: "zero_exit_fails", exit_code: Some(0), compile_errs: 0, timed_out: false, aborted: false, failed: &["tests::red"], ignored: &[], changed: &[], exp_trace: TraceStatus::Failed, exp_err: Some(ErrorCode::GateVerificationFailed) },
        ContractCase { label: "ignored_target_fails", exit_code: Some(101), compile_errs: 0, timed_out: false, aborted: false, failed: &["tests::red"], ignored: &["tests::red"], changed: &[], exp_trace: TraceStatus::Failed, exp_err: Some(ErrorCode::GateVerificationFailed) },
        ContractCase { label: "missing_target_fails", exit_code: Some(101), compile_errs: 0, timed_out: false, aborted: false, failed: &["tests::other"], ignored: &[], changed: &[], exp_trace: TraceStatus::Failed, exp_err: Some(ErrorCode::GateVerificationFailed) },
        ContractCase { label: "compile_error", exit_code: Some(101), compile_errs: 2, timed_out: false, aborted: false, failed: &["tests::red"], ignored: &[], changed: &[], exp_trace: TraceStatus::CompileError, exp_err: Some(ErrorCode::GateVerificationFailed) },
        ContractCase { label: "timed_out", exit_code: None, compile_errs: 0, timed_out: true, aborted: false, failed: &[], ignored: &[], changed: &[], exp_trace: TraceStatus::TimedOut, exp_err: Some(ErrorCode::GateVerificationFailed) },
        ContractCase { label: "out_of_scope", exit_code: Some(101), compile_errs: 0, timed_out: false, aborted: false, failed: &["tests::red"], ignored: &[], changed: &["Cargo.toml"], exp_trace: TraceStatus::ScopeViolation, exp_err: Some(ErrorCode::OutOfScopeChanges) },
    ];

    for case in contract_cases {
        let mut case_obs = obs.clone();
        case_obs.changed_paths = case.changed.iter().map(|s| (*s).to_owned()).collect();
        let req = VerifyInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            worker_id: Some(w("author-1")),
            claim_revision: Some(1),
            test_target: Some("tests::red".to_owned()),
            command_id: Some("test".to_owned()),
            command_line: Some("cargo test tests::red".to_owned()),
            resolved_commands: vec!["cargo test tests::red".to_owned()],
            output_spec: json!({}),
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/t1-contract.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: case.timed_out,
                aborted: case.aborted,
                exit_code: case.exit_code,
                stderr_tail: String::new(),
                compile_error_count: case.compile_errs,
                tests_passed: 0,
                tests_failed: case.failed.len() as u32,
                tests_ignored: case.ignored.len() as u32,
                passed_tests: Vec::new(),
                failed_tests: case.failed.iter().map(|s| (*s).to_owned()).collect(),
                ignored_tests: case.ignored.iter().map(|s| (*s).to_owned()).collect(),
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

    // Verify RFC-8785 execution_digest determinism regardless of JSON key order in output_spec.
    let d1 = compute_execution_digest(&ExecutionDigestInput {
        contract_revision: 1,
        gate_def_digest: "d".to_owned(),
        admitted_scope: vec!["crates/a/**".to_owned()],
        resolved_commands: vec!["cargo test".to_owned()],
        test_target: Some("tests::red".to_owned()),
        output_spec: json!({"b": 2, "a": 1}),
        call_with: json!({}),
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
        test_target: Some("tests::red".to_owned()),
        output_spec: json!({"a": 1, "b": 2}),
        call_with: json!({}),
        checks_params: json!([]),
        index_generation: Some("42".to_owned()),
        live_files_digest: Some("deadbeef".to_owned()),
    })
    .unwrap();
    assert_eq!(d1, d2);

    // Record valid contract verify -> submit contract -> review -> claim build -> verify build checks prior target!
    let ok_verify = evaluate_verify(
        &snap,
        &profile,
        &obs,
        &VerifyInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            worker_id: Some(w("author-1")),
            claim_revision: Some(1),
            test_target: Some("tests::red".to_owned()),
            command_id: Some("test".to_owned()),
            command_line: Some("cargo test tests::red".to_owned()),
            resolved_commands: vec!["cargo test tests::red".to_owned()],
            output_spec: json!({}),
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/t1-contract.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: Some(101),
                stderr_tail: String::new(),
                compile_error_count: 0,
                tests_passed: 0,
                tests_failed: 1,
                tests_ignored: 0,
                passed_tests: Vec::new(),
                failed_tests: vec!["tests::red".to_owned()],
                ignored_tests: Vec::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a.clone()),
            },
        },
    )
    .unwrap();
    assert!(ok_verify.outcome.is_ok());
    record_verify_in_memory(&mut snap, &ok_verify);

    let sub_c = evaluate_submit_plan(
        &snap,
        &profile,
        &obs,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            action: SubmitAction::Pass,
            worker_id: w("author-1"),
            claim_revision: Some(1),
            expected_execution_digest: Some(ok_verify.execution_digest.clone()),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &sub_c);

    // Pass contract_review
    let c_cr = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            worker_id: w("reviewer-1"),
            session_fingerprint: None,
            worktree: "/repo/.worktrees/T1".to_owned(),
            branch: Some("task/T1".to_owned()),
            baseline_commit: Some(commit('1')),
            claim_tree: t_a.clone(),
            span_id: "bbbbbbbbbbbbbbbb".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_cr);
    let sub_cr = evaluate_submit_plan(
        &snap,
        &profile,
        &obs,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            action: SubmitAction::Pass,
            worker_id: w("reviewer-1"),
            claim_revision: Some(1),
            expected_execution_digest: None,
            review_evidence: Some(ReviewEvidence {
                decision: SubmitAction::Pass,
                contours: BTreeMap::from([
                    ("paths".to_owned(), ReviewContourResult { applicable: true, evidence: "ok".to_owned() }),
                    ("claims".to_owned(), ReviewContourResult { applicable: true, evidence: "ok".to_owned() }),
                ]),
                summary: "looks good".to_owned(),
                notes: None,
            }),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &sub_cr);

    // Claim build; verify must require prior contract target `tests::red` in `passed_tests`!
    let c_b = admit_claim(
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
            span_id: "cccccccccccccccc".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_b);

    let missing_prior = evaluate_verify(
        &snap,
        &profile,
        &obs,
        &VerifyInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            worker_id: Some(w("builder-1")),
            claim_revision: Some(1),
            test_target: None,
            command_id: Some("build_check".to_owned()),
            command_line: Some("cargo test".to_owned()),
            resolved_commands: vec!["cargo test".to_owned()],
            output_spec: json!({}),
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/t1-build.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: Some(0),
                stderr_tail: String::new(),
                compile_error_count: 0,
                tests_passed: 5,
                tests_failed: 0,
                tests_ignored: 0,
                passed_tests: vec!["tests::other".to_owned()],
                failed_tests: Vec::new(),
                ignored_tests: Vec::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a.clone()),
            },
        },
    )
    .unwrap();
    assert_eq!(
        missing_prior.outcome.unwrap_err().code,
        ErrorCode::GateVerificationFailed
    );

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
                output_spec: json!({}),
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
                    compile_error_count: 0,
                    tests_passed: 0,
                    tests_failed: 1,
                    tests_ignored: 0,
                    passed_tests: Vec::new(),
                    failed_tests: vec!["t".to_owned()],
                    ignored_tests: Vec::new(),
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
        let v_node = root_snap.nodes.iter().find(|n| n.node_id == "root:verify").unwrap();
        if cycle < 3 {
            assert_eq!(v_node.status, NodeStatus::Pending);
            assert_eq!(
                root_snap.nodes.iter().find(|n| n.node_id == "root:polish").unwrap().status,
                NodeStatus::Ready
            );
            // Reset polish -> verify Ready for next cycle test
            root_snap.nodes.iter_mut().find(|n| n.node_id == "root:polish").unwrap().status = NodeStatus::Pending;
            root_snap.nodes.iter_mut().find(|n| n.node_id == "root:verify").unwrap().status = NodeStatus::Ready;
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
    let v_node = call_snap.nodes.iter_mut().find(|n| n.node_id == "root:verify").unwrap();
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
            output_spec: json!({}),
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
                compile_error_count: 0,
                tests_passed: 0,
                tests_failed: 0,
                tests_ignored: 0,
                passed_tests: Vec::new(),
                failed_tests: Vec::new(),
                ignored_tests: Vec::new(),
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

#[test]
fn submit_independence_immutable_need_drift_reject_escalation_and_receipt_closure() {
    let (profile, mut snap) = build_sample_graph(IsolationMode::Worktree, false);
    let t_a = tree('a');
    let t_b = tree('b');
    let obs_a = obs_with_tree(t_a.clone());

    // Claim & verify T1.contract by author-1
    let c1 = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            worker_id: w("author-1"),
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
                node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
                action: SubmitAction::Pass,
                worker_id: w("author-1"),
                claim_revision: Some(1),
                expected_execution_digest: None,
                review_evidence: None,
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_ref: None,
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
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            worker_id: Some(w("author-1")),
            claim_revision: Some(1),
            test_target: Some("tests::c1".to_owned()),
            command_id: Some("test".to_owned()),
            command_line: Some("cargo test tests::c1".to_owned()),
            resolved_commands: vec!["cargo test tests::c1".to_owned()],
            output_spec: json!({}),
            action_id: None,
            index_generation: None,
            live_files_digest: None,
            log_relpath: "logs/c1.log".to_owned(),
            started_at: "2026-11-17T09:59:00.000Z".to_owned(),
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: Some(101),
                stderr_tail: String::new(),
                compile_error_count: 0,
                tests_passed: 0,
                tests_failed: 1,
                tests_ignored: 0,
                passed_tests: Vec::new(),
                failed_tests: vec!["tests::c1".to_owned()],
                ignored_tests: Vec::new(),
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
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
            action: SubmitAction::Pass,
            worker_id: w("author-1"),
            claim_revision: Some(1),
            expected_execution_digest: Some(v1.execution_digest),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p1);

    // 1. Self-review on `contract_review` triggers TASK_REVIEW_NOT_INDEPENDENT.
    let c_cr_bad = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            worker_id: w("author-1"),
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
    apply_claim_in_memory(&mut snap, &c_cr_bad);
    let rev_ev = ReviewEvidence {
        decision: SubmitAction::Pass,
        contours: BTreeMap::from([
            ("paths".to_owned(), ReviewContourResult { applicable: true, evidence: "ok".to_owned() }),
            ("claims".to_owned(), ReviewContourResult { applicable: true, evidence: "ok".to_owned() }),
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
                node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
                action: SubmitAction::Pass,
                worker_id: w("author-1"),
                claim_revision: Some(1),
                expected_execution_digest: None,
                review_evidence: Some(rev_ev.clone()),
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_ref: None,
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskReviewNotIndependent
    );

    // Takeover by independent reviewer
    let c_cr_ok = admit_claim(
        &snap,
        &profile,
        &obs_a,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            worker_id: w("rev-1"),
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
    apply_claim_in_memory(&mut snap, &c_cr_ok);

    // 2. When both `immutable: true` and `need:` drift simultaneously, `evaluate_submit` emits
    // `MarkStaleAndReject` BEFORE failing on `immutable` so drifted `need:` predecessors don't stay Completed.
    let obs_b = obs_with_tree(t_b.clone());
    let both_drift = evaluate_submit(
        &snap,
        &profile,
        &obs_b,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-1"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: Some(rev_ev.clone()),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
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
            assert_eq!(stale_node_ids, vec!["task:T1:contract".to_owned()]);
            assert_eq!(error.code, ErrorCode::TaskCandidateMismatch);
        }
        other => panic!("expected MarkStaleAndReject when both need: and immutable drift, got {other:?}"),
    }

    // When `need:` matches `t_b` while `claim_tree` is still `t_a`, `immutable: true` still rejects directly.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:contract")
        .unwrap()
        .accepted_tree = Some(t_b.clone());
    assert_eq!(
        evaluate_submit_plan(
            &snap,
            &profile,
            &obs_b,
            &SubmitInput {
                node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
                action: SubmitAction::Pass,
                worker_id: w("rev-1"),
                claim_revision: Some(2),
                expected_execution_digest: None,
                review_evidence: Some(rev_ev.clone()),
                rejection_findings: None,
                custom_evidence: None,
                verified_invariants: Vec::new(),
                snapshot_ref: None,
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
        .find(|n| n.node_id == "task:T1:contract")
        .unwrap()
        .accepted_tree = Some(t_a.clone());

    // 3. `need:` drift when claim_tree == current_tree == t_b while predecessor `contract` had `accepted_tree == t_a`:
    // produces `MarkStaleAndReject { stale_node_ids: ["task:T1:contract"], ... }`.
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:contract_review")
        .unwrap()
        .claim_tree = Some(t_b.clone());
    let need_drift = evaluate_submit(
        &snap,
        &profile,
        &obs_b,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-1"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: Some(rev_ev.clone()),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
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
            assert_eq!(stale_node_ids, vec!["task:T1:contract".to_owned()]);
            assert_eq!(error.code, ErrorCode::TaskCandidateMismatch);
        }
        other => panic!("expected MarkStaleAndReject, got {other:?}"),
    }

    // Restore claim_tree to t_a and complete contract_review, build, review, deliver (with receipt: true!)
    snap.nodes
        .iter_mut()
        .find(|n| n.node_id == "task:T1:contract_review")
        .unwrap()
        .claim_tree = Some(t_a.clone());
    let p_cr = evaluate_submit_plan(
        &snap,
        &profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract_review")]),
            action: SubmitAction::Pass,
            worker_id: w("rev-1"),
            claim_revision: Some(2),
            expected_execution_digest: None,
            review_evidence: Some(rev_ev.clone()),
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p_cr);

    // Build -> verify -> pass
    let c_b = admit_claim(
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
            span_id: "1000000000000004".to_owned(),
            force: false,
            steal_remote: false,
            previous_local_pid_alive: None,
        },
    )
    .unwrap();
    apply_claim_in_memory(&mut snap, &c_b);
    let v_b = evaluate_verify(
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
            output_spec: json!({}),
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
                compile_error_count: 0,
                tests_passed: 3,
                tests_failed: 0,
                tests_ignored: 0,
                passed_tests: vec!["tests::c1".to_owned()],
                failed_tests: Vec::new(),
                ignored_tests: Vec::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: t_a.clone(),
                candidate_tree_after: Some(t_a.clone()),
            },
        },
    )
    .unwrap();
    record_verify_in_memory(&mut snap, &v_b);
    let p_b = evaluate_submit_plan(
        &snap,
        &profile,
        &obs_a,
        &SubmitInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("build")]),
            action: SubmitAction::Pass,
            worker_id: w("builder-1"),
            claim_revision: Some(1),
            expected_execution_digest: Some(v_b.execution_digest),
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p_b);

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
                snapshot_ref: None,
                snapshot_commit: None,
                summary: None,
            },
        )
        .unwrap();
        apply_plan_in_memory(&mut snap, &rej);
        if attempt < 3 {
            // Re-claim & re-pass build for next cycle
            snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build").unwrap().status = NodeStatus::Completed;
            snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build").unwrap().accepted_tree = Some(t_a.clone());
            snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:review").unwrap().status = NodeStatus::Ready;
        }
    }
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T1:review").unwrap().status,
        NodeStatus::Escalated
    );

    // Reopen review, pass review, then deliver (testing subtask missing evidence failure first!).
    let reopen_rev = plan_reopen(
        &snap,
        &obs_a,
        &ReopenInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("review")]),
            cascade: false,
            reason: None,
        },
    )
    .unwrap();
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
            snapshot_ref: None,
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
                snapshot_ref: None,
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
            snapshot_ref: None,
            snapshot_commit: Some(commit('9')),
            summary: Some("delivered".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut snap, &p_d);

    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T1").unwrap().status,
        NodeStatus::Completed
    );
    assert!(snap.dependencies.iter().find(|d| d.task_ref == t("T2")).unwrap().satisfied);
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T2").unwrap().status,
        NodeStatus::Ready
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.node_id == "task:T2:contract").unwrap().status,
        NodeStatus::Ready
    );
    assert!(snap.artifacts.iter().any(|a| a.node_id == "task:T1:deliver" && a.kind == ArtifactKind::Receipt));

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
    let mut drift_profile = canonical_profile(IsolationMode::Worktree);
    drift_profile.gates.remove(0);
    if let MachineGraphItem::TasksBlock(ref mut tb) = drift_profile.gates[0] {
        tb.gates[3].fail_drift = Some("rearchitect".to_owned());
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
        let build_n = d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build").unwrap();
        build_n.status = NodeStatus::Completed;
        build_n.accepted_tree = Some(t_a.clone());
        build_n.accepted_definition_digest = Some("def:build".to_owned());
        build_n.worker_id = Some(w("b1"));
        d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:contract").unwrap().status = NodeStatus::Completed;
        d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:contract_review").unwrap().status = NodeStatus::Completed;
        d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:review").unwrap().status = NodeStatus::Ready;
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
            snapshot_ref: None,
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
        d_snap.nodes.iter().find(|n| n.node_id == "task:T1:rearchitect").unwrap().status,
        NodeStatus::Ready
    );
    let reset_build = d_snap.nodes.iter().find(|n| n.node_id == "task:T1:build").unwrap();
    assert_eq!(reset_build.status, NodeStatus::Pending);
    assert_eq!(reset_build.accepted_tree, None);
    assert_eq!(reset_build.accepted_definition_digest, None);
    assert_eq!(reset_build.worker_id, None);
    assert_eq!(reset_build.session_fingerprint, None);
    assert_eq!(reset_build.active_span_id, None);
    assert_eq!(
        d_snap.nodes.iter().find(|n| n.node_id == "task:T1:review").unwrap().status,
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
            snapshot_ref: None,
            snapshot_commit: None,
            summary: Some("rearchitected".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &pass_arch);
    assert_eq!(
        d_snap.nodes.iter().find(|n| n.node_id == "task:T1:build").unwrap().status,
        NodeStatus::Ready
    );

    // Happy-path deliver on T2 cancels unused Pending `rearchitect`.
    {
        for gid in ["contract", "contract_review", "build", "review"] {
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
        d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T2:deliver").unwrap().status =
            NodeStatus::Ready;
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
            snapshot_ref: None,
            snapshot_commit: Some(commit('8')),
            summary: Some("delivered".to_owned()),
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &pass_d2);
    assert_eq!(
        d_snap.nodes.iter().find(|n| n.node_id == "task:T2:rearchitect").unwrap().status,
        NodeStatus::Cancelled
    );

    // ArchitecturalDrift when `fail_drift` sibling (`rearchitect`) was excluded by `when:` falls back to `fail` (`build`).
    d_snap.nodes.retain(|n| n.node_id != "task:T1:rearchitect");
    d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build").unwrap().status =
        NodeStatus::Completed;
    d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build").unwrap().accepted_tree =
        Some(t_a.clone());
    d_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:review").unwrap().status =
        NodeStatus::Ready;
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
            snapshot_ref: None,
            snapshot_commit: None,
            summary: None,
        },
    )
    .unwrap();
    apply_plan_in_memory(&mut d_snap, &rej_fallback);
    assert_eq!(
        d_snap.nodes.iter().find(|n| n.node_id == "task:T1:build").unwrap().status,
        NodeStatus::Ready
    );
    let fb_art = d_snap
        .artifacts
        .iter()
        .filter(|a| a.node_id == "task:T1:review" && a.kind == ArtifactKind::Findings)
        .max_by_key(|a| a.claim_revision)
        .unwrap();
    let fb_stored = crate::machine::submit::parse_findings_artifact(&fb_art.payload_json).unwrap();
    assert!(fb_stored.reset_worker_context);
    assert_eq!(fb_stored.reset_gate_id.as_deref(), Some("build"));
}

#[test]
fn extend_scope_reopen_cascade_and_sync_matrices() {
    let (profile, mut snap) = build_sample_graph(IsolationMode::Worktree, true);
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
    snap.artifacts.push(crate::types::GateArtifactRecord {
        run_id: snap.run.run_id.clone(),
        node_id: "root:blueprint_synthesis.synthesize".to_owned(),
        claim_revision: 1,
        kind: ArtifactKind::Proof,
        payload_json: r#"{"tasks":{"T1":{}}}"#.to_owned(),
        snapshot_ref: None,
        snapshot_commit: None,
        created_at: "2026-11-17T09:10:00.000Z".to_owned(),
    });
    for tid in ["task:T1", "task:T1:contract", "task:T3", "task:T3:contract"] {
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
    // Claim T1.contract; T3 is still unclaimed (claim_revision == 0) so extending T1 into `docs/guide/**` is ALLOWED,
    // while extending into `Cargo.toml` fails with OUT_OF_SCOPE_CHANGES;
    // then claiming T3 and extending T1 into `docs/guide/foo.md` fails with TASK_SCOPE_CONFLICT!
    let c1 = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
                node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
            .find(|n| n.node_id == "task:T1:contract")
            .unwrap()
            .contract_revision,
        2
    );

    let c3 = admit_claim(
        &snap,
        &profile,
        &obs,
        &ClaimInput {
            node_cursor: NodeCursor::task(t("T3"), vec![g("contract")]),
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
                node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
                paths: vec!["docs/guide/page.md".to_owned()],
                reason: "collides with claimed T3".to_owned(),
                worker_id: Some(w("w1")),
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::TaskScopeConflict
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
        snap.nodes.iter().find(|n| n.node_id == "task:T1:contract").unwrap().claim_revision,
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
    for nid in ["task:T2", "task:T2:contract"] {
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
        snap.nodes.iter().find(|n| n.node_id == "task:T2:contract").unwrap().status,
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
    let (_, mut chain_snap) = build_sample_graph(IsolationMode::Worktree, false);
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
        node_id: "task:T4:contract".to_owned(),
        parent_id: Some("task:T4".to_owned()),
        kind: NodeKind::TaskGate,
        task_ref: Some(t4.clone()),
        gate_id: "contract".to_owned(),
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
                node_cursor: NodeCursor::task(t4, vec![g("contract")]),
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
    let (p9_base_profile, mut p9_snap) = build_sample_graph(IsolationMode::Worktree, false);
    if let Some(c) = p9_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:contract") {
        c.status = NodeStatus::Completed;
    }
    if let Some(b) = p9_snap.nodes.iter_mut().find(|n| n.node_id == "task:T1:build") {
        b.status = NodeStatus::Ready;
        b.claim_revision = 0;
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
        p9_snap.nodes.iter().find(|n| n.node_id == "task:T1:build").unwrap().status,
        NodeStatus::Pending,
        "unclaimed later Ready sibling gate must demote to Pending"
    );

    // #5: Root gate demotion when `rebind_profile` inserts an earlier root sibling gate ahead of unclaimed `Ready` sibling.
    let (r_base, mut r_snap) = build_sample_graph(IsolationMode::Worktree, true);
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
    let (rc_base, mut rc_snap) = build_sample_graph(IsolationMode::Worktree, true);
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
    let (profile, snap) = build_sample_graph(IsolationMode::Worktree, false);
    let obs = obs_with_tree(tree('a'));
    let out = evaluate_transition(
        &snap,
        &profile,
        &obs,
        &TransitionInput::Claim(ClaimInput {
            node_cursor: NodeCursor::task(t("T1"), vec![g("contract")]),
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
