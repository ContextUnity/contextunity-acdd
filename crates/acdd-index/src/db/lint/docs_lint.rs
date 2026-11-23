use super::rules::{rule_invariant_id, selector_matches_file, LoadedRule};
use super::types::{is_archived_path, LintIndexData, LintScope, LintViolation};
use crate::db::commitments::{read_metadata_optional, MetadataKey};
use crate::db::{paging, reader::sql as reader, source};
use crate::models::{
    stable_hash64,
    tags::{EdgeKind, NodeKind},
};
use acdd_core::lint::{ArchRuleKind, LintCategory, LintSeverity};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct SymbolClaimRef {
    doc_path: String,
    line: usize,
    name: String,
    target_path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Debug, Clone)]
struct DeclaredInvariantRef {
    id: String,
    doc_path: String,
    line: usize,
    node_id: String,
}

#[derive(Debug, Clone)]
struct TargetCandidateNode {
    id: String,
    name: String,
    qualname: String,
    line: usize,
    end_line: usize,
}

#[derive(Debug, Clone)]
pub(super) struct DocIntegrityFinding {
    kind: &'static str,
    rule_id: &'static str,
    default_severity: LintSeverity,
    path: String,
    line: usize,
    message: String,
    invariant: Option<String>,
    symbol: Option<String>,
    target: Option<String>,
    raw_diagnostic: Option<Value>,
}

impl DocIntegrityFinding {
    fn new(
        kind: &'static str,
        rule_id: &'static str,
        default_severity: LintSeverity,
        path: String,
        line: usize,
        message: String,
    ) -> Self {
        Self {
            kind,
            rule_id,
            default_severity,
            path,
            line,
            message,
            invariant: None,
            symbol: None,
            target: None,
            raw_diagnostic: None,
        }
    }
}

fn is_document_link_target(target: &str) -> bool {
    if target.starts_with('#') {
        return true;
    }
    if target.contains("://") || target.starts_with("mailto:") {
        return false;
    }
    let linked_path = target.split('#').next().unwrap_or_default();
    match Path::new(linked_path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) => matches!(extension.to_ascii_lowercase().as_str(), "md" | "mdx"),
        None => true,
    }
}

fn match_symbol_claim<'a>(
    candidates: &'a [TargetCandidateNode],
    name: &str,
) -> (Vec<&'a TargetCandidateNode>, Option<&'a TargetCandidateNode>) {
    let normalized = name.replace("::", ".");
    let suffix_dot = format!(".{normalized}");
    let mut best_priority = u8::MAX;
    let mut matched = Vec::new();
    for cand in candidates {
        let priority = if cand.qualname == normalized {
            0
        } else if cand.name == name {
            1
        } else if cand.qualname.ends_with(&suffix_dot) {
            2
        } else {
            continue;
        };
        if priority < best_priority {
            best_priority = priority;
            matched.clear();
            matched.push(cand);
        } else if priority == best_priority {
            matched.push(cand);
        }
    }
    let suggestion = candidates
        .iter()
        .filter(|c| c.name.starts_with(name))
        .min_by(|a, b| a.name.cmp(&b.name).then_with(|| a.line.cmp(&b.line)));
    (matched, suggestion)
}

fn collect_broken_doc_links(
    conn: &Connection,
    single_doc_path: Option<&str>,
    scope: Option<&LintScope>,
    findings: &mut Vec<DocIntegrityFinding>,
) -> Result<()> {
    let doc_kind = NodeKind::Document.as_str();
    let path_filter = single_doc_path.unwrap_or("");
    let mut stmt = conn.prepare(&format!(
        "SELECT 'broken_doc_link' AS kind, p.path,
                CAST(json_extract(c.value,'$.line') AS INTEGER) AS line,
                json_extract(c.value,'$.target') AS target
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         JOIN json_each(n.details,'$.citations') c
         WHERE n.kind = '{doc_kind}'
           AND (?1 = '' OR p.path = ?1)
           AND p.path NOT LIKE 'archive/%'
           AND p.path NOT LIKE '%/archive/%'
           AND NOT EXISTS (
               SELECT 1 FROM edges e
               JOIN coverage_evidence v ON v.evidence_id = e.evidence_id
               WHERE e.kind = 'cites_doc'
                 AND e.src_hash = n.node_hash
                 AND e.line = CAST(json_extract(c.value,'$.line') AS INTEGER)
                 AND v.evidence = json_extract(c.value,'$.target')
           )
         ORDER BY p.path, line, target",
    ))?;
    let mut rows = stmt.query([path_filter])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(1)?;
        let line: i64 = row.get(2)?;
        let Some(target) = row.get::<_, Option<String>>(3)? else {
            continue;
        };
        if !is_document_link_target(&target) || scope.is_some_and(|s| !s.in_scope(&path)) {
            continue;
        }
        let mut f = DocIntegrityFinding::new(
            "broken_doc_link",
            "docs.broken_doc_link",
            LintSeverity::Error,
            path.clone(),
            line.max(1) as usize,
            format!("broken documentation link target '{target}'"),
        );
        f.target = Some(target.clone());
        f.raw_diagnostic =
            Some(json!({"kind": "broken_doc_link", "path": path, "line": line, "target": target}));
        findings.push(f);
    }
    Ok(())
}

