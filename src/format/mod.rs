//! Shared, pure response formatting and presentation compaction.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod coordination;
mod discovery;
mod lifecycle;
mod lint;
mod presentation;
mod roadmap;
mod traversal;
pub use coordination::{render_blackboard_markdown, render_plugin_tool_markdown};
pub use discovery::{file_uri_for_path, DiscoveryKind};
pub use lifecycle::{
    render_claim_markdown, render_state_command_markdown, render_state_query_markdown,
    render_submit_markdown, render_verify_markdown,
};
pub(crate) use presentation::{
    attach_document_presentation_uris, attach_lint_presentation_uris, attach_presentation_uris,
    attach_traversal_presentation_uris, present_value_bounded, BoundedPresentation,
    PresentationOptions,
};

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

/// Formats one resolved semantic value for the requested surface.
pub fn render(value: &Value, format: OutputFormat, title: &str) -> String {
    if format == OutputFormat::Json {
        return serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
    }
    match title {
        "code_map_overview" => discovery::render(value, title, DiscoveryKind::Overview),
        "code_map_inspect" => discovery::render(value, title, DiscoveryKind::Inspect),
        "code_map_search" => discovery::render(value, title, DiscoveryKind::Search),
        "ast_grep_search" => discovery::render(value, title, DiscoveryKind::Ast),
        "code_map_lint" => lint::render(value, title),
        "search_docs" | "get_doc" => coordination::render_docs(value, title),
        "acdd_read" => render_acdd_read(value),
        _ => render_markdown(value, title),
    }
}

/// Formats a discovery result using its semantic operation identity.
pub fn render_presented(
    value: &Value,
    format: OutputFormat,
    title: &str,
    discovery: Option<DiscoveryKind>,
) -> String {
    match (format, discovery) {
        (OutputFormat::Markdown, Some(kind)) => discovery::render(value, title, kind),
        _ => render(value, format, title),
    }
}

/// Renders one operation with an admitted domain layout when its semantic operation is known.
pub fn render_for_operation(
    value: &Value,
    format: OutputFormat,
    title: &str,
    discovery: Option<DiscoveryKind>,
    operation: OperationKind,
) -> String {
    if format == OutputFormat::Markdown && operation == OperationKind::Navigation && discovery.is_none() {
        if let Some(kind) = traversal::kind_for(title, value) {
            return traversal::render(value, title, kind);
        }
    }
    render_presented(value, format, title, discovery)
}

pub(crate) fn ast_legacy_error(diagnostic: &Value) -> String {
    discovery::ast_legacy_error(diagnostic)
}

