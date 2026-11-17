//! Claim context assembly (`ClaimResp`) enforcing 6-tier budget reservation and prompt rendering.

use crate::profile::{
    check_guidance_budget, resolve_and_budget_guidance, GateDef, ResolvedGateProfile,
};
use crate::spec::{Receipt, SpecDocument};
use crate::types::{BlackboardRecord, GateNodeRecord, RejectionFindings, RunRecord};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, utf8_tail};
use acdd_core::ids::{GateId, NodeCursor, RunId, TreeOid, WorkerId};
use acdd_core::ports::{CompositeDomainProvider, DomainContextReq};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Maximum blackboard messages included in Tier 5 (`ADR-0006`).
const MAX_BLACKBOARD_ITEMS: usize = 15;
/// Maximum character length per blackboard payload in Tier 5 (`ADR-0006`).
const MAX_BLACKBOARD_CHARS: usize = 500;

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
    /// Active claim revision.
    pub claim_revision: u32,
    /// Active worker ID.
    pub worker_id: WorkerId,
    /// Assigned worktree path.
    pub worktree: PathBuf,
    /// Candidate tree OID at claim admission.
    pub candidate_tree: TreeOid,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_lenses: BTreeMap<String, Value>,
    /// Lower-priority sections omitted due to `response_max_bytes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_sections: Vec<String>,
    /// Serialized response byte size.
    pub total_bytes: usize,
}

/// Assemble `ClaimResp` under the strict 6-tier budget reservation order (`GuidanceBudgetExceeded` on Tier 1 overflow).
pub fn build_claim_context(
    req: &ClaimContextRequest,
    provider: &CompositeDomainProvider,
) -> Result<ClaimResp, StateError> {
    let node_cursor = NodeCursor::from_node_id(&req.node.node_id)?;
    let inspect_cmd = format!("acdd show {} --run {}", node_cursor, req.run.run_id);

    // Tier 1: Mandatory contract + guidance (fail-closed, never silently truncated).
    let contract = build_contract_value(req);
    let contract_bytes = estimate_json_bytes(&contract);

    let mut guidance_texts = resolve_and_budget_guidance(
        &req.profile,
        &req.gate,
        contract_bytes,
        req.response_max_bytes,
    )?;
    if let Some(role_name) = &req.gate.role {
        if let Some(role_def) = req.profile.roles.get(role_name) {
            if let Some(role_guidance) = &role_def.guidance {
                guidance_texts.push(format!("[role:{role_name}] {role_guidance}"));
            }
        }
    }
    let guidance: Vec<Value> = guidance_texts.into_iter().map(Value::String).collect();
    let guidance_bytes = estimate_json_bytes(&guidance);
    check_guidance_budget(
        contract_bytes,
        guidance_bytes,
        &req.profile.limits,
        req.response_max_bytes,
    )?;

    let mut resp = ClaimResp {
        run_id: req.run.run_id.clone(),
        node_cursor: node_cursor.clone(),
        claim_revision: req.claim_revision,
        contract_revision: req.node.contract_revision,
        worker_id: req.worker_id.clone(),
        worktree: req.worktree.to_string_lossy().into_owned(),
        candidate_tree: req.candidate_tree.clone(),
        inspect_cmd,
        contract,
        guidance,
        unresolved_findings: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard: Vec::new(),
        context_lenses: BTreeMap::new(),
        omitted_sections: Vec::new(),
        total_bytes: 0,
    };

    let mut running_bytes = estimate_json_bytes(&resp);
    if running_bytes > req.response_max_bytes {
        return Err(StateError::new(
            ErrorCode::GuidanceBudgetExceeded,
            format!(
                "mandatory claim metadata + contract + guidance ({running_bytes} bytes) exceeds response_max_bytes ({} bytes)",
                req.response_max_bytes
            ),
        ));
    }

    // Tier 2: Unresolved findings + tail of failed trace log.
    apply_tier2_findings(&mut resp, &mut running_bytes, req);

    // Tier 3: Pre-task architectural slice.
    if let Some(slice) = &req.pre_task_slice {
        // `,"pre_task_slice":` = 18 bytes
        let slice_delta = 18 + estimate_json_bytes(slice);
        if running_bytes + slice_delta <= req.response_max_bytes {
            resp.pre_task_slice = Some(slice.clone());
            running_bytes += slice_delta;
        } else {
            add_omitted_section(&mut resp, &mut running_bytes, "pre_task_slice".to_owned());
        }
    }

    // Tier 4: Completed dependency receipts.
    apply_tier4_receipts(&mut resp, &mut running_bytes, req)?;

    // Tier 5: Blackboard items (up to 15 x 500 chars).
    apply_tier5_blackboard(&mut resp, &mut running_bytes, req);

    // Tier 6: Dynamic DomainProvider context lenses in YAML declaration order.
    apply_tier6_context_lenses(&mut resp, &mut running_bytes, req, provider, &node_cursor)?;

    debug_assert_eq!(running_bytes, estimate_json_bytes(&resp));

    // Exact serialized total_bytes fixpoint.
    resp.total_bytes = estimate_json_bytes(&resp);
    resp.total_bytes = estimate_json_bytes(&resp);

    Ok(resp)
}