fn collect_claim_refs_from_details(
    path: &str,
    section_line: i64,
    details: &Value,
    symbol_claims: &mut Vec<SymbolClaimRef>,
) {
    let Some(claims) = details.get("symbol_claims").and_then(Value::as_array) else {
        return;
    };
    for claim in claims {
        if claim
            .get("label_count")
            .and_then(Value::as_u64)
            .is_some_and(|c| c != 1)
        {
            continue;
        }
        if !claim
            .get("explicit_path")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let (Some(name), Some(target_path)) = (
            claim.get("name").and_then(Value::as_str),
            claim.get("target_path").and_then(Value::as_str),
        ) else {
            continue;
        };
        if target_path.split('/').any(|part| part == "..") {
            continue;
        }
        // Plain document links (`[`AGENTS.md`](AGENTS.md)`) and filename links
        // (`[`fs.rs`](.../fs.rs)`) are file navigation links, not symbol claims.
        if is_document_link_target(target_path)
            || Path::new(target_path)
                .file_name()
                .and_then(|file| file.to_str())
                == Some(name)
        {
            continue;
        }
        let line = claim
            .get("line")
            .and_then(Value::as_u64)
            .unwrap_or(section_line.max(1) as u64) as usize;
        symbol_claims.push(SymbolClaimRef {
            doc_path: path.to_owned(),
            line,
            name: name.to_owned(),
            target_path: target_path.to_owned(),
            start_line: claim
                .get("start_line")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            end_line: claim
                .get("end_line")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
        });
    }
}

fn collect_declared_invariants_from_details(
    path: &str,
    section_line: i64,
    details: &Value,
    declared_invariants: &mut Vec<DeclaredInvariantRef>,
    non_contract_by_id: &mut BTreeMap<String, Vec<DeclaredInvariantRef>>,
) {
    let Some(invs) = details.get("declared_invariants").and_then(Value::as_array) else {
        return;
    };
    for inv in invs {
        let Some(id) = inv.get("id").and_then(Value::as_str) else {
            continue;
        };
        let inv_line = inv
            .get("line")
            .and_then(Value::as_u64)
            .unwrap_or(section_line.max(1) as u64) as usize;
        let item = DeclaredInvariantRef {
            id: id.to_owned(),
            doc_path: path.to_owned(),
            line: inv_line,
            node_id: format!("invariant:{path}:{inv_line}:{id}"),
        };
        non_contract_by_id
            .entry(id.to_owned())
            .or_default()
            .push(item.clone());
        declared_invariants.push(item);
    }
}

struct ScannedDocClaims {
    symbol_claims: Vec<SymbolClaimRef>,
    declared_invariants: Vec<DeclaredInvariantRef>,
    non_contract_by_id: BTreeMap<String, Vec<DeclaredInvariantRef>>,
}

struct SymbolClaimTargets {
    candidates_by_path: HashMap<String, Vec<TargetCandidateNode>>,
    indexed_code_paths: HashSet<String>,
}

