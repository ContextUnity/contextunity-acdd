use super::preview::{with_source, SourcePreviewMode};
use super::{paging, reader, ExplainOptions, InspectOptions};
use crate::models::{
    details::NodeDetailKey,
    stable_hash64,
    tags::{EdgeKind, NodeKind},
};
use acdd_core::response::{CoverageOptions, Detail, QueryOptions, SourceOptions, MAX_PAGE_SIZE};
use anyhow::{ensure, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

/// Inspects a symbol with path scoping and presentation options.
pub fn inspect_with_path_options(
    conn: &Connection,
    root: &Path,
    selector: &str,
    path: Option<&str>,
    options: &InspectOptions<'_>,
) -> Result<Value> {
    let result = inspect_paged_response(conn, selector, path, options.show_doc, options.page, options.coverage)?;
    with_source(conn, root, result, options.source, options.page, SourcePreviewMode::Configured)
}

/// Performs snippet paged.
pub fn snippet_paged(
    conn: &Connection,
    root: &Path,
    selector: &str,
    source: &SourceOptions,
    options: &QueryOptions,
) -> Result<Value> {
    snippet_paged_with_mode(conn, root, selector, None, source, options, SourcePreviewMode::Configured)
}

/// Pages symbol snippet matches bounded by response byte budget.
pub fn snippet_paged_with_response_budget(
    conn: &Connection,
    root: &Path,
    selector: &str,
    path: Option<&str>,
    source: &SourceOptions,
    options: &QueryOptions,
    response_bytes: usize,
) -> Result<Value> {
    let mode = SourcePreviewMode::CompleteSymbol { response_bytes };
    snippet_paged_with_mode(conn, root, selector, path, source, options, mode)
}

fn snippet_paged_with_mode(
    conn: &Connection,
    root: &Path,
    selector: &str,
    path: Option<&str>,
    source: &SourceOptions,
    options: &QueryOptions,
    mode: SourcePreviewMode,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let node = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    with_source(
        conn,
        root,
        json!({"node":node,"generation":generation}),
        source,
        options,
        mode,
    )
}

/// Explains incoming/outgoing symbol relations with path scoping.
pub fn explain_with_path_options(
    conn: &Connection,
    root: &Path,
    selector: &str,
    path: Option<&str>,
    options: &ExplainOptions<'_>,
) -> Result<Value> {
    let result = explain_paged_response(conn, selector, path, &options.explain, options.coverage)?;
    with_source(
        conn,
        root,
        result,
        options.source,
        options.explain.page,
        SourcePreviewMode::Configured,
    )
}

pub(crate) fn inspect_paged_response(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    show_doc: bool,
    options: &QueryOptions,
    coverage: CoverageOptions,
) -> Result<Value> {
    let mut result = if coverage.include_coverage {
        reader::inspect_paged_with_path(conn, selector, path, show_doc, options)?
    } else {
        inspect_without_coverage(conn, selector, path, show_doc, options)?
    };
    let id = result["node"]["id"].as_str().context("invalid selected node id")?.to_owned();
    result["documented_in"] = paging::query(
        conn,
        &format!(
            "SELECT d.doc_id,d.path,d.section_title,d.doc_type,n.line doc_line,e.line FROM edges e JOIN nodes n ON n.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=n.id WHERE e.kind='{}' AND e.src_hash=?1 ORDER BY d.path,d.doc_id,e.line",
            EdgeKind::ReferencesDoc.as_str(),
        ),
        &[&stable_hash64(&id)],
        options,
    )?;
    result["governing_invariants"] = reader::governing_invariants(conn, &id)?;
    let result = with_analysis_gaps(conn, result, options)?;
    summarize_symbol(conn, result, coverage.include_coverage, options.detail)
}

/// Attaches document contents to symbol inspect response.
pub fn attach_inspect_document_contents(conn: &Connection, result: &mut Value) -> Result<()> {
    let Some(items) = result
        .get("documented_in")
        .and_then(|page| page.get("items"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    if items.is_empty() {
        return Ok(());
    }
    ensure!(
        items.len() <= MAX_PAGE_SIZE,
        "documented_in page exceeds the response page bound"
    );

    let doc_ids = items
        .iter()
        .map(|item| {
            item.get("doc_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("documented_in page item is missing doc_id")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let mut contents = BTreeMap::new();
    if let Some(documents) = result
        .get("documents")
        .and_then(|page| page.get("items"))
        .and_then(Value::as_array)
    {
        for document in documents {
            let (Some(doc_id), Some(content)) = (
                document.get("doc_id").and_then(Value::as_str),
                document.get("content").and_then(Value::as_str),
            ) else {
                continue;
            };
            contents
                .entry(doc_id.to_owned())
                .or_insert_with(|| content.to_owned());
        }
    }
    let available = contents.keys().cloned().collect::<BTreeSet<_>>();
    let missing = doc_ids
        .difference(&available)
        .cloned()
        .collect::<BTreeSet<_>>();
    contents.extend(reader::document_contents_by_ids(conn, &missing)?);
    ensure!(
        doc_ids.iter().all(|doc_id| contents.contains_key(doc_id)),
        "documented_in page has no matching document content"
    );

    let mut attached = HashSet::new();
    let items = result
        .get_mut("documented_in")
        .and_then(|page| page.get_mut("items"))
        .and_then(Value::as_array_mut)
        .context("documented_in page items changed during content projection")?;
    for item in items {
        let doc_id = item
            .get("doc_id")
            .and_then(Value::as_str)
            .context("documented_in page item is missing doc_id")?
            .to_owned();
        let object = item
            .as_object_mut()
            .context("documented_in page item is not an object")?;
        if attached.insert(doc_id.clone()) {
            let content = contents
                .get(&doc_id)
                .context("documented_in page has no matching document content")?;
            object.insert("content".into(), json!(content));
        } else {
            object.remove("content");
        }
    }
    Ok(())
}

pub(crate) fn explain_paged_response(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    explain: &reader::ExplainOptions<'_>,
    coverage: CoverageOptions,
) -> Result<Value> {
    let result = if coverage.include_coverage {
        reader::explain_with_path_options(conn, selector, path, explain)?
    } else {
        explain_without_coverage(conn, selector, path, explain)?
    };
    let result = with_analysis_gaps(conn, result, explain.page)?;
    summarize_symbol(
        conn,
        result,
        coverage.include_coverage,
        explain.page.detail,
    )
}

fn with_analysis_gaps(
    conn: &Connection,
    mut result: Value,
    options: &QueryOptions,
) -> Result<Value> {
    let node = &result["node"];
    let scope_symbol = node["id"].as_str().context("invalid selected node id")?;
    let path = node["path"]
        .as_str()
        .context("invalid selected node path")?;
    let start = node["line"]
        .as_i64()
        .context("invalid selected start line")?;
    let end = node["end_line"]
        .as_i64()
        .context("invalid selected end line")?;
    result["analysis_gaps"] = paging::query(
        conn,
        "SELECT p.path,c.line,x.expression AS reason,v.evidence,?4 AS scope_symbol \
         FROM resolution_coverage c \
         JOIN path_dictionary p ON p.path_id=c.path_id \
         JOIN coverage_expressions x ON x.expression_id=c.expression_id \
         JOIN coverage_evidence v ON v.evidence_id=c.evidence_id \
         WHERE c.path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) \
           AND c.line BETWEEN ?2 AND ?3 AND v.evidence LIKE 'analysis gap:%' \
         ORDER BY c.line,x.expression,v.evidence",
        &[&path, &start, &end, &scope_symbol],
        options,
    )?;
    Ok(result)
}

fn inspect_without_coverage(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    show_doc: bool,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let node = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    let id = node["id"].as_str().context("invalid node id")?;
    let mut documents = if show_doc {
        paging::query(
            conn,
            &format!(
                "SELECT {} FROM doc_sections d JOIN nodes dn ON dn.id=d.doc_id JOIN edges e ON e.dst_hash=dn.node_hash WHERE e.src_hash=?1 AND e.kind='{}' ORDER BY d.is_invariant DESC,d.path,d.doc_id",
                paging::docs("d", options.detail),
                EdgeKind::ReferencesDoc.as_str(),
            ),
            &[&stable_hash64(id)],
            options,
        )?
    } else {
        paging::value(Vec::new(), 0, options, &generation)
    };
    if documents["total"] == 0 {
        documents = json!({"total": 0});
    }
    Ok(json!({"node":node,"documents":documents,"generation":generation}))
}

fn explain_without_coverage(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    explain: &reader::ExplainOptions<'_>,
) -> Result<Value> {
    let mut result =
        inspect_without_coverage(conn, selector, path, explain.show_doc, explain.page)?;
    let id = result["node"]["id"].as_str().context("invalid node id")?.to_owned();
    let node_hash = stable_hash64(&id);
    paging::populate_edge_relations(conn, &mut result, node_hash, explain.direction, explain.page)?;
    Ok(result)
}

fn summarize_symbol(
    conn: &Connection,
    mut result: Value,
    include_coverage: bool,
    detail: Detail,
) -> Result<Value> {
    if !include_coverage {
        result
            .as_object_mut()
            .context("invalid symbol result")?
            .remove("coverage");
    }
    if detail == Detail::Compact {
        let summary = compact_summary(conn, &result["node"])?;
        result["summary"] = summary;
    }
    Ok(result)
}

fn compact_summary(conn: &Connection, node: &Value) -> Result<Value> {
    const PREVIEW_LIMIT: i64 = 5;
    let id = node["id"].as_str().context("selected node has no id")?;
    let (
        signature,
        docstring,
        receiver_type,
        receiver,
        receiver_name,
        is_method,
        is_static,
    ) = conn.query_row(
        &format!(
            "SELECT json_extract(details,'$.{}'),json_extract(details,'$.{}'),json_extract(details,'$.receiver_type'),json_extract(details,'$.receiver'),json_extract(details,'$.receiver_name'),json_extract(details,'$.is_method'),json_extract(details,'$.is_static') FROM nodes WHERE id=?1",
            NodeDetailKey::SIGNATURE,
            NodeDetailKey::DOC,
        ),
        [id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<bool>>(5)?,
                row.get::<_, Option<bool>>(6)?,
            ))
        },
    )?;
    let contains = EdgeKind::Contains.as_str();
    let module = NodeKind::Module.as_str();
    let mut container = reader::rows(
        conn,
        &format!(
            "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line FROM edges e JOIN nodes n ON n.node_hash=e.src_hash JOIN path_dictionary p ON p.path_id=n.path_id WHERE e.dst_hash=?1 AND e.kind='{contains}' ORDER BY CASE WHEN n.kind IN('{module}','file') THEN 1 ELSE 0 END,n.line DESC,n.end_line,n.id LIMIT 1"
        ),
        &[&stable_hash64(id)],
        1,
    )?
    .into_iter()
    .next()
    .unwrap_or(Value::Null);
    let receiver = receiver_type.or(receiver);
    let is_method = node["kind"] == NodeKind::Method.as_str() || is_method.unwrap_or(false);
    let is_type_container = matches!(
        container["kind"].as_str(),
        Some(kind)
            if kind == NodeKind::Class.as_str()
                || kind == NodeKind::Enum.as_str()
                || kind == NodeKind::Interface.as_str()
                || kind == "record"
                || kind == NodeKind::Struct.as_str()
                || kind == NodeKind::Trait.as_str()
                || kind == NodeKind::Type.as_str()
    );
    if is_method && !is_type_container {
        if let Some(receiver) = receiver.as_deref() {
            let path = node["path"].as_str().unwrap_or("");
            let candidates = reader::rows(
                conn,
                &format!(
                    "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.name=?2 COLLATE NOCASE AND n.name=?2 COLLATE BINARY AND n.kind IN('{}','{}','{}','record','{}','{}','{}') ORDER BY n.line,n.id LIMIT 101",
                    NodeKind::Class.as_str(),
                    NodeKind::Enum.as_str(),
                    NodeKind::Interface.as_str(),
                    NodeKind::Struct.as_str(),
                    NodeKind::Trait.as_str(),
                    NodeKind::Type.as_str(),
                ),
                &[&path, &receiver],
                101,
            )?;
            let receiver_container = if candidates.len() == 1 {
                candidates.into_iter().next()
            } else if (2..=100).contains(&candidates.len()) {
                let method_qualname = node["qualname"].as_str().unwrap_or("");
                let module_qualname = container["qualname"].as_str().unwrap_or("");
                let mut scoped = candidates
                    .into_iter()
                    .filter(|candidate| {
                        let qualname = candidate["qualname"].as_str().unwrap_or("");
                        let impl_owner = !module_qualname.is_empty() && qualname == module_qualname;
                        let method_owner = [".", "::"].iter().any(|separator| {
                            method_qualname.starts_with(&format!("{qualname}{separator}"))
                        });
                        let module_owner = [".", "::"].iter().any(|separator| {
                            qualname == format!("{module_qualname}{separator}{receiver}")
                        });
                        impl_owner || method_owner || module_owner
                    })
                    .collect::<Vec<_>>();
                if scoped.len() > 1 {
                    let method_line = node["line"].as_u64().unwrap_or_default();
                    let nearest_line = scoped
                        .iter()
                        .filter_map(|candidate| candidate["line"].as_u64())
                        .map(|line| line.abs_diff(method_line))
                        .min();
                    if let Some(nearest_line) = nearest_line {
                        scoped.retain(|candidate| {
                            candidate["line"]
                                .as_u64()
                                .is_some_and(|line| line.abs_diff(method_line) == nearest_line)
                        });
                    }
                }
                if scoped.len() == 1 {
                    scoped.pop()
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(receiver_container) = receiver_container {
                container = receiver_container;
            } else {
                container = json!({
                    "kind": "receiver",
                    "name": receiver,
                    "path": node["path"],
                    "line": node["line"]
                });
            }
        }
    }
    let inbound = direct_call_summary(conn, id, true, PREVIEW_LIMIT)?;
    let outbound = direct_call_summary(conn, id, false, PREVIEW_LIMIT)?;
    let mut summary = json!({
        "signature": signature,
        "docstring": docstring,
        "container": container,
        "calls": {
            "inbound": inbound,
            "outbound": outbound
        }
    });
    if is_method {
        summary["method"] = json!({
            "receiver": receiver,
            "receiver_name": receiver_name,
            "is_static": is_static
        });
    }
    Ok(summary)
}

pub(super) fn direct_call_summary(
    conn: &Connection,
    id: &str,
    inbound: bool,
    preview_limit: i64,
) -> Result<Value> {
    let calls = EdgeKind::Calls.as_str();
    let (filter_col, join_col) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let count_sql = format!(
        "SELECT count(*),coalesce(sum(occurrence_count),0) FROM edges WHERE {filter_col}=?1 AND kind='{calls}'"
    );
    let preview_sql = format!(
        "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line FROM edges e JOIN nodes n ON n.node_hash=e.{join_col} JOIN path_dictionary p ON p.path_id=n.path_id WHERE e.{filter_col}=?1 AND e.kind='{calls}' ORDER BY p.path,n.line,n.id LIMIT ?2"
    );
    let (count, occurrences) = {
        let budget = reader::QueryBudget::new(conn);
        let counts = conn.query_row(&count_sql, [stable_hash64(id)], |row| {
            Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?))
        })?;
        budget.check()?;
        counts
    };
    let preview = reader::rows(
        conn,
        &preview_sql,
        &[&stable_hash64(id), &preview_limit],
        preview_limit as usize,
    )?;
    Ok(json!({"count":count,"occurrences":occurrences,"preview":preview}))
}
