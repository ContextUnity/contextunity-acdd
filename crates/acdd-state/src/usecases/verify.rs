//! Gate verification orchestration (`verify`): scope barrier, `DomainProvider` actions/checks,
//! command execution, `execution_digest` computation, and crash-safe trace persistence.

use crate::git_ops::snapshot::{current_scope_tree, diff_scope_changed_paths};
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    compute_execution_digest, container_scope, evaluate_verify, ActionExecutionOutcome,
    ExecutionDigestInput, MachineProfileInput, VerifyExecutionObservation, VerifyInput,
};
use crate::workflow::{resolve_command_for_scope, GateDef, ResolvedWorkflow};
use crate::runner::{execute_gate_commands, render_invocation_command, CommandExecReq};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    CheckResultRecord, GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, TraceFinishInput,
    TraceStartInput, TraceStatus, ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::dto::{VerifyReq, VerifyResp};
use crate::usecases::lifecycle::{
    acquire_repo_lock, append_trace_event, combine_check_observations, ensure_state_enabled,
    load_active_run_graph, parse_cursor_for_snapshot, primary_workspace_root,
    resolve_node_worktree, resolve_repo_layout_optional,
};
use crate::usecases::queries::find_profile_gate_def;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ids::{GateId, TreeOid};
use acdd_core::ports::{
    CompositeDomainProvider, DomainActionReq, DomainCheckOutcome, DomainCheckReq,
    FreshnessObservation,
};
use serde_json::{json, Value};

struct CapturedVerifyClaim {
    claim_revision: u32,
    contract_revision: u32,
    worker_id: Option<acdd_core::ids::WorkerId>,
    node_version: u64,
}

fn verify_active_node_cas<'a>(
    snapshot: &'a RunGraphSnapshot,
    node_id: &str,
    req: &VerifyReq,
    captured: Option<&CapturedVerifyClaim>,
) -> Result<&'a GateNodeRecord, AcddError> {
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "node '{}' does not exist in run '{}'",
                    req.node, snapshot.run.run_id
                ),
            )
        })?;

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(AcddError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{node_id}' must be in_progress with an active claim before verify (status: {}, claim_revision: {})",
                node.status, node.claim_revision
            ),
        ));
    }
    if let Some(expected_rev) = req.claim_revision {
        if expected_rev != node.claim_revision {
            return Err(AcddError::new(
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
            return Err(AcddError::new(
                ErrorCode::TaskAlreadyClaimed,
                format!("worker '{worker}' does not hold the active claim on '{node_id}'"),
            ));
        }
    }
    if let Some(cap) = captured {
        if node.claim_revision != cap.claim_revision
            || node.contract_revision != cap.contract_revision
            || node.worker_id != cap.worker_id
            || node.node_version != cap.node_version
        {
            return Err(AcddError::new(
                ErrorCode::StaleClaimRevision,
                format!(
                    "node '{node_id}' claim state changed concurrently during verify (claim_rev {}->{}, contract_rev {}->{}, version {}->{})",
                    cap.claim_revision,
                    node.claim_revision,
                    cap.contract_revision,
                    node.contract_revision,
                    cap.node_version,
                    node.node_version
                ),
            ));
        }
    }
    Ok(node)
}

pub(crate) fn handle_verify_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: VerifyReq,
) -> Result<VerifyResp, AcddError> {
    handle_verify_with_failure_return(config, backend, provider, req, false)
}

