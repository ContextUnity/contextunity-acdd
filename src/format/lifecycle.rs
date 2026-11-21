//! Pure Markdown presentation for task lifecycle responses.

use acdd_state::context::{render_claim_prompt_markdown, ClaimResp};
use acdd_state::types::{GateNodeRecord, RunRecord};
use acdd_state::usecases::{
    ListResp, ShowResp, StartResp, StateCommandOutput, StateQueryOutput, StatusResp, SubmitResp,
    VerifyResp,
};
use serde::Serialize;
use serde_json::Value;

/// Render a [`ClaimResp`] via canonical `acdd_state::context::render_claim_prompt_markdown`.
pub fn render_claim_markdown(claim: &ClaimResp) -> String {
    render_claim_prompt_markdown(claim, None)
}

/// Render a [`VerifyResp`] as readable Markdown (`ADR-0006`).
pub fn render_verify_markdown(verify: &VerifyResp) -> String {
    super::render_serializable(verify, &format!("Gate Verification: {}", verify.node_cursor))
}

/// Render a [`SubmitResp`] as readable Markdown (`ADR-0006`).
pub fn render_submit_markdown(submit: &SubmitResp) -> String {
    super::render_serializable(submit, &format!("Gate Submission: {}", submit.node_cursor))
}

/// Render a [`StateCommandOutput`] as readable Markdown (`ADR-0006`).
pub fn render_state_command_markdown(output: &StateCommandOutput, title: &str) -> String {
    match output {
        StateCommandOutput::Claim(claim) => render_claim_markdown(claim),
        StateCommandOutput::Start(start) => render_start_markdown(start),
        StateCommandOutput::Verify(verify) => render_verify_markdown(verify),
        StateCommandOutput::Submit(submit) => render_submit_markdown(submit),
        StateCommandOutput::BlackboardPost(resp) => {
            super::coordination::render_blackboard_markdown(&serde_json::json!({ "message": resp.entry }), title)
        }
        StateCommandOutput::CallPluginTool(resp) => {
            super::coordination::render_plugin_tool_markdown(&resp.tool_name, &resp.output)
        }
        other => render_tagged_enum_markdown(other, title),
    }
}

/// Render a [`StateQueryOutput`] as readable Markdown (`ADR-0006`).
///
/// For [`StateQueryOutput::Roadmap`], `roadmap_detail` must be `spec_id.is_some()` on the
/// originating request (ignored for other variants).
pub fn render_state_query_markdown(
    output: &StateQueryOutput,
    title: &str,
    roadmap_detail: bool,
) -> String {
    match output {
        StateQueryOutput::Status(status) => render_status_markdown(status),
        StateQueryOutput::List(list) => render_list_markdown(list),
        StateQueryOutput::Show(show) => render_show_markdown(show),
        StateQueryOutput::BlackboardRead(resp) => {
            let val = serde_json::json!({
                "run_id": resp.run_id,
                "messages": resp.items,
                "pagination": {
                    "total": resp.total, "generation": resp.generation,
                    "limit": resp.limit, "offset": resp.offset,
                }
            });
            super::coordination::render_blackboard_markdown(&val, title)
        }
        StateQueryOutput::Observability(resp) => match resp.prometheus_text {
            Some(ref prom) => format!("# Observability\n\n```text\n{prom}\n```\n"),
            None => super::render_serializable(&resp.snapshot, "Observability"),
        },
        StateQueryOutput::Context(claim) => render_claim_markdown(claim),
        StateQueryOutput::Roadmap(resp) => super::roadmap::render_roadmap_markdown(resp, roadmap_detail),
        other => render_tagged_enum_markdown(other, title),
    }
}

