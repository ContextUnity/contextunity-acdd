//! Pure `extend_scope` planning, `is_agent_writable` / `protected_paths` validation,
//! and `TASK_SCOPE_CONFLICT` checks against already-claimed unfinished tasks.

use crate::machine::graph::{build_preconditions, container_scope, find_node};
use crate::machine::inputs::{ExtendScopeInput, MachineProfileInput};
use crate::machine::schedule::check_task_scope_overlap;
use crate::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, NodeStatusPatch, RunGraphSnapshot,
    TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::path_policy::{is_valid_scope_glob, normalize_rel_path};
use acdd_spec::ExtendedScopeEntry;

/// Scope enforcement mode for a gate or task node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMode {
    /// Protected-paths-only write barrier; non-protected changed paths auto-admit into `extended_scope`.
    Lazy,
    /// Strict admitted-scope enforcement (`scope ∪ extended_scope`).
    Strict,
}

/// Resolve the canonical task container for `node` (`Task` itself or parent of `TaskGate`).
fn resolve_task_container<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &'a GateNodeRecord,
) -> Option<&'a GateNodeRecord> {
    match node.kind {
        NodeKind::Task => Some(node),
        NodeKind::TaskGate => node
            .parent_id
            .as_deref()
            .and_then(|pid| find_node(snapshot, pid).ok()),
        _ => None,
    }
}

/// Resolve the concrete bound canonical worktree string for a task container from `snapshot`.
///
/// Prefers `task_node.worktree`, then any child gate's `worktree`, then any active claim's
/// `worktree` for the task or its child gates. Returns `None` when unresolved or blank.
pub(crate) fn task_bound_worktree<'a>(
    snapshot: &'a RunGraphSnapshot,
    task_node: &'a GateNodeRecord,
) -> Option<&'a str> {
    let non_empty = |s: &'a str| {
        let trimmed = s.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    };
    if let Some(wt) = task_node.worktree.as_deref().and_then(non_empty) {
        return Some(wt);
    }
    if let Some(wt) = snapshot
        .nodes
        .iter()
        .filter(|n| n.parent_id.as_deref() == Some(task_node.node_id.as_str()))
        .find_map(|n| n.worktree.as_deref().and_then(non_empty))
    {
        return Some(wt);
    }
    snapshot
        .claims
        .iter()
        .filter(|c| {
            c.outcome == crate::types::ClaimOutcome::Active
                && (c.node_id == task_node.node_id
                    || snapshot.nodes.iter().any(|n| {
                        n.node_id == c.node_id
                            && n.parent_id.as_deref() == Some(task_node.node_id.as_str())
                    }))
        })
        .find_map(|c| non_empty(&c.worktree))
}

/// Return `true` iff both tasks are `Worktree`-isolated and resolve to concrete, distinct
/// canonical worktree paths.
pub(crate) fn tasks_have_distinct_worktrees(
    snapshot: &RunGraphSnapshot,
    task_a: &GateNodeRecord,
    task_a_worktree_override: Option<&str>,
    task_b: &GateNodeRecord,
) -> bool {
    if snapshot.run.isolation != IsolationMode::Worktree
        || task_a.isolation != IsolationMode::Worktree
        || task_b.isolation != IsolationMode::Worktree
    {
        return false;
    }
    let wt_a = task_a_worktree_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| task_bound_worktree(snapshot, task_a));
    let wt_b = task_bound_worktree(snapshot, task_b);
    match (wt_a, wt_b) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// Canonical scope-mode resolver shared by `verify`, `schedule`, `commit`, and `budget`.
