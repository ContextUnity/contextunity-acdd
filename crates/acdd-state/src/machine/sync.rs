//! Pure `sync` and `sync --rebind-profile` planning built on canonical [`build_initial_run_graph`].

use crate::machine::graph::{
    active_claim_for_node, build_initial_run_graph, build_preconditions, is_pre_task_root_node,
};
use crate::machine::inputs::{BuildGraphInput, MachineProfileInput, SyncInput, SyncPlan};
use crate::machine::schedule::{is_dep_satisfied_against_statuses, pre_task_barrier_satisfied};
use crate::types::{
    ArtifactKind, ClaimOutcome, ClaimOutcomePatch, GateNodeRecord, InvariantsState, NodeKind,
    NodeStatus, NodeStatusPatch, RunGraphSnapshot, RunStatus, SubtaskRecord, SubtaskStatus,
    TaskDependencyRecord, TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::TaskRef;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Accumulates deduplicated node patches keyed by `node_id` plus CAS `watch_ids`.
struct PatchAccumulator<'a> {
    patches: Vec<NodeStatusPatch>,
    index_by_id: BTreeMap<String, usize>,
    watch_ids: BTreeSet<&'a str>,
}

impl<'a> PatchAccumulator<'a> {
    fn new() -> Self {
        Self {
            patches: Vec::new(),
            index_by_id: BTreeMap::new(),
            watch_ids: BTreeSet::new(),
        }
    }

    fn upsert(&mut self, node: &'a GateNodeRecord, mutate: impl FnOnce(&mut NodeStatusPatch)) {
        self.watch_ids.insert(node.node_id.as_str());
        let idx = *self
            .index_by_id
            .entry(node.node_id.clone())
            .or_insert_with(|| {
                let idx = self.patches.len();
                self.patches
                    .push(NodeStatusPatch::status_only(&node.node_id, node.status));
                idx
            });
        mutate(&mut self.patches[idx]);
    }

    fn effective_status(&self, node: &GateNodeRecord) -> NodeStatus {
        self.index_by_id
            .get(&node.node_id)
            .map_or(node.status, |&i| self.patches[i].new_status)
    }

    fn status(
        &self,
        snap: &'a RunGraphSnapshot,
        added: &[GateNodeRecord],
        id: &str,
    ) -> Option<NodeStatus> {
        snap.nodes
            .iter()
            .find(|n| n.node_id == id)
            .map(|n| self.effective_status(n))
            .or_else(|| added.iter().find(|n| n.node_id == id).map(|n| n.status))
    }

    fn set_status(
        &mut self,
        snap: &'a RunGraphSnapshot,
        added: &mut [GateNodeRecord],
        id: &str,
        status: NodeStatus,
    ) {
        if let Some(n) = snap.nodes.iter().find(|n| n.node_id == id) {
            self.upsert(n, |patch| patch.new_status = status);
        } else if let Some(n) = added.iter_mut().find(|n| n.node_id == id) {
            n.status = status;
        }
    }
}

fn upsert_reconciled_node<'a>(
    acc: &mut PatchAccumulator<'a>,
    node: &'a GateNodeRecord,
    status: NodeStatus,
    template: &GateNodeRecord,
    invariants: &InvariantsState,
) {
    acc.upsert(node, |patch| {
        patch.new_status = status;
        patch.tags = Some(template.tags.clone());
        patch.scope = Some(template.scope.clone());
        patch.invariants = Some(invariants.clone());
    });
}

fn is_active_status(status: NodeStatus) -> bool {
    matches!(
        status,
        NodeStatus::Ready | NodeStatus::InProgress | NodeStatus::Stale | NodeStatus::Escalated
    )
}

fn cancel_node<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &'a GateNodeRecord,
    now_utc: &str,
    acc: &mut PatchAccumulator<'a>,
    claim_patches: &mut Vec<ClaimOutcomePatch>,
    metadata: Option<(&[String], &[String], &InvariantsState)>,
) {
    if matches!(node.status, NodeStatus::Completed | NodeStatus::Cancelled) {
        return;
    }
    if let Some(active) = active_claim_for_node(snapshot, &node.node_id) {
        claim_patches.push(ClaimOutcomePatch {
            node_id: node.node_id.clone(),
            claim_revision: active.claim_revision,
            outcome: ClaimOutcome::Cancelled,
            ended_at: now_utc.to_owned(),
        });
    }
    acc.upsert(node, |patch| {
        patch.new_status = NodeStatus::Cancelled;
        if let Some((tags, scope, inv)) = metadata {
            patch.tags = Some(tags.to_vec());
            patch.scope = Some(scope.to_vec());
            patch.invariants = Some(inv.clone());
        }
    });
}

