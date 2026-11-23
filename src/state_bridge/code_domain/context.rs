use super::super::common::{
    budget_items, open_code_db_readonly, truncate_single_line, CODE_EXTERNAL_CALLERS_MAX_ITEMS,
    CODE_SYMBOLS_MAX_ITEMS, CODE_SYMBOLS_MAX_SIG_CHARS, CODE_SYMBOLS_SMALL_FILE_PRIVATE_THRESHOLD,
    CODE_TESTS_LENS_PAGE_SIZE, CODE_TESTS_LENS_SCOPED_FILE_LIMIT, CODE_TESTS_MAX_ITEMS,
    CODE_TESTS_PER_FILE_GRAPH_LIMIT,
};
use super::common::{
    collect_deleted_scoped_files, collect_git_diff_paths, collect_scoped_worktree_files,
    effective_baseline_ref, is_interface_like_kind, is_non_symbol_container, normalize_sig,
    public_callable_nodes, CodeDomainProvider, ParsedScopedFile, ScopedCodeSnapshot,
};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::git::read_git_blob_utf8;
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::DomainContextReq;
use acdd_core::response::{QueryOptions, ResponsePolicy};
use acdd_index::ast::extract_typed;
use acdd_index::db::symbols;
use acdd_index::languages;
use acdd_index::models::Node;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn collect_graph_test_items(
    worktree: &Path,
    conn: &Connection,
    snapshot: &ScopedCodeSnapshot,
    seen: &mut BTreeSet<(String, String)>,
    ranked_items: &mut Vec<(u8, usize, usize, Value)>,
) -> Result<()> {
    let page = QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(CODE_TESTS_LENS_PAGE_SIZE),
        0,
        None,
        None,
    )?;
    for (file_idx, rel) in snapshot
        .files
        .iter()
        .take(CODE_TESTS_LENS_SCOPED_FILE_LIMIT)
        .enumerate()
    {
        let Ok(res) = symbols::tests_paged_with_path(conn, rel, None, "inbound", &page) else {
            continue;
        };
        let Some(arr) = res.pointer("/nodes/items").and_then(Value::as_array) else {
            continue;
        };
        let mut per_file_rank = 0usize;
        for item in arr {
            if per_file_rank >= CODE_TESTS_PER_FILE_GRAPH_LIMIT {
                break;
            }
            let path = item["path"].as_str().unwrap_or_default().to_owned();
            if !checked_child(worktree, Path::new(&path)).is_ok_and(|p| p.is_file()) {
                continue;
            }
            let qualname = item["qualname"].as_str().unwrap_or_default().to_owned();
            if seen.insert((path.clone(), qualname.clone())) {
                ranked_items.push((
                    0u8,
                    per_file_rank,
                    file_idx,
                    json!({
                        "path": path,
                        "name": item["name"],
                        "qualname": qualname,
                        "line": item["line"],
                        "discovery": "graph",
                    }),
                ));
                per_file_rank += 1;
            }
        }
    }
    Ok(())
}

fn collect_live_test_items(
    snapshot: &ScopedCodeSnapshot,
    seen: &mut BTreeSet<(String, String)>,
    ranked_items: &mut Vec<(u8, usize, usize, Value)>,
) {
    for (file_idx, rel) in snapshot.files.iter().enumerate() {
        let Some(parsed) = snapshot.parsed.get(rel) else {
            continue;
        };
        let mut per_file_rank = 0usize;
        for node in &parsed.facts.nodes {
            let is_test_case =
                node.details.get("test_role").and_then(Value::as_str) == Some("test_case");
            if is_test_case && seen.insert((node.path.clone(), node.qualname.clone())) {
                ranked_items.push((
                    1u8,
                    per_file_rank,
                    file_idx,
                    json!({
                        "path": node.path,
                        "name": node.name,
                        "qualname": node.qualname,
                        "line": node.line,
                        "discovery": "live_ast",
                    }),
                ));
                per_file_rank += 1;
            }
        }
    }
}

