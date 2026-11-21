use super::super::commitments::MAX_QUERY_RESULT_BYTES;
use acdd_core::format::estimate_json_bytes;
use anyhow::{bail, Result};
use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};

const QUERY_RESULT_TOO_LARGE: &str = "query result exceeds the 8 MiB row budget; narrow the selector or path, request detail='compact' with a smaller limit, or select fewer and smaller SQL columns. Continue paged requests with the returned offset and generation";

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
///
/// Limits the query to two seconds, the supplied row count, and an 8 MiB
/// serialized result. Each SQLite value must also fit the configured cell limit.
///
/// # Errors
///
/// Rejects writes, oversized queries or results, timeouts, and SQLite errors.
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
pub(super) fn rows_with_mode_and_bytes(
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
    number.unsigned_abs().checked_ilog10().map_or(1, |l| l as usize + 1) + usize::from(number < 0)
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
    if limit == 0 || limit > 500000 { bail!("internal row budget exceeded"); }
    if sql.len() > 64 * 1024 { bail!("SQL exceeds64KiB"); }
    let _budget = QueryBudget::new(conn);
    let mut statement = conn.prepare(sql)?;
    if !statement.readonly() { bail!("query must be read-only"); }
    let names: Vec<_> = statement.column_names().into_iter().map(str::to_owned).collect();
    let key_colon_bytes: Vec<usize> = names.iter().map(|n| exact_json_str_bytes(n).saturating_add(1)).collect();
    let start_col = usize::from(strip_leading_total && !names.is_empty());
    let mut query = statement.query(params)?;
    let (mut result, mut total_bytes, mut window_total) = (Vec::new(), 2usize, None);

    while let Some(row) = query.next()? {
        if strip_leading_total && window_total.is_none() {
            window_total = Some(usize::try_from(row.get_ref(0)?.as_i64()?)?);
        }
        let (mut value, mut row_bytes, mut prop_count) = (Map::new(), 2usize, 0usize);
        let row_sep = usize::from(!result.is_empty());
        let avail = MAX_QUERY_RESULT_BYTES.saturating_sub(total_bytes).saturating_sub(row_sep);

        for i in start_col..names.len() {
            let name = &names[i];
            if mode == RowMode::Structural && matches!(name.as_str(), "node_hash" | "path_hash" | "path_id" | "owner_path_id") {
                continue;
            }
            if names[i + 1..].iter().any(|later| later == name) {
                value.entry(name.clone()).or_insert(Value::Null);
                continue;
            }
            let raw = row.get_ref(i)?;
            let json_col = mode == RowMode::Structural && matches!(name.as_str(), "details" | "invariants" | "referenced_symbols");
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
                ValueRef::Real(v) => { let c = json!(v); let l = estimate_json_bytes(&c); (c, l) }
                ValueRef::Text(v) => {
                    let s = String::from_utf8_lossy(v);
                    if json_col {
                        match serde_json::from_str::<Value>(&s) {
                            Ok(p) => { let l = estimate_json_bytes(&p); (p, l) }
                            Err(_) => (json!(s), exact_json_str_bytes(&s)),
                        }
                    } else {
                        (json!(s), exact_json_str_bytes(&s))
                    }
                }
                ValueRef::Blob(v) => (json!(hex::encode(v)), v.len().saturating_mul(2).saturating_add(2)),
            };
            row_bytes = row_bytes.saturating_add(overhead).saturating_add(val_bytes);
            prop_count += 1;
            if row_bytes > avail { bail!(QUERY_RESULT_TOO_LARGE); }
            value.insert(name.clone(), cell);
        }
        total_bytes = total_bytes.saturating_add(row_sep).saturating_add(row_bytes);
        result.push(Value::Object(value));
        if result.len() >= limit { break; }
    }
    Ok((result, window_total, total_bytes))
}
