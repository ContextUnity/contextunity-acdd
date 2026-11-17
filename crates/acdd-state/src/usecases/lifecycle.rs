//! Shared lifecycle infrastructure: lock ordering, path policies, profile/spec resolution,
//! cross-workspace dependency queries, PID liveness checks, and tombstone cleanup recovery.

use crate::git_ops::worktree::{
    canonicalize_macos_var, cleanup_run_refs, remove_worktree_and_branch, resolve_repo_layout,
    RepoWorktreeLayout,
};
use crate::machine::{
    compute_schedule, container_scope, MachineCheckDef, MachineExpectedFailure, MachineGateDef,
    MachineGitPolicy, MachineGraphItem, MachineProfileInput, MachineProofKind, MachineSavePolicy,
    MachineTaskDep, MachineTasksBlockDef,
};
use crate::profile::{
    validate_profile, CommandDef, GateDef, ProfileValidationOptions, ProofDef, ProofKind,
    ResolvedGateProfile,
};
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::{exec_tx_with, StateBackend, StateReader};
use crate::types::{
    canonical_json_sha256, GateNodeRecord, ListRunsReq, NodeKind, NodeStatus, RunGraphSnapshot,
    RunRecord, RunStatus, RunTombstoneRecord, SaveAction, TaskDependencyRecord, TerminalStatus,
    ValidatedObservations,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::SpecCollectionConfig;
use acdd_core::fs::{canonical_root, checked_child, open_confined};
use acdd_core::lock::{acquire_exclusive, FileLockGuard};
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, NodeCursor, RunId, TaskRef};
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

/// Return current UTC timestamp formatted as strict ISO-8601 `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub fn now_utc_iso() -> String {
    acdd_core::telemetry::now_utc_iso_ms()
}

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

/// Extract `domain_id -> enabled` flags from `config.modules` for [`CompositeDomainProvider`].
pub fn module_domain_flags(config: &StateEngineConfig) -> BTreeMap<String, bool> {
    let mut flags = config.modules.domains.clone();
    flags.insert("code".to_owned(), config.modules.is_enabled("code"));
    flags.insert("docs".to_owned(), config.modules.is_enabled("docs"));
    flags
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

/// Match a workspace-relative spec path to its owning collection key by longest path prefix.
pub fn match_spec_collection(
    collections: &BTreeMap<String, SpecCollectionConfig>,
    spec_relpath: &str,
) -> Option<String> {
    let norm = spec_relpath.trim().replace('\\', "/");
    let norm = norm.strip_prefix("./").unwrap_or(&norm);
    collections
        .iter()
        .filter(|(_, cfg)| {
            let dir = cfg.path.trim_end_matches('/');
            norm == dir
                || (norm.len() > dir.len()
                    && norm.starts_with(dir)
                    && norm.as_bytes()[dir.len()] == b'/')
        })
        .max_by_key(|(_, cfg)| cfg.path.len())
        .map(|(name, _)| name.clone())
}

/// Build [`WorkspacePathPolicy`] incorporating `spec_path` of all active runs plus `extra_managed`.
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
            }
        }
        offset += page.runs.len();
        if offset >= page.total_runs || page.runs.is_empty() {
            break;
        }
    }

    let primary = primary_workspace_root(config);
    let (state_db_abs, _) = config.resolved_state_db_path();
    let state_db_rel = state_db_abs
        .strip_prefix(&primary)
        .or_else(|_| state_db_abs.strip_prefix(&config.workspace_root))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| ".acdd/state.sqlite".to_owned());

    let code_db_rel = config
        .code_db_path
        .as_ref()
        .and_then(|p| {
            p.strip_prefix(&primary)
                .or_else(|_| p.strip_prefix(&config.workspace_root))
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        })
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
    match active.len() {
        0 => Ok(None),
        1 => Ok(Some(active.into_iter().next().expect("checked len == 1"))),
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
            commit_mode: profile.git.commit_mode,
            integrate: profile.git.integrate,
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
        ProofDef::Kind(ProofKind::Contract) => MachineProofKind::Contract,
        ProofDef::Kind(ProofKind::Command) => MachineProofKind::Command,
        ProofDef::Kind(ProofKind::Call) => MachineProofKind::Call,
        ProofDef::Kind(ProofKind::Review) => MachineProofKind::Review,
        ProofDef::Named(name) | ProofDef::ResolvedNamed { name, .. } => {
            MachineProofKind::CustomSchema(name.clone())
        }
        ProofDef::Scheme { scheme } => {
            let dig = canonical_json_sha256(&serde_json::to_value(scheme.as_ref()).map_err(|e| {
                StateError::new(ErrorCode::ProfileSchemaInvalid, e.to_string())
            })?)?;
            let synthetic = format!("__inline_{}", &dig[..12]);
            proof_schemas.insert(synthetic.clone(), *scheme.clone());
            MachineProofKind::CustomSchema(synthetic)
        }
    };

    let require_counts = gate
        .command
        .as_deref()
        .and_then(|cmd_id| profile.commands.get(cmd_id))
        .is_some_and(command_requires_counts);

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
        expected_failure: gate.expected_failure.as_ref().map(|ef| {
            MachineExpectedFailure {
                exit_codes: ef.exit_codes.clone(),
                stderr_contains: ef.stderr_contains.clone(),
            }
        }),
        contours: gate.contours.clone(),
        independent_from: gate.independent_from.clone(),
        need: gate.need.clone(),
        pass: gate.pass.as_ref().map(|g| g.as_str().to_owned()),
        fail: gate.fail.as_ref().map(|g| g.as_str().to_owned()),
        max_attempts: gate.max_attempts.max(1),
        receipt: gate.receipt,
        require_test_target: matches!(gate.proof, ProofDef::Kind(ProofKind::Contract)),
        require_counts,
        save: MachineSavePolicy {
            pre: gate.save.pre.unwrap_or(SaveAction::None),
            post: effective_post,
            immutable: gate.save.immutable,
            archive: gate.save.archive,
            prune: gate.save.prune,
            git_commit_note: gate.save.git_commit_note.clone(),
        },
        scope: gate.scope.clone(),
        when: gate.when.clone(),
        children,
        definition_digest: gate.definition_digest()?,
    })
}

