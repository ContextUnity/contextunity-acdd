//! Pure gate submission (`pass` vs `reject`), `independent_from` checks, trace freshness,
//! `immutable` / `need:` drift detection, `receipt: true` container closure, and DAG advancement.

use crate::machine::graph::{
    active_claim_for_node, build_preconditions, container_scope, find_node,
    is_post_task_root_node, is_pre_task_root_node, resolve_gate_def,
};
use crate::machine::inputs::{
    MachineGateDef, MachineProfileInput, MachineProofKind, SubmitEvaluation, SubmitInput,
};
use crate::machine::schedule::{
    all_non_cancelled_tasks_completed, check_post_task_integration_barrier,
};
use crate::machine::verify::detect_scope_violations;
use crate::types::{
    ArtifactKind, BlackboardStatus, BlackboardStatusPatch, BlackboardTopic, ClaimOutcome,
    ClaimOutcomePatch, DependencySatisfiedPatch, GateArtifactRecord, GateNodeRecord, IsolationMode,
    NodeKind, NodeStatus, NodeStatusPatch, PredecessorTreeExpectation, RejectionFindings,
    RejectionKind, RunGraphSnapshot, RunStatus, SubmitAction, SubtaskStatus, TraceStatus,
    TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateSelector, TaskRef, WorkerId};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Internal storage view of a persisted [`ArtifactKind::Findings`] JSON payload.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StoredFindingsArtifact {
    #[serde(flatten)]
    pub(crate) findings: RejectionFindings,
    #[serde(default)]
    pub(crate) reset_worker_context: bool,
    #[serde(default)]
    pub(crate) reset_gate_id: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) trace_row_id: Option<i64>,
}

/// Deserialize a persisted [`ArtifactKind::Findings`] JSON payload.
pub(crate) fn parse_findings_artifact(payload_json: &str) -> Option<StoredFindingsArtifact> {
    serde_json::from_str::<StoredFindingsArtifact>(payload_json).ok()
}

/// Evaluate a `submit` (`pass` or `reject`) purely against `snapshot`, `profile`, and `obs`.
pub fn evaluate_submit(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &SubmitInput,
) -> Result<SubmitEvaluation, StateError> {
    if snapshot.run.status != RunStatus::Active {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot submit gate in non-active run '{}' (status: {})",
                snapshot.run.run_id, snapshot.run.status
            ),
        ));
    }

    let node_id = input.node_cursor.to_node_id()?;
    let node = find_node(snapshot, &node_id)?;

    if !matches!(node.kind, NodeKind::RootGate | NodeKind::TaskGate) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot submit structural node '{}' (kind: {})",
                node_id, node.kind
            ),
        ));
    }

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(StateError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{node_id}' is not in_progress with an active claim (status: {})",
                node.status
            ),
        ));
    }

    if let Some(expected_rev) = input.claim_revision {
        if expected_rev != node.claim_revision {
            return Err(StateError::new(
                ErrorCode::StaleClaimRevision,
                format!(
                    "submit claim_revision {expected_rev} does not match active claim_revision {} on '{node_id}'",
                    node.claim_revision
                ),
            ));
        }
    }

    if node.worker_id.as_ref() != Some(&input.worker_id) {
        return Err(StateError::new(
            ErrorCode::TaskAlreadyClaimed,
            format!(
                "worker '{}' does not hold the active claim on '{node_id}'",
                input.worker_id
            ),
        ));
    }

    let active_claim = active_claim_for_node(snapshot, &node_id).ok_or_else(|| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("node '{node_id}' has no active gate_claims record"),
        )
    })?;

    let gate = resolve_gate_def(profile, node)?;

    // Enforce `independent_from` worker inequality on both pass and reject.
    enforce_independent_from(snapshot, node, gate, &input.worker_id)?;

    match input.action {
        SubmitAction::Reject => evaluate_reject(snapshot, node, gate, active_claim.claim_revision, obs, input),
        SubmitAction::Pass => evaluate_pass(
            snapshot,
            profile,
            obs,
            node,
            gate,
            active_claim.claim_revision,
            input,
        ),
    }
}

