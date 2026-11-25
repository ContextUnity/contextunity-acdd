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
pub use graph::{build_initial_run_graph, container_scope, effective_task_tags, matches_when};
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

use crate::types::{
    GateNodeRecord, NodeKind, NodeStatus, NodeStatusPatch, RunGraphSnapshot, RunStatus,
    ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, WorkerId};

/// Construct a [`NodeStatusPatch`] that clears active claim ownership (and optionally accepted tree/digest).
pub(crate) fn clear_claim_patch(
    node_id: &str,
    status: NodeStatus,
    clear_accepted: bool,
) -> NodeStatusPatch {
    let mut patch = NodeStatusPatch::status_only(node_id, status);
    if clear_accepted {
        patch.accepted_tree = Some(None);
        patch.accepted_definition_digest = Some(None);
    }
    patch.worker_id = Some(None);
    patch.session_fingerprint = Some(None);
    patch.active_span_id = Some(None);
    patch
}

/// Verify the node is claimed in_progress, matches any expected `claim_revision`, and belongs to `worker_id`.
pub(crate) fn require_claimed_in_progress<'a>(
    snapshot: &'a RunGraphSnapshot,
    cursor: &NodeCursor,
    action_label: &str,
    expected_rev: Option<u32>,
    worker_id: Option<&WorkerId>,
) -> Result<&'a GateNodeRecord, StateError> {
    let node = require_active_claimable_node(snapshot, cursor, action_label)?;
    let node_id = node.node_id.as_str();
    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(StateError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{node_id}' must be in_progress with an active claim before {action_label} (status: {}, claim_revision: {})",
                node.status, node.claim_revision
            ),
        ));
    }
    if let Some(expected) = expected_rev {
        if expected != node.claim_revision {
            return Err(StateError::new(
                ErrorCode::StaleClaimRevision,
                format!(
                    "{action_label} claim_revision {expected} does not match active claim_revision {} on '{node_id}'",
                    node.claim_revision
                ),
            ));
        }
    }
    if let Some(worker) = worker_id {
        if node.worker_id.as_ref() != Some(worker) {
            return Err(StateError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!("worker '{worker}' does not hold the active claim on '{node_id}'"),
            ));
        }
    }
    Ok(node)
}

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
