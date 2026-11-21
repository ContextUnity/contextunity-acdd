use super::commitments::{read_metadata, MetadataKey};
use super::reader::{
    rows, rows_with_mode, rows_with_window_total, ExplainDirection, QueryBudget, RowMode,
};
use crate::models::tags::EdgeKind;
use acdd_core::response::{Detail, QueryOptions, MAX_PAGE_SIZE};
use anyhow::{ensure, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};

/// Validates query options and returns the current index generation string.
pub fn generation(conn: &Connection, options: &QueryOptions) -> Result<String> {
    ensure!(
        (1..=MAX_PAGE_SIZE).contains(&options.limit),
        "limit must be 1..=100"
    );
    ensure!(
        options
            .offset
            .checked_add(options.limit)
            .is_some_and(|end| end <= i64::MAX as usize),
        "offset is too large"
    );
    ensure!(
        options.offset == 0 || options.generation.is_some(),
        "continuation requires generation from the previous page"
    );
    let current = read_metadata(conn, MetadataKey::OutputRoot)?;
    ensure!(
        options.generation.as_ref().is_none_or(|previous| {
            previous == &current || (previous.len() >= 8 && current.starts_with(previous.as_str()))
        }),
        "index generation changed; restart at offset 0 without generation"
    );
    Ok(current)
}

/// Encodes paginated query items and pagination metadata into a JSON Value.
pub fn value(items: Vec<Value>, total: usize, options: &QueryOptions, generation: &str) -> Value {
    let next = options.offset.saturating_add(items.len());
    let has_more = next < total;
    json!({
        "total": total, "offset": options.offset, "limit": options.limit,
        "has_more": has_more, "next_offset": if has_more { Some(next) } else { None },
        "items": items, "generation": generation
    })
}

/// Executes a read-only count query.
pub fn count(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<usize> {
    let _budget = QueryBudget::new(conn);
    let mut statement = conn.prepare(sql)?;
    ensure!(statement.readonly(), "query must be read-only");
    let total: i64 = statement.query_row(params, |r| r.get(0))?;
    Ok(usize::try_from(total)?)
}

/// Executes a paginated query with structural row formatting.
pub fn query(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
) -> Result<Value> {
    query_with_mode(conn, sql, params, options, RowMode::Structural)
}

/// Executes a paginated query with opaque row formatting.
pub fn query_opaque(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
) -> Result<Value> {
    query_with_mode(conn, sql, params, options, RowMode::Opaque)
}

const QUERY_RESULT_TOO_LARGE: &str = "query result exceeds the 8 MiB row budget; narrow the selector or path, request detail='compact' with a smaller limit, or select fewer and smaller SQL columns. Continue paged requests with the returned offset and generation";

fn is_sqlite_too_big(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(sqlite_err, _))
                if sqlite_err.code == rusqlite::ErrorCode::TooBig
        )
    })
}

fn query_with_mode(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
    mode: RowMode,
) -> Result<Value> {
    let generation = generation(conn, options)?;
    let limit = options.limit as i64;
    let offset = options.offset as i64;
    let mut bindings = params.to_vec();
    bindings.extend([&limit as &dyn ToSql, &offset as &dyn ToSql]);
    let bounded = format!(
        "SELECT COUNT(*) OVER() AS __acdd_total, _q.* FROM ({sql}\n) _q LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2
    );
    match rows_with_window_total(conn, &bounded, &bindings, options.limit, mode) {
        Ok((items, window_total)) => {
            let total = match window_total {
                Some(total) => total,
                None if items.is_empty() && options.offset > 0 => {
                    count(conn, &format!("SELECT count(*) FROM ({sql}\n)"), params)?
                }
                None => 0,
            };
            Ok(value(items, total, options, &generation))
        }
        Err(err) if is_sqlite_too_big(&err) => {
            // Window materialization packs all columns into one B-tree record and can hit
            // SQLITE_LIMIT_LENGTH before Rust's 8 MiB row check runs; retry without OVER().
            let paged = format!(
                "SELECT * FROM ({sql}\n) LIMIT ?{} OFFSET ?{}",
                params.len() + 1,
                params.len() + 2
            );
            let items =
                rows_with_mode(conn, &paged, &bindings, options.limit, mode).map_err(|fb| {
                    if is_sqlite_too_big(&fb) {
                        anyhow::anyhow!(QUERY_RESULT_TOO_LARGE)
                    } else {
                        fb
                    }
                })?;
            let total = count(conn, &format!("SELECT count(*) FROM ({sql}\n)"), params)?;
            Ok(value(items, total, options, &generation))
        }
        Err(err) => Err(err),
    }
}

/// Formats standard SQL projection columns for nodes with dictionary path.
pub fn nodes(alias: &str, detail: Detail) -> String {
    nodes_with_path(
        alias,
        detail,
        &format!("(SELECT path FROM path_dictionary WHERE path_id={alias}.path_id)"),
    )
}

