//! Pure core Markdown/JSON formatting primitives and wire presentation enums.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Selects the wire presentation without changing the semantic response value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename = "OutputFormat")]
pub enum OutputFormat {
    /// Readable Markdown is the default for human-facing output.
    #[default]
    Markdown,
    /// Machine-readable JSON compatible with previous output.
    Json,
}

impl OutputFormat {
    /// Parses the exact protocol spellings accepted by CLI and MCP.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            _ => Err(format!("invalid format {value:?}; expected 'markdown' or 'json'")),
        }
    }
}

/// Describes the semantic operation whose result is being presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Indexed navigation, analysis, documentation, and source results.
    Navigation,
    /// Read-only SQL result values, which are opaque user data.
    Sql,
    /// Durable task lifecycle data and guidance.
    Task,
    /// Built-in guidance text.
    Guide,
    /// Other structured values with no compaction policy.
    General,
}

/// Shortens a verified display hash, preserving UTF-8 boundaries.
pub fn shorten_hash(hash: &str) -> &str {
    if hash.len() > 8 { hash.get(..8).unwrap_or(hash) } else { hash }
}

/// Applies the legacy hash shortening helper only to explicit presentation metadata.
pub fn shorten_metadata_hashes(value: &mut Value) {
    let Some(root) = value.as_object_mut() else { return; };
    if let Some(Value::Object(m)) = root.get_mut("metadata") {
        if is_tool_metadata(m) { shorten_fields(m, &["output_root", "corpus_hash"]); }
    }
    if let Some(Value::Object(f)) = root.get_mut("freshness") {
        if is_freshness_envelope(f, false) { shorten_fields(f, &["output_root", "corpus_hash"]); }
    }
}

pub(super) fn render_serializable<T: Serialize>(value: &T, title: &str) -> String {
    render_markdown(&serde_json::to_value(value).unwrap_or(Value::Null), title)
}

pub(super) fn render_markdown(value: &Value, title: &str) -> String {
    let mut out = format!("# {}\n", escape_text(title));
    render_markdown_fields(&mut out, value, 0, &[]);
    out
}

pub(super) fn render_markdown_fields(out: &mut String, value: &Value, depth: usize, excluded: &[&str]) {
    match value {
        Value::Object(obj) if !obj.is_empty() => {
            for (k, child) in ordered_fields(obj) {
                if !excluded.contains(&k) { render_field(out, k, child, depth); }
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, item) in items.iter().enumerate() { render_field(out, &format!("[{i}]"), item, depth); }
        }
        other => {
            indent(out, depth);
            let s = scalar(other);
            out.push_str(&inline_code(&s));
            out.push('\n');
        }
    }
}

pub(super) fn render_field(out: &mut String, key: &str, value: &Value, depth: usize) {
    indent(out, depth);
    let label = escape_label(key);
    match value {
        Value::Object(o) if !o.is_empty() => {
            out.push_str(&format!("- **{label}**\n"));
            render_markdown_fields(out, value, depth + 1, &[]);
        }
        Value::Array(a) if !a.is_empty() => {
            out.push_str(&format!("- **{label}**\n"));
            render_markdown_fields(out, value, depth + 1, &[]);
        }
        _ => {
            let val = scalar(value);
            out.push_str(&format!("- **{label}**: {}\n", inline_code(&val)));
        }
    }
}

pub(super) fn ordered_fields(object: &Map<String, Value>) -> Vec<(&str, &Value)> {
    let mut fields: Vec<_> = object.iter().map(|(k, v)| (k.as_str(), v)).collect();
    fields.sort_unstable_by_key(|(left, _)| *left);
    fields
}

fn scalar(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

pub(super) fn scalar_text(value: &Value) -> String {
    match value { Value::String(text) => escape_text(text), other => other.to_string() }
}

fn longest_backticks(value: &str) -> usize {
    let (mut longest, mut current) = (0, 0);
    for ch in value.chars() {
        current = if ch == '`' { current + 1 } else { 0 };
        longest = longest.max(current);
    }
    longest
}

pub(super) fn inline_code(value: &str) -> String {
    let delim = "`".repeat(longest_backticks(value) + 1);
    if value.starts_with(['`', ' ']) || value.ends_with(['`', ' ']) {
        format!("{delim} {value} {delim}")
    } else {
        format!("{delim}{value}{delim}")
    }
}

pub(super) fn append_fenced_block(out: &mut String, lang: &str, code: &str, depth: usize) {
    let fence = "`".repeat(longest_backticks(code).max(2) + 1);
    indent(out, depth);
    out.push_str(&format!("{fence}{lang}\n{code}"));
    if !code.ends_with('\n') { out.push('\n'); }
    indent(out, depth);
    out.push_str(&format!("{fence}\n"));
}

pub(super) fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth { out.push_str("  "); }
}

pub(super) fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('*', "\\*").replace('[', "\\[").replace(']', "\\]")
}

pub(super) fn escape_text(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

pub(super) fn code_span(value: &str) -> String {
    value.replace('`', "\\`").replace('\n', " ")
}

pub(super) fn text_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(super) fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

pub(super) fn line_number(value: &Value) -> Option<i64> {
    value.get("line").and_then(Value::as_i64)
        .or_else(|| u64_field(value, "line").and_then(|l| i64::try_from(l).ok()))
}

pub(super) fn page_items(value: &Value) -> Option<&[Value]> {
    value.as_array().map(Vec::as_slice)
        .or_else(|| value.get("items").and_then(Value::as_array).map(Vec::as_slice))
}

pub(super) const KNOWN_PAGE_FIELDS: &[&str] = &[
    "components", "languages", "coverage_by_workspace", "nodes", "items", "matches",
    "documented_in", "governing_invariants", "analysis_gaps", "incoming", "outgoing", "calls",
    "callers", "implements", "implementors", "overrides", "dependencies", "unwired",
    "untested_public_surface", "doc_diagnostics", "referenced_symbol_candidates", "phantom_tested",
    "dead_tails", "dangling_calls",
];

pub(super) fn is_freshness_envelope(object: &Map<String, Value>, allow_compacted: bool) -> bool {
    const ALLOWED: [&str; 7] = ["status", "output_root", "corpus_hash", "checked_at_unix_ms", "files_checked", "inventory_scan_ms", "refresh"];
    let source_id = object.get("output_root").is_some_and(Value::is_string) && object.get("corpus_hash").is_some_and(Value::is_string);
    let compacted = allow_compacted && object.get("status").and_then(Value::as_str) == Some("matched")
        && object.get("refresh").and_then(Value::as_str) == Some("none") && object.get("files_checked").and_then(Value::as_u64).is_some();
    object.get("status").is_some_and(Value::is_string) && object.get("refresh").is_some_and(Value::is_string)
        && object.keys().all(|k| ALLOWED.contains(&k.as_str())) && (source_id || compacted)
}

pub(super) fn is_tool_metadata(object: &Map<String, Value>) -> bool {
    object.get("generation").is_some_and(Value::is_string) && object.iter().all(|(k, v)| match k.as_str() {
        "generation" | "status" | "refresh" | "output_root" | "corpus_hash" => v.is_string(),
        "files_checked" => v.as_u64().is_some(),
        _ => false,
    })
}

pub(super) fn shorten_fields(object: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if let Some(Value::String(hash)) = object.get_mut(*field) { *hash = shorten_hash(hash).to_owned(); }
    }
}
