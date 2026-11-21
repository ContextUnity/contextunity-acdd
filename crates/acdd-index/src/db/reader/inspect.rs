use super::super::paging;
use super::selectors::select_detail_with_path;
use super::sql::rows;
use super::{select_detail, ExplainOptions};
use crate::models::details::NodeDetailKey;
use crate::models::tags::EdgeKind;
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{Context, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};

/// Resolves one fail-closed code selector and returns its full node projection.
///
/// # Errors
///
/// Returns a typed [`super::SelectorError`] for missing, ambiguous, or invalid targets.
pub fn select(conn: &Connection, selector: &str) -> Result<Value> {
    select_detail(conn, selector, Detail::Full)
}

/// Inspects symbols matching path or symbol selector with paging options.
pub fn inspect_paged_with_path(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    show_doc: bool,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let node = select_detail_with_path(conn, selector, path, options.detail)?;
    let id = node["id"].as_str().context("invalid node id")?;
    let mut documents = if show_doc {
        paging::query(
            conn,
            &format!(
                "SELECT {} FROM doc_sections d JOIN nodes dn ON dn.id=d.doc_id JOIN edges e ON e.dst_hash=dn.node_hash WHERE e.src_hash=?1 AND e.kind='{}' ORDER BY d.is_invariant DESC,d.path,d.doc_id",
                paging::docs("d", options.detail),
                EdgeKind::ReferencesDoc.as_str(),
            ),
            &[&crate::models::stable_hash64(id)],
            options,
        )?
    } else {
        paging::value(Vec::new(), 0, options, &generation)
    };
    if documents["total"] == 0 {
        documents = json!({"total": 0});
    }
    let path = node["path"].as_str().context("invalid node path")?;
    let line = node["line"].as_i64().context("invalid start line")?;
    let end_line = node["end_line"].as_i64().context("invalid end line")?;
    let scope: &[&dyn ToSql] = &[&path, &line, &end_line];
    let mut coverage = paging::query(
        conn,
        &format!(
            "SELECT {} FROM resolution_coverage c WHERE c.path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) AND c.line BETWEEN ?2 AND ?3 ORDER BY c.line,c.expression_id,c.status,c.evidence_id",
            paging::coverage("c", options.detail)
        ),
        scope,
        options,
    )?;
    coverage["statuses"] = json!(rows(
        conn,
        "SELECT status,count(*) total FROM resolution_coverage WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path=?1) AND line BETWEEN ?2 AND ?3 GROUP BY status ORDER BY status",
        scope,
        20
    )?);
    if options.detail == Detail::Compact {
        coverage["detail_hint"] = json!("detail='full' includes per-reference evidence; statuses count the entire selected scope.");
    }
    Ok(json!({"node":node, "documents":documents, "coverage":coverage, "generation":generation}))
}

/// Explains symbol relationships with path filtering and paging options.
pub fn explain_with_path_options(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    request: &ExplainOptions<'_>,
) -> Result<Value> {
    let show_doc = request.show_doc;
    let options = request.page;
    let mut result = inspect_paged_with_path(conn, selector, path, show_doc, options)?;
    let id = result["node"]["id"]
        .as_str()
        .context("invalid node id")?
        .to_owned();
    let node_hash = crate::models::stable_hash64(&id);
    paging::populate_edge_relations(conn, &mut result, node_hash, request.direction, options)?;
    if let Some(gen) = result.as_object_mut().and_then(|m| m.remove("generation")) {
        if let Some(m) = result.as_object_mut() {
            m.insert("generation".into(), gen);
        }
    }
    Ok(result)
}

