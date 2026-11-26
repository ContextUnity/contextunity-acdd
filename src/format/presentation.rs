use super::coordination;
use super::core::{
    append_fenced_block, is_freshness_envelope, ordered_fields, render_field, render_markdown,
    render_markdown_fields, shorten_hash, text_field, KNOWN_PAGE_FIELDS,
};
use super::core::{OperationKind, OutputFormat};
use super::discovery::{self, file_uri_for_path, DiscoveryKind};
use super::lint;
use super::traversal;
use acdd_core::config::{resolve_file_path, Adapter};
use acdd_core::fs::canonical_root;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Deserialize, Serialize)]
struct CompactRootDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_preview: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    freshness: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relations: Option<Vec<String>>,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct NavigationItemDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    qualname: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_test: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct NavigationPageDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    items: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grouped_by_file: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    has_more: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation_hint: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    computation_truncated: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_limited: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

impl NavigationPageDto {
    fn is_paging_envelope(&self) -> bool {
        self.offset.is_some()
            && self.limit.is_some()
            && self.generation.as_ref().is_some_and(Value::is_string)
    }

    fn has_continuation(&self) -> bool {
        self.has_more == Some(Value::Bool(true))
            || self.next_offset.as_ref().is_some_and(|o| !o.is_null())
    }

    fn page_limited(&self) -> bool {
        matches!(self.computation_truncated, Some(Value::Bool(true)))
            || matches!(self.byte_limited, Some(Value::Bool(true)))
    }

    fn empty_page(&self) -> bool {
        !self.page_limited()
            && self.has_more != Some(Value::Bool(true))
            && (self.total.as_ref().is_some_and(|t| t == 0)
                || self.items.as_ref().is_some_and(Vec::is_empty))
    }
}