fn has_completed_or_cancelled_ancestor<'b>(
    snapshot: &'b RunGraphSnapshot,
    target_by_id: &BTreeMap<&'b str, &'b GateNodeRecord>,
    acc: &PatchAccumulator<'_>,
    mut parent_id: Option<&'b str>,
) -> bool {
    while let Some(pid) = parent_id {
        if let Some(parent) = snapshot.nodes.iter().find(|n| n.node_id == pid) {
            if matches!(
                acc.effective_status(parent),
                NodeStatus::Completed | NodeStatus::Cancelled
            ) {
                return true;
            }
            parent_id = parent.parent_id.as_deref();
        } else if let Some(t_parent) = target_by_id.get(pid) {
            parent_id = t_parent.parent_id.as_deref();
        } else {
            break;
        }
    }
    false
}

fn collect_descendant_ids(
    snap: &RunGraphSnapshot,
    added: &[GateNodeRecord],
    root_id: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut frontier = vec![root_id];
    while let Some(pid) = frontier.pop() {
        for n in snap.nodes.iter().chain(added.iter()) {
            if n.parent_id.as_deref() == Some(pid) && seen.insert(n.node_id.as_str()) {
                out.push(n.node_id.clone());
                frontier.push(n.node_id.as_str());
            }
        }
    }
    out
}

fn demote_ready_ordered_nodes<'a>(
    ordered_ids: &[String],
    snap: &'a RunGraphSnapshot,
    added: &mut [GateNodeRecord],
    acc: &mut PatchAccumulator<'a>,
) {
    let mut seen_non_terminal = false;
    for id in ordered_ids {
        if let Some(a) = added.iter().find(|n| &n.node_id == id) {
            if !matches!(a.status, NodeStatus::Completed | NodeStatus::Cancelled) {
                seen_non_terminal = true;
            }
            continue;
        }
        let Some(n) = snap.nodes.iter().find(|n| &n.node_id == id) else {
            continue;
        };
        let eff = acc.effective_status(n);
        if !(seen_non_terminal && eff == NodeStatus::Ready && n.claim_revision == 0) {
            if !matches!(eff, NodeStatus::Completed | NodeStatus::Cancelled) {
                seen_non_terminal = true;
            }
            continue;
        }
        if n.kind != NodeKind::Container {
            acc.upsert(n, |patch| patch.new_status = NodeStatus::Pending);
            continue;
        }
        let desc = collect_descendant_ids(snap, added, &n.node_id);
        let desc_safe = desc.iter().all(|did| {
            snap.nodes
                .iter()
                .chain(added.iter())
                .find(|x| &x.node_id == did)
                .is_some_and(|x| x.claim_revision == 0)
                && acc
                    .status(snap, added, did)
                    .is_some_and(|st| matches!(st, NodeStatus::Pending | NodeStatus::Ready))
        });
        if !desc_safe {
            seen_non_terminal = true;
            continue;
        }
        acc.set_status(snap, added, &n.node_id, NodeStatus::Pending);
        for did in desc {
            if acc.status(snap, added, &did) == Some(NodeStatus::Ready) {
                acc.set_status(snap, added, &did, NodeStatus::Pending);
            }
        }
    }
}

/// Shared sequential readiness reconciler for root sequences, root containers, and task gates.
fn reconcile_ordered_sequence<'a>(
    ordered_ids: &[String],
    demote: bool,
    snap: &'a RunGraphSnapshot,
    target: &RunGraphSnapshot,
    added: &mut [GateNodeRecord],
    acc: &mut PatchAccumulator<'a>,
) {
    if demote {
        demote_ready_ordered_nodes(ordered_ids, snap, added, acc);
    }

    if ordered_ids
        .iter()
        .any(|id| acc.status(snap, added, id).is_some_and(is_active_status))
    {
        return;
    }

    if let Some(first_pending) = ordered_ids
        .iter()
        .find(|id| acc.status(snap, added, id) == Some(NodeStatus::Pending))
    {
        acc.set_status(snap, added, first_pending, NodeStatus::Ready);
        if snap
            .nodes
            .iter()
            .chain(added.iter())
            .any(|n| &n.node_id == first_pending && n.kind == NodeKind::Container)
        {
            let child_ids = ordered_children_ids(first_pending, snap, target, added);
            reconcile_ordered_sequence(&child_ids, demote, snap, target, added, acc);
        }
    }
}

