//! Gate verification orchestration (`verify`): scope barrier, `DomainProvider` actions/checks,
//! command execution, `execution_digest` computation, and crash-safe trace persistence.

use crate::git_ops::snapshot::{current_scope_tree, diff_scope_changed_paths};
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    compute_execution_digest, container_scope, evaluate_verify, ExecutionDigestInput,
    ActionExecutionOutcome, MachineProfileInput, VerifyExecutionObservation, VerifyInput,
};
use crate::profile::{
    render_command_with_test_filter, resolve_command_for_scope, GateDef, ResolvedGateProfile,
};
use crate::runner::{execute_gate_commands, CommandExecReq};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    CheckResultRecord, GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, TraceFinishInput,
    TraceStartInput, TraceStatus, ValidatedObservations,
};
use crate::usecases::dto::{VerifyReq, VerifyResp};
use crate::usecases::lifecycle::{
    append_trace_event, build_path_policy, now_utc_iso, parse_cursor_for_snapshot,
    parse_profile_from_run, primary_workspace_root, resolve_active_run, resolve_node_worktree,
    to_machine_profile, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::StateEngineConfig;
use acdd_core::telemetry::TraceParent;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, NodeCursor};
use acdd_core::ports::{
    CompositeDomainProvider, DomainActionReq, DomainCheckOutcome, DomainCheckReq,
};
use serde_json::{json, Value};
use std::path::Path;

/// Execute `StateCommand::Verify` with full `repo.lock` -> `after_commit_maintenance` discipline.
pub fn handle_verify<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: VerifyReq,
) -> Result<VerifyResp, StateError> {
    handle_verify_dyn(config, backend, provider, req)
}

pub(crate) fn handle_verify_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: VerifyReq,
) -> Result<VerifyResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, layout| {
        verify_holding_repo_lock(config, backend, provider, layout, req)
    })
}

