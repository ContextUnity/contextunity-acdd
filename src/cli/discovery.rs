use crate::format::{file_uri_for_path, DiscoveryKind};
use acdd_core::response::QueryOptions;
use acdd_core::subsystems::SubsystemId;
use acdd_index::{
    db::reader::{self, OverviewAspect},
    scanner::{self, Adapter},
};
use acdd_state::types::RunStatus;
use acdd_state::usecases::{ListReq, StateQuery, StateQueryOutput};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// Composes indexed, scanner-owned, and state-owned workspace evidence.
///
/// This is an adapter-level composition point: the database reader stays read-only
/// and never invokes scanners or mutating state commands.
pub(crate) fn overview(
    conn: &Connection,
    root: &Path,
    adapter: Option<&Adapter>,
    aspects: Option<&[String]>,
    page: &QueryOptions,
) -> Result<Value> {
    let mut result =
        reader::overview_with_options(conn, &reader::OverviewOptions { aspects, page })?;
    if !includes_discovery(aspects) {
        return Ok(result);
    }

    let generation = result["generation"].as_str().unwrap_or_default().to_owned();
    let mut discovery = serde_json::Map::new();
    discovery.insert("workspace_root".into(), json!(root.display().to_string()));

    let manifests = match adapter {
        Some(adapter) => scanner::discovery::manifest_components(root, adapter, page, &generation)
            .unwrap_or_else(|error| unavailable("manifest_components", &error.to_string())),
        None => unavailable("manifest_components", "workspace adapter is unavailable"),
    };
    discovery.insert("manifest_components".into(), manifests);

    let hubs = if adapter.is_some() {
        reader::documentation_hubs(conn, page)?
    } else {
        unavailable("documentation_hubs", "workspace adapter is unavailable")
    };
    discovery.insert("documentation_hubs".into(), hubs);
    discovery.insert("active_runs".into(), active_runs(root, adapter, page));

    result["workspace_root"] = json!(root.display().to_string());
    result["discovery"] = Value::Object(discovery);
    Ok(result)
}

pub(super) fn includes_discovery(aspects: Option<&[String]>) -> bool {
    match aspects {
        None | Some([]) => true,
        Some(aspects) => aspects.iter().any(|aspect| {
            aspect
                .parse::<OverviewAspect>()
                .is_ok_and(|a| a.is_discovery() || a == OverviewAspect::Components)
        }),
    }
}

fn unavailable(field: &str, cause: &str) -> Value {
    json!({"availability":"unavailable","cause":cause,"items":[],"field":field})
}

fn active_runs(root: &Path, adapter: Option<&Adapter>, page: &QueryOptions) -> Value {
    let owned_adapter;
    let cfg = match adapter {
        Some(a) => a,
        None => match scanner::load_adapter(root, None) {
            Ok(a) => {
                owned_adapter = a;
                &owned_adapter
            }
            Err(_) => {
                return unavailable(
                    "active_runs",
                    "workspace adapter unavailable for state query",
                );
            }
        },
    };
    if !cfg.modules.is_subsystem_enabled(SubsystemId::State) {
        return unavailable("active_runs", "state module is disabled in acdd.yaml");
    }
    let query = StateQuery::List(ListReq {
        workspace: None,
        status: Some(RunStatus::Active),
        include_tombstones: false,
        limit: Some(page.limit.clamp(1, 100)),
        offset: page.offset,
        generation: None,
    });
    let Ok(providers) = crate::state_bridge::build_domain_providers(root, cfg, None, Vec::new())
    else {
        return unavailable("active_runs", "domain providers unavailable");
    };
    match crate::state_bridge::execute_state_query(root, root, None, None, cfg, providers, query) {
        Ok(StateQueryOutput::List(resp)) => {
            let items: Vec<Value> = resp
                .page
                .runs
                .iter()
                .map(|run| {
                    json!({
                        "run_id": run.run_id.as_str(),
                        "run_name": run.run_name.as_str(),
                        "collection": run.collection,
                        "profile_id": run.profile_id,
                        "isolation": run.isolation.as_str(),
                        "spec_path": run.spec_path,
                        "status": run.status.as_str(),
                        "worktree": run.worktree,
                        "created_at": run.created_at,
                    })
                })
                .collect();
            let returned = items.len();
            let total = resp.page.total_runs;
            json!({
                "availability": "available",
                "total": total,
                "offset": resp.page.offset,
                "limit": resp.page.limit,
                "items": items,
                "omitted": total.saturating_sub(resp.page.offset.saturating_add(returned)),
                "continuation_available": resp.page.offset.saturating_add(returned) < total,
                "generation": Value::Null,
            })
        }
        Ok(_) => unavailable("active_runs", "unexpected state query response"),
        Err(err) => unavailable("active_runs", &err.to_string()),
    }
}

