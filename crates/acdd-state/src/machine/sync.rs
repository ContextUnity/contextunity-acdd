//! Pure `sync` and `sync --rebind-profile` planning without resetting completed nodes.

use crate::machine::graph::{
    active_claim_for_node, append_task_nodes, build_preconditions, effective_task_tags,
    is_pre_task_root_node, matches_when, resolve_gate_def, validate_tasks_spec,
};
use crate::machine::inputs::{
    BuildGraphInput, MachineGraphItem, MachineProfileInput, MachineTaskSpec, SyncInput, SyncPlan,
};
use crate::machine::schedule::pre_task_barrier_satisfied;
use crate::types::{
    ArtifactKind, ClaimOutcome, ClaimOutcomePatch, GateNodeRecord, InvariantsState, NodeKind,
    NodeStatus, NodeStatusPatch, RunGraphSnapshot, RunStatus, SubtaskRecord, TaskDependencyRecord,
    TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, TaskRef};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Plan a pure `sync` or `sync --rebind-profile` update against `snapshot`.
pub fn plan_sync(
    snapshot: &RunGraphSnapshot,
    current_profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &SyncInput,
) -> Result<SyncPlan, StateError> {
    if snapshot.run.status != RunStatus::Active {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot sync non-active run '{}' (status: {})",
                snapshot.run.run_id, snapshot.run.status
            ),
        ));
    }

    let effective_profile = if input.rebind_profile {
        input.new_profile.as_ref().ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "sync --rebind-profile requires new_profile",
            )
        })?
    } else {
        current_profile
    };

    validate_tasks_spec(
        &input.spec.tasks,
        &snapshot.run.run_id,
        &effective_profile.tags,
    )?;

    let existing_tasks: BTreeMap<TaskRef, &GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .filter_map(|n| n.task_ref.as_ref().map(|tr| (tr.clone(), n)))
        .collect();

    let incoming_tasks: BTreeMap<TaskRef, &MachineTaskSpec> = input
        .spec
        .tasks
        .iter()
        .map(|t| (t.task_ref.clone(), t))
        .collect();

    // Detect newly added tasks after a pre-task root gate recorded a `tasks:` fan-out artifact.
    let adding_new_task = incoming_tasks
        .keys()
        .any(|tref| !existing_tasks.contains_key(tref));
    if adding_new_task && has_pre_task_tasks_fanout_artifact(snapshot) {
        return Err(StateError::new(
            ErrorCode::PreTaskArtifactStale,
            "cannot add new tasks after pre-task root gate already recorded a 'tasks:' fan-out artifact",
        )
        .with_hint("run 'acdd reopen <pre_task_gate>' before syncing new tasks"));
    }

    let mut watch_ids: BTreeSet<&str> = BTreeSet::new();
    let mut node_patches: Vec<NodeStatusPatch> = Vec::new();
    let mut claim_patches: Vec<ClaimOutcomePatch> = Vec::new();
    let mut new_subtasks: Vec<SubtaskRecord> = Vec::new();
    let mut added_nodes: Vec<GateNodeRecord> = Vec::new();
    let mut added_dependencies: Vec<TaskDependencyRecord> = Vec::new();

    // 1. Update existing tasks (NEVER reset Completed nodes!).
    for (task_ref, existing_task) in &existing_tasks {
        match incoming_tasks.get(task_ref) {
            None => {
                // Task removed from spec: cancel only if non-completed.
                if !matches!(
                    existing_task.status,
                    NodeStatus::Completed | NodeStatus::Cancelled
                ) {
                    watch_ids.insert(existing_task.node_id.as_str());
                    if let Some(active) = active_claim_for_node(snapshot, &existing_task.node_id) {
                        claim_patches.push(ClaimOutcomePatch {
                            node_id: existing_task.node_id.clone(),
                            claim_revision: active.claim_revision,
                            outcome: ClaimOutcome::Cancelled,
                            ended_at: obs.now_utc.clone(),
                        });
                    }
                    node_patches.push(NodeStatusPatch::status_only(
                        &existing_task.node_id,
                        NodeStatus::Cancelled,
                    ));
                    for child in snapshot
                        .nodes
                        .iter()
                        .filter(|n| n.parent_id.as_deref() == Some(existing_task.node_id.as_str()))
                    {
                        if !matches!(child.status, NodeStatus::Completed | NodeStatus::Cancelled) {
                            watch_ids.insert(child.node_id.as_str());
                            if let Some(active) = active_claim_for_node(snapshot, &child.node_id) {
                                claim_patches.push(ClaimOutcomePatch {
                                    node_id: child.node_id.clone(),
                                    claim_revision: active.claim_revision,
                                    outcome: ClaimOutcome::Cancelled,
                                    ended_at: obs.now_utc.clone(),
                                });
                            }
                            node_patches.push(NodeStatusPatch::status_only(
                                &child.node_id,
                                NodeStatus::Cancelled,
                            ));
                        }
                    }
                }
            }
            Some(spec_task) => {
                if existing_task.status == NodeStatus::Completed {
                    // Never mutate or reset completed tasks!
                    continue;
                }

                watch_ids.insert(existing_task.node_id.as_str());
                let new_tags = effective_task_tags(&spec_task.tags, &effective_profile.tags)?;
                let mut inv = existing_task.invariants.clone();
                inv.applicable = spec_task.invariants.clone();

                let mut task_patch =
                    NodeStatusPatch::status_only(&existing_task.node_id, existing_task.status);
                task_patch.tags = Some(new_tags.clone());
                task_patch.scope = Some(spec_task.scope.clone());
                task_patch.invariants = Some(inv.clone());
                node_patches.push(task_patch);

                for child in snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.parent_id.as_deref() == Some(existing_task.node_id.as_str()))
                {
                    if child.status == NodeStatus::Completed {
                        continue;
                    }
                    watch_ids.insert(child.node_id.as_str());
                    let mut child_patch =
                        NodeStatusPatch::status_only(&child.node_id, child.status);
                    child_patch.tags = Some(new_tags.clone());
                    child_patch.scope = Some(spec_task.scope.clone());
                    child_patch.invariants = Some(inv.clone());
                    if input.rebind_profile {
                        if let Ok(gate_def) = resolve_gate_def(effective_profile, child) {
                            if !matches_when(
                                gate_def.when.as_deref(),
                                &new_tags,
                                &effective_profile.tags,
                            )? {
                                child_patch.new_status = NodeStatus::Cancelled;
                            }
                        } else {
                            // Gate no longer exists in rebound profile; cancel non-completed gate.
                            child_patch.new_status = NodeStatus::Cancelled;
                        }
                    }
                    node_patches.push(child_patch);
                }

                // Sync subtasks metadata for non-completed task.
                for (sub_idx, sub) in spec_task.subtasks.iter().enumerate() {
                    new_subtasks.push(SubtaskRecord {
                        run_id: snapshot.run.run_id.clone(),
                        node_id: existing_task.node_id.clone(),
                        subtask_ref: sub.subtask_ref.clone(),
                        seq_order: sub_idx as u32,
                        title: sub.title.clone(),
                        status: sub.status,
                        evidence: sub.evidence.clone(),
                        updated_at: obs.now_utc.clone(),
                    });
                }

                // Record newly added dependencies for this task.
                for dep in &spec_task.depends_on {
                    let already = snapshot.dependencies.iter().any(|d| {
                        d.task_ref == spec_task.task_ref
                            && d.depends_on_workspace == dep.depends_on_workspace
                            && d.depends_on_run_id == dep.depends_on_run_id
                            && d.depends_on_ref == dep.depends_on_ref
                    });
                    if !already {
                        added_dependencies.push(TaskDependencyRecord {
                            run_id: snapshot.run.run_id.clone(),
                            task_ref: spec_task.task_ref.clone(),
                            depends_on_workspace: dep.depends_on_workspace.clone(),
                            depends_on_run_id: dep.depends_on_run_id.clone(),
                            depends_on_ref: dep.depends_on_ref.clone(),
                            satisfied: false,
                        });
                    }
                }
            }
        }
    }

    // 2. Rebind non-completed root gates when `rebind_profile == true`.
    if input.rebind_profile {
        for node in &snapshot.nodes {
            if node.kind != NodeKind::RootGate || node.status == NodeStatus::Completed {
                continue;
            }
            if let Ok(gate_def) = resolve_gate_def(effective_profile, node) {
                watch_ids.insert(node.node_id.as_str());
                let mut patch = NodeStatusPatch::status_only(&node.node_id, node.status);
                patch.scope = Some(gate_def.scope.clone());
                patch.invariants = Some(InvariantsState::default());
                node_patches.push(patch);
            }
        }
    }

    // 3. Instantiate newly added tasks into matching `- tasks:` block(s).
    let pre_ok = pre_task_barrier_satisfied(snapshot);
    let mut seq_counter = snapshot
        .nodes
        .iter()
        .map(|n| n.seq_order + 1)
        .max()
        .unwrap_or(0);

    let dummy_build_input = BuildGraphInput {
        run_id: snapshot.run.run_id.clone(),
        run_name: snapshot.run.run_name.clone(),
        collection: snapshot.run.collection.clone(),
        workspace: snapshot.run.workspace.clone(),
        spec_path: snapshot.run.spec_path.clone(),
        is_ephemeral: snapshot.run.is_ephemeral,
        target_base: snapshot.run.target_base.clone(),
        target_base_commit: snapshot.run.target_base_commit.clone(),
        branch: snapshot.run.branch.clone(),
        worktree: snapshot.run.worktree.clone(),
        trace_id: snapshot.run.trace_id.clone(),
        created_at: obs.now_utc.clone(),
        spec: input.spec.clone(),
    };

    for spec_task in &input.spec.tasks {
        if existing_tasks.contains_key(&spec_task.task_ref) {
            continue;
        }
        let tags = effective_task_tags(&spec_task.tags, &effective_profile.tags)?;
        let mut matched_block: Option<(String, &[crate::machine::inputs::MachineGateDef])> = None;
        let mut block_idx = 0usize;
        for item in &effective_profile.gates {
            if let MachineGraphItem::TasksBlock(block) = item {
                if matches_when(block.when.as_deref(), &tags, &effective_profile.tags)? {
                    if matched_block.is_some() {
                        return Err(StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!(
                                "new task '{}' matches multiple '- tasks:' blocks during sync",
                                spec_task.task_ref
                            ),
                        ));
                    }
                    matched_block = Some((
                        NodeCursor::tasks_block_node_id(block_idx),
                        block.gates.as_slice(),
                    ));
                }
                block_idx += 1;
            }
        }
        let Some((block_id, template_gates)) = matched_block else {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "new task '{}' did not match any '- tasks:' block during sync",
                    spec_task.task_ref
                ),
            ));
        };

        let deps_satisfied = pre_ok
            && spec_task.depends_on.iter().all(|dep| {
                if dep.depends_on_run_id == snapshot.run.run_id {
                    existing_tasks
                        .get(&dep.depends_on_ref)
                        .is_some_and(|t| t.status == NodeStatus::Completed)
                } else {
                    obs.external_satisfied_deps.iter().any(|(ws, rid, ref_id)| {
                        ws == &dep.depends_on_workspace
                            && rid == &dep.depends_on_run_id
                            && ref_id == &dep.depends_on_ref
                    })
                }
            });

        append_task_nodes(
            &mut added_nodes,
            &mut new_subtasks,
            &mut added_dependencies,
            &mut seq_counter,
            effective_profile,
            &dummy_build_input,
            spec_task,
            &block_id,
            template_gates,
            deps_satisfied,
        )?;
    }

    let watch_vec: Vec<&str> = watch_ids.into_iter().collect();
    let preconditions = build_preconditions(snapshot, &watch_vec, None);

    Ok(SyncPlan {
        transition: TransitionPlan {
            preconditions,
            bump_run_generation: true,
            new_run_status: None,
            new_root_cycle_count: None,
            completed_at: None,
            node_patches,
            dependency_patches: Vec::new(),
            claim_patches,
            new_artifacts: Vec::new(),
            new_blackboard: Vec::new(),
            blackboard_patches: Vec::new(),
            new_subtasks,
            significant_transition: true,
        },
        added_nodes,
        added_dependencies,
        spec_digest: input.spec.spec_digest.clone(),
    })
}

fn has_pre_task_tasks_fanout_artifact(snapshot: &RunGraphSnapshot) -> bool {
    let completed_pre_nodes: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| is_pre_task_root_node(snapshot, n) && n.status == NodeStatus::Completed)
        .map(|n| n.node_id.as_str())
        .collect();

    if completed_pre_nodes.is_empty() {
        return false;
    }

    snapshot.artifacts.iter().any(|art| {
        completed_pre_nodes.contains(art.node_id.as_str())
            && matches!(art.kind, ArtifactKind::Proof | ArtifactKind::Receipt)
            && serde_json::from_str::<Value>(&art.payload_json)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .is_some_and(|obj| obj.contains_key("tasks"))
    })
}
