use super::projection::{
    query_typed_ranked_page, query_typed_window_page, typed_search_columns, SearchRowTrailer,
    TypedSearchPage,
};
use crate::db::reader::{
    selectors::{path_bounds, strip_selector_uri_prefix},
    sql::QueryBudget,
};
use crate::db::symbols::types::SearchOptions;
use crate::models::{
    details::NodeDetailKey,
    tags::{EdgeKind, LanguageId, NodeKind},
};
use acdd_core::response::{QueryOptions, SymbolClassification};
use anyhow::{bail, Result};
use rusqlite::{Connection, ToSql};

const RRF_K: &str = "60.0";
const RRF_STRUCTURAL_WEIGHT: &str = "2.0";
const RRF_BM25_WEIGHT: &str = "1.0";
const RRF_AND_MATCH_WEIGHT: &str = "1.0";
const RRF_GRAPH_WEIGHT: &str = "0.75";
const RRF_CANDIDATE_LIMIT: usize = 500;
const GRAPH_DEGREE_WINDOW: usize = 50;

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

fn scope_filter_sql(
    kind_idx: usize,
    path_idx: usize,
    include_docs_idx: usize,
    class_idx: usize,
) -> String {
    let function = NodeKind::Function.as_str();
    let method = NodeKind::Method.as_str();
    let markdown = LanguageId::Markdown.as_str();
    let p_start = path_idx + 1;
    let p_end = path_idx + 2;
    let prod = SymbolClassification::Production.sql_code();
    let test = SymbolClassification::Test.sql_code();
    let docs = SymbolClassification::Documentation.sql_code();
    format!(
        "(?{kind_idx}='' OR n.kind=?{kind_idx} OR (?{kind_idx}='{method}' AND n.kind='{function}') OR (?{kind_idx}='{function}' AND n.kind='{method}')) \
         AND (?{path_idx}='' OR pd.path=?{path_idx} OR (pd.path>=?{p_start} AND pd.path<?{p_end})) \
         AND (?{include_docs_idx}=1 OR ?{class_idx}={docs} OR n.language!='{markdown}') \
         AND (?{class_idx} IS NULL \
              OR (?{class_idx}={prod} AND n.is_test=0 AND n.language!='{markdown}') \
              OR (?{class_idx}={test} AND n.is_test=1 AND n.language!='{markdown}') \
              OR (?{class_idx}={docs} AND n.language='{markdown}'))"
    )
}

fn graph_degree_ctes_sql() -> String {
    let contains = EdgeKind::Contains.as_str();
    let documents = EdgeKind::Documents.as_str();
    let references_doc = EdgeKind::ReferencesDoc.as_str();
    format!(
        "graph_degrees AS MATERIALIZED (
            SELECT g.node_id,
                   (
                       (SELECT count(*) FROM edges e WHERE e.dst_hash=g.node_hash AND e.kind NOT IN('{contains}','{documents}','{references_doc}'))
                       + (SELECT count(*) FROM edges e WHERE e.src_hash=g.node_hash AND e.kind NOT IN('{contains}','{documents}','{references_doc}'))
                   ) AS graph_degree
            FROM graph_window g
        ), graph_ranked AS MATERIALIZED (
            SELECT node_id, RANK() OVER (ORDER BY graph_degree DESC) AS graph_rank
            FROM graph_degrees
            WHERE graph_degree > 0
        )"
    )
}

