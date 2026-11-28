//! Host bridge wiring `acdd-core`, `acdd-index`, and `acdd-state`.

pub(crate) mod code_domain;
mod common;
pub(crate) mod docs_domain;
mod overview;
pub(crate) mod spec_domain;

pub(crate) use code_domain::CodeDomainProvider;
pub(crate) use common::{execute_state_command_with_config, resolve_gate_proof};
#[cfg(test)]
pub(crate) use common::state_engine_config;
pub use common::{execute_state_command, execute_state_query, InvocationSettings};
pub(crate) use docs_domain::DocsDomainProvider;
pub(crate) use overview::overview;
pub(crate) use spec_domain::SpecDomainProvider;

use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::git::primary_worktree_root;
use acdd_core::ports::{CompositeDomainProvider, DomainProvider};
use acdd_core::subsystems::SubsystemId;
use acdd_core::telemetry::WorktreeSpanContext;
use acdd_state::types::NodeStatus;
use acdd_state::usecases::StateEngineConfig;
use std::path::Path;
use std::sync::Arc;

/// Build the composite [`CompositeDomainProvider`] for the workspace (`code`, `docs`, `spec`, and optional external providers)
/// honoring an optional explicit `code_db` override.
///
/// Built-in `code`, `docs`, and `spec` follow `config.modules.is_enabled(...)` (default-on unless disabled);
/// external providers follow `config.modules.is_domain_enabled(id)` (default-off unless `modules.domains.<id>: true`).
pub fn build_domain_providers(
    root: &Path,
    config: &Adapter,
    code_db: Option<&Path>,
    extra_providers: Vec<Arc<dyn DomainProvider>>,
) -> Result<Arc<CompositeDomainProvider>, AcddError> {
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
        Arc::new(SpecDomainProvider::new(
            root.to_path_buf(),
            config.clone(),
        )),
    ];
    providers.extend(extra_providers);
    common::build_composite_providers(config, providers)
}

/// Inputs for resolving the target checkout directory and optional active run for `acdd exec`.
#[derive(Debug, Clone)]
pub struct ExecResolveInput<'a> {
    /// Explicit `-C` / `--root` passed on the CLI (`ValueSource::CommandLine`).
    pub explicit_cli_root: Option<&'a Path>,
    /// Ambient `ACDD_WORKSPACE_ROOT` environment value when `--root`/`-C` was not passed on CLI.
    pub env_workspace_root: Option<&'a Path>,
    /// Ambient `ACDD_WORKTREE` fallback when `--root`/`-C` and `ACDD_WORKSPACE_ROOT` are absent.
    pub env_worktree: Option<&'a Path>,
    /// Optional global `--config` / `ACDD_CONFIG` override.
    pub explicit_config: Option<&'a Path>,
    /// Optional global `--state-db` / `ACDD_STATE_DB` override.
    pub explicit_state_db: Option<&'a Path>,
    /// Optional `--worker <id>` claim selector.
    pub worker: Option<&'a str>,
    /// Optional `--run <id|name>` (or `ACDD_RUN`) selector.
    pub run_selector: Option<&'a str>,
    /// Whether `--run` came from the CLI rather than `ACDD_RUN`.
    pub run_from_cli: bool,
    /// Process current working directory.
    pub cwd: &'a Path,
}

/// Resolved target checkout directory and optional bound active run for `acdd exec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecTarget {
    /// Canonical checkout directory in which to execute the child command.
    pub worktree: std::path::PathBuf,
    /// Active `run_id` bound to the resolved target, if any.
    pub run_id: Option<acdd_core::ids::RunId>,
}

fn canonical_existing_dir(path: &Path) -> Result<std::path::PathBuf, AcddError> {
    let canon = path.canonicalize().map_err(|err| {
        AcddError::new(
            ErrorCode::ExternalGitDrift,
            format!("checkout path '{}' is unavailable: {err}", path.display()),
        )
    })?;
    let canon = acdd_core::fs::canonicalize_macos_var(&canon);
    if !canon.is_dir() {
        return Err(AcddError::new(
            ErrorCode::ExternalGitDrift,
            format!("checkout path '{}' is not a directory", canon.display()),
        ));
    }
    Ok(canon)
}

