pub(super) use acdd_core::ids::{CommitOid, GateId, RunId, RunName, TaskRef, TreeOid, WorkerId};
pub(super) use acdd_state::harness::RunHarness;
pub(super) use acdd_state::machine::claim::admit_claim;
pub(super) use acdd_state::machine::inputs::VerifyEvaluation;
pub(super) use acdd_state::machine::schedule::check_post_task_integration_barrier;
pub(super) use acdd_state::machine::*;
pub(super) use acdd_state::store::traits::{exec_tx_with, StateReader, StateTx};
pub(super) use acdd_spec::InvariantsState;
pub(super) use acdd_state::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, GateNodeRecord, NodeVersionExpectation,
    RunGraphSnapshot, StoragePreconditions, TransitionPlan, ValidatedObservations,
};
pub(super) use std::collections::BTreeSet;

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
    op: impl FnOnce(&mut dyn StateTx, &RunId) -> Result<(), acdd_core::errors::AcddError>,
) {
    let backend = RunHarness::in_memory().expect("memory backend");
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
                            expected_run_generation: transition
                                .preconditions
                                .expected_run_generation
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
    let added_ids: BTreeSet<&str> = plan
        .added_nodes
        .iter()
        .map(|n| n.node_id.as_str())
        .collect();
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

pub(super) fn make_task(
    name: &str,
    tags: &[&str],
    scope: &[&str],
    deps: &[&str],
    run_id: &RunId,
) -> MachineTaskSpec {
    RunHarness::make_sample_task(name, tags, scope, deps, run_id)
}
