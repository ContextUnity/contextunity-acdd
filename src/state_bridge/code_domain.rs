//! Host `CodeDomainProvider` (`domain_id = "code"`) implementation.

use super::{budget_items, combine_file_hashes, open_code_db_readonly};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::git::{git_command, read_git_blob_utf8};
use acdd_core::path_policy::{expand_glob_braces, normalize_rel_path, WorkspacePathPolicy};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider, FreshnessObservation,
};
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_core::schema::SchemaDef;
use acdd_index::ast::extract_typed;
use acdd_index::db::{paging, symbols};
use acdd_index::languages::{self, dependency_registry::DependencyRegistry};
use acdd_index::linker;
use acdd_index::models::{Facts, Node};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_SEAM_HOPS: usize = 8;
const MAX_SEAM_CONTEXT_FILES: usize = 128;
const MAX_SEAM_HYDRATION_ROUNDS: usize = 3;
const MAX_DB_HYDRATE_FILES: usize = 64;
const SEAM_EDGE_KINDS: &[&str] = &[
    "calls", "possible_calls", "imports", "implements", "overrides",
    "inherits", "references", "handles", "mutates", "decorates",
];

struct PreparedSeamState {
    conn: Option<Connection>,
    index_gen: String,
    dependency_registry: DependencyRegistry,
    scoped_files: Vec<String>,
    scoped_code_files: Vec<String>,
    has_code_scope: bool,
    live_facts: BTreeMap<String, Facts>,
    file_hashes: BTreeMap<String, String>,
    test_starts: Vec<Node>,
    live_digest: String,
}

impl PreparedSeamState {
    fn salt(&self) -> String {
        format!(
            "index_generation={};live_files_digest={}",
            self.index_gen, self.live_digest
        )
    }
}

struct ParsedScopedFile {
    source_digest: String,
    facts: Facts,
}

struct ScopedCodeSnapshot {
    worktree: PathBuf,
    files: Vec<String>,
    parsed: BTreeMap<String, ParsedScopedFile>,
}

/// Host `code` domain provider exposing `code.seam`, `code.signature_scope`, and `code.*` context lenses.
pub struct CodeDomainProvider {
    root: PathBuf,
    code_db: Option<PathBuf>,
    snapshot_cache: Mutex<Option<Arc<ScopedCodeSnapshot>>>,
}