fn command_requires_counts(cmd: &CommandDef) -> bool {
    match cmd {
        CommandDef::Unbound { .. } | CommandDef::Script(_) => false,
        CommandDef::Detailed(d) => d.output.as_ref().is_some_and(|o| o.require_counts),
        CommandDef::Pattern(entries) => entries
            .iter()
            .any(|e| e.output.as_ref().is_some_and(|o| o.require_counts)),
    }
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

/// Parse a `TaskSpec.depends_on` entry (`<task_ref>`, `<run_id>:<task_ref>`, or `<ws_alias>/<run_id>:<task_ref>`).
pub fn parse_task_dependency(
    raw: &str,
    current_workspace: &str,
    current_run_id: &RunId,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
) -> Result<MachineTaskDep, StateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "depends_on entry cannot be empty",
        ));
    }
    if let Some((left, task_part)) = trimmed.split_once(':') {
        let tref = TaskRef::new(task_part)?;
        if let Some((ws_alias, run_part)) = left.split_once('/') {
            let ws_path = linked_workspaces
                .get(ws_alias)
                .or_else(|| spec_repos.get(ws_alias))
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "external dependency '{trimmed}' references unknown linked workspace '{ws_alias}'"
                        ),
                    )
                })?;
            let canon_ws = canonical_root(ws_path).map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "linked workspace '{ws_alias}' path '{}' is invalid or confined-check failed: {err}",
                        ws_path.display()
                    ),
                )
            })?;
            let rid = resolve_external_run_id(&canon_ws, run_part)?;
            Ok(MachineTaskDep {
                depends_on_workspace: canon_ws.to_string_lossy().into_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        } else {
            let rid = resolve_external_run_id(Path::new(current_workspace), left)?;
            Ok(MachineTaskDep {
                depends_on_workspace: current_workspace.to_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        }
    } else {
        Ok(MachineTaskDep {
            depends_on_workspace: current_workspace.to_owned(),
            depends_on_run_id: current_run_id.clone(),
            depends_on_ref: TaskRef::new(trimmed)?,
        })
    }
}

fn resolve_external_run_id(ws_root: &Path, run_sel: &str) -> Result<RunId, StateError> {
    let reader = SqliteStateReader::new(ws_root.join(".acdd/state.sqlite"));
    if let Ok(runs) = list_all_runs(&reader, None) {
        if let Some(found) = runs
            .into_iter()
            .find(|r| r.run_id.as_str() == run_sel || r.run_name.as_str() == run_sel)
        {
            return Ok(found.run_id);
        }
    }
    if let Ok(tombs) = list_all_tombstones(&reader) {
        if let Some(found) = tombs.into_iter().find(|t| {
            t.run_id.as_str() == run_sel
                || t.run_name.as_str() == run_sel
                || t.spec_id.as_deref() == Some(run_sel)
        }) {
            return Ok(found.run_id);
        }
    }
    RunId::new(run_sel).map_err(StateError::from)
}

/// Evaluate external cross-run/cross-workspace dependencies via strictly read-only [`SqliteStateReader`].
///
/// Cleared runs (`terminal_status == 'cleared'`) NEVER satisfy dependencies.
pub fn collect_external_satisfied_deps(
    config: &StateEngineConfig,
    current_run_id: &RunId,
    deps: &[TaskDependencyRecord],
) -> Result<Vec<(String, RunId, TaskRef)>, StateError> {
    let primary = primary_workspace_root(config);
    let primary_str = primary.to_string_lossy().into_owned();
    let mut satisfied = Vec::new();
    let mut readers: BTreeMap<String, SqliteStateReader> = BTreeMap::new();

    for dep in deps {
        if dep.satisfied {
            satisfied.push((
                dep.depends_on_workspace.clone(),
                dep.depends_on_run_id.clone(),
                dep.depends_on_ref.clone(),
            ));
            continue;
        }
        if dep.depends_on_run_id == *current_run_id && dep.depends_on_workspace == primary_str {
            continue;
        }
        let ws_path = PathBuf::from(&dep.depends_on_workspace);
        let reader = readers.entry(dep.depends_on_workspace.clone()).or_insert_with(|| {
            SqliteStateReader::new(ws_path.join(".acdd/state.sqlite"))
        });

        if is_external_task_satisfied(
            reader,
            &ws_path,
            config,
            &dep.depends_on_run_id,
            &dep.depends_on_ref,
        )? {
            satisfied.push((
                dep.depends_on_workspace.clone(),
                dep.depends_on_run_id.clone(),
                dep.depends_on_ref.clone(),
            ));
        }
    }

    Ok(satisfied)
}

fn is_external_task_satisfied(
    reader: &dyn StateReader,
    ws_path: &Path,
    config: &StateEngineConfig,
    target_run_id: &RunId,
    target_task_ref: &TaskRef,
) -> Result<bool, StateError> {
    if let Some(tomb) = reader.load_tombstone(target_run_id)? {
        if tomb.terminal_status != TerminalStatus::Completed {
            // `cleared` tombstones NEVER satisfy dependencies.
            return Ok(false);
        }
        if let Some(ref arch_rel) = tomb.archive_path {
            if let Ok(raw) = read_confined_utf8(ws_path, arch_rel) {
                if let Ok(doc) = parse_spec(&raw) {
                    if let Some(task) = doc.tasks.iter().find(|t| t.task_ref == *target_task_ref) {
                        return Ok(task.status == SpecStatus::Completed);
                    }
                }
            }
        }
        return Ok(true);
    }

    if let Some(node) = reader.load_node(target_run_id, &format!("task:{target_task_ref}"))? {
        return Ok(node.status == NodeStatus::Completed);
    }

    // Fallback: check committed `<collection>/archive/*.md` in `ws_path`.
    let collections = effective_spec_collections(config);
    for col in collections.values() {
        let archive_dir = match checked_child(ws_path, &Path::new(&col.path).join("archive")) {
            Ok(d) if d.is_dir() => d,
            _ => continue,
        };
        for path in sorted_dir_paths(&archive_dir) {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(rel) = path.strip_prefix(ws_path) else {
                continue;
            };
            let Ok(raw) = read_confined_utf8(ws_path, &rel.to_string_lossy()) else {
                continue;
            };
            let Ok(doc) = parse_spec(&raw) else {
                continue;
            };
            if doc.id == target_run_id.as_str()
                && doc.status == SpecStatus::Completed
                && doc
                    .closure
                    .as_ref()
                    .is_some_and(|c| c.status == SpecStatus::Completed)
            {
                if let Some(task) = doc.tasks.iter().find(|t| t.task_ref == *target_task_ref) {
                    return Ok(task.status == SpecStatus::Completed);
                }
            }
        }
    }

    Ok(false)
}

/// Verify run-level `SpecDocument.depends_on` entries are satisfied by completed tombstones or committed archives.
pub fn verify_run_level_depends_on(
    config: &StateEngineConfig,
    local_reader: &dyn StateReader,
    doc: &SpecDocument,
) -> Result<(), StateError> {
    if doc.depends_on.is_empty() {
        return Ok(());
    }
    let primary = primary_workspace_root(config);
    let mut spec_repos = BTreeMap::new();
    for r in &doc.repositories {
        spec_repos.insert(r.name.clone(), PathBuf::from(&r.path));
    }

    for dep_raw in &doc.depends_on {
        let trimmed = dep_raw.trim();
        let (ws_path, run_sel, reader_owned): (PathBuf, &str, Option<SqliteStateReader>) =
            if let Some((ws_alias, rest)) = trimmed.split_once('/') {
                let target_path = config
                    .linked_workspaces
                    .get(ws_alias)
                    .or_else(|| spec_repos.get(ws_alias))
                    .ok_or_else(|| {
                        StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!("spec depends_on '{trimmed}' references unknown workspace '{ws_alias}'"),
                        )
                    })?;
                let canon = canonical_root(target_path).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("linked workspace '{ws_alias}' invalid: {err}"),
                    )
                })?;
                let r = SqliteStateReader::new(canon.join(".acdd/state.sqlite"));
                (canon, rest, Some(r))
            } else {
                (primary.clone(), trimmed, None)
            };

        let reader: &dyn StateReader = match reader_owned {
            Some(ref r) => r,
            None => local_reader,
        };

        let (run_sel, task_sel) = match run_sel.split_once(':') {
            Some((r, t)) => (r, Some(t)),
            None => (run_sel, None),
        };

        let tombs = list_all_tombstones(reader)?;
        let matching_tombs: Vec<&RunTombstoneRecord> = tombs
            .iter()
            .filter(|t| {
                t.run_id.as_str() == run_sel
                    || t.run_name.as_str() == run_sel
                    || t.spec_id.as_deref() == Some(run_sel)
            })
            .collect();
        if !matching_tombs.is_empty() {
            if let Some(completed) = matching_tombs
                .iter()
                .find(|t| t.terminal_status == TerminalStatus::Completed)
            {
                if let (Some(t_ref), Some(ref arch_rel)) = (task_sel, &completed.archive_path) {
                    if let Ok(raw) = read_confined_utf8(&ws_path, arch_rel) {
                        if let Ok(parsed) = parse_spec(&raw) {
                            if let Some(task) =
                                parsed.tasks.iter().find(|t| t.task_ref.as_str() == t_ref)
                            {
                                if task.status != SpecStatus::Completed {
                                    return Err(StateError::new(
                                        ErrorCode::GateVerificationFailed,
                                        format!("spec dependency '{trimmed}' task '{t_ref}' is not completed"),
                                    ));
                                }
                            }
                        }
                    }
                }
                continue;
            }
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "spec dependency '{trimmed}' was cleared (terminal_status='cleared') and cannot satisfy depends_on"
                ),
            ));
        }

        // Fallback: committed archive file matching `doc.id == run_sel`.
        let mut archived_ok = false;
        for col in effective_spec_collections(config).values() {
            let Ok(arch_dir) = checked_child(&ws_path, &Path::new(&col.path).join("archive")) else {
                continue;
            };
            for p in sorted_dir_paths(&arch_dir) {
                if p.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if let Ok(rel) = p.strip_prefix(&ws_path) {
                    if let Ok(raw) = read_confined_utf8(&ws_path, &rel.to_string_lossy()) {
                        if let Ok(parsed) = parse_spec(&raw) {
                            if parsed.id == run_sel
                                && parsed.status == SpecStatus::Completed
                                && parsed
                                    .closure
                                    .as_ref()
                                    .is_some_and(|c| c.status == SpecStatus::Completed)
                            {
                                archived_ok = true;
                                break;
                            }
                        }
                    }
                }
            }
            if archived_ok {
                break;
            }
        }

        if !archived_ok {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("spec dependency '{trimmed}' is not completed"),
            ));
        }
    }

    Ok(())
}

