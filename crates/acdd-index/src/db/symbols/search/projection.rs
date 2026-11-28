use crate::db::paging;
use crate::db::reader::sql::QueryBudget;
use crate::models::details::NodeDetailKey;
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, Row, ToSql};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

const TYPED_COMPACT_COLUMNS: &str =
    "n.id,n.kind,n.name,n.qualname,pd.path,n.line,n.end_line,n.language";
const TYPED_FULL_COLUMNS: &str =
    "n.id,n.kind,n.name,n.qualname,pd.path,n.line,n.end_line,n.language,n.node_id,n.is_test,n.generated,n.details";

pub(super) fn typed_search_columns(detail: Detail) -> &'static str {
    if detail == Detail::Full {
        TYPED_FULL_COLUMNS
    } else {
        TYPED_COMPACT_COLUMNS
    }
}

#[derive(Clone, Copy)]
pub(super) enum SearchRowTrailer {
    None,
    Reason,
    Rank,
}

pub(super) struct TypedSearchFullFields {
    pub(super) node_id: i64,
    pub(super) is_test: i64,
    pub(super) generated: i64,
    pub(super) details_raw: String,
}

pub(super) struct TypedSearchHit {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) qualname: String,
    pub(super) path: String,
    pub(super) line: u64,
    pub(super) end_line: u64,
    pub(super) language: String,
    pub(super) match_reason: Option<String>,
    pub(super) full: Option<TypedSearchFullFields>,
}

pub(super) struct TypedSearchPage {
    pub(super) total: Option<u64>,
    pub(super) offset: usize,
    pub(super) limit: usize,
    pub(super) has_more: bool,
    pub(super) next_offset: Option<usize>,
    pub(super) items: Vec<TypedSearchHit>,
    pub(super) generation: String,
}

/// Typed ranked search hit carrying the bounded indexed signature (no source reads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchSignatureHit {
    pub qualname: String,
    pub path: String,
    pub line: u64,
    pub end_line: u64,
    pub signature: String,
}

pub(super) fn parse_details_json(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn normalize_match_reason(
    rank: Option<u64>,
    existing_reason: Option<String>,
    detail: Detail,
) -> Option<String> {
    if let Some(rank) = rank {
        let reason = match rank {
            0 => "exact_name",
            1 => "name_prefix",
            2 => "exact_qualified_name",
            3 => "name_fragment",
            _ => "indexed_text",
        };
        return (detail == Detail::Full || !matches!(rank, 0..=1 | 3))
            .then(|| reason.to_owned());
    }
    if detail == Detail::Compact && existing_reason.as_deref() == Some("name_pattern") {
        None
    } else {
        existing_reason
    }
}

fn decode_typed_search_hit(
    row: &Row<'_>,
    col_offset: usize,
    detail: Detail,
    trailer: SearchRowTrailer,
) -> rusqlite::Result<(TypedSearchHit, usize)> {
    let id: String = row.get(col_offset)?;
    let kind: String = row.get(col_offset + 1)?;
    let name: String = row.get(col_offset + 2)?;
    let qualname: String = row.get(col_offset + 3)?;
    let path: String = row.get(col_offset + 4)?;
    let line: u64 = row.get(col_offset + 5)?;
    let end_line: u64 = row.get(col_offset + 6)?;
    let language: String = row.get(col_offset + 7)?;
    let (full, next_col) = if detail == Detail::Full {
        (
            Some(TypedSearchFullFields {
                node_id: row.get(col_offset + 8)?,
                is_test: row.get(col_offset + 9)?,
                generated: row.get(col_offset + 10)?,
                details_raw: row.get(col_offset + 11)?,
            }),
            col_offset + 12,
        )
    } else {
        (None, col_offset + 8)
    };
    let (match_reason, after_trailer) = match trailer {
        SearchRowTrailer::None => (None, next_col),
        SearchRowTrailer::Reason => (
            normalize_match_reason(None, row.get::<_, Option<String>>(next_col)?, detail),
            next_col + 1,
        ),
        SearchRowTrailer::Rank => (
            normalize_match_reason(row.get::<_, Option<u64>>(next_col)?, None, detail),
            next_col + 1,
        ),
    };
    Ok((
        TypedSearchHit {
            id,
            kind,
            name,
            qualname,
            path,
            line,
            end_line,
            language,
            match_reason,
            full,
        },
        after_trailer,
    ))
}

#[derive(Serialize)]
struct SearchHitWire<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<i64>,
    id: &'a str,
    kind: &'a str,
    name: &'a str,
    qualname: &'a str,
    path: &'a str,
    line: u64,
    end_line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_test: Option<i64>,
    language: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_reason: Option<&'a str>,
}