/// Core `verify` orchestration invoked while `repo.lock` is already held (also used by auto-verify in `submit`).
pub(crate) fn verify_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    req: VerifyReq,
) -> Result<VerifyResp, StateError> {
    let run = resolve_active_run(
        config,
        backend,
        req.run.as_deref(),
        req.worktree.as_deref(),
    )?;
    let snapshot = backend
        .load_run_graph(&run.run_id)?
        .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run graph missing"))?;
    let resolved_profile = parse_profile_from_run(&snapshot.run)?;
    let machine_profile = to_machine_profile(&resolved_profile)?;

    let cursor = parse_cursor_for_snapshot(&snapshot, &req.node)?;
    let node_id = cursor.to_node_id()?;
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node '{}' does not exist in run '{}'", req.node, run.run_id),
            )
        })?;

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(StateError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{node_id}' must be in_progress with an active claim before verify (status: {}, claim_revision: {})",
                node.status, node.claim_revision
            ),
        ));
    }
    if let Some(expected_rev) = req.claim_revision {
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
    if let Some(ref worker) = req.worker_id {
        if node.worker_id.as_ref() != Some(worker) {
            return Err(StateError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!("worker '{worker}' does not hold the active claim on '{node_id}'"),
            ));
        }
    }

    let gate_def = find_profile_gate_def(&resolved_profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let gate_id = GateId::new(&gate_def.id)?;
    let machine_gate = find_machine_gate(&machine_profile, node)?;

    let primary = primary_workspace_root(config);
    let target_worktree =
        resolve_node_worktree(config, &snapshot.run, node, req.worktree.as_deref());
    let policy = build_path_policy(config, backend, &[])?;
    let admitted_scope = container_scope(&snapshot, node);
    let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();

    let (resolved_commands, output_spec) = resolve_commands_and_output_spec(
        &resolved_profile,
        &gate_def,
        &admitted_scope,
        req.test_target.as_deref(),
    )?;

    let started_at = now_utc_iso();
    let log_relpath = default_log_relpath(&node_id, node.claim_revision);
    let candidate_tree_before = current_scope_tree(&target_worktree, None, &policy)?;
    let pre_changed_paths = diff_scope_changed_paths(
        &target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        &policy,
    )?;

    let pre_obs = ValidatedObservations {
        current_tree: Some(candidate_tree_before.clone()),
        changed_paths: pre_changed_paths.clone(),
        now_utc: started_at.clone(),
        ..ValidatedObservations::default()
    };

    let pre_violations = detect_scope_violations(
        &snapshot,
        &machine_profile,
        &pre_obs,
        &admitted_scope,
        is_unscoped_root,
    );
    if !pre_violations.is_empty() {
        let verify_input = VerifyInput {
            node_cursor: cursor.clone(),
            worker_id: req.worker_id.clone(),
            claim_revision: req.claim_revision,
            test_target: req.test_target.clone(),
            command_id: gate_def.command.clone(),
            command_line: resolved_commands.first().cloned(),
            resolved_commands,
            output_spec,
            action_id: gate_def.call.clone(),
            index_generation: None,
            live_files_digest: None,
            log_relpath: log_relpath.clone(),
            started_at,
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: None,
                stderr_tail: String::new(),
                compile_error_count: 0,
                tests_passed: 0,
                tests_failed: 0,
                tests_ignored: 0,
                passed_tests: Vec::new(),
                failed_tests: Vec::new(),
                ignored_tests: Vec::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before: candidate_tree_before.clone(),
                candidate_tree_after: Some(candidate_tree_before),
            },
        };
        return persist_and_finalize_verify(
            backend,
            layout,
            &primary,
            &snapshot,
            &machine_profile,
            &pre_obs,
            &verify_input,
            None,
        );
    }

    // 1. Run domain `checks:` before `call:` so any check salt is folded into `execution_digest`.
    let (check_results, live_files_digest) = run_gate_checks(
        provider,
        &snapshot,
        &cursor,
        &gate_id,
        &gate_def,
        &primary,
        &target_worktree,
        &admitted_scope,
        &pre_changed_paths,
        req.test_target.as_deref(),
    )?;

    let checks_params = Value::Array(
        machine_gate
            .checks
            .iter()
            .map(|c| json!({ "name": c.qualified_name, "params": c.params }))
            .collect(),
    );
    let execution_digest = compute_execution_digest(&ExecutionDigestInput {
        contract_revision: node.contract_revision,
        gate_def_digest: machine_gate.definition_digest.clone(),
        admitted_scope: admitted_scope.clone(),
        resolved_commands: resolved_commands.clone(),
        test_target: req.test_target.clone(),
        output_spec: output_spec.clone(),
        call_with: machine_gate.with_params.clone(),
        checks_params,
        index_generation: None,
        live_files_digest: live_files_digest.clone(),
    })?;

    // 2. Execute `call:` with pre-recorded `running` trace and crash-recovery idempotency.
    let (action_outcome, existing_running_trace_id) = execute_gate_call_if_configured(
        backend,
        provider,
        &snapshot,
        node,
        &cursor,
        &gate_id,
        &gate_def,
        &primary,
        &target_worktree,
        &admitted_scope,
        &execution_digest,
        &resolved_commands,
        req.test_target.as_deref(),
        &candidate_tree_before,
        &log_relpath,
        &started_at,
    )?;

    // 3. Execute `command:` (unless action already failed or crashed).
    let action_failed = matches!(
        action_outcome,
        Some(ActionExecutionOutcome::Failed(_) | ActionExecutionOutcome::UncertainCrash)
    );
    let cmd_outcome = if !action_failed {
        if let Some(ref cmd_id) = gate_def.command {
            let cmd_def = resolved_profile
                .commands
                .get(cmd_id)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("unknown command slot '{cmd_id}'"),
                    )
                })?
                .clone();
            let trace_parent = node.active_span_id.as_deref().map(|span_id| {
                format!("00-{}-{span_id}-01", snapshot.run.trace_id)
            });
            let _ = TraceParent::parse(trace_parent.as_deref().unwrap_or(""));
            Some(execute_gate_commands(&CommandExecReq {
                run_id: snapshot.run.run_id.clone(),
                node_cursor: cursor.clone(),
                claim_revision: node.claim_revision,
                worktree: target_worktree.clone(),
                primary_worktree: primary.clone(),
                command_id: cmd_id.clone(),
                command: cmd_def,
                scope: admitted_scope.clone(),
                test_target: req.test_target.clone(),
                limits: resolved_profile.limits.clone(),
                trace_parent,
            })?)
        } else {
            None
        }
    } else {
        None
    };

    // 4. Re-capture post-execution candidate tree and changed paths for scope barrier.
    let candidate_tree_after = current_scope_tree(&target_worktree, None, &policy)?;
    let post_changed_paths = diff_scope_changed_paths(
        &target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        &policy,
    )?;
    let post_obs = ValidatedObservations {
        current_tree: Some(candidate_tree_after.clone()),
        changed_paths: post_changed_paths,
        now_utc: now_utc_iso(),
        ..ValidatedObservations::default()
    };

    let (
        command_line,
        exit_code,
        timed_out,
        aborted,
        tests_passed,
        tests_failed,
        tests_ignored,
        passed_tests,
        failed_tests,
        ignored_tests,
        compile_error_count,
        stderr_tail,
        final_log_relpath,
    ) = match cmd_outcome {
        Some(c) => (
            Some(c.command_line),
            c.exit_code,
            c.timed_out,
            c.aborted,
            c.tests_passed,
            c.tests_failed,
            c.tests_ignored,
            c.passed_tests,
            c.failed_tests,
            c.ignored_tests,
            c.compile_error_count,
            c.stderr_tail,
            c.log_relpath,
        ),
        None => (
            resolved_commands.first().cloned(),
            None,
            false,
            false,
            0,
            0,
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
            String::new(),
            log_relpath,
        ),
    };

    let verify_input = VerifyInput {
        node_cursor: cursor,
        worker_id: req.worker_id,
        claim_revision: req.claim_revision,
        test_target: req.test_target,
        command_id: gate_def.command,
        command_line,
        resolved_commands,
        output_spec,
        action_id: gate_def.call,
        index_generation: None,
        live_files_digest,
        log_relpath: final_log_relpath,
        started_at,
        exec_obs: VerifyExecutionObservation {
            timed_out,
            aborted,
            exit_code,
            stderr_tail,
            compile_error_count,
            tests_passed,
            tests_failed,
            tests_ignored,
            passed_tests,
            failed_tests,
            ignored_tests,
            check_results,
            action_outcome,
            candidate_tree_before,
            candidate_tree_after: Some(candidate_tree_after),
        },
    };

    persist_and_finalize_verify(
        backend,
        layout,
        &primary,
        &snapshot,
        &machine_profile,
        &post_obs,
        &verify_input,
        existing_running_trace_id,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_and_finalize_verify(
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    verify_input: &VerifyInput,
    existing_running_trace_id: Option<i64>,
) -> Result<VerifyResp, StateError> {
    let eval = evaluate_verify(snapshot, machine_profile, obs, verify_input)?;

    exec_tx_with(backend, |tx| {
        let trace_row_id = match existing_running_trace_id {
            Some(id) => id,
            None => {
                let started = tx.start_trace(&eval.preconditions, &eval.trace_start)?;
                started.trace_row_id.ok_or_else(|| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        "start_trace returned no trace_row_id",
                    )
                })?
            }
        };
        let mut finish = eval.trace_finish.clone();
        finish.trace_row_id = trace_row_id;
        tx.finish_trace(&finish)?;

        if let Some(ref failure_plan) = eval.failure_transition {
            tx.apply_transition_plan(&snapshot.run.run_id, failure_plan)?;
        }
        Ok(())
    })?;

    let _ = append_trace_event(
        layout,
        primary,
        &snapshot.run.run_id,
        &json!({
            "kind": "gate_verify",
            "run_id": snapshot.run.run_id.as_str(),
            "node_cursor": verify_input.node_cursor.to_string(),
            "claim_revision": eval.trace_start.claim_revision,
            "execution_digest": eval.execution_digest,
            "trace_status": eval.trace_finish.status.as_str(),
            "timestamp": eval.trace_finish.finished_at,
        }),
    );

    eval.outcome?;

    Ok(VerifyResp {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: verify_input.node_cursor.clone(),
        claim_revision: eval.trace_start.claim_revision,
        trace_status: eval.trace_finish.status,
        execution_digest: eval.execution_digest,
        exit_code: eval.trace_finish.exit_code,
        tests_passed: eval.trace_finish.tests_passed,
        tests_failed: eval.trace_finish.tests_failed,
        check_results: eval.trace_finish.check_results,
        log_relpath: eval.trace_start.log_relpath,
        passed: true,
    })
}