fn ordered_children_ids(
    parent_id: &str,
    snap: &RunGraphSnapshot,
    target: &RunGraphSnapshot,
    added: &[GateNodeRecord],
) -> Vec<String> {
    let live: BTreeSet<&str> = snap
        .nodes
        .iter()
        .chain(added.iter())
        .filter(|n| n.parent_id.as_deref() == Some(parent_id))
        .map(|n| n.node_id.as_str())
        .collect();
    let mut ids: Vec<String> = target
        .nodes
        .iter()
        .filter(|n| n.parent_id.as_deref() == Some(parent_id) && live.contains(n.node_id.as_str()))
        .map(|n| n.node_id.clone())
        .collect();
    let mut extras: Vec<&GateNodeRecord> = snap
        .nodes
        .iter()
        .filter(|n| n.parent_id.as_deref() == Some(parent_id) && !ids.contains(&n.node_id))
        .collect();
    extras.sort_by_key(|n| n.seq_order);
    ids.extend(extras.into_iter().map(|n| n.node_id.clone()));
    ids
}

fn dep_satisfied_for_sync(
    snapshot: &RunGraphSnapshot,
    completed_tasks: &BTreeSet<&TaskRef>,
    dep: &TaskDependencyRecord,
    obs: &ValidatedObservations,
) -> bool {
    let mut probe = dep.clone();
    probe.satisfied = snapshot.dependencies.iter().any(|d| {
        d.task_ref == dep.task_ref
            && d.depends_on_workspace == dep.depends_on_workspace
            && d.depends_on_run_id == dep.depends_on_run_id
            && d.depends_on_ref == dep.depends_on_ref
            && d.satisfied
    });
    is_dep_satisfied_against_statuses(
        &snapshot.run.run_id,
        &probe,
        |r| completed_tasks.contains(r),
        obs,
    )
}

fn reconcile_task_dependencies(
    snapshot: &RunGraphSnapshot,
    target: &RunGraphSnapshot,
    obs: &ValidatedObservations,
    task_ref: &TaskRef,
    completed_tasks: &BTreeSet<&TaskRef>,
    added_dependencies: &mut Vec<TaskDependencyRecord>,
    removed_dependencies: &mut Vec<TaskDependencyRecord>,
) -> bool {
    let target_deps: Vec<&TaskDependencyRecord> = target
        .dependencies
        .iter()
        .filter(|d| &d.task_ref == task_ref)
        .collect();
    for t_dep in &target_deps {
        if !snapshot.dependencies.iter().any(|d| {
            d.task_ref == t_dep.task_ref
                && d.depends_on_workspace == t_dep.depends_on_workspace
                && d.depends_on_run_id == t_dep.depends_on_run_id
                && d.depends_on_ref == t_dep.depends_on_ref
        }) {
            let mut dep = (*t_dep).clone();
            dep.satisfied = dep_satisfied_for_sync(snapshot, completed_tasks, t_dep, obs);
            added_dependencies.push(dep);
        }
    }
    for existing_dep in snapshot
        .dependencies
        .iter()
        .filter(|d| &d.task_ref == task_ref)
    {
        if !target_deps.iter().any(|d| {
            d.depends_on_workspace == existing_dep.depends_on_workspace
                && d.depends_on_run_id == existing_dep.depends_on_run_id
                && d.depends_on_ref == existing_dep.depends_on_ref
        }) {
            removed_dependencies.push(existing_dep.clone());
        }
    }
    target_deps
        .iter()
        .all(|d| dep_satisfied_for_sync(snapshot, completed_tasks, d, obs))
}

struct SyncPlanBuilder<'a> {
    acc: PatchAccumulator<'a>,
    completed_tasks: BTreeSet<&'a TaskRef>,
    existing_by_id: BTreeMap<&'a str, &'a GateNodeRecord>,
    target_by_id: BTreeMap<&'a str, &'a GateNodeRecord>,
    claim_patches: Vec<ClaimOutcomePatch>,
    new_subtasks: Vec<SubtaskRecord>,
    added_dependencies: Vec<TaskDependencyRecord>,
    removed_dependencies: Vec<TaskDependencyRecord>,
    added_nodes: Vec<GateNodeRecord>,
}

