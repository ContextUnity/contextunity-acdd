use super::types::{is_test_context_path, LintScope, LintViolation};
use crate::db::{
    commitments::{read_metadata_optional, MetadataKey},
    reader::selectors as reader,
    source,
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

struct LoadedNodes {
    nodes: HashMap<i64, DeadTailsNode>,
    node_names: HashSet<String>,
    module_names: HashSet<String>,
}

fn load_dead_tails_nodes(conn: &Connection) -> Result<LoadedNodes> {
    let mut nodes: HashMap<i64, DeadTailsNode> = HashMap::new();
    let mut cfg_test_mods: HashSet<String> = HashSet::new();
    let mut node_names: HashSet<String> = HashSet::new();
    let mut module_names: HashSet<String> = HashSet::new();

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

fn blank_non_newline_byte(out: &mut [u8], idx: usize) {
    if out[idx] != b'\n' {
        out[idx] = b' ';
    }
}

fn blank_block_comment_step(bytes: &[u8], out: &mut [u8], i: usize, depth: &mut usize) -> usize {
    if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
        out[i] = b' ';
        out[i + 1] = b' ';
        *depth += 1;
        return i + 2;
    }
    if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
        out[i] = b' ';
        out[i + 1] = b' ';
        *depth -= 1;
        return i + 2;
    }
    blank_non_newline_byte(out, i);
    i + 1
}

