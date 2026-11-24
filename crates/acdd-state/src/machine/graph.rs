//! Pure graph construction, `NodeCursor`/`node_id` encoding, `when:` tag expression evaluation, and graph lookups.

use crate::machine::inputs::{
    BuildGraphInput, MachineGateDef, MachineGraphItem, MachineProfileInput, MachineTaskSpec,
};
use crate::types::{
    GateNodeRecord, InvariantsState, NodeKind, NodeStatus, NodeVersionExpectation,
    RunGraphSnapshot, RunRecord, RunStatus, StoragePreconditions, SubtaskRecord,
    TaskDependencyRecord,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, TaskRef};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) use crate::profile::when::WhenExpr;

/// Normalize a task's tags (`["default"]` when empty) and validate every tag against `profile.tags`.
pub fn effective_task_tags(
    raw_tags: &[String],
    allowed_tags: &BTreeSet<String>,
) -> Result<Vec<String>, StateError> {
    if raw_tags.is_empty() {
        return Ok(vec!["default".to_owned()]);
    }
    let mut out = Vec::with_capacity(raw_tags.len());
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for tag in raw_tags {
        if tag != "default" && !allowed_tags.contains(tag) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("unknown task tag '{tag}' is not declared in profile.tags"),
            ));
        }
        if seen.insert(tag.as_str()) {
            out.push(tag.clone());
        }
    }
    Ok(out)
}

/// Evaluate an optional `when:` expression against `task_tags` (`None` evaluates to `true`).
pub fn matches_when(
    when: Option<&str>,
    task_tags: &[String],
    allowed_tags: &BTreeSet<String>,
) -> Result<bool, StateError> {
    let Some(expr_raw) = when else {
        return Ok(true);
    };
    let expr = WhenExpr::parse(expr_raw)?;
    expr.validate_tags(allowed_tags, "when")?;
    Ok(expr.eval(task_tags))
}

