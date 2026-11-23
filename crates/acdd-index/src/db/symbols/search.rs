use super::inspect::direct_call_summary;
use super::preview::{first_docstring_sentence, resolve_indexed_text_match, source_preview};
use crate::db::source::VerifiedSourceCache;
use super::test_discovery::tests_paged_with_path;
use super::types::SearchOptions;
use crate::db::paging;
use crate::db::reader::{
    selectors::{path_bounds, strip_selector_uri_prefix},
    sql::{rows, QueryBudget},
};
use crate::models::{
    details::NodeDetailKey,
    stable_hash64,
    tags::{EdgeKind, LanguageId, NodeKind},
};
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy, SourceOptions};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

fn fts_tokens(pattern: &str) -> Vec<&str> {
    pattern
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect()
}

fn quoted_fts_terms(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|part| format!("\"{part}\"")).collect()
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn scope_filter_sql(kind_idx: usize, path_idx: usize, include_docs_idx: usize) -> String {
    let function = NodeKind::Function.as_str();
    let method = NodeKind::Method.as_str();
    let markdown = LanguageId::Markdown.as_str();
    let p_start = path_idx + 1;
    let p_end = path_idx + 2;
    format!(
        "(?{kind_idx}='' OR n.kind=?{kind_idx} OR (?{kind_idx}='{method}' AND n.kind='{function}') OR (?{kind_idx}='{function}' AND n.kind='{method}')) \
         AND (?{path_idx}='' OR pd.path=?{path_idx} OR (pd.path>=?{p_start} AND pd.path<?{p_end})) \
         AND (?{include_docs_idx}=1 OR n.language!='{markdown}')"
    )
}

struct ExactSearchContext<'a> {
    columns: &'a str,
    graph_boost: &'a str,
    pattern: &'a str,
    kind: &'a str,
    path: &'a str,
    path_start: &'a str,
    path_end: &'a str,
    include_docs: bool,
    options: &'a QueryOptions,
}

fn run_exact_search_query(
    conn: &Connection,
    ctx: &ExactSearchContext<'_>,
    ranked: bool,
) -> Result<Value> {
    let ExactSearchContext {
        columns,
        graph_boost,
        pattern,
        kind,
        path,
        path_start,
        path_end,
        include_docs,
        options,
    } = *ctx;
    let fts_query = quoted_fts_terms(&fts_tokens(pattern)).join(" AND ");
    let (from, match_pred, match_rank_expr, scope_filter) = if fts_query.is_empty() {
        (
            "(SELECT node_id FROM nodes WHERE name=?1 UNION SELECT node_id FROM nodes WHERE qualname=?1) exact JOIN nodes n ON n.node_id=exact.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id",
            String::new(),
            "CASE WHEN n.name=?1 THEN 0 ELSE 2 END",
            scope_filter_sql(2, 3, 6),
        )
    } else {
        (
            "node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id",
            "node_search MATCH ?1 AND (n.name=?2 COLLATE NOCASE OR n.qualname=?2 COLLATE NOCASE) AND ".to_owned(),
            "CASE WHEN n.name=?2 COLLATE NOCASE THEN 0 ELSE 2 END",
            scope_filter_sql(3, 4, 7),
        )
    };
    let where_clause = format!("{match_pred}{scope_filter}");
    let sql = if ranked {
        format!(
            "WITH exact_probe AS MATERIALIZED (SELECT n.node_id,{match_rank_expr} AS match_rank,pd.path,n.line,n.id FROM {from} WHERE {where_clause} ORDER BY match_rank,pd.path,n.line,n.id LIMIT 1001), result_total AS (SELECT CASE WHEN count(*)>=1001 THEN NULL ELSE count(*) END AS search_total FROM exact_probe), graph_window AS MATERIALIZED (SELECT node_id,match_rank,path,line,id FROM exact_probe ORDER BY match_rank,path,line,id LIMIT 50), boosted AS MATERIALIZED (SELECT p.node_id,{graph_boost} AS graph_boost FROM graph_window p JOIN nodes n ON n.node_id=p.node_id) SELECT {columns},{match_rank_expr} AS match_rank,COALESCE(b.graph_boost,0) AS graph_boost,r.search_total FROM {from} CROSS JOIN result_total r LEFT JOIN boosted b ON b.node_id=n.node_id WHERE {where_clause} ORDER BY match_rank,CASE WHEN b.node_id IS NULL THEN 1 ELSE 0 END,COALESCE(b.graph_boost,0) DESC,pd.path,n.line,n.id"
        )
    } else {
        format!(
            "SELECT {columns},0 AS match_rank,0.0 AS bm25_rank,0 AS graph_boost FROM {from} WHERE {where_clause} ORDER BY pd.path,n.line,n.id"
        )
    };
    let mut params: Vec<&dyn ToSql> = Vec::with_capacity(7);
    if !fts_query.is_empty() {
        params.push(&fts_query);
    }
    params.extend([
        &pattern as &dyn ToSql,
        &kind,
        &path,
        &path_start,
        &path_end,
        &include_docs,
    ]);
    if ranked {
        ranked_search_page(conn, &sql, &params, options)
    } else {
        paging::query(conn, &sql, &params, options)
    }
}

