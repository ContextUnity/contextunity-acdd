//! Pure verification evaluation, RFC-8785 `execution_digest` computation, scope barrier checks,
//! `proof: contract` runtime-red validation, universal `proof: command` validation, and mechanical `fail:` routing.

use crate::machine::graph::{
    active_claim_for_node, build_preconditions, container_scope, find_node, resolve_gate_def,
};
use crate::machine::inputs::{
    ActionExecutionOutcome, MachineGateDef, MachineProfileInput, MachineProofKind, VerifyEvaluation,
    VerifyInput,
};
use crate::types::{
    canonical_json_sha256, ClaimOutcome, ClaimOutcomePatch, GateNodeRecord, NodeStatus,
    NodeStatusPatch, RunGraphSnapshot, RunStatus, TraceFinishInput, TraceStartInput, TraceStatus,
    TransitionPlan, ValidatedObservations,
};
use acdd_core::errors::{ErrorCode, StateError};
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
    /// Canonical JSON representation of `CommandOutputSpec`.
    pub output_spec: Value,
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
        "output_spec": input.output_spec,
        "call_with": input.call_with,
        "checks_params": input.checks_params,
        "index_generation": input.index_generation,
        "live_files_digest": input.live_files_digest,
    });
    canonical_json_sha256(&payload)
}

/// Evaluate a verification observation purely against the claimed node's gate contract.
pub fn evaluate_verify(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &VerifyInput,
) -> Result<VerifyEvaluation, StateError> {
    if snapshot.run.status != RunStatus::Active {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "cannot verify node in non-active run '{}' (status: {})",
                snapshot.run.run_id, snapshot.run.status
            ),
        ));
    }

    let node_id = input.node_cursor.to_node_id()?;
    let node = find_node(snapshot, &node_id)?;

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
        output_spec: input.output_spec.clone(),
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
        tests_passed: input.exec_obs.tests_passed,
        tests_failed: input.exec_obs.tests_failed,
        tests_ignored: input.exec_obs.tests_ignored,
        passed_tests: input.exec_obs.passed_tests.clone(),
        failed_tests: input.exec_obs.failed_tests.clone(),
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

/// Check pure scope barrier for changed paths against `container_scope` and `protected_paths`.
pub fn detect_scope_violations(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    admitted_scope: &[String],
    is_unscoped_root_gate: bool,
) -> Vec<String> {
    let mut violations: BTreeSet<String> = obs.out_of_scope_paths.iter().cloned().collect();
    let engine_managed: Vec<String> = snapshot.run.spec_path.iter().cloned().collect();
    let policy = WorkspacePathPolicy::new(
        engine_managed,
        ".acdd/state.sqlite",
        ".acdd/code-map.sqlite",
    );

    for path in &obs.changed_paths {
        let allowed = if is_unscoped_root_gate {
            policy.is_candidate_included(path)
                && !policy.is_scope_admitted(path, &profile.protected_paths)
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

    let exec = &input.exec_obs;
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
    if exec.compile_error_count > 0 {
        return (
            TraceStatus::CompileError,
            Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "detected {} compile error(s) during verification of '{}'",
                    exec.compile_error_count, node.node_id
                ),
            )),
        );
    }

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
        MachineProofKind::Contract => {
            let Some(target) = input
                .test_target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("proof: contract on '{}' requires --test <target>", node.node_id),
                    )),
                );
            };

            let Some(ref expected) = gate.expected_failure else {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "proof: contract on '{}' is missing expected_failure configuration",
                            node.node_id
                        ),
                    )),
                );
            };

            let exit_ok = exec
                .exit_code
                .is_some_and(|code| expected.exit_codes.contains(&code));
            if !exit_ok {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "contract test '{target}' exited with {:?}, expected one of {:?}",
                            exec.exit_code, expected.exit_codes
                        ),
                    )),
                );
            }

            if let Some(ref substr) = expected.stderr_contains {
                if !exec.stderr_tail.contains(substr) {
                    return (
                        TraceStatus::Failed,
                        Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!(
                                "contract stderr did not contain required pattern '{substr}'"
                            ),
                        )),
                    );
                }
            }

            if exec.ignored_tests.iter().any(|t| t == target)
                || !exec.failed_tests.iter().any(|t| t == target)
            {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "contract target '{target}' must appear in failed_tests (and not ignored_tests)"
                        ),
                    )),
                );
            }

            if let Err(err) = verify_required_checks(gate, &exec.check_results) {
                return (TraceStatus::CheckFailed, Err(err));
            }

            (TraceStatus::ExpectedFailure, Ok(()))
        }
        MachineProofKind::Command => {
            if let Err(err) = verify_command_success(snapshot, profile, node, gate, input) {
                return (TraceStatus::Failed, Err(err));
            }
            if let Err(err) = verify_required_checks(gate, &exec.check_results) {
                return (TraceStatus::CheckFailed, Err(err));
            }
            (TraceStatus::Passed, Ok(()))
        }
        MachineProofKind::Call => {
            if !matches!(exec.action_outcome, Some(ActionExecutionOutcome::Succeeded)) {
                return (
                    TraceStatus::Failed,
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("proof: call on '{}' requires successful action execution", node.node_id),
                    )),
                );
            }
            if let Err(err) = verify_required_checks(gate, &exec.check_results) {
                return (TraceStatus::CheckFailed, Err(err));
            }
            (TraceStatus::Passed, Ok(()))
        }
        MachineProofKind::None | MachineProofKind::Review | MachineProofKind::CustomSchema(_) => {
            if gate.command.is_some() {
                if let Err(err) = verify_command_success(snapshot, profile, node, gate, input) {
                    return (TraceStatus::Failed, Err(err));
                }
            }
            if gate.call.is_some()
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

fn verify_command_success(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    node: &GateNodeRecord,
    gate: &MachineGateDef,
    input: &VerifyInput,
) -> Result<(), StateError> {
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
    if exec.tests_failed > 0 {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "command on '{}' reported {} failing test(s)",
                node.node_id, exec.tests_failed
            ),
        ));
    }
    if gate.require_counts && exec.tests_passed == 0 {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "command on '{}' requires tests_passed > 0 (require_counts = true)",
                node.node_id
            ),
        ));
    }

    // If an earlier gate in the same container completed with `proof: contract`,
    // its `<test_target>` must appear in `passed_tests`.
    for prior_target in prior_contract_targets(snapshot, profile, node)? {
        if !exec.passed_tests.iter().any(|t| t == &prior_target) {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "command on '{}' did not pass prior contract target '{prior_target}'",
                    node.node_id
                ),
            ));
        }
    }

    Ok(())
}