fn try_blank_raw_string(bytes: &[u8], out: &mut [u8], i: usize) -> Option<usize> {
    if bytes[i] != b'r'
        || (i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
    {
        return None;
    }
    let len = bytes.len();
    let mut j = i + 1;
    while j < len && bytes[j] == b'#' {
        j += 1;
    }
    if j >= len || bytes[j] != b'"' {
        return None;
    }
    let hashes = j - (i + 1);
    out[i..=j].fill(b' ');
    let mut cur = j + 1;
    while cur < len {
        if bytes[cur] == b'"'
            && cur + hashes < len
            && bytes[cur + 1..=cur + hashes].iter().all(|&b| b == b'#')
        {
            out[cur..=cur + hashes].fill(b' ');
            return Some(cur + hashes + 1);
        }
        blank_non_newline_byte(out, cur);
        cur += 1;
    }
    Some(cur)
}

fn blank_quoted_string(bytes: &[u8], out: &mut [u8], mut i: usize, quote: u8, multiline: bool) -> usize {
    let len = bytes.len();
    out[i] = b' ';
    i += 1;
    while i < len && (multiline || bytes[i] != b'\n') {
        if bytes[i] == b'\\' && i + 1 < len {
            blank_non_newline_byte(out, i);
            blank_non_newline_byte(out, i + 1);
            i += 2;
            continue;
        }
        let close = bytes[i] == quote;
        blank_non_newline_byte(out, i);
        i += 1;
        if close {
            break;
        }
    }
    i
}

fn strip_comments_and_strings_preserving_lines(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut block_depth = 0usize;

    while i < len {
        if block_depth > 0 {
            i = blank_block_comment_step(bytes, &mut out, i, &mut block_depth);
            continue;
        }
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            out[i..i + 2].fill(b' ');
            i += 2;
            while i < len && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out[i..i + 2].fill(b' ');
            block_depth = 1;
            i += 2;
            continue;
        }
        if let Some(next_i) = try_blank_raw_string(bytes, &mut out, i) {
            i = next_i;
            continue;
        }
        if bytes[i] == b'"' {
            i = blank_quoted_string(bytes, &mut out, i, b'"', true);
            continue;
        }
        if bytes[i] == b'\'' {
            if i + 2 < len
                && bytes[i + 1] != b'\\'
                && bytes[i + 1] != b'\n'
                && bytes[i + 2] == b'\''
            {
                out[i..i + 3].fill(b' ');
                i += 3;
                continue;
            }
            if i + 1 < len && bytes[i + 1] == b'\\' {
                i = blank_quoted_string(bytes, &mut out, i, b'\'', false);
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

fn for_matching_callback_targets<'a>(
    by_name: &HashMap<&'a str, Vec<&'a DeadTailsNode>>,
    ident: &str,
    has_scope: bool,
    mut on_match: impl FnMut(&'a DeadTailsNode),
) {
    let Some(matches) = by_name.get(ident) else {
        return;
    };
    for &target in matches {
        if has_scope || target.kind == NodeKind::Function.as_str() {
            on_match(target);
        }
    }
}

fn scan_rust_file_callbacks(
    raw: &str,
    cands: &[&DeadTailsNode],
    test_nodes: &[&DeadTailsNode],
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
    roots: &mut HashSet<i64>,
) {
    let by_name = index_cands_by_name(cands);
    let masked = strip_comments_and_strings_preserving_lines(raw);
    let mut in_top_level_const = false;
    let mut const_delim_depth: usize = 0;

    for (idx, line_text) in masked.lines().enumerate() {
        let line_num = (idx + 1) as i64;
        if test_nodes.iter().any(|t| line_num >= t.line && line_num <= t.end_line) {
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
        let enclosing = cands
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

        if enclosing.is_some() || in_top_level_const {
            scan_bare_callback_tokens(line_text, &mut |ident, has_scope| {
                for_matching_callback_targets(&by_name, ident, has_scope, |target| {
                    if line_num >= target.line && line_num <= target.end_line {
                        return;
                    }
                    if let Some(caller) = enclosing {
                        if caller.hash != target.hash {
                            fwd_set.entry(caller.hash).or_default().insert(target.hash);
                        }
                    } else {
                        roots.insert(target.hash);
                    }
                });
            });
        }

        if in_top_level_const {
            let mut terminated = false;
            for b in line_text.bytes() {
                match b {
                    b'(' | b'[' | b'{' => const_delim_depth += 1,
                    b')' | b']' | b'}' => const_delim_depth = const_delim_depth.saturating_sub(1),
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
            path_test_nodes.entry(node.path.as_str()).or_default().push(node);
            continue;
        }
        if candidates.contains(&node.hash) {
            path_candidates.entry(node.path.as_str()).or_default().push(node);
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
        let mut source_loader = source::VerifiedSourceCache::new();
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
            let test_nodes = path_test_nodes.get(path).map(Vec::as_slice).unwrap_or(&[]);
            scan_rust_file_callbacks(&raw, cands, test_nodes, fwd_set, roots);
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
                for_matching_callback_targets(&by_name, ident, has_scope, |target| {
                    if !test_targeted.contains(&target.hash) {
                        roots.insert(target.hash);
                    }
                });
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
    workspace_root: Option<&std::path::Path>,
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

    collect_unused_public_api_violations(conn, scope, workspace_root, &report.nodes, violations)?;

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ModLocation { crate_name: String, segments: Vec<String> }
struct CrateMeta { root_prefix: String, crate_name: String, lib_entry: String }
struct CrateLayout { crates: Vec<CrateMeta>, known_crate_names: HashSet<String>, build_script_files: HashSet<String> }
struct PublicApiItem { hash: i64, kind: String, name: String, qualname: String, path: String, line: usize, loc: ModLocation, parent_type: Option<String> }
struct ParsedUse { prefix: Vec<String>, item: String, alias: String, is_pub_top_level: bool, line: usize }
struct ResolvedUse { mod_loc: ModLocation, item: String, alias: String }
struct NamedReexport { mod_loc: ModLocation, mod_path: String, line: usize, target_loc: ModLocation, target_item: String, exposed_name: String }
struct GlobReexport { mod_loc: ModLocation, mod_path: String, line: usize, target_loc: ModLocation }
struct OwnerContext { crate_name: String, segments: Vec<String>, is_bin: bool }
struct CorpusFile { owner: OwnerContext, code_only: String, resolved_uses: Vec<ResolvedUse>, path_chains: Vec<Vec<String>>, bare_idents: HashSet<String>, pub_top_uses: Vec<ParsedUse> }
struct NonTestRustCorpus { files: HashMap<String, CorpusFile>, ident_files: HashMap<String, Vec<String>>, mod_importers: HashMap<ModLocation, Vec<String>> }
struct ProdCrossFileEdges<'a> { hashes: HashSet<i64>, qualnames: HashSet<&'a str> }

fn is_excluded_caller_path(path: &str) -> bool {
    is_test_context_path(path)
        || path.ends_with("/harness.rs")
        || path == "harness.rs"
        || path.contains("test_support")
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

fn is_excluded_public_api_path(path: &str, build_script_files: &HashSet<String>) -> bool {
    is_excluded_caller_path(path) || is_bin_or_build_path(path, build_script_files)
}

fn strip_toml_string(raw: &str) -> Option<String> {
    let val = raw.split('#').next().unwrap_or("").trim();
    if val.len() >= 2 && ((val.starts_with('"') && val.ends_with('"')) || (val.starts_with('\'') && val.ends_with('\''))) {
        return Some(val[1..val.len() - 1].to_owned());
    }
    None
}

fn parse_cargo_toml_crate_meta(content: &str, root_prefix: &str) -> Option<CrateMeta> {
    let (mut section, mut pkg_name, mut lib_name, mut lib_path) = ("", None, None, None);
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue; };
        let Some(val) = strip_toml_string(v) else { continue; };
        match (section, k.trim()) {
            ("package", "name") => pkg_name = Some(val),
            ("lib", "name") => lib_name = Some(val),
            ("lib", "path") => lib_path = Some(val),
            _ => {}
        }
    }
    let raw_name = lib_name.or(pkg_name)?;
    let lib_rel = lib_path.unwrap_or_else(|| "src/lib.rs".to_owned());
    let lib_rel = lib_rel.strip_prefix("./").unwrap_or(&lib_rel);
    Some(CrateMeta {
        root_prefix: root_prefix.to_owned(),
        crate_name: raw_name.replace('-', "_"),
        lib_entry: format!("{root_prefix}{lib_rel}"),
    })
}

fn normalize_rel_join(base_dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    let rel_norm = rel.replace('\\', "/");
    for seg in rel_norm.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop()?; }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

fn extract_build_script_submodules_from_source(build_rs_path: &str, src: &str, indexed_rs: &HashSet<String>, out: &mut HashSet<String>) {
    let parent = build_rs_path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut pending_path: Option<String> = None;
    for line in src.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("#[path") {
            if let Some((_, rhs)) = rest.split_once('=') {
                pending_path = strip_toml_string(rhs.trim().trim_end_matches(']').trim());
            }
            continue;
        }
        if trimmed.starts_with("#[") || trimmed.starts_with("//") || trimmed.is_empty() { continue; }
        let after_vis = trimmed.strip_prefix("pub ").unwrap_or(trimmed).trim();
        if let Some(name_part) = after_vis.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) {
            let mod_name = name_part.trim();
            if let Some(explicit) = pending_path.take() {
                if let Some(resolved) = normalize_rel_join(parent, &explicit) { out.insert(resolved); }
                continue;
            }
            for suffix in [format!("{mod_name}.rs"), format!("{mod_name}/mod.rs")] {
                if let Some(resolved) = normalize_rel_join(parent, &suffix).filter(|r| indexed_rs.contains(r)) {
                    out.insert(resolved);
                }
            }
            continue;
        }
        pending_path = None;
    }
}

fn discover_build_script_files(conn: &Connection, workspace_root: Option<&std::path::Path>, nodes: &HashMap<i64, DeadTailsNode>, indexed_rs: &HashSet<String>) -> HashSet<String> {
    let mut build_files: HashSet<String> = indexed_rs.iter().filter(|p| *p == "build.rs" || p.ends_with("/build.rs")).cloned().collect();
    if let Ok(mut stmt) = conn.prepare(&format!("SELECT src_hash, dst_hash FROM edges WHERE kind = '{}'", EdgeKind::Imports.as_str())) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                if let (Ok(src_h), Ok(dst_h)) = (row.get::<_, i64>(0), row.get::<_, i64>(1)) {
                    if let (Some(src), Some(dst)) = (nodes.get(&src_h), nodes.get(&dst_h)) {
                        if src.path == "build.rs" || src.path.ends_with("/build.rs") {
                            build_files.insert(dst.path.clone());
                        }
                    }
                }
            }
        }
    }
    if let Some(root) = workspace_root {
        let mut cache = source::VerifiedSourceCache::new();
        let seeds: Vec<String> = build_files.iter().filter(|p| *p == "build.rs" || p.ends_with("/build.rs")).cloned().collect();
        for build_rs in seeds {
            if let Ok(src) = cache.read(conn, root, &build_rs) {
                extract_build_script_submodules_from_source(&build_rs, &src, indexed_rs, &mut build_files);
            }
        }
    }
    build_files
}

impl CrateLayout {
    fn discover(conn: &Connection, workspace_root: Option<&std::path::Path>, nodes: &HashMap<i64, DeadTailsNode>) -> Result<Self> {
        let mut indexed_rs = HashSet::new();
        let mut stmt = conn.prepare("SELECT path FROM files WHERE language = 'rust'")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? { indexed_rs.insert(row.get::<_, String>(0)?); }

        let mut crates_by_prefix: HashMap<String, CrateMeta> = HashMap::new();
        if let Some(root) = workspace_root {
            let mut candidate_dirs: HashSet<String> = HashSet::from([String::new()]);
            for path in &indexed_rs {
                let mut cur = path.as_str();
                while let Some((parent, _)) = cur.rsplit_once('/') {
                    candidate_dirs.insert(format!("{parent}/"));
                    cur = parent;
                }
            }
            for prefix in candidate_dirs {
                let rel_cargo = format!("{prefix}Cargo.toml");
                let mut opts = std::fs::OpenOptions::new();
                opts.read(true);
                if let Ok(mut file) = acdd_core::fs::open_confined(root, std::path::Path::new(&rel_cargo), &mut opts) {
                    let mut content = String::new();
                    if std::io::Read::read_to_string(&mut file, &mut content).is_ok() {
                        if let Some(meta) = parse_cargo_toml_crate_meta(&content, &prefix) {
                            crates_by_prefix.insert(prefix, meta);
                        }
                    }
                }
            }
        }
        for path in &indexed_rs {
            if let Some((dir, _)) = path.strip_prefix("crates/").and_then(|r| r.split_once('/')) {
                let prefix = format!("crates/{dir}/");
                crates_by_prefix.entry(prefix.clone()).or_insert_with(|| CrateMeta {
                    root_prefix: prefix.clone(),
                    crate_name: dir.replace('-', "_"),
                    lib_entry: format!("{prefix}src/lib.rs"),
                });
            }
        }
        let mut crates: Vec<CrateMeta> = crates_by_prefix.into_values().collect();
        crates.sort_by_key(|a| std::cmp::Reverse(a.root_prefix.len()));
        let known_crate_names = crates.iter().map(|c| c.crate_name.clone()).collect();
        let build_script_files = discover_build_script_files(conn, workspace_root, nodes, &indexed_rs);
        Ok(Self { crates, known_crate_names, build_script_files })
    }

    fn crate_for_path(&self, path: &str) -> Option<&CrateMeta> {
        self.crates.iter().find(|c| c.root_prefix.is_empty() || path.starts_with(&c.root_prefix))
    }

    fn module_segments(&self, path: &str) -> (String, Vec<String>) {
        if let Some(meta) = self.crate_for_path(path) {
            if path == meta.lib_entry { return (meta.crate_name.clone(), Vec::new()); }
            let rel = path.strip_prefix(&meta.root_prefix).unwrap_or(path);
            let inner = rel.strip_prefix("src/").unwrap_or(rel);
            let stem = inner.strip_suffix(".rs").unwrap_or(inner).strip_suffix("/mod").unwrap_or_else(|| inner.strip_suffix(".rs").unwrap_or(inner));
            let segs = stem.split('/').filter(|s| !s.is_empty() && *s != "lib" && *s != "main").map(str::to_owned).collect();
            return (meta.crate_name.clone(), segs);
        }
        let stem = path.strip_suffix(".rs").unwrap_or(path).strip_suffix("/mod").unwrap_or_else(|| path.strip_suffix(".rs").unwrap_or(path));
        (String::new(), stem.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect())
    }

    fn owner_context(&self, path: &str) -> OwnerContext {
        let (crate_name, segments) = self.module_segments(path);
        OwnerContext { crate_name, segments, is_bin: is_bin_or_build_path(path, &self.build_script_files) }
    }
}

fn has_derive_or_serde_decorator(details: &serde_json::Value) -> bool {
    const WIRE_CLI_DERIVES: &[&str] = &[
        "Serialize",
        "Deserialize",
        "JsonSchema",
        "Parser",
        "Args",
        "Subcommand",
        "ValueEnum",
    ];
    let Some(arr) = details.get("decorators").and_then(|v| v.as_array()) else {
        return false;
    };
    arr.iter().filter_map(|v| v.as_str()).any(|raw| {
        let d = raw.trim();
        if d.starts_with("#[serde(") || d.starts_with("#[schemars(") || d.starts_with("#[clap(") {
            return true;
        }
        let Some(inner) = d.strip_prefix("#[derive(").and_then(|s| s.strip_suffix(")]")) else {
            return false;
        };
        inner.split(',').any(|trait_path| {
            let tail = trait_path.trim().rsplit("::").next().unwrap_or("").trim();
            WIRE_CLI_DERIVES.contains(&tail)
        })
    })
}

fn is_ident_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn split_top_level_comma_args(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (idx, ch) in s.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => { out.push(s[start..idx].trim()); start = idx + 1; }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() { out.push(tail); }
    out
}

fn eval_cfg_is_test_only(expr: &str) -> bool {
    let e = expr.trim();
    if e == "test" { return true; }
    if let Some(inner) = e.strip_prefix("all").map(str::trim).and_then(|r| r.strip_prefix('(')).and_then(|r| r.strip_suffix(')')) {
        return split_top_level_comma_args(inner).into_iter().any(eval_cfg_is_test_only);
    }
    if let Some(inner) = e.strip_prefix("any").map(str::trim).and_then(|r| r.strip_prefix('(')).and_then(|r| r.strip_suffix(')')) {
        let args = split_top_level_comma_args(inner);
        return args.iter().any(|a| a.trim() == "test") || (!args.is_empty() && args.into_iter().all(eval_cfg_is_test_only));
    }
    false
}

fn is_test_only_cfg_attr(attr_code: &str) -> bool {
    let compact: String = attr_code.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(body) = compact.strip_prefix("#[").and_then(|s| s.strip_suffix(']')) else { return false; };
    let Some(inner) = body.strip_prefix("cfg(").and_then(|s| s.strip_suffix(')')) else { return false; };
    eval_cfg_is_test_only(inner)
}

fn bracket_delta(code_line: &str) -> i32 {
    code_line.bytes().fold(0i32, |acc, b| match b { b'[' => acc + 1, b']' => acc - 1, _ => acc })
}

fn update_cfg_test_block_state(code_line: &str, pending_cfg_test: &mut bool, in_cfg_test_block: &mut bool, brace_depth: &mut i32) {
    for b in code_line.bytes() {
        if b == b'{' {
            *brace_depth += 1;
            *in_cfg_test_block = true;
            *pending_cfg_test = false;
        } else if b == b'}' && *in_cfg_test_block {
            *brace_depth -= 1;
            if *brace_depth <= 0 { *in_cfg_test_block = false; *brace_depth = 0; }
        } else if b == b';' && *pending_cfg_test && !*in_cfg_test_block {
            *pending_cfg_test = false;
        }
    }
}

fn strip_test_context_lines(source_text: &str, file_nodes: Option<&Vec<&DeadTailsNode>>) -> String {
    let ranges: Vec<(i64, i64)> = file_nodes.into_iter().flatten()
        .filter(|n| n.is_test_ctx && n.line > 0 && n.end_line >= n.line).map(|n| (n.line, n.end_line)).collect();
    let mut kept = String::with_capacity(source_text.len());
    let code_only = strip_comments_and_strings_preserving_lines(source_text);
    let (mut collecting_cfg_attr, mut attr_buf, mut attr_depth) = (false, String::new(), 0i32);
    let (mut pending_cfg_test, mut brace_depth, mut in_cfg_test_block) = (false, 0i32, false);

    for (idx, (line, code_line)) in source_text.lines().zip(code_only.lines()).enumerate() {
        let (line_no, trimmed) = ((idx + 1) as i64, code_line.trim());
        if collecting_cfg_attr {
            attr_buf.push(' ');
            attr_buf.push_str(trimmed);
            attr_depth += bracket_delta(trimmed);
            if attr_depth <= 0 {
                collecting_cfg_attr = false;
                if is_test_only_cfg_attr(&attr_buf) { pending_cfg_test = true; }
                else { kept.push_str(&attr_buf); kept.push('\n'); }
                attr_buf.clear();
            }
            continue;
        }
        if pending_cfg_test || in_cfg_test_block {
            update_cfg_test_block_state(code_line, &mut pending_cfg_test, &mut in_cfg_test_block, &mut brace_depth);
            continue;
        }
        if trimmed.starts_with("#[") && trimmed[2..].trim_start().starts_with("cfg") {
            let delta = bracket_delta(trimmed);
            if delta > 0 { collecting_cfg_attr = true; attr_depth = delta; attr_buf = trimmed.to_owned(); continue; }
            if is_test_only_cfg_attr(trimmed) { pending_cfg_test = true; continue; }
        }
        if ranges.iter().any(|&(s, e)| line_no >= s && line_no <= e) { continue; }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn parse_use_tree(tree: &str, prefix: &[String], is_pub_top_level: bool, line: usize, out: &mut Vec<ParsedUse>) {
    let t = tree.trim();
    if t.is_empty() { return; }
    if let (Some(open_idx), Some(close_idx)) = (t.find('{'), t.rfind('}')) {
        if open_idx < close_idx {
            let head = t[..open_idx].trim().trim_end_matches("::").trim();
            let mut next_prefix = prefix.to_vec();
            next_prefix.extend(head.split("::").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned));
            for part in split_top_level_comma_args(&t[open_idx + 1..close_idx]) {
                parse_use_tree(part, &next_prefix, is_pub_top_level, line, out);
            }
            return;
        }
    }
    let (path_part, alias_opt) = t.split_once(" as ").map_or((t, None), |(p, a)| (p.trim(), Some(a.trim())));
    let mut full = prefix.to_vec();
    full.extend(path_part.split("::").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned));
    let Some(item) = full.pop() else { return; };
    let alias = match alias_opt {
        Some(a) => a.to_owned(),
        None if item == "self" => full.last().cloned().unwrap_or_default(),
        None => item.clone(),
    };
    out.push(ParsedUse { prefix: full, item, alias, is_pub_top_level, line });
}

fn strip_use_prefix(trimmed: &str) -> Option<(&str, bool)> {
    if let Some(rest) = trimmed.strip_prefix("use ") {
        return Some((rest, false));
    }
    if let Some(rest) = trimmed.strip_prefix("pub use ") {
        return Some((rest, true));
    }
    if let Some(after_pub) = trimmed.strip_prefix("pub(") {
        if let Some((_, after_paren)) = after_pub.split_once(')') {
            if let Some(rest) = after_paren.trim_start().strip_prefix("use ") {
                return Some((rest, false));
            }
        }
    }
    None
}

fn extract_parsed_uses_and_non_use_code(code_only: &str) -> (Vec<ParsedUse>, String) {
    let mut uses = Vec::new();
    let mut non_use = String::with_capacity(code_only.len());
    let mut brace_depth: i32 = 0;
    let mut collecting = false;
    let mut collect_pub_top = false;
    let mut collect_start_line = 1usize;
    let mut buf = String::new();

    for (idx, line) in code_only.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        if !collecting {
            if let Some((rest, is_pub)) = strip_use_prefix(trimmed) {
                collecting = true;
                collect_pub_top = is_pub && brace_depth == 0;
                collect_start_line = line_no;
                buf.clear();
                buf.push_str(rest);
            }
        } else {
            buf.push(' ');
            buf.push_str(trimmed);
        }
        if collecting {
            if trimmed.ends_with(';') {
                collecting = false;
                let tree = buf.trim().trim_end_matches(';');
                parse_use_tree(tree, &[], collect_pub_top, collect_start_line, &mut uses);
            }
            continue;
        }
        for b in line.bytes() {
            if b == b'{' {
                brace_depth += 1;
            } else if b == b'}' {
                brace_depth = (brace_depth - 1).max(0);
            }
        }
        non_use.push_str(line);
        non_use.push('\n');
    }
    (uses, non_use)
}

fn resolve_mod_prefix(
    prefix: &[String],
    owner: &OwnerContext,
    known_crate_names: &HashSet<String>,
) -> Option<ModLocation> {
    if prefix.is_empty() {
        return Some(ModLocation {
            crate_name: owner.crate_name.clone(),
            segments: owner.segments.clone(),
        });
    }
    match prefix[0].as_str() {
        "crate" => (!owner.is_bin).then(|| ModLocation {
            crate_name: owner.crate_name.clone(),
            segments: prefix[1..].to_vec(),
        }),
        "self" => {
            let mut segs = owner.segments.clone();
            segs.extend_from_slice(&prefix[1..]);
            Some(ModLocation {
                crate_name: owner.crate_name.clone(),
                segments: segs,
            })
        }
        "super" => {
            let mut segs = owner.segments.clone();
            let mut idx = 0usize;
            while idx < prefix.len() && prefix[idx] == "super" {
                segs.pop()?;
                idx += 1;
            }
            segs.extend_from_slice(&prefix[idx..]);
            Some(ModLocation {
                crate_name: owner.crate_name.clone(),
                segments: segs,
            })
        }
        first => {
            if known_crate_names.contains(first) {
                Some(ModLocation {
                    crate_name: first.to_owned(),
                    segments: prefix[1..].to_vec(),
                })
            } else {
                let mut segs = owner.segments.clone();
                segs.extend_from_slice(prefix);
                Some(ModLocation {
                    crate_name: owner.crate_name.clone(),
                    segments: segs,
                })
            }
        }
    }
}

fn read_colon_chain(src: &str, mut cur: usize, first: &str, last_pos: usize) -> (Vec<String>, usize) {
    let bytes = src.as_bytes();
    let mut chain = vec![first.to_owned()];
    let mut end_pos = last_pos;
    loop {
        while cur < bytes.len() && bytes[cur].is_ascii_whitespace() {
            cur += 1;
        }
        let seg_start = cur;
        if seg_start >= bytes.len() || !is_ident_char(bytes[seg_start]) || bytes[seg_start].is_ascii_digit() {
            break;
        }
        while cur < bytes.len() && is_ident_char(bytes[cur]) {
            cur += 1;
        }
        chain.push(src[seg_start..cur].to_owned());
        end_pos = cur;
        while cur < bytes.len() && bytes[cur].is_ascii_whitespace() {
            cur += 1;
        }
        if bytes.get(cur..cur + 2) != Some(b"::") {
            break;
        }
        cur += 2;
    }
    (chain, end_pos)
}

fn scan_code_chains_and_idents(non_use_code: &str) -> (Vec<Vec<String>>, HashSet<String>) {
    const DECL_KEYWORDS: &[&str] = &["fn", "struct", "enum", "trait", "type", "const", "static", "mod"];
    let bytes = non_use_code.as_bytes();
    let mut chains = Vec::new();
    let mut bare_idents = HashSet::new();
    let mut i = 0usize;
    let mut prev_was_dot = false;
    let mut prev_was_decl_kw = false;

    while i < bytes.len() {
        let b = bytes[i];
        if !is_ident_char(b) || b.is_ascii_digit() {
            if !b.is_ascii_whitespace() {
                prev_was_dot = b == b'.';
                prev_was_decl_kw = false;
            }
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let first = &non_use_code[start..i];
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j..j + 2) == Some(b"::") {
            let (chain, end_pos) = read_colon_chain(non_use_code, j + 2, first, i);
            i = end_pos;
            if chain.len() > 1 {
                chains.push(chain);
            } else if !prev_was_dot && !prev_was_decl_kw {
                bare_idents.insert(first.to_owned());
            }
            prev_was_dot = false;
            prev_was_decl_kw = false;
        } else {
            if !prev_was_dot && !prev_was_decl_kw {
                bare_idents.insert(first.to_owned());
            }
            prev_was_dot = false;
            prev_was_decl_kw = DECL_KEYWORDS.contains(&first);
        }
    }
    (chains, bare_idents)
}

fn build_non_test_rust_corpus(conn: &Connection, workspace_root: Option<&std::path::Path>, nodes_by_path: &HashMap<&str, Vec<&DeadTailsNode>>, crate_layout: &CrateLayout) -> Result<NonTestRustCorpus> {
    let (mut files, mut ident_files, mut mod_importers) = (HashMap::new(), HashMap::<String, Vec<String>>::new(), HashMap::<ModLocation, Vec<String>>::new());
    let Some(root) = workspace_root else {
        return Ok(NonTestRustCorpus { files, ident_files, mod_importers });
    };
    let mut cache = source::VerifiedSourceCache::new();
    let mut stmt = conn.prepare("SELECT path FROM files WHERE is_test = 0 AND language = 'rust'")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if is_excluded_caller_path(&path) { continue; }
        let Ok(raw) = cache.read(conn, root, &path) else { continue; };
        let stripped = strip_test_context_lines(&raw, nodes_by_path.get(path.as_str()));
        let code_only = strip_comments_and_strings_preserving_lines(&stripped);
        let owner = crate_layout.owner_context(&path);
        let (parsed_uses, non_use_code) = extract_parsed_uses_and_non_use_code(&code_only);
        let (mut resolved_uses, mut pub_top_uses, mut seen_mods) = (Vec::new(), Vec::new(), HashSet::new());
        for u in parsed_uses {
            if u.is_pub_top_level {
                pub_top_uses.push(ParsedUse { prefix: u.prefix.clone(), item: u.item.clone(), alias: u.alias.clone(), is_pub_top_level: true, line: u.line });
            }
            if let Some(mod_loc) = resolve_mod_prefix(&u.prefix, &owner, &crate_layout.known_crate_names) {
                if seen_mods.insert(mod_loc.clone()) {
                    mod_importers.entry(mod_loc.clone()).or_default().push(path.clone());
                }
                resolved_uses.push(ResolvedUse { mod_loc, item: u.item, alias: u.alias });
            }
        }
        let (path_chains, bare_idents) = scan_code_chains_and_idents(&non_use_code);
        let mut all_idents: HashSet<String> = bare_idents.clone();
        all_idents.extend(path_chains.iter().flatten().cloned());
        all_idents.extend(resolved_uses.iter().map(|u| u.item.clone()));
        for id in all_idents { ident_files.entry(id).or_default().push(path.clone()); }
        files.insert(path, CorpusFile { owner, code_only, resolved_uses, path_chains, bare_idents, pub_top_uses });
    }
    Ok(NonTestRustCorpus { files, ident_files, mod_importers })
}

fn collect_prod_cross_file_edges<'a>(conn: &Connection, nodes: &'a HashMap<i64, DeadTailsNode>) -> Result<ProdCrossFileEdges<'a>> {
    let (mut hashes, mut qualnames) = (HashSet::new(), HashSet::new());
    let mut stmt = conn.prepare(&format!(
        "SELECT e.src_hash, e.dst_hash FROM edges e WHERE e.kind IN ('{}', '{}', '{}', '{}')",
        EdgeKind::Calls.as_str(), EdgeKind::References.as_str(), EdgeKind::PossibleCalls.as_str(), EdgeKind::Implements.as_str(),
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (src_hash, dst_hash): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (Some(src), Some(dst)) = (nodes.get(&src_hash), nodes.get(&dst_hash)) else { continue; };
        if src.is_test_ctx || is_excluded_caller_path(&src.path) || src.path == dst.path { continue; }
        hashes.insert(dst_hash);
        qualnames.insert(dst.qualname.as_str());
    }
    Ok(ProdCrossFileEdges { hashes, qualnames })
}

