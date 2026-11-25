use super::types::{is_test_context_path, LintScope, LintViolation};
use crate::db::reader::selectors as reader;
use crate::models::{
    details::NodeDetailFlags,
    tags::{CoverageStatus, EdgeKind, NodeKind},
};
use crate::semantic::types::ValueExpr;
use crate::typed_facts::{decode_facts, FlowState};
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

pub(crate) struct DeadTailsNode {
    pub(crate) hash: i64,
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) qualname: String,
    pub(crate) path: String,
    pub(crate) line: i64,
    pub(crate) end_line: i64,
    pub(crate) is_test_ctx: bool,
    pub(crate) generated: bool,
    pub(crate) language: String,
    pub(crate) is_public: bool,
    pub(crate) has_schema_derive: bool,
    pub(crate) has_entrypoint: bool,
    pub(crate) has_impl_trait: bool,
    pub(crate) has_trait_default: bool,
    pub(crate) has_tool_decorator: bool,
}

pub(crate) struct DanglingCall {
    pub(crate) path: String,
    pub(crate) symbol: String,
    pub(crate) kind: String,
    pub(crate) resolution: String,
}

pub(crate) struct DeadTailsReport {
    pub(crate) nodes: HashMap<i64, DeadTailsNode>,
    pub(crate) fwd: HashMap<i64, Vec<i64>>,
    pub(crate) dead: HashSet<i64>,
    pub(crate) test_callers: HashMap<i64, Vec<i64>>,
    pub(crate) phantom_nodes: Vec<(i64, i64)>,
    pub(crate) dead_tail_nodes: Vec<(i64, i64)>,
    pub(crate) dangling: Vec<DanglingCall>,
}

struct DeadTailsGraph {
    nodes: HashMap<i64, DeadTailsNode>,
    fwd: HashMap<i64, Vec<i64>>,
    candidates: HashSet<i64>,
    roots: HashSet<i64>,
    node_names: HashSet<String>,
    module_names: HashSet<String>,
}

fn is_under_cfg_test_module(qualname: &str, cfg_test_mods: &HashSet<String>) -> bool {
    if cfg_test_mods.is_empty() {
        return false;
    }
    if cfg_test_mods.contains(qualname) {
        return true;
    }
    for (idx, ch) in qualname.char_indices() {
        if ch == '.' && cfg_test_mods.contains(&qualname[..idx]) {
            return true;
        }
    }
    false
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(
        NodeKind::parse(kind),
        Some(NodeKind::Function | NodeKind::Method)
    )
}

struct LoadedNodes {
    nodes: HashMap<i64, DeadTailsNode>,
    node_names: HashSet<String>,
    module_names: HashSet<String>,
}

fn normalize_rust_mod_decl_qualname(qualname: &str, path: &str) -> Option<String> {
    let suffix = if path == "lib.rs" || path.ends_with("/lib.rs") {
        "lib"
    } else if path == "main.rs" || path.ends_with("/main.rs") {
        "main"
    } else {
        return None;
    };
    if let Some(rest) = qualname.strip_prefix(suffix).and_then(|s| s.strip_prefix('.')) {
        return Some(rest.to_owned());
    }
    let needle = format!(".{suffix}.");
    qualname
        .rsplit_once(&needle)
        .map(|(head, tail)| format!("{head}.{tail}"))
}