fn run_wildcard_search_query(
    conn: &Connection,
    pattern: &str,
    ctx: &ExactSearchContext<'_>,
) -> Result<Value> {
    let ExactSearchContext {
        columns,
        graph_boost,
        kind,
        path,
        path_start,
        path_end,
        include_docs,
        options,
        ..
    } = *ctx;
    let like = escape_like(pattern).replace('*', "%");
    if let Some(prefix) = pattern
        .strip_suffix('*')
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        let query = format!("\"{prefix}\"*");
        let scope_filter = scope_filter_sql(2, 4, 7);
        let from = format!(
            "FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE node_search MATCH ?3 AND (n.name LIKE ?1 ESCAPE '\\' OR n.qualname LIKE ?1 ESCAPE '\\' OR n.qualname LIKE '%::' || ?1 ESCAPE '\\' OR n.qualname LIKE '%.' || ?1 ESCAPE '\\') AND {scope_filter}"
        );
        let probe = format!("SELECT n.node_id {from} LIMIT 1001");
        let sql = format!(
            "WITH fts_scored AS MATERIALIZED (
                SELECT n.node_id,pd.path,n.line,n.id,(CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 500 ELSE 0 END - bm25(node_search)) AS score
                {from}
                ORDER BY score DESC,pd.path,n.line,n.id
            ), total_probe AS MATERIALIZED ({probe}), result_total AS (
                SELECT CASE WHEN (SELECT count(*) FROM total_probe)>=1001 THEN NULL
                    ELSE (SELECT count(*) FROM total_probe) END AS search_total
            ), graph_window AS MATERIALIZED (
                SELECT node_id,score FROM fts_scored ORDER BY score DESC,path,line,id LIMIT 50
            ), boosted AS MATERIALIZED (
                SELECT p.node_id,{graph_boost} AS graph_boost
                FROM graph_window p JOIN nodes n ON n.node_id=p.node_id
            ), ranked AS (
                SELECT f.node_id,f.score,CASE WHEN b.node_id IS NULL THEN 1 ELSE 0 END AS graph_group,
                       COALESCE(b.graph_boost,0) AS graph_boost,t.search_total
                FROM fts_scored f LEFT JOIN boosted b ON b.node_id=f.node_id CROSS JOIN result_total t
            ) SELECT {columns},CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 'name_pattern' ELSE 'qualified_pattern' END match_reason,c.graph_boost,c.search_total
            FROM ranked c JOIN nodes n ON n.node_id=c.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
            ORDER BY c.graph_group,(c.score+c.graph_boost) DESC,pd.path,n.line,n.id"
        );
        return ranked_search_page(
            conn,
            &sql,
            &[&like, &kind, &query, &path, &path_start, &path_end, &include_docs],
            options,
        );
    }
    let scope_filter = scope_filter_sql(2, 3, 6);
    paging::query(
        conn,
        &format!(
            "SELECT {columns},CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 'name_pattern' ELSE 'qualified_pattern' END match_reason FROM nodes n JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualname LIKE ?1 ESCAPE '\\') AND {scope_filter} ORDER BY CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 500 ELSE 0 END DESC,pd.path,n.line,n.id"
        ),
        &[&like, &kind, &path, &path_start, &path_end, &include_docs],
        options,
    )
}