fn enforce_independent_from(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    worker_id: &WorkerId,
) -> Result<(), StateError> {
    for selector in &gate.independent_from {
        match selector {
            GateSelector::Local(gates) => {
                let target = resolve_local_selector(snapshot, node, gates)?;
                if let Some(author) = latest_accepted_or_current_worker(snapshot, target) {
                    if &author == worker_id {
                        return Err(StateError::new(
                            ErrorCode::TaskReviewNotIndependent,
                            format!(
                                "worker '{worker_id}' cannot satisfy independent_from '{selector}' on '{}' because they authored '{}'",
                                node.node_id, target.node_id
                            ),
                        ));
                    }
                }
            }
            GateSelector::AllTasksGate(gates) => {
                let gate_suffix = gates
                    .iter()
                    .map(|g| g.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                for task_node in &snapshot.nodes {
                    if task_node.kind != NodeKind::Task
                        || task_node.status == NodeStatus::Cancelled
                    {
                        continue;
                    }
                    let Some(ref task_ref) = task_node.task_ref else {
                        continue;
                    };
                    let expected_node_id = format!("task:{task_ref}:{gate_suffix}");
                    if let Some(target) = snapshot
                        .nodes
                        .iter()
                        .find(|n| n.node_id == expected_node_id)
                    {
                        if let Some(author) = latest_accepted_or_current_worker(snapshot, target) {
                            if &author == worker_id {
                                return Err(StateError::new(
                                    ErrorCode::TaskReviewNotIndependent,
                                    format!(
                                        "worker '{worker_id}' cannot satisfy independent_from '{selector}' on '{}' because they authored '{}'",
                                        node.node_id, target.node_id
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn latest_accepted_or_current_worker(
    snapshot: &RunGraphSnapshot,
    target: &GateNodeRecord,
) -> Option<WorkerId> {
    if let Some(claim) = snapshot
        .claims
        .iter()
        .filter(|c| c.node_id == target.node_id && c.outcome == ClaimOutcome::Passed)
        .max_by_key(|c| c.claim_revision)
    {
        return Some(claim.worker_id.clone());
    }
    target.worker_id.clone()
}

fn resolve_local_selector<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &GateNodeRecord,
    gates: &[acdd_core::ids::GateId],
) -> Result<&'a GateNodeRecord, StateError> {
    let joined = gates
        .iter()
        .map(|g| g.as_str())
        .collect::<Vec<_>>()
        .join(".");
    let target_node_id = match node.kind {
        NodeKind::TaskGate => {
            let task_ref = node.task_ref.as_ref().ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task_gate '{}' missing task_ref", node.node_id),
                )
            })?;
            format!("task:{task_ref}:{joined}")
        }
        NodeKind::RootGate => {
            if let Some(ref parent_id) = node.parent_id {
                format!("{parent_id}.{joined}")
            } else {
                format!("root:{joined}")
            }
        }
        _ => format!("root:{joined}"),
    };

    snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == target_node_id)
        .or_else(|| {
            // Fallback: try direct `root:<joined>` if local selector references a top-level root gate.
            let root_id = format!("root:{joined}");
            snapshot.nodes.iter().find(|n| n.node_id == root_id)
        })
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "selector '{joined}' referenced from '{}' does not match any gate node",
                    node.node_id
                ),
            )
        })
}

fn evaluate_reject(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    claim_revision: u32,
    obs: &ValidatedObservations,
    input: &SubmitInput,
) -> Result<SubmitEvaluation, StateError> {
    let findings = input.rejection_findings.as_ref().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "submit --action reject on '{}' requires structured rejection findings",
                node.node_id
            ),
        )
    })?;

    if findings.summary.trim().is_empty() || findings.items.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "rejection findings must include a non-empty summary and at least one finding item",
        ));
    }
    for (idx, item) in findings.items.iter().enumerate() {
        if item.message.trim().is_empty() || item.remediation.trim().is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "rejection finding item [{idx}] must include non-empty message and remediation"
                ),
            ));
        }
    }

    if let Some(ref rev) = input.review_evidence {
        validate_review_evidence(gate, rev, SubmitAction::Reject)?;
    }

    let latest_trace_row_id = snapshot
        .traces
        .iter()
        .filter(|t| t.node_id == node.node_id && t.claim_revision == claim_revision)
        .max_by_key(|t| t.trace_row_id.unwrap_or(0))
        .and_then(|t| t.trace_row_id);

    let reset_worker_context = findings.kind == RejectionKind::ArchitecturalDrift;
    let reset_gate_id =
        reset_worker_context.then(|| gate.fail.as_deref().unwrap_or(node.gate_id.as_str()));
    let findings_payload = json!({
        "kind": findings.kind,
        "reset_worker_context": reset_worker_context,
        "reset_gate_id": reset_gate_id,
        "summary": findings.summary,
        "blackboard_message_id": findings.blackboard_message_id,
        "items": findings.items,
        "trace_row_id": latest_trace_row_id,
    });

    let artifact = GateArtifactRecord {
        run_id: snapshot.run.run_id.clone(),
        node_id: node.node_id.clone(),
        claim_revision,
        kind: ArtifactKind::Findings,
        payload_json: serde_json::to_string(&findings_payload).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize findings artifact: {err}"),
            )
        })?,
        snapshot_commit: None,
        created_at: obs.now_utc.clone(),
    };

    let target_fail_id = if findings.kind == RejectionKind::ArchitecturalDrift {
        gate.fail_drift
            .as_ref()
            .filter(|id| {
                snapshot
                    .nodes
                    .iter()
                    .any(|n| n.parent_id == node.parent_id && n.gate_id == **id)
            })
            .or(gate.fail.as_ref())
    } else {
        gate.fail.as_ref()
    };

    let new_attempt = node.attempt + 1;
    let is_top_level_root = node.parent_id.is_none();
    let (new_root_cycle_count, new_cycle_count, cycle_exceeded) = if target_fail_id.is_some() {
        if is_top_level_root {
            let next_rc = snapshot.run.root_cycle_count + 1;
            (Some(next_rc), None, next_rc >= gate.max_attempts)
        } else {
            let next_cc = node.cycle_count + 1;
            (None, Some(next_cc), next_cc >= gate.max_attempts)
        }
    } else {
        (None, None, false)
    };

    let escalated = new_attempt >= gate.max_attempts || cycle_exceeded;

    let mut watch_ids = vec![node.node_id.as_str()];
    let mut node_patches = Vec::new();

    if escalated {
        let mut patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Escalated);
        patch.attempt = Some(new_attempt);
        patch.cycle_count = new_cycle_count;
        patch.worker_id = Some(None);
        patch.session_fingerprint = Some(None);
        patch.active_span_id = Some(None);
        node_patches.push(patch);
    } else if let Some(fail_id) = target_fail_id {
        let target = snapshot
            .nodes
            .iter()
            .find(|n| n.parent_id == node.parent_id && n.gate_id == *fail_id)
            .ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "fail target '{fail_id}' not found in container of '{}'",
                        node.node_id
                    ),
                )
            })?;
        watch_ids.push(target.node_id.as_str());

        let mut target_patch = NodeStatusPatch::status_only(&target.node_id, NodeStatus::Ready);
        target_patch.accepted_tree = Some(None);
        target_patch.accepted_definition_digest = Some(None);
        target_patch.worker_id = Some(None);
        target_patch.session_fingerprint = Some(None);
        target_patch.active_span_id = Some(None);
        node_patches.push(target_patch);

        if target.seq_order <= node.seq_order {
            for mid in &snapshot.nodes {
                if mid.parent_id == node.parent_id
                    && mid.seq_order > target.seq_order
                    && mid.seq_order < node.seq_order
                {
                    watch_ids.push(mid.node_id.as_str());
                    let mut mid_patch =
                        NodeStatusPatch::status_only(&mid.node_id, NodeStatus::Pending);
                    mid_patch.accepted_tree = Some(None);
                    mid_patch.accepted_definition_digest = Some(None);
                    mid_patch.worker_id = Some(None);
                    mid_patch.session_fingerprint = Some(None);
                    mid_patch.active_span_id = Some(None);
                    node_patches.push(mid_patch);
                }
            }
        } else if let Some(ref fallback_fail_id) = gate.fail {
            // `fail_drift` targets placed after `node` (e.g. `rearchitect`): also reset
            // `gate.fail` (`build`) and intermediate gates (`< node.seq_order`) to `Pending`.
            let fb_node = snapshot
                .nodes
                .iter()
                .find(|n| n.parent_id == node.parent_id && n.gate_id == *fallback_fail_id)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "fail target '{fallback_fail_id}' not found in container of '{}'",
                            node.node_id
                        ),
                    )
                })?;
            for prev in &snapshot.nodes {
                if prev.parent_id == node.parent_id
                    && prev.seq_order >= fb_node.seq_order
                    && prev.seq_order < node.seq_order
                    && prev.node_id != target.node_id
                    && prev.node_id != node.node_id
                {
                    watch_ids.push(prev.node_id.as_str());
                    let mut prev_patch =
                        NodeStatusPatch::status_only(&prev.node_id, NodeStatus::Pending);
                    prev_patch.accepted_tree = Some(None);
                    prev_patch.accepted_definition_digest = Some(None);
                    prev_patch.worker_id = Some(None);
                    prev_patch.session_fingerprint = Some(None);
                    prev_patch.active_span_id = Some(None);
                    node_patches.push(prev_patch);
                }
            }
        }

        if target.node_id != node.node_id {
            let mut cur_patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Pending);
            cur_patch.attempt = Some(new_attempt);
            cur_patch.cycle_count = new_cycle_count;
            cur_patch.accepted_tree = Some(None);
            cur_patch.accepted_definition_digest = Some(None);
            cur_patch.worker_id = Some(None);
            cur_patch.session_fingerprint = Some(None);
            cur_patch.active_span_id = Some(None);
            node_patches.push(cur_patch);
        } else if let Some(first) = node_patches.first_mut() {
            first.attempt = Some(new_attempt);
            first.cycle_count = new_cycle_count;
        }
    } else {
        // Reject without explicit `fail:`: keep node ready for retry.
        let mut cur_patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Ready);
        cur_patch.attempt = Some(new_attempt);
        cur_patch.worker_id = Some(None);
        cur_patch.session_fingerprint = Some(None);
        cur_patch.active_span_id = Some(None);
        node_patches.push(cur_patch);
    }

    let preconditions = build_preconditions(snapshot, &watch_ids, Some(node));

    Ok(SubmitEvaluation::Apply(TransitionPlan {
        preconditions,
        bump_run_generation: true,
        new_run_status: None,
        new_root_cycle_count,
        completed_at: None,
        node_patches,
        dependency_patches: Vec::new(),
        claim_patches: vec![ClaimOutcomePatch {
            node_id: node.node_id.clone(),
            claim_revision,
            outcome: ClaimOutcome::Rejected,
            ended_at: obs.now_utc.clone(),
        }],
        new_artifacts: vec![artifact],
        new_blackboard: Vec::new(),
        blackboard_patches: Vec::new(),
        new_subtasks: Vec::new(),
        significant_transition: true,
    }))
}

