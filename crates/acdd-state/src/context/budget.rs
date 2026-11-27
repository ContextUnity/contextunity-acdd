//! Tiered `ClaimResp` budget reservation constants and Tier 1 contract/subtask sizing.

use super::types::{ClaimContextRequest, ClaimResp};
use crate::workflow::{check_guidance_budget, resolve_and_budget_guidance, ProofDef, ResolvedWorkflow};
use crate::types::GateNodeRecord;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::format::estimate_json_bytes;
use acdd_core::ids::NodeCursor;
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use acdd_core::{pack_knapsack_indices, KnapsackItem};
use acdd_spec::Subtask;
use serde_json::{json, Map, Value};

/// Maximum blackboard messages included in Tier 5 (`ADR-0006`).
pub const MAX_BLACKBOARD_ITEMS: usize = 6;
/// Maximum character length per blackboard payload in Tier 5 (`ADR-0006`).
pub const MAX_BLACKBOARD_CHARS: usize = 220;
/// Minimum byte budget floor offered to each Tier 6 context lens when remaining response space permits.
pub const MIN_LENS_BUDGET_BYTES: usize = 4_096;
/// Maximum byte slice of Tier 1 remaining budget allocated to live subtask checklist rows.
const SUBTASK_PROJECTION_BUDGET_CAP_BYTES: usize = 1_024;
/// Maximum live subtask rows projected into `contract.subtasks`.
const MAX_PROJECTED_SUBTASKS: usize = 16;
/// Maximum byte cap for inline completed/cancelled subtask evidence inside `contract.subtasks`.
const SUBTASK_EVIDENCE_BUDGET_CAP_BYTES: usize = 512;
/// Default soft byte ceiling for compact `ClaimResp` envelopes (`10,000` bytes).
pub(crate) const CLAIM_DEFAULT_MAX_BYTES: usize = 10_000;
/// Opt-in `detail="full"` byte ceiling for `ClaimResp` envelopes (`32 KiB`).
pub(crate) const CLAIM_FULL_MAX_BYTES: usize = 32_768;

/// Converge `resp.total_bytes` to its exact minimal serialized JSON byte length fixpoint.
pub(super) fn settle_total_bytes(resp: &mut ClaimResp) -> usize {
    resp.total_bytes = 0;
    for _ in 0..4 {
        let len = estimate_json_bytes(resp);
        if resp.total_bytes == len {
            return len;
        }
        resp.total_bytes = len;
    }
    resp.total_bytes = estimate_json_bytes(resp);
    resp.total_bytes
}

/// Validate Tier 1 mandatory budget (`contract` + `guidance` + mandatory envelope) and build initial `ClaimResp`.
pub(super) fn preflight_mandatory(
    req: &ClaimContextRequest,
) -> Result<(ClaimResp, NodeCursor, usize), AcddError> {
    let node_cursor = NodeCursor::from_node_id(&req.node.node_id)?;
    let inspect_cmd = format!("acdd show {} --run {}", node_cursor, req.run.run_id);
    let response_hard_cap = req.effective_hard_max_bytes();
    let tier1_cap = req.tier1_cap();
    let optional_target_cap = req.optional_target_cap();

    // Tier 1: Mandatory contract + guidance (fail-closed against `tier1_cap`, never silently truncated).
    let contract = build_contract_value(req);
    let contract_bytes = estimate_json_bytes(&contract);

    let guidance_texts =
        resolve_and_budget_guidance(&req.profile, &req.gate, contract_bytes, response_hard_cap)?;
    let guidance: Vec<Value> = guidance_texts.into_iter().map(Value::String).collect();
    let guidance_bytes = estimate_json_bytes(&guidance);
    check_guidance_budget(
        contract_bytes,
        guidance_bytes,
        &req.profile.limits,
        response_hard_cap,
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
        context_reset_required: req.context_reset_required,
        contract,
        guidance,
        unresolved_findings: None,
        pre_task_slice: None,
        dependency_receipts: Vec::new(),
        blackboard: Vec::new(),
        context_lenses: serde_json::Map::new(),
        omitted_sections: Vec::new(),
        full_detail: req.full_detail,
        omitted_proof_pointers: Vec::new(),
        total_bytes: 0,
    };

    let tier1_bytes = settle_total_bytes(&mut resp);
    if tier1_bytes > tier1_cap {
        return Err(AcddError::new(
            ErrorCode::GuidanceBudgetExceeded,
            format!(
                "mandatory claim metadata + contract + guidance ({tier1_bytes} bytes) exceeds Tier 1 cap ({tier1_cap} bytes)"
            ),
        ));
    }

    let working_cap = req.omitted_budget_cap(tier1_bytes);
    resp.total_bytes = working_cap.max(tier1_bytes);
    let running_bytes = estimate_json_bytes(&resp);

    project_live_subtasks(
        &mut resp.contract,
        &req.live_subtasks,
        optional_target_cap.saturating_sub(running_bytes) / 2,
    );
    let running_bytes = estimate_json_bytes(&resp);
    Ok((resp, node_cursor, running_bytes))
}

