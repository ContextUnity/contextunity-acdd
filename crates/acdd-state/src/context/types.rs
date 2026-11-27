//! Shared data types and helper functions for `ClaimResp` assembly and context lenses.

use crate::workflow::{GateDef, ResolvedWorkflow};
use crate::store::traits::StateReader;
use crate::types::{
    BlackboardRecord, GateNodeRecord, RejectionFindings, RunGraphSnapshot, RunRecord,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::ids::{NodeCursor, RunId, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_spec::{Receipt, SpecDocument, Subtask};
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
    pub profile: ResolvedWorkflow,
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
    /// Target response byte ceiling (`10,000` compact soft ceiling, `32,768` full ceiling, or explicit test cap).
    pub response_max_bytes: usize,
    /// Optional hard response envelope byte cap (`ResponsePolicy.max_output_bytes` when `response_max_bytes` is a soft ceiling).
    pub hard_max_bytes: Option<usize>,
    /// Whether `detail="full"` was explicitly requested.
    pub full_detail: bool,
    /// Compact inspection commands for pre-task/predecessor proof artifacts when omitted from `pre_task_slice`.
    pub proof_pointers: Vec<String>,
}

impl ClaimContextRequest {
    /// Hard response envelope ceiling for the entire `ClaimResp` (`hard_max_bytes` override or `response_max_bytes`).
    pub(crate) fn effective_hard_max_bytes(&self) -> usize {
        self.hard_max_bytes.unwrap_or(self.response_max_bytes)
    }

    /// Hard Tier 1 cap on mandatory metadata, `contract`, and `guidance` (`limits.max_guidance_bytes.min(response_hard_cap)`).
    pub(crate) fn tier1_cap(&self) -> usize {
        self.profile
            .limits
            .max_guidance_bytes
            .min(self.effective_hard_max_bytes())
    }

    /// Soft target ceiling for optional Tiers 2–6, never exceeding `effective_hard_max_bytes()`.
    pub(crate) fn optional_target_cap(&self) -> usize {
        self.response_max_bytes
            .min(self.effective_hard_max_bytes())
    }

    /// Byte ceiling for recording `omitted_sections`: uses `optional_target_cap()` when Tier 1 fits,
    /// and expands to `effective_hard_max_bytes()` when mandatory Tier 1 exceeds `optional_target_cap()`.
    pub(crate) fn omitted_budget_cap(&self, tier1_bytes: usize) -> usize {
        let response_hard_cap = self.effective_hard_max_bytes();
        let optional_target_cap = self.optional_target_cap();
        if tier1_bytes > optional_target_cap {
            response_hard_cap
        } else {
            optional_target_cap
        }
    }
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
    /// Tier 1: Mandatory resolved guidance entries (never truncated; fails with `GuidanceBudgetExceeded` on hard-cap overflow).
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
    /// True when assembled under `detail="full"` budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_detail: bool,
    /// CLI inspection pointers for omitted pre-task / predecessor proofs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_proof_pointers: Vec<String>,
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
    pub profile: &'a ResolvedWorkflow,
    pub claim_revision: u32,
    pub worker_id: WorkerId,
    pub worktree: PathBuf,
    pub candidate_tree: TreeOid,
    pub path_policy: WorkspacePathPolicy,
    pub primary: &'a Path,
    pub detail: acdd_core::response::Detail,
}