impl<'a> SyncPlanBuilder<'a> {
    fn new(snapshot: &'a RunGraphSnapshot, target: &'a RunGraphSnapshot) -> Self {
        Self {
            acc: PatchAccumulator::new(),
            completed_tasks: snapshot
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
                .filter_map(|n| n.task_ref.as_ref())
                .collect(),
            existing_by_id: snapshot
                .nodes
                .iter()
                .map(|n| (n.node_id.as_str(), n))
                .collect(),
            target_by_id: target
                .nodes
                .iter()
                .map(|n| (n.node_id.as_str(), n))
                .collect(),
            claim_patches: Vec::new(),
            new_subtasks: Vec::new(),
            added_dependencies: Vec::new(),
            removed_dependencies: Vec::new(),
            added_nodes: Vec::new(),
        }
    }

    fn reconcile_existing_task(
        &mut self,
        snapshot: &'a RunGraphSnapshot,
        target: &RunGraphSnapshot,
        obs: &ValidatedObservations,
        rebind_profile: bool,
        pre_ok: bool,
        existing_task: &'a GateNodeRecord,
    ) {
        let task_ref = existing_task
            .task_ref
            .as_ref()
            .expect("task nodes have task_ref");
        let Some(t_task) = self.target_by_id.get(existing_task.node_id.as_str()).copied() else {
            if !matches!(
                existing_task.status,
                NodeStatus::Completed | NodeStatus::Cancelled
            ) {
                cancel_node(
                    snapshot,
                    existing_task,
                    &obs.now_utc,
                    &mut self.acc,
                    &mut self.claim_patches,
                    None,
                );
                for child in snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.parent_id.as_deref() == Some(existing_task.node_id.as_str()))
                {
                    cancel_node(
                        snapshot,
                        child,
                        &obs.now_utc,
                        &mut self.acc,
                        &mut self.claim_patches,
                        None,
                    );
                }
            }
            return;
        };
        if matches!(
            existing_task.status,
            NodeStatus::Completed | NodeStatus::Cancelled
        ) {
            return;
        }

        for sub in target
            .subtasks
            .iter()
            .filter(|s| s.node_id == existing_task.node_id)
        {
            let mut merged = sub.clone();
            if let Some(prev) = snapshot
                .subtasks
                .iter()
                .find(|s| s.node_id == sub.node_id && s.subtask_ref == sub.subtask_ref)
            {
                let has_prev_ev = prev
                    .evidence
                    .as_deref()
                    .is_some_and(|ev| !ev.trim().is_empty());
                if prev.status != SubtaskStatus::Pending || has_prev_ev {
                    merged.status = prev.status;
                    if has_prev_ev {
                        merged.evidence = prev.evidence.clone();
                    }
                }
            }
            self.new_subtasks.push(merged);
        }

        let remaining_deps_satisfied = reconcile_task_dependencies(
            snapshot,
            target,
            obs,
            task_ref,
            &self.completed_tasks,
            &mut self.added_dependencies,
            &mut self.removed_dependencies,
        );
        let existing_children: Vec<&GateNodeRecord> = snapshot
            .nodes
            .iter()
            .filter(|n| n.parent_id.as_deref() == Some(existing_task.node_id.as_str()))
            .collect();
        let any_child_started = existing_children.iter().any(|c| {
            c.claim_revision > 0
                || matches!(
                    c.status,
                    NodeStatus::InProgress
                        | NodeStatus::Completed
                        | NodeStatus::Stale
                        | NodeStatus::Escalated
                )
        });
        let new_task_status =
            if existing_task.status == NodeStatus::Pending && pre_ok && remaining_deps_satisfied {
                NodeStatus::Ready
            } else if existing_task.status == NodeStatus::Ready
                && existing_task.claim_revision == 0
                && (!pre_ok || !remaining_deps_satisfied)
                && !any_child_started
            {
                NodeStatus::Pending
            } else {
                existing_task.status
            };

        let mut inv = existing_task.invariants.clone();
        inv.applicable = t_task.invariants.applicable.clone();
        upsert_reconciled_node(&mut self.acc, existing_task, new_task_status, t_task, &inv);
        if new_task_status == NodeStatus::Ready {
            if let Some(ref pid) = existing_task.parent_id {
                if self.acc.status(snapshot, &self.added_nodes, pid) == Some(NodeStatus::Pending) {
                    self.acc
                        .set_status(snapshot, &mut self.added_nodes, pid, NodeStatus::Ready);
                }
            }
        }

        for &child in &existing_children {
            if matches!(child.status, NodeStatus::Completed | NodeStatus::Cancelled) {
                continue;
            }
            if rebind_profile && !self.target_by_id.contains_key(child.node_id.as_str()) {
                cancel_node(
                    snapshot,
                    child,
                    &obs.now_utc,
                    &mut self.acc,
                    &mut self.claim_patches,
                    Some((&t_task.tags, &t_task.scope, &inv)),
                );
            } else {
                let demoted = if new_task_status == NodeStatus::Pending
                    && child.status == NodeStatus::Ready
                    && child.claim_revision == 0
                {
                    NodeStatus::Pending
                } else {
                    child.status
                };
                upsert_reconciled_node(&mut self.acc, child, demoted, t_task, &inv);
            }
        }

