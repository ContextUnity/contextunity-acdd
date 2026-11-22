use super::{is_test_context_path, LintScope, LintViolation};
use crate::db::{
    commitments::{read_metadata_optional, MetadataKey},
    reader, symbols,
};
use crate::models::{
    details::NodeDetailFlags,
    tags::{CoverageStatus, EdgeKind, NodeKind},
};
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

fn load_dead_tails_graph(conn: &Connection) -> Result<DeadTailsGraph> {
    let mut nodes: HashMap<i64, DeadTailsNode> = HashMap::new();
    let mut cfg_test_mods: HashSet<String> = HashSet::new();
    let mut node_names: HashSet<String> = HashSet::new();
    let mut module_names: HashSet<String> = HashSet::new();

    {
        let mut stmt = conn.prepare(
            "SELECT n.node_hash, n.id, n.kind, n.name, n.qualname, p.path, n.line, n.end_line,
                    (n.is_test != 0 OR COALESCE(f.is_test, 0) != 0), n.generated != 0, n.details
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
            let details_raw: String = row.get(10)?;

            let NodeDetailFlags {
                has_entrypoint,
                has_impl_trait,
                has_trait_default,
                has_cfg_test,
                has_tool,
                ..
            } = NodeDetailFlags::from_json(&details_raw);

            if kind == "module_declaration" && has_cfg_test {
                cfg_test_mods.insert(qualname.clone());
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
                    has_entrypoint,
                    has_impl_trait,
                    has_trait_default,
                    has_tool_decorator: has_tool,
                },
            );
        }
    }

    if !cfg_test_mods.is_empty() {
        for node in nodes.values_mut() {
            if !node.is_test_ctx && is_under_cfg_test_module(&node.qualname, &cfg_test_mods) {
                node.is_test_ctx = true;
            }
        }
    }

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
    wire_same_file_callbacks(
        conn,
        &nodes,
        &candidates,
        &test_targeted,
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

fn strip_comments_and_strings_preserving_lines(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut block_depth = 0usize;

    while i < len {
        if block_depth > 0 {
            if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                out[i] = b' ';
                out[i + 1] = b' ';
                block_depth += 1;
                i += 2;
                continue;
            }
            if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                out[i] = b' ';
                out[i + 1] = b' ';
                block_depth -= 1;
                i += 2;
                continue;
            }
            if bytes[i] != b'\n' {
                out[i] = b' ';
            }
            i += 1;
            continue;
        }

        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i < len && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out[i] = b' ';
            out[i + 1] = b' ';
            block_depth = 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'r'
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        {
            let mut j = i + 1;
            while j < len && bytes[j] == b'#' {
                j += 1;
            }
            if j < len && bytes[j] == b'"' {
                let hashes = j - (i + 1);
                for b in &mut out[i..=j] {
                    *b = b' ';
                }
                i = j + 1;
                while i < len {
                    if bytes[i] == b'"'
                        && i + hashes < len
                        && bytes[i + 1..=i + hashes].iter().all(|&b| b == b'#')
                    {
                        for b in &mut out[i..=i + hashes] {
                            *b = b' ';
                        }
                        i += hashes + 1;
                        break;
                    }
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
                continue;
            }
        }
        if bytes[i] == b'"' {
            out[i] = b' ';
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    if bytes[i + 1] != b'\n' {
                        out[i + 1] = b' ';
                    }
                    i += 2;
                    continue;
                }
                let close = bytes[i] == b'"';
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
                if close {
                    break;
                }
            }
            continue;
        }
        if bytes[i] == b'\'' {
            if i + 2 < len
                && bytes[i + 1] != b'\\'
                && bytes[i + 1] != b'\n'
                && bytes[i + 2] == b'\''
            {
                out[i] = b' ';
                out[i + 1] = b' ';
                out[i + 2] = b' ';
                i += 3;
                continue;
            }
            if i + 1 < len && bytes[i + 1] == b'\\' {
                out[i] = b' ';
                i += 1;
                while i < len && bytes[i] != b'\n' {
                    let close = bytes[i] == b'\'';
                    out[i] = b' ';
                    i += 1;
                    if close {
                        break;
                    }
                }
                continue;
            }
        }
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn scan_bare_callback_tokens<F>(text: &str, match_token: &mut F)
where
    F: FnMut(&str, bool),
{
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if !(b.is_ascii_alphabetic() || b == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let ident = &text[start..i];
        let prev_non_ws = bytes[..start]
            .iter()
            .rposition(|c| !c.is_ascii_whitespace())
            .map(|idx| (idx, bytes[idx]));
        let next_non_ws = bytes[i..]
            .iter()
            .position(|c| !c.is_ascii_whitespace())
            .map(|rel| bytes[i + rel]);

        let preceded_by_dot = matches!(prev_non_ws, Some((_, b'.')));
        let preceded_by_decl_kw = if let Some((idx, _)) = prev_non_ws {
            let word_end = idx + 1;
            let mut word_start = word_end;
            while word_start > 0
                && (bytes[word_start - 1].is_ascii_alphanumeric() || bytes[word_start - 1] == b'_')
            {
                word_start -= 1;
            }
            matches!(
                &text[word_start..word_end],
                "fn" | "mod"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "type"
                    | "let"
                    | "mut"
                    | "const"
                    | "static"
                    | "for"
            )
        } else {
            false
        };
        let preceded_by_scope = matches!(
            prev_non_ws,
            Some((idx, b':')) if idx > 0 && bytes[idx - 1] == b':'
        );
        let followed_by_call_or_colon = matches!(next_non_ws, Some(b'(' | b':'));

        if !preceded_by_dot && !preceded_by_decl_kw && !followed_by_call_or_colon {
            match_token(ident, preceded_by_scope);
        }
    }
}

