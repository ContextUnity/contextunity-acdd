use super::docs_lint::{evaluate_doc_integrity_rule, DocIntegrityFinding};
use super::rules::{
    edge_matches_rule, rule_invariant_id, selector_has_internal_criteria, selector_matches_file,
    selector_matches_node, LoadedRule,
};
use super::types::{
    is_archived_path, is_type_declaration, ExternalOccurrence, IndexedEdge, IndexedNode,
    LintIndexData, LintViolation,
};
use crate::ast::search::{search_matchers_in_source, PreparedAstMatcher};
use crate::ast::SEARCH_MATCH_HORIZON;
use crate::db::cycles::kosaraju_cycle_sccs;
use crate::db::source::VerifiedSourceCache;
use acdd_core::lint::{
    external_symbol_matches_prefix, ArchRuleKind, ArchRuleSpec, ArchitectureConfig, LintCategory,
    ZoneSelector,
};
use acdd_core::path_policy::glob_matches;
use anyhow::{bail, Result};
use petgraph::graphmap::DiGraphMap;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub(super) fn evaluate_arch_rules(
    conn: &Connection,
    index: &mut LintIndexData,
    arch: &ArchitectureConfig,
    rules: &[&LoadedRule],
    doc_findings: &[DocIntegrityFinding],
    workspace_root: Option<&Path>,
    violations: &mut Vec<LintViolation>,
) -> Result<HashSet<(String, usize, String)>> {
    let mut doc_integrity_covered = HashSet::new();
    if rules.is_empty() {
        return Ok(doc_integrity_covered);
    }
    let needs_graph = rules.iter().any(|r| {
        matches!(
            r.spec.kind,
            ArchRuleKind::ForbidDependency
                | ArchRuleKind::AllowOnlyDependencies
                | ArchRuleKind::SingleOwner
                | ArchRuleKind::ForbidCycles
                | ArchRuleKind::BoundaryEntry
                | ArchRuleKind::MustImplement
        )
    });
    let needs_external = rules.iter().any(|r| {
        let to_ext = r
            .spec
            .to
            .as_ref()
            .is_some_and(|s| !s.external_symbols.is_empty());
        to_ext
            && matches!(
                r.spec.kind,
                ArchRuleKind::ForbidDependency
                    | ArchRuleKind::SingleOwner
                    | ArchRuleKind::AllowOnlyDependencies
            )
    });

    if needs_graph {
        index.ensure_nodes(conn)?;
        index.ensure_edges(conn)?;
    }
    if needs_external {
        index.ensure_external(conn)?;
    }
    let dispatch_idx = needs_graph.then(|| InterfaceDispatchIndex::build(index));

    let mut source_cache = VerifiedSourceCache::new();
    let mut ast_rules: Vec<&LoadedRule> = Vec::new();
    for &loaded in rules {
        let spec = &loaded.spec;
        match spec.kind {
            ArchRuleKind::ForbidDependency => eval_dependency_rule(
                index,
                arch,
                dispatch_idx.as_ref().expect("dispatch_idx built"),
                loaded,
                false,
                violations,
            ),
            ArchRuleKind::AllowOnlyDependencies => eval_dependency_rule(
                index,
                arch,
                dispatch_idx.as_ref().expect("dispatch_idx built"),
                loaded,
                true,
                violations,
            ),
            ArchRuleKind::SingleOwner => eval_single_owner(index, arch, loaded, violations),
            ArchRuleKind::ForbidCycles => eval_forbid_cycles(index, arch, loaded, violations),
            ArchRuleKind::BoundaryEntry => eval_boundary_entry(
                index,
                arch,
                dispatch_idx.as_ref().expect("dispatch_idx built"),
                loaded,
                violations,
            ),
            ArchRuleKind::MustImplement => eval_must_implement(index, arch, loaded, violations),
            ArchRuleKind::DocIntegrity => evaluate_doc_integrity_rule(
                index,
                loaded,
                &arch.zones,
                doc_findings,
                violations,
                &mut doc_integrity_covered,
            ),
            ArchRuleKind::AstPattern => ast_rules.push(loaded),
        }
    }
    eval_ast_patterns_batched(
        conn,
        index,
        arch,
        &ast_rules,
        workspace_root,
        &mut source_cache,
        violations,
    )?;
    Ok(doc_integrity_covered)
}

