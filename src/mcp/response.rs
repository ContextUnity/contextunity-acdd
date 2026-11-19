use crate::format::{self, DiscoveryKind, OperationKind, OutputFormat};
use acdd_core::response::{ResponsePolicy, MAX_OUTPUT_BYTES};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Value};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

fn render_for_operation(
    value: &Value,
    output_format: OutputFormat,
    title: &str,
    discovery: Option<DiscoveryKind>,
    operation: OperationKind,
) -> String {
    if title == "acdd_read" && output_format == OutputFormat::Markdown {
        let mut body = value.clone();
        let hints = body
            .as_object_mut()
            .and_then(|object| object.remove("Hints"));
        let mut rendered =
            format::render_for_operation(&body, output_format, title, discovery, operation);
        if let Some(hints) = hints.and_then(|value| value.as_str().map(str::to_owned)) {
            rendered.push_str("\nHints: ");
            rendered.push_str(&hints.replace('\n', "\n       "));
            rendered.push('\n');
        }
        rendered
    } else {
        format::render_for_operation(value, output_format, title, discovery, operation)
    }
}

// The tool result is JSON inside a text block, then escaped by JSON-RPC again.
const ENVELOPE_RESERVE: usize = 256;

/// Performs result.
pub fn result(value: anyhow::Result<Value>, policy: &ResponsePolicy) -> CallToolResult {
    result_with_format(value, policy, OutputFormat::default())
}

/// Formats a public response with an explicit presentation selection.
pub fn result_with_format(
    value: anyhow::Result<Value>,
    policy: &ResponsePolicy,
    output_format: OutputFormat,
) -> CallToolResult {
    result_for(
        value,
        policy,
        output_format,
        OperationKind::Navigation,
        "Result",
        None,
    )
}

pub(crate) fn result_for(
    value: anyhow::Result<Value>,
    policy: &ResponsePolicy,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &str,
    explain: Option<bool>,
) -> CallToolResult {
    result_for_presented(
        value,
        policy,
        output_format,
        operation,
        title,
        explain,
        None,
    )
}

