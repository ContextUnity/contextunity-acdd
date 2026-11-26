//! Claim context assembly (`ClaimResp`) enforcing 6-tier budget reservation and prompt rendering.

pub mod budget;
pub mod markdown;
pub(crate) mod slices;
pub mod tiers;
pub mod types;

pub(crate) use budget::{CLAIM_DEFAULT_MAX_BYTES, CLAIM_FULL_MAX_BYTES};
pub use markdown::render_claim_prompt_markdown;
#[cfg(test)]
mod tests;
pub(crate) use tiers::is_context_reset_required;
pub(crate) use types::{baseline_ref_for_node, SnapshotContextInput};
pub use types::{ClaimContextRequest, ClaimResp};

use self::slices::{
    collect_dependency_receipts, extract_global_blueprint_slice, extract_pre_task_slice,
    extract_predecessor_gate_proofs, read_spec_readonly,
};
use crate::profile::{ProofDef, ProofKind};
use crate::spec::Subtask;
use crate::types::{BlackboardListReq, BlackboardStatus};
use acdd_core::errors::StateError;
use acdd_core::format::estimate_json_bytes;
use acdd_core::ids::NodeCursor;
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
use serde_json::{Map, Value};

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
            detail,
        } = input;
        let spec = read_spec_readonly(config, &snapshot.run);
        let skip_enrichment = matches!(gate.proof, ProofDef::Kind(ProofKind::None)) && gate.receipt;
        let (unresolved_findings, failed_trace_log_tail) = tiers::resolve_rework_diagnostics(
            snapshot,
            node,
            primary,
            profile.limits.diagnostic_inline_bytes,
        );
        let context_reset_required = is_context_reset_required(snapshot, node, gate);
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
            gate: gate.clone(),
            profile: profile.clone(),
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
            response_max_bytes: config.response_policy.max_output_bytes.min(match detail {
                acdd_core::response::Detail::Compact => CLAIM_DEFAULT_MAX_BYTES,
                acdd_core::response::Detail::Full => CLAIM_FULL_MAX_BYTES,
            }),
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
        tiers::apply_tier2_findings(&mut resp, &mut running_bytes, req)?;

        // Tier 3: Pre-task architectural slice.
        tiers::apply_tier3_pre_task_slice(&mut resp, &mut running_bytes, req);

        // Tier 4: Completed dependency receipts.
        tiers::apply_tier4_receipts(&mut resp, &mut running_bytes, req)?;

        // Tier 5: Blackboard items (up to 6 x 220 chars).
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
#[cfg(test)]
pub(crate) fn build_claim_context(
    req: &ClaimContextRequest,
    provider: &CompositeDomainProvider,
) -> Result<ClaimResp, StateError> {
    GateContextAssembler::assemble(req, provider)
}
