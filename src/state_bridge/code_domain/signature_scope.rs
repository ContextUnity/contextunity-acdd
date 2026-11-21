use super::super::open_code_db_readonly;
use super::{
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
use acdd_index::models::Node;
use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

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
        let live_digest = super::super::combine_file_hashes(&file_hashes);
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

        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Some(head_src) = read_git_blob_utf8(&req.worktree, baseline, rel).ok().flatten()
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
                    cur_sigs.insert((node.kind.clone(), node.qualname.clone()), sig);
                }
            }

            for head_node in public_callable_nodes(&head_facts) {
                let head_sig =
                    normalize_sig(head_node.details["signature"].as_str().unwrap_or_default());
                let cur_sig = cur_sigs.get(&(head_node.kind.clone(), head_node.qualname.clone()));
                if cur_sig.is_some_and(|s| s == &head_sig) {
                    continue;
                }
                // Public signature changed or removed; check external non-test callers.
                let callers = find_external_callers(
                    &req.worktree,
                    conn.as_ref(),
                    &head_node,
                    &req.admitted_scope,
                    policy,
                    baseline,
                )?;
                if !callers.is_empty() {
                    violations.push(format!(
                        "{}::{} signature changed with un-scoped non-test callers: {}",
                        rel,
                        head_node.qualname,
                        callers.into_iter().collect::<Vec<_>>().join(", ")
                    ));
                }
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

pub(super) fn find_external_callers(
    worktree: &Path,
    conn: Option<&Connection>,
    symbol: &Node,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Result<BTreeSet<String>> {
    let mut external = BTreeSet::new();
    if let Some(conn) = conn {
        let target_in_index: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                    FROM nodes target
                    JOIN path_dictionary tp ON tp.path_id = target.path_id
                    WHERE tp.path = ?1 AND target.kind = ?2 AND target.name = ?3 AND target.qualname = ?4
                )",
                params![symbol.path, symbol.kind, symbol.name, symbol.qualname],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if target_in_index {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT cp.path
                 FROM nodes target
                 JOIN path_dictionary tp ON tp.path_id = target.path_id
                 JOIN edges e ON e.dst_hash = target.node_hash
                   AND e.kind IN ('calls', 'imports', 'possible_calls', 'implements', 'overrides')
                 JOIN nodes caller ON caller.node_hash = e.src_hash AND caller.is_test = 0
                 JOIN path_dictionary cp ON cp.path_id = caller.path_id
                 WHERE tp.path = ?1 AND target.kind = ?2 AND target.name = ?3 AND target.qualname = ?4
                 ORDER BY cp.path",
            )?;
            let rows = stmt.query_map(
                params![symbol.path, symbol.kind, symbol.name, symbol.qualname],
                |row| row.get::<_, String>(0),
            )?;
            for path in rows.flatten() {
                if !checked_child(worktree, Path::new(&path)).is_ok_and(|abs| abs.is_file()) {
                    continue;
                }
                if !policy.is_scope_admitted(&path, admitted_scope) {
                    external.insert(path);
                }
            }
            return Ok(external);
        }
    }

    // Fallback when code_db is absent or refreshed past a deleted target symbol: consult `baseline`
    // and live external files on disk.
    if !baseline.starts_with('-')
        && (worktree.join(".git").exists() || acdd_core::git::git_common_dir(worktree).is_ok())
    {
        if let Ok(out) = git_command(
            worktree,
            &["grep", "-z", "-l", "-F", "-e", &symbol.name, baseline, "--"],
        ) {
            if out.status.success() {
                let prefix = format!("{baseline}:");
                for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
                    let s = String::from_utf8_lossy(raw);
                    let path = s.strip_prefix(&prefix).unwrap_or(&s);
                    if !policy.is_scope_admitted(path, admitted_scope) {
                        let Ok(abs) = checked_child(worktree, Path::new(path)) else {
                            continue;
                        };
                        if !abs.is_file() {
                            continue;
                        }
                        if let Some(lang) = languages::for_path(Path::new(path)) {
                            let src_opt = fs::read_to_string(&abs)
                                .ok()
                                .or_else(|| read_git_blob_utf8(worktree, baseline, path).ok().flatten());
                            if let Some(src) = src_opt {
                                if let Ok(typed) = extract_typed(path, lang.id(), &src) {
                                    let non_test_ref = typed.facts.references.iter().any(|r| {
                                        let is_test_src = typed
                                            .facts
                                            .nodes
                                            .iter()
                                            .any(|n| n.id == r.source && n.is_test);
                                        !is_test_src
                                            && contains_identifier(&r.expression, &symbol.name)
                                    });
                                    if non_test_ref {
                                        external.insert(path.to_owned());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(external)
}