pub(crate) fn result_for_presented(
    value: anyhow::Result<Value>,
    policy: &ResponsePolicy,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &str,
    explain: Option<bool>,
    discovery: Option<DiscoveryKind>,
) -> CallToolResult {
    let budget = policy.max_output_bytes.clamp(1024, MAX_OUTPUT_BYTES) - ENVELOPE_RESERVE;
    match value {
        Ok(mut value) => {
            if discovery == Some(DiscoveryKind::Ast) && value.get("diagnostic").is_some() {
                let diagnostic = &value["diagnostic"];
                let rendered = match output_format {
                    OutputFormat::Markdown => {
                        render_for_operation(&value, output_format, title, discovery, operation)
                    }
                    OutputFormat::Json => format::ast_legacy_error(diagnostic),
                };
                return if success_text_bytes(&rendered) <= budget {
                    CallToolResult::error(vec![ContentBlock::text(rendered)])
                } else {
                    bounded_error(
                        "AST diagnostic exceeds the response byte limit; reduce the pattern or use JSON output.",
                        budget,
                    )
                };
            }
            // Capture only active/candidate generation tokens before compacting in place,
            // so tail trimming can restore tokens if the compacted response exceeds budget.
            let saved_generations = GenerationSnapshot::capture(&value);
            format::compact(&mut value, operation, explain);
            let rendered =
                render_for_operation(&value, output_format, title, discovery, operation);
            if success_text_bytes(&rendered) <= budget {
                return CallToolResult::success(vec![ContentBlock::text(rendered)]);
            }

            if let Some(snapshot) = &saved_generations {
                snapshot.restore(&mut value);
            }
            if title == "acdd_read" {
                let trimmed = trim_snippet_source_to_budget(
                    &mut value,
                    output_format,
                    operation,
                    title,
                    explain,
                    discovery,
                    budget,
                );
                if !trimmed {
                    if value.get("source_preview").is_some() {
                        return bounded_error(
                            "Snippet line exceeds the serialized response byte budget; use a larger response budget or read the local source file. No partial line was returned.",
                            budget,
                        );
                    }
                    return oversized_response_error(
                        &value,
                        output_format,
                        operation,
                        title,
                        explain,
                        discovery,
                        budget,
                    );
                }
                format::compact(&mut value, operation, explain);
                let rendered =
                    render_for_operation(&value, output_format, title, discovery, operation);
                if success_text_bytes(&rendered) > budget {
                    return oversized_response_error(
                        &value,
                        output_format,
                        operation,
                        title,
                        explain,
                        discovery,
                        budget,
                    );
                }
                return CallToolResult::success(vec![ContentBlock::text(rendered)]);
            }

            let allow_tasks = operation == OperationKind::Navigation && value.get("tasks").is_some();
            let allow_items = matches!(operation, OperationKind::Navigation | OperationKind::Sql);
            while trim_page_tails(&mut value, allow_tasks, allow_items) {
                format::compact(&mut value, operation, explain);
                let candidate =
                    render_for_operation(&value, output_format, title, discovery, operation);
                if success_text_bytes(&candidate) <= budget {
                    return CallToolResult::success(vec![ContentBlock::text(candidate)]);
                }
                if let Some(snapshot) = &saved_generations {
                    snapshot.restore(&mut value);
                }
            }
            format::compact(&mut value, operation, explain);
            oversized_response_error(
                &value,
                output_format,
                operation,
                title,
                explain,
                discovery,
                budget,
            )
        }
        Err(error) => {
            if let Some(acdd_index::db::reader::SelectorError::Ambiguous {
                selector,
                candidates,
            }) = error.downcast_ref::<acdd_index::db::reader::SelectorError>()
            {
                let bounded =
                    |text: &str, maximum: usize| text.chars().take(maximum).collect::<String>();
                let candidate_values = candidates
                    .iter()
                    .take(8)
                    .map(|candidate| {
                        json!({
                            "id": bounded(&candidate.id, 256),
                            "path": bounded(&candidate.path, 256),
                            "line": candidate.line,
                            "qualname": bounded(&candidate.qualname, 256),
                            "signature": bounded(&candidate.signature, 512),
                        })
                    })
                    .collect::<Vec<_>>();
                let envelope = json!({
                    "outcome":"ambiguous",
                    "selector":bounded(selector, 512),
                    "message":format!("ambiguous selector {}; use an exact node id or workspace-relative path", bounded(selector, 512)),
                    "total":candidates.len(),
                    "candidates":candidate_values,
                });
                return CallToolResult::error(vec![ContentBlock::text(envelope.to_string())]);
            }
            if let Some(state_err) = error.downcast_ref::<acdd_core::errors::StateError>() {
                let mut err_obj = serde_json::Map::new();
                err_obj.insert("code".into(), json!(state_err.code.as_str()));
                err_obj.insert("message".into(), json!(state_err.message));
                if let Some(hint) = &state_err.hint {
                    err_obj.insert("hint".into(), json!(hint));
                }
                let text = json!({ "error": Value::Object(err_obj) }).to_string();
                return if text.len() <= budget {
                    CallToolResult::error(vec![ContentBlock::text(text)])
                } else {
                    bounded_error(&format!("{}: {}", state_err.code, state_err.message), budget)
                };
            }
            let interrupted = error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::SqliteFailure(code, _))
                        if code.code == rusqlite::ErrorCode::OperationInterrupted
                )
            });
            if interrupted {
                bounded_error(
                    "Graph query exceeded the 2-second SQLite budget. Narrow the selector to an indexed symbol or path, reduce traversal depth, and retry; reducing the page limit alone may not reduce query work. For diagnostic totals, use code_map_analyze with target='' (cycles are omitted by default); for cycles, analyze a smaller path with include_cycles=true.",
                    budget,
                )
            } else {
                bounded_error(&format!("{error:#}"), budget)
            }
        }
    }
}

