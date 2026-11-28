use super::projection::{
    bounded_signature, indexed_node_metadata, parse_details_json, IndexedNodeMetadata,
};
use super::query::search_typed;
use crate::db::reader::sql::QueryBudget;
use crate::db::source::VerifiedSourceCache;
use crate::db::symbols::inspect::direct_call_summary;
use crate::db::symbols::preview::{
    first_docstring_sentence, resolve_indexed_text_match, source_preview,
};
use crate::db::symbols::test_discovery::tests_paged_with_path;
use crate::db::symbols::types::SearchOptions;
use crate::models::{
    stable_hash64,
    tags::{EdgeKind, LanguageId},
};
use acdd_core::response::{
    Detail, QueryOptions, ResponsePolicy, SourceOptions, SymbolClassification,
};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

#[derive(Serialize)]
struct InspectSelectorDto {
    selector: String,
    show_source: bool,
}

#[derive(Serialize)]
struct SourcePreviewWindowDto {
    start_line: usize,
    end_line: usize,
    body_lines: usize,
    has_more: bool,
}

#[derive(Serialize)]
struct SearchNavigationItemDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<i64>,
    id: String,
    kind: String,
    name: String,
    qualname: String,
    path: String,
    line: u64,
    end_line: u64,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_reason: Option<String>,
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inspect_selector: Option<InspectSelectorDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_test: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    classification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring_first_sentence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_pulse: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_preview: Option<Value>,
}

struct SearchSourceCache<'a> {
    conn: &'a Connection,
    root: &'a Path,
    files: HashMap<String, Option<String>>,
    loader: VerifiedSourceCache,
}

impl<'a> SearchSourceCache<'a> {
    fn new(conn: &'a Connection, root: &'a Path) -> Self {
        Self {
            conn,
            root,
            files: HashMap::new(),
            loader: VerifiedSourceCache::new(),
        }
    }

    fn read(&mut self, path: &str) -> Option<&str> {
        self.files
            .entry(path.to_owned())
            .or_insert_with(|| self.loader.read(self.conn, self.root, path).ok())
            .as_deref()
    }
}

fn apply_indexed_text_match_preview(
    sources: &mut SearchSourceCache<'_>,
    item: &mut SearchNavigationItemDto,
    pattern: &str,
    include_preview: bool,
    preview_bytes_per_hit: usize,
) -> Option<(String, SourcePreviewWindowDto)> {
    let start = usize::try_from(item.line).ok()?;
    let end = usize::try_from(item.end_line).ok()?;
    let text = sources.read(&item.path)?;
    let matched = resolve_indexed_text_match(text, start, end, pattern, preview_bytes_per_hit)?;
    item.line = matched.line as u64;
    item.match_snippet = Some(matched.snippet);
    if !include_preview {
        return None;
    }
    let (preview_src, win_start, win_end) = matched.preview?;
    Some((
        preview_src,
        SourcePreviewWindowDto {
            start_line: win_start,
            end_line: win_end,
            body_lines: win_end.saturating_sub(win_start) + 1,
            has_more: false,
        },
    ))
}

