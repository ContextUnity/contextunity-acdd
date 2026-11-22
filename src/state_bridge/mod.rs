//! Host bridge wiring `acdd-core`, `acdd-index`, and `acdd-state`.

pub mod code_domain;
pub mod docs_domain;
mod overview;

pub use code_domain::CodeDomainProvider;
pub use docs_domain::DocsDomainProvider;
pub(crate) use overview::overview;

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

/// Default non-tight byte budget used across domain-provider context unit tests.
#[cfg(test)]
pub(super) const TEST_DEFAULT_CONTEXT_MAX_BYTES: usize = 8_192;
/// Tight byte budget asserting deterministic `truncated: true` capping on `docs.instructions`.
#[cfg(test)]
pub(super) const TEST_TIGHT_INSTRUCTIONS_MAX_BYTES: usize = 220;
/// Full 32 KiB context budget used in `code_domain` external-caller unit tests.
#[cfg(test)]
pub(super) const TEST_LARGE_CONTEXT_MAX_BYTES: usize = 32_768;

use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{estimate_json_bytes, sha256_hex};
use acdd_core::git::primary_worktree_root;
use acdd_core::ids::RunId;
use acdd_core::ports::{CompositeDomainProvider, DomainProvider};
use acdd_core::subsystems::SubsystemId;
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
        acdd_core::fs::check_git_entry_no_symlinks(root).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
        let wt = req.worktree.as_deref().unwrap_or(worktree);
        acdd_core::fs::check_git_entry_no_symlinks(wt).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
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
    let wt_candidate = acdd_core::config::resolve_code_db_path(
        worktree,
        wt_override.as_deref().or(code_db),
    );
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

pub(super) fn resolve_worktree_adapter(
    root: &Path,
    worktree: &Path,
    fallback: &Adapter,
) -> anyhow::Result<Adapter> {
    let local_path = local_adapter_path_for_worktree(root, worktree, fallback);
    let has_default = acdd_core::fs::checked_child(
        worktree,
        Path::new(acdd_core::config::ACDD_CONFIG_FILE),
    )
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

fn resolve_worktree_active_run_node(
    state_db: &Path,
    worktree: &Path,
    predicate: &dyn Fn(&acdd_state::types::GateNodeRecord) -> bool,
) -> std::result::Result<
    Option<(
        acdd_state::types::RunRecord,
        acdd_state::types::RunGraphSnapshot,
        acdd_state::types::GateNodeRecord,
    )>,
    StateError,
> {
    if !std::fs::symlink_metadata(state_db).is_ok_and(|m| m.file_type().is_file()) {
        return Ok(None);
    }
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db.to_path_buf());
    let wt_canon = acdd_core::fs::canonicalize_macos_var(worktree);
    let wt_canon_str = wt_canon.to_string_lossy();
    let wt_raw_str = worktree.to_string_lossy();
    let run = match acdd_state::store::traits::StateReader::resolve_active_run_by_worktree(
        &reader,
        &wt_canon_str,
    )? {
        Some(r) => Some(r),
        None if wt_raw_str != wt_canon_str => {
            acdd_state::store::traits::StateReader::resolve_active_run_by_worktree(
                &reader,
                &wt_raw_str,
            )?
        }
        None => None,
    };
    let Some(run) = run else {
        return Ok(None);
    };
    let Some(graph) =
        acdd_state::store::traits::StateReader::load_run_graph(&reader, &run.run_id)?
    else {
        return Ok(None);
    };
    let matches_wt = |candidate: Option<&str>| {
        candidate.is_some_and(|w| {
            w == wt_canon_str.as_ref()
                || w == wt_raw_str.as_ref()
                || acdd_core::fs::canonicalize_macos_var(Path::new(w)) == wt_canon
        })
    };
    let active_node = graph
        .nodes
        .iter()
        .find(|n| predicate(n) && matches_wt(n.worktree.as_deref()))
        .or_else(|| {
            if matches_wt(run.worktree.as_deref()) {
                let mut candidates = graph.nodes.iter().filter(|n| predicate(n));
                let first = candidates.next();
                if candidates.next().is_none() {
                    first
                } else {
                    None
                }
            } else {
                None
            }
        })
        .cloned();
    Ok(active_node.map(|node| (run, graph, node)))
}