/// Resolve deterministic command lines and canonical `output_spec` JSON for a gate's `command:`.
pub(crate) fn resolve_commands_and_output_spec(
    profile: &ResolvedGateProfile,
    gate_def: &GateDef,
    admitted_scope: &[String],
    test_target: Option<&str>,
) -> Result<(Vec<String>, Value), StateError> {
    let Some(ref cmd_id) = gate_def.command else {
        return Ok((Vec::new(), Value::Null));
    };
    let cmd_def = profile.commands.get(cmd_id).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("gate '{}' references unknown command '{cmd_id}'", gate_def.id),
        )
    })?;
    let invocations = resolve_command_for_scope(cmd_def, admitted_scope)?;
    let mut rendered = Vec::with_capacity(invocations.len());
    let mut output_specs = Vec::with_capacity(invocations.len());
    for inv in invocations {
        let cmd_str = render_command_with_test_filter(
            &inv.command,
            inv.test_filter.as_ref(),
            test_target,
        )?;
        rendered.push(cmd_str);
        output_specs.push(serde_json::to_value(&inv.output).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize output spec: {err}"),
            )
        })?);
    }
    let output_val = match output_specs.len() {
        0 => Value::Null,
        1 => output_specs.into_iter().next().unwrap_or(Value::Null),
        _ => Value::Array(output_specs),
    };
    Ok((rendered, output_val))
}

