//! Unified language-agnostic architectural, graph hygiene, syntax, and documentation lint engine.

mod arch;
mod cycles;
mod dead_tails;
mod docs_lint;
mod duplicates;
mod rules;
mod syntax;

use super::reader::QueryBudget;
use super::{paging, reader};
use crate::models::{
    details::{NodeDetailFlags, NodeDetailKey},
    tags::{CoverageStatus, NodeKind},
};
use acdd_core::lint::{ArchRuleKind, ArchitectureConfig, LintCategory, LintSeverity};
use acdd_core::path_policy::{glob_matches, normalize_rel_path};
use acdd_core::response::QueryOptions;
use anyhow::{ensure, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

pub(crate) use cycles::collect_cycle_components;
pub(crate) use dead_tails::{collect_dead_tails, DeadTailsNode};
pub(crate) use docs_lint::document_diagnostics_paged;
pub(crate) use duplicates::{
    collect_file_duplicates, collect_symbol_duplicates, format_duplicate_suggestion,
};
pub use rules::enforced_invariant_ids;
pub use syntax::syntax_paged;

/// Indexed documentation origin for an architectural rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSource {
    pub path: String,
    pub line: usize,
}

/// Structured diagnostic emitted by [`run_lint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LintViolation {
    pub category: LintCategory,
    pub rule_id: String,
    pub severity: LintSeverity,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invariant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_source: Option<RuleSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

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

#[derive(Debug, Clone)]
pub(super) struct LintScope {
    pub(super) target: String,
    pub(super) target_is_glob: bool,
    pub(super) paths: Vec<String>,
}

impl LintScope {
    pub(super) fn from_request(
        conn: &Connection,
        target: Option<&str>,
        paths: Option<&[String]>,
    ) -> Result<Self> {
        let raw_target = target.unwrap_or("").trim();
        let stripped = reader::strip_selector_uri_prefix(raw_target).trim_end_matches('/');
        ensure!(
            !stripped.starts_with('/')
                && !stripped
                    .split('/')
                    .any(|part| part == ".." || (part == "." && stripped != ".")),
            "lint target must be an indexed workspace-relative path or empty for the workspace"
        );
        let norm_target = if stripped.is_empty() || stripped == "." {
            String::new()
        } else {
            normalize_rel_path(stripped).unwrap_or_else(|| stripped.to_owned())
        };
        let target_is_glob = is_glob_pattern(&norm_target);
        if !norm_target.is_empty() && !target_is_glob {
            let (prefix, end) = reader::path_bounds(&norm_target);
            let indexed = paging::count(
                conn,
                "SELECT count(*) FROM files WHERE path=?1 OR (path>=?2 AND path<?3)",
                &[&norm_target, &prefix, &end],
            )?;
            ensure!(
                indexed > 0,
                "PATH_NOT_INDEXED: '{norm_target}' was not found in the indexed inventory"
            );
        }
        let norm_paths = paths
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| {
                let t = p.trim().trim_end_matches('/');
                if t.is_empty() || t == "." {
                    None
                } else {
                    normalize_rel_path(t).or_else(|| Some(t.to_owned()))
                }
            })
            .collect();
        Ok(Self {
            target: norm_target,
            target_is_glob,
            paths: norm_paths,
        })
    }

    pub(super) fn exact_prefix(&self) -> Option<&str> {
        (!self.target.is_empty() && !self.target_is_glob).then_some(self.target.as_str())
    }

    pub(super) fn in_scope(&self, path: &str) -> bool {
        let matches_target = if self.target.is_empty() {
            true
        } else if self.target_is_glob {
            glob_matches(&self.target, path)
        } else {
            path == self.target
                || (path.starts_with(&self.target)
                    && path.as_bytes().get(self.target.len()) == Some(&b'/'))
        };
        if !matches_target {
            return false;
        }
        if self.paths.is_empty() {
            return true;
        }
        self.paths.iter().any(|pat| {
            if is_glob_pattern(pat) {
                glob_matches(pat, path)
            } else {
                path == pat
                    || (path.starts_with(pat.as_str())
                        && path.as_bytes().get(pat.len()) == Some(&b'/'))
            }
        })
    }
}

