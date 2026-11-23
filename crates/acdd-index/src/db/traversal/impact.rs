use super::{
    change_impact::changed_impact_with_path_options,
    types::{
        decode_edge_descriptor, edge_endpoints, normalize_scope, resolve_traversal_kinds,
        ImpactDirection, TraversalMode, TraversalOptions, MAX_DEEP_TRAVERSAL_FRONTIER,
    },
};
use crate::db::paging::{
    self, immediate_links, REVERSE_DEPENDENCIES,
};
use crate::db::reader;
use crate::models::{
    details::NodeDetailKey,
    stable_hash64,
    tags::{CoverageStatus, EdgeKind, NodeKind},
};
use acdd_core::response::{Detail, QueryOptions};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// Parses an optional impact direction string into an inbound boolean.
pub fn impact_inbound(direction: Option<&str>) -> Result<bool> {
    Ok(ImpactDirection::parse_optional(direction)?.is_inbound())
}

fn admit_traversal(
    conn: &Connection,
    id: &str,
    depth: u32,
    direction: ImpactDirection,
) -> Result<usize> {
    if depth <= 1 {
        return Ok(0);
    }
    let immediate = immediate_links(conn, id, direction.is_inbound(), true)?;
    if immediate > MAX_DEEP_TRAVERSAL_FRONTIER {
        bail!("selector {id} has {immediate} immediate graph links; depth {depth} would expand a broad graph before pagination. Retry with depth=1 and page the result, or select a narrower module or symbol. Reducing limit alone does not reduce traversal work");
    }
    Ok(immediate)
}

pub(super) fn custom_dependency_steps(
    direction: ImpactDirection,
    bounded: bool,
    forward: &str,
    reverse: &str,
) -> String {
    let (from, to) = edge_endpoints(direction);
    let depth = if bounded { ",w.depth+1" } else { "" };
    let guard = if bounded { "w.depth<?2 AND " } else { "" };
    if reverse.is_empty() {
        format!(
            "SELECT e.{to}{depth} FROM walk w JOIN edges e ON e.{from}=w.id WHERE {guard}e.kind IN({forward})"
        )
    } else {
        format!(
            "SELECT e.{to}{depth} FROM walk w JOIN edges e ON e.{from}=w.id WHERE {guard}e.kind IN({forward}) UNION SELECT e.{from}{depth} FROM walk w JOIN edges e ON e.{to}=w.id WHERE {guard}e.kind IN({reverse})"
        )
    }
}

fn traversal_response(
    selector: Value,
    direction: ImpactDirection,
    depth: u32,
    mode: &str,
    edge_types: Option<&[String]>,
    nodes: Value,
) -> Value {
    let dir = direction.as_str();
    let mut result = json!({
        "selector": selector,
        "direction": dir,
        "depth": depth,
        "mode": mode,
        "nodes": nodes,
    });
    if let Some(types) = edge_types {
        result["edge_types"] = json!(types);
    }
    result
}