#[allow(clippy::too_many_arguments)]
fn run_gate_checks(
    provider: &CompositeDomainProvider,
    snapshot: &RunGraphSnapshot,
    cursor: &NodeCursor,
    gate_id: &GateId,
    gate_def: &GateDef,
    primary: &Path,
    target_worktree: &Path,
    admitted_scope: &[String],
    changed_paths: &[String],
    test_target: Option<&str>,
) -> Result<(Vec<CheckResultRecord>, Option<String>), StateError> {
    let mut results = Vec::with_capacity(gate_def.checks.len());
    let mut salts = Vec::new();
    let req = DomainCheckReq {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: gate_id.clone(),
        workspace_root: primary.to_path_buf(),
        worktree: target_worktree.to_path_buf(),
        admitted_scope: admitted_scope.to_vec(),
        changed_paths: changed_paths.to_vec(),
        test_target: test_target.map(str::to_owned),
    };

    for chk in &gate_def.checks {
        let (domain, check_name) = chk.check.split_once('.').ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("check '{}' must be qualified as '<domain>.<check>'", chk.check),
            )
        })?;
        match provider.run_check(&chk.check, &chk.with, &req) {
            Ok(DomainCheckOutcome::Passed {
                summary,
                execution_digest_salt,
            }) => {
                if let Some(ref salt) = execution_digest_salt {
                    salts.push(format!("{}:{salt}", chk.check));
                }
                results.push(CheckResultRecord {
                    domain: domain.to_owned(),
                    check: check_name.to_owned(),
                    passed: true,
                    summary,
                    diagnostics: Vec::new(),
                    execution_digest_salt,
                });
            }
            Ok(DomainCheckOutcome::Failed {
                summary,
                diagnostics,
            }) => {
                results.push(CheckResultRecord {
                    domain: domain.to_owned(),
                    check: check_name.to_owned(),
                    passed: false,
                    summary,
                    diagnostics,
                    execution_digest_salt: None,
                });
            }
            Err(err) => {
                if let Some(se) = StateError::from_anyhow(&err) {
                    return Err(se.clone());
                }
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("domain check '{}' errored: {err}", chk.check),
                ));
            }
        }
    }

    let combined_salt = if salts.is_empty() {
        None
    } else {
        Some(salts.join("|"))
    };
    Ok((results, combined_salt))
}

