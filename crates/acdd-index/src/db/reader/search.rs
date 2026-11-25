use super::super::commitments::{read_metadata, read_metadata_optional, MetadataKey};
use super::super::{cycles, paging};
use super::diagnostics::{diagnostic_reason, is_query_budget_exhaustion, resolution_cause_sql};
use super::sql::rows;
use super::types::{OverviewAspect, OverviewOptions};
use crate::models::details::NodeDetailKey;
use crate::models::tags::{CoverageStatus, EdgeKind, NodeKind};
use acdd_core::response::QueryOptions;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
struct OverviewPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    topology: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoints: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    languages: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_by_workspace: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compiled_profiles: Option<BTreeSet<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycles: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Vec<Value>>,
    generation: String,
}

/// Read a bounded overview of selected workspace sections.
pub fn overview_with_options(conn: &Connection, request: &OverviewOptions<'_>) -> Result<Value> {
    let options = request.page;
    let generation = paging::generation(conn, options)?;
    let selected: BTreeSet<OverviewAspect> = match request.aspects {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|raw| raw.parse::<OverviewAspect>())
            .collect::<Result<_>>()?,
        _ => OverviewAspect::ALL
            .iter()
            .copied()
            .filter(OverviewAspect::is_default_selected)
            .collect(),
    };
    let has_aspect = |aspect: OverviewAspect| selected.contains(&aspect);

    let topology = has_aspect(OverviewAspect::Topology)
        .then(|| module_topology(conn, options, &generation))
        .transpose()?;
    let entrypoints =
        (has_aspect(OverviewAspect::Entrypoints) || has_aspect(OverviewAspect::EntrypointsWithTests))
            .then(|| {
                paging::query(
                    conn,
                    &format!(
                        "SELECT json_extract(n.details,'$.{entrypoint}') category,n.id,n.kind,n.name,n.qualname,p.path,n.line AS start_line,json_extract(n.details,'$.{signature}') signature,(SELECT count(*) FROM edges e WHERE e.src_hash=n.node_hash AND e.kind='{calls}') outgoing_calls FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE json_extract(n.details,'$.{entrypoint}') IS NOT NULL AND (?1 OR json_extract(n.details,'$.{entrypoint}') <> 'test') ORDER BY category,outgoing_calls DESC,n.id ASC",
                        entrypoint = NodeDetailKey::ENTRYPOINT, signature = NodeDetailKey::SIGNATURE, calls = EdgeKind::Calls.as_str(),
                    ),
                    &[&has_aspect(OverviewAspect::EntrypointsWithTests)],
                    options,
                )
            })
            .transpose()?;
    let components = has_aspect(OverviewAspect::Components)
        .then(|| {
            paging::query(
                conn,
                &format!("SELECT n.id,n.name,p.path FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.kind='{}' ORDER BY p.path,n.id", NodeKind::Component.as_str()),
                &[],
                options,
            )
        })
        .transpose()?;
    let counts = has_aspect(OverviewAspect::Counts)
        .then(|| {
            Ok::<_, anyhow::Error>(
                rows(
                    conn,
                    &format!(
                        "SELECT (SELECT count(*) FROM files) files,(SELECT count(*) FROM nodes) nodes,(SELECT count(*) FROM edges) edges,(SELECT count(*) FROM doc_sections) doc_sections,(SELECT count(*) FROM errors) parse_errors,(SELECT count(*) FROM resolution_coverage WHERE status IN('{unresolved}','{ambiguous}')) unresolved,(SELECT count(*) FROM resolution_coverage WHERE status='{external}') external_imports",
                        unresolved = CoverageStatus::Unresolved.as_str(), ambiguous = CoverageStatus::Ambiguous.as_str(), external = CoverageStatus::External.as_str(),
                    ),
                    &[],
                    1,
                )?
                .remove(0),
            )
        })
        .transpose()?;
    let languages = if has_aspect(OverviewAspect::Languages) {
        let (island, resolved, ambiguous, unresolved, external) = (
            NodeKind::IslandScope.as_str(),
            CoverageStatus::Resolved.as_str(),
            CoverageStatus::Ambiguous.as_str(),
            CoverageStatus::Unresolved.as_str(),
            CoverageStatus::External.as_str(),
        );
        let lang_sql = format!(
            "SELECT l.language,coalesce(fc.files,0) files,coalesce(r.resolved,0) resolved,coalesce(r.ambiguous,0) ambiguous,coalesce(r.unresolved,0) unresolved,coalesce(r.external_imports,0) external_imports,coalesce(e.parse_errors,0) parse_errors FROM (SELECT language FROM files UNION SELECT language FROM nodes WHERE kind='{island}' UNION SELECT language FROM coverage_owner_language) l LEFT JOIN (SELECT language,count(*) files FROM files GROUP BY language) fc ON fc.language=l.language LEFT JOIN (SELECT language,sum(CASE WHEN status='{resolved}' THEN records ELSE 0 END) resolved,sum(CASE WHEN status='{ambiguous}' THEN records ELSE 0 END) ambiguous,sum(CASE WHEN status='{unresolved}' THEN records ELSE 0 END) unresolved,sum(CASE WHEN status='{external}' THEN records ELSE 0 END) external_imports FROM coverage_language_counts GROUP BY language) r ON r.language=l.language LEFT JOIN (SELECT f.language,count(*) parse_errors FROM errors er JOIN files f ON f.path=er.path GROUP BY f.language) e ON e.language=l.language ORDER BY l.language"
        );
        let language_total = paging::count(conn, &format!("SELECT count(*) FROM (SELECT language FROM files UNION SELECT language FROM nodes WHERE kind='{island}' UNION SELECT language FROM coverage_owner_language)"), &[])?;
        let (limit, offset) = (options.limit as i64, options.offset as i64);
        let languages = rows(
            conn,
            &format!("{lang_sql} LIMIT ?1 OFFSET ?2"),
            &[&limit, &offset],
            options.limit,
        )?;
        Some(paging::value(
            languages,
            language_total,
            options,
            &generation,
        ))
    } else {
        None
    };
    let coverage_by_workspace = has_aspect(OverviewAspect::CoverageByWorkspace)
        .then(|| workspace_coverage(conn, options, &generation))
        .transpose()?;
    let compiled_profiles = has_aspect(OverviewAspect::CompiledProfiles)
        .then(|| crate::languages::profiles().map(|p| p.id()).collect());
    let cycles = has_aspect(OverviewAspect::Cycles)
        .then(|| cycles::summary(conn, None))
        .transpose()?;
    let metadata = has_aspect(OverviewAspect::Metadata)
        .then(|| {
            rows(
                conn,
                "SELECT key,value FROM metadata WHERE key IN(?1,?2) ORDER BY key",
                &[
                    &MetadataKey::SchemaVersion.as_str(),
                    &MetadataKey::WorkspaceRoot.as_str(),
                ],
                2,
            )
        })
        .transpose()?;
    Ok(serde_json::to_value(OverviewPayload {
        topology,
        entrypoints,
        components,
        counts,
        languages,
        coverage_by_workspace,
        compiled_profiles,
        cycles,
        metadata,
        generation,
    })?)
}