/// Applies only presentation compaction permitted for a known operation.
pub fn compact(value: &mut Value, operation: OperationKind, explain: Option<bool>) {
    if operation != OperationKind::Navigation { return; }
    shorten_metadata_hashes(value);
    let Some(root) = value.as_object_mut() else { return; };
    if let Some(explain) = explain { compact_symbol(root, explain); }
    let source_cont = root.get("source_preview").and_then(Value::as_object).is_some_and(|p| {
        p.get("has_more") == Some(&Value::Bool(true))
            || p.get("next_source_offset").and_then(Value::as_u64).is_some_and(|o| o > 0)
    });
    let (mut nested_cont, mut saw_page, mut all_empty) = (false, false, true);
    for key in KNOWN_PAGE_FIELDS {
        if let Some(page) = root.get_mut(*key).and_then(Value::as_object_mut) {
            if is_paging_envelope(page) {
                saw_page = true;
                all_empty &= empty_page(page);
                nested_cont |= has_continuation(page);
            }
            compact_navigation_page(page);
        }
    }
    if !source_cont && !nested_cont && !has_continuation(root) { root.remove("generation"); }
    compact_freshness(root);
    if saw_page && all_empty && !source_cont && root.get("freshness").and_then(Value::as_object).is_some_and(|f| {
        f.get("status") == Some(&Value::String("matched".into()))
            && f.get("refresh") == Some(&Value::String("none".into()))
    }) {
        root.remove("freshness");
    }
    move_owned_metadata_to_end(root);
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

fn render_acdd_read(value: &Value) -> String {
    let mut out = String::from("# acdd_read\n");
    render_read_node(&mut out, None, value, 0);
    out
}

fn render_read_node(out: &mut String, key: Option<&str>, value: &Value, depth: usize) {
    match value {
        Value::Object(obj) if !obj.is_empty() => {
            let d = key.map_or(depth, |k| { indent(out, depth); out.push_str(&format!("- **{}**\n", escape_label(k))); depth + 1 });
            let (lang, mut deferred) = (infer_read_language(obj), Vec::new());
            for (k, child) in ordered_fields(obj) {
                if matches!(k, "content" | "source") { if let Some(code) = child.as_str() { deferred.push((k, code)); continue; } }
                render_read_node(out, Some(k), child, d);
            }
            for (k, code) in deferred { indent(out, d); out.push_str(&format!("- **{}**:\n", escape_label(k))); append_fenced_block(out, lang, code, 0); }
        }
        Value::Array(items) if !items.is_empty() && key.is_some() => {
            indent(out, depth);
            out.push_str(&format!("- **{}**\n", escape_label(key.unwrap_or(""))));
            for (i, item) in items.iter().enumerate() { render_read_node(out, Some(&format!("[{i}]")), item, depth + 1); }
        }
        _ => match key { Some(k) => render_field(out, k, value, depth), None => render_markdown_fields(out, value, depth, &[]) },
    }
}

fn infer_read_language(object: &Map<String, Value>) -> &str {
    if object.get("mode").and_then(Value::as_str) == Some("delta") { return "diff"; }
    if let Some(lang) = object.get("node").and_then(|n| text_field(n, "language")) { return lang; }
    let path = object.get("path").and_then(Value::as_str).or_else(|| object.get("node").and_then(|n| text_field(n, "path"))).unwrap_or("");
    if object.get("mode").and_then(Value::as_str) == Some("search") && path.contains(['*', '?', '[']) { return "text"; }
    match path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("rs") => "rust", Some("py") => "python", Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript", Some("md" | "markdown") => "markdown",
        Some("yaml" | "yml") => "yaml", Some("json") => "json", Some("toml") => "toml",
        Some("sh" | "bash") => "bash", Some("sql") => "sql", _ => "text",
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

fn ordered_fields(object: &Map<String, Value>) -> Vec<(&str, &Value)> {
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

fn indent(out: &mut String, depth: usize) {
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

const KNOWN_PAGE_FIELDS: &[&str] = &[
    "components", "languages", "coverage_by_workspace", "nodes", "items", "matches",
    "documented_in", "governing_invariants", "analysis_gaps", "incoming", "outgoing", "calls",
    "callers", "implements", "implementors", "overrides", "dependencies", "unwired",
    "untested_public_surface", "doc_diagnostics", "referenced_symbol_candidates", "phantom_tested",
    "dead_tails", "dangling_calls",
];

fn has_continuation(object: &Map<String, Value>) -> bool {
    object.get("has_more") == Some(&Value::Bool(true)) || object.get("next_offset").is_some_and(|o| !o.is_null())
}

fn compact_navigation_page(page: &mut Map<String, Value>) {
    if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
        items.iter_mut().for_each(compact_navigation_item);
    }
    if let Some(groups) = page.get_mut("grouped_by_file").and_then(Value::as_object_mut) {
        for g in groups.values_mut().filter_map(Value::as_array_mut) { g.iter_mut().for_each(compact_navigation_item); }
    }
    if !is_paging_envelope(page) || has_continuation(page) { return; }
    if page_limited(page) {
        if let Some(Value::String(gen)) = page.get_mut("generation") { *gen = shorten_hash(gen).to_owned(); }
        return;
    }
    if empty_page(page) {
        for f in ["offset", "limit", "has_more", "generation", "next_offset", "continuation_hint"] { page.remove(f); }
        return;
    }
    page.remove("generation");
    page.remove("continuation_hint");
    if page.get("has_more") == Some(&Value::Bool(false)) { page.remove("next_offset"); }
}

fn empty_page(page: &Map<String, Value>) -> bool {
    !page_limited(page) && page.get("has_more") != Some(&Value::Bool(true))
        && (page.get("total").is_some_and(|t| t == 0) || page.get("items").and_then(Value::as_array).is_some_and(Vec::is_empty))
}

fn compact_navigation_item(value: &mut Value) {
    let Some(item) = value.as_object_mut() else { return; };
    if !["id", "kind", "name", "path"].iter().all(|k| item.get(*k).is_some_and(Value::is_string)) { return; }
    for flag in ["is_test", "generated"] {
        if let Some(v) = item.get_mut(flag) {
            if v == false || v == 0 { item.remove(flag); }
            else if v == true || v.as_i64().is_some_and(|n| n != 0) { *v = Value::Bool(true); }
        }
    }
    if item.get("qualname") == item.get("name") { item.remove("qualname"); }
}

fn is_paging_envelope(object: &Map<String, Value>) -> bool {
    object.contains_key("offset") && object.contains_key("limit") && object.get("generation").is_some_and(Value::is_string)
}

fn page_limited(page: &Map<String, Value>) -> bool {
    ["computation_truncated", "byte_limited"].iter().any(|f| page.get(*f) == Some(&Value::Bool(true)))
}

fn compact_symbol(object: &mut Map<String, Value>, explain: bool) {
    for key in ["documents", "coverage"] {
        if object.get(key).is_some_and(is_empty_collection) { object.remove(key); }
    }
    if explain {
        const RELATIONS: &[&str] = &["calls", "callers", "implements", "implementors", "overrides", "dependencies", "unwired", "incoming", "outgoing"];
        for key in RELATIONS { if object.get(*key).is_some_and(is_empty_collection) { object.remove(*key); } }
        let present: Vec<&str> = RELATIONS.iter().copied().filter(|k| object.contains_key(*k)).collect();
        object.insert("relations".into(), serde_json::json!(present));
    }
}

fn is_empty_collection(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty) || value.get("total").is_some_and(|t| t == 0)
        || (value.get("total").is_none() && value.get("items").and_then(Value::as_array).is_some_and(Vec::is_empty))
        || value.as_object().is_some_and(Map::is_empty)
}

fn compact_freshness(root: &mut Map<String, Value>) {
    let Some(f) = root.get_mut("freshness").and_then(Value::as_object_mut) else { return; };
    if !is_freshness_envelope(f, false) { return; }
    f.remove("checked_at_unix_ms");
    f.remove("inventory_scan_ms");
    if matches!(f.get("status").and_then(Value::as_str), Some("matched" | "source_inventory_matched"))
        && f.get("refresh") == Some(&Value::String("none".into()))
    {
        for field in ["output_root", "corpus_hash", "generation"] { f.remove(field); }
        f.insert("status".into(), Value::String("matched".into()));
    }
}

pub(super) fn is_freshness_envelope(object: &Map<String, Value>, allow_compacted: bool) -> bool {
    const ALLOWED: [&str; 7] = ["status", "output_root", "corpus_hash", "checked_at_unix_ms", "files_checked", "inventory_scan_ms", "refresh"];
    let source_id = object.get("output_root").is_some_and(Value::is_string) && object.get("corpus_hash").is_some_and(Value::is_string);
    let compacted = allow_compacted && object.get("status").and_then(Value::as_str) == Some("matched")
        && object.get("refresh").and_then(Value::as_str) == Some("none") && object.get("files_checked").and_then(Value::as_u64).is_some();
    object.get("status").is_some_and(Value::is_string) && object.get("refresh").is_some_and(Value::is_string)
        && object.keys().all(|k| ALLOWED.contains(&k.as_str())) && (source_id || compacted)
}

fn is_tool_metadata(object: &Map<String, Value>) -> bool {
    object.get("generation").is_some_and(Value::is_string) && object.iter().all(|(k, v)| match k.as_str() {
        "generation" | "status" | "refresh" | "output_root" | "corpus_hash" => v.is_string(),
        "files_checked" => v.as_u64().is_some(),
        _ => false,
    })
}

fn shorten_fields(object: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if let Some(Value::String(hash)) = object.get_mut(*field) { *hash = shorten_hash(hash).to_owned(); }
    }
}

fn move_owned_metadata_to_end(root: &mut Map<String, Value>) {
    let metadata = root.get("metadata").and_then(Value::as_object).is_some_and(is_tool_metadata).then(|| root.remove("metadata")).flatten();
    let (generation, freshness) = (root.remove("generation"), root.remove("freshness"));
    for (k, val) in [("metadata", metadata), ("generation", generation), ("freshness", freshness)] {
        if let Some(v) = val { root.insert(k.into(), v); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_keeps_all_values_and_marks_the_operation() {
        let rendered = render(&json!({"count":0,"complete":false,"items":["row",null]}), OutputFormat::Markdown, "code_map_query result");
        for needle in ["# code_map_query result", "**count**: `0`", "**complete**: `false`", "\"row\"", "`null`"] {
            assert!(rendered.contains(needle), "missing {needle}: {rendered}");
        }
        let ep = || json!({"total":0,"offset":0,"limit":10,"items":[]});
        let heading_cases = vec![
            ("code_map_explain", json!({"incoming":ep(),"outgoing":ep(),"documents":ep(),"affected_docs":ep(),"analysis_gaps":ep()}),
             vec!["Inbound relationships", "Outbound relationships", "Verified documentation", "Affected documents", "Analysis gaps"]),
            ("code_map_impact", json!({"nodes":ep(),"changed_symbols":ep(),"tests":ep(),"unmapped_changes":ep(),"affected_docs":ep()}),
             vec!["Reached nodes", "Changed symbols", "Affected tests", "Unmapped changes", "Affected documents"]),
            ("code_map_tests", json!({"nodes":ep()}), vec!["Related tests and dependencies"]),
            ("code_map_analyze", json!({"diagnostics":ep(),"untested_public_surface":ep(),"errors":ep(),"resolution":ep(),"external_imports":ep(),"doc_diagnostics":ep(),"top_files":ep(),"resolution_statuses":ep(),"resolution_causes":ep(),"parse_error_languages":ep()}),
             vec!["Syntax diagnostics", "Untested public surface", "Parse errors", "Unresolved and ambiguous references", "External references", "Documentation diagnostics", "Files with the most recorded issues", "Resolution statuses", "Resolution causes", "Parse errors by language"]),
        ];
        let mut failures = Vec::new();
        for (title, value, headings) in heading_cases {
            let md = render_for_operation(&value, OutputFormat::Markdown, title, None, OperationKind::Navigation);
            for h in headings { if md.contains(&format!("## {h}\n")) { failures.push(format!("{title} rendered empty heading {h}")); } }
            if md.contains("No indexed results in this page.") { failures.push(format!("{title} rendered empty notice")); }
        }
        let stub = render_for_operation(&json!({"incoming":{"total":3,"offset":0,"limit":3,"omitted":true}}), OutputFormat::Markdown, "code_map_explain", None, OperationKind::Navigation);
        if !stub.contains("## Inbound relationships\n") { failures.push("compact stub lost heading".into()); }
        let unres = render_presented(&json!({"nodes":{"total":2,"items":[{"name":"missing","path":"src/missing.rs","line":4},{"name":"unsafe","path":"../outside.rs","line":5}]}}), OutputFormat::Markdown, "code_map_search", Some(DiscoveryKind::Search));
        if unres.contains("(link unavailable)") || !unres.contains("src/missing.rs") || !unres.contains("../outside.rs") { failures.push("search unresolved link check failed".into()); }
        let unres_ins = render_presented(&json!({"node":{"qualname":"selected","path":"src/missing.rs","line":7},"governing_invariants":{"direct":[],"omitted_sources":[{"id":"INV-MISSING","source":{"path":"src/missing.rs","line":7}},{"id":"INV-UNSAFE","source":{"path":"../outside.rs"}}]}}), OutputFormat::Markdown, "code_map_inspect", Some(DiscoveryKind::Inspect));
        if unres_ins.contains("link unavailable") || !unres_ins.contains("src/missing.rs") || !unres_ins.contains("../outside.rs") { failures.push("inspect unresolved link check failed".into()); }
        let existing_uri = "file:///tmp/workspace/src/space%20dir/%23unicode-%CE%BB.rs#L9";
        let rendered_uri = render_presented(&json!({"nodes":{"total":1,"items":[{"name":"symbol","path":"src/space dir/#unicode-λ.rs","line":9,"uri":existing_uri}]}}), OutputFormat::Markdown, "code_map_search", Some(DiscoveryKind::Search));
        if !rendered_uri.contains(existing_uri) || rendered_uri.contains("%2520") || rendered_uri.contains("%2523") { failures.push("pre-attached URI failed".into()); }
        assert!(failures.is_empty(), "formatter contract failures: {failures:#?}");
    }

    #[test]
    fn opaque_operations_are_never_compacted_by_field_name() {
        let value = json!({"generation":"opaque","metadata":{"generation":"user"},"items":[],"complete":false});
        for op in [OperationKind::Sql, OperationKind::Task, OperationKind::Guide, OperationKind::General] {
            let mut actual = value.clone();
            compact(&mut actual, op, None);
            assert_eq!(actual, value);
        }
    }

    #[test]
    fn navigation_compaction_keeps_active_tokens_and_false_or_zero() {
        let mut value = json!({
            "count":0,"complete":false,
            "nodes":{"total":1,"offset":0,"limit":1,"has_more":false,"next_offset":null,"generation":"inactive-token","items":[{"name":"x"}]},
            "outgoing":{"total":2,"offset":0,"limit":1,"has_more":true,"next_offset":1,"generation":"active-token","items":[{"name":"y"}]},
            "freshness":{"status":"source_inventory_matched","output_root":"display-root","corpus_hash":"display-hash","refresh":"none"}
        });
        compact(&mut value, OperationKind::Navigation, None);
        assert!(value.get("generation").is_none());
        assert!(value["nodes"].get("generation").is_none());
        assert_eq!(value["outgoing"]["generation"], "active-token");
        assert_eq!(value["count"], 0);
        assert_eq!(value["complete"], false);
        assert!(value["freshness"].get("output_root").is_none());
    }
}