impl CodeDomainProvider {
    /// Construct a `CodeDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self::with_code_db(root, config, None)
    }

    /// Construct a `CodeDomainProvider` bound to `root`, workspace `config`, and an optional explicit `code_db` path.
    pub fn with_code_db(root: PathBuf, _config: Adapter, code_db: Option<PathBuf>) -> Self {
        Self {
            root,
            code_db,
            snapshot_cache: Mutex::new(None),
        }
    }

    fn index_generation(conn: Option<&Connection>) -> String {
        let Some(conn) = conn else {
            return "no-index".to_owned();
        };
        let opts = QueryOptions {
            limit: 1,
            offset: 0,
            detail: Detail::Compact,
            generation: None,
        };
        paging::generation(conn, &opts).unwrap_or_else(|_| "unknown".to_owned())
    }

    fn prepare_seam_state(&self, req: &DomainCheckReq) -> Result<PreparedSeamState> {
        let policy = &req.path_policy;
        let conn = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref());
        let index_gen = Self::index_generation(conn.as_ref());

        let mut live_facts: BTreeMap<String, Facts> = BTreeMap::new();
        let mut file_hashes: BTreeMap<String, String> = BTreeMap::new();

        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, policy);
        for rel in &scoped_files {
            load_live_facts(&req.worktree, rel, &mut live_facts, &mut file_hashes);
        }
        let mut changed_code_scope = false;
        for rel in &req.changed_paths {
            if let Some(norm) = normalize_rel_path(rel) {
                if languages::for_path(Path::new(&norm)).is_some()
                    && (req.admitted_scope.is_empty()
                        || policy.is_scope_admitted(&norm, &req.admitted_scope))
                {
                    changed_code_scope = true;
                }
                load_live_facts(&req.worktree, &norm, &mut live_facts, &mut file_hashes);
            }
        }

        let scoped_code_files: Vec<String> = scoped_files
            .iter()
            .filter(|p| languages::for_path(Path::new(p)).is_some())
            .cloned()
            .collect();
        let has_code_scope = !scoped_code_files.is_empty()
            || changed_code_scope
            || req
                .admitted_scope
                .iter()
                .flat_map(|s| expand_glob_braces(s))
                .any(|expanded| languages::for_path(Path::new(&expanded)).is_some());

        let test_starts = if !has_code_scope {
            Vec::new()
        } else if let Some(raw_target) = req
            .test_target
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            resolve_test_start_nodes(
                &req.worktree,
                raw_target,
                conn.as_ref(),
                &mut live_facts,
                &mut file_hashes,
            )?
        } else if scoped_code_files.is_empty() {
            Vec::new()
        } else {
            discover_fallback_test_starts(
                &req.worktree,
                &scoped_files,
                conn.as_ref(),
                &mut live_facts,
                &mut file_hashes,
            )
        };

        let dependency_registry = DependencyRegistry::collect(Some(&req.worktree));
        if has_code_scope && !test_starts.is_empty() {
            if let Some(ref conn) = conn {
                hydrate_modified_db_targets(
                    &req.worktree,
                    conn,
                    policy,
                    &mut live_facts,
                    &mut file_hashes,
                );
            }
            hydrate_seam_context_files(
                &req.worktree,
                &dependency_registry,
                policy,
                &mut live_facts,
                &mut file_hashes,
            );
        }

        let live_digest = combine_file_hashes(&file_hashes);
        Ok(PreparedSeamState {
            conn,
            index_gen,
            dependency_registry,
            scoped_files,
            scoped_code_files,
            has_code_scope,
            live_facts,
            file_hashes,
            test_starts,
            live_digest,
        })
    }

    fn observe_signature_scope_state(&self, req: &DomainCheckReq) -> Result<(Option<Connection>, String, Vec<String>, String)> {
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

    fn run_seam_check(&self, params: &Value, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let target_symbol = params
            .get("target_symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let policy = &req.path_policy;
        let state = self.prepare_seam_state(req)?;
        let salt = state.salt();

        // Always verify `has_code_scope` first, even when `--test` was provided.
        if !state.has_code_scope {
            return Ok(DomainCheckOutcome::Passed {
                summary: "code.seam: no code language targets in admitted_scope".to_owned(),
                execution_digest_salt: Some(salt),
            });
        }

        // Pure-deletion tasks (all scoped code files deleted on disk).
        if state.scoped_code_files.is_empty() {
            if let Some(raw_target) = req
                .test_target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if state.test_starts.is_empty() {
                    return Ok(DomainCheckOutcome::Failed {
                        summary: format!(
                            "code.seam could not resolve live test symbol(s) for test target '{raw_target}'"
                        ),
                        diagnostics: vec![format!("admitted_scope: {:?}", req.admitted_scope)],
                    });
                }
            }
            let has_deleted_scoped_code = req.changed_paths.iter().any(|rel| {
                normalize_rel_path(rel).is_some_and(|norm| {
                    languages::for_path(Path::new(&norm)).is_some()
                        && (req.admitted_scope.is_empty()
                            || policy.is_scope_admitted(&norm, &req.admitted_scope))
                        && checked_child(&req.worktree, Path::new(&norm))
                            .is_ok_and(|abs| !abs.exists())
                })
            });
            if has_deleted_scoped_code {
                return Ok(DomainCheckOutcome::Passed {
                    summary: "code.seam: all scoped code files deleted on disk".to_owned(),
                    execution_digest_salt: Some(salt),
                });
            }
            return Ok(DomainCheckOutcome::Failed {
                summary: "code.seam found no live or deleted scoped code files in admitted_scope"
                    .to_owned(),
                diagnostics: vec![
                    format!("admitted_scope: {:?}", req.admitted_scope),
                    format!("changed_paths: {:?}", req.changed_paths),
                ],
            });
        }

        let PreparedSeamState {
            conn,
            index_gen,
            dependency_registry,
            scoped_files,
            scoped_code_files,
            live_facts,
            file_hashes,
            test_starts,
            ..
        } = state;

        // If `target_symbol` was explicitly given, verify it exists in live admitted_scope files.
        if let Some(ref sym) = target_symbol {
            let found = scoped_files.iter().any(|path| {
                live_facts.get(path).is_some_and(|facts| {
                    facts.nodes.iter().any(|n| !n.is_test && node_matches_symbol(n, sym))
                })
            });
            if !found {
                return Ok(DomainCheckOutcome::Failed {
                    summary: format!(
                        "code.seam target symbol '{sym}' was not found in live admitted scope"
                    ),
                    diagnostics: vec![format!("admitted_scope: {:?}", req.admitted_scope)],
                });
            }
        }

        if test_starts.is_empty() {
            let desc = req.test_target.as_deref().unwrap_or("<none>");
            return Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "code.seam could not resolve live test symbol(s) for test target '{desc}'"
                ),
                diagnostics: vec![format!("admitted_scope: {:?}", req.admitted_scope)],
            });
        }

        let has_callable_prod_symbol = scoped_code_files.iter().any(|path| {
            live_facts.get(path).is_some_and(|facts| {
                facts
                    .nodes
                    .iter()
                    .any(|n| !n.is_test && matches!(n.kind.as_str(), "function" | "method"))
            })
        });
        if !has_callable_prod_symbol {
            let test_overlaps_scope = test_starts
                .iter()
                .any(|s| policy.is_scope_admitted(&s.path, &req.admitted_scope));
            let test_references_scoped_module =
                test_starts_reference_scoped_files(&test_starts, &scoped_code_files, &live_facts);
            if test_overlaps_scope || test_references_scoped_module {
                return Ok(DomainCheckOutcome::Passed {
                    summary: "code.seam: pure test/type/const scope covered by matching test target"
                        .to_owned(),
                    execution_digest_salt: Some(salt),
                });
            }
        }

        // 3. Link all live-parsed files together via canonical `acdd-index` linker so seams resolve immediately.
        let live_graph = linker::link_with_registry(
            &live_facts,
            None,
            Some(&req.worktree),
            &dependency_registry,
        );
        let live_nodes: Vec<&Node> = live_facts.values().flat_map(|f| f.nodes.iter()).collect();
        let live_nodes_by_id: BTreeMap<String, &Node> =
            live_nodes.iter().map(|n| (n.id.clone(), *n)).collect();

        // 4. BFS up to MAX_SEAM_HOPS (8) from test start nodes over live linked graph + read-only code_db.
        #[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
        struct BfsKey {
            path: String,
            qualname: String,
        }
        struct BfsItem {
            key: BfsKey,
            node_id: String,
            depth: usize,
            trace: Vec<String>,
        }

        let mut queue: VecDeque<BfsItem> = VecDeque::new();
        let mut visited: BTreeSet<BfsKey> = BTreeSet::new();
        let mut discovered_edges: BTreeSet<String> = BTreeSet::new();
        let mut db_matches_cache: BTreeMap<String, bool> = BTreeMap::new();

        for start in &test_starts {
            let key = BfsKey {
                path: start.path.clone(),
                qualname: start.qualname.clone(),
            };
            if visited.insert(key.clone()) {
                let label = format!("{}::{}", start.path, start.qualname);
                queue.push_back(BfsItem {
                    key,
                    node_id: start.id.clone(),
                    depth: 0,
                    trace: vec![label],
                });
            }
        }

        while let Some(BfsItem {
            key: cur_key,
            node_id: cur_id,
            depth,
            trace,
        }) = queue.pop_front()
        {
            if depth >= MAX_SEAM_HOPS {
                continue;
            }

            // (a) Follow live linked graph edges from current node.
            for edge in live_graph
                .edges
                .iter()
                .filter(|e| e.src == cur_id && SEAM_EDGE_KINDS.contains(&e.kind.as_str()))
            {
                let Some(dst) = live_nodes_by_id.get(&edge.dst) else {
                    continue;
                };
                let edge_desc = format!(
                    "{}::{} -{}-> {}::{}",
                    cur_key.path, cur_key.qualname, edge.kind, dst.path, dst.qualname
                );
                discovered_edges.insert(edge_desc);

                let bare_module_import = edge.kind == "imports"
                    && dst.kind == "module"
                    && target_symbol.is_none();
                if !dst.is_test
                    && !bare_module_import
                    && policy.is_scope_admitted(&dst.path, &req.admitted_scope)
                    && target_symbol
                        .as_deref()
                        .is_none_or(|sym| node_matches_symbol(dst, sym))
                {
                    let mut final_trace = trace.clone();
                    final_trace.push(format!("{}::{}", dst.path, dst.qualname));
                    return Ok(DomainCheckOutcome::Passed {
                        summary: format!(
                            "code.seam reached {}::{} in {} hop(s): {}",
                            dst.path,
                            dst.qualname,
                            depth + 1,
                            final_trace.join(" -> ")
                        ),
                        execution_digest_salt: Some(salt),
                    });
                }

                let next_key = BfsKey {
                    path: dst.path.clone(),
                    qualname: dst.qualname.clone(),
                };
                if visited.insert(next_key.clone()) {
                    let mut next_trace = trace.clone();
                    next_trace.push(format!("{}::{}", dst.path, dst.qualname));
                    queue.push_back(BfsItem {
                        key: next_key,
                        node_id: dst.id.clone(),
                        depth: depth + 1,
                        trace: next_trace,
                    });
                }
            }

            // (b) Traverse read-only `code_db` edges only for files whose on-disk digest matches `code_db`.
            if let Some(ref conn) = conn {
                if path_matches_code_db(
                    &req.worktree,
                    conn,
                    &cur_key.path,
                    &file_hashes,
                    &mut db_matches_cache,
                ) && checked_child(&req.worktree, Path::new(&cur_key.path))
                    .is_ok_and(|p| p.is_file())
                {
                    for db_edge in query_db_seam_edges(conn, &cur_key.path, Some(&cur_key.qualname))? {
                        if !checked_child(&req.worktree, Path::new(&db_edge.dst_path))
                            .is_ok_and(|p| p.is_file())
                        {
                            continue;
                        }
                        let next_node_id = match live_facts.get(&db_edge.dst_path) {
                            Some(dst_live) => match dst_live
                                .nodes
                                .iter()
                                .find(|n| {
                                    n.qualname == db_edge.dst_qualname
                                        && n.kind == db_edge.dst_kind
                                })
                                .or_else(|| {
                                    dst_live
                                        .nodes
                                        .iter()
                                        .find(|n| n.qualname == db_edge.dst_qualname)
                                }) {
                                Some(n) => n.id.clone(),
                                None => continue,
                            },
                            None => db_edge.dst_id,
                        };

                        discovered_edges.insert(format!(
                            "{}::{} -{}-> {}::{}",
                            cur_key.path,
                            cur_key.qualname,
                            db_edge.kind,
                            db_edge.dst_path,
                            db_edge.dst_qualname
                        ));

                        let bare_module_import = db_edge.kind == "imports"
                            && db_edge.dst_kind == "module"
                            && target_symbol.is_none();
                        if !db_edge.dst_is_test
                            && !bare_module_import
                            && policy.is_scope_admitted(&db_edge.dst_path, &req.admitted_scope)
                            && target_symbol.as_deref().is_none_or(|sym| {
                                let normalized = sym.replace("::", ".");
                                db_edge.dst_name == sym
                                    || db_edge.dst_qualname == normalized
                                    || db_edge.dst_qualname.ends_with(&format!(".{normalized}"))
                            })
                        {
                            let mut final_trace = trace.clone();
                            final_trace
                                .push(format!("{}::{}", db_edge.dst_path, db_edge.dst_qualname));
                            return Ok(DomainCheckOutcome::Passed {
                                summary: format!(
                                    "code.seam reached {}::{} in {} hop(s): {}",
                                    db_edge.dst_path,
                                    db_edge.dst_qualname,
                                    depth + 1,
                                    final_trace.join(" -> ")
                                ),
                                execution_digest_salt: Some(salt),
                            });
                        }

                        let next_key = BfsKey {
                            path: db_edge.dst_path.clone(),
                            qualname: db_edge.dst_qualname.clone(),
                        };
                        if visited.insert(next_key.clone()) {
                            let mut next_trace = trace.clone();
                            next_trace
                                .push(format!("{}::{}", db_edge.dst_path, db_edge.dst_qualname));
                            queue.push_back(BfsItem {
                                key: next_key,
                                node_id: next_node_id,
                                depth: depth + 1,
                                trace: next_trace,
                            });
                        }
                    }
                }
            }
        }

        let mut diagnostics = vec![
            format!("admitted_scope: {:?}", req.admitted_scope),
            format!("test_target: {:?}", req.test_target),
            format!("max_hops: {MAX_SEAM_HOPS}"),
            format!("index_generation: {index_gen}"),
        ];
        if !discovered_edges.is_empty() {
            diagnostics.push(format!(
                "discovered_edges: {}",
                discovered_edges
                    .into_iter()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        Ok(DomainCheckOutcome::Failed {
            summary: format!(
                "code.seam found no call/import path (<= {MAX_SEAM_HOPS} hops) from test target to admitted_scope"
            ),
            diagnostics,
        })
    }

    fn run_signature_scope_check(&self, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
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

    fn effective_context_files(&self, req: &DomainContextReq) -> Vec<String> {
        let files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &req.path_policy);
        if !files.is_empty() || req.gate_id.as_str() != "integration_review" {
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

    fn provide_symbols(&self, req: &DomainContextReq) -> Result<Value> {
        let snapshot = self.scoped_code_snapshot(req);
        let mut items = Vec::new();
        for rel in &snapshot.files {
            let Some(parsed) = snapshot.parsed.get(rel) else {
                continue;
            };
            for node in &parsed.facts.nodes {
                if node.kind == "module" || node.is_test {
                    continue;
                }
                items.push(json!({
                    "path": node.path,
                    "kind": node.kind,
                    "name": node.name,
                    "qualname": node.qualname,
                    "line": node.line,
                    "end_line": node.end_line,
                    "is_test": node.is_test,
                    "signature": node.details["signature"],
                }));
            }
        }
        Ok(budget_items("symbols", items, req.max_bytes))
    }

    fn provide_tests(&self, req: &DomainContextReq) -> Result<Value> {
        let snapshot = self.scoped_code_snapshot(req);
        let conn = open_code_db_readonly(&self.root, &req.worktree, self.code_db.as_deref());
        let mut items = Vec::new();
        let mut seen = BTreeSet::new();

        if let Some(ref conn) = conn {
            let page = QueryOptions::resolve(&ResponsePolicy::default(), Some(25), 0, None, None)?;
            for rel in snapshot.files.iter().take(10) {
                if let Ok(res) = symbols::tests_paged_with_path(conn, rel, None, "inbound", &page) {
                    if let Some(arr) = res.pointer("/nodes/items").and_then(Value::as_array) {
                        for item in arr {
                            let path = item["path"].as_str().unwrap_or_default().to_owned();
                            if !checked_child(&req.worktree, Path::new(&path))
                                .is_ok_and(|p| p.is_file())
                            {
                                continue;
                            }
                            let qualname = item["qualname"].as_str().unwrap_or_default().to_owned();
                            if seen.insert((path.clone(), qualname.clone())) {
                                items.push(json!({
                                    "path": path,
                                    "name": item["name"],
                                    "qualname": qualname,
                                    "line": item["line"],
                                    "discovery": "graph",
                                }));
                            }
                        }
                    }
                }
            }
        }

        // Also include live test symbols from scoped files.
        for rel in &snapshot.files {
            let Some(parsed) = snapshot.parsed.get(rel) else {
                continue;
            };
            for node in &parsed.facts.nodes {
                if node.is_test && seen.insert((node.path.clone(), node.qualname.clone())) {
                    items.push(json!({
                        "path": node.path,
                        "name": node.name,
                        "qualname": node.qualname,
                        "line": node.line,
                        "discovery": "live_ast",
                    }));
                }
            }
        }

        Ok(budget_items("tests", items, req.max_bytes))
    }

    fn provide_external_callers(&self, req: &DomainContextReq) -> Result<Value> {
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
        let deleted_scope = if req.admitted_scope.is_empty() && req.gate_id.as_str() == "integration_review" {
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

                    let callers = find_external_callers(
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
                let callers = find_external_callers(
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

        Ok(budget_items("external_callers", items, req.max_bytes))
    }
}

impl DomainProvider for CodeDomainProvider {
    fn domain_id(&self) -> &str {
        "code"
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: Vec::new(),
            checks: vec![
                CheckCapability {
                    name: "seam".to_owned(),
                    input_schema: SchemaDef::object(),
                },
                CheckCapability {
                    name: "signature_scope".to_owned(),
                    input_schema: SchemaDef::object(),
                },
            ],
            context_lenses: BTreeSet::from([
                "symbols".to_owned(),
                "tests".to_owned(),
                "external_callers".to_owned(),
            ]),
            tools: Vec::new(),
        }
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        super::unsupported_domain_action("code", action)
    }

    fn run_check(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "seam" => self.run_seam_check(params, req),
            "signature_scope" => self.run_signature_scope_check(req),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code check 'code.{check}'"),
            )
            .into()),
        }
    }

    fn observe_freshness(
        &self,
        check: &str,
        _params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        match check {
            "seam" => {
                let state = self.prepare_seam_state(req)?;
                let salt = state.salt();
                Ok(FreshnessObservation::Passed {
                    index_generation: Some(state.index_gen),
                    execution_digest_salt: Some(salt),
                })
            }
            "signature_scope" => {
                let (_, index_gen, _, live_digest) = self.observe_signature_scope_state(req)?;
                let salt = format!("index_generation={index_gen};live_files_digest={live_digest}");
                Ok(FreshnessObservation::Passed {
                    index_generation: Some(index_gen),
                    execution_digest_salt: Some(salt),
                })
            }
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code check 'code.{check}'"),
            )
            .into()),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "symbols" => self.provide_symbols(req),
            "tests" => self.provide_tests(req),
            "external_callers" => self.provide_external_callers(req),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unknown code context lens 'code.{lens}'"),
            )
            .into()),
        }
    }
}