struct ExactSearchContext<'a> {
    columns: &'a str,
    pattern: &'a str,
    kind: &'a str,
    classification_code: Option<i64>,
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
) -> Result<TypedSearchPage> {
    let ExactSearchContext {
        columns,
        pattern,
        kind,
        classification_code,
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
            scope_filter_sql(2, 3, 6, 7),
        )
    } else {
        (
            "node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id",
            "node_search MATCH ?1 AND (n.name=?2 COLLATE NOCASE OR n.qualname=?2 COLLATE NOCASE) AND ".to_owned(),
            "CASE WHEN n.name=?2 COLLATE NOCASE THEN 0 ELSE 2 END",
            scope_filter_sql(3, 4, 7, 8),
        )
    };
    let where_clause = format!("{match_pred}{scope_filter}");
    let sql = if ranked {
        let graph_ctes = graph_degree_ctes_sql();
        format!(
            "WITH exact_probe AS MATERIALIZED (
                SELECT n.node_id,n.node_hash,{match_rank_expr} AS match_rank,pd.path,n.line,n.id
                FROM {from} WHERE {where_clause}
                ORDER BY match_rank ASC,pd.path ASC,n.line ASC,n.id ASC LIMIT 1001
            ), result_total AS (
                SELECT CASE WHEN count(*)>=1001 THEN NULL ELSE count(*) END AS search_total FROM exact_probe
            ), graph_window AS MATERIALIZED (
                SELECT node_id,node_hash FROM exact_probe
                ORDER BY match_rank ASC,path ASC,line ASC,id ASC LIMIT {GRAPH_DEGREE_WINDOW}
            ), {graph_ctes}, scored AS (
                SELECT n.node_id,{match_rank_expr} AS match_rank,
                       ({RRF_STRUCTURAL_WEIGHT} / ({RRF_K} + CASE WHEN {match_rank_expr}=0 THEN 1 ELSE 2 END)
                        + CASE WHEN gr.graph_rank IS NOT NULL THEN {RRF_GRAPH_WEIGHT} / ({RRF_K} + gr.graph_rank) ELSE 0.0 END) AS rrf_score
                FROM {from}
                LEFT JOIN graph_ranked gr ON gr.node_id=n.node_id
                WHERE {where_clause}
            ) SELECT {columns},s.match_rank,r.search_total
            FROM scored s
            JOIN nodes n ON n.node_id=s.node_id
            JOIN path_dictionary pd ON pd.path_id=n.path_id
            CROSS JOIN result_total r
            ORDER BY s.rrf_score DESC,s.match_rank ASC,pd.path ASC,n.line ASC,n.id ASC"
        )
    } else {
        format!(
            "SELECT {columns},0 AS match_rank FROM {from} WHERE {where_clause} ORDER BY pd.path,n.line,n.id"
        )
    };
    let mut params: Vec<&dyn ToSql> = Vec::with_capacity(8);
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
        &classification_code,
    ]);
    if ranked {
        query_typed_ranked_page(conn, &sql, &params, options, SearchRowTrailer::Rank)
    } else {
        query_typed_window_page(conn, &sql, &params, options, SearchRowTrailer::Rank)
    }
}

