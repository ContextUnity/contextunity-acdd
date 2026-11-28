mod baseline;
mod provenance;
mod similarity;
mod types;

use super::types::{is_archived_path, is_test_context_path, LintScope, LintViolation};
use crate::db::reader::selectors as reader;
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub(crate) use similarity::collect_symbol_duplicates;
pub use types::LintDiffContext;
pub(crate) use types::{
    DuplicateCandidate, DuplicateLocation, DuplicateScanRequest, FileDuplicateGroup, ReuseHint,
};

pub(crate) fn format_duplicate_suggestion(
    name: &str,
    candidates: &[DuplicateCandidate],
    locations: &[DuplicateLocation],
) -> String {
    let best_owner = locations
        .iter()
        .map(|l| &candidates[l.candidate_idx])
        .max_by(|a, b| {
            let a_pub = a.signature.trim_start().starts_with("pub");
            let b_pub = b.signature.trim_start().starts_with("pub");
            a.inbound_callers
                .cmp(&b.inbound_callers)
                .then_with(|| a_pub.cmp(&b_pub))
                .then_with(|| b.path.cmp(&a.path))
        });
    if let Some(owner) = best_owner {
        if owner.inbound_callers > 0 {
            return format!(
                "Reuse canonical owner '{}' at {}:{} ({} inbound callers) instead of duplicating '{name}'.",
                owner.name, owner.path, owner.line, owner.inbound_callers
            );
        }
        return format!(
            "Consolidate duplicate '{name}' into a shared helper (e.g. alongside '{}:{}').",
            owner.path, owner.line
        );
    }
    format!("Consolidate duplicate '{name}' into a single shared helper.")
}

fn format_reuse_hint_message(hint: &ReuseHint) -> String {
    let sig_note = if hint.owner_signature.is_empty() {
        String::new()
    } else {
        format!(" — `{}`", hint.owner_signature)
    };
    format!(
        "reuse_hint ({:.0}% similar): '{}' overlaps canonical owner '{}' at {}:{} ({} inbound callers){sig_note}",
        hint.similarity * 100.0,
        hint.probe_name,
        hint.owner_name,
        hint.owner_path,
        hint.owner_line,
        hint.inbound_callers,
    )
}

pub(crate) fn collect_file_duplicates(
    conn: &Connection,
    scope: &str,
) -> Result<(Vec<FileDuplicateGroup>, u64)> {
    let include_tests = is_test_context_path(scope);
    let (lower, upper) = reader::path_bounds(scope);
    let mut stmt = conn.prepare(
        "SELECT f.digest, f.size, f.path, f.is_test
         FROM files f
         WHERE (?1 = '' OR f.path = ?1 OR (f.path >= ?2 AND f.path < ?3))
           AND (?4 = 1 OR f.is_test = 0)
           AND f.digest IN (
               SELECT digest FROM files
               WHERE (?1 = '' OR path = ?1 OR (path >= ?2 AND path < ?3))
                 AND (?4 = 1 OR is_test = 0)
               GROUP BY digest HAVING count(*) > 1
           )
         ORDER BY f.size DESC, f.digest, f.path",
    )?;
    let mut groups: BTreeMap<String, (u64, Vec<String>)> = BTreeMap::new();
    let mut rows = stmt.query(rusqlite::params![
        scope,
        lower,
        upper,
        i64::from(include_tests)
    ])?;
    while let Some(row) = rows.next()? {
        let digest: String = row.get(0)?;
        let size: u64 = row.get(1)?;
        let p: String = row.get(2)?;
        if is_archived_path(&p) || (!include_tests && is_test_context_path(&p)) {
            continue;
        }
        groups.entry(digest).or_insert((size, Vec::new())).1.push(p);
    }
    drop(rows);
    drop(stmt);

    let mut file_groups = Vec::new();
    let mut total_duplicated_bytes: u64 = 0;
    for (digest, (size_bytes, mut locations)) in groups {
        if locations.len() < 2 {
            continue;
        }
        locations.sort();
        total_duplicated_bytes += size_bytes * (locations.len() as u64 - 1);
        file_groups.push(FileDuplicateGroup {
            digest,
            size_bytes,
            locations,
        });
    }
    file_groups.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| b.locations.len().cmp(&a.locations.len()))
            .then_with(|| a.digest.cmp(&b.digest))
    });
    Ok((file_groups, total_duplicated_bytes))
}