fn push_arch_violation(
    violations: &mut Vec<LintViolation>,
    loaded: &LoadedRule,
    path: String,
    line: Option<usize>,
    message: String,
    symbol: Option<String>,
    target: Option<String>,
) {
    let full_msg = match &loaded.spec.message {
        Some(custom) if !custom.trim().is_empty() => format!("{custom}: {message}"),
        _ => message,
    };
    violations.push(LintViolation {
        category: LintCategory::Arch,
        rule_id: loaded.spec.id.clone(),
        severity: loaded.spec.severity,
        path,
        line,
        message: full_msg,
        invariant: rule_invariant_id(&loaded.spec),
        rule_source: Some(loaded.source.clone()),
        symbol,
        target,
        suggestion: None,
    });
}

fn push_edge_violation(
    violations: &mut Vec<LintViolation>,
    loaded: &LoadedRule,
    edge: &IndexedEdge,
    dst: &IndexedNode,
    msg: String,
) {
    push_arch_violation(
        violations,
        loaded,
        edge.path.clone(),
        Some(edge.line),
        msg,
        Some(dst.qualname.clone()),
        Some(dst.path.clone()),
    );
}

fn push_ext_violation(
    violations: &mut Vec<LintViolation>,
    loaded: &LoadedRule,
    ext: &ExternalOccurrence,
    msg: String,
    display: String,
) {
    push_arch_violation(violations, loaded, ext.path.clone(), ext.line, msg, Some(display), None);
}

struct InterfaceDispatchIndex<'a> {
    method_ifaces: HashMap<i64, Vec<&'a IndexedNode>>,
    enclosing_impl: HashMap<i64, &'a IndexedNode>,
    impls_by_path: HashMap<&'a str, Vec<&'a IndexedNode>>,
    trait_by_name: HashMap<&'a str, Vec<&'a IndexedNode>>,
}

impl<'a> InterfaceDispatchIndex<'a> {
    fn build(index: &'a LintIndexData) -> Self {
        let nodes = index.nodes.as_ref().expect("nodes loaded");
        let edges = index.edges.as_deref().expect("edges loaded");
        let mut method_ifaces: HashMap<i64, Vec<&'a IndexedNode>> = HashMap::new();
        let mut enclosing_impl: HashMap<i64, &'a IndexedNode> = HashMap::new();
        for edge in edges {
            if matches!(edge.kind.as_str(), "implements" | "overrides" | "inherits") {
                if let Some(dst) = nodes.get(&edge.dst_hash) {
                    method_ifaces.entry(edge.src_hash).or_default().push(dst);
                }
            } else if edge.kind == "contains" {
                if let Some(src) = nodes.get(&edge.src_hash) {
                    if src.kind == "impl" {
                        enclosing_impl.insert(edge.dst_hash, src);
                    }
                }
            }
        }
        let mut trait_by_name: HashMap<&'a str, Vec<&'a IndexedNode>> = HashMap::new();
        let mut impls_by_path: HashMap<&'a str, Vec<&'a IndexedNode>> = HashMap::new();
        for node in nodes.values() {
            if matches!(node.kind.as_str(), "trait" | "interface") {
                trait_by_name.entry(node.name.as_str()).or_default().push(node);
            } else if node.kind == "impl" {
                impls_by_path.entry(node.path.as_str()).or_default().push(node);
            }
        }
        Self {
            method_ifaces,
            enclosing_impl,
            impls_by_path,
            trait_by_name,
        }
    }

