//! Pure verification evaluation, RFC-8785 `execution_digest` computation, scope barrier checks,
//! universal `proof: command` exit-code validation, and mechanical `fail:` routing.

use crate::machine::graph::{
    active_claim_for_node, build_preconditions, container_scope, resolve_gate_def,
};
use crate::machine::inputs::{
    ActionExecutionOutcome, MachineGateDef, MachineProfileInput, MachineProofKind, VerifyEvaluation,
    VerifyInput,
};
use crate::types::{
    ClaimOutcome, ClaimOutcomePatch, GateNodeRecord, NodeStatus, NodeStatusPatch,
    RunGraphSnapshot, TraceFinishInput, TraceStartInput, TraceStatus, TransitionPlan,
    ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::canonical_json_sha256;
use acdd_core::path_policy::WorkspacePathPolicy;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Structured inputs hashed via RFC-8785 SHA-256 into `gate_traces.execution_digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionDigestInput {
    /// Active `contract_revision` of the claimed node.
    pub contract_revision: u32,
    /// Canonical digest of the `GateDef`.
    pub gate_def_digest: String,
    /// Effective admitted scope (`scope ∪ extended_scope`).
    pub admitted_scope: Vec<String>,
    /// Deterministic resolved command lines/patterns.
    pub resolved_commands: Vec<String>,
    /// Optional `--test` target.
    pub test_target: Option<String>,
    /// JSON `with:` parameters passed to `call:`.
    pub call_with: Value,
    /// JSON check definitions/parameters (`[[qualified, params], ...]`).
    pub checks_params: Value,
    /// Optional live code-map index generation salt.
    pub index_generation: Option<String>,
    /// Optional live worktree files digest salt.
    pub live_files_digest: Option<String>,
}

/// Compute the deterministic RFC-8785 SHA-256 `execution_digest`.
pub fn compute_execution_digest(input: &ExecutionDigestInput) -> Result<String, StateError> {
    let payload = json!({
        "contract_revision": input.contract_revision,
        "gate_def_digest": input.gate_def_digest,
        "admitted_scope": input.admitted_scope,
        "resolved_commands": input.resolved_commands,
        "test_target": input.test_target,
        "call_with": input.call_with,
        "checks_params": input.checks_params,
        "index_generation": input.index_generation,
        "live_files_digest": input.live_files_digest,
    });
    Ok(canonical_json_sha256(&payload))
}

/// Evaluate a verification observation purely against the claimed node's gate contract.
pub fn evaluate_verify(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &VerifyInput,
) -> Result<VerifyEvaluation, StateError> {
    let node = crate::machine::require_active_claimable_node(
        snapshot,
        &input.node_cursor,
        "verify node",
    )?;
    let node_id = node.node_id.as_str();

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(StateError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{node_id}' must be in_progress with an active claim before verify (status: {}, claim_revision: {})",
                node.status, node.claim_revision
            ),
        ));
    }

    if let Some(expected_rev) = input.claim_revision {
        if expected_rev != node.claim_revision {
            return Err(StateError::new(
                ErrorCode::StaleClaimRevision,
                format!(
                    "verify claim_revision {expected_rev} does not match active claim_revision {} on '{node_id}'",
                    node.claim_revision
                ),
            ));
        }
    }

    if let Some(ref worker) = input.worker_id {
        if node.worker_id.as_ref() != Some(worker) {
            return Err(StateError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!(
                    "worker '{worker}' does not hold the active claim on '{node_id}'"
                ),
            ));
        }
    }

    let gate = resolve_gate_def(profile, node)?;
    let admitted_scope = container_scope(snapshot, node);
    let checks_params = Value::Array(
        gate.checks
            .iter()
            .map(|c| json!({ "name": c.qualified_name, "params": c.params }))
            .collect(),
    );

    let execution_digest = compute_execution_digest(&ExecutionDigestInput {
        contract_revision: node.contract_revision,
        gate_def_digest: gate.definition_digest.clone(),
        admitted_scope: admitted_scope.clone(),
        resolved_commands: input.resolved_commands.clone(),
        test_target: input.test_target.clone(),
        call_with: gate.with_params.clone(),
        checks_params,
        index_generation: input.index_generation.clone(),
        live_files_digest: input.live_files_digest.clone(),
    })?;

    let idempotency_key = gate.call.as_ref().map(|_| {
        format!(
            "{}:{}:{}:{}",
            snapshot.run.run_id, node.node_id, node.claim_revision, execution_digest
        )
    });

    let trace_start = TraceStartInput {
        run_id: snapshot.run.run_id.clone(),
        node_id: node.node_id.clone(),
        claim_revision: node.claim_revision,
        contract_revision: node.contract_revision,
        execution_digest: execution_digest.clone(),
        idempotency_key,
        command_id: input.command_id.clone(),
        command_line: input.command_line.clone(),
        action_id: input.action_id.clone(),
        test_target: input.test_target.clone(),
        candidate_tree_before: input.exec_obs.candidate_tree_before.clone(),
        log_relpath: input.log_relpath.clone(),
        started_at: input.started_at.clone(),
    };

    let (trace_status, outcome) = classify_verify_outcome(
        snapshot,
        profile,
        obs,
        node,
        gate,
        &admitted_scope,
        input,
    );

    let trace_finish = TraceFinishInput {
        trace_row_id: 0,
        status: trace_status,
        exit_code: input.exec_obs.exit_code,
        passed_tests: Vec::new(),
        failed_tests: Vec::new(),
        check_results: input.exec_obs.check_results.clone(),
        candidate_tree_after: input.exec_obs.candidate_tree_after.clone(),
        finished_at: obs.now_utc.clone(),
    };

    let preconditions = build_preconditions(snapshot, &[node.node_id.as_str()], Some(node));

    let failure_transition = if let Err(ref err) = outcome {
        if gate.role.is_none()
            && gate.fail.is_some()
            && err.code != ErrorCode::MutatingActionUncertainOutcome
        {
            Some(build_mechanical_failure_transition(
                snapshot, node, gate, obs,
            )?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(VerifyEvaluation {
        execution_digest,
        trace_start,
        trace_finish,
        preconditions,
        outcome,
        failure_transition,
    })
}

/// Resolve the effective `WorkspacePathPolicy` from shell observations (preserving configured
/// custom `state_db`/`code_db` overrides and active managed spec/archive paths).
pub(crate) fn effective_path_policy(
    snapshot: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> WorkspacePathPolicy {
    let mut policy = obs.path_policy.clone().unwrap_or_else(|| {
        WorkspacePathPolicy::new(
            snapshot.run.spec_path.iter().cloned(),
            ".acdd/state.sqlite",
            ".acdd/code-map.sqlite",
        )
    });
    if let Some(ref spec_path) = snapshot.run.spec_path {
        if let Some(norm) = acdd_core::path_policy::normalize_rel_path(spec_path) {
            policy.engine_managed_paths.insert(norm);
        }
    }
    policy
}

/// Check pure scope barrier for changed paths against `container_scope` and `protected_paths`.
pub fn detect_scope_violations(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    admitted_scope: &[String],
    is_unscoped_root_gate: bool,
) -> Vec<String> {
    let mut violations: BTreeSet<String> = obs.out_of_scope_paths.iter().cloned().collect();
    let policy = effective_path_policy(snapshot, obs);

    for path in &obs.changed_paths {
        let allowed = if is_unscoped_root_gate {
            policy.is_candidate_included(path)
                && !policy.is_protected_path(path, &profile.protected_paths)
        } else {
            policy.is_agent_writable(path, admitted_scope, &profile.protected_paths)
        };
        if !allowed {
            violations.insert(path.clone());
        }
    }
    violations.into_iter().collect()
}

fn classify_verify_outcome(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    admitted_scope: &[String],
    input: &VerifyInput,
) -> (TraceStatus, Result<(), StateError>) {
    let exec = &input.exec_obs;
    if let Some(ActionExecutionOutcome::UncertainCrash) = exec.action_outcome {
        return (
            TraceStatus::Aborted,
            Err(StateError::new(
                ErrorCode::MutatingActionUncertainOutcome,
                format!(
                    "mutating domain action crashed mid-execution on '{}' without recoverable lookup result",
                    node.node_id
                ),
            )),
        );
    }

    let is_unscoped_root =
        node.kind == crate::types::NodeKind::RootGate && admitted_scope.is_empty();
    let scope_violations =
        detect_scope_violations(snapshot, profile, obs, admitted_scope, is_unscoped_root);
    if !scope_violations.is_empty() {
        return (
            TraceStatus::ScopeViolation,
            Err(StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!(
                    "working tree changes outside admitted scope for '{}': {}",
                    node.node_id,
                    scope_violations.join(", ")
                ),
            )),
        );
    }

    if exec.aborted {
        return (
            TraceStatus::Aborted,
            Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("verification aborted on '{}'", node.node_id),
            )),
        );
    }
    if exec.timed_out {
        return (
            TraceStatus::TimedOut,
            Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("verification timed out on '{}'", node.node_id),
            )),
        );
    }

    if let Some(ActionExecutionOutcome::Failed(ref msg)) = exec.action_outcome {
        return (
            TraceStatus::Failed,
            Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("domain action failed on '{}': {msg}", node.node_id),
            )),
        );
    }

    match &gate.proof {
        MachineProofKind::Command
        | MachineProofKind::Call
        | MachineProofKind::None
        | MachineProofKind::Review
        | MachineProofKind::CustomSchema(_) => {
            if matches!(gate.proof, MachineProofKind::Command) || gate.command.is_some() {
                if let Err(err) = verify_command_success(node, input) {
                    return (TraceStatus::Failed, Err(err));
                }
            }
            if (matches!(gate.proof, MachineProofKind::Call) || gate.call.is_some())
                && !matches!(exec.action_outcome, Some(ActionExecutionOutcome::Succeeded))
            {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("gate '{}' requires successful call execution", node.node_id),
                    )),
                );
            }
            if let Err(err) = verify_required_checks(gate, &exec.check_results) {
                return (TraceStatus::CheckFailed, Err(err));
            }
            (TraceStatus::Passed, Ok(()))
        }
    }
}

fn verify_command_success(node: &GateNodeRecord, input: &VerifyInput) -> Result<(), StateError> {
    let exec = &input.exec_obs;
    if exec.exit_code != Some(0) {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "command on '{}' exited with {:?} (expected 0)",
                node.node_id, exec.exit_code
            ),
        ));
    }

    Ok(())
}

fn verify_required_checks(
    gate: &MachineGateDef,
    observed: &[crate::types::CheckResultRecord],
) -> Result<(), StateError> {
    let mut used = vec![false; observed.len()];
    for req in &gate.checks {
        let Some((domain, check)) = req.qualified_name.split_once('.') else {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid check identifier '{}'", req.qualified_name),
            ));
        };
        let Some(pos) = observed
            .iter()
            .enumerate()
            .position(|(idx, r)| !used[idx] && r.domain == domain && r.check == check)
        else {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("required domain check '{}' was not executed", req.qualified_name),
            ));
        };
        used[pos] = true;
        let rec = &observed[pos];
        if !rec.passed {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "domain check '{}' failed: {}",
                    req.qualified_name, rec.summary
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn reset_fail_target_and_intermediates<'a>(
    snapshot: &'a RunGraphSnapshot,
    node: &GateNodeRecord,
    fail_id: &str,
    clear_mid_claims: bool,
    watch_ids: &mut Vec<&'a str>,
    node_patches: &mut Vec<NodeStatusPatch>,
) -> Result<&'a GateNodeRecord, StateError> {
    let target = snapshot
        .nodes
        .iter()
        .find(|n| n.parent_id == node.parent_id && n.gate_id == fail_id)
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
                let mut mid_patch = NodeStatusPatch::status_only(&mid.node_id, NodeStatus::Pending);
                mid_patch.accepted_tree = Some(None);
                mid_patch.accepted_definition_digest = Some(None);
                if clear_mid_claims {
                    mid_patch.worker_id = Some(None);
                    mid_patch.session_fingerprint = Some(None);
                    mid_patch.active_span_id = Some(None);
                }
                node_patches.push(mid_patch);
            }
        }
    }
    Ok(target)
}

fn build_mechanical_failure_transition(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    obs: &ValidatedObservations,
) -> Result<TransitionPlan, StateError> {
    let fail_id = gate.fail.as_deref().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("gate '{}' missing fail target", node.node_id),
        )
    })?;

    let is_top_level_root = node.parent_id.is_none();
    let (new_root_cycle_count, new_node_cycle_count, exceeded) = if is_top_level_root {
        let next_cycle = snapshot.run.root_cycle_count + 1;
        (Some(next_cycle), None, next_cycle >= gate.max_attempts)
    } else {
        let next_cycle = node.cycle_count + 1;
        (None, Some(next_cycle), next_cycle >= gate.max_attempts)
    };

    let mut claim_patches = Vec::new();
    if let Some(active) = active_claim_for_node(snapshot, &node.node_id) {
        claim_patches.push(ClaimOutcomePatch {
            node_id: node.node_id.clone(),
            claim_revision: active.claim_revision,
            outcome: ClaimOutcome::Rejected,
            ended_at: obs.now_utc.clone(),
        });
    }

    let mut watch_ids = vec![node.node_id.as_str()];
    let mut node_patches = Vec::new();

    if exceeded {
        let mut patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Escalated);
        patch.cycle_count = new_node_cycle_count;
        patch.worker_id = Some(None);
        patch.session_fingerprint = Some(None);
        patch.active_span_id = Some(None);
        node_patches.push(patch);
    } else {
        let target = reset_fail_target_and_intermediates(
            snapshot,
            node,
            fail_id,
            false,
            &mut watch_ids,
            &mut node_patches,
        )?;

        if target.node_id != node.node_id {
            let mut cur_patch = NodeStatusPatch::status_only(&node.node_id, NodeStatus::Pending);
            cur_patch.cycle_count = new_node_cycle_count;
            cur_patch.worker_id = Some(None);
            cur_patch.session_fingerprint = Some(None);
            cur_patch.active_span_id = Some(None);
            node_patches.push(cur_patch);
        } else {
            // Self-loop fallback (rare, update cycle_count on target_patch).
            if let Some(first) = node_patches.first_mut() {
                first.cycle_count = new_node_cycle_count;
            }
        }
    }

    let preconditions = build_preconditions(snapshot, &watch_ids, Some(node));

    Ok(TransitionPlan {
        preconditions,
        bump_run_generation: true,
        new_run_status: None,
        new_root_cycle_count,
        completed_at: None,
        node_patches,
        dependency_patches: Vec::new(),
        claim_patches,
        new_artifacts: Vec::new(),
        new_blackboard: Vec::new(),
        blackboard_patches: Vec::new(),
        new_subtasks: Vec::new(),
        significant_transition: true,
    })
}

#[cfg(test)]
mod tests {
    use super::evaluate_verify;
    use crate::machine::inputs::{
        ActionExecutionOutcome, MachineGateDef, MachineGitPolicy, MachineGraphItem,
        MachineProfileInput, MachineProofKind, MachineTasksBlockDef, VerifyExecutionObservation,
        VerifyInput,
    };
    use crate::types::{
        ClaimOutcome, GateClaimRecord, GateNodeRecord, InvariantsState, IsolationMode, NodeKind,
        NodeStatus, RunGraphSnapshot, RunRecord, RunStatus, SaveAction, TraceStatus,
        ValidatedObservations,
    };
    use acdd_core::ids::{GateId, NodeCursor, RunId, RunName, TaskRef, TreeOid, WorkerId};
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn command_and_call_checks_verification_matrix() {
        let run_id = RunId::new("run-verify-call").expect("run_id");
        let t1 = TaskRef::new("T1").expect("task_ref");
        let build_gate = GateId::new("build").expect("gate_id");
        let node_id = "task:T1:build";
        let tree_oid = TreeOid::new("0".repeat(40)).expect("tree");

        let snapshot = RunGraphSnapshot {
            run: RunRecord {
                run_id: run_id.clone(),
                run_name: RunName::new("run-verify-call").expect("run_name"),
                collection: None,
                workspace: "/tmp/ws".to_owned(),
                profile_id: "code".to_owned(),
                profile_json: "{}".to_owned(),
                spec_path: None,
                spec_id: None,
                spec_digest: "d".to_owned(),
                is_ephemeral: false,
                isolation: IsolationMode::InPlace,
                target_base: None,
                target_base_commit: None,
                branch: None,
                worktree: None,
                trace_id: "t".to_owned(),
                status: RunStatus::Active,
                cancellation_reason: None,
                root_cycle_count: 0,
                generation: 1,
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                completed_at: None,
            },
            nodes: vec![
                GateNodeRecord {
                    run_id: run_id.clone(),
                    node_id: "task:T1".to_owned(),
                    parent_id: None,
                    kind: NodeKind::Task,
                    task_ref: Some(t1.clone()),
                    gate_id: "T1".to_owned(),
                    seq_order: 1,
                    status: NodeStatus::InProgress,
                    node_version: 1,
                    isolation: IsolationMode::InPlace,
                    attempt: 1,
                    cycle_count: 0,
                    max_attempts: 3,
                    claim_revision: 1,
                    contract_revision: 1,
                    worker_id: None,
                    session_fingerprint: None,
                    branch: None,
                    worktree: None,
                    baseline_commit: None,
                    claim_tree: Some(tree_oid.clone()),
                    accepted_tree: None,
                    accepted_definition_digest: None,
                    tags: vec![],
                    invariants: InvariantsState::default(),
                    scope: vec!["src/**".to_owned()],
                    extended_scope: vec![],
                    active_span_id: None,
                },
                GateNodeRecord {
                    run_id: run_id.clone(),
                    node_id: node_id.to_owned(),
                    parent_id: Some("task:T1".to_owned()),
                    kind: NodeKind::TaskGate,
                    task_ref: Some(t1.clone()),
                    gate_id: "build".to_owned(),
                    seq_order: 2,
                    status: NodeStatus::InProgress,
                    node_version: 1,
                    isolation: IsolationMode::InPlace,
                    attempt: 1,
                    cycle_count: 0,
                    max_attempts: 3,
                    claim_revision: 1,
                    contract_revision: 1,
                    worker_id: Some(WorkerId::new("w1").expect("worker_id")),
                    session_fingerprint: None,
                    branch: None,
                    worktree: None,
                    baseline_commit: None,
                    claim_tree: Some(tree_oid.clone()),
                    accepted_tree: None,
                    accepted_definition_digest: None,
                    tags: vec![],
                    invariants: InvariantsState::default(),
                    scope: vec![],
                    extended_scope: vec![],
                    active_span_id: None,
                },
            ],
            dependencies: vec![],
            claims: vec![GateClaimRecord {
                claim_id: Some(1),
                run_id,
                node_id: node_id.to_owned(),
                claim_revision: 1,
                worker_id: WorkerId::new("w1").expect("worker_id"),
                session_fingerprint: None,
                worktree: "/tmp/ws".to_owned(),
                span_id: "0123456789abcdef".to_owned(),
                claim_tree: tree_oid.clone(),
                claimed_at: "2026-01-01T00:00:00Z".to_owned(),
                ended_at: None,
                outcome: ClaimOutcome::Active,
            }],
            traces: vec![],
            artifacts: vec![],
            subtasks: vec![],
        };

        let obs = ValidatedObservations {
            current_tree: Some(tree_oid.clone()),
            container_tree: Some(tree_oid.clone()),
            now_utc: "2026-01-01T00:00:05Z".to_owned(),
            ..ValidatedObservations::default()
        };

        // Proof::Call with `command` enforces zero-exit, and duplicate checks match 1:1.
        use crate::machine::inputs::MachineCheckDef;
        use crate::types::CheckResultRecord;

        let chk = |passed: bool| CheckResultRecord {
            domain: "code".to_owned(),
            check: "seam".to_owned(),
            passed,
            summary: if passed { "ok".to_owned() } else { "fail".to_owned() },
            diagnostics: Vec::new(),
            execution_digest_salt: None,
        };
        let call_cases = [
            // (has_command, exit_code, action_outcome, checks_count, observed_checks, exp_pass, exp_trace)
            (true, Some(1), Some(ActionExecutionOutcome::Succeeded), 0, vec![], false, TraceStatus::Failed),
            (true, Some(0), Some(ActionExecutionOutcome::Succeeded), 0, vec![], true, TraceStatus::Passed),
            (false, None, Some(ActionExecutionOutcome::Succeeded), 2, vec![chk(true), chk(false)], false, TraceStatus::CheckFailed),
            (false, None, Some(ActionExecutionOutcome::Succeeded), 2, vec![chk(true)], false, TraceStatus::CheckFailed),
            (false, None, Some(ActionExecutionOutcome::Succeeded), 2, vec![chk(true), chk(true)], true, TraceStatus::Passed),
        ];

        for (idx, (has_cmd, exit_code, action_outcome, checks_count, check_results, exp_pass, exp_trace)) in
            call_cases.into_iter().enumerate()
        {
            let mut gate = MachineGateDef::minimal(build_gate.clone());
            gate.proof = MachineProofKind::Call;
            gate.call = Some("http.deploy".to_owned());
            gate.command = has_cmd.then(|| "build_check".to_owned());
            gate.checks = (0..checks_count)
                .map(|i| MachineCheckDef {
                    qualified_name: "code.seam".to_owned(),
                    params: json!({ "idx": i }),
                })
                .collect();
            let profile = MachineProfileInput {
                profile_id: "code".to_owned(),
                profile_json: "{}".to_owned(),
                git: MachineGitPolicy::default(),
                default_save: SaveAction::Commit,
                protected_paths: Vec::new(),
                tags: BTreeSet::from(["default".to_owned()]),
                proof_schemas: BTreeMap::new(),
                gates: vec![MachineGraphItem::TasksBlock(MachineTasksBlockDef {
                    when: None,
                    gates: vec![gate],
                })],
            };
            let input = VerifyInput {
                node_cursor: NodeCursor::task(t1.clone(), vec![build_gate.clone()]),
                worker_id: Some(WorkerId::new("w1").expect("worker_id")),
                claim_revision: Some(1),
                test_target: None,
                command_id: has_cmd.then(|| "build_check".to_owned()),
                command_line: has_cmd.then(|| "cargo test".to_owned()),
                resolved_commands: Vec::new(),
                action_id: Some("http.deploy".to_owned()),
                index_generation: None,
                live_files_digest: None,
                log_relpath: ".acdd/run/log.txt".to_owned(),
                started_at: "2026-01-01T00:00:00Z".to_owned(),
                exec_obs: VerifyExecutionObservation {
                    timed_out: false,
                    aborted: false,
                    exit_code,
                    stderr_tail: String::new(),
                    check_results,
                    action_outcome,
                    candidate_tree_before: tree_oid.clone(),
                    candidate_tree_after: Some(tree_oid.clone()),
                },
            };
            let eval = evaluate_verify(&snapshot, &profile, &obs, &input)
                .expect("evaluate_verify returns structured VerifyEvaluation");
            assert_eq!(eval.outcome.is_ok(), exp_pass, "call_case[{idx}]: {:?}", eval.outcome);
            assert_eq!(eval.trace_finish.status, exp_trace, "call_case[{idx}]");
        }

        // Custom state_db/code_db overrides and APFS case-variant protected_paths in detect_scope_violations.
        let custom_policy = acdd_core::path_policy::WorkspacePathPolicy::new(
            Vec::<String>::new(),
            "var/custom-state.sqlite",
            "var/custom-code.sqlite",
        );
        let mut custom_profile = MachineProfileInput {
            profile_id: "code".to_owned(),
            profile_json: "{}".to_owned(),
            git: MachineGitPolicy::default(),
            default_save: SaveAction::Commit,
            protected_paths: vec!["Cargo.toml".to_owned()],
            tags: BTreeSet::from(["default".to_owned()]),
            proof_schemas: BTreeMap::new(),
            gates: Vec::new(),
        };
        let scope = vec!["**".to_owned()];
        for &(changed, unscoped_root, expect_violation) in &[
            ("var/custom-state.sqlite", false, true),
            ("var/custom-state.sqlite-wal", false, true),
            ("VAR/CUSTOM-CODE.SQLITE", false, true),
            ("CARGO.TOML", false, true),
            ("CARGO.TOML", true, true),
            ("var/allowed.rs", false, false),
            ("var/allowed.rs", true, false),
        ] {
            let custom_obs = ValidatedObservations {
                changed_paths: vec![changed.to_owned()],
                path_policy: Some(custom_policy.clone()),
                ..ValidatedObservations::default()
            };
            let v = super::detect_scope_violations(
                &snapshot,
                &custom_profile,
                &custom_obs,
                &scope,
                unscoped_root,
            );
            assert_eq!(
                !v.is_empty(),
                expect_violation,
                "detect_scope_violations({changed:?}, unscoped={unscoped_root})"
            );
        }
        custom_profile.protected_paths.clear();
    }
}
