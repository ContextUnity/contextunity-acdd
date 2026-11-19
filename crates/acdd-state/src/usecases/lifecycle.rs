//! Shared lifecycle infrastructure: lock ordering, path policies, profile/spec resolution,
//! cross-workspace dependency queries, PID liveness checks, and tombstone cleanup recovery.

use acdd_core::git::read_git_blob_utf8;
use crate::git_ops::worktree::{
    canonicalize_macos_var, cleanup_run_refs, is_branch_merged_into_ref,
    remove_worktree_and_branch, resolve_repo_layout, RepoWorktreeLayout,
};
use crate::machine::{
    compute_schedule, container_scope, MachineCheckDef, MachineGateDef, MachineGitPolicy,
    MachineGraphItem, MachineProfileInput, MachineProofKind, MachineSavePolicy,
    MachineTasksBlockDef,
};
use crate::profile::{
    validate_profile, GateDef, ProfileValidationOptions, ProofDef, ProofKind, ResolvedGateProfile,
};
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::backup::RestoredActiveRun;
use crate::store::traits::{exec_tx_with, StateBackend, StateReader};
use crate::types::{
    ArchiveCleanupEntry, GateNodeRecord, ListRunsReq, NodeKind, NodeStatus,
    RunGraphSnapshot, RunRecord, RunStatus, RunTombstoneRecord, SaveAction, TerminalStatus,
    ValidatedObservations, WorktreeLifecycle,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::{load_adapter, Adapter, SpecCollectionConfig};
use crate::usecases::deps::load_ws_adapter;
use acdd_core::fs::{checked_child, open_confined};
use acdd_core::git::{git_common_dir, git_stdout};
use acdd_core::lock::{acquire_exclusive, FileLockGuard};
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, RunName, TaskRef, WorkerId};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::schema::SchemaDef;
use chrono::Utc;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TRACE_EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Read and lexicographically sort directory entries (returns empty `Vec` on unreadable/missing dir).
pub(crate) fn sorted_dir_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

/// Execute `f` while holding `repo.lock` (if inside a Git repository) after verifying
/// `modules.state` is enabled, then drop `repo.lock` BEFORE invoking
/// `StateBackend::after_commit_maintenance()`.
pub(crate) fn with_repo_lock_and_maintenance<T>(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    f: impl FnOnce(&Path, Option<&RepoWorktreeLayout>) -> Result<T, StateError>,
) -> Result<T, StateError> {
    ensure_state_enabled(config)?;
    with_repo_lock_and_maintenance_unchecked(config, backend, f)
}

/// Variant of [`with_repo_lock_and_maintenance`] for callers that already ran feature/input checks.
pub(crate) fn with_repo_lock_and_maintenance_unchecked<T>(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    f: impl FnOnce(&Path, Option<&RepoWorktreeLayout>) -> Result<T, StateError>,
) -> Result<T, StateError> {
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
pub fn ensure_state_enabled(config: &StateEngineConfig) -> Result<(), StateError> {
    if !config.modules.is_enabled("state") {
        return Err(StateError::new(
            ErrorCode::FeatureDisabled,
            "state subsystem is disabled in modules:",
        ));
    }
    Ok(())
}

/// Fail closed with `FEATURE_DISABLED` when `modules.blackboard` (or `modules.state`) is disabled.
pub fn ensure_blackboard_enabled(config: &StateEngineConfig) -> Result<(), StateError> {
    ensure_state_enabled(config)?;
    if !config.modules.is_enabled("blackboard") {
        return Err(StateError::new(
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
    for sub in ["code", "docs", "state", "blackboard", "memory"] {
        if !workspace_modules.is_enabled(sub) {
            merged.disabled.insert(sub.to_owned());
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

/// Validate a resolved profile fail-closed against `providers`, `modules:`, and command bindings.
pub fn validate_profile_fail_closed(
    config: &StateEngineConfig,
    profile: &ResolvedGateProfile,
    providers: &CompositeDomainProvider,
    require_bound_commands: bool,
) -> Result<(), StateError> {
    let opts = ProfileValidationOptions {
        domain_providers: Some(providers),
        acdd_modules: Some(&config.modules),
        require_bound_commands,
    };
    validate_profile(profile, &opts)
}

/// Resolve Git common-dir/worktree layout when `path` resides in a Git repository.
pub fn resolve_repo_layout_optional(path: &Path) -> Result<Option<RepoWorktreeLayout>, StateError> {
    if !path.exists() {
        return Ok(None);
    }
    if path.join(".git").exists() || acdd_core::git::git_common_dir(path).is_ok() {
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
) -> Result<Option<FileLockGuard>, StateError> {
    let Some(l) = layout else {
        return Ok(None);
    };
    acquire_exclusive(&l.repo_lock_path).map(Some).map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!(
                "failed to acquire repo.lock '{}': {err}",
                l.repo_lock_path.display()
            ),
        )
    })
}

pub(super) fn snapshot_worktree_layout(
    config: &StateEngineConfig, locked: Option<&RepoWorktreeLayout>, worktree: &Path,
) -> Result<RepoWorktreeLayout, StateError> {
    let layout = resolve_repo_layout(worktree)?;
    if layout.target_worktree == worktree
        && (locked.is_some_and(|primary| primary.git_common_dir == layout.git_common_dir)
            || config.linked_workspaces.values().any(|root| git_common_dir(root).is_ok_and(|dir| dir == layout.git_common_dir)))
    {
        return Ok(layout);
    }
    Err(StateError::new(ErrorCode::ExternalGitDrift, format!("snapshot worktree '{}' is outside the owning repositories", worktree.display())))
}

/// Determine the canonical primary workspace root for `config`.
pub fn primary_workspace_root(config: &StateEngineConfig) -> PathBuf {
    if let Some(primary) = acdd_core::git::primary_worktree_root(&config.workspace_root) {
        return canonicalize_macos_var(&primary);
    }
    canonicalize_macos_var(&config.workspace_root)
}

/// Return configured spec collections or the default `milestones` + `plans` map.
pub fn effective_spec_collections(
    config: &StateEngineConfig,
) -> BTreeMap<String, SpecCollectionConfig> {
    if config.spec_collections.is_empty() {
        acdd_core::config::default_specs()
    } else {
        config.spec_collections.clone()
    }
}

/// Match a workspace-relative spec path to its owning collection key by longest matching pattern.
pub fn match_spec_collection(
    collections: &BTreeMap<String, SpecCollectionConfig>,
    spec_relpath: &str,
) -> Option<String> {
    let norm = spec_relpath.trim().replace('\\', "/");
    let norm = norm.strip_prefix("./").unwrap_or(&norm);
    collections
        .iter()
        .filter(|(_, cfg)| cfg.matches_rel_path(norm))
        .max_by_key(|(_, cfg)| {
            cfg.patterns()
                .into_iter()
                .map(str::len)
                .max()
                .unwrap_or(0)
        })
        .map(|(name, _)| name.clone())
}

/// Derive the archive relative path for an active run's `spec_relpath`.
pub(crate) fn compute_archive_relpath(
    config: &StateEngineConfig,
    run_collection: Option<&str>,
    spec_relpath: &str,
) -> Result<String, StateError> {
    let collections = effective_spec_collections(config);
    let col_key = run_collection
        .map(str::to_owned)
        .or_else(|| match_spec_collection(&collections, spec_relpath));

    let file_name = Path::new(spec_relpath)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("spec_path '{spec_relpath}' has no file name"),
            )
        })?;

    if let Some(key) = col_key {
        if let Some(cfg) = collections.get(&key) {
            if let Some(root_dir) = cfg.matched_concrete_root(spec_relpath) {
                return Ok(format!("{root_dir}/archive/{file_name}"));
            }
            if let Some(dir) = cfg.default_draft_dir() {
                return Ok(format!("{dir}/archive/{file_name}"));
            }
        }
    }
    let parent = Path::new(spec_relpath)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');
    if parent.is_empty() {
        Ok(format!("archive/{file_name}"))
    } else {
        Ok(format!("{parent}/archive/{file_name}"))
    }
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

/// Build [`WorkspacePathPolicy`] incorporating `spec_path` and derived `archive_path` of all
/// active runs plus `extra_managed`.
pub fn build_path_policy(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    extra_managed: &[String],
) -> Result<WorkspacePathPolicy, StateError> {
    let mut managed: Vec<String> = extra_managed.to_vec();
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
            if let Some(ref sp) = run.spec_path {
                managed.push(sp.clone());
                if let Ok(archive_rel) =
                    compute_archive_relpath(config, run.collection.as_deref(), sp)
                {
                    managed.push(archive_rel);
                }
            }
        }
        offset += page.runs.len();
        if offset >= page.total_runs || page.runs.is_empty() {
            break;
        }
    }

    let primary = primary_workspace_root(config);
    let (state_db_abs, _) = config.resolved_state_db_path();
    let state_db_rel =
        relativize_db_path(&state_db_abs, &primary, &config.workspace_root, ".acdd/state.sqlite");

    let code_db_rel = config
        .code_db_path
        .as_ref()
        .map(|p| relativize_db_path(p, &primary, &config.workspace_root, ".acdd/code-map.sqlite"))
        .unwrap_or_else(|| ".acdd/code-map.sqlite".to_owned());

    Ok(WorkspacePathPolicy::new(managed, state_db_rel, code_db_rel))
}

/// Resolve an active `RunRecord` using `--run <id|name>` -> worktree -> single active run in primary worktree.
pub fn resolve_active_run(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    run_selector: Option<&str>,
    worktree_override: Option<&Path>,
) -> Result<RunRecord, StateError> {
    resolve_run_optional(config, reader, run_selector, worktree_override, true)?.ok_or_else(|| {
        StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            "no active run found for this workspace or worktree",
        )
        .with_hint("pass --run <run_id|run_name> or start a run with 'acdd start'")
    })
}

