use super::super::commitments::{read_metadata, MetadataKey};
use super::super::{cycles, paging, symbols};
use super::selectors::path_bounds;
use super::sql::rows;
use crate::models::tags::{CoverageStatus, EdgeKind};
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{bail, ensure, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Map, Value};
use std::path::Path;

pub(super) fn is_query_budget_exhaustion(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.to_string() == "graph query exceeds two second budget"
            || cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        rusqlite::Error::SqliteFailure(sqlite_error, _)
                            if matches!(
                                sqlite_error.code,
                                rusqlite::ErrorCode::OperationInterrupted | rusqlite::ErrorCode::TooBig
                            )
                    )
                })
    })
}

fn sql_schema_hint(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    if message.contains("no such column") || message.contains("no such table") {
        error.context(format!("{message}; Schema hint: nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id) joins path_dictionary(path_id, path) on path_id; edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count) joins nodes on src_hash=node_hash and dst_hash=node_hash; files(path, status, digest, size, language, is_test, generated, path_hash); doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant)."))
    } else {
        error
    }
}

/// Runs a bounded read-only analysis query with generation-aware pagination.
///
/// # Errors
///
/// Rejects unsupported SQL, stale generations, and result budget violations.
pub fn analyze_paged(
    conn: &Connection,
    target: &str,
    include_cycles: Option<bool>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let sql = target.trim();
    if let Some(statement_sql) = classify_read_only_sql(sql)? {
        let statement = conn
            .prepare(statement_sql)
            .map_err(|error| sql_schema_hint(error.into()))?;
        if !statement.readonly() {
            bail!("query must be read-only");
        }
        return Ok(
            json!({"rows":paging::query_opaque(conn, statement_sql, &[], options).map_err(sql_schema_hint)?, "ordering":"SQL order is preserved; include a deterministic ORDER BY for stable pagination."}),
        );
    }
    if sql.contains(';') {
        bail!("exactly one SELECT or WITH statement is allowed");
    }
    let path = target.trim_end_matches('/');
    let (prefix, end) = path_bounds(path);
    let indexed_path: bool = path.is_empty()
        || conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1 OR (path>=?2 AND path<?3))",
            rusqlite::params![path, prefix, end],
            |r| r.get(0),
        )?;
    if !indexed_path && matches!(path, "diagnostics" | "cycles") {
        bail!("analysis target '{path}' is interpreted as a path, not an analysis mode. Use target='' for workspace diagnostics; pass include_cycles=true to compute cycles for the workspace or a narrower indexed path");
    }
    let first = first_sql_keyword(sql)?.unwrap_or("");
    if !indexed_path
        && [
            "insert", "update", "delete", "drop", "alter", "create", "replace", "attach", "detach",
            "pragma", "vacuum", "reindex", "begin", "commit", "rollback",
        ]
        .iter()
        .any(|keyword| first.eq_ignore_ascii_case(keyword))
    {
        bail!("write or administrative SQL is not allowed");
    }
    if !indexed_path {
        bail!("PATH_NOT_INDEXED: '{path}' was not found in the indexed inventory");
    }
    let params: &[&dyn ToSql] = &[&path, &prefix, &end];
    let scope = "(?1='' OR path=?1 OR (path>=?2 AND path<?3))";
    let coverage_scope = "(?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3))";
    let unresolved = CoverageStatus::Unresolved.as_str();
    let ambiguous = CoverageStatus::Ambiguous.as_str();
    let resolved = CoverageStatus::Resolved.as_str();
    let external = CoverageStatus::External.as_str();
    let total_errors = paging::count(
        conn,
        &format!("SELECT count(*) FROM errors WHERE {scope}"),
        params,
    )?;
    let total_unresolved = paging::count(
        conn,
        &format!("SELECT count(*) FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id WHERE c.status IN('{unresolved}','{ambiguous}') AND {coverage_scope}"),
        params,
    )?;
    let total_external_imports = paging::count(
        conn,
        &format!("SELECT count(*) FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id WHERE c.status='{external}' AND {coverage_scope}"),
        params,
    )?;
    let exact_file: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
        [path],
        |r| r.get(0),
    )?;
    let mut result = json!({"target":target,"total_errors":total_errors,"total_unresolved":total_unresolved,"total_external_imports":total_external_imports,"scope":if exact_file {"file"} else {"summary"}});
    let public_surface_sql = format!(
        "WITH RECURSIVE covered(node_hash) AS (SELECT n.node_hash FROM nodes n WHERE n.is_test=1 AND json_extract(n.details,'$.test_role')='test_case' UNION SELECT e.dst_hash FROM edges e JOIN covered c ON c.node_hash=e.src_hash WHERE e.kind='{}') SELECT {} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE (?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3)) AND n.is_test=0 AND json_extract(n.details,'$.is_public')=1 AND NOT EXISTS(SELECT 1 FROM covered c WHERE c.node_hash=n.node_hash) ORDER BY p.path,n.line,n.id",
        EdgeKind::Calls.as_str(),
        paging::nodes_with_path("n", options.detail, "p.path")
    );
    result["untested_public_surface"] = paging::query(conn, &public_surface_sql, params, options)?;
    let status_sums = format!(
        "sum(CASE WHEN c.status='{resolved}' THEN c.records ELSE 0 END) resolved,sum(CASE WHEN c.status='{ambiguous}' THEN c.records ELSE 0 END) ambiguous,sum(CASE WHEN c.status='{unresolved}' THEN c.records ELSE 0 END) unresolved,sum(CASE WHEN c.status='{external}' THEN c.records ELSE 0 END) external_imports"
    );
    let (language_sql, language_params): (String, &[&dyn ToSql]) = if path.is_empty() {
        (format!("SELECT c.language,{status_sums} FROM coverage_language_counts c GROUP BY c.language ORDER BY c.language LIMIT 100"), &[])
    } else {
        (format!("SELECT c.language,{status_sums} FROM (SELECT path_id FROM path_dictionary WHERE path=?1 OR (path>=?2 AND path<?3)) p JOIN coverage_language_counts c ON c.path_id=p.path_id GROUP BY c.language ORDER BY c.language LIMIT 100"), params)
    };
    let mut resolution_languages = Map::new();
    for row in rows(conn, &language_sql, language_params, 100)? {
        if let Some(language) = row["language"].as_str() {
            resolution_languages.insert(language.to_owned(), json!({resolved:row[resolved],ambiguous:row[ambiguous],unresolved:row[unresolved],"external_imports":row["external_imports"]}));
        }
    }
    result["resolution_languages"] = Value::Object(resolution_languages);
    if exact_file {
        let error_columns = if options.detail == Detail::Full { "*" } else { "path,line,substr(message,1,512) message,length(message)>512 message_truncated" };
        result["errors"] = paging::query(conn, &format!("SELECT {error_columns} FROM errors WHERE path=?1 ORDER BY line,message,rowid"), &[&path], options)?;
        let resolution_sql = if options.detail == Detail::Full {
            format!("SELECT {},{} cause,e.evidence AS __evidence FROM resolution_coverage c JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE c.status IN('{unresolved}','{ambiguous}') AND c.path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) ORDER BY c.line,c.expression_id,c.status,c.evidence_id", paging::coverage("c", Detail::Full), resolution_cause_sql())
        } else {
            format!("SELECT c.line,(SELECT expression FROM coverage_expressions WHERE expression_id=c.expression_id) AS expression,c.status,{} cause,e.evidence AS __evidence FROM resolution_coverage c JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE c.status IN('{unresolved}','{ambiguous}') AND c.path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) ORDER BY c.line,c.expression_id,c.status,c.evidence_id", resolution_cause_sql())
        };
        let mut resolution = paging::query(conn, &resolution_sql, &[&path], options)?;
        let external_sql = format!("SELECT {},e.evidence AS __evidence FROM resolution_coverage c JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE c.status='{external}' AND c.path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) ORDER BY c.line,c.expression_id,c.evidence_id", paging::coverage("c", options.detail));
        let mut external_imports = paging::query(conn, &external_sql, &[&path], options)?;
        let workspace_root = read_metadata(conn, MetadataKey::WorkspaceRoot)?;
        let source = symbols::verified_source(conn, Path::new(&workspace_root), path)?;
        annotate_diagnostic_page(&mut resolution, &source);
        annotate_diagnostic_page(&mut external_imports, &source);
        result["resolution"] = resolution;
        result["external_imports"] = external_imports;
    } else {
        result["top_files"] = json!(rows(conn, &format!("SELECT path,sum(parse_errors) parse_errors,sum(unresolved) unresolved FROM (SELECT path,count(*) parse_errors,0 unresolved FROM errors WHERE {scope} GROUP BY path UNION ALL SELECT p.path,0 parse_errors,count(*) unresolved FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id WHERE c.status IN('{unresolved}','{ambiguous}') AND {coverage_scope} GROUP BY p.path) GROUP BY path ORDER BY sum(parse_errors)+sum(unresolved) DESC,path LIMIT 5"), params, 5)?);
        result["resolution_statuses"] = json!(rows(conn, &format!("SELECT c.status,count(*) total FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id WHERE {coverage_scope} GROUP BY c.status ORDER BY c.status LIMIT 20"), params, 20)?);
        result["resolution_causes"] = json!(rows(conn, &format!("SELECT status,cause,count(*) total,count(DISTINCT path) affected_files FROM (SELECT p.path,c.status,{} cause FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE c.status IN('{unresolved}','{ambiguous}') AND {coverage_scope}) GROUP BY status,cause ORDER BY total DESC,status,cause", resolution_cause_sql()), params, 20)?);
        result["parse_error_languages"] = json!(rows(conn, "SELECT f.language,count(*) errors,count(DISTINCT e.path) affected_files FROM errors e JOIN files f ON f.path=e.path WHERE (?1='' OR e.path=?1 OR (e.path>=?2 AND e.path<?3)) GROUP BY f.language ORDER BY errors DESC,f.language", params, 100)?);
        result["cause_note"] = json!("Causes classify recorded resolver evidence; inspect an exact file with detail='full' for evidence and parse error messages.");
        result["continuation_hint"] = json!(
            "Call code_map_analyze with an exact path from top_files to page its diagnostics."
        );
    }
    if exact_file && (path.ends_with(".md") || path.ends_with(".mdx")) {
        result["doc_diagnostics"] =
            super::super::lint::document_diagnostics_paged(conn, path, options, &generation)?;
    }
    let compute_cycles = include_cycles.unwrap_or(false);
    result["cycles"] = if compute_cycles {
        cycles::summary(conn, if path.is_empty() { None } else { Some(path) })?
    } else {
        json!({"omitted": true, "hint": "Pass include_cycles=true to compute cyclic dependencies."})
    };
    result["generation"] = json!(generation);
    Ok(result)
}

