//! 100% pure functional state-machine transitions and DAG scheduling core.
//!
//! This module MUST remain completely free of `std::fs`, `std::process`, `std::net`, and `rusqlite`.

pub mod claim;
pub mod graph;
pub mod inputs;
pub mod reopen;
pub mod schedule;
pub mod scope;
pub mod submit;
pub mod sync;
pub mod verify;

#[cfg(test)]
mod tests;

use claim::admit_claim;
pub use graph::{
    build_initial_run_graph, container_scope, effective_task_tags, matches_when,
};
pub use inputs::{
    ActionExecutionOutcome, BuildGraphInput, ClaimDecision, ClaimInput, ExtendScopeInput,
    MachineCheckDef, MachineGateDef, MachineGitPolicy, MachineGraphItem, MachineProfileInput,
    MachineProofKind, MachineSavePolicy, MachineSpecInput, MachineSubtaskInput, MachineTaskDep,
    MachineTaskSpec, MachineTasksBlockDef, ReopenInput, SubmitEvaluation, SubmitInput, SyncInput,
    SyncPlan, TransitionInput, TransitionOutcome, VerifyExecutionObservation, VerifyInput,
};
pub(crate) use reopen::plan_reopen;
pub use schedule::compute_schedule;
pub use submit::{evaluate_submit, plan_container_blackboard_cleanup};
pub(crate) use sync::plan_sync;
pub use verify::{compute_execution_digest, evaluate_verify, ExecutionDigestInput};

use crate::types::{GateNodeRecord, NodeKind, RunGraphSnapshot, RunStatus, ValidatedObservations};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::NodeCursor;

/// Verify the run is active, resolve `cursor` to a canonical `node_id`, and return the [`GateNodeRecord`].
pub(crate) fn require_active_node<'a>(
    snapshot: &'a RunGraphSnapshot,
    cursor: &NodeCursor,
    action_label: &str,
) -> Result<&'a GateNodeRecord, StateError> {
    if snapshot.run.status != RunStatus::Active {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot {action_label} in non-active run '{}' (status: {})",
                snapshot.run.run_id, snapshot.run.status
            ),
        ));
    }
    let node_id = cursor.to_node_id()?;
    graph::find_node(snapshot, &node_id)
}

/// Verify the run is active and `cursor` resolves to a claimable gate node (`RootGate` or `TaskGate`).
pub(crate) fn require_active_claimable_node<'a>(
    snapshot: &'a RunGraphSnapshot,
    cursor: &NodeCursor,
    action_label: &str,
) -> Result<&'a GateNodeRecord, StateError> {
    let node = require_active_node(snapshot, cursor, action_label)?;
    if !matches!(node.kind, NodeKind::RootGate | NodeKind::TaskGate) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot {action_label} structural node '{}' (kind: {})",
                node.node_id, node.kind
            ),
        ));
    }
    Ok(node)
}

/// Unified pure state-machine transition dispatcher.
pub fn evaluate_transition(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &TransitionInput,
) -> Result<TransitionOutcome, StateError> {
    match input {
        TransitionInput::Claim(req) => Ok(TransitionOutcome::Claim(Box::new(admit_claim(
            snapshot, profile, obs, req,
        )?))),
        TransitionInput::Verify(req) => Ok(TransitionOutcome::Verify(Box::new(evaluate_verify(
            snapshot, profile, obs, req,
        )?))),
        TransitionInput::Submit(req) => Ok(TransitionOutcome::Submit(Box::new(evaluate_submit(
            snapshot, profile, obs, req,
        )?))),
        TransitionInput::ExtendScope(req) => Ok(TransitionOutcome::Plan(Box::new(
            scope::plan_extend_scope_with_obs(snapshot, profile, obs, req)?,
        ))),
        TransitionInput::Reopen(req) => Ok(TransitionOutcome::Plan(Box::new(plan_reopen(
            snapshot, obs, req,
        )?))),
        TransitionInput::Sync(req) => Ok(TransitionOutcome::Sync(Box::new(plan_sync(
            snapshot, profile, obs, req,
        )?))),
    }
}
