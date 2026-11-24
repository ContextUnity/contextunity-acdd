use crate::db::paging;
use crate::db::reader::{
    selectors::{path_bounds, strip_selector_uri_prefix},
    sql::QueryBudget,
};
use crate::models::{
    details::{NodeDetailFlags, NodeDetailKey},
    tags::{CoverageStatus, NodeKind},
};
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::path_policy::{glob_matches, normalize_rel_path};
use anyhow::{ensure, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Indexed documentation origin for an architectural rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSource {
    pub path: String,
    pub line: usize,
}

/// Structured diagnostic emitted by [`super::run_lint`].
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
        let stripped = strip_selector_uri_prefix(raw_target).trim_end_matches('/');
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
            let (prefix, end) = path_bounds(&norm_target);
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

pub(super) fn load_test_line_spans(conn: &Connection) -> Result<HashMap<String, Vec<(usize, usize)>>> {
    let mut spans: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT p.path, n.line, n.end_line, n.is_test, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.is_test != 0 OR n.details LIKE '%cfg(%test%'",
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
            spans.entry(path).or_default().push((start, end));
        }
    }
    Ok(spans)
}

pub(super) fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains(['*', '?', '{'])
}

pub(super) use crate::db::cycles::{is_archived_path, is_test_context_path};

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
    pub(super) explicit_is_public: Option<bool>,
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
        let test_line_spans = load_test_line_spans(conn)?;
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
                let (decorators, receiver_type, impl_trait, explicit_is_public) =
                    parse_node_lint_details(&details_raw);
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
                        explicit_is_public,
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
                    let line_usize = line.max(1) as usize;
                    let module_symbol = extract_external_module_from_evidence(&evidence)
                        .filter(|m| m != &symbol);
                    seen_in_coverage.insert((path.clone(), symbol.clone()));
                    by_path.entry(path.clone()).or_default().push(list.len());
                    list.push(ExternalOccurrence {
                        path,
                        line: Some(line_usize),
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

fn parse_node_lint_details(
    raw: &str,
) -> (Vec<String>, Option<String>, Option<String>, Option<bool>) {
    if !raw.starts_with('{')
        || (!raw.contains(NodeDetailKey::DECORATORS)
            && !raw.contains("receiver_type")
            && !raw.contains(NodeDetailKey::IMPL_TRAIT)
            && !raw.contains(NodeDetailKey::IS_PUBLIC))
    {
        return (Vec::new(), None, None, None);
    }
    let Ok(val) = serde_json::from_str::<Value>(raw) else {
        return (Vec::new(), None, None, None);
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
    let explicit_is_public = val.get(NodeDetailKey::IS_PUBLIC).and_then(Value::as_bool);
    (decorators, receiver_type, impl_trait, explicit_is_public)
}

pub(super) fn is_type_declaration(kind: &str) -> bool {
    matches!(
        NodeKind::parse(kind),
        Some(NodeKind::Class | NodeKind::Struct | NodeKind::Enum)
    )
}