    fn innermost_enclosing_impl(&self, dst: &'a IndexedNode) -> Option<&'a IndexedNode> {
        if let Some(&imp) = self.enclosing_impl.get(&dst.hash) {
            return Some(imp);
        }
        self.impls_by_path.get(dst.path.as_str()).and_then(|imps| {
            imps.iter()
                .copied()
                .filter(|imp| imp.line <= dst.line && dst.line <= imp.end_line)
                .min_by_key(|imp| imp.end_line.saturating_sub(imp.line))
        })
    }

    fn dispatched_interfaces(&self, dst: &'a IndexedNode) -> Vec<&'a IndexedNode> {
        if let Some(direct) = self.method_ifaces.get(&dst.hash) {
            if !direct.is_empty() {
                return direct.clone();
            }
        }
        let Some(imp) = self.innermost_enclosing_impl(dst) else {
            return Vec::new();
        };
        let Some(tr) = imp.impl_trait.as_deref() else {
            return Vec::new();
        };
        if let Some(direct_impl) = self.method_ifaces.get(&imp.hash) {
            if !direct_impl.is_empty() {
                return direct_impl.clone();
            }
        }
        let clean_tr = clean_type_head(tr);
        self.trait_by_name.get(clean_tr).cloned().unwrap_or_default()
    }
}

fn clean_type_head(raw: &str) -> &str {
    raw.split('<')
        .next()
        .unwrap_or(raw)
        .rsplit("::")
        .next()
        .unwrap_or(raw)
        .trim()
}

fn edge_allowed_for_test_policy(
    index: &LintIndexData,
    src_node: &IndexedNode,
    dst_node: &IndexedNode,
    edge: &IndexedEdge,
    include_tests: bool,
) -> bool {
    if include_tests {
        return true;
    }
    !src_node.is_test
        && !dst_node.is_test
        && !index.is_test_location(&src_node.path, src_node.line)
        && !index.is_test_location(&dst_node.path, dst_node.line)
        && !index.is_test_location(&edge.path, edge.line)
}

fn external_matches_from_selector(
    index: &LintIndexData,
    from_sel: &ZoneSelector,
    arch: &ArchitectureConfig,
    path: &str,
    line: Option<usize>,
) -> bool {
    let lang = index.file_language(path);
    if !selector_matches_file(from_sel, path, &arch.zones, lang) {
        return false;
    }
    if from_sel.symbols.is_empty() && from_sel.decorators.is_empty() {
        return true;
    }
    let (Some(nodes), Some(by_path)) = (index.nodes.as_ref(), index.nodes_by_path.as_ref()) else {
        return false;
    };
    let Some(hashes) = by_path.get(path) else {
        return false;
    };
    let line_num = line.unwrap_or(1);
    hashes.iter().filter_map(|h| nodes.get(h)).any(|n| {
        (line.is_none() || (line_num >= n.line && line_num <= n.end_line))
            && selector_matches_node(from_sel, n, &arch.zones, lang)
    })
}

fn iter_rule_edges<'a>(
    index: &'a LintIndexData,
    spec: &'a ArchRuleSpec,
    require_in_scope: bool,
) -> impl Iterator<Item = (&'a IndexedEdge, &'a IndexedNode, &'a IndexedNode)> {
    let nodes = index.nodes.as_ref().expect("nodes loaded");
    let edges = index.edges.as_deref().expect("edges loaded");
    edges.iter().filter_map(move |edge| {
        if !edge_matches_rule(edge, spec) {
            return None;
        }
        let (src, dst) = (nodes.get(&edge.src_hash)?, nodes.get(&edge.dst_hash)?);
        if is_archived_path(&src.path) || is_archived_path(&dst.path) {
            return None;
        }
        if require_in_scope && !index.scope.in_scope(&edge.path) && !index.scope.in_scope(&src.path)
        {
            return None;
        }
        if !edge_allowed_for_test_policy(index, src, dst, edge, spec.include_tests) {
            return None;
        }
        Some((edge, src, dst))
    })
}

fn iter_rule_externals<'a>(
    index: &'a LintIndexData,
    arch: &'a ArchitectureConfig,
    spec: &'a ArchRuleSpec,
    from_sel: Option<&'a ZoneSelector>,
) -> impl Iterator<Item = &'a ExternalOccurrence> {
    let externals = index
        .external_occurrences
        .as_deref()
        .expect("externals loaded");
    externals.iter().filter(move |ext| {
        if is_archived_path(&ext.path) || !index.scope.in_scope(&ext.path) {
            return false;
        }
        if !spec.include_tests
            && ext.line.map_or_else(
                || index.is_file_test(&ext.path),
                |l| index.is_test_location(&ext.path, l),
            )
        {
            return false;
        }
        from_sel.is_none_or(|f| external_matches_from_selector(index, f, arch, &ext.path, ext.line))
    })
}

fn is_permitted_interface_dispatch<F>(
    dispatch_idx: &InterfaceDispatchIndex<'_>,
    spec: &ArchRuleSpec,
    edge: &IndexedEdge,
    dst: &IndexedNode,
    mut iface_allowed: F,
) -> bool
where
    F: FnMut(&IndexedNode) -> bool,
{
    if !spec.edge_kinds.is_empty() || !spec.confidences.is_empty() || edge.kind != "possible_calls"
    {
        return false;
    }
    let ifaces = dispatch_idx.dispatched_interfaces(dst);
    !ifaces.is_empty() && ifaces.iter().all(|iface| iface_allowed(iface))
}