fn classify_read_only_sql(sql: &str) -> Result<Option<&str>> {
    let Some(keyword) = first_sql_keyword(sql)? else {
        return Ok(None);
    };
    if !keyword.eq_ignore_ascii_case("select") && !keyword.eq_ignore_ascii_case("with") {
        return Ok(None);
    }
    let end = sql_statement_end(sql)?;
    Ok(Some(&sql[..end]))
}

fn first_sql_keyword(sql: &str) -> Result<Option<&str>> {
    let start = skip_sql_trivia(sql, 0)?;
    let bytes = sql.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'$'))
    {
        end += 1;
    }
    Ok((start < end).then(|| &sql[start..end]))
}

fn sql_statement_end(sql: &str) -> Result<usize> {
    let bytes = sql.as_bytes();
    let mut offset = 0;
    loop {
        offset = skip_sql_trivia(sql, offset)?;
        if offset == bytes.len() {
            return Ok(offset);
        }
        match bytes[offset] {
            b';' => {
                ensure!(
                    skip_sql_trivia(sql, offset + 1)? == bytes.len(),
                    "exactly one SELECT or WITH statement is allowed"
                );
                return Ok(offset);
            }
            b'\'' | b'"' | b'`' => {
                offset = skip_sql_quoted(sql, offset, bytes[offset])?;
            }
            b'[' => {
                offset = skip_sql_quoted(sql, offset, b']')?;
            }
            _ => offset += 1,
        }
    }
}