fn attach_manifest_uris(root: &Path, adapter: &Adapter, page: &mut Value) {
    let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let workspace = item["workspace"].as_str().unwrap_or("primary").to_owned();
        if let Some(relative) = item["root"].as_str() {
            if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative) {
                if let Some(uri) = file_uri_for_path(&absolute, None) {
                    item["root_uri"] = json!(uri);
                }
            }
        }
        if let Some(relative) = item["manifest_path"].as_str() {
            if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative) {
                if let Some(uri) = file_uri_for_path(&absolute, None) {
                    item["manifest_uri"] = json!(uri);
                }
            }
        }
    }
}

fn resolve_component_path(
    root: &Path,
    adapter: &Adapter,
    workspace: &str,
    relative: &str,
) -> Option<std::path::PathBuf> {
    if relative.is_empty() || relative == "." {
        if workspace == "primary" {
            return scanner::canonical_root(root).ok();
        }
        return adapter
            .linked_workspaces
            .iter()
            .find(|linked| linked.name == workspace)
            .and_then(|linked| scanner::canonical_root(&linked.path).ok());
    }
    let selector = if workspace == "primary" {
        relative.to_owned()
    } else {
        format!("[{workspace}]/{relative}")
    };
    scanner::resolve_file_path(root, adapter, &selector).ok()
}

fn attach_document_uris(root: &Path, adapter: &Adapter, page: &mut Value) {
    let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let Some(path) = item.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Ok(absolute) = scanner::resolve_file_path(root, adapter, path) else {
            continue;
        };
        let line = item
            .get("line")
            .and_then(Value::as_i64)
            .and_then(|line| usize::try_from(line).ok());
        if let Some(uri) = file_uri_for_path(&absolute, line) {
            item["uri"] = json!(uri);
        }
    }
}

