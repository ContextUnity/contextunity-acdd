use super::super::common::{combine_file_hashes, open_code_db_readonly};
use super::common::{
    collect_deleted_scoped_files, collect_scoped_worktree_files, contains_identifier,
    effective_baseline_ref, normalize_sig, public_callable_nodes, CodeDomainProvider,
};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::git::{git_command, read_git_blob_utf8};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq};
use acdd_index::ast::extract_typed;
use acdd_index::languages;
use acdd_index::models::tags::NodeKind;
use acdd_index::models::{Facts, Node};
use anyhow::Result;
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub(super) type ExternalCallerTargetKey = (String, String, String, String);

pub(super) fn external_caller_target_key(symbol: &Node) -> ExternalCallerTargetKey {
    (
        symbol.path.clone(),
        symbol.kind.as_str().to_owned(),
        symbol.name.clone(),
        symbol.qualname.clone(),
    )
}

impl CodeDomainProvider {
    pub(super) fn observe_signature_scope_state(
        &self,
        req: &DomainCheckReq,
    ) -> Result<(Option<Connection>, String, Vec<String>, String)> {
        let policy = &req.path_policy;
        let baseline = effective_baseline_ref(req.baseline_ref.as_deref());
        let conn = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref());
        let index_gen = Self::index_generation(conn.as_ref());
        let mut scoped_set: BTreeSet<String> =
            collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, policy)
                .into_iter()
                .collect();
        scoped_set.extend(collect_deleted_scoped_files(
            &req.worktree,
            &req.admitted_scope,
            policy,
            baseline,
        ));
        let scoped_files: Vec<String> = scoped_set.into_iter().collect();
        let mut file_hashes: BTreeMap<String, String> = BTreeMap::new();
        for rel in &scoped_files {
            if languages::for_path(Path::new(rel)).is_none() {
                continue;
            }
            let hash = checked_child(&req.worktree, Path::new(rel))
                .ok()
                .and_then(|abs| fs::read_to_string(&abs).ok())
                .map_or_else(|| "<deleted>".to_owned(), |src| sha256_hex(src.as_bytes()));
            file_hashes.insert(rel.clone(), hash);
        }
        let live_digest = combine_file_hashes(&file_hashes);
        Ok((conn, index_gen, scoped_files, live_digest))
    }

    pub(super) fn run_signature_scope_check(
        &self,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        let policy = &req.path_policy;
        let baseline = effective_baseline_ref(req.baseline_ref.as_deref());
        let (conn, index_gen, scoped_files, live_digest) =
            self.observe_signature_scope_state(req)?;
        let mut violations: Vec<String> = Vec::new();

        let mut changed_nodes: Vec<(String, Node)> = Vec::new();
        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Some(head_src) = read_git_blob_utf8(&req.worktree, baseline, rel)
                .ok()
                .flatten()
            else {
                continue;
            };

            let head_facts = extract_typed(rel, lang.id(), &head_src)?.into_public();
            let mut cur_sigs: BTreeMap<(String, String), String> = BTreeMap::new();
            if let Some(current_src) = checked_child(&req.worktree, Path::new(rel))
                .ok()
                .and_then(|abs| fs::read_to_string(&abs).ok())
            {
                let cur_facts = extract_typed(rel, lang.id(), &current_src)?.into_public();
                for node in public_callable_nodes(&cur_facts) {
                    let sig = normalize_sig(node.details["signature"].as_str().unwrap_or_default());
                    cur_sigs.insert((node.kind.as_str().to_owned(), node.qualname.clone()), sig);
                }
            }

            for head_node in public_callable_nodes(&head_facts) {
                let head_sig =
                    normalize_sig(head_node.details["signature"].as_str().unwrap_or_default());
                let cur_sig = cur_sigs.get(&(
                    head_node.kind.as_str().to_owned(),
                    head_node.qualname.clone(),
                ));
                if cur_sig.is_some_and(|s| s == &head_sig) {
                    continue;
                }
                changed_nodes.push((rel.clone(), head_node));
            }
        }

        let target_refs: Vec<&Node> = changed_nodes.iter().map(|(_, node)| node).collect();
        let mut callers_by_target = find_external_callers(
            &req.worktree,
            conn.as_ref(),
            &target_refs,
            &req.admitted_scope,
            policy,
            baseline,
        )?;
        for (rel, head_node) in changed_nodes {
            let callers = callers_by_target
                .remove(&external_caller_target_key(&head_node))
                .unwrap_or_default();
            if !callers.is_empty() {
                violations.push(format!(
                    "{}::{} signature changed with un-scoped non-test callers: {}",
                    rel,
                    head_node.qualname,
                    callers.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }

        if violations.is_empty() {
            Ok(DomainCheckOutcome::Passed {
                summary: format!(
                    "code.signature_scope verified {} scoped file(s) with zero un-scoped external callers",
                    scoped_files.len()
                ),
                execution_digest_salt: Some(format!(
                    "index_generation={index_gen};live_files_digest={live_digest}"
                )),
            })
        } else {
            Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "code.signature_scope detected {} public signature change(s) with callers outside admitted_scope",
                    violations.len()
                ),
                diagnostics: violations,
            })
        }
    }
}

