use super::lint::collect_cycle_components;
use super::reader::QueryBudget;
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

/// Performs cycles.
pub fn cycles(conn: &Connection, filter_path: Option<&str>) -> Result<Value> {
    let components = collect_cycle_components(conn, filter_path)?;
    let budget = QueryBudget::new(conn);
    let mut lookup = conn.prepare_cached("SELECT id FROM nodes WHERE node_hash=?1")?;
    let mut output = Vec::new();
    let mut bytes = 2usize;
    for component in components {
        let mut ids = Vec::with_capacity(component.len());
        for node in component {
            budget.check()?;
            let id: String = lookup.query_row([node], |r| r.get(0))?;
            bytes = bytes.saturating_add(id.len().saturating_mul(6) + 4);
            if bytes > super::commitments::MAX_QUERY_RESULT_BYTES {
                bail!("cycle result exceeds the 8 MiB output budget; analyze an indexed file or directory path to narrow the scope, or use code_map_analyze with include_cycles=false for diagnostic totals");
            }
            ids.push(id);
        }
        ids.sort();
        output.push(ids);
    }
    output.sort();
    Ok(json!(output))
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