fn build_contract_value(req: &ClaimContextRequest) -> Value {
    let mut contours = serde_json::Map::new();
    for contour_id in &req.gate.contours {
        if let Some(def) = req.profile.contours.get(contour_id) {
            contours.insert(
                contour_id.clone(),
                json!({
                    "description": def.description,
                    "criteria": def.criteria,
                }),
            );
        }
    }

    let task_spec = req.node.task_ref.as_ref().and_then(|tref| {
        req.spec
            .as_ref()
            .and_then(|spec| spec.tasks.iter().find(|t| &t.task_ref == tref))
    });

    let mut obj = serde_json::Map::new();
    obj.insert("gate_id".to_owned(), json!(req.gate.id));
    obj.insert("proof".to_owned(), json!(req.gate.proof));
    obj.insert("scope".to_owned(), json!(req.node.scope));
    if !req.node.extended_scope.is_empty() {
        obj.insert(
            "extended_scope".to_owned(),
            json!(req.node.extended_scope),
        );
    }
    obj.insert("invariants".to_owned(), json!(req.node.invariants));
    obj.insert("tools".to_owned(), json!(req.gate.tools));
    if let Some(cmd) = &req.gate.command {
        obj.insert("command".to_owned(), json!(cmd));
    }
    if let Some(call) = &req.gate.call {
        obj.insert("call".to_owned(), json!(call));
    }
    if !req.gate.checks.is_empty() {
        obj.insert("checks".to_owned(), json!(req.gate.checks));
    }
    if !contours.is_empty() {
        obj.insert("contours".to_owned(), Value::Object(contours));
    }

    if let Some(spec) = &req.spec {
        obj.insert("spec_id".to_owned(), json!(spec.id));
        obj.insert("title".to_owned(), json!(spec.title));
        if let Some(task) = task_spec {
            obj.insert("task_ref".to_owned(), json!(task.task_ref));
            if let Some(target) = &task.target {
                obj.insert("target".to_owned(), json!(target));
            }
            if let Some(body) = &task.spec {
                obj.insert("task_spec".to_owned(), json!(body));
            }
            if !task.acceptance_criteria.is_empty() {
                obj.insert(
                    "acceptance_criteria".to_owned(),
                    json!(task.acceptance_criteria),
                );
            }
            if !task.subtasks.is_empty() {
                obj.insert("subtasks".to_owned(), json!(task.subtasks));
            }
        } else {
            if let Some(root_spec) = &spec.spec {
                obj.insert("spec".to_owned(), json!(root_spec));
            }
            if !spec.acceptance_criteria.is_empty() {
                obj.insert(
                    "acceptance_criteria".to_owned(),
                    json!(spec.acceptance_criteria),
                );
            }
        }
    }

    Value::Object(obj)
}

