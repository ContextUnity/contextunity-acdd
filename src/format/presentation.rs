use super::coordination;
use super::core::{
    append_fenced_block, escape_label, is_freshness_envelope, is_tool_metadata, ordered_fields,
    render_field, render_markdown, render_markdown_fields, shorten_hash, text_field,
    KNOWN_PAGE_FIELDS,
};
use super::core::{OperationKind, OutputFormat};
use super::discovery::{self, file_uri_for_path, DiscoveryKind};
use super::lint;
use super::traversal;
use acdd_core::config::{resolve_file_path, Adapter};
use acdd_core::fs::canonical_root;
use serde_json::{json, Map, Value};
use std::path::Path;

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
    super::core::shorten_metadata_hashes(value);
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

fn render_acdd_read(value: &Value) -> String {
    let mut out = String::from("# acdd_read\n");
    render_read_node(&mut out, None, value, 0);
    out
}

fn render_read_node(out: &mut String, key: Option<&str>, value: &Value, depth: usize) {
    match value {
        Value::Object(obj) if !obj.is_empty() => {
            let d = key.map_or(depth, |k| {
                super::core::indent(out, depth);
                out.push_str(&format!("- **{}**\n", escape_label(k)));
                depth + 1
            });
            let (lang, mut deferred) = (infer_read_language(obj), Vec::new());
            for (k, child) in ordered_fields(obj) {
                if matches!(k, "content" | "source") { if let Some(code) = child.as_str() { deferred.push((k, code)); continue; } }
                render_read_node(out, Some(k), child, d);
            }
            for (k, code) in deferred {
                super::core::indent(out, d);
                out.push_str(&format!("- **{}**:\n", escape_label(k)));
                append_fenced_block(out, lang, code, 0);
            }
        }
        Value::Array(items) if !items.is_empty() && key.is_some() => {
            super::core::indent(out, depth);
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
        || value.get("items").and_then(Value::as_object).is_some_and(Map::is_empty)
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

fn move_owned_metadata_to_end(root: &mut Map<String, Value>) {
    let metadata = root.get("metadata").and_then(Value::as_object).is_some_and(is_tool_metadata).then(|| root.remove("metadata")).flatten();
    let (generation, freshness) = (root.remove("generation"), root.remove("freshness"));
    for (k, val) in [("metadata", metadata), ("generation", generation), ("freshness", freshness)] {
        if let Some(v) = val { root.insert(k.into(), v); }
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
        let mut body = value.clone();
        let hints = body
            .as_object_mut()
            .and_then(|object| object.remove("Hints"));
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
            OutputFormat::Json => ast_legacy_error(diagnostic),
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
    compact(&mut value, opts.operation, opts.explain);
    let rendered = render_with_acdd_read_hints(&value, opts);
    if measure_bytes(&rendered) <= budget {
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
        compact(&mut value, opts.operation, opts.explain);
        let rendered = render_with_acdd_read_hints(&value, opts);
        return if measure_bytes(&rendered) <= budget {
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
        compact(&mut value, opts.operation, opts.explain);
        let candidate = render_with_acdd_read_hints(&value, opts);
        if measure_bytes(&candidate) <= budget {
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
            compact(&mut metadata, opts.operation, opts.explain);
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
        compact(candidate, opts.operation, opts.explain);
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

fn trim_tasks_page_tail(object: &mut serde_json::Map<String, Value>) -> bool {
    let Some(pagination) = object.get("pagination").and_then(Value::as_object).cloned() else {
        return false;
    };
    let page_is_continuable = pagination.get("total").and_then(Value::as_u64).is_some()
        && pagination.get("offset").and_then(Value::as_u64).is_some()
        && pagination
            .get("generation")
            .and_then(Value::as_str)
            .is_some_and(|generation| !generation.is_empty());
    if !page_is_continuable {
        return false;
    }
    let Some(tasks) = object.get_mut("tasks").and_then(Value::as_array_mut) else {
        return false;
    };
    if tasks.len() <= 1 {
        return false;
    }
    tasks.pop();
    let returned = tasks.len() as u64;
    let offset = pagination.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let total = pagination.get("total").and_then(Value::as_u64).unwrap_or(returned);
    let next = offset.saturating_add(returned);
    if let Some(page) = object.get_mut("pagination").and_then(Value::as_object_mut) {
        page.insert("returned".into(), json!(returned));
        let omitted = page.get("byte_omitted").and_then(Value::as_u64).unwrap_or(0);
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
    true
}

fn trim_items_page_tail(object: &mut serde_json::Map<String, Value>) -> bool {
    let total = object.get("total").and_then(Value::as_u64);
    let offset = object.get("offset").and_then(Value::as_u64);
    let generation = object
        .get("generation")
        .and_then(Value::as_str)
        .filter(|generation| !generation.is_empty())
        .map(str::to_owned);
    let has_more = object.get("has_more").and_then(Value::as_bool) == Some(true);
    let has_usable_cursor = object.get("next_offset").and_then(Value::as_u64).is_some();
    if offset.is_none()
        || generation.is_none()
        || (total.is_none() && !(has_more && has_usable_cursor))
    {
        return false;
    }
    let Some(items) = object.get_mut("items").and_then(Value::as_array_mut) else {
        return false;
    };
    if items.len() <= 1 {
        return false;
    }
    items.pop();
    let returned = items.len() as u64;
    let next = offset.unwrap_or_default().saturating_add(returned);
    let has_more = total.map_or(has_more, |total| next < total);
    let omitted = object.get("byte_omitted").and_then(Value::as_u64).unwrap_or(0);
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
    true
}

fn trim_page_tails(value: &mut Value, allow_tasks: bool, allow_items: bool) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    if allow_tasks && trim_tasks_page_tail(object) {
        return true;
    }
    if allow_tasks && object.contains_key("tasks") {
        return false;
    }
    if allow_items && trim_items_page_tail(object) {
        return true;
    }
    if allow_items && object.contains_key("items") {
        return false;
    }
    for child in object.values_mut() {
        if trim_page_tails(child, allow_tasks, allow_items) {
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
    attach_array_items_uris(root, adapter, value.get_mut("changes"), "files", None, "__no_line");
    if let Some(calls) = value.get_mut("summary").and_then(|s| s.get_mut("calls")) {
        for direction in ["inbound", "outbound"] {
            attach_array_items_uris(root, adapter, calls.get_mut(direction), "preview", None, "line");
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
    if let Some(items) = value["nodes"]["items"].as_array_mut() {
        for item in items {
            attach_uri_from_fields(root, adapter, item, "path", "line");
            attach_search_pulse_uris(root, adapter, item);
        }
    }
    if let Some(groups) = value["nodes"]["grouped_by_file"].as_object_mut() {
        for (path, items) in groups {
            if let Some(items) = items.as_array_mut() {
                for item in items {
                    attach_uri(root, adapter, item, path, "line");
                    attach_search_pulse_uris(root, adapter, item);
                }
            }
        }
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
