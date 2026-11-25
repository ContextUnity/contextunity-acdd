//! Pure Markdown presentation for task lifecycle responses.

use acdd_state::context::{render_claim_prompt_markdown, ClaimResp};
use acdd_state::profile::{GateDispatchInfo, RoleExecutionMode};
use acdd_state::types::{GateNodeRecord, RunRecord};
use acdd_state::usecases::{
    ListResp, NextResp, ShowResp, StartResp, StateCommandOutput, StateQueryOutput, StatusResp,
    SubmitResp, VerifyResp,
};
use serde::Serialize;
use serde_json::Value;

/// Render a [`ClaimResp`] via canonical `acdd_state::context::render_claim_prompt_markdown`.
pub fn render_claim_markdown(claim: &ClaimResp) -> String {
    render_claim_prompt_markdown(claim, None)
}

/// Render a [`VerifyResp`] as readable Markdown (`ADR-0006`).
pub fn render_verify_markdown(verify: &VerifyResp) -> String {
    super::core::render_serializable(
        verify,
        &format!("Gate Verification: {}", verify.node_cursor),
    )
}

/// Render a [`SubmitResp`] as readable Markdown (`ADR-0006`).
pub fn render_submit_markdown(submit: &SubmitResp) -> String {
    super::core::render_serializable(submit, &format!("Gate Submission: {}", submit.node_cursor))
}

/// Render a [`StateCommandOutput`] as readable Markdown (`ADR-0006`).
pub fn render_state_command_markdown(output: &StateCommandOutput, title: &str) -> String {
    match output {
        StateCommandOutput::Claim(claim) => render_claim_markdown(claim),
        StateCommandOutput::Start(start) => render_start_markdown(start),
        StateCommandOutput::Verify(verify) => render_verify_markdown(verify),
        StateCommandOutput::Submit(submit) => render_submit_markdown(submit),
        StateCommandOutput::BlackboardPost(resp) => {
            super::coordination::render_blackboard_markdown(
                &serde_json::json!({ "message": resp.entry }),
                title,
            )
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
        StateQueryOutput::Next(next) => render_next_markdown(next),
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
            None => super::core::render_serializable(&resp.snapshot, "Observability"),
        },
        StateQueryOutput::Context(claim) => render_claim_markdown(claim),
        StateQueryOutput::Roadmap(resp) => {
            super::roadmap::render_roadmap_markdown(resp, roadmap_detail)
        }
        other => render_tagged_enum_markdown(other, title),
    }
}

fn push_run_summary(out: &mut String, run: &RunRecord) {
    out.push_str(&format!(
        "- **Run**: `{}` (`{}`)\n",
        run.run_id, run.run_name
    ));
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
        let joined = items
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("- **{label}**: {joined}\n"));
    }
}

fn push_gate_dispatch_markdown(out: &mut String, entry: &GateDispatchInfo) {
    out.push_str(&format!("- `{}`: ", entry.cursor));
    let mut details = Vec::new();
    if let Some(ref role) = entry.role {
        let mode = entry.mode.unwrap_or_default();
        match mode {
            RoleExecutionMode::Inline => out.push_str(&format!("run `{role}` `inline`")),
            RoleExecutionMode::Subagent => out.push_str(&format!("spawn `{role}` (`subagent`)")),
        }
        if mode == RoleExecutionMode::Subagent {
            details.push(if entry.reuse_on_reject == Some(true) {
                "reuse on reject".to_owned()
            } else {
                "fresh worker on reject".to_owned()
            });
        }
        if !entry.models.is_empty() {
            let models = entry
                .models
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            details.push(format!("models: {models}"));
        }
    } else {
        out.push_str("run directly");
    }
    if let Some(ref cmd) = entry.command {
        let suffix = if entry.role.is_some() {
            ""
        } else {
            " via `verify` + `submit`"
        };
        details.push(format!("cmd: `{cmd}`{suffix}"));
    }
    if let Some(ref call) = entry.call {
        details.push(format!("call: `{call}`"));
    }
    if !details.is_empty() {
        out.push_str(&format!(" ({})", details.join("; ")));
    }
    if let Some(ref guidance) = entry.guidance {
        out.push_str(&format!(" — {guidance}"));
    }
    out.push('\n');
}