///
/// Worktree-isolated task builds/gates use [`ScopeMode::Lazy`] so `protected_paths` and
/// `WorkspacePathPolicy::is_candidate_included` are the sole hard write barriers, while
/// `IsolationMode::InPlace` runs, narrow root gates (`polish`, `memory_synthesis`,
/// `documentation`), and tasks sharing a canonical worktree with another claimed unfinished
/// task stay strictly scoped (`ScopeMode::Strict`).
pub fn effective_scope_mode(snapshot: &RunGraphSnapshot, node: &GateNodeRecord) -> ScopeMode {
    let is_lazy_task_node = node.kind == NodeKind::Task
        || (node.kind == NodeKind::TaskGate && node.node_id.ends_with(":build"));
    if snapshot.run.isolation != IsolationMode::Worktree
        || node.isolation != IsolationMode::Worktree
        || !is_lazy_task_node
    {
        return ScopeMode::Strict;
    }
    let Some(task_node) = resolve_task_container(snapshot, node) else {
        return ScopeMode::Strict;
    };
    if let Some(my_wt) = task_bound_worktree(snapshot, task_node) {
        let shares_worktree = snapshot.nodes.iter().any(|other| {
            other.kind == NodeKind::Task
                && other.node_id != task_node.node_id
                && crate::machine::schedule::is_task_claimed_unfinished(snapshot, other)
                && task_bound_worktree(snapshot, other) == Some(my_wt)
        });
        if shares_worktree {
            return ScopeMode::Strict;
        }
    }
    ScopeMode::Lazy
}

/// Shared transition-plan builder for both automatic (`ScopeMode::Lazy`) and manual `extend_scope`.
fn build_extended_scope_plan(
    snapshot: &RunGraphSnapshot,
    task_node: &GateNodeRecord,
    primary: &GateNodeRecord,
    new_extended: Vec<ExtendedScopeEntry>,
    next_contract_rev: Option<u32>,
    significant: bool,
) -> TransitionPlan {
    let mut watch_ids = vec![primary.node_id.as_str()];
    if task_node.node_id != primary.node_id {
        watch_ids.push(task_node.node_id.as_str());
    }
    let mut node_patches = Vec::new();
    let mut task_patch = NodeStatusPatch::status_only(&task_node.node_id, task_node.status);
    task_patch.contract_revision = next_contract_rev;
    task_patch.extended_scope = Some(new_extended.clone());
    node_patches.push(task_patch);

    for child in &snapshot.nodes {
        if child.parent_id.as_deref() == Some(task_node.node_id.as_str()) {
            if child.node_id != primary.node_id {
                watch_ids.push(child.node_id.as_str());
            }
            let mut child_patch = NodeStatusPatch::status_only(&child.node_id, child.status);
            child_patch.contract_revision = next_contract_rev;
            child_patch.extended_scope = Some(new_extended.clone());
            node_patches.push(child_patch);
        }
    }

    let preconditions = build_preconditions(snapshot, &watch_ids, Some(primary));
    TransitionPlan {
        preconditions,
        bump_run_generation: significant,
        new_run_status: None,
        new_root_cycle_count: None,
        completed_at: None,
        node_patches,
        dependency_patches: Vec::new(),
        claim_patches: Vec::new(),
        new_artifacts: Vec::new(),
        new_blackboard: Vec::new(),
        blackboard_patches: Vec::new(),
        new_subtasks: Vec::new(),
        significant_transition: significant,
    }
}

/// Reason tag recorded on `extended_scope` entries auto-admitted under [`ScopeMode::Lazy`].
const AUTO_LAZY_SCOPE_REASON: &str = "auto:lazy_scope";

/// Plan a pure `extended_scope` auto-admission for non-protected `changed_paths` under
/// [`ScopeMode::Lazy`] without bumping `contract_revision` or invalidating the active claim.
pub(crate) fn plan_auto_admit_lazy_scope(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    node: &GateNodeRecord,
    changed_paths: &[String],
) -> Result<Option<TransitionPlan>, AcddError> {
    if effective_scope_mode(snapshot, node) != ScopeMode::Lazy || changed_paths.is_empty() {
        return Ok(None);
    }
    let task_node = match node.kind {
        NodeKind::Task => node,
        NodeKind::TaskGate => {
            let parent_id = node.parent_id.as_deref().ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("task_gate '{}' has no parent task container", node.node_id),
                )
            })?;
            find_node(snapshot, parent_id)?
        }
        _ => return Ok(None),
    };
    let policy = crate::machine::verify::effective_path_policy(snapshot, obs);
    let admitted_scope = container_scope(snapshot, node);
    let mut new_extended = task_node.extended_scope.clone();
    let mut added = false;
    for raw_path in changed_paths {
        let Some(norm) = normalize_rel_path(raw_path) else {
            continue;
        };
        if !policy.is_candidate_included(&norm)
            || policy.is_protected_path(&norm, &profile.protected_paths)
            || policy.is_scope_admitted(&norm, &admitted_scope)
        {
            continue;
        }
        if !new_extended.iter().any(|e| e.path == norm)
            && !task_node.scope.iter().any(|s| s == &norm)
        {
            new_extended.push(ExtendedScopeEntry {
                path: norm,
                reason: AUTO_LAZY_SCOPE_REASON.to_owned(),
            });
            added = true;
        }
    }
    if !added {
        return Ok(None);
    }

    Ok(Some(build_extended_scope_plan(
        snapshot,
        task_node,
        node,
        new_extended,
        None,
        false,
    )))
}