fn load_dead_tails_nodes(conn: &Connection) -> Result<LoadedNodes> {
    let mut nodes: HashMap<i64, DeadTailsNode> = HashMap::new();
    let mut cfg_test_mods: HashSet<String> = HashSet::new();
    let mut non_test_mod_decls: HashSet<String> = HashSet::new();
    let mut raw_cfg_test_decls: Vec<(String, String)> = Vec::new();
    let mut node_names: HashSet<String> = HashSet::new();
    let mut module_names: HashSet<String> = HashSet::new();

    let mut stmt = conn.prepare(
        "SELECT n.node_hash, n.id, n.kind, n.name, n.qualname, p.path, n.line, n.end_line,
                (n.is_test != 0 OR COALESCE(f.is_test, 0) != 0), n.generated != 0, n.language, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         LEFT JOIN files f ON f.path = p.path",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let hash: i64 = row.get(0)?;
        let id: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let name: String = row.get(3)?;
        let qualname: String = row.get(4)?;
        let path: String = row.get(5)?;
        let line: i64 = row.get(6)?;
        let end_line: i64 = row.get(7)?;
        let base_test: bool = row.get(8)?;
        let generated: bool = row.get(9)?;
        let language: String = row.get(10)?;
        let details_raw: String = row.get(11)?;

        let NodeDetailFlags {
            has_entrypoint,
            has_impl_trait,
            has_trait_default,
            has_cfg_test,
            has_tool,
            has_schema_derive,
            is_public,
            ..
        } = NodeDetailFlags::from_json(&details_raw);

        if kind == "module_declaration" {
            if has_cfg_test {
                raw_cfg_test_decls.push((qualname.clone(), path.clone()));
            } else {
                non_test_mod_decls.insert(qualname.clone());
                if let Some(norm) = normalize_rust_mod_decl_qualname(&qualname, &path) {
                    non_test_mod_decls.insert(norm);
                }
            }
        }
        if !matches!(
            NodeKind::parse(&kind),
            Some(
                NodeKind::Document
                    | NodeKind::Invariant
                    | NodeKind::Field
                    | NodeKind::Variable
                    | NodeKind::Table
                    | NodeKind::Component
            )
        ) {
            node_names.insert(name.clone());
        }
        if kind == "module_declaration"
            || matches!(
                NodeKind::parse(&kind),
                Some(NodeKind::Module | NodeKind::Namespace)
            )
        {
            if let Some(tail) = qualname.rsplit('.').next() {
                if !tail.is_empty() {
                    module_names.insert(tail.to_owned());
                }
            }
        }

        let is_test_ctx = base_test
            || has_cfg_test
            || is_test_context_path(&path)
            || qualname.contains(".tests.")
            || qualname.ends_with(".tests");

        nodes.insert(
            hash,
            DeadTailsNode {
                hash,
                id,
                kind,
                name,
                qualname,
                path,
                line,
                end_line,
                is_test_ctx,
                generated,
                language,
                is_public,
                has_schema_derive,
                has_entrypoint,
                has_impl_trait,
                has_trait_default,
                has_tool_decorator: has_tool,
            },
        );
    }

    for (qn, path) in raw_cfg_test_decls {
        let norm = normalize_rust_mod_decl_qualname(&qn, &path);
        if non_test_mod_decls.contains(&qn)
            || norm.as_deref().is_some_and(|n| non_test_mod_decls.contains(n))
        {
            continue;
        }
        cfg_test_mods.insert(qn);
        if let Some(n) = norm {
            cfg_test_mods.insert(n);
        }
    }

    if !cfg_test_mods.is_empty() {
        for node in nodes.values_mut() {
            if !node.is_test_ctx && is_under_cfg_test_module(&node.qualname, &cfg_test_mods) {
                node.is_test_ctx = true;
            }
        }
    }

    Ok(LoadedNodes {
        nodes,
        node_names,
        module_names,
    })
}