fn collect_non_test_dependency_refs(conn: &Connection, nodes_by_path: &HashMap<&str, Vec<&DeadTailsNode>>, non_test_corpus: &NonTestRustCorpus) -> Result<HashMap<String, HashSet<String>>> {
    let mut deps_by_path: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT p.path, d.symbol FROM dependencies d JOIN path_dictionary p ON p.path_id = d.owner_id LEFT JOIN files f ON f.path = p.path WHERE COALESCE(f.is_test, 0) = 0 AND d.kind IN ('imports', 'calls', 'references', 'unresolved') AND d.symbol != ''")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        if is_excluded_caller_path(&path) || non_test_corpus.files.contains_key(&path) { continue; }
        let symbol: String = row.get(1)?;
        if symbol.contains('\n') || symbol.contains('(') { continue; }
        if !nodes_by_path.get(path.as_str()).is_some_and(|ns| ns.iter().any(|n| n.is_test_ctx)) {
            deps_by_path.entry(path).or_default().insert(symbol);
        }
    }
    Ok(deps_by_path)
}

fn dependency_matches_location(symbol: &str, loc: &ModLocation, export_name: &str, parent_type: Option<&str>, same_crate_lib: bool) -> bool {
    let seg_prefix = if loc.segments.is_empty() { String::new() } else { format!("{}::", loc.segments.join("::")) };
    let mut exact = vec![format!("{}::{seg_prefix}", loc.crate_name)];
    if same_crate_lib { exact.push(format!("crate::{seg_prefix}")); }
    exact.iter().any(|prefix| {
        symbol == format!("{prefix}{export_name}") || parent_type.is_some_and(|pt| symbol == format!("{prefix}{pt}::{export_name}"))
    })
}