fn skip_sql_trivia(sql: &str, mut offset: usize) -> Result<usize> {
    let bytes = sql.as_bytes();
    loop {
        while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
            offset += 1;
        }
        if bytes.get(offset..offset + 2) == Some(b"--") {
            while offset < bytes.len() && bytes[offset] != b'\n' {
                offset += 1;
            }
            continue;
        }
        if bytes.get(offset..offset + 2) == Some(b"/*") {
            let comment_start = offset + 2;
            let Some(comment_end) = sql[comment_start..].find("*/") else {
                bail!("unterminated SQL block comment");
            };
            offset = comment_start + comment_end + 2;
            continue;
        }
        return Ok(offset);
    }
}

fn skip_sql_quoted(sql: &str, mut offset: usize, closing: u8) -> Result<usize> {
    let bytes = sql.as_bytes();
    let doubled_delimiter = closing != b']';
    offset += 1;
    while offset < bytes.len() {
        if bytes[offset] == closing {
            if doubled_delimiter && bytes.get(offset + 1) == Some(&closing) {
                offset += 2;
            } else {
                return Ok(offset + 1);
            }
        } else {
            offset += 1;
        }
    }
    bail!("unterminated quoted value in SQL")
}

pub(super) fn resolution_cause_sql() -> &'static str {
    "CASE WHEN c.status='ambiguous' THEN 'ambiguous_candidates' WHEN e.evidence='computed receiver or dynamic callee; callable identity is unknown' THEN 'dynamic_callee' WHEN e.evidence='callee is shadowed by a parameter or local binding of unknown callable identity' THEN 'shadowed_binding' WHEN e.evidence LIKE 'call through external import %' THEN 'external_import_call' WHEN e.evidence LIKE 'import %: 1 modules, 0 alias targets' THEN 'alias_target_missing' WHEN e.evidence LIKE 'no indexed provider for absolute import %' THEN 'missing_indexed_import' WHEN e.evidence LIKE 'import %: 0 modules, %' THEN 'missing_indexed_import' WHEN e.evidence LIKE '0 lexically justified candidates;%' THEN 'no_lexical_candidate' ELSE 'other' END"
}