/// Construct the initial `RunGraphSnapshot` from a profile, specification tasks, and run metadata.
pub fn build_initial_run_graph(
    profile: &MachineProfileInput,
    input: &BuildGraphInput,
) -> Result<RunGraphSnapshot, StateError> {
    validate_tasks_spec(&input.spec.tasks, &input.run_id, &profile.tags)?;

    let first_tasks_idx = profile
        .gates
        .iter()
        .position(|item| matches!(item, MachineGraphItem::TasksBlock(_)));
    let has_pre_task_root = first_tasks_idx.is_some_and(|idx| idx > 0);

    let mut builder = RunGraphBuilder {
        profile,
        input,
        nodes: Vec::new(),
        subtasks: Vec::new(),
        dependencies: Vec::new(),
        seq_counter: 0,
    };
    let mut assigned_tasks: BTreeSet<TaskRef> = BTreeSet::new();
    let mut tasks_block_idx = 0usize;
    let mut activated_first_pre_root = false;

    for (top_idx, item) in profile.gates.iter().enumerate() {
        match item {
            MachineGraphItem::Gate(gate) => {
                if gate.when.is_some() {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("when: is forbidden on root gate '{}'", gate.id),
                    ));
                }
                let is_pre_task = first_tasks_idx.is_some_and(|idx| top_idx < idx);
                let activate_entry =
                    !activated_first_pre_root && (is_pre_task || input.spec.tasks.is_empty());
                if activate_entry {
                    activated_first_pre_root = true;
                }
                builder.append_root_gate_nodes(gate, None, Vec::new(), activate_entry)?;
            }
            MachineGraphItem::TasksBlock(block) => {
                let block_node_id = NodeCursor::tasks_block_node_id(tasks_block_idx);
                let block_seq = builder.seq_counter;
                builder.seq_counter += 1;

                let mut matched_tasks: Vec<&MachineTaskSpec> = Vec::new();
                for task in &input.spec.tasks {
                    let tags = effective_task_tags(&task.tags, &profile.tags)?;
                    if matches_when(block.when.as_deref(), &tags, &profile.tags)? {
                        if !assigned_tasks.insert(task.task_ref.clone()) {
                            return Err(StateError::new(
                                ErrorCode::ProfileSchemaInvalid,
                                format!(
                                    "task '{}' matches multiple '- tasks:' blocks in profile '{}'",
                                    task.task_ref, profile.profile_id
                                ),
                            ));
                        }
                        matched_tasks.push(task);
                    }
                }

                let block_status = if !has_pre_task_root && !matched_tasks.is_empty() {
                    NodeStatus::Ready
                } else {
                    NodeStatus::Pending
                };

                builder.nodes.push(GateNodeRecord {
                    run_id: input.run_id.clone(),
                    node_id: block_node_id.clone(),
                    parent_id: None,
                    kind: NodeKind::Container,
                    task_ref: None,
                    gate_id: block_node_id.clone(),
                    seq_order: block_seq,
                    status: block_status,
                    node_version: 1,
                    isolation: profile.git.isolation,
                    attempt: 0,
                    cycle_count: 0,
                    max_attempts: 1,
                    claim_revision: 0,
                    contract_revision: 1,
                    worker_id: None,
                    session_fingerprint: None,
                    branch: None,
                    worktree: None,
                    baseline_commit: None,
                    claim_tree: None,
                    accepted_tree: None,
                    accepted_definition_digest: None,
                    tags: vec!["default".to_owned()],
                    invariants: InvariantsState::default(),
                    scope: Vec::new(),
                    extended_scope: Vec::new(),
                    active_span_id: None,
                });

                for task in matched_tasks {
                    builder.append_task_nodes(
                        task,
                        &block_node_id,
                        &block.gates,
                        !has_pre_task_root,
                    )?;
                }

                tasks_block_idx += 1;
            }
        }
    }

    for task in &input.spec.tasks {
        if !assigned_tasks.contains(&task.task_ref) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "task '{}' did not match any '- tasks:' block in profile '{}'",
                    task.task_ref, profile.profile_id
                ),
            ));
        }
    }

    let run = RunRecord {
        run_id: input.run_id.clone(),
        run_name: input.run_name.clone(),
        collection: input.collection.clone(),
        workspace: input.workspace.clone(),
        profile_id: profile.profile_id.clone(),
        profile_json: profile.profile_json.clone(),
        spec_path: input.spec_path.clone(),
        spec_id: None,
        spec_digest: input.spec.spec_digest.clone(),
        is_ephemeral: input.is_ephemeral,
        isolation: profile.git.isolation,
        target_base: input.target_base.clone(),
        target_base_commit: input.target_base_commit.clone(),
        branch: input.branch.clone(),
        worktree: input.worktree.clone(),
        trace_id: input.trace_id.clone(),
        status: RunStatus::Active,
        cancellation_reason: None,
        root_cycle_count: 0,
        generation: 1,
        created_at: input.created_at.clone(),
        completed_at: None,
    };

    let RunGraphBuilder {
        nodes,
        subtasks,
        dependencies,
        ..
    } = builder;
    Ok(RunGraphSnapshot {
        run,
        nodes,
        dependencies,
        subtasks,
        claims: Vec::new(),
        traces: Vec::new(),
        artifacts: Vec::new(),
    })
}

struct RunGraphBuilder<'a> {
    profile: &'a MachineProfileInput,
    input: &'a BuildGraphInput,
    nodes: Vec<GateNodeRecord>,
    subtasks: Vec<SubtaskRecord>,
    dependencies: Vec<TaskDependencyRecord>,
    seq_counter: u32,
}