pub(super) fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains(['*', '?', '{'])
}

pub(super) fn is_archived_path(path: &str) -> bool {
    path.starts_with("archive/") || path.contains("/archive/")
}

pub(super) fn is_test_context_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("/tests.rs")
        || path == "tests.rs"
        || path.starts_with("benchmarks/")
}

#[derive(Debug, Clone)]
pub(super) struct IndexedFile {
    pub(super) language: Option<String>,
    pub(super) is_test: bool,
}

#[derive(Debug, Clone)]
pub(super) struct IndexedNode {
    pub(super) hash: i64,
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) qualname: String,
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) end_line: usize,
    pub(super) is_test: bool,
    pub(super) language: String,
    pub(super) generated: bool,
    pub(super) decorators: Vec<String>,
    pub(super) receiver_type: Option<String>,
    pub(super) impl_trait: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IndexedEdge {
    pub(super) src_hash: i64,
    pub(super) dst_hash: i64,
    pub(super) kind: String,
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) confidence: String,
}

#[derive(Debug, Clone)]
pub(super) struct ExternalOccurrence {
    pub(super) path: String,
    pub(super) line: Option<usize>,
    pub(super) symbol: String,
    pub(super) module_symbol: Option<String>,
}

impl ExternalOccurrence {
    pub(super) fn matches_prefix(&self, prefix: &str) -> bool {
        acdd_core::lint::external_symbol_matches_prefix(&self.symbol, prefix)
            || self
                .module_symbol
                .as_deref()
                .is_some_and(|m| acdd_core::lint::external_symbol_matches_prefix(m, prefix))
    }

    pub(super) fn display_symbol(&self) -> &str {
        if matches!(self.symbol.as_str(), "default" | "*") {
            self.module_symbol.as_deref().unwrap_or(&self.symbol)
        } else {
            &self.symbol
        }
    }
}