fn eval_dependency_rule(
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    dispatch_idx: &InterfaceDispatchIndex<'_>,
    loaded: &LoadedRule,
    allow_only: bool,
    violations: &mut Vec<LintViolation>,
) {
    let spec = &loaded.spec;
    let (Some(from_sel), Some(to_sel)) = (&spec.from, &spec.to) else {
        return;
    };
    let check_internal = allow_only || selector_has_internal_criteria(to_sel);
    if check_internal {
        for (edge, src, dst) in iter_rule_edges(index, spec, true) {
            if !selector_matches_node(from_sel, src, &arch.zones, index.file_language(&src.path)) {
                continue;
            }
            if is_permitted_interface_dispatch(dispatch_idx, spec, edge, dst, |iface| {
                let iface_lang = index.file_language(&iface.path);
                if allow_only {
                    iface.path == src.path
                        || selector_matches_node(from_sel, iface, &arch.zones, iface_lang)
                        || selector_matches_node(to_sel, iface, &arch.zones, iface_lang)
                } else {
                    !selector_matches_node(to_sel, iface, &arch.zones, iface_lang)
                }
            }) {
                continue;
            }
            let dst_lang = index.file_language(&dst.path);
            let in_to = selector_has_internal_criteria(to_sel)
                && selector_matches_node(to_sel, dst, &arch.zones, dst_lang);
            let violated = if allow_only {
                let same_file = src.path == dst.path;
                let in_from = selector_matches_node(from_sel, dst, &arch.zones, dst_lang);
                !same_file && !in_from && !in_to
            } else {
                in_to
            };
            if violated {
                let msg = if allow_only {
                    format!(
                        "disallowed dependency '{}' -> '{}' (target '{}' outside allow_only_dependencies whitelist)",
                        src.qualname, dst.qualname, dst.path
                    )
                } else {
                    format!(
                        "forbidden dependency '{}' -> '{}' ({} -> {})",
                        src.qualname, dst.qualname, src.path, dst.path
                    )
                };
                push_edge_violation(violations, loaded, edge, dst, msg);
            }
        }
    }

    if !to_sel.external_symbols.is_empty() {
        for ext in iter_rule_externals(index, arch, spec, Some(from_sel)) {
            let matched = to_sel
                .external_symbols
                .iter()
                .find(|pre| ext.matches_prefix(pre));
            let violated = if allow_only {
                matched.is_none()
            } else {
                matched.is_some()
            };
            if violated {
                let display = ext.display_symbol().to_owned();
                let msg = if let Some(prefix) = matched {
                    format!("forbidden external dependency '{display}' matching '{prefix}'")
                } else {
                    format!(
                        "disallowed external dependency '{display}' outside allow_only_dependencies whitelist"
                    )
                };
                push_ext_violation(violations, loaded, ext, msg, display);
            }
        }
    }
}

fn eval_single_owner(
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    loaded: &LoadedRule,
    violations: &mut Vec<LintViolation>,
) {
    let spec = &loaded.spec;
    let Some(to_sel) = &spec.to else {
        return;
    };
    let is_owner = |path: &str| spec.owner_paths.iter().any(|pat| glob_matches(pat, path));

    if selector_has_internal_criteria(to_sel) {
        for (edge, src, dst) in iter_rule_edges(index, spec, true) {
            if is_owner(&src.path) {
                continue;
            }
            if let Some(from_sel) = &spec.from {
                if !selector_matches_node(from_sel, src, &arch.zones, index.file_language(&src.path))
                {
                    continue;
                }
            }
            if selector_matches_node(to_sel, dst, &arch.zones, index.file_language(&dst.path)) {
                push_edge_violation(
                    violations,
                    loaded,
                    edge,
                    dst,
                    format!(
                        "single_owner violation: '{}' accesses '{}' outside owner_paths",
                        src.path, dst.qualname
                    ),
                );
            }
        }
    }

    if !to_sel.external_symbols.is_empty() {
        for ext in iter_rule_externals(index, arch, spec, spec.from.as_ref()) {
            if is_owner(&ext.path) {
                continue;
            }
            if to_sel
                .external_symbols
                .iter()
                .any(|pre| ext.matches_prefix(pre))
            {
                let display = ext.display_symbol().to_owned();
                push_ext_violation(
                    violations,
                    loaded,
                    ext,
                    format!(
                        "single_owner violation: '{}' uses restricted symbol '{display}' outside owner_paths",
                        ext.path
                    ),
                    display,
                );
            }
        }
    }
}