impl RunGraphBuilder<'_> {
    fn append_root_gate_nodes(
        &mut self,
        gate: &MachineGateDef,
        parent_id: Option<&str>,
        prefix_gates: Vec<acdd_core::ids::GateId>,
        activate_entry: bool,
    ) -> Result<(), StateError> {
        let profile = self.profile;
        let input = self.input;
        if gate.when.is_some() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("when: is forbidden on root gate '{}'", gate.id),
            ));
        }

        let mut path_gates = prefix_gates;
        path_gates.push(gate.id.clone());
        let node_id = NodeCursor::root(path_gates.clone())?.to_node_id()?;
        let seq = self.seq_counter;
        self.seq_counter += 1;

        let is_container = !gate.children.is_empty();
        let kind = if is_container {
            NodeKind::Container
        } else {
            NodeKind::RootGate
        };

        let status = if activate_entry {
            NodeStatus::Ready
        } else {
            NodeStatus::Pending
        };

        self.nodes.push(GateNodeRecord {
            run_id: input.run_id.clone(),
            node_id: node_id.clone(),
            parent_id: parent_id.map(str::to_owned),
            kind,
            task_ref: None,
            gate_id: gate.id.as_str().to_owned(),
            seq_order: seq,
            status,
            node_version: 1,
            isolation: profile.git.isolation,
            attempt: 0,
            cycle_count: 0,
            max_attempts: gate.max_attempts.max(1),
            claim_revision: 0,
            contract_revision: 1,
            worker_id: None,
            session_fingerprint: None,
            branch: input.branch.clone(),
            worktree: input.worktree.clone(),
            baseline_commit: input.target_base_commit.clone(),
            claim_tree: None,
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: vec!["default".to_owned()],
            invariants: InvariantsState::default(),
            scope: gate.scope.clone(),
            extended_scope: Vec::new(),
            active_span_id: None,
        });

        for (child_idx, child) in gate.children.iter().enumerate() {
            self.append_root_gate_nodes(
                child,
                Some(&node_id),
                path_gates.clone(),
                activate_entry && child_idx == 0,
            )?;
        }

        Ok(())
    }

    pub(crate) fn append_task_nodes(
        &mut self,
        task: &MachineTaskSpec,
        parent_block_id: &str,
        template_gates: &[MachineGateDef],
        pre_task_satisfied: bool,
    ) -> Result<(), StateError> {
        let profile = self.profile;
        let input = self.input;
        let tags = effective_task_tags(&task.tags, &profile.tags)?;
        let task_node_id = NodeCursor::task(task.task_ref.clone(), Vec::new()).to_node_id()?;
        let task_seq = self.seq_counter;
        self.seq_counter += 1;

        let mut matched_gates: Vec<&MachineGateDef> = Vec::new();
        for gate in template_gates {
            if matches_when(gate.when.as_deref(), &tags, &profile.tags)? {
                matched_gates.push(gate);
            }
        }
        if matched_gates.is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "task '{}' matched zero task gates in profile '{}'",
                    task.task_ref, profile.profile_id
                ),
            ));
        }

        let task_ready = pre_task_satisfied && task.depends_on.is_empty();
        let task_status = if task_ready {
            NodeStatus::Ready
        } else {
            NodeStatus::Pending
        };

        self.nodes.push(GateNodeRecord {
            run_id: input.run_id.clone(),
            node_id: task_node_id.clone(),
            parent_id: Some(parent_block_id.to_owned()),
            kind: NodeKind::Task,
            task_ref: Some(task.task_ref.clone()),
            gate_id: "__task__".to_owned(),
            seq_order: task_seq,
            status: task_status,
            node_version: 1,
            isolation: profile.git.isolation,
            attempt: 0,
            cycle_count: 0,
            max_attempts: 3,
            claim_revision: 0,
            contract_revision: 1,
            worker_id: None,
            session_fingerprint: None,
            branch: None,
            worktree: None,
            baseline_commit: None,
            claim_tree: None,
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: tags.clone(),
            invariants: InvariantsState {
                applicable: task.invariants.clone(),
                verified: Vec::new(),
            },
            scope: task.scope.clone(),
            extended_scope: task.extended_scope.clone(),
            active_span_id: None,
        });

        for (g_idx, gate) in matched_gates.iter().enumerate() {
            let gate_node_id =
                NodeCursor::task(task.task_ref.clone(), vec![gate.id.clone()]).to_node_id()?;
            let gate_seq = self.seq_counter;
            self.seq_counter += 1;
            let gate_status = if task_ready && g_idx == 0 {
                NodeStatus::Ready
            } else {
                NodeStatus::Pending
            };

            self.nodes.push(GateNodeRecord {
                run_id: input.run_id.clone(),
                node_id: gate_node_id,
                parent_id: Some(task_node_id.clone()),
                kind: NodeKind::TaskGate,
                task_ref: Some(task.task_ref.clone()),
                gate_id: gate.id.as_str().to_owned(),
                seq_order: gate_seq,
                status: gate_status,
                node_version: 1,
                isolation: profile.git.isolation,
                attempt: 0,
                cycle_count: 0,
                max_attempts: gate.max_attempts.max(1),
                claim_revision: 0,
                contract_revision: 1,
                worker_id: None,
                session_fingerprint: None,
                branch: None,
                worktree: None,
                baseline_commit: None,
                claim_tree: None,
                accepted_tree: None,
                accepted_definition_digest: None,
                tags: tags.clone(),
                invariants: InvariantsState {
                    applicable: task.invariants.clone(),
                    verified: Vec::new(),
                },
                scope: task.scope.clone(),
                extended_scope: task.extended_scope.clone(),
                active_span_id: None,
            });
        }

        for (sub_idx, sub) in task.subtasks.iter().enumerate() {
            self.subtasks.push(SubtaskRecord {
                run_id: input.run_id.clone(),
                node_id: task_node_id.clone(),
                subtask_ref: sub.subtask_ref.clone(),
                seq_order: sub_idx as u32,
                title: sub.title.clone(),
                status: sub.status,
                evidence: sub.evidence.clone(),
                updated_at: input.created_at.clone(),
            });
        }

        for dep in &task.depends_on {
            self.dependencies.push(TaskDependencyRecord {
                run_id: input.run_id.clone(),
                task_ref: task.task_ref.clone(),
                depends_on_workspace: dep.depends_on_workspace.clone(),
                depends_on_run_id: dep.depends_on_run_id.clone(),
                depends_on_ref: dep.depends_on_ref.clone(),
                satisfied: false,
            });
        }

        Ok(())
    }
}

