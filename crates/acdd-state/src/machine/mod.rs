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

pub use claim::admit_claim;
pub use graph::{
    build_initial_run_graph, container_scope, effective_task_tags, matches_when, WhenExpr,
};
pub use inputs::{
    ActionExecutionOutcome, BuildGraphInput, ClaimDecision, ClaimInput, ExtendScopeInput,
    MachineCheckDef, MachineExpectedFailure, MachineGateDef, MachineGitPolicy, MachineGraphItem,
    MachineProfileInput, MachineProofKind, MachineSavePolicy, MachineSpecInput,
    MachineSubtaskInput, MachineTaskDep, MachineTaskSpec, MachineTasksBlockDef, ReopenInput,
    SubmitEvaluation,
    SubmitInput, SyncInput, SyncPlan, TransitionInput, TransitionOutcome, VerifyEvaluation,
    VerifyExecutionObservation, VerifyInput,
};
pub use reopen::plan_reopen;
pub use schedule::{
    check_post_task_integration_barrier, compute_schedule, patterns_overlap, scopes_overlap,
    ScheduleReport,
};
pub use scope::plan_extend_scope;
pub use submit::{evaluate_submit, evaluate_submit_plan, plan_container_blackboard_cleanup};
pub use sync::plan_sync;
pub use verify::{compute_execution_digest, evaluate_verify, ExecutionDigestInput};

use crate::types::{RunGraphSnapshot, ValidatedObservations};
use acdd_core::errors::StateError;

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
            plan_extend_scope(snapshot, profile, req)?,
        ))),
        TransitionInput::Reopen(req) => Ok(TransitionOutcome::Plan(Box::new(plan_reopen(
            snapshot, obs, req,
        )?))),
        TransitionInput::Sync(req) => Ok(TransitionOutcome::Sync(Box::new(plan_sync(
            snapshot, profile, obs, req,
        )?))),
    }
}
