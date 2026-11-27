//! Pure `reopen` planning: counter resets, `claim_revision` increment, downstream gate reset,
//! `satisfied = false`, `DependentTaskCompleted` guard, and `--cascade` stale propagation.

use crate::machine::graph::{
    active_claim_for_node, build_preconditions, find_node, is_post_task_root_node,
    is_pre_task_root_node,
};
use crate::machine::inputs::ReopenInput;
use crate::types::{
    ClaimOutcome, ClaimOutcomePatch, DependencySatisfiedPatch, GateNodeRecord, NodeKind,
    NodeStatus, NodeStatusPatch, RunGraphSnapshot, TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ids::TaskRef;
use std::collections::{BTreeSet, VecDeque};

fn close_active_claim_at(
    snapshot: &RunGraphSnapshot,
    now_utc: &str,
    node_id: &str,
    claims: &mut Vec<ClaimOutcomePatch>,
) {
    if let Some(active) = active_claim_for_node(snapshot, node_id) {
        claims.push(ClaimOutcomePatch {
            node_id: node_id.to_owned(),
            claim_revision: active.claim_revision,
            outcome: ClaimOutcome::Reopened,
            ended_at: now_utc.to_owned(),
        });
    }
}

fn make_reset_patch(
    n: &GateNodeRecord,
    status: NodeStatus,
    bump_claim_rev: bool,
) -> NodeStatusPatch {
    let mut p = crate::machine::clear_claim_patch(&n.node_id, status, true);
    p.attempt = Some(0);
    p.cycle_count = Some(0);
    if bump_claim_rev {
        p.claim_revision = Some(n.claim_revision + 1);
    }
    p
}

fn reset_container_children<'a>(
    snapshot: &'a RunGraphSnapshot,
    now_utc: &str,
    container_id: &str,
    watch_ids: &mut BTreeSet<&'a str>,
    claim_patches: &mut Vec<ClaimOutcomePatch>,
    patches_by_id: &mut Vec<NodeStatusPatch>,
) {
    let mut children: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.parent_id.as_deref() == Some(container_id))
        .collect();
    children.sort_by_key(|n| n.seq_order);

    for (idx, child) in children.into_iter().enumerate() {
        watch_ids.insert(child.node_id.as_str());
        close_active_claim_at(snapshot, now_utc, &child.node_id, claim_patches);
        let child_status = if idx == 0 {
            NodeStatus::Ready
        } else {
            NodeStatus::Pending
        };
        patches_by_id.push(make_reset_patch(child, child_status, idx == 0));
    }
}

fn reopen_completed_task_dependents<'a>(
    snapshot: &'a RunGraphSnapshot,
    now_utc: &str,
    task_node: &'a GateNodeRecord,
    cascade: bool,
    watch_ids: &mut BTreeSet<&'a str>,
    transition: &mut TransitionPlan,
) -> Result<(), AcddError> {
    if task_node.status != NodeStatus::Completed {
        return Ok(());
    }
    let Some(ref task_ref) = task_node.task_ref else {
        return Ok(());
    };
    transition
        .dependency_patches
        .push(DependencySatisfiedPatch {
            depends_on_ref: task_ref.clone(),
            dependent_task_ref: None,
            satisfied: false,
        });

    for dep_ref in collect_transitive_dependents(snapshot, task_ref) {
        let Some(dep_task) = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Task && n.task_ref.as_ref() == Some(&dep_ref))
        else {
            continue;
        };

        let has_active_or_done = matches!(
            dep_task.status,
            NodeStatus::InProgress | NodeStatus::Completed
        ) || snapshot.nodes.iter().any(|c| {
            c.parent_id.as_deref() == Some(dep_task.node_id.as_str())
                && matches!(c.status, NodeStatus::InProgress | NodeStatus::Completed)
        });

        if has_active_or_done && !cascade {
            return Err(AcddError::new(
                ErrorCode::DependentTaskCompleted,
                format!(
                    "cannot reopen completed task '{task_ref}' without --cascade because dependent task '{dep_ref}' is '{}'",
                    dep_task.status
                ),
            )
            .with_hint("re-run reopen with --cascade to mark dependent tasks stale"));
        }

        watch_ids.insert(dep_task.node_id.as_str());
        if has_active_or_done {
            if dep_task.status == NodeStatus::Completed {
                transition
                    .dependency_patches
                    .push(DependencySatisfiedPatch {
                        depends_on_ref: dep_ref,
                        dependent_task_ref: None,
                        satisfied: false,
                    });
            }
            close_active_claim_at(
                snapshot,
                now_utc,
                &dep_task.node_id,
                &mut transition.claim_patches,
            );
            let mut dp = NodeStatusPatch::status_only(&dep_task.node_id, NodeStatus::Stale);
            dp.accepted_tree = Some(None);
            transition.node_patches.push(dp);
            for child in snapshot
                .nodes
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(dep_task.node_id.as_str()))
            {
                watch_ids.insert(child.node_id.as_str());
                close_active_claim_at(
                    snapshot,
                    now_utc,
                    &child.node_id,
                    &mut transition.claim_patches,
                );
                let child_status =
                    if matches!(child.status, NodeStatus::InProgress | NodeStatus::Completed) {
                        NodeStatus::Stale
                    } else {
                        NodeStatus::Pending
                    };
                let mut cp = NodeStatusPatch::status_only(&child.node_id, child_status);
                cp.accepted_tree = Some(None);
                transition.node_patches.push(cp);
            }
        } else if dep_task.status == NodeStatus::Ready {
            transition.node_patches.push(NodeStatusPatch::status_only(
                &dep_task.node_id,
                NodeStatus::Pending,
            ));
            for child in snapshot
                .nodes
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(dep_task.node_id.as_str()))
                .filter(|c| c.status == NodeStatus::Ready)
            {
                watch_ids.insert(child.node_id.as_str());
                transition.node_patches.push(NodeStatusPatch::status_only(
                    &child.node_id,
                    NodeStatus::Pending,
                ));
            }
        }
    }
    Ok(())
}