fn extract_external_module_from_evidence(evidence: &str) -> Option<String> {
    let raw = if let Some((_, rest)) = evidence.split_once("no indexed provider for absolute import ")
    {
        rest.split(';').next().unwrap_or(rest)
    } else if let Some((_, rest)) = evidence.split_once("no indexed provider for ") {
        rest.split(';').next().unwrap_or(rest)
    } else if let Some((_, rest)) = evidence.split_once("external import ") {
        rest.split(" at line ")
            .next()
            .unwrap_or(rest)
            .split(';')
            .next()
            .unwrap_or(rest)
    } else {
        return None;
    };
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub(super) struct LintIndexData {
    pub(super) scope: LintScope,
    pub(super) files: BTreeMap<String, IndexedFile>,
    pub(super) test_line_spans: HashMap<String, Vec<(usize, usize)>>,
    pub(super) nodes: Option<HashMap<i64, IndexedNode>>,
    pub(super) nodes_by_path: Option<HashMap<String, Vec<i64>>>,
    pub(super) edges: Option<Vec<IndexedEdge>>,
    pub(super) external_occurrences: Option<Vec<ExternalOccurrence>>,
    pub(super) externals_by_path: Option<HashMap<String, Vec<usize>>>,
}

impl LintIndexData {
    pub(super) fn load_base(conn: &Connection, scope: LintScope) -> Result<Self> {
        let _budget = QueryBudget::new(conn);
        let mut files = BTreeMap::new();
        {
            let mut stmt = conn.prepare("SELECT path, language, is_test FROM files ORDER BY path")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let language: Option<String> = row.get(1)?;
                let is_test: i64 = row.get(2)?;
                files.insert(
                    path,
                    IndexedFile {
                        language,
                        is_test: is_test != 0,
                    },
                );
            }
        }
        let mut test_line_spans: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT p.path, n.line, n.end_line, n.is_test, n.details
                 FROM nodes n
                 JOIN path_dictionary p ON p.path_id = n.path_id
                 WHERE n.is_test != 0 OR n.details LIKE '%cfg(test)%'",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let line: i64 = row.get(1)?;
                let end_line: i64 = row.get(2)?;
                let is_test: i64 = row.get(3)?;
                let details: String = row.get(4)?;
                if is_test != 0 || NodeDetailFlags::from_json(&details).has_cfg_test {
                    let start = line.max(1) as usize;
                    let end = end_line.max(line).max(1) as usize;
                    test_line_spans.entry(path).or_default().push((start, end));
                }
            }
        }
        Ok(Self {
            scope,
            files,
            test_line_spans,
            nodes: None,
            nodes_by_path: None,
            edges: None,
            external_occurrences: None,
            externals_by_path: None,
        })
    }

    pub(super) fn file_language(&self, path: &str) -> Option<&str> {
        self.files.get(path).and_then(|f| f.language.as_deref())
    }

    pub(super) fn is_file_test(&self, path: &str) -> bool {
        self.files.get(path).is_some_and(|f| f.is_test) || is_test_context_path(path)
    }

    pub(super) fn is_test_location(&self, path: &str, line: usize) -> bool {
        self.is_file_test(path)
            || self
                .test_line_spans
                .get(path)
                .is_some_and(|spans| spans.iter().any(|&(s, e)| line >= s && line <= e))
    }

    pub(super) fn ensure_nodes(&mut self, conn: &Connection) -> Result<&HashMap<i64, IndexedNode>> {
        if self.nodes.is_none() {
            let _budget = QueryBudget::new(conn);
            let mut map = HashMap::new();
            let mut by_path: HashMap<String, Vec<i64>> = HashMap::new();
            let mut stmt = conn.prepare(
                "SELECT n.node_hash, n.id, n.kind, n.name, n.qualname, p.path, n.line, n.end_line,
                        n.is_test, n.language, n.generated, n.details
                 FROM nodes n
                 JOIN path_dictionary p ON p.path_id = n.path_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let (hash, id, kind, name, qualname, path): (i64, String, String, String, String, String) =
                    (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?);
                let (line, end_line, is_test, language, generated, details_raw): (i64, i64, i64, String, i64, String) =
                    (row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?);
                let (decorators, receiver_type, impl_trait) = parse_node_lint_details(&details_raw);
                by_path.entry(path.clone()).or_default().push(hash);
                map.insert(
                    hash,
                    IndexedNode {
                        hash,
                        id,
                        kind,
                        name,
                        qualname,
                        path,
                        line: line.max(1) as usize,
                        end_line: end_line.max(line).max(1) as usize,
                        is_test: is_test != 0,
                        language,
                        generated: generated != 0,
                        decorators,
                        receiver_type,
                        impl_trait,
                    },
                );
            }
            self.nodes = Some(map);
            self.nodes_by_path = Some(by_path);
        }
        Ok(self.nodes.as_ref().expect("nodes initialized"))
    }

    pub(super) fn ensure_edges(&mut self, conn: &Connection) -> Result<&[IndexedEdge]> {
        if self.edges.is_none() {
            let _budget = QueryBudget::new(conn);
            let mut list = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT e.src_hash, e.dst_hash, e.kind, p.path, e.line, ev.evidence
                 FROM edges e
                 JOIN path_dictionary p ON p.path_id = e.path_id
                 JOIN coverage_evidence ev ON ev.evidence_id = e.confidence_id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                list.push(IndexedEdge {
                    src_hash: row.get(0)?,
                    dst_hash: row.get(1)?,
                    kind: row.get(2)?,
                    path: row.get(3)?,
                    line: row.get::<_, i64>(4)?.max(1) as usize,
                    confidence: row.get(5)?,
                });
            }
            self.edges = Some(list);
        }
        Ok(self.edges.as_deref().expect("edges initialized"))
    }

    pub(super) fn ensure_external(
        &mut self,
        conn: &Connection,
    ) -> Result<&[ExternalOccurrence]> {
        if self.external_occurrences.is_none() {
            let _budget = QueryBudget::new(conn);
            let mut list = Vec::new();
            let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
            let mut seen_in_coverage: HashSet<(String, String)> = HashSet::new();
            let external = CoverageStatus::External.as_str();
            {
                let mut stmt = conn.prepare(&format!(
                    "SELECT p.path, c.line, x.expression, ev.evidence
                     FROM resolution_coverage c
                     JOIN path_dictionary p ON p.path_id = c.path_id
                     JOIN coverage_expressions x ON x.expression_id = c.expression_id
                     JOIN coverage_evidence ev ON ev.evidence_id = c.evidence_id
                     WHERE c.status = '{external}'
                     ORDER BY p.path, c.line, x.expression"
                ))?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let path: String = row.get(0)?;
                    let line: i64 = row.get(1)?;
                    let symbol: String = row.get(2)?;
                    let evidence: String = row.get(3)?;
                    let module_symbol = extract_external_module_from_evidence(&evidence)
                        .filter(|m| m != &symbol);
                    seen_in_coverage.insert((path.clone(), symbol.clone()));
                    by_path.entry(path.clone()).or_default().push(list.len());
                    list.push(ExternalOccurrence {
                        path,
                        line: Some(line.max(1) as usize),
                        symbol,
                        module_symbol,
                    });
                }
            }
            {
                let mut stmt = conn.prepare(&format!(
                    "SELECT p.path, d.symbol
                     FROM dependencies d
                     JOIN path_dictionary p ON p.path_id = d.owner_id
                     WHERE d.resolution = '{external}'
                     ORDER BY p.path, d.symbol"
                ))?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let path: String = row.get(0)?;
                    let symbol: String = row.get(1)?;
                    if !seen_in_coverage.contains(&(path.clone(), symbol.clone())) {
                        by_path.entry(path.clone()).or_default().push(list.len());
                        list.push(ExternalOccurrence {
                            path,
                            line: None,
                            symbol,
                            module_symbol: None,
                        });
                    }
                }
            }
            self.external_occurrences = Some(list);
            self.externals_by_path = Some(by_path);
        }
        Ok(self
            .external_occurrences
            .as_deref()
            .expect("external occurrences initialized"))
    }
}