/// Resolve a `RunRecord` without failing when the workspace has zero active runs (unless selector is ambiguous/missing).
pub fn resolve_run_optional(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    run_selector: Option<&str>,
    worktree_override: Option<&Path>,
    active_only: bool,
) -> Result<Option<RunRecord>, StateError> {
    if let Some(sel) = run_selector {
        let trimmed = sel.trim();
        if trimmed.is_empty() {
            return Err(StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                "run selector cannot be empty",
            ));
        }
        let all = list_all_runs(reader, if active_only { Some(RunStatus::Active) } else { None })?;
        let matches: Vec<RunRecord> = all
            .into_iter()
            .filter(|r| r.run_id.as_str() == trimmed || r.run_name.as_str() == trimmed)
            .collect();
        return match matches.len() {
            0 => Err(StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!("no run matching '{trimmed}'"),
            )),
            1 => Ok(Some(matches.into_iter().next().expect("checked len == 1"))),
            _ => Err(StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!("multiple runs match selector '{trimmed}'"),
            )),
        };
    }

    let target_wt = worktree_override.unwrap_or(&config.worktree);
    let wt_str = target_wt.to_string_lossy();
    if let Some(run) = reader.resolve_active_run_by_worktree(&wt_str)? {
        return Ok(Some(run));
    }
    let canon_wt = canonicalize_macos_var(target_wt);
    let canon_str = canon_wt.to_string_lossy();
    if canon_str != wt_str {
        if let Some(run) = reader.resolve_active_run_by_worktree(&canon_str)? {
            return Ok(Some(run));
        }
    }

    let active = list_all_runs(reader, Some(RunStatus::Active))?;
    let is_primary = canon_wt == primary_workspace_root(config);
    match active.len() {
        0 => Ok(None),
        1 if is_primary => Ok(Some(active.into_iter().next().expect("checked len == 1"))),
        1 => Err(StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!(
                "active run exists, but worktree '{}' is not bound to it",
                target_wt.display()
            ),
        )
        .with_hint("specify --run <run_id|run_name> or --worktree <path>")),
        _ => Err(StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!(
                "{} active runs exist and current worktree '{}' does not uniquely select one",
                active.len(),
                target_wt.display()
            ),
        )
        .with_hint("specify --run <run_id|run_name> or --worktree <path>")),
    }
}

/// Paginate all runs matching `status` from `reader`.
pub fn list_all_runs(
    reader: &dyn StateReader,
    status: Option<RunStatus>,
) -> Result<Vec<RunRecord>, StateError> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = reader.list_runs(&ListRunsReq {
            workspace: None,
            status,
            include_tombstones: false,
            limit: 100,
            offset,
            generation: None,
        })?;
        out.extend(page.runs);
        if out.len() >= page.total_runs || page.total_runs == 0 {
            break;
        }
        offset = out.len();
    }
    Ok(out)
}

/// Paginate all tombstones from `reader`.
pub fn list_all_tombstones(reader: &dyn StateReader) -> Result<Vec<RunTombstoneRecord>, StateError> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = reader.list_runs(&ListRunsReq {
            workspace: None,
            status: None,
            include_tombstones: true,
            limit: 100,
            offset,
            generation: None,
        })?;
        let is_empty = page.tombstones.is_empty();
        out.extend(page.tombstones);
        if out.len() >= page.total_tombstones || is_empty {
            break;
        }
        offset = out.len();
    }
    Ok(out)
}

/// Parse a `ResolvedGateProfile` from `runs.profile_json`.
pub fn parse_profile_from_run(run: &RunRecord) -> Result<ResolvedGateProfile, StateError> {
    serde_json::from_str(&run.profile_json).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("corrupt runs.profile_json for '{}': {err}", run.run_id),
        )
    })
}

/// Convert a [`ResolvedGateProfile`] into the pure state machine's [`MachineProfileInput`].
pub fn to_machine_profile(profile: &ResolvedGateProfile) -> Result<MachineProfileInput, StateError> {
    let mut proof_schemas: BTreeMap<String, SchemaDef> = profile
        .proofs
        .iter()
        .map(|(k, v)| (k.clone(), v.schema.clone()))
        .collect();

    let last_top_root_idx = profile
        .gates
        .iter()
        .rposition(|g| !g.is_tasks_block() && g.gates.is_empty());

    let mut gates = Vec::with_capacity(profile.gates.len());
    for (idx, item) in profile.gates.iter().enumerate() {
        if let Some(ref tb) = item.tasks {
            let mut task_gates = Vec::with_capacity(tb.gates.len());
            for g in &tb.gates {
                task_gates.push(convert_gate_def(profile, g, false, &mut proof_schemas)?);
            }
            gates.push(MachineGraphItem::TasksBlock(MachineTasksBlockDef {
                when: tb.when.clone(),
                gates: task_gates,
            }));
        } else {
            let is_terminal_root = last_top_root_idx == Some(idx) && item.pass.is_none();
            gates.push(MachineGraphItem::Gate(Box::new(convert_gate_def(
                profile,
                item,
                is_terminal_root,
                &mut proof_schemas,
            )?)));
        }
    }

    Ok(MachineProfileInput {
        profile_id: profile.profile_id.clone(),
        profile_json: profile.to_canonical_json()?,
        git: MachineGitPolicy {
            isolation: profile.git.isolation,
            worktrees: profile.git.worktrees,
        },
        default_save: profile.save,
        protected_paths: profile.protected_paths.clone(),
        tags: profile.tags.clone(),
        proof_schemas,
        gates,
    })
}