fn chain_resolves_to_location(
    prefix_segs: &[String],
    file: &CorpusFile,
    loc: &ModLocation,
    crate_layout: &CrateLayout,
) -> bool {
    if prefix_segs.is_empty() {
        return false;
    }
    if resolve_mod_prefix(prefix_segs, &file.owner, &crate_layout.known_crate_names)
        .as_ref()
        == Some(loc)
    {
        return true;
    }
    // Resolve module alias imports like `use crate::foo::bar as b; b::baz()` or `use crate::foo; foo::bar::baz()`.
    for u in &file.resolved_uses {
        if u.alias != prefix_segs[0] || u.item == "*" {
            continue;
        }
        let mut combined = u.mod_loc.clone();
        if u.item != "self" {
            combined.segments.push(u.item.clone());
        }
        combined.segments.extend_from_slice(&prefix_segs[1..]);
        if &combined == loc {
            return true;
        }
    }
    false
}

fn file_uses_location_item(file: &CorpusFile, loc: &ModLocation, export_name: &str, parent_type: Option<&str>, crate_layout: &CrateLayout) -> bool {
    for u in &file.resolved_uses {
        if &u.mod_loc != loc { continue; }
        if u.item == export_name || (u.item == "*" && file.bare_idents.contains(export_name)) {
            return true;
        }
        if let Some(pt) = parent_type {
            let has_chain = file.path_chains.iter().any(|c| {
                c.len() >= 2 && c[1] == export_name && ((u.item == pt && (c[0] == pt || c[0] == u.alias)) || (u.item == "*" && c[0] == pt))
            });
            if has_chain { return true; }
        }
    }
    for chain in &file.path_chains {
        if chain.len() < 2 { continue; }
        if (1..chain.len()).any(|i| chain[i] == export_name && chain_resolves_to_location(&chain[..i], file, loc, crate_layout)) {
            return true;
        }
        if let Some(pt) = parent_type {
            if chain.len() >= 3
                && chain[chain.len() - 2] == pt
                && chain.last().map(String::as_str) == Some(export_name)
                && chain_resolves_to_location(&chain[..chain.len() - 2], file, loc, crate_layout)
            {
                return true;
            }
        }
    }
    false
}