fn try_type_filter_search(
    conn: &Connection,
    pattern: &str,
    columns: &str,
    (kind, path, path_start, path_end, include_docs): (&str, &str, &str, &str, bool),
    options: &QueryOptions,
) -> Result<Option<Value>> {
    let Some((field, value)) = pattern
        .strip_prefix("returns:")
        .map(|value| ("return", value))
        .or_else(|| {
            pattern
                .strip_prefix("takes:")
                .map(|value| ("parameter", value))
        })
    else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() || value.contains('*') || value.contains('\0') {
        bail!("type filter must contain a type fragment without wildcards");
    }
    let query = format!("%{}%", escape_like(value));
    let predicate = if field == "return" {
        format!(
            "json_extract(n.details,'$.{}') LIKE ?1 ESCAPE '\\'",
            NodeDetailKey::RETURN_TYPE
        )
    } else {
        "EXISTS (SELECT 1 FROM json_each(n.details,'$.param_types') p WHERE p.type='text' AND p.value LIKE ?1 ESCAPE '\\')".to_owned()
    };
    let scope_filter = scope_filter_sql(2, 3, 6);
    let sql = format!(
        "SELECT {columns} FROM nodes n JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE {predicate} AND n.kind IN ('{}','{}') AND {scope_filter} ORDER BY pd.path,n.line,n.id",
        NodeKind::Function.as_str(),
        NodeKind::Method.as_str(),
    );
    let nodes = paging::query(
        conn,
        &sql,
        &[&query, &kind, &path, &path_start, &path_end, &include_docs],
        options,
    )?;
    Ok(Some(json!({"pattern":pattern,"nodes":nodes})))
}

fn try_default_exact_search(
    conn: &Connection,
    pattern: &str,
    exact_ctx: &ExactSearchContext<'_>,
) -> Result<Option<Value>> {
    let candidates = run_exact_search_query(conn, exact_ctx, true)?;
    let tokens = fts_tokens(pattern);
    let has_type_match = if !tokens.is_empty() && candidates["total"].as_u64() != Some(0) {
        let term = |prefix| {
            tokens
                .iter()
                .map(|token| format!("\"{prefix}{token}\""))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let query = format!("({}) OR ({})", term("acddtakes"), term("acddreturns"));
        let budget = QueryBudget::new(conn);
        let scope_filter = scope_filter_sql(2, 3, 6);
        let matched = conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE node_search MATCH ?1 AND {scope_filter})"
            ),
            rusqlite::params![
                query,
                exact_ctx.kind,
                exact_ctx.path,
                exact_ctx.path_start,
                exact_ctx.path_end,
                exact_ctx.include_docs
            ],
            |row| row.get::<_, bool>(0),
        )?;
        budget.check()?;
        matched
    } else {
        false
    };
    Ok((candidates["total"].as_u64() != Some(0) && !has_type_match).then_some(candidates))
}

