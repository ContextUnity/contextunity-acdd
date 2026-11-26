//! Pure read-only module/scope boundary analyzer over stored `code-map.sqlite` facts.

use super::common::{
    is_non_prod_raw, is_symbol_kind, load_nodes, page_slice, path_in_scope, resolve_indexed_scope,
    NodeSummary, NODE_COLS, NON_DEP_EDGES,
};
use crate::db::{
    paging::{self, QueryBudget},
    reader::selectors::path_bounds,
};
use crate::models::tags::{CoverageStatus, EdgeKind};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
};

#[derive(Default)]
struct OutboundGroup {
    edge_count: usize,
    kinds: BTreeSet<String>,
    targets: BTreeSet<String>,
}

pub(super) fn analyze_boundary(
    conn: &Connection,
    target: &str,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    let generation = paging::generation(conn, options)?;
    let scope = resolve_indexed_scope(conn, target)?;
    let (prefix, end) = path_bounds(&scope);
    let params: &[&dyn ToSql] = &[&scope, &prefix, &end];

    let scoped_sql = format!(
        "SELECT {NODE_COLS} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id \
         WHERE (?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3)) \
           AND n.is_test=0 AND n.generated=0 AND n.kind NOT IN('document','invariant')"
    );
    let scoped_nodes = load_nodes(conn, &scoped_sql, params, None, true, budget)?;

    let calls_kind = EdgeKind::Calls.as_str();
    let mut edge_stmt = conn.prepare(&format!(
        "SELECT e.src_hash,src_n.id,src_p.path,src_n.line,src_n.details,e.dst_hash,dst_n.id,dst_p.path,dst_n.details,e.kind \
         FROM edges e JOIN nodes src_n ON src_n.node_hash=e.src_hash JOIN path_dictionary src_p ON src_p.path_id=src_n.path_id \
         JOIN nodes dst_n ON dst_n.node_hash=e.dst_hash JOIN path_dictionary dst_p ON dst_p.path_id=dst_n.path_id \
         WHERE e.kind NOT IN({NON_DEP_EDGES}) AND src_n.is_test=0 AND src_n.generated=0 AND dst_n.is_test=0 AND dst_n.generated=0 \
           AND (?1='' OR src_p.path=?1 OR (src_p.path>=?2 AND src_p.path<?3) OR dst_p.path=?1 OR (dst_p.path>=?2 AND dst_p.path<?3))"
    ))?;
    let mut inbound_callers: Vec<(String, i64, String, String, String)> = Vec::new();
    let (mut externally_called, mut internal_callers) =
        (HashSet::new(), HashMap::<i64, usize>::new());
    let mut outbound_map: BTreeMap<(u8, String, String), OutboundGroup> = BTreeMap::new();
    for row in edge_stmt.query_map(params, |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
        ))
    })? {
        budget.check()?;
        let (src_h, src_id, src_p, src_l, src_d, dst_h, dst_id, dst_p, dst_d, kind) = row?;
        if is_non_prod_raw(&src_p, &src_d) || is_non_prod_raw(&dst_p, &dst_d) {
            continue;
        }
        match (path_in_scope(&src_p, &scope), path_in_scope(&dst_p, &scope)) {
            (false, true) if kind == calls_kind => {
                externally_called.insert(dst_h);
                inbound_callers.push((src_p, src_l, src_id, kind, dst_id));
            }
            (true, false) => {
                let g = outbound_map
                    .entry((0, "internal".into(), dst_p))
                    .or_default();
                g.edge_count += 1;
                g.kinds.insert(kind);
                g.targets.insert(dst_id);
            }
            (true, true) if src_h != dst_h && kind == calls_kind => {
                *internal_callers.entry(dst_h).or_default() += 1;
            }
            _ => {}
        }
    }
    inbound_callers.sort();

    let (ext_res, unres_res, ambig_res) = (
        CoverageStatus::External.as_str(),
        CoverageStatus::Unresolved.as_str(),
        CoverageStatus::Ambiguous.as_str(),
    );
    let mut dep_stmt = conn.prepare(&format!(
        "SELECT d.symbol,d.kind,d.resolution FROM dependencies d JOIN path_dictionary p ON p.path_id=d.owner_id \
         WHERE (?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3)) AND d.resolution IN('{ext_res}','{unres_res}','{ambig_res}')"
    ))?;
    for row in dep_stmt.query_map(params, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        budget.check()?;
        let (symbol, kind, resolution) = row?;
        let bucket = if resolution == ext_res { 1 } else { 2 };
        let g = outbound_map
            .entry((bucket, resolution, symbol))
            .or_default();
        g.edge_count += 1;
        g.kinds.insert(kind);
    }

    let mut outbound: Vec<_> = outbound_map.into_iter().collect();
    let count_bucket = |b: u8| outbound.iter().filter(|((k, _, _), _)| *k == b).count();
    let (out_int, out_ext, out_unres) = (count_bucket(0), count_bucket(1), count_bucket(2));
    outbound.sort_by(|(ka, ga), (kb, gb)| {
        ka.0.cmp(&kb.0)
            .then_with(|| Reverse(ga.edge_count).cmp(&Reverse(gb.edge_count)))
            .then_with(|| (&ka.1, &ka.2).cmp(&(&kb.1, &kb.2)))
    });

    let pub_syms: Vec<&NodeSummary> = scoped_nodes
        .iter()
        .filter(|n| n.is_public && is_symbol_kind(&n.kind))
        .collect();
    let mut internal_pub: Vec<&NodeSummary> = pub_syms
        .iter()
        .copied()
        .filter(|n| !externally_called.contains(&n.hash))
        .collect();
    internal_pub.sort_by(|a, b| (&a.path, a.line, &a.id).cmp(&(&b.path, b.line, &b.id)));

    let inbound_items: Vec<Value> = page_slice(&inbound_callers, options)
        .iter()
        .map(|(p, l, src, kind, dst)| json!({"caller": src, "caller_path": p, "caller_line": l, "edge_kind": kind, "target": dst}))
        .collect();
    let outbound_items: Vec<Value> = page_slice(&outbound, options)
        .iter()
        .map(|((_, label, t), g)| json!({"target": t, "scope": label, "edge_count": g.edge_count, "kinds": g.kinds, "targets": g.targets.iter().take(10).collect::<Vec<_>>()}))
        .collect();
    let pub_items: Vec<Value> = page_slice(&internal_pub, options)
        .iter()
        .map(|n| {
            n.with_fields(vec![
                ("external_caller_count", json!(0)),
                (
                    "internal_caller_count",
                    json!(internal_callers.get(&n.hash).copied().unwrap_or(0)),
                ),
            ])
        })
        .collect();

    Ok(json!({
        "mode": "boundary", "target": scope,
        "summary": {
            "scoped_symbols": scoped_nodes.iter().filter(|n| is_symbol_kind(&n.kind)).count(),
            "public_symbols": pub_syms.len(),
            "inbound_external_callers": inbound_callers.len(),
            "outbound_internal_groups": out_int,
            "outbound_external_groups": out_ext,
            "outbound_unresolved_groups": out_unres,
            "internal_only_public_count": internal_pub.len(),
        },
        "inbound_callers": paging::value(inbound_items, inbound_callers.len(), options, &generation),
        "outbound_dependencies": paging::value(outbound_items, outbound.len(), options, &generation),
        "internal_only_public": paging::value(pub_items, internal_pub.len(), options, &generation),
        "generation": generation,
    }))
}
