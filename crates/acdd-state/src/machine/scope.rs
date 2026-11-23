//! Pure `extend_scope` planning, `is_agent_writable` / `protected_paths` validation,
//! and `TASK_SCOPE_CONFLICT` checks against already-claimed unfinished tasks.

use crate::machine::graph::{build_preconditions, container_scope, find_node};
use crate::machine::inputs::{ExtendScopeInput, MachineProfileInput};
use crate::machine::schedule::{is_task_claimed_unfinished, scopes_overlap};
use crate::types::{
    ExtendedScopeEntry, NodeKind, NodeStatus, NodeStatusPatch, RunGraphSnapshot, TransitionPlan,
    ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::path_policy::{is_valid_scope_glob, normalize_rel_path};

/// Plan a pure `extend_scope` update honoring shell `ValidatedObservations` (e.g. custom DB path policy).
pub(crate) fn plan_extend_scope_with_obs(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &ExtendScopeInput,
) -> Result<TransitionPlan, StateError> {
    let target_node = crate::machine::require_active_node(
        snapshot,
        &input.node_cursor,
        "extend_scope",
    )?;
    let target_id = target_node.node_id.as_str();

    if input.reason.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "extend_scope requires a non-empty reason",
        ));
    }
    if input.paths.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "extend_scope requires at least one path or glob pattern",
        ));
    }

    if let Some(ref worker) = input.worker_id {
        if target_node.kind == NodeKind::TaskGate
            && target_node.status == NodeStatus::InProgress
            && target_node.worker_id.as_ref() != Some(worker)
        {
            return Err(StateError::new(
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
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task_gate '{target_id}' has no parent task container"),
                )
            })?;
            find_node(snapshot, parent_id)?
        }
        _ => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "extend_scope requires a task or task_gate cursor (got '{target_id}')"
                ),
            ));
        }
    };

    if matches!(task_node.status, NodeStatus::Completed | NodeStatus::Cancelled) {
        return Err(StateError::new(
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
            return Err(StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope rejected invalid scope path or glob '{raw_path}'"),
            ));
        }
        let norm = normalize_rel_path(raw_path).ok_or_else(|| {
            StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope rejected invalid relative path '{raw_path}'"),
            )
        })?;
        if !policy.is_candidate_included(&norm) {
            return Err(StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope path '{norm}' is excluded from candidate/agent writes"),
            ));
        }
        if acdd_core::path_policy::scopes_overlap_case_insensitive(
            std::slice::from_ref(&norm),
            &profile.protected_paths,
        ) {
            return Err(StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!("extend_scope path '{norm}' overlaps protected_paths"),
            ));
        }

        // Check conflict against other already-claimed unfinished tasks ONLY.
        for other in &snapshot.nodes {
            if other.kind != NodeKind::Task || other.node_id == task_node.node_id {
                continue;
            }
            if is_task_claimed_unfinished(snapshot, other) {
                let other_scope = container_scope(snapshot, other);
                if scopes_overlap(std::slice::from_ref(&norm), &other_scope) {
                    return Err(StateError::new(
                        ErrorCode::TaskScopeConflict,
                        format!(
                            "extend_scope path '{norm}' conflicts with already-claimed unfinished task '{}'",
                            other.node_id
                        ),
                    ));
                }
            }
        }
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
    let mut watch_ids = vec![primary.node_id.as_str()];
    if task_node.node_id != primary.node_id {
        watch_ids.push(task_node.node_id.as_str());
    }
    let mut node_patches = Vec::new();

    let mut task_patch = NodeStatusPatch::status_only(&task_node.node_id, task_node.status);
    task_patch.contract_revision = Some(next_contract_rev);
    task_patch.extended_scope = Some(new_extended.clone());
    node_patches.push(task_patch);

    for child in &snapshot.nodes {
        if child.parent_id.as_deref() == Some(task_node.node_id.as_str()) {
            if child.node_id != primary.node_id {
                watch_ids.push(child.node_id.as_str());
            }
            let mut child_patch = NodeStatusPatch::status_only(&child.node_id, child.status);
            child_patch.contract_revision = Some(next_contract_rev);
            child_patch.extended_scope = Some(new_extended.clone());
            node_patches.push(child_patch);
        }
    }

    let preconditions = build_preconditions(snapshot, &watch_ids, Some(primary));

    Ok(TransitionPlan {
        preconditions,
        bump_run_generation: true,
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
        significant_transition: true,
    })
}