fn reset_task_gate_reopen<'a>(
    snapshot: &'a RunGraphSnapshot,
    now_utc: &str,
    target: &'a GateNodeRecord,
    watch_ids: &mut BTreeSet<&'a str>,
    claim_patches: &mut Vec<ClaimOutcomePatch>,
    patches_by_id: &mut Vec<NodeStatusPatch>,
) -> Result<(), AcddError> {
    close_active_claim_at(snapshot, now_utc, &target.node_id, claim_patches);
    patches_by_id.push(make_reset_patch(target, NodeStatus::Ready, true));
    for sib in &snapshot.nodes {
        if sib.parent_id == target.parent_id && sib.seq_order > target.seq_order {
            watch_ids.insert(sib.node_id.as_str());
            close_active_claim_at(snapshot, now_utc, &sib.node_id, claim_patches);
            patches_by_id.push(make_reset_patch(sib, NodeStatus::Pending, false));
        }
    }
    let Some(ref parent_id) = target.parent_id else {
        return Ok(());
    };
    let parent_task = find_node(snapshot, parent_id)?;
    if parent_task.status == NodeStatus::InProgress || parent_task.status == NodeStatus::Ready {
        return Ok(());
    }
    watch_ids.insert(parent_task.node_id.as_str());
    let has_prior_completed = snapshot.nodes.iter().any(|sib| {
        sib.parent_id.as_deref() == Some(parent_id.as_str())
            && sib.seq_order < target.seq_order
            && sib.status == NodeStatus::Completed
    });
    let parent_status = if has_prior_completed {
        NodeStatus::InProgress
    } else {
        NodeStatus::Ready
    };
    let mut p = NodeStatusPatch::status_only(&parent_task.node_id, parent_status);
    p.accepted_tree = Some(None);
    patches_by_id.push(p);
    Ok(())
}

fn reset_root_gate_reopen<'a>(
    snapshot: &'a RunGraphSnapshot,
    now_utc: &str,
    target: &'a GateNodeRecord,
    watch_ids: &mut BTreeSet<&'a str>,
    claim_patches: &mut Vec<ClaimOutcomePatch>,
    patches_by_id: &mut Vec<NodeStatusPatch>,
) -> Result<(), AcddError> {
    close_active_claim_at(snapshot, now_utc, &target.node_id, claim_patches);
    patches_by_id.push(make_reset_patch(target, NodeStatus::Ready, true));
    for sib in &snapshot.nodes {
        if sib.parent_id == target.parent_id
            && sib.seq_order > target.seq_order
            && matches!(sib.kind, NodeKind::RootGate | NodeKind::Container)
            && !sib.node_id.starts_with("tasks#")
            && is_pre_task_root_node(snapshot, target) == is_pre_task_root_node(snapshot, sib)
        {
            watch_ids.insert(sib.node_id.as_str());
            close_active_claim_at(snapshot, now_utc, &sib.node_id, claim_patches);
            patches_by_id.push(make_reset_patch(sib, NodeStatus::Pending, false));
        }
    }
    if let Some(ref parent_id) = target.parent_id {
        let parent = find_node(snapshot, parent_id)?;
        if parent.status != NodeStatus::Ready {
            watch_ids.insert(parent.node_id.as_str());
            let mut p = NodeStatusPatch::status_only(&parent.node_id, NodeStatus::Ready);
            p.accepted_tree = Some(None);
            patches_by_id.push(p);
        }
    }
    Ok(())
}

