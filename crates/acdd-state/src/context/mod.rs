//! Claim context assembly (`ClaimResp`) enforcing 6-tier budget reservation and prompt rendering.

pub mod budget;
pub mod markdown;
pub mod tiers;

pub use budget::{
    CLAIM_DEFAULT_MAX_BYTES, MAX_BLACKBOARD_CHARS, MAX_BLACKBOARD_ITEMS, MAX_PROJECTED_SUBTASKS,
    MIN_LENS_BUDGET_BYTES, SUBTASK_EVIDENCE_BUDGET_CAP_BYTES, SUBTASK_PROJECTION_BUDGET_CAP_BYTES,
};
pub use markdown::render_claim_prompt_markdown;
pub(crate) use tiers::is_context_reset_required;

use crate::profile::{GateDef, ProofDef, ProofKind, ResolvedGateProfile};
use crate::spec::{Receipt, SpecDocument, Subtask};
use crate::store::traits::StateReader;
use crate::types::{
    BlackboardListReq, BlackboardRecord, BlackboardStatus, GateNodeRecord, RejectionFindings,
    RunGraphSnapshot, RunRecord,
};
use crate::usecases::queries::{
    collect_dependency_receipts, extract_global_blueprint_slice, extract_pre_task_slice,
    extract_predecessor_gate_proofs, read_spec_readonly,
};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::StateError;
use acdd_core::format::estimate_json_bytes;
use acdd_core::ids::{NodeCursor, RunId, TreeOid, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
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
    /// Relevant blackboard messages (capped to 15 x 500 chars per `ADR-0006`).
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
    /// Tier 5: Blackboard notes/blockers/decisions (up to 15 x 500 chars).
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

/// Parameters for [`GateContextAssembler::build_request_from_snapshot`].
pub(crate) struct SnapshotContextInput<'a> {
    pub config: &'a StateEngineConfig,
    pub reader: &'a dyn StateReader,
    pub snapshot: &'a RunGraphSnapshot,
    pub node: &'a GateNodeRecord,
    pub gate: GateDef,
    pub profile: ResolvedGateProfile,
    pub claim_revision: u32,
    pub worker_id: WorkerId,
    pub worktree: PathBuf,
    pub candidate_tree: TreeOid,
    pub path_policy: WorkspacePathPolicy,
    pub primary: &'a Path,
}

/// Single authoritative context builder shared by `claim`, `show`, and `run`.
pub struct GateContextAssembler;

