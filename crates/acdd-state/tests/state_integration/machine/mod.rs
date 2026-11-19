mod reopen_sync;
mod schedule_claim_verify;
mod submit;

use acdd_core::ids::{
    CommitOid, GateId, GateSelector, RunId, RunName, SubtaskRef, TaskRef, TreeOid,
    WorkerId,
};
use acdd_core::schema::SchemaDef;
use acdd_state::machine::*;
use acdd_state::store::{exec_tx_with, MemoryStateBackend, StateReader, StateTx};
use acdd_state::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, GateNodeRecord, InvariantsState,
    IsolationMode, NodeVersionExpectation, RunGraphSnapshot, SaveAction, StoragePreconditions,
    SubtaskStatus, TransitionPlan, ValidatedObservations,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn g(s: &str) -> GateId {
    GateId::new(s).unwrap()
}
pub(super) fn t(s: &str) -> TaskRef {
    TaskRef::new(s).unwrap()
}
pub(super) fn w(s: &str) -> WorkerId {
    WorkerId::new(s).unwrap()
}
pub(super) fn tree(ch: char) -> TreeOid {
    TreeOid::new(ch.to_string().repeat(40)).unwrap()
}
pub(super) fn commit(ch: char) -> CommitOid {
    CommitOid::new(ch.to_string().repeat(40)).unwrap()
}

pub(super) fn obs_with_tree(t_oid: TreeOid) -> ValidatedObservations {
    ValidatedObservations {
        current_tree: Some(t_oid.clone()),
        container_tree: Some(t_oid),
        changed_paths: Vec::new(),
        out_of_scope_paths: Vec::new(),
        unintegrated_tasks: Vec::new(),
        external_satisfied_deps: Vec::new(),
        external_deps_authoritative: false,
        path_policy: None,
        now_utc: "2026-11-17T10:00:00.000Z".to_owned(),
    }
}

fn with_memory_backend(
    snap: &mut RunGraphSnapshot,
    op: impl FnOnce(&mut dyn StateTx, &RunId) -> Result<(), acdd_core::errors::StateError>,
) {
    let backend = MemoryStateBackend::new().expect("memory backend");
    let run_id = snap.run.run_id.clone();
    exec_tx_with(&backend, |tx| {
        tx.insert_run_graph(snap)?;
        op(tx, &run_id)
    })
    .expect("memory state mutation");
    *snap = backend
        .load_run_graph(&run_id)
        .expect("load_run_graph")
        .expect("run exists");
}

pub(super) fn apply_claim_in_memory(snap: &mut RunGraphSnapshot, decision: &ClaimDecision) {
    if let ClaimDecision::Admit { claim, transition } = decision {
        let parent_patches: Vec<_> = transition
            .node_patches
            .iter()
            .filter(|p| p.node_id != claim.node_id)
            .cloned()
            .collect();
        let parent_versions: Vec<_> = parent_patches
            .iter()
            .filter_map(|p| {
                snap.nodes
                    .iter()
                    .find(|n| n.node_id == p.node_id)
                    .map(|n| NodeVersionExpectation {
                        node_id: n.node_id.clone(),
                        expected_version: n.node_version,
                    })
            })
            .collect();
        with_memory_backend(snap, |tx, run_id| {
            tx.admit_claim(run_id, &transition.preconditions, claim)?;
            if !parent_patches.is_empty() {
                tx.apply_transition_plan(
                    run_id,
                    &TransitionPlan {
                        preconditions: StoragePreconditions {
                            expected_run_generation: transition.preconditions.expected_run_generation
                                + 1,
                            expected_node_versions: parent_versions,
                            expected_claim_revision: None,
                            expected_contract_revision: None,
                            expected_worker_id: None,
                            required_predecessor_trees: Vec::new(),
                        },
                        bump_run_generation: false,
                        new_run_status: None,
                        new_root_cycle_count: None,
                        completed_at: None,
                        node_patches: parent_patches,
                        dependency_patches: Vec::new(),
                        claim_patches: Vec::new(),
                        new_artifacts: Vec::new(),
                        new_blackboard: Vec::new(),
                        blackboard_patches: Vec::new(),
                        new_subtasks: Vec::new(),
                        significant_transition: true,
                    },
                )?;
            }
            Ok(())
        });
    }
}

pub(super) fn record_verify_in_memory(snap: &mut RunGraphSnapshot, eval: &VerifyEvaluation) {
    with_memory_backend(snap, |tx, run_id| {
        let row = tx.start_trace(&eval.preconditions, &eval.trace_start)?;
        let mut finish = eval.trace_finish.clone();
        finish.trace_row_id = row.trace_row_id.expect("trace_row_id");
        tx.finish_trace(&finish)?;
        if let Some(ref plan) = eval.failure_transition {
            tx.apply_transition_plan(run_id, plan)?;
        }
        Ok(())
    });
}

pub(super) fn apply_plan_in_memory(snap: &mut RunGraphSnapshot, plan: &TransitionPlan) {
    with_memory_backend(snap, |tx, run_id| tx.apply_transition_plan(run_id, plan));
}

pub(super) fn apply_sync_plan_in_memory(snap: &mut RunGraphSnapshot, plan: &SyncPlan) {
    let added_ids: BTreeSet<&str> = plan.added_nodes.iter().map(|n| n.node_id.as_str()).collect();
    let (deferred_subtasks, immediate_subtasks): (Vec<_>, Vec<_>) = plan
        .transition
        .new_subtasks
        .iter()
        .cloned()
        .partition(|s| added_ids.contains(s.node_id.as_str()));
    let mut adjusted = plan.clone();
    adjusted.transition.new_subtasks = immediate_subtasks;
    with_memory_backend(snap, |tx, run_id| {
        tx.apply_sync_plan(run_id, &adjusted, None)?;
        for sub in &deferred_subtasks {
            tx.upsert_subtask(sub)?;
        }
        Ok(())
    });
}

pub(super) fn canonical_profile(isolation: IsolationMode) -> MachineProfileInput {
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

    let build = MachineGateDef {
        role: Some("builder".to_owned()),
        proof: MachineProofKind::Command,
        command: Some("build_check".to_owned()),
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
                gates: vec![build, task_review, deliver],
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

pub(super) fn make_task(
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

pub(super) fn build_sample_graph(isolation: IsolationMode, with_pre_task: bool) -> (MachineProfileInput, RunGraphSnapshot) {
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