fn load_exec_adapter(root: &Path, explicit_config: Option<&Path>) -> Result<Adapter, AcddError> {
    let config_path = explicit_config.map(|cfg| {
        if cfg.is_absolute() {
            cfg.to_path_buf()
        } else {
            root.join(cfg)
        }
    });
    acdd_core::config::load_adapter(root, config_path.as_deref())
}

fn resolve_exec_state_db(
    primary: &Path,
    root: &Path,
    explicit_state_db: Option<&Path>,
    adapter: &Adapter,
) -> Result<std::path::PathBuf, AcddError> {
    if let Some(path) = explicit_state_db {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        return Ok(acdd_core::config::resolve_effective_state_db_path(
            primary,
            Some(&resolved),
            adapter,
        ));
    }
    if primary == root {
        return Ok(acdd_core::config::resolve_effective_state_db_path(
            primary, None, adapter,
        ));
    }
    common::resolve_effective_state_db_with_fallback(root, None, adapter)
}

fn resolve_worktree_active_run(
    state_db: &Path,
    worktree: &Path,
) -> std::result::Result<
    Option<(
        acdd_state::store::sqlite::SqliteStateReader,
        acdd_state::types::RunRecord,
    )>,
    AcddError,
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
    Ok(run.map(|r| (reader, r)))
}

fn resolve_exec_checkout_target(
    checkout: std::path::PathBuf,
    primary: std::path::PathBuf,
    explicit_config: Option<&Path>,
    explicit_state_db: Option<&Path>,
) -> Result<ExecTarget, AcddError> {
    let adapter_root = if explicit_config.is_some()
        || checkout
            .join(acdd_core::config::ACDD_CONFIG_FILE)
            .is_file()
    {
        &checkout
    } else {
        &primary
    };
    let adapter = load_exec_adapter(adapter_root, explicit_config)?;
    let run_id = if adapter.modules.is_subsystem_enabled(SubsystemId::State) {
        let state_db = resolve_exec_state_db(&primary, &checkout, explicit_state_db, &adapter)?;
        resolve_worktree_active_run(&state_db, &checkout)?.map(|(_, run)| run.run_id)
    } else {
        None
    };
    Ok(ExecTarget {
        worktree: checkout,
        run_id,
    })
}

fn resolve_exec_run_target(
    run: &acdd_state::types::RunRecord,
    primary: &Path,
) -> Result<ExecTarget, AcddError> {
    let raw_wt = run
        .worktree
        .as_deref()
        .map(Path::new)
        .unwrap_or(primary);
    let worktree = canonical_existing_dir(raw_wt)?;
    Ok(ExecTarget {
        worktree,
        run_id: Some(run.run_id.clone()),
    })
}

fn resolve_exec_worker_target(
    reader: &acdd_state::store::sqlite::SqliteStateReader,
    primary: &Path,
    raw_worker: &str,
) -> Result<ExecTarget, AcddError> {
    let worker_id = acdd_core::ids::WorkerId::new(raw_worker)?;
    let claims = acdd_state::store::traits::StateReader::active_worker_claims(reader, &worker_id)?;
    match claims.len() {
        0 => Err(AcddError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!("no active in_progress gate found claimed by worker '{worker_id}'"),
        )
        .with_hint("pass -C <worktree>, --run <run_id|run_name>, or claim a gate first")),
        1 => {
            let claim = claims.into_iter().next().expect("checked len == 1");
            let raw_wt = claim.worktree.as_deref().map(Path::new).unwrap_or(primary);
            Ok(ExecTarget {
                worktree: canonical_existing_dir(raw_wt)?,
                run_id: Some(claim.run_id),
            })
        }
        _ => {
            let candidates: Vec<String> = claims
                .iter()
                .map(|c| {
                    let wt = c
                        .worktree
                        .as_deref()
                        .map(|w| acdd_core::fs::canonicalize_macos_var(Path::new(w)))
                        .unwrap_or_else(|| primary.to_path_buf());
                    format!("{}:{} -> {}", c.run_id, c.node_id, wt.display())
                })
                .collect();
            Err(AcddError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "worker '{worker_id}' has {} active in_progress claims: {}",
                    claims.len(),
                    candidates.join(", ")
                ),
            )
            .with_hint("pass -C <worktree> or --run <run_id|run_name> to disambiguate"))
        }
    }
}

