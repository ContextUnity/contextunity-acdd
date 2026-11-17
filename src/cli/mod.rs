/// Implements ast support.
pub mod ast;
pub(crate) mod docs_check;
pub(crate) mod discovery {
    use crate::format::{file_uri_for_path, DiscoveryKind};
    use acdd_core::response::QueryOptions;
    use acdd_index::{
        db::reader,
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
            Some(adapter) => {
                scanner::discovery::manifest_components(root, adapter, page, &generation)
                    .unwrap_or_else(|error| unavailable("manifest_components", &error.to_string()))
            }
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

    fn includes_discovery(aspects: Option<&[String]>) -> bool {
        match aspects {
            None => true,
            Some([]) => true,
            Some(aspects) => aspects.iter().any(|aspect| {
                matches!(
                    aspect.to_ascii_lowercase().as_str(),
                    "components"
                        | "discovery"
                        | "manifest_components"
                        | "documentation_hubs"
                        | "runs"
                        | "milestones"
                )
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
        if !cfg.modules.is_enabled("state") {
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
        match crate::state_bridge::execute_state_query(root, root, None, cfg, query) {
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
                if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative)
                {
                    if let Some(uri) = file_uri_for_path(&absolute, None) {
                        item["root_uri"] = json!(uri);
                    }
                }
            }
            if let Some(relative) = item["manifest_path"].as_str() {
                if let Some(absolute) = resolve_component_path(root, adapter, &workspace, relative)
                {
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
                for key in ["/discovery/active_runs/items", "/discovery/active_milestones/items"] {
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
        ] {
            if let Some(page) = value.get_mut(field) {
                attach_traversal_page(root, adapter, page, "line");
            }
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
}
/// Implements guide support.
pub mod guide;
mod paths;
/// Sends a reload signal to running MCP servers.
pub mod reload;
/// Fail-fast ACDD profile and spec validation.
pub mod validate;

use crate::{
    engine::pipeline as writer,
    format::{self, OperationKind, OutputFormat},
    state_bridge::{execute_state_command, execute_state_query, InvocationSettings},
};
use acdd_core::response::{
    CoverageOptions, Detail, QueryOptions, ResponsePolicy, SourceOptions, MAX_PAGE_SIZE,
};
use acdd_index::db::{reader, symbols, traversal};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_state::types::{
    BlackboardStatus, BlackboardTopic, IntegrateStrategy, RejectionFindings, ReviewEvidence,
    RunStatus, SubmitAction, SubtaskStatus,
};
use acdd_state::usecases::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, ClaimReq, ClearReq, DraftReq,
    ExtendScopeReq, IntegrateReq, ListReq, NextReq, ReopenReq, RollbackReq, RunReq, ShowReq,
    SnapshotReq, StartReq, StateCommand, StateQuery, StatusReq, SubmitReq, SubtaskReq, SyncReq,
    VerifyReq,
};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(
    name = "acdd",
    version,
    about = "Native code graph, documentation engine, unified state engine, MCP server and CLI"
)]
/// Top-level CLI parser.
pub struct Cli {
    #[arg(long, global = true, env = "ACDD_WORKSPACE_ROOT")]
    /// Optional workspace root override.
    pub root: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_DB")]
    /// Optional code-map SQLite database path override.
    pub db: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_STATE_DB")]
    /// Optional state SQLite database path override.
    pub state_db: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_CONFIG")]
    /// Explicit root-confined ACDD configuration file.
    pub config: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_PROFILE")]
    /// Explicit gate profile reference or root-confined profile path.
    pub profile: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value = "markdown")]
    /// Output format: markdown (default) or json.
    pub format: CliOutputFormat,
    #[command(subcommand)]
    /// Selected subcommand (defaults to `serve` when omitted).
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
/// Output representation selected for command results.
pub enum CliOutputFormat {
    #[default]
    /// Human-readable Markdown, selected when `--format` is omitted.
    Markdown,
    /// JSON for callers that parse stdout.
    Json,
}

impl From<CliOutputFormat> for OutputFormat {
    fn from(value: CliOutputFormat) -> Self {
        match value {
            CliOutputFormat::Markdown => Self::Markdown,
            CliOutputFormat::Json => Self::Json,
        }
    }
}

#[derive(Subcommand, Debug)]
/// Flat `0.6.0` CLI subcommand surface.
pub enum Command {
    /// Initialize `acdd.yaml`, `.acdd/profile.yaml`, and canonical `.gitignore` rules.
    Init {
        /// Overwrite existing `acdd.yaml` and `.acdd/profile.yaml`.
        #[arg(long)]
        force: bool,
    },
    /// Start a new `Run` from a spec file or ephemeral `--goal`.
    Start {
        /// Workspace-relative spec Markdown path (mutually exclusive with `--goal`).
        spec: Option<String>,
        /// Ephemeral goal text materializing `.acdd/run/<run_id>/spec.md`.
        #[arg(long)]
        goal: Option<String>,
        /// Optional explicit run name override.
        #[arg(long)]
        name: Option<String>,
        /// Initial scope globs (for `--goal` ephemeral runs).
        #[arg(long = "scope")]
        scope: Vec<String>,
        /// Target integration base branch/ref for worktree isolation runs.
        #[arg(long)]
        target_base: Option<String>,
        /// Create a managed run worktree under `.worktrees/<run_name>`.
        #[arg(long)]
        worktree: bool,
        /// Force `in_place` isolation for this run.
        #[arg(long)]
        in_place: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree_path: Option<PathBuf>,
    },
    /// Draft the next sequentially numbered spec document in a configured collection.
    Draft {
        /// Collection key from `acdd.yaml` `specs:` (e.g. `milestones`, `plans`).
        collection: String,
        /// URL/file slug for the drafted spec document.
        slug: String,
        /// Optional prefix override (must belong to `collection.prefixes`).
        #[arg(long)]
        prefix: Option<String>,
        /// Optional human-readable title.
        #[arg(long)]
        title: Option<String>,
    },
    /// Inspect status of an active `Run` (read-only).
    Status {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// List active runs and optional closed tombstones (read-only).
    List {
        /// Optional workspace filter.
        #[arg(long)]
        workspace: Option<String>,
        /// Optional status filter (`active | completed | cancelled`).
        #[arg(long)]
        status: Option<String>,
        /// Include closed `run_tombstones`.
        #[arg(long)]
        include_tombstones: bool,
        /// Maximum items per page (`1..=100`).
        #[arg(long)]
        limit: Option<usize>,
        /// Zero-based page offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Optional `runs.generation` pin for stable pagination.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// Show detailed run or node state, subtasks, and budgeted context preview (read-only).
    Show {
        /// Optional target node cursor (`<gate>`, `<task_ref>`, or `<task_ref>.<gate>`).
        node: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// Show ready node cursors and topological execution waves (read-only).
    Next {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional maximum number of ready cursors.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Claim or re-claim a ready/active node and materialize budgeted claim context.
    Claim {
        /// Optional node cursor (omitting auto-selects next ready node).
        node: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Claiming worker identity.
        #[arg(long, alias = "worker-id", default_value = "cli")]
        worker: String,
        /// Optional explicit session fingerprint override.
        #[arg(long)]
        session_fingerprint: Option<String>,
        /// Force take-over of an existing active claim.
        #[arg(long)]
        force: bool,
        /// Explicitly authorize stealing an active claim from another host/remote session.
        #[arg(long)]
        steal_remote: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Run gate verification (`command:`, `call:`, `checks:`, scope barrier) without advancing state.
    Verify {
        /// Target node cursor.
        node: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional claiming worker identity check.
        #[arg(long, alias = "worker-id")]
        worker: Option<String>,
        /// Test target filter passed to `{test_filter}` and `code.seam` (required for `proof: contract`).
        #[arg(long, alias = "test-target")]
        test: Option<String>,
        /// Optional expected `claim_revision`.
        #[arg(long)]
        claim_revision: Option<u32>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Submit `pass` or `reject` for a claimed node.
    Submit {
        /// Target node cursor.
        node: String,
        /// Submission decision (`pass` or `reject`).
        #[arg(long)]
        action: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Submitting worker identity.
        #[arg(long, alias = "worker-id", default_value = "cli")]
        worker: String,
        /// Optional expected `claim_revision`.
        #[arg(long)]
        claim_revision: Option<u32>,
        /// Optional `--test` target forwarded if `submit` auto-triggers `verify`.
        #[arg(long, alias = "test-target")]
        test: Option<String>,
        /// Typed review or custom schema JSON evidence (or `@path` to a confined JSON file).
        #[arg(long)]
        evidence: Option<String>,
        /// Structured rejection findings JSON (or `@path` to a confined JSON file) required for `--action reject`.
        #[arg(long)]
        findings: Option<String>,
        /// Verified invariant IDs attested by this submission.
        #[arg(long = "invariant")]
        invariants: Vec<String>,
        /// Optional human-readable summary for receipt rollups.
        #[arg(long)]
        summary: Option<String>,
        /// Explicitly permit committing onto `main` / `master`.
        #[arg(long)]
        allow_main: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Extend a task's admitted write scope with a non-empty reason.
    ExtendScope {
        /// Target task or task-gate cursor.
        node: String,
        /// Additional workspace-relative paths or globs to admit.
        #[arg(long = "path", required = true)]
        paths: Vec<String>,
        /// Non-empty justification for the scope extension.
        #[arg(long)]
        reason: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional worker identity check.
        #[arg(long, alias = "worker-id")]
        worker: Option<String>,
    },
    /// Reopen a gate or task container (with optional `--cascade`).
    Reopen {
        /// Target gate or task container cursor.
        node: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Cascade-stale `in_progress` / `completed` downstream dependent tasks.
        #[arg(long)]
        cascade: bool,
        /// Optional operator reason recorded on the blackboard.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Synchronize spec edits (and optional `--rebind-profile`) into an active `Run`.
    Sync {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Rebind non-completed gates against the latest resolved profile.
        #[arg(long)]
        rebind_profile: bool,
    },
    /// Update a task's subtask status and mandatory terminal evidence.
    Subtask {
        /// Owning task reference.
        task_ref: String,
        /// Target subtask reference.
        subtask_ref: String,
        /// New subtask status (`pending | in_progress | completed | cancelled`).
        #[arg(long)]
        status: String,
        /// Required non-empty evidence when `--status` is `completed` or `cancelled`.
        #[arg(long)]
        evidence: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// Read from or post to the run blackboard (`read | post`).
    Blackboard {
        /// Blackboard action (`read` or `post`).
        #[arg(long, default_value = "read")]
        action: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional source node cursor.
        #[arg(long)]
        node: Option<String>,
        /// Optional target recipient node cursor.
        #[arg(long)]
        recipient_node: Option<String>,
        /// Optional subtask reference.
        #[arg(long)]
        subtask_ref: Option<String>,
        /// Optional gate label.
        #[arg(long)]
        gate: Option<String>,
        /// Author identity (for `--action post`).
        #[arg(long)]
        author: Option<String>,
        /// Topic (`draft | notes | findings | blockers | decisions | deferred`).
        #[arg(long)]
        topic: Option<String>,
        /// Message body (for `--action post`).
        #[arg(long)]
        payload: Option<String>,
        /// Optional status filter (`open | acknowledged | resolved | carried_over`) for `read`.
        #[arg(long)]
        status: Option<String>,
        /// Page size (`1..=100`) for `read`.
        #[arg(long)]
        limit: Option<usize>,
        /// Zero-based page offset for `read`.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Optional `runs.generation` pin for `read`.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// Capture an explicit manual Git snapshot under `refs/acdd/snapshots/<run_id>/manual/`.
    Snapshot {
        /// Optional human-readable snapshot label (defaults to UTC timestamp).
        label: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Confined-restore working-tree files from a snapshot ref or commit/tree OID.
    Rollback {
        /// Snapshot ref, commit OID, or tree OID to restore.
        target: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional node cursor restricting rollback to that node's `admitted_scope`.
        #[arg(long)]
        node: Option<String>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Integrate completed task branches into the run worktree.
    Integrate {
        /// Optional specific task reference (omitting integrates all unintegrated completed tasks).
        task_ref: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Strategy override (`manual | rebase-ff | ff-only | no-ff | squash`).
        #[arg(long)]
        strategy: Option<String>,
        /// Explicitly permit integrating onto `main` / `master`.
        #[arg(long)]
        allow_main: bool,
    },
    /// Clear/cancel an active `Run` into `run_tombstones(terminal_status = 'cleared')`.
    Clear {
        /// Optional run ID or name selector.
        run_id: Option<String>,
        /// Optional run ID or name selector flag.
        #[arg(long)]
        run: Option<String>,
        /// Optional cancellation reason recorded in the tombstone.
        #[arg(long)]
        reason: Option<String>,
        /// Force cleanup of dirty managed worktrees.
        #[arg(long)]
        force: bool,
    },
    /// Execute automated DAG waves (`--jobs N`, `--dry-run`, `--allow-main`).
    Run {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Maximum concurrent wave jobs (clamped to `1` for `in_place`).
        #[arg(long)]
        jobs: Option<usize>,
        /// Explicitly permit commits/integration onto `main` / `master`.
        #[arg(long)]
        allow_main: bool,
        /// Dry-run mode computing waves without mutating state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Invoke a plugin tool exposed by an active `DomainProvider` (`<domain>_<name>`).
    Tool {
        /// Full plugin tool name (`<domain>_<name>`).
        tool_name: String,
        /// JSON arguments validated against `DomainToolDef.input_schema`.
        #[arg(long, default_value = "{}")]
        args: String,
        /// Optional active run selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional active node cursor.
        #[arg(long)]
        node: Option<String>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Read text, symbols, outlines, or Git changes through the layered reader.
    Read {
        /// Workspace-relative file, or symbol selector with --mode snippet.
        path: String,
        /// auto, lines, raw, search, delta, snippet, or outline.
        #[arg(long, default_value = "auto")]
        mode: String,
        /// Indexed symbol selector, optionally confined by path.
        #[arg(long)]
        selector: Option<String>,
        /// One-based first line.
        #[arg(long)]
        start_line: Option<usize>,
        /// Maximum number of text lines.
        #[arg(long)]
        line_count: Option<usize>,
        /// Search pattern.
        #[arg(long)]
        pattern: Option<String>,
        /// Interpret pattern as a regular expression.
        #[arg(long)]
        regex: bool,
        /// Search context lines on each side.
        #[arg(long)]
        context: Option<usize>,
        /// Git baseline revision.
        #[arg(long)]
        baseline: Option<String>,
        /// Workspace-relative export destination.
        #[arg(long)]
        output_file: Option<String>,
        /// Include Hints footer with follow-up suggestions.
        #[arg(long)]
        include_hints: bool,
        /// Symbol source page offset.
        #[arg(long, default_value_t = 0)]
        source_offset: usize,
        /// Previous source page generation.
        #[arg(long)]
        generation: Option<String>,
        /// Smaller symbol body limit.
        #[arg(long)]
        max_body_lines: Option<usize>,
        /// Symbol leading context lines.
        #[arg(long)]
        leading_lines: Option<usize>,
    },
    /// Validate `acdd.yaml`, `.acdd/profile.yaml`, `DomainProvider` capabilities, and specs fail-closed.
    Validate {
        /// Optional workspace-relative file; when omitted, validates the full workspace.
        path: Option<PathBuf>,
    },
    /// Serve the MCP stdio server.
    Serve,
    /// Reload running MCP servers owned by this user.
    Reload,
    /// Build or rebuild `.acdd/code-map.sqlite`.
    Build {
        /// Optional workspace root value.
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        /// Optional output database path.
        output: Option<PathBuf>,
        #[arg(long)]
        /// Optional adapter config path.
        adapter: Option<PathBuf>,
        #[arg(long)]
        /// Whether verbose applies.
        verbose: bool,
    },
    /// Scan workspace files without writing the code-map database.
    Scan {
        /// Optional workspace root value.
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        /// Optional adapter config path.
        adapter: Option<PathBuf>,
        #[arg(long)]
        /// Whether pretty applies.
        pretty: bool,
    },
    /// Incrementally update `.acdd/code-map.sqlite` for modified files.
    Delta {
        /// Optional workspace root value.
        workspace_root: Option<PathBuf>,
        /// Modified workspace-relative files.
        modified_files: Vec<PathBuf>,
        #[arg(long = "modified", value_name = "FILE")]
        /// Additional modified files.
        modified: Vec<PathBuf>,
    },
    /// Query the indexed code graph.
    Query {
        #[command(subcommand)]
        /// Selected query operation.
        command: QueryCommand,
    },
    /// Check or query indexed documentation.
    Docs {
        #[command(subcommand)]
        /// Selected docs operation.
        command: DocsCommand,
    },
    /// Run structural AST pattern searches.
    Ast {
        #[command(subcommand)]
        /// Selected AST operation.
        command: AstCommand,
    },
    /// Display built-in ACDD guidance topics (`init | adapter | docs | acdd | query | ast | validate | lifecycle | gates | subtasks | code`).
    Guide {
        /// Topic name.
        topic: String,
        #[arg(long)]
        /// Force overwrite when `topic == "init"`.
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
/// Enumerates the supported query command values.
pub enum QueryCommand {
    /// Represents the overview case.
    Overview {
        /// Comma-separated overview aspects.
        #[arg(long, value_delimiter = ',')]
        aspects: Option<Vec<String>>,
        /// Maximum items per aspect.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Number of items to skip.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Generation from the previous page.
        #[arg(long)]
        generation: Option<String>,
    },
    /// Represents the inspect case.
    Inspect {
        /// The selector value.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long,default_value_t=true,action=clap::ArgAction::Set,num_args=0..=1,default_missing_value="true")]
        /// Whether show doc applies.
        show_doc: bool,
        #[arg(long)]
        /// Whether show source applies.
        show_source: bool,
        #[arg(long, default_value_t = 0)]
        /// Source body offset returned by the prior preview.
        source_offset: usize,
        #[arg(long)]
        /// Generation returned by the prior source preview.
        generation: Option<String>,
    },
    /// Represents the search case.
    Search {
        /// The pattern value.
        pattern: String,
        #[arg(long)]
        /// Optional kind value.
        kind: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
    },
    /// Represents the tests case.
    Tests {
        /// The selector value.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value = "inbound")]
        /// The direction value.
        direction: String,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
    },
    /// Represents the impact case.
    Impact {
        /// Optional exact symbol selector; provide this or --changes.
        selector: Option<String>,
        #[arg(long)]
        /// Git change view: staged, unstaged, or head.
        changes: Option<String>,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 1)]
        /// Maximum impact depth; defaults to one edge.
        depth: u32,
        #[arg(long, default_value_t = 100)]
        /// Maximum result page size (1..=100).
        limit: usize,
        #[arg(long, default_value_t = 0)]
        /// Result page offset.
        offset: usize,
        #[arg(long)]
        /// Continuation generation from the preceding result page.
        generation: Option<String>,
    },
    /// Represents the explain case.
    Explain {
        /// The selector value.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
    },
    /// Represents the remove case.
    Remove {
        /// The selector value.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
    },
    /// Represents the analyze case.
    Analyze {
        #[arg(default_value = "")]
        /// The target value.
        target: String,
    },
    /// Execute a read-only SELECT/WITH SQL statement against the code graph.
    Sql {
        /// Read-only SELECT or WITH SQL statement.
        sql: String,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
    },
    /// Represents the run case.
    Run {
        /// The operation value ('sql', 'untested', 'implementations', 'slice', 'unwired', 'duplicates', or a direct SELECT/WITH statement).
        operation: String,
        /// Optional selector value.
        selector: Option<String>,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 2)]
        /// Maximum graph depth for generic query operations; impact defaults to two.
        depth: u32,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
        #[arg(long, default_value_t = 0)]
        /// Result page offset.
        offset: usize,
        #[arg(long)]
        /// Continuation generation from the preceding result page.
        generation: Option<String>,
        #[arg(long)]
        /// Deduplication level: 'symbol' (functions/methods) or 'file' (identical files).
        level: Option<String>,
        #[arg(long, default_value_t = 10)]
        /// Minimum line count for duplicates detection (default 10).
        min_lines: usize,
    },
}

#[derive(Subcommand, Debug)]
/// Enumerates the supported docs command values.
pub enum DocsCommand {
    /// Check Markdown links and anchors in the workspace.
    Check,
    /// Represents the search case.
    Search {
        /// The query value.
        query: String,
        #[arg(long)]
        /// Optional doc type value.
        doc_type: Option<String>,
        #[arg(long)]
        /// Optional component value.
        component: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
    },
    /// Represents the get case.
    Get {
        /// The path or id value.
        path_or_id: String,
        #[arg(long)]
        /// Optional section value.
        section: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
/// Enumerates the supported ast command values.
pub enum AstCommand {
    /// Represents the grep case.
    Grep {
        /// The pattern value.
        pattern: String,
        #[arg(long = "lang", alias = "language")]
        /// The language value.
        language: String,
        #[arg(long)]
        /// Optional path value.
        path: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// The limit value.
        limit: usize,
    },
}

/// Default code-map SQLite path under `root`.
pub fn default_db(root: &Path) -> PathBuf {
    root.join(".acdd/code-map.sqlite")
}

fn command_presentation(command: &Command) -> (OperationKind, &'static str, Option<bool>) {
    match command {
        Command::Init { .. } => (OperationKind::General, "acdd_init", None),
        Command::Start { .. }
        | Command::Draft { .. }
        | Command::Status { .. }
        | Command::List { .. }
        | Command::Show { .. }
        | Command::Next { .. }
        | Command::ExtendScope { .. }
        | Command::Reopen { .. }
        | Command::Sync { .. }
        | Command::Subtask { .. }
        | Command::Snapshot { .. }
        | Command::Rollback { .. }
        | Command::Integrate { .. }
        | Command::Clear { .. }
        | Command::Run { .. }
        | Command::Tool { .. } => (OperationKind::Task, "acdd_state", None),
        Command::Claim { .. } => (OperationKind::Task, "acdd_claim", None),
        Command::Verify { .. } => (OperationKind::Task, "acdd_verify", None),
        Command::Submit { .. } => (OperationKind::Task, "acdd_submit", None),
        Command::Blackboard { .. } => (OperationKind::Task, "acdd_blackboard", None),
        Command::Read { .. } => (OperationKind::Navigation, "acdd_read", None),
        Command::Guide { .. } => (OperationKind::Guide, "acdd_guide", None),
        Command::Ast { .. } => (OperationKind::Navigation, "ast_grep_search", None),
        Command::Query {
            command: QueryCommand::Sql { .. },
        } => (OperationKind::Sql, "code_map_query", None),
        Command::Query {
            command: QueryCommand::Run { operation, .. },
        } => (
            if operation == "sql" || looks_like_sql(operation) {
                OperationKind::Sql
            } else {
                OperationKind::Navigation
            },
            "code_map_query",
            None,
        ),
        Command::Query {
            command: QueryCommand::Analyze { target },
        } => (
            if looks_like_sql(target) {
                OperationKind::Sql
            } else {
                OperationKind::Navigation
            },
            "code_map_analyze",
            None,
        ),
        Command::Query { command } => match command {
            QueryCommand::Overview { .. } => (OperationKind::Navigation, "code_map_overview", None),
            QueryCommand::Inspect { .. } => {
                (OperationKind::Navigation, "code_map_inspect", Some(false))
            }
            QueryCommand::Search { .. } => (OperationKind::Navigation, "code_map_search", None),
            QueryCommand::Tests { .. } => (OperationKind::Navigation, "code_map_tests", None),
            QueryCommand::Impact { .. } => (OperationKind::Navigation, "code_map_impact", None),
            QueryCommand::Explain { .. } => {
                (OperationKind::Navigation, "code_map_explain", Some(true))
            }
            QueryCommand::Remove { .. } => {
                (OperationKind::Navigation, "code_map_prove_removal", None)
            }
            QueryCommand::Analyze { .. } | QueryCommand::Run { .. } | QueryCommand::Sql { .. } => {
                unreachable!()
            }
        },
        Command::Docs { command } => match command {
            DocsCommand::Check => (OperationKind::General, "docs check", None),
            DocsCommand::Search { .. } => (OperationKind::Navigation, "search_docs", None),
            DocsCommand::Get { .. } => (OperationKind::Navigation, "get_doc", None),
        },
        Command::Build { .. } => (OperationKind::General, "build result", None),
        Command::Scan { .. } => (OperationKind::General, "scan result", None),
        Command::Delta { .. } => (OperationKind::General, "delta result", None),
        Command::Validate { .. } => (OperationKind::Task, "validate result", None),
        Command::Reload => (OperationKind::General, "reload result", None),
        Command::Serve => (OperationKind::General, "server", None),
    }
}

fn command_discovery(command: &Command) -> Option<format::DiscoveryKind> {
    match command {
        Command::Ast { .. } => Some(format::DiscoveryKind::Ast),
        Command::Query { command } => match command {
            QueryCommand::Overview { .. } => Some(format::DiscoveryKind::Overview),
            QueryCommand::Inspect { .. } => Some(format::DiscoveryKind::Inspect),
            QueryCommand::Run { operation, .. } if operation.trim() == "inspect" => {
                Some(format::DiscoveryKind::Inspect)
            }
            QueryCommand::Search { .. } => Some(format::DiscoveryKind::Search),
            _ => None,
        },
        _ => None,
    }
}

fn looks_like_sql(value: &str) -> bool {
    value.split_whitespace().next().is_some_and(|keyword| {
        keyword.eq_ignore_ascii_case("select") || keyword.eq_ignore_ascii_case("with")
    })
}

fn cli_page(limit: usize) -> Result<QueryOptions> {
    QueryOptions::resolve(&ResponsePolicy::default(), Some(limit), 0, None, None)
}

fn cli_page_with_offset(
    limit: usize,
    offset: usize,
    generation: Option<String>,
) -> Result<QueryOptions> {
    QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(limit),
        offset,
        None,
        generation,
    )
}

fn cli_source(enabled: bool) -> Result<SourceOptions> {
    SourceOptions::resolve(&ResponsePolicy::default(), Some(enabled), None, None, 0)
}

fn command_root(global: Option<&Path>, positional: Option<&Path>) -> Result<PathBuf> {
    let global = global.map(Path::canonicalize).transpose()?;
    let positional = positional.map(Path::canonicalize).transpose()?;
    if let (Some(global), Some(positional)) = (&global, &positional) {
        if global != positional {
            bail!(
                "conflicting workspace roots: --root {} and positional {}",
                global.display(),
                positional.display()
            );
        }
    }
    Ok(positional
        .or(global)
        .unwrap_or(std::env::current_dir()?.canonicalize()?))
}

fn check_explicit_config_migration(
    root: &Path,
    config_path: &Path,
) -> std::result::Result<(), StateError> {
    let confined_target = if let Ok(rel) = config_path.strip_prefix(root) {
        paths::verify_no_symlink_traversal(root, rel, "CLI_PATH_INVALID").map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("cannot inspect confined config '{}': {err}", config_path.display()),
            )
        })?
    } else {
        match std::fs::symlink_metadata(config_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("config path '{}' must not be a symlink", config_path.display()),
                ));
            }
            Ok(_) => config_path.to_path_buf(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("cannot inspect config '{}': {err}", config_path.display()),
                ));
            }
        }
    };
    if !confined_target.is_file() {
        return Ok(());
    }
    let Ok(raw) = std::fs::read_to_string(&confined_target) else {
        return Ok(());
    };
    let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return Ok(());
    };
    if let Some(map) = yaml.as_mapping() {
        for legacy_key in [
            "milestones",
            "plans",
            "tasks_db",
            "task_repository",
            "task_project",
            "acdd_profile",
        ] {
            if map.contains_key(serde_yaml::Value::String(legacy_key.to_owned())) {
                return Err(StateError::new(
                    ErrorCode::ConfigMigrationRequired,
                    format!(
                        "legacy 0.5.x key '{legacy_key}' found in '{}'",
                        confined_target.display()
                    ),
                )
                .with_hint(
                    "Follow docs/runbooks/migration-0.6.md to update 0.5.x configuration keys",
                ));
            }
        }
    }
    Ok(())
}

fn ensure_migrated_preflight(root: &Path, explicit_config: Option<&Path>) -> Result<()> {
    check_explicit_config_migration(root, &root.join("acdd.yaml"))?;
    if let Some(cfg) = explicit_config {
        let resolved = if cfg.is_absolute() {
            cfg.to_path_buf()
        } else {
            root.join(cfg)
        };
        if resolved != root.join("acdd.yaml") {
            check_explicit_config_migration(root, &resolved)?;
        }
    }
    Ok(())
}

fn open_or_rebuild(
    db: &Path,
    root: &Path,
    explicit_config: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<reader::LockedConnection> {
    writer::validate_code_map_target(root, db)?;
    match reader::open_locked(db, root) {
        Ok(conn) => {
            let selected = acdd_index::scanner::load_adapter(root, explicit_config)?;
            let persisted = reader::read_policy(root, &conn)?;
            if selected.digest != persisted.digest
                || selected.adapter_path != persisted.adapter_path
                || selected.linked_workspaces != persisted.linked_workspaces
            {
                drop(conn);
                match cache_dir {
                    Some(cache_dir) => crate::engine::pipeline::build_with_cache_dir(
                        root,
                        db,
                        explicit_config,
                        cache_dir,
                    )?,
                    None => writer::build(root, db, explicit_config)?,
                };
                return reader::open_locked(db, root);
            }
            Ok(conn)
        }
        Err(err) => {
            let msg = err.to_string();
            let requires_rebuild = err
                .downcast_ref::<reader::IndexRebuildRequired>()
                .is_some();
            if requires_rebuild || !db.exists() {
                eprintln!(
                    "ACDD index incompatible or missing ({msg}); auto-rebuilding for {}...",
                    root.display()
                );
                match cache_dir {
                    Some(cache_dir) => crate::engine::pipeline::build_with_cache_dir(
                        root,
                        db,
                        explicit_config,
                        cache_dir,
                    )?,
                    None => writer::build(root, db, explicit_config)?,
                };
                reader::open_locked(db, root)
            } else {
                Err(err)
            }
        }
    }
}

fn parse_json_or_at_file(root: &Path, raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    let payload = if let Some(rel) = trimmed.strip_prefix('@') {
        let confined = paths::confined_file(root, Path::new(rel))?;
        std::fs::read_to_string(&confined)
            .with_context(|| format!("reading JSON payload '{}'", confined.display()))?
    } else {
        trimmed.to_owned()
    };
    serde_json::from_str(&payload)
        .map_err(|err| StateError::new(ErrorCode::ProfileSchemaInvalid, format!("invalid JSON payload: {err}")).into())
}

fn state_err_invalid(msg: impl Into<String>) -> anyhow::Error {
    StateError::new(ErrorCode::ProfileSchemaInvalid, msg).into()
}

impl Cli {
    /// Run the parsed CLI command and terminate with canonical `StateError` exit codes (`2`/`3`/`4`).
    pub async fn run(self) -> Result<()> {
        match self.run_with_writer(&mut io::stdout()).await {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(state_err) = StateError::from_anyhow(&error) {
                    eprintln!("{error:#}");
                    std::process::exit(state_err.exit_code());
                }
                Err(error)
            }
        }
    }

    /// Runs CLI dispatch and formatting with a caller-provided output writer.
    pub async fn run_with_writer<W: io::Write>(self, output: &mut W) -> Result<()> {
        self.run_inner(output, None).await
    }

    /// Runs CLI dispatch with explicit output and extraction-cache locations.
    pub async fn run_with_writer_and_cache_dir<W: io::Write>(
        self,
        output: &mut W,
        cache_dir: &Path,
    ) -> Result<()> {
        self.run_inner(output, Some(cache_dir)).await
    }

    async fn run_inner(
        self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
    ) -> Result<()> {
        let positional_root = match self.command.as_ref() {
            Some(
                Command::Build { workspace_root, .. }
                | Command::Scan { workspace_root, .. }
                | Command::Delta { workspace_root, .. },
            ) => workspace_root.as_deref(),
            _ => None,
        };
        let root = match command_root(self.root.as_deref(), positional_root) {
            Ok(root) => root,
            Err(error) => {
                if matches!(
                    self.command.as_ref(),
                    Some(Command::Docs {
                        command: DocsCommand::Check
                    })
                ) {
                    return docs_check::check(
                        self.root.as_deref().unwrap_or_else(|| Path::new(".")),
                        OutputFormat::from(self.format),
                        output,
                    );
                }
                return Err(error);
            }
        };

        // Enforce CONFIG_MIGRATION_REQUIRED preflight on every command.
        ensure_migrated_preflight(&root, self.config.as_deref())?;

        // `init` and `guide init` must work even before `.acdd/profile.yaml` exists.
        if matches!(self.command.as_ref(), Some(Command::Init { .. })) {
            let force = matches!(self.command.as_ref(), Some(Command::Init { force: true }));
            let result = guide::initialize(&root, force)?;
            let formatted = format::render_for_operation(
                &result,
                OutputFormat::from(self.format),
                "acdd_init",
                None,
                OperationKind::General,
            );
            writeln!(output, "{formatted}")?;
            return Ok(());
        }
        if let Some(Command::Guide { topic, force }) = self.command.as_ref() {
            if topic.trim() == "init" {
                let result = guide::run(&root, topic, *force)?;
                let formatted = format::render_for_operation(
                    &result,
                    OutputFormat::from(self.format),
                    "acdd_guide",
                    None,
                    OperationKind::Guide,
                );
                writeln!(output, "{formatted}")?;
                return Ok(());
            }
        }

        // `validate` handles potentially broken configs/profiles directly without ResolvedPaths pre-failing.
        if let Some(Command::Validate { path }) = self.command {
            let result = validate::run(&root, path.as_deref())?;
            let failed = result.get("valid").and_then(Value::as_bool) == Some(false);
            let formatted = format::render_for_operation(
                &result,
                OutputFormat::from(self.format),
                "validate result",
                None,
                OperationKind::Task,
            );
            writeln!(output, "{formatted}")?;
            if failed {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "VALIDATION_FAILED: one or more documents failed validation",
                )
                .into());
            }
            return Ok(());
        }

        let settings =
            paths::ResolvedPaths::resolve(&root, self.config.as_deref(), self.profile.as_deref())?;
        let database = self
            .db
            .as_ref()
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(path)
                }
            })
            .unwrap_or_else(|| default_db(&root));
        let state_db = self.state_db.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });
        let profile_argument = self.profile.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });

        self.run_selected(
            output,
            cache_dir,
            root,
            settings,
            database,
            state_db,
            profile_argument,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_selected(
        self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
        root: PathBuf,
        settings: paths::ResolvedPaths,
        db: PathBuf,
        state_db: Option<PathBuf>,
        profile_argument: Option<PathBuf>,
    ) -> Result<()> {
        let output_format = OutputFormat::from(self.format);
        let command = self.command.unwrap_or(Command::Serve);
        if matches!(
            &command,
            Command::Docs {
                command: DocsCommand::Check
            }
        ) {
            return docs_check::check(&root, output_format, output);
        }

        let (operation, title, explain) = command_presentation(&command);
        let discovery = command_discovery(&command);
        let paths::ResolvedPaths {
            adapter,
            config: explicit_config,
            profile_ref: explicit_profile_ref,
            ..
        } = settings;

        let span_worktree = match &command {
            Command::Claim { worktree, .. }
            | Command::Verify { worktree, .. }
            | Command::Submit { worktree, .. }
            | Command::Snapshot { worktree, .. }
            | Command::Rollback { worktree, .. }
            | Command::Tool { worktree, .. } => worktree.as_deref().unwrap_or(&root),
            _ => &root,
        };
        let span_ctx = if adapter.modules.is_enabled("state") {
            crate::state_bridge::resolve_worktree_span_context(&root, span_worktree, &adapter)
                .ok()
                .flatten()
        } else {
            None
        };
        let mut span = crate::telemetry::SpanGuard::from_worktree_context(
            &root,
            span_ctx.as_ref(),
            title,
            adapter.retain_traces,
        );

        let exec_result: Result<(Value, Option<String>)> = (|| async {
            let (val, pre_rendered) = match command {
                Command::Init { .. } | Command::Validate { .. } => {
                    unreachable!("handled prior to run_selected")
                }
                Command::Start {
                    spec,
                    goal,
                    name,
                    scope,
                    target_base,
                    worktree,
                    in_place,
                    worktree_path,
                } => {
                    let run_name = name
                        .as_deref()
                        .map(RunName::new)
                        .transpose()
                        .map_err(anyhow::Error::from)?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Start(StartReq {
                            spec_path: spec,
                            goal,
                            run_name,
                            profile: explicit_profile_ref,
                            scope,
                            target_base,
                            worktree,
                            in_place,
                            worktree_path,
                        }),
                    )?
                }
                Command::Draft {
                    collection,
                    slug,
                    prefix,
                    title: spec_title,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Draft(DraftReq {
                        collection,
                        slug,
                        prefix,
                        title: spec_title,
                        profile: explicit_profile_ref,
                    }),
                )?,
                Command::Status { run } => exec_cli_state_query(
                    &root,
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateQuery::Status(StatusReq { run }),
                )?,
                Command::List {
                    workspace,
                    status,
                    include_tombstones,
                    limit,
                    offset,
                    generation,
                } => {
                    let status = status
                        .as_deref()
                        .map(RunStatus::from_str)
                        .transpose()
                        .map_err(anyhow::Error::from)?;
                    exec_cli_state_query(
                        &root,
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateQuery::List(ListReq {
                            workspace,
                            status,
                            include_tombstones,
                            limit,
                            offset,
                            generation,
                        }),
                    )?
                }
                Command::Show { node, run } => exec_cli_state_query(
                    &root,
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateQuery::Show(ShowReq { run, node }),
                )?,
                Command::Next { run, limit } => exec_cli_state_query(
                    &root,
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateQuery::Next(NextReq { run, limit }),
                )?,
                Command::Claim {
                    node,
                    run,
                    worker,
                    session_fingerprint,
                    force,
                    steal_remote,
                    worktree,
                } => {
                    let worker_id = WorkerId::new(&worker)?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Claim(ClaimReq {
                            run,
                            node,
                            worker_id,
                            session_fingerprint,
                            force,
                            steal_remote,
                            worktree,
                        }),
                    )?
                }
                Command::Verify {
                    node,
                    run,
                    worker,
                    test,
                    claim_revision,
                    worktree,
                } => {
                    let worker_id = worker.as_deref().map(WorkerId::new).transpose()?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Verify(VerifyReq {
                            run,
                            node,
                            worker_id,
                            test_target: test,
                            claim_revision,
                            worktree,
                        }),
                    )?
                }
                Command::Submit {
                    node,
                    action,
                    run,
                    worker,
                    claim_revision,
                    test,
                    evidence,
                    findings,
                    invariants,
                    summary,
                    allow_main,
                    worktree,
                } => {
                    let submit_action = SubmitAction::from_str(&action)?;
                    let worker_id = WorkerId::new(&worker)?;
                    let mut review_evidence: Option<ReviewEvidence> = None;
                    let mut custom_evidence: Option<Value> = None;
                    if let Some(raw) = evidence.as_deref() {
                        let parsed = parse_json_or_at_file(&root, raw)?;
                        if let Ok(re) = serde_json::from_value::<ReviewEvidence>(parsed.clone()) {
                            review_evidence = Some(re);
                        } else {
                            custom_evidence = Some(parsed);
                        }
                    }
                    let rejection_findings = findings
                        .as_deref()
                        .map(|raw| {
                            let parsed = parse_json_or_at_file(&root, raw)?;
                            serde_json::from_value::<RejectionFindings>(parsed).map_err(|err| {
                                state_err_invalid(format!("invalid rejection findings: {err}"))
                            })
                        })
                        .transpose()?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Submit(Box::new(SubmitReq {
                            run,
                            node,
                            action: submit_action,
                            worker_id,
                            claim_revision,
                            test_target: test,
                            review_evidence,
                            rejection_findings,
                            custom_evidence,
                            verified_invariants: invariants,
                            summary,
                            allow_main,
                            worktree,
                        })),
                    )?
                }
                Command::ExtendScope {
                    node,
                    paths,
                    reason,
                    run,
                    worker,
                } => {
                    let worker_id = worker.as_deref().map(WorkerId::new).transpose()?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::ExtendScope(ExtendScopeReq {
                            run,
                            node,
                            paths,
                            reason,
                            worker_id,
                        }),
                    )?
                }
                Command::Reopen {
                    node,
                    run,
                    cascade,
                    reason,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Reopen(ReopenReq {
                        run,
                        node,
                        cascade,
                        reason,
                    }),
                )?,
                Command::Sync {
                    run,
                    rebind_profile,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Sync(SyncReq {
                        run,
                        rebind_profile,
                    }),
                )?,
                Command::Subtask {
                    task_ref,
                    subtask_ref,
                    status,
                    evidence,
                    run,
                } => {
                    let task_ref = TaskRef::new(&task_ref)?;
                    let subtask_ref = SubtaskRef::new(&subtask_ref)?;
                    let status = SubtaskStatus::from_str(&status)?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Subtask(SubtaskReq {
                            run,
                            task_ref,
                            subtask_ref,
                            status,
                            evidence,
                        }),
                    )?
                }
                Command::Blackboard {
                    action,
                    run,
                    node,
                    recipient_node,
                    subtask_ref,
                    gate,
                    author,
                    topic,
                    payload,
                    status,
                    limit,
                    offset,
                    generation,
                } => match action.as_str() {
                    "read" => {
                        let topic = topic.as_deref().map(BlackboardTopic::from_str).transpose()?;
                        let status = status
                            .as_deref()
                            .map(BlackboardStatus::from_str)
                            .transpose()?;
                        exec_cli_state_query(
                            &root,
                            &root,
                            state_db.as_deref(),
                            &adapter,
                            output_format,
                            title,
                            StateQuery::BlackboardRead(BlackboardReadReq {
                                run,
                                node,
                                recipient_node,
                                topic,
                                status,
                                limit,
                                offset,
                                generation,
                            }),
                        )?
                    }
                    "post" => {
                        let topic_val = BlackboardTopic::from_str(
                            topic
                                .as_deref()
                                .ok_or_else(|| state_err_invalid("--topic is required for blackboard --action post"))?,
                        )?;
                        let payload_val = payload.ok_or_else(|| {
                            state_err_invalid("--payload is required for blackboard --action post")
                        })?;
                        let author_val = author.unwrap_or_else(|| "cli".to_owned());
                        let subtask_ref = subtask_ref.as_deref().map(SubtaskRef::new).transpose()?;
                        exec_cli_state_command(
                            &root,
                            state_db.as_deref(),
                            &adapter,
                            output_format,
                            title,
                            StateCommand::BlackboardPost(BlackboardPostReq {
                                run,
                                node,
                                recipient_node,
                                subtask_ref,
                                gate,
                                author: author_val,
                                topic: topic_val,
                                payload: payload_val,
                            }),
                        )?
                    }
                    other => {
                        return Err(state_err_invalid(format!(
                            "blackboard --action must be 'read' or 'post', got '{other}'"
                        )));
                    }
                },
                Command::Snapshot {
                    label,
                    run,
                    worktree,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Snapshot(SnapshotReq {
                        run,
                        label,
                        worktree,
                    }),
                )?,
                Command::Rollback {
                    target,
                    run,
                    node,
                    worktree,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Rollback(RollbackReq {
                        run,
                        target,
                        node,
                        worktree,
                    }),
                )?,
                Command::Integrate {
                    task_ref,
                    run,
                    strategy,
                    allow_main,
                } => {
                    let task_ref = task_ref.as_deref().map(TaskRef::new).transpose()?;
                    let strategy = strategy
                        .as_deref()
                        .map(IntegrateStrategy::from_str)
                        .transpose()?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Integrate(IntegrateReq {
                            run,
                            task_ref,
                            strategy,
                            allow_main,
                        }),
                    )?
                }
                Command::Clear {
                    run_id,
                    run,
                    reason,
                    force,
                } => {
                    let effective_run = run.or(run_id);
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::Clear(ClearReq {
                            run: effective_run,
                            reason,
                            force,
                        }),
                    )?
                }
                Command::Run {
                    run,
                    jobs,
                    allow_main,
                    dry_run,
                } => exec_cli_state_command(
                    &root,
                    state_db.as_deref(),
                    &adapter,
                    output_format,
                    title,
                    StateCommand::Run(RunReq {
                        run,
                        jobs,
                        allow_main,
                        dry_run,
                    }),
                )?,
                Command::Tool {
                    tool_name,
                    args,
                    run,
                    node,
                    worktree,
                } => {
                    let parsed_args = parse_json_or_at_file(&root, &args)?;
                    exec_cli_state_command(
                        &root,
                        state_db.as_deref(),
                        &adapter,
                        output_format,
                        title,
                        StateCommand::CallPluginTool(CallPluginToolReq {
                            tool_name,
                            args: parsed_args,
                            run,
                            node,
                            worktree,
                        }),
                    )?
                }
                Command::Read {
                    path,
                    mode,
                    selector,
                    start_line,
                    line_count,
                    pattern,
                    regex,
                    context,
                    baseline,
                    output_file,
                    include_hints,
                    source_offset,
                    generation,
                    max_body_lines,
                    leading_lines,
                } => {
                    let mode = serde_json::from_value::<acdd_core::model::ReadMode>(json!(mode))?;
                    let request = crate::engine::reader::ReadRequest {
                        path: Some(path),
                        paths: None,
                        mode,
                        selector,
                        start_line,
                        line_count,
                        pattern,
                        regex,
                        context,
                        baseline,
                        output_file,
                        include_hints,
                    };
                    let source = SourceOptions::resolve_full_symbol(
                        &adapter.response,
                        leading_lines,
                        max_body_lines,
                        source_offset,
                    )?;
                    anyhow::ensure!(
                        source.has_valid_paging_anchor(generation.as_deref()),
                        "source continuation requires generation"
                    );
                    let mut page =
                        QueryOptions::resolve(&adapter.response, None, 0, None, generation)?;
                    page.detail = Detail::Full;
                    let structural = request.requires_index(&adapter)?;
                    let val = if structural {
                        let conn =
                            open_or_rebuild(&db, &root, explicit_config.as_deref(), cache_dir)?;
                        crate::engine::reader::acdd_read_tool(
                            &root,
                            &adapter,
                            &request,
                            &page,
                            &source,
                            Some(&conn),
                        )?
                    } else if db.is_file()
                        && (adapter.modules.is_enabled("code") || adapter.modules.is_enabled("docs"))
                    {
                        reader::open_locked(&db, &root)
                            .and_then(|conn| {
                                crate::engine::reader::acdd_read_tool(
                                    &root,
                                    &adapter,
                                    &request,
                                    &page,
                                    &source,
                                    Some(&conn),
                                )
                            })
                            .or_else(|_| {
                                crate::engine::reader::acdd_read_tool(
                                    &root, &adapter, &request, &page, &source, None,
                                )
                            })?
                    } else {
                        crate::engine::reader::acdd_read_tool(
                            &root, &adapter, &request, &page, &source, None,
                        )?
                    };
                    (val, None)
                }
                Command::Serve => {
                    writer::validate_code_map_target(&root, &db)?;
                    let invocation = InvocationSettings {
                        root: root.clone(),
                        config: explicit_config.clone(),
                        profile: profile_argument,
                        state_db,
                        code_db: db.clone(),
                    };
                    crate::mcp::server::serve_with_state_config(
                        root.clone(),
                        db,
                        cache_dir.map(Path::to_path_buf),
                        Some(invocation),
                    )
                    .await?;
                    return Ok((Value::Null, Some(String::new())));
                }
                Command::Reload => (reload::run()?, None),
                Command::Build {
                    workspace_root: _,
                    output,
                    adapter: adapter_arg,
                    verbose: _,
                } => {
                    let adapter_path = match adapter_arg {
                        Some(adapter_arg) => {
                            let confined = paths::confined_file(&root, &adapter_arg)?;
                            if self.config.is_some() {
                                anyhow::ensure!(
                                    explicit_config.as_ref() == Some(&confined),
                                    "CLI_CONFIG_CONFLICT: --adapter and --config select different files"
                                );
                            }
                            Some(confined)
                        }
                        None => explicit_config.clone(),
                    };
                    let output_db = output
                        .map(|path| {
                            if path.is_absolute() {
                                path
                            } else {
                                root.join(path)
                            }
                        })
                        .unwrap_or_else(|| db.clone());
                    writer::validate_code_map_target(&root, &output_db)?;
                    let val = match cache_dir {
                        Some(cache_dir) => crate::engine::pipeline::build_with_cache_dir(
                            &root,
                            &output_db,
                            adapter_path.as_deref(),
                            cache_dir,
                        )?,
                        None => writer::build(&root, &output_db, adapter_path.as_deref())?,
                    };
                    (val, None)
                }
                Command::Scan {
                    workspace_root: _,
                    adapter: adapter_arg,
                    pretty: _,
                } => {
                    let adapter_path = match adapter_arg {
                        Some(adapter_arg) => {
                            let confined = paths::confined_file(&root, &adapter_arg)?;
                            if self.config.is_some() {
                                anyhow::ensure!(
                                    explicit_config.as_ref() == Some(&confined),
                                    "CLI_CONFIG_CONFLICT: --adapter and --config select different files"
                                );
                            }
                            Some(confined)
                        }
                        None => explicit_config.clone(),
                    };
                    (
                        json!(acdd_index::scanner::scan(&root, adapter_path.as_deref())?),
                        None,
                    )
                }
                Command::Delta {
                    workspace_root: _,
                    mut modified_files,
                    modified,
                } => {
                    modified_files.extend(modified);
                    if modified_files.is_empty() {
                        bail!("delta requires at least one modified file");
                    }
                    let val = crate::engine::pipeline::delta_with_selection(
                        &root,
                        &db,
                        &modified_files,
                        self.config.as_ref().and(explicit_config.as_deref()),
                        cache_dir,
                    )?;
                    (val, None)
                }
                Command::Guide { topic, force } => {
                    (guide::run_with_adapter(&root, &topic, force, &adapter)?, None)
                }
                Command::Ast {
                    command:
                        AstCommand::Grep {
                            pattern,
                            language,
                            path,
                            limit,
                        },
                } => {
                    let conn =
                        open_or_rebuild(&db, &root, explicit_config.as_deref(), cache_dir)?;
                    let mut result = if (1..=MAX_PAGE_SIZE).contains(&limit) {
                        ast::search_paged_with_adapter(
                            &conn,
                            &root,
                            &adapter,
                            &pattern,
                            &language,
                            path.as_deref(),
                            &cli_page(limit)?,
                        )?
                    } else {
                        ast::search(&conn, &root, &pattern, &language, path.as_deref(), limit)?
                    };
                    result["pattern"] = json!(pattern);
                    result["language"] = json!(language);
                    if output_format == OutputFormat::Markdown {
                        result["request_scope"] = json!({
                            "workspace_root": root.display().to_string(),
                            "pattern": pattern,
                            "language": language,
                            "path": path,
                        });
                    }
                    (result, None)
                }
                Command::Query { command } => {
                    let conn =
                        open_or_rebuild(&db, &root, explicit_config.as_deref(), cache_dir)?;
                    let val = match command {
                        QueryCommand::Overview {
                            aspects,
                            limit,
                            offset,
                            generation,
                        } => {
                            let page = QueryOptions::resolve(
                                &ResponsePolicy::default(),
                                Some(limit),
                                offset,
                                None,
                                generation,
                            )?;
                            discovery::overview(
                                &conn,
                                &root,
                                Some(&adapter),
                                aspects.as_deref(),
                                &page,
                            )?
                        }
                        QueryCommand::Inspect {
                            selector,
                            path,
                            show_doc,
                            show_source,
                            source_offset,
                            generation,
                        } => {
                            let source = SourceOptions::resolve(
                                &ResponsePolicy::default(),
                                Some(show_source),
                                None,
                                None,
                                source_offset,
                            )?;
                            anyhow::ensure!(
                                source.has_valid_paging_anchor(generation.as_deref()),
                                "source continuation requires restarting at source_offset=0 or providing the previous page's generation"
                            );
                            let page = QueryOptions::resolve(
                                &ResponsePolicy::default(),
                                Some(100),
                                0,
                                None,
                                generation,
                            )?;
                            let mut result = symbols::inspect_with_path_options(
                                &conn,
                                &root,
                                &selector,
                                path.as_deref(),
                                &symbols::InspectOptions {
                                    show_doc,
                                    source: &source,
                                    coverage: CoverageOptions {
                                        include_coverage: true,
                                    },
                                    page: &page,
                                },
                            )?;
                            if output_format == OutputFormat::Markdown {
                                result["request_scope"] = json!({
                                    "workspace_root": root.display().to_string(),
                                    "selector": selector,
                                    "path": path,
                                });
                            }
                            result
                        }
                        QueryCommand::Search {
                            pattern,
                            kind,
                            limit,
                        } => {
                            let page = cli_page(limit)?;
                            let exact = false;
                            let include_docs = false;
                            let result = symbols::search_with_options(
                                &conn,
                                &pattern,
                                &symbols::SearchOptions {
                                    kind: kind.as_deref(),
                                    path: None,
                                    include_docs,
                                    exact,
                                    page: &page,
                                },
                            )?;
                            let mut result = symbols::search_results_with_navigation(
                                &conn, &root, result, false,
                            )?;
                            if output_format == OutputFormat::Markdown {
                                result["request_scope"] = json!({
                                    "workspace_root": root.display().to_string(),
                                    "pattern": pattern,
                                    "kind": kind,
                                    "exact": exact,
                                    "include_docs": include_docs,
                                });
                            }
                            result
                        }
                        QueryCommand::Tests {
                            selector,
                            path,
                            direction,
                            limit,
                        } => symbols::tests_paged_with_path(
                            &conn,
                            &selector,
                            path.as_deref(),
                            &direction,
                            &cli_page(limit)?,
                        )?,
                        QueryCommand::Impact {
                            selector,
                            changes,
                            path,
                            depth,
                            limit,
                            offset,
                            generation,
                        } => traversal::impact_with_path_options(
                            &conn,
                            &root,
                            selector.as_deref(),
                            changes.as_deref(),
                            path.as_deref(),
                            &traversal::TraversalOptions {
                                depth,
                                inbound: true,
                                mode: None,
                                edge_types: None,
                                page: &QueryOptions::resolve(
                                    &ResponsePolicy::default(),
                                    Some(limit),
                                    offset,
                                    None,
                                    generation,
                                )?,
                            },
                        )?,
                        QueryCommand::Explain { selector, path } => {
                            symbols::explain_with_path_options(
                                &conn,
                                &root,
                                &selector,
                                path.as_deref(),
                                &symbols::ExplainOptions {
                                    direction: None,
                                    show_doc: true,
                                    source: &cli_source(false)?,
                                    coverage: CoverageOptions {
                                        include_coverage: true,
                                    },
                                    page: &cli_page(100)?,
                                },
                            )?
                        }
                        QueryCommand::Remove { selector, path } => {
                            traversal::removal_paged_with_path(
                                &conn,
                                &selector,
                                path.as_deref(),
                                &cli_page(100)?,
                            )?
                        }
                        QueryCommand::Analyze { target } => {
                            reader::analyze_paged(&conn, &target, None, &cli_page(100)?)?
                        }
                        QueryCommand::Sql { sql, limit } => traversal::query_with_path_options(
                            &conn,
                            "sql",
                            Some(sql.as_str()),
                            None,
                            &traversal::GraphQueryOptions {
                                depth: 2,
                                direction: None,
                                coverage: CoverageOptions::default(),
                                page: &cli_page(limit)?,
                            },
                        )?,
                        QueryCommand::Run {
                            operation,
                            selector,
                            path,
                            depth,
                            limit,
                            offset,
                            generation,
                            level,
                            min_lines,
                        } => {
                            let (effective_op, effective_selector) =
                                if selector.is_none() && looks_like_sql(&operation) {
                                    ("sql", Some(operation.as_str()))
                                } else {
                                    (operation.as_str(), selector.as_deref())
                                };
                            let page = cli_page_with_offset(limit, offset, generation)?;
                            if effective_op.trim() == "duplicates" {
                                traversal::duplicates_paged(
                                    &conn,
                                    effective_selector,
                                    path.as_deref(),
                                    level.as_deref(),
                                    Some(min_lines),
                                    &page,
                                )?
                            } else {
                                traversal::query_with_path_options(
                                    &conn,
                                    effective_op,
                                    effective_selector,
                                    path.as_deref(),
                                    &traversal::GraphQueryOptions {
                                        depth,
                                        direction: None,
                                        coverage: CoverageOptions::default(),
                                        page: &page,
                                    },
                                )?
                            }
                        }
                    };
                    (val, None)
                }
                Command::Docs { command } => {
                    let conn =
                        open_or_rebuild(&db, &root, explicit_config.as_deref(), cache_dir)?;
                    let val = match command {
                        DocsCommand::Check => unreachable!(),
                        DocsCommand::Search {
                            query,
                            doc_type,
                            component,
                            limit,
                        } => reader::search_docs_with_options(
                            &conn,
                            &query,
                            &reader::DocSearchOptions {
                                doc_type: doc_type.as_deref(),
                                component: component.as_deref(),
                                include_excerpt: false,
                                page: &cli_page(limit)?,
                            },
                        )?,
                        DocsCommand::Get {
                            path_or_id,
                            section,
                        } => {
                            let mut options = cli_page(100)?;
                            options.detail = Detail::Full;
                            reader::get_doc_paged(&conn, &path_or_id, section.as_deref(), &options)?
                        }
                    };
                    (val, None)
                }
            };
            Ok((val, pre_rendered))
        })()
        .await;

        let (mut result, pre_rendered) = match exec_result {
            Ok(pair) => {
                span.set_status("ok");
                span.finish();
                pair
            }
            Err(err) => {
                span.set_status("error");
                span.finish();
                return Err(err);
            }
        };

        if let Some(text) = pre_rendered {
            if text.is_empty() && title == "server" {
                return Ok(());
            }
            if adapter.debug {
                let cli_cmd = {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    if args.is_empty() {
                        "serve".to_string()
                    } else {
                        args.join(" ")
                    }
                };
                crate::telemetry::log_cli(&root, &cli_cmd, None, &result);
            }
            if let Err(e) = writeln!(output, "{text}") {
                if e.kind() == io::ErrorKind::BrokenPipe {
                    return Ok(());
                }
                return Err(e.into());
            }
            return Ok(());
        }

        if output_format == OutputFormat::Markdown {
            if let Some(kind) = discovery {
                discovery::attach_presentation_uris(&root, &adapter, &mut result, kind);
            }
            if matches!(title, "search_docs" | "get_doc") {
                discovery::attach_document_presentation_uris(&root, &adapter, &mut result, title);
            }
            if operation == OperationKind::Navigation
                && discovery.is_none()
                && matches!(
                    title,
                    "code_map_impact"
                        | "code_map_explain"
                        | "code_map_tests"
                        | "code_map_analyze"
                        | "code_map_query"
                )
            {
                discovery::attach_traversal_presentation_uris(&root, &adapter, &mut result);
            }
        }
        let formatted = if title == "acdd_read" {
            let debug_snapshot = adapter.debug.then(|| result.clone());
            let bounded = crate::mcp::response::result_for_presented(
                Ok(result),
                &adapter.response,
                output_format,
                operation,
                title,
                explain,
                discovery,
            );
            let text = bounded
                .content
                .first()
                .and_then(rmcp::model::ContentBlock::as_text)
                .map(|block| block.text.as_str())
                .ok_or_else(|| anyhow::anyhow!("acdd_read response has no text content"))?;
            if bounded.is_error == Some(true) {
                bail!("{text}");
            }
            if let Some(debug_result) = debug_snapshot {
                let cli_cmd = {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    if args.is_empty() {
                        "serve".to_string()
                    } else {
                        args.join(" ")
                    }
                };
                crate::telemetry::log_cli(&root, &cli_cmd, None, &debug_result);
            }
            text.to_owned()
        } else {
            format::compact(&mut result, operation, explain);
            let formatted =
                format::render_for_operation(&result, output_format, title, discovery, operation);
            if adapter.debug {
                let cli_cmd = {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    if args.is_empty() {
                        "serve".to_string()
                    } else {
                        args.join(" ")
                    }
                };
                crate::telemetry::log_cli(&root, &cli_cmd, None, &result);
            }
            formatted
        };
        if let Err(e) = writeln!(output, "{formatted}") {
            if e.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(e.into());
        }
        Ok(())
    }
}