fn convert_gate_def(
    profile: &ResolvedGateProfile,
    gate: &GateDef,
    is_terminal_root: bool,
    proof_schemas: &mut BTreeMap<String, SchemaDef>,
) -> Result<MachineGateDef, StateError> {
    let id = GateId::new(&gate.id)?;
    let proof = match &gate.proof {
        ProofDef::Kind(ProofKind::None) => MachineProofKind::None,
        ProofDef::Kind(ProofKind::Command) => MachineProofKind::Command,
        ProofDef::Kind(ProofKind::Call) => MachineProofKind::Call,
        ProofDef::Kind(ProofKind::Review) => MachineProofKind::Review,
        ProofDef::Named(name) | ProofDef::ResolvedNamed { name, .. } => {
            MachineProofKind::CustomSchema(name.clone())
        }
        ProofDef::Scheme { scheme } => {
            let dig = acdd_core::format::canonical_json_sha256(
                &serde_json::to_value(scheme.as_ref())
                    .map_err(|e| StateError::new(ErrorCode::ProfileSchemaInvalid, e.to_string()))?,
            );
            let synthetic = format!("__inline_{}", &dig[..12]);
            proof_schemas.insert(synthetic.clone(), *scheme.clone());
            MachineProofKind::CustomSchema(synthetic)
        }
    };

    let mut children = Vec::with_capacity(gate.gates.len());
    for child in &gate.gates {
        children.push(convert_gate_def(profile, child, false, proof_schemas)?);
    }

    let effective_post = gate.save.post.unwrap_or(if is_terminal_root {
        profile.save
    } else {
        SaveAction::None
    });

    Ok(MachineGateDef {
        id,
        role: gate.role.clone(),
        proof,
        command: gate.command.clone(),
        call: gate.call.clone(),
        with_params: gate
            .with
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default())),
        checks: gate
            .checks
            .iter()
            .map(|c| MachineCheckDef {
                qualified_name: c.check.clone(),
                params: c.with.clone(),
            })
            .collect(),
        contours: gate.contours.clone(),
        independent_from: gate.independent_from.clone(),
        need: gate.need.clone(),
        pass: gate.pass.as_ref().map(|g| g.as_str().to_owned()),
        fail: gate.fail.as_ref().map(|g| g.as_str().to_owned()),
        fail_drift: gate.fail_drift.as_ref().map(|g| g.as_str().to_owned()),
        max_attempts: gate.max_attempts.max(1),
        receipt: gate.receipt,
        save: MachineSavePolicy {
            post: effective_post,
            immutable: gate.save.immutable,
            archive: gate.save.archive,
            prune: gate.save.prune,
            report: gate.save.report.clone(),
        },
        scope: gate.scope.clone(),
        when: gate.when.clone(),
        children,
        definition_digest: gate.definition_digest()?,
    })
}

/// Parse a user-supplied node cursor string against the tasks present in `snapshot`.
pub fn parse_cursor_for_snapshot(
    snapshot: &RunGraphSnapshot,
    raw: &str,
) -> Result<NodeCursor, StateError> {
    let trimmed = raw.trim();
    NodeCursor::parse_with_task_lookup(trimmed, |seg| {
        snapshot
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Task && n.task_ref.as_ref().is_some_and(|t| t.as_str() == seg))
    })
    .map_err(StateError::from)
}



/// Resolve an owned source checkout root for spec/profile IO while keeping state DB anchored on `primary`.
pub fn resolve_source_checkout(
    primary: &Path,
    layout: Option<&RepoWorktreeLayout>,
    candidate: &Path,
    explicit: bool,
) -> Result<PathBuf, StateError> {
    let _ = primary;
    let candidate = canonicalize_macos_var(candidate);
    if !candidate.is_dir() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("checkout path '{}' is not a directory", candidate.display()),
        ));
    }
    let Some(layout) = layout else {
        return Ok(candidate);
    };
    if explicit {
        let resolved = resolve_repo_layout(&candidate)?;
        if resolved.primary_worktree != layout.primary_worktree {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "worktree path '{}' is not owned by this repository",
                    candidate.display()
                ),
            ));
        }
    }
    Ok(candidate)
}

/// Overlay checkout-local `acdd.yaml` bindings onto `config` while preserving primary DB routing.
pub fn checkout_config(
    config: &StateEngineConfig,
    primary: &Path,
    source: &Path,
    source_override: Option<&Adapter>,
) -> Result<StateEngineConfig, StateError> {
    let source = canonicalize_macos_var(source);
    let adapter = if let Some(ad) = source_override {
        ad.clone()
    } else if source == canonicalize_macos_var(primary) {
        load_ws_adapter(primary)?
    } else {
        load_adapter(&source, None).map_err(|err| {
            StateError::new(
                ErrorCode::ConfigMigrationRequired,
                format!("failed to load acdd.yaml from '{}': {err}", source.display()),
            )
        })?
    };
    let mut checkout = config.clone();
    checkout.worktree = source;
    checkout.spec_collections = adapter.specs.clone();
    checkout.profiles_dirs = adapter.profiles_dirs.clone();
    checkout.linked_workspaces = adapter
        .linked_workspaces
        .iter()
        .map(|lw| (lw.name.clone(), lw.path.clone()))
        .collect();
    checkout.modules = adapter.modules.clone();
    checkout.response_policy = adapter.response.clone();
    checkout.retain_traces = adapter.retain_traces;
    Ok(checkout)
}

/// Auto-select the next ready gate cursor when `ClaimReq.node` is omitted.
pub fn auto_select_ready_cursor(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> Result<NodeCursor, StateError> {
    let schedule = compute_schedule(snapshot, obs)?;
    if let Some(root_node_id) = schedule.ready_root_nodes.first() {
        return NodeCursor::from_node_id(root_node_id).map_err(StateError::from);
    }
    for task_ref in &schedule.ready_tasks {
        let parent_id = format!("task:{task_ref}");
        if let Some(gate_node) = snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.parent_id.as_deref() == Some(parent_id.as_str())
                    && matches!(n.status, NodeStatus::Ready | NodeStatus::InProgress)
            })
            .min_by_key(|n| n.seq_order)
        {
            return NodeCursor::from_node_id(&gate_node.node_id).map_err(StateError::from);
        }
    }
    if let Some(in_prog) = snapshot
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                && n.status == NodeStatus::Ready
        })
        .min_by_key(|n| n.seq_order)
    {
        return NodeCursor::from_node_id(&in_prog.node_id).map_err(StateError::from);
    }
    Err(StateError::new(
        ErrorCode::GateVerificationFailed,
        format!("no ready gate nodes available to claim in run '{}'", snapshot.run.run_id),
    ))
}