fn evaluate_pass(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    claim_revision: u32,
    input: &SubmitInput,
) -> Result<SubmitEvaluation, StateError> {
    let current_tree = obs.current_tree.as_ref().ok_or_else(|| {
        StateError::new(
            ErrorCode::TaskCandidateMismatch,
            format!("submit --action pass on '{}' requires current_tree observation", node.node_id),
        )
    })?;

    // 1. Scope barrier check.
    let admitted_scope = container_scope(snapshot, node);
    let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();
    let violations =
        detect_scope_violations(snapshot, profile, obs, &admitted_scope, is_unscoped_root);
    if !violations.is_empty() {
        return Err(StateError::new(
            ErrorCode::OutOfScopeChanges,
            format!(
                "cannot submit '{}': working tree has out-of-scope changes: {}",
                node.node_id,
                violations.join(", ")
            ),
        ));
    }

    // 2. `need:` predecessor accepted_tree check (marks drifted predecessors stale BEFORE
    // returning any immutable-gate mismatch so drifted `need:` predecessors do not stay Completed).
    let mut required_predecessor_trees = Vec::new();
    let mut stale_node_ids = Vec::new();
    let mut stale_watch_ids = Vec::new();
    for selector in &gate.need {
        let preds = resolve_need_targets(snapshot, node, selector)?;
        for pred in preds {
            stale_watch_ids.push(pred.node_id.as_str());
            if pred.status != NodeStatus::Completed {
                stale_node_ids.push(pred.node_id.clone());
                continue;
            }
            match &pred.accepted_tree {
                Some(pred_tree) if pred_tree == current_tree => {
                    required_predecessor_trees.push(PredecessorTreeExpectation {
                        node_id: pred.node_id.clone(),
                        expected_accepted_tree: pred_tree.clone(),
                    });
                }
                _ => {
                    stale_node_ids.push(pred.node_id.clone());
                }
            }
        }
    }

    if !stale_node_ids.is_empty() {
        stale_node_ids.sort();
        stale_node_ids.dedup();
        let preconditions = build_preconditions(snapshot, &stale_watch_ids, None);
        return Ok(SubmitEvaluation::MarkStaleAndReject {
            error: StateError::new(
                ErrorCode::TaskCandidateMismatch,
                format!(
                    "predecessor node(s) [{}] drifted from current tree '{}' and were marked stale",
                    stale_node_ids.join(", "),
                    current_tree
                ),
            ),
            stale_node_ids,
            preconditions,
        });
    }

    // 3. `save.immutable: true` check (`current_tree == claim_tree`).
    if gate.save.immutable && node.claim_tree.as_ref() != Some(current_tree) {
        return Err(StateError::new(
            ErrorCode::TaskCandidateMismatch,
            format!(
                "immutable gate '{}' detected working-tree drift from claim_tree ({:?} != {})",
                node.node_id, node.claim_tree, current_tree
            ),
        ));
    }

    // 4. Trace freshness + `current_tree` check.
    let needs_trace = matches!(
        gate.proof,
        MachineProofKind::Command | MachineProofKind::Call
    ) || !gate.checks.is_empty()
        || gate.command.is_some()
        || gate.call.is_some();

    if needs_trace {
        let trace = snapshot
            .traces
            .iter()
            .filter(|t| {
                t.node_id == node.node_id
                    && t.claim_revision == claim_revision
                    && t.status == TraceStatus::Passed
            })
            .max_by_key(|t| t.trace_row_id.unwrap_or(0))
            .ok_or_else(|| {
                StateError::new(
                    ErrorCode::TaskProofUnobserved,
                    format!(
                        "missing fresh '{}' verification trace for '{}' at claim_revision {}",
                        TraceStatus::Passed,
                        node.node_id,
                        claim_revision
                    ),
                )
            })?;

        if trace.contract_revision != node.contract_revision {
            return Err(StateError::new(
                ErrorCode::TaskProofUnobserved,
                format!(
                    "trace contract_revision {} is stale (current contract_revision: {})",
                    trace.contract_revision, node.contract_revision
                ),
            ));
        }

        if let Some(ref expected_digest) = input.expected_execution_digest {
            if &trace.execution_digest != expected_digest {
                return Err(StateError::new(
                    ErrorCode::TaskProofUnobserved,
                    format!(
                        "trace execution_digest '{}' does not match current '{}'",
                        trace.execution_digest, expected_digest
                    ),
                ));
            }
        }

        let tree_after_ok = trace.candidate_tree_after.as_ref() == Some(current_tree);
        if &trace.candidate_tree_before != current_tree || !tree_after_ok {
            return Err(StateError::new(
                ErrorCode::TaskProofUnobserved,
                format!(
                    "trace candidate trees ({} -> {:?}) do not match current_tree '{}'",
                    trace.candidate_tree_before, trace.candidate_tree_after, current_tree
                ),
            ));
        }
    }

    // 5. Proof evidence validation (`review` / `CustomSchema`).
    let mut new_artifacts = Vec::new();
    match &gate.proof {
        MachineProofKind::Review => {
            let rev = input.review_evidence.as_ref().ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("proof: review on '{}' requires --evidence", node.node_id),
                )
            })?;
            validate_review_evidence(gate, rev, SubmitAction::Pass)?;
            new_artifacts.push(GateArtifactRecord {
                run_id: snapshot.run.run_id.clone(),
                node_id: node.node_id.clone(),
                claim_revision,
                kind: ArtifactKind::Proof,
                payload_json: serde_json::to_string(rev).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to serialize review evidence: {err}"),
                    )
                })?,
                snapshot_commit: input.snapshot_commit.clone(),
                created_at: obs.now_utc.clone(),
            });
        }
        MachineProofKind::CustomSchema(schema_name) => {
            let schema = profile.proof_schemas.get(schema_name).ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("proof schema '{schema_name}' is not declared in profile.proofs"),
                )
            })?;
            let val = input.custom_evidence.as_ref().ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "proof schema '{schema_name}' on '{}' requires --evidence JSON",
                        node.node_id
                    ),
                )
            })?;
            schema.validate_value(val, &format!("proof.{schema_name}"))?;
            new_artifacts.push(GateArtifactRecord {
                run_id: snapshot.run.run_id.clone(),
                node_id: node.node_id.clone(),
                claim_revision,
                kind: ArtifactKind::Proof,
                payload_json: serde_json::to_string(val).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to serialize custom proof evidence: {err}"),
                    )
                })?,
                snapshot_commit: input.snapshot_commit.clone(),
                created_at: obs.now_utc.clone(),
            });
        }
        _ => {}
    }

    // Record accepted_snapshot artifact for `sync --rebind-profile` immutability.
    let snapshot_payload = json!({
        "gate_id": gate.id.as_str(),
        "definition_digest": gate.definition_digest,
        "accepted_tree": current_tree.as_str(),
        "contract_revision": node.contract_revision,
    });
    new_artifacts.push(GateArtifactRecord {
        run_id: snapshot.run.run_id.clone(),
        node_id: node.node_id.clone(),
        claim_revision,
        kind: ArtifactKind::AcceptedSnapshot,
        payload_json: serde_json::to_string(&snapshot_payload).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize accepted_snapshot: {err}"),
            )
        })?,
        snapshot_commit: input.snapshot_commit.clone(),
        created_at: obs.now_utc.clone(),
    });

    // 6. Compute node, dependency, blackboard, and run transitions.
    let mut watch_ids = vec![node.node_id.as_str()];
    let mut node_patches = Vec::new();
    let mut dependency_patches = Vec::new();
    let blackboard_patches = Vec::new();
    let mut new_run_status = None;
    let mut completed_at = None;

    let mut complete_patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Completed);
    complete_patch.accepted_tree = Some(Some(current_tree.clone()));
    complete_patch.accepted_definition_digest = Some(Some(gate.definition_digest.clone()));
    complete_patch.worker_id = Some(Some(input.worker_id.clone()));
    complete_patch.active_span_id = Some(None);
    node_patches.push(complete_patch);

    // Advance `pass:` target inside the same container/level.
    if let Some(ref pass_id) = gate.pass {
        let next_node = snapshot
            .nodes
            .iter()
            .find(|n| n.parent_id == node.parent_id && n.gate_id == *pass_id)
            .ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "pass target '{pass_id}' not found in container of '{}'",
                        node.node_id
                    ),
                )
            })?;
        watch_ids.push(next_node.node_id.as_str());
        node_patches.push(NodeStatusPatch::status_only(&next_node.node_id, NodeStatus::Ready));
    }

    // Handle `receipt: true` container completion & readiness fan-out.
    if gate.receipt {
        match node.kind {
            NodeKind::TaskGate => {
                let parent_id = node.parent_id.as_deref().ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("task_gate '{}' has no parent task container", node.node_id),
                    )
                })?;
                let parent_task = find_node(snapshot, parent_id)?;
                let task_ref = parent_task.task_ref.as_ref().ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("task container '{parent_id}' missing task_ref"),
                    )
                })?;
                watch_ids.push(parent_task.node_id.as_str());

                // Verify all subtasks are completed/cancelled with non-empty evidence.
                for sub in snapshot.subtasks.iter().filter(|s| s.node_id == parent_task.node_id) {
                    if !matches!(sub.status, SubtaskStatus::Completed | SubtaskStatus::Cancelled) {
                        return Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!(
                                "cannot complete task '{task_ref}': subtask '{}' is '{}' (expected completed or cancelled)",
                                sub.subtask_ref, sub.status
                            ),
                        ));
                    }
                    let has_ev = sub
                        .evidence
                        .as_deref()
                        .is_some_and(|e| !e.trim().is_empty());
                    if !has_ev {
                        return Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!(
                                "cannot complete task '{task_ref}': subtask '{}' missing non-empty evidence",
                                sub.subtask_ref
                            ),
                        ));
                    }
                }

                let mut task_patch =
                    NodeStatusPatch::status_only(&parent_task.node_id, NodeStatus::Completed);
                task_patch.accepted_tree = Some(Some(current_tree.clone()));
                let mut inv = parent_task.invariants.clone();
                for v in &input.verified_invariants {
                    if !inv.verified.contains(v) {
                        inv.verified.push(v.clone());
                    }
                }
                task_patch.invariants = Some(inv.clone());
                node_patches.push(task_patch);

                // Cancel unused fallback gates (such as `rearchitect`) still `Pending` in this task container.
                for sibling in &snapshot.nodes {
                    if sibling.kind == NodeKind::TaskGate
                        && sibling.parent_id.as_deref() == Some(parent_task.node_id.as_str())
                        && sibling.node_id != node.node_id
                        && sibling.status == NodeStatus::Pending
                    {
                        watch_ids.push(sibling.node_id.as_str());
                        node_patches.push(NodeStatusPatch::status_only(
                            &sibling.node_id,
                            NodeStatus::Cancelled,
                        ));
                    }
                }

                dependency_patches.push(DependencySatisfiedPatch {
                    depends_on_ref: task_ref.clone(),
                    dependent_task_ref: None,
                    satisfied: true,
                });

                // Record task receipt artifact.
                let receipt_payload = json!({
                    "run_id": snapshot.run.run_id.as_str(),
                    "node_cursor": input.node_cursor.to_string(),
                    "gate_id": gate.id.as_str(),
                    "worker_id": input.worker_id.as_str(),
                    "claim_revision": claim_revision,
                    "contract_revision": node.contract_revision,
                    "candidate_tree": current_tree.as_str(),
                    "committed_oid": input.snapshot_commit.as_ref().map(|c| c.as_str()),
                    "accepted_at": obs.now_utc,
                    "rollup": {
                        "verified_invariants": inv,
                        "review_summary": input.summary,
                    }
                });
                new_artifacts.push(GateArtifactRecord {
                    run_id: snapshot.run.run_id.clone(),
                    node_id: node.node_id.clone(),
                    claim_revision,
                    kind: ArtifactKind::Receipt,
                    payload_json: serde_json::to_string(&receipt_payload).map_err(|err| {
                        StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!("failed to serialize task receipt: {err}"),
                        )
                    })?,
                    snapshot_commit: input.snapshot_commit.clone(),
                    created_at: obs.now_utc.clone(),
                });

                // Unblock dependent tasks whose dependencies are now satisfied.
                unblock_newly_ready_tasks(
                    snapshot,
                    task_ref,
                    obs,
                    &mut watch_ids,
                    &mut node_patches,
                );

                // If all non-cancelled tasks are now completed and integrated, ready the first post-task root gate.
                let others_done = snapshot.nodes.iter().all(|n| {
                    if n.kind != NodeKind::Task
                        || n.node_id == parent_task.node_id
                        || n.status == NodeStatus::Cancelled
                    {
                        true
                    } else {
                        n.status == NodeStatus::Completed
                    }
                });
                if others_done
                    && (snapshot.run.isolation != IsolationMode::Worktree
                        || obs.unintegrated_tasks.is_empty())
                {
                    if let Some(first_post) = snapshot
                        .nodes
                        .iter()
                        .filter(|n| is_post_task_root_node(snapshot, n))
                        .min_by_key(|n| n.seq_order)
                    {
                        if first_post.status == NodeStatus::Pending {
                            watch_ids.push(first_post.node_id.as_str());
                            node_patches.push(NodeStatusPatch::status_only(
                                &first_post.node_id,
                                NodeStatus::Ready,
                            ));
                        }
                    }
                }
            }
            NodeKind::RootGate => {
                // Post-task root receipt requires all non-cancelled tasks completed & integrated.
                if is_post_task_root_node(snapshot, node) {
                    if !all_non_cancelled_tasks_completed(snapshot) {
                        return Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            "cannot close root receipt gate before all non-cancelled tasks complete",
                        ));
                    }
                    check_post_task_integration_barrier(snapshot, obs)?;
                }

                let receipt_payload = json!({
                    "run_id": snapshot.run.run_id.as_str(),
                    "node_cursor": input.node_cursor.to_string(),
                    "gate_id": gate.id.as_str(),
                    "worker_id": input.worker_id.as_str(),
                    "claim_revision": claim_revision,
                    "contract_revision": node.contract_revision,
                    "candidate_tree": current_tree.as_str(),
                    "committed_oid": input.snapshot_commit.as_ref().map(|c| c.as_str()),
                    "accepted_at": obs.now_utc,
                    "rollup": {
                        "verified_invariants": {
                            "applicable": input.verified_invariants,
                            "verified": input.verified_invariants,
                        },
                        "review_summary": input.summary,
                    }
                });
                new_artifacts.push(GateArtifactRecord {
                    run_id: snapshot.run.run_id.clone(),
                    node_id: node.node_id.clone(),
                    claim_revision,
                    kind: ArtifactKind::Receipt,
                    payload_json: serde_json::to_string(&receipt_payload).map_err(|err| {
                        StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!("failed to serialize root receipt: {err}"),
                        )
                    })?,
                    snapshot_commit: input.snapshot_commit.clone(),
                    created_at: obs.now_utc.clone(),
                });

                if gate.pass.is_none() {
                    new_run_status = Some(RunStatus::Completed);
                    completed_at = Some(obs.now_utc.clone());
                }
            }
            _ => {}
        }
    }

    // Handle pre-task root container completion (e.g., `blueprint_synthesis.review` with `pass: None`).
    if node.kind == NodeKind::RootGate && gate.pass.is_none() && is_pre_task_root_node(snapshot, node)
    {
        if let Some(ref parent_id) = node.parent_id {
            let sibling_incomplete = snapshot.nodes.iter().any(|n| {
                n.parent_id.as_deref() == Some(parent_id.as_str())
                    && n.node_id != node.node_id
                    && !matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled)
            });
            if !sibling_incomplete {
                watch_ids.push(parent_id.as_str());
                let mut parent_patch = NodeStatusPatch::status_only(parent_id, NodeStatus::Completed);
                parent_patch.accepted_tree = Some(Some(current_tree.clone()));
                node_patches.push(parent_patch);

                // Check if all pre-task root containers are now completed; if so, activate tasks# blocks & wave-0 tasks.
                let other_pre_incomplete = snapshot.nodes.iter().any(|n| {
                    is_pre_task_root_node(snapshot, n)
                        && n.node_id != node.node_id
                        && n.node_id != *parent_id
                        && !matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled)
                });
                if !other_pre_incomplete {
                    activate_tasks_after_pre_barrier(snapshot, obs, &mut watch_ids, &mut node_patches);
                }
            }
        } else {
            let other_pre_incomplete = snapshot.nodes.iter().any(|n| {
                is_pre_task_root_node(snapshot, n)
                    && n.node_id != node.node_id
                    && !matches!(n.status, NodeStatus::Completed | NodeStatus::Cancelled)
            });
            if !other_pre_incomplete {
                activate_tasks_after_pre_barrier(snapshot, obs, &mut watch_ids, &mut node_patches);
            }
        }
    }

    let mut preconditions = build_preconditions(snapshot, &watch_ids, Some(node));
    preconditions.required_predecessor_trees = required_predecessor_trees;

    Ok(SubmitEvaluation::Apply(TransitionPlan {
        preconditions,
        bump_run_generation: true,
        new_run_status,
        new_root_cycle_count: None,
        completed_at,
        node_patches,
        dependency_patches,
        claim_patches: vec![ClaimOutcomePatch {
            node_id: node.node_id.clone(),
            claim_revision,
            outcome: ClaimOutcome::Passed,
            ended_at: obs.now_utc.clone(),
        }],
        new_artifacts,
        new_blackboard: Vec::new(),
        blackboard_patches,
        new_subtasks: Vec::new(),
        significant_transition: true,
    }))
}