fn load_dead_tails_graph(conn: &Connection) -> Result<DeadTailsGraph> {
    let LoadedNodes {
        nodes,
        node_names,
        module_names,
    } = load_dead_tails_nodes(conn)?;

    let mut fwd_set: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut contains_children: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut trait_members: HashSet<i64> = HashSet::new();
    let mut implements_connected: HashSet<i64> = HashSet::new();
    let mut non_test_ref_targets: HashSet<i64> = HashSet::new();
    let mut non_callable_prod_targets: HashSet<i64> = HashSet::new();
    let mut test_targeted: HashSet<i64> = HashSet::new();

    {
        let mut stmt = conn.prepare(&format!(
            "SELECT src_hash, kind, dst_hash
             FROM edges
             WHERE kind IN ('{calls}', '{refs}', '{possible}', '{impls}', '{contains}')",
            calls = EdgeKind::Calls.as_str(),
            refs = EdgeKind::References.as_str(),
            possible = EdgeKind::PossibleCalls.as_str(),
            impls = EdgeKind::Implements.as_str(),
            contains = EdgeKind::Contains.as_str(),
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let src: i64 = row.get(0)?;
            let kind_str: String = row.get(1)?;
            let dst: i64 = row.get(2)?;
            match EdgeKind::parse(&kind_str) {
                Some(EdgeKind::Contains) => {
                    if let Some(parent) = nodes.get(&src) {
                        if parent.kind == NodeKind::Trait.as_str()
                            || (parent.kind == NodeKind::Impl.as_str() && parent.has_impl_trait)
                        {
                            trait_members.insert(dst);
                        }
                    }
                    if src != dst && nodes.contains_key(&src) && nodes.contains_key(&dst) {
                        contains_children.entry(src).or_default().push(dst);
                    }
                }
                Some(EdgeKind::Implements) => {
                    implements_connected.insert(src);
                    implements_connected.insert(dst);
                }
                Some(
                    edge_kind @ (EdgeKind::Calls | EdgeKind::References | EdgeKind::PossibleCalls),
                ) if src != dst => {
                    let Some(src_node) = nodes.get(&src) else {
                        continue;
                    };
                    if !nodes.contains_key(&dst) {
                        continue;
                    }
                    if src_node.is_test_ctx {
                        test_targeted.insert(dst);
                    }
                    if edge_kind == EdgeKind::References && !src_node.is_test_ctx {
                        non_test_ref_targets.insert(dst);
                    }
                    if !src_node.is_test_ctx
                        && (!is_callable_kind(&src_node.kind) || src_node.generated)
                    {
                        non_callable_prod_targets.insert(dst);
                    }
                    fwd_set.entry(src).or_default().insert(dst);
                }
                _ => {}
            }
        }
    }

    let mut candidates = HashSet::new();
    let mut roots = HashSet::new();
    for node in nodes.values() {
        if !is_callable_kind(&node.kind) || node.generated || node.is_test_ctx {
            continue;
        }
        candidates.insert(node.hash);
        let is_main =
            node.name == "main" && (node.path == "main.rs" || node.path.ends_with("/main.rs"));
        if node.has_entrypoint
            || is_main
            || node.has_tool_decorator
            || implements_connected.contains(&node.hash)
            || trait_members.contains(&node.hash)
            || node.has_trait_default
            || non_test_ref_targets.contains(&node.hash)
            || non_callable_prod_targets.contains(&node.hash)
        {
            roots.insert(node.hash);
        }
    }

    propagate_nested_contains(&nodes, &contains_children, &candidates, &mut fwd_set);
    wire_indexed_callbacks_and_unresolved_receivers(
        conn,
        &nodes,
        &candidates,
        &mut fwd_set,
        &mut roots,
    )?;

    let fwd = fwd_set
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();

    Ok(DeadTailsGraph {
        nodes,
        fwd,
        candidates,
        roots,
        node_names,
        module_names,
    })
}

fn propagate_nested_contains(
    nodes: &HashMap<i64, DeadTailsNode>,
    contains_children: &HashMap<i64, Vec<i64>>,
    candidates: &HashSet<i64>,
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
) {
    for &outer in candidates {
        let Some(children) = contains_children.get(&outer) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut queue: VecDeque<i64> = children.iter().copied().collect();
        while let Some(child_hash) = queue.pop_front() {
            if !seen.insert(child_hash) {
                continue;
            }
            if candidates.contains(&child_hash) {
                fwd_set.entry(outer).or_default().insert(child_hash);
                continue;
            }
            let Some(child) = nodes.get(&child_hash) else {
                continue;
            };
            if !child.is_test_ctx
                && matches!(
                    NodeKind::parse(&child.kind),
                    Some(NodeKind::Struct | NodeKind::Impl | NodeKind::Enum)
                )
            {
                if let Some(nested) = contains_children.get(&child_hash) {
                    queue.extend(nested.iter().copied());
                }
            }
        }
    }
}

fn wire_value_expr_callbacks<F: FnMut(&str, &str, i64)>(
    owner_id: &str,
    expr: &ValueExpr,
    line: i64,
    wire: &mut F,
) {
    match expr {
        ValueExpr::Alias { name } => wire(owner_id, name, line),
        ValueExpr::Call { callee, site } => {
            wire(owner_id, callee, line);
            if let Some(receiver) = site.as_ref().and_then(|s| s.receiver.as_deref()) {
                wire_value_expr_callbacks(owner_id, receiver, line, wire);
            }
        }
        ValueExpr::Await { value }
        | ValueExpr::Scoped { value, .. }
        | ValueExpr::Project { value, .. }
        | ValueExpr::RustField { value, .. }
        | ValueExpr::RustProject { value, .. } => {
            wire_value_expr_callbacks(owner_id, value, line, wire);
        }
        ValueExpr::BranchJoin { alternatives } => {
            for alt in alternatives {
                wire_value_expr_callbacks(owner_id, alt, line, wire);
            }
        }
        ValueExpr::Object { members, .. } => {
            for member in members {
                wire(owner_id, member.declaration_name(), line);
            }
        }
        _ => {}
    }
}

fn wire_indexed_callbacks_and_unresolved_receivers(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    candidates: &HashSet<i64>,
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
    roots: &mut HashSet<i64>,
) -> Result<()> {
    let mut path_cands_by_name: HashMap<&str, HashMap<&str, Vec<&DeadTailsNode>>> = HashMap::new();
    let mut method_cands_by_name: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut path_test_nodes: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut nodes_by_id: HashMap<&str, &DeadTailsNode> = HashMap::with_capacity(nodes.len());
    let mut paths_needing_facts: HashSet<&str> = HashSet::new();

    for node in nodes.values() {
        nodes_by_id.insert(node.id.as_str(), node);
        if node.is_test_ctx {
            path_test_nodes.entry(node.path.as_str()).or_default().push(node);
            continue;
        }
        if candidates.contains(&node.hash) {
            path_cands_by_name
                .entry(node.path.as_str())
                .or_default()
                .entry(node.name.as_str())
                .or_default()
                .push(node);
            if node.kind == NodeKind::Method.as_str() {
                method_cands_by_name
                    .entry(node.name.as_str())
                    .or_default()
                    .push(node);
            }
            if !roots.contains(&node.hash) {
                paths_needing_facts.insert(node.path.as_str());
            }
        }
    }

    if !paths_needing_facts.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT path, facts_blob FROM local_facts WHERE is_test = 0 AND generated = 0",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            if !paths_needing_facts.contains(path.as_str()) {
                continue;
            }
            let Some(by_name) = path_cands_by_name.get(path.as_str()) else {
                continue;
            };
            let blob: Vec<u8> = row.get(1)?;
            let Ok(typed) = decode_facts(&blob) else {
                continue;
            };
            let test_nodes = path_test_nodes.get(path.as_str()).map(Vec::as_slice).unwrap_or(&[]);
            let mut wire_expr = |source_id: &str, expr: &str, line: i64| {
                if test_nodes.iter().any(|t| line >= t.line && line <= t.end_line) {
                    return;
                }
                let caller = nodes_by_id.get(source_id).copied();
                if caller.is_some_and(|c| c.is_test_ctx) {
                    return;
                }
                let tail = expr
                    .rsplit("::")
                    .next()
                    .unwrap_or(expr)
                    .rsplit('.')
                    .next()
                    .unwrap_or(expr);
                let has_scope = expr.contains("::");
                let Some(targets) = by_name.get(tail) else {
                    return;
                };
                for &target in targets {
                    if !has_scope && target.kind != NodeKind::Function.as_str() {
                        continue;
                    }
                    if line >= target.line && line <= target.end_line {
                        continue;
                    }
                    match caller {
                        Some(c) if candidates.contains(&c.hash) && c.hash != target.hash => {
                            fwd_set.entry(c.hash).or_default().insert(target.hash);
                        }
                        Some(_) | None => {
                            roots.insert(target.hash);
                        }
                    }
                }
            };
            for r in &typed.facts.references {
                if r.kind == EdgeKind::References.as_str() {
                    wire_expr(r.source.as_str(), r.expression.as_str(), r.line as i64);
                }
            }
            for (owner_id, state) in typed.flows.cache_entries() {
                let FlowState::Valid { facts, .. } = state else {
                    continue;
                };
                for binding in &facts.bindings {
                    wire_value_expr_callbacks(
                        owner_id,
                        &binding.value,
                        binding.position.line as i64,
                        &mut wire_expr,
                    );
                }
                for join in &facts.branch_joins {
                    wire_value_expr_callbacks(
                        owner_id,
                        &join.value,
                        join.position.line as i64,
                        &mut wire_expr,
                    );
                }
                for field in &facts.fields {
                    wire_value_expr_callbacks(
                        owner_id,
                        &field.value,
                        field.position.line as i64,
                        &mut wire_expr,
                    );
                }
            }
        }
    }

    // Conservatively treat production method candidates as reachable when non-test code
    // has an unresolved receiver method call with the same method name.
    if !method_cands_by_name.is_empty() {
        let mut stmt = conn.prepare(&format!(
            "SELECT p.path, c.line, x.expression
             FROM resolution_coverage c
             JOIN path_dictionary p ON p.path_id = c.path_id
             JOIN coverage_expressions x ON x.expression_id = c.expression_id
             LEFT JOIN files f ON f.path = p.path
             WHERE c.status = '{unresolved}'
               AND COALESCE(f.is_test, 0) = 0
               AND x.expression LIKE '%.%'",
            unresolved = CoverageStatus::Unresolved.as_str(),
        ))?;
        let mut rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        while let Some((path, line, expr)) = rows.next().transpose()? {
            if is_test_context_path(&path)
                || path_test_nodes
                    .get(path.as_str())
                    .is_some_and(|tns| tns.iter().any(|t| line >= t.line && line <= t.end_line))
            {
                continue;
            }
            let tail = expr.rsplit('.').next().unwrap_or("");
            if !tail.is_empty()
                && tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                if let Some(cands) = method_cands_by_name.get(tail) {
                    for &cand in cands {
                        roots.insert(cand.hash);
                    }
                }
            }
        }
    }

    Ok(())
}

