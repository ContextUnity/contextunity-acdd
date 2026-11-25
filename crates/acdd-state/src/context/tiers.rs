//! Tier 2–6 `ClaimResp` section assembly, rework diagnostics, blackboard filtering, and context lenses.

use super::budget::{
    add_omitted_section, is_pre_task_root_in_profile, MAX_BLACKBOARD_CHARS, MAX_BLACKBOARD_ITEMS,
    MIN_LENS_BUDGET_BYTES,
};
use super::types::{baseline_ref_for_node, ClaimContextRequest, ClaimResp};
use crate::profile::GateDef;
use crate::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, BlackboardTopic, ClaimOutcome,
    GateArtifactRecord, GateNodeRecord, NodeKind, NodeStatus, RejectionFindings, RunGraphSnapshot,
    TraceStatus,
};
use acdd_core::config::ACDD_DIR;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, utf8_tail};
use acdd_core::fs::open_confined;
use acdd_core::ids::{GateId, NodeCursor};
use acdd_core::ports::{CompositeDomainProvider, DomainContextReq};
use acdd_core::{pack_knapsack_indices, KnapsackItem};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

fn latest_container_findings<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &GateNodeRecord,
) -> (BTreeSet<&'a str>, Option<&'a GateArtifactRecord>) {
    let container_node_ids: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            n.parent_id == node.parent_id
                && matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
        })
        .map(|n| n.node_id.as_str())
        .collect();
    let latest_findings = snapshot
        .artifacts
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            a.kind == ArtifactKind::Findings && container_node_ids.contains(a.node_id.as_str())
        })
        .max_by(|(idx_a, a), (idx_b, b)| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.claim_revision.cmp(&b.claim_revision))
                .then_with(|| idx_a.cmp(idx_b))
        })
        .map(|(_, a)| a);
    (container_node_ids, latest_findings)
}

/// Return whether prior `architectural_drift` findings require discarding cached builder context.
pub(crate) fn is_context_reset_required(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate: &GateDef,
) -> bool {
    let (_, latest_findings) = latest_container_findings(snapshot, node);
    let last_passed_at = snapshot
        .claims
        .iter()
        .filter(|c| c.node_id == node.node_id && c.outcome == ClaimOutcome::Passed)
        .filter_map(|c| c.ended_at.as_deref())
        .max();
    let Some(art) = latest_findings else {
        return false;
    };
    if let Some(passed_ts) = last_passed_at {
        if art.created_at.as_str() < passed_ts {
            return false;
        }
        if art.created_at.as_str() == passed_ts
            && node.status == NodeStatus::Completed
            && node.accepted_tree.is_some()
        {
            return false;
        }
    }
    let Some(stored) = crate::machine::submit::parse_findings_artifact(&art.payload_json) else {
        return false;
    };
    if !stored.reset_worker_context {
        return false;
    }
    match stored.reset_gate_id.as_deref() {
        Some(target_gate_id) => target_gate_id == node.gate_id.as_str(),
        None => node.parent_id.is_some() && gate.role.as_deref() == Some("builder"),
    }
}

pub(super) fn resolve_rework_diagnostics(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    primary: &Path,
    max_bytes: usize,
) -> (Option<RejectionFindings>, Option<String>) {
    let (container_node_ids, latest_findings) = latest_container_findings(snapshot, node);

    let unresolved_findings = latest_findings.and_then(|art| {
        let art_node_resolved = snapshot.claims.iter().any(|c| {
            c.node_id == art.node_id
                && c.outcome == ClaimOutcome::Passed
                && c.claim_revision >= art.claim_revision
        });
        if art_node_resolved {
            return None;
        }
        let show_findings = if art.node_id == node.node_id {
            node.status == NodeStatus::Escalated
        } else {
            node.status != NodeStatus::Completed
        };
        if !show_findings {
            return None;
        }
        crate::machine::submit::parse_findings_artifact(&art.payload_json)
            .map(|stored| stored.findings)
    });

    let failed_trace_log_tail = snapshot
        .traces
        .iter()
        .filter(|t| container_node_ids.contains(t.node_id.as_str()))
        .max_by(|a, b| {
            a.trace_row_id
                .unwrap_or(0)
                .cmp(&b.trace_row_id.unwrap_or(0))
                .then_with(|| a.started_at.cmp(&b.started_at))
        })
        .filter(|t| !matches!(t.status, TraceStatus::Running | TraceStatus::Passed))
        .and_then(|trace| {
            let rel = Path::new(ACDD_DIR)
                .join("run")
                .join(snapshot.run.run_id.as_str())
                .join(&trace.log_relpath);
            let mut file = open_confined(primary, &rel, fs::OpenOptions::new().read(true)).ok()?;
            let mut content = String::new();
            file.read_to_string(&mut content).ok()?;
            Some(utf8_tail(&content, max_bytes).to_owned())
        });

    (unresolved_findings, failed_trace_log_tail)
}