/// Read a bounded overview scoped to one workspace-relative directory or file prefix.
pub fn overview_for_path(
    conn: &Connection,
    raw_path: &str,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let norm = raw_path
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/');
    anyhow::ensure!(
        !norm.is_empty() && !norm.split('/').any(|s| s == ".."),
        "invalid overview path '{raw_path}'; expected a workspace-relative path"
    );
    let (start, end) = super::selectors::path_bounds(norm);
    let cond = "p.path=?1 OR (p.path>=?2 AND p.path<?3)";
    let fcond = "path=?1 OR (path>=?2 AND path<?3)";
    let counts = rows(
        conn,
        &format!(
            "SELECT (SELECT count(*) FROM files WHERE {fcond}) files,(SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE {cond}) nodes,(SELECT count(*) FROM edges e JOIN path_dictionary p ON p.path_id=e.path_id WHERE {cond}) edges,(SELECT count(*) FROM doc_sections WHERE {fcond}) doc_sections,(SELECT count(*) FROM errors WHERE {fcond}) parse_errors,(SELECT count(*) FROM resolution_coverage rc JOIN path_dictionary p ON p.path_id=rc.path_id WHERE ({cond}) AND rc.status IN('{}','{}')) unresolved,(SELECT count(*) FROM resolution_coverage rc JOIN path_dictionary p ON p.path_id=rc.path_id WHERE ({cond}) AND rc.status='{}') external_imports",
            CoverageStatus::Unresolved.as_str(), CoverageStatus::Ambiguous.as_str(), CoverageStatus::External.as_str(),
        ),
        &[&norm, &start, &end],
        1,
    )?.remove(0);
    let languages = paging::query(conn, "SELECT language,count(*) files FROM files WHERE path=?1 OR (path>=?2 AND path<?3) GROUP BY language ORDER BY files DESC,language", &[&norm, &start, &end], options)?;
    let files = paging::query(conn, "SELECT f.path,f.language,(SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=f.path) symbols FROM files f WHERE f.path=?1 OR (f.path>=?2 AND f.path<?3) ORDER BY f.path", &[&norm, &start, &end], options)?;
    Ok(
        json!({"path": norm, "counts": counts, "languages": languages, "files": files, "generation": generation}),
    )
}