/// Read-only resolution of the target worktree and active `run_id` for `acdd exec`
/// without creating `.acdd/state.sqlite` or opening it read-write.
pub fn resolve_exec_target(input: &ExecResolveInput<'_>) -> Result<ExecTarget, AcddError> {
    if let Some(raw_root) = input.explicit_cli_root {
        let checkout = canonical_existing_dir(raw_root)?;
        let primary = acdd_state::usecases::lifecycle::resolve_repo_layout_optional(&checkout)?
            .map(|l| l.primary_worktree)
            .unwrap_or_else(|| checkout.clone());
        return resolve_exec_checkout_target(
            checkout,
            primary,
            input.explicit_config,
            input.explicit_state_db,
        );
    }

    let cwd_canon = canonical_existing_dir(input.cwd)?;
    if let Some(layout) = acdd_state::usecases::lifecycle::resolve_repo_layout_optional(&cwd_canon)?
    {
        if layout.target_worktree != layout.primary_worktree {
            return resolve_exec_checkout_target(
                layout.target_worktree,
                layout.primary_worktree,
                input.explicit_config,
                input.explicit_state_db,
            );
        }
    }

    if input.env_workspace_root.is_none() && input.worker.is_none() && !input.run_from_cli {
        if let Some(env_wt) = input.env_worktree {
            let wt_canon = canonical_existing_dir(env_wt)?;
            if let Some(layout) =
                acdd_state::usecases::lifecycle::resolve_repo_layout_optional(&wt_canon)?
            {
                if layout.target_worktree != layout.primary_worktree {
                    return resolve_exec_checkout_target(
                        layout.target_worktree,
                        layout.primary_worktree,
                        input.explicit_config,
                        input.explicit_state_db,
                    );
                }
            }
        }
    }

    let anchor_raw = input
        .env_workspace_root
        .or(input.env_worktree)
        .unwrap_or(cwd_canon.as_path());
    let anchor = canonical_existing_dir(anchor_raw)?;
    let primary = acdd_state::usecases::lifecycle::resolve_repo_layout_optional(&anchor)?
        .map(|l| l.primary_worktree)
        .unwrap_or_else(|| anchor.clone());
    let adapter = load_exec_adapter(&primary, input.explicit_config)?;
    if !adapter.modules.is_subsystem_enabled(SubsystemId::State) {
        return Err(AcddError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    let state_db = resolve_exec_state_db(&primary, &anchor, input.explicit_state_db, &adapter)?;
    let engine_cfg = build_state_engine_config(primary.clone(), &primary, &state_db, None, &adapter);
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db);

    if let Some(raw_worker) = input.worker {
        return resolve_exec_worker_target(&reader, &primary, raw_worker);
    }

    if let Some(run_sel) = input.run_selector {
        let run = acdd_state::usecases::lifecycle::resolve_active_run(
            &engine_cfg,
            &reader,
            Some(run_sel),
            None,
        )?;
        return resolve_exec_run_target(&run, &primary);
    }

    let active_runs = acdd_state::usecases::lifecycle::list_all_runs(
        &reader,
        Some(acdd_state::types::RunStatus::Active),
    )?;
    match active_runs.len() {
        0 => Err(AcddError::new(
            ErrorCode::RunSelectorAmbiguous,
            "no active run found for this workspace",
        )
        .with_hint(
            "pass -C <worktree>, --run <run_id|run_name>, --worker <id>, or start a run with 'acdd start'",
        )),
        1 => {
            let run = active_runs.into_iter().next().expect("checked len == 1");
            resolve_exec_run_target(&run, &primary)
        }
        _ => {
            let candidates: Vec<String> = active_runs
                .iter()
                .map(|r| {
                    format!(
                        "{} ({}) -> {}",
                        r.run_id,
                        r.run_name,
                        r.worktree.as_deref().unwrap_or("<in_place>")
                    )
                })
                .collect();
            Err(AcddError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "{} active runs exist; target is ambiguous: {}",
                    active_runs.len(),
                    candidates.join(", ")
                ),
            )
            .with_hint("pass -C <worktree>, --run <run_id|run_name>, or --worker <id>"))
        }
    }
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
    AcddError,
