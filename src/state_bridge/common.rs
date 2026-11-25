//! Shared leaf constants, path/adapter resolution, SQLite read helpers, and state execution helpers for `state_bridge`.

use acdd_core::config::{Adapter, ACDD_CONFIG_FILE};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, sha256_hex};
use acdd_core::git::primary_worktree_root;
use acdd_core::ports::{CompositeDomainProvider, DomainActionResult, DomainProvider};
use acdd_core::subsystems::SubsystemId;
use acdd_index::db::reader::open_raw_confined_readonly;
use acdd_state::usecases::{
    StateCommand, StateCommandOutput, StateEngine, StateEngineConfig, StateQuery, StateQueryEngine,
    StateQueryOutput,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Mandatory instruction banner emitted in `docs.instructions` to direct workers to scoped `AGENTS.md` files.
pub(super) const DOCS_INSTRUCTIONS_DIRECTIVE: &str =
    "MANDATORY: read repository rules for respective scope.";
/// Maximum top-level Markdown headings extracted per instruction file in `docs.instructions`.
pub(super) const DOCS_INSTRUCTION_MAX_HEADINGS: usize = 10;
/// Maximum fallback invariants returned when `code-map.sqlite` has no scope-matched `governs` edges or `admitted_scope` is empty.
pub(super) const DOCS_INVARIANT_FALLBACK_ITEM_LIMIT: usize = 8;
/// Maximum rule summary characters kept per entry in `docs.invariants` so full rules stay readable without mid-word clipping.
pub(super) const DOCS_INVARIANT_MAX_RULE_CHARS: usize = 400;
/// Maximum declarations per file below which private helpers remain visible alongside public symbols in `code.symbols`.
pub(super) const CODE_SYMBOLS_SMALL_FILE_PRIVATE_THRESHOLD: usize = 5;
/// Maximum scoped symbol entries retained in `code.symbols` after per-file round-robin ranking.
pub(super) const CODE_SYMBOLS_MAX_ITEMS: usize = 20;
/// Maximum single-line signature character length kept per entry in `code.symbols`.
pub(super) const CODE_SYMBOLS_MAX_SIG_CHARS: usize = 120;
/// Maximum scoped files queried against the indexed test graph when building `code.tests`.
pub(super) const CODE_TESTS_LENS_SCOPED_FILE_LIMIT: usize = 10;
/// Maximum inbound test entries fetched per scoped file from the indexed test graph in `code.tests`.
pub(super) const CODE_TESTS_LENS_PAGE_SIZE: usize = 25;
/// Maximum inbound graph test entries retained per scoped file before interleaving sibling files in `code.tests`.
pub(super) const CODE_TESTS_PER_FILE_GRAPH_LIMIT: usize = 8;
/// Maximum total test entries retained in `code.tests` per claim context.
pub(super) const CODE_TESTS_MAX_ITEMS: usize = 12;
/// Maximum external-caller symbol entries retained in `code.external_callers` per claim context.
pub(super) const CODE_EXTERNAL_CALLERS_MAX_ITEMS: usize = 12;
/// Maximum candidate call edges rendered in `code.seam` reachability failure diagnostics.
pub(super) const CODE_SEAM_DIAGNOSTIC_EDGE_LIMIT: usize = 10;

/// Default non-tight byte budget used across domain-provider context unit tests.
#[cfg(test)]
pub(super) const TEST_DEFAULT_CONTEXT_MAX_BYTES: usize = 8_192;
/// Tight byte budget asserting deterministic `truncated: true` capping on `docs.instructions`.
#[cfg(test)]
pub(super) const TEST_TIGHT_INSTRUCTIONS_MAX_BYTES: usize = 220;
/// Full 32 KiB context budget used in `code_domain` external-caller unit tests.
#[cfg(test)]
pub(super) const TEST_LARGE_CONTEXT_MAX_BYTES: usize = 32_768;

/// Collapse whitespace into a single line and truncate cleanly at a UTF-8 word boundary within `max_chars`.
pub(super) fn truncate_single_line(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let trimmed = prefix
            .rfind(char::is_whitespace)
            .filter(|&idx| idx * 2 >= prefix.len())
            .map_or(prefix.as_str(), |idx| prefix[..idx].trim_end());
        format!("{trimmed}...")
    } else {
        prefix
    }
}

/// Construct a [`CompositeDomainProvider`] applying `config.modules` enablement flags to `providers`.
pub(super) fn build_composite_providers(
    config: &Adapter,
    providers: Vec<Arc<dyn DomainProvider>>,
) -> Result<Arc<CompositeDomainProvider>, StateError> {
    let mut flags = BTreeMap::new();
    for provider in &providers {
        let id = provider.domain_id();
        let enabled = if id == SubsystemId::Code.as_str() || id == SubsystemId::Docs.as_str() {
            config.modules.is_enabled(id)
        } else {
            config.modules.is_domain_enabled(id)
        };
        flags.insert(id.to_owned(), enabled);
    }
    let composite = CompositeDomainProvider::with_module_flags(providers, flags)?;
    Ok(Arc::new(composite))
}