fn compute_prod_live(graph: &DeadTailsGraph) -> HashSet<i64> {
    let mut siblings_by_qualname: HashMap<(&str, &str), Vec<i64>> = HashMap::new();
    for &nh in &graph.candidates {
        if let Some(node) = graph.nodes.get(&nh) {
            if !node.qualname.is_empty() {
                siblings_by_qualname
                    .entry((node.path.as_str(), node.qualname.as_str()))
                    .or_default()
                    .push(nh);
            }
        }
    }

    let mut live: HashSet<i64> = graph.roots.iter().copied().collect();
    let mut queue: VecDeque<i64> = graph.roots.iter().copied().collect();

    let enqueue_siblings =
        |node_hash: i64, live: &mut HashSet<i64>, queue: &mut VecDeque<i64>| {
            if let Some(node) = graph.nodes.get(&node_hash) {
                if let Some(siblings) =
                    siblings_by_qualname.get(&(node.path.as_str(), node.qualname.as_str()))
                {
                    for &sib in siblings {
                        if live.insert(sib) {
                            queue.push_back(sib);
                        }
                    }
                }
            }
        };

    while let Some(u) = queue.pop_front() {
        enqueue_siblings(u, &mut live, &mut queue);
        if let Some(callees) = graph.fwd.get(&u) {
            for &v in callees {
                if graph.candidates.contains(&v) {
                    if live.insert(v) {
                        queue.push_back(v);
                    }
                } else {
                    enqueue_siblings(v, &mut live, &mut queue);
                }
            }
        }
    }

    live
}