fn push_run_summary(out: &mut String, run: &RunRecord) {
    out.push_str(&format!("- **Run**: `{}` (`{}`)\n", run.run_id, run.run_name));
    out.push_str(&format!(
        "- **Status**: `{:?}` | **Profile**: `{}` | **Isolation**: `{:?}` | **Generation**: `{}`\n",
        run.status, run.profile_id, run.isolation, run.generation
    ));
    if let Some(ref sid) = run.spec_id {
        out.push_str(&format!("- **Spec**: `{sid}`"));
        if let Some(ref sp) = run.spec_path {
            out.push_str(&format!(" (`{sp}`)"));
        }
        out.push('\n');
    } else if let Some(ref sp) = run.spec_path {
        out.push_str(&format!("- **Spec Path**: `{sp}`\n"));
    }
    if let Some(ref branch) = run.branch {
        out.push_str(&format!("- **Branch**: `{branch}`\n"));
    }
    if let Some(ref wt) = run.worktree {
        out.push_str(&format!("- **Worktree**: `{wt}`\n"));
    }
    if let Some(ref tb) = run.target_base {
        out.push_str(&format!("- **Target Base**: `{tb}`"));
        if let Some(ref oid) = run.target_base_commit {
            out.push_str(&format!(" @ `{oid}`"));
        }
        out.push('\n');
    }
    if let Some(ref reason) = run.cancellation_reason {
        out.push_str(&format!("- **Cancellation Reason**: {reason}\n"));
    }
}

fn push_backtick_list(out: &mut String, label: &str, items: &[String]) {
    if !items.is_empty() {
        let joined = items.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("- **{label}**: {joined}\n"));
    }
}

fn render_start_markdown(resp: &StartResp) -> String {
    let mut out = String::from("# Run Started\n\n");
    push_run_summary(&mut out, &resp.run);
    if let Some(ref spec_path) = resp.materialized_spec_path {
        out.push_str(&format!("- **Materialized Spec**: `{spec_path}`\n"));
    }
    push_backtick_list(&mut out, "Ready", &resp.ready_nodes);
    out
}

fn render_status_markdown(resp: &StatusResp) -> String {
    let Some(ref run) = resp.run else {
        return "# Run Status\n\n_No active run._\n".to_owned();
    };
    let mut out = String::from("# Run Status\n\n");
    push_run_summary(&mut out, run);
    if let Some(ref spec_status) = resp.spec_status {
        out.push_str(&format!("- **Spec Status**: `{spec_status:?}`\n"));
    }
    push_backtick_list(&mut out, "Ready Nodes", &resp.ready_nodes);
    if !resp.nodes.is_empty() {
        out.push_str("\n## Nodes\n\n| Node | Kind | Status | Worker | Attempt | Rev | Claim/Accepted |\n|---|---|---|---|---:|---:|---|\n");
        for node in &resp.nodes {
            let worker = node.worker_id.as_ref().map(|w| w.as_str()).unwrap_or("-");
            let tree = node
                .claim_tree
                .as_ref()
                .or(node.accepted_tree.as_ref())
                .map(|t| format!("`{}`", super::shorten_hash(t.as_str())))
                .unwrap_or_else(|| "-".to_owned());
            let row = format!(
                "| `{}` | `{:?}` | `{:?}` | `{}` | {}/{} | {} | {} |\n",
                node.node_id, node.kind, node.status, worker, node.attempt, node.max_attempts, node.claim_revision, tree
            );
            out.push_str(&row);
        }
    }
    out
}

