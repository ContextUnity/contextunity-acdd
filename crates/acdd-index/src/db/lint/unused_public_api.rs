use super::dead_tails::DeadTailsNode;
use super::types::{is_test_context_path, LintScope, LintViolation};
use crate::db::commitments::{read_metadata_optional, MetadataKey};
use crate::languages::dependency_registry::DependencyRegistry;
use crate::models::tags::{CoverageStatus, EdgeKind, NodeKind};
use crate::typed_facts::decode_facts;
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::Result;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct StoredExportBinding {
    name: String,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    star: bool,
}

struct ModuleExport {
    module_hash: i64,
    module_qualname: String,
    mod_path: String,
    name: String,
    target_expr: String,
    star: bool,
    line: usize,
    target_paths: HashSet<String>,
    target_hashes: HashSet<i64>,
}

struct ReexportEdge {
    export_idx: usize,
    dst_hash: i64,
    dst_path: String,
    dst_is_module: bool,
}

#[derive(Default)]
struct EdgeEvidence {
    live_nodes: HashSet<i64>,
    same_file_refs: HashMap<i64, Vec<i64>>,
    reexport_edges: Vec<ReexportEdge>,
    consumer_module_imports: Vec<(i64, String, String)>,
    consumer_imported_names: HashMap<(String, i64), HashSet<String>>,
    consumer_module_aliases: HashMap<(String, i64), HashSet<String>>,
    consumer_direct_refs: Vec<(String, i64, String)>,
    consumer_imported_paths: HashMap<(String, String), HashSet<String>>,
}

fn is_excluded_caller_path(path: &str) -> bool {
    is_test_context_path(path)
}

fn is_bin_or_build_path(path: &str, build_script_files: &HashSet<String>) -> bool {
    build_script_files.contains(path)
        || path == "src/main.rs"
        || path.ends_with("/src/main.rs")
        || path == "main.rs"
        || path.ends_with("/main.rs")
        || path.starts_with("src/bin/")
        || path.contains("/src/bin/")
}

fn evidence_tail(evidence: &str) -> &str {
    evidence.rsplit("::").next().unwrap_or(evidence)
}

fn module_tail_name(qualname: &str) -> &str {
    qualname
        .rsplit('.')
        .find(|s| !matches!(*s, "lib" | "mod" | "main"))
        .unwrap_or("")
}

fn is_crate_lib_root(path: &str) -> bool {
    path == "src/lib.rs"
        || path.ends_with("/src/lib.rs")
        || path == "lib.rs"
        || path.ends_with("/lib.rs")
}

fn is_crate_root_path(path: &str) -> bool {
    is_crate_lib_root(path)
        || path == "src/main.rs"
        || path.ends_with("/src/main.rs")
        || path == "main.rs"
        || path.ends_with("/main.rs")
}

fn is_dir_root_mod(path: &str) -> bool {
    is_crate_root_path(path) || path == "mod.rs" || path.ends_with("/mod.rs")
}

fn pkg_scope_for_path(path: &str, dep_registry: Option<&DependencyRegistry>) -> String {
    if let Some((scope, _)) = dep_registry.and_then(|reg| reg.rust_package_for_path(path)) {
        return scope.to_owned();
    }
    path.split_once("/src/")
        .map_or_else(String::new, |(prefix, _)| prefix.to_owned())
}

fn crate_ident_for_lib(path: &str, dep_registry: Option<&DependencyRegistry>) -> Option<String> {
    if !is_crate_lib_root(path) {
        return None;
    }
    if let Some((_, name)) = dep_registry.and_then(|reg| reg.rust_package_for_path(path)) {
        return Some(name.to_owned());
    }
    path.strip_suffix("/src/lib.rs")
        .and_then(|prefix| prefix.rsplit('/').next())
        .map(|s| s.replace('-', "_"))
}

fn should_track_public_item(
    node: &DeadTailsNode,
    enum_qualnames: &HashSet<&str>,
    build_script_files: &HashSet<String>,
) -> bool {
    if node.is_test_ctx
        || node.generated
        || !node.is_public
        || node.language != "rust"
        || is_excluded_caller_path(&node.path)
        || is_bin_or_build_path(&node.path, build_script_files)
    {
        return false;
    }
    if !matches!(
        node.kind.as_str(),
        "function" | "type" | "trait" | "constant" | "struct" | "enum"
    ) || node.has_entrypoint
        || node.name == "main"
        || node.has_tool_decorator
        || node.has_impl_trait
        || node.has_trait_default
        || node.has_schema_derive
    {
        return false;
    }
    !(node.kind == NodeKind::Constant.as_str()
        && node
            .qualname
            .rsplit_once('.')
            .is_some_and(|(parent_qn, _)| enum_qualnames.contains(parent_qn)))
}

struct RustLocalFactsData {
    module_import_aliases: HashMap<(String, String), Vec<String>>,
    mixed_prod_refs: HashMap<String, HashSet<String>>,
}