/// Find any `<test_target>` proven by an earlier `proof: contract` gate in the same container.
pub fn prior_contract_targets(
    snapshot: &RunGraphSnapshot,
    profile: &MachineProfileInput,
    node: &GateNodeRecord,
) -> Result<Vec<String>, StateError> {
    let mut targets = Vec::new();
    for sibling in &snapshot.nodes {
        if sibling.parent_id != node.parent_id
            || sibling.seq_order >= node.seq_order
            || sibling.status != NodeStatus::Completed
        {
            continue;
        }
        let Ok(sib_gate) = resolve_gate_def(profile, sibling) else {
            continue;
        };
        if sib_gate.proof != MachineProofKind::Contract {
            continue;
        }
        if let Some(trace) = snapshot
            .traces
            .iter()
            .filter(|t| {
                t.node_id == sibling.node_id
                    && t.claim_revision == sibling.claim_revision
                    && t.status == TraceStatus::ExpectedFailure
            })
            .max_by_key(|t| t.trace_row_id.unwrap_or(0))
        {
            if let Some(ref target) = trace.test_target {
                if !targets.contains(target) {
                    targets.push(target.clone());
                }
            }
        }
    }
    Ok(targets)
}

fn verify_required_checks(
    gate: &MachineGateDef,
    observed: &[crate::types::CheckResultRecord],
) -> Result<(), StateError> {
    for req in &gate.checks {
        let Some((domain, check)) = req.qualified_name.split_once('.') else {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid check identifier '{}'", req.qualified_name),
            ));
        };
        let Some(rec) = observed
            .iter()
            .find(|r| r.domain == domain && r.check == check)
        else {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("required domain check '{}' was not executed", req.qualified_name),
            ));
        };
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

        // Reset any intermediate sibling gates strictly between target and current node.
        for mid in &snapshot.nodes {
            if mid.parent_id == node.parent_id
                && mid.seq_order > target.seq_order
                && mid.seq_order < node.seq_order
            {
                watch_ids.push(mid.node_id.as_str());
                let mut mid_patch = NodeStatusPatch::status_only(&mid.node_id, NodeStatus::Pending);
                mid_patch.accepted_tree = Some(None);
                mid_patch.accepted_definition_digest = Some(None);
                node_patches.push(mid_patch);
            }
        }

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