/// Traverse direct indexed relationships with path scoping and bounded paging.
pub fn traverse_with_path_options(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    let depth = request.depth;
    let direction = ImpactDirection::from_inbound(request.inbound);
    let mode = TraversalMode::parse_optional(request.mode);
    let edge_types = request.edge_types;
    let options = request.page;
    if depth > 16 {
        bail!("depth must be <=16");
    }
    let generation = paging::generation(conn, options)?;
    let node = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    let id = node["id"].as_str().context("selected node has no id")?;
    let id_hash = stable_hash64(id);
    let immediate = admit_traversal(conn, id, depth, direction)?;
    let (forward, reverse) = resolve_traversal_kinds(mode, edge_types, true);
    let mode_str = request.mode.unwrap_or(mode.as_str());

    if depth == 0 || (depth > 1 && immediate == 0) {
        let items = if options.offset == 0 {
            let mut seed = node.clone();
            let seed_fields = seed.as_object_mut().context("invalid selected node")?;
            seed_fields.insert("distance".into(), json!(0));
            seed_fields.insert("is_seed".into(), json!(true));
            vec![seed]
        } else {
            Vec::new()
        };
        return Ok(traversal_response(
            node,
            direction,
            depth,
            mode_str,
            edge_types,
            paging::value(items, 1, options, &generation),
        ));
    }
    let steps = custom_dependency_steps(direction, true, &forward, &reverse);
    if depth == 1 {
        let columns = paging::nodes("n", options.detail);
        let base = format!(
            "WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id) SELECT {columns},r.distance FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id"
        );
        let count = reader::rows(
            conn,
            &format!("SELECT count(*) total FROM ({base})"),
            &[&id_hash, &depth],
            1,
        )?;
        let total = count[0]["total"]
            .as_u64()
            .context("traversal total is missing")?;
        let total = usize::try_from(total)?;
        let limit = i64::try_from(options.limit)?;
        let offset = i64::try_from(options.offset)?;
        let (from, to) = edge_endpoints(direction);
        let reverse_edge = if reverse.is_empty() {
            String::new()
        } else {
            format!(" OR (e.{to}=?1 AND e.{from}=n.node_hash AND e.kind IN({reverse}))")
        };
        let evidence = if options.detail == Detail::Full {
            ", 'evidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.evidence_id)"
        } else {
            ""
        };
        let edge_json = format!(
            "CASE WHEN r.distance=1 THEN (SELECT json_object('src_public_id',(SELECT id FROM nodes WHERE node_hash=e.src_hash),'dst_public_id',(SELECT id FROM nodes WHERE node_hash=e.dst_hash),'kind',e.kind,'path',(SELECT path FROM path_dictionary WHERE path_id=e.path_id),'line',e.line,'confidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.confidence_id),'occurrence_count',e.occurrence_count,'direction',CASE WHEN e.src_hash=?1 THEN 'outbound' ELSE 'inbound' END{evidence}) FROM edges e WHERE ((e.{from}=?1 AND e.{to}=n.node_hash AND e.kind IN({forward})){reverse_edge}) ORDER BY e.kind,e.src_hash,e.dst_hash,e.path_id,e.line,e.evidence_id LIMIT 1) END AS __edge"
        );
        let page_sql = format!(
            "WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id) SELECT {columns},r.distance,{edge_json} FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4"
        );
        let mut items = reader::rows(
            conn,
            &page_sql,
            &[&id_hash, &depth, &limit, &offset],
            options.limit,
        )?;
        for item in &mut items {
            let fields = item.as_object_mut().context("invalid traversal node")?;
            let edge = decode_edge_descriptor(fields.remove("__edge").unwrap_or(Value::Null))?;
            fields.insert(
                "is_seed".into(),
                json!(fields.get("id").and_then(Value::as_str) == Some(id)),
            );
            if !edge.is_null() {
                fields.insert("edge".into(), edge);
            }
        }
        return Ok(traversal_response(
            node,
            direction,
            depth,
            mode_str,
            edge_types,
            paging::value(items, total, options, &generation),
        ));
    }
    // Reuse the bounded reachability result for the exact count, page, and path evidence.
    let columns = paging::nodes("n", options.detail);
    let (forward_from, forward_to, reverse_from, reverse_to) = if direction.is_inbound() {
        ("src_hash", "dst_hash", "dst_hash", "src_hash")
    } else {
        ("dst_hash", "src_hash", "src_hash", "dst_hash")
    };
    let reverse_union = if reverse.is_empty() {
        String::new()
    } else {
        format!(
            "UNION ALL SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{reverse_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{reverse_to} WHERE e.{reverse_from}=p.__hash AND e.kind IN({reverse})"
        )
    };
    let sql = if immediate <= 3 && options.offset == 0 {
        format!(
            "WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), \
        reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id), \
        page AS (SELECT {columns},n.node_hash __hash,r.distance,count(*) OVER() __total FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4) \
        SELECT p.*,CASE WHEN p.distance=0 THEN NULL ELSE (SELECT json_array(predecessor,edge_kind) FROM ( \
            SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{forward_to} WHERE e.{forward_from}=p.__hash AND e.kind IN({forward}) \
            {reverse_union} \
        ) ORDER BY predecessor,edge_kind LIMIT 1) END __reason FROM page p"
        )
    } else {
        format!(
            "WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), \
        reached(id,distance) AS MATERIALIZED (SELECT id,min(depth) FROM walk GROUP BY id), \
        page AS (SELECT {columns},n.node_hash __hash,r.distance FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4) \
        SELECT totals.__total,p.*,CASE WHEN p.distance=0 THEN NULL ELSE (SELECT json_array(predecessor,edge_kind) FROM ( \
            SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{forward_to} WHERE e.{forward_from}=p.__hash AND e.kind IN({forward}) \
            {reverse_union} \
        ) ORDER BY predecessor,edge_kind LIMIT 1) END __reason \
        FROM (SELECT count(*) __total FROM reached r JOIN nodes n ON n.node_hash=r.id) totals \
        LEFT JOIN page p ON 1 ORDER BY p.distance,p.id"
        )
    };
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let mut items = reader::rows(
        conn,
        &sql,
        &[&id_hash, &depth, &limit, &offset],
        options.limit,
    )?;
    let total = items
        .first()
        .and_then(|row| row["__total"].as_u64())
        .context("traversal total is missing")?;
    let total = usize::try_from(total)?;
    items.retain(|row| !row["id"].is_null());
    for item in &mut items {
        let item = item.as_object_mut().context("invalid traversal node")?;
        item.remove("__total");
        item.remove("__hash");
        let reason = item
            .remove("__reason")
            .and_then(|value| value.as_str().map(str::to_owned));
        let reason = reason
            .map(|encoded| serde_json::from_str::<(String, String)>(&encoded))
            .transpose()?;
        let predecessor = item
            .remove("__predecessor")
            .and_then(|value| value.as_str().map(str::to_owned))
            .or_else(|| reason.as_ref().map(|pair| pair.0.clone()));
        let edge_kind = item
            .remove("__edge_kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            .or_else(|| reason.map(|pair| pair.1));
        if item.get("id").and_then(Value::as_str) == Some(id) {
            item.insert("is_seed".into(), json!(true));
        } else {
            item.insert("is_seed".into(), json!(false));
            item.insert(
                "via".into(),
                json!({
                    "id": predecessor.context("reached node has no predecessor")?,
                    "kind": edge_kind.context("reached node has no edge kind")?
                }),
            );
        }
    }
    Ok(traversal_response(
        node,
        direction,
        depth,
        mode_str,
        edge_types,
        paging::value(items, total, options, &generation),
    ))
}

/// Impact either one exact selector or the selected Git change view.
pub fn impact_with_path_options(
    conn: &Connection,
    root: &Path,
    selector: Option<&str>,
    changes: Option<&str>,
    path: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    let empty_selector = selector.is_some_and(|value| value.trim().is_empty());
    match (
        selector.filter(|value| !value.trim().is_empty()),
        changes.filter(|value| !value.trim().is_empty()),
    ) {
        (Some(selector), None) => {
            let mut result = traverse_with_path_options(conn, selector, path, request)?;
            let selected_id = result["selector"]["id"]
                .as_str()
                .or_else(|| result["selector"]["node"]["id"].as_str())
                .unwrap_or(selector)
                .to_owned();
            let selected_path = result["selector"]["path"]
                .as_str()
                .or_else(|| result["selector"]["node"]["path"].as_str())
                .unwrap_or(selector)
                .to_owned();
            result["governing_invariants"] = reader::governing_invariants(conn, &selected_id)?;
            let path_is_indexed: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM path_dictionary WHERE path=?1)",
                [&selected_path],
                |row| row.get(0),
            )?;
            let refs_doc = EdgeKind::ReferencesDoc.as_str();
            result["affected_docs"] = if path_is_indexed {
                paging::query(
                    conn,
                    &format!(
                        "SELECT d.doc_id,d.path,d.section_title,d.doc_type,e.line FROM edges e JOIN nodes symbol ON symbol.node_hash=e.src_hash JOIN path_dictionary sp ON sp.path_id=symbol.path_id JOIN nodes dn ON dn.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=dn.id WHERE e.kind='{refs_doc}' AND sp.path=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' UNION SELECT d.doc_id,d.path,d.section_title,d.doc_type,CAST(json_extract(c.value,'$.line') AS INTEGER) FROM nodes dn JOIN doc_sections d ON d.doc_id=dn.id JOIN json_each(dn.details,'$.symbol_claims') c WHERE json_extract(c.value,'$.target_path')=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' ORDER BY 2,1,5"
                    ),
                    &[&selected_path],
                    request.page,
                )?
            } else {
                paging::query(
                    conn,
                    &format!(
                        "SELECT DISTINCT d.doc_id,d.path,d.section_title,d.doc_type,e.line FROM edges e JOIN nodes dn ON dn.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=dn.id WHERE e.kind='{refs_doc}' AND e.src_hash=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' ORDER BY d.path,d.doc_id,e.line"
                    ),
                    &[&stable_hash64(&selected_id)],
                    request.page,
                )?
            };
            Ok(result)
        }
        (None, Some(mode)) => changed_impact_with_path_options(conn, root, mode, path, request),
        (Some(_), Some(_)) => bail!("provide exactly one of selector or changes"),
        (None, None) if empty_selector => traverse_with_path_options(
            conn,
            selector.context("empty selector was unexpectedly absent")?,
            path,
            request,
        ),
        (None, None) => bail!("provide exactly one of selector or changes"),
    }
}

