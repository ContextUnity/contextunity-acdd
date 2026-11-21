//! Canonical Markdown prompt rendering for `ClaimResp` across CLI, MCP, and role runners.

use super::ClaimResp;
use serde_json::{Map, Value};

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
    let mut top_bullets = Vec::new();
    let mut section_blocks = Vec::new();
    for item in guidance {
        let text = item.as_str().map(str::to_owned).unwrap_or_else(|| item.to_string());
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("### ") {
            section_blocks.push(trimmed.to_owned());
        } else {
            top_bullets.push(trimmed.to_owned());
        }
    }

    let mut out = String::new();
    if !top_bullets.is_empty() {
        for bullet in top_bullets {
            out.push_str("- ");
            out.push_str(&bullet);
            out.push('\n');
        }
        if !section_blocks.is_empty() {
            out.push('\n');
        }
    }
    for (idx, block) in section_blocks.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(block);
        out.push('\n');
    }
    out
}

fn push_str_list(out: &mut String, label: &str, items: &[Value]) {
    if items.is_empty() {
        return;
    }
    out.push_str("- **");
    out.push_str(label);
    out.push_str("**:\n");
    for item in items {
        let s = item.as_str().map(str::to_owned).unwrap_or_else(|| item.to_string());
        out.push_str("  - ");
        out.push_str(&s);
        out.push('\n');
    }
}

fn format_cursor_value(val: &Value) -> String {
    if let Some(s) = val.as_str() {
        return s.to_owned();
    }
    if let Some(root) = val.get("Root").and_then(Value::as_array) {
        let joined: Vec<&str> = root.iter().filter_map(Value::as_str).collect();
        if !joined.is_empty() {
            return joined.join(".");
        }
    }
    if let Some(task_obj) = val.get("Task").and_then(Value::as_object) {
        if let Some(task) = task_obj.get("task").and_then(Value::as_str) {
            let gates: Vec<&str> = task_obj
                .get("gates")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            return if gates.is_empty() {
                task.to_owned()
            } else {
                format!("{task}.{}", gates.join("."))
            };
        }
    }
    val.to_string()
}

fn render_contract_markdown(out: &mut String, contract: &Value) {
    let Some(obj) = contract.as_object() else {
        out.push_str(&format!("{contract}\n"));
        return;
    };
    for (key, label) in [
        ("gate_id", "Gate"),
        ("proof", "Proof"),
        ("command", "Command"),
        ("call", "Call"),
        ("spec_id", "Spec"),
        ("title", "Title"),
        ("spec_path", "Spec Path"),
        ("task_ref", "Task"),
        ("target", "Target"),
    ] {
        if let Some(val) = obj.get(key) {
            let s = val.as_str().map(str::to_owned).unwrap_or_else(|| val.to_string());
            if !s.is_empty() {
                out.push_str(&format!("- **{label}**: {s}\n"));
            }
        }
    }
    for (key, label) in [
        ("required_skills", "Required Skills"),
        ("tools", "Tools"),
        ("scope", "Scope"),
        ("owners", "Owners"),
    ] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array).filter(|a| !a.is_empty()) {
            let joined = arr
                .iter()
                .map(|v| v.as_str().map(|s| format!("`{s}`")).unwrap_or_else(|| v.to_string()))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("- **{label}**: {joined}\n"));
        }
    }
    if let Some(arr) = obj.get("checks").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let joined = arr
            .iter()
            .filter_map(|v| v.get("check").and_then(Value::as_str))
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            out.push_str(&format!("- **Checks**: {joined}\n"));
        }
    }
    if let Some(arr) = obj.get("extended_scope").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        out.push_str("- **Extended Scope**:\n");
        for entry in arr {
            let path = entry.get("path").and_then(Value::as_str).unwrap_or_default();
            let reason = entry.get("reason").and_then(Value::as_str).unwrap_or_default();
            if reason.is_empty() {
                out.push_str(&format!("  - `{path}`\n"));
            } else {
                out.push_str(&format!("  - `{path}` — {reason}\n"));
            }
        }
    }
    if let Some(inv) = obj.get("invariants").and_then(Value::as_object) {
        if let Some(app) = inv.get("applicable").and_then(Value::as_array) {
            push_str_list(out, "Applicable Invariants", app);
        }
        if let Some(ver) = inv.get("verified").and_then(Value::as_array) {
            push_str_list(out, "Verified Invariants", ver);
        }
    }
    for (key, label) in [("spec", "Spec Details"), ("task_spec", "Task Spec")] {
        if let Some(s) = obj.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            out.push_str(&format!("\n### {label}\n\n{}\n", s.trim()));
        }
    }
    if let Some(arr) = obj.get("acceptance_criteria").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        out.push('\n');
        push_str_list(out, "Acceptance Criteria", arr);
    }
    if let Some(arr) = obj.get("subtasks").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        out.push_str("\n### Subtasks\n\n");
        for sub in arr {
            let sref = sub.get("subtask_ref").and_then(Value::as_str).unwrap_or_default();
            let title = sub.get("title").and_then(Value::as_str).unwrap_or_default();
            let status = sub.get("status").and_then(Value::as_str).unwrap_or("pending");
            out.push_str(&format!("- [{status}] `{sref}`: {title}"));
            if let Some(ev) = sub.get("evidence").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                out.push_str(&format!(" — {ev}"));
            }
            out.push('\n');
        }
    }
    if let Some(contours) = obj.get("contours").and_then(Value::as_object).filter(|c| !c.is_empty()) {
        out.push_str("\n### Contours\n");
        for (name, def) in contours {
            let desc = def.get("description").and_then(Value::as_str).unwrap_or_default();
            out.push_str(&format!("\n#### `{name}`\n"));
            if !desc.is_empty() {
                out.push_str(&format!("{desc}\n"));
            }
            if let Some(criteria) = def.get("criteria").and_then(Value::as_array) {
                for c in criteria {
                    if let Some(cs) = c.as_str() {
                        out.push_str(&format!("- {cs}\n"));
                    }
                }
            }
        }
    }
}

