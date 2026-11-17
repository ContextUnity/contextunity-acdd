//! Host `CodeDomainProvider` (`domain_id = "code"`) implementation.

use super::{budget_items, combine_file_hashes, open_code_db_readonly};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::git::git_command;
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider,
};
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
use acdd_core::schema::SchemaDef;
use acdd_index::ast::extract_typed;
use acdd_index::db::{paging, symbols};
use acdd_index::languages;
use acdd_index::linker;
use acdd_index::models::{Facts, Node};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_SEAM_HOPS: usize = 8;

/// Host `code` domain provider exposing `code.seam`, `code.signature_scope`, and `code.*` context lenses.
pub struct CodeDomainProvider {
    root: PathBuf,
}

impl CodeDomainProvider {
    /// Construct a `CodeDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, _config: Adapter) -> Self {
        Self { root }
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

    fn run_seam_check(&self, params: &Value, req: &DomainCheckReq) -> Result<DomainCheckOutcome> {
        let target_symbol = params
            .get("target_symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let policy = WorkspacePathPolicy::default();
        let conn = open_code_db_readonly(&self.root, &req.worktree);
        let index_gen = Self::index_generation(conn.as_ref());

        // 1. Collect live code files for admitted_scope and changed_paths.
        let mut live_facts: BTreeMap<String, Facts> = BTreeMap::new();
        let mut file_hashes: BTreeMap<String, String> = BTreeMap::new();

        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
        for rel in &scoped_files {
            load_live_facts(&req.worktree, rel, &mut live_facts, &mut file_hashes);
        }
        for rel in &req.changed_paths {
            if let Some(norm) = normalize_rel_path(rel) {
                load_live_facts(&req.worktree, &norm, &mut live_facts, &mut file_hashes);
            }
        }

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

        // 2. Resolve test start nodes from `req.test_target` (or covering tests if omitted).
        let test_starts = match req.test_target.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            Some(raw_target) => {
                resolve_test_start_nodes(
                    &req.worktree,
                    raw_target,
                    conn.as_ref(),
                    &mut live_facts,
                    &mut file_hashes,
                )?
            }
            None => {
                // If admitted_scope contains no code files (e.g. docs-only task under `code` profile),
                // check if any changed code files exist; if none, pass cleanly.
                let has_code_scope = scoped_files
                    .iter()
                    .any(|p| languages::for_path(Path::new(p)).is_some())
                    || req
                        .admitted_scope
                        .iter()
                        .any(|s| languages::for_path(Path::new(s)).is_some());
                if !has_code_scope {
                    let live_digest = combine_file_hashes(&file_hashes);
                    return Ok(DomainCheckOutcome::Passed {
                        summary: "code.seam: no code language targets in admitted_scope".to_owned(),
                        execution_digest_salt: Some(format!(
                            "index_generation={index_gen};live_files_digest={live_digest}"
                        )),
                    });
                }
                discover_fallback_test_starts(
                    &req.worktree,
                    &scoped_files,
                    conn.as_ref(),
                    &mut live_facts,
                    &mut file_hashes,
                )
            }
        };

        if test_starts.is_empty() {
            let desc = req.test_target.as_deref().unwrap_or("<none>");
            return Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "code.seam could not resolve live test symbol(s) for test target '{desc}'"
                ),
                diagnostics: vec![format!("admitted_scope: {:?}", req.admitted_scope)],
            });
        }

        let live_digest = combine_file_hashes(&file_hashes);
        let salt = format!("index_generation={index_gen};live_files_digest={live_digest}");

        // 3. Link all live-parsed files together so new/modified seams in the worktree resolve immediately.
        let live_graph = linker::link_with_root(&live_facts, None, Some(&req.worktree));
        let live_nodes: Vec<&Node> = live_facts.values().flat_map(|f| f.nodes.iter()).collect();
        let live_nodes_by_id: BTreeMap<String, &Node> =
            live_nodes.iter().map(|n| (n.id.clone(), *n)).collect();

        // Index live non-test nodes inside admitted_scope for direct reference matching.
        let mut scoped_targets: Vec<&Node> = Vec::new();
        for &node in &live_nodes {
            if !node.is_test
                && matches!(node.kind.as_str(), "function" | "method" | "struct" | "class" | "trait" | "interface" | "enum")
                && policy.is_scope_admitted(&node.path, &req.admitted_scope)
                && target_symbol
                    .as_deref()
                    .is_none_or(|sym| node_matches_symbol(node, sym))
            {
                scoped_targets.push(node);
            }
        }

        // 4. BFS up to MAX_SEAM_HOPS (8) from test start nodes over live graph + live references + read-only code_db.
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
            for edge in live_graph.edges.iter().filter(|e| {
                e.src == cur_id
                    && matches!(
                        e.kind.as_str(),
                        "calls" | "possible_calls" | "imports" | "implements" | "overrides"
                    )
            }) {
                let Some(dst) = live_nodes_by_id.get(&edge.dst) else {
                    continue;
                };
                let edge_desc = format!(
                    "{}::{} -{}-> {}::{}",
                    cur_key.path, cur_key.qualname, edge.kind, dst.path, dst.qualname
                );
                discovered_edges.insert(edge_desc);

                if !dst.is_test
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

            // (b) Match live AST references from current node directly against live admitted_scope targets
            //     (handles newly added cross-file/cross-crate calls before Cargo metadata or code-map rebuild).
            if let Some(cur_facts) = live_facts.get(&cur_key.path) {
                let mut cur_node_ids: BTreeSet<&str> = BTreeSet::new();
                for n in &cur_facts.nodes {
                    if n.qualname == cur_key.qualname {
                        cur_node_ids.insert(n.id.as_str());
                    }
                }
                let file_imports: Vec<&str> = cur_facts
                    .references
                    .iter()
                    .filter(|r| r.kind == "imports")
                    .map(|r| r.expression.as_str())
                    .collect();

                for reference in cur_facts.references.iter().filter(|r| {
                    cur_node_ids.contains(r.source.as_str())
                        && matches!(
                            r.kind.as_str(),
                            "calls" | "possible_calls" | "imports" | "unresolved"
                        )
                }) {
                    for target in &scoped_targets {
                        if reference_connects_to_target(
                            &cur_key.path,
                            reference.expression.as_str(),
                            reference.alias.as_deref(),
                            &file_imports,
                            target,
                        ) {
                            let mut final_trace = trace.clone();
                            final_trace.push(format!("{}::{}", target.path, target.qualname));
                            return Ok(DomainCheckOutcome::Passed {
                                summary: format!(
                                    "code.seam reached {}::{} in {} hop(s): {}",
                                    target.path,
                                    target.qualname,
                                    depth + 1,
                                    final_trace.join(" -> ")
                                ),
                                execution_digest_salt: Some(salt),
                            });
                        }
                    }

                    // Also follow live references to any other live-parsed node (e.g. helper in changed file).
                    for &candidate in &live_nodes {
                        if matches!(candidate.kind.as_str(), "function" | "method")
                            && reference_connects_to_target(
                                &cur_key.path,
                                reference.expression.as_str(),
                                reference.alias.as_deref(),
                                &file_imports,
                                candidate,
                            )
                        {
                            let next_key = BfsKey {
                                path: candidate.path.clone(),
                                qualname: candidate.qualname.clone(),
                            };
                            if visited.insert(next_key.clone()) {
                                let mut next_trace = trace.clone();
                                next_trace
                                    .push(format!("{}::{}", candidate.path, candidate.qualname));
                                queue.push_back(BfsItem {
                                    key: next_key,
                                    node_id: candidate.id.clone(),
                                    depth: depth + 1,
                                    trace: next_trace,
                                });
                            }
                        }
                    }
                }
            }

            // (c) Traverse read-only `code_db` edges for untouched files in the workspace.
            if let Some(ref conn) = conn {
                for db_edge in query_db_seam_edges(conn, &cur_key.path, &cur_key.qualname)? {
                    // Validate against live files if either endpoint was modified/loaded live.
                    if let Some(src_live) = live_facts.get(&cur_key.path) {
                        let still_referenced = src_live.references.iter().any(|r| {
                            contains_identifier(&r.expression, &db_edge.dst_name)
                                || r.alias.as_deref() == Some(db_edge.dst_name.as_str())
                        });
                        if !still_referenced {
                            continue;
                        }
                    }
                    if let Some(dst_live) = live_facts.get(&db_edge.dst_path) {
                        let still_exists = dst_live
                            .nodes
                            .iter()
                            .any(|n| n.qualname == db_edge.dst_qualname);
                        if !still_exists {
                            continue;
                        }
                    }

                    discovered_edges.insert(format!(
                        "{}::{} -{}-> {}::{}",
                        cur_key.path,
                        cur_key.qualname,
                        db_edge.kind,
                        db_edge.dst_path,
                        db_edge.dst_qualname
                    ));

                    if !db_edge.dst_is_test
                        && policy.is_scope_admitted(&db_edge.dst_path, &req.admitted_scope)
                        && target_symbol.as_deref().is_none_or(|sym| {
                            db_edge.dst_name == sym
                                || db_edge.dst_qualname == sym
                                || db_edge.dst_qualname.replace('.', "::") == sym
                        })
                    {
                        let mut final_trace = trace.clone();
                        final_trace.push(format!("{}::{}", db_edge.dst_path, db_edge.dst_qualname));
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
                        next_trace.push(format!("{}::{}", db_edge.dst_path, db_edge.dst_qualname));
                        queue.push_back(BfsItem {
                            key: next_key,
                            node_id: db_edge.dst_id,
                            depth: depth + 1,
                            trace: next_trace,
                        });
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
        let policy = WorkspacePathPolicy::default();
        let conn = open_code_db_readonly(&self.root, &req.worktree);
        let index_gen = Self::index_generation(conn.as_ref());
        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
        let mut file_hashes: BTreeMap<String, String> = BTreeMap::new();
        let mut violations: Vec<String> = Vec::new();

        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Ok(abs) = checked_child(&req.worktree, Path::new(rel)) else {
                continue;
            };
            let Ok(current_src) = fs::read_to_string(&abs) else {
                continue;
            };
            file_hashes.insert(
                rel.clone(),
                sha256_hex(current_src.as_bytes()),
            );
            let Some(head_src) = git_show_head(&req.worktree, rel) else {
                continue;
            };

            let head_facts = extract_typed(rel, lang.id(), &head_src)?.into_public();
            let cur_facts = extract_typed(rel, lang.id(), &current_src)?.into_public();

            let mut cur_sigs: BTreeMap<(String, String), String> = BTreeMap::new();
            for node in public_callable_nodes(&cur_facts) {
                let sig = normalize_sig(node.details["signature"].as_str().unwrap_or_default());
                cur_sigs.insert((node.kind.clone(), node.qualname.clone()), sig);
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
                    &policy,
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

        let live_digest = combine_file_hashes(&file_hashes);
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

    fn provide_symbols(&self, req: &DomainContextReq) -> Result<Value> {
        let policy = WorkspacePathPolicy::default();
        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
        let mut items = Vec::new();
        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Ok(abs) = checked_child(&req.worktree, Path::new(rel)) else {
                continue;
            };
            let Ok(source) = fs::read_to_string(&abs) else {
                continue;
            };
            let Ok(typed) = extract_typed(rel, lang.id(), &source) else {
                continue;
            };
            for node in typed.facts.nodes {
                if node.kind == "module" {
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
        let policy = WorkspacePathPolicy::default();
        let conn = open_code_db_readonly(&self.root, &req.worktree);
        let mut items = Vec::new();
        let mut seen = BTreeSet::new();

        if let Some(ref conn) = conn {
            let page = QueryOptions::resolve(&ResponsePolicy::default(), Some(25), 0, None, None)?;
            let scoped_files =
                collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
            for rel in scoped_files.iter().take(10) {
                if let Ok(res) = symbols::tests_paged_with_path(conn, rel, None, "inbound", &page) {
                    if let Some(arr) = res.pointer("/nodes/items").and_then(Value::as_array) {
                        for item in arr {
                            let path = item["path"].as_str().unwrap_or_default().to_owned();
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
        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Ok(abs) = checked_child(&req.worktree, Path::new(rel)) else {
                continue;
            };
            let Ok(source) = fs::read_to_string(&abs) else {
                continue;
            };
            if let Ok(typed) = extract_typed(rel, lang.id(), &source) {
                for node in typed.facts.nodes {
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
        }

        Ok(budget_items("tests", items, req.max_bytes))
    }

    fn provide_external_callers(&self, req: &DomainContextReq) -> Result<Value> {
        let policy = WorkspacePathPolicy::default();
        let conn = open_code_db_readonly(&self.root, &req.worktree);
        let scoped_files = collect_scoped_worktree_files(&req.worktree, &req.admitted_scope, &policy);
        let mut items = Vec::new();

        for rel in &scoped_files {
            let Some(lang) = languages::for_path(Path::new(rel)) else {
                continue;
            };
            let Ok(abs) = checked_child(&req.worktree, Path::new(rel)) else {
                continue;
            };
            let Ok(current_src) = fs::read_to_string(&abs) else {
                continue;
            };
            let cur_facts = extract_typed(rel, lang.id(), &current_src)?.into_public();
            let head_sigs = git_show_head(&req.worktree, rel)
                .and_then(|head_src| extract_typed(rel, lang.id(), &head_src).ok())
                .map(|typed| {
                    public_callable_nodes(&typed.into_public())
                        .into_iter()
                        .map(|n| {
                            (
                                (n.kind, n.qualname),
                                normalize_sig(n.details["signature"].as_str().unwrap_or_default()),
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();

            for node in public_callable_nodes(&cur_facts) {
                let cur_sig = normalize_sig(node.details["signature"].as_str().unwrap_or_default());
                let signature_changed = head_sigs
                    .get(&(node.kind.clone(), node.qualname.clone()))
                    .is_some_and(|head_sig| head_sig != &cur_sig);

                let callers = find_external_callers(
                    &req.worktree,
                    conn.as_ref(),
                    &node,
                    &req.admitted_scope,
                    &policy,
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
        Err(StateError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("code domain does not expose action '{action}'"),
        )
        .into())
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
    dst_name: String,
    dst_qualname: String,
    dst_is_test: bool,
}

fn query_db_seam_edges(
    conn: &Connection,
    src_path: &str,
    src_qualname: &str,
) -> Result<Vec<DbSeamEdge>> {
    let mut stmt = conn.prepare(
        "SELECT e.kind, dst.id, dp.path, dst.name, dst.qualname, dst.is_test
         FROM nodes src
         JOIN path_dictionary sp ON sp.path_id = src.path_id
         JOIN edges e ON e.src_hash = src.node_hash
         JOIN nodes dst ON dst.node_hash = e.dst_hash
         JOIN path_dictionary dp ON dp.path_id = dst.path_id
         WHERE sp.path = ?1 AND src.qualname = ?2
           AND e.kind IN ('calls', 'possible_calls', 'imports', 'implements', 'overrides')
         ORDER BY e.kind, dp.path, dst.qualname",
    )?;
    let rows = stmt.query_map(params![src_path, src_qualname], |row| {
        Ok(DbSeamEdge {
            kind: row.get(0)?,
            dst_id: row.get(1)?,
            dst_path: row.get(2)?,
            dst_name: row.get(3)?,
            dst_qualname: row.get(4)?,
            dst_is_test: row.get(5)?,
        })
    })?;
    Ok(rows.flatten().collect())
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
    for entry in scope {
        if !entry.contains('*') && !entry.contains('?') {
            if let Some(norm) = normalize_rel_path(entry) {
                if let Ok(abs) = checked_child(worktree, Path::new(&norm)) {
                    if abs.is_file() {
                        found.insert(norm);
                    }
                }
            }
        }
    }
    let walker = ignore::WalkBuilder::new(worktree)
        .follow_links(false)
        .standard_filters(true)
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(".git" | ".worktrees" | ".acdd" | "target" | "node_modules")
            )
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
        if policy.is_scope_admitted(&rel_str, scope) {
            found.insert(rel_str);
        }
    }
    found.into_iter().collect()
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
             ORDER BY p.path
             LIMIT 10",
        )?;
        let dotted = format!("%{}", target_sym.replace("::", "."));
        let paths: Vec<String> = stmt
            .query_map(params![target_sym, dotted], |row| row.get::<_, String>(0))?
            .flatten()
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
            limit: 20,
            offset: 0,
            detail: Detail::Compact,
            generation: None,
        };
        for rel in scoped_files.iter().take(5) {
            if let Ok(res) = symbols::tests_paged_with_path(conn, rel, None, "inbound", &page) {
                if let Some(arr) = res.pointer("/nodes/items").and_then(Value::as_array) {
                    for item in arr {
                        if let (Some(p), Some(q)) =
                            (item["path"].as_str(), item["qualname"].as_str())
                        {
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

fn node_matches_symbol(node: &Node, raw_symbol: &str) -> bool {
    let normalized = raw_symbol.replace("::", ".");
    node.name == raw_symbol
        || node.qualname == normalized
        || node.qualname.ends_with(&format!(".{normalized}"))
}

fn reference_connects_to_target(
    cur_path: &str,
    expr: &str,
    alias: Option<&str>,
    file_imports: &[&str],
    target: &Node,
) -> bool {
    let mentions_name = contains_identifier(expr, &target.name) || alias == Some(target.name.as_str());
    if !mentions_name {
        return false;
    }
    if cur_path == target.path {
        return true;
    }
    if expr.contains("::") || expr.contains('.') {
        return true;
    }
    file_imports
        .iter()
        .any(|imp| contains_identifier(imp, &target.name))
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

fn git_show_head(worktree: &Path, rel_path: &str) -> Option<String> {
    let obj = format!("HEAD:{rel_path}");
    let out = git_command(worktree, &["show", &obj]).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
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
) -> Result<BTreeSet<String>> {
    let mut external = BTreeSet::new();
    if let Some(conn) = conn {
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
            if !policy.is_scope_admitted(&path, admitted_scope) {
                external.insert(path);
            }
        }
        return Ok(external);
    }

    // Fallback when code_db does not exist: check git grep at HEAD for non-test callers.
    if worktree.join(".git").exists() {
        if let Ok(out) = git_command(worktree, &["grep", "-z", "-l", "-F", "-e", &symbol.name, "HEAD", "--"]) {
            if out.status.success() {
                for raw in out.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
                    let s = String::from_utf8_lossy(raw);
                    let path = s.strip_prefix("HEAD:").unwrap_or(&s);
                    if !policy.is_scope_admitted(path, admitted_scope) {
                        if let Some(lang) = languages::for_path(Path::new(path)) {
                            if let Some(src) = git_show_head(worktree, path) {
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
    use acdd_core::ids::RunId;
    use acdd_core::ports::{DomainCheckOutcome, DomainCheckReq, DomainProvider};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "acdd-ut-{tag}-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn code_seam_checks_live_worktree_hops_and_signature_scope() {
        let dir = TempDir::new("code-domain");
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
            "use crate::engine::target_entry;\n#[test]\nfn test_live_seam() {\n    let got = target_entry(1);\n    assert_eq!(got, 2);\n}\n",
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
            let outcome = provider
                .run_check(
                    "seam",
                    &serde_json::Value::Null,
                    &DomainCheckReq {
                        run_id: RunId::new("r1").unwrap(),
                        node_cursor: "t1.contract".parse().unwrap(),
                        gate_id: "contract".parse().unwrap(),
                        workspace_root: root.to_path_buf(),
                        worktree: root.to_path_buf(),
                        admitted_scope: vec!["src/engine.rs".into(), "tests/seam_test.rs".into()],
                        changed_paths: vec![],
                        test_target: Some(target.to_owned()),
                    },
                )
                .unwrap();
            match (&outcome, expect_pass) {
                (DomainCheckOutcome::Passed { execution_digest_salt, .. }, true) => {
                    let salt = execution_digest_salt.as_deref().unwrap_or_default();
                    assert!(salt.contains("index_generation="), "{salt}");
                    assert!(salt.contains("live_files_digest="), "{salt}");
                }
                (DomainCheckOutcome::Failed { .. }, false) => {}
                _ => panic!("unexpected outcome for {label}: {outcome:?}"),
            }
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
                },
            )
            .unwrap();
        assert!(
            matches!(sig_fail, DomainCheckOutcome::Failed { .. }),
            "expected un-scoped external caller to fail: {sig_fail:?}"
        );

        let sig_pass = provider
            .run_check(
                "signature_scope",
                &serde_json::json!({ "changed_signatures": ["target_entry"] }),
                &DomainCheckReq {
                    run_id: RunId::new("r1").unwrap(),
                    node_cursor: "t1.build".parse().unwrap(),
                    gate_id: "build".parse().unwrap(),
                    workspace_root: root.to_path_buf(),
                    worktree: root.to_path_buf(),
                    admitted_scope: vec!["src/engine.rs".into(), "src/outside.rs".into()],
                    changed_paths: vec!["src/engine.rs".into()],
                    test_target: None,
                },
            )
            .unwrap();
        assert!(
            matches!(sig_pass, DomainCheckOutcome::Passed { .. }),
            "expected scoped external caller to pass: {sig_pass:?}"
        );
    }
}