/// Formats SQL projection columns for nodes with a custom path expression.
pub fn nodes_with_path(alias: &str, detail: Detail, path_expression: &str) -> String {
    let path = format!("{path_expression} AS path");
    if detail == Detail::Full {
        format!(
            "{alias}.node_id,{alias}.id,{alias}.kind,{alias}.name,{alias}.qualname,{alias}.path_id,{path},{alias}.line,{alias}.end_line,{alias}.is_test,{alias}.language,{alias}.generated,{alias}.details,{alias}.node_hash,{alias}.owner_path_id"
        )
    } else {
        format!(
            "{alias}.id,{alias}.kind,{alias}.name,{alias}.qualname,{path},{alias}.line,{alias}.end_line,{alias}.language"
        )
    }
}

pub(crate) fn docs(alias: &str, detail: Detail) -> String {
    columns(
        alias,
        detail,
        &[
            "doc_id",
            "path",
            "section_title",
            "doc_type",
            "size",
            "is_invariant",
        ],
    )
}

pub(crate) fn edges(alias: &str, detail: Detail) -> String {
    let src = format!("(SELECT id FROM nodes WHERE node_hash={alias}.src_hash) AS src_public_id");
    let dst = format!("(SELECT id FROM nodes WHERE node_hash={alias}.dst_hash) AS dst_public_id");
    let path = format!("(SELECT path FROM path_dictionary WHERE path_id={alias}.path_id) AS path");
    let confidence = format!(
        "(SELECT evidence FROM coverage_evidence WHERE evidence_id={alias}.confidence_id) AS confidence"
    );
    if detail == Detail::Full {
        let evidence = format!(
            "(SELECT evidence FROM coverage_evidence WHERE evidence_id={alias}.evidence_id) AS evidence"
        );
        format!(
            "{src},{dst},{alias}.kind,{path},{alias}.line,{evidence},{confidence},{alias}.occurrence_count"
        )
    } else {
        format!(
            "{src},{dst},{alias}.kind,{path},{alias}.line,{confidence},{alias}.occurrence_count"
        )
    }
}

pub(crate) fn coverage(alias: &str, detail: Detail) -> String {
    let expression = format!(
        "(SELECT expression FROM coverage_expressions WHERE expression_id={alias}.expression_id) AS expression"
    );
    if detail == Detail::Full {
        format!(
            "(SELECT path FROM path_dictionary WHERE path_id={alias}.path_id) AS path,{alias}.line,{expression},{alias}.status,(SELECT evidence FROM coverage_evidence WHERE evidence_id={alias}.evidence_id) AS evidence"
        )
    } else {
        format!("{alias}.line,{expression},{alias}.status")
    }
}

fn columns(alias: &str, detail: Detail, names: &[&str]) -> String {
    if detail == Detail::Full {
        format!("{alias}.*")
    } else {
        names
            .iter()
            .map(|name| format!("{alias}.{name}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

pub(crate) fn annotate_edge_direction(page: &mut Value, direction: &str) {
    if let Some(items) = page["items"].as_array_mut() {
        for item in items {
            if let Some(edge) = item.as_object_mut() {
                edge.insert("direction".into(), json!(direction));
            }
        }
    }
}

pub(crate) fn populate_edge_relations(
    conn: &Connection,
    result: &mut Value,
    node_hash: i64,
    direction: ExplainDirection,
    options: &QueryOptions,
) -> Result<()> {
    for (include, key, filter_col, order_col, dir_label) in [
        (
            direction.includes_incoming(),
            "incoming",
            "dst_hash",
            "src_hash",
            "inbound",
        ),
        (
            direction.includes_outgoing(),
            "outgoing",
            "src_hash",
            "dst_hash",
            "outbound",
        ),
    ] {
        if include {
            let mut page = query(
                conn,
                &format!(
                    "SELECT {} FROM edges e WHERE e.{filter_col}=?1 ORDER BY e.kind,e.{order_col},e.path_id,e.line",
                    edges("e", options.detail)
                ),
                &[&node_hash],
                options,
            )?;
            annotate_edge_direction(&mut page, dir_label);
            result[key] = page;
        } else {
            let total = count(
                conn,
                &format!("SELECT count(*) FROM edges WHERE {filter_col}=?1"),
                &[&node_hash],
            )?;
            result[key] = json!({
                "total": total,
                "omitted": true,
                "hint": format!("Pass direction='{key}' to page {key} edges.")
            });
        }
    }
    for (key, id_alias, join_col, filter_col) in [
        ("implementors", "src_public_id", "src_hash", "dst_hash"),
        ("implements", "dst_public_id", "dst_hash", "src_hash"),
    ] {
        let items = rows(
            conn,
            &format!(
                "SELECT n.id AS {id_alias},e.kind,p.path,e.line,v.evidence,n.name,n.kind AS node_kind \
                 FROM edges e JOIN nodes n ON n.node_hash=e.{join_col} \
                 JOIN path_dictionary p ON p.path_id=e.path_id \
                 JOIN coverage_evidence v ON v.evidence_id=e.evidence_id \
                 WHERE e.{filter_col}=?1 AND e.kind IN('{implements}','{overrides}','{extends}') \
                 ORDER BY p.path,e.line",
                implements = EdgeKind::Implements.as_str(),
                overrides = EdgeKind::Overrides.as_str(),
                extends = EdgeKind::Extends.as_str(),
            ),
            &[&node_hash],
            50,
        )?;
        if !items.is_empty() {
            result[key] = json!(items);
        }
    }
    result["direction"] = json!(direction.as_str());
    Ok(())
}