fn render_findings_markdown(out: &mut String, payload: &Value) {
    if let Some(findings) = payload.get("findings").filter(|v| !v.is_null()) {
        let kind = findings.get("kind").and_then(Value::as_str).unwrap_or("defect");
        let summary = findings.get("summary").and_then(Value::as_str).unwrap_or_default();
        out.push_str(&format!("- **Kind**: `{kind}`\n"));
        if !summary.is_empty() {
            out.push_str(&format!("- **Summary**: {summary}\n"));
        }
        if let Some(bb_id) = findings.get("blackboard_message_id").and_then(Value::as_i64) {
            out.push_str(&format!("- **Blackboard**: #{bb_id}\n"));
        }
        if let Some(items) = findings.get("items").and_then(Value::as_array) {
            for item in items {
                let sev = item.get("severity").and_then(Value::as_str).unwrap_or("medium");
                let path = item.get("path").and_then(Value::as_str).unwrap_or("-");
                let msg = item.get("message").and_then(Value::as_str).unwrap_or_default();
                let fix = item.get("remediation").and_then(Value::as_str).unwrap_or_default();
                out.push_str(&format!("- [{sev}] `{path}`: {msg}"));
                if !fix.is_empty() {
                    out.push_str(&format!(" | Remediation: {fix}"));
                }
                out.push('\n');
            }
        }
    }
    if let Some(tail) = payload.get("failed_trace_log_tail").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        out.push_str("\n### Failed Trace Log Tail\n\n```text\n");
        out.push_str(tail);
        if !tail.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }
}

fn push_labeled_scalars(out: &mut String, obj: &Map<String, Value>, fields: &[(&str, &str)]) {
    for (k, label) in fields {
        if let Some(v) = obj.get(*k).filter(|v| !v.is_null()) {
            let s = v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string());
            if !s.is_empty() {
                out.push_str(&format!("- **{label}**: {s}\n"));
            }
        }
    }
}

fn push_labeled_lists(out: &mut String, obj: &Map<String, Value>, fields: &[(&str, &str)]) {
    for (k, label) in fields {
        if let Some(arr) = obj.get(*k).and_then(Value::as_array) {
            push_str_list(out, label, arr);
        }
    }
}