/// Adds workspace-resolved URIs to documentation coordinates before Markdown rendering.
pub(crate) fn attach_document_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
    title: &str,
) {
    match title {
        "search_docs" => attach_page_uris(root, adapter, value.pointer_mut("/sections")),
        "get_doc" => {
            for field in ["sections", "referenced_symbols"] {
                attach_page_uris(root, adapter, value.get_mut(field));
            }
            for field in ["cites_doc", "cited_by_doc"] {
                attach_page_uris_without_line(root, adapter, value.get_mut(field));
            }
            if let Some(items) = value
                .pointer_mut("/referenced_symbol_candidates/items")
                .and_then(Value::as_array_mut)
            {
                for candidate in items {
                    for entity in ["source", "target"] {
                        if let Some(entity) = candidate.get_mut(entity) {
                            attach_uri_from_fields(root, adapter, entity, "path", "line");
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn attach_page_uris(root: &Path, adapter: &Adapter, page: Option<&mut Value>) {
    let Some(items) = page
        .and_then(|page| page.get_mut("items"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
    }
}

fn attach_page_uris_without_line(root: &Path, adapter: &Adapter, page: Option<&mut Value>) {
    let Some(items) = page
        .and_then(|page| page.get_mut("items"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        let Some(path) = item.get("path").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let Ok(absolute) = scanner::resolve_file_path(root, adapter, &path) else {
            continue;
        };
        if let Some(uri) = file_uri_for_path(&absolute, None) {
            item["uri"] = json!(uri);
        }
    }
}

/// Adds absolute coordinates for Markdown presentation after canonical path resolution.
pub(crate) fn attach_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
    kind: DiscoveryKind,
) {
    match kind {
        DiscoveryKind::Overview => {
            if !value.get("workspace_root").is_some_and(Value::is_string) {
                value["workspace_root"] = json!(root.display().to_string());
            }
            if let Some(manifests) = value.pointer_mut("/discovery/manifest_components") {
                attach_manifest_uris(root, adapter, manifests);
            }
            if let Some(hubs) = value.pointer_mut("/discovery/documentation_hubs") {
                attach_document_uris(root, adapter, hubs);
            }
            for key in [
                "/discovery/active_runs/items",
                "/discovery/active_milestones/items",
            ] {
                if let Some(items) = value.pointer_mut(key).and_then(Value::as_array_mut) {
                    for item in items {
                        if let Some(path) = item
                            .get("spec_path")
                            .or_else(|| item.get("path"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                        {
                            attach_uri(root, adapter, item, &path, "line");
                        }
                    }
                }
            }
        }
        DiscoveryKind::Inspect => attach_inspect_uris(root, adapter, value),
        DiscoveryKind::Search => attach_search_uris(root, adapter, value),
        DiscoveryKind::Ast => attach_ast_uris(root, adapter, value),
    }
}

/// Adds workspace-resolved URIs to lint violation coordinates before Markdown rendering.
pub(crate) fn attach_lint_presentation_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
        if let Some(rule_source) = item.get_mut("rule_source") {
            attach_uri_from_fields(root, adapter, rule_source, "path", "line");
        }
    }
}

/// Adds guarded file links for supplied traversal coordinates in Markdown output.
pub(crate) fn attach_traversal_presentation_uris(
    root: &Path,
    adapter: &Adapter,
    value: &mut Value,
) {
    if let Some(selector) = value.get_mut("selector") {
        attach_uri_from_fields(root, adapter, selector, "path", "line");
    }
    if let Some(node) = value.get_mut("node") {
        attach_uri_from_fields(root, adapter, node, "path", "line");
    }

    for field in ["nodes", "changed_symbols", "tests", "unmapped_changes"] {
        if let Some(page) = value.get_mut(field) {
            attach_traversal_page(root, adapter, page, "line");
        }
    }
    for field in ["incoming", "outgoing"] {
        if let Some(page) = value.get_mut(field) {
            if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    attach_uri_from_fields(root, adapter, item, "path", "line");
                }
            }
        }
    }
    if let Some(page) = value.get_mut("affected_docs") {
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", "__no_line");
            }
        }
    }
    if let Some(page) = value.get_mut("documents") {
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", "doc_line");
            }
        }
    }
    for field in [
        "analysis_gaps",
        "untested_public_surface",
        "errors",
        "resolution",
        "external_imports",
        "doc_diagnostics",
        "diagnostics",
        "top_files",
        "resolution_statuses",
        "resolution_causes",
        "parse_error_languages",
        "dead_tails",
    ] {
        if let Some(page) = value.get_mut(field) {
            attach_traversal_page(root, adapter, page, "line");
        }
    }
    if let Some(page) = value.get_mut("phantom_tested") {
        attach_traversal_page(root, adapter, page, "line");
        if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                if let Some(callers) = item.get_mut("test_callers").and_then(Value::as_array_mut) {
                    for caller in callers {
                        attach_uri_from_fields(root, adapter, caller, "path", "line");
                    }
                }
            }
        }
    }
    if let Some(page) = value.get_mut("dangling_calls") {
        attach_traversal_page(root, adapter, page, "__no_line");
    }
    if let Some(invariants) = value.get_mut("governing_invariants") {
        if let Some(rules) = invariants.get_mut("direct").and_then(Value::as_array_mut) {
            for rule in rules {
                if let Some(source) = rule.get_mut("source") {
                    attach_uri_from_fields(root, adapter, source, "path", "line");
                }
            }
        }
        if let Some(entries) = invariants
            .get_mut("omitted_sources")
            .and_then(Value::as_array_mut)
        {
            for entry in entries {
                if let Some(source) = entry.get_mut("source") {
                    attach_uri_from_fields(root, adapter, source, "path", "line");
                }
            }
        }
    }
    if let Some(changes) = value.get_mut("changes") {
        if let Some(files) = changes.get_mut("files").and_then(Value::as_array_mut) {
            for file in files {
                attach_uri_from_fields(root, adapter, file, "path", "__no_line");
            }
        }
    }
    if let Some(summary) = value.get_mut("summary") {
        if let Some(calls) = summary.get_mut("calls") {
            for direction in ["inbound", "outbound"] {
                if let Some(preview) = calls
                    .get_mut(direction)
                    .and_then(|direction| direction.get_mut("preview"))
                    .and_then(Value::as_array_mut)
                {
                    for item in preview {
                        attach_uri_from_fields(root, adapter, item, "path", "line");
                    }
                }
            }
        }
    }
}