/// Validate task list uniqueness, tags, and acyclicity of intra-run `depends_on` edges.
pub(crate) fn validate_tasks_spec(
    tasks: &[MachineTaskSpec],
    run_id: &acdd_core::ids::RunId,
    allowed_tags: &BTreeSet<String>,
) -> Result<(), StateError> {
    let mut task_refs = BTreeSet::new();
    let mut adj: BTreeMap<&TaskRef, Vec<&TaskRef>> = BTreeMap::new();

    for task in tasks {
        if !task_refs.insert(&task.task_ref) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("duplicate task_ref '{}' in specification", task.task_ref),
            ));
        }
        let _ = effective_task_tags(&task.tags, allowed_tags)?;
        adj.entry(&task.task_ref).or_default();
    }

    for task in tasks {
        for dep in &task.depends_on {
            if dep.depends_on_run_id == *run_id {
                if dep.depends_on_ref == task.task_ref {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("task '{}' cannot depend on itself", task.task_ref),
                    ));
                }
                if !task_refs.contains(&&dep.depends_on_ref) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "task '{}' depends on unknown intra-run task '{}'",
                            task.task_ref, dep.depends_on_ref
                        ),
                    ));
                }
                adj.entry(&task.task_ref)
                    .or_default()
                    .push(&dep.depends_on_ref);
            }
        }
    }

    // Standard 3-color DFS cycle detection (0 = unvisited, 1 = visiting, 2 = visited).
    let mut state: BTreeMap<&TaskRef, u8> = BTreeMap::new();
    for task_ref in adj.keys() {
        if state.get(task_ref).copied().unwrap_or(0) == 0
            && has_dep_cycle(task_ref, &adj, &mut state)
        {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("cycle detected in task_dependencies involving '{task_ref}'"),
            ));
        }
    }
    Ok(())
}