fn trim_snippet_source_to_budget(
    value: &mut Value,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &str,
    explain: Option<bool>,
    discovery: Option<DiscoveryKind>,
    budget: usize,
) -> bool {
    let Some(source) = value.get("source").and_then(Value::as_str) else {
        return false;
    };
    let Some(preview) = value.get("source_preview") else {
        return false;
    };
    let Some(node_line) = value["node"]["line"]
        .as_u64()
        .and_then(|line| usize::try_from(line).ok())
    else {
        return false;
    };
    let Some(body_offset) = preview["body_offset"]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
    else {
        return false;
    };
    let Some(total_body_lines) = preview["total_body_lines"]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
    else {
        return false;
    };
    let Some(body_lines) = preview["body_lines"]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
    else {
        return false;
    };
    let Some(leading_lines) = preview["leading_lines"]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
    else {
        return false;
    };
    let omitted_leading = preview["omitted_leading_lines"]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .unwrap_or_default();
    let Some(body_end) = body_offset.checked_add(body_lines) else {
        return false;
    };
    if body_end > total_body_lines {
        return false;
    }

    let source_lines = source.split_inclusive('\n').collect::<Vec<_>>();
    if leading_lines > source_lines.len() {
        return false;
    }
    let leading_parts = &source_lines[..leading_lines];
    let body_parts = &source_lines[leading_lines..];
    if body_parts.len() < body_lines {
        return false;
    }
    let source_page = SnippetSourcePage {
        node_line,
        body_offset,
        total_body_lines,
        original_leading: leading_lines,
        omitted_leading,
    };
    let fits = |candidate: &mut Value| {
        format::compact(candidate, operation, explain);
        let rendered = render_for_operation(candidate, output_format, title, discovery, operation);
        success_text_bytes(&rendered) <= budget
    };

    if body_lines > 0 {
        for kept_leading in (0..=leading_lines).rev() {
            let leading_text = leading_parts[leading_lines - kept_leading..].concat();
            let mut low = 1;
            let mut high = body_lines;
            let mut best = None;
            while low <= high {
                let kept_body = low + (high - low) / 2;
                let body_text = body_parts[..kept_body].concat();
                let Some(mut candidate) = snippet_source_candidate(
                    value,
                    &source_page,
                    kept_leading,
                    kept_body,
                    &leading_text,
                    &body_text,
                ) else {
                    return false;
                };
                if fits(&mut candidate) {
                    best = Some((kept_body, candidate));
                    low = kept_body.saturating_add(1);
                } else {
                    high = kept_body - 1;
                }
            }
            if let Some((_, candidate)) = best {
                *value = candidate;
                return true;
            }
        }
    } else if body_offset == total_body_lines {
        for kept_leading in (0..=leading_lines).rev() {
            let leading_text = leading_parts[leading_lines - kept_leading..].concat();
            let Some(mut candidate) =
                snippet_source_candidate(value, &source_page, kept_leading, 0, &leading_text, "")
            else {
                return false;
            };
            if fits(&mut candidate) {
                *value = candidate;
                return true;
            }
        }
    }

    false
}

struct SnippetSourcePage {
    node_line: usize,
    body_offset: usize,
    total_body_lines: usize,
    original_leading: usize,
    omitted_leading: usize,
}

fn snippet_source_candidate(
    original: &Value,
    page: &SnippetSourcePage,
    leading_lines: usize,
    body_lines: usize,
    leading: &str,
    body: &str,
) -> Option<Value> {
    let next = page.body_offset.checked_add(body_lines)?;
    let more = next < page.total_body_lines;
    let start_line = if leading_lines > 0 {
        page.node_line.checked_sub(leading_lines)?
    } else {
        page.node_line.checked_add(page.body_offset)?
    };
    let end_line = if body_lines > 0 {
        page.node_line.checked_add(next.checked_sub(1)?)
    } else if leading_lines > 0 {
        page.node_line.checked_sub(1)
    } else {
        None
    };
    let hint = if more {
        Some("Repeat the same tool and selector with show_source=true, next_source_offset as source_offset, and this generation.".to_owned())
    } else {
        None
    };
    let dropped_leading = page.original_leading.checked_sub(leading_lines)?;
    let omitted_leading = page.omitted_leading.checked_add(dropped_leading)?;
    let start_line_value = (!leading.is_empty() || !body.is_empty()).then_some(start_line);
    let mut candidate = original.clone();
    candidate["source"] = json!(format!("{leading}{body}"));
    {
        let preview = candidate.get_mut("source_preview")?.as_object_mut()?;
        preview.insert("start_line".into(), json!(start_line_value));
        preview.insert("end_line".into(), json!(end_line));
        preview.insert("body_lines".into(), json!(body_lines));
        preview.insert("leading_lines".into(), json!(leading_lines));
        preview.insert("omitted_leading_lines".into(), json!(omitted_leading));
        preview.insert(
            "omitted_body_lines".into(),
            json!(page.total_body_lines.saturating_sub(next)),
        );
        preview.insert("has_more".into(), json!(more));
        preview.insert("truncated_line".into(), json!(false));
        preview.insert("next_source_offset".into(), json!(more.then_some(next)));
        preview.insert("continuation_hint".into(), json!(hint));
    }
    if let (Some(path), Some(start), Some(end)) = (
        candidate["node"]["path"].as_str(),
        start_line_value.and_then(|line| u64::try_from(line).ok()),
        end_line.and_then(|line| u64::try_from(line).ok()),
    ) {
        candidate["header"] = json!(format!(
            "{} {} in {path}:{start}-{end}",
            candidate["node"]["kind"].as_str().unwrap_or("symbol"),
            candidate["node"]["qualname"]
                .as_str()
                .or_else(|| candidate["node"]["name"].as_str())
                .unwrap_or("symbol")
        ));
    } else if let Some(object) = candidate.as_object_mut() {
        object.remove("header");
    }
    Some(candidate)
}