fn load_rust_local_facts(
    conn: &Connection,
    paths_needing_lines: &HashSet<String>,
    mixed_files: &HashSet<&str>,
    nodes_by_id: &HashMap<&str, &DeadTailsNode>,
    exports: &mut [ModuleExport],
) -> Result<RustLocalFactsData> {
    let mut ref_lines: HashMap<(String, String, Option<String>), usize> = HashMap::new();
    let mut module_import_aliases: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut mixed_prod_refs: HashMap<String, HashSet<String>> = mixed_files
        .iter()
        .map(|&p| (p.to_owned(), HashSet::new()))
        .collect();
    let mut stmt = conn.prepare("SELECT path, facts_blob FROM local_facts WHERE language = 'rust'")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let need_lines = paths_needing_lines.contains(&path);
        let is_mixed = mixed_files.contains(path.as_str());
        let blob: Vec<u8> = row.get(1)?;
        let Ok(typed) = decode_facts(&blob) else {
            continue;
        };
        for r in typed.facts.references {
            let is_prod_source = nodes_by_id
                .get(r.source.as_str())
                .is_none_or(|n| !n.is_test_ctx);
            if r.kind == EdgeKind::Imports.as_str() {
                if let Some(module) = r.module.as_ref() {
                    if is_prod_source {
                        if let Some(alias) = r.alias.as_ref() {
                            if alias != "*" && alias != evidence_tail(module) {
                                module_import_aliases
                                    .entry((path.clone(), module.clone()))
                                    .or_default()
                                    .push(alias.clone());
                            }
                        }
                    }
                    if need_lines {
                        ref_lines
                            .entry((path.clone(), module.clone(), r.alias.clone()))
                            .or_insert(r.line);
                    }
                }
            }
            if is_mixed && is_prod_source {
                let entry = mixed_prod_refs.entry(path.clone()).or_default();
                entry.insert(r.expression);
                if let Some(m) = r.module {
                    entry.insert(m);
                }
            }
        }
    }
    for exp in exports {
        let alias = Some(if exp.star { "*" } else { exp.name.as_str() }.to_owned());
        if let Some(&line) = ref_lines.get(&(exp.mod_path.clone(), exp.target_expr.clone(), alias)) {
            exp.line = line.max(1);
        }
    }
    Ok(RustLocalFactsData {
        module_import_aliases,
        mixed_prod_refs,
    })
}

fn load_module_exports(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    build_script_files: &HashSet<String>,
) -> Result<(Vec<ModuleExport>, HashSet<String>)> {
    let mut exports = Vec::new();
    let mut paths_needing_lines = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT node_hash, details
         FROM nodes
         WHERE kind = 'module' AND language = 'rust' AND is_test = 0 AND details LIKE '%\"exports\"%'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (hash, details_raw): (i64, String) = (row.get(0)?, row.get(1)?);
        let Some(node) = nodes.get(&hash) else {
            continue;
        };
        if node.is_test_ctx
            || node.generated
            || is_excluded_caller_path(&node.path)
            || is_bin_or_build_path(&node.path, build_script_files)
        {
            continue;
        }
        let Ok(details) = serde_json::from_str::<Value>(&details_raw) else {
            continue;
        };
        let Some(bindings) = details
            .get("exports")
            .and_then(|v| serde_json::from_value::<Vec<StoredExportBinding>>(v.clone()).ok())
        else {
            continue;
        };
        for binding in bindings {
            let Some(target_expr) = binding.module else {
                continue;
            };
            paths_needing_lines.insert(node.path.clone());
            exports.push(ModuleExport {
                module_hash: hash,
                module_qualname: node.qualname.clone(),
                mod_path: node.path.clone(),
                name: binding.name,
                target_expr,
                star: binding.star,
                line: 1,
                target_paths: HashSet::new(),
                target_hashes: HashSet::new(),
            });
        }
    }
    Ok((exports, paths_needing_lines))
}

fn collect_build_script_files(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
) -> Result<HashSet<String>> {
    let mut build_files = HashSet::new();
    let mut stmt = conn.prepare("SELECT path FROM files WHERE language = 'rust'")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if path == "build.rs" || path.ends_with("/build.rs") {
            build_files.insert(path);
        }
    }
    if build_files.is_empty() {
        return Ok(build_files);
    }
    let mut import_edges: Vec<(String, String)> = Vec::new();
    let mut estmt = conn.prepare(&format!(
        "SELECT src_hash, dst_hash FROM edges WHERE kind = '{}'",
        EdgeKind::Imports.as_str()
    ))?;
    let mut erows = estmt.query([])?;
    while let Some(row) = erows.next()? {
        if let (Some(src), Some(dst)) = (nodes.get(&row.get::<_, i64>(0)?), nodes.get(&row.get::<_, i64>(1)?)) {
            if src.path != dst.path {
                import_edges.push((src.path.clone(), dst.path.clone()));
            }
        }
    }
    while import_edges
        .iter()
        .fold(false, |changed, (s, d)| changed | (build_files.contains(s) && build_files.insert(d.clone())))
    {}
    Ok(build_files)
}