impl CodeDomainProvider {
    fn effective_context_files(&self, req: &DomainContextReq) -> Vec<String> {
        let files =
            collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &req.path_policy);
        if !files.is_empty() || !req.admitted_scope.is_empty() || req.baseline_ref.is_none() {
            return files;
        }
        let baseline = effective_baseline_ref(req.baseline_ref.as_deref());
        collect_changed_worktree_files(&req.worktree, &req.path_policy, baseline)
    }

    fn scoped_code_snapshot(&self, req: &DomainContextReq) -> Arc<ScopedCodeSnapshot> {
        let files = self.effective_context_files(req);
        let mut live_inputs: Vec<(String, &'static str, String, String)> = Vec::new();
        for rel in &files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Ok(abs) = checked_child(&req.worktree, Path::new(rel)) else {
                continue;
            };
            let Ok(source) = fs::read_to_string(&abs) else {
                continue;
            };
            let digest = sha256_hex(source.as_bytes());
            live_inputs.push((rel.clone(), lang.id(), source, digest));
        }

        if let Ok(guard) = self.snapshot_cache.lock() {
            if let Some(ref cached) = *guard {
                let digests_match = cached.worktree == req.worktree
                    && cached.files == files
                    && cached.parsed.len() == live_inputs.len()
                    && live_inputs.iter().all(|(rel, _, _, digest)| {
                        cached
                            .parsed
                            .get(rel)
                            .is_some_and(|entry| entry.source_digest == *digest)
                    });
                if digests_match {
                    return Arc::clone(cached);
                }
            }
        }

        let mut parsed = BTreeMap::new();
        for (rel, lang_id, source, digest) in live_inputs {
            if let Ok(typed) = extract_typed(&rel, lang_id, &source) {
                parsed.insert(
                    rel,
                    ParsedScopedFile {
                        source_digest: digest,
                        facts: typed.into_public(),
                    },
                );
            }
        }
        let snap = Arc::new(ScopedCodeSnapshot {
            worktree: req.worktree.clone(),
            files,
            parsed,
        });
        if let Ok(mut guard) = self.snapshot_cache.lock() {
            *guard = Some(Arc::clone(&snap));
        }
        snap
    }

    pub(super) fn provide_symbols(&self, req: &DomainContextReq) -> Result<Value> {
        let snapshot = self.scoped_code_snapshot(req);
        let target_text = req.target.as_deref().unwrap_or_default();
        let mut all_candidates = Vec::new();
        for (file_idx, rel) in snapshot.files.iter().enumerate() {
            let Some(parsed) = snapshot.parsed.get(rel) else {
                continue;
            };
            let file_nodes: Vec<_> = parsed
                .facts
                .nodes
                .iter()
                .filter(|n| !is_non_symbol_container(&n.kind) && !n.is_test)
                .collect();
            let keep_private = file_nodes.len() <= CODE_SYMBOLS_SMALL_FILE_PRIVATE_THRESHOLD;
            let mut file_candidates = Vec::new();
            for node in file_nodes {
                let raw_sig = node.details["signature"].as_str().unwrap_or_default();
                let compact_sig = truncate_single_line(raw_sig, CODE_SYMBOLS_MAX_SIG_CHARS);
                let is_exported = compact_sig.starts_with("pub")
                    || compact_sig.starts_with("export ")
                    || is_interface_like_kind(&node.kind);
                let target_hit = !target_text.is_empty()
                    && (target_text.contains(&node.name) || target_text.contains(&node.qualname));
                if !keep_private && !is_exported && !target_hit {
                    continue;
                }
                file_candidates.push((
                    !target_hit,
                    !is_exported,
                    node.line,
                    json!({
                        "qualname": node.qualname,
                        "line": node.line,
                        "end_line": node.end_line,
                        "signature": compact_sig,
                    }),
                ));
            }
            file_candidates.sort_by_key(|c| (c.0, c.1, c.2));
            for (rank_in_file, (not_target, not_exported, _, val)) in
                file_candidates.into_iter().enumerate()
            {
                all_candidates.push((not_target, rank_in_file, not_exported, file_idx, val));
            }
        }
        all_candidates.sort_by_key(|a| (a.0, a.1, a.2, a.3));
        let mut items: Vec<Value> = all_candidates.into_iter().map(|(_, _, _, _, v)| v).collect();
        items.truncate(CODE_SYMBOLS_MAX_ITEMS);
        Ok(budget_items("symbols", items, req.max_bytes))
    }

    pub(super) fn provide_tests(&self, req: &DomainContextReq) -> Result<Value> {
        let snapshot = self.scoped_code_snapshot(req);
        let conn = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref());
        let mut ranked_items = Vec::new();
        let mut seen = BTreeSet::new();

        if let Some(ref conn) = conn {
            collect_graph_test_items(&req.worktree, conn, &snapshot, &mut seen, &mut ranked_items)?;
        }
        collect_live_test_items(&snapshot, &mut seen, &mut ranked_items);

        ranked_items.sort_by_key(|a| (a.0, a.1, a.2));
        let mut items: Vec<Value> = ranked_items.into_iter().map(|(_, _, _, v)| v).collect();
        items.truncate(CODE_TESTS_MAX_ITEMS);
        Ok(budget_items("tests", items, req.max_bytes))
    }

    pub(super) fn provide_external_callers(&self, req: &DomainContextReq) -> Result<Value> {
        let snapshot = self.scoped_code_snapshot(req);
        let policy = &req.path_policy;
        let baseline = effective_baseline_ref(req.baseline_ref.as_deref());
        let conn = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref());
        let scope_for_external: Vec<String> = if req.admitted_scope.is_empty() {
            snapshot.files.clone()
        } else {
            req.admitted_scope.clone()
        };
        let mut scoped_set: BTreeSet<String> = snapshot.files.iter().cloned().collect();
        let deleted_scope = if req.admitted_scope.is_empty() && req.baseline_ref.is_some() {
            vec!["**".to_owned()]
        } else {
            req.admitted_scope.clone()
        };
        scoped_set.extend(collect_deleted_scoped_files(
            &req.worktree,
            &deleted_scope,
            policy,
            baseline,
        ));
        let mut items = Vec::new();

        for rel in &scoped_set {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let head_nodes: Vec<Node> = read_git_blob_utf8(&req.worktree, baseline, rel)
                .ok()
                .flatten()
                .and_then(|head_src| extract_typed(rel, lang.id(), &head_src).ok())
                .map(|typed| public_callable_nodes(&typed.into_public()))
                .unwrap_or_default();
            let head_sigs: BTreeMap<(String, String), String> = head_nodes
                .iter()
                .map(|n| {
                    (
                        (n.kind.clone(), n.qualname.clone()),
                        normalize_sig(n.details["signature"].as_str().unwrap_or_default()),
                    )
                })
                .collect();

            let mut cur_keys: BTreeSet<(String, String)> = BTreeSet::new();
            if let Some(parsed) = snapshot.parsed.get(rel) {
                for node in public_callable_nodes(&parsed.facts) {
                    cur_keys.insert((node.kind.clone(), node.qualname.clone()));
                    let cur_sig =
                        normalize_sig(node.details["signature"].as_str().unwrap_or_default());
                    let signature_changed = head_sigs
                        .get(&(node.kind.clone(), node.qualname.clone()))
                        .is_some_and(|head_sig| head_sig != &cur_sig);

                    let callers = super::signature_scope::find_external_callers(
                        &req.worktree,
                        conn.as_ref(),
                        &node,
                        &scope_for_external,
                        policy,
                        baseline,
                    )?;
                    if !callers.is_empty() || signature_changed {
                        items.push(json!({
                            "path": node.path,
                            "symbol": node.qualname,
                            "signature_changed": signature_changed,
                            "external_callers": callers.into_iter().collect::<Vec<_>>(),
                        }));
                    }
                }
            }

            for head_node in head_nodes {
                if cur_keys.contains(&(head_node.kind.clone(), head_node.qualname.clone())) {
                    continue;
                }
                let callers = super::signature_scope::find_external_callers(
                    &req.worktree,
                    conn.as_ref(),
                    &head_node,
                    &scope_for_external,
                    policy,
                    baseline,
                )?;
                items.push(json!({
                    "path": head_node.path,
                    "symbol": head_node.qualname,
                    "signature_changed": true,
                    "external_callers": callers.into_iter().collect::<Vec<_>>(),
                }));
            }
        }

        items.truncate(CODE_EXTERNAL_CALLERS_MAX_ITEMS);
        Ok(budget_items("external_callers", items, req.max_bytes))
    }
}

fn collect_changed_worktree_files(
    worktree: &Path,
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Vec<String> {
    collect_git_diff_paths(worktree, policy, baseline, "--diff-filter=ACMRT", |norm| {
        checked_child(worktree, Path::new(norm)).is_ok_and(|abs| abs.is_file())
    })
}
