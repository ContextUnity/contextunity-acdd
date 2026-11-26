//! Reverse entrypoint tracer over stored `nodes.details.entrypoint` and uncalled public roots.

use super::common::{
    fetch_frontier_edges, is_non_prod_raw, is_symbol_kind, load_nodes, load_nodes_by_hashes,
    page_slice, NodeSummary, NODE_COLS,
};
use crate::db::{
    paging::{self, QueryBudget},
    reader::selectors::select_detail_with_path,
};
use crate::models::{details::NodeDetailKey, stable_hash64, tags::EdgeKind};
use acdd_core::response::QueryOptions;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const MAX_ENTRYPOINT_DEPTH: usize = 32;
const MAX_ENTRYPOINT_NODES: usize = 20_000;

type RootEntry = (NodeSummary, bool, String, Vec<String>, Vec<String>);

fn classify_root(node: &NodeSummary, uncalled: bool) -> Option<(bool, String)> {
    if !node.is_prod {
        return None;
    }
    if let Some(cat) = node.entrypoint.as_deref().filter(|c| *c != "test") {
        Some((true, cat.to_owned()))
    } else if uncalled && node.is_public && is_symbol_kind(&node.kind) {
        Some((false, "public_root".to_owned()))
    } else {
        None
    }
}

fn format_roots_response(
    target: Value,
    mut roots: Vec<RootEntry>,
    options: &QueryOptions,
    generation: &str,
) -> Value {
    roots.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.4.len().cmp(&b.4.len()))
            .then_with(|| {
                (&a.2, &a.0.path, a.0.line, &a.0.id).cmp(&(&b.2, &b.0.path, b.0.line, &b.0.id))
            })
    });
    let total = roots.len();
    let explicit = roots.iter().filter(|r| r.1).count();
    let items: Vec<Value> = page_slice(&roots, options)
        .iter()
        .map(|(node, is_explicit, category, call_path, edge_kinds)| {
            let root_kind = if *is_explicit {
                "entrypoint"
            } else {
                "uncalled_public_root"
            };
            node.with_fields(vec![
                ("root_kind", json!(root_kind)),
                ("category", json!(category)),
                ("hops", json!(edge_kinds.len())),
                ("call_path", json!(call_path)),
                ("edge_kinds", json!(edge_kinds)),
            ])
        })
        .collect();
    json!({
        "mode": "entrypoints", "target": target,
        "summary": { "total_roots": total, "explicit_entrypoints": explicit, "uncalled_public_roots": total.saturating_sub(explicit) },
        "roots": paging::value(items, total, options, generation), "generation": generation,
    })
}

pub(super) fn analyze_entrypoints(
    conn: &Connection,
    target: &str,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    let generation = paging::generation(conn, options)?;
    let calls_kind = EdgeKind::Calls.as_str();

    if target.trim().is_empty() {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT e.dst_hash,sp.path,s.details FROM edges e \
         JOIN nodes s ON s.node_hash=e.src_hash JOIN path_dictionary sp ON sp.path_id=s.path_id \
         WHERE e.kind='{calls_kind}' AND e.src_hash!=e.dst_hash AND s.is_test=0 AND s.generated=0"
        ))?;
        let mut called_targets = HashSet::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            budget.check()?;
            let (dst, sp, sd) = row?;
            if !is_non_prod_raw(&sp, &sd) {
                called_targets.insert(dst);
            }
        }
        let sql = format!(
            "SELECT {NODE_COLS} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id \
             WHERE n.is_test=0 AND n.generated=0 AND (\
               (json_extract(n.details,'$.{ep}') IS NOT NULL AND json_extract(n.details,'$.{ep}')<>'test') \
               OR json_extract(n.details,'$.{pk}')=1)",
            ep = NodeDetailKey::ENTRYPOINT,
            pk = NodeDetailKey::IS_PUBLIC,
        );
        let roots = load_nodes(conn, &sql, &[], None, true, budget)?
            .into_iter()
            .filter_map(|node| {
                let (is_explicit, category) =
                    classify_root(&node, !called_targets.contains(&node.hash))?;
                let id = node.id.clone();
                Some((node, is_explicit, category, vec![id], Vec::new()))
            })
            .collect();
        return Ok(format_roots_response(
            json!(""),
            roots,
            options,
            &generation,
        ));
    }

    let selected = select_detail_with_path(conn, target, None, options.detail)?;
    let selected_hash = stable_hash64(
        selected["id"]
            .as_str()
            .context("invalid selected node id")?,
    );
    let mut paths: BTreeMap<i64, (Vec<i64>, Vec<String>)> =
        BTreeMap::from([(selected_hash, (vec![selected_hash], Vec::new()))]);
    let (mut expanded, mut has_verified_caller, mut frontier) =
        (HashSet::new(), HashSet::new(), vec![selected_hash]);

    for depth in 0..=MAX_ENTRYPOINT_DEPTH {
        if frontier.is_empty() {
            break;
        }
        expanded.extend(frontier.iter().copied());
        let mut next_paths: HashMap<i64, (Vec<i64>, Vec<String>)> = HashMap::new();
        for edge in fetch_frontier_edges(conn, &frontier, true, true, budget)? {
            if edge.src == edge.dst {
                continue;
            }
            if edge.kind == calls_kind {
                has_verified_caller.insert(edge.dst);
            }
            if paths.contains_key(&edge.src) {
                continue;
            }
            let Some((tail_n, tail_e)) = paths.get(&edge.dst) else {
                continue;
            };
            let cn: Vec<i64> = std::iter::once(edge.src)
                .chain(tail_n.iter().copied())
                .collect();
            let ce: Vec<String> = std::iter::once(edge.kind)
                .chain(tail_e.iter().cloned())
                .collect();
            next_paths
                .entry(edge.src)
                .and_modify(|e| {
                    if (&cn, &ce) < (&e.0, &e.1) {
                        *e = (cn.clone(), ce.clone());
                    }
                })
                .or_insert((cn, ce));
        }
        if depth == MAX_ENTRYPOINT_DEPTH && !next_paths.is_empty() {
            bail!(
                "reverse entrypoint walk exceeds depth budget ({MAX_ENTRYPOINT_DEPTH}); select a narrower symbol"
            );
        }
        if paths.len().saturating_add(next_paths.len()) > MAX_ENTRYPOINT_NODES {
            bail!("reverse entrypoint walk exceeds node budget; select a narrower symbol");
        }
        frontier = next_paths
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        paths.extend(next_paths);
    }

    let hashes: Vec<i64> = paths.keys().copied().collect();
    let node_map = load_nodes_by_hashes(conn, &hashes, budget)?;
    let roots = paths
        .into_iter()
        .filter_map(|(hash, (chain_hashes, edge_kinds))| {
            let node = node_map.get(&hash)?;
            let (is_explicit, category) = classify_root(
                node,
                expanded.contains(&hash) && !has_verified_caller.contains(&hash),
            )?;
            let call_path = chain_hashes
                .iter()
                .filter_map(|h| node_map.get(h).map(|n| n.id.clone()))
                .collect();
            Some((node.clone(), is_explicit, category, call_path, edge_kinds))
        })
        .collect();
    Ok(format_roots_response(selected, roots, options, &generation))
}