fn dir_bucket(path: &str, depth: usize) -> String {
    let mut parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() > 1 {
        parts.pop();
    } else {
        return ".".to_owned();
    }
    parts.truncate(depth.max(1));
    parts.join("/")
}

fn glob_specificity(glob: &str) -> (usize, usize, bool, usize) {
    let literal_prefix_len = glob.find(['*', '?', '{']).unwrap_or(glob.len());
    let non_wildcard_len = glob
        .chars()
        .filter(|c| !matches!(c, '*' | '?' | '{' | '}'))
        .count();
    let exact_glob = !glob.contains("**");
    (literal_prefix_len, non_wildcard_len, exact_glob, glob.len())
}

fn zone_bucket(
    path: &str,
    zone_names: &[String],
    zones: &BTreeMap<String, Vec<String>>,
) -> Option<String> {
    zone_names
        .iter()
        .filter_map(|name| {
            let best = zones
                .get(name)?
                .iter()
                .filter(|g| glob_matches(g, path))
                .map(|g| glob_specificity(g))
                .max()?;
            Some((best, name))
        })
        .max_by(|(spec_a, name_a), (spec_b, name_b)| {
            spec_a.cmp(spec_b).then_with(|| name_b.cmp(name_a))
        })
        .map(|(_, name)| name.clone())
}

fn eval_forbid_cycles(
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    loaded: &LoadedRule,
    violations: &mut Vec<LintViolation>,
) {
    let spec = &loaded.spec;
    let zone_list: Vec<String> = if !spec.zones.is_empty() {
        spec.zones.clone()
    } else if spec.depth.is_none() && spec.from.is_none() && !arch.zones.is_empty() {
        arch.zones.keys().cloned().collect()
    } else {
        Vec::new()
    };
    let use_zones = !zone_list.is_empty();
    let file_level = !use_zones && spec.depth.is_none();
    let depth = spec.depth.unwrap_or(2);
    let bucket_of = |path: &str| -> Option<String> {
        if use_zones {
            zone_bucket(path, &zone_list, &arch.zones)
        } else if file_level {
            Some(path.to_owned())
        } else {
            Some(dir_bucket(path, depth))
        }
    };
    let mut buckets: Vec<String> = Vec::new();
    let mut bucket_ids: HashMap<String, usize> = HashMap::new();
    let mut intern_bucket = |name: String| -> usize {
        if let Some(&id) = bucket_ids.get(&name) {
            return id;
        }
        let id = buckets.len();
        buckets.push(name.clone());
        bucket_ids.insert(name, id);
        id
    };
    let mut graph = DiGraphMap::<usize, ()>::new();
    let mut sample_edges: HashMap<(usize, usize), (String, usize, bool)> = HashMap::new();

    for (edge, src, dst) in iter_rule_edges(index, spec, false) {
        if let Some(from_sel) = &spec.from {
            if !selector_matches_node(from_sel, src, &arch.zones, index.file_language(&src.path)) {
                continue;
            }
        }
        let (Some(src_b), Some(dst_b)) = (bucket_of(&src.path), bucket_of(&dst.path)) else {
            continue;
        };
        if src_b == dst_b {
            continue;
        }
        let u = intern_bucket(src_b);
        let v = intern_bucket(dst_b);
        graph.add_edge(u, v, ());
        let edge_in_scope = index.scope.in_scope(&edge.path)
            || index.scope.in_scope(&src.path)
            || index.scope.in_scope(&dst.path);
        sample_edges
            .entry((u, v))
            .and_modify(|existing| {
                existing.2 |= edge_in_scope;
                if (edge.path.as_str(), edge.line) < (existing.0.as_str(), existing.1) {
                    existing.0 = edge.path.clone();
                    existing.1 = edge.line;
                }
            })
            .or_insert_with(|| (edge.path.clone(), edge.line, edge_in_scope));
    }

    for scc in kosaraju_cycle_sccs(&graph) {
        let scc_set: HashSet<usize> = scc.iter().copied().collect();
        let mut names: Vec<&str> = scc.iter().map(|&id| buckets[id].as_str()).collect();
        names.sort_unstable();
        let mut best_loc: Option<(String, usize)> = None;
        let mut any_in_scope = false;
        for (&(u, v), (path, line, in_scope)) in &sample_edges {
            if scc_set.contains(&u) && scc_set.contains(&v) {
                any_in_scope |= *in_scope;
                if best_loc
                    .as_ref()
                    .is_none_or(|(bp, bl)| (path.as_str(), *line) < (bp.as_str(), *bl))
                {
                    best_loc = Some((path.clone(), *line));
                }
            }
        }
        if !any_in_scope {
            continue;
        }
        let (path, line) = best_loc.unwrap_or_else(|| (loaded.source.path.clone(), loaded.source.line));
        let msg = format!("dependency cycle across modules/zones: {}", names.join(" <-> "));
        push_arch_violation(violations, loaded, path, Some(line), msg, None, None);
    }
}

