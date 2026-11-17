//! Automated wave orchestration (`acdd run`) wiring topological wave scheduling,
//! mechanical gate verification/submission, `RoleDef.runner` spawning, and wave-boundary integration.

use crate::context::render_claim_prompt_markdown;
use crate::git_ops::integrate::verify_tasks_integrated;
use crate::machine::compute_schedule;
use crate::machine::schedule::all_non_cancelled_tasks_completed;
use crate::profile::ResolvedGateProfile;
use crate::runner::{
    spawn_role_runner_borrowed, verify_runner_submitted, RoleSpawnBorrow, WaveExecutionReport,
};
use crate::store::traits::StateBackend;
use crate::types::{
    GateNodeRecord, IntegrateStrategy, IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot,
    RunStatus, SubmitAction, ValidatedObservations,
};
use crate::usecases::commands::{handle_claim_dyn, handle_integrate_dyn};
use crate::usecases::dto::{ClaimReq, IntegrateReq, RunReq, RunResp, SubmitReq, VerifyReq};
use crate::usecases::lifecycle::{
    collect_external_satisfied_deps, collect_integration_inputs, ensure_state_enabled,
    now_utc_iso, parse_profile_from_run, primary_workspace_root, resolve_active_run,
    resolve_node_worktree,
};
use crate::usecases::queries::find_profile_gate_def_ref;
use crate::usecases::submit::handle_submit_dyn;
use crate::usecases::verify::handle_verify_dyn;
use crate::usecases::StateEngineConfig;
use acdd_core::telemetry::{SpanId, TraceId, TraceParent};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId, TaskRef, WorkerId};
use acdd_core::ports::CompositeDomainProvider;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Execute automated DAG waves (`StateCommand::Run`).
pub fn handle_run<B: StateBackend + Send + Sync>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: RunReq,
) -> Result<RunResp, StateError> {
    handle_run_dyn(config, backend, provider, req)
}