fn starts_top_level_const_or_static(trimmed: &str) -> bool {
    let mut rest = trimmed;
    if let Some(after_pub) = rest.strip_prefix("pub") {
        let after_pub = after_pub.trim_start();
        if let Some(after_vis) = after_pub.strip_prefix('(') {
            let Some(close) = after_vis.find(')') else {
                return false;
            };
            rest = after_vis[close + 1..].trim_start();
        } else if after_pub.len() < rest.len() - 3 || rest == "pub" {
            rest = after_pub;
        } else {
            return false;
        }
    }
    let after_kw = if let Some(s) = rest.strip_prefix("const") {
        s
    } else if let Some(s) = rest.strip_prefix("static") {
        s
    } else {
        return false;
    };
    if !after_kw.starts_with(char::is_whitespace) {
        return false;
    }
    let after_kw = after_kw.trim_start();
    !after_kw.starts_with("fn")
        || after_kw["fn".len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn index_cands_by_name<'a>(
    cands: &[&'a DeadTailsNode],
) -> HashMap<&'a str, Vec<&'a DeadTailsNode>> {
    let mut by_name: HashMap<&'a str, Vec<&'a DeadTailsNode>> = HashMap::new();
    for &cand in cands {
        by_name.entry(cand.name.as_str()).or_default().push(cand);
    }
    by_name
}

fn wire_same_file_callbacks(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    candidates: &HashSet<i64>,
    test_targeted: &HashSet<i64>,
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
    roots: &mut HashSet<i64>,
) -> Result<()> {
    let mut path_candidates: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut path_test_nodes: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut paths_needing_scan: HashSet<&str> = HashSet::new();

    for node in nodes.values() {
        if node.is_test_ctx {
            path_test_nodes
                .entry(node.path.as_str())
                .or_default()
                .push(node);
            continue;
        }
        if candidates.contains(&node.hash) {
            path_candidates
                .entry(node.path.as_str())
                .or_default()
                .push(node);
            if !roots.contains(&node.hash) {
                paths_needing_scan.insert(node.path.as_str());
            }
        }
    }

    if paths_needing_scan.is_empty() {
        return Ok(());
    }

    let workspace_root =
        read_metadata_optional(conn, MetadataKey::WorkspaceRoot)?.map(std::path::PathBuf::from);

    let mut scanned_on_disk: HashSet<&str> = HashSet::new();
    if let Some(root) = workspace_root.as_deref() {
        let mut source_loader = symbols::VerifiedSourceCache::new();
        for &path in &paths_needing_scan {
            if !path.ends_with(".rs") {
                continue;
            }
            let Ok(raw) = source_loader.read(conn, root, path) else {
                continue;
            };
            scanned_on_disk.insert(path);
            let Some(cands) = path_candidates.get(path) else {
                continue;
            };
            let by_name = index_cands_by_name(cands);
            let prod_fns = cands.as_slice();
            let test_nodes = path_test_nodes.get(path).map(Vec::as_slice).unwrap_or(&[]);

            let masked = strip_comments_and_strings_preserving_lines(&raw);
            let mut in_top_level_const = false;
            let mut const_delim_depth: usize = 0;

            for (idx, line_text) in masked.lines().enumerate() {
                let line_num = (idx + 1) as i64;
                if test_nodes
                    .iter()
                    .any(|t| line_num >= t.line && line_num <= t.end_line)
                {
                    in_top_level_const = false;
                    const_delim_depth = 0;
                    continue;
                }
                let trimmed = line_text.trim_start();
                if trimmed.starts_with("#[")
                    || trimmed.starts_with("#![")
                    || trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ")
                    || (trimmed.starts_with("pub(") && trimmed.contains(" use "))
                {
                    continue;
                }
                let enclosing = prod_fns
                    .iter()
                    .copied()
                    .filter(|f| line_num >= f.line && line_num <= f.end_line)
                    .min_by_key(|f| (f.end_line - f.line, f.line, f.hash));

                if enclosing.is_none()
                    && !in_top_level_const
                    && starts_top_level_const_or_static(trimmed)
                {
                    in_top_level_const = true;
                    const_delim_depth = 0;
                }

                let scan_top_level_const = enclosing.is_none() && in_top_level_const;
                if enclosing.is_some() || scan_top_level_const {
                    scan_bare_callback_tokens(line_text, &mut |ident, has_scope| {
                        let Some(matches) = by_name.get(ident) else {
                            return;
                        };
                        for &target in matches {
                            if !has_scope && target.kind != NodeKind::Function.as_str() {
                                continue;
                            }
                            if line_num >= target.line && line_num <= target.end_line {
                                continue;
                            }
                            if let Some(caller) = enclosing {
                                if caller.hash != target.hash {
                                    fwd_set.entry(caller.hash).or_default().insert(target.hash);
                                }
                            } else {
                                roots.insert(target.hash);
                            }
                        }
                    });
                }

                if in_top_level_const {
                    let mut terminated = false;
                    for b in line_text.bytes() {
                        match b {
                            b'(' | b'[' | b'{' => const_delim_depth += 1,
                            b')' | b']' | b'}' => {
                                const_delim_depth = const_delim_depth.saturating_sub(1)
                            }
                            b';' if const_delim_depth == 0 => terminated = true,
                            _ => {}
                        }
                    }
                    if terminated || enclosing.is_some() {
                        in_top_level_const = false;
                        const_delim_depth = 0;
                    }
                }
            }
        }
    }

    if scanned_on_disk.len() < paths_needing_scan.len() {
        let mut stmt = conn.prepare(
            "SELECT p.path, d.symbol
             FROM dependencies d
             JOIN path_dictionary p ON p.path_id = d.owner_id
             WHERE d.symbol != ''",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            if !paths_needing_scan.contains(path.as_str())
                || scanned_on_disk.contains(path.as_str())
            {
                continue;
            }
            let symbol: String = row.get(1)?;
            if !symbol.contains('(') && !symbol.contains('{') {
                continue;
            }
            let Some(cands) = path_candidates.get(path.as_str()) else {
                continue;
            };
            let by_name = index_cands_by_name(cands);
            scan_bare_callback_tokens(&symbol, &mut |ident, has_scope| {
                let Some(matches) = by_name.get(ident) else {
                    return;
                };
                for &target in matches {
                    if !has_scope && target.kind != NodeKind::Function.as_str() {
                        continue;
                    }
                    if !test_targeted.contains(&target.hash) {
                        roots.insert(target.hash);
                    }
                }
            });
        }
    }

    Ok(())
}

fn compute_prod_live(graph: &DeadTailsGraph) -> HashSet<i64> {
    let mut live: HashSet<i64> = graph.roots.iter().copied().collect();
    let mut queue: VecDeque<i64> = graph.roots.iter().copied().collect();

    while let Some(u) = queue.pop_front() {
        if let Some(callees) = graph.fwd.get(&u) {
            for &v in callees {
                if graph.candidates.contains(&v) && live.insert(v) {
                    queue.push_back(v);
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

    Ok(())
}