/// Plan a pure `extend_scope` update honoring shell `ValidatedObservations` (e.g. custom DB path policy).
pub(crate) fn plan_extend_scope_with_obs(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &ExtendScopeInput,
) -> Result<TransitionPlan, AcddError> {
    let target_node =
        crate::machine::require_active_node(snapshot, &input.node_cursor, "extend_scope")?;
    let target_id = target_node.node_id.as_str();

    if input.reason.trim().is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "extend_scope requires a non-empty reason",
        ));
    }
    if input.paths.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "extend_scope requires at least one path or glob pattern",
        ));
    }

    if let Some(ref worker) = input.worker_id {
        if matches!(target_node.kind, NodeKind::TaskGate | NodeKind::RootGate)
            && target_node.status == NodeStatus::InProgress
            && target_node.worker_id.as_ref() != Some(worker)
        {
            return Err(AcddError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!(
                    "worker '{worker}' does not hold the active claim on '{}'",
                    target_node.node_id
                ),
            ));
        }
    }

    let task_node = match target_node.kind {
        NodeKind::Task => target_node,
        NodeKind::TaskGate => {
            let parent_id = target_node.parent_id.as_deref().ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("task_gate '{target_id}' has no parent task container"),
                )
            })?;
            find_node(snapshot, parent_id)?
        }
        NodeKind::RootGate if target_node.node_id == "root:polish" => target_node,
        _ => {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "extend_scope requires a task, task_gate, or root:polish cursor (got '{target_id}')"
                ),
            ));
        }
    };

    if matches!(
        task_node.status,
        NodeStatus::Completed | NodeStatus::Cancelled
    ) {
        return Err(AcddError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "cannot extend_scope on terminal task '{}' (status: {})",
                task_node.node_id, task_node.status
            ),
        ));
    }

    let policy = crate::machine::verify::effective_path_policy(snapshot, obs);

    let mut normalized_paths = Vec::with_capacity(input.paths.len());
    for raw_path in &input.paths {
        if !is_valid_scope_glob(raw_path) {
            return Err(AcddError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope rejected invalid scope path or glob '{raw_path}'"),
            ));
        }
        let norm = normalize_rel_path(raw_path).ok_or_else(|| {
            AcddError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope rejected invalid relative path '{raw_path}'"),
            )
        })?;
        if !policy.is_candidate_included(&norm) {
            return Err(AcddError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope path '{norm}' is excluded from candidate/agent writes"),
            ));
        }
        if acdd_core::path_policy::scopes_overlap_case_insensitive(
            std::slice::from_ref(&norm),
            &profile.protected_paths,
        ) {
            return Err(AcddError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope path '{norm}' overlaps protected_paths"),
            ));
        }

        // Check conflict against other already-claimed unfinished tasks ONLY.
        check_task_scope_overlap(
            snapshot,
            &task_node.node_id,
            None,
            std::slice::from_ref(&norm),
            &format!("extend_scope path '{norm}' conflicts with"),
        )?;
        normalized_paths.push(norm);
    }

    let mut new_extended = task_node.extended_scope.clone();
    let trimmed_reason = input.reason.trim().to_owned();
    for path in normalized_paths {
        if !new_extended.iter().any(|e| e.path == path)
            && !task_node.scope.iter().any(|s| s == &path)
        {
            new_extended.push(ExtendedScopeEntry {
                path,
                reason: trimmed_reason.clone(),
            });
        }
    }

    let next_contract_rev = task_node.contract_revision + 1;
    let primary = if target_node.kind == NodeKind::TaskGate {
        target_node
    } else {
        task_node
    };
    Ok(build_extended_scope_plan(
        snapshot,
        task_node,
        primary,
        new_extended,
        Some(next_contract_rev),
        true,
    ))
}