pub(crate) fn handle_run_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: RunReq,
) -> Result<RunResp, StateError> {
    ensure_state_enabled(config)?;
    let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
    let profile = parse_profile_from_run(&run)?;

    let jobs = if run.isolation == IsolationMode::InPlace {
        1
    } else {
        req.jobs.unwrap_or(1).max(1)
    };

    if req.dry_run {
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let obs = build_schedule_observations(config, &snapshot)?;
        let schedule = compute_schedule(&snapshot, &obs)?;
        return Ok(RunResp {
            run_id: run.run_id,
            report: WaveExecutionReport {
                waves_executed: 0,
                gates_completed: 0,
                tasks_integrated: Vec::new(),
                halted_reason: Some(format!(
                    "dry_run: {} ready root gate(s), {} planned task wave(s) (jobs={})",
                    schedule.ready_root_nodes.len(),
                    schedule.waves.len(),
                    jobs
                )),
            },
        });
    }

    let session_cache: Mutex<BTreeMap<(RunId, String, String), String>> =
        Mutex::new(BTreeMap::new());
    let mut waves_executed = 0u32;
    let mut gates_completed = 0u32;
    let mut tasks_integrated: Vec<TaskRef> = Vec::new();
    let mut halted_reason: Option<String> = None;

    // Guard against pathological infinite loops on state backends that refuse forward progress.
    let max_iterations = 1024usize;
    for _ in 0..max_iterations {
        let Some(snapshot) = backend.load_run_graph(&run.run_id)? else {
            break;
        };
        match snapshot.run.status {
            RunStatus::Active => {}
            RunStatus::Completed => break,
            RunStatus::Cancelled => {
                halted_reason = Some("run cancelled".to_owned());
                break;
            }
        }

        let obs = build_schedule_observations(config, &snapshot)?;
        let schedule = compute_schedule(&snapshot, &obs)?;

        if !schedule.ready_root_nodes.is_empty() {
            let root_id = schedule.ready_root_nodes[0].clone();
            let outcome = execute_gate_node(
                config,
                backend,
                provider,
                &profile,
                &snapshot,
                &root_id,
                req.allow_main,
                &session_cache,
            )?;
            waves_executed += 1;
            gates_completed += outcome.gates_completed;
            if let Some(halt) = outcome.halted_reason {
                halted_reason = Some(halt);
                break;
            }
            continue;
        }

        if let Some(first_wave) = schedule.waves.first() {
            let wave_tasks: Vec<TaskRef> = first_wave
                .iter()
                .filter(|tref| schedule.ready_tasks.contains(tref))
                .take(jobs)
                .cloned()
                .collect();

            if !wave_tasks.is_empty() {
                let task_outcomes = if jobs > 1 && wave_tasks.len() > 1 {
                    std::thread::scope(|scope| {
                        let mut handles = Vec::with_capacity(wave_tasks.len());
                        for tref in &wave_tasks {
                            let snap_ref = &snapshot;
                            let prof_ref = &profile;
                            let cache_ref = &session_cache;
                            handles.push(scope.spawn(move || {
                                execute_task_to_completion(
                                    config,
                                    backend,
                                    provider,
                                    prof_ref,
                                    snap_ref,
                                    tref,
                                    req.allow_main,
                                    cache_ref,
                                )
                            }));
                        }
                        let mut results = Vec::with_capacity(handles.len());
                        for handle in handles {
                            let joined = handle.join().map_err(|_| {
                                StateError::new(
                                    ErrorCode::BackendConflict,
                                    "wave worker thread panicked during acdd run",
                                )
                            })??;
                            results.push(joined);
                        }
                        Ok::<Vec<StepOutcome>, StateError>(results)
                    })?
                } else {
                    let mut results = Vec::with_capacity(wave_tasks.len());
                    for tref in &wave_tasks {
                        results.push(execute_task_to_completion(
                            config,
                            backend,
                            provider,
                            &profile,
                            &snapshot,
                            tref,
                            req.allow_main,
                            &session_cache,
                        )?);
                    }
                    results
                };

                waves_executed += 1;
                for out in &task_outcomes {
                    gates_completed += out.gates_completed;
                    if out.halted_reason.is_some() && halted_reason.is_none() {
                        halted_reason = out.halted_reason.clone();
                    }
                }

                if snapshot.run.isolation == IsolationMode::Worktree
                    && profile.git.integrate != IntegrateStrategy::Manual
                {
                    let int_resp = handle_integrate_dyn(
                        config,
                        backend,
                        IntegrateReq {
                            run: Some(run.run_id.to_string()),
                            task_ref: None,
                            strategy: None,
                            allow_main: req.allow_main,
                        },
                    )?;
                    tasks_integrated.extend(int_resp.integrated_tasks);
                }

                if halted_reason.is_some() {
                    break;
                }
                continue;
            }
        }

        if schedule.all_tasks_completed
            && schedule.post_task_blocked_reason == Some(ErrorCode::TaskNotIntegrated)
        {
            if profile.git.integrate != IntegrateStrategy::Manual {
                let int_resp = handle_integrate_dyn(
                    config,
                    backend,
                    IntegrateReq {
                        run: Some(run.run_id.to_string()),
                        task_ref: None,
                        strategy: None,
                        allow_main: req.allow_main,
                    },
                )?;
                tasks_integrated.extend(int_resp.integrated_tasks);
                continue;
            }
            halted_reason = Some(
                "all tasks completed; awaiting manual `acdd integrate` before post-task gates"
                    .to_owned(),
            );
            break;
        }

        let any_escalated = snapshot
            .nodes
            .iter()
            .any(|n| n.status == NodeStatus::Escalated);
        if any_escalated {
            halted_reason = Some("one or more nodes escalated".to_owned());
        } else {
            halted_reason = Some("no ready nodes remaining in run graph".to_owned());
        }
        break;
    }

    Ok(RunResp {
        run_id: run.run_id,
        report: WaveExecutionReport {
            waves_executed,
            gates_completed,
            tasks_integrated,
            halted_reason,
        },
    })
}

#[derive(Debug, Default)]
struct StepOutcome {
    gates_completed: u32,
    halted_reason: Option<String>,
}