#[derive(Deserialize, Serialize)]
struct FreshnessDto {
    status: String,
    refresh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_root: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corpus_hash: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked_at_unix_ms: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files_checked: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inventory_scan_ms: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct AcddReadEnvelopeDto {
    #[serde(rename = "Hints", default, skip_serializing)]
    hints: Option<Value>,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct GenerationNodeDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(flatten)]
    children: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct SourcePreviewDto {
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    body_lines: usize,
    #[serde(default)]
    leading_lines: usize,
    #[serde(default)]
    omitted_leading_lines: usize,
    #[serde(default)]
    omitted_body_lines: usize,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    truncated_line: bool,
    #[serde(default)]
    next_source_offset: Option<usize>,
    #[serde(default)]
    continuation_hint: Option<String>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct SourceCandidateDto {
    #[serde(default)]
    source: String,
    source_preview: SourcePreviewDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header: Option<String>,
    node: Value,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct TaskContinuationHintDto {
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    generation: String,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct TaskPaginationDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    returned: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_omitted: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    has_more: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation_hint: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct TasksPageDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pagination: Option<TaskPaginationDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tasks: Option<Vec<Value>>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct ItemsPageDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    has_more: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_offset: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    items: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    returned: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_omitted: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_limited: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_scope: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation_hint: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct SearchNodesEnvelopeDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    items: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grouped_by_file: Option<BTreeMap<String, Value>>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// Formats one resolved semantic value for the requested surface.
pub(crate) fn render(value: &Value, format: OutputFormat, title: &str) -> String {
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
pub(super) fn render_presented(
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
    if format == OutputFormat::Markdown
        && operation == OperationKind::Navigation
        && discovery.is_none()
    {
        if let Some(rendered) = super::analyze::render_if_analyze(value, title) {
            return rendered;
        }
        if let Some(kind) = traversal::kind_for(title, value) {
            return traversal::render(value, title, kind);
        }
    }
    render_presented(value, format, title, discovery)
}

/// Applies only presentation compaction permitted for a known operation.
pub fn compact(value: &mut Value, operation: OperationKind, explain: Option<bool>) {
    if operation != OperationKind::Navigation {
        return;
    }
    super::core::shorten_metadata_hashes(value);
    if !value.is_object() {
        return;
    }
    let Ok(mut root) = serde_json::from_value::<CompactRootDto>(value.clone()) else {
        return;
    };
    if let Some(explain) = explain {
        compact_symbol(&mut root, explain);
    }
    let source_cont = root
        .source_preview
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|p| {
            p.get("has_more") == Some(&Value::Bool(true))
                || p.get("next_source_offset")
                    .and_then(Value::as_u64)
                    .is_some_and(|o| o > 0)
        });
    let (mut nested_cont, mut saw_page, mut all_empty) = (false, false, true);
    for key in KNOWN_PAGE_FIELDS {
        let Some(page_val) = root.fields.get_mut(*key) else {
            continue;
        };
        if !page_val.is_object() {
            continue;
        }
        let Ok(mut page) = serde_json::from_value::<NavigationPageDto>(page_val.clone()) else {
            continue;
        };
        if page.is_paging_envelope() {
            saw_page = true;
            all_empty &= page.empty_page();
            nested_cont |= page.has_continuation();
        }
        compact_navigation_page(&mut page);
        if let Ok(encoded) = serde_json::to_value(page) {
            *page_val = encoded;
        }
    }
    if !source_cont && !nested_cont && !has_root_continuation(&root.fields) {
        root.generation = None;
    } else if let Some(Value::String(gen)) = &mut root.generation {
        if is_hex_hash(gen) {
            *gen = shorten_hash(gen).to_owned();
        }
    }
    compact_freshness(&mut root.freshness);
    if saw_page
        && all_empty
        && !source_cont
        && root.freshness.as_ref().is_some_and(is_noop_freshness)
    {
        root.freshness = None;
    }
    if let Ok(encoded) = serde_json::to_value(root) {
        *value = encoded;
    }
}

fn is_hex_hash(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_noop_freshness(v: &Value) -> bool {
    v.as_object().is_some_and(|f| {
        matches!(
            f.get("status").and_then(Value::as_str),
            Some("matched" | "source_inventory_matched")
        ) && f.get("refresh").and_then(Value::as_str) == Some("none")
    })
}

fn compact_for_format(value: &mut Value, opts: PresentationOptions<'_>) {
    compact(value, opts.operation, opts.explain);
    if opts.output_format == OutputFormat::Markdown
        && opts.operation == OperationKind::Navigation
        && value.get("freshness").is_some_and(is_noop_freshness)
    {
        if let Some(obj) = value.as_object_mut() {
            obj.remove("freshness");
        }
    }
}

fn render_acdd_read(value: &Value) -> String {
    let Some(obj) = value.as_object() else {
        let mut out = String::from("# acdd_read\n");
        render_markdown_fields(&mut out, value, 0, &[]);
        return out;
    };
    let mut out = if let Some(header) = text_field(value, "header") {
        format!("# acdd_read — {}\n", super::core::escape_text(header))
    } else if let Some(path) =
        text_field(value, "path").or_else(|| obj.get("node").and_then(|n| text_field(n, "path")))
    {
        let mode = text_field(value, "mode")
            .map(|m| format!(" (`{m}`)"))
            .unwrap_or_default();
        format!("# acdd_read — `{path}`{mode}\n")
    } else {
        String::from("# acdd_read\n")
    };
    let mut meta = Vec::new();
    if let Some(scope) = text_field(value, "in_write_scope") {
        meta.push(format!("scope=`{scope}`"));
    }
    if let Some(digest) = text_field(value, "content_digest") {
        meta.push(format!("digest=`{}`", shorten_hash(digest)));
    }
    if let Some(pulse) = obj.get("graph_pulse").and_then(Value::as_object) {
        let parts: Vec<String> = ["callers", "edges", "invariants"]
            .into_iter()
            .filter_map(|k| {
                pulse
                    .get(k)
                    .and_then(Value::as_u64)
                    .filter(|&n| n > 0)
                    .map(|n| format!("{n} {k}"))
            })
            .collect();
        if !parts.is_empty() {
            meta.push(format!("pulse={}", parts.join(", ")));
        }
    }
    if obj.get("truncated") == Some(&Value::Bool(true)) {
        meta.push("truncated=true".to_owned());
    }
    if let Some(next_start) = obj.get("next_start_line").and_then(Value::as_u64) {
        meta.push(format!("next_start_line={next_start}"));
    }
    if let Some(next_path) = obj.get("next_path_index").and_then(Value::as_u64) {
        meta.push(format!("next_path_index={next_path}"));
    }
    if let Some(p) = obj.get("source_preview").and_then(Value::as_object) {
        if let (Some(s), Some(e)) = (
            p.get("start_line").and_then(Value::as_u64),
            p.get("end_line").and_then(Value::as_u64),
        ) {
            meta.push(format!("lines={s}-{e}"));
        }
        if let Some(bo) = p
            .get("body_offset")
            .and_then(Value::as_u64)
            .filter(|&n| n > 0)
        {
            meta.push(format!("**body_offset**: `{bo}`"));
        }
        if p.get("has_more") == Some(&Value::Bool(true)) {
            let bl = p.get("body_lines").and_then(Value::as_u64).unwrap_or(0);
            let tbl = p
                .get("total_body_lines")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let next = p
                .get("next_source_offset")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let gen = text_field(value, "generation")
                .map(|g| if is_hex_hash(g) { shorten_hash(g) } else { g })
                .unwrap_or("unavailable");
            meta.push(format!("**body_lines**: `{bl}` | **total_body_lines**: `{tbl}` | **next_source_offset**: `{next}` | **generation**: `\"{gen}\"`"));
        }
    }
    if !meta.is_empty() {
        out.push_str(&format!("{}\n", meta.join(" | ")));
    }
    const CONSUMED: &[&str] = &[
        "mode",
        "path",
        "header",
        "in_write_scope",
        "content_digest",
        "graph_pulse",
        "truncated",
        "next_start_line",
        "next_path_index",
        "source_preview",
        "node",
        "generation",
        "content",
        "source",
        "footer",
    ];
    for (k, child) in ordered_fields(obj) {
        if !CONSUMED.contains(&k) {
            render_field(&mut out, k, child, 0);
        }
    }
    let lang = infer_read_language(obj);
    for k in ["content", "source"] {
        if let Some(code) = obj.get(k).and_then(Value::as_str) {
            append_fenced_block(&mut out, lang, code, 0);
        }
    }
    if let Some(footer) = text_field(value, "footer") {
        out.push_str(&format!("{footer}\n"));
    }
    out
}

fn infer_read_language(object: &Map<String, Value>) -> &str {
    if object.get("mode").and_then(Value::as_str) == Some("delta") {
        return "diff";
    }
    if let Some(lang) = object.get("node").and_then(|n| text_field(n, "language")) {
        return lang;
    }
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| object.get("node").and_then(|n| text_field(n, "path")))
        .unwrap_or("");
    if object.get("mode").and_then(Value::as_str) == Some("search")
        && path.contains(['*', '?', '['])
    {
        return "text";
    }
    match path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("md" | "markdown") => "markdown",
        Some("yaml" | "yml") => "yaml",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("sh" | "bash") => "bash",
        Some("sql") => "sql",
        _ => "text",
    }
}

fn has_root_continuation(fields: &BTreeMap<String, Value>) -> bool {
    fields.get("has_more") == Some(&Value::Bool(true))
        || fields.get("next_offset").is_some_and(|o| !o.is_null())
}

fn compact_navigation_page(page: &mut NavigationPageDto) {
    if let Some(items) = &mut page.items {
        for item in items {
            compact_navigation_item(item);
        }
    }
    if let Some(groups) = &mut page.grouped_by_file {
        for g in groups.values_mut().filter_map(Value::as_array_mut) {
            for item in g {
                compact_navigation_item(item);
            }
        }
    }
    if !page.is_paging_envelope() {
        return;
    }
    if page.has_continuation() {
        if let Some(Value::String(gen)) = &mut page.generation {
            if is_hex_hash(gen) {
                let short = shorten_hash(gen).to_owned();
                if let Some(Value::String(hint)) = &mut page.continuation_hint {
                    *hint = hint.replace(gen.as_str(), &short);
                }
                *gen = short;
            }
        }
        return;
    }
    if page.page_limited() {
        if let Some(Value::String(gen)) = &mut page.generation {
            *gen = shorten_hash(gen).to_owned();
        }
        return;
    }
    if page.empty_page() {
        page.offset = None;
        page.limit = None;
        page.has_more = None;
        page.generation = None;
        page.next_offset = None;
        page.continuation_hint = None;
        return;
    }
    page.generation = None;
    page.continuation_hint = None;
    if page.has_more == Some(Value::Bool(false)) {
        page.next_offset = None;
    }
}

fn compact_navigation_item(value: &mut Value) {
    if !value.is_object() {
        return;
    }
    let Ok(mut item) = serde_json::from_value::<NavigationItemDto>(value.clone()) else {
        return;
    };
    if ![&item.id, &item.kind, &item.name, &item.path]
        .iter()
        .all(|v| v.as_ref().is_some_and(Value::is_string))
    {
        return;
    }
    for flag in [&mut item.is_test, &mut item.generated] {
        if let Some(v) = flag {
            if *v == false || *v == 0 {
                *flag = None;
            } else if *v == true || v.as_i64().is_some_and(|n| n != 0) {
                *v = Value::Bool(true);
            }
        }
    }
    if item.qualname.is_some() && item.qualname == item.name {
        item.qualname = None;
    }
    if let Ok(encoded) = serde_json::to_value(item) {
        *value = encoded;
    }
}

fn compact_symbol(root: &mut CompactRootDto, explain: bool) {
    for key in ["documents", "coverage"] {
        if matches!(root.fields.get(key), Some(v) if is_empty_collection(v)) {
            root.fields.remove(key);
        }
    }
    if explain {
        const RELATIONS: &[&str] = &[
            "calls",
            "callers",
            "implements",
            "implementors",
            "overrides",
            "dependencies",
            "unwired",
            "incoming",
            "outgoing",
        ];
        for key in RELATIONS {
            if matches!(root.fields.get(*key), Some(v) if is_empty_collection(v)) {
                root.fields.remove(*key);
            }
        }
        let present: Vec<String> = RELATIONS
            .iter()
            .copied()
            .filter(|k| root.fields.contains_key(*k))
            .map(str::to_owned)
            .collect();
        root.relations = Some(present);
    }
}

fn is_empty_collection(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
        || value.get("total").is_some_and(|t| t == 0)
        || (value.get("total").is_none()
            && value
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty))
        || value
            .get("items")
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
}

fn compact_freshness(freshness: &mut Option<Value>) {
    let Some(value) = freshness else {
        return;
    };
    if !value
        .as_object()
        .is_some_and(|f| is_freshness_envelope(f, false))
    {
        return;
    }
    let Ok(mut dto) = serde_json::from_value::<FreshnessDto>(value.clone()) else {
        return;
    };
    dto.checked_at_unix_ms = None;
    dto.inventory_scan_ms = None;
    if matches!(dto.status.as_str(), "matched" | "source_inventory_matched")
        && dto.refresh == "none"
    {
        dto.output_root = None;
        dto.corpus_hash = None;
        dto.generation = None;
        dto.status = "matched".to_owned();
    }
    if let Ok(encoded) = serde_json::to_value(dto) {
        *value = encoded;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PresentationOptions<'a> {
    pub(crate) output_format: OutputFormat,
    pub(crate) operation: OperationKind,
    pub(crate) title: &'a str,
    pub(crate) explain: Option<bool>,
    pub(crate) discovery: Option<DiscoveryKind>,
}

pub(crate) enum BoundedPresentation {
    Ok(String),
    AstDiagnostic(String),
    Oversized(String),
}

pub(crate) fn render_with_acdd_read_hints(value: &Value, opts: PresentationOptions<'_>) -> String {
    if opts.title == "acdd_read" && opts.output_format == OutputFormat::Markdown {
        let parsed = value
            .is_object()
            .then(|| serde_json::from_value::<AcddReadEnvelopeDto>(value.clone()).ok())
            .flatten();
        let (body, hints) = match parsed {
            Some(dto) => (
                serde_json::to_value(&dto).unwrap_or_else(|_| value.clone()),
                dto.hints,
            ),
            None => (value.clone(), None),
        };
        let mut rendered = render_for_operation(
            &body,
            opts.output_format,
            opts.title,
            opts.discovery,
            opts.operation,
        );
        if let Some(hints) = hints.and_then(|value| value.as_str().map(str::to_owned)) {
            rendered.push_str("\nHints: ");
            rendered.push_str(&hints.replace('\n', "\n       "));
            rendered.push('\n');
        }
        rendered
    } else {
        render_for_operation(
            value,
            opts.output_format,
            opts.title,
            opts.discovery,
            opts.operation,
        )
    }
}

fn render_fitting_budget(
    value: &mut Value,
    opts: PresentationOptions<'_>,
    budget: usize,
    measure_bytes: &dyn Fn(&str) -> usize,
) -> Option<String> {
    compact_for_format(value, opts);
    let rendered = render_with_acdd_read_hints(value, opts);
    (measure_bytes(&rendered) <= budget).then_some(rendered)
}

pub(crate) fn present_value_bounded(
    mut value: Value,
    budget: usize,
    opts: PresentationOptions<'_>,
    measure_bytes: &dyn Fn(&str) -> usize,
) -> BoundedPresentation {
    if opts.discovery == Some(DiscoveryKind::Ast) && value.get("diagnostic").is_some() {
        let diagnostic = &value["diagnostic"];
        let rendered = match opts.output_format {
            OutputFormat::Markdown => render_with_acdd_read_hints(&value, opts),
            OutputFormat::Json => discovery::ast_legacy_error(diagnostic),
        };
        return if measure_bytes(&rendered) <= budget {
            BoundedPresentation::AstDiagnostic(rendered)
        } else {
            BoundedPresentation::Oversized(
                "AST diagnostic exceeds the response byte limit; reduce the pattern or use JSON output."
                    .to_owned(),
            )
        };
    }

    let saved_generations = GenerationSnapshot::capture(&value);
    if let Some(rendered) = render_fitting_budget(&mut value, opts, budget, measure_bytes) {
        return BoundedPresentation::Ok(rendered);
    }

    if let Some(snapshot) = &saved_generations {
        snapshot.restore(&mut value);
    }
    if opts.title == "acdd_read" {
        let trimmed = trim_snippet_source_to_budget(&mut value, opts, budget, measure_bytes);
        if !trimmed {
            if value.get("source_preview").is_some() {
                return BoundedPresentation::Oversized(
                    "Snippet line exceeds the serialized response byte budget; use a larger response budget or read the local source file. No partial line was returned."
                        .to_owned(),
                );
            }
            return BoundedPresentation::Oversized(oversized_response_message(
                &value,
                opts,
                budget,
                measure_bytes,
            ));
        }
        return if let Some(rendered) =
            render_fitting_budget(&mut value, opts, budget, measure_bytes)
        {
            BoundedPresentation::Ok(rendered)
        } else {
            BoundedPresentation::Oversized(oversized_response_message(
                &value,
                opts,
                budget,
                measure_bytes,
            ))
        };
    }

    let allow_tasks = opts.operation == OperationKind::Navigation && value.get("tasks").is_some();
    let allow_items = matches!(
        opts.operation,
        OperationKind::Navigation | OperationKind::Sql
    );
    while trim_page_tails(&mut value, allow_tasks, allow_items) {
        if let Some(candidate) = render_fitting_budget(&mut value, opts, budget, measure_bytes) {
            return BoundedPresentation::Ok(candidate);
        }
        if let Some(snapshot) = &saved_generations {
            snapshot.restore(&mut value);
        }
    }
    compact(&mut value, opts.operation, opts.explain);
    BoundedPresentation::Oversized(oversized_response_message(
        &value,
        opts,
        budget,
        measure_bytes,
    ))
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
        if !value.is_object() {
            return;
        }
        let Ok(mut node) = serde_json::from_value::<GenerationNodeDto>(value.clone()) else {
            return;
        };
        if let Some(gen) = &self.generation {
            node.generation = Some(gen.clone());
        }
        for (key, snap) in &self.children {
            if let Some(child) = node.children.get_mut(key) {
                snap.restore(child);
            }
        }
        if let Ok(encoded) = serde_json::to_value(node) {
            *value = encoded;
        }
    }
}

fn oversized_response_message(
    value: &Value,
    opts: PresentationOptions<'_>,
    budget: usize,
    measure_bytes: &dyn Fn(&str) -> usize,
) -> String {
    let is_task_list = opts.operation == OperationKind::Navigation && value.get("tasks").is_some();
    if is_task_list {
        if value
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(|tasks| tasks.len() == 1)
        {
            let mut metadata = value.clone();
            metadata["tasks"] = json!([]);
            compact_for_format(&mut metadata, opts);
            let rendered = render_with_acdd_read_hints(&metadata, opts);
            if measure_bytes(&rendered) <= budget {
                "One complete task record exceeds the configured byte limit. Request compact task details, a smaller page, or a narrower filter; no partial task or continuation was returned."
                    .to_owned()
            } else {
                "Required task-list pagination metadata exceeds the configured byte limit. Narrow the workspace or filters; no partial task or continuation was returned."
                    .to_owned()
            }
        } else {
            "Task-list pagination metadata exceeds the configured byte limit. Narrow the workspace or filters; no partial task or continuation was returned."
                .to_owned()
        }
    } else {
        "Output exceeds the response byte limit because a complete item cannot fit. Use detail='compact', a smaller page, a narrower filter, or read the oversized record from its source; no partial item was returned and no continuation was consumed."
            .to_owned()
    }
}

fn trim_snippet_source_to_budget(
    value: &mut Value,
    opts: PresentationOptions<'_>,
    budget: usize,
    measure_bytes: &dyn Fn(&str) -> usize,
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
        compact_for_format(candidate, opts);
        let rendered = render_with_acdd_read_hints(candidate, opts);
        measure_bytes(&rendered) <= budget
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
    let mut candidate: SourceCandidateDto = serde_json::from_value(original.clone()).ok()?;
    candidate.source = format!("{leading}{body}");
    candidate.source_preview.start_line = start_line_value;
    candidate.source_preview.end_line = end_line;
    candidate.source_preview.body_lines = body_lines;
    candidate.source_preview.leading_lines = leading_lines;
    candidate.source_preview.omitted_leading_lines = omitted_leading;
    candidate.source_preview.omitted_body_lines = page.total_body_lines.saturating_sub(next);
    candidate.source_preview.has_more = more;
    candidate.source_preview.truncated_line = false;
    candidate.source_preview.next_source_offset = more.then_some(next);
    candidate.source_preview.continuation_hint = hint;
    if let (Some(path), Some(start), Some(end)) = (
        candidate.node["path"].as_str(),
        start_line_value.and_then(|line| u64::try_from(line).ok()),
        end_line.and_then(|line| u64::try_from(line).ok()),
    ) {
        candidate.header = Some(format!(
            "{} {} in {path}:{start}-{end}",
            candidate.node["kind"].as_str().unwrap_or("symbol"),
            candidate.node["qualname"]
                .as_str()
                .or_else(|| candidate.node["name"].as_str())
                .unwrap_or("symbol")
        ));
    } else {
        candidate.header = None;
    }
    serde_json::to_value(candidate).ok()
}

fn update_trimmed_page_tail_fields<T>(
    items_slot: &mut Option<Vec<T>>,
    [returned_slot, byte_omitted_slot, has_more_slot, next_offset_slot]: [&mut Option<Value>; 4],
    offset: u64,
    total: Option<u64>,
    fallback_has_more: bool,
) -> Option<(u64, bool)> {
    let items = items_slot.as_mut()?;
    if items.len() <= 1 {
        return None;
    }
    items.pop();
    let returned = items.len() as u64;
    let next = offset.saturating_add(returned);
    let has_more = total.map_or(fallback_has_more, |total| next < total);
    let omitted = byte_omitted_slot
        .as_ref()
        .and_then(Value::as_u64)
        .unwrap_or(0);
    *returned_slot = Some(json!(returned));
    *byte_omitted_slot = Some(json!(omitted.saturating_add(1)));
    *has_more_slot = Some(json!(has_more));
    *next_offset_slot = has_more.then(|| json!(next));
    Some((next, has_more))
}

fn trim_tasks_page_tail(value: &mut Value) -> bool {
    if !value.is_object() {
        return false;
    }
    let Ok(mut page_dto) = serde_json::from_value::<TasksPageDto>(value.clone()) else {
        return false;
    };
    let Some(pagination) = page_dto.pagination.as_mut() else {
        return false;
    };
    let page_is_continuable = pagination.total.as_ref().and_then(Value::as_u64).is_some()
        && pagination.offset.as_ref().and_then(Value::as_u64).is_some()
        && pagination
            .generation
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|generation| !generation.is_empty());
    if !page_is_continuable {
        return false;
    }
    let offset = pagination
        .offset
        .as_ref()
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = pagination.total.as_ref().and_then(Value::as_u64);
    let Some((next, has_more)) = update_trimmed_page_tail_fields(
        &mut page_dto.tasks,
        [
            &mut pagination.returned,
            &mut pagination.byte_omitted,
            &mut pagination.has_more,
            &mut pagination.next_offset,
        ],
        offset,
        total,
        false,
    ) else {
        return false;
    };
    let filters = pagination.filters.clone().unwrap_or(Value::Null);
    let generation = pagination
        .generation
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if has_more {
        if let Some(hint_val) = pagination
            .continuation_hint
            .as_ref()
            .filter(|hint| hint.is_object())
        {
            if let Ok(mut hint) =
                serde_json::from_value::<TaskContinuationHintDto>(hint_val.clone())
            {
                hint.offset = next;
                hint.generation = generation;
                pagination.continuation_hint = serde_json::to_value(hint).ok();
            }
        } else {
            pagination.continuation_hint = Some(json!(format!(
                "Repeat the same tool with the same filters ({filters}) and generation={generation}, offset={next}."
            )));
        }
    } else {
        pagination.continuation_hint = Some(json!(format!(
            "No next page. Repeat the same tool with the same filters ({filters}) at offset=0 to refresh; generation={generation}."
        )));
    }
    if let Ok(encoded) = serde_json::to_value(page_dto) {
        *value = encoded;
    }
    true
}

fn trim_items_page_tail(value: &mut Value) -> bool {
    if !value.is_object() {
        return false;
    }
    let Ok(mut page) = serde_json::from_value::<ItemsPageDto>(value.clone()) else {
        return false;
    };
    let total = page.total.as_ref().and_then(Value::as_u64);
    let offset = page.offset.as_ref().and_then(Value::as_u64);
    let generation = page
        .generation
        .as_ref()
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
        .map(str::to_owned);
    let has_more = page.has_more.as_ref().and_then(Value::as_bool) == Some(true);
    let has_usable_cursor = page.next_offset.as_ref().and_then(Value::as_u64).is_some();
    if offset.is_none()
        || generation.is_none()
        || (total.is_none() && !(has_more && has_usable_cursor))
    {
        return false;
    }
    let items_tail_offset = offset.unwrap_or_default();
    let mut items = page.items.take();
    let Some((next, has_more)) = update_trimmed_page_tail_fields(
        &mut items,
        [
            &mut page.returned,
            &mut page.byte_omitted,
            &mut page.has_more,
            &mut page.next_offset,
        ],
        items_tail_offset,
        total,
        has_more,
    ) else {
        return false;
    };
    page.items = items;
    if has_more {
        page.byte_limited = Some(json!(true));
        let generation = generation.as_deref().unwrap_or_default();
        let generation = if is_hex_hash(generation) {
            shorten_hash(generation)
        } else {
            generation
        };
        let filters = page
            .filters
            .as_ref()
            .or(page.request_scope.as_ref())
            .or_else(|| {
                page.continuation_hint
                    .as_ref()
                    .filter(|hint| hint.is_object())
            })
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .unwrap_or_else(|| "the original request filters".to_owned());
        page.continuation_hint = Some(json!(format!("Repeat the same tool and filters ({filters}) with generation={generation} and offset={next}; whole items after this offset were omitted to respect max_output_bytes.")));
    } else {
        page.continuation_hint = None;
    }
    if let Ok(encoded) = serde_json::to_value(page) {
        *value = encoded;
    }
    true
}

fn trim_page_tails(value: &mut Value, allow_tasks: bool, allow_items: bool) -> bool {
    if !value.is_object() {
        return false;
    }
    if allow_tasks && trim_tasks_page_tail(value) {
        return true;
    }
    if allow_tasks && value.get("tasks").is_some() {
        return false;
    }
    if allow_items && trim_items_page_tail(value) {
        return true;
    }
    if allow_items && value.get("items").is_some() {
        return false;
    }
    let Ok(mut children) = serde_json::from_value::<BTreeMap<String, Value>>(value.clone()) else {
        return false;
    };
    for child in children.values_mut() {
        if trim_page_tails(child, allow_tasks, allow_items) {
            if let Ok(encoded) = serde_json::to_value(children) {
                *value = encoded;
            }
            return true;
        }
    }
    false
}

fn attach_manifest_uris(root: &Path, adapter: &Adapter, page: &mut Value) {
    let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let workspace = item["workspace"].as_str().unwrap_or("primary").to_owned();
        if let Some(relative) = item["root"].as_str() {
            if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative) {
                if let Some(uri) = file_uri_for_path(&absolute, None) {
                    item["root_uri"] = json!(uri);
                }
            }
        }
        if let Some(relative) = item["manifest_path"].as_str() {
            if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative) {
                if let Some(uri) = file_uri_for_path(&absolute, None) {
                    item["manifest_uri"] = json!(uri);
                }
            }
        }
    }
}