/// Search indexed symbols using one bounded options contract.
pub fn search_with_options(
    conn: &Connection,
    pattern: &str,
    options: &SearchOptions<'_>,
) -> Result<Value> {
    let SearchOptions {
        kind,
        path,
        include_docs,
        exact,
        page: options,
    } = *options;
    let pattern = pattern.trim();
    if pattern.is_empty()
        || pattern.len() > 1024
        || (pattern.chars().all(|c| c == '*') && pattern != "*")
    {
        bail!("pattern must contain a symbol fragment and be at most 1024 bytes");
    }
    let kind = kind.unwrap_or("");
    let path = path.map(|value| {
        strip_selector_uri_prefix(value.trim())
            .trim_start_matches("./")
            .trim_end_matches('/')
    });
    if let Some(path) = path {
        if path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            bail!("path must be a nonempty workspace-relative file or directory");
        }
    }
    let path = path.unwrap_or("");
    if pattern == "*" && kind.is_empty() && path.is_empty() {
        bail!("pattern must contain a symbol fragment, or use '*' with a path or kind scope");
    }
    let (path_start, path_end) = path_bounds(path);
    let columns = paging::nodes_with_path("n", options.detail, "pd.path");
    let graph_boost = format!(
        "CASE WHEN EXISTS(SELECT 1 FROM edges e WHERE e.dst_hash=n.node_hash AND e.kind NOT IN('{}','{}','{}')) THEN 50 ELSE 0 END",
        EdgeKind::Contains.as_str(),
        EdgeKind::Documents.as_str(),
        EdgeKind::ReferencesDoc.as_str(),
    );
    if let Some(res) = try_type_filter_search(
        conn,
        pattern,
        &columns,
        (kind, path, &path_start, &path_end, include_docs),
        options,
    )? {
        return Ok(res);
    }
    let exact_ctx = ExactSearchContext {
        columns: &columns,
        graph_boost: &graph_boost,
        pattern,
        kind,
        path,
        path_start: &path_start,
        path_end: &path_end,
        include_docs,
        options,
    };
    let default_exact = if !exact && !pattern.contains('*') {
        try_default_exact_search(conn, pattern, &exact_ctx)?
    } else {
        None
    };
    let mut nodes = if let Some(candidates) = default_exact {
        candidates
    } else if exact && pattern != "*" {
        run_exact_search_query(conn, &exact_ctx, false)?
    } else if pattern.contains('*') {
        run_wildcard_search_query(conn, pattern, &exact_ctx)?
    } else {
        let terms = quoted_fts_terms(&fts_tokens(pattern));
        if terms.is_empty() {
            bail!("pattern must contain a symbol fragment");
        }
        let prefix = format!("{}%", escape_like(pattern));
        let candidate_limit = i64::try_from(
            options
                .offset
                .saturating_add(options.limit)
                .saturating_add(1)
                .max(500),
        )
        .context("search offset exceeds SQLite integer range")?;
        let structural_candidates = "SELECT node_id FROM nodes WHERE name LIKE ?7 ESCAPE '\\' UNION SELECT node_id FROM nodes WHERE qualname=?6 COLLATE NOCASE";
        let filters = scope_filter_sql(2, 3, 8);
        let structural_score = "CASE WHEN n.name=?6 COLLATE NOCASE OR n.qualname=?6 COLLATE NOCASE THEN 1000 WHEN n.name LIKE ?7 ESCAPE '\\' THEN 500 ELSE 0 END";
        let fts_score = "CASE WHEN n.name=?6 COLLATE NOCASE OR n.qualname=?6 COLLATE NOCASE THEN 1000 WHEN n.name LIKE ?7 ESCAPE '\\' THEN 500 ELSE 0 END + CASE WHEN ?11=1 AND n.node_hash IN (SELECT node_hash FROM fts_and_matches) THEN 250 ELSE 0 END - bm25(node_search)";
        let sql = format!(
            "WITH
            fts_and_matches AS MATERIALIZED (
                SELECT n.node_hash
                FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE ?11=1 AND node_search MATCH ?10 AND {filters}
            ),
            fts AS MATERIALIZED (
                SELECT n.node_id,pd.path,n.line,n.id,({fts_score}) AS score
                FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE node_search MATCH ?1 AND {filters}
                ORDER BY score DESC,pd.path,n.line,n.id LIMIT ?9
            ), fts_probe AS MATERIALIZED (
                SELECT n.node_id FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE node_search MATCH ?1 AND {filters} LIMIT 1001
            ), structural_all AS MATERIALIZED (
                SELECT n.node_id,pd.path,n.line,n.id,({structural_score}) AS score
                FROM ({structural_candidates}) s JOIN nodes n ON n.node_id=s.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE {filters}
            ), structural AS MATERIALIZED (
                SELECT node_id,path,line,id,score FROM structural_all
                ORDER BY score DESC,path,line,id LIMIT ?9
            ), candidates AS (
                SELECT node_id,max(score) AS score FROM (
                    SELECT node_id,score FROM fts UNION ALL SELECT node_id,score FROM structural
                ) GROUP BY node_id
            ), result_total AS (
                SELECT CASE WHEN (SELECT count(*) FROM fts_probe)>=1001 THEN NULL
                    ELSE (SELECT count(*) FROM (
                        SELECT node_id FROM fts_probe UNION SELECT node_id FROM structural_all
                    )) END AS search_total
            ), graph_window AS MATERIALIZED (
                SELECT c.node_id,c.score FROM candidates c JOIN nodes n ON n.node_id=c.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
                ORDER BY c.score DESC,pd.path,n.line,n.id LIMIT 50
            ), boosted AS MATERIALIZED (
                SELECT p.node_id,{graph_boost} AS graph_boost
                FROM graph_window p JOIN nodes n ON n.node_id=p.node_id
            ), ranked AS (
                SELECT c.node_id,c.score,CASE WHEN b.node_id IS NULL THEN 1 ELSE 0 END AS graph_group,
                       COALESCE(b.graph_boost,0) AS graph_boost,t.search_total
                FROM candidates c LEFT JOIN boosted b ON b.node_id=c.node_id CROSS JOIN result_total t
            ) SELECT {columns},CASE WHEN n.name=?6 COLLATE NOCASE THEN 0
                WHEN n.qualname=?6 COLLATE NOCASE THEN 2
                WHEN n.name LIKE ?7 ESCAPE '\\' THEN 1
                WHEN n.name LIKE '%' || ?7 ESCAPE '\\' THEN 3 ELSE 4 END AS match_rank
            ,c.graph_boost,c.search_total FROM ranked c JOIN nodes n ON n.node_id=c.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
            ORDER BY c.graph_group,(c.score+c.graph_boost) DESC,pd.path,n.line,n.id"
        );
        let and_query = terms.join(" AND ");
        let or_query = terms.join(" OR ");
        let is_natural_query =
            !pattern.starts_with('@') && pattern.chars().any(char::is_whitespace);
        let apply_and_bonus = i64::from(is_natural_query && terms.len() > 1);
        let run_query = |query: &str, and_bonus: i64| {
            ranked_search_page(
                conn,
                &sql,
                &[
                    &query,
                    &kind,
                    &path,
                    &path_start,
                    &path_end,
                    &pattern,
                    &prefix,
                    &include_docs,
                    &candidate_limit,
                    &and_query,
                    &and_bonus,
                ],
                options,
            )
        };
        if is_natural_query {
            run_query(&or_query, apply_and_bonus)?
        } else {
            let rows = run_query(&and_query, 0)?;
            // An exhausted page must not switch a matching AND query to OR results.
            if terms.len() > 1 && !pattern.starts_with('@') && rows["total"] == 0 {
                run_query(&or_query, 0)?
            } else {
                rows
            }
        }
    };
    if let Some(items) = nodes["items"].as_array_mut() {
        for item in items {
            if options.detail == Detail::Compact && item["match_reason"] == "name_pattern" {
                item.as_object_mut()
                    .context("invalid search result")?
                    .remove("match_reason");
            }
            if let Some(rank) = item
                .as_object_mut()
                .and_then(|item| item.remove("match_rank"))
            {
                let reason = match rank.as_u64() {
                    Some(0) => "exact_name",
                    Some(1) => "name_prefix",
                    Some(2) => "exact_qualified_name",
                    Some(3) => "name_fragment",
                    _ => "indexed_text",
                };
                if options.detail == Detail::Full || !matches!(rank.as_u64(), Some(0..=1 | 3)) {
                    item["match_reason"] = json!(reason);
                }
                item.as_object_mut()
                    .context("invalid search result")?
                    .remove("bm25_rank");
                item.as_object_mut()
                    .context("invalid search result")?
                    .remove("graph_boost");
            }
        }
    }
    Ok(json!({"pattern":pattern,"nodes":nodes}))
}