/// Auto-select the single active claimed or ready gate when `ContextReq.node` is omitted.
pub fn auto_select_context_cursor(
    snapshot: &RunGraphSnapshot,
    worker_id: Option<&WorkerId>,
) -> Result<NodeCursor, StateError> {
    let is_gate = |n: &GateNodeRecord| matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate);
    let matches_worker = |n: &GateNodeRecord| {
        worker_id.is_none_or(|w| n.worker_id.as_ref() == Some(w))
    };
    let to_cursors = |nodes: &[&GateNodeRecord]| -> Result<Vec<String>, StateError> {
        nodes
            .iter()
            .map(|n| NodeCursor::from_node_id(&n.node_id).map(|c| c.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(StateError::from)
    };

    let mut claimed: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| is_gate(n) && n.status == NodeStatus::InProgress && matches_worker(n))
        .collect();
    claimed.sort_by_key(|n| n.seq_order);
    match claimed.len() {
        1 => return NodeCursor::from_node_id(&claimed[0].node_id).map_err(StateError::from),
        n if n > 1 => {
            let cursors = to_cursors(&claimed)?;
            return Err(StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "multiple active claimed gates match context request: {}; specify `node`",
                    cursors.join(", ")
                ),
            ));
        }
        _ => {}
    }

    let mut ready: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| is_gate(n) && n.status == NodeStatus::Ready && matches_worker(n))
        .collect();
    ready.sort_by_key(|n| n.seq_order);
    match ready.len() {
        1 => NodeCursor::from_node_id(&ready[0].node_id).map_err(StateError::from),
        n if n > 1 => {
            let cursors = to_cursors(&ready)?;
            Err(StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                format!(
                    "multiple ready gates match context request: {}; specify `node`",
                    cursors.join(", ")
                ),
            ))
        }
        _ => Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "no active claimed or ready gate found for context in run '{}'",
                snapshot.run.run_id
            ),
        )),
    }
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

/// Read a confined file inside `root` as UTF-8.
pub fn read_confined_utf8(root: &Path, relpath: &str) -> Result<String, StateError> {
    let mut file = open_confined(root, Path::new(relpath), OpenOptions::new().read(true)).map_err(
        |err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to read confined file '{relpath}': {err}"),
            )
        },
    )?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to decode UTF-8 in '{relpath}': {err}"),
        )
    })?;
    Ok(buf)
}

pub use crate::usecases::deps::{
    collect_external_satisfied_deps, parse_task_dependency, verify_run_level_depends_on,
};

/// Build the current process session fingerprint `host_id:pid:start_time`.
pub fn current_session_fingerprint() -> String {
    let host = current_host_id();
    let pid = std::process::id();
    let start_time = acdd_core::sys::read_process_start_time(pid)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    format!("{host}:{pid}:{start_time}")
}

fn current_host_id() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end]) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_owned())
}

/// Probe whether the previous local claim PID in `host_id:pid:start_time` is alive (`Some(false)` = dead).
///
/// Returns `None` when the fingerprint belongs to a different host or cannot be parsed.
pub fn is_local_pid_alive(session_fingerprint: Option<&str>) -> Option<bool> {
    let fp = session_fingerprint?;
    let mut parts = fp.splitn(3, ':');
    let host = parts.next()?;
    let pid_str = parts.next()?;
    let recorded_start = parts.next()?;
    if host != current_host_id() {
        return None;
    }
    let pid = pid_str.parse::<u32>().ok()?;
    if pid == 0 {
        return Some(false);
    }

    #[cfg(target_os = "linux")]
    {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return Some(false);
        }
        if let Some(current_start) =
            acdd_core::sys::read_process_start_time(pid).map(|v| v.to_string())
        {
            if recorded_start != "0" && current_start != recorded_start {
                return Some(false);
            }
            return Some(true);
        }
        Some(false)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Ok(pid_t) = libc::pid_t::try_from(pid) else {
            return Some(false);
        };
        if pid_t <= 0 {
            return Some(false);
        }
        if recorded_start != "0" {
            if let Some(current_start) =
                acdd_core::sys::read_process_start_time(pid).map(|v| v.to_string())
            {
                if current_start != recorded_start {
                    return Some(false);
                }
            }
        }
        let rc = unsafe { libc::kill(pid_t, 0) };
        if rc == 0 {
            return Some(true);
        }
        let err = std::io::Error::last_os_error().raw_os_error();
        if err == Some(libc::ESRCH) {
            Some(false)
        } else {
            Some(true)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, recorded_start);
        None
    }
}

/// Input fields for building a canonical `run_tombstones.cleanup_manifest`.
pub(crate) struct CleanupManifestInput<'a> {
    pub spec_path: Option<&'a str>,
    pub run_worktree: Option<&'a str>,
    pub run_branch: Option<&'a str>,
    pub task_worktrees_and_branches: &'a [(Option<&'a str>, Option<&'a str>)],
}

/// Return `true` only when `branch` is an ACDD-managed `run/<run_name>` branch for a `worktrees: managed` run.
pub(crate) fn is_managed_run_branch(run: &RunRecord, branch: &str) -> bool {
    parse_profile_from_run(run).is_ok_and(|p| p.git.worktrees == WorktreeLifecycle::Managed)
        && branch == format!("run/{}", run.run_name)
}

fn is_managed_worktree_entry(
    primary: &Path,
    wt: &str,
    run_name: Option<&str>,
    is_run_worktree: bool,
) -> bool {
    let canon_primary = canonicalize_macos_var(primary);
    let canon_wt = canonicalize_macos_var(Path::new(wt));
    if canon_wt == canon_primary {
        return false;
    }
    let managed_root = canonicalize_macos_var(&primary.join(".worktrees"));
    if canon_wt.parent() != Some(managed_root.as_path()) {
        return false;
    }
    let Some(name) = canon_wt.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match (run_name, is_run_worktree) {
        (Some(rn), true) => name == rn,
        (Some(rn), false) => name.starts_with(&format!("{rn}-")),
        (None, _) => !name.is_empty(),
    }
}

fn is_managed_task_branch_entry(
    br: &str,
    run_branch: Option<&str>,
    run_name: Option<&str>,
) -> bool {
    if run_branch == Some(br) {
        return false;
    }
    match run_name {
        Some(rn) => br.starts_with(&format!("task/{rn}-")),
        None => br.starts_with("task/"),
    }
}

/// Build the canonical `run_tombstones.cleanup_manifest` (`INV-SINGLE-OWNER`).
///
/// Includes `spec_file` (when present under `primary`), managed task worktrees/branches, and any
/// managed run worktree distinct from `primary`. Never includes `run.branch`
/// or manual `--worktree` checkouts/branches.
pub(crate) fn build_tombstone_cleanup_manifest(
    input: &CleanupManifestInput<'_>,
    primary: &Path,
) -> Vec<ArchiveCleanupEntry> {
    build_tombstone_cleanup_manifest_with_policy(input, primary, true, None)
}