fn find_external_callers_in_index(
    worktree: &Path,
    conn: &Connection,
    symbols: &[&Node],
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<BTreeMap<ExternalCallerTargetKey, BTreeSet<String>>> {
    let mut indexed: BTreeMap<ExternalCallerTargetKey, BTreeSet<String>> = BTreeMap::new();
    let paths: Vec<&str> = symbols
        .iter()
        .map(|s| s.path.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if paths.is_empty() {
        return Ok(indexed);
    }

    for chunk in paths.chunks(256) {
        let placeholders = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT tp.path, target.kind, target.name, target.qualname, cp.path
             FROM nodes target
             JOIN path_dictionary tp ON tp.path_id = target.path_id
             LEFT JOIN edges e ON e.dst_hash = target.node_hash
               AND (
                   (target.kind = 'table' AND e.kind IN ('defines_table', 'queries_table', 'mutates_table', 'references_table'))
                   OR (target.kind != 'table' AND e.kind IN ('calls', 'imports', 'possible_calls', 'implements', 'overrides'))
               )
             LEFT JOIN nodes caller ON caller.node_hash = e.src_hash AND caller.is_test = 0
             LEFT JOIN path_dictionary cp ON cp.path_id = caller.path_id
             WHERE tp.path IN ({placeholders})
               AND target.kind IN ('function', 'method', 'table')
             ORDER BY tp.path, target.qualname, cp.path"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<SqlValue> = chunk
            .iter()
            .map(|p| SqlValue::Text((*p).to_owned()))
            .collect();
        let rows = stmt.query_map(params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows.flatten() {
            let (tpath, tkind, tname, tqualname, cpath) = row;
            let entry = indexed.entry((tpath, tkind, tname, tqualname)).or_default();
            if let Some(path) = cpath {
                if checked_child(worktree, Path::new(&path)).is_ok_and(|abs| abs.is_file())
                    && !policy.is_scope_admitted(&path, admitted_scope)
                {
                    entry.insert(path);
                }
            }
        }
    }
    Ok(indexed)
}

fn normalize_sql_ident(raw: &str) -> Vec<String> {
    raw.split('.')
        .map(|seg| {
            let trimmed = seg.trim();
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                trimmed[1..trimmed.len() - 1].to_owned()
            } else {
                trimmed.trim_matches(['`', '[', ']']).to_ascii_lowercase()
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn matches_sql_table_expression(expr: &str, symbol: &Node) -> bool {
    let expr_parts = normalize_sql_ident(expr);
    let target_parts = normalize_sql_ident(&symbol.qualname);
    let (Some(expr_short), Some(target_short)) = (expr_parts.last(), target_parts.last()) else {
        return false;
    };
    if expr_parts.len() > 1 {
        expr_parts == target_parts
    } else {
        expr_short == target_short
    }
}

fn is_sql_table_reference_kind(kind: &str) -> bool {
    matches!(
        kind,
        "defines_table"
            | "queries_table"
            | "mutates_table"
            | "references_table"
            | "sql_type_queries_table"
    )
}

fn extract_external_facts(worktree: &Path, baseline: &str, path: &str) -> Option<Facts> {
    let abs = checked_child(worktree, Path::new(path)).ok()?;
    if !abs.is_file() {
        return None;
    }
    let lang = languages::for_path(Path::new(path))?;
    let src = fs::read_to_string(&abs)
        .ok()
        .or_else(|| read_git_blob_utf8(worktree, baseline, path).ok().flatten())?;
    extract_typed(path, lang.id(), &src)
        .ok()
        .map(|typed| typed.into_public())
}

fn facts_reference_symbol(facts: &Facts, symbol: &Node) -> bool {
    let is_table = symbol.kind == NodeKind::Table;
    facts.references.iter().any(|r| {
        let is_test_src = facts.nodes.iter().any(|n| n.id == r.source && n.is_test);
        if is_test_src {
            return false;
        }
        if is_table {
            is_sql_table_reference_kind(r.kind.as_str())
                && matches_sql_table_expression(&r.expression, symbol)
        } else {
            contains_identifier(&r.expression, &symbol.name)
        }
    })
}

pub(super) fn find_external_callers(
    worktree: &Path,
    conn: Option<&Connection>,
    symbols: &[&Node],
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Result<BTreeMap<ExternalCallerTargetKey, BTreeSet<String>>> {
    let mut results: BTreeMap<ExternalCallerTargetKey, BTreeSet<String>> = BTreeMap::new();
    if symbols.is_empty() {
        return Ok(results);
    }
    if let Some(conn) = conn {
        let mut indexed =
            find_external_callers_in_index(worktree, conn, symbols, admitted_scope, policy)?;
        for sym in symbols {
            let key = external_caller_target_key(sym);
            if let Some(callers) = indexed.remove(&key) {
                results.insert(key, callers);
            }
        }
    }

    let missing: Vec<&Node> = symbols
        .iter()
        .copied()
        .filter(|sym| !results.contains_key(&external_caller_target_key(sym)))
        .collect();
    if missing.is_empty()
        || baseline.starts_with('-')
        || (!worktree.join(".git").exists() && acdd_core::git::git_common_dir(worktree).is_err())
    {
        return Ok(results);
    }

    // Fallback when code_db is absent or refreshed past a deleted target symbol: consult `baseline`
    // and live external files on disk in batched git-grep passes.
    let case_insensitive = missing.iter().any(|sym| sym.kind == NodeKind::Table);
    let names: Vec<&str> = missing
        .iter()
        .map(|sym| sym.name.as_str())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let prefix = format!("{baseline}:");
    let mut candidate_paths = BTreeSet::new();
    for chunk in names.chunks(64) {
        let mut args: Vec<&str> = vec!["grep", "-z", "-l", "-F"];
        if case_insensitive {
            args.push("-i");
        }
        for name in chunk {
            args.push("-e");
            args.push(name);
        }
        args.push(baseline);
        args.push("--");
        let Ok(out) = git_command(worktree, &args) else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let s = String::from_utf8_lossy(raw);
            let path = s.strip_prefix(&prefix).unwrap_or(&s);
            if !policy.is_scope_admitted(path, admitted_scope) {
                candidate_paths.insert(path.to_owned());
            }
        }
    }

    for path in candidate_paths {
        let Some(facts) = extract_external_facts(worktree, baseline, &path) else {
            continue;
        };
        for sym in &missing {
            if facts_reference_symbol(&facts, sym) {
                results
                    .entry(external_caller_target_key(sym))
                    .or_default()
                    .insert(path.clone());
            }
        }
    }
    Ok(results)
}
