//! Pure DAG readiness, pre/post-task barriers, `in_place` single-active constraints,
//! non-overlapping scope checks, and topological wave scheduling.

use crate::machine::graph::{container_scope, is_post_task_root_node, is_pre_task_root_node};
use crate::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::TaskRef;
pub use acdd_core::path_policy::{patterns_overlap, scopes_overlap};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Pure scheduling report produced for a `RunGraphSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleReport {
    /// Whether all pre-task root gates/containers are `Completed` or `Cancelled`.
    pub pre_task_satisfied: bool,
    /// Whether every non-cancelled task in the run has reached `Completed`.
    pub all_tasks_completed: bool,
    /// Reason post-task root gates cannot yet run (`TASK_NOT_INTEGRATED`), if any.
    pub post_task_blocked_reason: Option<ErrorCode>,
    /// Root gate node IDs currently ready to claim/execute.
    pub ready_root_nodes: Vec<String>,
    /// Task references immediately eligible to start without scope or dependency conflicts.
    pub ready_tasks: Vec<TaskRef>,
    /// Topological execution waves for remaining unfinished tasks.
    pub waves: Vec<Vec<TaskRef>>,
}

/// Return `true` if a task has already been claimed (`claim_revision >= 1` on task or any child gate)
/// and has not yet reached a terminal state (`Completed` or `Cancelled`).
pub fn is_task_claimed_unfinished(snapshot: &RunGraphSnapshot, task_node: &GateNodeRecord) -> bool {
    if matches!(task_node.status, NodeStatus::Completed | NodeStatus::Cancelled) {
        return false;
    }
    if task_node.claim_revision >= 1 || task_node.status == NodeStatus::InProgress {
        return true;
    }
    snapshot.nodes.iter().any(|n| {
        n.parent_id.as_deref() == Some(task_node.node_id.as_str())
            && (n.claim_revision >= 1 || n.status == NodeStatus::InProgress)
    })
}

/// Return `true` if any node in the run is currently `InProgress`.
pub fn has_any_in_progress_node(snapshot: &RunGraphSnapshot) -> bool {
    snapshot
        .nodes
        .iter()
        .any(|n| n.kind != NodeKind::Container && n.status == NodeStatus::InProgress)
        || snapshot.nodes.iter().any(|n| {
            n.kind == NodeKind::Task && n.status == NodeStatus::InProgress
        })
}

/// Return `true` if `dep` is satisfied given live in-run completion status and external observations.
///
/// For intra-run dependencies, the live in-run task status is authoritative so stale `dep.satisfied`
/// rows never make a downstream task claimable while its producer is non-`Completed`.
pub(crate) fn is_dep_satisfied_against_statuses(
    run_id: &acdd_core::ids::RunId,
    dep: &crate::types::TaskDependencyRecord,
    is_intra_run_completed: impl FnOnce(&TaskRef) -> bool,
    obs: &ValidatedObservations,
) -> bool {
    if &dep.depends_on_run_id == run_id {
        is_intra_run_completed(&dep.depends_on_ref)
    } else {
        dep.satisfied
            || obs.external_satisfied_deps.iter().any(|(ws, rid, tref)| {
                ws == &dep.depends_on_workspace
                    && rid == &dep.depends_on_run_id
                    && tref == &dep.depends_on_ref
            })
    }
}

/// Return `true` if all dependencies for `task_ref` are satisfied.
pub fn task_dependencies_satisfied(
    snapshot: &RunGraphSnapshot,
    task_ref: &TaskRef,
    obs: &ValidatedObservations,
) -> bool {
    let task_status_map: BTreeMap<&TaskRef, NodeStatus> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .filter_map(|n| n.task_ref.as_ref().map(|r| (r, n.status)))
        .collect();

    snapshot
        .dependencies
        .iter()
        .filter(|d| &d.task_ref == task_ref)
        .all(|dep| {
            is_dep_satisfied_against_statuses(
                &snapshot.run.run_id,
                dep,
                |ref_id| task_status_map.get(ref_id).copied() == Some(NodeStatus::Completed),
                obs,
            )
        })
}

/// Return `true` if all pre-task root gates/containers are `Completed` or `Cancelled`.
pub fn pre_task_barrier_satisfied(snapshot: &RunGraphSnapshot) -> bool {
    snapshot
        .nodes
        .iter()
        .filter(|n| is_pre_task_root_node(snapshot, n))
        .all(|n| matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled))
}

/// Return `true` if every non-cancelled task in the run has `status == Completed`.
pub fn all_non_cancelled_tasks_completed(snapshot: &RunGraphSnapshot) -> bool {
    snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.status != NodeStatus::Cancelled)
        .all(|n| n.status == NodeStatus::Completed)
}

/// Enforce the post-task integration completeness barrier (`TASK_NOT_INTEGRATED`).
pub fn check_post_task_integration_barrier(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> Result<(), StateError> {
    if !all_non_cancelled_tasks_completed(snapshot) {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            "cannot run post-task root gates before all non-cancelled tasks are completed",
        ));
    }
    if snapshot.run.isolation == IsolationMode::Worktree && !obs.unintegrated_tasks.is_empty() {
        let missing: Vec<&str> = obs.unintegrated_tasks.iter().map(TaskRef::as_str).collect();
        return Err(StateError::new(
            ErrorCode::TaskNotIntegrated,
            format!(
                "completed task deltas missing from run worktree: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Compute ready nodes, post-task barrier status, and topological execution waves.
pub fn compute_schedule(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> Result<ScheduleReport, StateError> {
    let pre_task_satisfied = pre_task_barrier_satisfied(snapshot);
    let all_tasks_completed = all_non_cancelled_tasks_completed(snapshot);
    let post_task_blocked_reason = if all_tasks_completed
        && snapshot.run.isolation == IsolationMode::Worktree
        && !obs.unintegrated_tasks.is_empty()
    {
        Some(ErrorCode::TaskNotIntegrated)
    } else {
        None
    };

    let any_active_in_place = snapshot.run.isolation == IsolationMode::InPlace
        && has_any_in_progress_node(snapshot);

    let mut ready_root_nodes = Vec::new();
    for node in &snapshot.nodes {
        if node.kind != NodeKind::RootGate {
            continue;
        }
        if is_post_task_root_node(snapshot, node)
            && (!all_tasks_completed || post_task_blocked_reason.is_some())
        {
            continue;
        }
        if node.status == NodeStatus::Ready && !any_active_in_place {
            ready_root_nodes.push(node.node_id.clone());
        }
    }

    let task_nodes: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .collect();

    let claimed_unfinished_scopes: Vec<(TaskRef, Vec<String>)> = task_nodes
        .iter()
        .filter(|n| is_task_claimed_unfinished(snapshot, n))
        .filter_map(|n| n.task_ref.clone().map(|tr| (tr, container_scope(snapshot, n))))
        .collect();

    let mut ready_tasks = Vec::new();
    if pre_task_satisfied && !any_active_in_place {
        for task_node in &task_nodes {
            if !matches!(task_node.status, NodeStatus::Ready | NodeStatus::Pending) {
                continue;
            }
            let Some(ref task_ref) = task_node.task_ref else {
                continue;
            };
            if !task_dependencies_satisfied(snapshot, task_ref, obs) {
                continue;
            }
            let my_scope = container_scope(snapshot, task_node);
            let conflicts_claimed = claimed_unfinished_scopes.iter().any(|(other_ref, other_scope)| {
                other_ref != task_ref && scopes_overlap(&my_scope, other_scope)
            });
            if !conflicts_claimed {
                ready_tasks.push(task_ref.clone());
            }
        }
    }

    let waves = compute_task_waves(snapshot, &task_nodes, obs, &claimed_unfinished_scopes)?;

    Ok(ScheduleReport {
        pre_task_satisfied,
        all_tasks_completed,
        post_task_blocked_reason,
        ready_root_nodes,
        ready_tasks,
        waves,
    })
}

fn compute_task_waves(
    snapshot: &RunGraphSnapshot,
    task_nodes: &[&GateNodeRecord],
    obs: &ValidatedObservations,
    claimed_unfinished_scopes: &[(TaskRef, Vec<String>)],
) -> Result<Vec<Vec<TaskRef>>, StateError> {
    let mut remaining: BTreeMap<TaskRef, &GateNodeRecord> = BTreeMap::new();
    let mut completed: BTreeSet<TaskRef> = BTreeSet::new();

    for &node in task_nodes {
        let Some(ref tref) = node.task_ref else {
            continue;
        };
        match node.status {
            NodeStatus::Completed => {
                completed.insert(tref.clone());
            }
            NodeStatus::Cancelled => {}
            _ => {
                remaining.insert(tref.clone(), node);
            }
        }
    }

    let mut scope_map: BTreeMap<TaskRef, Vec<String>> = BTreeMap::new();
    for (tref, node) in &remaining {
        scope_map.insert(tref.clone(), container_scope(snapshot, node));
    }

    let mut waves: Vec<Vec<TaskRef>> = Vec::new();
    let mut wave_index = 0usize;

    while !remaining.is_empty() {
        let mut eligible: Vec<TaskRef> = Vec::new();
        for tref in remaining.keys() {
            let deps_ok = snapshot
                .dependencies
                .iter()
                .filter(|d| &d.task_ref == tref)
                .all(|dep| {
                    is_dep_satisfied_against_statuses(
                        &snapshot.run.run_id,
                        dep,
                        |ref_id| completed.contains(ref_id),
                        obs,
                    )
                });
            if deps_ok {
                eligible.push(tref.clone());
            }
        }

        if eligible.is_empty() {
            let blocked_on_external = remaining.keys().any(|tref| {
                snapshot.dependencies.iter().any(|dep| {
                    &dep.task_ref == tref
                        && dep.depends_on_run_id != snapshot.run.run_id
                        && !is_dep_satisfied_against_statuses(
                            &snapshot.run.run_id,
                            dep,
                            |ref_id| completed.contains(ref_id),
                            obs,
                        )
                })
            });
            if blocked_on_external {
                break;
            }
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "remaining tasks have unsatisfied or cyclic dependencies",
            ));
        }

        // Prioritize already-claimed unfinished tasks in the first wave so scope barriers
        // do not starve active work.
        eligible.sort_by_key(|tref| {
            let is_claimed = claimed_unfinished_scopes
                .iter()
                .any(|(ct, _)| ct == tref);
            (!is_claimed, tref.clone())
        });

        let mut wave: Vec<TaskRef> = Vec::new();
        let mut wave_scopes: Vec<Vec<String>> = Vec::new();

        for tref in eligible {
            if snapshot.run.isolation == IsolationMode::InPlace && !wave.is_empty() {
                break;
            }
            let my_scope = scope_map.get(&tref).cloned().unwrap_or_default();
            if wave_index == 0 {
                let blocked_by_claimed = claimed_unfinished_scopes
                    .iter()
                    .any(|(ct, cscope)| ct != &tref && scopes_overlap(&my_scope, cscope));
                if blocked_by_claimed {
                    continue;
                }
            }
            let conflicts_wave = wave_scopes
                .iter()
                .any(|ws| scopes_overlap(&my_scope, ws));
            if !conflicts_wave {
                wave.push(tref);
                wave_scopes.push(my_scope);
            }
        }

        if wave.is_empty() {
            // If all eligible tasks in wave 0 were blocked by already-claimed unfinished tasks,
            // fall back to scheduling the claimed unfinished tasks first, or break tie on wave >= 1.
            return Err(StateError::new(
                ErrorCode::TaskScopeConflict,
                "eligible tasks are all blocked by overlapping active task scopes",
            ));
        }

        for tref in &wave {
            remaining.remove(tref);
            completed.insert(tref.clone());
        }
        waves.push(wave);
        wave_index += 1;
    }

    Ok(waves)
}
