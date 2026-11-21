use super::super::open_code_db_readonly;
use super::{
    collect_scoped_worktree_files, contains_identifier, is_callable_kind, load_live_facts,
    node_matches_symbol, CodeDomainProvider,
};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::path_policy::{expand_glob_braces, normalize_rel_path, WorkspacePathPolicy};
use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq};
use acdd_core::response::{Detail, QueryOptions};
use acdd_index::db::symbols;
use acdd_index::languages::{self, dependency_registry::DependencyRegistry};
use acdd_index::linker;
use acdd_index::models::tags::{EdgeKind, NodeKind};
use acdd_index::models::{Facts, Node};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

const MAX_SEAM_HOPS: usize = 8;
const MAX_SEAM_CONTEXT_FILES: usize = 128;
const MAX_SEAM_HYDRATION_ROUNDS: usize = 3;
const MAX_DB_HYDRATE_FILES: usize = 64;
const SEAM_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::PossibleCalls,
    EdgeKind::Imports,
    EdgeKind::Implements,
    EdgeKind::Overrides,
    EdgeKind::Inherits,
    EdgeKind::References,
    EdgeKind::Handles,
    EdgeKind::Mutates,
    EdgeKind::Decorates,
];

pub(super) struct PreparedSeamState {
    pub conn: Option<Connection>,
    pub index_gen: String,
    pub dependency_registry: DependencyRegistry,
    pub scoped_files: Vec<String>,
    pub scoped_code_files: Vec<String>,
    pub has_code_scope: bool,
    pub live_facts: BTreeMap<String, Facts>,
    pub file_hashes: BTreeMap<String, String>,
    pub test_starts: Vec<Node>,
    pub live_digest: String,
}

impl PreparedSeamState {
    pub(super) fn salt(&self) -> String {
        format!(
            "index_generation={};live_files_digest={}",
            self.index_gen, self.live_digest
        )
    }
}

fn passed_seam_hop(
    dst_path: &str,
    dst_qualname: &str,
    depth: usize,
    trace: &[String],
    salt: &str,
) -> DomainCheckOutcome {
    let mut final_trace = trace.to_vec();
    final_trace.push(format!("{dst_path}::{dst_qualname}"));
    DomainCheckOutcome::Passed {
        summary: format!(
            "code.seam reached {dst_path}::{dst_qualname} in {} hop(s): {}",
            depth + 1,
            final_trace.join(" -> ")
        ),
        execution_digest_salt: Some(salt.to_owned()),
    }
}

impl CodeDomainProvider {
    pub(super) fn prepare_seam_state(&self, req: &DomainCheckReq) -> Result<PreparedSeamState> {
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

        let live_digest = super::super::combine_file_hashes(&file_hashes);
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

    pub(super) fn run_seam_check(
        &self,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
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
                    facts
                        .nodes
                        .iter()
                        .any(|n| !n.is_test && node_matches_symbol(n, sym))
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
            live_facts
                .get(path)
                .is_some_and(|facts| facts.nodes.iter().any(|n| !n.is_test && is_callable_kind(&n.kind)))
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
                .filter(|e| e.src == cur_id && SEAM_EDGE_KINDS.iter().any(|k| k.as_str() == e.kind))
            {
                let Some(dst) = live_nodes_by_id.get(&edge.dst) else {
                    continue;
                };
                let edge_desc = format!(
                    "{}::{} -{}-> {}::{}",
                    cur_key.path, cur_key.qualname, edge.kind, dst.path, dst.qualname
                );
                discovered_edges.insert(edge_desc);

                let bare_module_import = edge.kind == EdgeKind::Imports.as_str()
                    && dst.kind == NodeKind::Module.as_str()
                    && target_symbol.is_none();
                if !dst.is_test
                    && !bare_module_import
                    && policy.is_scope_admitted(&dst.path, &req.admitted_scope)
                    && target_symbol
                        .as_deref()
                        .is_none_or(|sym| node_matches_symbol(dst, sym))
                {
                    return Ok(passed_seam_hop(
                        &dst.path,
                        &dst.qualname,
                        depth,
                        &trace,
                        &salt,
                    ));
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
                    for db_edge in
                        query_db_seam_edges(conn, &cur_key.path, Some(&cur_key.qualname))?
                    {
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

                        let bare_module_import = db_edge.kind == EdgeKind::Imports.as_str()
                            && db_edge.dst_kind == NodeKind::Module.as_str()
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
                            return Ok(passed_seam_hop(
                                &db_edge.dst_path,
                                &db_edge.dst_qualname,
                                depth,
                                &trace,
                                &salt,
                            ));
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
                    .take(super::super::CODE_SEAM_DIAGNOSTIC_EDGE_LIMIT)
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
                .filter(|r| {
                    r.kind == EdgeKind::Imports.as_str() || r.expression.contains("::")
                })
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
                            is_callable_kind(&n.kind)
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
            if n.is_test && is_callable_kind(&n.kind) && node_matches_symbol(n, target_sym) {
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
                    if n.is_test && is_callable_kind(&n.kind) && node_matches_symbol(n, target_sym) {
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
            if n.is_test && is_callable_kind(&n.kind) {
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
                if !node.is_test && node.kind != NodeKind::Module.as_str() && !node.name.is_empty()
                {
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
