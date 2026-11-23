use super::types::{normalize_scope, page_bounds};
use crate::db::{
    lint::{collect_dead_tails, DeadTailsNode},
    paging,
};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

fn dead_tail_node_map(node: &DeadTailsNode, lines: i64) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    map.insert("id".into(), json!(node.id));
    map.insert("name".into(), json!(node.name));
    map.insert("qualname".into(), json!(node.qualname));
    map.insert("kind".into(), json!(node.kind));
    map.insert("path".into(), json!(node.path));
    map.insert("line".into(), json!(node.line));
    map.insert("end_line".into(), json!(node.end_line));
    map.insert("lines".into(), json!(lines));
    map
}

/// Identifies dead tails, phantom-tested production callables, and unresolved internal calls.
pub fn dead_tails_paged(
    conn: &Connection,
    target: Option<&str>,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let scope = normalize_scope(target, path, "target")?;
    let report = collect_dead_tails(conn, &scope)?;

    let (phantom_count, phantom_slice) = page_bounds(&report.phantom_nodes, options);
    let phantom_items: Vec<Value> = phantom_slice
        .iter()
        .map(|&(nh, lines)| {
            let node = &report.nodes[&nh];
            let mut caller_nodes: Vec<&DeadTailsNode> = report
                .test_callers
                .get(&node.hash)
                .map(|hashes| hashes.iter().filter_map(|h| report.nodes.get(h)).collect())
                .unwrap_or_default();
            caller_nodes.sort_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let test_caller_count = caller_nodes.len();
            let callers_preview: Vec<_> = caller_nodes
                .into_iter()
                .take(5)
                .map(|c| {
                    json!({
                        "id": c.id,
                        "name": c.name,
                        "qualname": c.qualname,
                        "path": c.path,
                        "line": c.line,
                    })
                })
                .collect();
            let mut obj = dead_tail_node_map(node, lines);
            obj.insert("test_caller_count".into(), json!(test_caller_count));
            obj.insert("test_callers".into(), json!(callers_preview));
            Value::Object(obj)
        })
        .collect();
    let phantom_page = paging::value(phantom_items, phantom_count, options, &generation);

    let (dead_tails_count, dead_tails_slice) = page_bounds(&report.dead_tail_nodes, options);
    let dead_tail_items: Vec<Value> = dead_tails_slice
        .iter()
        .map(|&(nh, lines)| {
            let node = &report.nodes[&nh];
            let dead_callees_count = report.fwd.get(&node.hash).map_or(0, |callees| {
                callees.iter().filter(|c| report.dead.contains(*c)).count()
            });
            let mut obj = dead_tail_node_map(node, lines);
            obj.insert("dead_callees_count".into(), json!(dead_callees_count));
            Value::Object(obj)
        })
        .collect();
    let dead_tails_page = paging::value(dead_tail_items, dead_tails_count, options, &generation);

    let (dangling_calls_count, dangling_slice) = page_bounds(&report.dangling, options);
    let dangling_items: Vec<Value> = dangling_slice
        .iter()
        .map(|d| {
            json!({
                "path": d.path,
                "symbol": d.symbol,
                "kind": d.kind,
                "resolution": d.resolution,
            })
        })
        .collect();
    let dangling_page = paging::value(dangling_items, dangling_calls_count, options, &generation);

    Ok(json!({
        "operation": "dead_tails",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "generation": generation,
        "summary": {
            "phantom_tested_count": phantom_count,
            "dead_tails_count": dead_tails_count,
            "dangling_calls_count": dangling_calls_count,
        },
        "phantom_tested": phantom_page,
        "dead_tails": dead_tails_page,
        "dangling_calls": dangling_page,
    }))
}