pub(super) fn apply_tier2_findings(
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
    let candidates = [
        Some(json!({ "findings": req.unresolved_findings, "failed_trace_log_tail": tail })),
        (req.unresolved_findings.is_some() && tail.is_some())
            .then(|| json!({ "findings": req.unresolved_findings })),
    ];
    for payload in candidates.into_iter().flatten() {
        let delta = 23 + estimate_json_bytes(&payload);
        if *running_bytes + delta <= req.response_max_bytes {
            resp.unresolved_findings = Some(payload);
            *running_bytes += delta;
            return;
        }
    }
    add_omitted_section(
        resp,
        running_bytes,
        "unresolved_findings",
        req.response_max_bytes,
    );
}

pub(super) fn apply_tier3_pre_task_slice(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) {
    if let Some(slice) = &req.pre_task_slice {
        // `,"pre_task_slice":` = 18 bytes
        let slice_delta = 18 + estimate_json_bytes(slice);
        if *running_bytes + slice_delta <= req.response_max_bytes {
            resp.pre_task_slice = Some(slice.clone());
            *running_bytes += slice_delta;
        } else {
            add_omitted_section(
                resp,
                running_bytes,
                "pre_task_slice",
                req.response_max_bytes,
            );
        }
    }
}

pub(super) fn apply_tier4_receipts(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) -> Result<(), StateError> {
    if req.dependency_receipts.is_empty() {
        return Ok(());
    }
    let mut omitted = false;
    for receipt in &req.dependency_receipts {
        let mut obj = Map::new();
        obj.insert("node_cursor".to_owned(), json!(receipt.node_cursor));
        obj.insert("candidate_tree".to_owned(), json!(receipt.candidate_tree));
        if let Some(oid) = &receipt.committed_oid {
            obj.insert("committed_oid".to_owned(), json!(oid));
        }
        obj.insert("accepted_at".to_owned(), json!(receipt.accepted_at));
        let val = Value::Object(obj);
        // `,"dependency_receipts":[]` = 25 bytes on first element, `,` = 1 byte thereafter.
        let overhead = if resp.dependency_receipts.is_empty() {
            25
        } else {
            1
        };
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
        add_omitted_section(
            resp,
            running_bytes,
            "dependency_receipts",
            req.response_max_bytes,
        );
    }
    Ok(())
}

fn is_blackboard_relevant_to_node(item: &BlackboardRecord, node: &GateNodeRecord) -> bool {
    if node.kind != NodeKind::TaskGate {
        return true;
    }
    let Some(source_node) = item.node_id.as_deref() else {
        return true;
    };
    if source_node.starts_with("root:") {
        return true;
    }
    if source_node == node.node_id || node.parent_id.as_deref() == Some(source_node) {
        return true;
    }
    let matches_task = |candidate: &str| -> bool {
        node.task_ref.as_ref().is_some_and(|tref| {
            let prefix = format!("task:{tref}");
            candidate == prefix
                || candidate
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with(':'))
        })
    };
    matches_task(source_node)
        || item.recipient_node_id.as_deref().is_some_and(|rec| {
            rec == node.node_id || node.parent_id.as_deref() == Some(rec) || matches_task(rec)
        })
}

fn blackboard_dedup_key(item: &BlackboardRecord) -> (String, String) {
    let trimmed = item.payload.trim();
    if item.topic == BlackboardTopic::Decisions && trimmed.starts_with("BUDGET_OVERRIDE:") {
        let scope_key = match item.node_id.as_deref() {
            Some(nid) if nid.starts_with("task:") => {
                let rest = &nid["task:".len()..];
                let task_part = rest.split(':').next().unwrap_or(rest);
                format!("task:{task_part}")
            }
            Some(nid) => nid.to_owned(),
            None => String::new(),
        };
        return ("decisions:budget_override".to_owned(), scope_key);
    }
    (item.topic.to_string(), trimmed.to_owned())
}