> {
    let Some((reader, run)) = resolve_worktree_active_run(state_db, worktree)? else {
        return Ok(None);
    };
    let wt_canon = acdd_core::fs::canonicalize_macos_var(worktree);
    let wt_canon_str = wt_canon.to_string_lossy();
    let wt_raw_str = worktree.to_string_lossy();
    let Some(graph) = acdd_state::store::traits::StateReader::load_run_graph(&reader, &run.run_id)?
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
    profile: &'a acdd_state::workflow::ResolvedWorkflow,
    cursor: &acdd_core::ids::NodeCursor,
) -> Option<&'a acdd_state::workflow::GateDef> {
    match cursor {
        acdd_core::ids::NodeCursor::Root(gates) => {
            acdd_state::usecases::queries::find_root_gate_def_ref(profile, gates, false)
        }
        acdd_core::ids::NodeCursor::Task { gates, .. } => {
            acdd_state::usecases::queries::find_task_block_gate_def_ref(
                profile,
                gates.first()?.as_str(),
            )
        }
    }
}

fn build_state_engine_config(
    primary: std::path::PathBuf,
    root: &Path,
    state_db: &Path,
    code_db: Option<&Path>,
    adapter: &Adapter,
) -> StateEngineConfig {
    let default_state_db = primary.join(".acdd/state.sqlite");
    let state_db_override = (state_db != default_state_db).then(|| state_db.to_path_buf());
    let code_db_path = (adapter.modules.is_enabled("code") || adapter.modules.is_enabled("docs"))
        .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    StateEngineConfig {
        workspace_root: primary,
        worktree: root.to_path_buf(),
        state_db_override,
        code_db_path,
        spec_collections: adapter.specs.clone(),
        workflows_dirs: adapter.workflows_dirs.clone(),
        linked_workspaces: adapter
            .linked_workspaces
            .iter()
            .map(|lw| (lw.name.clone(), lw.path.clone()))
            .collect(),
        modules: adapter.modules.clone(),
        response_policy: adapter.response.clone(),
        retain_traces: adapter.retain_traces,
        max_snapshot_file_bytes: adapter.limits.max_snapshot_file_bytes,
        allowed_binary_extensions: adapter
            .limits
            .normalized_allowed_binary_extensions()
            .ok()
            .flatten(),
    }
}

/// Resolve the canonical [`acdd_core::path_policy::WorkspacePathPolicy`] for `root`
/// in read-only mode (never creating `state.sqlite` when absent; failing closed on unreadable
/// or corrupt existing state databases).
fn resolve_workspace_path_policy(
    root: &Path,
    adapter: &Adapter,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
) -> anyhow::Result<acdd_core::path_policy::WorkspacePathPolicy> {
    let primary = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let resolved_state_db =
        acdd_core::config::resolve_effective_state_db_path(&primary, state_db, adapter);
    let engine_cfg = build_state_engine_config(primary, root, &resolved_state_db, code_db, adapter);
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(resolved_state_db);
    acdd_state::usecases::lifecycle::build_path_policy(
        &engine_cfg,
        &reader,
        &[],
        None,
        Some(&engine_cfg.worktree),
    )
    .map_err(anyhow::Error::from)
}