fn find_gate_in_profile<'a>(
    profile: &'a acdd_state::profile::ResolvedGateProfile,
    cursor: &acdd_core::ids::NodeCursor,
) -> Option<&'a acdd_state::profile::GateDef> {
    match cursor {
        acdd_core::ids::NodeCursor::Root(gates) => {
            let mut list = &profile.gates;
            for (idx, g) in gates.iter().enumerate() {
                let found = list.iter().find(|def| def.id == g.as_str())?;
                if idx + 1 == gates.len() {
                    return Some(found);
                }
                list = &found.gates;
            }
            None
        }
        acdd_core::ids::NodeCursor::Task { gates, .. } => {
            let first = gates.first()?;
            for item in &profile.gates {
                if let Some(ref tasks_block) = item.tasks {
                    if let Some(found) = tasks_block.gates.iter().find(|def| def.id == first.as_str()) {
                        return Some(found);
                    }
                }
            }
            None
        }
    }
}

/// Resolve read-only active `in_progress` node scope/policy context for `acdd_read`.
pub fn active_node_context(
    root: &Path,
    adapter: &Adapter,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
) -> anyhow::Result<Option<crate::engine::reader::ActiveNodeContext>> {
    if !adapter.modules.is_enabled("state") {
        return Ok(None);
    }
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let state_db = acdd_core::config::resolve_effective_state_db_path(&primary, state_db, adapter);
    let Some((run, graph, node)) = resolve_worktree_active_run_node(&state_db, root, &|n| {
        n.status == NodeStatus::InProgress
            && matches!(
                n.kind,
                acdd_state::types::NodeKind::RootGate | acdd_state::types::NodeKind::TaskGate
            )
    })?
    else {
        return Ok(None);
    };
    let node_cursor = acdd_core::ids::NodeCursor::from_node_id(&node.node_id)?.to_string();
    let admitted_scope = acdd_state::machine::container_scope(&graph, &node);
    let profile =
        serde_json::from_str::<acdd_state::profile::ResolvedGateProfile>(&run.profile_json).ok();
    let protected_paths = profile
        .as_ref()
        .map(|p| p.protected_paths.clone())
        .unwrap_or_default();
    let has_command = profile
        .as_ref()
        .and_then(|profile| {
            let cursor = acdd_core::ids::NodeCursor::from_node_id(&node.node_id).ok()?;
            let gate_def = find_gate_in_profile(profile, &cursor)?;
            Some(
                gate_def
                    .command
                    .as_ref()
                    .is_some_and(|cmd| profile.commands.contains_key(cmd)),
            )
        })
        .unwrap_or(false);

    let default_state_db = primary.join(".acdd/state.sqlite");
    let state_db_override = (state_db != default_state_db).then(|| state_db.clone());
    let code_db_path = (adapter.modules.is_enabled("code") || adapter.modules.is_enabled("docs"))
        .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    let engine_cfg = StateEngineConfig {
        workspace_root: primary,
        worktree: root.to_path_buf(),
        state_db_override,
        code_db_path,
        spec_collections: adapter.specs.clone(),
        profiles_dirs: adapter.profiles_dirs.clone(),
        linked_workspaces: adapter
            .linked_workspaces
            .iter()
            .map(|lw| (lw.name.clone(), lw.path.clone()))
            .collect(),
        modules: adapter.modules.clone(),
        response_policy: adapter.response.clone(),
        retain_traces: adapter.retain_traces,
    };
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db);
    let path_policy = acdd_state::usecases::lifecycle::build_path_policy(
        &engine_cfg,
        &reader,
        &[],
        Some(&run),
        Some(&engine_cfg.worktree),
    )
    .unwrap_or_default();

    Ok(Some(crate::engine::reader::ActiveNodeContext {
        run_id: run.run_id,
        node_cursor,
        is_root_gate: node.kind == acdd_state::types::NodeKind::RootGate,
        is_in_place: node.isolation == acdd_state::types::IsolationMode::InPlace,
        baseline_commit: node.baseline_commit.as_ref().map(|c| c.as_str().to_owned()),
        claim_tree: node.claim_tree.as_ref().map(|t| t.as_str().to_owned()),
        admitted_scope,
        protected_paths,
        path_policy,
        has_command,
    }))
}

