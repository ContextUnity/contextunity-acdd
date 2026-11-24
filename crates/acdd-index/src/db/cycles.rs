use super::reader::{selectors::path_bounds, sql::QueryBudget};
use crate::models::details::NodeDetailFlags;
use anyhow::{bail, Result};
use petgraph::graphmap::DiGraphMap;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) fn is_archived_path(path: &str) -> bool {
    path.starts_with("archive/") || path.contains("/archive/")
}

pub(crate) fn is_test_context_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("/tests.rs")
        || path == "tests.rs"
        || path.starts_with("benchmarks/")
}

fn is_non_prod_cycle_node(path: &str, details: &str) -> bool {
    is_archived_path(path)
        || is_test_context_path(path)
        || NodeDetailFlags::from_json(details).has_cfg_test
}

pub(crate) fn kosaraju_cycle_sccs<N>(graph: &DiGraphMap<N, ()>) -> Vec<Vec<N>>
where
    N: Copy + Ord + std::hash::Hash,
{
    let mut output = Vec::new();
    for mut component in petgraph::algo::kosaraju_scc(graph) {
        if component.len() == 1 && !graph.contains_edge(component[0], component[0]) {
            continue;
        }
        component.sort_unstable();
        output.push(component);
    }
    output
}

fn collect_cycle_components_filtered(
    conn: &Connection,
    filter_path: Option<&str>,
    kinds_sql: &str,
    min_component_len: usize,
) -> Result<Vec<Vec<i64>>> {
    let budget = QueryBudget::new(conn);
    let selected = if let Some(path) = filter_path.filter(|p| !p.is_empty()) {
        let path = path.trim_end_matches('/');
        let (prefix, end) = path_bounds(path);
        let mut statement = conn.prepare(
            "SELECT n.node_hash FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE p.path=?1 OR (p.path>=?2 AND p.path<?3) LIMIT 1000001",
        )?;
        let selected = statement
            .query_map([path, &prefix, &end], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        if selected.len() > 1_000_000 {
            bail!("cycle scope exceeds node budget");
        }
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        Some(selected)
    } else {
        None
    };

    let sql = format!(
        "SELECT e.src_hash, e.dst_hash, src_p.path, dst_p.path, src_n.details, dst_n.details
         FROM edges e
         JOIN nodes src_n ON src_n.node_hash = e.src_hash
         JOIN nodes dst_n ON dst_n.node_hash = e.dst_hash
         JOIN path_dictionary src_p ON src_p.path_id = src_n.path_id
         JOIN path_dictionary dst_p ON dst_p.path_id = dst_n.path_id
         LEFT JOIN files src_f ON src_f.path = src_p.path
         LEFT JOIN files dst_f ON dst_f.path = dst_p.path
         WHERE e.kind IN {kinds_sql}
           AND src_n.is_test = 0 AND dst_n.is_test = 0
           AND src_n.generated = 0 AND dst_n.generated = 0
           AND COALESCE(src_f.is_test, 0) = 0 AND COALESCE(dst_f.is_test, 0) = 0
         LIMIT 500001"
    );
    let mut statement = conn.prepare(&sql)?;
    let edges = statement.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut graph = DiGraphMap::<i64, ()>::new();
    for (count, edge) in edges.enumerate() {
        if count == 500_000 {
            bail!("cycle analysis exceeds 500000 edge budget");
        }
        let (src, dst, src_path, dst_path, src_details, dst_details) = edge?;
        budget.check()?;
        if is_non_prod_cycle_node(&src_path, &src_details)
            || is_non_prod_cycle_node(&dst_path, &dst_details)
        {
            continue;
        }
        graph.add_edge(src, dst, ());
    }
    let components = kosaraju_cycle_sccs(&graph);
    budget.check()?;
    let mut output = Vec::new();
    for component in components {
        budget.check()?;
        if component.len() < min_component_len {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|scope| !component.iter().any(|n| scope.contains(n)))
        {
            continue;
        }
        output.push(component);
    }
    Ok(output)
}

pub(crate) fn collect_cycle_components(
    conn: &Connection,
    filter_path: Option<&str>,
) -> Result<Vec<Vec<i64>>> {
    collect_cycle_components_filtered(conn, filter_path, "('calls','imports')", 1)
}

pub(crate) fn collect_import_cycle_components(
    conn: &Connection,
    filter_path: Option<&str>,
) -> Result<Vec<Vec<i64>>> {
    collect_cycle_components_filtered(conn, filter_path, "('imports')", 2)
}

pub(super) fn summary(conn: &Connection, filter_path: Option<&str>) -> Result<Value> {
    let mut components = collect_cycle_components(conn, filter_path)?;
    components.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    let total = components.len();
    let total_nodes: usize = components.iter().map(Vec::len).sum();
    let mut lookup = conn.prepare_cached("SELECT n.id,p.path,n.line FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.node_hash=?1")?;
    let mut largest = Vec::new();
    for component in components.iter().take(5) {
        let (id, path, line): (String, String, i64) =
            lookup.query_row([component[0]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        largest.push(
            json!({"node_count":component.len(),"representative":id,"path":path,"line":line}),
        );
    }
    Ok(
        json!({"total":total,"total_nodes":total_nodes,"largest":largest,"omitted_cycles":total.saturating_sub(5),"continuation_hint":if total > 0 {Some("Use code_map_query with operation slice and a representative selector to inspect bounded dependencies.")} else {None}}),
    )
}