fn add_omitted_section(resp: &mut ClaimResp, running_bytes: &mut usize, section: String) {
    // `,"omitted_sections":[]` = 22 bytes on first element, `,` = 1 byte thereafter.
    let overhead = if resp.omitted_sections.is_empty() { 22 } else { 1 };
    *running_bytes += overhead + estimate_json_bytes(&section);
    resp.omitted_sections.push(section);
}

fn apply_tier2_findings(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) {
    if req.unresolved_findings.is_none() && req.failed_trace_log_tail.is_none() {
        return;
    }
    let tail = req
        .failed_trace_log_tail
        .as_deref()
        .map(|s| utf8_tail(s, req.profile.limits.diagnostic_inline_bytes));

    // `,"unresolved_findings":` = 23 bytes
    let full_payload = json!({
        "findings": req.unresolved_findings,
        "failed_trace_log_tail": tail,
    });
    let full_delta = 23 + estimate_json_bytes(&full_payload);
    if *running_bytes + full_delta <= req.response_max_bytes {
        resp.unresolved_findings = Some(full_payload);
        *running_bytes += full_delta;
        return;
    }

    if req.unresolved_findings.is_some() && tail.is_some() {
        let findings_only = json!({
            "findings": req.unresolved_findings,
        });
        let findings_delta = 23 + estimate_json_bytes(&findings_only);
        if *running_bytes + findings_delta <= req.response_max_bytes {
            resp.unresolved_findings = Some(findings_only);
            *running_bytes += findings_delta;
            return;
        }
    }

    add_omitted_section(resp, running_bytes, "unresolved_findings".to_owned());
}

fn apply_tier4_receipts(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) -> Result<(), StateError> {
    if req.dependency_receipts.is_empty() {
        return Ok(());
    }
    let mut omitted = false;
    for receipt in &req.dependency_receipts {
        let val = serde_json::to_value(receipt).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize dependency receipt: {err}"),
            )
        })?;
        // `,"dependency_receipts":[]` = 25 bytes on first element, `,` = 1 byte thereafter.
        let overhead = if resp.dependency_receipts.is_empty() { 25 } else { 1 };
        let delta = overhead + estimate_json_bytes(&val);
        if *running_bytes + delta <= req.response_max_bytes {
            resp.dependency_receipts.push(val);
            *running_bytes += delta;
        } else {
            omitted = true;
            break;
        }
    }
    if omitted {
        add_omitted_section(resp, running_bytes, "dependency_receipts".to_owned());
    }
    Ok(())
}

fn apply_tier5_blackboard(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) {
    if req.blackboard_items.is_empty() {
        return;
    }
    let start = req
        .blackboard_items
        .len()
        .saturating_sub(MAX_BLACKBOARD_ITEMS);
    let mut omitted = false;
    for item in &req.blackboard_items[start..] {
        let truncated_payload: String = item.payload.chars().take(MAX_BLACKBOARD_CHARS).collect();
        let entry = json!({
            "id": item.id,
            "author": item.author,
            "topic": item.topic,
            "status": item.status,
            "gate": item.gate,
            "payload": truncated_payload,
            "created_at": item.created_at,
        });
        // `,"blackboard":[]` = 16 bytes on first element, `,` = 1 byte thereafter.
        let overhead = if resp.blackboard.is_empty() { 16 } else { 1 };
        let delta = overhead + estimate_json_bytes(&entry);
        if *running_bytes + delta <= req.response_max_bytes {
            resp.blackboard.push(entry);
            *running_bytes += delta;
        } else {
            omitted = true;
            break;
        }
    }
    if omitted {
        add_omitted_section(resp, running_bytes, "blackboard".to_owned());
    }
}