fn has_dep_cycle<'a>(
    node: &'a TaskRef,
    adj: &BTreeMap<&'a TaskRef, Vec<&'a TaskRef>>,
    state: &mut BTreeMap<&'a TaskRef, u8>,
) -> bool {
    state.insert(node, 1);
    if let Some(deps) = adj.get(node) {
        for dep in deps {
            match state.get(dep).copied().unwrap_or(0) {
                1 => return true,
                0 if has_dep_cycle(dep, adj, state) => return true,
                _ => {}
            }
        }
    }
    state.insert(node, 2);
    false
}

/// Find a node record in `snapshot` by canonical `node_id`.
pub fn find_node<'a>(
    snapshot: &'a RunGraphSnapshot,
    node_id: &str,
) -> Result<&'a GateNodeRecord, StateError> {
    snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "node '{node_id}' does not exist in run '{}'",
                    snapshot.run.run_id
                ),
            )
        })
}

/// Resolve the `MachineGateDef` corresponding to a `GateNodeRecord`.
pub fn resolve_gate_def<'a>(
    profile: &'a MachineProfileInput,
    node: &GateNodeRecord,
) -> Result<&'a MachineGateDef, StateError> {
    match node.kind {
        NodeKind::RootGate | NodeKind::Container => {
            if let Ok(NodeCursor::Root(gates)) = NodeCursor::from_node_id(&node.node_id) {
                return find_root_gate_def(&profile.gates, &gates).ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "no gate definition for root node '{}' in profile '{}'",
                            node.node_id, profile.profile_id
                        ),
                    )
                });
            }
            Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "node '{}' is a structural tasks container, not a gate",
                    node.node_id
                ),
            ))
        }
        NodeKind::TaskGate => {
            let mut cursor_gates = match NodeCursor::from_node_id(&node.node_id)? {
                NodeCursor::Task { gates, .. } => gates,
                NodeCursor::Root(_) => Vec::new(),
            };
            let target_gate_id = cursor_gates.pop().ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task_gate '{}' has empty gate path", node.node_id),
                )
            })?;
            for item in &profile.gates {
                if let MachineGraphItem::TasksBlock(block) = item {
                    if let Some(found) = block.gates.iter().find(|g| g.id == target_gate_id) {
                        return Ok(found);
                    }
                }
            }
            Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "no task gate definition for '{}' in profile '{}'",
                    node.node_id, profile.profile_id
                ),
            ))
        }
        NodeKind::Task => Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "task container '{}' has no single gate definition",
                node.node_id
            ),
        )),
    }
}

fn find_root_gate_def<'a>(
    items: &'a [MachineGraphItem],
    path: &[acdd_core::ids::GateId],
) -> Option<&'a MachineGateDef> {
    let first = path.first()?;
    for item in items {
        if let MachineGraphItem::Gate(gate) = item {
            if &gate.id == first {
                return descend_gate_def(gate, &path[1..]);
            }
        }
    }
    None
}

fn descend_gate_def<'a>(
    gate: &'a MachineGateDef,
    rest: &[acdd_core::ids::GateId],
) -> Option<&'a MachineGateDef> {
    if rest.is_empty() {
        return Some(gate);
    }
    let next = &rest[0];
    let child = gate.children.iter().find(|c| &c.id == next)?;
    descend_gate_def(child, &rest[1..])
}

/// Return the effective admitted scope (`scope ∪ extended_scope`) for a node.
///
/// For `TaskGate`, inherits the owning `Task` container's `scope ∪ extended_scope` if present.
/// For `RootGate`, returns the gate's declared `scope` (empty means full candidate tree).
pub fn container_scope(snapshot: &RunGraphSnapshot, node: &GateNodeRecord) -> Vec<String> {
    let mut combined = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let source = match node.kind {
        NodeKind::TaskGate => {
            if let Some(ref parent_id) = node.parent_id {
                snapshot
                    .nodes
                    .iter()
                    .find(|n| &n.node_id == parent_id)
                    .unwrap_or(node)
            } else {
                node
            }
        }
        _ => node,
    };

    for s in &source.scope {
        if seen.insert(s.as_str()) {
            combined.push(s.clone());
        }
    }
    for ext in &source.extended_scope {
        if seen.insert(ext.path.as_str()) {
            combined.push(ext.path.clone());
        }
    }
    combined
}