fn render_list_markdown(resp: &ListResp) -> String {
    let page = &resp.page;
    let mut out = format!(
        "# Runs\n\n_total_runs_: **{}** | _total_tombstones_: **{}** | _limit_: **{}** | _offset_: **{}**\n",
        page.total_runs, page.total_tombstones, page.limit, page.offset
    );
    if !page.runs.is_empty() {
        out.push_str("\n## Active / Persisted Runs\n\n| Run ID | Name | Status | Spec | Profile | Isolation | Gen |\n|---|---|---|---|---|---|---:|\n");
        for run in &page.runs {
            let spec = run.spec_id.as_deref().or(run.spec_path.as_deref()).unwrap_or("-");
            out.push_str(&format!(
                "| `{}` | `{}` | `{:?}` | `{}` | `{}` | `{:?}` | {} |\n",
                run.run_id, run.run_name, run.status, spec, run.profile_id, run.isolation, run.generation
            ));
        }
    }
    if !page.tombstones.is_empty() {
        out.push_str("\n## Tombstones\n\n| Run ID | Name | Disposition | Spec / Archive | Closed |\n|---|---|---|---|---|\n");
        for tomb in &page.tombstones {
            let spec = tomb.spec_id.as_deref().or(tomb.archive_path.as_deref()).unwrap_or("-");
            out.push_str(&format!(
                "| `{}` | `{}` | `{:?}` | `{}` | {} |\n",
                tomb.run_id, tomb.run_name, tomb.terminal_status, spec, tomb.closed_at
            ));
        }
    }
    out
}

fn push_node_details(out: &mut String, node: &GateNodeRecord) {
    out.push_str("\n## Node\n\n");
    out.push_str(&format!(
        "- **Node ID**: `{}` (`{:?}`, `{:?}`)\n",
        node.node_id, node.kind, node.status
    ));
    if let Some(ref tref) = node.task_ref {
        out.push_str(&format!("- **Task**: `{tref}`\n"));
    }
    out.push_str(&format!(
        "- **Attempt**: {}/{} | **Claim Rev**: {} | **Contract Rev**: {} | **Version**: {}\n",
        node.attempt, node.max_attempts, node.claim_revision, node.contract_revision, node.node_version
    ));
    if let Some(ref worker) = node.worker_id {
        out.push_str(&format!("- **Worker**: `{worker}`\n"));
    }
    if let Some(ref branch) = node.branch {
        out.push_str(&format!("- **Branch**: `{branch}`\n"));
    }
    if let Some(ref wt) = node.worktree {
        out.push_str(&format!("- **Worktree**: `{wt}`\n"));
    }
    if let Some(ref oid) = node.baseline_commit {
        out.push_str(&format!("- **Baseline Commit**: `{oid}`\n"));
    }
    if let Some(ref tree) = node.claim_tree {
        out.push_str(&format!("- **Claim Tree**: `{tree}`\n"));
    }
    if let Some(ref tree) = node.accepted_tree {
        out.push_str(&format!("- **Accepted Tree**: `{tree}`\n"));
    }
    if !node.scope.is_empty() {
        let scope = node.scope.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("- **Scope**: {scope}\n"));
    }
    if !node.extended_scope.is_empty() {
        out.push_str("- **Extended Scope**:\n");
        for ext in &node.extended_scope {
            out.push_str(&format!("  - `{}` — {}\n", ext.path, ext.reason));
        }
    }
}

fn render_show_markdown(resp: &ShowResp) -> String {
    let mut out = String::from("# Run Detail\n\n");
    push_run_summary(&mut out, &resp.run);
    if let Some(ref node) = resp.node {
        push_node_details(&mut out, node);
    }
    if !resp.subtasks.is_empty() {
        out.push_str("\n## Subtasks\n\n");
        for sub in &resp.subtasks {
            out.push_str(&format!("- [{:?}] `{}`: {}", sub.status, sub.subtask_ref, sub.title));
            if let Some(ref ev) = sub.evidence {
                out.push_str(&format!(" — {ev}"));
            }
            out.push('\n');
        }
    }
    if let Some(ref preview_val) = resp.context_preview {
        if let Ok(claim) = serde_json::from_value::<ClaimResp>(preview_val.clone()) {
            out.push('\n');
            out.push_str(&render_claim_markdown(&claim));
        } else {
            out.push('\n');
            out.push_str(&super::render_markdown(preview_val, "Context Preview"));
        }
    }
    out
}

fn render_tagged_enum_markdown<T: Serialize>(value: &T, title: &str) -> String {
    let val = serde_json::to_value(value).unwrap_or(Value::Null);
    let inner = val.as_object().and_then(|map| map.values().next()).unwrap_or(&val);
    super::render_markdown(inner, title)
}
