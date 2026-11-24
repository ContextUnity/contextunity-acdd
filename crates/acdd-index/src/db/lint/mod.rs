//! Unified language-agnostic architectural, graph hygiene, syntax, and documentation lint engine.

mod arch;
mod cycles;
mod dead_tails;
mod docs_lint;
mod duplicates;
mod rules;
mod syntax;
mod types;
mod unused_public_api;

use super::paging;
use acdd_core::lint::{ArchRuleKind, ArchitectureConfig, LintCategory, LintSeverity};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

pub(crate) use dead_tails::{collect_dead_tails, DeadTailsNode};
pub(crate) use docs_lint::document_diagnostics_paged;
pub(crate) use duplicates::{
    collect_file_duplicates, collect_symbol_duplicates, format_duplicate_suggestion,
};
pub use rules::enforced_invariant_ids;
pub use syntax::syntax_paged;
use types::{LintIndexData, LintScope, LintViolation};

/// Request parameters for [`run_lint`].
pub struct LintRequest<'a> {
    pub target: Option<&'a str>,
    pub paths: Option<&'a [String]>,
    pub categories: Option<&'a [LintCategory]>,
    pub rule_ids: Option<&'a [String]>,
    pub minimum_severity: Option<LintSeverity>,
    pub architecture: Option<&'a ArchitectureConfig>,
    pub workspace_root: Option<&'a Path>,
    pub page: &'a QueryOptions,
}

/// Executes the unified language-agnostic lint engine across requested categories.
pub fn run_lint(conn: &Connection, request: &LintRequest<'_>) -> Result<Value> {
    let generation = paging::generation(conn, request.page)?;
    let scope = LintScope::from_request(conn, request.target, request.paths)?;
    let categories = resolve_categories(request.categories);
    let minimum_severity = request.minimum_severity.unwrap_or(LintSeverity::Warn);
    let arch_cfg = rules::resolve_architecture(conn, request.architecture)?;
    let mut index = LintIndexData::load_base(conn, scope)?;

    let mut violations: Vec<LintViolation> = Vec::new();
    let need_rules =
        categories.contains(&LintCategory::Arch) || categories.contains(&LintCategory::Docs);
    let loaded_rules = if need_rules {
        let mut loaded = rules::load_all_rules(conn, arch_cfg.as_ref())?;
        for v in loaded.violations.drain(..) {
            if categories.contains(&v.category) && index.scope.in_scope(&v.path) {
                violations.push(v);
            }
        }
        loaded.rules
    } else {
        Vec::new()
    };

    let active_rules: Vec<&rules::LoadedRule> = loaded_rules
        .iter()
        .filter(|r| {
            request
                .rule_ids
                .is_none_or(|ids| ids.is_empty() || ids.iter().any(|id| id == &r.spec.id))
        })
        .collect();

    let need_doc_findings = categories.contains(&LintCategory::Docs)
        || (categories.contains(&LintCategory::Arch)
            && active_rules
                .iter()
                .any(|r| r.spec.kind == ArchRuleKind::DocIntegrity));
    let scoped_doc_findings = if need_doc_findings {
        let mut enforced = docs_lint::enforced_set_from_rules(&loaded_rules);
        if let Some(inv) = arch_cfg.complexity.invariant.as_deref() {
            let trimmed = inv.trim();
            if !trimmed.is_empty() {
                enforced.insert(trimmed.to_owned());
            }
        }
        docs_lint::collect_scoped_doc_findings(conn, &index, &enforced, request.workspace_root)?
    } else {
        Vec::new()
    };

    let doc_integrity_covered = if categories.contains(&LintCategory::Arch) {
        arch::evaluate_arch_rules(
            conn,
            &mut index,
            arch_cfg.as_ref(),
            &active_rules,
            &scoped_doc_findings,
            request.workspace_root,
            &mut violations,
        )?
    } else {
        HashSet::new()
    };
    if categories.contains(&LintCategory::Docs) {
        docs_lint::evaluate_docs_lint(&scoped_doc_findings, &doc_integrity_covered, &mut violations);
    }
    if categories.contains(&LintCategory::Syntax) {
        syntax::collect_syntax_violations(
            conn,
            &index,
            &arch_cfg.complexity,
            request.workspace_root,
            &mut violations,
        )?;
    }
    if categories.contains(&LintCategory::Duplicates) {
        duplicates::collect_duplicate_violations(conn, &index.scope, &mut violations)?;
    }
    if categories.contains(&LintCategory::DeadTails) {
        dead_tails::collect_dead_tails_violations(
            conn,
            &index.scope,
            request.workspace_root,
            arch_cfg.unused_public_api,
            &mut violations,
        )?;
    }
    if categories.contains(&LintCategory::Cycles) {
        cycles::collect_cycle_violations(conn, &index.scope, &mut violations)?;
    }

    if let Some(rule_ids) = request.rule_ids.filter(|ids| !ids.is_empty()) {
        violations.retain(|v| rule_ids.iter().any(|id| id == &v.rule_id));
    }
    violations.retain(|v| v.severity.meets_threshold(minimum_severity));
    violations.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
            .then_with(|| a.message.cmp(&b.message))
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.category.cmp(&b.category))
    });
    violations.dedup();

    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut by_category = Map::new();
    for cat in &categories {
        by_category.insert(cat.as_str().to_owned(), json!(0));
    }
    for v in &violations {
        match v.severity {
            LintSeverity::Error => errors += 1,
            LintSeverity::Warn => warnings += 1,
        }
        let key = v.category.as_str();
        let next = by_category
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        by_category.insert(key.to_owned(), json!(next));
    }

    let total = violations.len();
    let start = request.page.offset.min(total);
    let end = start.saturating_add(request.page.limit).min(total);
    let paged_items: Vec<Value> = violations[start..end]
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let page = paging::value(paged_items, total, request.page, &generation);

    Ok(json!({
        "mode": "lint",
        "target": request.target.unwrap_or(""),
        "categories": categories.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "minimum_severity": minimum_severity.as_str(),
        "summary": {
            "total": total,
            "errors": errors,
            "warnings": warnings,
            "rules_evaluated": active_rules.len(),
            "by_category": Value::Object(by_category),
        },
        "items": page["items"],
        "total": page["total"],
        "offset": page["offset"],
        "limit": page["limit"],
        "has_more": page["has_more"],
        "next_offset": page["next_offset"],
        "generation": page["generation"],
    }))
}

fn resolve_categories(requested: Option<&[LintCategory]>) -> Vec<LintCategory> {
    let source = requested
        .filter(|cats| !cats.is_empty())
        .unwrap_or(&LintCategory::DEFAULT_VERIFY);
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(source.len());
    for &cat in source {
        if seen.insert(cat) {
            out.push(cat);
        }
    }
    out
}