pub(super) fn is_pre_task_root_in_profile(
    profile: &ResolvedWorkflow,
    node: &GateNodeRecord,
) -> bool {
    let top_id = node
        .node_id
        .strip_prefix("root:")
        .and_then(|rest| rest.split('.').next())
        .unwrap_or(&node.gate_id);
    for g in &profile.gates {
        if g.tasks.is_some() {
            return false;
        }
        if g.id == top_id {
            return true;
        }
    }
    false
}

pub(super) fn add_live_subtask_evidence(
    contract: &mut Value,
    subtasks: &[Subtask],
    remaining: usize,
) -> usize {
    // Keep short evidence when it fits. The complete records remain available through subtask reads.
    let budget = remaining.min(SUBTASK_EVIDENCE_BUDGET_CAP_BYTES);
    let Some(items) = contract.get_mut("subtasks").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut used = 0;
    for item in items {
        let evidence = subtasks
            .iter()
            .find(|subtask| item["subtask_ref"] == subtask.subtask_ref.as_str())
            .and_then(|subtask| subtask.evidence.as_deref());
        if let Some(evidence) = evidence {
            let available = budget - used;
            if evidence.len() <= available {
                let bytes = ",\"evidence\":".len() + estimate_json_bytes(evidence);
                if bytes <= available {
                    item["evidence"] = json!(evidence);
                    used += bytes;
                }
            }
        }
    }
    used
}

fn push_unique_omitted(resp: &mut ClaimResp, section: &str) {
    if !resp.omitted_sections.iter().any(|s| s == section) {
        resp.omitted_sections.push(section.to_owned());
    }
}

fn upsert_blackboard_omission(resp: &mut ClaimResp, relevant_total: usize) {
    let omitted = relevant_total.saturating_sub(resp.blackboard.len());
    if omitted == 0 {
        return;
    }
    let entry = format!("blackboard ({omitted} of {relevant_total} omitted)");
    if let Some(slot) = resp
        .omitted_sections
        .iter_mut()
        .find(|s| *s == "blackboard" || s.starts_with("blackboard "))
    {
        *slot = entry;
    } else {
        resp.omitted_sections.push(entry);
    }
}

