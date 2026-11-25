use super::types::{is_archived_path, LintIndexData, LintScope, LintViolation};
use crate::db::commitments::{read_metadata_optional, MetadataKey};
use crate::db::paging;
use crate::db::reader::{selectors::path_bounds, sql::rows};
use crate::languages::dependency_registry::DependencyRegistry;
use crate::models::{details::NodeDetailKey, tags::NodeKind};
use acdd_core::lint::{ComplexityLimits, LintCategory, LintSeverity};
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{ensure, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn supported_language_profiles() -> Vec<&'static str> {
    crate::languages::profiles()
        .map(|profile| profile.id())
        .collect()
}

/// Returns stored Tree-sitter syntax diagnostics for compiled language profiles.
pub fn syntax_paged(conn: &Connection, target: &str, options: &QueryOptions) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let normalized = target.trim();
    let path = normalized.trim_end_matches('/');
    ensure!(
        !normalized.starts_with('/') && !path.split('/').any(|part| part == ".."),
        "lint target must be an indexed workspace-relative path or empty for the workspace"
    );
    let (prefix, end) = path_bounds(path);
    let profiles = supported_language_profiles();
    let profiles_json = serde_json::to_string(&profiles)?;
    let params: &[&dyn ToSql] = &[&path, &prefix, &end, &profiles_json];
    let scope = "(?1='' OR f.path=?1 OR (f.path>=?2 AND f.path<?3))";
    let supported = "f.language IN (SELECT value FROM json_each(?4))";
    let indexed_files = paging::count(
        conn,
        &format!("SELECT count(*) FROM files f WHERE {scope}"),
        &params[..3],
    )?;
    ensure!(
        path.is_empty() || indexed_files > 0,
        "PATH_NOT_INDEXED: '{path}' was not found in the indexed inventory"
    );
    let source_files = paging::count(
        conn,
        &format!("SELECT count(*) FROM files f WHERE {scope} AND {supported}"),
        params,
    )?;
    ensure!(source_files > 0, "lint target has no indexed source files with a compiled language profile; excluded, unsupported, and documentation files are not syntax-checked");
    let mut languages = rows(conn, &format!("SELECT f.language,count(*) files FROM files f WHERE {scope} AND {supported} GROUP BY f.language ORDER BY f.language"), params, profiles.len())?;
    for language in &mut languages {
        language["diagnostic_scope"] = json!(match language["language"].as_str() {
            Some("vue") => "embedded JavaScript/TypeScript scripts; template and style syntax are not checked",
            Some("html") if crate::languages::by_id("javascript").is_some() => "HTML grammar and executable inline JavaScript; styles and template expressions are not checked",
            Some("html") => "HTML grammar only; inline JavaScript requires the JavaScript profile",
            _ => "stored Tree-sitter syntax diagnostics",
        });
    }
    let message = if options.detail == Detail::Full {
        "e.message"
    } else {
        "substr(e.message,1,512)"
    };
    let diagnostics = paging::query(conn, &format!("SELECT 'syntax.parse' rule_id,'error' severity,f.language,e.path,e.line,{message} message,{} message_truncated FROM errors e JOIN files f ON f.path=e.path WHERE {scope} AND {supported} ORDER BY e.path,e.line,e.message,e.rowid", if options.detail == Detail::Full { "0" } else { "length(e.message)>512" }), params, options)?;
    let status = if diagnostics["total"] == 0 {
        "no_stored_syntax_diagnostics"
    } else {
        "diagnostics_found"
    };
    Ok(json!({
        "target": target, "mode": "lint", "status": status,
        "diagnostics": diagnostics,
        "coverage": {
            "scope": "indexed_sources_only", "indexed_source_files": source_files,
            "other_indexed_files": indexed_files - source_files, "languages": languages,
            "unindexed_files": "not_enumerated", "checks": ["stored_parser_diagnostics"],
            "limitations": "No style, type, semantic, or security rules. Excluded and unsupported files are not checked; absence of stored diagnostics does not prove syntactic validity."
        },
        "generation": generation,
    }))
}