fn ranked_search_page(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let limit = i64::try_from(options.limit.saturating_add(1))
        .context("search limit exceeds SQLite integer range")?;
    let offset = options.offset as i64;
    let mut bindings = params.to_vec();
    bindings.extend([&limit as &dyn ToSql, &offset]);
    let limit_param = params.len() + 1;
    let mut items = rows(
        conn,
        &format!("{sql} LIMIT ?{} OFFSET ?{}", limit_param, limit_param + 1),
        &bindings,
        options.limit.saturating_add(1),
    )?;
    let total = if let Some(total) = items.first().and_then(|item| item.get("search_total")) {
        total.clone()
    } else if options.offset == 0 {
        json!(0)
    } else {
        let first_limit = 1_i64;
        let first_offset = 0_i64;
        let mut first_page_bindings = params.to_vec();
        first_page_bindings.extend([
            &first_limit as &dyn ToSql,
            &first_offset as &dyn ToSql,
        ]);
        let first_page = rows(
            conn,
            &format!(
                "SELECT * FROM ({sql}) LIMIT ?{} OFFSET ?{}",
                params.len() + 1,
                params.len() + 2
            ),
            &first_page_bindings,
            1,
        )?;
        first_page
            .first()
            .and_then(|item| item.get("search_total"))
            .cloned()
            .unwrap_or_else(|| json!(0))
    };
    for item in &mut items {
        item.as_object_mut()
            .context("invalid search result")?
            .remove("search_total");
    }
    let has_more = items.len() > options.limit;
    items.truncate(options.limit);
    Ok(json!({
        "total": total,
        "offset": options.offset,
        "limit": options.limit,
        "has_more": has_more,
        "next_offset": if has_more { Some(options.offset.saturating_add(items.len())) } else { None },
        "items": items,
        "generation": generation
    }))
}