fn attach_traversal_page(root: &Path, adapter: &Adapter, page: &mut Value, line: &str) {
    if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            attach_uri_from_fields(root, adapter, item, "path", line);
            if let Some(edge) = item.get_mut("edge") {
                attach_uri_from_fields(root, adapter, edge, "path", "line");
            }
            if let Some(path_nodes) = item
                .pointer_mut("/connection/verified_call_path_nodes")
                .and_then(Value::as_array_mut)
            {
                for node in path_nodes {
                    attach_uri_from_fields(root, adapter, node, "path", "line");
                }
            }
        }
    }
}

fn attach_inspect_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    attach_uri_from_fields(root, adapter, &mut value["node"], "path", "line");
    if let Some(summary) = value.get_mut("summary") {
        attach_uri_from_fields(root, adapter, &mut summary["container"], "path", "line");
        for direction in ["inbound", "outbound"] {
            if let Some(items) = summary["calls"][direction]["preview"].as_array_mut() {
                for item in items {
                    attach_uri_from_fields(root, adapter, item, "path", "line");
                }
            }
        }
    }
    for rule in value["governing_invariants"]["direct"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        attach_uri_from_fields(root, adapter, &mut rule["source"], "path", "line");
    }
    for source in value["governing_invariants"]["omitted_sources"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        attach_uri_from_fields(root, adapter, &mut source["source"], "path", "line");
    }
    for (field, line) in [
        ("documented_in", "doc_line"),
        ("analysis_gaps", "line"),
        ("documents", "doc_line"),
    ] {
        if let Some(items) = value[field]["items"].as_array_mut() {
            for item in items {
                attach_uri_from_fields(root, adapter, item, "path", line);
            }
        }
    }
}

fn attach_search_uris(root: &Path, adapter: &Adapter, value: &mut Value) {
    if let Some(items) = value["nodes"]["items"].as_array_mut() {
        for item in items {
            attach_uri_from_fields(root, adapter, item, "path", "line");
            attach_search_pulse_uris(root, adapter, item);
        }
    }
    if let Some(groups) = value["nodes"]["grouped_by_file"].as_object_mut() {
        for (path, items) in groups {
            if let Some(items) = items.as_array_mut() {
                for item in items {
                    attach_uri(root, adapter, item, path, "line");
                    attach_search_pulse_uris(root, adapter, item);
                }
            }
        }
    }
}

fn attach_search_pulse_uris(root: &Path, adapter: &Adapter, item: &mut Value) {
    if let Some(callers) = item["graph_pulse"]["callers"]["preview"].as_array_mut() {
        for caller in callers {
            attach_uri_from_fields(root, adapter, caller, "path", "line");
        }
    }
    if let Some(tests) = item["graph_pulse"]["covering_tests"]["items"].as_array_mut() {
        for test in tests {
            attach_uri_from_fields(root, adapter, test, "path", "line");
        }
    }
}

fn attach_uri_from_fields(
    root: &Path,
    adapter: &Adapter,
    item: &mut Value,
    path_field: &str,
    line_field: &str,
) {
    let Some(path) = item
        .get(path_field)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    attach_uri(root, adapter, item, &path, line_field);
}

fn attach_uri(root: &Path, adapter: &Adapter, item: &mut Value, path: &str, line_field: &str) {
    let Ok(absolute) = scanner::resolve_file_path(root, adapter, path) else {
        return;
    };
    let line = item
        .get(line_field)
        .and_then(|line| {
            line.as_i64()
                .or_else(|| line.as_u64().and_then(|line| i64::try_from(line).ok()))
        })
        .and_then(|line| usize::try_from(line).ok());
    if let Some(uri) = file_uri_for_path(&absolute, line) {
        item["uri"] = json!(uri);
    }
}

pub(crate) fn attach_ast_uris(root: &Path, adapter: &Adapter, result: &mut Value) {
    let Some(matches) = result.get_mut("matches") else {
        return;
    };
    let items = match matches {
        Value::Array(items) => Some(items),
        Value::Object(page) => page.get_mut("items").and_then(Value::as_array_mut),
        _ => None,
    };
    let Some(items) = items else { return };
    for item in items {
        attach_uri_from_fields(root, adapter, item, "path", "line");
    }
}