/// Shared MCP/CLI input for canonical `code_map_analyze` / `acdd query analyze` dispatch.
pub(crate) struct AnalyzeDispatchInput<'a> {
    /// Relative file/directory target or read-only SQL statement (empty for workspace scope).
    pub(crate) target: &'a str,
    /// Optional analysis mode (`summary`, `hotspots`, `boundary`, `path`, `entrypoints`, `diff_impact`, `cochange`).
    pub(crate) mode: Option<&'a str>,
    /// Optional destination selector for `mode="path"`.
    pub(crate) to: Option<&'a str>,
    /// Optional Git base ref for `mode="diff_impact"` or `mode="cochange"`.
    pub(crate) base: Option<&'a str>,
    /// Optional cycle detection flag (`mode="summary"`).
    pub(crate) include_cycles: Option<bool>,
    /// Stored syntax diagnostics only (`mode="summary"`).
    pub(crate) lint: bool,
    /// Resolved pagination and detail policy.
    pub(crate) page: &'a acdd_core::response::QueryOptions,
}

/// Canonical shared owner dispatching `code_map_analyze` and CLI `acdd query analyze`.
pub(crate) fn dispatch_analyze(
    conn: &rusqlite::Connection,
    root: &Path,
    adapter: &Adapter,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
    input: &AnalyzeDispatchInput<'_>,
) -> anyhow::Result<serde_json::Value> {
    use acdd_index::db::analyze::{analyze_with_mode, AnalyzeMode, AnalyzeRequest};
    let mode = input
        .mode
        .map(str::parse::<AnalyzeMode>)
        .transpose()?
        .unwrap_or_default();
    if input.lint {
        anyhow::ensure!(
            mode == AnalyzeMode::Summary,
            "lint=true cannot be combined with mode='{}'",
            mode.as_str()
        );
        anyhow::ensure!(
            input.include_cycles != Some(true),
            "lint=true cannot be combined with include_cycles=true"
        );
        anyhow::ensure!(
            input.to.is_none() && input.base.is_none(),
            "lint=true cannot be combined with 'to' or 'base'"
        );
        return acdd_index::db::lint::syntax_paged(conn, input.target, input.page);
    }
    anyhow::ensure!(
        input.include_cycles != Some(true) || mode == AnalyzeMode::Summary,
        "include_cycles=true is only supported for mode='summary'"
    );
    anyhow::ensure!(
        input.to.is_none() || mode == AnalyzeMode::Path,
        "'to' is only supported for mode='path'"
    );
    anyhow::ensure!(
        input.base.is_none() || matches!(mode, AnalyzeMode::DiffImpact | AnalyzeMode::Cochange),
        "'base' is only supported for mode='diff_impact' or 'cochange'"
    );
    let policy = if matches!(mode, AnalyzeMode::DiffImpact | AnalyzeMode::Cochange) {
        Some(resolve_workspace_path_policy(
            root, adapter, state_db, code_db,
        )?)
    } else {
        None
    };
    analyze_with_mode(
        conn,
        &AnalyzeRequest {
            mode,
            target: input.target,
            to: input.to,
            include_cycles: input.include_cycles,
            page: input.page,
            root: Some(root),
            base: input.base,
            policy: policy.as_ref(),
        },
    )
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
        serde_json::from_str::<acdd_state::workflow::ResolvedWorkflow>(&run.profile_json).ok();
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

    let engine_cfg = build_state_engine_config(primary, root, &state_db, code_db, adapter);
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db);
    let path_policy = acdd_state::usecases::lifecycle::build_path_policy(
        &engine_cfg,
        &reader,
        &[],
        Some(&run),
        Some(&engine_cfg.worktree),
    )?;

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
    config: &Adapter,
) -> Result<Option<WorktreeSpanContext>, AcddError> {
    if !config.modules.is_subsystem_enabled(SubsystemId::State) {
        return Err(AcddError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    let resolved_state_db =
        common::resolve_effective_state_db_with_fallback(root, state_db, config)?;
    let resolved = resolve_worktree_active_run_node(&resolved_state_db, worktree, &|n| {
        n.status == NodeStatus::InProgress && n.active_span_id.is_some()
    })?;
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
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".acdd/workflows")).unwrap();
        fs::write(
            root.join(".acdd/workflows/default.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  builder:\n    mode: subagent\ncommands:\n  build_check: \"cargo check --lib\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          command: build_check\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
        fs::write(root.join("src/other.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);
        let base_commit = run_git(&["rev-parse", "HEAD"]);
        // Pre-claim in-place edit and pre-claim untracked file captured in `claim_tree`.
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 10 }\n").unwrap();
        fs::write(
            root.join("src/pre_claim_untracked.rs"),
            "pub fn pre_claim() {}\n",
        )
        .unwrap();

        let adapter = Adapter::default();
        let engine = StateEngine::open(
            StateEngineConfig {
                workspace_root: root.clone(),
                worktree: root.clone(),
                state_db_override: None,
                code_db_path: None,
                spec_collections: adapter.specs.clone(),
                workflows_dirs: adapter.workflows_dirs.clone(),
                linked_workspaces: std::collections::BTreeMap::new(),
                modules: adapter.modules.clone(),
                response_policy: adapter.response.clone(),
                retain_traces: adapter.retain_traces,
                max_snapshot_file_bytes: None,
                allowed_binary_extensions: None,
            },
            Arc::new(
                CompositeDomainProvider::with_module_flags(
                    vec![],
                    std::collections::BTreeMap::new(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let started = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                spec_paths: Vec::new(),
                goal: Some("edit scoped".into()),
                run_name: None,
                workflow: None,
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
                detail: acdd_core::response::Detail::Compact,
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
            let node_ctx = active_node_context(&root, &adapter, None, None)
                .ok()
                .flatten();
            acdd_read_tool(
                &root,
                &adapter,
                req,
                &page,
                &source,
                None,
                node_ctx.as_ref(),
            )
            .unwrap()
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
        assert!(
            delta_text.contains("-pub fn a() -> i32 { 10 }"),
            "{delta_text}"
        );
        assert!(
            delta_text.contains("+pub fn a() -> i32 { 42 }"),
            "{delta_text}"
        );
        let hints = allowed["Hints"].as_str().unwrap_or_default();
        assert!(hints.contains("acdd_blackboard"), "{hints}");

        let untracked_delta = read(&ReadRequest {
            path: Some("src/untracked.rs".into()),
            mode: ReadMode::Delta,
            ..Default::default()
        });
        let untracked_diff = untracked_delta["content"].as_str().unwrap_or_default();
        assert!(untracked_diff.contains("--- /dev/null"), "{untracked_diff}");
        assert!(
            untracked_diff.contains("+pub fn newly_added() {}"),
            "{untracked_diff}"
        );
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
            assert_eq!(
                mode_diff.matches("old mode 100644").count(),
                1,
                "{mode_diff}"
            );
            assert_eq!(
                mode_diff.matches("new mode 100755").count(),
                1,
                "{mode_diff}"
            );

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
            assert!(
                mode_content_diff.contains("+pub fn pre_claim_untracked_v2()"),
                "{mode_content_diff}"
            );
        }

        let protected_read = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(
            protected_read["in_write_scope"], "OUT_OF_SCOPE",
            "{protected_read}"
        );

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
                purge: false,
            }))
            .unwrap();
        fs::write(
            root.join(".acdd/workflows/default.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  architect:\n    mode: subagent\n  builder:\n    mode: subagent\ngates:\n  - id: verify\n    role: architect\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n",
        )
        .unwrap();
        let root_start = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                spec_paths: Vec::new(),
                goal: Some("unscoped root check".into()),
                run_name: None,
                workflow: None,
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
                detail: acdd_core::response::Detail::Compact,
            }))
            .unwrap();
        let root_protected = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(
            root_protected["in_write_scope"], "OUT_OF_SCOPE",
            "{root_protected}"
        );
        let root_allowed = read(&ReadRequest {
            path: Some("src/other.rs".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(root_allowed["in_write_scope"], "ALLOWED", "{root_allowed}");
    }
}