        let mut task_added_gates = Vec::new();
        if rebind_profile {
            for t_gate in target
                .nodes
                .iter()
                .filter(|n| n.parent_id.as_deref() == Some(existing_task.node_id.as_str()))
            {
                if !self.existing_by_id.contains_key(t_gate.node_id.as_str()) {
                    let mut gate = t_gate.clone();
                    gate.status = NodeStatus::Pending;
                    gate.invariants = inv.clone();
                    gate.extended_scope = existing_task.extended_scope.clone();
                    task_added_gates.push(gate);
                }
            }
        }
        if matches!(new_task_status, NodeStatus::Ready | NodeStatus::InProgress) {
            let ordered_gates =
                ordered_children_ids(&existing_task.node_id, snapshot, target, &task_added_gates);
            reconcile_ordered_sequence(
                &ordered_gates,
                rebind_profile,
                snapshot,
                target,
                &mut task_added_gates,
                &mut self.acc,
            );
        }
        self.added_nodes.extend(task_added_gates);
    }

    fn instantiate_new_task(
        &mut self,
        snapshot: &'a RunGraphSnapshot,
        target: &RunGraphSnapshot,
        obs: &ValidatedObservations,
        pre_ok: bool,
        t_task: &GateNodeRecord,
    ) {
        let tref = t_task.task_ref.as_ref().expect("task nodes have task_ref");
        if let Some(ref block_id) = t_task.parent_id {
            if !self.existing_by_id.contains_key(block_id.as_str())
                && !self.added_nodes.iter().any(|n| &n.node_id == block_id)
            {
                if let Some(t_block) = self.target_by_id.get(block_id.as_str()) {
                    let mut block = (*t_block).clone();
                    block.status = if pre_ok {
                        NodeStatus::Ready
                    } else {
                        NodeStatus::Pending
                    };
                    self.added_nodes.push(block);
                }
            } else if pre_ok
                && self.acc.status(snapshot, &self.added_nodes, block_id)
                    == Some(NodeStatus::Pending)
            {
                self.acc
                    .set_status(snapshot, &mut self.added_nodes, block_id, NodeStatus::Ready);
            }
        }
        let task_deps: Vec<&TaskDependencyRecord> = target
            .dependencies
            .iter()
            .filter(|d| &d.task_ref == tref)
            .collect();
        let deps_satisfied = pre_ok
            && task_deps
                .iter()
                .all(|d| dep_satisfied_for_sync(snapshot, &self.completed_tasks, d, obs));
        for dep in task_deps {
            let mut dep = dep.clone();
            dep.satisfied = dep_satisfied_for_sync(snapshot, &self.completed_tasks, &dep, obs);
            self.added_dependencies.push(dep);
        }
        self.new_subtasks.extend(
            target
                .subtasks
                .iter()
                .filter(|s| s.node_id == t_task.node_id)
                .cloned(),
        );

        let mut added_task = t_task.clone();
        added_task.status = if deps_satisfied {
            NodeStatus::Ready
        } else {
            NodeStatus::Pending
        };
        self.added_nodes.push(added_task);
        for (g_idx, t_gate) in target
            .nodes
            .iter()
            .filter(|n| n.parent_id.as_deref() == Some(t_task.node_id.as_str()))
            .enumerate()
        {
            let mut gate = t_gate.clone();
            gate.status = if deps_satisfied && g_idx == 0 {
                NodeStatus::Ready
            } else {
                NodeStatus::Pending
            };
            self.added_nodes.push(gate);
        }
    }
}