fn removal_assessment(
    dependencies: usize,
    exact_dependencies: bool,
    target_unresolved: usize,
    target_errors: usize,
) -> Value {
    let mut reasons = Vec::new();
    if dependencies > 0 {
        reasons.push(if exact_dependencies {
            json!({"kind":"incoming_dependencies","scope":"selection","count":dependencies})
        } else {
            json!({"kind":"incoming_dependencies","scope":"selection","at_least":dependencies})
        });
    }
    if target_unresolved > 0 {
        reasons.push(
            json!({"kind":"unresolved_references","scope":"target","count":target_unresolved}),
        );
    }
    if target_errors > 0 {
        reasons.push(json!({"kind":"parse_errors","scope":"target","count":target_errors}));
    }
    json!({"verdict":if reasons.is_empty() {"no_indexed_blockers"} else {"blocked"},"blocking_reasons":reasons})
}

fn removal_diagnostics(conn: &Connection, selected_ids: &[String]) -> Result<(usize, usize)> {
    let ids = serde_json::to_string(selected_ids)?;
    let target_unresolved = paging::count(
        conn,
        &format!(
            "SELECT count(*) FROM resolution_coverage c JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN path_dictionary p ON p.path_id=c.path_id WHERE c.status IN('{unresolved}','{ambiguous}') AND (x.expression IN (SELECT n.name FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?1))) OR x.expression IN (SELECT n.qualname FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?1))) OR p.path IN (SELECT np.path FROM nodes n JOIN path_dictionary np ON np.path_id=n.path_id WHERE n.kind='{module}' AND n.id IN (SELECT value FROM json_each(?1))) OR (EXISTS(SELECT 1 FROM nodes n WHERE n.kind='{module}' AND n.id IN (SELECT value FROM json_each(?1))) AND p.path IN (SELECT ep.path FROM edges e JOIN path_dictionary ep ON ep.path_id=e.path_id JOIN nodes m ON m.node_hash=e.dst_hash JOIN path_dictionary mp ON mp.path_id=m.path_id WHERE e.kind='{imports}' AND mp.path IN (SELECT np.path FROM nodes n JOIN path_dictionary np ON np.path_id=n.path_id WHERE n.kind='{module}' AND n.id IN (SELECT value FROM json_each(?1))))))",
            unresolved = CoverageStatus::Unresolved.as_str(),
            ambiguous = CoverageStatus::Ambiguous.as_str(),
            module = NodeKind::Module.as_str(),
            imports = EdgeKind::Imports.as_str(),
        ),
        &[&ids],
    )?;
    let target_errors = paging::count(
        conn,
        "SELECT count(*) FROM errors WHERE path IN (SELECT p.path FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id IN (SELECT value FROM json_each(?1)))",
        &[&ids],
    )?;
    Ok((target_unresolved, target_errors))
}

