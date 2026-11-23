//! Active run loading/resolution, tombstone cleanup manifests, run ID generation, and integration checks.

use super::cleanup::{build_tombstone_cleanup_manifest_with_policy, CleanupManifestInput};
use super::io::{list_all_runs, prune_old_run_trace_dirs};
use super::specs::{parse_profile_from_run, to_machine_profile};
use super::workspace::{build_path_policy, primary_workspace_root, resolve_repo_layout_optional};
use crate::git_ops::integrate::{verify_tasks_integrated, TaskIntegrationCheckInput};
use crate::git_ops::worktree::{
    canonicalize_macos_var, cleanup_run_refs, is_branch_merged_into_ref,
    remove_worktree_and_branch, RepoWorktreeLayout,
};
use crate::machine::{container_scope, MachineProfileInput};
use crate::profile::ResolvedGateProfile;
use crate::store::traits::{exec_tx_with, StateBackend, StateReader};
use crate::types::{
    ArchiveCleanupEntry, GateNodeRecord, IsolationMode, ListRunsReq, NodeKind, NodeStatus,
    RunGraphSnapshot, RunRecord, RunStatus, RunTombstoneRecord, TerminalStatus, WorktreeLifecycle,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::checked_child;
use acdd_core::ids::{RunId, TaskRef};
use acdd_core::path_policy::WorkspacePathPolicy;
use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Active run graph snapshot paired with its parsed resolved and machine profiles.
pub struct LoadedRunGraph {
    /// Loaded run record and full graph snapshot (`nodes`, `dependencies`, `subtasks`, `claims`, `traces`, `artifacts`).
    pub snapshot: RunGraphSnapshot,
    /// Parsed resolved gate profile from `runs.profile_json`.
    pub profile: ResolvedGateProfile,
    /// Pure state-machine representation of `profile`.
    pub machine_profile: MachineProfileInput,
}

impl LoadedRunGraph {
    /// Resolve a checkout-scoped [`WorkspacePathPolicy`] for `target_worktree` using
    /// the already-loaded run record.
    pub fn path_policy(
        &self,
        config: &StateEngineConfig,
        reader: &dyn StateReader,
        target_worktree: Option<&Path>,
    ) -> Result<WorkspacePathPolicy, StateError> {
        build_path_policy(config, reader, &[], Some(&self.snapshot.run), target_worktree)
    }
}

/// Resolve an active run, load its graph snapshot, and parse its resolved and machine profiles.
pub fn load_active_run_graph(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    run_selector: Option<&str>,
    worktree_override: Option<&Path>,
) -> Result<LoadedRunGraph, StateError> {
    let run = resolve_active_run(config, reader, run_selector, worktree_override)?;
    let snapshot = reader
        .load_run_graph(&run.run_id)?
        .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run graph missing"))?;
    let profile = parse_profile_from_run(&snapshot.run)?;
    let machine_profile = to_machine_profile(&profile)?;
    Ok(LoadedRunGraph {
        snapshot,
        profile,
        machine_profile,
    })
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

/// Return `true` only when `branch` is an ACDD-managed `run/<run_name>` branch for a `worktrees: managed` run.
pub(crate) fn is_managed_run_branch(run: &RunRecord, branch: &str) -> bool {
    parse_profile_from_run(run).is_ok_and(|p| p.git.worktrees == WorktreeLifecycle::Managed)
        && branch == format!("run/{}", run.run_name)
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

pub(in crate::usecases) fn cleanup_tombstone(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    tomb: &RunTombstoneRecord,
    locked: Option<&RepoWorktreeLayout>,
) -> Result<Vec<String>, StateError> {
    let primary = primary_workspace_root(config);
    let mut workspaces = config
        .linked_workspaces
        .values()
        .map(|ws| canonicalize_macos_var(ws))
        .collect::<BTreeSet<_>>();
    if locked.is_none() && resolve_repo_layout_optional(&primary)?.is_some() {
        workspaces.insert(primary.clone());
    }
    let mut cleaned = Vec::new();
    let mut all_ok = true;
    for entry in &tomb.cleanup_manifest {
        let path = Path::new(&entry.path_or_ref);
        let workspace = entry.workspace.as_deref().map(Path::new);
        if let Some(ws) = workspace
            .filter(|ws| !matches!(entry.kind.as_str(), "spec_file" | "worktree") && ws.exists())
        {
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
                let force = entry.force
                    || (tomb.terminal_status == TerminalStatus::Completed
                        && tomb.closed_commit.as_ref().is_some_and(|closed| {
                            is_branch_merged_into_ref(&primary, &entry.path_or_ref, closed.as_str())
                                .unwrap_or(false)
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

/// Compute `unintegrated_tasks` against `run_worktree` when `Worktree` isolation and all tasks are completed.
pub fn compute_unintegrated_tasks(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
) -> Result<Vec<TaskRef>, StateError> {
    if snapshot.run.isolation != IsolationMode::Worktree
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
    verify_tasks_integrated(&snapshot.run.run_id, &run_wt, &inputs)
}

/// Generate a fresh unique run identifier matching `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`.
pub fn generate_run_id(prefix: &str) -> Result<RunId, StateError> {
    let seq = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = Utc::now().format("%Y%m%d%H%M%S");
    let pid = std::process::id();
    let clean_prefix: String = prefix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(20)
        .collect();
    let head = if clean_prefix.is_empty() || !clean_prefix.as_bytes()[0].is_ascii_alphanumeric() {
        "run"
    } else {
        clean_prefix.as_str()
    };
    let raw = format!("{head}-{ts}-{pid}-{seq}");
    RunId::new(raw).map_err(StateError::from)
}

/// Collect `TaskIntegrationCheckInput` descriptors for all completed tasks in `snapshot`.
pub fn collect_integration_inputs(snapshot: &RunGraphSnapshot) -> Vec<TaskIntegrationCheckInput> {
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
        out.push(TaskIntegrationCheckInput {
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