fn pop_projected_subtask(contract: &mut Value) -> bool {
    let Some(obj) = contract.as_object_mut() else {
        return false;
    };
    let Some(Value::Array(rows)) = obj.get_mut("subtasks") else {
        return false;
    };
    if rows.pop().is_none() {
        return false;
    }
    if rows.is_empty() {
        obj.remove("subtasks");
    }
    let prev_omitted = obj
        .get("subtasks_omitted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    obj.insert("subtasks_omitted".into(), json!(prev_omitted + 1));
    true
}

fn reclaim_one_optional_item(resp: &mut ClaimResp, relevant_blackboard_total: usize) -> bool {
    if let Some(lens_key) = resp.context_lenses.keys().next_back().cloned() {
        resp.context_lenses.shift_remove(&lens_key);
        push_unique_omitted(resp, &format!("context:{lens_key}"));
        return true;
    }
    if resp.blackboard.pop().is_some() {
        let total = relevant_blackboard_total.max(resp.blackboard.len() + 1);
        upsert_blackboard_omission(resp, total);
        return true;
    }
    if resp.dependency_receipts.pop().is_some() {
        push_unique_omitted(resp, "dependency_receipts");
        return true;
    }
    if resp.pre_task_slice.take().is_some() {
        push_unique_omitted(resp, "pre_task_slice");
        return true;
    }
    if resp.omitted_proof_pointers.pop().is_some() {
        return true;
    }
    if let Some(ref mut findings) = resp.unresolved_findings {
        if let Some(obj) = findings.as_object_mut() {
            if obj.remove("failed_trace_log_tail").is_some()
                && obj.get("findings").is_some_and(|v| !v.is_null())
            {
                return true;
            }
        }
        resp.unresolved_findings = None;
        push_unique_omitted(resp, "unresolved_findings");
        return true;
    }
    pop_projected_subtask(&mut resp.contract)
}

/// Settle `resp.total_bytes` and unconditionally enforce `response_hard_cap` by reclaiming optional items first.
pub(super) fn enforce_hard_cap_with_reclaim(
    resp: &mut ClaimResp,
    response_hard_cap: usize,
    relevant_blackboard_total: usize,
) -> Result<(usize, bool), AcddError> {
    let mut reclaimed = false;
    let mut settled = settle_total_bytes(resp);
    while settled > response_hard_cap {
        let bb_total = relevant_blackboard_total.max(resp.blackboard.len());
        if !reclaim_one_optional_item(resp, bb_total) {
            break;
        }
        reclaimed = true;
        settled = settle_total_bytes(resp);
    }
    if settled > response_hard_cap {
        return Err(AcddError::new(
            ErrorCode::GuidanceBudgetExceeded,
            format!(
                "mandatory claim context + required omission notices ({settled} bytes) exceeds hard cap ({response_hard_cap} bytes)"
            ),
        ));
    }
    Ok((settled, reclaimed))
}

/// Record an omitted section notice, reclaiming optional items if needed so omissions are never silently dropped
/// while honoring `response_hard_cap`. Returns whether earlier optional items were reclaimed.
pub(super) fn record_omitted_section(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    section: &str,
    target_cap: usize,
    response_hard_cap: usize,
    relevant_blackboard_total: Option<usize>,
) -> Result<bool, AcddError> {
    let effective_target_cap = target_cap.min(response_hard_cap);
    if section == "blackboard" {
        if let Some(total) = relevant_blackboard_total {
            upsert_blackboard_omission(resp, total);
        }
    } else {
        push_unique_omitted(resp, section);
    }
    let mut reclaimed = false;
    let mut current_bytes = estimate_json_bytes(resp);
    while current_bytes > effective_target_cap {
        let bb_total = relevant_blackboard_total.unwrap_or(resp.blackboard.len());
        if !reclaim_one_optional_item(resp, bb_total) {
            break;
        }
        reclaimed = true;
        current_bytes = estimate_json_bytes(resp);
    }
    if current_bytes > effective_target_cap {
        let bb_total = relevant_blackboard_total.unwrap_or(resp.blackboard.len());
        let (settled, extra_reclaimed) =
            enforce_hard_cap_with_reclaim(resp, response_hard_cap, bb_total)?;
        reclaimed |= extra_reclaimed;
        current_bytes = settled;
    }
    *running_bytes = current_bytes;
    Ok(reclaimed)
}

/// Packs scored items into a JSON lens, preserving any prior truncation and
/// counting the compact envelope and array separators against the byte limit.
pub fn pack_json_lens(
    mut envelope: Map<String, Value>,
    items: impl ExactSizeIterator<Item = (u64, Value)>,
    max_bytes: usize,
) -> (Value, usize) {
    let total = items.len();
    let already_truncated = envelope.get("truncated").and_then(Value::as_bool) == Some(true);
    envelope.entry("total").or_insert_with(|| json!(total));
    envelope.insert("truncated".into(), json!(already_truncated));
    envelope.insert("items".into(), Value::Array(Vec::new()));
    let items: Vec<_> = items.collect();
    let candidates: Vec<_> = items
        .iter()
        .map(|(score, item)| KnapsackItem {
            weight: estimate_json_bytes(item).saturating_add(1),
            score: *score,
        })
        .collect();
    let sum = candidates.iter().fold(0_usize, |sum, candidate| {
        sum.saturating_add(candidate.weight)
    });
    let complete_bytes = estimate_json_bytes(&envelope).saturating_add(sum.saturating_sub(1));
    let kept = if total == 0
        || (complete_bytes <= max_bytes && candidates.iter().all(|item| item.score > 0))
    {
        items.into_iter().map(|(_, item)| item).collect()
    } else {
        envelope.insert("truncated".into(), json!(true));
        let mut capacity = max_bytes
            .checked_sub(estimate_json_bytes(&envelope))
            .map_or(0, |remaining| remaining.saturating_add(1));
        // A complete lens must fit with its longer `false` flag.
        if !already_truncated {
            capacity = capacity.min(sum.saturating_sub(1));
        }
        let mut selected = pack_knapsack_indices(&candidates, capacity)
            .into_iter()
            .peekable();
        items
            .into_iter()
            .enumerate()
            .filter_map(|(index, (_, item))| {
                (selected.peek() == Some(&index)).then(|| {
                    selected.next();
                    item
                })
            })
            .collect()
    };
    envelope.insert("items".into(), Value::Array(kept));
    let packed = Value::Object(envelope);
    let packed_bytes = estimate_json_bytes(&packed);
    (packed, packed_bytes)
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
    let proof_schema = match &req.gate.proof {
        ProofDef::Scheme { scheme } => Some(scheme.as_ref()),
        ProofDef::ResolvedNamed { schema, .. } => Some(schema.as_ref()),
        ProofDef::Named(name) => req.profile.proofs.get(name).map(|p| &p.schema),
        _ => None,
    };
    if let Some(schema) = proof_schema {
        obj.insert("proof_schema".to_owned(), summarize_proof_schema(schema));
    }
    obj.insert("scope".to_owned(), json!(req.node.scope));
    if !req.node.extended_scope.is_empty() {
        obj.insert("extended_scope".to_owned(), json!(req.node.extended_scope));
    }
    if !req.node.invariants.applicable.is_empty() || !req.node.invariants.verified.is_empty() {
        obj.insert("invariants".to_owned(), json!(req.node.invariants));
    }
    let tools = req.profile.effective_tools(&req.gate);
    if !tools.is_empty() && tools != req.profile.default_tools.as_slice() {
        obj.insert("tools".to_owned(), json!(tools));
    }
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
        if !spec.required_skills.is_empty() {
            obj.insert("required_skills".to_owned(), json!(spec.required_skills));
        }
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
        } else {
            let include_full_spec =
                is_pre_task_root_in_profile(&req.profile, &req.node) || spec.tasks.is_empty();
            if include_full_spec {
                if let Some(root_spec) = &spec.spec {
                    obj.insert("spec".to_owned(), json!(root_spec));
                }
            } else if let Some(ref sp) = req.run.spec_path {
                obj.insert("spec_path".to_owned(), json!(sp));
            }
            if !spec.acceptance_criteria.is_empty() {
                obj.insert(
                    "acceptance_criteria".to_owned(),
                    json!(spec.acceptance_criteria),
                );
            }
            if !spec.owners.is_empty() {
                obj.insert("owners".to_owned(), json!(spec.owners));
            }
        }
    }

    if req.live_subtasks.is_empty() {
        if let Some(task) = task_spec.filter(|task| !task.subtasks.is_empty()) {
            obj.insert("subtasks".to_owned(), json!(task.subtasks));
        }
    }

    Value::Object(obj)
}