/// Returns removal proof or blocked dependents for a symbol selector.
pub fn removal_paged_with_path(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let normalized_path = reader::strip_selector_uri_prefix(selector.trim());
    if normalized_path.ends_with(".md") {
        return Err(reader::SelectorError::DocLink {
            target: normalized_path.to_owned(),
        }
        .into());
    }
    let generation = paging::generation(conn, options)?;
    let selected_node = reader::select_detail_with_path(conn, selector, path, Detail::Compact)?;
    let file_count = paging::count(
        conn,
        "SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1",
        &[&normalized_path],
    )?;
    let (selection, selected_id) = if file_count > 0 {
        (
            "SELECT n.node_hash FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1",
            normalized_path.to_owned(),
        )
    } else {
        (
            "SELECT node_hash FROM nodes WHERE id=?1",
            selected_node["id"].as_str().unwrap_or("").to_owned(),
        )
    };
    if file_count > 10000 {
        bail!("removal scope exceeds 10000 nodes");
    }
    let selected = format!("selected(hash) AS ({selection})");
    let dependency_rows = format!(
        "SELECT e.src_hash,e.dst_hash,e.kind FROM edges e WHERE e.dst_hash IN(SELECT hash FROM selected) AND e.src_hash NOT IN(SELECT hash FROM selected) AND e.kind NOT IN('{contains}','{refs_doc}',{REVERSE_DEPENDENCIES}) UNION SELECT e.src_hash,e.dst_hash,e.kind FROM edges e WHERE e.src_hash IN(SELECT hash FROM selected) AND e.dst_hash NOT IN(SELECT hash FROM selected) AND e.kind IN({REVERSE_DEPENDENCIES})",
        contains = EdgeKind::Contains.as_str(),
        refs_doc = EdgeKind::ReferencesDoc.as_str(),
    );
    let dependency_cte =
        format!("WITH {selected}, dependencies AS MATERIALIZED ({dependency_rows})");
    let selection_sql = if file_count > 0 {
        "SELECT n.id FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 ORDER BY n.id"
    } else {
        "SELECT id FROM nodes WHERE id=?1 ORDER BY id"
    };
    let diagnostic_selected = reader::rows(conn, selection_sql, &[&selected_id], 10001)?;
    let selected_ids: Vec<String> = diagnostic_selected
        .iter()
        .filter_map(|node| node["id"].as_str().map(str::to_owned))
        .collect();
    let (target_unresolved, target_errors) = removal_diagnostics(conn, &selected_ids)?;
    let selected_total = selected_ids.len();
    let selected_items = selected_ids
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|id| json!({"id":id}))
        .collect();
    let selected_page = paging::value(selected_items, selected_total, options, &generation);
    let callers = paging::query(
        conn,
        &format!(
            "{dependency_cte} SELECT {} FROM dependencies d JOIN edges e ON e.src_hash=d.src_hash AND e.dst_hash=d.dst_hash AND e.kind=d.kind ORDER BY e.path_id,e.line,e.src_hash,e.dst_hash,e.kind",
            paging::edges("e", options.detail)
        ),
        &[&selected_id],
        options,
    )?;
    let dependency_count = callers["total"]
        .as_u64()
        .map(usize::try_from)
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("removal dependency total is missing"))?;
    let safe_to_remove = dependency_count == 0 && target_unresolved == 0 && target_errors == 0;
    let assessment = removal_assessment(dependency_count, true, target_unresolved, target_errors);
    Ok(json!({
        "assessment": assessment,
        "selector": selector,
        "selected_ids": selected_page,
        "incoming_dependencies": callers,
        "safe_to_remove": safe_to_remove,
        "unresolved_references": target_unresolved,
        "parse_errors": target_errors,
        "proof_scope": "indexed static references only; dynamic entrypoints and external callers require separate authority",
        "generation": generation
    }))
}