/// Plan a pure `sync` or `sync --rebind-profile` update against `snapshot`.
pub(crate) fn plan_sync(
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

    let target = build_initial_run_graph(
        effective_profile,
        &BuildGraphInput {
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
        },
    )?;

    let existing_tasks: BTreeMap<TaskRef, &GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .filter_map(|n| n.task_ref.as_ref().map(|tr| (tr.clone(), n)))
        .collect();
    if input
        .spec
        .tasks
        .iter()
        .any(|t| !existing_tasks.contains_key(&t.task_ref))
        && has_pre_task_tasks_fanout_artifact(snapshot)
    {
        return Err(StateError::new(
            ErrorCode::PreTaskArtifactStale,
            "cannot add new tasks after pre-task root gate already recorded a 'tasks:' fan-out artifact",
        ).with_hint("run 'acdd reopen <pre_task_gate>' before syncing new tasks"));
    }

    let mut sync = SyncPlanBuilder::new(snapshot, &target);
    let target_first_tasks_seq = target
        .nodes
        .iter()
        .find(|n| n.node_id.starts_with("tasks#"))
        .map(|n| n.seq_order);

    let mut pre_task_added_ids = BTreeSet::new();

    // 1. Reconcile root gates/containers and `tasks#N` containers on `rebind_profile`.
    if input.rebind_profile {
        for node in &snapshot.nodes {
            if !matches!(node.kind, NodeKind::RootGate | NodeKind::Container)
                || node.node_id.starts_with("tasks#")
                || matches!(node.status, NodeStatus::Completed | NodeStatus::Cancelled)
            {
                continue;
            }
            match sync.target_by_id.get(node.node_id.as_str()) {
                Some(t_node) if t_node.kind == node.kind => {
                    if node.kind == NodeKind::RootGate {
                        sync.acc.upsert(node, |patch| {
                            patch.scope = Some(t_node.scope.clone());
                            patch.invariants = Some(InvariantsState::default());
                        });
                    }
                }
                Some(t_node) => {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "cannot rebind gate '{}' across node kind change ({} -> {})",
                            node.node_id, node.kind, t_node.kind
                        ),
                    ))
                }
                None => cancel_node(
                    snapshot,
                    node,
                    &obs.now_utc,
                    &mut sync.acc,
                    &mut sync.claim_patches,
                    None,
                ),
            }
        }
        for t_node in &target.nodes {
            if matches!(t_node.kind, NodeKind::RootGate | NodeKind::Container)
                && !sync.existing_by_id.contains_key(t_node.node_id.as_str())
                && !has_completed_or_cancelled_ancestor(
                    snapshot,
                    &sync.target_by_id,
                    &sync.acc,
                    t_node.parent_id.as_deref(),
                )
            {
                if target_first_tasks_seq.is_some_and(|ts| t_node.seq_order < ts)
                    && !t_node.node_id.starts_with("tasks#")
                {
                    pre_task_added_ids.insert(t_node.node_id.clone());
                }
                let mut inserted = t_node.clone();
                inserted.status = NodeStatus::Pending;
                sync.added_nodes.push(inserted);
            }
        }
    }

    let pre_ok = if input.rebind_profile {
        pre_task_added_ids.is_empty()
            && snapshot
                .nodes
                .iter()
                .filter(|n| is_pre_task_root_node(snapshot, n))
                .all(|n| {
                    matches!(
                        sync.acc.effective_status(n),
                        NodeStatus::Completed | NodeStatus::Cancelled
                    )
                })
    } else {
        pre_task_barrier_satisfied(snapshot)
    };

    // 2. Reconcile existing tasks (`INV-SYNC-COMPLETED-IMMUTABLE`: never mutate Completed/Cancelled).
    {
        for (task_ref, existing_task) in &existing_tasks {
            let _ = task_ref;
            sync.reconcile_existing_task(
                snapshot,
                &target,
                obs,
                input.rebind_profile,
                pre_ok,
                existing_task,
            );
        }

        // 3. Instantiate newly added tasks from `target` (plus missing `tasks#N` containers).
        for t_task in target.nodes.iter().filter(|n| n.kind == NodeKind::Task) {
            let Some(ref tref) = t_task.task_ref else {
                continue;
            };
            if existing_tasks.contains_key(tref) {
                continue;
            }
            let _ = tref;
            sync.instantiate_new_task(snapshot, &target, obs, pre_ok, t_task);
        }
    }

    // 4. Reconcile root-level readiness when `rebind_profile == true`.
    if input.rebind_profile {
        reconcile_root_readiness(
            snapshot,
            &target,
            target_first_tasks_seq,
            &mut sync.acc,
            &mut sync.added_nodes,
            pre_ok,
        );
    }

    // 5. Maintain canonical `seq_order` whenever profile rebound or new nodes were added.
    let seq_order_updates = if input.rebind_profile || !sync.added_nodes.is_empty() {
        compute_reconciled_seq_order(snapshot, &target, &mut sync.added_nodes)
    } else {
        Vec::new()
    };

    let watch_vec: Vec<&str> = sync.acc.watch_ids.iter().copied().collect();
    let preconditions = build_preconditions(snapshot, &watch_vec, None);
    Ok(SyncPlan {
        transition: TransitionPlan {
            preconditions,
            bump_run_generation: true,
            new_run_status: None,
            new_root_cycle_count: None,
            completed_at: None,
            node_patches: sync.acc.patches,
            dependency_patches: Vec::new(),
            claim_patches: sync.claim_patches,
            new_artifacts: Vec::new(),
            new_blackboard: Vec::new(),
            blackboard_patches: Vec::new(),
            new_subtasks: sync.new_subtasks,
            significant_transition: true,
        },
        added_nodes: sync.added_nodes,
        added_dependencies: sync.added_dependencies,
        removed_dependencies: sync.removed_dependencies,
        seq_order_updates,
        spec_digest: input.spec.spec_digest.clone(),
    })
}

