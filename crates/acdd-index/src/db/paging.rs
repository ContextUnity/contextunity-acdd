use super::commitments::{read_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES};
use crate::models::stable_hash64;
use acdd_core::{
    format::estimate_json_bytes,
    response::{Detail, QueryOptions, MAX_PAGE_SIZE},
};
use anyhow::{bail, ensure, Result};
use rusqlite::{types::ValueRef, Connection, ToSql};
use serde_json::{json, Map, Value};

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

pub(crate) struct QueryBudget<'a> {
    conn: &'a Connection,
    started: std::time::Instant,
}

impl<'a> QueryBudget<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        let started = std::time::Instant::now();
        conn.progress_handler(
            1000,
            Some(move || started.elapsed() > std::time::Duration::from_secs(2)),
        );
        Self { conn, started }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.started.elapsed() > std::time::Duration::from_secs(2) {
            bail!("graph query exceeds two second budget");
        }
        Ok(())
    }
}

impl Drop for QueryBudget<'_> {
    fn drop(&mut self) {
        self.conn.progress_handler(0, None::<fn() -> bool>);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowMode {
    Structural,
    Opaque,
}

/// Executes bounded read-only SQL and converts rows to JSON values.
pub fn rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
) -> Result<Vec<Value>> {
    rows_with_mode(conn, sql, params, limit, RowMode::Structural)
}

pub(crate) fn rows_with_mode(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
    mode: RowMode,
) -> Result<Vec<Value>> {
    let (items, _, _) = collect_rows(conn, sql, params, limit, mode, false)?;
    Ok(items)
}

#[cfg(test)]
pub(crate) fn rows_with_mode_and_bytes(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
    mode: RowMode,
) -> Result<(Vec<Value>, usize)> {
    let (items, _, bytes) = collect_rows(conn, sql, params, limit, mode, false)?;
    Ok((items, bytes))
}

pub(crate) fn rows_with_window_total(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
    mode: RowMode,
) -> Result<(Vec<Value>, Option<usize>)> {
    let (items, total, _) = collect_rows(conn, sql, params, limit, mode, true)?;
    Ok((items, total))
}

#[inline]
fn i64_decimal_len(number: i64) -> usize {
    number
        .unsigned_abs()
        .checked_ilog10()
        .map_or(1, |l| l as usize + 1)
        + usize::from(number < 0)
}

#[inline]
fn exact_json_str_bytes(text: &str) -> usize {
    if text.bytes().all(|b| b >= 0x20 && b != b'"' && b != b'\\') {
        text.len().saturating_add(2)
    } else {
        estimate_json_bytes(text)
    }
}

