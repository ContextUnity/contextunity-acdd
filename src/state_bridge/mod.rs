//! Host bridge wiring `acdd-core`, `acdd-index`, and `acdd-state`.

pub mod code_domain;
pub mod docs_domain;

pub use code_domain::CodeDomainProvider;
pub use docs_domain::DocsDomainProvider;

use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, sha256_hex};
use acdd_core::git::primary_worktree_root;
use acdd_core::ids::RunId;
use acdd_core::ports::{CompositeDomainProvider, DomainProvider};
use acdd_index::db::reader::open_raw_confined_readonly;
use acdd_state::types::NodeStatus;
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateEngineConfig, StateQuery,
    StateQueryEngine, StateQueryOutput,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Host workspace configuration type alias matching [`Adapter`].
pub type AcddConfig = Adapter;

/// Explicit CLI/MCP invocation settings replacing legacy `acdd_tasks::tasks::configuration::InvocationSettings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationSettings {
    /// Primary workspace root.
    pub root: PathBuf,
    /// Explicit `--config` / `acdd.yaml` path override.
    pub config: Option<PathBuf>,
    /// Explicit `--profile` override.
    pub profile: Option<PathBuf>,
    /// Explicit `--state-db` override.
    pub state_db: Option<PathBuf>,
    /// Resolved `code-map.sqlite` path.
    pub code_db: PathBuf,
}

/// Active W3C Trace Context span metadata resolved for an active worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSpanContext {
    /// Active run identifier.
    pub run_id: RunId,
    /// Active `in_progress` node ID bound to the worktree.
    pub node_id: String,
    /// Run root W3C 32-hex trace ID.
    pub trace_id: String,
    /// Active node W3C 16-hex span ID.
    pub span_id: String,
}

/// Resolve the effective `state.sqlite` path from workspace root, optional `--state-db` override, and [`Adapter`].
pub fn resolve_effective_state_db(
    root: &Path,
    state_db: Option<&Path>,
    config: &AcddConfig,
) -> Result<PathBuf, StateError> {
    let _ = config;
    acdd_state::usecases::deps::try_resolve_effective_state_db(root, state_db)
}

/// Build the composite [`CompositeDomainProvider`] for the workspace (`code`, `docs`, and optional external providers)
/// honoring an optional explicit `code_db` override.
///
/// Built-in `code` and `docs` follow `config.modules.is_enabled(...)` (default-on unless disabled);
/// external providers follow `config.modules.is_domain_enabled(id)` (default-off unless `modules.domains.<id>: true`).
pub fn build_domain_providers(
    root: &Path,
    config: &AcddConfig,
    code_db: Option<&Path>,
    extra_providers: Vec<Arc<dyn DomainProvider>>,
) -> Result<Arc<CompositeDomainProvider>, StateError> {
    let code_db_owned = acdd_core::config::resolve_custom_db_path(root, code_db);
    let mut providers: Vec<Arc<dyn DomainProvider>> = vec![
        Arc::new(CodeDomainProvider::with_code_db(
            root.to_path_buf(),
            config.clone(),
            code_db_owned.clone(),
        )),
        Arc::new(DocsDomainProvider::with_code_db(
            root.to_path_buf(),
            config.clone(),
            code_db_owned,
        )),
    ];
    providers.extend(extra_providers);
    let mut flags = BTreeMap::new();
    for provider in &providers {
        let id = provider.domain_id();
        let enabled = match id {
            "code" | "docs" => config.modules.is_enabled(id),
            _ => config.modules.is_domain_enabled(id),
        };
        flags.insert(id.to_owned(), enabled);
    }
    let composite = CompositeDomainProvider::with_module_flags(providers, flags)?;
    Ok(Arc::new(composite))
}

/// Construct a [`StateEngineConfig`] from workspace root, active worktree, optional `--state-db` / `--db` overrides, and [`Adapter`].
pub fn state_engine_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &AcddConfig,
) -> Result<StateEngineConfig, StateError> {
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let default_state_db = primary.join(".acdd/state.sqlite");
    let configured = resolve_effective_state_db(root, state_db, config)?;
    let state_db_override = (configured != default_state_db).then_some(configured);
    let linked_workspaces = config
        .linked_workspaces
        .iter()
        .map(|lw| (lw.name.clone(), lw.path.clone()))
        .collect();
    let code_db_path = (config.modules.is_enabled("code") || config.modules.is_enabled("docs"))
        .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    Ok(StateEngineConfig {
        workspace_root: primary,
        worktree: worktree.to_path_buf(),
        state_db_override,
        code_db_path,
        spec_collections: config.specs.clone(),
        profiles_dirs: config.profiles_dirs.clone(),
        linked_workspaces,
        modules: config.modules.clone(),
        response_policy: config.response.clone(),
        retain_traces: config.retain_traces,
    })
}

pub(crate) fn unsupported_domain_action(
    domain: &str,
    action: &str,
) -> anyhow::Result<acdd_core::ports::DomainActionResult> {
    Err(StateError::new(
        ErrorCode::DomainCapabilityUnavailable,
        format!("{domain} domain does not expose action '{action}'"),
    )
    .into())
}