/// Construct a [`StateEngineConfig`] from workspace root, active worktree, optional `--state-db` / `--db` overrides, and [`Adapter`].
pub(crate) fn state_engine_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
) -> Result<StateEngineConfig, StateError> {
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let default_state_db = primary.join(".acdd/state.sqlite");
    let configured = acdd_state::usecases::deps::try_resolve_effective_state_db(root, state_db)?;
    let state_db_override = (configured != default_state_db).then_some(configured);
    let linked_workspaces = config
        .linked_workspaces
        .iter()
        .map(|lw| (lw.name.clone(), lw.path.clone()))
        .collect();
    let code_db_path = (config.modules.is_subsystem_enabled(SubsystemId::Code)
        || config.modules.is_subsystem_enabled(SubsystemId::Docs))
    .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    let allowed_binary_extensions = config.limits.normalized_allowed_binary_extensions()?;
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
        max_snapshot_file_bytes: config.limits.max_snapshot_file_bytes,
        allowed_binary_extensions,
    })
}

pub(crate) fn unsupported_domain_action(
    domain: &str,
    action: &str,
) -> anyhow::Result<DomainActionResult> {
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
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
) -> Result<StateCommandOutput, StateError> {
    execute_state_command_with_config(
        root, worktree, state_db, code_db, config, providers, cmd, false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_state_command_with_config(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    cmd: StateCommand,
    explicit_config: bool,
) -> Result<StateCommandOutput, StateError> {
    if let StateCommand::Snapshot(req) = &cmd {
        acdd_core::fs::check_git_entry_no_symlinks(root)
            .map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
        let wt = req.worktree.as_deref().unwrap_or(worktree);
        acdd_core::fs::check_git_entry_no_symlinks(wt)
            .map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
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
    config: &Adapter,
    providers: Arc<CompositeDomainProvider>,
    query: StateQuery,
) -> Result<StateQueryOutput, StateError> {
    let engine_cfg = state_engine_config(root, worktree, state_db, code_db, config)?;
    let engine = StateQueryEngine::open_read_only(engine_cfg, providers)?;
    engine.query(query)
}

pub(super) fn worktree_code_db_candidate(
    root: &Path,
    worktree: &Path,
    code_db: Option<&Path>,
) -> (PathBuf, PathBuf) {
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
    let wt_candidate =
        acdd_core::config::resolve_code_db_path(worktree, wt_override.as_deref().or(code_db));
    let primary_candidate = acdd_core::config::resolve_code_db_path(&primary, code_db);
    (wt_candidate, primary_candidate)
}

pub(super) fn open_code_db_readonly(
    root: &Path,
    worktree: &Path,
    code_db: Option<&Path>,
) -> Option<Connection> {
    let (wt_candidate, primary_candidate) = worktree_code_db_candidate(root, worktree, code_db);
    for candidate in [wt_candidate, primary_candidate] {
        if candidate.is_file() {
            if let Ok(conn) = open_raw_confined_readonly(&candidate) {
                return Some(conn);
            }
        }
    }
    None
}

pub(super) fn local_adapter_path_for_worktree(
    root: &Path,
    worktree: &Path,
    fallback: &Adapter,
) -> Option<PathBuf> {
    let abs = fallback.adapter_path.as_deref()?;
    let canon = acdd_core::fs::canonicalize_macos_var(abs);
    let primary = primary_worktree_root(worktree).or_else(|| primary_worktree_root(root));
    let rel = canon
        .strip_prefix(acdd_core::fs::canonicalize_macos_var(worktree))
        .ok()
        .or_else(|| {
            canon
                .strip_prefix(acdd_core::fs::canonicalize_macos_var(root))
                .ok()
        })
        .or_else(|| {
            primary.as_deref().and_then(|p| {
                canon
                    .strip_prefix(acdd_core::fs::canonicalize_macos_var(p))
                    .ok()
            })
        })?;
    acdd_core::fs::checked_child(worktree, rel)
        .ok()
        .filter(|p| p.is_file())
}

pub(crate) fn resolve_worktree_adapter(
    root: &Path,
    worktree: &Path,
    fallback: &Adapter,
) -> anyhow::Result<Adapter> {
    let local_path = local_adapter_path_for_worktree(root, worktree, fallback);
    let has_default = acdd_core::fs::checked_child(worktree, Path::new(ACDD_CONFIG_FILE))
        .is_ok_and(|p| p.is_file());
    if local_path.is_some() || has_default {
        return acdd_core::config::load_adapter(worktree, local_path.as_deref()).map_err(|err| {
            anyhow::anyhow!(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to load worktree adapter: {err}"),
            ))
        });
    }
    Ok(fallback.clone())
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
        if used_bytes.saturating_add(sep).saturating_add(item_bytes) > max_bytes && !kept.is_empty()
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