fn push_dispatch_section(out: &mut String, heading: &str, items: &[GateDispatchInfo]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {heading}\n\n"));
    for entry in items {
        push_gate_dispatch_markdown(out, entry);
    }
}

fn render_start_markdown(resp: &StartResp) -> String {
    let mut out = String::from("# Run Started\n\n");
    push_run_summary(&mut out, &resp.run);
    if let Some(ref spec_path) = resp.materialized_spec_path {
        out.push_str(&format!("- **Materialized Spec**: `{spec_path}`\n"));
    }
    push_backtick_list(&mut out, "Ready", &resp.ready_nodes);
    push_dispatch_section(&mut out, "Gate Dispatch Plan", &resp.gate_plan);
    out
}

fn render_next_markdown(resp: &NextResp) -> String {
    let Some(ref run_id) = resp.run_id else {
        return "# Next Ready Gates\n\n_No active run._\n".to_owned();
    };
    let mut out = String::from("# Next Ready Gates\n\n");
    out.push_str(&format!("- **Run**: `{run_id}`"));
    if let Some(isolation) = resp.isolation {
        out.push_str(&format!(" | **Isolation**: `{isolation:?}`"));
    }
    out.push('\n');
    push_backtick_list(&mut out, "Ready Cursors", &resp.ready_cursors);
    if !resp.waves.is_empty() {
        let wave_strs: Vec<String> = resp
            .waves
            .iter()
            .enumerate()
            .map(|(idx, wave)| {
                let items = wave
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("W{idx}: [{items}]")
            })
            .collect();
        out.push_str(&format!("- **Waves**: {}\n", wave_strs.join(" -> ")));
    }
    push_dispatch_section(&mut out, "Ready Gate Dispatch", &resp.ready_gates);
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
                .map(|t| format!("`{}`", super::core::shorten_hash(t.as_str())))
                .unwrap_or_else(|| "-".to_owned());
            let row = format!(
                "| `{}` | `{:?}` | `{:?}` | `{}` | {}/{} | {} | {} |\n",
                node.node_id,
                node.kind,
                node.status,
                worker,
                node.attempt,
                node.max_attempts,
                node.claim_revision,
                tree
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
            let spec = run
                .spec_id
                .as_deref()
                .or(run.spec_path.as_deref())
                .unwrap_or("-");
            out.push_str(&format!(
                "| `{}` | `{}` | `{:?}` | `{}` | `{}` | `{:?}` | {} |\n",
                run.run_id,
                run.run_name,
                run.status,
                spec,
                run.profile_id,
                run.isolation,
                run.generation
            ));
        }
    }
    if !page.tombstones.is_empty() {
        out.push_str("\n## Tombstones\n\n| Run ID | Name | Disposition | Spec / Archive | Closed |\n|---|---|---|---|---|\n");
        for tomb in &page.tombstones {
            let spec = tomb
                .spec_id
                .as_deref()
                .or(tomb.archive_path.as_deref())
                .unwrap_or("-");
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
        node.attempt,
        node.max_attempts,
        node.claim_revision,
        node.contract_revision,
        node.node_version
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
        let scope = node
            .scope
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ");
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
            out.push_str(&format!(
                "- [{:?}] `{}`: {}",
                sub.status, sub.subtask_ref, sub.title
            ));
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
            out.push_str(&super::core::render_markdown(
                preview_val,
                "Context Preview",
            ));
        }
    }
    out
}