pub(crate) fn rejected_arguments(
    fields: &[String],
    message: Option<String>,
    supported_parameters: Option<&[&str]>,
    suggestions: &[(String, &str)],
) -> CallToolResult {
    let truncated_fields = fields
        .iter()
        .take(16)
        .map(|field| field.chars().take(128).collect::<String>())
        .collect::<Vec<_>>();
    let mut error_obj = serde_json::Map::new();
    error_obj.insert("code".into(), serde_json::json!("rejected_arguments"));
    error_obj.insert("fields".into(), serde_json::json!(truncated_fields));
    if let Some(msg) = message {
        error_obj.insert("message".into(), serde_json::json!(msg));
    }
    if let Some(allowed) = supported_parameters {
        error_obj.insert("supported_parameters".into(), serde_json::json!(allowed));
    }
    if !suggestions.is_empty() {
        let mut sugg_map = serde_json::Map::new();
        for (orig, sugg) in suggestions {
            sugg_map.insert(orig.clone(), serde_json::json!(sugg));
        }
        error_obj.insert("suggestions".into(), Value::Object(sugg_map));
    }
    CallToolResult::error(vec![ContentBlock::text(
        json!({"error": Value::Object(error_obj)}).to_string(),
    )])
}

pub(crate) fn invalid_format(value: &Value, policy: &ResponsePolicy) -> CallToolResult {
    let budget = policy.max_output_bytes.clamp(1024, MAX_OUTPUT_BYTES) - ENVELOPE_RESERVE;
    let mut preview_chars = 128;
    loop {
        let display_value = match value {
            Value::String(value) => {
                let preview = value.chars().take(preview_chars).collect::<String>();
                let truncated = preview.chars().count() < value.chars().count();
                let encoded =
                    serde_json::to_string(&preview).unwrap_or_else(|_| "\"<string>\"".into());
                if truncated {
                    format!("{encoded} (truncated)")
                } else {
                    encoded
                }
            }
            Value::Array(_) => "<array>".to_owned(),
            Value::Object(_) => "<object>".to_owned(),
            _ => value.to_string(),
        };
        let message = format!("invalid format {display_value}; expected 'markdown' or 'json'");
        let result = CallToolResult::error(vec![ContentBlock::text(
            json!({
                "error": {
                    "code": "INVALID_FORMAT",
                    "message": message
                }
            })
            .to_string(),
        )]);
        if serialized_bytes(&result) <= budget {
            return result;
        }
        if !matches!(value, Value::String(_)) || preview_chars == 0 {
            return CallToolResult::error(vec![ContentBlock::text(
                json!({
                    "error": {
                        "code": "INVALID_FORMAT",
                        "message": "invalid format value; expected 'markdown' or 'json'"
                    }
                })
                .to_string(),
            )]);
        }
        preview_chars /= 2;
    }
}

/// Formats a state or plugin tool output in Markdown or JSON, enforcing the active `ResponsePolicy`.
pub(crate) fn state_result(
    rendered_or_value: anyhow::Result<(String, Value)>,
    policy: &ResponsePolicy,
    output_format: OutputFormat,
    title: &str,
) -> CallToolResult {
    match rendered_or_value {
        Ok((markdown, json_value)) => {
            let text = match output_format {
                OutputFormat::Markdown => markdown,
                OutputFormat::Json => {
                    serde_json::to_string(&json_value).unwrap_or_else(|_| "null".to_owned())
                }
            };
            enforce(CallToolResult::success(vec![ContentBlock::text(text)]), policy)
        }
        Err(error) => result_for(
            Err(error),
            policy,
            output_format,
            OperationKind::Task,
            title,
            None,
        ),
    }
}