impl TypedSearchHit {
    pub(super) fn to_wire_json(&self) -> Result<Value> {
        let (node_id, is_test, generated, details) = match &self.full {
            Some(full) => (
                Some(full.node_id),
                Some(full.is_test),
                Some(full.generated),
                Some(parse_details_json(&full.details_raw)),
            ),
            None => (None, None, None, None),
        };
        Ok(serde_json::to_value(SearchHitWire {
            node_id,
            id: &self.id,
            kind: &self.kind,
            name: &self.name,
            qualname: &self.qualname,
            path: &self.path,
            line: self.line,
            end_line: self.end_line,
            is_test,
            language: &self.language,
            generated,
            details,
            match_reason: self.match_reason.as_deref(),
        })?)
    }
}

impl TypedSearchPage {
    pub(super) fn into_page_json(self, items: Vec<Value>) -> Value {
        json!({
            "total": self.total,
            "offset": self.offset,
            "limit": self.limit,
            "has_more": self.has_more,
            "next_offset": self.next_offset,
            "items": items,
            "generation": self.generation
        })
    }
}

fn collect_typed_search_rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    (fetch_limit, offset): (usize, usize),
    (detail, trailer, leading_window_total): (Detail, SearchRowTrailer, bool),
) -> Result<(Vec<TypedSearchHit>, Option<Option<u64>>)> {
    let limit_i64 =
        i64::try_from(fetch_limit).context("search limit exceeds SQLite integer range")?;
    let offset_i64 =
        i64::try_from(offset).context("search offset exceeds SQLite integer range")?;
    let mut bindings = params.to_vec();
    bindings.extend([&limit_i64 as &dyn ToSql, &offset_i64]);
    let budget = QueryBudget::new(conn);
    budget.check()?;
    let mut stmt = conn.prepare(sql)?;
    ensure!(stmt.readonly(), "query must be read-only");
    let col_offset = usize::from(leading_window_total);
    let mut rows = stmt.query(rusqlite::params_from_iter(bindings))?;
    let mut items = Vec::with_capacity(fetch_limit);
    let mut total: Option<Option<u64>> = None;
    while let Some(row) = rows.next()? {
        let (hit, after_trailer) = decode_typed_search_hit(row, col_offset, detail, trailer)?;
        if total.is_none() {
            let total_col = if leading_window_total {
                0
            } else {
                after_trailer
            };
            total = Some(row.get::<_, Option<u64>>(total_col)?);
        }
        items.push(hit);
        if items.len() >= fetch_limit {
            break;
        }
    }
    drop(rows);
    drop(stmt);
    budget.check()?;
    Ok((items, total))
}