fn exec_cli_state_command(
    root: &Path,
    state_db: Option<&Path>,
    adapter: &acdd_core::config::Adapter,
    output_format: OutputFormat,
    title: &str,
    cmd: StateCommand,
) -> Result<(Value, Option<String>)> {
    let worktree = match &cmd {
        StateCommand::Claim(req) => req.worktree.as_deref(),
        StateCommand::Verify(req) => req.worktree.as_deref(),
        StateCommand::Submit(req) => req.worktree.as_deref(),
        StateCommand::Snapshot(req) => req.worktree.as_deref(),
        StateCommand::Rollback(req) => req.worktree.as_deref(),
        StateCommand::CallPluginTool(req) => req.worktree.as_deref(),
        _ => None,
    }
    .map(Path::to_path_buf);
    let res = execute_state_command(
        root,
        worktree.as_deref().unwrap_or(root),
        state_db,
        adapter,
        cmd,
    )?;
    let md = (output_format == OutputFormat::Markdown)
        .then(|| format::render_state_command_markdown(&res, title));
    let val = if output_format == OutputFormat::Json || adapter.debug {
        serde_json::to_value(&res)?
    } else {
        Value::Null
    };
    Ok((val, md))
}

fn exec_cli_state_query(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    adapter: &acdd_core::config::Adapter,
    output_format: OutputFormat,
    title: &str,
    query: StateQuery,
) -> Result<(Value, Option<String>)> {
    let res = execute_state_query(root, worktree, state_db, adapter, query)?;
    let md = (output_format == OutputFormat::Markdown)
        .then(|| format::render_state_query_markdown(&res, title));
    let val = if output_format == OutputFormat::Json || adapter.debug {
        serde_json::to_value(&res)?
    } else {
        Value::Null
    };
    Ok((val, md))
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use acdd_core::errors::{ErrorCode, StateError};
    use clap::Parser;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "acdd-ut-{tag}-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cli_exit_codes_preflight_init_validate_and_flat_dispatch() {
        // 1. Exact ErrorCode -> exit code mapping (v7 §4.3: 2/3/4)
        let exit_cases = [
            (ErrorCode::ConfigMigrationRequired, 2),
            (ErrorCode::ProfileSchemaInvalid, 2),
            (ErrorCode::TaskAlreadyClaimed, 3),
            (ErrorCode::OutOfScopeChanges, 3),
            (ErrorCode::GateVerificationFailed, 4),
            (ErrorCode::BackendConflict, 4),
        ];
        for (code, expected) in exit_cases {
            assert_eq!(StateError::new(code, "test").exit_code(), expected, "{code:?}");
        }

        // 2. Flat CLI subcommand parsing preserves Cli/Command AST & removes legacy trees
        for args in [
            vec!["acdd", "init"],
            vec!["acdd", "start", "--goal", "ship feature", "--name", "m1"],
            vec!["acdd", "start", "--goal", "ship feature", "--name", "m1", "--profile", "in_place"],
            vec!["acdd", "draft", "milestones", "my-slug"],
            vec!["acdd", "draft", "milestones", "my-slug", "--profile", "code"],
            vec!["acdd", "status", "--run", "m1"],
            vec!["acdd", "list"],
            vec!["acdd", "show", "--run", "m1"],
            vec!["acdd", "next", "--run", "m1"],
            vec!["acdd", "claim", "t1.build", "--run", "m1", "--worker", "w1"],
            vec!["acdd", "verify", "t1.build", "--run", "m1"],
            vec!["acdd", "submit", "t1.build", "--run", "m1", "--action", "pass"],
            vec!["acdd", "extend-scope", "t1.build", "--run", "m1", "--path", "src/a.rs", "--reason", "need helper"],
            vec!["acdd", "reopen", "t1.build", "--run", "m1"],
            vec!["acdd", "sync", "--run", "m1"],
            vec!["acdd", "subtask", "t1", "s1", "--run", "m1", "--status", "completed", "--evidence", "ok"],
            vec!["acdd", "blackboard", "--run", "m1", "--action", "read"],
            vec!["acdd", "snapshot", "snap1", "--run", "m1"],
            vec!["acdd", "rollback", "snap1", "--run", "m1"],
            vec!["acdd", "integrate", "--run", "m1"],
            vec!["acdd", "clear", "--run", "m1"],
            vec!["acdd", "run", "--run", "m1"],
            vec!["acdd", "tool", "code_extra"],
            vec!["acdd", "validate"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "failed to parse {args:?}");
        }
        for legacy in [
            vec!["acdd", "milestone", "list"],
            vec!["acdd", "plan", "list"],
            vec!["acdd", "workflow", "list"],
            vec!["acdd", "task", "list"],
            vec!["acdd", "checkpoint", "save"],
            vec!["acdd", "migrate"],
        ] {
            assert!(Cli::try_parse_from(&legacy).is_err(), "legacy cmd should be removed: {legacy:?}");
        }

        // 3. `acdd init` picks inherit: code vs default and writes canonical .gitignore block
        for (acdd_yaml, expected_inherit) in [
            ("modules:\n  code: true\n  docs: true\n  state: true\n", "inherit: code"),
            ("modules:\n  code: false\n  docs: true\n  state: true\n", "inherit: default"),
        ] {
            let dir = TempDir::new("cli-init");
            let root = dir.path();
            assert!(acdd_core::git::git_command(root, &["init"]).unwrap().status.success());
            fs::write(root.join("acdd.yaml"), acdd_yaml).unwrap();

            let mut out = Vec::new();
            let cli = Cli::try_parse_from([
                "acdd",
                "--root",
                root.to_str().unwrap(),
                "init",
            ])
            .unwrap();
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(cli.run_with_writer(&mut out))
                .unwrap();

            let profile = fs::read_to_string(root.join(".acdd/profile.yaml")).unwrap();
            assert!(profile.contains(expected_inherit), "{profile}");
            let gi = fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(gi.contains(".acdd/*.sqlite*"), "{gi}");
            assert!(acdd_core::git::git_command(root, &["check-ignore", "-q", ".acdd/state.sqlite"])
                .unwrap()
                .status
                .success());

            if expected_inherit == "inherit: default" {
                let cli_force = Cli::try_parse_from([
                    "acdd",
                    "--root",
                    root.to_str().unwrap(),
                    "init",
                    "--force",
                ])
                .unwrap();
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(cli_force.run_with_writer(&mut out))
                    .unwrap();
                assert!(fs::read_to_string(root.join(".acdd/profile.yaml"))
                    .unwrap()
                    .contains("inherit: code"));
            }
        }

        // 4. `CONFIG_MIGRATION_REQUIRED` preflight blocks legacy 0.5.x config keys
        let dir = TempDir::new("cli-migrate");
        let root = dir.path();
        fs::write(root.join("acdd.yaml"), "milestones:\n  path: docs/milestones\ntasks_db: .acdd/tasks.sqlite\n").unwrap();
        let err = super::ensure_migrated_preflight(root, None).unwrap_err();
        let state_err = StateError::from_anyhow(&err).expect("StateError");
        assert_eq!(state_err.code, ErrorCode::ConfigMigrationRequired);
        assert_eq!(state_err.exit_code(), 2);

        // 5. `acdd validate` succeeds on valid workspace with bound commands and fails on unbound/broken profile
        let vdir = TempDir::new("cli-validate");
        let vroot = vdir.path();
        fs::create_dir_all(vroot.join(".acdd")).unwrap();
        fs::write(vroot.join("acdd.yaml"), "modules:\n  code: true\n  docs: true\n  state: true\n").unwrap();
        fs::write(
            vroot.join(".acdd/profile.yaml"),
            "version: 2\ninherit: default\ncommands:\n  test: \"cargo test --lib {test_filter}\"\n  build_check: \"cargo check --lib\"\n  verify_all: \"cargo test --lib\"\n",
        )
        .unwrap();
        let val = super::validate::run(vroot, None).unwrap();
        assert_eq!(val["valid"], serde_json::json!(true), "{val}");

        fs::write(vroot.join(".acdd/profile.yaml"), "version: 2\ninherit: default\n").unwrap();
        let unbound = super::validate::run(vroot, None).unwrap();
        assert_eq!(unbound["valid"], serde_json::json!(false), "{unbound}");
        assert_eq!(unbound["diagnostics"][1]["code"], "COMMAND_UNBOUND");

        fs::write(vroot.join(".acdd/profile.yaml"), "version: 1\nbogus: true\n").unwrap();
        let bad = super::validate::run(vroot, None).unwrap();
        assert_eq!(bad["valid"], serde_json::json!(false), "{bad}");

        // 6. Default `.acdd/state.sqlite` keeps `state_db_override == None` (enabling `state.bak.sqlite`) even from linked worktrees.
        let cfg_default = crate::state_bridge::AcddConfig::default();
        let eng_default = crate::state_bridge::state_engine_config(vroot, vroot, None, &cfg_default);
        assert!(eng_default.state_db_override.is_none());
        let cfg_custom = crate::state_bridge::AcddConfig {
            state_db: ".acdd/custom.sqlite".to_owned(),
            ..Default::default()
        };
        let eng_custom = crate::state_bridge::state_engine_config(vroot, vroot, None, &cfg_custom);
        assert!(eng_custom.state_db_override.is_some());
    }
}