fn eval_boundary_entry(
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    dispatch_idx: &InterfaceDispatchIndex<'_>,
    loaded: &LoadedRule,
    violations: &mut Vec<LintViolation>,
) {
    let spec = &loaded.spec;
    let Some(to_sel) = &spec.to else {
        return;
    };
    let is_entrypoint = |path: &str| spec.entrypoints.iter().any(|pat| glob_matches(pat, path));

    for (edge, src, dst) in iter_rule_edges(index, spec, true) {
        if let Some(from_sel) = &spec.from {
            if !selector_matches_node(from_sel, src, &arch.zones, index.file_language(&src.path)) {
                continue;
            }
        }
        if is_permitted_interface_dispatch(dispatch_idx, spec, edge, dst, |iface| {
            !selector_matches_node(to_sel, iface, &arch.zones, index.file_language(&iface.path))
                || is_entrypoint(&iface.path)
        }) {
            continue;
        }
        let dst_lang = index.file_language(&dst.path);
        let src_lang = index.file_language(&src.path);
        if selector_matches_node(to_sel, dst, &arch.zones, dst_lang)
            && !selector_matches_node(to_sel, src, &arch.zones, src_lang)
            && !is_entrypoint(&dst.path)
        {
            push_edge_violation(
                violations,
                loaded,
                edge,
                dst,
                format!(
                    "boundary_entry violation: '{}' crosses into non-entrypoint '{}' ({})",
                    src.qualname, dst.qualname, dst.path
                ),
            );
        }
    }
}

fn is_public_for_must_implement(node: &IndexedNode) -> bool {
    match node.explicit_is_public {
        Some(flag) => flag,
        // `compact_node_details` strips `"is_public": false` before persistence; for languages
        // whose extractors always emit `is_public`, an absent flag means non-public.
        None => !matches!(
            node.language.as_str(),
            "rust" | "python" | "typescript" | "tsx" | "javascript" | "vue"
        ),
    }
}