struct TextMatchPreviewParams<'a> {
    conn: &'a Connection,
    root: &'a Path,
    pattern: &'a str,
    previews_enabled: bool,
    preview_hit_limit: usize,
    preview_bytes_per_hit: usize,
}

fn apply_indexed_text_match_preview(
    params: &TextMatchPreviewParams<'_>,
    index: usize,
    object: &mut serde_json::Map<String, Value>,
    source_cache: &mut HashMap<String, Option<String>>,
    source_loader: &mut VerifiedSourceCache,
) -> Option<(String, Value)> {
    let (Some(path), Some(start), Some(end)) = (
        object
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned),
        object
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok()),
        object
            .get("end_line")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok()),
    ) else {
        return None;
    };
    let cached = source_cache
        .entry(path.clone())
        .or_insert_with(|| source_loader.read(params.conn, params.root, &path).ok());
    let text = cached.as_deref()?;
    let matched = resolve_indexed_text_match(
        text,
        start,
        end,
        params.pattern,
        params.preview_bytes_per_hit,
    )?;
    object.insert("line".into(), json!(matched.line));
    object.insert("match_snippet".into(), json!(matched.snippet));
    if !params.previews_enabled || index >= params.preview_hit_limit {
        return None;
    }
    let (preview_src, win_start, win_end) = matched.preview?;
    Some((
        preview_src,
        json!({
            "start_line": win_start,
            "end_line": win_end,
            "body_lines": win_end.saturating_sub(win_start) + 1,
            "has_more": false,
        }),
    ))
}