fn collect_rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    limit: usize,
    mode: RowMode,
    strip_leading_total: bool,
) -> Result<(Vec<Value>, Option<usize>, usize)> {
    if limit == 0 || limit > 500000 {
        bail!("internal row budget exceeded");
    }
    if sql.len() > 64 * 1024 {
        bail!("SQL exceeds64KiB");
    }
    let _budget = QueryBudget::new(conn);
    let mut statement = conn.prepare(sql)?;
    if !statement.readonly() {
        bail!("query must be read-only");
    }
    let names: Vec<_> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let key_colon_bytes: Vec<usize> = names
        .iter()
        .map(|n| exact_json_str_bytes(n).saturating_add(1))
        .collect();
    let start_col = usize::from(strip_leading_total && !names.is_empty());
    let mut query = statement.query(params)?;
    let (mut result, mut total_bytes, mut window_total) = (Vec::new(), 2usize, None);

    while let Some(row) = query.next()? {
        if strip_leading_total && window_total.is_none() {
            window_total = Some(usize::try_from(row.get_ref(0)?.as_i64()?)?);
        }
        let (mut value, mut row_bytes, mut prop_count) = (Map::new(), 2usize, 0usize);
        let row_sep = usize::from(!result.is_empty());
        let avail = MAX_QUERY_RESULT_BYTES
            .saturating_sub(total_bytes)
            .saturating_sub(row_sep);

        for i in start_col..names.len() {
            let name = &names[i];
            if mode == RowMode::Structural
                && matches!(
                    name.as_str(),
                    "node_hash" | "path_hash" | "path_id" | "owner_path_id"
                )
            {
                continue;
            }
            if names[i + 1..].iter().any(|later| later == name) {
                value.entry(name.clone()).or_insert(Value::Null);
                continue;
            }
            let raw = row.get_ref(i)?;
            let json_col = mode == RowMode::Structural
                && matches!(
                    name.as_str(),
                    "details" | "invariants" | "referenced_symbols"
                );
            let min_val = match raw {
                ValueRef::Null => 4,
                ValueRef::Integer(n) => i64_decimal_len(n),
                ValueRef::Real(_) => 1,
                ValueRef::Text(_) if json_col => 1,
                ValueRef::Text(b) => b.len().saturating_add(2),
                ValueRef::Blob(b) => b.len().saturating_mul(2).saturating_add(2),
            };
            let overhead = usize::from(prop_count > 0).saturating_add(key_colon_bytes[i]);
            if row_bytes.saturating_add(overhead).saturating_add(min_val) > avail {
                bail!(QUERY_RESULT_TOO_LARGE);
            }
            let (cell, val_bytes) = match raw {
                ValueRef::Null => (Value::Null, 4),
                ValueRef::Integer(v) => (json!(v), i64_decimal_len(v)),
                ValueRef::Real(v) => {
                    let c = json!(v);
                    let l = estimate_json_bytes(&c);
                    (c, l)
                }
                ValueRef::Text(v) => {
                    let s = String::from_utf8_lossy(v);
                    if json_col {
                        match serde_json::from_str::<Value>(&s) {
                            Ok(p) => {
                                let l = estimate_json_bytes(&p);
                                (p, l)
                            }
                            Err(_) => (json!(s), exact_json_str_bytes(&s)),
                        }
                    } else {
                        (json!(s), exact_json_str_bytes(&s))
                    }
                }
                ValueRef::Blob(v) => (
                    json!(hex::encode(v)),
                    v.len().saturating_mul(2).saturating_add(2),
                ),
            };
            row_bytes = row_bytes.saturating_add(overhead).saturating_add(val_bytes);
            prop_count += 1;
            if row_bytes > avail {
                bail!(QUERY_RESULT_TOO_LARGE);
            }
            value.insert(name.clone(), cell);
        }
        total_bytes = total_bytes
            .saturating_add(row_sep)
            .saturating_add(row_bytes);
        result.push(Value::Object(value));
        if result.len() >= limit {
            break;
        }
    }
    Ok((result, window_total, total_bytes))
}

// Stored decorates edges point from decorator to target; dependency walks reverse them.
pub(crate) const FORWARD_DEPENDENCIES: &str =
    "'calls','possible_calls','inherits','implements','overrides','extends','includes','mutates','handles','references','calls_endpoint','defines_table','queries_table','mutates_table','references_table'";
pub(crate) const REVERSE_DEPENDENCIES: &str = "'decorates'";
pub(crate) const STRUCTURAL_DEPENDENCIES: &str = ",'imports','contains','documents'";

/// Counts immediate dependency links for a symbol ID in the given direction.
pub(crate) fn immediate_links(
    conn: &Connection,
    id: &str,
    inbound: bool,
    structural: bool,
) -> Result<usize> {
    let (forward, reverse) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let extra = if structural {
        STRUCTURAL_DEPENDENCIES
    } else {
        ""
    };
    let forward = count(
        conn,
        &format!(
            "SELECT count(*) FROM edges WHERE {forward}=?1 AND kind IN({FORWARD_DEPENDENCIES}{extra})"
        ),
        &[&stable_hash64(id)],
    )?;
    let reverse = count(
        conn,
        &format!(
            "SELECT count(*) FROM edges WHERE {reverse}=?1 AND kind IN({REVERSE_DEPENDENCIES})"
        ),
        &[&stable_hash64(id)],
    )?;
    Ok(forward.saturating_add(reverse))
}
