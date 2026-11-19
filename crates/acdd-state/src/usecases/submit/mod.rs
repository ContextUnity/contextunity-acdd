//! Gate submission orchestration (`submit`): freshness checks, `need:` stale-marking,
//! `save.post` (`none | commit`), stateless archive retry,
//! and atomic `archive_and_prune_run` + post-commit cleanup.

mod archive;
mod budget;
mod freshness;
mod post_save;

use crate::git_ops::integrate::verify_tasks_integrated;
use crate::git_ops::snapshot::{
    current_scope_tree, diff_scope_changed_paths, remove_confined_file,
};
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::graph::is_post_task_root_node;
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    container_scope, evaluate_submit, MachineProfileInput,
    SubmitEvaluation, SubmitInput,
};
use crate::spec::Receipt;
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArtifactKind, NodeKind, NodeStatus,
    RunGraphSnapshot, RunStatus,
    SubmitAction, TransitionPlan, ValidatedObservations,
};
use crate::usecases::dto::{SubmitReq, SubmitResp, VerifyReq};
use crate::usecases::lifecycle::{
    acquire_repo_lock, append_trace_event, build_path_policy, collect_external_satisfied_deps,
    collect_integration_inputs, ensure_state_enabled,
    parse_cursor_for_snapshot, parse_profile_from_run, primary_workspace_root,
    resolve_active_run, resolve_node_worktree, resolve_repo_layout_optional, to_machine_profile, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::verify::{find_machine_gate, handle_verify_dyn};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::checked_child;
use acdd_core::ids::{CommitOid, NodeCursor, TreeOid};
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::path::Path;

use archive::{finalize_with_prune, probe_interrupted_archive, relax_need_for_node};
use budget::enforce_blueprint_max_net_loc;
use freshness::{
    ensure_fresh_trace_or_auto_verify, inspect_submit_preflight, observe_existing_trace_freshness,
};
use post_save::{attach_container_blackboard_cleanup, execute_post_save_git};

pub(crate) fn handle_submit_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SubmitReq,
) -> Result<SubmitResp, StateError> {
    ensure_state_enabled(config)?;
    if req.action == SubmitAction::Reject {
        return with_repo_lock_and_maintenance(config, backend, |_primary, layout| {
            submit_holding_repo_lock(config, backend, layout, req, None)
        });
    }

    let primary = primary_workspace_root(config);
    let layout = resolve_repo_layout_optional(&primary)?;

    // Phase 1 (short `repo.lock`): preflight claim, immutable/scope state, and existing trace candidate.
    let pre_lock = acquire_repo_lock(layout.as_ref())?;
    let preflight = inspect_submit_preflight(config, backend, &primary, &req)?;
    drop(pre_lock);

    // Phase 2 (no `repo.lock`): check domain freshness or run unlocked auto-verify outside `repo.lock`.
    let verified_hint = if let Some(pf) = preflight {
        let fresh_digest = match pf.existing_fresh {
            Some(ref tr) => observe_existing_trace_freshness(
                provider,
                &pf.primary,
                &pf.target_worktree,
                &pf.snapshot,
                &pf.node,
                &pf.resolved_profile,
                &pf.machine_gate,
                &pf.cursor,
                &pf.admitted_scope,
                &pf.policy,
                &req,
                tr,
            )?,
            None => None,
        };
        if let Some(digest) = fresh_digest {
            Some((pf.current_tree, digest))
        } else {
            let verify_resp = handle_verify_dyn(
                config,
                backend,
                provider,
                VerifyReq {
                    run: Some(pf.snapshot.run.run_id.as_str().to_owned()),
                    node: pf.cursor.to_string(),
                    worker_id: Some(req.worker_id.clone()),
                    test_target: req.test_target.clone(),
                    claim_revision: Some(pf.node.claim_revision),
                    worktree: req.worktree.clone(),
                },
            )?;
            Some((pf.current_tree, verify_resp.execution_digest))
        }
    } else {
        None
    };

    // Phase 3 (`repo.lock` + maintenance): final CAS checks, scoped commit/snapshot/archive, SQLite commit.
    with_repo_lock_and_maintenance(config, backend, |_primary, lock_layout| {
        submit_holding_repo_lock(
            config,
            backend,
            lock_layout,
            req,
            verified_hint.as_ref(),
        )
    })
}