// Compute a module once for each path and aggregate graph statistics in SQLite.
const TOPOLOGY_CTES: &str = r#"
WITH RECURSIVE
parts(path_id,component,rest,module,depth) AS (
 SELECT DISTINCT n.path_id,n.kind='component',p.path,'',0
 FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id
 UNION ALL
 SELECT path_id,component,
   CASE WHEN instr(rest,'/')=0 THEN '' ELSE substr(rest,instr(rest,'/')+1) END,
   CASE WHEN substr(rest,1,CASE WHEN instr(rest,'/')=0 THEN length(rest) ELSE instr(rest,'/')-1 END)=''
              OR (component=0 AND instr(rest,'/')=0) THEN module
        ELSE module || CASE WHEN module='' THEN '' ELSE '/' END ||
             substr(rest,1,CASE WHEN instr(rest,'/')=0 THEN length(rest) ELSE instr(rest,'/')-1 END) END,
   depth + CASE WHEN substr(rest,1,CASE WHEN instr(rest,'/')=0 THEN length(rest) ELSE instr(rest,'/')-1 END)<>''
                   AND (component=1 OR instr(rest,'/')>0) THEN 1 ELSE 0 END
 FROM parts WHERE rest<>'' AND (depth<2 OR (module='src/engine' AND depth<3))
),
path_modules AS MATERIALIZED (
 SELECT path_id,component,module FROM parts
 WHERE rest='' OR (depth>=2 AND (module<>'src/engine' OR depth>=3))
),
mapped AS MATERIALIZED (
 SELECT n.node_hash,n.kind,json_extract(n.details,'$.entrypoint') AS entrypoint,pm.module
 FROM nodes n JOIN path_modules pm ON pm.path_id=n.path_id AND pm.component=(n.kind='component')
),
edge_kinds AS MATERIALIZED (
 SELECT s.module AS source,t.module AS target,e.kind,count(*) AS count
 FROM edges e JOIN mapped s ON s.node_hash=e.src_hash JOIN mapped t ON t.node_hash=e.dst_hash
 GROUP BY s.module,t.module,e.kind
),
node_stats AS (
 SELECT module,count(*) AS symbols,sum(kind='table') AS tables,count(entrypoint) AS entrypoints,
 sum(CASE WHEN entrypoint IN ('mcp_tool','cli_command','http_route') THEN 1 ELSE 0 END) AS adapters
 FROM mapped GROUP BY module
),
source_stats AS (
 SELECT source AS module,sum(CASE WHEN source=target THEN count ELSE 0 END) AS internal,
 sum(CASE WHEN source<>target THEN count ELSE 0 END) AS outbound,
 sum(CASE WHEN kind IN ('queries_table','mutates_table') THEN count ELSE 0 END) AS table_queries
 FROM edge_kinds GROUP BY source
),
target_stats AS (SELECT target AS module,sum(count) AS inbound FROM edge_kinds WHERE source<>target GROUP BY target),
neighbor_pairs AS (
 SELECT source AS module,target AS neighbor FROM edge_kinds WHERE source<>target
 UNION SELECT target,source FROM edge_kinds WHERE source<>target
),
neighbors AS (SELECT module,count(*) AS total FROM neighbor_pairs GROUP BY module),
stats AS (
 SELECT n.*,coalesce(s.internal,0) AS internal,coalesce(s.outbound,0) AS outbound,
 coalesce(s.table_queries,0) AS table_queries,coalesce(t.inbound,0) AS inbound,
 coalesce(neighbors.total,0) AS neighbor_count
 FROM node_stats n LEFT JOIN source_stats s ON s.module=n.module
 LEFT JOIN target_stats t ON t.module=n.module LEFT JOIN neighbors ON neighbors.module=n.module
)
"#;