fn render_tagged_enum_markdown<T: Serialize>(value: &T, title: &str) -> String {
    let val = serde_json::to_value(value).unwrap_or(Value::Null);
    let inner = val
        .as_object()
        .and_then(|map| map.values().next())
        .unwrap_or(&val);
    super::core::render_markdown(inner, title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acdd_core::ids::{RunId, RunName};
    use acdd_state::profile::schema::ModelSpec;
    use acdd_state::types::{IsolationMode, RunStatus};

    #[test]
    fn start_and_next_render_sequential_coordinator_instructions_without_tables() {
        let start_plan = vec![
            GateDispatchInfo {
                cursor: "tasks.build".to_owned(),
                role: Some("builder".to_owned()),
                mode: Some(RoleExecutionMode::Subagent),
                reuse_on_reject: Some(true),
                models: vec![ModelSpec {
                    model: "model-build".to_owned(),
                    reasoning: None,
                }],
                guidance: None,
                command: Some("build_check".to_owned()),
                call: None,
            },
            GateDispatchInfo {
                cursor: "run_tests".to_owned(),
                role: None,
                mode: None,
                reuse_on_reject: None,
                models: Vec::new(),
                guidance: None,
                command: Some("verify_all".to_owned()),
                call: None,
            },
            GateDispatchInfo {
                cursor: "handoff".to_owned(),
                role: Some("coordinator".to_owned()),
                mode: Some(RoleExecutionMode::Inline),
                reuse_on_reject: Some(true),
                models: vec![ModelSpec {
                    model: "model-orch".to_owned(),
                    reasoning: None,
                }],
                guidance: None,
                command: None,
                call: None,
            },
        ];
        let mut next_plan = start_plan[..1].to_vec();
        next_plan[0].guidance = Some("Spawn builder in assigned worktree.".to_owned());
        let run = RunRecord {
            run_id: RunId::new("run-fmt").unwrap(),
            run_name: RunName::new("fmt").unwrap(),
            collection: None,
            workspace: "/tmp/ws".to_owned(),
            profile_id: "code".to_owned(),
            profile_json: "{}".to_owned(),
            spec_path: None,
            spec_id: None,
            spec_digest: "a".repeat(64),
            is_ephemeral: true,
            isolation: IsolationMode::InPlace,
            target_base: None,
            target_base_commit: None,
            branch: None,
            worktree: None,
            trace_id: "0".repeat(32),
            status: RunStatus::Active,
            cancellation_reason: None,
            root_cycle_count: 0,
            generation: 1,
            created_at: "2026-11-21T00:00:00Z".to_owned(),
            completed_at: None,
        };
        let start_md = render_start_markdown(&StartResp {
            run,
            ready_nodes: vec!["t1.build".to_owned()],
            materialized_spec_path: None,
            gate_plan: start_plan,
        });
        assert!(!start_md.contains("|---"), "{start_md}");
        assert!(start_md.contains("## Gate Dispatch Plan"), "{start_md}");
        assert!(start_md.contains("- `tasks.build`: spawn `builder` (`subagent`) (reuse on reject; models: `model-build`; cmd: `build_check`)"), "{start_md}");
        assert!(
            start_md.contains(
                "- `run_tests`: run directly (cmd: `verify_all` via `verify` + `submit`)"
            ),
            "{start_md}"
        );
        assert!(
            start_md.contains("- `handoff`: run `coordinator` `inline` (models: `model-orch`)"),
            "{start_md}"
        );

        let next_md = render_state_query_markdown(
            &StateQueryOutput::Next(NextResp {
                run_id: Some(RunId::new("run-fmt").unwrap()),
                isolation: Some(IsolationMode::InPlace),
                ready_cursors: vec!["t1.build".to_owned()],
                waves: vec![vec!["t1".to_owned()]],
                ready_gates: next_plan,
            }),
            "Next",
            false,
        );
        assert!(!next_md.contains("|---"), "{next_md}");
        assert!(next_md.contains("## Ready Gate Dispatch"), "{next_md}");
        assert!(next_md.contains("- `tasks.build`: spawn `builder` (`subagent`) (reuse on reject; models: `model-build`; cmd: `build_check`) — Spawn builder in assigned worktree."), "{next_md}");
    }
}