fn reconcile_root_readiness<'a>(
    snapshot: &'a RunGraphSnapshot,
    target: &RunGraphSnapshot,
    target_first_tasks_seq: Option<u32>,
    acc: &mut PatchAccumulator<'a>,
    added_nodes: &mut [GateNodeRecord],
    pre_ok: bool,
) {
    let top_ids_for = |added: &[GateNodeRecord], want_pre: bool| -> Vec<String> {
        let mut ids: Vec<String> = target
            .nodes
            .iter()
            .filter(|n| {
                n.parent_id.is_none()
                    && !n.node_id.starts_with("tasks#")
                    && target_first_tasks_seq.is_some_and(|ts| n.seq_order < ts) == want_pre
                    && (snapshot.nodes.iter().any(|e| e.node_id == n.node_id)
                        || added.iter().any(|a| a.node_id == n.node_id))
            })
            .map(|n| n.node_id.clone())
            .collect();
        if want_pre {
            for n in &snapshot.nodes {
                if n.parent_id.is_none()
                    && is_pre_task_root_node(snapshot, n)
                    && !ids.contains(&n.node_id)
                {
                    ids.push(n.node_id.clone());
                }
            }
        }
        ids
    };

    if !pre_ok {
        let pre_top_ids = top_ids_for(added_nodes, true);
        reconcile_ordered_sequence(&pre_top_ids, true, snapshot, target, added_nodes, acc);
    }
    let all_tasks_done = pre_ok
        && snapshot
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task)
            .all(|n| {
                matches!(
                    acc.effective_status(n),
                    NodeStatus::Completed | NodeStatus::Cancelled
                )
            })
        && added_nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task)
            .all(|n| matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled));
    if all_tasks_done {
        let post_top_ids = top_ids_for(added_nodes, false);
        reconcile_ordered_sequence(&post_top_ids, true, snapshot, target, added_nodes, acc);
    }
    let container_ids: Vec<String> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            n.kind == NodeKind::Container
                && !n.node_id.starts_with("tasks#")
                && matches!(
                    acc.effective_status(n),
                    NodeStatus::Ready | NodeStatus::InProgress
                )
        })
        .map(|n| n.node_id.clone())
        .collect();
    for cid in container_ids {
        let child_ids = ordered_children_ids(&cid, snapshot, target, added_nodes);
        reconcile_ordered_sequence(&child_ids, true, snapshot, target, added_nodes, acc);
    }
}