fn module_topology(conn: &Connection, options: &QueryOptions, _generation: &str) -> Result<Value> {
    let modules_sql = format!(
        "{TOPOLOGY_CTES}
      SELECT module,
        CASE WHEN adapters>0 OR (inbound=0 AND outbound>0) THEN 'adapter_or_entrypoint'
             WHEN outbound=0 AND inbound>0 THEN 'foundation'
             WHEN (tables>0 AND tables*10>=symbols) OR table_queries>=3 THEN 'data_access'
             WHEN inbound>=3 AND outbound>=3 AND neighbor_count>=3 THEN 'orchestrator'
             ELSE 'domain' END AS role,
        symbols AS symbol_count,tables AS table_count,internal AS internal_edges,
        inbound AS inbound_external_edges,outbound AS outbound_external_edges,
        entrypoints AS entrypoint_count,table_queries AS table_query_count,
        CASE WHEN internal+inbound+outbound=0 THEN 0.0
             ELSE CAST(internal AS REAL)/(internal+inbound+outbound) END AS cohesion_ratio
      FROM stats ORDER BY module"
    );
    let edges_sql = format!(
        "{TOPOLOGY_CTES},
      ranked AS (
        SELECT source,target,kind,count,sum(count) OVER (PARTITION BY source,target) AS total,
        row_number() OVER (PARTITION BY source,target ORDER BY count DESC,kind) AS rank
        FROM edge_kinds WHERE source<>target
      )
      SELECT source AS source_module,target AS target_module,total AS edge_count,
             kind AS dominant_kind FROM ranked WHERE rank=1
      ORDER BY edge_count DESC,source_module,target_module"
    );
    Ok(json!({
        "modules": paging::query(conn, &modules_sql, &[], options)?,
        "module_edges": paging::query(conn, &edges_sql, &[], options)?
    }))
}

/// Pages one indexed entry point per Markdown document for workspace discovery.
///
/// These rows come only from the existing document index; this read path does not
/// scan the filesystem or promote unresolved documentation candidates.
pub fn documentation_hubs(conn: &Connection, options: &QueryOptions) -> Result<Value> {
    paging::query(
        conn,
        "SELECT d.doc_id,d.path,d.section_title,d.doc_type,n.line FROM doc_sections d LEFT JOIN nodes n ON n.id=d.doc_id WHERE d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' AND d.rowid IN (SELECT min(rowid) FROM doc_sections GROUP BY path) ORDER BY d.path,d.doc_id",
        &[],
        options,
    )
}