pub(super) fn query_typed_ranked_page(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
    trailer: SearchRowTrailer,
) -> Result<TypedSearchPage> {
    ensure!(sql.len() <= 64 * 1024, "SQL exceeds 64KiB");
    let generation = paging::generation(conn, options)?;
    let fetch_limit = options.limit.saturating_add(1);
    let limit_param = params.len() + 1;
    let paged_sql = format!("{sql} LIMIT ?{} OFFSET ?{}", limit_param, limit_param + 1);
    let (mut items, found_total) = collect_typed_search_rows(
        conn,
        &paged_sql,
        params,
        (fetch_limit, options.offset),
        (options.detail, trailer, false),
    )?;

    let total = if let Some(found) = found_total {
        found
    } else if options.offset == 0 {
        Some(0)
    } else {
        let probe_sql = format!(
            "SELECT * FROM ({sql}) LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2
        );
        let (_, probe_total) = collect_typed_search_rows(
            conn,
            &probe_sql,
            params,
            (1, 0),
            (options.detail, trailer, false),
        )?;
        probe_total.unwrap_or(Some(0))
    };

    let has_more = items.len() > options.limit;
    items.truncate(options.limit);
    let next_offset = has_more.then(|| options.offset.saturating_add(items.len()));
    Ok(TypedSearchPage {
        total,
        offset: options.offset,
        limit: options.limit,
        has_more,
        next_offset,
        items,
        generation,
    })
}

pub(super) fn query_typed_window_page(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
    trailer: SearchRowTrailer,
) -> Result<TypedSearchPage> {
    ensure!(sql.len() <= 64 * 1024, "SQL exceeds 64KiB");
    let generation = paging::generation(conn, options)?;
    let bounded = format!(
        "SELECT COUNT(*) OVER() AS __acdd_total, _q.* FROM ({sql}\n) _q LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2
    );
    let (items, found_total) = collect_typed_search_rows(
        conn,
        &bounded,
        params,
        (options.limit, options.offset),
        (options.detail, trailer, true),
    )?;

    let total = match found_total.flatten() {
        Some(total) => total,
        None if items.is_empty() && options.offset > 0 => {
            paging::count(conn, &format!("SELECT count(*) FROM ({sql}\n)"), params)? as u64
        }
        None => 0,
    };
    let next = options.offset.saturating_add(items.len());
    let has_more = (next as u64) < total;
    Ok(TypedSearchPage {
        total: Some(total),
        offset: options.offset,
        limit: options.limit,
        has_more,
        next_offset: has_more.then_some(next),
        items,
        generation,
    })
}

#[derive(Clone, Default)]
pub(super) struct IndexedNodeMetadata {
    pub(super) signature: Option<String>,
    pub(super) is_test: Option<bool>,
    pub(super) docstring: Option<String>,
}

/// Loads indexed signature, test flag, and docstring for every hit in one batched typed query.
pub(super) fn indexed_node_metadata(
    conn: &Connection,
    ids: &[&str],
) -> Result<HashMap<String, IndexedNodeMetadata>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let serialized_ids = serde_json::to_string(ids)?;
    let budget = QueryBudget::new(conn);
    budget.check()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT n.id,json_extract(n.details,'$.{}'),CASE WHEN n.is_test THEN 1 ELSE 0 END,json_extract(n.details,'$.{}') FROM nodes n JOIN json_each(?1) requested ON requested.value=n.id",
        NodeDetailKey::SIGNATURE,
        NodeDetailKey::DOC,
    ))?;
    ensure!(stmt.readonly(), "query must be read-only");
    let mapped = stmt.query_map([serialized_ids.as_str()], |row| {
        let id: String = row.get(0)?;
        let signature: Option<String> = row.get(1)?;
        let is_test: Option<i64> = row.get(2)?;
        let docstring: Option<String> = row.get(3)?;
        Ok((
            id,
            IndexedNodeMetadata {
                signature,
                is_test: is_test.map(|value| value != 0),
                docstring,
            },
        ))
    })?;
    let mut result = HashMap::with_capacity(ids.len());
    for entry in mapped {
        let (id, meta) = entry?;
        result.insert(id, meta);
    }
    budget.check()?;
    Ok(result)
}

pub(super) fn bounded_signature(value: &str, char_limit: usize) -> String {
    let mut bounded = value.chars().take(char_limit).collect::<String>();
    if value.chars().count() > char_limit {
        bounded.push('…');
    }
    bounded
}