fn collect_edge_evidence(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    qual_to_hashes: &HashMap<&str, Vec<i64>>,
    export_lookup: &HashMap<(i64, &str), Vec<usize>>,
    module_import_aliases: &HashMap<(String, String), Vec<String>>,
) -> Result<EdgeEvidence> {
    let mut ev = EdgeEvidence::default();
    let mut stmt = conn.prepare(&format!(
        "SELECT o.src_hash, o.kind, o.dst_hash, ev.evidence
         FROM edge_occurrences o
         JOIN coverage_evidence ev ON ev.evidence_id = o.evidence_id
         WHERE o.kind IN ('{}', '{}', '{}', '{}', '{}')",
        EdgeKind::Calls.as_str(),
        EdgeKind::References.as_str(),
        EdgeKind::PossibleCalls.as_str(),
        EdgeKind::Implements.as_str(),
        EdgeKind::Imports.as_str(),
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (src_hash, kind_str, dst_hash, evidence): (i64, String, i64, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let (Some(src), Some(dst)) = (nodes.get(&src_hash), nodes.get(&dst_hash)) else {
            continue;
        };
        if src.is_test_ctx || is_excluded_caller_path(&src.path) {
            continue;
        }
        if src.path == dst.path {
            if kind_str != EdgeKind::Imports.as_str() {
                ev.same_file_refs.entry(src_hash).or_default().push(dst_hash);
            }
            continue;
        }
        if kind_str == EdgeKind::Imports.as_str() {
            if let Some(exp_idxs) = export_lookup.get(&(src_hash, evidence.as_str())) {
                let tail = evidence_tail(&evidence);
                if dst.kind == NodeKind::Module.as_str() {
                    ev.consumer_imported_names
                        .entry((src.path.clone(), dst_hash))
                        .or_default()
                        .insert(tail.to_owned());
                }
                let direct_targets = (dst.kind == NodeKind::Module.as_str() && tail != "*")
                    .then(|| qual_to_hashes.get(format!("{}.{tail}", dst.qualname).as_str()))
                    .flatten();
                for &export_idx in exp_idxs {
                    ev.reexport_edges.push(ReexportEdge {
                        export_idx,
                        dst_hash,
                        dst_path: dst.path.clone(),
                        dst_is_module: dst.kind == NodeKind::Module.as_str(),
                    });
                    for &target_hash in direct_targets.into_iter().flatten() {
                        if let Some(target_node) = nodes.get(&target_hash) {
                            ev.reexport_edges.push(ReexportEdge {
                                export_idx,
                                dst_hash: target_hash,
                                dst_path: target_node.path.clone(),
                                dst_is_module: false,
                            });
                        }
                    }
                }
                continue;
            }
            if dst.kind == NodeKind::Module.as_str() {
                let tail = evidence_tail(&evidence);
                ev.consumer_imported_names
                    .entry((src.path.clone(), dst_hash))
                    .or_default()
                    .insert(tail.to_owned());
                if let Some(aliases) = module_import_aliases.get(&(src.path.clone(), evidence.clone())) {
                    ev.consumer_module_aliases
                        .entry((src.path.clone(), dst_hash))
                        .or_default()
                        .extend(aliases.iter().cloned());
                }
                if tail != "*" {
                    let target_qual = format!("{}.{tail}", dst.qualname);
                    if let Some(targets) = qual_to_hashes.get(target_qual.as_str()) {
                        for &target_hash in targets {
                            if let Some(target_node) = nodes.get(&target_hash) {
                                if target_node.path != src.path {
                                    ev.live_nodes.insert(target_hash);
                                    ev.consumer_imported_paths
                                        .entry((src.path.clone(), evidence.clone()))
                                        .or_default()
                                        .insert(target_node.path.clone());
                                }
                            }
                        }
                    }
                }
                ev.consumer_module_imports.push((dst_hash, src.path.clone(), evidence));
                continue;
            }
        }
        ev.live_nodes.insert(dst_hash);
        if dst.kind != NodeKind::Module.as_str() {
            ev.consumer_direct_refs.push((src.path.clone(), dst_hash, evidence.clone()));
        }
        ev.consumer_imported_paths
            .entry((src.path.clone(), evidence))
            .or_default()
            .insert(dst.path.clone());
    }
    Ok(ev)
}

fn expand_reexport_targets(
    reexport_edges: &[ReexportEdge],
    exports_by_module: &HashMap<i64, Vec<usize>>,
    nodes: &HashMap<i64, DeadTailsNode>,
    qual_to_hashes: &HashMap<&str, Vec<i64>>,
    exports: &mut [ModuleExport],
) {
    let mut star_modules: HashMap<usize, Vec<i64>> = HashMap::new();
    for edge in reexport_edges {
        exports[edge.export_idx].target_paths.insert(edge.dst_path.clone());
        if edge.dst_is_module {
            if exports[edge.export_idx].star {
                star_modules.entry(edge.export_idx).or_default().push(edge.dst_hash);
            }
        } else {
            exports[edge.export_idx].target_hashes.insert(edge.dst_hash);
        }
    }
    loop {
        let mut changed = false;
        for edge in reexport_edges {
            if !edge.dst_is_module {
                continue;
            }
            let parent_idx = edge.export_idx;
            let parent_star = exports[parent_idx].star;
            let parent_tail = evidence_tail(&exports[parent_idx].target_expr).to_owned();
            if !parent_star {
                if let Some(m) = nodes.get(&edge.dst_hash) {
                    if let Some(ths) = qual_to_hashes.get(format!("{}.{parent_tail}", m.qualname).as_str()) {
                        for &th in ths {
                            changed |= exports[parent_idx].target_hashes.insert(th);
                            if let Some(tn) = nodes.get(&th) {
                                changed |= exports[parent_idx].target_paths.insert(tn.path.clone());
                            }
                        }
                    }
                }
            }
            let Some(child_exps) = exports_by_module.get(&edge.dst_hash) else {
                continue;
            };
            let has_named_child = !parent_star
                && child_exps
                    .iter()
                    .any(|&c| !exports[c].star && exports[c].name == parent_tail);
            let has_direct_in_mod = !parent_star
                && nodes.get(&edge.dst_hash).is_some_and(|m| {
                    qual_to_hashes.contains_key(format!("{}.{parent_tail}", m.qualname).as_str())
                });
            for &child_idx in child_exps {
                if parent_star || (!exports[child_idx].star && exports[child_idx].name == parent_tail) {
                    let paths: Vec<String> = exports[child_idx].target_paths.iter().cloned().collect();
                    let hashes: Vec<i64> = exports[child_idx].target_hashes.iter().copied().collect();
                    changed |= paths
                        .into_iter()
                        .fold(false, |c, p| c | exports[parent_idx].target_paths.insert(p));
                    changed |= hashes
                        .into_iter()
                        .fold(false, |c, h| c | exports[parent_idx].target_hashes.insert(h));
                } else if exports[child_idx].star && !has_named_child && !has_direct_in_mod {
                    let matching_hashes: Vec<i64> = exports[child_idx]
                        .target_hashes
                        .iter()
                        .copied()
                        .filter(|th| nodes.get(th).is_some_and(|n| n.name == parent_tail))
                        .collect();
                    for th in matching_hashes {
                        changed |= exports[parent_idx].target_hashes.insert(th);
                        if let Some(tn) = nodes.get(&th) {
                            changed |= exports[parent_idx].target_paths.insert(tn.path.clone());
                        }
                    }
                    for &mod_hash in star_modules.get(&child_idx).into_iter().flatten() {
                        if let Some(m) = nodes.get(&mod_hash) {
                            if let Some(ths) =
                                qual_to_hashes.get(format!("{}.{parent_tail}", m.qualname).as_str())
                            {
                                for &th in ths {
                                    changed |= exports[parent_idx].target_hashes.insert(th);
                                    changed |= exports[parent_idx].target_paths.insert(m.path.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
}

struct ExportMeta<'a> {
    mod_tail: &'a str,
    crate_ident: Option<String>,
    pkg_scope: String,
    exp_dir: &'a str,
    is_crate_root: bool,
    is_dir_root_mod: bool,
}

struct ExportResolutionCtx<'a> {
    nodes: &'a HashMap<i64, DeadTailsNode>,
    qual_to_hashes: &'a HashMap<&'a str, Vec<i64>>,
    type_qualnames: &'a HashSet<&'a str>,
    exports_by_module: &'a HashMap<i64, Vec<usize>>,
    exports: &'a [ModuleExport],
    metas: &'a [ExportMeta<'a>],
    dep_registry: Option<&'a DependencyRegistry>,
}

struct LiveExportState {
    live_exports: Vec<bool>,
    live_targets_by_export: Vec<HashSet<i64>>,
}

struct DirectRefSite<'a> {
    caller_path: &'a str,
    dst_node: &'a DeadTailsNode,
    dst_parent_type: Option<&'a str>,
    segs: &'a [&'a str],
}

fn target_segment_matches(
    seg: &str,
    exp: &ModuleExport,
    dst_node: &DeadTailsNode,
    dst_parent_type: Option<&str>,
) -> bool {
    if !exp.star {
        seg == exp.name
    } else {
        seg == dst_node.name || dst_parent_type.is_some_and(|pt| seg == pt)
    }
}

fn evidence_references_module_prefix(
    ctx: &ExportResolutionCtx<'_>,
    exp_idx: usize,
    site: &DirectRefSite<'_>,
) -> bool {
    if site.segs.len() < 2 {
        return false;
    }
    let exp = &ctx.exports[exp_idx];
    let meta = &ctx.metas[exp_idx];
    let caller_dir = site.caller_path.rsplit_once('/').map_or("", |(d, _)| d);
    for i in 0..site.segs.len() - 1 {
        if !target_segment_matches(site.segs[i + 1], exp, site.dst_node, site.dst_parent_type) {
            continue;
        }
        let prefix = site.segs[i];
        if i == 0
            && prefix == "crate"
            && meta.is_crate_root
            && pkg_scope_for_path(site.caller_path, ctx.dep_registry) == meta.pkg_scope
        {
            return true;
        }
        if (i == 0 || (i == 1 && site.segs[0].is_empty()))
            && meta.crate_ident.as_deref() == Some(prefix)
        {
            return true;
        }
        if i == 0
            && prefix == "super"
            && meta.is_dir_root_mod
            && !meta.exp_dir.is_empty()
            && caller_dir == meta.exp_dir
        {
            return true;
        }
        if !meta.mod_tail.is_empty() && prefix == meta.mod_tail {
            return true;
        }
    }
    false
}

fn export_matches_direct_ref(
    ctx: &ExportResolutionCtx<'_>,
    exp_idx: usize,
    site: &DirectRefSite<'_>,
    consumer_imported_names: &HashMap<(String, i64), HashSet<String>>,
    consumer_module_aliases: &HashMap<(String, i64), HashSet<String>>,
) -> bool {
    let exp = &ctx.exports[exp_idx];
    let meta = &ctx.metas[exp_idx];
    if site.caller_path == exp.mod_path {
        return false;
    }
    if let Some(imported) =
        consumer_imported_names.get(&(site.caller_path.to_owned(), exp.module_hash))
    {
        let uses_bare_or_type_prefix = site.segs.len() <= 1
            || target_segment_matches(site.segs[0], exp, site.dst_node, site.dst_parent_type);
        if uses_bare_or_type_prefix
            && ((!exp.star && imported.contains(&exp.name))
                || imported.contains("*")
                || (exp.star
                    && (imported.contains(&site.dst_node.name)
                        || site.dst_parent_type.is_some_and(|pt| imported.contains(pt)))))
        {
            return true;
        }
        if !exp.star
            && !meta.mod_tail.is_empty()
            && imported.contains(meta.mod_tail)
            && site
                .segs
                .windows(2)
                .any(|w| w[0] == meta.mod_tail && w[1] == exp.name)
        {
            return true;
        }
    }
    if let Some(aliases) =
        consumer_module_aliases.get(&(site.caller_path.to_owned(), exp.module_hash))
    {
        if site.segs.windows(2).any(|w| {
            aliases.contains(w[0])
                && target_segment_matches(w[1], exp, site.dst_node, site.dst_parent_type)
        }) {
            return true;
        }
    }
    evidence_references_module_prefix(ctx, exp_idx, site)
}

fn mark_live_exports(
    ctx: &ExportResolutionCtx<'_>,
    ev: &EdgeEvidence,
    state: &mut LiveExportState,
) {
    let mut by_target_hash: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut by_target_qual: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut star_by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, exp) in ctx.exports.iter().enumerate() {
        for &th in &exp.target_hashes {
            by_target_hash.entry(th).or_default().push(idx);
            if let Some(tn) = ctx.nodes.get(&th) {
                by_target_qual.entry(tn.qualname.as_str()).or_default().push(idx);
            }
        }
        if exp.star {
            for tp in &exp.target_paths {
                star_by_path.entry(tp.as_str()).or_default().push(idx);
            }
        }
    }

    let mut matched_idxs: Vec<usize> = Vec::new();
    let mut segs: Vec<&str> = Vec::new();
    for (caller_path, dst_hash, evidence) in &ev.consumer_direct_refs {
        let Some(dst_node) = ctx.nodes.get(dst_hash) else {
            continue;
        };
        let dst_parent_type = dst_node
            .qualname
            .rsplit_once('.')
            .filter(|(pq, _)| ctx.type_qualnames.contains(pq))
            .and_then(|(pq, _)| pq.rsplit('.').next());
        segs.clear();
        segs.extend(evidence.split("::"));
        matched_idxs.clear();
        matched_idxs.extend(by_target_hash.get(dst_hash).into_iter().flatten().copied());
        if let Some(star_idxs) = star_by_path.get(dst_node.path.as_str()) {
            matched_idxs.extend(star_idxs.iter().copied());
        }
        let mut parent = dst_node.qualname.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('.') {
            if let Some(idxs) = by_target_qual.get(prefix) {
                matched_idxs.extend(idxs.iter().copied());
            }
            parent = prefix;
        }
        let site = DirectRefSite {
            caller_path,
            dst_node,
            dst_parent_type,
            segs: &segs,
        };
        for &idx in &matched_idxs {
            if state.live_exports[idx] && state.live_targets_by_export[idx].contains(dst_hash) {
                continue;
            }
            if export_matches_direct_ref(
                ctx,
                idx,
                &site,
                &ev.consumer_imported_names,
                &ev.consumer_module_aliases,
            ) {
                state.live_exports[idx] = true;
                state.live_targets_by_export[idx].insert(*dst_hash);
                let mut p = dst_node.qualname.as_str();
                while let Some((prefix, _)) = p.rsplit_once('.') {
                    if let Some(ths) = ctx.qual_to_hashes.get(prefix) {
                        for &th in ths {
                            if ctx.exports[idx].target_hashes.contains(&th) {
                                state.live_targets_by_export[idx].insert(th);
                            }
                        }
                    }
                    p = prefix;
                }
            }
        }
    }
    for (dst_mod_hash, caller_path, evidence) in &ev.consumer_module_imports {
        let Some(mod_exp_idxs) = ctx.exports_by_module.get(dst_mod_hash) else {
            continue;
        };
        let tail = evidence_tail(evidence);
        let mut matched_named = false;
        for &exp_idx in mod_exp_idxs {
            if !ctx.exports[exp_idx].star && ctx.exports[exp_idx].name == tail {
                state.live_exports[exp_idx] = true;
                state.live_targets_by_export[exp_idx]
                    .extend(ctx.exports[exp_idx].target_hashes.iter().copied());
                matched_named = true;
            }
        }
        if matched_named {
            continue;
        }
        let target_paths = ev.consumer_imported_paths.get(&(caller_path.clone(), evidence.clone()));
        for &exp_idx in mod_exp_idxs {
            if !ctx.exports[exp_idx].star {
                continue;
            }
            let matching_name_hashes: Vec<i64> = if tail != "*" {
                ctx.exports[exp_idx]
                    .target_hashes
                    .iter()
                    .copied()
                    .filter(|th| ctx.nodes.get(th).is_some_and(|n| n.name == tail))
                    .collect()
            } else {
                Vec::new()
            };
            let path_matches = target_paths.is_some_and(|paths| {
                paths
                    .iter()
                    .any(|p| ctx.exports[exp_idx].target_paths.contains(p))
            });
            if tail == "*" || path_matches || !matching_name_hashes.is_empty() {
                state.live_exports[exp_idx] = true;
                state.live_targets_by_export[exp_idx].extend(matching_name_hashes);
            }
        }
    }
}

fn apply_unresolved_dependencies(
    conn: &Connection,
    ctx: &ExportResolutionCtx<'_>,
    mixed_prod_refs: &HashMap<String, HashSet<String>>,
    consumer_imported_names: &HashMap<(String, i64), HashSet<String>>,
    state: &mut LiveExportState,
    live_nodes: &mut HashSet<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!(
        "SELECT p.path, d.target_hash, d.symbol
         FROM dependencies d
         JOIN path_dictionary p ON p.path_id = d.owner_id
         LEFT JOIN files f ON f.path = p.path
         WHERE COALESCE(f.is_test, 0) = 0
           AND d.resolution = '{}'
           AND d.symbol != ''",
        CoverageStatus::Unresolved.as_str()
    ))?;
    let mut rows = stmt.query([])?;
    let mut unresolved_by_pair: HashMap<String, Vec<String>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let (path, target_hash, symbol): (String, Option<i64>, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        if is_excluded_caller_path(&path) {
            continue;
        }
        if let Some(allowed_syms) = mixed_prod_refs.get(&path) {
            if !allowed_syms.contains(&symbol) {
                continue;
            }
        }
        if let Some(th) = target_hash {
            if ctx
                .nodes
                .get(&th)
                .is_some_and(|dst| dst.path != path && !dst.is_test_ctx)
            {
                live_nodes.insert(th);
            }
        } else if !symbol.contains('\n') && !symbol.contains('(') {
            if let Some((head, tail)) = symbol.rsplit_once("::") {
                let mod_head = head.rsplit("::").next().unwrap_or(head);
                if !mod_head.is_empty() && !tail.is_empty() {
                    unresolved_by_pair
                        .entry(format!("{mod_head}::{tail}"))
                        .or_default()
                        .push(path);
                }
            }
        }
    }
    for (idx, exp) in ctx.exports.iter().enumerate() {
        if state.live_exports[idx] || exp.star {
            continue;
        }
        let meta = &ctx.metas[idx];
        let mut matched = false;
        if !meta.mod_tail.is_empty() {
            let expected_pair = format!("{}::{}", meta.mod_tail, exp.name);
            if let Some(callers) = unresolved_by_pair.get(&expected_pair) {
                matched = callers.iter().any(|caller_path| {
                    consumer_imported_names
                        .get(&(caller_path.clone(), exp.module_hash))
                        .is_some_and(|names| {
                            names.contains(&exp.name)
                                || names.contains("*")
                                || names.contains(meta.mod_tail)
                        })
                });
            }
        }
        if !matched && meta.is_crate_root {
            if let Some(crate_ident) = meta.crate_ident.as_deref() {
                let pkg_pair = format!("{crate_ident}::{}", exp.name);
                if let Some(callers) = unresolved_by_pair.get(&pkg_pair) {
                    matched = callers.iter().any(|caller_path| caller_path != &exp.mod_path);
                }
            }
            if !matched {
                let crate_pair = format!("crate::{}", exp.name);
                if let Some(callers) = unresolved_by_pair.get(&crate_pair) {
                    matched = callers.iter().any(|caller_path| {
                        caller_path != &exp.mod_path
                            && pkg_scope_for_path(caller_path, ctx.dep_registry) == meta.pkg_scope
                    });
                }
            }
        }
        if matched {
            state.live_exports[idx] = true;
            state.live_targets_by_export[idx].extend(exp.target_hashes.iter().copied());
            live_nodes.extend(exp.target_hashes.iter().copied());
        }
    }
    Ok(())
}

fn target_belongs_to_child_export(
    th: i64,
    child: &ModuleExport,
    nodes: &HashMap<i64, DeadTailsNode>,
) -> bool {
    child.target_hashes.contains(&th)
        || (child.star
            && nodes
                .get(&th)
                .is_some_and(|n| child.target_paths.contains(n.path.as_str())))
}

fn activate_matching_child_targets(
    ctx: &ExportResolutionCtx<'_>,
    parent_targets: &HashSet<i64>,
    child_idx: usize,
    state: &mut LiveExportState,
) -> (bool, bool) {
    let child = &ctx.exports[child_idx];
    let mut matched_any = false;
    let mut changed = false;
    for &th in parent_targets {
        if target_belongs_to_child_export(th, child, ctx.nodes) {
            matched_any = true;
            changed |= state.live_targets_by_export[child_idx].insert(th);
        }
    }
    if matched_any && !state.live_exports[child_idx] {
        state.live_exports[child_idx] = true;
        changed = true;
    }
    (matched_any, changed)
}

fn propagate_live_reexports(
    ctx: &ExportResolutionCtx<'_>,
    reexport_edges: &[ReexportEdge],
    state: &mut LiveExportState,
    live_nodes: &mut HashSet<i64>,
) {
    loop {
        let mut changed = false;
        for edge in reexport_edges {
            if !state.live_exports[edge.export_idx] {
                continue;
            }
            changed |= live_nodes.insert(edge.dst_hash);
            if !edge.dst_is_module {
                continue;
            }
            let Some(child_exps) = ctx.exports_by_module.get(&edge.dst_hash) else {
                continue;
            };
            let parent = &ctx.exports[edge.export_idx];
            let parent_targets = state.live_targets_by_export[edge.export_idx].clone();
            if !parent.star {
                let tail = evidence_tail(&parent.target_expr);
                let has_named_child = child_exps
                    .iter()
                    .any(|&c| !ctx.exports[c].star && ctx.exports[c].name == tail);
                let has_direct_in_mod = ctx.nodes.get(&edge.dst_hash).is_some_and(|m| {
                    ctx.qual_to_hashes
                        .contains_key(format!("{}.{tail}", m.qualname).as_str())
                });
                for &child_idx in child_exps {
                    let child = &ctx.exports[child_idx];
                    if !child.star && child.name == tail {
                        if !state.live_exports[child_idx] {
                            state.live_exports[child_idx] = true;
                            changed = true;
                        }
                        if parent_targets.is_empty() {
                            for &th in &child.target_hashes {
                                changed |= state.live_targets_by_export[child_idx].insert(th);
                            }
                        } else {
                            for &th in &parent_targets {
                                if target_belongs_to_child_export(th, child, ctx.nodes) {
                                    changed |= state.live_targets_by_export[child_idx].insert(th);
                                }
                            }
                        }
                    } else if child.star && !has_named_child && !has_direct_in_mod {
                        let (mut matched_any, branch_changed) =
                            activate_matching_child_targets(ctx, &parent_targets, child_idx, state);
                        changed |= branch_changed;
                        if !matched_any && parent_targets.is_empty() {
                            for &th in &child.target_hashes {
                                if ctx.nodes.get(&th).is_some_and(|n| n.name == tail) {
                                    matched_any = true;
                                    changed |= state.live_targets_by_export[child_idx].insert(th);
                                }
                            }
                            if !matched_any {
                                for child_edge in reexport_edges {
                                    if child_edge.export_idx == child_idx && child_edge.dst_is_module
                                    {
                                        if let Some(m) = ctx.nodes.get(&child_edge.dst_hash) {
                                            if let Some(ths) = ctx
                                                .qual_to_hashes
                                                .get(format!("{}.{tail}", m.qualname).as_str())
                                            {
                                                matched_any = true;
                                                for &th in ths {
                                                    changed |= state.live_targets_by_export
                                                        [child_idx]
                                                        .insert(th);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if matched_any && !state.live_exports[child_idx] {
                                state.live_exports[child_idx] = true;
                                changed = true;
                            }
                        }
                    }
                }
            } else {
                for &child_idx in child_exps {
                    changed |=
                        activate_matching_child_targets(ctx, &parent_targets, child_idx, state).1;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn load_node_signatures(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut sigs = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT node_hash, details FROM nodes
         WHERE language = 'rust' AND is_test = 0 AND details != '{}'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (hash, raw): (i64, String) = (row.get(0)?, row.get(1)?);
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let mut text = String::new();
        for key in ["signature", "return_type", "alias_target", "receiver_type"] {
            if let Some(s) = v.get(key).and_then(Value::as_str) {
                text.push_str(s);
                text.push(' ');
            }
        }
        if let Some(params) = v.get("param_types").and_then(Value::as_object) {
            for val in params.values().filter_map(Value::as_str) {
                text.push_str(val);
                text.push(' ');
            }
        }
        if !text.is_empty() {
            sigs.insert(hash, text);
        }
    }
    Ok(sigs)
}

fn mark_signature_reachable_types(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    same_file_refs: &HashMap<i64, Vec<i64>>,
    live_nodes: &mut HashSet<i64>,
) -> Result<()> {
    let is_type_decl = |n: &DeadTailsNode| {
        !n.is_test_ctx && matches!(n.kind.as_str(), "struct" | "enum" | "type" | "trait")
    };
    let mut queue: VecDeque<i64> = live_nodes.iter().copied().collect();
    while let Some(src) = queue.pop_front() {
        for &dst in same_file_refs.get(&src).into_iter().flatten() {
            if nodes.get(&dst).is_some_and(is_type_decl) && live_nodes.insert(dst) {
                queue.push_back(dst);
            }
        }
    }
    let sigs_by_hash = load_node_signatures(conn)?;
    let mut type_nodes_by_path: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    for node in nodes.values().filter(|n| is_type_decl(n)) {
        type_nodes_by_path.entry(node.path.as_str()).or_default().push(node);
    }
    let mut live_by_path: HashMap<&str, Vec<i64>> = HashMap::new();
    for &h in live_nodes.iter() {
        if let Some(n) = nodes.get(&h) {
            live_by_path.entry(n.path.as_str()).or_default().push(h);
        }
    }
    for (path, type_nodes) in type_nodes_by_path {
        let mut stack = live_by_path.get(path).cloned().unwrap_or_default();
        while let Some(src) = stack.pop() {
            let Some(sig) = sigs_by_hash.get(&src) else {
                continue;
            };
            for candidate in &type_nodes {
                if !live_nodes.contains(&candidate.hash)
                    && sig
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .any(|tok| tok == candidate.name)
                    && live_nodes.insert(candidate.hash)
                {
                    stack.push(candidate.hash);
                }
            }
        }
    }
    Ok(())
}

fn push_violation(
    violations: &mut Vec<LintViolation>,
    path: String,
    line: usize,
    message: String,
    symbol: String,
    suggestion: &str,
) {
    violations.push(LintViolation {
        category: LintCategory::DeadTails,
        rule_id: "dead_tails.unused_public_api".to_owned(),
        severity: LintSeverity::Error,
        path,
        line: Some(line),
        message,
        invariant: Some("INV-SURFACE-COMPATIBILITY".to_owned()),
        rule_source: None,
        symbol: Some(symbol),
        target: None,
        suggestion: Some(suggestion.to_owned()),
    });
}

fn emit_export_violations(
    scope: &LintScope,
    exports: &[ModuleExport],
    live_exports: &[bool],
    violations: &mut Vec<LintViolation>,
) {
    for (idx, exp) in exports.iter().enumerate() {
        if live_exports[idx] || !scope.in_scope(&exp.mod_path) {
            continue;
        }
        let msg = if exp.star {
            format!("unused or test-only glob `pub use {}` in '{}'", exp.target_expr, exp.module_qualname)
        } else {
            format!("unused or test-only `pub use` re-export '{}' in '{}'", exp.name, exp.module_qualname)
        };
        push_violation(
            violations,
            exp.mod_path.clone(),
            exp.line,
            msg,
            format!("{}.{}", exp.module_qualname, exp.name),
            "Remove unused `pub use` or narrow visibility to `pub(crate)` / `#[cfg(test)]`.",
        );
    }
}

fn emit_item_violations(
    scope: &LintScope,
    nodes: &HashMap<i64, DeadTailsNode>,
    enum_qualnames: &HashSet<&str>,
    build_script_files: &HashSet<String>,
    live_nodes: &HashSet<i64>,
    violations: &mut Vec<LintViolation>,
) {
    let mut live_qualnames: HashSet<&str> = live_nodes
        .iter()
        .filter_map(|h| nodes.get(h).map(|n| n.qualname.as_str()))
        .collect();
    for h in live_nodes {
        if let Some(node) = nodes.get(h).filter(|n| n.kind == NodeKind::Constant.as_str()) {
            if let Some((parent_qn, _)) = node.qualname.rsplit_once('.') {
                if enum_qualnames.contains(parent_qn) {
                    live_qualnames.insert(parent_qn);
                }
            }
        }
    }
    for node in nodes.values() {
        if !should_track_public_item(node, enum_qualnames, build_script_files)
            || live_nodes.contains(&node.hash)
            || live_qualnames.contains(node.qualname.as_str())
            || !scope.in_scope(&node.path)
        {
            continue;
        }
        push_violation(
            violations,
            node.path.clone(),
            node.line.max(1) as usize,
            format!("public {} '{}' lacks non-test cross-module production callers", node.kind, node.qualname),
            node.qualname.clone(),
            "Narrow to private/`pub(crate)`/`#[cfg(test)]` or delete unused compatibility item.",
        );
    }
}

pub(super) fn collect_unused_public_api_violations(
    conn: &Connection,
    scope: &LintScope,
    workspace_root: Option<&Path>,
    nodes: &HashMap<i64, DeadTailsNode>,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let mut enum_qualnames: HashSet<&str> = HashSet::new();
    let mut type_qualnames: HashSet<&str> = HashSet::new();
    let mut files_with_test_nodes: HashSet<&str> = HashSet::new();
    let mut qual_to_hashes: HashMap<&str, Vec<i64>> = HashMap::new();
    for (&hash, node) in nodes {
        if node.kind == NodeKind::Enum.as_str() {
            enum_qualnames.insert(node.qualname.as_str());
        }
        if matches!(
            node.kind.as_str(),
            "struct" | "enum" | "trait" | "type"
        ) {
            type_qualnames.insert(node.qualname.as_str());
        }
        if node.is_test_ctx {
            files_with_test_nodes.insert(node.path.as_str());
        }
        if node.kind != NodeKind::Module.as_str() {
            qual_to_hashes
                .entry(node.qualname.as_str())
                .or_default()
                .push(hash);
        }
    }

    let resolved_root = workspace_root.map(Path::to_path_buf).or_else(|| {
        read_metadata_optional(conn, MetadataKey::WorkspaceRoot)
            .ok()
            .flatten()
            .map(PathBuf::from)
    });
    let dep_registry = resolved_root
        .as_deref()
        .filter(|root| root.is_dir())
        .map(|root| DependencyRegistry::collect(Some(root)));

    let build_script_files = collect_build_script_files(conn, nodes)?;
    let (mut exports, paths_needing_lines) = load_module_exports(conn, nodes, &build_script_files)?;
    let nodes_by_id: HashMap<&str, &DeadTailsNode> =
        nodes.values().map(|n| (n.id.as_str(), n)).collect();
    let local_facts_data = load_rust_local_facts(
        conn,
        &paths_needing_lines,
        &files_with_test_nodes,
        &nodes_by_id,
        &mut exports,
    )?;
    let mut export_lookup: HashMap<(i64, &str), Vec<usize>> = HashMap::new();
    let mut exports_by_module: HashMap<i64, Vec<usize>> = HashMap::new();
    for (idx, exp) in exports.iter().enumerate() {
        export_lookup.entry((exp.module_hash, exp.target_expr.as_str())).or_default().push(idx);
        exports_by_module.entry(exp.module_hash).or_default().push(idx);
    }

    let mut ev = collect_edge_evidence(
        conn,
        nodes,
        &qual_to_hashes,
        &export_lookup,
        &local_facts_data.module_import_aliases,
    )?;
    expand_reexport_targets(
        &ev.reexport_edges,
        &exports_by_module,
        nodes,
        &qual_to_hashes,
        &mut exports,
    );
    let metas: Vec<ExportMeta<'_>> = exports
        .iter()
        .map(|exp| {
            let is_crate_root = is_crate_root_path(&exp.mod_path);
            ExportMeta {
                mod_tail: if is_crate_root {
                    ""
                } else {
                    module_tail_name(&exp.module_qualname)
                },
                crate_ident: crate_ident_for_lib(&exp.mod_path, dep_registry.as_ref()),
                pkg_scope: pkg_scope_for_path(&exp.mod_path, dep_registry.as_ref()),
                exp_dir: exp.mod_path.rsplit_once('/').map_or("", |(d, _)| d),
                is_crate_root,
                is_dir_root_mod: is_dir_root_mod(&exp.mod_path),
            }
        })
        .collect();
    let mut state = LiveExportState {
        live_exports: vec![false; exports.len()],
        live_targets_by_export: vec![HashSet::new(); exports.len()],
    };
    let ctx = ExportResolutionCtx {
        nodes,
        qual_to_hashes: &qual_to_hashes,
        type_qualnames: &type_qualnames,
        exports_by_module: &exports_by_module,
        exports: &exports,
        metas: &metas,
        dep_registry: dep_registry.as_ref(),
    };
    mark_live_exports(&ctx, &ev, &mut state);
    apply_unresolved_dependencies(
        conn,
        &ctx,
        &local_facts_data.mixed_prod_refs,
        &ev.consumer_imported_names,
        &mut state,
        &mut ev.live_nodes,
    )?;
    propagate_live_reexports(&ctx, &ev.reexport_edges, &mut state, &mut ev.live_nodes);
    mark_signature_reachable_types(conn, nodes, &ev.same_file_refs, &mut ev.live_nodes)?;
    emit_export_violations(scope, &exports, &state.live_exports, violations);
    emit_item_violations(scope, nodes, &enum_qualnames, &build_script_files, &ev.live_nodes, violations);
    Ok(())
}