fn workspace_coverage(
    conn: &Connection,
    options: &QueryOptions,
    generation: &str,
) -> Result<Value> {
    let root = read_metadata(conn, MetadataKey::WorkspaceRoot)?;
    let linked_json = read_metadata_optional(conn, MetadataKey::AdapterLinkedWorkspaces)?
        .unwrap_or_else(|| "[]".into());
    let linked: Value = serde_json::from_str(&linked_json)?;
    let mut scopes = vec![json!({"name":"primary","root":root,"path_prefix":""})];
    if let Some(workspaces) = linked.as_array() {
        for workspace in workspaces {
            let (Some(name), Some(path)) = (
                workspace.get("name").and_then(Value::as_str),
                workspace.get("path").and_then(Value::as_str),
            ) else {
                continue;
            };
            scopes.push(json!({
                "name": name,
                "root": path,
                "path_prefix": format!("[{name}]/")
            }));
        }
    }
    scopes.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    let total = scopes.len();
    let linked_json = serde_json::to_string(&linked)?;
    let mut items = Vec::with_capacity(options.limit.min(total.saturating_sub(options.offset)));
    let sum_statuses = format!(
        "coalesce(sum(CASE WHEN c.status='{resolved}' THEN c.records ELSE 0 END),0) resolved,\
         coalesce(sum(CASE WHEN c.status='{ambiguous}' THEN c.records ELSE 0 END),0) ambiguous,\
         coalesce(sum(CASE WHEN c.status='{unresolved}' THEN c.records ELSE 0 END),0) unresolved,\
         coalesce(sum(CASE WHEN c.status='{external}' THEN c.records ELSE 0 END),0) external_imports",
        resolved = CoverageStatus::Resolved.as_str(),
        ambiguous = CoverageStatus::Ambiguous.as_str(),
        unresolved = CoverageStatus::Unresolved.as_str(),
        external = CoverageStatus::External.as_str(),
    );
    for scope in scopes.into_iter().skip(options.offset).take(options.limit) {
        let prefix = scope["path_prefix"].as_str().unwrap_or_default();
        let counts = if prefix.is_empty() {
            rows(
                conn,
                &format!(
                    "SELECT {sum_statuses} FROM coverage_language_counts c JOIN path_dictionary p ON p.path_id=c.path_id WHERE NOT EXISTS(SELECT 1 FROM json_each(?1) linked WHERE substr(p.path,1,length('['||json_extract(linked.value,'$.name')||']/'))='['||json_extract(linked.value,'$.name')||']/')"
                ),
                &[&linked_json],
                1,
            )?
            .remove(0)
        } else {
            rows(
                conn,
                &format!(
                    "SELECT {sum_statuses} FROM coverage_language_counts c JOIN path_dictionary p ON p.path_id=c.path_id WHERE substr(p.path,1,length(?1))=?1"
                ),
                &[&prefix],
                1,
            )?
            .remove(0)
        };
        let mut item = scope.as_object().cloned().unwrap_or_default();
        for field in [
            CoverageStatus::Resolved.as_str(),
            CoverageStatus::Ambiguous.as_str(),
            CoverageStatus::Unresolved.as_str(),
            "external_imports",
        ] {
            item.insert(field.into(), counts[field].clone());
        }
        item.insert("resolution_health".into(), resolution_health(&counts));
        let reasons = match workspace_diagnostic_reason_counts(conn, prefix, &linked_json) {
            Ok(counts) => json!({"availability":"available","counts":counts}),
            Err(error) if is_query_budget_exhaustion(&error) => json!({
                "availability":"unavailable",
                "cause":"canonical reason aggregation exceeded an existing SQLite query budget"
            }),
            Err(error) => return Err(error),
        };
        item.insert("diagnostic_reasons".into(), reasons);
        items.push(Value::Object(item));
    }
    Ok(paging::value(items, total, options, generation))
}