fn describe_schema_compact(schema: &SchemaDef) -> String {
    let base = if schema.schema_type == "array" {
        schema
            .items
            .as_deref()
            .map(|item| format!("array<{}>", describe_schema_compact(item)))
            .unwrap_or_else(|| "array".to_owned())
    } else {
        schema.schema_type.clone()
    };
    let mut constraints = Vec::new();
    if let Some(min_len) = schema.min_length {
        constraints.push(format!("minLength={min_len}"));
    }
    if let Some(min_items) = schema.min_items {
        constraints.push(format!("minItems={min_items}"));
    }
    if let Some(min_props) = schema.min_properties {
        constraints.push(format!("minProperties={min_props}"));
    }
    if let Some(min) = schema.minimum {
        constraints.push(format!("minimum={min}"));
    }
    if let Some(ref allowed) = schema.enum_values {
        let vals: Vec<String> = allowed
            .iter()
            .map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))
            .collect();
        constraints.push(format!("enum=[{}]", vals.join("|")));
    }
    if constraints.is_empty() {
        base
    } else {
        format!("{base} ({})", constraints.join(", "))
    }
}

fn summarize_proof_schema(schema: &SchemaDef) -> Value {
    let mut out = Map::new();
    out.insert("type".to_owned(), json!(schema.schema_type));
    if !schema.required.is_empty() {
        out.insert("required".to_owned(), json!(schema.required));
    }
    if !schema.properties.is_empty() {
        let fields: Map<String, Value> = schema
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(describe_schema_compact(v))))
            .collect();
        out.insert("fields".to_owned(), Value::Object(fields));
    }
    if let Some(AdditionalPropertiesDef::Schema(task_entry)) = schema
        .properties
        .get("tasks")
        .and_then(|t| t.additional_properties.as_ref())
    {
        if !task_entry.required.is_empty() {
            out.insert(
                "tasks_entry_required".to_owned(),
                json!(task_entry.required),
            );
        }
        if let Some(loc_schema) = task_entry.properties.get("max_net_loc") {
            out.insert(
                "tasks_entry_constraints".to_owned(),
                json!({ "max_net_loc": describe_schema_compact(loc_schema) }),
            );
        }
    }
    Value::Object(out)
}

fn project_live_subtasks(contract: &mut Value, subtasks: &[Subtask], available: usize) {
    let omitted_bytes = ",\"subtasks_omitted\":".len() + estimate_json_bytes(&subtasks.len());
    if subtasks.is_empty() || omitted_bytes > available {
        return;
    }
    let mut remaining = available
        .min(SUBTASK_PROJECTION_BUDGET_CAP_BYTES)
        .saturating_sub(omitted_bytes + ",\"subtasks\":[]".len());
    let mut rows = Vec::new();
    for subtask in subtasks {
        if rows.len() == MAX_PROJECTED_SUBTASKS {
            break;
        }
        let row = json!({
            "subtask_ref": subtask.subtask_ref,
            "title": subtask.title,
            "status": subtask.status,
        });
        let bytes = estimate_json_bytes(&row) + usize::from(!rows.is_empty());
        if bytes <= remaining {
            remaining -= bytes;
            rows.push(row);
        }
    }
    let omitted = subtasks.len() - rows.len();
    if !rows.is_empty() {
        contract["subtasks"] = Value::Array(rows);
    }
    if omitted != 0 {
        contract["subtasks_omitted"] = json!(omitted);
    }
}
