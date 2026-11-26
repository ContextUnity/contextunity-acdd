//! Bounded shortest and top-K typed edge-chain path tracer over `code-map.sqlite`.

use super::common::{fetch_frontier_edges, load_nodes_by_hashes, GraphEdge, NodeSummary};
use crate::db::{
    paging::{self, QueryBudget},
    reader::selectors::select_detail_with_path,
};
use crate::models::stable_hash64;
use acdd_core::response::QueryOptions;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{hash_map::Entry, BTreeMap, BTreeSet, BinaryHeap, HashMap, VecDeque};

const MAX_PATH_DEPTH: usize = 12;
const MAX_TOP_K_PATHS: usize = 5;
const MAX_BFS_NODES: usize = 20_000;
const MAX_PATH_STATES: usize = 20_000;

fn path_response(from: Value, to: Value, paths: Vec<Value>, generation: &str) -> Value {
    json!({
        "mode": "path", "from": from, "to": to, "reachable": !paths.is_empty(),
        "shortest_hops": paths.first().and_then(|p| p["hops"].as_u64()),
        "max_depth": MAX_PATH_DEPTH, "total_paths": paths.len(), "paths": paths, "generation": generation,
    })
}

pub(super) fn analyze_path(
    conn: &Connection,
    target: &str,
    to: Option<&str>,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    let generation = paging::generation(conn, options)?;
    let from_sel = target.trim();
    if from_sel.is_empty() {
        bail!("path mode requires a non-empty source selector in target");
    }
    let to_sel = to
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("path mode requires a destination selector in 'to'")?;
    let from_node = select_detail_with_path(conn, from_sel, None, options.detail)?;
    let to_node = select_detail_with_path(conn, to_sel, None, options.detail)?;
    let from_hash = stable_hash64(from_node["id"].as_str().context("invalid source node id")?);
    let to_hash = stable_hash64(
        to_node["id"]
            .as_str()
            .context("invalid destination node id")?,
    );
    if from_hash == to_hash {
        let single = json!({"hops": 0, "nodes": [from_node], "edges": []});
        return Ok(path_response(from_node, to_node, vec![single], &generation));
    }

    let k = options.limit.clamp(1, MAX_TOP_K_PATHS);
    let mut adj: BTreeMap<i64, Vec<GraphEdge>> = BTreeMap::new();
    let mut rev_adj: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut dist_from: HashMap<i64, usize> = HashMap::from([(from_hash, 0)]);
    let mut pred: HashMap<i64, GraphEdge> = HashMap::new();
    let mut frontier = vec![from_hash];

    'bfs: for depth in 1..=MAX_PATH_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let mut next_frontier = BTreeSet::new();
        for edge in fetch_frontier_edges(conn, &frontier, false, false, budget)? {
            if edge.src == edge.dst {
                continue;
            }
            if k == 1 && edge.dst == to_hash {
                dist_from.insert(to_hash, depth);
                pred.insert(to_hash, edge);
                break 'bfs;
            }
            if let Entry::Vacant(slot) = dist_from.entry(edge.dst) {
                slot.insert(depth);
                if dist_from.len() > MAX_BFS_NODES {
                    bail!("path search exceeds node budget; select a narrower symbol");
                }
                if k == 1 {
                    pred.insert(edge.dst, edge.clone());
                }
                if edge.dst != to_hash && depth < MAX_PATH_DEPTH {
                    next_frontier.insert(edge.dst);
                }
            }
            if k > 1 {
                rev_adj.entry(edge.dst).or_default().push(edge.src);
                adj.entry(edge.src).or_default().push(edge);
            }
        }
        frontier = next_frontier.into_iter().collect();
    }

    if !dist_from.contains_key(&to_hash) {
        return Ok(path_response(from_node, to_node, Vec::new(), &generation));
    }

    let mut found: Vec<(Vec<i64>, Vec<GraphEdge>)> = Vec::new();
    if k == 1 {
        let mut nodes_path = vec![to_hash];
        let mut edges_path = Vec::new();
        let mut cur = to_hash;
        while cur != from_hash {
            budget.check()?;
            let Some(edge) = pred.get(&cur) else {
                break;
            };
            cur = edge.src;
            nodes_path.push(cur);
            edges_path.push(edge.clone());
        }
        if cur == from_hash {
            nodes_path.reverse();
            edges_path.reverse();
            found.push((nodes_path, edges_path));
        }
    } else {
        let mut dist_to: HashMap<i64, usize> = HashMap::from([(to_hash, 0)]);
        let mut rev_queue = VecDeque::from([to_hash]);
        while let Some(cur) = rev_queue.pop_front() {
            budget.check()?;
            let next_d = dist_to[&cur] + 1;
            if next_d <= MAX_PATH_DEPTH {
                for &prev in rev_adj.get(&cur).into_iter().flatten() {
                    if !dist_to.contains_key(&prev)
                        && dist_from
                            .get(&prev)
                            .is_some_and(|d| d + next_d <= MAX_PATH_DEPTH)
                    {
                        dist_to.insert(prev, next_d);
                        rev_queue.push_back(prev);
                    }
                }
            }
        }

        // Order equal f-scores by ascending remaining distance so search drives straight to `to_hash`.
        type State = (usize, usize, Vec<i64>, Vec<GraphEdge>);
        let mut heap: BinaryHeap<Reverse<State>> = dist_to
            .get(&from_hash)
            .map(|&r| Reverse((r, r, vec![from_hash], Vec::new())))
            .into_iter()
            .collect();
        let mut created_states = 1usize;

        while let Some(Reverse((f_score, _rem, nodes_path, edges_path))) = heap.pop() {
            budget.check()?;
            if f_score > MAX_PATH_DEPTH {
                break;
            }
            let current = *nodes_path.last().expect("non-empty path");
            if current == to_hash {
                found.push((nodes_path, edges_path));
                if found.len() >= k {
                    break;
                }
                continue;
            }
            let next_hops = edges_path.len() + 1;
            for edge in adj.get(&current).into_iter().flatten() {
                let Some(&rem) = dist_to.get(&edge.dst) else {
                    continue;
                };
                if nodes_path.contains(&edge.dst) || next_hops + rem > MAX_PATH_DEPTH {
                    continue;
                }
                created_states += 1;
                if created_states > MAX_PATH_STATES {
                    bail!("path search exceeds state budget; narrow the selector or limit");
                }
                let (mut next_n, mut next_e) = (nodes_path.clone(), edges_path.clone());
                next_n.push(edge.dst);
                next_e.push(edge.clone());
                heap.push(Reverse((next_hops + rem, rem, next_n, next_e)));
            }
        }
    }

    let unique: Vec<i64> = found
        .iter()
        .flat_map(|(n, _)| n.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let node_map = load_nodes_by_hashes(conn, &unique, budget)?;
    let paths_json: Vec<Value> = found
        .into_iter()
        .map(|(nodes, edges)| {
            json!({
                "hops": edges.len(),
                "nodes": nodes.iter().filter_map(|h| node_map.get(h).map(NodeSummary::to_json)).collect::<Vec<_>>(),
                "edges": edges.iter().map(|e| json!({"src": node_map.get(&e.src).map_or("", |n| n.id.as_str()), "dst": node_map.get(&e.dst).map_or("", |n| n.id.as_str()), "kind": e.kind, "path": e.path, "line": e.line, "occurrence_count": e.occurrence_count})).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(path_response(from_node, to_node, paths_json, &generation))
}