fn collect_rust_public_items(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    enum_qualnames: &HashSet<&str>,
    crate_layout: &CrateLayout,
) -> Result<Vec<PublicApiItem>> {
    let mut public_items = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT n.node_hash, n.kind, n.name, n.qualname, p.path, n.line, n.details
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.language = 'rust'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let hash: i64 = row.get(0)?;
        let kind: String = row.get(1)?;
        let name: String = row.get(2)?;
        let qualname: String = row.get(3)?;
        let path: String = row.get(4)?;
        let line: i64 = row.get(5)?;
        let details_raw: String = row.get(6)?;
        let Some(node) = nodes.get(&hash) else {
            continue;
        };
        if node.is_test_ctx
            || node.generated
            || is_excluded_public_api_path(&path, &crate_layout.build_script_files)
            || kind == NodeKind::Module.as_str()
        {
            continue;
        }
        let Ok(details) = serde_json::from_str::<serde_json::Value>(&details_raw) else {
            continue;
        };
        if should_track_public_item(node, &kind, &name, &qualname, &details, enum_qualnames) {
            let (crate_name, segments) = crate_layout.module_segments(&path);
            let parent_type = qualname
                .rsplit_once('.')
                .and_then(|(prefix, _)| prefix.rsplit('.').next())
                .filter(|seg| seg.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
                .map(str::to_owned);
            public_items.push(PublicApiItem {
                hash, kind, name, qualname, path, line: line.max(1) as usize,
                loc: ModLocation { crate_name, segments }, parent_type,
            });
        }
    }
    Ok(public_items)
}

