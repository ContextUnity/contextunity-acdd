use super::{impact::traverse_with_path_options, ImpactDirection, TraversalOptions};
use crate::db::{paging, reader};
use crate::models::{
    details::NodeDetailKey,
    tags::NodeKind,
};
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

pub(super) fn slice_paged(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    depth: u32,
    options: &QueryOptions,
) -> Result<Value> {
    if depth > 16 {
        bail!("depth must be <=16");
    }
    paging::generation(conn, options)?;
    let directory_selector =
        reader::strip_selector_uri_prefix(selector.trim()).trim_end_matches('/');
    match reader::select_detail_with_path(conn, selector, path, options.detail) {
        Ok(node)
            if node["kind"] == NodeKind::Component.as_str()
                && node["path"].as_str() == Some(directory_selector) => {}
        Ok(_) => {
            return traverse_with_path_options(
                conn,
                selector,
                path,
                &TraversalOptions {
                    depth,
                    inbound: false,
                    mode: None,
                    edge_types: None,
                    page: options,
                },
            );
        }
        Err(error)
            if !matches!(
                error.downcast_ref::<reader::SelectorError>(),
                Some(reader::SelectorError::NotFound { .. })
            ) =>
        {
            return Err(error);
        }
        Err(_) => {}
    }
    let Some(path) = reader::directory_path(conn, selector)? else {
        let suggestions = reader::indexed_path_suggestions(conn, selector)?;
        return Err(reader::SelectorError::NotFound {
            selector: selector.to_owned(),
            suggestions,
        }
        .into());
    };
    let (lower, upper) = reader::path_bounds(&path);
    let nodes = paging::query(
        conn,
        &format!(
            "SELECT {} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path>=?1 AND p.path<?2 \
             ORDER BY CASE WHEN n.kind IN ('{module}','file') THEN 0 ELSE 1 END,p.path,n.line,n.id",
            paging::nodes_with_path("n", options.detail, "p.path"),
            module = NodeKind::Module.as_str(),
        ),
        &[&lower, &upper],
        options,
    )?;
    let (boundary, edges) = directory_boundary(conn, &lower, &upper, options)?;
    Ok(json!({
        "selector": {"kind":"directory","path":path},
        "scope":"directory_members",
        "direction":"outbound",
        "requested_depth":depth,
        "graph_depth_applied":0,
        "nodes":nodes,
        "edges":edges,
        "boundary_contract":boundary,
        "hint":"Directory mode lists indexed nodes with graph depth 0. Select a returned exact node id or file path to traverse graph relationships at the requested depth."
    }))
}