fn build_tombstone_cleanup_manifest_with_policy(
    input: &CleanupManifestInput<'_>,
    primary: &Path,
    worktrees_managed: bool,
    run_name: Option<&str>,
) -> Vec<ArchiveCleanupEntry> {
    let mut entries = Vec::new();

    if let Some(sp) = input.spec_path {
        if checked_child(primary, Path::new(sp)).is_ok_and(|p| p.exists()) {
            entries.push(ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: sp.to_owned(),
                workspace: None,
                force: false,
            });
        }
    }

    let mut seen_wt = BTreeSet::new();
    let mut seen_br = BTreeSet::new();

    if worktrees_managed {
        for &(wt_opt, br_opt) in input.task_worktrees_and_branches {
            if let Some(wt) = wt_opt {
                if is_managed_worktree_entry(primary, wt, run_name, false)
                    && seen_wt.insert(wt.to_owned())
                {
                    entries.push(ArchiveCleanupEntry {
                        kind: "worktree".to_owned(),
                        path_or_ref: wt.to_owned(),
                        workspace: None,
                        force: false,
                    });
                }
            }
            if let Some(br) = br_opt {
                if is_managed_task_branch_entry(br, input.run_branch, run_name)
                    && seen_br.insert(br.to_owned())
                {
                    entries.push(ArchiveCleanupEntry {
                        kind: "branch".to_owned(),
                        path_or_ref: br.to_owned(),
                        workspace: None,
                        force: false,
                    });
                }
            }
        }

        if let Some(run_wt) = input.run_worktree {
            if is_managed_worktree_entry(primary, run_wt, run_name, true)
                && seen_wt.insert(run_wt.to_owned())
            {
                entries.push(ArchiveCleanupEntry {
                    kind: "worktree".to_owned(),
                    path_or_ref: run_wt.to_owned(),
                    workspace: None,
                    force: false,
                });
            }
        }
    }

    entries
}

/// Adapter building the canonical cleanup manifest from a [`RunGraphSnapshot`].
pub(crate) fn build_tombstone_cleanup_manifest_from_snapshot(
    _config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
    primary: &Path,
) -> Vec<ArchiveCleanupEntry> {
    let worktrees_managed = parse_profile_from_run(&snapshot.run)
        .is_ok_and(|p| p.git.worktrees == WorktreeLifecycle::Managed);
    let task_wt_br: Vec<(Option<&str>, Option<&str>)> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .map(|n| (n.worktree.as_deref(), n.branch.as_deref()))
        .collect();
    build_tombstone_cleanup_manifest_with_policy(
        &CleanupManifestInput {
            spec_path: snapshot.run.spec_path.as_deref(),
            run_worktree: snapshot.run.worktree.as_deref(),
            run_branch: snapshot.run.branch.as_deref(),
            task_worktrees_and_branches: &task_wt_br,
        },
        primary,
        worktrees_managed,
        Some(snapshot.run.run_name.as_str()),
    )
}

pub(crate) fn resume_pending_tombstone_cleanups_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
) -> Result<Vec<String>, StateError> {
    let mut cleaned = Vec::new();
    for tomb in list_all_tombstones(backend)?.iter().filter(|t| t.cleanup_pending) {
        cleaned.extend(cleanup_tombstone(config, backend, tomb, layout)?);
    }
    Ok(cleaned)
}

pub(super) fn cleanup_tombstone(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    tomb: &RunTombstoneRecord,
    locked: Option<&RepoWorktreeLayout>,
) -> Result<Vec<String>, StateError> {
    let primary = primary_workspace_root(config);
    let mut workspaces = config.linked_workspaces.values().map(|ws| canonicalize_macos_var(ws)).collect::<BTreeSet<_>>();
    if locked.is_none() && resolve_repo_layout_optional(&primary)?.is_some() {
        workspaces.insert(primary.clone());
    }
    let mut cleaned = Vec::new();
    let mut all_ok = true;
    for entry in &tomb.cleanup_manifest {
        let path = Path::new(&entry.path_or_ref);
        let workspace = entry.workspace.as_deref().map(Path::new);
        if let Some(ws) = workspace.filter(|ws| !matches!(entry.kind.as_str(), "spec_file" | "worktree") && ws.exists()) {
            workspaces.insert(canonicalize_macos_var(ws));
        }
        let result = match entry.kind.as_str() {
            "spec_file" => match checked_child(workspace.unwrap_or(&primary), path) {
                Ok(target) if !target.exists() => continue,
                Ok(target) => fs::remove_file(target).is_ok(),
                Err(_) => false,
            },
            "worktree" => remove_worktree_and_branch(&primary, Some(path), None, entry.force).is_ok(),
            "branch" => {
                let force = entry.force || (tomb.terminal_status == TerminalStatus::Completed
                    && tomb.closed_commit.as_ref().is_some_and(|closed| {
                        is_branch_merged_into_ref(&primary, &entry.path_or_ref, closed.as_str()).unwrap_or(false)
                    }));
                remove_worktree_and_branch(&primary, None, Some(&entry.path_or_ref), force).is_ok()
            }
            _ => continue,
        };
        if result {
            cleaned.push(entry.path_or_ref.clone());
        }
        all_ok &= result;
    }
    match cleanup_run_refs(locked, &workspaces, &tomb.run_id) {
        Ok(refs) => cleaned.extend(refs),
        Err(_) => all_ok = false,
    }
    let _ = prune_old_run_trace_dirs(&primary, backend, config.retain_traces, None, None);
    if all_ok {
        exec_tx_with(backend, |tx| tx.mark_tombstone_cleaned(&tomb.run_id))?;
    }
    Ok(cleaned)
}

/// Canonical helper applying effective `IsolationMode` and `WorktreeLifecycle` to a resolved
/// profile, neutralizing `save: commit` -> `none` whenever `isolation == IsolationMode::InPlace`.
pub(crate) fn apply_effective_isolation(
    profile: &mut ResolvedGateProfile,
    isolation: crate::types::IsolationMode,
    worktrees: WorktreeLifecycle,
) {
    profile.git.isolation = isolation;
    profile.git.worktrees = if isolation == crate::types::IsolationMode::InPlace {
        WorktreeLifecycle::Manual
    } else {
        worktrees
    };
    if isolation == crate::types::IsolationMode::InPlace {
        neutralize_in_place_save(profile);
    }
}

fn neutralize_in_place_save(profile: &mut ResolvedGateProfile) {
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

/// Join non-empty check observation salts or index generations deterministically (`None` if empty).
pub(crate) fn combine_check_observations(items: &[String]) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        Some(items.join("|"))
    }
}

fn dir_size_bytes_no_follow(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes_no_follow(&p));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn confined_run_traces_dir(primary_worktree: &Path) -> Option<PathBuf> {
    let run_dir = checked_child(primary_worktree, Path::new(".acdd/run")).ok()?;
    fs::symlink_metadata(&run_dir)
        .is_ok_and(|m| m.file_type().is_dir())
        .then_some(run_dir)
}