/// Searches and adds indexed signatures and bounded navigation previews in page order.
pub fn search_results_with_navigation(
    conn: &Connection,
    root: &Path,
    pattern: &str,
    options: &SearchOptions<'_>,
    include_preview: bool,
    include_pulse: bool,
) -> Result<Value> {
    const SIGNATURE_CHAR_LIMIT: usize = 512;
    const PREVIEW_HIT_LIMIT: usize = 3;
    const PREVIEW_BYTES_PER_HIT: usize = 4096;

    let (pattern, mut page) = search_typed(conn, pattern, options)?;
    let has_small_total = page
        .total
        .is_some_and(|total| total <= PREVIEW_HIT_LIMIT as u64);
    if page.items.is_empty() {
        return Ok(json!({"pattern": pattern, "nodes": page.into_page_json(Vec::new())}));
    }

    let mut sources = SearchSourceCache::new(conn, root);
    let ids: Vec<&str> = page.items.iter().map(|hit| hit.id.as_str()).collect();
    let metadata = indexed_node_metadata(conn, &ids)?;

    let previews_enabled = include_preview || has_small_total;
    let markdown = LanguageId::Markdown.as_str();
    let raw_hits = std::mem::take(&mut page.items);
    let mut rendered_items = Vec::with_capacity(raw_hits.len());
    for (index, hit) in raw_hits.into_iter().enumerate() {
        let (node_id, full_is_test, generated, details) = match hit.full {
            Some(full) => (
                Some(full.node_id),
                Some(json!(full.is_test)),
                Some(full.generated),
                Some(parse_details_json(&full.details_raw)),
            ),
            None => (None, None, None, None),
        };
        let IndexedNodeMetadata {
            signature,
            is_test,
            docstring,
        } = metadata.get(&hit.id).cloned().unwrap_or_default();
        let (is_test_val, classification) = if let Some(is_test) = is_test {
            let class = if hit.language == markdown {
                SymbolClassification::Documentation
            } else if is_test {
                SymbolClassification::Test
            } else {
                SymbolClassification::Production
            };
            (Some(json!(is_test)), Some(class.as_str().to_owned()))
        } else {
            (full_is_test, None)
        };
        let docstring_first_sentence = docstring.as_deref().and_then(first_docstring_sentence);
        let mut dto = SearchNavigationItemDto {
            node_id,
            id: hit.id.clone(),
            kind: hit.kind,
            name: hit.name,
            qualname: hit.qualname,
            path: hit.path,
            line: hit.line,
            end_line: hit.end_line,
            language: hit.language,
            generated,
            details,
            match_reason: hit.match_reason,
            signature: signature.map(|value| bounded_signature(&value, SIGNATURE_CHAR_LIMIT)),
            inspect_selector: Some(InspectSelectorDto {
                selector: hit.id,
                show_source: true,
            }),
            is_test: is_test_val,
            classification,
            docstring_first_sentence,
            match_snippet: None,
            graph_pulse: None,
            source: None,
            source_preview: None,
        };

        let is_indexed_text = dto.match_reason.as_deref() == Some("indexed_text");
        let is_markdown = dto.language == markdown;
        let matched_window = if is_indexed_text && !is_markdown && !pattern.is_empty() {
            apply_indexed_text_match_preview(
                &mut sources,
                &mut dto,
                &pattern,
                previews_enabled,
                PREVIEW_BYTES_PER_HIT,
            )
        } else {
            None
        };
        if index == 0 && !options.exact && include_pulse {
            dto.graph_pulse = Some(search_graph_pulse(conn, &dto.id));
        }
        if previews_enabled && index < PREVIEW_HIT_LIMIT {
            if let Some((preview_src, preview_meta)) = matched_window {
                dto.source = Some(preview_src);
                dto.source_preview = Some(serde_json::to_value(preview_meta)?);
            } else if dto.kind != "file" {
                if let Some(text) = sources.read(&dto.path) {
                    let node = json!({
                        "id": dto.id,
                        "kind": dto.kind,
                        "name": dto.name,
                        "qualname": dto.qualname,
                        "path": dto.path,
                        "line": dto.line,
                        "end_line": dto.end_line,
                        "language": dto.language
                    });
                    let options = SourceOptions {
                        enabled: true,
                        leading_lines: 5,
                        max_body_lines: 15,
                        offset: 0,
                    };
                    if let Ok((source, preview)) =
                        source_preview(conn, &node, text, &options, PREVIEW_BYTES_PER_HIT)
                    {
                        dto.source = Some(source);
                        dto.source_preview = Some(preview);
                    }
                }
            }
        }
        rendered_items.push(serde_json::to_value(dto)?);
    }
    Ok(json!({"pattern": pattern, "nodes": page.into_page_json(rendered_items)}))
}

fn search_graph_pulse(conn: &Connection, id: &str) -> Value {
    let callers = match direct_call_summary(conn, id, true, 3) {
        Ok(summary) => summary,
        Err(_) => {
            json!({"availability":"unavailable","cause":"caller relationships exceeded the indexed query budget"})
        }
    };
    let implementations = (|| -> Result<usize> {
        let budget = QueryBudget::new(conn);
        let count = conn.query_row(
            &format!(
                "SELECT count(*) FROM edges WHERE kind='{}' AND (src_hash=?1 OR dst_hash=?1)",
                EdgeKind::Implements.as_str(),
            ),
            [stable_hash64(id)],
            |row| row.get::<_, usize>(0),
        )?;
        budget.check()?;
        Ok(count)
    })()
    .map(|count| json!({"availability":"available","count":count}))
    .unwrap_or_else(|_| json!({"availability":"unavailable","cause":"implementation relationships are unavailable within the indexed query budget"}));

    let covering_tests = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(3),
        0,
        Some(Detail::Compact),
        None,
    )
    .and_then(|page| tests_paged_with_path(conn, id, None, "inbound", &page))
    .map(|result| {
        let method = result["discovery_method"]
            .as_str()
            .unwrap_or("unavailable")
            .to_owned();
        let total = result["nodes"]["total"].clone();
        json!({
            "availability":"available",
            "discovery_method":method,
            "total":total,
            "items":result["nodes"]["items"],
            "unresolved_references":result["unresolved_references"]
        })
    })
    .unwrap_or_else(|_| json!({"availability":"unavailable","cause":"test relationships are unavailable within the indexed query budget"}));

    json!({
        "availability":"available",
        "callers":callers,
        "implementations":implementations,
        "covering_tests":covering_tests
    })
}
