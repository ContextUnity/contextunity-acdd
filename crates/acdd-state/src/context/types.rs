//! Shared data types and helper functions for `ClaimResp` assembly and context lenses.

use crate::profile::{GateDef, ResolvedGateProfile};
use crate::spec::{Receipt, SpecDocument, Subtask};
use crate::store::traits::StateReader;
use crate::types::{
    BlackboardRecord, GateNodeRecord, RejectionFindings, RunGraphSnapshot, RunRecord,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::ids::{NodeCursor, RunId, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Input parameters for assembling a budgeted `ClaimResp`.
#[derive(Debug, Clone)]
pub struct ClaimContextRequest {
    /// Owning `Run` record.
    pub run: RunRecord,
    /// Claimed `GateNodeRecord`.
    pub node: GateNodeRecord,
    /// Resolved `GateDef` for the node.
    pub gate: GateDef,
    /// Full resolved profile holding guidance slots and limits.
    pub profile: ResolvedGateProfile,
    /// Optional parsed specification document.
    pub spec: Option<SpecDocument>,
    /// Current subtask checklist from the owning task's persisted rows.
    pub live_subtasks: Vec<Subtask>,
    /// Active claim revision.
    pub claim_revision: u32,
    /// Active worker ID.
    pub worker_id: WorkerId,
    /// Assigned worktree path.
    pub worktree: PathBuf,
    /// Candidate tree OID at claim admission.
    pub candidate_tree: TreeOid,
    /// Workspace path policy enforced when context lenses inspect files.
    pub path_policy: WorkspacePathPolicy,
    /// Unresolved rejection findings from the prior failed/rejected attempt, if any.
    pub unresolved_findings: Option<RejectionFindings>,
    /// Tail of the latest failed trace log, if any.
    pub failed_trace_log_tail: Option<String>,
    /// Pre-task blueprint slice (`proof.tasks[<task_ref>]`) if present.
    pub pre_task_slice: Option<Value>,
    /// Receipts of completed dependency tasks.
    pub dependency_receipts: Vec<Receipt>,
    /// Relevant blackboard messages (capped to 6 x 220 chars per `ADR-0006`).
    pub blackboard_items: Vec<BlackboardRecord>,
    /// True when prior `architectural_drift` findings require discarding cached builder context.
    pub context_reset_required: bool,
    /// Total byte ceiling from `ResponsePolicy.max_output_bytes`.
    pub response_max_bytes: usize,
}

/// Budgeted response payload returned by `claim` and rendered for role runner `stdin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimResp {
    /// Active run identifier.
    pub run_id: RunId,
    /// Claimed node cursor.
    pub node_cursor: NodeCursor,
    /// Active claim revision.
    pub claim_revision: u32,
    /// Active contract/scope revision.
    pub contract_revision: u32,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Assigned worktree directory.
    pub worktree: String,
    /// Candidate tree OID at claim admission.
    pub candidate_tree: TreeOid,
    /// Suggested inspection command (`acdd show <node_cursor> --run <run_id>`).
    pub inspect_cmd: String,
    /// True when prior `architectural_drift` findings require discarding cached builder context.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context_reset_required: bool,
    /// Tier 1: Mandatory contract/task specification excerpt.
    pub contract: Value,
    /// Tier 1: Mandatory resolved guidance entries (never truncated; fails with `GuidanceBudgetExceeded`).
    pub guidance: Vec<Value>,
    /// Tier 2: Unresolved findings and failed trace log tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_findings: Option<Value>,
    /// Tier 3: Pre-task architectural slice for this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_task_slice: Option<Value>,
    /// Tier 4: Completed dependency task receipts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_receipts: Vec<Value>,
    /// Tier 5: Blackboard notes/blockers/decisions (up to 6 x 220 chars).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blackboard: Vec<Value>,
    /// Tier 6: Dynamic `DomainProvider` context lenses in YAML declaration order.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub context_lenses: Map<String, Value>,
    /// Lower-priority sections omitted due to `response_max_bytes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_sections: Vec<String>,
    /// Serialized response byte size.
    pub total_bytes: usize,
}

/// Canonical git baseline reference (`claim_tree` for `in_place`, `baseline_commit` for `worktree`).
pub(crate) fn baseline_ref_for_node(node: &GateNodeRecord) -> Option<String> {
    match node.isolation {
        crate::types::IsolationMode::InPlace => node
            .claim_tree
            .as_ref()
            .map(|t| t.to_string())
            .or_else(|| node.baseline_commit.as_ref().map(|c| c.to_string())),
        crate::types::IsolationMode::Worktree => node
            .baseline_commit
            .as_ref()
            .map(|c| c.to_string())
            .or_else(|| node.claim_tree.as_ref().map(|t| t.to_string())),
    }
}

/// Parameters for [`super::GateContextAssembler::build_request_from_snapshot`].
pub(crate) struct SnapshotContextInput<'a> {
    pub config: &'a StateEngineConfig,
    pub reader: &'a dyn StateReader,
    pub snapshot: &'a RunGraphSnapshot,
    pub node: &'a GateNodeRecord,
    pub gate: &'a GateDef,
    pub profile: &'a ResolvedGateProfile,
    pub claim_revision: u32,
    pub worker_id: WorkerId,
    pub worktree: PathBuf,
    pub candidate_tree: TreeOid,
    pub path_policy: WorkspacePathPolicy,
    pub primary: &'a Path,
    pub detail: acdd_core::response::Detail,
}
