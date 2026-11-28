//! Workspace root resolution, repo locking, module gating, path policies, and checkout config.

use super::specs::compute_archive_relpath;
use crate::git_ops::worktree::{canonicalize_macos_var, resolve_repo_layout, RepoWorktreeLayout};
use crate::workflow::{validate_workflow, GateDef, ResolvedWorkflow, WorkflowValidationOptions};
use crate::store::traits::{StateBackend, StateReader};
use crate::types::{
    GateNodeRecord, IsolationMode, ListRunsReq, NodeKind, RunRecord, RunStatus, SaveAction,
    WorktreeLifecycle,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::config::{
    load_adapter, Adapter, ACDD_CONFIG_FILE, DEFAULT_CODE_DB, DEFAULT_STATE_DB,
};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::git::git_common_dir;
use acdd_core::lock::{acquire_exclusive, FileLockGuard};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::{SubsystemId, SubsystemRegistry};
use std::path::{Path, PathBuf};

/// Execute `f` while holding `repo.lock` (if inside a Git repository) after verifying
/// `modules.state` is enabled, then drop `repo.lock` BEFORE invoking
/// `StateBackend::after_commit_maintenance()`.
pub(crate) fn with_repo_lock_and_maintenance<T>(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    f: impl FnOnce(&Path, Option<&RepoWorktreeLayout>) -> Result<T, AcddError>,
) -> Result<T, AcddError> {
    ensure_state_enabled(config)?;
    with_repo_lock_and_maintenance_unchecked(config, backend, f)
}

/// Variant of [`with_repo_lock_and_maintenance`] for callers that already ran feature/input checks.
pub(crate) fn with_repo_lock_and_maintenance_unchecked<T>(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    f: impl FnOnce(&Path, Option<&RepoWorktreeLayout>) -> Result<T, AcddError>,
) -> Result<T, AcddError> {
    let primary = primary_workspace_root(config);
    let layout = resolve_repo_layout_optional(&primary)?;
    let lock_guard = acquire_repo_lock(layout.as_ref())?;

    let result = f(&primary, layout.as_ref());
    // Release repo.lock BEFORE after_commit_maintenance acquires backup.lock!
    drop(lock_guard);

    backend.after_commit_maintenance()?;
    result
}

/// Fail closed with `FEATURE_DISABLED` when `modules.state` is disabled.
pub fn ensure_state_enabled(config: &StateEngineConfig) -> Result<(), AcddError> {
    if !config.modules.is_subsystem_enabled(SubsystemId::State) {
        return Err(AcddError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    Ok(())
}

/// Fail closed with `FEATURE_DISABLED` when `modules.blackboard` (or `modules.state`) is disabled.
pub fn ensure_blackboard_enabled(config: &StateEngineConfig) -> Result<(), AcddError> {
    ensure_state_enabled(config)?;
    if !config.modules.is_subsystem_enabled(SubsystemId::Blackboard) {
        return Err(AcddError::new(
            ErrorCode::FeatureDisabled,
            "blackboard subsystem is disabled in modules:",
        ));
    }
    Ok(())
}

/// Compute the effective intersection of workspace `modules:` and profile `modules:`.
pub fn effective_modules_for_profile(
    workspace_modules: &SubsystemRegistry,
    profile_modules: &SubsystemRegistry,
) -> SubsystemRegistry {
    let mut merged = profile_modules.clone();
    for item in &workspace_modules.disabled {
        merged.disabled.insert(item.clone());
    }
    for sub in SubsystemId::ALL {
        if !workspace_modules.is_subsystem_enabled(sub) {
            merged.disabled.insert(sub.as_str().to_owned());
        }
    }
    for (domain, &enabled) in &workspace_modules.domains {
        merged
            .domains
            .entry(domain.clone())
            .and_modify(|cur| *cur = *cur && enabled)
            .or_insert(enabled);
    }
    for req in &workspace_modules.required {
        merged.required.insert(req.clone());
    }
    merged
}

/// Validate a resolved workflow fail-closed against `providers`, `modules:`, and command bindings.
pub fn validate_profile_fail_closed(
    config: &StateEngineConfig,
    workflow: &ResolvedWorkflow,
    providers: &CompositeDomainProvider,
    require_bound_commands: bool,
) -> Result<(), AcddError> {
    let spec_cols = super::specs::effective_spec_collections(config);
    let opts = WorkflowValidationOptions {
        domain_providers: Some(providers),
        acdd_modules: Some(&config.modules),
        spec_collections: Some(&spec_cols),
        require_bound_commands,
    };
    validate_workflow(workflow, &opts)
}

/// Resolve Git common-dir/worktree layout when `path` resides in a Git repository.
pub fn resolve_repo_layout_optional(path: &Path) -> Result<Option<RepoWorktreeLayout>, AcddError> {
    if !path.exists() {
        return Ok(None);
    }
    if path.join(".git").exists() || git_common_dir(path).is_ok() {
        resolve_repo_layout(path).map(Some)
    } else {
        Ok(None)
    }
}

/// Acquire `<git_common_dir>/acdd/repo.lock` before any mutating Git or SQLite operation.
///
/// Callers MUST drop the returned guard BEFORE invoking `StateBackend::after_commit_maintenance()`
/// so `repo.lock` -> `backup.lock` lock nesting never occurs.
pub fn acquire_repo_lock(
    layout: Option<&RepoWorktreeLayout>,
) -> Result<Option<FileLockGuard>, AcddError> {
    let Some(l) = layout else {
        return Ok(None);
    };
    acquire_exclusive(&l.repo_lock_path)
        .map(Some)
        .map_err(|err| {
            AcddError::new(
                ErrorCode::BackendConflict,
                format!(
                    "failed to acquire repo.lock '{}': {err}",
                    l.repo_lock_path.display()
                ),
            )
        })
}

pub(in crate::usecases) fn snapshot_worktree_layout(
    config: &StateEngineConfig,
    locked: Option<&RepoWorktreeLayout>,
    worktree: &Path,
) -> Result<RepoWorktreeLayout, AcddError> {
    let layout = resolve_repo_layout(worktree)?;
    if layout.target_worktree == worktree
        && (locked.is_some_and(|primary| primary.git_common_dir == layout.git_common_dir)
            || config
                .linked_workspaces
                .values()
                .any(|root| git_common_dir(root).is_ok_and(|dir| dir == layout.git_common_dir)))
    {
        return Ok(layout);
    }
    Err(AcddError::new(
        ErrorCode::ExternalGitDrift,
        format!(
            "snapshot worktree '{}' is outside the owning repositories",
            worktree.display()
        ),
    ))
}

/// Determine the canonical primary workspace root for `config`.
pub fn primary_workspace_root(config: &StateEngineConfig) -> PathBuf {
    if let Some(primary) = acdd_core::git::primary_worktree_root(&config.workspace_root) {
        return canonicalize_macos_var(&primary);
    }
    canonicalize_macos_var(&config.workspace_root)
}

fn relativize_db_path(
    raw: &Path,
    primary: &Path,
    workspace_root: &Path,
    default_rel: &str,
) -> String {
    if raw.is_relative() {
        return acdd_core::path_policy::normalize_rel_path(&raw.to_string_lossy())
            .unwrap_or_else(|| default_rel.to_owned());
    }
    let canon = canonicalize_macos_var(raw);
    let canon_ws = canonicalize_macos_var(workspace_root);
    canon
        .strip_prefix(primary)
        .or_else(|_| canon.strip_prefix(&canon_ws))
        .or_else(|_| raw.strip_prefix(primary))
        .or_else(|_| raw.strip_prefix(workspace_root))
        .ok()
        .and_then(|rel| acdd_core::path_policy::normalize_rel_path(&rel.to_string_lossy()))
        .unwrap_or_else(|| default_rel.to_owned())
}

fn push_run_managed_paths(config: &StateEngineConfig, run: &RunRecord, managed: &mut Vec<String>) {
    if let Some(ref sp) = run.spec_path {
        managed.push(sp.clone());
        if let Ok(archive_rel) = compute_archive_relpath(config, run.collection.as_deref(), sp) {
            managed.push(archive_rel);
        }
    }
}

/// Build [`WorkspacePathPolicy`] scoped to `current_run` and `target_worktree`.
///
/// When `current_run` has [`IsolationMode::Worktree`] and `target_worktree` is not the primary
/// checkout, `managed` is computed directly from `current_run` with zero SQLite `list_runs`
/// queries. Shared checkouts (`in_place`, `target_worktree == primary`, or `current_run == None`)
/// scan active runs so sibling specs on the same checkout stay excluded from `candidate_tree`.
pub fn build_path_policy(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    extra_managed: &[String],
    current_run: Option<&RunRecord>,
    target_worktree: Option<&Path>,
) -> Result<WorkspacePathPolicy, AcddError> {
    let primary = primary_workspace_root(config);
    let target_canon = target_worktree.map(canonicalize_macos_var);
    let mut managed: Vec<String> = extra_managed.to_vec();
    let fast_isolated = current_run.filter(|run| {
        run.isolation == IsolationMode::Worktree
            && target_canon
                .as_ref()
                .is_some_and(|target| *target != primary)
    });
    if let Some(run) = fast_isolated {
        push_run_managed_paths(config, run, &mut managed);
    } else {
        let mut offset = 0usize;
        loop {
            let page = reader.list_runs(&ListRunsReq {
                workspace: None,
                status: Some(RunStatus::Active),
                include_tombstones: false,
                limit: 100,
                offset,
                generation: None,
            })?;
            for run in &page.runs {
                let same_run = current_run.is_some_and(|cur| cur.run_id == run.run_id);
                let same_checkout = match &target_canon {
                    None => true,
                    Some(target) => {
                        let run_checkout = run.worktree.as_deref().unwrap_or(&run.workspace);
                        canonicalize_macos_var(Path::new(run_checkout)) == *target
                    }
                };
                if !(same_run || same_checkout) {
                    continue;
                }
                push_run_managed_paths(config, run, &mut managed);
            }
            offset += page.runs.len();
            if offset >= page.total_runs || page.runs.is_empty() {
                break;
            }
        }
    }

    let (state_db_abs, _) = config.resolved_state_db_path();
    let state_db_rel = relativize_db_path(
        &state_db_abs,
        &primary,
        &config.workspace_root,
        DEFAULT_STATE_DB,
    );

    let code_db_rel = config
        .code_db_path
        .as_ref()
        .map(|p| relativize_db_path(p, &primary, &config.workspace_root, DEFAULT_CODE_DB))
        .unwrap_or_else(|| DEFAULT_CODE_DB.to_owned());

    Ok(
        WorkspacePathPolicy::new(managed, state_db_rel, code_db_rel).with_snapshot_limits(
            config.max_snapshot_file_bytes,
            config.allowed_binary_extensions.clone(),
        ),
    )
}

/// Resolve an owned source checkout root for spec/profile IO while keeping state DB anchored on `primary`.
/// When `explicit` is true, non-existent destinations are returned normalized for managed worktree creation
/// once symlink/confinement checks succeed; existing paths must be owned Git worktree roots.
pub fn resolve_source_checkout(
    primary: &Path,
    layout: Option<&RepoWorktreeLayout>,
    candidate: &Path,
    explicit: bool,
) -> Result<PathBuf, AcddError> {
    let abs_raw = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        primary.join(candidate)
    };
    let drift = |msg: String| AcddError::new(ErrorCode::ExternalGitDrift, msg);
    acdd_core::fs::check_git_entry_no_symlinks(&abs_raw).map_err(|err| drift(err.to_string()))?;
    let canon = canonicalize_macos_var(&abs_raw);
    if !canon.exists() && explicit && layout.is_some() {
        return Ok(canon);
    }
    if !canon.is_dir() {
        return Err(drift(format!(
            "checkout path '{}' is not a directory",
            canon.display()
        )));
    }
    let Some(layout) = layout else {
        return Ok(canon);
    };
    let resolved = resolve_repo_layout(&canon)?;
    if resolved.primary_worktree != layout.primary_worktree {
        return Err(drift(format!(
            "worktree path '{}' is not owned by this repository",
            canon.display()
        )));
    }
    if resolved.target_worktree != canon {
        return Err(drift(format!(
            "checkout path '{}' is not a git worktree root",
            canon.display()
        )));
    }
    Ok(canon)
}

/// Overlay checkout-local `acdd.yaml` bindings onto `config` while preserving primary DB routing.
pub fn checkout_config(
    config: &StateEngineConfig,
    primary: &Path,
    source: &Path,
    source_override: Option<&Adapter>,
) -> Result<StateEngineConfig, AcddError> {
    let source = canonicalize_macos_var(source);
    let primary_canon = canonicalize_macos_var(primary);
    let mut checkout = config.clone();
    checkout.worktree = source.clone();
    let apply_adapter = |target: &mut StateEngineConfig,
                         adapter: &Adapter|
     -> Result<(), AcddError> {
        target.spec_collections = adapter.specs.clone();
        target.workflows_dirs = adapter.workflows_dirs.clone();
        for lw in &adapter.linked_workspaces {
            target
                .linked_workspaces
                .insert(lw.name.clone(), lw.path.clone());
        }
        target.modules = adapter.modules.clone();
        target.response_policy = adapter.response.clone();
        target.retain_traces = adapter.retain_traces;
        target.max_snapshot_file_bytes = adapter.limits.max_snapshot_file_bytes;
        target.allowed_binary_extensions = adapter.limits.normalized_allowed_binary_extensions()?;
        Ok(())
    };
    if let Some(ad) = source_override {
        apply_adapter(&mut checkout, ad)?;
        return Ok(checkout);
    }
    if source == primary_canon {
        return Ok(checkout);
    }
    if source.join(ACDD_CONFIG_FILE).is_file() || primary_canon.join(ACDD_CONFIG_FILE).is_file() {
        let adapter = load_adapter(&source, None).map_err(|err| {
            AcddError::new(
                ErrorCode::ConfigMigrationRequired,
                format!(
                    "failed to load {ACDD_CONFIG_FILE} from '{}': {err}",
                    source.display()
                ),
            )
        })?;
        apply_adapter(&mut checkout, &adapter)?;
    }
    Ok(checkout)
}

/// Resolve the effective worktree directory for a node, canonicalizing macOS `/var` aliases.
pub fn resolve_node_worktree(
    config: &StateEngineConfig,
    run: &RunRecord,
    node: &GateNodeRecord,
    override_wt: Option<&Path>,
) -> PathBuf {
    if let Some(wt) = override_wt {
        return canonicalize_macos_var(wt);
    }
    if let Some(ref wt) = node.worktree {
        return canonicalize_macos_var(Path::new(wt));
    }
    if node.kind == NodeKind::TaskGate {
        if let Some(ref wt) = run.worktree {
            return canonicalize_macos_var(Path::new(wt));
        }
    } else if let Some(ref wt) = run.worktree {
        return canonicalize_macos_var(Path::new(wt));
    }
    canonicalize_macos_var(&config.worktree)
}

/// Canonical helper applying effective `IsolationMode` and `WorktreeLifecycle` to a resolved
/// profile, neutralizing `save: commit` -> `none` whenever `isolation == IsolationMode::InPlace`.
pub(crate) fn apply_effective_isolation(
    profile: &mut ResolvedWorkflow,
    isolation: IsolationMode,
    worktrees: WorktreeLifecycle,
) {
    profile.git.isolation = isolation;
    profile.git.worktrees = if isolation == IsolationMode::InPlace {
        WorktreeLifecycle::Manual
    } else {
        worktrees
    };
    if isolation == IsolationMode::InPlace {
        neutralize_in_place_save(profile);
    }
}

fn neutralize_in_place_save(profile: &mut ResolvedWorkflow) {
    profile.save = SaveAction::None;
    for gate in &mut profile.gates {
        neutralize_gate_commit(gate);
    }
}

fn neutralize_gate_commit(gate: &mut GateDef) {
    if gate.save.post == Some(SaveAction::Commit) {
        gate.save.post = Some(SaveAction::None);
    }
    for child in &mut gate.gates {
        neutralize_gate_commit(child);
    }
    if let Some(ref mut tb) = gate.tasks {
        for task_gate in &mut tb.gates {
            neutralize_gate_commit(task_gate);
        }
    }
}