fn collect_test_reachability(
    graph: &DeadTailsGraph,
    dead: &HashSet<i64>,
) -> HashMap<i64, Vec<i64>> {
    let mut callers_of: HashMap<i64, Vec<i64>> = HashMap::new();
    for (&seed, node) in &graph.nodes {
        if !node.is_test_ctx {
            continue;
        }
        let Some(targets) = graph.fwd.get(&seed) else {
            continue;
        };
        let mut seen: HashSet<i64> = HashSet::new();
        let mut queue = VecDeque::new();
        for &target in targets {
            if dead.contains(&target) && seen.insert(target) {
                queue.push_back(target);
            }
        }
        while let Some(u) = queue.pop_front() {
            callers_of.entry(u).or_default().push(seed);
            if let Some(nexts) = graph.fwd.get(&u) {
                for &v in nexts {
                    if dead.contains(&v) && seen.insert(v) {
                        queue.push_back(v);
                    }
                }
            }
        }
    }
    callers_of
}

fn is_dangling_internal_symbol(
    symbol: &str,
    node_names: &HashSet<String>,
    module_names: &HashSet<String>,
) -> bool {
    let Some(rest) = symbol
        .strip_prefix("crate::")
        .or_else(|| symbol.strip_prefix("super::"))
        .or_else(|| symbol.strip_prefix("self::"))
    else {
        return false;
    };
    if rest.is_empty()
        || symbol.contains('(')
        || symbol.contains('.')
        || symbol.contains('\n')
        || symbol.contains('\r')
        || symbol.contains(' ')
    {
        return false;
    }
    let segs: Vec<&str> = rest.split("::").collect();
    let (Some(&first), Some(&last)) = (segs.first(), segs.last()) else {
        return false;
    };
    let is_lower_ident = |s: &str| {
        !s.is_empty()
            && s.as_bytes()[0].is_ascii_lowercase()
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if !is_lower_ident(first) || !is_lower_ident(last) {
        return false;
    }
    if segs
        .iter()
        .any(|s| s.as_bytes().first().is_some_and(u8::is_ascii_uppercase))
    {
        return false;
    }
    !node_names.contains(last) && !module_names.contains(last)
}

fn collect_dangling_calls(
    conn: &Connection,
    scope: &str,
    lower: &str,
    upper: &str,
    graph: &DeadTailsGraph,
) -> Result<Vec<DanglingCall>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT p.path, d.symbol, d.kind, d.resolution
         FROM dependencies d
         JOIN path_dictionary p ON p.path_id = d.owner_id
         WHERE d.resolution = '{unresolved}'
           AND (?1 = '' OR p.path = ?1 OR (p.path >= ?2 AND p.path < ?3))
           AND (d.symbol GLOB 'crate::*' OR d.symbol GLOB 'super::*' OR d.symbol GLOB 'self::*')
           AND d.symbol NOT GLOB '*(*'
           AND d.symbol NOT GLOB '*.*'",
        unresolved = CoverageStatus::Unresolved.as_str(),
    ))?;
    let mut seen = BTreeSet::new();
    let mut rows = stmt.query(rusqlite::params![scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let symbol: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let resolution: String = row.get(3)?;
        if is_dangling_internal_symbol(&symbol, &graph.node_names, &graph.module_names) {
            seen.insert((path, symbol, kind, resolution));
        }
    }
    Ok(seen
        .into_iter()
        .map(|(path, symbol, kind, resolution)| DanglingCall {
            path,
            symbol,
            kind,
            resolution,
        })
        .collect())
}