struct GenerationSnapshot {
    generation: Option<Value>,
    children: Vec<(String, GenerationSnapshot)>,
}

impl GenerationSnapshot {
    fn capture(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let generation = obj.get("generation").cloned();
        let mut children = Vec::new();
        for (key, child) in obj {
            if let Some(snap) = Self::capture(child) {
                children.push((key.clone(), snap));
            }
        }
        (generation.is_some() || !children.is_empty()).then_some(Self {
            generation,
            children,
        })
    }

    fn restore(&self, value: &mut Value) {
        let Some(obj) = value.as_object_mut() else {
            return;
        };
        if let Some(gen) = &self.generation {
            obj.insert("generation".into(), gen.clone());
        }
        for (key, snap) in &self.children {
            if let Some(child) = obj.get_mut(key) {
                snap.restore(child);
            }
        }
    }
}

/// Performs serialized bytes.
pub fn serialized_bytes(result: &CallToolResult) -> usize {
    let mut counter = acdd_core::format::JsonByteCounter::default();
    serde_json::to_writer(&mut counter, result).map_or(usize::MAX, |()| counter.0)
}

fn success_text_bytes(text: &str) -> usize {
    let result = CallToolResult::success(vec![ContentBlock::text(text)]);
    serialized_bytes(&result)
}

fn oversized_response_error(
    value: &Value,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &str,
    explain: Option<bool>,
    discovery: Option<DiscoveryKind>,
    budget: usize,
) -> CallToolResult {
    let is_task_list = operation == OperationKind::Navigation && value.get("tasks").is_some();
    let message = if is_task_list {
        if value
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(|tasks| tasks.len() == 1)
        {
            let mut metadata = value.clone();
            metadata["tasks"] = json!([]);
            format::compact(&mut metadata, operation, explain);
            let rendered =
                render_for_operation(&metadata, output_format, title, discovery, operation);
            if success_text_bytes(&rendered) <= budget {
                "One complete task record exceeds the configured byte limit. Request compact task details, a smaller page, or a narrower filter; no partial task or continuation was returned."
            } else {
                "Required task-list pagination metadata exceeds the configured byte limit. Narrow the workspace or filters; no partial task or continuation was returned."
            }
        } else {
            "Task-list pagination metadata exceeds the configured byte limit. Narrow the workspace or filters; no partial task or continuation was returned."
        }
    } else {
        "Output exceeds the response byte limit because a complete item cannot fit. Use detail='compact', a smaller page, a narrower filter, or read the oversized record from its source; no partial item was returned and no continuation was consumed."
    };
    bounded_error(message, budget)
}

/// Performs enforce.
pub fn enforce(result: CallToolResult, policy: &ResponsePolicy) -> CallToolResult {
    let budget = policy.max_output_bytes.clamp(1024, MAX_OUTPUT_BYTES) - ENVELOPE_RESERVE;
    if serialized_bytes(&result) <= budget {
        return result;
    }
    let text = result
        .content
        .first()
        .and_then(ContentBlock::as_text)
        .map(|text| text.text.as_str());
    if result.is_error == Some(true) {
        bounded_error(
            text.unwrap_or("Tool error exceeds the response byte limit; narrow the request"),
            budget,
        )
    } else {
        bounded_error(
            "Tool output exceeds the response byte limit; narrow the request",
            budget,
        )
    }
}

/// Performs bounded error.
pub fn bounded_error(message: &str, budget: usize) -> CallToolResult {
    let mut end = acdd_core::format::utf8_floor_boundary(message, budget);
    loop {
        end = acdd_core::format::utf8_floor_boundary(message, end);
        let text = if end < message.len() {
            format!("{} [error truncated; narrow the request]", &message[..end])
        } else {
            message.to_owned()
        };
        let result = CallToolResult::error(vec![ContentBlock::text(text)]);
        if serialized_bytes(&result) <= budget || end == 0 {
            return result;
        }
        end /= 2;
    }
}