fn resolve_component_path(
    root: &Path,
    adapter: &Adapter,
    workspace: &str,
    relative: &str,
) -> Option<std::path::PathBuf> {
    if relative.is_empty() || relative == "." {
        if workspace == "primary" {
            return canonical_root(root).ok();
        }
        return adapter
            .linked_workspaces
            .iter()
            .find(|linked| linked.name == workspace)
            .and_then(|linked| canonical_root(&linked.path).ok());
    }
    let selector = if workspace == "primary" {
        relative.to_owned()
    } else {
        format!("[{workspace}]/{relative}")
    };
    resolve_file_path(root, adapter, &selector).ok()
}

fn attach_document_uris(root: &Path, adapter: &Adapter, page: &mut Value) {
    let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let Some(path) = item.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Ok(absolute) = resolve_file_path(root, adapter, path) else {
            continue;
        };
        let line = item
            .get("line")
            .and_then(Value::as_i64)
            .and_then(|line| usize::try_from(line).ok());
        if let Some(uri) = file_uri_for_path(&absolute, line) {
            item["uri"] = json!(uri);
        }
    }
}

/// Adds workspace-resolved URIs to documentation coordinates before Markdown rendering.
pub(crate) fn attach_document_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
    title: &str,
) {
    match title {
        "search_docs" => attach_page_uris(root, adapter, value.pointer_mut("/sections")),
        "get_doc" => {
            for field in ["sections", "referenced_symbols"] {
                attach_page_uris(root, adapter, value.get_mut(field));
            }
            for field in ["cites_doc", "cited_by_doc"] {
                attach_page_uris_without_line(root, adapter, value.get_mut(field));
            }
            if let Some(items) = value
                .pointer_mut("/referenced_symbol_candidates/items")
                .and_then(Value::as_array_mut)
            {
                for candidate in items {
                    for entity in ["source", "target"] {
                        if let Some(entity) = candidate.get_mut(entity) {
                            attach_uri_from_fields(root, adapter, entity, "path", "line");
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn attach_page_uris(root: &Path, adapter: &Adapter, page: Option<&mut Value>) {
    let Some(items) = page
        .and_then(|page| page.get_mut("items"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
    }
}

fn attach_page_uris_without_line(root: &Path, adapter: &Adapter, page: Option<&mut Value>) {
    let Some(items) = page
        .and_then(|page| page.get_mut("items"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        let Some(path) = item.get("path").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let Ok(absolute) = resolve_file_path(root, adapter, &path) else {
            continue;
        };
        if let Some(uri) = file_uri_for_path(&absolute, None) {
            item["uri"] = json!(uri);
        }
    }
}

/// Adds absolute coordinates for Markdown presentation after canonical path resolution.
pub(crate) fn attach_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
    kind: DiscoveryKind,
) {
    match kind {
        DiscoveryKind::Overview => {
            if !value.get("workspace_root").is_some_and(Value::is_string) {
                value["workspace_root"] = json!(root.display().to_string());
            }
            if let Some(manifests) = value.pointer_mut("/discovery/manifest_components") {
                attach_manifest_uris(root, adapter, manifests);
            }
            if let Some(hubs) = value.pointer_mut("/discovery/documentation_hubs") {
                attach_document_uris(root, adapter, hubs);
            }
            for key in [
                "/discovery/active_runs/items",
                "/discovery/active_milestones/items",
            ] {
                if let Some(items) = value.pointer_mut(key).and_then(Value::as_array_mut) {
                    for item in items {
                        if let Some(path) = item
                            .get("spec_path")
                            .or_else(|| item.get("path"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                        {
                            attach_uri(root, adapter, item, &path, "line");
                        }
                    }
                }
            }
        }
        DiscoveryKind::Inspect => attach_inspect_uris(root, adapter, value),
        DiscoveryKind::Search => attach_search_uris(root, adapter, value),
        DiscoveryKind::Ast => attach_ast_uris(root, adapter, value),
    }
}

/// Adds workspace-resolved URIs to lint violation coordinates before Markdown rendering.
pub(crate) fn attach_lint_presentation_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
        if let Some(rule_source) = item.get_mut("rule_source") {
            attach_uri_from_fields(root, adapter, rule_source, "path", "line");
        }
    }
}

fn attach_array_items_uris(
    root: &Path,
    adapter: &Adapter,
    parent: Option<&mut Value>,
    array_key: &str,
    nested_key: Option<&str>,
    line_key: &str,
) {
    let Some(items) = parent
        .and_then(|value| value.get_mut(array_key))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        let target = match nested_key {
            Some(key) => item.get_mut(key),
            None => Some(item),
        };
        if let Some(target) = target {
            attach_uri_from_fields(root, adapter, target, "path", line_key);
        }
    }
}

/// Adds guarded file links for supplied traversal coordinates in Markdown output.
pub(crate) fn attach_traversal_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
) {
    for field in ["selector", "node"] {
        if let Some(entry) = value.get_mut(field) {
            attach_uri_from_fields(root, adapter, entry, "path", "line");
        }
    }
    for field in [
        "nodes",
        "changed_symbols",
        "tests",
        "unmapped_changes",
        "analysis_gaps",
        "untested_public_surface",
        "errors",
        "resolution",
        "external_imports",
        "doc_diagnostics",
        "diagnostics",
        "top_files",
        "resolution_statuses",
        "resolution_causes",
        "parse_error_languages",
        "dead_tails",
    ] {
        if let Some(page) = value.get_mut(field) {
            attach_traversal_page(root, adapter, page, "line");
        }
    }
    for (field, line_key) in [
        ("incoming", "line"),
        ("outgoing", "line"),
        ("affected_docs", "__no_line"),
        ("documents", "doc_line"),
    ] {
        attach_array_items_uris(root, adapter, value.get_mut(field), "items", None, line_key);
    }
    if let Some(page) = value.get_mut("phantom_tested") {
        attach_traversal_page(root, adapter, page, "line");
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                attach_array_items_uris(root, adapter, Some(item), "test_callers", None, "line");
            }
        }
    }
    if let Some(page) = value.get_mut("dangling_calls") {
        attach_traversal_page(root, adapter, page, "__no_line");
    }
    if let Some(invariants) = value.get_mut("governing_invariants") {
        for key in ["direct", "omitted_sources"] {
            attach_array_items_uris(root, adapter, Some(invariants), key, Some("source"), "line");
        }
    }
    attach_array_items_uris(
        root,
        adapter,
        value.get_mut("changes"),
        "files",
        None,
        "__no_line",
    );
    if let Some(calls) = value.get_mut("summary").and_then(|s| s.get_mut("calls")) {
        for direction in ["inbound", "outbound"] {
            attach_array_items_uris(
                root,
                adapter,
                calls.get_mut(direction),
                "preview",
                None,
                "line",
            );
        }
    }
}

fn attach_traversal_page(root: &Path, adapter: &Adapter, page: &mut Value, line: &str) {
    if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            attach_uri_from_fields(root, adapter, item, "path", line);
            if let Some(edge) = item.get_mut("edge") {
                attach_uri_from_fields(root, adapter, edge, "path", "line");
            }
            if let Some(path_nodes) = item
                .pointer_mut("/connection/verified_call_path_nodes")
                .and_then(Value::as_array_mut)
            {
                for node in path_nodes {
                    attach_uri_from_fields(root, adapter, node, "path", "line");
                }
            }
        }
    }
}

fn attach_inspect_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    attach_uri_from_fields(root, adapter, &mut value["node"], "path", "line");
    if let Some(summary) = value.get_mut("summary") {
        attach_uri_from_fields(root, adapter, &mut summary["container"], "path", "line");
        for direction in ["inbound", "outbound"] {
            if let Some(items) = summary["calls"][direction]["preview"].as_array_mut() {
                for item in items {
                    attach_uri_from_fields(root, adapter, item, "path", "line");
                }
            }
        }
    }
    for rule in value["governing_invariants"]["direct"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        attach_uri_from_fields(root, adapter, &mut rule["source"], "path", "line");
    }
    for source in value["governing_invariants"]["omitted_sources"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        attach_uri_from_fields(root, adapter, &mut source["source"], "path", "line");
    }
    for (field, line) in [
        ("documented_in", "doc_line"),
        ("analysis_gaps", "line"),
        ("documents", "doc_line"),
    ] {
        if let Some(items) = value[field]["items"].as_array_mut() {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", line);
            }
        }
    }
}