fn should_track_public_item(node: &DeadTailsNode, kind: &str, name: &str, qualname: &str, details: &serde_json::Value, enum_qualnames: &HashSet<&str>) -> bool {
    let is_public = details.get("is_public").and_then(|v| v.as_bool()).unwrap_or(false);
    if !is_public || !matches!(kind, "function" | "type" | "trait" | "constant" | "struct" | "enum") { return false; }
    if node.has_entrypoint || name == "main" || node.has_tool_decorator || node.has_impl_trait || node.has_trait_default || has_derive_or_serde_decorator(details) {
        return false;
    }
    !(kind == "constant" && qualname.rsplit_once('.').is_some_and(|(parent_qn, _)| enum_qualnames.contains(parent_qn)))
}

fn location_used_in_corpus_or_deps(loc: &ModLocation, export_name: &str, parent_type: Option<&str>, exclude_paths: &[&str], deps_by_path: &HashMap<String, HashSet<String>>, non_test_corpus: &NonTestRustCorpus, crate_layout: &CrateLayout) -> bool {
    let mut candidate_files: HashSet<&str> = HashSet::new();
    for key in [export_name, "*"] {
        if let Some(list) = non_test_corpus.ident_files.get(key) {
            candidate_files.extend(list.iter().map(String::as_str));
        }
    }
    if let Some(list) = non_test_corpus.mod_importers.get(loc) {
        candidate_files.extend(list.iter().map(String::as_str));
    }
    if candidate_files.into_iter().filter(|p| !exclude_paths.contains(p)).any(|path| {
        non_test_corpus.files.get(path).is_some_and(|f| file_uses_location_item(f, loc, export_name, parent_type, crate_layout))
    }) {
        return true;
    }
    deps_by_path.iter().filter(|(p, _)| !exclude_paths.contains(&p.as_str())).any(|(owner_path, symbols)| {
        let owner = crate_layout.owner_context(owner_path);
        let same_crate_lib = !owner.is_bin && owner.crate_name == loc.crate_name;
        symbols.iter().any(|sym| dependency_matches_location(sym, loc, export_name, parent_type, same_crate_lib))
    })
}

fn collect_pub_reexports(
    non_test_corpus: &NonTestRustCorpus,
    crate_layout: &CrateLayout,
) -> (Vec<NamedReexport>, Vec<GlobReexport>) {
    let mut named = Vec::new();
    let mut globs = Vec::new();
    for (mod_path, file) in &non_test_corpus.files {
        if is_excluded_public_api_path(mod_path, &crate_layout.build_script_files) {
            continue;
        }
        for u in &file.pub_top_uses {
            let Some(target_loc) =
                resolve_mod_prefix(&u.prefix, &file.owner, &crate_layout.known_crate_names)
            else {
                continue;
            };
            let mod_loc = ModLocation {
                crate_name: file.owner.crate_name.clone(),
                segments: file.owner.segments.clone(),
            };
            if u.item == "*" {
                globs.push(GlobReexport {
                    mod_loc,
                    mod_path: mod_path.clone(),
                    line: u.line,
                    target_loc,
                });
            } else {
                named.push(NamedReexport {
                    mod_loc,
                    mod_path: mod_path.clone(),
                    line: u.line,
                    target_loc,
                    target_item: u.item.clone(),
                    exposed_name: u.alias.clone(),
                });
            }
        }
    }
    (named, globs)
}