fn scan_doc_metadata_claims(
    conn: &Connection,
    single_doc_path: Option<&str>,
) -> Result<ScannedDocClaims> {
    let doc_kind = NodeKind::Document.as_str();
    let mut symbol_claims = Vec::new();
    let mut declared_invariants = Vec::new();
    let mut non_contract_by_id: BTreeMap<String, Vec<DeclaredInvariantRef>> = BTreeMap::new();
    let path_filter = single_doc_path.unwrap_or("");
    let mut stmt = conn.prepare(&format!(
        "SELECT p.path, n.line, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind = '{doc_kind}'
           AND (?1 = '' OR p.path = ?1)
           AND p.path NOT LIKE 'archive/%'
           AND p.path NOT LIKE '%/archive/%'
         ORDER BY p.path, n.line, n.id",
    ))?;
    let mut rows = stmt.query([path_filter])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if is_archived_path(&path) {
            continue;
        }
        let section_line: i64 = row.get(1)?;
        let details_raw: String = row.get(2)?;
        let Ok(details) = serde_json::from_str::<Value>(&details_raw) else {
            continue;
        };
        let doc_type = details
            .get("doc_type")
            .and_then(Value::as_str)
            .unwrap_or("guide");

        if single_doc_path.is_some() || doc_type != "contract" {
            collect_claim_refs_from_details(&path, section_line, &details, &mut symbol_claims);
        }
        if single_doc_path.is_none() && doc_type != "contract" {
            collect_declared_invariants_from_details(
                &path,
                section_line,
                &details,
                &mut declared_invariants,
                &mut non_contract_by_id,
            );
        }
    }
    Ok(ScannedDocClaims {
        symbol_claims,
        declared_invariants,
        non_contract_by_id,
    })
}

fn collect_stored_doc_errors(
    conn: &Connection,
    scope: Option<&LintScope>,
    findings: &mut Vec<DocIntegrityFinding>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT e.path, e.line, e.message
         FROM errors e
         JOIN files f ON f.path = e.path
         WHERE (f.language = 'markdown' OR e.path LIKE '%.md' OR e.path LIKE '%.mdx')
           AND e.path NOT LIKE 'archive/%'
           AND e.path NOT LIKE '%/archive/%'
         ORDER BY e.path, e.line, e.message",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let line: i64 = row.get(1)?;
        let message: String = row.get(2)?;
        if scope.is_some_and(|s| !s.in_scope(&path)) {
            continue;
        }
        findings.push(DocIntegrityFinding::new(
            "stored_doc_error",
            "docs.stored_error",
            LintSeverity::Error,
            path,
            line.max(1) as usize,
            message,
        ));
    }
    Ok(())
}

fn load_symbol_claim_targets(
    conn: &Connection,
    scope: Option<&LintScope>,
    symbol_claims: &[SymbolClaimRef],
) -> Result<SymbolClaimTargets> {
    let target_paths: BTreeSet<&str> = symbol_claims
        .iter()
        .filter(|c| scope.is_none_or(|s| s.in_scope(&c.doc_path)))
        .map(|c| c.target_path.as_str())
        .collect();
    let mut candidates_by_path: HashMap<String, Vec<TargetCandidateNode>> = HashMap::new();
    let mut indexed_code_paths: HashSet<String> = HashSet::new();
    if target_paths.is_empty() {
        return Ok(SymbolClaimTargets {
            candidates_by_path,
            indexed_code_paths,
        });
    }
    let paths_json = serde_json::to_string(&target_paths)?;
    {
        let mut stmt = conn.prepare(
            "SELECT path FROM files
             WHERE path IN (SELECT value FROM json_each(?1))
               AND COALESCE(language, '') != 'markdown'",
        )?;
        let mut rows = stmt.query([&paths_json])?;
        while let Some(row) = rows.next()? {
            indexed_code_paths.insert(row.get(0)?);
        }
    }
    let doc_kind = NodeKind::Document.as_str();
    let inv_kind = NodeKind::Invariant.as_str();
    let mod_kind = NodeKind::Module.as_str();
    let mut stmt = conn.prepare(&format!(
        "SELECT p.path, n.id, n.name, n.qualname, n.line, n.end_line
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE p.path IN (SELECT value FROM json_each(?1))
           AND n.kind NOT IN ('{doc_kind}','{inv_kind}','{mod_kind}','file','impl')
         ORDER BY p.path, n.line, n.id"
    ))?;
    let mut rows = stmt.query([&paths_json])?;
    while let Some(row) = rows.next()? {
        let p: String = row.get(0)?;
        candidates_by_path
            .entry(p)
            .or_default()
            .push(TargetCandidateNode {
                id: row.get(1)?,
                name: row.get(2)?,
                qualname: row.get(3)?,
                line: row.get::<_, i64>(4)?.max(1) as usize,
                end_line: row.get::<_, i64>(5)?.max(1) as usize,
            });
    }
    Ok(SymbolClaimTargets {
        candidates_by_path,
        indexed_code_paths,
    })
}

