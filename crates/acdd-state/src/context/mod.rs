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
    collect_dependency_receipts, extract_slice_and_proof_pointers, read_spec_readonly,
};
use crate::workflow::{ProofDef, ProofKind};
use crate::types::{BlackboardListReq, BlackboardStatus};
use acdd_core::errors::AcddError;
use acdd_core::format::estimate_json_bytes;
use acdd_core::ids::NodeCursor;
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
use acdd_spec::{Subtask, SubtaskStatus};

/// Single authoritative context builder shared by `claim`, `show`, and `run`.
pub struct GateContextAssembler;

impl GateContextAssembler {
    /// Build a [`ClaimContextRequest`] from a [`RunGraphSnapshot`] with container rework diagnostics and filtered blackboard state.
    pub(crate) fn build_request_from_snapshot(
        input: SnapshotContextInput<'_>,
    ) -> Result<ClaimContextRequest, AcddError> {
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
        let (pre_task_slice, dependency_receipts, blackboard_items, proof_pointers) =
            if skip_enrichment {
                (None, Vec::new(), Vec::new(), Vec::new())
            } else {
                let (slice, pointers) = extract_slice_and_proof_pointers(snapshot, node);
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
                (slice, receipts, bb, pointers)
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
            .sort_by_key(|subtask| subtask.status != SubtaskStatus::InProgress);
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
            hard_max_bytes: Some(match detail {
                acdd_core::response::Detail::Compact => config.response_policy.max_output_bytes,
                acdd_core::response::Detail::Full => config
                    .response_policy
                    .max_output_bytes
                    .min(CLAIM_FULL_MAX_BYTES),
            }),
            full_detail: matches!(detail, acdd_core::response::Detail::Full),
            proof_pointers,
        })
    }

    /// Validate Tier 1 mandatory budget (`contract` + `guidance` + mandatory envelope) and build initial `ClaimResp`.
    pub(crate) fn preflight_mandatory(
        req: &ClaimContextRequest,
    ) -> Result<(ClaimResp, NodeCursor, usize), AcddError> {
        budget::preflight_mandatory(req)
    }

    /// Assemble `ClaimResp` under the strict 6-tier budget reservation order (`GuidanceBudgetExceeded` on Tier 1 or response hard-cap overflow).
    pub fn assemble(
        req: &ClaimContextRequest,
        provider: &CompositeDomainProvider,
    ) -> Result<ClaimResp, AcddError> {
        let (mut resp, node_cursor, mut running_bytes) = Self::preflight_mandatory(req)?;
        let response_hard_cap = req.effective_hard_max_bytes();
        let optional_target_cap = req.optional_target_cap();
        let target_cap = req.omitted_budget_cap(running_bytes);

        // Tier 2: Unresolved findings + tail of failed trace log.
        tiers::apply_tier2_findings(
            &mut resp,
            &mut running_bytes,
            req,
            target_cap,
            response_hard_cap,
        )?;

        // Tier 3: Pre-task architectural slice.
        tiers::apply_tier3_pre_task_slice(
            &mut resp,
            &mut running_bytes,
            req,
            target_cap,
            response_hard_cap,
        )?;

        // Tier 4: Completed dependency receipts.
        tiers::apply_tier4_receipts(
            &mut resp,
            &mut running_bytes,
            req,
            target_cap,
            response_hard_cap,
        )?;

        // Tier 5: Blackboard items (up to 6 x 220 chars).
        let bb_relevant_total = tiers::apply_tier5_blackboard(
            &mut resp,
            &mut running_bytes,
            req,
            target_cap,
            response_hard_cap,
        )?;

        // Tier 6: Dynamic DomainProvider context lenses in YAML declaration order.
        tiers::apply_tier6_context_lenses(
            &mut resp,
            &mut running_bytes,
            req,
            provider,
            &node_cursor,
            target_cap,
            response_hard_cap,
            bb_relevant_total,
        )?;
        running_bytes += budget::add_live_subtask_evidence(
            &mut resp.contract,
            &req.live_subtasks,
            optional_target_cap.saturating_sub(running_bytes),
        );

        debug_assert_eq!(running_bytes, estimate_json_bytes(&resp));

        // Exact serialized total_bytes fixpoint, unconditionally enforcing `response_hard_cap`.
        let pointers_populated_in_tier3 = !resp.omitted_proof_pointers.is_empty();
        let (mut settled, _) =
            budget::enforce_hard_cap_with_reclaim(&mut resp, response_hard_cap, bb_relevant_total)?;
        if !pointers_populated_in_tier3
            && resp.pre_task_slice.is_none()
            && resp.omitted_sections.iter().any(|s| s == "pre_task_slice")
            && resp.omitted_proof_pointers.is_empty()
            && !req.proof_pointers.is_empty()
        {
            resp.omitted_proof_pointers = req.proof_pointers.clone();
            let (after_ptrs, _) = budget::enforce_hard_cap_with_reclaim(
                &mut resp,
                response_hard_cap,
                bb_relevant_total,
            )?;
            settled = after_ptrs;
        }
        debug_assert!(settled <= response_hard_cap);

        Ok(resp)
    }
}

/// Assemble `ClaimResp` under the strict 6-tier budget reservation order (`GuidanceBudgetExceeded` on Tier 1 overflow).
#[cfg(test)]
pub(crate) fn build_claim_context(
    req: &ClaimContextRequest,
    provider: &CompositeDomainProvider,
) -> Result<ClaimResp, AcddError> {
    GateContextAssembler::assemble(req, provider)
}