fn render_slice_markdown(out: &mut String, slice: &Value) {
    let Some(obj) = slice.as_object() else {
        out.push_str(&format!("{slice}\n"));
        return;
    };
    push_labeled_scalars(
        out,
        obj,
        &[
            ("layer", "Layer"),
            ("summary", "Summary"),
            ("max_net_loc", "Max Net LOC"),
            ("verification_seam", "Verification Seam"),
        ],
    );
    push_labeled_lists(
        out,
        obj,
        &[
            ("canonical_owners", "Canonical Owners"),
            ("exported_interfaces", "Exported Interfaces"),
            ("mechanism_rules", "Mechanism Rules"),
            ("negative_boundaries", "Negative Boundaries"),
            ("cited_invariants", "Cited Invariants"),
            ("global_negative_boundaries", "Global Negative Boundaries"),
        ],
    );
    for (k, label, peer_key) in [
        ("incoming_seams", "Incoming Seams", "source_task"),
        ("outgoing_seams", "Outgoing Seams", "target_task"),
    ] {
        if let Some(arr) = obj.get(k).and_then(Value::as_array).filter(|a| !a.is_empty()) {
            out.push_str(&format!("- **{label}**:\n"));
            for s in arr {
                let peer = s.get(peer_key).and_then(Value::as_str).unwrap_or("-");
                let ctype = s.get("contract_type").and_then(Value::as_str).unwrap_or("-");
                let cref = s.get("contract_ref").and_then(Value::as_str).unwrap_or("-");
                let spec = s.get("specification").and_then(Value::as_str).unwrap_or_default();
                out.push_str(&format!("  - `{peer}` ({ctype}) `{cref}`: {spec}\n"));
            }
        }
    }
    if let Some(preds) = obj.get("predecessor_proofs").and_then(Value::as_object) {
        render_predecessor_proofs_markdown(out, preds);
    }
}

fn render_predecessor_proofs_markdown(out: &mut String, preds: &Map<String, Value>) {
    for (gate_name, proof_val) in preds {
        out.push_str(&format!("\n### Predecessor Proof: `{gate_name}`\n\n"));
        let Some(pobj) = proof_val.as_object() else {
            out.push_str(&format!("- {proof_val}\n"));
            continue;
        };
        push_labeled_scalars(
            out,
            pobj,
            &[
                ("blackboard_decision_id", "Blackboard Decision ID"),
                ("c2_boundary_impact", "C2 Boundary Impact"),
                ("adr_required", "ADR Required"),
            ],
        );
        push_labeled_lists(
            out,
            pobj,
            &[
                ("cited_invariants", "Cited Invariants"),
                ("cited_symbols", "Cited Symbols"),
                ("global_negative_boundaries", "Global Negative Boundaries"),
                ("ac_mapping", "AC Mapping"),
                ("sad_updates", "SAD Updates"),
            ],
        );
        if let Some(tasks) = pobj.get("tasks").and_then(Value::as_object) {
            for (tref, task_slice) in tasks {
                out.push_str(&format!("\n#### Task Slice: `{tref}`\n\n"));
                render_slice_markdown(out, task_slice);
            }
        }
        for (k, v) in pobj {
            if matches!(
                k.as_str(),
                "blackboard_decision_id"
                    | "c2_boundary_impact"
                    | "adr_required"
                    | "cited_invariants"
                    | "cited_symbols"
                    | "global_negative_boundaries"
                    | "ac_mapping"
                    | "sad_updates"
                    | "tasks"
            ) || v.is_null()
            {
                continue;
            }
            match v.as_array() {
                Some(arr) => push_str_list(out, k, arr),
                None => push_labeled_scalars(out, pobj, &[(k.as_str(), k.as_str())]),
            }
        }
    }
}