/// Adds indexed signatures and bounded navigation previews to one search page.
pub fn search_results_with_navigation(
    conn: &Connection,
    root: &Path,
    mut result: Value,
    include_preview: bool,
) -> Result<Value> {
    const SIGNATURE_CHAR_LIMIT: usize = 512;
    const PREVIEW_HIT_LIMIT: usize = 3;
    const PREVIEW_BYTES_PER_HIT: usize = 4096;

    let pattern = result["pattern"].as_str().unwrap_or("").to_owned();
    let has_small_total = result["nodes"]["total"]
        .as_u64()
        .is_some_and(|total| total <= PREVIEW_HIT_LIMIT as u64);
    let Some(items) = result["nodes"]["items"].as_array_mut() else {
        return Ok(result);
    };
    if items.is_empty() {
        return Ok(result);
    }

    let mut source_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut source_loader = VerifiedSourceCache::new();

    let ids = items
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    let serialized_ids = serde_json::to_string(&ids)?;
    let metadata_rows = rows(
        conn,
        &format!(
            "SELECT n.id,json_extract(n.details,'$.{}') AS signature,CASE WHEN n.is_test THEN 1 ELSE 0 END AS is_test,json_extract(n.details,'$.{}') AS docstring FROM nodes n JOIN json_each(?1) requested ON requested.value=n.id",
            NodeDetailKey::SIGNATURE,
            NodeDetailKey::DOC,
        ),
        &[&serialized_ids],
        ids.len(),
    )?;
    let metadata = metadata_rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row["id"].as_str()?.to_owned(),
                (
                    row["signature"].as_str().map(str::to_owned),
                    row["is_test"].as_i64().map(|value| value != 0),
                    row["docstring"].as_str().map(str::to_owned),
                ),
            ))
        })
        .collect::<HashMap<_, _>>();

    let previews_enabled = include_preview || has_small_total;
    let markdown = LanguageId::Markdown.as_str();
    for (index, item) in items.iter_mut().enumerate() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .context("search result has no symbol id")?
            .to_owned();
        let (signature, is_test, docstring) =
            metadata.get(&id).cloned().unwrap_or((None, None, None));
        object.insert(
            "signature".into(),
            signature
                .map(|value| {
                    let mut bounded = value.chars().take(SIGNATURE_CHAR_LIMIT).collect::<String>();
                    if value.chars().count() > SIGNATURE_CHAR_LIMIT {
                        bounded.push('…');
                    }
                    json!(bounded)
                })
                .unwrap_or(Value::Null),
        );
        object.insert(
            "inspect_selector".into(),
            json!({"selector":id,"show_source":true}),
        );

        if let Some(is_test) = is_test {
            object.insert("is_test".into(), json!(is_test));
            let classification =
                if object.get("language").and_then(Value::as_str) == Some(markdown) {
                    "documentation"
                } else if is_test {
                    "test"
                } else {
                    "production"
                };
            object.insert("classification".into(), json!(classification));
        }
        if let Some(docstring) = docstring.as_deref().and_then(first_docstring_sentence) {
            object.insert("docstring_first_sentence".into(), json!(docstring));
        }

        let is_indexed_text =
            object.get("match_reason").and_then(Value::as_str) == Some("indexed_text");
        let is_markdown = object.get("language").and_then(Value::as_str) == Some(markdown);
        let matched_window = if is_indexed_text && !is_markdown && !pattern.trim().is_empty() {
            apply_indexed_text_match_preview(
                &TextMatchPreviewParams {
                    conn,
                    root,
                    pattern: &pattern,
                    previews_enabled,
                    preview_hit_limit: PREVIEW_HIT_LIMIT,
                    preview_bytes_per_hit: PREVIEW_BYTES_PER_HIT,
                },
                index,
                object,
                &mut source_cache,
                &mut source_loader,
            )
        } else {
            None
        };

        if index == 0 {
            object.insert("graph_pulse".into(), search_graph_pulse(conn, &id));
        }
        if !previews_enabled || index >= PREVIEW_HIT_LIMIT {
            continue;
        }
        if let Some((preview_src, preview_meta)) = matched_window {
            object.insert("source".into(), json!(preview_src));
            object.insert("source_preview".into(), preview_meta);
            continue;
        }
        if object.get("kind").and_then(Value::as_str) == Some("file") {
            // File rows span the whole file and their start line already resolves to line 1;
            // skip symbol_range AST recovery rather than falling back to line 1 of an enclosing module container.
            continue;
        }
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .context("search result has no path")?;
        let Some(text) = source_cache
            .entry(path.to_owned())
            .or_insert_with(|| source_loader.read(conn, root, path).ok())
            .clone()
        else {
            continue;
        };
        let node = json!({
            "id": id,
            "kind": object.get("kind"),
            "name": object.get("name"),
            "qualname": object.get("qualname"),
            "path": path,
            "line": object.get("line"),
            "end_line": object.get("end_line"),
            "language": object.get("language")
        });
        let options = SourceOptions {
            enabled: true,
            leading_lines: 5,
            max_body_lines: 15,
            offset: 0,
        };
        if let Ok((source, preview)) =
            source_preview(conn, &node, &text, &options, PREVIEW_BYTES_PER_HIT)
        {
            object.insert("source".into(), json!(source));
            object.insert("source_preview".into(), preview);
        }
    }
    Ok(result)
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