#[allow(clippy::too_many_arguments)]
fn check_symbol_claims(
    conn: &Connection,
    scope: Option<&LintScope>,
    symbol_claims: &[SymbolClaimRef],
    candidates_by_path: &HashMap<String, Vec<TargetCandidateNode>>,
    indexed_code_paths: &HashSet<String>,
    resolved_root: Option<&Path>,
    generation: Option<&str>,
    findings: &mut Vec<DocIntegrityFinding>,
) {
    let mut source_loader = source::VerifiedSourceCache::new();
    let mut line_count_cache: HashMap<String, Option<usize>> = HashMap::new();
    let claim_json = |c: &SymbolClaimRef| {
        json!({"name": c.name, "target_path": c.target_path, "start_line": c.start_line, "end_line": c.end_line})
    };
    let push_claim = |findings: &mut Vec<DocIntegrityFinding>,
                      kind: &'static str,
                      rule_id: &'static str,
                      sev: LintSeverity,
                      c: &SymbolClaimRef,
                      msg: String,
                      raw: Value| {
        let mut f = DocIntegrityFinding::new(kind, rule_id, sev, c.doc_path.clone(), c.line, msg);
        f.symbol = Some(c.name.clone());
        f.target = Some(c.target_path.clone());
        f.raw_diagnostic = Some(raw);
        findings.push(f);
    };

    for claim in symbol_claims {
        if scope.is_some_and(|s| !s.in_scope(&claim.doc_path)) {
            continue;
        }
        let path_cands = candidates_by_path
            .get(&claim.target_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (matched, suggestion) = match_symbol_claim(path_cands, &claim.name);
        if matched.is_empty() {
            // Existing files outside indexed `roots` (e.g. `tests/`, scripts, YAML) have no symbol
            // nodes in `code-map.sqlite`; skip them while still failing closed on missing targets.
            if !indexed_code_paths.contains(&claim.target_path)
                && !claim.target_path.starts_with('[')
                && resolved_root.is_some_and(|root| {
                    acdd_core::fs::checked_child(root, Path::new(&claim.target_path))
                        .is_ok_and(|path| path.is_file())
                })
            {
                continue;
            }
            let mut raw = json!({
                "kind": "dead_doc_symbol",
                "path": claim.doc_path,
                "line": claim.line,
                "claim": claim_json(claim),
            });
            if let Some(sug) = suggestion {
                raw["suggested_symbol"] =
                    json!({"name": sug.name, "path": claim.target_path, "line": sug.line});
            }
            let msg = format!(
                "broken symbol claim '{}' against '{}'",
                claim.name, claim.target_path
            );
            push_claim(
                findings,
                "dead_doc_symbol",
                "docs.broken_symbol_claim",
                LintSeverity::Error,
                claim,
                msg,
                raw,
            );
            continue;
        }
        if matched.len() > 1 {
            continue;
        }
        let target = matched[0];
        let (Some(start), Some(end)) = (claim.start_line, claim.end_line) else {
            continue;
        };
        let Some(root) = resolved_root else {
            continue;
        };
        let source_lines = *line_count_cache
            .entry(claim.target_path.clone())
            .or_insert_with(|| {
                source_loader
                    .read(conn, root, &claim.target_path)
                    .ok()
                    .map(|src| src.lines().count().max(1))
            });
        let Some(source_lines) = source_lines else {
            continue;
        };
        if start == 0 || end < start || end > source_lines {
            continue;
        }
        if end < target.line || start > target.end_line {
            let raw = json!({
                "kind": "stale_doc_sections",
                "reason": "documented_symbol_range_mismatch",
                "path": claim.doc_path,
                "line": claim.line,
                "claim": claim_json(claim),
                "target": {"id": target.id, "path": claim.target_path, "line": target.line, "end_line": target.end_line},
                "generation": generation.unwrap_or(""),
            });
            let msg = format!(
                "stale symbol line range #L{start}-L{end} for '{}' in '{}' (indexed at L{}-L{})",
                claim.name, claim.target_path, target.line, target.end_line
            );
            push_claim(
                findings,
                "stale_doc_sections",
                "docs.stale_symbol_range",
                LintSeverity::Warn,
                claim,
                msg,
                raw,
            );
        }
    }
}

fn check_duplicate_and_orphan_invariants(
    conn: &Connection,
    scope: Option<&LintScope>,
    enforced_invariants: &BTreeSet<String>,
    declared_invariants: &[DeclaredInvariantRef],
    non_contract_by_id: &BTreeMap<String, Vec<DeclaredInvariantRef>>,
    findings: &mut Vec<DocIntegrityFinding>,
) -> Result<()> {
    for (inv_id, refs) in non_contract_by_id {
        if refs.len() <= 1 {
            continue;
        }
        let first = &refs[0];
        for dup in refs {
            if scope.is_some_and(|s| !s.in_scope(&dup.doc_path)) {
                continue;
            }
            let mut f = DocIntegrityFinding::new(
                "duplicate_invariant",
                "docs.duplicate_invariant",
                LintSeverity::Error,
                dup.doc_path.clone(),
                dup.line,
                format!(
                    "duplicate non-contract invariant '{inv_id}' (declared {} times; first at {}:{})",
                    refs.len(),
                    first.doc_path,
                    first.line
                ),
            );
            f.invariant = Some(inv_id.clone());
            findings.push(f);
        }
    }

    if declared_invariants.is_empty() {
        return Ok(());
    }
    let mut governed_hashes = HashSet::new();
    let governs = EdgeKind::Governs.as_str();
    let inv_kind = NodeKind::Invariant.as_str();
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT e.src_hash
         FROM edges e
         JOIN nodes n ON n.node_hash = e.src_hash
         WHERE e.kind = '{governs}' AND n.kind = '{inv_kind}'"
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        governed_hashes.insert(row.get::<_, i64>(0)?);
    }
    for inv in declared_invariants {
        if scope.is_some_and(|s| !s.in_scope(&inv.doc_path)) {
            continue;
        }
        let node_hash = stable_hash64(&inv.node_id);
        if !governed_hashes.contains(&node_hash) && !enforced_invariants.contains(&inv.id) {
            let mut f = DocIntegrityFinding::new(
                "orphan_invariant",
                "docs.orphan_invariant",
                LintSeverity::Warn,
                inv.doc_path.clone(),
                inv.line,
                format!(
                    "orphan invariant '{}' has no verified governs symbol link or acdd-rule",
                    inv.id
                ),
            );
            f.invariant = Some(inv.id.clone());
            findings.push(f);
        }
    }
    Ok(())
}