/// Remove closed `.acdd/run/<run_id>/` directories beyond `retain_traces` and optional
/// `max_total_bytes` budget, ALWAYS preserving active runs queried from `reader` plus optional
/// `exempt_run_id`. Fails closed (deletes nothing) if querying active runs errors.
pub fn prune_old_run_trace_dirs(
    primary_worktree: &Path,
    reader: &dyn StateReader,
    retain_traces: usize,
    max_total_bytes: Option<u64>,
    exempt_run_id: Option<&str>,
) -> Result<(), StateError> {
    let Some(run_dir) = confined_run_traces_dir(primary_worktree) else {
        return Ok(());
    };
    let mut active_ids: BTreeSet<String> = list_all_runs(reader, Some(RunStatus::Active))?
        .into_iter()
        .map(|r| r.run_id.as_str().to_owned())
        .collect();
    if let Some(exempt) = exempt_run_id {
        active_ids.insert(exempt.to_owned());
    }
    let Ok(entries) = fs::read_dir(&run_dir) else {
        return Ok(());
    };

    let mut closed_dirs: Vec<(std::time::SystemTime, String, u64, PathBuf)> = Vec::new();
    let mut kept_count = 0usize;
    let mut kept_bytes: u64 = 0;

    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let bytes = if max_total_bytes.is_some() {
            dir_size_bytes_no_follow(&p)
        } else {
            0
        };
        if active_ids.contains(name) {
            kept_count = kept_count.saturating_add(1);
            kept_bytes = kept_bytes.saturating_add(bytes);
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        closed_dirs.push((mtime, name.to_owned(), bytes, p));
    }

    closed_dirs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    for (_, _, bytes, old_dir) in closed_dirs {
        let within_count = kept_count < retain_traces;
        let within_bytes =
            max_total_bytes.is_none_or(|limit| kept_bytes.saturating_add(bytes) <= limit);
        if within_count && within_bytes {
            kept_count = kept_count.saturating_add(1);
            kept_bytes = kept_bytes.saturating_add(bytes);
        } else {
            let _ = fs::remove_dir_all(old_dir);
        }
    }
    Ok(())
}

/// Append a single JSONL trace event to `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`
/// under `<git_common_dir>/acdd/traces.lock` (leaf lock, never inside `StateTx`).
pub fn append_trace_event(
    layout: Option<&RepoWorktreeLayout>,
    primary_worktree: &Path,
    run_id: &RunId,
    event: &Value,
) -> Result<(), StateError> {
    let lock_path = layout
        .map(|l| l.traces_lock_path.clone())
        .unwrap_or_else(|| primary_worktree.join(".acdd/traces.lock"));
    let _guard = acquire_exclusive(&lock_path).map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to acquire traces.lock: {err}"),
        )
    })?;

    let rel = Path::new(".acdd/run")
        .join(run_id.as_str())
        .join("traces.jsonl");
    if let Some(parent) = primary_worktree.join(&rel).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut line = serde_json::to_string(event).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize trace event: {err}"),
        )
    })?;
    line.push('\n');

    let mut file = open_confined(
        primary_worktree,
        &rel,
        OpenOptions::new().create(true).append(true),
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to open traces.jsonl '{}': {err}", rel.display()),
        )
    })?;
    file.write_all(line.as_bytes()).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to append trace event: {err}"),
        )
    })?;
    Ok(())
}

/// Compute `unintegrated_tasks` against `run_worktree` when `Worktree` isolation and all tasks are completed.
pub fn compute_unintegrated_tasks(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
) -> Result<Vec<TaskRef>, StateError> {
    if snapshot.run.isolation != crate::types::IsolationMode::Worktree
        || !crate::machine::schedule::all_non_cancelled_tasks_completed(snapshot)
    {
        return Ok(Vec::new());
    }
    let primary = primary_workspace_root(config);
    let run_wt = snapshot
        .run
        .worktree
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(primary);
    if resolve_repo_layout_optional(&run_wt)?.is_none() {
        return Ok(Vec::new());
    }
    let inputs = collect_integration_inputs(snapshot);
    crate::git_ops::integrate::verify_tasks_integrated(&snapshot.run.run_id, &run_wt, &inputs)
}

/// Generate a fresh unique run identifier matching `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`.
pub fn generate_run_id(prefix: &str) -> Result<RunId, StateError> {
    let seq = TRACE_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = Utc::now().format("%Y%m%d%H%M%S");
    let pid = std::process::id();
    let clean_prefix: String = prefix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(20)
        .collect();
    let head = if clean_prefix.is_empty()
        || !clean_prefix.as_bytes()[0].is_ascii_alphanumeric()
    {
        "run"
    } else {
        clean_prefix.as_str()
    };
    let raw = format!("{head}-{ts}-{pid}-{seq}");
    RunId::new(raw).map_err(StateError::from)
}

/// Collect `TaskIntegrationCheckInput` descriptors for all completed tasks in `snapshot`.
pub fn collect_integration_inputs(
    snapshot: &RunGraphSnapshot,
) -> Vec<crate::git_ops::integrate::TaskIntegrationCheckInput> {
    let completed_tasks: BTreeMap<TaskRef, &GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
        .filter_map(|n| n.task_ref.as_ref().map(|tr| (tr.clone(), n)))
        .collect();

    let mut out = Vec::new();
    for (tref, task_node) in &completed_tasks {
        let Some(ref initial_claim_tree) = task_node.claim_tree else {
            continue;
        };
        let Some(ref accepted_tree) = task_node.accepted_tree else {
            continue;
        };
        let mut downstream = Vec::new();
        for other_ref in completed_tasks.keys() {
            if other_ref != tref && is_transitively_dependent(snapshot, other_ref, tref) {
                downstream.push(other_ref.clone());
            }
        }
        out.push(crate::git_ops::integrate::TaskIntegrationCheckInput {
            task_ref: tref.clone(),
            initial_claim_tree: initial_claim_tree.clone(),
            accepted_tree: accepted_tree.clone(),
            admitted_scope: container_scope(snapshot, task_node),
            downstream_tasks: downstream,
        });
    }
    out
}

fn is_transitively_dependent(
    snapshot: &RunGraphSnapshot,
    candidate_downstream: &TaskRef,
    ancestor: &TaskRef,
) -> bool {
    let mut visited = BTreeSet::new();
    let mut stack = vec![candidate_downstream.clone()];
    while let Some(cur) = stack.pop() {
        if !visited.insert(cur.clone()) {
            continue;
        }
        for dep in &snapshot.dependencies {
            if dep.task_ref == cur && dep.depends_on_run_id == snapshot.run.run_id {
                if &dep.depends_on_ref == ancestor {
                    return true;
                }
                stack.push(dep.depends_on_ref.clone());
            }
        }
    }
    false
}

fn candidate_archive_relpaths(spec_path: &str, spec_collection_paths: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    let normalized = spec_path.trim().replace('\\', "/");
    let norm_spec = normalized.strip_prefix("./").unwrap_or(&normalized);
    let path = Path::new(norm_spec);
    let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
        return candidates;
    };

    let parent = path
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');

    let mut matched_col: Option<String> = None;
    let mut matched_len = 0usize;
    for col_path in spec_collection_paths {
        let trimmed = col_path.trim().replace('\\', "/");
        let trimmed = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if !trimmed.is_empty() && path.starts_with(Path::new(trimmed)) && trimmed.len() > matched_len
        {
            matched_len = trimmed.len();
            matched_col = Some(trimmed.to_owned());
        }
    }

    if !parent.is_empty() && parent != "." && parent != "archive" && !parent.ends_with("/archive") {
        candidates.push(format!("{parent}/archive/{file_name}"));
        if let Some(col) = matched_col {
            if col != parent {
                let col_rel = format!("{col}/archive/{file_name}");
                if !candidates.contains(&col_rel) {
                    candidates.push(col_rel);
                }
            }
        }
        return candidates;
    }

    // Absent or non-standard parent fallback (e.g., bare filename or already under archive/).
    for col_path in spec_collection_paths {
        let trimmed = col_path.trim().replace('\\', "/");
        let trimmed = trimmed
            .strip_prefix("./")
            .unwrap_or(&trimmed)
            .trim_end_matches('/');
        if !trimmed.is_empty() {
            let rel = format!("{trimmed}/archive/{file_name}");
            if !candidates.contains(&rel) {
                candidates.push(rel);
            }
        }
    }
    let root_rel = format!("archive/{file_name}");
    if !candidates.contains(&root_rel) {
        candidates.push(root_rel);
    }
    candidates
}