/// Core `submit` implementation executed while `repo.lock` is held.
pub(crate) fn submit_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    req: SubmitReq,
    verified_hint: Option<&(TreeOid, String)>,
) -> Result<SubmitResp, StateError> {
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

    if req.action == SubmitAction::Reject {
        return handle_reject(
            backend,
            layout,
            config,
            &snapshot,
            &machine_profile,
            &cursor,
            &node_id,
            req,
        );
    }

    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node '{node_id}' not found in run '{}'", run.run_id),
            )
        })?
        .clone();
    let _gate_def = find_profile_gate_def(&resolved_profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let machine_gate = find_machine_gate(&machine_profile, &node)?.clone();

    let primary = primary_workspace_root(config);
    let target_worktree =
        resolve_node_worktree(config, &snapshot.run, &node, req.worktree.as_deref());
    let policy = build_path_policy(config, backend, &[])?;

    // Check stateless archive retry probe before trace/need checks when archive already landed before crash.
    let recovered = probe_interrupted_archive(
        config,
        backend,
        &target_worktree,
        &snapshot,
        &node,
        &machine_gate,
        &cursor,
        &req,
        &policy,
    )?;

    let admitted_scope = container_scope(&snapshot, &node);
    let current_tree = match recovered.as_ref() {
        Some(rec) => rec.current_tree.clone(),
        None => current_scope_tree(&target_worktree, None, &policy)?,
    };

    let (obs, expected_digest, effective_machine_profile) = if recovered
        .as_ref()
        .is_some_and(|rec| rec.already_committed)
    {
        // Never relax `need:` in no-commit mode; only do so when a verified `Acdd-Claim` commit
        // already finalized Git state before SQLite prune.
        let mut relaxed = machine_profile.clone();
        relax_need_for_node(&mut relaxed, &node.gate_id);
        let obs = ValidatedObservations {
            current_tree: Some(current_tree.clone()),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };
        (obs, None, std::borrow::Cow::Owned(relaxed))
    } else {
        let mut changed_paths = diff_scope_changed_paths(
            &target_worktree,
            node.isolation,
            node.baseline_commit.as_ref(),
            node.claim_tree.as_ref(),
            &policy,
        )?;
        if let Some(ref rec) = recovered {
            changed_paths.retain(|p| p != &rec.archive_rel);
        }
        let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();
        let scope_obs = ValidatedObservations {
            current_tree: Some(current_tree.clone()),
            changed_paths: changed_paths.clone(),
            path_policy: Some(policy.clone()),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };
        let scope_violations = detect_scope_violations(
            &snapshot,
            &machine_profile,
            &scope_obs,
            &admitted_scope,
            is_unscoped_root,
        );
        if !scope_violations.is_empty() {
            return Err(StateError::new(
                ErrorCode::OutOfScopeChanges,
                format!(
                    "cannot submit '{}': working tree has out-of-scope changes: {}",
                    node.node_id,
                    scope_violations.join(", ")
                ),
            ));
        }

        let expected_digest = ensure_fresh_trace_or_auto_verify(
            backend,
            &snapshot,
            &resolved_profile,
            &machine_profile,
            &node,
            &machine_gate,
            &cursor,
            &admitted_scope,
            &current_tree,
            &changed_paths,
            &policy,
            &req,
            verified_hint,
        )?;

        let unintegrated_tasks = if machine_gate.receipt && is_post_task_root_node(&snapshot, &node)
        {
            let inputs = collect_integration_inputs(&snapshot);
            verify_tasks_integrated(&snapshot.run.run_id, &target_worktree, &inputs)?
        } else {
            Vec::new()
        };
        let external_satisfied_deps =
            collect_external_satisfied_deps(config, &snapshot)?;
        let container_tree = if admitted_scope.is_empty() {
            None
        } else {
            Some(current_scope_tree(
                &target_worktree,
                Some(&admitted_scope),
                &policy,
            )?)
        };
        let obs = ValidatedObservations {
            current_tree: Some(current_tree.clone()),
            container_tree,
            changed_paths,
            out_of_scope_paths: Vec::new(),
            unintegrated_tasks,
            external_satisfied_deps,
            external_deps_authoritative: true,
            path_policy: Some(policy.clone()),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        };
        (obs, expected_digest, std::borrow::Cow::Borrowed(&machine_profile))
    };

    let expected_scoped_tree = obs.container_tree.as_ref().unwrap_or(&current_tree);
    let was_recovered = recovered.is_some();
    let (committed_oid, archive_path_written, report_path_written) =
        if let Some(rec) = recovered {
            (rec.committed_oid, Some(rec.archive_rel), None)
        } else {
            // Pre-save pure evaluation before mutating Git state.
            let pre_input = make_submit_pass_input(&cursor, &req, expected_digest.clone(), None);
            let _ = eval_submit_or_mark_stale(
                backend,
                &snapshot,
                effective_machine_profile.as_ref(),
                &obs,
                &pre_input,
            )?;

            if node.kind == NodeKind::TaskGate && machine_gate.role.as_deref() == Some("builder") {
                enforce_blueprint_max_net_loc(
                    config,
                    backend,
                    &snapshot,
                    &node,
                    &target_worktree,
                    &current_tree,
                    &admitted_scope,
                    &policy,
                )?;
            }

            execute_post_save_git(
                config,
                backend,
                &primary,
                &target_worktree,
                &snapshot,
                &node,
                &machine_gate,
                &cursor,
                &admitted_scope,
                &machine_profile.protected_paths,
                &current_tree,
                expected_scoped_tree,
                &obs.now_utc,
                &req,
                &policy,
            )?
        };

    // Single shared submit evaluation, container blackboard cleanup, and SQLite finalizer.
    let final_input = make_submit_pass_input(&cursor, &req, expected_digest, committed_oid.clone());
    let mut plan = eval_submit_or_mark_stale(
        backend,
        &snapshot,
        effective_machine_profile.as_ref(),
        &obs,
        &final_input,
    )?;

    attach_container_blackboard_cleanup(config, backend, &snapshot, &node, &machine_gate, &mut plan)?;
    let emitted_receipt = extract_receipt_from_plan(&plan);

    let final_run_status = if machine_gate.save.prune {
        finalize_with_prune(
            config,
            backend,
            layout,
            &primary,
            &target_worktree,
            &snapshot,
            &plan,
            archive_path_written.clone(),
            committed_oid.clone(),
            &obs.now_utc,
        )?;
        RunStatus::Completed
    } else {
        exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;
        plan.new_run_status.unwrap_or(snapshot.run.status)
    };

    if archive_path_written.is_some() && (was_recovered || committed_oid.is_none()) {
        if let Some(ref spec_rel) = snapshot.run.spec_path {
            if checked_child(&target_worktree, Path::new(spec_rel)).is_ok_and(|p| p.exists()) {
                remove_confined_file(&target_worktree, spec_rel)?;
            }
        }
    }

    let unblocked_nodes = collect_unblocked_cursors(&plan, &node_id)?;

    let _ = append_trace_event(
        layout,
        &primary,
        &snapshot.run.run_id,
        &json!({
            "kind": "gate_submit",
            "action": "pass",
            "run_id": snapshot.run.run_id.as_str(),
            "node_cursor": cursor.to_string(),
            "claim_revision": node.claim_revision,
            "committed_oid": committed_oid.as_ref().map(|c| c.as_str()),
            "archive_path": archive_path_written,
            "report_path": report_path_written,
            "timestamp": obs.now_utc,
        }),
    );

    Ok(SubmitResp {
        run_id: snapshot.run.run_id,
        node_cursor: cursor,
        action: SubmitAction::Pass,
        new_node_status: NodeStatus::Completed,
        run_status: final_run_status,
        committed_oid,
        receipt: emitted_receipt,
        archive_path: archive_path_written,
        report_path: report_path_written,
        unblocked_nodes,
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_reject(
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    cursor: &NodeCursor,
    node_id: &str,
    req: SubmitReq,
) -> Result<SubmitResp, StateError> {
    let obs = ValidatedObservations {
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };
    let input = SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Reject,
        worker_id: req.worker_id,
        claim_revision: req.claim_revision,
        expected_execution_digest: None,
        review_evidence: req.review_evidence,
        rejection_findings: req.rejection_findings,
        custom_evidence: req.custom_evidence,
        verified_invariants: Vec::new(),
        snapshot_commit: None,
        summary: req.summary,
    };

    let plan = match evaluate_submit(snapshot, machine_profile, &obs, &input)? {
        SubmitEvaluation::Apply(plan) => plan,
        SubmitEvaluation::MarkStaleAndReject { error, .. } => return Err(error),
    };

    exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;

    let new_node_status = plan
        .node_patches
        .iter()
        .find(|p| p.node_id == node_id)
        .map(|p| p.new_status)
        .unwrap_or(NodeStatus::Pending);
    let unblocked_nodes = collect_unblocked_cursors(&plan, node_id)?;

    let primary = primary_workspace_root(config);
    let _ = append_trace_event(
        layout,
        &primary,
        &snapshot.run.run_id,
        &json!({
            "kind": "gate_submit",
            "action": "reject",
            "run_id": snapshot.run.run_id.as_str(),
            "node_cursor": cursor.to_string(),
            "new_node_status": new_node_status.as_str(),
            "timestamp": obs.now_utc,
        }),
    );

    Ok(SubmitResp {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        action: SubmitAction::Reject,
        new_node_status,
        run_status: snapshot.run.status,
        committed_oid: None,
        receipt: None,
        archive_path: None,
        report_path: None,
        unblocked_nodes,
    })
}

fn make_submit_pass_input(
    cursor: &NodeCursor,
    req: &SubmitReq,
    expected_execution_digest: Option<String>,
    snapshot_commit: Option<CommitOid>,
) -> SubmitInput {
    SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        worker_id: req.worker_id.clone(),
        claim_revision: req.claim_revision,
        expected_execution_digest,
        review_evidence: req.review_evidence.clone(),
        rejection_findings: None,
        custom_evidence: req.custom_evidence.clone(),
        verified_invariants: req.verified_invariants.clone(),
        snapshot_commit,
        summary: req.summary.clone(),
    }
}

fn eval_submit_or_mark_stale(
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    obs: &ValidatedObservations,
    input: &SubmitInput,
) -> Result<TransitionPlan, StateError> {
    match evaluate_submit(snapshot, machine_profile, obs, input)? {
        SubmitEvaluation::Apply(plan) => Ok(plan),
        SubmitEvaluation::MarkStaleAndReject {
            stale_node_ids,
            preconditions,
            error,
        } => {
            exec_tx_with(backend, |tx| {
                tx.mark_nodes_stale(
                    &snapshot.run.run_id,
                    &preconditions,
                    &stale_node_ids,
                    &obs.now_utc,
                )
            })?;
            Err(error)
        }
    }
}

fn extract_receipt_from_plan(plan: &TransitionPlan) -> Option<Receipt> {
    plan.new_artifacts
        .iter()
        .find(|a| a.kind == ArtifactKind::Receipt)
        .and_then(|a| serde_json::from_str::<Receipt>(&a.payload_json).ok())
}

fn collect_unblocked_cursors(
    plan: &TransitionPlan,
    submitted_node_id: &str,
) -> Result<Vec<String>, StateError> {
    let mut out = Vec::new();
    for patch in &plan.node_patches {
        if patch.node_id != submitted_node_id && patch.new_status == NodeStatus::Ready {
            if let Ok(c) = NodeCursor::from_node_id(&patch.node_id) {
                out.push(c.to_string());
            }
        }
    }
    Ok(out)
}