pub(crate) fn handle_verify_with_failure_return(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: VerifyReq,
    apply_failure_transition: bool,
) -> Result<VerifyResp, AcddError> {
    ensure_state_enabled(config)?;
    let primary = primary_workspace_root(config);
    let layout = resolve_repo_layout_optional(&primary)?;
    let persist_and_finalize_verify =
        |snapshot: &RunGraphSnapshot,
         machine_profile: &MachineProfileInput,
         obs: &ValidatedObservations,
         verify_input: &VerifyInput,
         existing_running_trace_id: Option<i64>| {
            let eval = evaluate_verify(snapshot, machine_profile, obs, verify_input)?;
            if let Err(err) = &eval.outcome {
                if err.code == ErrorCode::MutatingActionUncertainOutcome {
                    return Err(err.clone());
                }
            }

            let failure_transition = eval
                .failure_transition
                .as_ref()
                .filter(|_| apply_failure_transition);
            exec_tx_with(backend, |tx| {
                let trace_row_id = match existing_running_trace_id {
                    Some(id) => id,
                    None => {
                        let started = tx.start_trace(&eval.preconditions, &eval.trace_start)?;
                        started.trace_row_id.ok_or_else(|| {
                            AcddError::new(
                                ErrorCode::BackendConflict,
                                "start_trace returned no trace_row_id",
                            )
                        })?
                    }
                };
                let mut finish = eval.trace_finish.clone();
                finish.trace_row_id = trace_row_id;
                tx.finish_trace(&finish)?;
                if let Some(failure_plan) = failure_transition {
                    tx.apply_transition_plan(&snapshot.run.run_id, failure_plan)?;
                }
                Ok(())
            })?;

            let _ = append_trace_event(
                layout.as_ref(),
                &primary,
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

            let passed = if eval.outcome.is_ok() {
                true
            } else if failure_transition.is_some() {
                false
            } else {
                eval.outcome?;
                unreachable!()
            };
            Ok(VerifyResp {
                run_id: snapshot.run.run_id.clone(),
                node_cursor: verify_input.node_cursor.clone(),
                claim_revision: eval.trace_start.claim_revision,
                trace_status: eval.trace_finish.status,
                execution_digest: eval.execution_digest,
                exit_code: eval.trace_finish.exit_code,
                check_results: eval.trace_finish.check_results,
                log_relpath: eval.trace_start.log_relpath,
                passed,
            })
        };

    let _pre_lock = acquire_repo_lock(layout.as_ref())?;
    let loaded =
        load_active_run_graph(config, backend, req.run.as_deref(), req.worktree.as_deref())?;
    let run_id = &loaded.snapshot.run.run_id;
    let cursor = parse_cursor_for_snapshot(&loaded.snapshot, &req.node)?;
    let node_id = cursor.to_node_id()?;
    let node = verify_active_node_cas(&loaded.snapshot, &node_id, &req, None)?;
    let captured = CapturedVerifyClaim {
        claim_revision: node.claim_revision,
        contract_revision: node.contract_revision,
        worker_id: node.worker_id.clone(),
        node_version: node.node_version,
    };
    let gate_def = find_profile_gate_def(&loaded.profile, &cursor).ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let gate_id = GateId::new(&gate_def.id)?;
    let machine_gate = find_machine_gate(&loaded.machine_profile, node)?;
    let target_worktree =
        resolve_node_worktree(config, &loaded.snapshot.run, node, req.worktree.as_deref());
    let policy = loaded.path_policy(config, backend, Some(&target_worktree))?;
    let admitted_scope = container_scope(&loaded.snapshot, node);
    let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();
    let resolved_commands = resolve_commands(
        &loaded.profile,
        &gate_def,
        &admitted_scope,
        req.test_target.as_deref(),
    )?;
    let started_at = acdd_core::telemetry::now_utc_iso_ms();
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
        path_policy: Some(policy.clone()),
        now_utc: started_at.clone(),
        ..ValidatedObservations::default()
    };
    let has_running_mutating_trace = if let Some(ref call_ref) = gate_def.call {
        provider
            .action_capability(call_ref)
            .is_ok_and(|c| c.mutating)
            && backend
                .latest_trace(run_id, &node_id, node.claim_revision)?
                .is_some_and(|t| {
                    t.status == TraceStatus::Running
                        && t.action_id.as_deref() == Some(call_ref.as_str())
                })
    } else {
        false
    };
    let pre_violations = detect_scope_violations(
        &loaded.snapshot,
        &loaded.machine_profile,
        &pre_obs,
        &admitted_scope,
        is_unscoped_root,
    );
    let recovery_only = !pre_violations.is_empty();
    if recovery_only && !has_running_mutating_trace {
        drop(_pre_lock);
        let candidate_tree_after = candidate_tree_before.clone();
        let verify_input = VerifyInput {
            node_cursor: cursor,
            worker_id: req.worker_id,
            claim_revision: req.claim_revision,
            test_target: req.test_target,
            command_id: gate_def.command,
            command_line: resolved_commands.first().cloned(),
            resolved_commands,
            action_id: gate_def.call,
            index_generation: None,
            live_files_digest: None,
            log_relpath,
            started_at,
            exec_obs: VerifyExecutionObservation {
                timed_out: false,
                aborted: false,
                exit_code: None,
                stderr_tail: String::new(),
                check_results: Vec::new(),
                action_outcome: None,
                candidate_tree_before,
                candidate_tree_after: Some(candidate_tree_after),
            },
        };
        let result = persist_and_finalize_verify(
            &loaded.snapshot,
            &loaded.machine_profile,
            &pre_obs,
            &verify_input,
            None,
        );
        backend.after_commit_maintenance()?;
        return result;
    }
    // Phase 1 has captured/persisted its snapshot. Release the repository lock
    // before any Phase 2/3 work can acquire the same process-level flock.
    drop(_pre_lock);
    let run_id = &loaded.snapshot.run.run_id;

    // Phase 2 (no `repo.lock`): run domain `checks:`, optional `call:`, and optional `command:`.
    let check_req = DomainCheckReq {
        run_id: loaded.snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: gate_id.clone(),
        workspace_root: primary.clone(),
        worktree: target_worktree.clone(),
        admitted_scope: admitted_scope.clone(),
        changed_paths: pre_changed_paths.clone(),
        test_target: req.test_target.clone(),
        path_policy: policy.clone(),
        baseline_ref: crate::context::baseline_ref_for_node(node),
        node_tags: node.tags.clone(),
    };
    let (check_results, index_generation, live_files_digest) =
        run_gate_checks(provider, &gate_def, &check_req)?;

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
        call_with: machine_gate.with_params.clone(),
        checks_params,
        index_generation: index_generation.clone(),
        live_files_digest: live_files_digest.clone(),
    })?;

    let (action_outcome, existing_running_trace_id) =
        (|| -> Result<(Option<ActionExecutionOutcome>, Option<i64>), AcddError> {
            let Some(ref call_ref) = gate_def.call else {
                return Ok((None, None));
            };
            let cap = provider.action_capability(call_ref)?;
            let with_params = gate_def.with.clone().unwrap_or_else(|| json!({}));
            provider.validate_action_ref(call_ref, &with_params)?;
            let idempotency_key = format!(
                "{}:{}:{}:{}",
                run_id, node_id, captured.claim_revision, execution_digest
            );

            let call_lock = acquire_repo_lock(layout.as_ref())?;
            let call_snap = backend.load_run_graph(run_id)?.ok_or_else(|| {
                AcddError::new(ErrorCode::BackendConflict, "run disappeared before call")
            })?;
            let call_node = verify_active_node_cas(&call_snap, &node_id, &req, Some(&captured))?;
            let mut prev_abort_row = None;
            if let Some(prev) = backend.latest_trace(run_id, &node_id, call_node.claim_revision)? {
                if prev.status == TraceStatus::Running {
                    let prev_row_id = prev.trace_row_id.ok_or_else(|| {
                        AcddError::new(
                            ErrorCode::BackendConflict,
                            "running trace missing trace_row_id",
                        )
                    })?;
                    if cap.mutating && prev.action_id.as_deref() == Some(call_ref.as_str()) {
                        let prev_key = prev.idempotency_key;
                        drop(call_lock);
                        return Ok(recover_running_mutating_trace(
                            provider,
                            call_ref,
                            cap,
                            prev_key.as_deref(),
                            prev_row_id,
                            &idempotency_key,
                        ));
                    }
                    prev_abort_row = Some(prev_row_id);
                }
            }
            if recovery_only {
                drop(call_lock);
                return Ok((None, None));
            }

            let preconditions = crate::machine::graph::build_preconditions(
                &call_snap,
                &[call_node.node_id.as_str()],
                Some(call_node),
            );
            let trace_row = exec_tx_with(backend, |tx| {
                if let Some(prev_row_id) = prev_abort_row {
                    tx.finish_trace(&TraceFinishInput {
                        trace_row_id: prev_row_id,
                        status: TraceStatus::Aborted,
                        exit_code: None,
                        passed_tests: Vec::new(),
                        failed_tests: Vec::new(),
                        check_results: Vec::new(),
                        candidate_tree_after: Some(candidate_tree_before.clone()),
                        finished_at: acdd_core::telemetry::now_utc_iso_ms(),
                    })?;
                }
                tx.start_trace(
                    &preconditions,
                    &TraceStartInput {
                        run_id: run_id.clone(),
                        node_id: call_node.node_id.clone(),
                        claim_revision: call_node.claim_revision,
                        contract_revision: call_node.contract_revision,
                        execution_digest: execution_digest.to_owned(),
                        idempotency_key: Some(idempotency_key.clone()),
                        command_id: gate_def.command.clone(),
                        command_line: resolved_commands.first().cloned(),
                        action_id: Some(call_ref.clone()),
                        test_target: req.test_target.as_deref().map(str::to_owned),
                        candidate_tree_before: candidate_tree_before.clone(),
                        log_relpath: log_relpath.to_owned(),
                        started_at: started_at.to_owned(),
                    },
                )
            })?;
            let trace_row_id = trace_row.trace_row_id.ok_or_else(|| {
                AcddError::new(ErrorCode::BackendConflict, "start_trace missing row id")
            })?;
            drop(call_lock);
            acdd_core::failpoint::hit("verify_after_start_trace");

            let action_req = DomainActionReq {
                run_id: run_id.clone(),
                node_cursor: cursor.clone(),
                gate_id: gate_id.clone(),
                worker_id: call_node.worker_id.clone(),
                workspace_root: primary.to_path_buf(),
                worktree: target_worktree.to_path_buf(),
                admitted_scope: admitted_scope.to_vec(),
            };
            let outcome = match provider.execute_action(
                call_ref,
                &with_params,
                &idempotency_key,
                &action_req,
            ) {
                Ok(_res) => ActionExecutionOutcome::Succeeded,
                Err(err) => {
                    mark_trace_failed_if_contract_error(
                        backend,
                        layout.as_ref(),
                        trace_row_id,
                        &candidate_tree_before,
                        &err,
                    )?;
                    ActionExecutionOutcome::Failed(err.to_string())
                }
            };

            Ok((Some(outcome), Some(trace_row_id)))
        })()?;

    let action_failed = matches!(
        action_outcome,
        Some(ActionExecutionOutcome::Failed(_) | ActionExecutionOutcome::UncertainCrash)
    );
    let cmd_outcome = if !action_failed && !recovery_only {
        if let Some(ref cmd_id) = gate_def.command {
            let cmd_def = loaded
                .profile
                .commands
                .get(cmd_id)
                .ok_or_else(|| {
                    AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("unknown command slot '{cmd_id}'"),
                    )
                })?
                .clone();
            Some(execute_gate_commands(&CommandExecReq {
                run_id: run_id.clone(),
                node_cursor: cursor.clone(),
                claim_revision: node.claim_revision,
                worktree: target_worktree.clone(),
                primary_worktree: primary.clone(),
                command_id: cmd_id.clone(),
                command: cmd_def,
                scope: admitted_scope.clone(),
                profile_scope_args: loaded.profile.scope_args_by_pattern.clone(),
                test_target: req.test_target.clone(),
                limits: loaded.profile.limits.clone(),
            })?)
        } else {
            None
        }
    } else {
        None
    };

    // Phase 3 (re-acquire `repo.lock`): CAS-check node state, re-capture post-execution tree, persist trace.
    let post_lock = acquire_repo_lock(layout.as_ref())?;
    let latest_snapshot = backend.load_run_graph(run_id)?.ok_or_else(|| {
        AcddError::new(ErrorCode::BackendConflict, "run disappeared during verify")
    })?;
    let latest_node = verify_active_node_cas(&latest_snapshot, &node_id, &req, Some(&captured))?;

    let candidate_tree_after = current_scope_tree(&target_worktree, None, &policy)?;
    let post_changed_paths = diff_scope_changed_paths(
        &target_worktree,
        latest_node.isolation,
        latest_node.baseline_commit.as_ref(),
        latest_node.claim_tree.as_ref(),
        &policy,
    )?;
    let post_obs = ValidatedObservations {
        current_tree: Some(candidate_tree_after.clone()),
        changed_paths: post_changed_paths,
        path_policy: Some(policy.clone()),
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };

    let (command_line, exit_code, timed_out, aborted, stderr_tail, final_log_relpath) =
        match cmd_outcome {
            Some(c) => (
                Some(c.command_line),
                c.exit_code,
                c.timed_out,
                c.aborted,
                c.stderr_tail,
                c.log_relpath,
            ),
            None => (
                resolved_commands.first().cloned(),
                None,
                false,
                false,
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
        action_id: gate_def.call,
        index_generation,
        live_files_digest,
        log_relpath: final_log_relpath,
        started_at,
        exec_obs: VerifyExecutionObservation {
            timed_out,
            aborted,
            exit_code,
            stderr_tail,
            check_results,
            action_outcome,
            candidate_tree_before,
            candidate_tree_after: Some(candidate_tree_after),
        },
    };

    let res = persist_and_finalize_verify(
        &latest_snapshot,
        &loaded.machine_profile,
        &post_obs,
        &verify_input,
        existing_running_trace_id,
    );
    drop(post_lock);
    backend.after_commit_maintenance()?;
    res
}

