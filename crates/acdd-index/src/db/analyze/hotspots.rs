//! Deterministic PageRank centrality and hotspots analyzer over `code-map.sqlite`.

use super::common::{
    is_symbol_kind, load_nodes, page_slice, path_in_scope, resolve_indexed_scope, NodeSummary,
    NODE_COLS, NON_DEP_EDGES,
};
use crate::db::{
    paging::{self, QueryBudget},
    symbols,
};
use acdd_core::response::QueryOptions;
use anyhow::{ensure, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

/// Bounds and damping parameters for deterministic PageRank centrality.
#[derive(Debug, Clone, Copy)]
pub struct PageRankConfig {
    pub damping: f64,
    pub max_iterations: usize,
    pub tolerance: f64,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub budget: Duration,
}

impl Default for PageRankConfig {
    fn default() -> Self {
        Self {
            damping: 0.85,
            max_iterations: 100,
            tolerance: 1e-6,
            max_nodes: 100_000,
            max_edges: 500_000,
            budget: Duration::from_secs(2),
        }
    }
}

/// Deterministic PageRank output keyed by ascending `node_hash`.
#[derive(Debug, Clone, Default)]
pub struct PageRankResult {
    pub scores: BTreeMap<i64, f64>,
    pub iterations: usize,
    pub converged: bool,
    pub node_count: usize,
    pub edge_count: usize,
    nodes: Vec<NodeSummary>,
    fan_in: HashMap<i64, usize>,
    fan_out: HashMap<i64, usize>,
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn compute_pagerank(
    nodes: &[i64],
    edges: &[(i64, i64)],
    config: &PageRankConfig,
    deadline: Instant,
) -> Result<PageRankResult> {
    ensure!(
        config.damping > 0.0 && config.damping < 1.0,
        "PageRank damping must be in (0, 1)"
    );
    ensure!(
        config.max_iterations >= 1,
        "PageRank max_iterations must be >= 1"
    );
    let mut sorted = nodes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let n = sorted.len();
    ensure!(
        n <= config.max_nodes,
        "PageRank graph exceeds {} node budget",
        config.max_nodes
    );
    ensure!(
        Instant::now() < deadline,
        "graph query exceeds two second budget"
    );
    if n == 0 {
        return Ok(PageRankResult {
            converged: true,
            ..Default::default()
        });
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(src, dst) in edges {
        if src != dst {
            if let (Ok(u), Ok(v)) = (sorted.binary_search(&src), sorted.binary_search(&dst)) {
                adj[u].push(v);
            }
        }
    }
    let mut edge_count = 0usize;
    for nb in &mut adj {
        nb.sort_unstable();
        nb.dedup();
        edge_count += nb.len();
        ensure!(
            edge_count <= config.max_edges,
            "PageRank graph exceeds {} edge budget",
            config.max_edges
        );
    }
    let n_f64 = n as f64;
    let (mut rank, mut next, mut iterations, mut converged) =
        (vec![1.0 / n_f64; n], vec![0.0; n], 0, false);
    for _ in 0..config.max_iterations {
        ensure!(
            Instant::now() < deadline,
            "graph query exceeds two second budget"
        );
        iterations += 1;
        let dangling: f64 = adj
            .iter()
            .enumerate()
            .filter_map(|(u, nb)| nb.is_empty().then_some(rank[u]))
            .sum();
        next.fill((1.0 - config.damping + config.damping * dangling) / n_f64);
        for (u, nb) in adj.iter().enumerate().filter(|(_, nb)| !nb.is_empty()) {
            let share = config.damping * rank[u] / nb.len() as f64;
            for &v in nb {
                next[v] += share;
            }
        }
        let delta: f64 = rank.iter().zip(&next).map(|(a, b)| (b - a).abs()).sum();
        std::mem::swap(&mut rank, &mut next);
        if delta < config.tolerance {
            converged = true;
            break;
        }
    }
    ensure!(
        Instant::now() < deadline,
        "graph query exceeds two second budget"
    );
    Ok(PageRankResult {
        scores: sorted.into_iter().zip(rank).collect(),
        iterations,
        converged,
        node_count: n,
        edge_count,
        ..Default::default()
    })
}

/// Computes deterministic PageRank centrality across all production nodes in `code-map.sqlite`.
pub fn pagerank_centrality(conn: &Connection, config: &PageRankConfig) -> Result<PageRankResult> {
    let budget = QueryBudget::with_duration(conn, config.budget);
    budget.check()?;
    let node_sql = format!(
        "SELECT {NODE_COLS} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.is_test=0 AND n.generated=0 AND n.kind NOT IN('document','invariant') ORDER BY n.node_hash"
    );
    let nodes = load_nodes(conn, &node_sql, &[], Some(config.max_nodes), true, &budget)?;
    let eligible: HashSet<i64> = nodes.iter().map(|n| n.hash).collect();
    let mut edge_stmt = conn.prepare(&format!(
        "SELECT DISTINCT e.src_hash,e.dst_hash FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash \
         WHERE e.kind NOT IN({NON_DEP_EDGES}) AND e.src_hash!=e.dst_hash AND s.is_test=0 AND s.generated=0 AND d.is_test=0 AND d.generated=0 ORDER BY e.src_hash,e.dst_hash"
    ))?;
    let mut edges = Vec::new();
    for row in edge_stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        budget.check()?;
        let (src, dst) = row?;
        if eligible.contains(&src) && eligible.contains(&dst) {
            edges.push((src, dst));
            ensure!(
                edges.len() <= config.max_edges,
                "PageRank graph exceeds {} edge budget",
                config.max_edges
            );
        }
    }
    budget.check()?;
    let hashes: Vec<i64> = nodes.iter().map(|n| n.hash).collect();
    let mut pr = compute_pagerank(&hashes, &edges, config, budget.deadline)?;
    for &(src, dst) in &edges {
        *pr.fan_out.entry(src).or_default() += 1;
        *pr.fan_in.entry(dst).or_default() += 1;
    }
    pr.nodes = nodes;
    budget.check()?;
    Ok(pr)
}

pub(super) fn analyze_hotspots(
    conn: &Connection,
    target: &str,
    config: &PageRankConfig,
    pr: &PageRankResult,
    options: &QueryOptions,
    budget: &QueryBudget<'_>,
) -> Result<Value> {
    budget.check()?;
    let generation = paging::generation(conn, options)?;
    let scope = resolve_indexed_scope(conn, target)?;
    let mut scoped: Vec<&NodeSummary> = pr
        .nodes
        .iter()
        .filter(|n| path_in_scope(&n.path, &scope) && is_symbol_kind(&n.kind))
        .collect();
    if scoped.is_empty() {
        scoped = pr
            .nodes
            .iter()
            .filter(|n| path_in_scope(&n.path, &scope))
            .collect();
    }
    let cand_hashes: Vec<i64> = scoped.iter().map(|n| n.hash).collect();
    let covered = symbols::verified_calls_covered_hashes(conn, Some(&cand_hashes))?;
    budget.check()?;

    let max_pr = scoped
        .iter()
        .filter_map(|n| pr.scores.get(&n.hash).copied())
        .fold(0.0_f64, f64::max);
    let max_cx = scoped
        .iter()
        .map(|n| n.cognitive_complexity.unwrap_or_else(|| n.loc()))
        .max()
        .unwrap_or(1);
    let max_deg = scoped
        .iter()
        .map(|n| {
            pr.fan_in.get(&n.hash).copied().unwrap_or(0)
                + pr.fan_out.get(&n.hash).copied().unwrap_or(0)
        })
        .max()
        .unwrap_or(0);

    let mut ranked: Vec<(&NodeSummary, f64, f64, usize, usize, usize, bool)> = scoped
        .into_iter()
        .map(|node| {
            let pr_val = pr.scores.get(&node.hash).copied().unwrap_or(0.0);
            let fi = pr.fan_in.get(&node.hash).copied().unwrap_or(0);
            let fo = pr.fan_out.get(&node.hash).copied().unwrap_or(0);
            let cx = node.cognitive_complexity.unwrap_or_else(|| node.loc());
            let cov = covered.contains(&node.hash);
            let pr_n = if max_pr > 0.0 { pr_val / max_pr } else { 0.0 };
            let deg_n = if max_deg > 0 {
                ((fi + fo) as f64).ln_1p() / (max_deg as f64).ln_1p()
            } else {
                0.0
            };
            let loc_n = (cx as f64).ln_1p() / (max_cx as f64).ln_1p().max(1.0);
            let score = 0.50 * pr_n + 0.20 * deg_n + 0.15 * loc_n + if cov { 0.0 } else { 0.15 };
            (node, round6(score), round6(pr_val), fi, fo, cx, cov)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| (b.3, b.5).cmp(&(a.3, a.5)))
            .then_with(|| (&a.0.path, a.0.line, &a.0.id).cmp(&(&b.0.path, b.0.line, &b.0.id)))
    });
    let total = ranked.len();
    let tested = ranked.iter().filter(|e| e.6).count();
    let items: Vec<Value> = page_slice(&ranked, options)
        .iter()
        .map(|(n, s, p, fi, fo, cx, cov)| {
            n.with_fields(vec![
                ("hotspot_score", json!(s)),
                ("pagerank", json!(p)),
                ("fan_in", json!(fi)),
                ("fan_out", json!(fo)),
                ("loc", json!(n.loc())),
                ("complexity_proxy", json!(cx)),
                ("test_covered", json!(cov)),
                ("test_gap", json!(!cov)),
            ])
        })
        .collect();
    Ok(json!({
        "mode": "hotspots", "target": scope,
        "pagerank": { "damping": config.damping, "iterations": pr.iterations, "converged": pr.converged, "graph_nodes": pr.node_count, "graph_edges": pr.edge_count },
        "summary": { "total_symbols": total, "tested_symbols": tested, "untested_symbols": total.saturating_sub(tested) },
        "hotspots": paging::value(items, total, options, &generation),
        "generation": generation,
    }))
}