fn annotate_diagnostic_page(page: &mut Value, source: &str) {
    let Some(items) = page["items"].as_array_mut() else {
        return;
    };
    for item in items {
        let evidence = item
            .get("__evidence")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let reason = diagnostic_reason(
            item.get("status").and_then(Value::as_str),
            item.get("cause").and_then(Value::as_str),
            item.get("expression").and_then(Value::as_str),
            &evidence,
        );
        let snippet = item
            .get("line")
            .and_then(Value::as_u64)
            .filter(|line| *line > 0)
            .and_then(|line| {
                source
                    .split_inclusive('\n')
                    .nth(line.saturating_sub(1) as usize)
            })
            .map(|line| line.trim_end_matches(['\r', '\n']))
            .map(bounded_diagnostic_snippet);
        if let Some(object) = item.as_object_mut() {
            object.remove("__evidence");
            object.insert(
                "reason".into(),
                reason.map_or(Value::Null, |reason| json!(reason)),
            );
            object.insert(
                "snippet".into(),
                snippet.map_or(Value::Null, |snippet| json!(snippet)),
            );
        }
    }
}

fn bounded_diagnostic_snippet(line: &str) -> String {
    let end = acdd_core::format::utf8_floor_boundary(line, 512);
    line[..end].to_owned()
}

pub(super) fn diagnostic_reason(
    status: Option<&str>,
    cause: Option<&str>,
    expression: Option<&str>,
    evidence: &str,
) -> Option<&'static str> {
    if status == Some(CoverageStatus::Ambiguous.as_str()) {
        return Some("ambiguous_candidates");
    }
    if evidence.contains("diagnostic_reason=framework_magic") {
        return Some("framework_magic");
    }
    if evidence
        .contains("callee is shadowed by a parameter or local binding of unknown callable identity")
    {
        return Some(
            if expression.is_some_and(|expression| expression.contains('.')) {
                "unresolved_receiver_type"
            } else {
                "dynamic_callback"
            },
        );
    }
    if evidence.starts_with("call through external import ")
        || cause == Some("external_import_call")
    {
        return Some("external_unindexed");
    }
    match cause {
        Some("shadowed_binding") => Some("shadowed_binding"),
        _ => None,
    }
}