struct ExposurePath {
    loc: ModLocation, name: String, parent_type: Option<String>,
    via_named: Vec<usize>, via_globs: Vec<usize>, exclude_paths: Vec<String>,
}

fn push_exposure(
    out: &mut Vec<ExposurePath>,
    visited: &mut HashSet<(ModLocation, String, Option<String>)>,
    next: ExposurePath,
) {
    if visited.insert((next.loc.clone(), next.name.clone(), next.parent_type.clone())) {
        out.push(next);
    }
}

fn expand_item_exposures(
    item: &PublicApiItem,
    named_by_target: &HashMap<(&ModLocation, &str), Vec<usize>>,
    named_reexports: &[NamedReexport],
    globs_by_target: &HashMap<&ModLocation, Vec<usize>>,
    glob_reexports: &[GlobReexport],
) -> Vec<ExposurePath> {
    let mut out = vec![ExposurePath {
        loc: item.loc.clone(), name: item.name.clone(), parent_type: item.parent_type.clone(),
        via_named: Vec::new(), via_globs: Vec::new(), exclude_paths: vec![item.path.clone()],
    }];
    let mut visited = HashSet::from([(item.loc.clone(), item.name.clone(), item.parent_type.clone())]);
    let mut idx = 0usize;
    while idx < out.len() && out.len() < 64 {
        let (cur_loc, cur_name, cur_pt) = (out[idx].loc.clone(), out[idx].name.clone(), out[idx].parent_type.clone());
        let (cur_named, cur_globs, cur_excl) = (out[idx].via_named.clone(), out[idx].via_globs.clone(), out[idx].exclude_paths.clone());

        for (lookup, is_parent) in [(Some(cur_name.as_str()), false), (cur_pt.as_deref(), true)] {
            if let Some(n_idxs) = lookup.and_then(|k| named_by_target.get(&(&cur_loc, k))) {
                for &n_idx in n_idxs {
                    let nr = &named_reexports[n_idx];
                    let (mut via_named, mut exclude_paths) = (cur_named.clone(), cur_excl.clone());
                    via_named.push(n_idx);
                    exclude_paths.push(nr.mod_path.clone());
                    let (name, parent_type) = if is_parent {
                        (cur_name.clone(), Some(nr.exposed_name.clone()))
                    } else {
                        (nr.exposed_name.clone(), cur_pt.clone())
                    };
                    push_exposure(&mut out, &mut visited, ExposurePath { loc: nr.mod_loc.clone(), name, parent_type, via_named, via_globs: cur_globs.clone(), exclude_paths });
                }
            }
        }
        if let Some(g_idxs) = globs_by_target.get(&cur_loc) {
            for &g_idx in g_idxs {
                let gr = &glob_reexports[g_idx];
                let (mut via_globs, mut exclude_paths) = (cur_globs.clone(), cur_excl.clone());
                via_globs.push(g_idx);
                exclude_paths.push(gr.mod_path.clone());
                push_exposure(&mut out, &mut visited, ExposurePath { loc: gr.mod_loc.clone(), name: cur_name.clone(), parent_type: cur_pt.clone(), via_named: cur_named.clone(), via_globs, exclude_paths });
            }
        }
        idx += 1;
    }
    out
}

fn check_external_glob_used(
    gr: &GlobReexport,
    non_test_corpus: &NonTestRustCorpus,
    crate_layout: &CrateLayout,
) -> bool {
    let mod_tail = gr.mod_loc.segments.last().map(String::as_str).unwrap_or("");
    non_test_corpus.files.iter().any(|(path, file)| {
        if path == &gr.mod_path {
            return false;
        }
        file.resolved_uses.iter().any(|u| {
            u.mod_loc == gr.mod_loc || (u.item == mod_tail && {
                let mut joined = u.mod_loc.clone();
                joined.segments.push(u.item.clone());
                joined == gr.mod_loc
            })
        }) || file.path_chains.iter().any(|chain| {
            chain.len() >= 2
                && chain_resolves_to_location(&chain[..chain.len() - 1], file, &gr.mod_loc, crate_layout)
        })
    })
}

fn collect_brace_or_sig(lines: &[&str], start: usize, is_type_def: bool, out: &mut String) {
    let mut brace_depth: i32 = 0;
    let mut saw_brace = false;
    for line in lines.iter().skip(start).take(32) {
        out.push_str(line);
        out.push('\n');
        for b in line.bytes() {
            if b == b'{' {
                if !is_type_def {
                    return;
                }
                brace_depth += 1;
                saw_brace = true;
            } else if b == b'}' && is_type_def {
                brace_depth -= 1;
                if saw_brace && brace_depth <= 0 {
                    return;
                }
            } else if b == b';' && !saw_brace {
                return;
            }
        }
    }
}

fn append_impl_pub_signatures(lines: &[&str], type_name: &str, out: &mut String) {
    let mut in_impl = false;
    let mut depth: i32 = 0;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !in_impl
            && trimmed.starts_with("impl")
            && contains_ident_token(trimmed, type_name)
            && !trimmed.contains(" for ")
        {
            in_impl = true;
            depth = 0;
        }
        if in_impl {
            if trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub const fn ")
            {
                collect_brace_or_sig(lines, idx, false, out);
            }
            for b in line.bytes() {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth <= 0 {
                        in_impl = false;
                        break;
                    }
                }
            }
        }
    }
}

fn contains_ident_token(haystack: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut start = 0usize;
    while let Some(offset) = haystack[start..].find(token) {
        let idx = start + offset;
        let end = idx + token.len();
        let left_ok = idx == 0 || !is_ident_char(bytes[idx - 1]);
        let right_ok = end == bytes.len() || !is_ident_char(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        start = idx + token.len().max(1);
    }
    false
}

fn extract_item_interface_slice(code_only: &str, name: &str, kind: &str) -> String {
    let lines: Vec<&str> = code_only.lines().collect();
    let Some(start) = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("pub ") && contains_ident_token(trimmed, name)
    }) else {
        return String::new();
    };
    let is_type_def = matches!(kind, "struct" | "enum" | "trait");
    let mut out = String::new();
    collect_brace_or_sig(&lines, start, is_type_def, &mut out);
    if matches!(kind, "struct" | "enum") {
        append_impl_pub_signatures(&lines, name, &mut out);
    }
    out
}

fn add_interface_slice<'a>(item: &'a PublicApiItem, non_test_corpus: &NonTestRustCorpus, map: &mut HashMap<&'a str, Vec<String>>) {
    if let Some(file) = non_test_corpus.files.get(&item.path) {
        let slice = extract_item_interface_slice(&file.code_only, &item.name, &item.kind);
        if !slice.is_empty() {
            map.entry(item.path.as_str()).or_default().push(slice);
        }
    }
}

