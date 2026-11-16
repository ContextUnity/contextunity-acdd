use super::{
    role::{sync_command_findings, RoleObservations},
    task_runner::{run_task_lifecycle, TaskRunOptions},
    types::OrchestratorEvent,
    workflow::{ensure_command_completed, run_gate_graph, GateDecision},
};
use crate::{
    engine::{self, gate_graph, git_snapshot, Registry},
    milestones,
    tasks::{
        profile::{GateDriver, PersistenceAction, ProofKind},
        Milestone,
    },
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Dispatch policy for one milestone runner invocation.
pub struct MilestoneRunOptions {
    pub task: TaskRunOptions,
    pub in_place: bool,
    pub target_base: String,
}

/// Run validated task waves through the existing task lifecycle, then root gates.
pub fn run_milestone_waves(
    root: &Path,
    selector: &str,
    options: &MilestoneRunOptions,
    observer: &mut dyn FnMut(OrchestratorEvent),
) -> Result<Value> {
    ensure!(
        !options.target_base.trim().is_empty(),
        "MILESTONE_TARGET_BASE_REQUIRED"
    );
    let root = root.canonicalize()?;
    let registry = Registry::load(&root)?;
    let (workspace, reference) = registry.resolve_milestone(None, selector)?;
    ensure!(
        workspace.root == root,
        "MILESTONE_RUN_WORKSPACE_REQUIRED: run in the owning workspace"
    );
    let text = fs::read_to_string(root.join(&reference))?;
    let milestone = registry.parse_milestone_manifest(workspace, &reference, &text)?;
    let waves = milestones::validate_wave_dag(&milestone)?;
    let mut sync = engine::Manage {
        action: engine::ManageAction::Sync,
        ..Default::default()
    };
    sync.milestone_ref = Some(reference.clone());
    engine::manage(&root, sync)?;
    let store = registry.open(workspace)?;
    let synchronized_tasks = store.list(Some(&reference), "all", None)?;
    let active_tasks = synchronized_tasks
        .iter()
        .filter(|task| task.status != "cancelled")
        .cloned()
        .collect::<Vec<_>>();
    let task = active_tasks
        .first()
        .or_else(|| synchronized_tasks.first())
        .context("MILESTONE_TASK_CONTAINER_INVALID: milestone has no tasks")?;
    let profile = store.profile_for_milestone(
        &root,
        milestone.acdd_profile.as_deref(),
        &workspace.gates,
        &synchronized_tasks,
    )?;
    let cursors = gate_graph::collect_root_cursors(&profile)?;
    let task_position = gate_graph::task_container_position(&profile, Some(task))?
        .context("MILESTONE_TASK_CONTAINER_INVALID: no task lifecycle container applies")?;
    if !options.in_place {
        ensure!(clean_worktree(&root)?, "MILESTONE_ISOLATION_DIRTY: isolated dispatch requires a clean integrated milestone checkout");
    }
    let identity = format!(
        "{}/{}/{}",
        milestone.repository, milestone.project, milestone.id
    );
    let scope = milestone
        .tasks
        .iter()
        .flat_map(|task| task.scope.iter().cloned())
        .collect::<Vec<_>>();
    let pre_task_required = cursors
        .iter()
        .filter(|cursor| {
            cursor.root_index < task_position
                && gate_graph::gate_condition_matches(
                    cursor.when.as_deref(),
                    &scope,
                    &active_tasks,
                    None,
                    Some(&root),
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut tasks_done = false;
    let mut verification: Option<Value> = None;
    let mut review: Option<(milestones::HandoffBinding, milestones::IntegrationReview)> = None;
    let mut roles = RoleObservations::default();
    let mut handoff: Option<Value> = None;
    let profile_digest = digest(profile.as_ref())?;
    let mut authority = milestone.clone();
    authority.status = None;
    for task in &mut authority.tasks {
        task.status = None;
        task.receipt = None;
    }
    let authority_digest = digest(&authority)?;
    // Replay observations through the canonical router. These are existing transition
    // records, not a second cursor or task registry. A changed profile/authority or
    // candidate invalidates the remaining prefix and requires fresh observations.
    let mut history = {
        let store = registry.open(workspace)?;
        let rows = gate_graph::active_milestone_history(&store.connection, &identity)?;
        let epoch = rows
            .iter()
            .filter(|row| row.3 == "candidate_drift")
            .map(|row| row.0)
            .max()
            .unwrap_or(0);
        rows.into_iter()
            .filter(|row| row.0 > epoch)
            .filter(|row| row.3 == "orchestrator")
            .map(|(id, gate, _, _, payload)| (id, gate, payload))
            .collect::<std::collections::VecDeque<_>>()
    };
    let mut previous_observation = None;
    let mut accepted_snapshot: Option<Value> = None;
    let outcomes = run_gate_graph(&root, &scope, &active_tasks, &cursors, |cursor, attempt| {
        roles.note_executed(&cursor.canonical_stage_id);
        if !tasks_done && cursor.root_index > task_position {
            gate_graph::ensure_root_gate_transitions_passed_where(
                &registry.open(workspace)?.connection,
                &identity,
                &cursors,
                |candidate| {
                    pre_task_required
                        .iter()
                        .any(|cursor| cursor.canonical_stage_id == candidate.canonical_stage_id)
                },
            )?;
            dispatch_waves(&root, &milestone, &waves, options, observer)?;
            tasks_done = true;
            accepted_snapshot = None;
        }
        let binding = if tasks_done {
            milestones::handoff_context(&root, selector, &options.target_base)?["binding"].clone()
        } else {
            json!({"candidate_fingerprint":engine::candidate_fingerprint(&root)?})
        };
        let before = engine::candidate_fingerprint(&root)?;
        let cached = history
            .front()
            .filter(|(_, gate, saved)| {
                gate == &cursor.canonical_stage_id
                    && saved["profile_digest"] == profile_digest
                    && saved["authority_digest"] == authority_digest
                    && saved["binding"] == binding
                    && saved["input_binding"] == binding
                    && saved["candidate_before"] == before
                    && saved["candidate_after"] == before
                    && [
                        (cursor.gate.persistence.pre, "pre_snapshot"),
                        (cursor.gate.persistence.post, "post_snapshot"),
                    ]
                    .into_iter()
                    .all(|(action, key)| {
                        action != Some(PersistenceAction::Snapshot)
                            || key == "post_snapshot" && saved["passed"] == false
                            || saved[key]["candidate_fingerprint"] == before
                                && saved[key]["snapshot_ref"]
                                    .as_str()
                                    .is_some_and(|reference| {
                                        crate::git::git_stdout(
                                            &root,
                                            &["rev-parse", "--verify", reference],
                                            "TASK_SNAPSHOT_FAILED",
                                        )
                                        .is_ok_and(|commit| saved[key]["snapshot_commit"] == commit)
                                    })
                    })
            })
            .cloned();
        if cached.is_some() {
            history.pop_front();
        } else {
            history.clear();
        }
        if cursor.gate.persistence.immutable {
            if let Some(prior) = &accepted_snapshot {
                if prior["candidate_fingerprint"] != before {
                    let prior_commit = prior["snapshot_commit"]
                        .as_str()
                        .context("TASK_CANDIDATE_MISMATCH: prior root snapshot missing")?;
                    let prior_head = prior["head"]
                        .as_str()
                        .context("TASK_CANDIDATE_MISMATCH: prior root baseline missing")?;
                    let (current, head, _objects) = git_snapshot::capture_scoped_snapshot(
                        &root,
                        &format!("{identity}:drift-before"),
                        &[],
                    )?
                    .context("TASK_SNAPSHOT_FAILED")?;
                    ensure!(
                        head == prior_head,
                        "TASK_CANDIDATE_MISMATCH: root HEAD diverged from candidate baseline"
                    );
                    let diff = crate::git::git_command(
                        &root,
                        &[
                            "diff-tree",
                            "--no-commit-id",
                            "--name-only",
                            "--no-renames",
                            "-r",
                            "-z",
                            prior_commit,
                            &current,
                        ],
                    )?;
                    ensure!(
                        diff.status.success(),
                        "TASK_CANDIDATE_MISMATCH: cannot compare root snapshots"
                    );
                    let paths = diff
                        .stdout
                        .split(|byte| *byte == 0)
                        .filter(|path| {
                            !path.is_empty() && !git_snapshot::is_acdd_runtime_path(path)
                        })
                        .map(|path| String::from_utf8_lossy(path).into_owned())
                        .collect::<Vec<_>>();
                    let warning = json!({"prior_binding":prior["candidate_fingerprint"],"current_binding":before,"changed_paths":paths,"needs_verification":true,"needs_independent_review":true});
                    let store = registry.open(workspace)?;
                    engine::record_root_transition_with_watermark(
                        &store,
                        crate::store::GateTransition {
                            milestone_id: identity.clone(),
                            subject: "milestone".into(),
                            gate_path: cursor.canonical_stage_id.clone(),
                            action: "fail".into(),
                            proof_kind: "candidate_drift".into(),
                            proof_payload: json!({"previous_observation":previous_observation,"passed":false,"candidate_drift_warning":warning,"findings":{"code":"CANDIDATE_DRIFT"}}),
                            git_tree_sha: head,
                            worker_id: options.task.worker_id.clone(),
                        },
                    )?;
                    anyhow::bail!("CANDIDATE_DRIFT_REBIND_REQUIRED: root snapshot changed; verification and independent review must be renewed");
                }
            }
        }
        let snapshot_identity = format!(
            "{}:{}:{}:{}",
            identity, profile_digest, authority_digest, cursor.canonical_stage_id
        );
        let pre_snapshot = if cached.is_none()
            && cursor.gate.persistence.pre == Some(PersistenceAction::Snapshot)
        {
            Some(git_snapshot::capture_root_snapshot(
                &root,
                &format!("{snapshot_identity}:pre"),
                &before,
            )?)
        } else {
            cached.as_ref().and_then(|(_, _, saved)| {
                saved.get("pre_snapshot").filter(|v| !v.is_null()).cloned()
            })
        };
        if cursor.gate.receipt || cursor.gate.proof.kind() == Some(ProofKind::Delivery) {
            ensure!(tasks_done, "MILESTONE_TASKS_INCOMPLETE");
            let current = milestones::handoff_context(&root, selector, &options.target_base)?;
            let observed = verification.as_ref().context(
                "HANDOFF_VERIFICATION_REQUIRED: no accepted root verification was observed",
            )?;
            ensure!(
                observed["binding"] == current["binding"],
                "HANDOFF_EVIDENCE_STALE: verification is not bound to final content"
            );
            let (binding, accepted_review) = review.as_ref().context("HANDOFF_REVIEW_REQUIRED: profile reached handoff without current-content independent integration review")?;
            ensure!(
                serde_json::to_value(binding)? == current["binding"],
                "HANDOFF_EVIDENCE_STALE: review is not bound to final content"
            );
            let check = &observed["verification"];
            let evidence = milestones::IntegrationEvidence {
                binding: binding.clone(),
                target_base: options.target_base.clone(),
                review: accepted_review.clone(),
                verification: milestones::IntegrationVerification {
                    command: check["command"]
                        .as_str()
                        .context("HANDOFF_VERIFICATION_INVALID")?
                        .into(),
                    exit_code: i32::try_from(
                        check["exit_code"]
                            .as_i64()
                            .context("HANDOFF_VERIFICATION_INVALID")?,
                    )?,
                    tests_passed: serde_json::from_value(check["tests_passed"].clone())
                        .context("HANDOFF_VERIFICATION_INVALID")?,
                    tests_failed: serde_json::from_value(check["tests_failed"].clone())
                        .context("HANDOFF_VERIFICATION_INVALID")?,
                    lint_command: None,
                    project_command: Some(
                        check["command_id"]
                            .as_str()
                            .context("HANDOFF_VERIFICATION_INVALID")?
                            .into(),
                    ),
                },
            };
            if cursor.gate.persistence.immutable {
                git_snapshot::ensure_root_candidate(
                    &before,
                    &engine::candidate_fingerprint(&root)?,
                )?;
            }
            let result = milestones::handoff(&root, selector, None, &evidence)?;
            if cursor.gate.persistence.post == Some(PersistenceAction::Snapshot) {
                let after = engine::candidate_fingerprint(&root)?;
                git_snapshot::capture_root_snapshot(
                    &root,
                    &format!("{snapshot_identity}:post"),
                    &after,
                )?;
            }
            handoff = Some(result.clone());
            return Ok(GateDecision {
                passed: true,
                evidence: result,
                findings: None,
            });
        }
        let immutable_snapshot = if cursor.gate.persistence.immutable && cached.is_none() {
            Some(
                git_snapshot::capture_scoped_snapshot(
                    &root,
                    &format!("{snapshot_identity}:drift-check"),
                    &[],
                )?
                .context("TASK_SNAPSHOT_FAILED: immutable root gate requires a snapshot")?,
            )
        } else {
            None
        };
        let mut replayed = cached.is_some();
        let mut decision = if let Some((id, _, saved)) = cached.clone() {
            previous_observation = Some(id);
            let decision = GateDecision {
                passed: saved["passed"]
                    .as_bool()
                    .context("MILESTONE_CHECKPOINT_INVALID")?,
                evidence: saved["evidence"].clone(),
                findings: saved
                    .get("findings")
                    .filter(|value| !value.is_null())
                    .cloned(),
            };
            roles.restore(&cursor.canonical_stage_id, &decision);
            if cursor.gate.driver() == Some(GateDriver::Command) && tasks_done && decision.passed {
                verification = Some(decision.evidence.clone());
            }
            decision
        } else if cursor.gate.driver() == Some(GateDriver::Command) && tasks_done {
            ensure!(
                matches!(
                    cursor.gate.proof.kind(),
                    Some(ProofKind::None | ProofKind::Command)
                ),
                "MILESTONE_PROOF_REQUIRED: post-task verification cannot supply this proof"
            );
            let mut result = milestones::execute_wave_verification(
                &root,
                selector,
                &options.target_base,
                &cursor.canonical_stage_id,
            )?;
            let command_id = cursor
                .gate
                .command
                .as_deref()
                .context("MILESTONE_COMMAND_MISSING")?;
            let trace = registry.open(workspace)?.connection.query_row(
                "SELECT command_id, command_str, status, exit_code, output_snippet, log_path, attempt_id FROM gate_execution_traces WHERE milestone_id=?1 AND subject='milestone' AND gate_path=?2 AND command_id=?3 ORDER BY attempt_id DESC, id DESC LIMIT 1",
                rusqlite::params![identity, cursor.canonical_stage_id, command_id],
                |row| Ok(json!({"command_id": row.get::<_, String>(0)?, "command": row.get::<_, String>(1)?, "status": row.get::<_, String>(2)?, "exit_code": row.get::<_, Option<i64>>(3)?, "output": row.get::<_, Option<String>>(4)?, "log_path": row.get::<_, Option<String>>(5)?, "attempt_id": row.get::<_, i64>(6)?})),
            )?;
            result["trace"] = trace;
            ensure_command_completed(&result["verification"], &cursor.canonical_stage_id)?;
            let passed = result["status"] == "passed";
            if passed {
                verification = Some(result.clone());
            } else {
                verification = None;
            }
            GateDecision {
                passed,
                evidence: result,
                findings: None,
            }
        } else if cursor.gate.driver() == Some(GateDriver::Command) {
            ensure!(
                matches!(
                    cursor.gate.proof.kind(),
                    Some(
                        ProofKind::None
                            | ProofKind::Command
                            | ProofKind::Contract
                            | ProofKind::SeamTest
                    )
                ),
                "MILESTONE_PROOF_REQUIRED: command execution cannot supply this proof"
            );
            let name = cursor
                .gate
                .command
                .as_deref()
                .context("MILESTONE_COMMAND_MISSING")?;
            let command = profile
                .commands
                .get(name)
                .context("MILESTONE_COMMAND_MISSING")?;
            let plan = milestones::resolve_command_plan(&root, name, command, &scope, &profile)?;
            ensure!(!plan.is_empty(), "MILESTONE_COMMAND_MISSING");
            let mut observations = Vec::new();
            for (id, command_str, timeout) in plan {
                let result = milestones::command_gate::observe_command_for_id(
                    &root,
                    &id,
                    &command_str,
                    timeout,
                    command.output(),
                    cursor.gate.expected_failure.as_ref(),
                )?;
                ensure_command_completed(&result, &cursor.canonical_stage_id)?;
                observations.push((id, result));
            }
            let passed = crate::engine::evidence_compiler::evaluate_root_command_gate(
                &cursor.gate,
                &profile,
                &observations,
                cursor.gate.policy.as_deref().unwrap_or("seam-test-first"),
                None,
            )?;
            GateDecision {
                passed,
                evidence: json!(observations),
                findings: None,
            }
        } else {
            ensure!(
                cursor.gate.role.is_some() || cursor.gate.command.is_some(),
                "MILESTONE_COMMAND_MISSING: gate '{}' has no bound command",
                cursor.canonical_stage_id
            );
            if tasks_done {
                let context = milestones::handoff_context(&root, selector, &options.target_base)?;
                let binding: milestones::HandoffBinding =
                    serde_json::from_value(context["binding"].clone())?;
                let store = registry.open(workspace)?;
                let mut forbidden = Vec::new();
                for builder in &binding.builders {
                    let mut statement = store.connection.prepare("SELECT DISTINCT c.session_fingerprint FROM task_claims c JOIN tasks t ON t.task_id=c.task_id WHERE c.worker_id=?1 AND t.milestone_ref=?2")?;
                    let sessions = statement
                        .query_map(rusqlite::params![builder, reference], |row| {
                            row.get::<_, Option<String>>(0)
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    ensure!(!sessions.is_empty() && sessions.iter().all(|session| session.as_ref().is_some_and(|session| !session.is_empty() && session != crate::store::UNKNOWN_SESSION_FINGERPRINT)), "TASK_REVIEW_SESSION_UNKNOWN: accepted milestone builder session is unavailable");
                    forbidden.extend(sessions.into_iter().flatten());
                }
                roles.set_subject_context(context, forbidden);
            } else {
                roles.set_subject_context(
                    json!({"manifest_path":reference,"manifest":milestone,"source":text}),
                    Vec::new(),
                );
            }
            roles.execute(
                &root,
                &identity,
                &format!("Complete milestone {}", milestone.id),
                &scope,
                cursor,
                &profile,
            )?
        };
        let after_execution = engine::candidate_fingerprint(&root)?;
        if cursor.gate.persistence.immutable && before != after_execution {
            let (prior, head, _objects) =
                immutable_snapshot.context("TASK_SNAPSHOT_FAILED: immutable snapshot missing")?;
            let (current, current_head, _current_objects) = git_snapshot::capture_scoped_snapshot(
                &root,
                &format!("{snapshot_identity}:drift-current"),
                &[],
            )?
            .context("TASK_SNAPSHOT_FAILED: immutable current snapshot missing")?;
            ensure!(
                head == current_head,
                "TASK_CANDIDATE_MISMATCH: immutable gate changed HEAD"
            );
            let diff = crate::git::git_command(
                &root,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "--no-renames",
                    "-r",
                    "-z",
                    &prior,
                    &current,
                ],
            )?;
            ensure!(
                diff.status.success(),
                "TASK_CANDIDATE_MISMATCH: cannot compare immutable snapshots"
            );
            let paths = diff
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty() && !git_snapshot::is_acdd_runtime_path(path))
                .map(|path| String::from_utf8_lossy(path).into_owned())
                .collect::<Vec<_>>();
            let warning = json!({"prior_binding":before,"current_binding":after_execution,"changed_paths":paths,"needs_verification":true,"needs_independent_review":true});
            let store = registry.open(workspace)?;
            engine::record_root_transition_with_watermark(
                &store,
                crate::store::GateTransition {
                    milestone_id: identity.clone(),
                    subject: "milestone".into(),
                    gate_path: cursor.canonical_stage_id.clone(),
                    action: "fail".into(),
                    proof_kind: "candidate_drift".into(),
                    proof_payload: json!({"previous_observation":previous_observation,"passed":false,"candidate_drift_warning":warning,"findings":{"code":"CANDIDATE_DRIFT"}}),
                    git_tree_sha: current_head,
                    worker_id: options.task.worker_id.clone(),
                },
            )?;
            anyhow::bail!("CANDIDATE_DRIFT_REBIND_REQUIRED: immutable gate changed candidate; verification and independent review must be renewed");
        }
        let output_binding = if tasks_done {
            milestones::handoff_context(&root, selector, &options.target_base)?["binding"].clone()
        } else {
            json!({"candidate_fingerprint":after_execution})
        };
        if cursor.gate.driver() == Some(GateDriver::Command) {
            sync_command_findings(&cursor.canonical_stage_id, &mut decision, &mut roles);
        }
        let after = engine::candidate_fingerprint(&root)?;
        if decision.passed && !tasks_done && cursor.root_index < task_position {
            let proof_validation = registry.open(workspace).and_then(|store| {
                super::super::lifecycle::validate_pre_task_root_proof(
                    &store,
                    workspace,
                    task,
                    &decision.evidence,
                )
            });
            if let Err(error) = proof_validation {
                decision = GateDecision {
                    passed: false,
                    evidence: decision.evidence,
                    findings: Some(json!({
                        "code": "MILESTONE_TASK_PROOF_INVALID",
                        "message": format!("{error:#}"),
                    })),
                };
                roles.restore(&cursor.canonical_stage_id, &decision);
                // A replayed pass with invalid task coverage must be superseded
                // by a durable failure so the configured repair route can run.
                replayed = false;
            }
        }
        if decision.passed && !decision.evidence["integration_review"].is_null() {
            ensure!(cursor.gate.independent_from.is_some(), "HANDOFF_REVIEW_NOT_INDEPENDENT: integration review needs a declared independent gate");
            review = Some((
                serde_json::from_value(decision.evidence["binding"].clone())?,
                serde_json::from_value(decision.evidence["integration_review"].clone())?,
            ));
        } else if !decision.passed && cursor.gate.independent_from.is_some() {
            review = None;
        }
        let post_snapshot = if decision.passed
            && cursor.gate.persistence.post == Some(PersistenceAction::Snapshot)
        {
            let snapshot = if replayed {
                cached
                    .as_ref()
                    .and_then(|(_, _, saved)| saved.get("post_snapshot"))
                    .cloned()
                    .context("MILESTONE_CHECKPOINT_INVALID: missing post snapshot")?
            } else {
                git_snapshot::capture_root_snapshot(
                    &root,
                    &format!("{snapshot_identity}:post"),
                    &after,
                )?
            };
            accepted_snapshot = Some(snapshot.clone());
            Some(snapshot)
        } else {
            None
        };
        if replayed {
            return Ok(decision);
        }
        let store = registry.open(workspace)?;
        let transition = crate::store::GateTransition {
            milestone_id: identity.clone(),
            subject: "milestone".into(),
            gate_path: cursor.canonical_stage_id.clone(),
            action: if decision.passed { "pass" } else { "fail" }.into(),
            proof_kind: "orchestrator".into(),
            proof_payload: json!({"previous_observation":previous_observation,"profile_digest":profile_digest,"authority_digest":authority_digest,"input_binding":binding,"binding":output_binding,"candidate_before":before,"candidate_after":after,"pre_snapshot":pre_snapshot,"post_snapshot":post_snapshot,"root_snapshot_fingerprint":if post_snapshot.is_some() { Some(after) } else { None },"passed":decision.passed,"attempt":attempt,"evidence":decision.evidence,"findings":decision.findings}),
            git_tree_sha: crate::git::git_stdout(
                &root,
                &["rev-parse", "HEAD"],
                "MILESTONE_HEAD_REQUIRED",
            )?,
            worker_id: decision.evidence["session_id"]
                .as_str()
                .unwrap_or(&options.task.worker_id)
                .to_owned(),
        };
        let id = if post_snapshot.is_some() {
            engine::record_root_transition_with_watermark(&store, transition)?
        } else {
            store.record_gate_transition(&transition)?
        };
        if decision.passed && !tasks_done && cursor.root_index < task_position {
            super::super::lifecycle::fan_out_pre_task_proofs_if_ready(
                &store, workspace, task, &profile, &root,
            )?;
        }
        previous_observation = Some(id);
        Ok(decision)
    })?;
    if !tasks_done {
        gate_graph::ensure_root_gate_transitions_passed_where(
            &registry.open(workspace)?.connection,
            &identity,
            &cursors,
            |candidate| {
                pre_task_required
                    .iter()
                    .any(|cursor| cursor.canonical_stage_id == candidate.canonical_stage_id)
            },
        )?;
        dispatch_waves(&root, &milestone, &waves, options, observer)?;
    }
    let result: Value = handoff
        .take()
        .context("MILESTONE_HANDOFF_REQUIRED: root graph completed without accepted handoff")?;
    Ok(json!({"status":"completed","milestone":milestone.id,"outcomes":outcomes,"handoff":result}))
}

fn digest(value: &(impl serde::Serialize + ?Sized)) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn dispatch_waves(
    root: &Path,
    milestone: &Milestone,
    waves: &[Vec<String>],
    options: &MilestoneRunOptions,
    observer: &mut dyn FnMut(OrchestratorEvent),
) -> Result<()> {
    let registry = Registry::load(root)?;
    for (wave_index, wave) in waves.iter().enumerate() {
        let ids = wave
            .iter()
            .map(|task| format!("{}/{}:{task}", milestone.project, milestone.id))
            .collect::<Vec<_>>();
        observer(OrchestratorEvent::WaveStarted {
            wave_index,
            task_ids: ids.clone(),
        });
        // In-place writers and isolated integration are both serial. Every task
        // starts from the integrated predecessor content, never a stale wave base.
        for id in &ids {
            let workspace = registry.owner(id)?;
            let store = registry.open(workspace)?;
            let task = store.inspect(id)?;
            if task.status == "cancelled" {
                continue;
            }
            if task.status == "completed" {
                require_integrated_delivery(root, &task, options.in_place)?;
                continue;
            }
            ensure!(options.in_place || task.status != "in_progress", "TASK_ALREADY_CLAIMED: isolated task '{id}' retains an active claim in {:?}; recover it before provisioning another worktree", task.worktree);
            for dependency in &task.spec.depends_on {
                if let Some(dependency) = milestone
                    .tasks
                    .iter()
                    .find(|task| &task.task_ref == dependency)
                {
                    let prior = store.inspect(&format!(
                        "{}/{}:{}",
                        milestone.project, milestone.id, dependency.task_ref
                    ))?;
                    ensure!(
                        prior.status == "completed" && prior.receipt.is_some(),
                        "TASK_DEPENDENCY_INCOMPLETE: {}",
                        prior.task_id
                    );
                    require_integrated_delivery(root, &prior, options.in_place)?;
                }
            }
            drop(store);
            if options.in_place {
                let mut task_options = options.task.clone();
                task_options.worktree = root.to_owned();
                run_task_lifecycle(root, id, &task_options, observer)?;
            } else {
                run_isolated_task(root, id, options, observer)?;
            }
            let accepted = registry.open(workspace)?.inspect(id)?;
            ensure!(
                accepted.status == "completed" && accepted.receipt.is_some(),
                "MILESTONE_WAVE_INCOMPLETE: {id}"
            );
            require_integrated_delivery(root, &accepted, options.in_place)?;
        }
        for id in &ids {
            let workspace = registry.owner(id)?;
            let accepted = registry.open(workspace)?.inspect(id)?;
            ensure!(
                accepted.status == "cancelled"
                    || (accepted.status == "completed" && accepted.receipt.is_some()),
                "MILESTONE_WAVE_INCOMPLETE: {id}"
            );
            if accepted.status == "completed" {
                require_integrated_delivery(root, &accepted, options.in_place)?;
            }
        }
        observer(OrchestratorEvent::WaveCompleted {
            wave_index,
            task_ids: ids,
        });
    }
    Ok(())
}

fn require_integrated_delivery(
    root: &Path,
    task: &crate::store::Task,
    in_place: bool,
) -> Result<()> {
    let receipt = task.receipt.as_ref().context("TASK_RECEIPT_MISSING")?;
    if !in_place || receipt.commit.is_some() {
        let commit = receipt.commit.as_deref().context("MILESTONE_DELIVERY_NOT_INTEGRATED: isolated delivery has no commit; preserve and reconcile its worktree before resuming")?;
        ensure!(
            crate::git::git_ancestor(root, commit, "HEAD")?,
            "MILESTONE_DELIVERY_NOT_INTEGRATED: task '{}' delivery {} is absent from integrated HEAD; reconcile the accepted delivery from {:?} before resuming; worktree and proofs are preserved",
            task.task_id, commit, task.worktree
        );
    }
    Ok(())
}

fn run_isolated_task(
    root: &Path,
    id: &str,
    options: &MilestoneRunOptions,
    observer: &mut dyn FnMut(OrchestratorEvent),
) -> Result<()> {
    let token = format!(
        "{:x}",
        Sha256::digest(format!(
            "{id}\n{}\n{}",
            options.task.session_fingerprint,
            chrono::Utc::now()
                .timestamp_nanos_opt()
                .context("MILESTONE_CLOCK_INVALID")?
        ))
    );
    let branch = format!("acdd-wave-{}", &token[..24]);
    let path = crate::fs::checked_child(root, Path::new(&format!(".worktrees/{branch}")))?;
    ensure!(
        !path.exists(),
        "MILESTONE_WORKTREE_EXISTS: refusing to acquire a foreign worktree"
    );
    fs::create_dir_all(path.parent().context("MILESTONE_WORKTREE_PARENT_INVALID")?)?;
    let path_text = path.to_str().context("MILESTONE_WORKTREE_PATH_INVALID")?;
    git_success(
        root,
        &["worktree", "add", "-b", &branch, "--", path_text, "HEAD"],
    )?;
    let mut task_options = options.task.clone();
    task_options.worktree = path.clone();
    // Failure intentionally preserves this owned worktree and its branch for recovery.
    let mut run = || run_task_lifecycle(&path, id, &task_options, observer);
    let result = match crate::tasks::configuration::current(root) {
        Some(settings) => settings.rebase(&path).run(run),
        None => run(),
    };
    result.with_context(|| {
        format!(
            "MILESTONE_TASK_FAILED: recoverable worktree {}",
            path.display()
        )
    })?;
    let commit = crate::git::git_stdout(
        &path,
        &["rev-parse", "HEAD"],
        "MILESTONE_INTEGRATION_FAILED",
    )?;
    ensure!(
        clean_worktree(&path)?,
        "MILESTONE_INTEGRATION_DIRTY: preserve worktree {}",
        path.display()
    );
    git_success(root, &["merge", "--ff-only", "--", &commit])?;
    git_success(root, &["worktree", "remove", "--", path_text])?;
    git_success(root, &["branch", "-d", "--", &branch])?;
    Ok(())
}

fn git_success(root: &Path, arguments: &[&str]) -> Result<()> {
    let output = crate::git::git_command(root, arguments)?;
    ensure!(
        output.status.success(),
        "MILESTONE_GIT_FAILED: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn clean_worktree(root: &Path) -> Result<bool> {
    let output = crate::git::git_command(root, &["status", "--porcelain"])?;
    ensure!(output.status.success(), "MILESTONE_STATUS_FAILED");
    Ok(output.stdout.is_empty())
}