fn trim_page_tails(value: &mut Value, allow_tasks: bool, allow_items: bool) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    if allow_tasks {
        if let Some(pagination) = object.get("pagination").and_then(Value::as_object).cloned() {
            let page_is_continuable = pagination.get("total").and_then(Value::as_u64).is_some()
                && pagination.get("offset").and_then(Value::as_u64).is_some()
                && pagination
                    .get("generation")
                    .and_then(Value::as_str)
                    .is_some_and(|generation| !generation.is_empty());
            if page_is_continuable {
                if let Some(tasks) = object.get_mut("tasks").and_then(Value::as_array_mut) {
                    if tasks.len() > 1 {
                        tasks.pop();
                        let returned = tasks.len() as u64;
                        let offset = pagination
                            .get("offset")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let total = pagination
                            .get("total")
                            .and_then(Value::as_u64)
                            .unwrap_or(returned);
                        let next = offset.saturating_add(returned);
                        if let Some(page) =
                            object.get_mut("pagination").and_then(Value::as_object_mut)
                        {
                            page.insert("returned".into(), json!(returned));
                            let omitted = page
                                .get("byte_omitted")
                                .and_then(Value::as_u64)
                                .unwrap_or(0);
                            page.insert("byte_omitted".into(), json!(omitted.saturating_add(1)));
                            page.insert("has_more".into(), json!(next < total));
                            let filters = page.get("filters").cloned().unwrap_or(Value::Null);
                            let generation = page
                                .get("generation")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            if next < total {
                                page.insert("next_offset".into(), json!(next));
                                let hint = page
                                    .get("continuation_hint")
                                    .and_then(Value::as_object)
                                    .cloned();
                                if let Some(mut hint) = hint {
                                    hint.insert("offset".into(), json!(next));
                                    hint.insert("generation".into(), json!(generation));
                                    page.insert("continuation_hint".into(), Value::Object(hint));
                                } else {
                                    page.insert("continuation_hint".into(), json!(format!("Repeat the same tool with the same filters ({filters}) and generation={generation}, offset={next}.")));
                                }
                            } else {
                                page.remove("next_offset");
                                page.insert("continuation_hint".into(), json!(format!(
                            "No next page. Repeat the same tool with the same filters ({filters}) at offset=0 to refresh; generation={generation}."
                        )));
                            }
                        }
                        return true;
                    }
                }
            }
        }
    }
    if allow_tasks && object.contains_key("tasks") {
        return false;
    }
    let total = object.get("total").and_then(Value::as_u64);
    let offset = object.get("offset").and_then(Value::as_u64);
    let generation = object
        .get("generation")
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
        .map(str::to_owned);
    let has_more = object.get("has_more").and_then(Value::as_bool) == Some(true);
    let has_usable_cursor = object.get("next_offset").and_then(Value::as_u64).is_some();
    if allow_items
        && offset.is_some()
        && generation.is_some()
        && (total.is_some() || (has_more && has_usable_cursor))
    {
        if let Some(items) = object.get_mut("items").and_then(Value::as_array_mut) {
            if items.len() > 1 {
                items.pop();
                let returned = items.len();
                let offset = offset.unwrap_or_default();
                let returned = returned as u64;
                let next = offset.saturating_add(returned);
                let has_more = total.map_or(has_more, |total| next < total);
                let omitted = object
                    .get("byte_omitted")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                object.insert("returned".into(), json!(returned));
                object.insert("byte_omitted".into(), json!(omitted.saturating_add(1)));
                object.insert("has_more".into(), json!(has_more));
                if has_more {
                    object.insert("next_offset".into(), json!(next));
                    object.insert("byte_limited".into(), json!(true));
                    let generation = generation.unwrap_or_default();
                    let filters = object
                        .get("filters")
                        .or_else(|| object.get("request_scope"))
                        .or_else(|| {
                            object
                                .get("continuation_hint")
                                .filter(|hint| hint.is_object())
                        })
                        .cloned()
                        .unwrap_or(json!("the original request filters"));
                    object.insert("continuation_hint".into(), json!(format!("Repeat the same tool and filters ({filters}) with generation={generation} and offset={next}; whole items after this offset were omitted to respect max_output_bytes.")));
                } else {
                    object.remove("next_offset");
                    object.remove("continuation_hint");
                }
                return true;
            }
            return false;
        }
    }
    // Preserve scalar node metadata, source continuation and freshness evidence.
    // Collection boundaries are produced by the database; arbitrary arrays are not pages.
    for child in object.values_mut() {
        if trim_page_tails(child, allow_tasks, allow_items) {
            return true;
        }
    }
    false
}

