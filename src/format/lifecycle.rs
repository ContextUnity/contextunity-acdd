//! Pure Markdown presentation for task lifecycle responses.

use acdd_state::context::{render_claim_prompt_markdown, ClaimResp};
use acdd_state::usecases::{StateCommandOutput, StateQueryOutput, SubmitResp, VerifyResp};
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
pub fn render_state_query_markdown(output: &StateQueryOutput, title: &str) -> String {
    match output {
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
        other => render_tagged_enum_markdown(other, title),
    }
}

fn render_tagged_enum_markdown<T: Serialize>(value: &T, title: &str) -> String {
    let val = serde_json::to_value(value).unwrap_or(Value::Null);
    let inner = val.as_object().and_then(|map| map.values().next()).unwrap_or(&val);
    super::render_markdown(inner, title)
}