/// Resolve the active W3C span context (`trace_id`, `span_id`) for `worktree` in read-only mode,
/// honoring an optional explicit `--state-db` override.
pub fn resolve_worktree_span_context(
    root: &Path,
    worktree: &Path,
    state_db: Option<&Path>,
    config: &AcddConfig,
) -> Result<Option<WorktreeSpanContext>, StateError> {
    if !config.modules.is_subsystem_enabled(SubsystemId::State) {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    let resolved_state_db = resolve_effective_state_db(root, state_db, config)?;
    let resolved = resolve_worktree_active_run_node(
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

#[cfg(test)]
mod tests {
    use super::active_node_context;
    use crate::engine::reader::{acdd_read_tool, ReadRequest};
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::ids::WorkerId;
    use acdd_core::model::ReadMode;
    use acdd_core::ports::CompositeDomainProvider;
    use acdd_core::response::{QueryOptions, SourceOptions};
    use acdd_state::usecases::{
        ClaimReq, ClearReq, StartReq, StateCommand, StateCommandOutput, StateEngine,
        StateEngineConfig,
    };
    use std::fs;
    use std::sync::Arc;

    #[test]
    fn read_with_state_module_resolves_active_node_scope_delta_and_hints() {
        let dir = ScopedTempDir::create("acdd-ut-reader-state").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let run_git = |args: &[&str]| {
            let out = acdd_core::git::git_command(&root, args).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".acdd")).unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  builder:\n    mode: subagent\ncommands:\n  build_check: \"cargo check --lib\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          command: build_check\n",
        )
        .unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"p\"\nversion = \"0.1.0\"\n").unwrap();
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
        fs::write(root.join("src/other.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);
        let base_commit = run_git(&["rev-parse", "HEAD"]);
        // Pre-claim in-place edit and pre-claim untracked file captured in `claim_tree`.
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 10 }\n").unwrap();
        fs::write(root.join("src/pre_claim_untracked.rs"), "pub fn pre_claim() {}\n").unwrap();

        let adapter = Adapter::default();
        let engine = StateEngine::open(
            StateEngineConfig {
                workspace_root: root.clone(),
                worktree: root.clone(),
                state_db_override: None,
                code_db_path: None,
                spec_collections: adapter.specs.clone(),
                profiles_dirs: adapter.profiles_dirs.clone(),
                linked_workspaces: std::collections::BTreeMap::new(),
                modules: adapter.modules.clone(),
                response_policy: adapter.response.clone(),
                retain_traces: adapter.retain_traces,
                max_snapshot_file_bytes: None,
                allowed_binary_extensions: None,
            },
            Arc::new(CompositeDomainProvider::new(vec![]).unwrap()),
        )
        .unwrap();
        let started = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                goal: Some("edit scoped".into()),
                run_name: None,
                profile: None,
                scope: vec!["src/scoped.rs".into()],
                target_base: None,
                worktree: false,
                in_place: true,
                worktree_path: None,
            }))
            .unwrap();
        let run_id = match started {
            StateCommandOutput::Start(resp) => resp.run.run_id.to_string(),
            other => panic!("unexpected start output: {other:?}"),
        };
        let claim_resp = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(run_id.clone()),
                node: None,
                worker_id: WorkerId::new("w1").unwrap(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: Some(root.clone()),
            }))
            .unwrap();
        let claim_tree = match claim_resp {
            StateCommandOutput::Claim(c) => c.candidate_tree.to_string(),
            other => panic!("unexpected claim output: {other:?}"),
        };
        assert_ne!(claim_tree, run_git(&["rev-parse", "HEAD^{tree}"]));

        // Post-claim edit + untracked file.
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 42 }\n").unwrap();
        fs::write(root.join("src/untracked.rs"), "pub fn newly_added() {}\n").unwrap();
        fs::write(root.join("src/ leading.rs"), "pub fn spaced() {}\n").unwrap();
        fs::write(root.join("src/leading.rs"), "pub fn plain_name() {}\n").unwrap();

        let page = QueryOptions::resolve(&adapter.response, None, 0, None, None).unwrap();
        let source = SourceOptions::resolve(&adapter.response, None, None, None, 0).unwrap();
        let read = |req: &ReadRequest| {
            let node_ctx = active_node_context(&root, &adapter, None, None).ok().flatten();
            acdd_read_tool(&root, &adapter, req, &page, &source, None, node_ctx.as_ref()).unwrap()
        };
        let req_allowed = ReadRequest {
            path: Some("src/scoped.rs".into()),
            mode: ReadMode::Delta,
            include_hints: true,
            ..Default::default()
        };
        let allowed = read(&req_allowed);
        assert_eq!(allowed["in_write_scope"], "ALLOWED");
        assert_eq!(allowed["baseline_commit"], base_commit);
        assert_eq!(allowed["claim_tree"], claim_tree);
        let delta_text = allowed["content"].as_str().unwrap_or_default();
        assert!(delta_text.contains("-pub fn a() -> i32 { 10 }"), "{delta_text}");
        assert!(delta_text.contains("+pub fn a() -> i32 { 42 }"), "{delta_text}");
        let hints = allowed["Hints"].as_str().unwrap_or_default();
        assert!(hints.contains("acdd_blackboard"), "{hints}");

        let untracked_delta = read(&ReadRequest {
            path: Some("src/untracked.rs".into()),
            mode: ReadMode::Delta,
            ..Default::default()
        });
        let untracked_diff = untracked_delta["content"].as_str().unwrap_or_default();
        assert!(untracked_diff.contains("--- /dev/null"), "{untracked_diff}");
        assert!(untracked_diff.contains("+pub fn newly_added() {}"), "{untracked_diff}");
        for (rel, expected_fn, forbidden_fn) in [
            ("src/ leading.rs", "pub fn spaced()", "plain_name()"),
            ("src/leading.rs", "pub fn plain_name()", "spaced()"),
        ] {
            let delta = read(&ReadRequest {
                path: Some(rel.to_owned()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let diff = delta["content"].as_str().unwrap_or_default();
            assert!(diff.contains(expected_fn), "{rel}: {diff}");
            assert!(!diff.contains(forbidden_fn), "{rel}: {diff}");
        }

        // #12: Untracked file already identical in `claim_tree` must yield empty delta, NOT full `/dev/null` add.
        let pre_claim_untracked_delta = read(&ReadRequest {
            path: Some("src/pre_claim_untracked.rs".into()),
            mode: ReadMode::Delta,
            ..Default::default()
        });
        assert_eq!(
            pre_claim_untracked_delta["content"].as_str(),
            Some(""),
            "untracked file present in claim_tree must return empty delta: {pre_claim_untracked_delta}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let pre_abs = root.join("src/pre_claim_untracked.rs");
            let mut perms = fs::metadata(&pre_abs).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&pre_abs, perms).unwrap();
            let mode_delta = read(&ReadRequest {
                path: Some("src/pre_claim_untracked.rs".into()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let mode_diff = mode_delta["content"].as_str().unwrap_or_default();
            assert_eq!(mode_diff.matches("old mode 100644").count(), 1, "{mode_diff}");
            assert_eq!(mode_diff.matches("new mode 100755").count(), 1, "{mode_diff}");

            fs::write(&pre_abs, "pub fn pre_claim_untracked_v2() {}\n").unwrap();
            let mut perms2 = fs::metadata(&pre_abs).unwrap().permissions();
            perms2.set_mode(0o755);
            fs::set_permissions(&pre_abs, perms2).unwrap();
            let mode_content_delta = read(&ReadRequest {
                path: Some("src/pre_claim_untracked.rs".into()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let mode_content_diff = mode_content_delta["content"].as_str().unwrap_or_default();
            assert_eq!(
                mode_content_diff.matches("old mode 100644").count(),
                1,
                "{mode_content_diff}"
            );
            assert_eq!(
                mode_content_diff.matches("new mode 100755").count(),
                1,
                "{mode_content_diff}"
            );
            assert!(mode_content_diff.contains("+pub fn pre_claim_untracked_v2()"), "{mode_content_diff}");
        }

        let protected_read = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(protected_read["in_write_scope"], "OUT_OF_SCOPE", "{protected_read}");

        let req_outside = ReadRequest {
            path: Some("src/other.rs".into()),
            mode: ReadMode::Lines,
            include_hints: true,
            ..Default::default()
        };
        let outside = read(&req_outside);
        assert_eq!(outside["in_write_scope"], "OUT_OF_SCOPE");
        let outside_hints = outside["Hints"].as_str().unwrap_or_default();
        assert!(outside_hints.contains("acdd_state"), "{outside_hints}");
        assert!(outside_hints.contains("acdd_blackboard"), "{outside_hints}");

        // Unscoped RootGate must also report OUT_OF_SCOPE for protected_paths (`Cargo.toml`).
        engine
            .execute(StateCommand::Clear(ClearReq {
                run: Some(run_id),
                reason: Some("reset for unscoped root gate check".into()),
                force: true,
            }))
            .unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  architect:\n    mode: subagent\n  builder:\n    mode: subagent\ngates:\n  - id: verify\n    role: architect\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n",
        )
        .unwrap();
        let root_start = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                goal: Some("unscoped root check".into()),
                run_name: None,
                profile: None,
                scope: vec![],
                target_base: None,
                worktree: false,
                in_place: true,
                worktree_path: None,
            }))
            .unwrap();
        let root_run_id = match root_start {
            StateCommandOutput::Start(resp) => resp.run.run_id.to_string(),
            other => panic!("unexpected root start: {other:?}"),
        };
        engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(root_run_id),
                node: Some("verify".into()),
                worker_id: WorkerId::new("w1").unwrap(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: Some(root.clone()),
            }))
            .unwrap();
        let root_protected = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(root_protected["in_write_scope"], "OUT_OF_SCOPE", "{root_protected}");
        let root_allowed = read(&ReadRequest {
            path: Some("src/other.rs".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(root_allowed["in_write_scope"], "ALLOWED", "{root_allowed}");
    }
}
