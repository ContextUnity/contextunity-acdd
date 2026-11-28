//! Pure claim admission, idempotent session re-claim, `--force` takeover rules,
//! `in_place` single-active enforcement, and non-overlapping scope checks.

use crate::machine::graph::{
    active_claim_for_node, build_preconditions, container_scope, find_node, is_post_task_root_node,
    is_pre_task_root_node,
};
use crate::machine::inputs::{ClaimDecision, ClaimInput, MachineProfileInput};
use crate::machine::schedule::{
    check_post_task_integration_barrier, check_task_scope_overlap, pre_task_barrier_satisfied,
    task_dependencies_satisfied,
};
use crate::types::{
    ClaimInsertInput, ClaimOutcome, ClaimOutcomePatch, IsolationMode, NodeKind, NodeStatus,
    NodeStatusPatch, RunGraphSnapshot, TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, AcddError};

fn build_claim_admission(
    node: &crate::types::GateNodeRecord,
    new_claim_rev: u32,
    input: &ClaimInput,
    claimed_at: &str,
) -> (ClaimInsertInput, NodeStatusPatch) {
    let claim_insert = ClaimInsertInput {
        node_id: node.node_id.clone(),
        claim_revision: new_claim_rev,
        worker_id: input.worker_id.clone(),
        session_fingerprint: input.session_fingerprint.clone(),
        worktree: input.worktree.clone(),
        branch: input.branch.clone(),
        baseline_commit: input.baseline_commit.clone(),
        span_id: input.span_id.clone(),
        claim_tree: input.claim_tree.clone(),
        claimed_at: claimed_at.to_owned(),
    };

    let mut patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::InProgress);
    patch.claim_revision = Some(new_claim_rev);
    patch.worker_id = Some(Some(input.worker_id.clone()));
    patch.session_fingerprint = Some(input.session_fingerprint.clone());
    patch.worktree = Some(Some(input.worktree.clone()));
    patch.branch = Some(input.branch.clone());
    patch.baseline_commit = Some(input.baseline_commit.clone());
    patch.claim_tree = Some(Some(input.claim_tree.clone()));
    patch.active_span_id = Some(Some(input.span_id.clone()));
    (claim_insert, patch)
}

fn admit_reclaim_in_progress(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
    input: &ClaimInput,
    node: &crate::types::GateNodeRecord,
) -> Result<ClaimDecision, AcddError> {
    let node_id = node.node_id.as_str();
    let active = active_claim_for_node(snapshot, node_id).ok_or_else(|| {
        AcddError::new(
            ErrorCode::BackendConflict,
            format!("node '{node_id}' is in_progress without an active gate_claims row"),
        )
    })?;

    if active.worker_id == input.worker_id
        && active.session_fingerprint == input.session_fingerprint
        && active.worktree == input.worktree
    {
        return Ok(ClaimDecision::Idempotent(active.clone()));
    }

    if !input.force {
        return Err(AcddError::new(
            ErrorCode::TaskAlreadyClaimed,
            format!(
                "node '{node_id}' is already claimed by worker '{}'",
                active.worker_id
            ),
        )
        .with_hint(
            "pass --force only to recover your own session, dead local PID, or with --steal-remote",
        ));
    }

    let same_worker = active.worker_id == input.worker_id;
    let dead_local_pid = input.previous_local_pid_alive == Some(false);
    if !same_worker && !dead_local_pid && !input.steal_remote {
        return Err(AcddError::new(
            ErrorCode::TaskAlreadyClaimed,
            format!(
                "refusing --force claim on '{node_id}' held by active worker '{}' without dead local PID or --steal-remote",
                active.worker_id
            ),
        ));
    }

    let new_claim_rev = node.claim_revision + 1;
    let mut watch_ids = vec![node.node_id.as_str()];
    if let Some(ref parent_id) = node.parent_id {
        watch_ids.push(parent_id.as_str());
    }
    let preconditions = build_preconditions(snapshot, &watch_ids, Some(node));

    let (claim_insert, patch) = build_claim_admission(node, new_claim_rev, input, &obs.now_utc);

    let mut node_patches = vec![patch];
    if node.kind == NodeKind::TaskGate {
        if let Some(parent_task) = node
            .parent_id
            .as_deref()
            .and_then(|parent_id| find_node(snapshot, parent_id).ok())
            .filter(|parent| {
                parent.worktree.as_deref() != Some(input.worktree.as_str())
                    || parent.branch != input.branch
            })
        {
            let mut parent_patch =
                NodeStatusPatch::status_only(&parent_task.node_id, parent_task.status);
            parent_patch.worktree = Some(Some(input.worktree.clone()));
            parent_patch.branch = Some(input.branch.clone());
            node_patches.push(parent_patch);
        }
    }

    let transition = TransitionPlan {
        preconditions,
        bump_run_generation: true,
        new_run_status: None,
        new_root_cycle_count: None,
        completed_at: None,
        node_patches,
        dependency_patches: Vec::new(),
        claim_patches: vec![ClaimOutcomePatch {
            node_id: node.node_id.clone(),
            claim_revision: active.claim_revision,
            outcome: ClaimOutcome::Superseded,
            ended_at: obs.now_utc.clone(),
        }],
        new_artifacts: Vec::new(),
        new_blackboard: Vec::new(),
        blackboard_patches: Vec::new(),
        new_subtasks: Vec::new(),
        significant_transition: true,
    };

    Ok(ClaimDecision::Admit {
        claim: claim_insert,
        transition: Box::new(transition),
    })
}