pub(super) fn apply_tier5_blackboard(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    req: &ClaimContextRequest,
) {
    let mut priority: Vec<&BlackboardRecord> = Vec::new();
    let mut notes: Vec<&BlackboardRecord> = Vec::new();
    for item in &req.blackboard_items {
        if item.status != BlackboardStatus::Open
            || matches!(
                item.topic,
                BlackboardTopic::Deferred | BlackboardTopic::Findings
            )
            || !is_blackboard_relevant_to_node(item, &req.node)
        {
            continue;
        }
        if matches!(
            item.topic,
            BlackboardTopic::Decisions | BlackboardTopic::Blockers
        ) {
            priority.push(item);
        } else {
            notes.push(item);
        }
    }
    if priority.is_empty() && notes.is_empty() {
        return;
    }

    let mut deduped: Vec<&BlackboardRecord> = Vec::new();
    let mut seen_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for item in priority.iter().chain(notes.iter()).rev() {
        if seen_keys.insert(blackboard_dedup_key(item)) {
            deduped.push(item);
        }
    }
    deduped.reverse();

    let mut priority_dedup: Vec<&BlackboardRecord> = Vec::new();
    let mut notes_dedup: Vec<&BlackboardRecord> = Vec::new();
    for item in deduped {
        if matches!(
            item.topic,
            BlackboardTopic::Decisions | BlackboardTopic::Blockers
        ) {
            priority_dedup.push(item);
        } else {
            notes_dedup.push(item);
        }
    }

    let selected: Vec<&BlackboardRecord> = if priority_dedup.len() >= MAX_BLACKBOARD_ITEMS {
        let start = priority_dedup.len().saturating_sub(MAX_BLACKBOARD_ITEMS);
        priority_dedup[start..].to_vec()
    } else {
        let mut combined = priority_dedup;
        let remaining_slots = MAX_BLACKBOARD_ITEMS - combined.len();
        let notes_start = notes_dedup.len().saturating_sub(remaining_slots);
        combined.extend(&notes_dedup[notes_start..]);
        combined
    };
    let mut omitted = false;
    for item in selected {
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
        add_omitted_section(resp, running_bytes, "blackboard", req.response_max_bytes);
    }
}

fn admitted_scope_for_request(req: &ClaimContextRequest) -> Vec<String> {
    let mut combined = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let task = (req.node.scope.is_empty() && req.node.extended_scope.is_empty())
        .then(|| req.node.task_ref.as_ref().zip(req.spec.as_ref()))
        .flatten()
        .and_then(|(tref, spec)| spec.tasks.iter().find(|t| &t.task_ref == tref));
    let (scope, ext) = task.map_or((&req.node.scope, &req.node.extended_scope), |t| {
        (&t.scope, &t.extended_scope)
    });
    for path in scope.iter().chain(ext.iter().map(|ext| &ext.path)) {
        if seen.insert(path.as_str()) {
            combined.push(path.clone());
        }
    }
    combined
}

const CONTEXT_LENSES_OVERHEAD: usize = ",\"context_lenses\":{}".len();

fn is_empty_items_lens(val: &Value) -> bool {
    val.as_object().is_some_and(|obj| {
        obj.get("items")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
            && obj.get("total").and_then(Value::as_u64).unwrap_or(0) == 0
            && obj.get("truncated").and_then(Value::as_bool) != Some(true)
    })
}

fn is_exhausted_items_lens(val: &Value) -> bool {
    val.as_object().is_some_and(|obj| {
        obj.get("items")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
            && (obj.get("truncated").and_then(Value::as_bool) == Some(true)
                || obj.get("total").and_then(Value::as_u64).unwrap_or(0) > 0)
    })
}

fn pack_oversized_items_lens(val: Value, max_bytes: usize) -> Option<Value> {
    let Value::Object(mut obj) = val else {
        return None;
    };
    let Value::Array(items) = obj.remove("items")? else {
        return None;
    };
    let total = items.len();
    if total == 0 {
        return None;
    }
    obj.entry("total").or_insert_with(|| json!(total));
    obj.insert("items".to_owned(), json!([]));
    obj.insert("truncated".to_owned(), json!(true));
    let base_bytes = estimate_json_bytes(&Value::Object(obj.clone()));
    let capacity = max_bytes.checked_sub(base_bytes).filter(|&r| r > 0)?.saturating_add(1);
    let candidates: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| KnapsackItem {
            weight: estimate_json_bytes(item).saturating_add(1),
            score: u64::try_from(total - idx).unwrap_or(u64::MAX),
        })
        .collect();
    let mut selected = pack_knapsack_indices(&candidates, capacity).into_iter().peekable();
    let kept: Vec<_> = items
        .into_iter()
        .enumerate()
        .filter_map(|(idx, item)| (selected.peek() == Some(&idx)).then(|| { selected.next(); item }))
        .collect();
    if kept.is_empty() {
        return None;
    }
    obj.insert("items".to_owned(), Value::Array(kept));
    let packed = Value::Object(obj);
    (estimate_json_bytes(&packed) <= max_bytes).then_some(packed)
}