/// Resolve deterministic rendered command lines for a gate's `command:`.
pub(crate) fn resolve_commands(
    profile: &ResolvedWorkflow,
    gate_def: &GateDef,
    admitted_scope: &[String],
    test_target: Option<&str>,
) -> Result<Vec<String>, AcddError> {
    let Some(ref cmd_id) = gate_def.command else {
        return Ok(Vec::new());
    };
    let cmd_def = profile.commands.get(cmd_id).ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "gate '{}' references unknown command '{cmd_id}'",
                gate_def.id
            ),
        )
    })?;
    let invocations = resolve_command_for_scope(cmd_def, admitted_scope)?;
    let mut rendered = Vec::with_capacity(invocations.len());
    for inv in invocations {
        rendered.extend(render_invocation_command(
            &inv,
            admitted_scope,
            &profile.scope_args_by_pattern,
            test_target,
        )?);
    }
    Ok(rendered)
}

type GateChecksOutput = (Vec<CheckResultRecord>, Option<String>, Option<String>);

pub(crate) fn split_qualified_check(check: &str) -> Result<(&str, &str), AcddError> {
    check.split_once('.').ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("check '{check}' must be qualified as '<domain>.<check>'"),
        )
    })
}

fn run_gate_checks(
    provider: &CompositeDomainProvider,
    gate_def: &GateDef,
    req: &DomainCheckReq,
) -> Result<GateChecksOutput, AcddError> {
    let mut results = Vec::with_capacity(gate_def.checks.len());
    let mut index_generations = Vec::new();
    let mut salts = Vec::new();
    for chk in &gate_def.checks {
        let (domain, check_name) = split_qualified_check(&chk.check)?;
        match provider.run_check(&chk.check, &chk.with, req) {
            Ok(DomainCheckOutcome::Passed {
                summary,
                execution_digest_salt,
            }) => {
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
                if let Some(se) = AcddError::from_anyhow(&err) {
                    return Err(se.clone());
                }
                return Err(AcddError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("domain check '{}' errored: {err}", chk.check),
                ));
            }
        }
    }

    for (chk, result) in gate_def.checks.iter().zip(&mut results) {
        if !result.passed {
            continue;
        }
        match provider.observe_freshness(&chk.check, &chk.with, req) {
            Ok(FreshnessObservation::Passed {
                index_generation,
                execution_digest_salt,
            }) => {
                if let Some(gen) = index_generation {
                    index_generations.push(format!("{}:{gen}", chk.check));
                }
                if let Some(ref salt) = execution_digest_salt {
                    salts.push(format!("{}:{salt}", chk.check));
                }
                result.execution_digest_salt = execution_digest_salt;
            }
            Ok(FreshnessObservation::Failed { summary }) => {
                return Err(AcddError::new(
                    ErrorCode::GateVerificationFailed,
                    format!(
                        "domain check '{}' freshness failed after checks: {summary}",
                        chk.check
                    ),
                ))
            }
            Err(err) => {
                return Err(AcddError::from_anyhow(&err).cloned().unwrap_or_else(|| {
                    AcddError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("domain check '{}' freshness errored: {err}", chk.check),
                    )
                }))
            }
        }
    }

    let combined_index_gen = combine_check_observations(&index_generations);
    let combined_salt = combine_check_observations(&salts);
    Ok((results, combined_index_gen, combined_salt))
}