pub(super) fn untested_paged(
    conn: &Connection,
    selector: Option<&str>,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let scope = normalize_scope(selector, path, "untested selector")?;
    let (lower, upper) = reader::path_bounds(&scope);
    let nodes = paging::query(
        conn,
        &format!(
            "SELECT {},json_extract(n.details,'$.{sig}') AS signature FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.kind IN('{function}','{method}') AND n.is_test=0 AND (?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3)) AND NOT EXISTS (SELECT 1 FROM edges e JOIN nodes caller ON caller.node_hash=e.src_hash WHERE e.dst_hash=n.node_hash AND e.kind='{calls}' AND caller.is_test=1) ORDER BY p.path,n.line,n.id",
            paging::nodes_with_path("n", options.detail, "p.path"),
            sig = NodeDetailKey::SIGNATURE,
            function = NodeKind::Function.as_str(),
            method = NodeKind::Method.as_str(),
            calls = EdgeKind::Calls.as_str(),
        ),
        &[&scope, &lower, &upper],
        options,
    )?;
    Ok(json!({"nodes":nodes,"meaning":"no direct indexed test caller; not a runtime coverage proof"}))
}

pub(super) fn implementations_paged(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let selected = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    let id = selected["id"].as_str().context("selected node has no id")?;
    let kind = selected["kind"]
        .as_str()
        .context("selected node has no kind")?;
    let is_type = kind == "record"
        || matches!(
            NodeKind::parse(kind),
            Some(
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Interface
                    | NodeKind::Enum
                    | NodeKind::Type
            )
        );
    anyhow::ensure!(is_type, "implementations selector must identify a type");
    let sql = format!(
        "WITH RECURSIVE descendants(hash) AS (SELECT node_hash FROM nodes WHERE id=?1 UNION SELECT e.src_hash FROM descendants d JOIN edges e ON e.dst_hash=d.hash JOIN nodes child ON child.node_hash=e.src_hash WHERE e.kind IN('{inherits}','{implements}','{extends}') AND child.kind IN('{class}','{struct_k}','{trait_k}','{interface}','{enum_k}','record','{type_k}')) SELECT {} FROM descendants d JOIN nodes n ON n.node_hash=d.hash JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id<>?1 ORDER BY p.path,n.line,n.id",
        paging::nodes_with_path("n", options.detail, "p.path"),
        inherits = EdgeKind::Inherits.as_str(),
        implements = EdgeKind::Implements.as_str(),
        extends = EdgeKind::Extends.as_str(),
        class = NodeKind::Class.as_str(),
        struct_k = NodeKind::Struct.as_str(),
        trait_k = NodeKind::Trait.as_str(),
        interface = NodeKind::Interface.as_str(),
        enum_k = NodeKind::Enum.as_str(),
        type_k = NodeKind::Type.as_str(),
    );
    Ok(json!({"selector":id,"nodes":paging::query(conn, &sql, &[&id], options)?}))
}