fn mark_signature_reachable_types(items: &[PublicApiItem], non_test_corpus: &NonTestRustCorpus, is_used: &mut [bool]) {
    let mut used_headers_by_path: HashMap<&str, Vec<String>> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        if is_used[idx] { add_interface_slice(item, non_test_corpus, &mut used_headers_by_path); }
    }
    loop {
        let mut changed = false;
        for (idx, item) in items.iter().enumerate() {
            if is_used[idx] || !matches!(item.kind.as_str(), "struct" | "enum" | "type" | "trait") { continue; }
            if used_headers_by_path.get(item.path.as_str()).is_some_and(|hs| hs.iter().any(|h| contains_ident_token(h, &item.name))) {
                is_used[idx] = true;
                changed = true;
                add_interface_slice(item, non_test_corpus, &mut used_headers_by_path);
            }
        }
        if !changed { break; }
    }
}

fn push_unused_public_api_violation(
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

fn format_mod_label(loc: &ModLocation) -> String {
    if loc.segments.is_empty() {
        loc.crate_name.clone()
    } else {
        format!("{}::{}", loc.crate_name, loc.segments.join("::"))
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_reexport_violations(
    scope: &LintScope,
    named_reexports: &[NamedReexport],
    glob_reexports: &[GlobReexport],
    named_used: &mut [bool],
    glob_used: &mut [bool],
    indexed_locs: &HashSet<&ModLocation>,
    deps_by_path: &HashMap<String, HashSet<String>>,
    non_test_corpus: &NonTestRustCorpus,
    crate_layout: &CrateLayout,
    violations: &mut Vec<LintViolation>,
) {
    for (idx, nr) in named_reexports.iter().enumerate() {
        if !named_used[idx]
            && location_used_in_corpus_or_deps(
                &nr.mod_loc,
                &nr.exposed_name,
                None,
                &[nr.mod_path.as_str()],
                deps_by_path,
                non_test_corpus,
                crate_layout,
            )
        {
            named_used[idx] = true;
        }
        if !named_used[idx] && scope.in_scope(&nr.mod_path) {
            let mod_label = format_mod_label(&nr.mod_loc);
            push_unused_public_api_violation(
                violations,
                nr.mod_path.clone(),
                nr.line,
                format!("unused or test-only `pub use` re-export '{}' in '{}'", nr.exposed_name, mod_label),
                format!("{mod_label}.{}", nr.exposed_name),
                "Remove unused `pub use` or narrow visibility to `pub(crate)` / `#[cfg(test)]`.",
            );
        }
    }

    for (idx, gr) in glob_reexports.iter().enumerate() {
        if !glob_used[idx]
            && !indexed_locs.contains(&gr.target_loc)
            && check_external_glob_used(gr, non_test_corpus, crate_layout)
        {
            glob_used[idx] = true;
        }
        if !glob_used[idx] && scope.in_scope(&gr.mod_path) {
            let mod_label = format_mod_label(&gr.mod_loc);
            let target_label = format_mod_label(&gr.target_loc);
            push_unused_public_api_violation(
                violations,
                gr.mod_path.clone(),
                gr.line,
                format!("unused or test-only glob `pub use {target_label}::*` in '{mod_label}'"),
                format!("{mod_label}.*"),
                "Remove unused glob `pub use` or narrow visibility to `pub(crate)` / `#[cfg(test)]`.",
            );
        }
    }
}

fn collect_unused_public_api_violations(conn: &Connection, scope: &LintScope, workspace_root: Option<&std::path::Path>, nodes: &HashMap<i64, DeadTailsNode>, violations: &mut Vec<LintViolation>) -> Result<()> {
    let (mut nodes_by_path, mut enum_qualnames) = (HashMap::<&str, Vec<&DeadTailsNode>>::new(), HashSet::<&str>::new());
    for node in nodes.values() {
        nodes_by_path.entry(node.path.as_str()).or_default().push(node);
        if node.kind == NodeKind::Enum.as_str() { enum_qualnames.insert(node.qualname.as_str()); }
    }
    let crate_layout = CrateLayout::discover(conn, workspace_root, nodes)?;
    let prod_edges = collect_prod_cross_file_edges(conn, nodes)?;
    let non_test_corpus = build_non_test_rust_corpus(conn, workspace_root, &nodes_by_path, &crate_layout)?;
    let deps_by_path = collect_non_test_dependency_refs(conn, &nodes_by_path, &non_test_corpus)?;
    let public_items = collect_rust_public_items(conn, nodes, &enum_qualnames, &crate_layout)?;
    let (named_reexports, glob_reexports) = collect_pub_reexports(&non_test_corpus, &crate_layout);

    let mut named_by_target: HashMap<(&ModLocation, &str), Vec<usize>> = HashMap::new();
    for (idx, nr) in named_reexports.iter().enumerate() {
        named_by_target.entry((&nr.target_loc, nr.target_item.as_str())).or_default().push(idx);
    }
    let mut globs_by_target: HashMap<&ModLocation, Vec<usize>> = HashMap::new();
    for (idx, gr) in glob_reexports.iter().enumerate() {
        globs_by_target.entry(&gr.target_loc).or_default().push(idx);
    }
    let (mut named_used, mut glob_used, mut is_used) = (vec![false; named_reexports.len()], vec![false; glob_reexports.len()], vec![false; public_items.len()]);

    for (idx, item) in public_items.iter().enumerate() {
        if prod_edges.hashes.contains(&item.hash) || prod_edges.qualnames.contains(item.qualname.as_str()) { is_used[idx] = true; }
        for exp in expand_item_exposures(item, &named_by_target, &named_reexports, &globs_by_target, &glob_reexports) {
            let excl: Vec<&str> = exp.exclude_paths.iter().map(String::as_str).collect();
            if location_used_in_corpus_or_deps(&exp.loc, &exp.name, exp.parent_type.as_deref(), &excl, &deps_by_path, &non_test_corpus, &crate_layout) {
                is_used[idx] = true;
                for n_idx in exp.via_named { named_used[n_idx] = true; }
                for g_idx in exp.via_globs { glob_used[g_idx] = true; }
            }
        }
    }
    mark_signature_reachable_types(&public_items, &non_test_corpus, &mut is_used);
    let indexed_locs: HashSet<&ModLocation> = public_items.iter().map(|i| &i.loc).collect();
    collect_reexport_violations(scope, &named_reexports, &glob_reexports, &mut named_used, &mut glob_used, &indexed_locs, &deps_by_path, &non_test_corpus, &crate_layout, violations);

    for (idx, item) in public_items.into_iter().enumerate() {
        if is_used[idx] || !scope.in_scope(&item.path) { continue; }
        push_unused_public_api_violation(
            violations, item.path, item.line,
            format!("public {} '{}' lacks non-test cross-module production callers", item.kind, item.qualname),
            item.qualname, "Narrow to private/`pub(crate)`/`#[cfg(test)]` or delete unused compatibility item.",
        );
    }
    Ok(())
}