fn render_context_lenses_markdown(out: &mut String, lenses: &Map<String, Value>) {
    for (key, val) in lenses {
        match key.as_str() {
            "docs.instructions" => {
                out.push_str("\n## Repository Instructions - MANDATORY\n\n");
                let directive = val
                    .get("directive")
                    .and_then(Value::as_str)
                    .unwrap_or("MANDATORY: read repository rules for respective scope.");
                out.push_str(directive);
                out.push_str("\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                        let headings = item
                            .get("headings")
                            .and_then(Value::as_array)
                            .map(|h| {
                                h.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        if headings.is_empty() {
                            out.push_str(&format!("- `{path}`\n"));
                        } else {
                            out.push_str(&format!("- `{path}` — headings: {headings}\n"));
                        }
                    }
                }
            }
            "docs.adrs" => {
                out.push_str("\n## Architectural Decision Records - MANDATORY\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let title = item.get("title").and_then(Value::as_str).unwrap_or_default();
                        let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                        out.push_str(&format!("- {title}: `{path}`\n"));
                    }
                }
            }
            "docs.invariants" => {
                out.push_str("\n## Governing Invariants\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                        let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                        let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
                        let loc = if line > 0 {
                            format!("{path}:{line}")
                        } else {
                            path.to_owned()
                        };
                        let rule = item.get("rule").and_then(Value::as_str).unwrap_or_default();
                        if rule.is_empty() {
                            out.push_str(&format!("- `{id}` (`{loc}`)\n"));
                        } else {
                            out.push_str(&format!("- `{id}` (`{loc}`): {rule}\n"));
                        }
                    }
                }
            }
            "code.symbols" => {
                out.push_str("\n## Scoped Symbols\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let qn = item.get("qualname").and_then(Value::as_str).unwrap_or_default();
                        let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
                        let end = item.get("end_line").and_then(Value::as_u64).unwrap_or(line);
                        let sig = item.get("signature").and_then(Value::as_str).unwrap_or_default();
                        out.push_str(&format!("- `{qn}` : {line}:{end} : `{sig}`\n"));
                    }
                }
            }
            "code.tests" => {
                out.push_str("\n## Scoped Tests\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let qn = item.get("qualname").and_then(Value::as_str).unwrap_or_default();
                        let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                        let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
                        out.push_str(&format!("- `{qn}` (`{path}:{line}`)\n"));
                    }
                }
            }
            "code.external_callers" => {
                out.push_str("\n## External Callers\n\n");
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    for item in items {
                        let sym = item.get("symbol").and_then(Value::as_str).unwrap_or_default();
                        let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                        let sig_chg = item.get("signature_changed").and_then(Value::as_bool).unwrap_or(false);
                        let callers = item
                            .get("external_callers")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default();
                        let flag = if sig_chg { " [SIG CHANGED]" } else { "" };
                        out.push_str(&format!("- `{sym}` (`{path}`{flag}) -> {callers}\n"));
                    }
                }
            }
            other => {
                out.push_str(&format!("\n### Context: `{other}`\n\n```json\n"));
                out.push_str(&serde_json::to_string_pretty(val).unwrap_or_default());
                out.push_str("\n```\n");
            }
        }
    }
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
         - Inspect: `{}`\n",
        claim.node_cursor,
        claim.run_id,
        claim.claim_revision,
        claim.contract_revision,
        claim.worker_id,
        claim.worktree,
        claim.candidate_tree,
        claim.inspect_cmd,
    );
    if claim.context_reset_required {
        out.push_str(
            "\n> [!WARNING]\n\
             > **CONTEXT RESET REQUIRED**: A prior `architectural_drift` rejection triggered `rearchitect`. \
             Discard cached builder assumptions, read the latest `rearchitect` decision on `acdd_blackboard`, and implement cleanly from scratch.\n",
        );
    }
    out.push_str("\n## Contract\n\n");
    render_contract_markdown(&mut out, &claim.contract);
    out.push_str("\n## Guidance\n\n");
    out.push_str(&render_guidance_lines(&claim.guidance));

    if let Some(findings) = &claim.unresolved_findings {
        out.push_str("\n## Unresolved Findings\n\n");
        render_findings_markdown(&mut out, findings);
    }
    if let Some(slice) = &claim.pre_task_slice {
        out.push_str("\n## Pre-Task Architectural Slice\n\n");
        render_slice_markdown(&mut out, slice);
    }
    if !claim.dependency_receipts.is_empty() {
        out.push_str("\n## Dependency Receipts\n\n");
        for r in &claim.dependency_receipts {
            let cursor = r.get("node_cursor").map(format_cursor_value).unwrap_or_default();
            let tree = r.get("candidate_tree").and_then(Value::as_str).unwrap_or_default();
            let oid = r
                .get("committed_oid")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && *s != "-");
            let ts = r.get("accepted_at").and_then(Value::as_str).unwrap_or_default();
            match oid {
                Some(oid) => out.push_str(&format!("- `{cursor}` @ `{tree}` (commit `{oid}`, {ts})\n")),
                None => out.push_str(&format!("- `{cursor}` @ `{tree}` ({ts})\n")),
            }
        }
    }
    if !claim.blackboard.is_empty() {
        out.push_str("\n## Blackboard\n\n");
        for item in &claim.blackboard {
            let id = item.get("id").and_then(Value::as_i64).map(|v| format!("#{v}")).unwrap_or_default();
            let topic = item.get("topic").and_then(Value::as_str).unwrap_or("notes");
            let author = item.get("author").and_then(Value::as_str).unwrap_or("-");
            let payload = item.get("payload").and_then(Value::as_str).unwrap_or_default();
            out.push_str(&format!("- {id} [{topic} | {author}]: {payload}\n"));
        }
    }
    if !claim.context_lenses.is_empty() {
        render_context_lenses_markdown(&mut out, &claim.context_lenses);
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