fn run_wildcard_search_query(
    conn: &Connection,
    pattern: &str,
    ctx: &ExactSearchContext<'_>,
) -> Result<TypedSearchPage> {
    let ExactSearchContext {
        columns,
        kind,
        classification_code,
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
        let scope_filter = scope_filter_sql(2, 4, 7, 8);
        let from = format!(
            "FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE node_search MATCH ?3 AND (n.name LIKE ?1 ESCAPE '\\' OR n.qualname LIKE ?1 ESCAPE '\\' OR n.qualname LIKE '%::' || ?1 ESCAPE '\\' OR n.qualname LIKE '%.' || ?1 ESCAPE '\\') AND {scope_filter}"
        );
        let probe = format!("SELECT n.node_id {from} LIMIT 1001");
        let graph_ctes = graph_degree_ctes_sql();
        let sql = format!(
            "WITH fts_raw AS MATERIALIZED (
                SELECT n.node_id,n.node_hash,pd.path,n.line,n.id,
                       CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 1 ELSE 2 END AS match_rank,
                       bm25(node_search) AS bm25_val
                {from}
            ), total_probe AS MATERIALIZED ({probe}), result_total AS (
                SELECT CASE WHEN (SELECT count(*) FROM total_probe)>=1001 THEN NULL
                    ELSE (SELECT count(*) FROM total_probe) END AS search_total
            ), pre_scored AS MATERIALIZED (
                SELECT node_id,node_hash,path,line,id,match_rank,
                       ({RRF_STRUCTURAL_WEIGHT} / ({RRF_K} + RANK() OVER (ORDER BY match_rank ASC))
                        + {RRF_BM25_WEIGHT} / ({RRF_K} + RANK() OVER (ORDER BY bm25_val ASC))) AS base_rrf
                FROM fts_raw
            ), graph_window AS MATERIALIZED (
                SELECT node_id,node_hash FROM pre_scored
                ORDER BY base_rrf DESC,match_rank ASC,path ASC,line ASC,id ASC LIMIT {GRAPH_DEGREE_WINDOW}
            ), {graph_ctes}, scored AS (
                SELECT p.node_id,p.match_rank,
                       (p.base_rrf
                        + CASE WHEN gr.graph_rank IS NOT NULL THEN {RRF_GRAPH_WEIGHT} / ({RRF_K} + gr.graph_rank) ELSE 0.0 END) AS rrf_score,
                       t.search_total
                FROM pre_scored p
                LEFT JOIN graph_ranked gr ON gr.node_id=p.node_id
                CROSS JOIN result_total t
            ) SELECT {columns},CASE WHEN c.match_rank=1 THEN 'name_pattern' ELSE 'qualified_pattern' END AS match_reason,c.search_total
            FROM scored c JOIN nodes n ON n.node_id=c.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
            ORDER BY c.rrf_score DESC,c.match_rank ASC,pd.path ASC,n.line ASC,n.id ASC"
        );
        return query_typed_ranked_page(
            conn,
            &sql,
            &[
                &like,
                &kind,
                &query,
                &path,
                &path_start,
                &path_end,
                &include_docs,
                &classification_code,
            ],
            options,
            SearchRowTrailer::Reason,
        );
    }
    let scope_filter = scope_filter_sql(2, 3, 6, 7);
    query_typed_window_page(
        conn,
        &format!(
            "SELECT {columns},CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 'name_pattern' ELSE 'qualified_pattern' END match_reason FROM nodes n JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualname LIKE ?1 ESCAPE '\\') AND {scope_filter} ORDER BY CASE WHEN n.name LIKE ?1 ESCAPE '\\' THEN 500 ELSE 0 END DESC,pd.path,n.line,n.id"
        ),
        &[
            &like,
            &kind,
            &path,
            &path_start,
            &path_end,
            &include_docs,
            &classification_code,
        ],
        options,
        SearchRowTrailer::Reason,
    )
}

fn try_type_filter_search(
    conn: &Connection,
    pattern: &str,
    columns: &str,
    (kind, classification_code, path, path_start, path_end, include_docs): (
        &str,
        Option<i64>,
        &str,
        &str,
        &str,
        bool,
    ),
    options: &QueryOptions,
) -> Result<Option<TypedSearchPage>> {
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
    let scope_filter = scope_filter_sql(2, 3, 6, 7);
    let sql = format!(
        "SELECT {columns} FROM nodes n JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE {predicate} AND n.kind IN ('{}','{}') AND {scope_filter} ORDER BY pd.path,n.line,n.id",
        NodeKind::Function.as_str(),
        NodeKind::Method.as_str(),
    );
    let page = query_typed_window_page(
        conn,
        &sql,
        &[
            &query,
            &kind,
            &path,
            &path_start,
            &path_end,
            &include_docs,
            &classification_code,
        ],
        options,
        SearchRowTrailer::None,
    )?;
    Ok(Some(page))
}

fn try_default_exact_search(
    conn: &Connection,
    pattern: &str,
    exact_ctx: &ExactSearchContext<'_>,
) -> Result<Option<TypedSearchPage>> {
    let candidates = run_exact_search_query(conn, exact_ctx, true)?;
    let tokens = fts_tokens(pattern);
    let has_type_match = if !tokens.is_empty() && candidates.total != Some(0) {
        let term = |prefix| {
            tokens
                .iter()
                .map(|token| format!("\"{prefix}{token}\""))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        let query = format!("({}) OR ({})", term("acddtakes"), term("acddreturns"));
        let budget = QueryBudget::new(conn);
        let scope_filter = scope_filter_sql(2, 3, 6, 7);
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
                exact_ctx.include_docs,
                exact_ctx.classification_code
            ],
            |row| row.get::<_, bool>(0),
        )?;
        budget.check()?;
        matched
    } else {
        false
    };
    Ok((candidates.total != Some(0) && !has_type_match).then_some(candidates))
}

