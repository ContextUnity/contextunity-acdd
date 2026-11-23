use super::types::{LintScope, LintViolation};
use crate::db::cycles::collect_import_cycle_components;
use acdd_core::lint::{LintCategory, LintSeverity};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap};

pub(super) fn collect_cycle_violations(
    conn: &Connection,
    scope: &LintScope,
    violations: &mut Vec<LintViolation>,
) -> Result<()> {
    let components = collect_import_cycle_components(conn, scope.exact_prefix())?;
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