fn compute_reconciled_seq_order(
    snapshot: &RunGraphSnapshot,
    target: &RunGraphSnapshot,
    added_nodes: &mut [GateNodeRecord],
) -> Vec<(String, u32)> {
    let live_ids: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .chain(added_nodes.iter())
        .map(|n| n.node_id.as_str())
        .collect();
    let target_ids: BTreeSet<&str> = target.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let target_idx_by_id: BTreeMap<&str, usize> = target
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.node_id.as_str(), i))
        .collect();

    let mut subtree_end: Vec<usize> = (0..target.nodes.len()).collect();
    for i in (0..target.nodes.len()).rev() {
        if let Some(pid) = target.nodes[i].parent_id.as_deref() {
            if let Some(&p_idx) = target_idx_by_id.get(pid) {
                subtree_end[p_idx] = subtree_end[p_idx].max(subtree_end[i]);
            }
        }
    }

    let mut orphan_by_parent: BTreeMap<&str, Vec<&GateNodeRecord>> = BTreeMap::new();
    for n in &snapshot.nodes {
        if !target_ids.contains(n.node_id.as_str()) {
            if let Some(pid) = n.parent_id.as_deref() {
                orphan_by_parent.entry(pid).or_default().push(n);
            }
        }
    }
    for children in orphan_by_parent.values_mut() {
        children.sort_by_key(|n| n.seq_order);
    }

    fn push_subtree<'a>(
        node_id: &str,
        orphan_by_parent: &BTreeMap<&'a str, Vec<&'a GateNodeRecord>>,
        live_ids: &BTreeSet<&str>,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) {
        if live_ids.contains(node_id) && seen.insert(node_id.to_owned()) {
            out.push(node_id.to_owned());
        }
        if let Some(children) = orphan_by_parent.get(node_id) {
            for child in children {
                push_subtree(&child.node_id, orphan_by_parent, live_ids, seen, out);
            }
        }
    }

    let first_tasks_idx = target
        .nodes
        .iter()
        .position(|n| n.node_id.starts_with("tasks#"));
    let first_post_idx = target.nodes.iter().enumerate().position(|(idx, n)| {
        first_tasks_idx.is_some_and(|t_idx| idx > t_idx)
            && n.parent_id.is_none()
            && !n.node_id.starts_with("tasks#")
    });
    let mut legacy_pre: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| is_pre_task_root_node(snapshot, n) && !target_ids.contains(n.node_id.as_str()))
        .collect();
    legacy_pre.sort_by_key(|n| n.seq_order);
    let mut legacy_tasks: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            ((n.kind == NodeKind::Container && n.node_id.starts_with("tasks#"))
                || n.kind == NodeKind::Task
                || n.kind == NodeKind::TaskGate)
                && !target_ids.contains(n.node_id.as_str())
        })
        .collect();
    legacy_tasks.sort_by_key(|n| n.seq_order);

    let mut ordered_ids = Vec::with_capacity(live_ids.len());
    let mut seen = BTreeSet::new();
    let mut open_ends: Vec<(usize, usize)> = Vec::new();
    let (mut flushed_pre, mut flushed_tasks) = (false, false);

    let flush_group =
        |group: &[&GateNodeRecord], seen: &mut BTreeSet<String>, out: &mut Vec<String>| {
            for n in group {
                push_subtree(&n.node_id, &orphan_by_parent, &live_ids, seen, out);
            }
        };

    for (idx, t_node) in target.nodes.iter().enumerate() {
        if !flushed_pre && (first_tasks_idx == Some(idx) || first_post_idx == Some(idx)) {
            flush_group(&legacy_pre, &mut seen, &mut ordered_ids);
            flushed_pre = true;
        }
        if !flushed_tasks && first_post_idx == Some(idx) {
            flush_group(&legacy_tasks, &mut seen, &mut ordered_ids);
            flushed_tasks = true;
        }
        if live_ids.contains(t_node.node_id.as_str()) && seen.insert(t_node.node_id.clone()) {
            ordered_ids.push(t_node.node_id.clone());
        }
        open_ends.push((idx, subtree_end[idx]));
        while open_ends.last().is_some_and(|&(_, end)| end == idx) {
            let (done_idx, _) = open_ends.pop().expect("checked non-empty");
            if let Some(orphans) = orphan_by_parent.get(target.nodes[done_idx].node_id.as_str()) {
                flush_group(orphans, &mut seen, &mut ordered_ids);
            }
        }
    }
    if !flushed_pre {
        flush_group(&legacy_pre, &mut seen, &mut ordered_ids);
    }
    if !flushed_tasks {
        flush_group(&legacy_tasks, &mut seen, &mut ordered_ids);
    }
    let mut remaining: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| !seen.contains(&n.node_id))
        .collect();
    remaining.sort_by_key(|n| n.seq_order);
    flush_group(&remaining, &mut seen, &mut ordered_ids);

    let mut seq_updates = Vec::new();
    for (next_seq, nid) in ordered_ids.into_iter().enumerate() {
        let seq = next_seq as u32;
        if let Some(existing) = snapshot.nodes.iter().find(|n| n.node_id == nid) {
            if existing.seq_order != seq {
                seq_updates.push((nid, seq));
            }
        } else if let Some(added) = added_nodes.iter_mut().find(|n| n.node_id == nid) {
            added.seq_order = seq;
        }
    }
    seq_updates
}

fn has_pre_task_tasks_fanout_artifact(snapshot: &RunGraphSnapshot) -> bool {
    let completed_pre: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| is_pre_task_root_node(snapshot, n) && n.status == NodeStatus::Completed)
        .map(|n| n.node_id.as_str())
        .collect();
    !completed_pre.is_empty()
        && snapshot.artifacts.iter().any(|art| {
            completed_pre.contains(art.node_id.as_str())
                && matches!(art.kind, ArtifactKind::Proof | ArtifactKind::Receipt)
                && serde_json::from_str::<Value>(&art.payload_json)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .is_some_and(|obj| obj.contains_key("tasks"))
        })
}