pub(super) fn collect_syntax_violations(
    conn: &Connection,
    index: &LintIndexData,
    complexity: &ComplexityLimits,
    workspace_root: Option<&Path>,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    collect_parse_violations(conn, &index.scope, violations)?;
    collect_fact_budget_violations(conn, index, complexity, violations)?;
    collect_type_homonym_violations(conn, index, complexity, workspace_root, violations)?;
    Ok(())
}

fn collect_parse_violations(
    conn: &Connection,
    scope: &LintScope,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let profiles = supported_language_profiles();
    let profiles_json = serde_json::to_string(&profiles)?;
    let prefix_target = scope.exact_prefix().unwrap_or("");
    let (lower, upper) = path_bounds(prefix_target);
    let mut stmt = conn.prepare(
        "SELECT e.path, e.line, e.message
         FROM errors e
         JOIN files f ON f.path = e.path
         WHERE (?1 = '' OR e.path = ?1 OR (e.path >= ?2 AND e.path < ?3))
           AND f.language IN (SELECT value FROM json_each(?4))
         ORDER BY e.path, e.line, e.message, e.rowid",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        prefix_target,
        lower,
        upper,
        profiles_json
    ])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if !scope.in_scope(&path) {
            continue;
        }
        let line: i64 = row.get(1)?;
        let message: String = row.get(2)?;
        violations.push(LintViolation {
            category: LintCategory::Syntax,
            rule_id: "syntax.parse".to_owned(),
            severity: LintSeverity::Error,
            path,
            line: Some(line.max(1) as usize),
            message,
            invariant: None,
            rule_source: None,
            symbol: None,
            target: None,
            suggestion: None,
        });
    }
    Ok(())
}

fn push_syntax(
    violations: &mut Vec<LintViolation>,
    rule_id: &str,
    severity: LintSeverity,
    loc: (&str, usize, &str),
    message: String,
    invariant: Option<&str>,
    suggestion: Option<&str>,
) {
    violations.push(LintViolation {
        category: LintCategory::Syntax,
        rule_id: rule_id.to_owned(),
        severity,
        path: loc.0.to_owned(),
        line: Some(loc.1),
        message,
        invariant: invariant.map(str::to_owned),
        rule_source: None,
        symbol: Some(loc.2.to_owned()),
        target: None,
        suggestion: suggestion.map(str::to_owned),
    });
}

fn is_production_node_in_scope(index: &LintIndexData, path: &str, line: usize) -> bool {
    !is_archived_path(path) && index.scope.in_scope(path) && !index.is_test_location(path, line)
}

fn collect_fact_budget_violations(
    conn: &Connection,
    index: &LintIndexData,
    complexity: &ComplexityLimits,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let prefix_target = index.scope.exact_prefix().unwrap_or("");
    let (lower, upper) = path_bounds(prefix_target);
    let budget_pat = format!("%{}%", NodeDetailKey::FACT_LIMIT_EXCEEDED);
    let mut stmt = conn.prepare(
        "SELECT n.qualname, p.path, n.line, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.is_test = 0 AND n.generated = 0
           AND (?1 = '' OR p.path = ?1 OR (p.path >= ?2 AND p.path < ?3))
           AND n.details LIKE ?4
         ORDER BY p.path, n.line, n.id",
    )?;
    let mut rows = stmt.query(rusqlite::params![prefix_target, lower, upper, budget_pat])?;
    let inv = complexity.invariant.as_deref();
    while let Some(row) = rows.next()? {
        let (qualname, path, line): (String, String, usize) = (
            row.get(0)?,
            row.get(1)?,
            row.get::<_, i64>(2)?.max(1) as usize,
        );
        if !is_production_node_in_scope(index, &path, line) {
            continue;
        }
        let details_raw: String = row.get(3)?;
        let Ok(details) = serde_json::from_str::<Value>(&details_raw) else {
            continue;
        };
        if details
            .get(NodeDetailKey::FACT_LIMIT_EXCEEDED)
            .and_then(Value::as_bool)
            == Some(true)
        {
            push_syntax(
                violations,
                "syntax.fact_budget_exceeded",
                complexity.severity,
                (path.as_str(), line, qualname.as_str()),
                format!("callable '{qualname}' exceeded ValueFlow fact extraction budget"),
                inv,
                Some("Split oversized callable so static value-flow analysis stays within budget."),
            );
        }
    }
    Ok(())
}