/// Evaluate a claim or re-claim request purely against `snapshot` and `obs`.
pub fn admit_claim(
    snapshot: &RunGraphSnapshot,
    _profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &ClaimInput,
) -> Result<ClaimDecision, AcddError> {
    let node =
        crate::machine::require_active_claimable_node(snapshot, &input.node_cursor, "claim node")?;
    let node_id = node.node_id.clone();

    if node.status == NodeStatus::InProgress {
        return admit_reclaim_in_progress(snapshot, obs, input, node);
    }

    if node.kind == NodeKind::TaskGate
        && matches!(node.status, NodeStatus::Pending | NodeStatus::Ready)
        && !pre_task_barrier_satisfied(snapshot)
    {
        let pending_cursor = snapshot
            .nodes
            .iter()
            .filter(|n| {
                is_pre_task_root_node(snapshot, n)
                    && !matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled)
            })
            .min_by_key(|n| {
                (
                    n.kind != NodeKind::RootGate,
                    !matches!(n.status, NodeStatus::Ready | NodeStatus::InProgress),
                    n.seq_order,
                )
            })
            .map(|n| {
                acdd_core::ids::NodeCursor::from_node_id(&n.node_id)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|_| n.node_id.clone())
            });
        let msg = match pending_cursor {
            Some(ref cursor) => format!(
                "Pre-task gate '{cursor}' must complete before claiming task gate '{node_id}'; claim '{cursor}' next"
            ),
            None => format!(
                "cannot claim task gate '{node_id}' before pre-task root gates complete"
            ),
        };
        let mut err = AcddError::new(ErrorCode::GateVerificationFailed, msg);
        if let Some(cursor) = pending_cursor {
            err = err.with_hint(format!("claim '{cursor}' next"));
        }
        return Err(err);
    }

    if node.status != NodeStatus::Ready {
        let code = match node.status {
            NodeStatus::Stale => ErrorCode::TaskCandidateMismatch,
            _ => ErrorCode::GateVerificationFailed,
        };
        return Err(AcddError::new(
            code,
            format!(
                "cannot claim node '{node_id}' in status '{}' (expected 'ready' or 'in_progress')",
                node.status
            ),
        ));
    }

    // Post-task root gate integration barrier check.
    if is_post_task_root_node(snapshot, node) {
        check_post_task_integration_barrier(snapshot, obs)?;
    }

    // In-place single-active constraint: at most one task/gate may be in_progress.
    if snapshot.run.isolation == IsolationMode::InPlace || node.isolation == IsolationMode::InPlace
    {
        if let Some(other) = snapshot.nodes.iter().find(|n| {
            n.node_id != node.node_id
                && Some(n.node_id.as_str()) != node.parent_id.as_deref()
                && n.kind != NodeKind::Container
                && n.status == NodeStatus::InProgress
        }) {
            return Err(AcddError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!(
                    "in_place isolation permits only one active node at a time ('{}' is currently in_progress)",
                    other.node_id
                ),
            ));
        }
    }

    let mut node_patches = Vec::new();
    let mut watch_ids = vec![node.node_id.as_str()];

    if node.kind == NodeKind::TaskGate {
        let parent_id = node.parent_id.as_deref().ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("task_gate '{node_id}' has no parent task container"),
            )
        })?;
        let parent_task = find_node(snapshot, parent_id)?;
        watch_ids.push(parent_task.node_id.as_str());

        let task_ref = parent_task.task_ref.as_ref().ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("parent node '{parent_id}' missing task_ref"),
            )
        })?;

        if !task_dependencies_satisfied(snapshot, task_ref, obs) {
            return Err(AcddError::new(
                ErrorCode::GateVerificationFailed,
                format!("task '{task_ref}' still has unsatisfied dependencies"),
            ));
        }

        // Non-overlapping scope check against already-claimed unfinished tasks.
        let my_scope = container_scope(snapshot, node);
        check_task_scope_overlap(
            snapshot,
            &parent_task.node_id,
            Some(input.worktree.as_str()),
            &my_scope,
            &format!("task '{task_ref}' scope overlaps"),
        )?;

        if parent_task.status == NodeStatus::Ready {
            let mut parent_patch =
                NodeStatusPatch::status_only(&parent_task.node_id, NodeStatus::InProgress);
            parent_patch.claim_revision = Some(parent_task.claim_revision.max(1));
            parent_patch.worktree = Some(Some(input.worktree.clone()));
            parent_patch.branch = Some(input.branch.clone());
            if parent_task.baseline_commit.is_none() {
                parent_patch.baseline_commit = Some(input.baseline_commit.clone());
            }
            if parent_task.claim_tree.is_none() {
                parent_patch.claim_tree = Some(Some(input.claim_tree.clone()));
            }
            node_patches.push(parent_patch);
        }
    }

    let new_claim_rev = node.claim_revision + 1;
    let preconditions = build_preconditions(snapshot, &watch_ids, Some(node));

    let (claim_insert, gate_patch) =
        build_claim_admission(node, new_claim_rev, input, &obs.now_utc);
    node_patches.push(gate_patch);

    let transition = TransitionPlan {
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
    };

    Ok(ClaimDecision::Admit {
        claim: claim_insert,
        transition: Box::new(transition),
    })
}