// Paths are stored once in the dictionary. Starting at the indexed path range
// keeps the query local to directory nodes and their incident indexed edges.
fn boundary_incidence(direction: ImpactDirection) -> String {
    let (inside_key, outside_key) = if direction.is_inbound() {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let sig = NodeDetailKey::SIGNATURE;
    format!(
        "SELECT inside.id AS inside_id,inside.name AS inside_name,
                json_extract(inside.details,'$.{sig}') AS inside_signature,
                outside.id AS outside_id,outside.name AS outside_name,
                json_extract(outside.details,'$.{sig}') AS outside_signature,
                outside.path_id AS outside_path_id,e.kind AS kind
         FROM path_dictionary p
         CROSS JOIN nodes inside ON inside.path_id=p.path_id
         CROSS JOIN edges e ON e.{inside_key}=inside.node_hash
         CROSS JOIN nodes outside ON outside.node_hash=e.{outside_key}
         CROSS JOIN path_dictionary op ON op.path_id=outside.path_id
         WHERE p.path>=?1 AND p.path<?2 AND (op.path<?1 OR op.path>=?2)"
    )
}

fn boundary_modules(
    conn: &Connection,
    incidence: &str,
    direction: ImpactDirection,
    lower: &str,
    upper: &str,
    options: &QueryOptions,
) -> Result<Value> {
    let (field, example) = if direction.is_inbound() {
        (
            "symbol_pairs",
            "json_object('source_id',outside_id,'target_id',inside_id,'kind',kind)",
        )
    } else {
        (
            "targets",
            "json_object('target_id',outside_id,'name',outside_name,'signature',outside_signature,'source_id',inside_id,'kind',kind)",
        )
    };
    // Rank representatives in the same stable order as the old boundary
    // contract. Only the requested module page is materialized in Rust.
    let sql = format!(
        "WITH RECURSIVE boundary AS MATERIALIZED ({incidence}),
         path_parts(path_id,remaining,prefix) AS (
             SELECT DISTINCT outside_path_id,op.path,'' FROM boundary
             JOIN path_dictionary op ON op.path_id=outside_path_id
             UNION ALL
             SELECT path_id,substr(remaining,instr(remaining,'/')+1),
                    prefix || substr(remaining,1,instr(remaining,'/'))
             FROM path_parts WHERE instr(remaining,'/')>0
         ), path_modules AS (
             SELECT path_id,CASE WHEN prefix='' THEN ''
                    ELSE substr(prefix,1,length(prefix)-1) END AS module
             FROM path_parts WHERE instr(remaining,'/')=0
         ),
         ranked AS (
             SELECT boundary.*,path_modules.module,row_number() OVER (
                 PARTITION BY path_modules.module ORDER BY outside_id,inside_id,kind
             ) AS example_rank FROM boundary JOIN path_modules
               ON path_modules.path_id=boundary.outside_path_id
         )
         SELECT module,count(*) AS edge_count,
                json_group_array({example} ORDER BY example_rank)
                    FILTER (WHERE example_rank<=5) AS {field}
         FROM ranked GROUP BY module ORDER BY module"
    );
    let mut page = paging::query(conn, &sql, &[&lower, &upper], options)?;
    if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            let text = item[field].as_str().context("invalid boundary examples")?;
            item[field] = serde_json::from_str(text)?;
        }
    }
    Ok(page)
}

fn directory_boundary(
    conn: &Connection,
    lower: &str,
    upper: &str,
    options: &QueryOptions,
) -> Result<(Value, Value)> {
    let incoming = boundary_incidence(ImpactDirection::Inbound);
    let outgoing = boundary_incidence(ImpactDirection::Outbound);
    let exports = paging::query(
        conn,
        &format!(
            "SELECT inside_id AS id,inside_name AS name,inside_signature AS signature,
                    count(DISTINCT outside_id) AS external_caller_count
             FROM ({incoming}) GROUP BY inside_id ORDER BY inside_id"
        ),
        &[&lower, &upper],
        options,
    )?;
    let inbound = boundary_modules(conn, &incoming, ImpactDirection::Inbound, lower, upper, options)?;
    let outbound = boundary_modules(
        conn,
        &outgoing,
        ImpactDirection::Outbound,
        lower,
        upper,
        options,
    )?;
    let evidence_order = if options.detail == Detail::Full {
        ",evidence"
    } else {
        ""
    };
    let internal = paging::query(
        conn,
        &format!(
            "SELECT * FROM (SELECT {} FROM path_dictionary p
             CROSS JOIN nodes inside ON inside.path_id=p.path_id
             CROSS JOIN edges e ON e.src_hash=inside.node_hash
             CROSS JOIN nodes outside ON outside.node_hash=e.dst_hash
             CROSS JOIN path_dictionary op ON op.path_id=outside.path_id
             WHERE p.path>=?1 AND p.path<?2 AND op.path>=?1 AND op.path<?2)
             ORDER BY src_public_id,dst_public_id,kind,line,occurrence_count,path,confidence{evidence_order}",
            paging::edges("e", options.detail)
        ),
        &[&lower, &upper],
        options,
    )?;
    Ok((
        json!({"exported_symbols":exports,"inbound_modules":inbound,"outbound_modules":outbound}),
        internal,
    ))
}