/// Return the minimum `seq_order` of any `tasks#<idx>` container block, if present.
pub(crate) fn first_tasks_block_seq(snapshot: &RunGraphSnapshot) -> Option<u32> {
    snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Container && n.node_id.starts_with("tasks#"))
        .map(|n| n.seq_order)
        .min()
}

/// Return the maximum `seq_order` of any `tasks#<idx>` container or task/task_gate node.
pub(crate) fn last_tasks_region_seq(snapshot: &RunGraphSnapshot) -> Option<u32> {
    snapshot
        .nodes
        .iter()
        .filter(|n| {
            (n.kind == NodeKind::Container && n.node_id.starts_with("tasks#"))
                || n.kind == NodeKind::Task
                || n.kind == NodeKind::TaskGate
        })
        .map(|n| n.seq_order)
        .max()
}

/// Return `true` if `node` is a post-task root gate (positioned after `tasks#...` blocks).
pub fn is_post_task_root_node(snapshot: &RunGraphSnapshot, node: &GateNodeRecord) -> bool {
    if !matches!(node.kind, NodeKind::RootGate | NodeKind::Container)
        || node.node_id.starts_with("tasks#")
    {
        return false;
    }
    match last_tasks_region_seq(snapshot) {
        Some(max_tasks_seq) => node.seq_order > max_tasks_seq,
        None => false,
    }
}

/// Return `true` if `node` is a pre-task root gate or container (positioned before `tasks#...`).
pub fn is_pre_task_root_node(snapshot: &RunGraphSnapshot, node: &GateNodeRecord) -> bool {
    if !matches!(node.kind, NodeKind::RootGate | NodeKind::Container)
        || node.node_id.starts_with("tasks#")
    {
        return false;
    }
    match first_tasks_block_seq(snapshot) {
        Some(min_tasks_seq) => node.seq_order < min_tasks_seq,
        None => false,
    }
}

/// Build `StoragePreconditions` asserting `runs.generation` and `node_version` for a set of nodes.
pub fn build_preconditions(
    snapshot: &RunGraphSnapshot,
    node_ids: &[&str],
    primary_node: Option<&GateNodeRecord>,
) -> StoragePreconditions {
    let mut seen = BTreeSet::new();
    let mut expected_node_versions = Vec::new();
    for &id in node_ids {
        if seen.insert(id) {
            if let Some(n) = snapshot.nodes.iter().find(|n| n.node_id == id) {
                expected_node_versions.push(NodeVersionExpectation {
                    node_id: n.node_id.clone(),
                    expected_version: n.node_version,
                });
            }
        }
    }
    StoragePreconditions {
        expected_run_generation: snapshot.run.generation,
        expected_node_versions,
        expected_claim_revision: primary_node.and_then(|n| {
            if n.claim_revision > 0 {
                Some(n.claim_revision)
            } else {
                None
            }
        }),
        expected_contract_revision: primary_node.map(|n| n.contract_revision),
        expected_worker_id: primary_node.and_then(|n| n.worker_id.clone()),
        required_predecessor_trees: Vec::new(),
    }
}

/// Return the currently active claim record (`outcome == Active`) for `node_id`, if any.
pub fn active_claim_for_node<'a>(
    snapshot: &'a RunGraphSnapshot,
    node_id: &str,
) -> Option<&'a crate::types::GateClaimRecord> {
    snapshot
        .claims
        .iter()
        .filter(|c| c.node_id == node_id && c.outcome == crate::types::ClaimOutcome::Active)
        .max_by_key(|c| c.claim_revision)
}