/// Execute a mutating [`StateCommand`] against the workspace state engine using caller-supplied
/// [`CompositeDomainProvider`] and optional `--state-db` / `--db` overrides.
pub fn execute_state_command(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &AcddConfig,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
) -> Result<StateCommandOutput, StateError> {
    execute_state_command_with_config(root, worktree, state_db, code_db, config, providers, cmd, false)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_state_command_with_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &AcddConfig,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
    explicit_config: bool,
) -> Result<StateCommandOutput, StateError> {
    if let StateCommand::Snapshot(req) = &cmd {
        acdd_core::fs::check_ancestors_no_symlinks(root, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
        let wt = req.worktree.as_deref().unwrap_or(worktree); acdd_core::fs::check_ancestors_no_symlinks(wt, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    }
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    if let StateCommand::Snapshot(req) = cmd {
        return acdd_state::usecases::execute_snapshot_readonly(&engine_cfg, req);
    }
    let engine = StateEngine::open(engine_cfg, providers)?;
    if explicit_config {
        engine.execute_with_source_config(cmd, config)
    } else {
        engine.execute(cmd)
    }
}

/// Execute a read-only [`StateQuery`] against the workspace state query engine (`SQLITE_OPEN_READ_ONLY`)
/// using caller-supplied [`CompositeDomainProvider`] and optional `--state-db` / `--db` overrides.
pub fn execute_state_query(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &AcddConfig,
    providers: Arc<CompositeDomainProvider>,
    query: StateQuery,
) -> Result<StateQueryOutput, StateError> {
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    let engine = StateQueryEngine::open_read_only(engine_cfg, providers)?;
    engine.query(query)
}

pub(super) fn open_code_db_readonly(
    root: &Path,
    worktree: &Path,
    code_db: Option<&Path>,
) -> Option<Connection> {
    let primary = primary_worktree_root(worktree)
        .or_else(|| primary_worktree_root(root))
        .unwrap_or_else(|| root.to_path_buf());
    let wt_override: Option<PathBuf> = code_db.filter(|p| p.is_absolute()).and_then(|p| {
        let canon = acdd_core::fs::canonicalize_macos_var(p);
        canon
            .strip_prefix(acdd_core::fs::canonicalize_macos_var(&primary))
            .or_else(|_| canon.strip_prefix(acdd_core::fs::canonicalize_macos_var(root)))
            .ok()
            .map(Path::to_path_buf)
    });
    let wt_candidate = acdd_core::config::resolve_code_db_path(
        worktree,
        wt_override.as_deref().or(code_db),
    );
    let primary_candidate = acdd_core::config::resolve_code_db_path(&primary, code_db);
    for candidate in [wt_candidate, primary_candidate] {
        if candidate.is_file() {
            if let Ok(conn) = open_raw_confined_readonly(&candidate) {
                return Some(conn);
            }
        }
    }
    None
}

pub(super) fn combine_file_hashes(file_hashes: &BTreeMap<String, String>) -> String {
    let mut buf = String::new();
    for (path, digest) in file_hashes {
        buf.push_str(path);
        buf.push(':');
        buf.push_str(digest);
        buf.push('\n');
    }
    sha256_hex(buf.as_bytes())
}

pub(super) fn budget_items(kind: &str, items: Vec<Value>, max_bytes: usize) -> Value {
    let total = items.len();
    let mut envelope = serde_json::Map::new();
    envelope.insert("kind".to_owned(), json!(kind));
    envelope.insert("items".to_owned(), json!([]));
    envelope.insert("total".to_owned(), json!(total));
    envelope.insert("truncated".to_owned(), json!(false));

    let base_bytes = estimate_json_bytes(&Value::Object(envelope.clone()));
    let mut used_bytes = base_bytes;
    let mut kept = Vec::new();
    for item in items {
        let item_bytes = estimate_json_bytes(&item);
        let sep = usize::from(!kept.is_empty());
        if used_bytes.saturating_add(sep).saturating_add(item_bytes) > max_bytes
            && !kept.is_empty()
        {
            break;
        }
        used_bytes = used_bytes.saturating_add(sep).saturating_add(item_bytes);
        kept.push(item);
    }
    let truncated = kept.len() < total;
    envelope.insert("items".to_owned(), Value::Array(kept));
    envelope.insert("truncated".to_owned(), json!(truncated));
    Value::Object(envelope)
}

/// Resolve the active W3C span context (`trace_id`, `span_id`) for `worktree` in read-only mode,
/// honoring an optional explicit `--state-db` override.
pub fn resolve_worktree_span_context(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    config: &AcddConfig,
) -> Result<Option<WorktreeSpanContext>, StateError> {
    if !config.modules.is_enabled("state") {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    let resolved_state_db = resolve_effective_state_db(root, state_db, config)?;
    let resolved = crate::engine::reader::resolve_worktree_active_run_node(
        &resolved_state_db,
        worktree,
        &|n| n.status == NodeStatus::InProgress && n.active_span_id.is_some(),
    )?;
    Ok(resolved.and_then(|(run, _, node)| {
        node.active_span_id
            .as_ref()
            .map(|span_id| WorktreeSpanContext {
                run_id: run.run_id.clone(),
                node_id: node.node_id.clone(),
                trace_id: run.trace_id.clone(),
                span_id: span_id.clone(),
            })
    }))
}