/// Plan a pure `reopen <NodeCursor>` transition.
pub(crate) fn plan_reopen(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
    input: &ReopenInput,
) -> Result<TransitionPlan, AcddError> {
    let target = crate::machine::require_active_node(snapshot, &input.node_cursor, "reopen node")?;
    let target_id = target.node_id.as_str();

    if target.kind == NodeKind::Container && target.node_id.starts_with("tasks#") {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("cannot reopen structural tasks block '{target_id}' directly"),
        ));
    }

    let mut transition = TransitionPlan {
        preconditions: build_preconditions(snapshot, &[], None),
        bump_run_generation: true,
        new_run_status: None,
        new_root_cycle_count: None,
        completed_at: None,
        node_patches: Vec::new(),
        dependency_patches: Vec::new(),
        claim_patches: Vec::new(),
        new_artifacts: Vec::new(),
        new_blackboard: Vec::new(),
        blackboard_patches: Vec::new(),
        new_subtasks: Vec::new(),
        significant_transition: true,
    };
    let mut watch_ids: BTreeSet<&str> = BTreeSet::new();

    watch_ids.insert(target.node_id.as_str());

    match target.kind {
        NodeKind::Task => {
            close_active_claim_at(
                snapshot,
                &obs.now_utc,
                &target.node_id,
                &mut transition.claim_patches,
            );
            transition
                .node_patches
                .push(make_reset_patch(target, NodeStatus::Ready, true));
            reset_container_children(
                snapshot,
                &obs.now_utc,
                &target.node_id,
                &mut watch_ids,
                &mut transition.claim_patches,
                &mut transition.node_patches,
            );
        }
        NodeKind::TaskGate => reset_task_gate_reopen(
            snapshot,
            &obs.now_utc,
            target,
            &mut watch_ids,
            &mut transition.claim_patches,
            &mut transition.node_patches,
        )?,
        NodeKind::Container => {
            transition.new_root_cycle_count = Some(0);
            close_active_claim_at(
                snapshot,
                &obs.now_utc,
                &target.node_id,
                &mut transition.claim_patches,
            );
            transition
                .node_patches
                .push(make_reset_patch(target, NodeStatus::Ready, true));
            reset_container_children(
                snapshot,
                &obs.now_utc,
                &target.node_id,
                &mut watch_ids,
                &mut transition.claim_patches,
                &mut transition.node_patches,
            );
        }
        NodeKind::RootGate => {
            transition.new_root_cycle_count = Some(0);
            reset_root_gate_reopen(
                snapshot,
                &obs.now_utc,
                target,
                &mut watch_ids,
                &mut transition.claim_patches,
                &mut transition.node_patches,
            )?;
        }
    }

    // Handle completed task reopening: reset `satisfied = false` and check dependent tasks (`--cascade`).
    let reopened_task: Option<&GateNodeRecord> = match target.kind {
        NodeKind::Task => Some(target),
        NodeKind::TaskGate => target
            .parent_id
            .as_deref()
            .and_then(|pid| snapshot.nodes.iter().find(|n| n.node_id == pid)),
        _ => None,
    };

    if let Some(task_node) = reopened_task {
        reopen_completed_task_dependents(
            snapshot,
            &obs.now_utc,
            task_node,
            input.cascade,
            &mut watch_ids,
            &mut transition,
        )?;

        // Reopening any task (completed or active/pending) invalidates post-task root gates if they advanced.
        for post in &snapshot.nodes {
            if !is_post_task_root_node(snapshot, post) {
                continue;
            }
            match post.status {
                NodeStatus::Ready => {
                    watch_ids.insert(post.node_id.as_str());
                    transition.node_patches.push(NodeStatusPatch::status_only(
                        &post.node_id,
                        NodeStatus::Pending,
                    ));
                }
                NodeStatus::InProgress | NodeStatus::Completed => {
                    if !input.cascade {
                        return Err(AcddError::new(
                            ErrorCode::DependentTaskCompleted,
                            format!(
                                "cannot reopen task '{}' without --cascade because post-task root gate '{}' is '{}'",
                                task_node.node_id, post.node_id, post.status
                            ),
                        ));
                    }
                    watch_ids.insert(post.node_id.as_str());
                    close_active_claim_at(
                        snapshot,
                        &obs.now_utc,
                        &post.node_id,
                        &mut transition.claim_patches,
                    );
                    let mut p = NodeStatusPatch::status_only(&post.node_id, NodeStatus::Stale);
                    p.accepted_tree = Some(None);
                    transition.node_patches.push(p);
                }
                _ => {}
            }
        }
    }

    let watch_vec: Vec<&str> = watch_ids.into_iter().collect();
    transition.preconditions = build_preconditions(snapshot, &watch_vec, None);
    Ok(transition)
}

fn collect_transitive_dependents(snapshot: &RunGraphSnapshot, root_task: &TaskRef) -> Vec<TaskRef> {
    let mut visited: BTreeSet<&TaskRef> = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut order = Vec::new();
    queue.push_back(root_task);

    while let Some(cur) = queue.pop_front() {
        for dep in &snapshot.dependencies {
            if dep.depends_on_run_id == snapshot.run.run_id
                && dep.depends_on_ref == *cur
                && visited.insert(&dep.task_ref)
            {
                order.push(dep.task_ref.clone());
                queue.push_back(&dep.task_ref);
            }
        }
    }
    order
}
