//! Automated wave orchestration (`acdd run`) wiring topological wave scheduling,
//! mechanical gate verification/submission, `RoleDef.runner` spawning, and wave-boundary integration.

use crate::context::render_claim_prompt_markdown;
use crate::git_ops::commit::{current_branch_name, is_protected_main_branch};
use crate::git_ops::integrate::{
    abort_in_progress_git_ops, integrate_task_branch, verify_tasks_integrated, IntegrateTaskReq,
};
use crate::git_ops::worktree::canonicalize_macos_var;
use crate::machine::compute_schedule;
use crate::machine::graph::{build_preconditions, is_post_task_root_node};
use crate::machine::schedule::all_non_cancelled_tasks_completed;
use crate::runner::{
    spawn_role_runner_borrowed, verify_runner_submitted, RoleSpawnBorrow, WaveExecutionReport,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ClaimOutcome, GateNodeRecord, IsolationMode, NodeKind, NodeStatus, NodeStatusPatch,
    RunGraphSnapshot, RunStatus, SubmitAction, TerminalStatus, TransitionPlan,
    ValidatedObservations,
};
use crate::usecases::commands::{handle_claim_dyn, is_context_reset_required};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::collect_external_satisfied_deps;
use crate::usecases::dto::{ClaimReq, RunReq, RunResp, SubmitReq, VerifyReq};
use crate::usecases::lifecycle::{
    collect_integration_inputs, ensure_state_enabled, parse_profile_from_run,
    primary_workspace_root, resolve_active_run, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def_ref;
use crate::usecases::submit::handle_submit_dyn;
use crate::usecases::verify::handle_verify_with_failure_return;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId, TaskRef, WorkerId};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::telemetry::{SpanId, TraceId, TraceParent};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) fn handle_run_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: RunReq,
) -> Result<RunResp, StateError> {
    ensure_state_enabled(config)?;
    let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;

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
    let execute_ready_wave_tasks = |snapshot: &RunGraphSnapshot,
                                    wave_tasks: &[TaskRef],
                                    jobs: usize|
     -> Result<Vec<StepOutcome>, StateError> {
        let execute_task = |task_ref: &TaskRef| -> Result<StepOutcome, StateError> {
            let mut total = StepOutcome::default();
            let parent_id = format!("task:{task_ref}");
            let run_id = snapshot.run.run_id.clone();
            for _ in 0..256usize {
                let Some(current) = backend.load_run_graph(&run_id)? else {
                    break;
                };
                let Some(task_node) = current
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
                let Some(next_gate) = find_next_task_gate(&current, &parent_id) else {
                    break;
                };
                let step = execute_gate_node(
                    config,
                    backend,
                    provider,
                    &current,
                    next_gate,
                    req.allow_main,
                    &session_cache,
                )?;
                total.gates_completed += step.gates_completed;
                if step.halted_reason.is_some() {
                    total.halted_reason = step.halted_reason;
                    break;
                }
            }
            Ok(total)
        };

        if jobs > 1 && wave_tasks.len() > 1 {
            return std::thread::scope(|scope| {
                let handles: Vec<_> = wave_tasks
                    .iter()
                    .map(|task_ref| scope.spawn(|| execute_task(task_ref)))
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().map_err(|_| {
                            StateError::new(
                                ErrorCode::BackendConflict,
                                "wave worker thread panicked during acdd run",
                            )
                        })?
                    })
                    .collect()
            });
        }
        wave_tasks.iter().map(execute_task).collect()
    };
    let mut waves_executed = 0u32;
    let mut gates_completed = 0u32;
    let mut tasks_integrated: Vec<TaskRef> = Vec::new();
    let mut integrated_set: BTreeSet<TaskRef> = BTreeSet::new();
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
            let node = snapshot
                .nodes
                .iter()
                .find(|node| node.node_id == root_id)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("node `{root_id}` not found in run graph"),
                    )
                })?;
            let outcome = execute_gate_node(
                config,
                backend,
                provider,
                &snapshot,
                node,
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
                let task_outcomes = execute_ready_wave_tasks(&snapshot, &wave_tasks, jobs)?;

                waves_executed += 1;
                for out in &task_outcomes {
                    gates_completed += out.gates_completed;
                    if out.halted_reason.is_some() && halted_reason.is_none() {
                        halted_reason = out.halted_reason.clone();
                    }
                }

                if snapshot.run.isolation == IsolationMode::Worktree {
                    let landed = land_completed_task_branches(
                        config,
                        backend,
                        &run.run_id,
                        req.allow_main,
                        &mut integrated_set,
                    )?;
                    tasks_integrated.extend(landed);
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
            let landed = land_completed_task_branches(
                config,
                backend,
                &run.run_id,
                req.allow_main,
                &mut integrated_set,
            )?;
            if !landed.is_empty() {
                tasks_integrated.extend(landed);
                continue;
            }
            halted_reason = Some(
                "all tasks completed; unintegrated task deltas remain in run worktree".to_owned(),
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
pub(crate) struct StepOutcome {
    pub(crate) gates_completed: u32,
    pub(crate) halted_reason: Option<String>,
}

fn build_schedule_observations(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
) -> Result<ValidatedObservations, StateError> {
    let external_satisfied_deps = collect_external_satisfied_deps(config, snapshot)?;
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
        external_deps_authoritative: true,
        unintegrated_tasks,
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    })
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

pub(crate) fn execute_gate_node(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    allow_main: bool,
    session_cache: &Mutex<BTreeMap<(RunId, String, String), String>>,
) -> Result<StepOutcome, StateError> {
    let profile = parse_profile_from_run(&snapshot.run)?;
    let cursor = NodeCursor::from_node_id(&node.node_id)?;
    let gate_def = find_profile_gate_def_ref(&profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no profile gate definition found for cursor `{cursor}`"),
        )
    })?;
    debug_assert!(snapshot
        .nodes
        .iter()
        .any(|candidate| candidate.node_id == node.node_id));

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
        let worktree = std::path::PathBuf::from(&claim_resp.worktree);
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
            let mut guard = session_cache.lock().map_err(|_| {
                StateError::new(ErrorCode::BackendConflict, "session cache lock poisoned")
            })?;
            if claim_resp.context_reset_required
                || is_context_reset_required(snapshot, node, gate_def)
            {
                guard.remove(&key);
            } else if let Some(existing) = guard.get(&key) {
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

        let completed_now = check_post_runner_completion(
            backend,
            &snapshot.run.run_id,
            &node.node_id,
            claim_resp.claim_revision,
            &outcome,
        )?;

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

    let verify_resp = handle_verify_with_failure_return(
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
        true,
    )?;

    if !verify_resp.passed {
        let escalated = backend
            .load_run_graph(&snapshot.run.run_id)?
            .and_then(|g| g.nodes.into_iter().find(|n| n.node_id == node.node_id))
            .is_some_and(|n| n.status == NodeStatus::Escalated);
        if escalated {
            return Ok(StepOutcome {
                gates_completed: 0,
                halted_reason: Some(format!("mechanical gate `{cursor}` escalated")),
            });
        }
        // Recoverable `fail:` transition reset the target recovery gate to `Ready`;
        // continue the runner loop without crashing so the recovery gate executes.
        return Ok(StepOutcome {
            gates_completed: 0,
            halted_reason: None,
        });
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

pub(crate) fn check_post_runner_completion(
    backend: &dyn StateBackend,
    run_id: &RunId,
    node_id: &str,
    claim_revision: u32,
    outcome: &crate::runner::RoleSpawnOutcome,
) -> Result<bool, StateError> {
    let after_graph = backend.load_run_graph(run_id)?;
    let (claim_outcome_after, completed_now) = if let Some(ref g) = after_graph {
        let outcome_after = g
            .claims
            .iter()
            .filter(|c| c.node_id == node_id && c.claim_revision == claim_revision)
            .map(|c| c.outcome)
            .next_back();
        let done = g
            .nodes
            .iter()
            .find(|n| n.node_id == node_id)
            .is_some_and(|n| n.status == NodeStatus::Completed);
        (outcome_after, done)
    } else if backend
        .load_tombstone(run_id)?
        .is_some_and(|t| t.terminal_status == TerminalStatus::Completed)
    {
        (Some(ClaimOutcome::Passed), true)
    } else {
        (None, false)
    };
    verify_runner_submitted(outcome, claim_outcome_after)?;
    Ok(completed_now)
}

fn land_completed_task_branches(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    run_id: &RunId,
    allow_main: bool,
    already_integrated: &mut BTreeSet<TaskRef>,
) -> Result<Vec<TaskRef>, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, _layout| {
        let snapshot = backend
            .load_run_graph(run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let run_worktree = snapshot
            .run
            .worktree
            .as_deref()
            .map(|p| canonicalize_macos_var(Path::new(p)))
            .unwrap_or_else(|| primary.to_path_buf());
        let _ = abort_in_progress_git_ops(&run_worktree)?;

        let run_branch = current_branch_name(&run_worktree)?;
        if is_protected_main_branch(run_branch.as_deref()) && !allow_main {
            return Err(StateError::new(
                ErrorCode::MainBranchProtected,
                format!(
                    "refusing wave integration onto protected branch '{}' without --allow-main",
                    run_branch.as_deref().unwrap_or("main")
                ),
            ));
        }

        let mut completed_tasks: Vec<&GateNodeRecord> = snapshot
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
            .collect();
        completed_tasks.sort_by_key(|n| n.seq_order);

        let mut landed = Vec::new();
        for task_node in completed_tasks {
            let Some(ref tref) = task_node.task_ref else {
                continue;
            };
            if already_integrated.contains(tref) {
                continue;
            }
            let Some(ref accepted_tree) = task_node.accepted_tree else {
                continue;
            };
            let Some(ref source_branch) = task_node.branch else {
                continue;
            };
            if run_branch.as_deref() == Some(source_branch.as_str()) {
                continue;
            }
            let source_wt = task_node.worktree.as_deref().map(PathBuf::from);
            if let Some(ref swt) = source_wt {
                if swt.exists() {
                    let _ = abort_in_progress_git_ops(swt)?;
                }
            }

            let resp = integrate_task_branch(&IntegrateTaskReq {
                target_worktree: run_worktree.clone(),
                source_worktree: source_wt,
                source_branch: source_branch.clone(),
                run_id: snapshot.run.run_id.clone(),
                task_ref: tref.clone(),
                accepted_tree: accepted_tree.clone(),
                allow_main,
            })?;
            if resp.integrated_commit.is_some() {
                already_integrated.insert(tref.clone());
                landed.push(tref.clone());
            }
        }

        if all_non_cancelled_tasks_completed(&snapshot) {
            let inputs = collect_integration_inputs(&snapshot);
            let missing = verify_tasks_integrated(&snapshot.run.run_id, &run_worktree, &inputs)?;
            if missing.is_empty() {
                if let Some(first_post) = snapshot
                    .nodes
                    .iter()
                    .filter(|n| is_post_task_root_node(&snapshot, n))
                    .min_by_key(|n| n.seq_order)
                {
                    if first_post.status == NodeStatus::Pending {
                        let plan = TransitionPlan {
                            preconditions: build_preconditions(
                                &snapshot,
                                &[&first_post.node_id],
                                None,
                            ),
                            bump_run_generation: true,
                            new_run_status: None,
                            new_root_cycle_count: None,
                            completed_at: None,
                            node_patches: vec![NodeStatusPatch::status_only(
                                &first_post.node_id,
                                NodeStatus::Ready,
                            )],
                            dependency_patches: Vec::new(),
                            claim_patches: Vec::new(),
                            new_artifacts: Vec::new(),
                            new_blackboard: Vec::new(),
                            blackboard_patches: Vec::new(),
                            new_subtasks: Vec::new(),
                            significant_transition: true,
                        };
                        exec_tx_with(backend, |tx| {
                            tx.apply_transition_plan(&snapshot.run.run_id, &plan)
                        })?;
                    }
                }
            }
        }

        Ok(landed)
    })
}