fn collect_doc_findings(
    conn: &Connection,
    single_doc_path: Option<&str>,
    scope: Option<&LintScope>,
    enforced_invariants: &BTreeSet<String>,
    workspace_root: Option<&Path>,
    include_broken_links: bool,
    generation: Option<&str>,
) -> Result<Vec<DocIntegrityFinding>> {
    let _budget = reader::QueryBudget::new(conn);
    let mut findings = Vec::new();

    if include_broken_links {
        collect_broken_doc_links(conn, single_doc_path, scope, &mut findings)?;
    }

    let ScannedDocClaims {
        symbol_claims,
        declared_invariants,
        non_contract_by_id,
    } = scan_doc_metadata_claims(conn, single_doc_path)?;

    if single_doc_path.is_none() {
        collect_stored_doc_errors(conn, scope, &mut findings)?;
    }

    let SymbolClaimTargets {
        candidates_by_path,
        indexed_code_paths,
    } = load_symbol_claim_targets(conn, scope, &symbol_claims)?;

    let resolved_root: Option<PathBuf> = workspace_root.map(Path::to_path_buf).or_else(|| {
        read_metadata_optional(conn, MetadataKey::WorkspaceRoot)
            .ok()
            .flatten()
            .map(PathBuf::from)
    });
    check_symbol_claims(
        conn,
        scope,
        &symbol_claims,
        &candidates_by_path,
        &indexed_code_paths,
        resolved_root.as_deref(),
        generation,
        &mut findings,
    );

    if single_doc_path.is_none() {
        check_duplicate_and_orphan_invariants(
            conn,
            scope,
            enforced_invariants,
            &declared_invariants,
            &non_contract_by_id,
            &mut findings,
        )?;
    }

    Ok(findings)
}