/// Performs stdio.
pub fn stdio() -> (tokio::io::Stdin, BoundedWriter<tokio::io::Stdout>) {
    (tokio::io::stdin(), BoundedWriter::new(tokio::io::stdout()))
}

/// Buffer a complete JSON-RPC line before publishing any of its bytes.
/// Oversized protocol errors are replaced before any bytes reach the client.
pub struct BoundedWriter<W> {
    writer: W,
    line: Vec<u8>,
    pending: Vec<u8>,
    written: usize,
    overflow: bool,
}

impl<W> BoundedWriter<W> {
    /// Creates a new instance.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            line: Vec::new(),
            pending: Vec::new(),
            written: 0,
            overflow: false,
        }
    }
}

impl<W: AsyncWrite + Unpin> BoundedWriter<W> {
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.pending.len() {
            let count = std::task::ready!(
                Pin::new(&mut self.writer).poll_write(cx, &self.pending[self.written..])
            )?;
            if count == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.written += count;
        }
        self.pending.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for BoundedWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        let count = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |at| at + 1);
        let accepted = count.min(MAX_OUTPUT_BYTES.saturating_sub(this.line.len()));
        this.overflow |= accepted < count;
        this.line.extend_from_slice(&bytes[..accepted]);
        if count > 0 && bytes[count - 1] == b'\n' {
            if this.overflow {
                this.pending = protocol_limit_error(&this.line);
                this.line.clear();
                this.overflow = false;
            } else {
                std::mem::swap(&mut this.line, &mut this.pending);
            }
        }
        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        Pin::new(&mut this.writer).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.drain(cx))?;
        if !this.line.is_empty() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete JSON-RPC response",
            )));
        }
        Pin::new(&mut this.writer).poll_shutdown(cx)
    }
}

fn protocol_limit_error(prefix: &[u8]) -> Vec<u8> {
    // rmcp serializes the top-level id before result/error. Decode only that value;
    // the retained prefix may end inside a large message or an oversized id.
    let id = prefix
        .windows(5)
        .position(|part| part == b"\"id\":")
        .and_then(|at| {
            let mut decoder = serde_json::Deserializer::from_slice(&prefix[at + 5..]);
            <Value as serde::Deserialize>::deserialize(&mut decoder).ok()
        })
        .filter(|id| {
            matches!(id, Value::String(_) | Value::Number(_)) && id.to_string().len() <= 128
        })
        .unwrap_or(Value::Null);
    let mut bytes = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":format!("Response exceeds {MAX_OUTPUT_BYTES} bytes. Narrow the request arguments or output. Request ids longer than 128 bytes are omitted from this error.")}}).to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_size_matches_the_serialized_mcp_envelope() {
        let value = json!({"message":"quotes: \"; slash: \\; newline: \n; unicode: λ", "items":[1, true, null]});
        let text = value.to_string();
        let actual = CallToolResult::success(vec![ContentBlock::text(text.clone())]);
        assert_eq!(success_text_bytes(&text), serialized_bytes(&actual));
    }

    #[test]
    fn interrupted_sqlite_query_reports_a_recovery_path() {
        let sqlite_error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
            None,
        );
        let result = result(Err(sqlite_error.into()), &ResponsePolicy::default());
        assert_eq!(result.is_error, Some(true));
        let message = result.content[0].as_text().unwrap().text.as_str();
        assert!(message.contains("2-second SQLite budget"), "{message}");
        assert!(message.contains("reduce traversal depth"), "{message}");
    }

    #[test]
    fn state_error_formats_structured_code_message_and_hint() {
        let err = acdd_core::errors::StateError::new(
            acdd_core::errors::ErrorCode::FeatureDisabled,
            "state subsystem disabled",
        )
        .with_hint("enable modules.state");
        let res = state_result(
            Err(err.into()),
            &ResponsePolicy::default(),
            OutputFormat::Json,
            "acdd_state",
        );
        assert_eq!(res.is_error, Some(true));
        let payload: Value =
            serde_json::from_str(res.content[0].as_text().unwrap().text.as_str()).unwrap();
        assert_eq!(payload["error"]["code"], "FEATURE_DISABLED");
        assert_eq!(payload["error"]["message"], "state subsystem disabled");
        assert_eq!(payload["error"]["hint"], "enable modules.state");
    }
}