fn recover_running_mutating_trace(
    provider: &CompositeDomainProvider,
    call_ref: &str,
    cap: &acdd_core::ports::ActionCapability,
    prev_key: Option<&str>,
    prev_row_id: i64,
    idempotency_key: &str,
) -> (Option<ActionExecutionOutcome>, Option<i64>) {
    if cap.supports_result_lookup {
        if let Some(prev_key) = prev_key {
            if provider
                .lookup_action_result(call_ref, prev_key)
                .is_ok_and(|opt| opt.is_some())
                && prev_key == idempotency_key
            {
                return (Some(ActionExecutionOutcome::Succeeded), Some(prev_row_id));
            }
        }
    }
    (
        Some(ActionExecutionOutcome::UncertainCrash),
        Some(prev_row_id),
    )
}

fn mark_trace_failed_if_contract_error(
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    trace_row_id: i64,
    candidate_tree_before: &TreeOid,
    err: &anyhow::Error,
) -> Result<(), AcddError> {
    let Some(se) = AcddError::from_anyhow(err) else {
        return Ok(());
    };
    if !matches!(
        se.code,
        ErrorCode::DomainCapabilityUnavailable | ErrorCode::WorkflowSchemaInvalid
    ) {
        return Ok(());
    }
    let fail_lock = acquire_repo_lock(layout)?;
    let _ = exec_tx_with(backend, |tx| {
        tx.finish_trace(&TraceFinishInput {
            trace_row_id,
            status: TraceStatus::Failed,
            exit_code: None,
            passed_tests: Vec::new(),
            failed_tests: Vec::new(),
            check_results: Vec::new(),
            candidate_tree_after: Some(candidate_tree_before.clone()),
            finished_at: acdd_core::telemetry::now_utc_iso_ms(),
        })
    });
    drop(fail_lock);
    let _ = backend.after_commit_maintenance();
    Err(se.clone())
}

pub(crate) fn find_machine_gate<'a>(
    profile: &'a MachineProfileInput,
    node: &GateNodeRecord,
) -> Result<&'a crate::machine::MachineGateDef, AcddError> {
    crate::machine::graph::resolve_gate_def(profile, node)
}

fn default_log_relpath(node_id: &str, claim_rev: u32) -> String {
    let slug: String = node_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("logs/{slug}-rev{claim_rev}.log")
}