fn eval_must_implement(
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    loaded: &LoadedRule,
    violations: &mut Vec<LintViolation>,
) {
    let spec = &loaded.spec;
    let (Some(from_sel), Some(to_sel)) = (&spec.from, &spec.to) else {
        return;
    };
    let nodes = index.nodes.as_ref().expect("nodes loaded");
    let edges = index.edges.as_deref().expect("edges loaded");

    // Precompute type -> satisfied via direct edge or Rust `impl` node
    let mut direct_edges_by_src: HashMap<i64, Vec<&IndexedEdge>> = HashMap::new();
    let mut path_refs_to_target: HashSet<(&str, i64)> = HashSet::new();
    for edge in edges {
        if edge_matches_rule(edge, spec) {
            direct_edges_by_src
                .entry(edge.src_hash)
                .or_default()
                .push(edge);
        }
        if matches!(edge.kind.as_str(), "references" | "implements") {
            path_refs_to_target.insert((edge.path.as_str(), edge.dst_hash));
        }
    }

    // Index Rust `impl Trait for Type` nodes by clean receiver name (supporting cross-file impls)
    let mut impl_nodes_by_receiver: HashMap<&str, Vec<&IndexedNode>> = HashMap::new();
    for node in nodes.values() {
        if node.kind == "impl" {
            if let Some(recv) = node.receiver_type.as_deref() {
                let clean_recv = recv
                    .split('<')
                    .next()
                    .unwrap_or(recv)
                    .rsplit("::")
                    .next()
                    .unwrap_or(recv)
                    .trim();
                if !clean_recv.is_empty() {
                    impl_nodes_by_receiver
                        .entry(clean_recv)
                        .or_default()
                        .push(node);
                }
            }
        }
    }

    let edge_satisfies_to = |src_hash: i64| -> bool {
        let Some(out_edges) = direct_edges_by_src.get(&src_hash) else {
            return false;
        };
        out_edges.iter().any(|edge| {
            nodes
                .get(&edge.dst_hash)
                .is_some_and(|dst| selector_matches_node(to_sel, dst, &arch.zones, index.file_language(&dst.path)))
        })
    };

    let impl_node_satisfies_to = |impl_node: &IndexedNode| -> bool {
        if edge_satisfies_to(impl_node.hash) {
            return true;
        }
        if let Some(trait_name) = impl_node.impl_trait.as_deref() {
            let clean = trait_name.split('<').next().unwrap_or(trait_name).trim();
            let sym_ok = !to_sel.has_path_scope()
                && !direct_edges_by_src.contains_key(&impl_node.hash)
                && to_sel
                    .symbols
                    .iter()
                    .any(|s| clean == s || clean.ends_with(&format!("::{s}")));
            if sym_ok
                || to_sel
                    .external_symbols
                    .iter()
                    .any(|pre| external_symbol_matches_prefix(clean, pre))
            {
                return true;
            }
        }
        false
    };

    let mut candidates: Vec<&IndexedNode> = nodes
        .values()
        .filter(|n| {
            is_type_declaration(&n.kind)
                && !n.generated
                && is_public_for_must_implement(n)
                && !is_archived_path(&n.path)
                && index.scope.in_scope(&n.path)
                && (spec.include_tests || (!n.is_test && !index.is_test_location(&n.path, n.line)))
                && selector_matches_node(from_sel, n, &arch.zones, index.file_language(&n.path))
                && !selector_matches_node(to_sel, n, &arch.zones, index.file_language(&n.path))
        })
        .collect();
    candidates.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line)));

    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for n in nodes.values().filter(|n| is_type_declaration(&n.kind)) {
        *name_counts.entry(n.name.as_str()).or_default() += 1;
    }

    for ty in candidates {
        let unique_name = name_counts.get(ty.name.as_str()).copied().unwrap_or(0) <= 1;
        let satisfied_internal = edge_satisfies_to(ty.hash)
            || impl_nodes_by_receiver
                .get(ty.name.as_str())
                .is_some_and(|impls| {
                    impls.iter().any(|im| {
                        (im.path == ty.path
                            || path_refs_to_target.contains(&(im.path.as_str(), ty.hash))
                            || unique_name)
                            && impl_node_satisfies_to(im)
                    })
                });
        if !satisfied_internal {
            let expected: Vec<String> = to_sel
                .symbols
                .iter()
                .chain(to_sel.external_symbols.iter())
                .cloned()
                .collect();
            push_arch_violation(
                violations,
                loaded,
                ty.path.clone(),
                Some(ty.line),
                format!(
                    "type '{}' ({}) does not implement required contract [{}]",
                    ty.qualname,
                    ty.kind,
                    expected.join(", ")
                ),
                Some(ty.qualname.clone()),
                None,
            );
        }
    }
}

fn compile_ast_matcher(
    lang: &str,
    pattern: &str,
    sample: Option<&str>,
    explicit_langs: bool,
    first_compile_err: &mut Option<anyhow::Error>,
) -> Result<Option<PreparedAstMatcher>> {
    let compiled = PreparedAstMatcher::new(lang, pattern).and_then(|mut m| {
        if let Some(path) = sample {
            m.required_anchors(path)?;
        }
        Ok(m)
    });
    match compiled {
        Ok(m) => Ok(Some(m)),
        Err(err) if explicit_langs => Err(err),
        Err(err) => {
            if first_compile_err.is_none() {
                *first_compile_err = Some(err);
            }
            Ok(None)
        }
    }
}

struct CompiledAstRule<'a> {
    loaded: &'a LoadedRule,
    pattern: &'a str,
    matcher: PreparedAstMatcher,
}