/// Build the current process session fingerprint `host_id:pid:start_time`.
pub fn current_session_fingerprint() -> String {
    let host = current_host_id();
    let pid = std::process::id();
    let start_time = read_process_start_time(pid).unwrap_or_else(|| "0".to_owned());
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

fn read_process_start_time(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rparen = stat.rfind(')')?;
        let after = stat.get(rparen + 2..)?;
        // Field 22 overall is index 19 after `)` (field 3 is state at index 0).
        let fields: Vec<&str> = after.split_whitespace().collect();
        fields.get(19).map(|s| (*s).to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
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

    #[cfg(target_os = "linux")]
    {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return Some(false);
        }
        if let Some(current_start) = read_process_start_time(pid) {
            if recorded_start != "0" && current_start != recorded_start {
                return Some(false);
            }
            return Some(true);
        }
        Some(false)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let _ = recorded_start;
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
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

/// Resume and finalize any `run_tombstones` rows with `cleanup_pending == true`.
pub fn resume_pending_tombstone_cleanups<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    layout: Option<&RepoWorktreeLayout>,
) -> Result<Vec<String>, StateError> {
    resume_pending_tombstone_cleanups_dyn(config, backend, layout)
}

pub(crate) fn resume_pending_tombstone_cleanups_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    _layout: Option<&RepoWorktreeLayout>,
) -> Result<Vec<String>, StateError> {
    let tombs = list_all_tombstones(backend)?;
    let primary = primary_workspace_root(config);
    let mut cleaned = Vec::new();

    for tomb in tombs {
        if !tomb.cleanup_pending {
            continue;
        }
        let mut workspaces = BTreeSet::new();
        workspaces.insert(primary.clone());
        for ws in config.linked_workspaces.values() {
            workspaces.insert(canonicalize_macos_var(ws));
        }
        for entry in &tomb.cleanup_manifest {
            if let Some(ref ws) = entry.workspace {
                workspaces.insert(canonicalize_macos_var(Path::new(ws)));
            }
            match entry.kind.as_str() {
                "spec_file" => {
                    if let Ok(target) = checked_child(&primary, Path::new(&entry.path_or_ref)) {
                        if target.exists() {
                            let _ = fs::remove_file(&target);
                            cleaned.push(entry.path_or_ref.clone());
                        }
                    }
                }
                "worktree" => {
                    let _ = remove_worktree_and_branch(
                        &primary,
                        Some(Path::new(&entry.path_or_ref)),
                        None,
                        true,
                    );
                    cleaned.push(entry.path_or_ref.clone());
                }
                "branch" => {
                    let _ = remove_worktree_and_branch(
                        &primary,
                        None,
                        Some(&entry.path_or_ref),
                        true,
                    );
                    cleaned.push(entry.path_or_ref.clone());
                }
                _ => {}
            }
        }
        let ws_vec: Vec<PathBuf> = workspaces.into_iter().collect();
        if let Ok(refs) = cleanup_run_refs(&ws_vec, &tomb.run_id) {
            cleaned.extend(refs);
        }
        let _ = prune_old_run_trace_dirs(&primary, backend, config.retain_traces);
        exec_tx_with(backend, |tx| tx.mark_tombstone_cleaned(&tomb.run_id))?;
    }

    Ok(cleaned)
}

/// Remove closed `.acdd/run/<run_id>/` directories beyond `retain_traces`, preserving active runs.
pub fn prune_old_run_trace_dirs(
    primary_worktree: &Path,
    reader: &dyn StateReader,
    retain_traces: usize,
) -> Result<(), StateError> {
    let run_dir = primary_worktree.join(".acdd/run");
    if !run_dir.is_dir() {
        return Ok(());
    }
    let active_ids: BTreeSet<String> = list_all_runs(reader, Some(RunStatus::Active))?
        .into_iter()
        .map(|r| r.run_id.as_str().to_owned())
        .collect();

    let mut closed_dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let Ok(entries) = fs::read_dir(&run_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if active_ids.contains(name) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        closed_dirs.push((mtime, p));
    }

    closed_dirs.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, old_dir) in closed_dirs.into_iter().skip(retain_traces) {
        let _ = fs::remove_dir_all(old_dir);
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

/// Build the `ValidatedObservations` structure for pure state machine evaluation.
pub fn empty_observations() -> ValidatedObservations {
    ValidatedObservations {
        now_utc: now_utc_iso(),
        ..ValidatedObservations::default()
    }
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
