use crate::format::{self, BoundedPresentation, DiscoveryKind, OperationKind, OutputFormat};
use acdd_core::response::{ResponsePolicy, MAX_OUTPUT_BYTES};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Value};
use std::{
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

// The tool result is JSON inside a text block, then escaped by JSON-RPC again.
const ENVELOPE_RESERVE: usize = 256;

/// Formats a test response value using the default output format (`Markdown`).
#[cfg(test)]
pub(crate) fn result(value: anyhow::Result<Value>, policy: &ResponsePolicy) -> CallToolResult {
    result_with_format(value, policy, OutputFormat::default())
}

/// Formats a public response with an explicit presentation selection.
#[cfg(test)]
pub(crate) fn result_with_format(
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
        Ok(value) => match format::present_value_bounded(
            value,
            budget,
            format::PresentationOptions {
                output_format,
                operation,
                title,
                explain,
                discovery,
            },
            &success_text_bytes,
        ) {
            BoundedPresentation::Ok(rendered) => {
                CallToolResult::success(vec![ContentBlock::text(rendered)])
            }
            BoundedPresentation::AstDiagnostic(rendered) => {
                CallToolResult::error(vec![ContentBlock::text(rendered)])
            }
            BoundedPresentation::Oversized(message) => bounded_error(&message, budget),
        },
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
            if let Some(state_err) = error.downcast_ref::<acdd_core::errors::AcddError>() {
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
                    bounded_error(
                        &format!("{}: {}", state_err.code, state_err.message),
                        budget,
                    )
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
            enforce(
                CallToolResult::success(vec![ContentBlock::text(text)]),
                policy,
            )
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

fn success_text_bytes(text: &str) -> usize {
    let result = CallToolResult::success(vec![ContentBlock::text(text)]);
    serialized_bytes(&result)
}

/// Computes the exact serialized JSON byte length of an MCP tool result.
pub(crate) fn serialized_bytes(result: &CallToolResult) -> usize {
    let mut counter = acdd_core::format::JsonByteCounter::default();
    serde_json::to_writer(&mut counter, result).map_or(usize::MAX, |()| counter.0)
}

/// Clamps an arbitrary MCP `CallToolResult` to the configured `policy.max_output_bytes` ceiling.
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

/// Truncates an error message along UTF-8 boundaries until its JSON envelope fits within `budget`.
fn bounded_error(message: &str, budget: usize) -> CallToolResult {
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

pub(crate) fn stdio_with_flush_reload(
    armed: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
) -> (tokio::io::Stdin, BoundedWriter<tokio::io::Stdout>) {
    let mut writer = BoundedWriter::new(tokio::io::stdout());
    writer.flush_reload = Some((armed, notify));
    (tokio::io::stdin(), writer)
}

/// Buffer a complete JSON-RPC line before publishing any of its bytes.
/// Oversized protocol errors are replaced before any bytes reach the client.
pub(crate) struct BoundedWriter<W> {
    writer: W,
    line: Vec<u8>,
    pending: Vec<u8>,
    written: usize,
    overflow: bool,
    flush_reload: Option<(Arc<AtomicBool>, Arc<tokio::sync::Notify>)>,
}

impl<W> BoundedWriter<W> {
    /// Wraps `writer` in a line-buffered JSON-RPC envelope guard.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            line: Vec::new(),
            pending: Vec::new(),
            written: 0,
            overflow: false,
            flush_reload: None,
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
        std::task::ready!(Pin::new(&mut this.writer).poll_flush(cx))?;
        if this.line.is_empty() && this.pending.is_empty() {
            if let Some((armed, notify)) = &this.flush_reload {
                if armed.swap(false, Ordering::AcqRel) {
                    notify.notify_one();
                }
            }
        }
        Poll::Ready(Ok(()))
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
        let err = acdd_core::errors::AcddError::new(
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

    #[tokio::test]
    async fn transport_accepts_exact_bound_and_replaces_oversized_frames() {
        use tokio::io::AsyncWriteExt;

        let frame_for = |size: usize| {
            let prefix = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"text\":\"";
            let suffix = "\"}}\n";
            format!(
                "{prefix}{}{suffix}",
                "x".repeat(size - prefix.len() - suffix.len())
            )
        };
        for size in [MAX_OUTPUT_BYTES - 1, MAX_OUTPUT_BYTES] {
            let mut writer = BoundedWriter::new(tokio::io::sink());
            let frame = frame_for(size);
            assert_eq!(frame.len(), size);
            serde_json::from_str::<Value>(frame.trim_end()).unwrap();
            writer.write_all(frame.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        }
        let mut output = Vec::new();
        let mut writer = BoundedWriter::new(&mut output);
        let oversized = frame_for(MAX_OUTPUT_BYTES + 1);
        writer.write_all(oversized.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
        assert!(output.len() < MAX_OUTPUT_BYTES);
        let error: Value = serde_json::from_slice(&output).unwrap();
        assert!(error.get("error").is_some());
    }

    #[test]
    fn serialization_counts_second_escaping_and_bounds_a_single_item() {
        let policy = ResponsePolicy::default();
        let res = result_with_format(
            Ok(
                json!({"nodes":{"total":2,"offset":0,"limit":30,"items":[{"id":"first","text":"\"\\\n".repeat(6000)},{"id":"second","text":"\"\\\n".repeat(6000)}],"has_more":false,"next_offset":null,"generation":"snapshot"}}),
            ),
            &policy,
            OutputFormat::Json,
        );
        assert!(serialized_bytes(&res) <= MAX_OUTPUT_BYTES - 256);
        assert_ne!(res.is_error, Some(true), "{res:?}");
        let wire = serde_json::to_value(res).unwrap();
        let payload: Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["nodes"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(payload["nodes"]["total"], 2);
        assert_eq!(payload["nodes"]["has_more"], true);
        assert_eq!(payload["nodes"]["byte_limited"], true);
        assert_eq!(payload["nodes"]["next_offset"], 1);

        for format in [OutputFormat::Json, OutputFormat::Markdown] {
            let r = result_with_format(
                Ok(json!({
                    "nodes": {
                        "total": 2, "offset": 0, "limit": 30, "has_more": false, "next_offset": null,
                        "generation": "snapshot-full-token-123456",
                        "items": [{"id": "a", "text": "\"\\\n".repeat(6000)}, {"id": "b", "text": "\"\\\n".repeat(6000)}]
                    }
                })),
                &policy,
                format,
            );
            assert_ne!(r.is_error, Some(true), "{format:?}: {r:?}");
            assert!(serialized_bytes(&r) <= MAX_OUTPUT_BYTES - 256);
            assert!(r.content[0]
                .as_text()
                .unwrap()
                .text
                .contains("snapshot-full-token-123456"));
        }

        let err_res = result_with_format(
            Ok(json!({"scalar":"\"\\Привіт".repeat(30_000)})),
            &policy,
            OutputFormat::Json,
        );
        assert_eq!(err_res.is_error, Some(true));
        assert!(serialized_bytes(&err_res) <= MAX_OUTPUT_BYTES - 256);

        let fitting_text = "λ".repeat(36_000);
        let fitting = result_with_format(
            Ok(json!({
                "tasks": [{"task_id":"fit-96kib","target":fitting_text.clone()}],
                "pagination": {"total": 1, "limit": 1, "offset": 0, "returned": 1, "next_offset": null, "has_more": false, "generation": "task-view-fit", "byte_omitted": 0}
            })),
            &policy,
            OutputFormat::Json,
        );
        assert_ne!(fitting.is_error, Some(true), "{fitting:?}");
        assert!(serialized_bytes(&fitting) <= MAX_OUTPUT_BYTES - 256);

        let low = ResponsePolicy {
            max_output_bytes: 4_096,
            ..ResponsePolicy::default()
        };
        let low_err = result_with_format(
            Ok(json!({"items":[{"id":"configured-limit","text":"x".repeat(5_000)}]})),
            &low,
            OutputFormat::Json,
        );
        assert_eq!(low_err.is_error, Some(true));
        assert!(serialized_bytes(&low_err) <= 4_096 - 256);

        let trim_policy = ResponsePolicy {
            max_output_bytes: 2_048,
            ..ResponsePolicy::default()
        };
        let bounded = result(
            Ok(json!({"nodes":{"total":3,"offset":0,"limit":3,
                "has_more":false,"next_offset":null,"generation":"tail-trim-continuation-token",
                "items":[{"name":"first-retained","text":"x".repeat(800)},
                    {"name":"middle","text":"x".repeat(800)},
                    {"name":"last-omitted","text":"x".repeat(800)}]}})),
            &trim_policy,
        );
        assert_ne!(bounded.is_error, Some(true), "{bounded:?}");
        assert!(serialized_bytes(&bounded) <= 2_048 - 256);
        let text = bounded.content[0].as_text().unwrap().text.as_str();
        assert!(
            text.contains("first-retained")
                && text.contains("tail-trim-continuation-token")
                && !text.contains("last-omitted")
        );
    }
}