fn crate_or_package_bucket(path: &str) -> String {
    let mut parts = path.split('/');
    match (parts.next(), parts.next()) {
        (Some(root @ ("crates" | "packages")), Some(member)) => format!("{root}/{member}"),
        (Some("src"), _) => "src".to_owned(),
        _ => String::new(),
    }
}

fn package_bucket_for_node(
    path: &str,
    language: &str,
    dep_registry: Option<&DependencyRegistry>,
) -> String {
    if let (Some(reg), Some(profile)) = (dep_registry, crate::languages::by_id(language)) {
        if let Some(scope) = reg.nearest_manifest_scope_for_path(profile.family(), path) {
            return if scope.is_empty() {
                "workspace-root".to_owned()
            } else {
                scope.to_owned()
            };
        }
    }
    crate_or_package_bucket(path)
}

fn collect_type_homonym_violations(
    conn: &Connection,
    index: &LintIndexData,
    complexity: &ComplexityLimits,
    workspace_root: Option<&Path>,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let resolved_root = workspace_root.map(Path::to_path_buf).or_else(|| {
        read_metadata_optional(conn, MetadataKey::WorkspaceRoot)
            .ok()
            .flatten()
            .map(PathBuf::from)
    });
    let dep_registry = resolved_root
        .as_deref()
        .filter(|root| root.is_dir())
        .map(|root| DependencyRegistry::collect(Some(root)));

    let sql = format!(
        "SELECT n.name, n.qualname, p.path, n.line, n.language
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind IN ('{class}', '{strukt}', '{enm}', '{trt}', '{iface}')
           AND n.is_test = 0 AND n.generated = 0
         ORDER BY p.path, n.line, n.id",
        class = NodeKind::Class.as_str(),
        strukt = NodeKind::Struct.as_str(),
        enm = NodeKind::Enum.as_str(),
        trt = NodeKind::Trait.as_str(),
        iface = NodeKind::Interface.as_str(),
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    type GroupKey = (String, String, String);
    let mut groups: BTreeMap<GroupKey, Vec<(String, String, usize)>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let (name, qualname, path, line, language): (String, String, String, usize, String) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get::<_, i64>(3)?.max(1) as usize,
            row.get(4)?,
        );
        if is_archived_path(&path) || index.is_test_location(&path, line) {
            continue;
        }
        let bucket = package_bucket_for_node(&path, &language, dep_registry.as_ref());
        groups
            .entry((bucket, language, name))
            .or_default()
            .push((qualname, path, line));
    }
    for ((bucket, _lang, name), entries) in groups {
        let distinct_file_count = entries
            .iter()
            .map(|(_, p, _)| p.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if distinct_file_count < 2 {
            continue;
        }
        let scope_label = if bucket.is_empty() {
            "workspace"
        } else {
            bucket.as_str()
        };
        for (qualname, path, line) in entries {
            if !index.scope.in_scope(&path) {
                continue;
            }
            push_syntax(
                violations, "syntax.intra_crate_type_homonym", complexity.severity, (&path, line, &qualname),
                format!("type '{name}' ({qualname}) is declared in {distinct_file_count} distinct files inside '{scope_label}'"),
                complexity.invariant.as_deref(),
                Some("Disambiguate intra-crate type homonyms so suffix resolution stays unique."),
            );
        }
    }
    Ok(())
}