/// Pure helper to compute blackboard status patches when closing a container at `receipt: true`.
pub fn plan_container_blackboard_cleanup(
    snapshot: &RunGraphSnapshot,
    container_node_id: &str,
    items: &[crate::types::BlackboardRecord],
) -> Vec<BlackboardStatusPatch> {
    let owned_node_ids: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            n.node_id == container_node_id || n.parent_id.as_deref() == Some(container_node_id)
        })
        .map(|n| n.node_id.as_str())
        .collect();

    let mut patches = Vec::new();
    for item in items {
        let Some(id) = item.id else {
            continue;
        };
        if !matches!(
            item.status,
            BlackboardStatus::Open | BlackboardStatus::Acknowledged
        ) {
            continue;
        }
        let in_container = item
            .node_id
            .as_deref()
            .is_some_and(|nid| owned_node_ids.contains(nid));
        if !in_container {
            continue;
        }
        let new_status = match item.topic {
            BlackboardTopic::Notes | BlackboardTopic::Draft => BlackboardStatus::Resolved,
            BlackboardTopic::Deferred => BlackboardStatus::CarriedOver,
            _ => continue,
        };
        patches.push(BlackboardStatusPatch {
            id,
            status: new_status,
        });
    }
    patches
}

fn validate_review_evidence(
    gate: &MachineGateDef,
    rev: &crate::types::ReviewEvidence,
    expected_action: SubmitAction,
) -> Result<(), StateError> {
    if rev.decision != expected_action {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "review evidence decision '{}' does not match submit action '{}'",
                rev.decision, expected_action
            ),
        ));
    }
    if rev.summary.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "review evidence requires a non-empty summary",
        ));
    }

    let declared: BTreeSet<&str> = gate.contours.iter().map(String::as_str).collect();
    let provided: BTreeSet<&str> = rev.contours.keys().map(String::as_str).collect();
    if declared != provided {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "review evidence contours {:?} must match declared gate contours {:?} 1:1",
                provided, declared
            ),
        ));
    }

    if expected_action == SubmitAction::Pass {
        for (contour, res) in &rev.contours {
            if res.evidence.trim().is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "review evidence contour '{contour}' must provide non-empty evidence on pass"
                    ),
                ));
            }
        }
    }

    Ok(())
}