fn omit_context_lens(
    resp: &mut ClaimResp,
    running_bytes: &mut usize,
    qualified_lens: &str,
    max_bytes: usize,
) {
    if let Some(old_val) = resp.context_lenses.shift_remove(qualified_lens) {
        let key_bytes = estimate_json_bytes(qualified_lens) + 1;
        let sep = if resp.context_lenses.is_empty() { CONTEXT_LENSES_OVERHEAD } else { 1 };
        *running_bytes -= estimate_json_bytes(&old_val) + key_bytes + sep;
    }
    add_omitted_section(resp, running_bytes, &format!("context:{qualified_lens}"), max_bytes);
}

pub(super) fn apply_tier6_context_lenses(
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

    for (idx, qualified_lens) in req.gate.context.iter().enumerate() {
        let key_bytes = estimate_json_bytes(qualified_lens) + 1;
        let effective_base = resp.context_lenses.get(qualified_lens).map_or_else(
            || {
                let sep = if resp.context_lenses.is_empty() { CONTEXT_LENSES_OVERHEAD } else { 1 };
                *running_bytes + key_bytes + sep
            },
            |old_val| *running_bytes - estimate_json_bytes(old_val),
        );
        let remaining = req.response_max_bytes.saturating_sub(effective_base);
        if remaining == 0 {
            omit_context_lens(resp, running_bytes, qualified_lens, req.response_max_bytes);
            continue;
        }

        let remaining_lenses = req.gate.context.len().saturating_sub(idx).max(1);
        let lens_budget =
            (remaining / remaining_lenses).max(remaining.min(MIN_LENS_BUDGET_BYTES / 8));
        let baseline_ref = if req.node.kind == NodeKind::RootGate
            && !is_pre_task_root_in_profile(&req.profile, &req.node)
        {
            req.run
                .target_base_commit
                .as_ref()
                .map(|c| c.to_string())
                .or_else(|| req.run.target_base.clone())
                .or_else(|| baseline_ref_for_node(&req.node))
        } else {
            baseline_ref_for_node(&req.node)
        };
        let lens_scope = if matches!(qualified_lens.as_str(), "code.symbols" | "code.tests")
            && !req.node.scope.is_empty()
        {
            req.node.scope.clone()
        } else {
            admitted_scope.clone()
        };
        let ctx_req = DomainContextReq {
            run_id: req.run.run_id.clone(),
            node_cursor: node_cursor.clone(),
            gate_id: gate_id.clone(),
            workspace_root: PathBuf::from(&req.run.workspace),
            worktree: req.worktree.clone(),
            admitted_scope: lens_scope,
            target: target.clone(),
            max_bytes: lens_budget,
            path_policy: req.path_policy.clone(),
            baseline_ref,
        };

        let val = provider
            .provide_context(qualified_lens, &ctx_req)
            .map_err(|err| {
                if let Some(state_err) = StateError::from_anyhow(&err) {
                    state_err.clone()
                } else {
                    StateError::new(
                        ErrorCode::DomainCapabilityUnavailable,
                        format!("context lens '{qualified_lens}' failed: {err}"),
                    )
                }
            })?;

        if is_empty_items_lens(&val) && !resp.context_lenses.contains_key(qualified_lens) {
            continue;
        }
        if is_exhausted_items_lens(&val) {
            omit_context_lens(resp, running_bytes, qualified_lens, req.response_max_bytes);
            continue;
        }

        let new_val_bytes = estimate_json_bytes(&val);
        let accepted = if new_val_bytes <= remaining {
            Some((val, new_val_bytes))
        } else {
            pack_oversized_items_lens(val, remaining).map(|packed| {
                let bytes = estimate_json_bytes(&packed);
                (packed, bytes)
            })
        };
        if let Some((accepted_val, accepted_bytes)) = accepted {
            resp.context_lenses.insert(qualified_lens.clone(), accepted_val);
            *running_bytes = effective_base + accepted_bytes;
        } else {
            omit_context_lens(resp, running_bytes, qualified_lens, req.response_max_bytes);
        }
    }

    Ok(())
}