#[allow(clippy::too_many_arguments)]
fn execute_gate_call_if_configured(
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
    gate_id: &GateId,
    gate_def: &GateDef,
    primary: &Path,
    target_worktree: &Path,
    admitted_scope: &[String],
    execution_digest: &str,
    resolved_commands: &[String],
    test_target: Option<&str>,
    candidate_tree_before: &acdd_core::ids::TreeOid,
    log_relpath: &str,
    started_at: &str,
) -> Result<(Option<ActionExecutionOutcome>, Option<i64>), StateError> {
    let Some(ref call_ref) = gate_def.call else {
        return Ok((None, None));
    };

    let cap = provider.action_capability(call_ref)?;
    let with_params = gate_def.with.clone().unwrap_or_else(|| json!({}));
    provider.validate_action_ref(call_ref, &with_params)?;

    let idempotency_key = format!(
        "{}:{}:{}:{}",
        snapshot.run.run_id, node.node_id, node.claim_revision, execution_digest
    );

    if let Some(prev) = backend.latest_trace(&snapshot.run.run_id, &node.node_id, node.claim_revision)? {
        if prev.status == TraceStatus::Running {
            let prev_row_id = prev.trace_row_id.ok_or_else(|| {
                StateError::new(ErrorCode::BackendConflict, "running trace missing trace_row_id")
            })?;
            if cap.mutating && prev.idempotency_key.as_deref() == Some(idempotency_key.as_str()) {
                if cap.supports_result_lookup {
                    match provider.lookup_action_result(call_ref, &idempotency_key) {
                        Ok(Some(_recovered)) => {
                            return Ok((Some(ActionExecutionOutcome::Succeeded), Some(prev_row_id)));
                        }
                        Ok(None) | Err(_) => {
                            return Ok((
                                Some(ActionExecutionOutcome::UncertainCrash),
                                Some(prev_row_id),
                            ));
                        }
                    }
                } else {
                    return Ok((
                        Some(ActionExecutionOutcome::UncertainCrash),
                        Some(prev_row_id),
                    ));
                }
            } else {
                // Close unrelated stale running trace before starting a fresh one.
                exec_tx_with(backend, |tx| {
                    tx.finish_trace(&TraceFinishInput {
                        trace_row_id: prev_row_id,
                        status: TraceStatus::Aborted,
                        exit_code: None,
                        tests_passed: 0,
                        tests_failed: 0,
                        tests_ignored: 0,
                        passed_tests: Vec::new(),
                        failed_tests: Vec::new(),
                        check_results: Vec::new(),
                        candidate_tree_after: Some(candidate_tree_before.clone()),
                        finished_at: now_utc_iso(),
                    })
                })?;
            }
        }
    }

    // Record `running` trace BEFORE invoking the external action.
    let preconditions = crate::machine::graph::build_preconditions(
        snapshot,
        &[node.node_id.as_str()],
        Some(node),
    );
    let trace_row = exec_tx_with(backend, |tx| {
        tx.start_trace(
            &preconditions,
            &TraceStartInput {
                run_id: snapshot.run.run_id.clone(),
                node_id: node.node_id.clone(),
                claim_revision: node.claim_revision,
                contract_revision: node.contract_revision,
                execution_digest: execution_digest.to_owned(),
                idempotency_key: Some(idempotency_key.clone()),
                command_id: gate_def.command.clone(),
                command_line: resolved_commands.first().cloned(),
                action_id: Some(call_ref.clone()),
                test_target: test_target.map(str::to_owned),
                candidate_tree_before: candidate_tree_before.clone(),
                log_relpath: log_relpath.to_owned(),
                started_at: started_at.to_owned(),
            },
        )
    })?;
    let trace_row_id = trace_row.trace_row_id.ok_or_else(|| {
        StateError::new(ErrorCode::BackendConflict, "start_trace missing row id")
    })?;

    acdd_core::failpoint::hit("verify_after_start_trace");

    let action_req = DomainActionReq {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: gate_id.clone(),
        worker_id: node.worker_id.clone(),
        workspace_root: primary.to_path_buf(),
        worktree: target_worktree.to_path_buf(),
        admitted_scope: admitted_scope.to_vec(),
    };

    let outcome = match provider.execute_action(call_ref, &with_params, &idempotency_key, &action_req) {
        Ok(_res) => ActionExecutionOutcome::Succeeded,
        Err(err) => {
            if let Some(se) = StateError::from_anyhow(&err) {
                if matches!(
                    se.code,
                    ErrorCode::DomainCapabilityUnavailable | ErrorCode::ProfileSchemaInvalid
                ) {
                    let _ = exec_tx_with(backend, |tx| {
                        tx.finish_trace(&TraceFinishInput {
                            trace_row_id,
                            status: TraceStatus::Failed,
                            exit_code: None,
                            tests_passed: 0,
                            tests_failed: 0,
                            tests_ignored: 0,
                            passed_tests: Vec::new(),
                            failed_tests: Vec::new(),
                            check_results: Vec::new(),
                            candidate_tree_after: Some(candidate_tree_before.clone()),
                            finished_at: now_utc_iso(),
                        })
                    });
                    return Err(se.clone());
                }
            }
            ActionExecutionOutcome::Failed(err.to_string())
        }
    };

    Ok((Some(outcome), Some(trace_row_id)))
}

pub(crate) fn find_machine_gate<'a>(
    profile: &'a MachineProfileInput,
    node: &GateNodeRecord,
) -> Result<&'a crate::machine::MachineGateDef, StateError> {
    crate::machine::graph::resolve_gate_def(profile, node)
}

fn default_log_relpath(node_id: &str, claim_rev: u32) -> String {
    let slug: String = node_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("logs/{slug}-rev{claim_rev}.log")
}