fn governing_invariants_for_cte(
    conn: &Connection,
    scope_nodes_cte: &str,
    param: &dyn ToSql,
    exclude_direct_from_transitive: bool,
) -> Result<Value> {
    const DIRECT_LIMIT: usize = 5;
    const TRANSITIVE_LIMIT: usize = 5;
    let governs = EdgeKind::Governs.as_str();
    let calls = EdgeKind::Calls.as_str();
    let possible_calls = EdgeKind::PossibleCalls.as_str();
    let direct_cte = format!(
        "direct AS (SELECT DISTINCT e.src_hash FROM edges e JOIN scope_nodes s ON s.hash=e.dst_hash WHERE e.kind='{governs}')"
    );
    let neighbors_cte = format!(
        "neighbors(hash) AS (SELECT e.src_hash FROM edges e JOIN scope_nodes s ON s.hash=e.dst_hash WHERE e.kind IN('{calls}','{possible_calls}') UNION SELECT e.dst_hash FROM edges e JOIN scope_nodes s ON s.hash=e.src_hash WHERE e.kind IN('{calls}','{possible_calls}'))"
    );
    let direct_exclusion = if exclude_direct_from_transitive {
        " AND NOT EXISTS(SELECT 1 FROM direct d WHERE d.src_hash=e.src_hash)"
    } else {
        ""
    };
    let direct_total = paging::count(
        conn,
        &format!("{scope_nodes_cte}, {direct_cte} SELECT count(*) FROM direct"),
        &[param],
    )?;
    let direct_rows = rows(
        conn,
        &format!(
            "{scope_nodes_cte} SELECT DISTINCT n.name AS id,json_extract(n.details,'$.{rule}') AS rule,p.path,n.line \
             FROM edges e JOIN scope_nodes s ON s.hash=e.dst_hash JOIN nodes n ON n.node_hash=e.src_hash \
             JOIN path_dictionary p ON p.path_id=n.path_id WHERE e.kind='{governs}' ORDER BY n.name,p.path,n.line LIMIT ?2",
            rule = NodeDetailKey::RULE,
        ),
        &[param, &(DIRECT_LIMIT as i64)],
        DIRECT_LIMIT,
    )?;
    let direct = direct_rows
        .iter()
        .map(|item| {
            json!({
                "id": item["id"],
                "rule": item["rule"],
                "source": {"path": item["path"], "line": item["line"]},
            })
        })
        .collect::<Vec<_>>();
    let transitive_total = paging::count(
        conn,
        &format!(
            "{scope_nodes_cte}, {neighbors_cte}, {direct_cte}, \
             transitive AS (SELECT DISTINCT e.src_hash FROM edges e JOIN neighbors g ON g.hash=e.dst_hash WHERE e.kind='{governs}'{direct_exclusion}) \
             SELECT count(*) FROM transitive"
        ),
        &[param],
    )?;
    let transitive_rows = rows(
        conn,
        &format!(
            "{scope_nodes_cte}, {neighbors_cte}, {direct_cte} \
             SELECT DISTINCT n.name AS id,json_extract(n.details,'$.{rule}') AS rule,p.path,n.line \
             FROM edges e JOIN neighbors g ON g.hash=e.dst_hash JOIN nodes n ON n.node_hash=e.src_hash \
             JOIN path_dictionary p ON p.path_id=n.path_id WHERE e.kind='{governs}'{direct_exclusion} \
             ORDER BY n.name,p.path,n.line LIMIT ?2",
            rule = NodeDetailKey::RULE,
        ),
        &[param, &(TRANSITIVE_LIMIT as i64)],
        TRANSITIVE_LIMIT,
    )?;
    let transitive_ids = transitive_rows
        .iter()
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let omitted_sources = transitive_rows
        .iter()
        .map(|item| json!({"id": item["id"], "source": {"path": item["path"], "line": item["line"]}}))
        .collect::<Vec<_>>();
    Ok(json!({
        "direct": direct,
        "omitted_direct": direct_total.saturating_sub(direct.len()),
        "transitive_ids": transitive_ids,
        "omitted_transitive": transitive_total.saturating_sub(transitive_rows.len()),
        "omitted_sources": omitted_sources,
    }))
}

/// Returns governing documentation invariants for a node.
pub fn governing_invariants(conn: &Connection, node_id: &str) -> Result<Value> {
    let node_hash = crate::models::stable_hash64(node_id);
    governing_invariants_for_cte(conn, "WITH scope_nodes(hash) AS (SELECT ?1)", &node_hash, false)
}

/// Returns bounded governing rules for every indexed symbol in a task scope.
pub fn governing_invariants_for_scope(conn: &Connection, scope: &[String]) -> Result<Value> {
    if scope.is_empty() {
        return Ok(json!({
            "direct": [],
            "omitted_direct": 0,
            "transitive_ids": [],
            "omitted_transitive": 0,
            "omitted_sources": [],
        }));
    }
    let scope_json = serde_json::to_string(scope)?;
    let scope_nodes = "WITH scope_nodes(hash) AS (SELECT DISTINCT n.node_hash FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE EXISTS(SELECT 1 FROM json_each(?1) s WHERE p.path=trim(s.value,'/') OR substr(p.path,1,length(trim(s.value,'/'))+1)=trim(s.value,'/')||'/'))";
    governing_invariants_for_cte(conn, scope_nodes, &scope_json, true)
}