fn resolution_health(counts: &Value) -> Value {
    let status_count = |status: &str| counts[status].as_u64();
    let Some(resolved) = status_count(CoverageStatus::Resolved.as_str()) else {
        return json!({"availability":"unavailable","cause":"resolution status counts are outside the supported integer range"});
    };
    let Some(unresolved) = status_count(CoverageStatus::Unresolved.as_str()) else {
        return json!({"availability":"unavailable","cause":"resolution status counts are outside the supported integer range"});
    };
    let Some(ambiguous) = status_count(CoverageStatus::Ambiguous.as_str()) else {
        return json!({"availability":"unavailable","cause":"resolution status counts are outside the supported integer range"});
    };
    let Some(denominator) = resolved
        .checked_add(unresolved)
        .and_then(|total| total.checked_add(ambiguous))
    else {
        return json!({"availability":"unavailable","cause":"resolution status counts exceed the supported integer range"});
    };
    if denominator == 0 {
        return json!({"availability":"unavailable","cause":"no in-scope resolution references"});
    }
    json!({
        "availability":"available",
        "resolved":resolved,
        "denominator":denominator,
        "percent":resolved as f64 * 100.0 / denominator as f64
    })
}

fn workspace_diagnostic_reason_counts(
    conn: &Connection,
    prefix: &str,
    linked_json: &str,
) -> Result<Value> {
    let cause = resolution_cause_sql();
    let (sql, parameter) = if prefix.is_empty() {
        (
            format!(
                "SELECT c.status,{cause} cause,x.expression,e.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE NOT EXISTS(SELECT 1 FROM json_each(?1) linked WHERE substr(p.path,1,length('['||json_extract(linked.value,'$.name')||']/'))='['||json_extract(linked.value,'$.name')||']/')"
            ),
            linked_json,
        )
    } else {
        (
            format!(
                "SELECT c.status,{cause} cause,x.expression,e.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence e ON e.evidence_id=c.evidence_id WHERE substr(p.path,1,length(?1))=?1"
            ),
            prefix,
        )
    };

    const REASONS: &[&str] = &[
        "dynamic_callback",
        "unresolved_receiver_type",
        "framework_magic",
        "external_unindexed",
        "ambiguous_candidates",
        "shadowed_binding",
        "unknown",
    ];
    let mut counts: BTreeMap<String, BTreeMap<String, u64>> = REASONS
        .iter()
        .map(|reason| {
            (
                (*reason).to_owned(),
                CoverageStatus::ALL
                    .iter()
                    .map(|status| (status.as_str().to_owned(), 0))
                    .collect(),
            )
        })
        .collect();

    let budget = super::sql::QueryBudget::new(conn);
    let mut statement = conn.prepare(&sql)?;
    if !statement.readonly() {
        anyhow::bail!("query must be read-only");
    }
    let mut query = statement.query([parameter])?;
    while let Some(row) = query.next()? {
        budget.check()?;
        let status = row.get_ref(0)?.as_str()?;
        let cause = row.get_ref(1)?.as_str()?;
        let expression = row.get_ref(2)?.as_str()?;
        let evidence = row.get_ref(3)?.as_str()?;
        let reason = diagnostic_reason(Some(status), Some(cause), Some(expression), evidence)
            .or_else(|| (status != CoverageStatus::Resolved.as_str()).then_some("unknown"));
        if let Some(reason) = reason {
            let count = counts
                .entry(reason.to_owned())
                .or_default()
                .entry(status.to_owned())
                .or_default();
            *count = count.checked_add(1).ok_or_else(|| {
                anyhow::anyhow!(
                    "diagnostic reason reference count exceeds the supported integer range"
                )
            })?;
        }
    }
    budget.check()?;
    Ok(json!(counts))
}