fn git_refs_to_probe(row: &RestoredActiveRun) -> Vec<String> {
    let mut refs = vec!["HEAD".to_owned()];
    if let Some(ref br) = row.branch {
        let trimmed = br.trim();
        if !trimmed.is_empty() && trimmed != "HEAD" {
            let full_ref = if trimmed.starts_with("refs/") {
                trimmed.to_owned()
            } else {
                format!("refs/heads/{trimmed}")
            };
            if !refs.contains(&full_ref) {
                refs.push(full_ref);
            }
        }
    }
    refs
}

fn archive_matches_run(doc: &SpecDocument, row: &RestoredActiveRun) -> bool {
    if doc.status != SpecStatus::Completed {
        return false;
    }
    if let Some(ref expected_spec_id) = row.spec_id {
        if &doc.id != expected_spec_id {
            return false;
        }
    }
    if let Some(ref closure) = doc.closure {
        if closure.status != SpecStatus::Completed {
            return false;
        }
        if closure
            .task_receipts
            .values()
            .any(|r| r.run_id.as_str() != row.run_id)
        {
            return false;
        }
    }
    !doc.tasks
        .iter()
        .filter_map(|t| t.receipt.as_ref())
        .any(|r| r.run_id.as_str() != row.run_id)
}

fn find_committed_matching_archive(
    ws_root: &Path,
    is_git_repo: bool,
    row: &RestoredActiveRun,
    candidates: &[String],
) -> Option<(String, Option<String>, SpecDocument)> {
    let refs = git_refs_to_probe(row);
    for archive_rel in candidates {
        let Ok(child_path) = checked_child(ws_root, Path::new(archive_rel)) else {
            continue;
        };
        if is_git_repo {
            for git_ref in &refs {
                let Ok(Some(content)) = read_git_blob_utf8(ws_root, git_ref, archive_rel) else {
                    continue;
                };
                let Ok(doc) = parse_spec(&content) else {
                    continue;
                };
                if archive_matches_run(&doc, row) {
                    return Some((archive_rel.clone(), Some(git_ref.clone()), doc));
                }
            }
        } else if child_path.is_file() {
            let Ok(content) = fs::read_to_string(&child_path) else {
                continue;
            };
            let Ok(doc) = parse_spec(&content) else {
                continue;
            };
            if archive_matches_run(&doc, row) {
                return Some((archive_rel.clone(), None, doc));
            }
        }
    }
    None
}