pub(in crate::db::lint) fn collect_duplicate_violations(
    conn: &Connection,
    scope: &LintScope,
    diff: Option<&LintDiffContext<'_>>,
    workspace_root: Option<&Path>,
    violations: &mut Vec<LintViolation>,
) -> Result<Vec<String>> {
    let sql_scope = scope.exact_prefix().unwrap_or("");
    let explicit_diff = diff.filter(|d| !d.diff_paths.is_empty());
    let probe_paths = explicit_diff
        .map(|d| d.diff_paths)
        .or_else(|| (!scope.paths.is_empty()).then_some(scope.paths.as_slice()));
    let report = collect_symbol_duplicates(
        conn,
        &DuplicateScanRequest {
            scope: sql_scope,
            probe_paths,
            min_lines: 5,
        },
    )?;
    let changed_members: HashSet<usize> = match explicit_diff {
        Some(d) => baseline::changed_candidates_vs_baseline(
            &report,
            d.diff_paths,
            d.baseline_ref.unwrap_or("HEAD"),
            workspace_root,
        ),
        None => HashSet::new(),
    };
    for g in &report.groups {
        let rep = &report.candidates[g.rep_idx];
        let loc_paths: Vec<&str> = g
            .locations
            .iter()
            .map(|l| report.candidates[l.candidate_idx].path.as_str())
            .collect();
        let any_in_scope = loc_paths.iter().any(|p| scope.in_scope(p));
        if !any_in_scope {
            continue;
        }
        let first_loc = g
            .locations
            .iter()
            .copied()
            .find(|l| scope.in_scope(&report.candidates[l.candidate_idx].path))
            .or_else(|| g.locations.first().copied())
            .unwrap_or(DuplicateLocation {
                candidate_idx: g.rep_idx,
                line: rep.line,
                end_line: rep.end_line,
            });
        let first_path = &report.candidates[first_loc.candidate_idx].path;
        let suggestion = format_duplicate_suggestion(&rep.name, &report.candidates, &g.locations);
        let has_changed_member = explicit_diff.is_some()
            && g.locations
                .iter()
                .any(|l| changed_members.contains(&l.candidate_idx));
        let severity = if has_changed_member && matches!(g.match_type, "exact" | "similar") {
            LintSeverity::Error
        } else {
            LintSeverity::Warn
        };
        violations.push(LintViolation {
            category: LintCategory::Duplicates,
            rule_id: format!("duplicates.{}", g.match_type),
            severity,
            path: first_path.clone(),
            line: Some(first_loc.line),
            message: format!(
                "{} duplicate of '{}' ({} lines across {} locations)",
                g.match_type,
                rep.name,
                g.line_count,
                g.locations.len()
            ),
            invariant: None,
            rule_source: None,
            symbol: Some(rep.name.clone()),
            target: None,
            suggestion: Some(suggestion),
        });
    }
    let mut diff_reuse_hints = Vec::new();
    for hint in &report.reuse_hints {
        let message = format_reuse_hint_message(hint);
        if explicit_diff.is_some() {
            diff_reuse_hints.push(format!(
                "[xray.reuse_hint] {}:{}: {message}",
                hint.probe_path, hint.probe_line
            ));
        }
        violations.push(LintViolation {
            category: LintCategory::Duplicates,
            rule_id: "duplicates.reuse_hint".to_owned(),
            severity: LintSeverity::Warn,
            path: hint.probe_path.clone(),
            line: Some(hint.probe_line),
            message,
            invariant: None,
            rule_source: None,
            symbol: Some(hint.probe_name.clone()),
            target: Some(hint.owner_path.clone()),
            suggestion: Some(format!(
                "Consider reusing '{}' ({}:{}) instead of re-implementing.",
                hint.owner_name, hint.owner_path, hint.owner_line
            )),
        });
    }
    Ok(diff_reuse_hints)
}
