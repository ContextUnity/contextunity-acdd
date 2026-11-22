use super::{normalize_scope, page_bounds};
use crate::db::{
    lint::{collect_file_duplicates, collect_symbol_duplicates, format_duplicate_suggestion},
    paging,
};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

fn file_duplicates_paged(
    conn: &Connection,
    scope: &str,
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
    let (file_groups, total_duplicated_bytes) = collect_file_duplicates(conn, scope)?;
    let (total, slice) = page_bounds(&file_groups, options);
    let paged_items: Vec<Value> = slice
        .iter()
        .map(|g| {
            let duplicate_count = g.locations.len();
            json!({
                "digest": g.digest,
                "size_bytes": g.size_bytes,
                "duplicate_count": duplicate_count,
                "locations": g.locations,
                "suggestion": format!(
                    "Consolidate duplicate files across {duplicate_count} locations into a single shared file or replace with re-exports."
                ),
            })
        })
        .collect();
    let page = paging::value(paged_items, total, options, generation);

    Ok(json!({
        "operation": "duplicates",
        "level": "file",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "total_duplicate_groups": total,
        "total_duplicated_bytes": total_duplicated_bytes,
        "items": page["items"],
        "total": page["total"],
        "offset": page["offset"],
        "limit": page["limit"],
        "has_more": page["has_more"],
        "next_offset": page["next_offset"],
        "generation": page["generation"],
    }))
}

/// Identifies duplicate files, functions, methods, and intra-function blocks with bounded paging.
pub fn duplicates_paged(
    conn: &Connection,
    target: Option<&str>,
    path: Option<&str>,
    level: Option<&str>,
    min_lines: Option<usize>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let scope_owned = normalize_scope(target, path, "target")?;
    let scope = scope_owned.as_str();

    let level_str = level.unwrap_or("symbol").trim().to_ascii_lowercase();
    anyhow::ensure!(
        level_str == "symbol" || level_str == "file",
        "invalid level '{level_str}': expected 'symbol' or 'file'"
    );

    let min_lines = min_lines.unwrap_or(5).max(1);
    if level_str == "file" {
        return file_duplicates_paged(conn, scope, options, &generation);
    }

    let report = collect_symbol_duplicates(conn, scope, min_lines)?;
    let (total, slice) = page_bounds(&report.groups, options);
    let paged_items: Vec<Value> = slice
        .iter()
        .map(|g| {
            let rep = &report.candidates[g.rep_idx];
            let loc_paths: Vec<&str> = g
                .locations
                .iter()
                .map(|l| report.candidates[l.candidate_idx].path.as_str())
                .collect();
            let locations: Vec<Value> = g
                .locations
                .iter()
                .map(|l| {
                    json!({
                        "path": report.candidates[l.candidate_idx].path,
                        "line": l.line,
                        "end_line": l.end_line,
                    })
                })
                .collect();
            let suggestion = format_duplicate_suggestion(&rep.name, &loc_paths);
            json!({
                "name": rep.name,
                "kind": rep.kind,
                "line_count": g.line_count,
                "match_type": g.match_type,
                "locations": locations,
                "duplicate_count": g.locations.len(),
                "duplicated_lines": g.duplicated_lines,
                "suggestion": suggestion,
            })
        })
        .collect();
    let page = paging::value(paged_items, total, options, &generation);

    let mut response = json!({
        "operation": "duplicates",
        "level": "symbol",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "min_lines": min_lines,
        "total_duplicate_groups": total,
        "total_duplicated_lines": report.total_duplicated_lines,
        "items": page["items"],
        "total": page["total"],
        "offset": page["offset"],
        "limit": page["limit"],
        "has_more": page["has_more"],
        "next_offset": page["next_offset"],
        "generation": page["generation"],
    });

    if !report.stale_files.is_empty() {
        response["stale_files"] = json!(report.stale_files);
    }

    Ok(response)
}