fn attach_search_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    if !value["nodes"].is_object() {
        return;
    }
    let Ok(mut nodes) = serde_json::from_value::<SearchNodesEnvelopeDto>(value["nodes"].clone())
    else {
        return;
    };
    if let Some(items) = &mut nodes.items {
        for item in items {
            attach_uri_from_fields(root, adapter, item, "path", "line");
            attach_search_pulse_uris(root, adapter, item);
        }
    }
    if let Some(groups) = &mut nodes.grouped_by_file {
        for (path, items) in groups {
            if let Some(items) = items.as_array_mut() {
                for item in items {
                    attach_uri(root, adapter, item, path, "line");
                    attach_search_pulse_uris(root, adapter, item);
                }
            }
        }
    }
    if let Ok(encoded) = serde_json::to_value(nodes) {
        value["nodes"] = encoded;
    }
}

fn attach_search_pulse_uris(root: &Path, adapter: &Adapter, item: &mut Value) {
    if let Some(callers) = item["graph_pulse"]["callers"]["preview"].as_array_mut() {
        for caller in callers {
            attach_uri_from_fields(root, adapter, caller, "path", "line");
        }
    }
    if let Some(tests) = item["graph_pulse"]["covering_tests"]["items"].as_array_mut() {
        for test in tests {
            attach_uri_from_fields(root, adapter, test, "path", "line");
        }
    }
}

fn attach_uri_from_fields(
    root: &Path,
    adapter: &Adapter,
    item: &mut Value,
    path_field: &str,
    line_field: &str,
) {
    let Some(path) = item
        .get(path_field)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    attach_uri(root, adapter, item, &path, line_field);
}

fn attach_uri(root: &Path, adapter: &Adapter, item: &mut Value, path: &str, line_field: &str) {
    let Ok(absolute) = resolve_file_path(root, adapter, path) else {
        return;
    };
    let line = item
        .get(line_field)
        .and_then(|line| {
            line.as_i64()
                .or_else(|| line.as_u64().and_then(|line| i64::try_from(line).ok()))
        })
        .and_then(|line| usize::try_from(line).ok());
    if let Some(uri) = file_uri_for_path(&absolute, line) {
        item["uri"] = json!(uri);
    }
}

fn attach_ast_uris(root: &Path, adapter: &Adapter, result: &mut Value) {
    let Some(matches) = result.get_mut("matches") else {
        return;
    };
    let items = match matches {
        Value::Array(items) => Some(items),
        Value::Object(page) => page.get_mut("items").and_then(Value::as_array_mut),
        _ => None,
    };
    let Some(items) = items else { return };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
    }
}