fn build_schedule_observations(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
) -> Result<ValidatedObservations, StateError> {
    let external_satisfied_deps =
        collect_external_satisfied_deps(config, &snapshot.run.run_id, &snapshot.dependencies)?;
    let unintegrated_tasks = if snapshot.run.isolation == IsolationMode::Worktree
        && all_non_cancelled_tasks_completed(snapshot)
    {
        let primary = primary_workspace_root(config);
        let target_worktree = snapshot
            .run
            .worktree
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or(primary);
        let inputs = collect_integration_inputs(snapshot);
        verify_tasks_integrated(&snapshot.run.run_id, &target_worktree, &inputs)?
    } else {
        Vec::new()
    };

    Ok(ValidatedObservations {
        external_satisfied_deps,
        unintegrated_tasks,
        now_utc: now_utc_iso(),
        ..ValidatedObservations::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_task_to_completion(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    profile: &ResolvedGateProfile,
    initial_snapshot: &RunGraphSnapshot,
    task_ref: &TaskRef,
    allow_main: bool,
    session_cache: &Mutex<BTreeMap<(RunId, String, String), String>>,
) -> Result<StepOutcome, StateError> {
    let mut total = StepOutcome::default();
    let parent_id = format!("task:{task_ref}");
    let run_id = initial_snapshot.run.run_id.clone();

    for _ in 0..256usize {
        let Some(snapshot) = backend.load_run_graph(&run_id)? else {
            break;
        };
        let Some(task_node) = snapshot
            .nodes
            .iter()
            .find(|n| n.node_id == parent_id && n.kind == NodeKind::Task)
        else {
            break;
        };
        match task_node.status {
            NodeStatus::Completed | NodeStatus::Cancelled => break,
            NodeStatus::Escalated => {
                total.halted_reason = Some(format!("task {task_ref} escalated"));
                break;
            }
            _ => {}
        }

        let Some(next_gate) = find_next_task_gate(&snapshot, &parent_id) else {
            break;
        };
        let step = execute_gate_node(
            config,
            backend,
            provider,
            profile,
            &snapshot,
            &next_gate.node_id,
            allow_main,
            session_cache,
        )?;
        total.gates_completed += step.gates_completed;
        if let Some(halt) = step.halted_reason {
            total.halted_reason = Some(halt);
            break;
        }
    }

    Ok(total)
}

fn find_next_task_gate<'a>(
    snapshot: &'a RunGraphSnapshot,
    parent_id: &str,
) -> Option<&'a crate::types::GateNodeRecord> {
    let mut gates: Vec<&GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            n.parent_id.as_deref() == Some(parent_id)
                && matches!(
                    n.status,
                    NodeStatus::Ready | NodeStatus::InProgress | NodeStatus::Stale
                )
        })
        .collect();
    gates.sort_by_key(|g| g.seq_order);
    gates.into_iter().next()
}