pub(crate) fn collect_dead_tails(
    conn: &Connection,
    scope: &str,
) -> Result<DeadTailsReport> {
    let (lower, upper) = reader::path_bounds(scope);
    let graph = load_dead_tails_graph(conn)?;
    let prod_live = compute_prod_live(&graph);
    let dead: HashSet<i64> = graph
        .candidates
        .iter()
        .copied()
        .filter(|nh| !prod_live.contains(nh))
        .collect();
    let test_callers = collect_test_reachability(&graph, &dead);

    let in_scope = |p: &str| -> bool {
        scope.is_empty() || p == scope || (p >= lower.as_str() && p < upper.as_str())
    };

    let mut phantom_nodes: Vec<(i64, i64)> = Vec::new();
    let mut dead_tail_nodes: Vec<(i64, i64)> = Vec::new();

    for &nh in &dead {
        let Some(node) = graph.nodes.get(&nh) else {
            continue;
        };
        if !in_scope(&node.path) {
            continue;
        }
        let lines = (node.end_line - node.line + 1).max(1);
        if test_callers.contains_key(&nh) {
            phantom_nodes.push((nh, lines));
        } else {
            dead_tail_nodes.push((nh, lines));
        }
    }

    let sort_nodes = |a: &(i64, i64), b: &(i64, i64)| {
        let na = &graph.nodes[&a.0];
        let nb = &graph.nodes[&b.0];
        b.1.cmp(&a.1)
            .then_with(|| na.path.cmp(&nb.path))
            .then_with(|| na.line.cmp(&nb.line))
            .then_with(|| na.id.cmp(&nb.id))
    };
    phantom_nodes.sort_by(sort_nodes);
    dead_tail_nodes.sort_by(sort_nodes);

    let dangling = collect_dangling_calls(conn, scope, &lower, &upper, &graph)?;

    Ok(DeadTailsReport {
        nodes: graph.nodes,
        fwd: graph.fwd,
        dead,
        test_callers,
        phantom_nodes,
        dead_tail_nodes,
        dangling,
    })
}

