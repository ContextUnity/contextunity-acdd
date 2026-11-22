use super::{LintScope, LintViolation};
use crate::db::reader::{self, QueryBudget};
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::{bail, Result};
use petgraph::graphmap::DiGraphMap;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap, HashSet};

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

pub(crate) fn collect_cycle_components(
    conn: &Connection,
    filter_path: Option<&str>,
) -> Result<Vec<Vec<i64>>> {
    let budget = QueryBudget::new(conn);
    let selected = if let Some(path) = filter_path.filter(|p| !p.is_empty()) {
        let path = path.trim_end_matches('/');
        let (prefix, end) = reader::path_bounds(path);
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

    let mut statement = conn.prepare(
        "SELECT src_hash,dst_hash FROM edges WHERE kind IN('calls','imports') LIMIT 500001",
    )?;
    let edges = statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut graph = DiGraphMap::<i64, ()>::new();
    for (count, edge) in edges.enumerate() {
        if count == 500_000 {
            bail!("cycle analysis exceeds 500000 edge budget");
        }
        let (src, dst) = edge?;
        graph.add_edge(src, dst, ());
        budget.check()?;
    }
    let components = kosaraju_cycle_sccs(&graph);
    budget.check()?;
    let mut output = Vec::new();
    for component in components {
        budget.check()?;
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

pub(super) fn collect_cycle_violations(
    conn: &Connection,
    scope: &LintScope,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let components = collect_cycle_components(conn, scope.exact_prefix())?;
    if components.is_empty() {
        return Ok(());
    }
    let mut all_hashes = BTreeSet::new();
    for comp in &components {
        for &h in comp {
            all_hashes.insert(h);
        }
    }
    let hashes_json = serde_json::to_string(&all_hashes)?;
    let mut info_by_hash: HashMap<i64, (String, String, usize)> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT n.node_hash, n.id, p.path, n.line
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.node_hash IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))",
    )?;
    let mut rows = stmt.query([hashes_json])?;
    while let Some(row) = rows.next()? {
        info_by_hash.insert(
            row.get(0)?,
            (
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)?.max(1) as usize,
            ),
        );
    }
    for comp in components {
        let mut nodes_in_scc: Vec<&(String, String, usize)> = comp
            .iter()
            .filter_map(|h| info_by_hash.get(h))
            .collect();
        if nodes_in_scc.is_empty() {
            continue;
        }
        nodes_in_scc.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)).then_with(|| a.0.cmp(&b.0)));
        if !nodes_in_scc.iter().any(|(_, p, _)| scope.in_scope(p)) {
            continue;
        }
        let (rep_id, rep_path, rep_line) = nodes_in_scc[0];
        violations.push(LintViolation {
            category: LintCategory::Cycles,
            rule_id: "cycles.scc".to_owned(),
            severity: LintSeverity::Warn,
            path: rep_path.clone(),
            line: Some(*rep_line),
            message: format!(
                "dependency cycle of {} nodes (representative: {})",
                comp.len(),
                rep_id
            ),
            invariant: None,
            rule_source: None,
            symbol: Some(rep_id.clone()),
            target: None,
            suggestion: Some("Use code_map_query with operation 'slice' to inspect cycle boundaries.".to_owned()),
        });
    }
    Ok(())
}