#[allow(clippy::too_many_arguments)]
fn execute_gate_node(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    profile: &ResolvedGateProfile,
    snapshot: &RunGraphSnapshot,
    node_id: &str,
    allow_main: bool,
    session_cache: &Mutex<BTreeMap<(RunId, String, String), String>>,
) -> Result<StepOutcome, StateError> {
    let cursor = NodeCursor::from_node_id(node_id)?;
    let gate_def = find_profile_gate_def_ref(profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no profile gate definition found for cursor `{cursor}`"),
        )
    })?;
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node `{node_id}` not found in run graph"),
            )
        })?;

    let container_id = node.parent_id.as_deref().unwrap_or(&node.node_id);

    if let Some(ref role_name) = gate_def.role {
        let role_def = profile.roles.get(role_name).ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("gate `{cursor}` references unknown role `{role_name}`"),
            )
        })?;
        if role_def.runner.is_none() {
            return Err(StateError::new(
                ErrorCode::WorkflowRunnerRequired,
                format!(
                    "role `{role_name}` on gate `{cursor}` has no configured `runner:` command for `acdd run`"
                ),
            ));
        }

        let worker_id = WorkerId::new(format!("runner-{role_name}"))?;
        let claim_resp = handle_claim_dyn(
            config,
            backend,
            provider,
            ClaimReq {
                run: Some(snapshot.run.run_id.to_string()),
                node: Some(cursor.to_string()),
                worker_id: worker_id.clone(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: None,
            },
        )?;

        let prompt_markdown =
            render_claim_prompt_markdown(&claim_resp, role_def.prompt_template.as_deref());
        let primary = primary_workspace_root(config);
        let worktree = resolve_node_worktree(config, &snapshot.run, node, None);
        let (state_db_path, _) = config.resolved_state_db_path();
        let trace_id = snapshot
            .run
            .trace_id
            .parse::<TraceId>()
            .unwrap_or_else(|_| TraceId::new());
        let trace_parent = TraceParent::new(trace_id, SpanId::new()).to_string();

        let mut local_cache = BTreeMap::new();
        let cache_key = if role_def.reuse_on_reject {
            let key = (
                snapshot.run.run_id.clone(),
                container_id.to_owned(),
                role_name.clone(),
            );
            let guard = session_cache.lock().map_err(|_| {
                StateError::new(ErrorCode::BackendConflict, "session cache lock poisoned")
            })?;
            if let Some(existing) = guard.get(&key) {
                local_cache.insert(key.clone(), existing.clone());
            }
            Some(key)
        } else {
            None
        };

        let spawn_req = RoleSpawnBorrow {
            run_id: &snapshot.run.run_id,
            node_cursor: &cursor,
            node_container_id: container_id,
            claim_revision: claim_resp.claim_revision,
            role_name,
            role_def,
            worker_id: &worker_id,
            worktree: &worktree,
            primary_worktree: &primary,
            state_db_path: &state_db_path,
            code_db_path: config.code_db_path.as_deref(),
            trace_parent: &trace_parent,
            prompt_markdown: &prompt_markdown,
            timeout_seconds: profile.limits.max_command_timeout_seconds,
            max_log_bytes: profile.limits.max_log_bytes,
            max_line_bytes: profile.limits.max_line_bytes,
        };
        let outcome = spawn_role_runner_borrowed(&spawn_req, &mut local_cache)?;
        if let Some(key) = cache_key {
            let mut guard = session_cache.lock().map_err(|_| {
                StateError::new(ErrorCode::BackendConflict, "session cache lock poisoned")
            })?;
            guard.insert(key, outcome.agent_session_id.clone());
        }

        let after_graph = backend.load_run_graph(&snapshot.run.run_id)?;
        let claim_outcome_after = after_graph.as_ref().and_then(|g| {
            g.claims
                .iter()
                .filter(|c| c.node_id == node_id && c.claim_revision == claim_resp.claim_revision)
                .map(|c| c.outcome)
                .next_back()
        });
        verify_runner_submitted(&outcome, claim_outcome_after)?;

        let completed_now = after_graph
            .as_ref()
            .and_then(|g| g.nodes.iter().find(|n| n.node_id == node_id))
            .is_some_and(|n| n.status == NodeStatus::Completed);
        return Ok(StepOutcome {
            gates_completed: u32::from(completed_now),
            halted_reason: None,
        });
    }

    // Mechanical gate (`role: None`): claim -> verify -> submit(pass).
    let worker_id = WorkerId::new("acdd-run-auto")?;
    let claim_resp = handle_claim_dyn(
        config,
        backend,
        provider,
        ClaimReq {
            run: Some(snapshot.run.run_id.to_string()),
            node: Some(cursor.to_string()),
            worker_id: worker_id.clone(),
            session_fingerprint: None,
            force: false,
            steal_remote: false,
            worktree: None,
        },
    )?;

    let verify_resp = handle_verify_dyn(
        config,
        backend,
        provider,
        VerifyReq {
            run: Some(snapshot.run.run_id.to_string()),
            node: cursor.to_string(),
            worker_id: Some(worker_id.clone()),
            test_target: None,
            claim_revision: Some(claim_resp.claim_revision),
            worktree: None,
        },
    )?;

    if !verify_resp.passed {
        let escalated = backend
            .load_run_graph(&snapshot.run.run_id)?
            .and_then(|g| g.nodes.into_iter().find(|n| n.node_id == node_id))
            .is_some_and(|n| n.status == NodeStatus::Escalated);
        if escalated {
            return Ok(StepOutcome {
                gates_completed: 0,
                halted_reason: Some(format!("mechanical gate `{cursor}` escalated")),
            });
        }
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("mechanical gate `{cursor}` failed verification"),
        ));
    }

    let submit_resp = handle_submit_dyn(
        config,
        backend,
        provider,
        SubmitReq {
            run: Some(snapshot.run.run_id.to_string()),
            node: cursor.to_string(),
            action: SubmitAction::Pass,
            worker_id,
            claim_revision: Some(claim_resp.claim_revision),
            test_target: None,
            review_evidence: None,
            rejection_findings: None,
            custom_evidence: None,
            verified_invariants: Vec::new(),
            summary: Some(format!("automated mechanical gate pass for {cursor}")),
            allow_main,
            worktree: None,
        },
    )?;

    Ok(StepOutcome {
        gates_completed: u32::from(submit_resp.new_node_status == NodeStatus::Completed),
        halted_reason: None,
    })
}