fn resolve_need_targets<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &GateNodeRecord,
    selector: &GateSelector,
) -> Result<Vec<&'a GateNodeRecord>, StateError> {
    match selector {
        GateSelector::Local(gates) => Ok(vec![resolve_local_selector(snapshot, node, gates)?]),
        GateSelector::AllTasksGate(gates) => {
            let suffix = gates
                .iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let mut out = Vec::new();
            for task_node in &snapshot.nodes {
                if task_node.kind != NodeKind::Task || task_node.status == NodeStatus::Cancelled {
                    continue;
                }
                if let Some(ref task_ref) = task_node.task_ref {
                    let nid = format!("task:{task_ref}:{suffix}");
                    if let Some(found) = snapshot.nodes.iter().find(|n| n.node_id == nid) {
                        out.push(found);
                    }
                }
            }
            if out.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("need selector '{selector}' matched no task gates"),
                ));
            }
            Ok(out)
        }
    }
}

fn unblock_newly_ready_tasks<'a>(
    snapshot: &'a RunGraphSnapshot,
    just_completed: &TaskRef,
    obs: &ValidatedObservations,
    watch_ids: &mut Vec<&'a str>,
    node_patches: &mut Vec<NodeStatusPatch>,
) {
    let task_statuses: BTreeMap<&TaskRef, NodeStatus> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .filter_map(|n| {
            n.task_ref.as_ref().map(|tr| {
                let st = if tr == just_completed {
                    NodeStatus::Completed
                } else {
                    n.status
                };
                (tr, st)
            })
        })
        .collect();

    for task_node in &snapshot.nodes {
        if task_node.kind != NodeKind::Task || task_node.status != NodeStatus::Pending {
            continue;
        }
        let Some(ref tref) = task_node.task_ref else {
            continue;
        };
        let all_deps_ok = snapshot
            .dependencies
            .iter()
            .filter(|d| &d.task_ref == tref)
            .all(|dep| {
                crate::machine::schedule::is_dep_satisfied_against_statuses(
                    &snapshot.run.run_id,
                    dep,
                    |ref_id| task_statuses.get(ref_id).copied() == Some(NodeStatus::Completed),
                    obs,
                )
            });
        if all_deps_ok {
            watch_ids.push(task_node.node_id.as_str());
            node_patches.push(NodeStatusPatch::status_only(
                &task_node.node_id,
                NodeStatus::Ready,
            ));
            if let Some(first_gate) = snapshot
                .nodes
                .iter()
                .filter(|n| n.parent_id.as_deref() == Some(task_node.node_id.as_str()))
                .min_by_key(|n| n.seq_order)
            {
                if first_gate.status == NodeStatus::Pending {
                    watch_ids.push(first_gate.node_id.as_str());
                    node_patches.push(NodeStatusPatch::status_only(
                        &first_gate.node_id,
                        NodeStatus::Ready,
                    ));
                }
            }
        }
    }
}