/// Reconcile restored active non-ephemeral runs against committed `<collection>/archive/*.md`,
/// producing terminal `completed` tombstones for runs whose archive already landed.
pub(crate) fn reconcile_restored_runs_with_archives(
    active_runs: &[RestoredActiveRun],
    workspace_root: Option<&Path>,
    spec_collection_paths: &[String],
) -> Result<Vec<RunTombstoneRecord>, StateError> {
    let mut tombstones = Vec::new();
    for row in active_runs {
        if row.is_ephemeral {
            continue;
        }
        let Some(ref spec_path) = row.spec_path else {
            continue;
        };
        let ws_root = workspace_root.unwrap_or_else(|| Path::new(&row.workspace));
        let is_git_repo = ws_root.join(".git").exists() || git_common_dir(ws_root).is_ok();
        let candidates = candidate_archive_relpaths(spec_path, spec_collection_paths);
        let Some((archive_rel, matched_ref, doc)) =
            find_committed_matching_archive(ws_root, is_git_repo, row, &candidates)
        else {
            continue;
        };

        let spec_id = Some(doc.id).or_else(|| row.spec_id.clone());
        let title = if doc.title.trim().is_empty() {
            row.run_name.clone()
        } else {
            doc.title
        };

        let closed_commit = if let Some(ref git_ref) = matched_ref {
            git_stdout(
                ws_root,
                &["log", "-n", "1", "--format=%H", git_ref, "--", &archive_rel],
                "GIT_LOG_FAILED",
            )
            .ok()
            .and_then(|oid| CommitOid::new(oid).ok())
        } else {
            None
        };

        let task_refs: Vec<(Option<&str>, Option<&str>)> = row
            .task_worktrees_and_branches
            .iter()
            .map(|(wt, br)| (wt.as_deref(), br.as_deref()))
            .collect();
        let cleanup_manifest = build_tombstone_cleanup_manifest(
            &CleanupManifestInput {
                spec_path: Some(spec_path.as_str()),
                run_worktree: row.worktree.as_deref(),
                run_branch: row.branch.as_deref(),
                task_worktrees_and_branches: &task_refs,
            },
            ws_root,
        );

        let run_id = RunId::new(row.run_id.clone())?;
        let run_name = RunName::new(row.run_name.clone())?;
        let generation = u64::try_from(row.generation).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("negative generation on restored run '{}'", row.run_id),
            )
        })?;
        let target_base_commit = row
            .target_base_commit
            .clone()
            .map(CommitOid::new)
            .transpose()?;

        tombstones.push(RunTombstoneRecord {
            run_id,
            run_name,
            collection: row.collection.clone(),
            workspace: row.workspace.clone(),
            spec_id,
            title,
            profile_id: row.profile_id.clone(),
            is_ephemeral: false,
            generation,
            terminal_status: TerminalStatus::Completed,
            target_base: row.target_base.clone(),
            target_base_commit,
            archive_path: Some(archive_rel),
            closed_commit,
            cleanup_pending: true,
            cleanup_manifest,
            summary_json: r#"{"reconciled_from_archive":true}"#.to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: acdd_core::telemetry::now_utc_iso_ms(),
        });
    }
    Ok(tombstones)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::RunHarness;
    use acdd_core::config::default_specs;
    use acdd_core::response::ResponsePolicy;
    use tempfile::TempDir;

    #[test]
    fn build_path_policy_and_cleanup_manifest_invariants() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let harness = RunHarness::in_memory().expect("in-memory harness");
        let mut graph = RunHarness::build_sample_graph(
            "run-life-1",
            "AC-001",
            root.to_str().unwrap(),
            None,
            &["t1"],
        )
        .expect("build sample graph");
        graph.run.collection = Some("milestones".to_owned());
        graph.run.spec_path = Some("docs/milestones/AC-001.md".to_owned());
        graph.run.branch = Some("feature/user-owned".to_owned());
        for node in &mut graph.nodes {
            if node.kind == NodeKind::Task {
                node.branch = Some("task/AC-001-t1".to_owned());
                node.worktree = Some(root.join(".worktrees/AC-001-t1").to_string_lossy().into_owned());
            }
        }
        harness
            .exec_tx(|tx| tx.insert_run_graph(&graph))
            .expect("insert graph");

        let cfg = StateEngineConfig {
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: default_specs(),
            profiles_dirs: Vec::new(),
            linked_workspaces: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            response_policy: ResponsePolicy::default(),
            retain_traces: 10,
        };

        // 1. build_path_policy marks BOTH active spec_path and derived archive_path as engine-managed.
        let policy = build_path_policy(&cfg, &harness, &[]).expect("build_path_policy");
        assert!(!policy.is_candidate_included("docs/milestones/AC-001.md"));
        assert!(!policy.is_candidate_included("docs/milestones/archive/AC-001.md"));
        assert!(policy.is_candidate_included("src/lib.rs"));

        // 2. Manual worktrees profile never populates worktrees/branches into cleanup_manifest.
        fs::create_dir_all(root.join("docs/milestones")).expect("mkdir");
        fs::write(root.join("docs/milestones/AC-001.md"), "# spec\n").expect("write spec");
        let manual_manifest = build_tombstone_cleanup_manifest_from_snapshot(&cfg, &graph, root);
        assert!(manual_manifest.iter().any(|e| e.kind == "spec_file" && e.path_or_ref == "docs/milestones/AC-001.md"));
        assert!(!manual_manifest.iter().any(|e| matches!(e.kind.as_str(), "worktree" | "branch")));

        // 3. Managed worktrees profile includes ONLY ACDD-managed `.worktrees/<run>-...` and `task/<run>-...`.
        let mut managed_graph = graph.clone();
        managed_graph.run.profile_json = r#"{"version":2,"git":{"worktrees":"managed"}}"#.to_owned();
        managed_graph.run.worktree = Some(root.join(".worktrees/AC-001").to_string_lossy().into_owned());
        managed_graph.nodes.push(GateNodeRecord {
            worktree: Some(root.join("../manual-wt").to_string_lossy().into_owned()),
            branch: Some("feature/manual-branch".to_owned()),
            ..managed_graph
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::Task)
                .unwrap()
                .clone()
        });
        let manifest = build_tombstone_cleanup_manifest_from_snapshot(&cfg, &managed_graph, root);
        assert!(manifest.iter().any(|e| e.kind == "spec_file" && e.path_or_ref == "docs/milestones/AC-001.md"));
        assert!(manifest.iter().any(|e| e.kind == "branch" && e.path_or_ref == "task/AC-001-t1"));
        assert!(manifest.iter().any(|e| e.kind == "worktree" && e.path_or_ref.ends_with(".worktrees/AC-001-t1")));
        assert!(manifest.iter().any(|e| e.kind == "worktree" && e.path_or_ref.ends_with(".worktrees/AC-001")));
        assert!(!manifest.iter().any(|e| e.path_or_ref.contains("manual-wt") || e.path_or_ref == "feature/manual-branch"));
        assert!(
            !manifest.iter().any(|e| e.kind == "snapshot_ref"),
            "cleanup_manifest must not emit dead snapshot_ref entries: {manifest:?}"
        );
        assert!(
            !manifest.iter().any(|e| e.path_or_ref == "feature/user-owned"),
            "cleanup_manifest must never include user/run branch: {manifest:?}"
        );
        assert!(
            !manifest.iter().any(|e| e.kind == "run_ref"),
            "cleanup_manifest must not emit dead run_ref entries: {manifest:?}"
        );

        // 4. resume_pending_tombstone_cleanups retains `cleanup_pending == true` when an entry fails to clean.
        let mut fail_tomb = RunTombstoneRecord {
            run_id: RunId::new("run-life-1").expect("rid"),
            run_name: acdd_core::ids::RunName::new("AC-001").expect("rname"),
            collection: Some("milestones".to_owned()),
            workspace: root.to_string_lossy().into_owned(),
            spec_id: Some("AC-001".to_owned()),
            title: "AC-001".to_owned(),
            profile_id: "code".to_owned(),
            is_ephemeral: false,
            generation: 1,
            terminal_status: crate::types::TerminalStatus::Cleared,
            target_base: None,
            target_base_commit: None,
            archive_path: None,
            closed_commit: None,
            cleanup_pending: true,
            cleanup_manifest: vec![ArchiveCleanupEntry {
                kind: "worktree".to_owned(),
                path_or_ref: root.to_string_lossy().into_owned(),
                workspace: None,
                force: false,
            }],
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: "2026-11-17T10:00:00.000Z".to_owned(),
        };
        harness
            .exec_tx(|tx| {
                tx.archive_and_prune_run(
                    &crate::types::StoragePreconditions {
                        expected_run_generation: 1,
                        expected_node_versions: Vec::new(),
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    &fail_tomb,
                )
            })
            .expect("archive fail tomb");
        let _ = resume_pending_tombstone_cleanups_dyn(&cfg, &harness, None).expect("resume");
        fail_tomb = list_all_tombstones(&harness)
            .expect("list tombs")
            .into_iter()
            .find(|t| t.run_id.as_str() == "run-life-1")
            .expect("tomb exists");
        assert!(
            fail_tomb.cleanup_pending,
            "failed worktree cleanup must leave cleanup_pending=true for retry"
        );

        // 5. `is_local_pid_alive` guards `pid == 0` and overflowing/negative `pid_t` values.
        let host = current_host_id();
        for bad_fp in [
            format!("{host}:0:0"),
            format!("{host}:{}:0", u32::MAX),
            format!("{host}:{}:0", (i32::MAX as u32) + 1),
        ] {
            assert_eq!(
                is_local_pid_alive(Some(&bad_fp)),
                Some(false),
                "is_local_pid_alive must reject unsafe pid in '{bad_fp}'"
            );
        }

        // 6. `prune_old_run_trace_dirs` preserves active runs & `exempt_run_id` under count & byte
        //    budgets, and fails closed (deletes nothing) when the state DB reader errors.
        let run_root = root.join(".acdd/run");
        let active_dir = run_root.join("run-active-keep");
        let exempt_dir = run_root.join("run-exempt-keep");
        let closed_small = run_root.join("run-closed-small");
        let closed_big = run_root.join("run-closed-big");
        for d in [&active_dir, &exempt_dir, &closed_small, &closed_big] {
            fs::create_dir_all(d).expect("mkdir trace dir");
        }
        fs::write(active_dir.join("traces.jsonl"), vec![b'a'; 64]).unwrap();
        fs::write(exempt_dir.join("traces.jsonl"), vec![b'e'; 64]).unwrap();
        fs::write(closed_big.join("traces.jsonl"), vec![b'b'; 256]).unwrap();
        fs::write(closed_small.join("traces.jsonl"), vec![b's'; 32]).unwrap();
        let active_graph = RunHarness::build_sample_graph(
            "run-active-keep",
            "AC-ACTIVE",
            root.to_str().unwrap(),
            None,
            &[],
        )
        .expect("active graph");
        harness
            .exec_tx(|tx| tx.insert_run_graph(&active_graph))
            .expect("seed active");

        // Budget = 160 bytes (active 64 + exempt 64 = 128 -> closed_small 32 fits, closed_big 256 pruned!).
        prune_old_run_trace_dirs(root, &harness, 10, Some(160), Some("run-exempt-keep"))
            .expect("prune with budget");
        assert!(active_dir.is_dir(), "active run dir must never be pruned");
        assert!(exempt_dir.is_dir(), "exempt_run_id dir must never be pruned");
        assert!(!closed_big.exists(), "closed dir exceeding byte budget must be pruned");
        assert!(closed_small.is_dir(), "closed dir within count & byte budget kept");

        // Fail closed when the state DB cannot be read: preserve existing closed dirs.
        let corrupt_db = root.join(".acdd/corrupt-state.sqlite");
        fs::write(&corrupt_db, b"not-a-valid-sqlite-database").unwrap();
        let corrupt_reader = crate::store::sqlite::SqliteStateReader::new(corrupt_db);
        assert!(prune_old_run_trace_dirs(root, &corrupt_reader, 0, Some(0), None).is_err());
        assert!(closed_small.is_dir(), "corrupt reader must fail closed without deleting trace dirs");
    }
}