struct DbSeamEdge {
    kind: String,
    dst_id: String,
    dst_path: String,
    dst_kind: String,
    dst_name: String,
    dst_qualname: String,
    dst_is_test: bool,
}

fn query_db_seam_edges(
    conn: &Connection,
    src_path: &str,
    src_qualname: Option<&str>,
) -> Result<Vec<DbSeamEdge>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.kind, dst.id, dp.path, dst.kind, dst.name, dst.qualname, dst.is_test
         FROM nodes src
         JOIN path_dictionary sp ON sp.path_id = src.path_id
         JOIN edges e ON e.src_hash = src.node_hash
         JOIN nodes dst ON dst.node_hash = e.dst_hash
         JOIN path_dictionary dp ON dp.path_id = dst.path_id
         WHERE sp.path = ?1 AND (?2 IS NULL OR src.qualname = ?2)
           AND e.kind IN ('calls', 'possible_calls', 'imports', 'implements', 'overrides', 'inherits', 'references', 'handles', 'mutates', 'decorates')
         ORDER BY e.kind, dp.path, dst.qualname",
    )?;
    let rows = stmt.query_map(params![src_path, src_qualname], |row| {
        Ok(DbSeamEdge {
            kind: row.get(0)?,
            dst_id: row.get(1)?,
            dst_path: row.get(2)?,
            dst_kind: row.get(3)?,
            dst_name: row.get(4)?,
            dst_qualname: row.get(5)?,
            dst_is_test: row.get(6)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

fn db_file_digest(conn: &Connection, rel_path: &str) -> Option<String> {
    conn.prepare_cached("SELECT digest FROM files WHERE path = ?1")
        .ok()?
        .query_row(params![rel_path], |row| row.get(0))
        .ok()
}

fn path_matches_code_db(
    worktree: &Path,
    conn: &Connection,
    rel_path: &str,
    file_hashes: &BTreeMap<String, String>,
    cache: &mut BTreeMap<String, bool>,
) -> bool {
    *cache.entry(rel_path.to_owned()).or_insert_with(|| {
        db_file_digest(conn, rel_path).is_some_and(|expected| {
            file_hashes.get(rel_path).is_some_and(|h| h == &expected)
                || (!file_hashes.contains_key(rel_path)
                    && checked_child(worktree, Path::new(rel_path))
                        .ok()
                        .and_then(|abs| fs::read_to_string(&abs).ok())
                        .is_some_and(|src| sha256_hex(src.as_bytes()) == expected))
        })
    })
}

fn hydrate_modified_db_targets(
    worktree: &Path,
    conn: &Connection,
    policy: &WorkspacePathPolicy,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) {
    let mut visited: BTreeSet<String> = live_facts
        .keys()
        .filter(|rel| file_hashes.get(*rel) != db_file_digest(conn, rel).as_ref())
        .cloned()
        .collect();
    let mut queue: VecDeque<String> = visited.iter().cloned().collect();
    let mut hydrated = 0usize;
    while let Some(src) = queue.pop_front() {
        for edge in query_db_seam_edges(conn, &src, None).unwrap_or_default() {
            if hydrated >= MAX_DB_HYDRATE_FILES || live_facts.len() >= MAX_SEAM_CONTEXT_FILES {
                return;
            }
            if let Some(norm) = normalize_rel_path(&edge.dst_path)
                .filter(|p| visited.insert(p.clone()) && policy.is_candidate_included(p))
            {
                let was_loaded = live_facts.contains_key(&norm);
                load_live_facts(worktree, &norm, live_facts, file_hashes);
                if !was_loaded && live_facts.contains_key(&norm) {
                    hydrated += 1;
                    queue.push_back(norm);
                }
            }
        }
    }
}

fn collect_scoped_worktree_files(
    worktree: &Path,
    scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Vec<String> {
    if scope.is_empty() || !worktree.exists() {
        return Vec::new();
    }
    let mut found = BTreeSet::new();
    let mut needs_walk = false;
    for entry in scope {
        for expanded in expand_glob_braces(entry) {
            if expanded.contains('*')
                || expanded.contains('?')
                || expanded.contains('[')
                || expanded.ends_with('/')
            {
                needs_walk = true;
                continue;
            }
            if let Some(norm) = normalize_rel_path(&expanded) {
                if let Ok(abs) = checked_child(worktree, Path::new(&norm)) {
                    if abs.is_dir() {
                        needs_walk = true;
                    } else if policy.is_candidate_included(&norm) && abs.is_file() {
                        found.insert(norm);
                    }
                }
            }
        }
    }
    if !needs_walk {
        return found.into_iter().collect();
    }
    let walker = ignore::WalkBuilder::new(worktree)
        .follow_links(false)
        .standard_filters(true)
        .hidden(false)
        .filter_entry(|e| {
            !matches!(e.file_name().to_str(), Some(".git" | ".worktrees"))
        })
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(worktree) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if policy.is_candidate_included(&rel_str) && policy.is_scope_admitted(&rel_str, scope) {
            found.insert(rel_str);
        }
    }
    found.into_iter().collect()
}

fn collect_changed_worktree_files(
    worktree: &Path,
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Vec<String> {
    if baseline.starts_with('-')
        || (!worktree.join(".git").exists() && acdd_core::git::git_common_dir(worktree).is_err())
    {
        return Vec::new();
    }
    let Ok(out) = git_command(
        worktree,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMRT",
            "--no-renames",
            "-z",
            baseline,
            "--",
        ],
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut changed = BTreeSet::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let rel = String::from_utf8_lossy(raw);
        if let Some(norm) = normalize_rel_path(&rel) {
            if policy.is_candidate_included(&norm)
                && checked_child(worktree, Path::new(&norm)).is_ok_and(|abs| abs.is_file())
            {
                changed.insert(norm);
            }
        }
    }
    changed.into_iter().collect()
}

fn collect_deleted_scoped_files(
    worktree: &Path,
    scope: &[String],
    policy: &WorkspacePathPolicy,
    baseline: &str,
) -> Vec<String> {
    if scope.is_empty()
        || baseline.starts_with('-')
        || (!worktree.join(".git").exists() && acdd_core::git::git_common_dir(worktree).is_err())
    {
        return Vec::new();
    }
    let Ok(out) = git_command(
        worktree,
        &[
            "diff",
            "--name-only",
            "--diff-filter=D",
            "--no-renames",
            "-z",
            baseline,
            "--",
        ],
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut deleted = BTreeSet::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let rel = String::from_utf8_lossy(raw);
        if let Some(norm) = normalize_rel_path(&rel) {
            if policy.is_candidate_included(&norm) && policy.is_scope_admitted(&norm, scope) {
                deleted.insert(norm);
            }
        }
    }
    deleted.into_iter().collect()
}

fn ancestor_module_candidates(rel_path: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut cur = Path::new(rel_path).parent();
    while let Some(dir) = cur {
        let dir_str = dir.to_string_lossy().replace('\\', "/");
        if dir_str.is_empty() || dir_str == "." {
            for root_file in [
                "lib.rs",
                "main.rs",
                "__init__.py",
                "index.ts",
                "index.tsx",
                "index.js",
                "index.jsx",
                "index.mts",
                "index.cts",
                "index.mjs",
                "index.cjs",
                "index.d.ts",
            ] {
                candidates.push(root_file.to_owned());
            }
            break;
        }
        candidates.push(format!("{dir_str}.rs"));
        for entry in [
            "mod.rs",
            "lib.rs",
            "main.rs",
            "__init__.py",
            "index.ts",
            "index.tsx",
            "index.js",
            "index.jsx",
            "index.mts",
            "index.cts",
            "index.mjs",
            "index.cjs",
            "index.d.ts",
        ] {
            candidates.push(format!("{dir_str}/{entry}"));
        }
        cur = dir.parent();
    }
    candidates
}

fn hydrate_seam_context_files(
    worktree: &Path,
    dependency_registry: &DependencyRegistry,
    policy: &WorkspacePathPolicy,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) {
    let try_load = |rel: &str,
                        live_facts: &mut BTreeMap<String, Facts>,
                        file_hashes: &mut BTreeMap<String, String>,
                        newly_added: &mut Vec<String>| {
        if live_facts.len() >= MAX_SEAM_CONTEXT_FILES || live_facts.contains_key(rel) {
            return false;
        }
        let Some(norm) = normalize_rel_path(rel) else {
            return false;
        };
        if live_facts.contains_key(&norm)
            || !policy.is_candidate_included(&norm)
            || languages::for_path(Path::new(&norm)).is_none()
        {
            return false;
        }
        if checked_child(worktree, Path::new(&norm)).is_ok_and(|abs| abs.is_file()) {
            load_live_facts(worktree, &norm, live_facts, file_hashes);
            if live_facts.contains_key(&norm) {
                newly_added.push(norm);
                return true;
            }
        }
        false
    };

    let initial: Vec<String> = live_facts.keys().cloned().collect();
    let mut frontier = initial.clone();
    for rel in &initial {
        for anc in ancestor_module_candidates(rel) {
            try_load(&anc, live_facts, file_hashes, &mut frontier);
        }
    }
    for entry in dependency_registry.workspace_entrypoint_candidates() {
        if try_load(&entry, live_facts, file_hashes, &mut frontier) {
            for anc in ancestor_module_candidates(&entry) {
                try_load(&anc, live_facts, file_hashes, &mut frontier);
            }
        }
    }

    for _ in 0..MAX_SEAM_HYDRATION_ROUNDS {
        if frontier.is_empty() || live_facts.len() >= MAX_SEAM_CONTEXT_FILES {
            break;
        }
        let mut next_frontier = Vec::new();
        for owner_path in frontier {
            let Some(facts) = live_facts.get(&owner_path) else {
                continue;
            };
            let import_refs: Vec<String> = facts
                .references
                .iter()
                .filter(|r| r.kind == "imports" || r.expression.contains("::"))
                .map(|r| r.module.as_deref().unwrap_or(&r.expression).to_owned())
                .collect();
            for raw_mod in import_refs {
                for cand in
                    dependency_registry.import_file_candidates(Some(worktree), &owner_path, &raw_mod)
                {
                    if try_load(&cand, live_facts, file_hashes, &mut next_frontier) {
                        for anc in ancestor_module_candidates(&cand) {
                            try_load(&anc, live_facts, file_hashes, &mut next_frontier);
                        }
                    }
                }
            }
        }
        frontier = next_frontier;
    }
}

fn load_live_facts(
    worktree: &Path,
    rel_path: &str,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) {
    if live_facts.contains_key(rel_path) {
        return;
    }
    let Some(lang) = languages::for_path(Path::new(rel_path)) else {
        return;
    };
    let Ok(abs) = checked_child(worktree, Path::new(rel_path)) else {
        return;
    };
    let Ok(source) = fs::read_to_string(&abs) else {
        return;
    };
    file_hashes.insert(
        rel_path.to_owned(),
        sha256_hex(source.as_bytes()),
    );
    if let Ok(typed) = extract_typed(rel_path, lang.id(), &source) {
        live_facts.insert(rel_path.to_owned(), typed.into_public());
    }
}

fn resolve_test_start_nodes(
    worktree: &Path,
    raw_target: &str,
    conn: Option<&Connection>,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) -> Result<Vec<Node>> {
    let (without_line, line) = match raw_target.split_once('#') {
        Some((prefix, suffix)) => {
            let line = suffix.strip_prefix('L').unwrap_or(suffix).parse::<usize>().ok();
            (prefix, line)
        }
        None => (raw_target, None),
    };
    let (candidate_path, symbol) = match without_line.split_once("::") {
        Some((p, s)) if !s.trim().is_empty() => (p.trim(), Some(s.trim())),
        _ => (without_line.trim(), None),
    };

    // Case 1: `candidate_path` names a confined file inside `worktree`.
    if let Some(norm) = normalize_rel_path(candidate_path) {
        if let Ok(abs) = checked_child(worktree, Path::new(&norm)) {
            if abs.is_file() {
                load_live_facts(worktree, &norm, live_facts, file_hashes);
                if let Some(facts) = live_facts.get(&norm) {
                    let nodes: Vec<Node> = facts
                        .nodes
                        .iter()
                        .filter(|n| {
                            matches!(n.kind.as_str(), "function" | "method")
                                && (n.is_test || symbol.is_some())
                                && line.is_none_or(|l| n.line == l)
                                && symbol.is_none_or(|s| node_matches_symbol(n, s))
                        })
                        .cloned()
                        .collect();
                    if !nodes.is_empty() {
                        return Ok(nodes);
                    }
                }
            }
        }
    }

    // Case 2: Bare test symbol name (e.g. `my_test` or `mod::my_test`) — match in live facts or code_db.
    let target_sym = symbol.unwrap_or(without_line);
    let mut matched = Vec::new();
    for facts in live_facts.values() {
        for n in &facts.nodes {
            if n.is_test
                && matches!(n.kind.as_str(), "function" | "method")
                && node_matches_symbol(n, target_sym)
            {
                matched.push(n.clone());
            }
        }
    }
    if !matched.is_empty() {
        return Ok(matched);
    }

    if let Some(conn) = conn {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.path FROM nodes n
             JOIN path_dictionary p ON p.path_id = n.path_id
             WHERE n.is_test = 1 AND (n.name = ?1 OR n.qualname = ?1 OR n.qualname LIKE ?2)
             ORDER BY p.path",
        )?;
        let dotted = format!("%{}", target_sym.replace("::", "."));
        let paths: Vec<String> = stmt
            .query_map(params![target_sym, dotted], |row| row.get::<_, String>(0))?
            .flatten()
            .filter(|p| checked_child(worktree, Path::new(p)).is_ok_and(|abs| abs.is_file()))
            .collect();
        for p in paths {
            load_live_facts(worktree, &p, live_facts, file_hashes);
            if let Some(facts) = live_facts.get(&p) {
                for n in &facts.nodes {
                    if n.is_test
                        && matches!(n.kind.as_str(), "function" | "method")
                        && node_matches_symbol(n, target_sym)
                    {
                        matched.push(n.clone());
                    }
                }
            }
        }
    }

    Ok(matched)
}

fn discover_fallback_test_starts(
    worktree: &Path,
    scoped_files: &[String],
    conn: Option<&Connection>,
    live_facts: &mut BTreeMap<String, Facts>,
    file_hashes: &mut BTreeMap<String, String>,
) -> Vec<Node> {
    let mut starts = Vec::new();
    for facts in live_facts.values() {
        for n in &facts.nodes {
            if n.is_test && matches!(n.kind.as_str(), "function" | "method") {
                starts.push(n.clone());
            }
        }
    }
    if !starts.is_empty() {
        return starts;
    }

    if let Some(conn) = conn {
        let page = QueryOptions {
            limit: 100,
            offset: 0,
            detail: Detail::Compact,
            generation: None,
        };
        for rel in scoped_files {
            if let Ok(res) = symbols::tests_paged_with_path(conn, rel, None, "inbound", &page) {
                if let Some(arr) = res.pointer("/nodes/items").and_then(Value::as_array) {
                    for item in arr {
                        if let (Some(p), Some(q)) =
                            (item["path"].as_str(), item["qualname"].as_str())
                        {
                            if !checked_child(worktree, Path::new(p)).is_ok_and(|abs| abs.is_file())
                            {
                                continue;
                            }
                            load_live_facts(worktree, p, live_facts, file_hashes);
                            if let Some(facts) = live_facts.get(p) {
                                for n in &facts.nodes {
                                    if n.is_test && n.qualname == q {
                                        starts.push(n.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    starts
}

fn test_starts_reference_scoped_files(
    test_starts: &[Node],
    scoped_code_files: &[String],
    live_facts: &BTreeMap<String, Facts>,
) -> bool {
    let mut scoped_tokens: BTreeSet<String> = BTreeSet::new();
    for rel in scoped_code_files {
        if let Some(stem) = Path::new(rel).file_stem().and_then(|s| s.to_str()) {
            if matches!(stem, "mod" | "lib" | "main" | "index" | "__init__") {
                if let Some(parent) = Path::new(rel)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                {
                    scoped_tokens.insert(parent.to_owned());
                }
            } else {
                scoped_tokens.insert(stem.to_owned());
            }
        }
        if let Some(facts) = live_facts.get(rel) {
            for node in &facts.nodes {
                if !node.is_test && node.kind != "module" && !node.name.is_empty() {
                    scoped_tokens.insert(node.name.clone());
                }
            }
        }
    }
    if scoped_tokens.is_empty() {
        return false;
    }
    for start in test_starts {
        if let Some(facts) = live_facts.get(&start.path) {
            for r in &facts.references {
                if scoped_tokens
                    .iter()
                    .any(|tok| contains_identifier(&r.expression, tok))
                {
                    return true;
                }
            }
        }
    }
    false
}

fn node_matches_symbol(node: &Node, raw_symbol: &str) -> bool {
    let normalized = raw_symbol.replace("::", ".");
    node.name == raw_symbol
        || node.qualname == normalized
        || node.qualname.ends_with(&format!(".{normalized}"))
}

fn contains_identifier(text: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    text.match_indices(ident).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + ident.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric() && c != '_')
            && after.is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

fn effective_baseline_ref(baseline_ref: Option<&str>) -> &str {
    baseline_ref
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .unwrap_or("HEAD")
}

fn normalize_sig(sig: &str) -> String {
    sig.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn public_callable_nodes(facts: &Facts) -> Vec<Node> {
    let exports: BTreeSet<String> = facts
        .nodes
        .iter()
        .filter(|n| n.kind == "module")
        .filter_map(|n| n.details["exports"].as_array())
        .flatten()
        .filter(|e| e["module"].is_null() && e["type_only"] != Value::Bool(true))
        .filter_map(|e| e["local"].as_str().map(str::to_owned))
        .collect();
    facts
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind.as_str(), "function" | "method")
                && !n.is_test
                && (n.details["is_public"].as_bool() == Some(true)
                    || n.details["visibility"].as_str() == Some("public")
                    || n.details["default_export"].as_bool() == Some(true)
                    || exports.contains(&n.name))
        })
        .cloned()
        .collect()
}

fn find_external_callers(
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
        if let Ok(out) = git_command(worktree, &["grep", "-z", "-l", "-F", "-e", &symbol.name, baseline, "--"]) {
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
                                        !is_test_src && contains_identifier(&r.expression, &symbol.name)
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

#[cfg(test)]
mod tests {
    use super::CodeDomainProvider;
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::ids::RunId;
    use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, DomainProvider};
    use std::fs;

    #[test]
    fn code_seam_checks_live_worktree_hops_and_signature_scope() {
        let dir = ScopedTempDir::create("contextunity-test-code-domain").unwrap();
        let root = dir.path();
        let run_git = |args: &[&str]| {
            let out = acdd_core::git::git_command(root, args).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"seam_demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod outside;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/engine.rs"),
            "pub fn target_entry(x: i32) -> i32 { x + 1 }\npub fn dead_fn() -> i32 { 0 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/outside.rs"),
            "use crate::engine::target_entry;\npub fn external_user() -> i32 { target_entry(2) }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_test.rs"),
            "use seam_demo::engine::target_entry;\n#[test]\nfn test_live_seam() {\n    let got = target_entry(1);\n    assert_eq!(got, 2);\n}\n",
        )
        .unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);
        // Mutate public signature of `target_entry` in working tree without touching `src/outside.rs`.
        fs::write(
            root.join("src/engine.rs"),
            "pub fn target_entry(x: i32, y: i32) -> i32 { x + y }\npub fn dead_fn() -> i32 { 0 }\n",
        )
        .unwrap();

        let provider = CodeDomainProvider::new(root.to_path_buf(), Adapter::default());
        let seam_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/engine.rs".into(), "tests/seam_test.rs".into()],
            changed_paths: vec![],
            test_target: Some("tests/seam_test.rs::test_live_seam".to_owned()),
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
        };
        let cases = [
            (
                "tests/seam_test.rs::test_live_seam",
                true,
                "live test reaching admitted_scope",
            ),
            (
                "tests/seam_test.rs::missing_test",
                false,
                "missing test target fails closed",
            ),
        ];
        for (target, expect_pass, label) in cases {
            let req = DomainCheckReq {
                test_target: Some(target.to_owned()),
                ..seam_req.clone()
            };
            let outcome = provider
                .run_check("seam", &serde_json::Value::Null, &req)
                .unwrap();
            match (&outcome, expect_pass) {
                (DomainCheckOutcome::Passed { execution_digest_salt, .. }, true) => {
                    let salt = execution_digest_salt.as_deref().unwrap_or_default();
                    assert!(salt.contains("index_generation="), "{salt}");
                    assert!(salt.contains("live_files_digest="), "{salt}");
                    let observed = provider
                        .observe_freshness("seam", &serde_json::Value::Null, &req)
                        .unwrap();
                    assert_eq!(
                        observed,
                        acdd_core::ports::FreshnessObservation::Passed {
                            index_generation: Some("no-index".to_owned()),
                            execution_digest_salt: Some(salt.to_owned()),
                        }
                    );
                }
                (DomainCheckOutcome::Failed { .. }, false) => {}
                _ => panic!("unexpected outcome for {label}: {outcome:?}"),
            }
        }

        // Verify pure-deletion, pure-test, pure-const, and docs-with---test scopes.
        fs::write(root.join("src/consts.rs"), "pub const MAX_ITEMS: usize = 8;\n").unwrap();
        fs::write(root.join("docs_note.md"), "# Note\n").unwrap();
        for (scope, changed, target, expect_pass, label) in [
            (
                vec!["src/deleted.rs".to_owned()],
                vec!["src/deleted.rs".to_owned()],
                None,
                true,
                "pure deletion scope",
            ),
            (
                vec!["src/never_created.rs".to_owned()],
                vec![],
                None,
                false,
                "nonexistent never-created code scope fails",
            ),
            (
                vec!["src/{never_a.rs,never_b.rs}".to_owned()],
                vec![],
                None,
                false,
                "nonexistent brace-glob code scope fails",
            ),
            (
                vec!["tests/seam_test.rs".to_owned()],
                vec![],
                Some("tests/seam_test.rs::test_live_seam"),
                true,
                "pure test scope",
            ),
            (
                vec!["src/consts.rs".to_owned(), "tests/seam_test.rs".to_owned()],
                vec![],
                Some("tests/seam_test.rs::test_live_seam"),
                true,
                "pure const module with overlapping test",
            ),
            (
                vec!["docs_note.md".to_owned()],
                vec![],
                Some("tests/seam_test.rs::missing_test"),
                true,
                "non-code scope with --test",
            ),
        ] {
            let req = DomainCheckReq {
                admitted_scope: scope,
                changed_paths: changed,
                test_target: target.map(str::to_owned),
                ..seam_req.clone()
            };
            let outcome = provider
                .run_check("seam", &serde_json::Value::Null, &req)
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                expect_pass,
                "{label}: {outcome:?}"
            );
        }

        // Verify signature_scope flags an external non-test caller when a public signature changes.
        let sig_fail = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &DomainCheckReq {
                    run_id: RunId::new("r1").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["src/engine.rs".into()],
                    changed_paths: vec!["src/engine.rs".into()],
                    test_target: None,
                    path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
                    baseline_ref: None,
                },
            )
            .unwrap();
        assert!(
            matches!(sig_fail, DomainCheckOutcome::Failed { .. }),
            "expected un-scoped external caller to fail: {sig_fail:?}"
        );

        let sig_pass_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/engine.rs".into(), "src/outside.rs".into()],
            changed_paths: vec!["src/engine.rs".into()],
            test_target: None,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
        };
        let sig_pass = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &sig_pass_req,
            )
            .unwrap();
        let sig_observed = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &sig_pass_req)
            .unwrap();
        match sig_pass {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => assert_eq!(
                sig_observed,
                acdd_core::ports::FreshnessObservation::Passed {
                    index_generation: Some("no-index".to_owned()),
                    execution_digest_salt,
                }
            ),
            other => panic!("expected scoped external caller to pass: {other:?}"),
        }

        // Build code-map.sqlite, then delete `src/outside.rs` without rebuilding:
        // stale `code_db` edges from the deleted file must be filtered out.
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        fs::remove_file(root.join("src/outside.rs")).unwrap();
        let sig_after_delete = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &DomainCheckReq {
                    admitted_scope: vec!["src/engine.rs".into()],
                    ..sig_pass_req
                },
            )
            .unwrap();
        assert!(
            matches!(sig_after_delete, DomainCheckOutcome::Passed { .. }),
            "deleted external caller in stale code_db must be skipped: {sig_after_delete:?}"
        );

        // #12: Qualified paths, imports, aliases, re-exports, and methods resolve via `acdd-index::linker`.
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod other;\npub mod outer;\npub mod seam_qualifiers;\npub mod seam_crate_prefix;\npub mod seam_nested_src;\npub use engine::target_entry as reexported_entry;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/engine.rs"),
            "pub struct Engine;\nimpl Engine { pub fn run_method(&self) -> i32 { 1 } }\npub fn target_entry(x: i32, y: i32) -> i32 { x + y }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/other.rs"),
            "pub fn target_entry(x: i32) -> i32 { x }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_qualifiers.rs"),
            "use crate::other;\nuse crate::engine::{Engine, target_entry as aliased_entry};\nuse crate::engine as eng_mod;\n#[test]\nfn bad_qualified() { other::target_entry(1); }\n#[test]\nfn bad_imported_shadow() { use crate::other::target_entry; target_entry(1); }\n#[test]\nfn good_qualified() { crate::engine::target_entry(1, 2); }\n#[test]\nfn good_crate_reexport() { crate::reexported_entry(1, 2); }\n#[test]\nfn good_aliased_fn() { aliased_entry(1, 2); }\n#[test]\nfn good_aliased_mod() { eng_mod::target_entry(1, 2); }\n#[test]\nfn good_method(e: Engine) { let _ = e.run_method(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_direct_crate.rs"),
            "#[test]\nfn direct_crate_fn() { seam_demo::engine::target_entry(1, 2); }\n#[test]\nfn direct_crate_reexport() { seam_demo::reexported_entry(1, 2); }\n",
        )
        .unwrap();
        for (target, expect_pass, label) in [
            ("src/seam_qualifiers.rs::bad_qualified", false, "unrelated :: qualifier fails"),
            ("src/seam_qualifiers.rs::bad_imported_shadow", false, "unrelated import fails"),
            ("src/seam_qualifiers.rs::good_qualified", true, "crate:: qualified path passes"),
            ("src/seam_qualifiers.rs::good_crate_reexport", true, "crate:: re-export path passes"),
            ("src/seam_qualifiers.rs::good_aliased_fn", true, "aliased function call passes"),
            ("src/seam_qualifiers.rs::good_aliased_mod", true, "aliased module call passes"),
            ("src/seam_qualifiers.rs::good_method", true, "instance obj.method() passes"),
            ("tests/seam_direct_crate.rs::direct_crate_fn", true, "direct crate::fn without use passes"),
            ("tests/seam_direct_crate.rs::direct_crate_reexport", true, "direct crate re-export without use passes"),
        ] {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: vec!["src/engine.rs".into()],
                        test_target: Some(target.to_owned()),
                        ..seam_req.clone()
                    },
                )
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                expect_pass,
                "{label}: {outcome:?}"
            );
        }

        // #8: `crate::service::build` MUST NOT match `src/outer/service.rs::build`, while `crate::outer::service::build` passes.
        fs::create_dir_all(root.join("src/outer/src")).unwrap();
        fs::write(
            root.join("src/outer/mod.rs"),
            "pub mod service;\npub mod src;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/outer/service.rs"),
            "pub fn build() -> i32 { 7 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_crate_prefix.rs"),
            "#[test]\nfn false_crate_root() { crate::service::build(); }\n#[test]\nfn exact_crate_outer() { crate::outer::service::build(); }\n",
        )
        .unwrap();
        for (target, expect_pass, label) in [
            ("src/seam_crate_prefix.rs::false_crate_root", false, "crate::service::build must not match src/outer/service.rs"),
            ("src/seam_crate_prefix.rs::exact_crate_outer", true, "crate::outer::service::build matches src/outer/service.rs"),
        ] {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: vec!["src/outer/service.rs".into()],
                        test_target: Some(target.to_owned()),
                        ..seam_req.clone()
                    },
                )
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                expect_pass,
                "{label}: {outcome:?}"
            );
        }

        // #4: Nested `src/` (`src/outer/src/service.rs`) must resolve from crate root `src/`, not inner `src/`.
        fs::write(
            root.join("src/outer/src/mod.rs"),
            "pub mod service;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/outer/src/service.rs"),
            "pub fn nested_build() -> i32 { 9 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/seam_nested_src.rs"),
            "#[test]\nfn bad_inner() { crate::service::nested_build(); }\n#[test]\nfn good_outer_src() { crate::outer::src::service::nested_build(); }\n",
        )
        .unwrap();
        for (target, expect_pass, label) in [
            ("src/seam_nested_src.rs::bad_inner", false, "crate::service::nested_build rejects src/outer/src/service.rs"),
            ("src/seam_nested_src.rs::good_outer_src", true, "crate::outer::src::service::nested_build matches src/outer/src/service.rs"),
        ] {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: vec!["src/outer/src/service.rs".into()],
                        test_target: Some(target.to_owned()),
                        ..seam_req.clone()
                    },
                )
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                expect_pass,
                "{label}: {outcome:?}"
            );
        }

        // Python & TypeScript barrel hydration (`__init__.py` & `index.ts`) resolves automatically.
        fs::create_dir_all(root.join("py_pkg")).unwrap();
        fs::create_dir_all(root.join("ts_pkg")).unwrap();
        fs::write(root.join("py_pkg/__init__.py"), "from .worker import py_target\n").unwrap();
        fs::write(root.join("py_pkg/worker.py"), "def py_target():\n    return 1\n").unwrap();
        fs::write(
            root.join("tests/test_py_seam.py"),
            "from py_pkg import py_target\ndef test_py_barrel():\n    return py_target()\n",
        )
        .unwrap();
        fs::write(root.join("ts_pkg/index.ts"), "export { tsTarget } from './worker';\n").unwrap();
        fs::write(root.join("ts_pkg/worker.ts"), "export function tsTarget(): number { return 1; }\n").unwrap();
        fs::write(
            root.join("tests/ts_seam.test.ts"),
            "import { tsTarget } from '../ts_pkg';\nfunction test_ts_barrel() { return tsTarget(); }\n",
        )
        .unwrap();
        for (scope, target, label) in [
            ("py_pkg/worker.py", "tests/test_py_seam.py::test_py_barrel", "Python __init__.py barrel"),
            ("ts_pkg/worker.ts", "tests/ts_seam.test.ts::test_ts_barrel", "TypeScript index.ts barrel"),
        ] {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: vec![scope.to_owned()],
                        test_target: Some(target.to_owned()),
                        ..seam_req.clone()
                    },
                )
                .unwrap();
            assert!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                "{label} failed: {outcome:?}"
            );
        }

        // #28: Deleted tracked file in `admitted_scope` is included in signature_scope digest, check, and external_callers.
        fs::write(
            root.join("src/deleted_mod.rs"),
            "pub fn doomed_api() -> i32 { 7 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/caller_of_deleted.rs"),
            "use crate::deleted_mod::doomed_api;\npub fn still_calls() -> i32 { doomed_api() }\n",
        )
        .unwrap();
        run_git(&["add", "src/deleted_mod.rs", "src/caller_of_deleted.rs"]);
        run_git(&["commit", "-m", "add deleted_mod and caller"]);
        let _ = fs::remove_file(root.join(".acdd/code-map.sqlite"));
        let del_req = DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: "t1.build".parse().unwrap(),
            gate_id: "build".parse().unwrap(),
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            admitted_scope: vec!["src/deleted_mod.rs".into()],
            changed_paths: vec!["src/deleted_mod.rs".into()],
            test_target: None,
            path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
            baseline_ref: None,
        };
        let obs_before_del = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        fs::remove_file(root.join("src/deleted_mod.rs")).unwrap();
        let obs_after_del = provider
            .observe_freshness("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        assert_ne!(
            obs_before_del, obs_after_del,
            "live_digest must change when tracked scoped file is deleted"
        );
        let del_sig_outcome = provider
            .run_check("signature_scope", &serde_json::Value::Null, &del_req)
            .unwrap();
        assert!(
            matches!(del_sig_outcome, DomainCheckOutcome::Failed { .. }),
            "deleting tracked scoped file with un-scoped external caller must fail: {del_sig_outcome:?}"
        );
        let ext_callers = provider
            .provide_context(
                "external_callers",
                &acdd_core::ports::DomainContextReq {
                    run_id: RunId::new("r1").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["src/deleted_mod.rs".into()],
                    target: None,
                    max_bytes: 32768,
                    path_policy: acdd_core::path_policy::WorkspacePathPolicy::default(),
                    baseline_ref: None,
                },
            )
            .unwrap();
        assert!(
            ext_callers["items"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|item| {
                    item["path"] == "src/deleted_mod.rs"
                        && item["signature_changed"] == true
                        && item["external_callers"]
                            .as_array()
                            .is_some_and(|c| c.iter().any(|p| p == "src/caller_of_deleted.rs"))
                })),
            "external_callers must report deleted file external callers: {ext_callers}"
        );

        // #11: Signature check with `baseline_ref` catches committed deletions/edits since claim even after commit advances HEAD.
        run_git(&["checkout", "--", "src/deleted_mod.rs"]);
        let baseline_out = super::git_command(root, &["rev-parse", "HEAD"]).unwrap();
        let claim_baseline = String::from_utf8_lossy(&baseline_out.stdout).trim().to_owned();
        fs::remove_file(root.join("src/deleted_mod.rs")).unwrap();
        run_git(&["add", "-u", "src/deleted_mod.rs"]);
        run_git(&["commit", "-m", "commit deletion after claim"]);
        let committed_del_req = DomainCheckReq {
            baseline_ref: Some(claim_baseline),
            ..del_req.clone()
        };
        let committed_del_outcome = provider
            .run_check("signature_scope", &serde_json::Value::Null, &committed_del_req)
            .unwrap();
        assert!(
            matches!(committed_del_outcome, DomainCheckOutcome::Failed { .. }),
            "baseline_ref must detect committed deletion since claim: {committed_del_outcome:?}"
        );

        // #6: Even if live `code-map.sqlite` is refreshed WITHOUT deleted `src/deleted_mod.rs`,
        // `run_signature_scope_check` still consults `baseline_ref` callers and fails!
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        let refreshed_del_outcome = provider
            .run_check("signature_scope", &serde_json::Value::Null, &committed_del_req)
            .unwrap();
        assert!(
            matches!(refreshed_del_outcome, DomainCheckOutcome::Failed { .. }),
            "refreshed code-map.sqlite missing deleted target must still fail via baseline_ref: {refreshed_del_outcome:?}"
        );

        // Stale `code_db` edge validation against `src_live` references/imports.
        fs::write(
            root.join("src/lib.rs"),
            "pub mod engine;\npub mod other;\npub mod outer;\npub mod hop1;\npub mod hop2;\npub mod hop3;\npub mod hop4;\npub mod wrong_hop4;\npub mod seam_qualifiers;\npub mod seam_crate_prefix;\npub mod seam_nested_src;\npub use engine::target_entry as reexported_entry;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop4.rs"),
            "pub fn step4() -> i32 { crate::engine::target_entry(1, 2) }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/wrong_hop4.rs"),
            "pub fn step4() -> i32 { 0 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop3.rs"),
            "pub fn step3() -> i32 { crate::hop4::step4() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop2.rs"),
            "pub fn step2() -> i32 { crate::hop3::step3() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop1.rs"),
            "pub fn step1() -> i32 { crate::hop2::step2() }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/seam_stale.rs"),
            "#[test]\nfn direct_stale() { seam_demo::engine::target_entry(1, 2); }\n#[test]\nfn import_stale() { use seam_demo::engine::target_entry; target_entry(1, 2); }\n#[test]\nfn alias_stale() { use seam_demo::engine as eng; eng::target_entry(1, 2); }\n#[test]\nfn deep_chain() { seam_demo::hop1::step1(); }\n",
        )
        .unwrap();
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();

        // Mutate live files WITHOUT rebuilding `code-map.sqlite`:
        // `wrong::target_entry` and `wrong_hop4::step4` share bare identifier names with stale edges.
        fs::write(
            root.join("tests/seam_stale.rs"),
            "#[test]\nfn direct_stale() { seam_demo::other::target_entry(1); }\n#[test]\nfn import_stale() { use seam_demo::other::target_entry; target_entry(1); }\n#[test]\nfn alias_stale() { use seam_demo::other as eng; eng::target_entry(1); }\n#[test]\nfn deep_chain() { seam_demo::hop1::step1(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/hop3.rs"),
            "pub fn step3() -> i32 { crate::wrong_hop4::step4() }\n",
        )
        .unwrap();
        for (target, changed, expect_pass, label) in [
            ("tests/seam_stale.rs::direct_stale", vec![], false, "stale qualified code_db edge rejected after edit to other::target_entry"),
            ("tests/seam_stale.rs::import_stale", vec![], false, "stale bare call rejected after import changed to other::target_entry"),
            ("tests/seam_stale.rs::alias_stale", vec![], false, "stale aliased module call rejected after alias changed to other"),
            ("tests/seam_stale.rs::deep_chain", vec!["src/hop3.rs".to_owned()], false, "stale mid-chain code_db edge rejected after hop3 edited to wrong_hop4::step4"),
        ] {
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        admitted_scope: vec!["src/engine.rs".into()],
                        changed_paths: changed,
                        test_target: Some(target.to_owned()),
                        ..seam_req.clone()
                    },
                )
                .unwrap();
            assert_eq!(
                matches!(outcome, DomainCheckOutcome::Passed { .. }),
                expect_pass,
                "{label}: {outcome:?}"
            );
        }

        // Restoring `src/hop3.rs` via an aliased import to `hop4` still traverses unhydrated `hop4 -> engine` in `code_db`.
        fs::write(
            root.join("src/hop3.rs"),
            "use crate::hop4 as h4;\npub fn step3() -> i32 { h4::step4() }\n",
        )
        .unwrap();
        let valid_chain = provider
            .run_check(
                "seam",
                &serde_json::Value::Null,
                &DomainCheckReq {
                    admitted_scope: vec!["src/engine.rs".into()],
                    changed_paths: vec!["src/hop3.rs".to_owned()],
                    test_target: Some("tests/seam_stale.rs::deep_chain".to_owned()),
                    ..seam_req.clone()
                },
            )
            .unwrap();
        assert!(
            matches!(valid_chain, DomainCheckOutcome::Passed { .. }),
            "valid cross-file hop via code_db must still pass: {valid_chain:?}"
        );

        // Rebuild code_db, then test untouched chain, line-shifted live edit, and deleted target symbol.
        crate::engine::pipeline::build(root, &root.join(".acdd/code-map.sqlite"), None).unwrap();
        let deep_req = DomainCheckReq {
            admitted_scope: vec!["src/engine.rs".into()],
            changed_paths: vec![],
            test_target: Some("tests/seam_stale.rs::deep_chain".to_owned()),
            ..seam_req.clone()
        };
        assert!(matches!(
            provider.run_check("seam", &serde_json::Value::Null, &deep_req).unwrap(),
            DomainCheckOutcome::Passed { .. }
        ));
        fs::write(
            root.join("src/hop3.rs"),
            "// shift 1\n// shift 2\nuse crate::hop4 as h4;\npub fn step3() -> i32 { h4::step4() }\n",
        )
        .unwrap();
        assert!(matches!(
            provider.run_check("seam", &serde_json::Value::Null, &deep_req).unwrap(),
            DomainCheckOutcome::Passed { .. }
        ));
        fs::write(
            root.join("src/engine.rs"),
            "pub struct Engine;\nimpl Engine { pub fn run_method(&self) -> i32 { 1 } }\npub fn renamed_entry(x: i32, y: i32) -> i32 { x + y }\n",
        )
        .unwrap();
        assert!(matches!(
            provider.run_check("seam", &serde_json::Value::Null, &deep_req).unwrap(),
            DomainCheckOutcome::Failed { .. }
        ));
    }
}
