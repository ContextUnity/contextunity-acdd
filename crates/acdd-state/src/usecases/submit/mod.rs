//! Gate submission orchestration (`submit`): freshness checks, `need:` stale-marking,
//! `save.post` (`none | commit | snapshot`), stateless archive retry,
//! and atomic `archive_and_prune_run` + post-commit cleanup.

mod archive;
mod budget;
mod freshness;
mod post_save;

use self::archive::RecoveredArchiveGitState;
use crate::git_ops::integrate::verify_tasks_integrated;
use crate::git_ops::snapshot::{
    current_scope_tree, diff_scope_changed_paths, remove_confined_file,
};
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::graph::is_post_task_root_node;
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    container_scope, evaluate_submit, MachineProfileInput, SubmitEvaluation, SubmitInput,
};
use crate::workflow::ResolvedWorkflow;
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArtifactKind, GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, RunStatus, SaveAction,
    SubmitAction, TransitionPlan, ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::collect_external_satisfied_deps;
use crate::usecases::dto::{SubmitReq, SubmitResp};
use acdd_spec::Receipt;
use crate::usecases::lifecycle::{
    acquire_repo_lock, append_trace_event, collect_integration_inputs, ensure_state_enabled,
    load_active_run_graph, parse_cursor_for_snapshot, primary_workspace_root,
    resolve_bound_node_worktree, resolve_repo_layout_optional, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::verify::find_machine_gate;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::fs::checked_child;
use acdd_core::ids::{CommitOid, NodeCursor, TreeOid};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::path::Path;

use archive::{finalize_with_prune, probe_interrupted_archive, relax_need_for_node};
use budget::enforce_blueprint_max_net_loc;
use freshness::{ensure_fresh_trace, inspect_submit_preflight, observe_existing_trace_freshness};
use post_save::{attach_container_blackboard_cleanup, execute_post_save_git};

pub(crate) fn handle_submit_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SubmitReq,
) -> Result<SubmitResp, AcddError> {
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

    // Phase 2 (no `repo.lock`): check domain freshness without running verification.
    let verified_hint = if let Some(pf) = preflight {
        let fresh_digest = match pf.existing_fresh {
            Some(ref tr) => observe_existing_trace_freshness(provider, &pf, &req, tr)?,
            None => None,
        };
        fresh_digest.map(|digest| (pf.current_tree, digest))
    } else {
        None
    };

    // Phase 3 (`repo.lock` + maintenance): final CAS checks, scoped commit/snapshot/archive, SQLite commit.
    with_repo_lock_and_maintenance(config, backend, |_primary, lock_layout| {
        submit_holding_repo_lock(config, backend, lock_layout, req, verified_hint.as_ref())
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_submit_observations<'m>(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    resolved_profile: &ResolvedWorkflow,
    machine_profile: &'m MachineProfileInput,
    node: &GateNodeRecord,
    machine_gate: &crate::machine::MachineGateDef,
    cursor: &NodeCursor,
    target_worktree: &Path,
    admitted_scope: &[String],
    current_tree: &TreeOid,
    policy: &WorkspacePathPolicy,
    recovered: Option<&RecoveredArchiveGitState>,
    req: &SubmitReq,
    verified_hint: Option<&(TreeOid, String)>,
) -> Result<
    (
        ValidatedObservations,
        Option<String>,
        std::borrow::Cow<'m, MachineProfileInput>,
    ),
    AcddError,
> {
    if recovered.is_some_and(|rec| rec.already_committed) {
        let mut relaxed = machine_profile.clone();
        relax_need_for_node(&mut relaxed, &node.gate_id);
        let obs = ValidatedObservations {
            current_tree: Some(current_tree.clone()),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };
        return Ok((obs, None, std::borrow::Cow::Owned(relaxed)));
    }

    let mut changed_paths = diff_scope_changed_paths(
        target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        policy,
    )?;
    if let Some(rec) = recovered {
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
    let scope_mode = crate::machine::scope::effective_scope_mode(snapshot, node);
    let scope_violations = detect_scope_violations(
        snapshot,
        machine_profile,
        &scope_obs,
        admitted_scope,
        is_unscoped_root,
        scope_mode,
    );
    if !scope_violations.is_empty() {
        return Err(AcddError::new(
            ErrorCode::OutOfScopeChanges,
            format!(
                "cannot submit '{}': working tree has out-of-scope changes: {}",
                node.node_id,
                scope_violations.join(", ")
            ),
        ));
    }

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(AcddError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{}' must be in_progress with an active claim before submit (status: {}, claim_revision: {})",
                node.node_id, node.status, node.claim_revision
            ),
        ));
    }
    if node.worker_id.as_ref() != Some(&req.worker_id) {
        return Err(AcddError::new(
            ErrorCode::TaskAlreadyClaimed,
            format!("node '{}' is claimed by another worker", node.node_id),
        ));
    }
    if req
        .claim_revision
        .is_some_and(|expected| expected != node.claim_revision)
    {
        return Err(AcddError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "submit claim_revision does not match active claim_revision {} on '{}'",
                node.claim_revision, node.node_id
            ),
        ));
    }

    let expected_digest = ensure_fresh_trace(
        backend,
        snapshot,
        resolved_profile,
        machine_profile,
        node,
        machine_gate,
        cursor,
        admitted_scope,
        current_tree,
        &changed_paths,
        policy,
        req,
        verified_hint,
    )?;

    let unintegrated_tasks = if machine_gate.receipt && is_post_task_root_node(snapshot, node) {
        let inputs = collect_integration_inputs(snapshot);
        verify_tasks_integrated(&snapshot.run.run_id, target_worktree, &inputs)?
    } else {
        Vec::new()
    };
    let external_satisfied_deps = collect_external_satisfied_deps(config, snapshot)?;
    let container_tree =
        if admitted_scope.is_empty() && machine_gate.save.post != SaveAction::Snapshot {
            None
        } else {
            Some(current_scope_tree(
                target_worktree,
                Some(admitted_scope),
                policy,
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
    Ok((
        obs,
        expected_digest,
        std::borrow::Cow::Borrowed(machine_profile),
    ))
}

/// Core `submit` implementation executed while `repo.lock` is held.
pub(crate) fn submit_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    req: SubmitReq,
    verified_hint: Option<&(TreeOid, String)>,
) -> Result<SubmitResp, AcddError> {
    let loaded =
        load_active_run_graph(config, backend, req.run.as_deref(), req.worktree.as_deref())?;
    let snapshot = loaded.snapshot;
    let resolved_profile = loaded.profile;
    let machine_profile = loaded.machine_profile;
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
            req,
        );
    }

    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "node '{node_id}' not found in run '{}'",
                    snapshot.run.run_id
                ),
            )
        })?;
    let _gate_def = find_profile_gate_def(&resolved_profile, &cursor).ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let machine_gate = find_machine_gate(&machine_profile, node)?;

    let primary = primary_workspace_root(config);
    let target_worktree =
        resolve_bound_node_worktree(config, &snapshot, node, req.worktree.as_deref())?;
    let policy = crate::usecases::lifecycle::build_path_policy(
        config,
        backend,
        &[],
        Some(&snapshot.run),
        Some(&target_worktree),
    )?;

    // Check stateless archive retry probe before trace/need checks when archive already landed before crash.
    let recovered = probe_interrupted_archive(
        config,
        backend,
        &snapshot,
        node,
        machine_gate,
        &target_worktree,
        &req,
        &policy,
    )?;

    let admitted_scope = container_scope(&snapshot, node);
    let current_tree = match recovered.as_ref() {
        Some(rec) => rec.current_tree.clone(),
        None => current_scope_tree(&target_worktree, None, &policy)?,
    };

    let (obs, expected_digest, effective_machine_profile) = prepare_submit_observations(
        config,
        backend,
        &snapshot,
        &resolved_profile,
        &machine_profile,
        node,
        machine_gate,
        &cursor,
        &target_worktree,
        &admitted_scope,
        &current_tree,
        &policy,
        recovered.as_ref(),
        &req,
        verified_hint,
    )?;

    let expected_scoped_tree = obs.container_tree.as_ref().unwrap_or(&current_tree);
    let was_recovered = recovered.is_some();
    let mut submit_input = make_submit_pass_input(&cursor, &req, expected_digest, None);
    let (committed_oid, archive_path_written, report_path_written) = if let Some(rec) = recovered {
        (rec.committed_oid, Some(rec.archive_rel), None)
    } else {
        // Pre-save pure evaluation before mutating Git state.
        let _ = eval_submit_or_mark_stale(
            backend,
            &snapshot,
            effective_machine_profile.as_ref(),
            &obs,
            &submit_input,
        )?;

        let is_drift_target = effective_machine_profile
            .gates
            .iter()
            .any(|item| match item {
                crate::machine::MachineGraphItem::TasksBlock(tb) => tb
                    .gates
                    .iter()
                    .any(|g| g.fail_drift.as_deref() == Some(machine_gate.id.as_str())),
                crate::machine::MachineGraphItem::Gate(_) => false,
            });
        let is_task_impl_gate = node.kind == NodeKind::TaskGate
            && !machine_gate.save.immutable
            && !machine_gate.receipt
            && machine_gate.independent_from.is_empty()
            && machine_gate.need.is_empty()
            && !is_drift_target;
        if is_task_impl_gate {
            enforce_blueprint_max_net_loc(
                config,
                backend,
                &snapshot,
                node,
                machine_gate,
                &target_worktree,
                &obs,
            )?;
        }

        execute_post_save_git(
            config,
            backend,
            layout,
            &primary,
            &target_worktree,
            &snapshot,
            node,
            machine_gate,
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
    submit_input.snapshot_commit = committed_oid.clone();
    let mut plan = eval_submit_or_mark_stale(
        backend,
        &snapshot,
        effective_machine_profile.as_ref(),
        &obs,
        &submit_input,
    )?;

    attach_container_blackboard_cleanup(config, backend, &snapshot, node, machine_gate, &mut plan)?;
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
        exec_tx_with(backend, |tx| {
            tx.apply_transition_plan(&snapshot.run.run_id, &plan)
        })?;
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

fn handle_reject(
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    cursor: &NodeCursor,
    req: SubmitReq,
) -> Result<SubmitResp, AcddError> {
    let node_id = cursor.to_node_id()?;
    if let Some(node) = snapshot.nodes.iter().find(|n| n.node_id == node_id) {
        let _ = resolve_bound_node_worktree(config, snapshot, node, req.worktree.as_deref())?;
    }
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

    exec_tx_with(backend, |tx| {
        tx.apply_transition_plan(&snapshot.run.run_id, &plan)
    })?;

    let new_node_status = plan
        .node_patches
        .iter()
        .find(|p| p.node_id == node_id)
        .map(|p| p.new_status)
        .unwrap_or(NodeStatus::Pending);
    let unblocked_nodes = collect_unblocked_cursors(&plan, &node_id)?;

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
) -> Result<TransitionPlan, AcddError> {
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
) -> Result<Vec<String>, AcddError> {
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