pub(super) fn search_typed(
    conn: &Connection,
    pattern: &str,
    options: &SearchOptions<'_>,
) -> Result<(String, TypedSearchPage)> {
    let SearchOptions {
        kind,
        classification,
        path,
        include_docs,
        exact,
        page: options,
    } = *options;
    let classification_code = classification.map(SymbolClassification::sql_code);
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
    if pattern == "*" && kind.is_empty() && path.is_empty() && classification.is_none() {
        bail!("pattern must contain a symbol fragment, or use '*' with a path, kind, or classification scope");
    }
    let (path_start, path_end) = path_bounds(path);
    let columns = typed_search_columns(options.detail);
    if let Some(page) = try_type_filter_search(
        conn,
        pattern,
        columns,
        (
            kind,
            classification_code,
            path,
            &path_start,
            &path_end,
            include_docs,
        ),
        options,
    )? {
        return Ok((pattern.to_owned(), page));
    }
    let exact_ctx = ExactSearchContext {
        columns,
        pattern,
        kind,
        classification_code,
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
    let page = if let Some(candidates) = default_exact {
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
        // Bound the scoring shortlist independently of the requested page.
        // Keep all matching IDs in the domain that the final sort paginates.
        let structural_candidates = "SELECT node_id FROM nodes WHERE name LIKE ?7 ESCAPE '\\' UNION SELECT node_id FROM nodes WHERE qualname=?6 COLLATE NOCASE";
        let filters = scope_filter_sql(2, 3, 8, 11);
        let match_rank_expr = "CASE WHEN n.name=?6 COLLATE NOCASE THEN 0 \
            WHEN n.qualname=?6 COLLATE NOCASE THEN 2 \
            WHEN n.name LIKE ?7 ESCAPE '\\' THEN 1 \
            WHEN n.name LIKE '%' || ?7 ESCAPE '\\' THEN 3 ELSE 4 END";
        let module_kind = NodeKind::Module.as_str();
        let graph_ctes = graph_degree_ctes_sql();
        let sql = format!(
            "WITH
            fts_and_raw AS MATERIALIZED (
                SELECT n.node_id,bm25(node_search) AS bm25_val
                FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE ?10=1 AND node_search MATCH ?9 AND {filters}
                ORDER BY bm25_val ASC,pd.path ASC,n.line ASC,n.id ASC LIMIT {RRF_CANDIDATE_LIMIT}
            ), fts_and_ranked AS MATERIALIZED (
                SELECT node_id,RANK() OVER (ORDER BY bm25_val ASC) AS and_rank
                FROM fts_and_raw
            ), fts_matches AS MATERIALIZED (
                SELECT n.node_id,bm25(node_search) AS bm25_val,pd.path,n.line,n.id
                FROM node_search JOIN nodes n ON n.node_hash=node_search.rowid JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE node_search MATCH ?1 AND {filters}
            ), fts_raw AS MATERIALIZED (
                SELECT node_id,bm25_val FROM fts_matches
                ORDER BY bm25_val ASC,path ASC,line ASC,id ASC LIMIT {RRF_CANDIDATE_LIMIT}
            ), fts_ranked AS MATERIALIZED (
                SELECT node_id,RANK() OVER (ORDER BY bm25_val ASC) AS bm25_rank
                FROM fts_raw
            ), structural_all AS MATERIALIZED (
                SELECT n.node_id,pd.path,n.line,n.id,{match_rank_expr} AS match_rank
                FROM ({structural_candidates}) s JOIN nodes n ON n.node_id=s.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
                WHERE {filters}
            ), structural AS MATERIALIZED (
                SELECT node_id FROM structural_all
                ORDER BY match_rank ASC,path ASC,line ASC,id ASC LIMIT {RRF_CANDIDATE_LIMIT}
            ), candidate_ids AS MATERIALIZED (
                SELECT node_id FROM fts_raw UNION SELECT node_id FROM structural
            ), matched_ids AS MATERIALIZED (
                SELECT node_id FROM fts_matches UNION SELECT node_id FROM structural_all
            ), result_total AS (
                SELECT CASE WHEN (SELECT count(*) FROM (SELECT 1 FROM fts_matches LIMIT 1001))>=1001 THEN NULL
                    ELSE (SELECT count(*) FROM matched_ids) END AS search_total
            ), base_candidates AS MATERIALIZED (
                SELECT n.node_id,n.node_hash,n.kind,pd.path,n.line,n.id,
                       {match_rank_expr} AS match_rank,
                       fr.bm25_rank,
                       fa.and_rank
                FROM candidate_ids c
                JOIN nodes n ON n.node_id=c.node_id
                JOIN path_dictionary pd ON pd.path_id=n.path_id
                LEFT JOIN fts_ranked fr ON fr.node_id=c.node_id
                LEFT JOIN fts_and_ranked fa ON fa.node_id=c.node_id
            ), pre_scored AS MATERIALIZED (
                SELECT node_id,node_hash,kind,path,line,id,match_rank,
                        (match_rank=4 AND kind='{module_kind}') AS module_text,
                       (CASE WHEN match_rank < 4 THEN {RRF_STRUCTURAL_WEIGHT} / ({RRF_K} + RANK() OVER (PARTITION BY (match_rank < 4) ORDER BY match_rank ASC)) ELSE 0.0 END
                        + CASE WHEN bm25_rank IS NOT NULL THEN {RRF_BM25_WEIGHT} / ({RRF_K} + bm25_rank) ELSE 0.0 END
                        + CASE WHEN and_rank IS NOT NULL THEN {RRF_AND_MATCH_WEIGHT} / ({RRF_K} + and_rank) ELSE 0.0 END) AS base_rrf
                FROM base_candidates
            ), graph_window AS MATERIALIZED (
                SELECT node_id,node_hash FROM pre_scored
                 ORDER BY module_text ASC,base_rrf DESC,
                         match_rank ASC,path ASC,line ASC,id ASC
                LIMIT {GRAPH_DEGREE_WINDOW}
            ), {graph_ctes}, matched_ranks AS (
                SELECT n.node_id,n.kind,{match_rank_expr} AS match_rank
                FROM matched_ids m JOIN nodes n ON n.node_id=m.node_id
            ), scored AS (
                SELECT m.node_id,m.match_rank,
                       (m.match_rank=4 AND m.kind='{module_kind}') AS module_text,
                       (COALESCE(p.base_rrf,0.0) + CASE WHEN gr.graph_rank IS NOT NULL THEN {RRF_GRAPH_WEIGHT} / ({RRF_K} + gr.graph_rank) ELSE 0.0 END) AS rrf_score
                FROM matched_ranks m
                LEFT JOIN pre_scored p ON p.node_id=m.node_id
                LEFT JOIN graph_ranked gr ON gr.node_id=m.node_id
            ) SELECT {columns},c.match_rank,t.search_total
            FROM scored c JOIN nodes n ON n.node_id=c.node_id JOIN path_dictionary pd ON pd.path_id=n.path_id
            CROSS JOIN result_total t
            ORDER BY c.module_text ASC,c.rrf_score DESC,c.match_rank ASC,pd.path ASC,n.line ASC,n.id ASC"
        );
        let and_query = terms.join(" AND ");
        let or_query = terms.join(" OR ");
        let is_natural_query =
            !pattern.starts_with('@') && pattern.chars().any(char::is_whitespace);
        let apply_and_bonus = i64::from(is_natural_query && terms.len() > 1);
        let run_query = |query: &str, and_bonus: i64| {
            query_typed_ranked_page(
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
                    &and_query,
                    &and_bonus,
                    &classification_code,
                ],
                options,
                SearchRowTrailer::Rank,
            )
        };
        if is_natural_query {
            run_query(&or_query, apply_and_bonus)?
        } else {
            let rows = run_query(&and_query, 0)?;
            // An exhausted page must not switch a matching AND query to OR results.
            if terms.len() > 1 && !pattern.starts_with('@') && rows.total == Some(0) {
                run_query(&or_query, 0)?
            } else {
                rows
            }
        }
    };
    Ok((pattern.to_owned(), page))
}