fn admitted_scope_for_request(req: &ClaimContextRequest) -> Vec<String> {
    let mut combined = Vec::new();
    let mut seen = BTreeSet::new();
    for path in &req.node.scope {
        if seen.insert(path.clone()) {
            combined.push(path.clone());
        }
    }
    for ext in &req.node.extended_scope {
        if seen.insert(ext.path.clone()) {
            combined.push(ext.path.clone());
        }
    }
    if combined.is_empty() {
        if let (Some(tref), Some(spec)) = (&req.node.task_ref, &req.spec) {
            if let Some(task) = spec.tasks.iter().find(|t| &t.task_ref == tref) {
                for path in &task.scope {
                    if seen.insert(path.clone()) {
                        combined.push(path.clone());
                    }
                }
                for ext in &task.extended_scope {
                    if seen.insert(ext.path.clone()) {
                        combined.push(ext.path.clone());
                    }
                }
            }
        }
    }
    combined
}

fn apply_tier6_context_lenses(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
    provider: &CompositeDomainProvider,
    node_cursor: &NodeCursor,
) -> Result<(), StateError> {
    if req.gate.context.is_empty() {
        return Ok(());
    }
    let gate_id = GateId::new(&req.gate.id)?;
    let admitted_scope = admitted_scope_for_request(req);
    let target = req.node.task_ref.as_ref().and_then(|tref| {
        req.spec
            .as_ref()
            .and_then(|spec| spec.tasks.iter().find(|t| &t.task_ref == tref))
            .and_then(|t| t.target.clone())
    });

    for qualified_lens in &req.gate.context {
        let remaining = req.response_max_bytes.saturating_sub(*running_bytes);
        if remaining == 0 {
            add_omitted_section(resp, running_bytes, format!("context:{qualified_lens}"));
            continue;
        }

        let ctx_req = DomainContextReq {
            run_id: req.run.run_id.clone(),
            node_cursor: node_cursor.clone(),
            gate_id: gate_id.clone(),
            workspace_root: PathBuf::from(&req.run.workspace),
            worktree: req.worktree.clone(),
            admitted_scope: admitted_scope.clone(),
            target: target.clone(),
            max_bytes: remaining,
        };

        let val = provider.provide_context(qualified_lens, &ctx_req).map_err(|err| {
            if let Some(state_err) = StateError::from_anyhow(&err) {
                state_err.clone()
            } else {
                StateError::new(
                    ErrorCode::DomainCapabilityUnavailable,
                    format!("context lens '{qualified_lens}' failed: {err}"),
                )
            }
        })?;

        let new_val_bytes = estimate_json_bytes(&val);
        if let Some(old_val) = resp.context_lenses.get(qualified_lens) {
            let old_val_bytes = estimate_json_bytes(old_val);
            let next_bytes = *running_bytes - old_val_bytes + new_val_bytes;
            if next_bytes <= req.response_max_bytes {
                resp.context_lenses.insert(qualified_lens.clone(), val);
                *running_bytes = next_bytes;
            } else {
                resp.context_lenses.remove(qualified_lens);
                let removed_delta = old_val_bytes
                    + estimate_json_bytes(qualified_lens)
                    + 1
                    + if resp.context_lenses.is_empty() { 20 } else { 1 };
                *running_bytes -= removed_delta;
                add_omitted_section(resp, running_bytes, format!("context:{qualified_lens}"));
            }
        } else {
            // `,"context_lenses":{}` = 20 bytes on first entry, `,` = 1 byte thereafter; plus `<key>:<val>`.
            let overhead = if resp.context_lenses.is_empty() { 20 } else { 1 };
            let entry_delta = overhead + estimate_json_bytes(qualified_lens) + 1 + new_val_bytes;
            if *running_bytes + entry_delta <= req.response_max_bytes {
                resp.context_lenses.insert(qualified_lens.clone(), val);
                *running_bytes += entry_delta;
            } else {
                add_omitted_section(resp, running_bytes, format!("context:{qualified_lens}"));
            }
        }
    }

    Ok(())
}