fn activate_tasks_after_pre_barrier<'a>(
    snapshot: &'a RunGraphSnapshot,
    obs: &ValidatedObservations,
    watch_ids: &mut Vec<&'a str>,
    node_patches: &mut Vec<NodeStatusPatch>,
) {
    for block in &snapshot.nodes {
        if block.kind == NodeKind::Container
            && block.node_id.starts_with("tasks#")
            && block.status == NodeStatus::Pending
        {
            watch_ids.push(block.node_id.as_str());
            node_patches.push(NodeStatusPatch::status_only(
                &block.node_id,
                NodeStatus::Ready,
            ));
        }
    }

    let task_statuses: BTreeMap<&TaskRef, NodeStatus> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task)
        .filter_map(|n| n.task_ref.as_ref().map(|tr| (tr, n.status)))
        .collect();

    for task_node in &snapshot.nodes {
        if task_node.kind != NodeKind::Task || task_node.status != NodeStatus::Pending {
            continue;
        }
        let Some(ref tref) = task_node.task_ref else {
            continue;
        };
        let deps_ok = snapshot
            .dependencies
            .iter()
            .filter(|d| &d.task_ref == tref)
            .all(|dep| {
                crate::machine::schedule::is_dep_satisfied_against_statuses(
                    &snapshot.run.run_id,
                    dep,
                    |ref_id| task_statuses.get(ref_id).copied() == Some(NodeStatus::Completed),
                    obs,
                )
            });
        if deps_ok {
            watch_ids.push(task_node.node_id.as_str());
            node_patches.push(NodeStatusPatch::status_only(
                &task_node.node_id,
                NodeStatus::Ready,
            ));
            if let Some(first_gate) = snapshot
                .nodes
                .iter()
                .filter(|n| n.parent_id.as_deref() == Some(task_node.node_id.as_str()))
                .min_by_key(|n| n.seq_order)
            {
                if first_gate.status == NodeStatus::Pending {
                    watch_ids.push(first_gate.node_id.as_str());
                    node_patches.push(NodeStatusPatch::status_only(
                        &first_gate.node_id,
                        NodeStatus::Ready,
                    ));
                }
            }
        }
    }
}