impl GateContextAssembler {
    /// Build a [`ClaimContextRequest`] from a [`RunGraphSnapshot`] with container rework diagnostics and filtered blackboard state.
    pub(crate) fn build_request_from_snapshot(
        input: SnapshotContextInput<'_>,
    ) -> Result<ClaimContextRequest, StateError> {
        let SnapshotContextInput {
            config,
            reader,
            snapshot,
            node,
            gate,
            profile,
            claim_revision,
            worker_id,
            worktree,
            candidate_tree,
            path_policy,
            primary,
        } = input;
        let spec = read_spec_readonly(config, &snapshot.run);
        let skip_enrichment = matches!(gate.proof, ProofDef::Kind(ProofKind::None)) && gate.receipt;
        let (unresolved_findings, failed_trace_log_tail) = tiers::resolve_rework_diagnostics(
            snapshot,
            node,
            primary,
            profile.limits.diagnostic_inline_bytes,
        );
        let context_reset_required = is_context_reset_required(snapshot, node, &gate);
        let (pre_task_slice, dependency_receipts, blackboard_items) = if skip_enrichment {
            (None, Vec::new(), Vec::new())
        } else {
            let mut slice = match node.task_ref.as_ref() {
                Some(tr) => extract_pre_task_slice(snapshot, tr.as_str()),
                None if crate::machine::graph::is_post_task_root_node(snapshot, node) => {
                    extract_global_blueprint_slice(snapshot)
                }
                None => None,
            };
            if let Some(pred_proofs) = extract_predecessor_gate_proofs(snapshot, node) {
                let pred_val = Value::Object(pred_proofs);
                slice = match slice {
                    Some(Value::Object(mut map)) => {
                        map.insert("predecessor_proofs".to_owned(), pred_val);
                        Some(Value::Object(map))
                    }
                    _ => {
                        let mut map = Map::new();
                        map.insert("predecessor_proofs".to_owned(), pred_val);
                        Some(Value::Object(map))
                    }
                };
            }
            let receipts = collect_dependency_receipts(snapshot, node);
            let bb = if config.modules.is_enabled(SubsystemId::Blackboard.as_str()) {
                reader
                    .list_blackboard(
                        &snapshot.run.run_id,
                        &BlackboardListReq {
                            status: Some(BlackboardStatus::Open),
                            limit: 100,
                            offset: 0,
                            ..BlackboardListReq::default()
                        },
                    )?
                    .items
            } else {
                Vec::new()
            };
            (slice, receipts, bb)
        };
        let mut live_subtasks: Vec<_> = snapshot
            .subtasks
            .iter()
            .filter(|subtask| {
                node.task_ref.is_some()
                    && (subtask.node_id == node.node_id
                        || node.parent_id.as_deref() == Some(subtask.node_id.as_str()))
            })
            .map(|subtask| Subtask {
                subtask_ref: subtask.subtask_ref.clone(),
                title: subtask.title.clone(),
                status: subtask.status,
                evidence: subtask.evidence.clone(),
            })
            .collect();
        live_subtasks
            .sort_by_key(|subtask| subtask.status != crate::types::SubtaskStatus::InProgress);
        Ok(ClaimContextRequest {
            run: snapshot.run.clone(),
            node: node.clone(),
            gate,
            profile,
            spec,
            live_subtasks,
            claim_revision,
            worker_id,
            worktree,
            candidate_tree,
            path_policy,
            unresolved_findings,
            failed_trace_log_tail,
            pre_task_slice,
            dependency_receipts,
            blackboard_items,
            context_reset_required,
            response_max_bytes: config
                .response_policy
                .max_output_bytes
                .min(CLAIM_DEFAULT_MAX_BYTES),
        })
    }

    /// Validate Tier 1 mandatory budget (`contract` + `guidance` + mandatory envelope) and build initial `ClaimResp`.
    pub(crate) fn preflight_mandatory(
        req: &ClaimContextRequest,
    ) -> Result<(ClaimResp, NodeCursor, usize), StateError> {
        budget::preflight_mandatory(req)
    }

    /// Assemble `ClaimResp` under the strict 6-tier budget reservation order (`GuidanceBudgetExceeded` on Tier 1 overflow).
    pub fn assemble(
        req: &ClaimContextRequest,
        provider: &CompositeDomainProvider,
    ) -> Result<ClaimResp, StateError> {
        let (mut resp, node_cursor, mut running_bytes) = Self::preflight_mandatory(req)?;

        // Tier 2: Unresolved findings + tail of failed trace log.
        tiers::apply_tier2_findings(&mut resp, &mut running_bytes, req);

        // Tier 3: Pre-task architectural slice.
        tiers::apply_tier3_pre_task_slice(&mut resp, &mut running_bytes, req);

        // Tier 4: Completed dependency receipts.
        tiers::apply_tier4_receipts(&mut resp, &mut running_bytes, req)?;

        // Tier 5: Blackboard items (up to 15 x 500 chars).
        tiers::apply_tier5_blackboard(&mut resp, &mut running_bytes, req);

        // Tier 6: Dynamic DomainProvider context lenses in YAML declaration order.
        tiers::apply_tier6_context_lenses(
            &mut resp,
            &mut running_bytes,
            req,
            provider,
            &node_cursor,
        )?;
        running_bytes += budget::add_live_subtask_evidence(
            &mut resp.contract,
            &req.live_subtasks,
            req.response_max_bytes - running_bytes,
        );

        debug_assert_eq!(running_bytes, estimate_json_bytes(&resp));

        // Exact serialized total_bytes fixpoint.
        resp.total_bytes = estimate_json_bytes(&resp);
        resp.total_bytes = estimate_json_bytes(&resp);

        Ok(resp)
    }
}

/// Assemble `ClaimResp` under the strict 6-tier budget reservation order (`GuidanceBudgetExceeded` on Tier 1 overflow).
pub fn build_claim_context(
    req: &ClaimContextRequest,
    provider: &CompositeDomainProvider,
) -> Result<ClaimResp, StateError> {
    GateContextAssembler::assemble(req, provider)
}