/// Render `ClaimResp` into Markdown for `RoleDef.runner` `stdin` delivery using optional `prompt_template`.
pub fn render_claim_prompt_markdown(claim: &ClaimResp, template: Option<&str>) -> String {
    let default_md = render_default_markdown(claim);
    let Some(tpl) = template.map(str::trim).filter(|s| !s.is_empty()) else {
        return default_md;
    };

    let claim_json = serde_json::to_string_pretty(claim).unwrap_or_else(|_| "{}".to_owned());
    let contract_json =
        serde_json::to_string_pretty(&claim.contract).unwrap_or_else(|_| "{}".to_owned());
    let guidance_md = render_guidance_lines(&claim.guidance);
    let findings_json = claim
        .unresolved_findings
        .as_ref()
        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        .unwrap_or_default();
    let pre_task_json = claim
        .pre_task_slice
        .as_ref()
        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        .unwrap_or_default();
    let receipts_json =
        serde_json::to_string_pretty(&claim.dependency_receipts).unwrap_or_else(|_| "[]".to_owned());
    let blackboard_json =
        serde_json::to_string_pretty(&claim.blackboard).unwrap_or_else(|_| "[]".to_owned());
    let lenses_json =
        serde_json::to_string_pretty(&claim.context_lenses).unwrap_or_else(|_| "{}".to_owned());

    let includes_authority = tpl.contains("{contract}") || tpl.contains("{claim_json}");
    let rendered = tpl
        .replace("{run_id}", claim.run_id.as_str())
        .replace("{node_cursor}", &claim.node_cursor.to_string())
        .replace("{claim_revision}", &claim.claim_revision.to_string())
        .replace("{contract_revision}", &claim.contract_revision.to_string())
        .replace("{worker_id}", claim.worker_id.as_str())
        .replace("{worktree}", &claim.worktree)
        .replace("{candidate_tree}", claim.candidate_tree.as_str())
        .replace("{inspect_cmd}", &claim.inspect_cmd)
        .replace("{contract}", &contract_json)
        .replace("{guidance}", &guidance_md)
        .replace("{unresolved_findings}", &findings_json)
        .replace("{pre_task_slice}", &pre_task_json)
        .replace("{dependency_receipts}", &receipts_json)
        .replace("{blackboard}", &blackboard_json)
        .replace("{context_lenses}", &lenses_json)
        .replace("{claim_json}", &claim_json);

    if includes_authority {
        rendered
    } else {
        format!("{rendered}\n\n{default_md}")
    }
}

fn render_guidance_lines(guidance: &[Value]) -> String {
    let mut out = String::new();
    for item in guidance {
        if let Some(s) = item.as_str() {
            out.push_str("- ");
            out.push_str(s);
            out.push('\n');
        } else {
            out.push_str("- ");
            out.push_str(&item.to_string());
            out.push('\n');
        }
    }
    out
}

