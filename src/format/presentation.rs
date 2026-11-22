use super::{
    ast_legacy_error, compact, file_uri_for_path, render_for_operation, DiscoveryKind,
    OperationKind, OutputFormat,
};
use acdd_core::config::{resolve_file_path, Adapter};
use acdd_core::fs::canonical_root;
use serde_json::{json, Value};
use std::path::Path;

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
                let returned = items.len() as u64;
                let offset = offset.unwrap_or_default();
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

/// Adds guarded file links for supplied traversal coordinates in Markdown output.
pub(crate) fn attach_traversal_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
) {
    if let Some(selector) = value.get_mut("selector") {
        attach_uri_from_fields(root, adapter, selector, "path", "line");
    }
    if let Some(node) = value.get_mut("node") {
        attach_uri_from_fields(root, adapter, node, "path", "line");
    }

    for field in ["nodes", "changed_symbols", "tests", "unmapped_changes"] {
        if let Some(page) = value.get_mut(field) {
            attach_traversal_page(root, adapter, page, "line");
        }
    }
    for field in ["incoming", "outgoing"] {
        if let Some(page) = value.get_mut(field) {
            if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    attach_uri_from_fields(root, adapter, item, "path", "line");
                }
            }
        }
    }
    if let Some(page) = value.get_mut("affected_docs") {
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", "__no_line");
            }
        }
    }
    if let Some(page) = value.get_mut("documents") {
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", "doc_line");
            }
        }
    }
    for field in [
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
    if let Some(page) = value.get_mut("phantom_tested") {
        attach_traversal_page(root, adapter, page, "line");
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                if let Some(callers) = item.get_mut("test_callers").and_then(Value::as_array_mut) {
                    for caller in callers {
                        attach_uri_from_fields(root, adapter, caller, "path", "line");
                    }
                }
            }
        }
    }
    if let Some(page) = value.get_mut("dangling_calls") {
        attach_traversal_page(root, adapter, page, "__no_line");
    }
    if let Some(invariants) = value.get_mut("governing_invariants") {
        if let Some(rules) = invariants.get_mut("direct").and_then(Value::as_array_mut) {
            for rule in rules {
                if let Some(source) = rule.get_mut("source") {
                    attach_uri_from_fields(root, adapter, source, "path", "line");
                }
            }
        }
        if let Some(entries) = invariants
            .get_mut("omitted_sources")
            .and_then(Value::as_array_mut)
        {
            for entry in entries {
                if let Some(source) = entry.get_mut("source") {
                    attach_uri_from_fields(root, adapter, source, "path", "line");
                }
            }
        }
    }
    if let Some(changes) = value.get_mut("changes") {
        if let Some(files) = changes.get_mut("files").and_then(Value::as_array_mut) {
            for file in files {
                attach_uri_from_fields(root, adapter, file, "path", "__no_line");
            }
        }
    }
    if let Some(summary) = value.get_mut("summary") {
        if let Some(calls) = summary.get_mut("calls") {
            for direction in ["inbound", "outbound"] {
                if let Some(preview) = calls
                    .get_mut(direction)
                    .and_then(|direction| direction.get_mut("preview"))
                    .and_then(Value::as_array_mut)
                {
                    for item in preview {
                        attach_uri_from_fields(root, adapter, item, "path", "line");
                    }
                }
            }
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