fn parse_node_lint_details(raw: &str) -> (Vec<String>, Option<String>, Option<String>) {
    if !raw.starts_with('{')
        || (!raw.contains(NodeDetailKey::DECORATORS)
            && !raw.contains("receiver_type")
            && !raw.contains(NodeDetailKey::IMPL_TRAIT))
    {
        return (Vec::new(), None, None);
    }
    let Ok(val) = serde_json::from_str::<Value>(raw) else {
        return (Vec::new(), None, None);
    };
    let decorators = val
        .get(NodeDetailKey::DECORATORS)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let receiver_type = val
        .get("receiver_type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let impl_trait = val
        .get(NodeDetailKey::IMPL_TRAIT)
        .and_then(Value::as_str)
        .map(str::to_owned);
    (decorators, receiver_type, impl_trait)
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
        syntax::collect_syntax_violations(conn, &index, &arch_cfg.complexity, &mut violations)?;
    }
    if categories.contains(&LintCategory::Duplicates) {
        duplicates::collect_duplicate_violations(conn, &index.scope, &mut violations)?;
    }
    if categories.contains(&LintCategory::DeadTails) {
        dead_tails::collect_dead_tails_violations(conn, &index.scope, &mut violations)?;
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

pub(super) fn is_type_declaration(kind: &str) -> bool {
    matches!(
        NodeKind::parse(kind),
        Some(NodeKind::Class | NodeKind::Struct)
    )
}