pub(super) fn collect_dead_tails_violations(
    conn: &Connection,
    scope: &LintScope,
    workspace_root: Option<&std::path::Path>,
    include_unused_public_api: bool,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let sql_scope = scope.exact_prefix().unwrap_or("");
    let report = collect_dead_tails(conn, sql_scope)?;

    for (rule_id, label, items) in [
        (
            "dead_tails.phantom_tested",
            "phantom_tested production callable (reachable only from tests)",
            &report.phantom_nodes,
        ),
        (
            "dead_tails.unreachable",
            "dead_tail unreachable production callable",
            &report.dead_tail_nodes,
        ),
    ] {
        for (nh, lines) in items {
            let node = &report.nodes[nh];
            if !scope.in_scope(&node.path) {
                continue;
            }
            violations.push(LintViolation {
                category: LintCategory::DeadTails,
                rule_id: rule_id.to_owned(),
                severity: LintSeverity::Warn,
                path: node.path.clone(),
                line: Some(node.line.max(1) as usize),
                message: format!("{label} '{}' ({lines} lines)", node.qualname),
                invariant: None,
                rule_source: None,
                symbol: Some(node.qualname.clone()),
                target: None,
                suggestion: None,
            });
        }
    }

    for d in report.dangling {
        if !scope.in_scope(&d.path) {
            continue;
        }
        violations.push(LintViolation {
            category: LintCategory::DeadTails,
            rule_id: "dead_tails.dangling_call".to_owned(),
            severity: LintSeverity::Error,
            path: d.path,
            line: None,
            message: format!("unresolved internal reference '{}'", d.symbol),
            invariant: None,
            rule_source: None,
            symbol: Some(d.symbol),
            target: None,
            suggestion: None,
        });
    }

    if include_unused_public_api {
        super::unused_public_api::collect_unused_public_api_violations(
            conn,
            scope,
            workspace_root,
            &report.nodes,
            violations,
        )?;
    }

    Ok(())
}