pub(super) fn enforced_set_from_rules(rules: &[LoadedRule]) -> BTreeSet<String> {
    rules
        .iter()
        .filter_map(|r| rule_invariant_id(&r.spec))
        .collect()
}

pub(super) fn collect_scoped_doc_findings(
    conn: &Connection,
    index: &LintIndexData,
    enforced_invariants: &BTreeSet<String>,
    workspace_root: Option<&Path>,
) -> Result<Vec<DocIntegrityFinding>> {
    collect_doc_findings(
        conn,
        None,
        Some(&index.scope),
        enforced_invariants,
        workspace_root,
        false,
        None,
    )
}

fn push_finding_violation(
    violations: &mut Vec<LintViolation>,
    f: &DocIntegrityFinding,
    category: LintCategory,
    rule_id: String,
    severity: LintSeverity,
    rule: Option<&LoadedRule>,
) {
    let message = match rule.and_then(|r| r.spec.message.as_deref()) {
        Some(prefix) if !prefix.trim().is_empty() => format!("{prefix}: {}", f.message),
        _ => f.message.clone(),
    };
    violations.push(LintViolation {
        category,
        rule_id,
        severity,
        path: f.path.clone(),
        line: Some(f.line),
        message,
        invariant: f.invariant.clone().or_else(|| rule.and_then(|r| rule_invariant_id(&r.spec))),
        rule_source: rule.map(|r| r.source.clone()),
        symbol: f.symbol.clone(),
        target: f.target.clone(),
        suggestion: None,
    });
}

pub(super) fn evaluate_doc_integrity_rule(
    index: &LintIndexData,
    rule: &LoadedRule,
    zones: &BTreeMap<String, Vec<String>>,
    findings: &[DocIntegrityFinding],
    violations: &mut Vec<LintViolation>,
    covered: &mut HashSet<(String, usize, String)>,
) {
    for f in findings {
        if let Some(from_sel) = &rule.spec.from {
            if !selector_matches_file(from_sel, &f.path, zones, index.file_language(&f.path)) {
                continue;
            }
        }
        let severity = match f.kind {
            "orphan_invariant" | "stale_doc_sections" => rule.spec.severity,
            _ => f.default_severity.max(rule.spec.severity),
        };
        covered.insert((f.path.clone(), f.line, f.kind.to_owned()));
        push_finding_violation(violations, f, LintCategory::Arch, rule.spec.id.clone(), severity, Some(rule));
    }
    let _ = ArchRuleKind::DocIntegrity;
}

pub(super) fn evaluate_docs_lint(
    findings: &[DocIntegrityFinding],
    doc_integrity_covered: &HashSet<(String, usize, String)>,
    violations: &mut Vec<LintViolation>,
) {
    for f in findings {
        if !doc_integrity_covered.contains(&(f.path.clone(), f.line, f.kind.to_owned())) {
            push_finding_violation(violations, f, LintCategory::Docs, f.rule_id.to_owned(), f.default_severity, None);
        }
    }
}

/// Canonical `doc_diagnostics` projection shared with `db::reader::diagnostics::analyze_paged`.
pub(crate) fn document_diagnostics_paged(
    conn: &Connection,
    path: &str,
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
    const MAX_DOCUMENT_DIAGNOSTICS: usize = 2000;
    let empty_enforced = BTreeSet::new();
    let findings = collect_doc_findings(
        conn,
        Some(path),
        None,
        &empty_enforced,
        None,
        true,
        Some(generation),
    )?;
    let mut diagnostics: Vec<Value> = findings
        .into_iter()
        .filter_map(|f| f.raw_diagnostic)
        .collect();
    diagnostics.sort_by(|left, right| {
        left["line"]
            .as_u64()
            .cmp(&right["line"].as_u64())
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
            .then_with(|| left.to_string().cmp(&right.to_string()))
    });
    diagnostics.truncate(MAX_DOCUMENT_DIAGNOSTICS);
    let total = diagnostics.len();
    let items = diagnostics
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    Ok(paging::value(items, total, options, generation))
}