fn render_default_markdown(claim: &ClaimResp) -> String {
    let mut out = format!(
        "# Gate Claim: `{}`\n\n\
         - Run: `{}`\n\
         - Claim Revision: `{}`\n\
         - Contract Revision: `{}`\n\
         - Worker: `{}`\n\
         - Worktree: `{}`\n\
         - Candidate Tree: `{}`\n\
         - Inspect: `{}`\n\n\
         ## Contract\n\n```json\n{}\n```\n\n\
         ## Guidance\n\n{}\n",
        claim.node_cursor,
        claim.run_id,
        claim.claim_revision,
        claim.contract_revision,
        claim.worker_id,
        claim.worktree,
        claim.candidate_tree,
        claim.inspect_cmd,
        serde_json::to_string_pretty(&claim.contract).unwrap_or_else(|_| "{}".to_owned()),
        render_guidance_lines(&claim.guidance),
    );

    if let Some(findings) = &claim.unresolved_findings {
        out.push_str("\n## Unresolved Findings\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(findings).unwrap_or_default());
        out.push_str("\n```\n");
    }
    if let Some(slice) = &claim.pre_task_slice {
        out.push_str("\n## Pre-Task Architectural Slice\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(slice).unwrap_or_default());
        out.push_str("\n```\n");
    }
    if !claim.dependency_receipts.is_empty() {
        out.push_str("\n## Dependency Receipts\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(&claim.dependency_receipts).unwrap_or_default());
        out.push_str("\n```\n");
    }
    if !claim.blackboard.is_empty() {
        out.push_str("\n## Blackboard\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(&claim.blackboard).unwrap_or_default());
        out.push_str("\n```\n");
    }
    if !claim.context_lenses.is_empty() {
        out.push_str("\n## Context Lenses\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(&claim.context_lenses).unwrap_or_default());
        out.push_str("\n```\n");
    }
    if !claim.omitted_sections.is_empty() {
        out.push_str("\n## Omitted Sections\n\n");
        for section in &claim.omitted_sections {
            out.push_str("- ");
            out.push_str(section);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{GateGuidanceRef, GuidanceEntry, InlineGuidance, RoleDef};
    use crate::spec::{SpecDocument, SpecStatus, TaskSpec};
    use crate::types::{
        BlackboardStatus, BlackboardTopic, FindingItem, FindingSeverity, InvariantsState,
        IsolationMode, NodeKind, NodeStatus, RunStatus,
    };
    use acdd_core::ids::{CommitOid, RunName, TaskRef};
    use acdd_core::ports::{
        DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
        DomainCheckReq, DomainProvider,
    };
    use std::sync::Arc;

    struct LensMockProvider {
        domain: String,
        lenses: BTreeMap<String, Value>,
    }

    impl DomainProvider for LensMockProvider {
        fn domain_id(&self) -> &str {
            &self.domain
        }

        fn capabilities(&self) -> DomainCapabilities {
            DomainCapabilities {
                context_lenses: self.lenses.keys().cloned().collect(),
                ..DomainCapabilities::default()
            }
        }

        fn execute_action(
            &self,
            _action: &str,
            _params: &Value,
            _idempotency_key: &str,
            _req: &DomainActionReq,
        ) -> anyhow::Result<DomainActionResult> {
            anyhow::bail!("unused")
        }

        fn run_check(
            &self,
            _check: &str,
            _params: &Value,
            _req: &DomainCheckReq,
        ) -> anyhow::Result<DomainCheckOutcome> {
            anyhow::bail!("unused")
        }

        fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
            self.lenses
                .get(lens)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing lens '{lens}'"))
        }
    }

    fn base_request() -> ClaimContextRequest {
        let run_id = RunId::new("run-1").unwrap();
        let worker_id = WorkerId::new("w1").unwrap();
        let tree = TreeOid::new("1".repeat(40)).unwrap();
        let commit = CommitOid::new("2".repeat(40)).unwrap();
        let mut profile = ResolvedGateProfile::default();
        profile
            .guidance
            .insert("g1".to_owned(), GuidanceEntry::Text("Follow rules".to_owned()));
        profile
            .roles
            .insert("builder".to_owned(), RoleDef::default());

        let gate = GateDef {
            id: "build".to_owned(),
            role: Some("builder".to_owned()),
            guidance: vec![GateGuidanceRef::Slot("g1".to_owned())],
            context: vec!["code.symbols".to_owned(), "code.tests".to_owned()],
            ..GateDef::default()
        };

        let run = RunRecord {
            run_id: run_id.clone(),
            run_name: RunName::new("r1").unwrap(),
            collection: Some("milestones".to_owned()),
            workspace: "/tmp/ws".to_owned(),
            profile_id: "code".to_owned(),
            profile_json: "{}".to_owned(),
            spec_path: Some("docs/milestones/AC-001.md".to_owned()),
            spec_digest: "a".repeat(64),
            is_ephemeral: false,
            isolation: IsolationMode::Worktree,
            target_base: Some("main".to_owned()),
            target_base_commit: Some(commit),
            branch: Some("milestone/r1".to_owned()),
            worktree: Some("/tmp/ws".to_owned()),
            trace_id: "3".repeat(32),
            status: RunStatus::Active,
            cancellation_reason: None,
            root_cycle_count: 0,
            generation: 1,
            created_at: "2026-11-17T10:00:00.000Z".to_owned(),
            completed_at: None,
        };

        let node = GateNodeRecord {
            run_id: run_id.clone(),
            node_id: "task:t1:build".to_owned(),
            parent_id: Some("task:t1".to_owned()),
            kind: NodeKind::TaskGate,
            task_ref: Some(TaskRef::new("t1").unwrap()),
            gate_id: "build".to_owned(),
            seq_order: 1,
            status: NodeStatus::InProgress,
            node_version: 1,
            isolation: IsolationMode::Worktree,
            attempt: 0,
            cycle_count: 0,
            max_attempts: 3,
            claim_revision: 1,
            contract_revision: 1,
            worker_id: Some(worker_id.clone()),
            session_fingerprint: None,
            branch: None,
            worktree: Some("/tmp/ws".to_owned()),
            baseline_commit: None,
            claim_tree: Some(tree.clone()),
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: vec!["default".to_owned()],
            invariants: InvariantsState::default(),
            scope: vec!["src/**".to_owned()],
            extended_scope: Vec::new(),
            active_span_id: None,
        };

        let spec = SpecDocument {
            id: "AC-001".to_owned(),
            title: "Test Spec".to_owned(),
            doc_type: "contract".to_owned(),
            profile: None,
            status: SpecStatus::Active,
            cancellation_reason: None,
            spec: Some("Root spec".to_owned()),
            depends_on: Vec::new(),
            owners: Vec::new(),
            invariants: Vec::new(),
            acceptance_criteria: Vec::new(),
            guidance: None,
            required_skills: Vec::new(),
            repositories: Vec::new(),
            linked_to: Vec::new(),
            closure: None,
            tasks: vec![TaskSpec {
                task_ref: TaskRef::new("t1").unwrap(),
                target: Some("Implement feature".to_owned()),
                spec: Some("Details".to_owned()),
                scope: vec!["src/**".to_owned()],
                extended_scope: Vec::new(),
                depends_on: Vec::new(),
                tags: vec!["default".to_owned()],
                invariants: Vec::new(),
                acceptance_criteria: vec!["Works".to_owned()],
                contract_revision: 1,
                status: SpecStatus::Active,
                subtasks: Vec::new(),
                receipt: None,
            }],
            body: String::new(),
            digest: "b".repeat(64),
        };

        ClaimContextRequest {
            run,
            node,
            gate,
            profile,
            spec: Some(spec),
            claim_revision: 1,
            worker_id,
            worktree: PathBuf::from("/tmp/ws"),
            candidate_tree: tree,
            unresolved_findings: None,
            failed_trace_log_tail: None,
            pre_task_slice: None,
            dependency_receipts: Vec::new(),
            blackboard_items: Vec::new(),
            response_max_bytes: 32_768,
        }
    }

    #[test]
    fn context_six_tier_budget_and_guidance_fail_closed_matrix() {
        let empty_provider = CompositeDomainProvider::default();

        // Case 1: Guidance overflow fails closed with GuidanceBudgetExceeded (never silently truncated).
        let mut overflow_req = base_request();
        overflow_req.profile.limits.max_guidance_bytes = 128;
        overflow_req.gate.guidance.push(GateGuidanceRef::Inline(InlineGuidance {
            inline: "x".repeat(512),
        }));
        let err = build_claim_context(&overflow_req, &empty_provider).expect_err("must fail closed");
        assert_eq!(err.code, ErrorCode::GuidanceBudgetExceeded);

        // Case 2: Tier priority ordering drops lower-priority context lenses first, keeps higher tiers,
        // caps blackboard at 15 x 500 chars, and preserves YAML lens declaration order.
        let mut tiered_req = base_request();
        tiered_req.unresolved_findings = Some(RejectionFindings {
            summary: "Fix defect".to_owned(),
            blackboard_message_id: Some(7),
            items: vec![FindingItem {
                severity: FindingSeverity::High,
                path: Some("src/lib.rs".to_owned()),
                message: "Missing guard".to_owned(),
                remediation: "Add guard".to_owned(),
            }],
        });
        tiered_req.failed_trace_log_tail = Some("stderr line\n".repeat(10));
        tiered_req.pre_task_slice = Some(json!({ "layer": "core", "summary": "slice" }));
        tiered_req.dependency_receipts = vec![Receipt {
            run_id: RunId::new("run-1").unwrap(),
            node_cursor: NodeCursor::task(TaskRef::new("t0").unwrap(), vec![GateId::new("deliver").unwrap()]),
            gate_id: GateId::new("deliver").unwrap(),
            worker_id: WorkerId::new("w0").unwrap(),
            claim_revision: 1,
            contract_revision: 1,
            candidate_tree: TreeOid::new("4".repeat(40)).unwrap(),
            committed_oid: None,
            accepted_at: "2026-11-17T10:00:00.000Z".to_owned(),
            need_proofs: BTreeMap::new(),
            rollup: None,
        }];
        tiered_req.blackboard_items = (1..=20)
            .map(|i| BlackboardRecord {
                id: Some(i),
                run_id: RunId::new("run-1").unwrap(),
                node_id: Some("task:t1".to_owned()),
                recipient_node_id: None,
                subtask_ref: None,
                gate: Some("build".to_owned()),
                author: "w1".to_owned(),
                topic: BlackboardTopic::Notes,
                status: BlackboardStatus::Open,
                payload: format!("note-{i}:{}", "z".repeat(600)),
                created_at: "2026-11-17T10:00:00.000Z".to_owned(),
            })
            .collect();

        let mut lenses = BTreeMap::new();
        lenses.insert("symbols".to_owned(), json!({ "items": ["fn_a", "fn_b"] }));
        lenses.insert("tests".to_owned(), json!({ "blob": "t".repeat(1500) }));
        let code_provider = Arc::new(LensMockProvider {
            domain: "code".to_owned(),
            lenses,
        });
        let provider = CompositeDomainProvider::new(vec![code_provider]).unwrap();

        let full = build_claim_context(&tiered_req, &provider).unwrap();
        assert_eq!(full.blackboard.len(), 15);
        for item in &full.blackboard {
            assert!(item["payload"].as_str().unwrap().chars().count() <= 500);
        }
        assert!(full.context_lenses.contains_key("code.symbols"));
        assert!(full.context_lenses.contains_key("code.tests"));

        // Tight budget: fits Tier 1..5 and `code.symbols`, must omit `code.tests` (2nd in YAML order).
        tiered_req.response_max_bytes = full.total_bytes - 500;
        let trimmed = build_claim_context(&tiered_req, &provider).unwrap();
        assert!(trimmed.total_bytes <= tiered_req.response_max_bytes);
        assert!(trimmed.unresolved_findings.is_some());
        assert!(trimmed.pre_task_slice.is_some());
        assert_eq!(trimmed.dependency_receipts.len(), 1);
        assert!(!trimmed.blackboard.is_empty());
        assert!(trimmed.context_lenses.contains_key("code.symbols"));
        assert!(!trimmed.context_lenses.contains_key("code.tests"));
        assert!(trimmed.omitted_sections.contains(&"context:code.tests".to_owned()));

        // Disabled/missing provider lens fails closed with DomainCapabilityUnavailable.
        let err_missing = build_claim_context(&tiered_req, &empty_provider).unwrap_err();
        assert_eq!(err_missing.code, ErrorCode::DomainCapabilityUnavailable);

        // Markdown rendering works both with fallback template and custom template.
        let md_default = render_claim_prompt_markdown(&trimmed, None);
        assert!(md_default.contains("# Gate Claim: `t1.build`"));
        assert!(md_default.contains("## Contract"));
        assert!(md_default.contains("## Guidance"));

        let md_custom = render_claim_prompt_markdown(
            &trimmed,
            Some("Run={run_id} Node={node_cursor} Rev={claim_revision}\n{contract}"),
        );
        assert!(md_custom.contains("Run=run-1 Node=t1.build Rev=1"));
        assert!(md_custom.contains("\"gate_id\": \"build\""));
    }
}