fn eval_ast_patterns_batched(
    conn: &Connection,
    index: &LintIndexData,
    arch: &ArchitectureConfig,
    ast_rules: &[&LoadedRule],
    workspace_root: Option<&Path>,
    source_cache: &mut VerifiedSourceCache,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let match_limit = SEARCH_MATCH_HORIZON;
    let timeout = std::time::Duration::from_secs(2);
    if ast_rules.is_empty() {
        return Ok(());
    }
    let Some(root) = workspace_root else {
        bail!(
            "ast_pattern rule '{}' requires workspace_root for digest-verified source reads",
            ast_rules[0].spec.id
        );
    };

    let mut by_lang_rules: BTreeMap<&str, Vec<CompiledAstRule<'_>>> = BTreeMap::new();
    let mut by_lang_files: BTreeMap<&str, BTreeMap<&str, Vec<usize>>> = BTreeMap::new();

    for &loaded in ast_rules {
        let spec = &loaded.spec;
        let Some(from_sel) = &spec.from else {
            continue;
        };
        let Some(pattern) = spec.ast_pattern.as_deref() else {
            continue;
        };
        let mut files_by_lang: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (path, info) in &index.files {
            if is_archived_path(path) || !index.scope.in_scope(path) {
                continue;
            }
            if !spec.include_tests && index.is_file_test(path) {
                continue;
            }
            let Some(lang) = info.language.as_deref() else {
                continue;
            };
            if crate::languages::by_id(lang).is_none() {
                continue;
            }
            if selector_matches_file(from_sel, path, &arch.zones, Some(lang)) {
                files_by_lang.entry(lang).or_default().push(path.as_str());
            }
        }

        let explicit_langs = !from_sel.languages.is_empty();
        let mut compiled_any = false;
        let mut first_compile_err: Option<anyhow::Error> = None;

        for (lang, paths) in files_by_lang {
            let Some(matcher) = compile_ast_matcher(
                lang,
                pattern,
                paths.first().copied(),
                explicit_langs,
                &mut first_compile_err,
            )?
            else {
                continue;
            };
            compiled_any = true;
            let rule_vec = by_lang_rules.entry(lang).or_default();
            let rule_slot = rule_vec.len();
            rule_vec.push(CompiledAstRule {
                loaded,
                pattern,
                matcher,
            });
            let lang_files = by_lang_files.entry(lang).or_default();
            for path in paths {
                lang_files.entry(path).or_default().push(rule_slot);
            }
        }
        if !compiled_any {
            if let Some(err) = first_compile_err {
                return Err(err);
            }
        }
    }

    for (lang, mut rules_for_lang) in by_lang_rules {
        let Some(file_map) = by_lang_files.get(lang) else {
            continue;
        };
        for (path, rule_slots) in file_map {
            let source = source_cache.read(conn, root, path)?;
            let mut active_set: Vec<bool> = vec![false; rules_for_lang.len()];
            for &slot in rule_slots {
                active_set[slot] = true;
            }
            let mut slot_by_matcher_idx: Vec<usize> = Vec::with_capacity(rule_slots.len());
            let mut active_matchers: Vec<&mut PreparedAstMatcher> = Vec::with_capacity(rule_slots.len());
            for (slot, rule) in rules_for_lang.iter_mut().enumerate() {
                if active_set[slot] {
                    slot_by_matcher_idx.push(slot);
                    active_matchers.push(&mut rule.matcher);
                }
            }
            let deadline = std::time::Instant::now() + timeout;
            let matches = search_matchers_in_source(
                &mut active_matchers,
                &source,
                path,
                match_limit,
                deadline,
            )?;
            for entry in matches {
                let rule_entry = &rules_for_lang[slot_by_matcher_idx[entry.matcher_index]];
                let spec = &rule_entry.loaded.spec;
                if entry.page.work_limited {
                    bail!(
                        "ast_pattern rule '{}' hit AST search work limit/timeout on '{}'",
                        spec.id,
                        path
                    );
                }
                if !entry.page.complete {
                    bail!(
                        "ast_pattern rule '{}' returned incomplete AST search results (limit {match_limit}) on '{}'",
                        spec.id,
                        path
                    );
                }
                for item in entry.page.items {
                    let line = item["line"].as_u64().unwrap_or(1) as usize;
                    if !spec.include_tests && index.is_test_location(path, line) {
                        continue;
                    }
                    let matched_text = item["text"]
                        .as_str()
                        .unwrap_or(rule_entry.pattern)
                        .trim();
                    let msg = format!("forbidden AST pattern match `{matched_text}`");
                    push_arch_violation(
                        violations,
                        rule_entry.loaded,
                        (*path).to_owned(),
                        Some(line),
                        msg,
                        None,
                        None,
                    );
                }
            }
        }
    }
    Ok(())
}
