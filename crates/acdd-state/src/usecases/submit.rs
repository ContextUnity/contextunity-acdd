//! Gate submission orchestration (`submit`): freshness checks, `need:` stale-marking,
//! `save.post` (`none | commit`), `git_commit_notes`, stateless archive retry,
//! and atomic `archive_and_prune_run` + post-commit cleanup.

use crate::git_ops::commit::{
    archive_spec_with_git, create_scoped_commit_with_expected, current_branch_name,
    current_head_commit, find_claim_commit_in_range, is_protected_main_branch, ArchiveCommitReq,
    ScopedCommitReq,
};
use crate::git_ops::integrate::verify_tasks_integrated;
use crate::git_ops::snapshot::{
    current_scope_tree, diff_scope_changed_paths, remove_confined_file,
};
use acdd_core::git::{git_command, read_git_blob_utf8};
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::graph::is_post_task_root_node;
use crate::machine::verify::detect_scope_violations;
use crate::machine::{
    compute_execution_digest, container_scope, evaluate_submit, plan_container_blackboard_cleanup,
    ExecutionDigestInput, MachineGateDef, MachineGraphItem, MachineProfileInput, MachineProofKind,
    SubmitEvaluation, SubmitInput,
};
use crate::observability::build_completed_run_digest;
use crate::profile::ResolvedGateProfile;
use crate::spec::{parse_spec, render_archive_markdown, ArchiveTaskUpdate, Receipt, SpecDocument, SpecStatus};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArtifactKind, BlackboardListReq, BlackboardRecord, BlackboardStatus, BlackboardTopic,
    CommitMode, GateNodeRecord, GateTraceRecord, NodeKind, NodeStatus, NodeVersionExpectation,
    RunGraphSnapshot, RunStatus, RunTombstoneRecord, SaveAction, StoragePreconditions,
    SubmitAction, TerminalStatus, TraceStatus, TransitionPlan, ValidatedObservations,
};
use crate::usecases::archive_notes::{
    attach_best_effort_git_note, build_archive_render_input, build_provisional_receipt,
};
use crate::usecases::dto::{SubmitReq, SubmitResp, VerifyReq};
use crate::usecases::lifecycle::{
    acquire_repo_lock, append_trace_event, build_path_policy,
    build_tombstone_cleanup_manifest_from_snapshot, collect_external_satisfied_deps,
    collect_integration_inputs, compute_archive_relpath, ensure_state_enabled,
    parse_cursor_for_snapshot, parse_profile_from_run, primary_workspace_root, read_confined_utf8,
    resolve_active_run, resolve_node_worktree, resolve_repo_layout_optional,
    resume_pending_tombstone_cleanups_dyn, to_machine_profile, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::verify::{
    find_machine_gate, handle_verify_dyn, resolve_commands_and_output_spec,
};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, SubtaskRef, TreeOid};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use acdd_core::ports::{CompositeDomainProvider, DomainCheckReq, FreshnessObservation};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Execute `StateCommand::Submit` with strict lock/transaction ordering and crash-safe recovery.
pub fn handle_submit<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: SubmitReq,
) -> Result<SubmitResp, StateError> {
    handle_submit_dyn(config, backend, provider, req)
}

struct SubmitPreflight {
    primary: std::path::PathBuf,
    target_worktree: std::path::PathBuf,
    snapshot: RunGraphSnapshot,
    resolved_profile: ResolvedGateProfile,
    node: GateNodeRecord,
    machine_gate: MachineGateDef,
    cursor: NodeCursor,
    admitted_scope: Vec<String>,
    current_tree: TreeOid,
    policy: WorkspacePathPolicy,
    existing_fresh: Option<GateTraceRecord>,
}

fn inspect_submit_preflight(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    primary: &Path,
    req: &SubmitReq,
) -> Result<Option<SubmitPreflight>, StateError> {
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
    let Some(node) = snapshot.nodes.iter().find(|n| n.node_id == node_id).cloned() else {
        return Ok(None);
    };
    if node.status != NodeStatus::InProgress
        || node.claim_revision == 0
        || node.worker_id.as_ref() != Some(&req.worker_id)
        || req
            .claim_revision
            .is_some_and(|expected| expected != node.claim_revision)
    {
        return Ok(None);
    }
    let machine_gate = find_machine_gate(&machine_profile, &node)?.clone();
    let target_worktree =
        resolve_node_worktree(config, &snapshot.run, &node, req.worktree.as_deref());

    if machine_gate.save.archive && !snapshot.run.is_ephemeral && machine_gate.save.post == SaveAction::Commit {
        if let Some(ref spec_rel) = snapshot.run.spec_path {
            let archive_rel =
                compute_archive_relpath(config, snapshot.run.collection.as_deref(), spec_rel)?;
            let trailer_needle = format!(
                "Acdd-Claim: {}/{}/{}",
                snapshot.run.run_id, node.node_id, node.claim_revision
            );
            if checked_child(&target_worktree, Path::new(&archive_rel)).is_ok_and(|p| p.is_file())
                && find_claim_commit_in_range(
                    &target_worktree,
                    snapshot.run.target_base_commit.as_ref(),
                    &trailer_needle,
                )?
                .is_some()
            {
                return Ok(None);
            }
        }
    }

    let needs_trace = matches!(
        machine_gate.proof,
        MachineProofKind::Contract | MachineProofKind::Command | MachineProofKind::Call
    ) || !machine_gate.checks.is_empty()
        || machine_gate.command.is_some()
        || machine_gate.call.is_some();
    if !needs_trace {
        return Ok(None);
    }

    let policy = build_path_policy(config, backend, &[])?;
    let current_tree = current_scope_tree(&target_worktree, None, &policy)?;
    if machine_gate.save.immutable && node.claim_tree.as_ref() != Some(&current_tree) {
        return Ok(None);
    }

    let admitted_scope = container_scope(&snapshot, &node);
    let is_unscoped_root = node.kind == NodeKind::RootGate && admitted_scope.is_empty();
    let changed_paths = diff_scope_changed_paths(
        &target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        &policy,
    )?;
    let pre_obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        changed_paths,
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };
    if !detect_scope_violations(
        &snapshot,
        &machine_profile,
        &pre_obs,
        &admitted_scope,
        is_unscoped_root,
    )
    .is_empty()
    {
        return Ok(None);
    }

    let required_status = match machine_gate.proof {
        MachineProofKind::Contract => TraceStatus::ExpectedFailure,
        _ => TraceStatus::Passed,
    };
    let existing_fresh = select_fresh_candidate_trace(
        &snapshot,
        &node,
        required_status,
        &current_tree,
    );

    Ok(Some(SubmitPreflight {
        primary: primary.to_path_buf(),
        target_worktree,
        snapshot,
        resolved_profile,
        node,
        machine_gate,
        cursor,
        admitted_scope,
        current_tree,
        policy,
        existing_fresh,
    }))
}

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
        } else if pf.machine_gate.proof != MachineProofKind::Contract {
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
        } else {
            None
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
            collect_external_satisfied_deps(config, &snapshot.run.run_id, &snapshot.dependencies)?;
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
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        };
        (obs, expected_digest, std::borrow::Cow::Borrowed(&machine_profile))
    };

    let expected_scoped_tree = obs.container_tree.as_ref().unwrap_or(&current_tree);
    let was_recovered = recovered.is_some();
    let (snapshot_ref, committed_oid, archive_path_written) = if let Some(rec) = recovered {
        (None, rec.committed_oid, Some(rec.archive_rel))
    } else {
        // Pre-save pure evaluation before mutating Git state.
        let pre_input = make_submit_pass_input(&cursor, &req, expected_digest.clone(), None, None);
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
            &resolved_profile,
            &node,
            &machine_gate,
            &cursor,
            &admitted_scope,
            &obs.changed_paths,
            &current_tree,
            expected_scoped_tree,
            &obs.now_utc,
            &req,
            &policy,
        )?
    };

    // Single shared submit evaluation, container blackboard cleanup, and SQLite finalizer.
    let final_input = make_submit_pass_input(
        &cursor,
        &req,
        expected_digest,
        snapshot_ref.clone(),
        committed_oid.clone(),
    );
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
            "snapshot_ref": snapshot_ref,
            "archive_path": archive_path_written,
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
        snapshot_ref,
        receipt: emitted_receipt,
        archive_path: archive_path_written,
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
        snapshot_ref: None,
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
        snapshot_ref: None,
        receipt: None,
        archive_path: None,
        unblocked_nodes,
    })
}

fn select_fresh_candidate_trace(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    required_status: TraceStatus,
    current_tree: &TreeOid,
) -> Option<GateTraceRecord> {
    snapshot
        .traces
        .iter()
        .filter(|t| {
            t.node_id == node.node_id
                && t.claim_revision == node.claim_revision
                && t.contract_revision == node.contract_revision
                && t.status == required_status
                && &t.candidate_tree_before == current_tree
                && t.candidate_tree_after.as_ref() == Some(current_tree)
        })
        .max_by_key(|t| t.trace_row_id.unwrap_or(0))
        .cloned()
}

#[allow(clippy::too_many_arguments)]
fn observe_existing_trace_freshness(
    provider: &CompositeDomainProvider,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    resolved_profile: &ResolvedGateProfile,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
    req: &SubmitReq,
    tr: &GateTraceRecord,
) -> Result<Option<String>, StateError> {
    let gate_def = find_profile_gate_def(resolved_profile, cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let checks_params = serde_json::Value::Array(
        machine_gate
            .checks
            .iter()
            .map(|c| json!({ "name": c.qualified_name, "params": c.params }))
            .collect(),
    );
    let effective_test_target = req.test_target.as_deref().or(tr.test_target.as_deref());
    let (trace_commands, trace_output_spec) = resolve_commands_and_output_spec(
        resolved_profile,
        &gate_def,
        admitted_scope,
        effective_test_target,
    )?;
    let live_changed_paths = diff_scope_changed_paths(
        target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        policy,
    )?;
    let check_req = DomainCheckReq {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: GateId::new(&gate_def.id)?,
        workspace_root: primary.to_path_buf(),
        worktree: target_worktree.to_path_buf(),
        admitted_scope: admitted_scope.to_vec(),
        changed_paths: live_changed_paths,
        test_target: effective_test_target.map(ToOwned::to_owned),
        path_policy: policy.clone(),
        baseline_ref: crate::context::baseline_ref_for_node(node),
    };
    let mut live_salts = Vec::new();
    for chk in &gate_def.checks {
        let (domain, check_name) = chk.check.split_once('.').ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("check '{}' must be qualified as '<domain>.<check>'", chk.check),
            )
        })?;
        match provider
            .observe_freshness(&chk.check, &chk.with, &check_req)
            .map_err(|err| {
                StateError::from_anyhow(&err).cloned().unwrap_or_else(|| {
                    StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("domain check '{}' freshness check errored: {err}", chk.check),
                    )
                })
            })?
        {
            FreshnessObservation::Passed {
                execution_digest_salt,
                ..
            } => {
                if let Some(salt) = execution_digest_salt {
                    live_salts.push(format!("{domain}.{check_name}:{salt}"));
                }
            }
            FreshnessObservation::Failed { .. } => return Ok(None),
        }
    }
    let live_files_digest = if live_salts.is_empty() {
        None
    } else {
        Some(live_salts.join("|"))
    };
    let mut digest_input = ExecutionDigestInput {
        contract_revision: node.contract_revision,
        gate_def_digest: machine_gate.definition_digest.clone(),
        admitted_scope: admitted_scope.to_vec(),
        resolved_commands: trace_commands,
        test_target: effective_test_target.map(ToOwned::to_owned),
        output_spec: trace_output_spec,
        call_with: machine_gate.with_params.clone(),
        checks_params,
        index_generation: None,
        live_files_digest,
    };
    let expected_digest = compute_execution_digest(&digest_input)?;
    if tr.execution_digest == expected_digest {
        return Ok(Some(expected_digest));
    }
    if gate_def.save.had_legacy_save_compat {
        let legacy_def_digest = gate_def.legacy_v060_definition_digest()?;
        if legacy_def_digest != machine_gate.definition_digest {
            digest_input.gate_def_digest = legacy_def_digest;
            let legacy_expected = compute_execution_digest(&digest_input)?;
            if tr.execution_digest == legacy_expected {
                return Ok(Some(legacy_expected));
            }
        }
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn mark_drifted_need_predecessors_stale(
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    changed_paths: &[String],
    req: &SubmitReq,
) -> Result<(), StateError> {
    let mut probe_profile = machine_profile.clone();
    for item in &mut probe_profile.gates {
        match item {
            MachineGraphItem::Gate(g) => {
                if g.id.as_str() == node.gate_id && node.kind == NodeKind::RootGate {
                    g.save.immutable = false;
                }
            }
            MachineGraphItem::TasksBlock(tb) => {
                if node.kind == NodeKind::TaskGate {
                    for g in &mut tb.gates {
                        if g.id.as_str() == node.gate_id {
                            g.save.immutable = false;
                        }
                    }
                }
            }
        }
    }
    let now_utc = acdd_core::telemetry::now_utc_iso_ms();
    let probe_obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        changed_paths: changed_paths.to_vec(),
        now_utc: now_utc.clone(),
        ..ValidatedObservations::default()
    };
    let probe_input = SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        worker_id: req.worker_id.clone(),
        claim_revision: req.claim_revision,
        expected_execution_digest: None,
        review_evidence: req.review_evidence.clone(),
        rejection_findings: None,
        custom_evidence: req.custom_evidence.clone(),
        verified_invariants: req.verified_invariants.clone(),
        snapshot_ref: None,
        snapshot_commit: None,
        summary: req.summary.clone(),
    };
    match evaluate_submit(snapshot, &probe_profile, &probe_obs, &probe_input) {
        Ok(SubmitEvaluation::MarkStaleAndReject {
            stale_node_ids,
            preconditions,
            ..
        }) => {
            exec_tx_with(backend, |tx| {
                tx.mark_nodes_stale(&snapshot.run.run_id, &preconditions, &stale_node_ids, &now_utc)
            })?;
        }
        Err(err) if err.code == ErrorCode::OutOfScopeChanges => {
            return Err(err);
        }
        Ok(_) | Err(_) => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ensure_fresh_trace_or_auto_verify(
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    resolved_profile: &ResolvedGateProfile,
    machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    _admitted_scope: &[String],
    current_tree: &TreeOid,
    changed_paths: &[String],
    req: &SubmitReq,
    verified_hint: Option<&(TreeOid, String)>,
) -> Result<Option<String>, StateError> {
    if machine_gate.save.immutable {
        if let Some(ref claim_tree) = node.claim_tree {
            if claim_tree != current_tree {
                mark_drifted_need_predecessors_stale(
                    backend,
                    snapshot,
                    machine_profile,
                    node,
                    cursor,
                    current_tree,
                    changed_paths,
                    req,
                )?;
                return Err(StateError::new(
                    ErrorCode::TaskCandidateMismatch,
                    format!(
                        "immutable gate '{}' working tree '{}' drifted from claim_tree '{}'",
                        node.node_id, current_tree, claim_tree
                    ),
                ));
            }
        }
    }

    let needs_trace = matches!(
        machine_gate.proof,
        MachineProofKind::Contract | MachineProofKind::Command | MachineProofKind::Call
    ) || !machine_gate.checks.is_empty()
        || machine_gate.command.is_some()
        || machine_gate.call.is_some();

    if !needs_trace {
        return Ok(None);
    }

    let _gate_def = find_profile_gate_def(resolved_profile, cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let required_status = match machine_gate.proof {
        MachineProofKind::Contract => TraceStatus::ExpectedFailure,
        _ => TraceStatus::Passed,
    };

    let existing_fresh =
        select_fresh_candidate_trace(snapshot, node, required_status, current_tree);

    if let (Some(ref tr), Some((hint_tree, hint_digest))) = (existing_fresh, verified_hint) {
        if hint_tree == current_tree && tr.execution_digest == *hint_digest {
            return Ok(Some(hint_digest.clone()));
        }
    }

    if machine_gate.proof == MachineProofKind::Contract {
        return Err(StateError::new(
            ErrorCode::TaskProofUnobserved,
            format!(
                "proof: contract on '{}' requires a fresh 'verify --test <target>' trace before submit",
                node.node_id
            ),
        ));
    }

    Err(StateError::new(
        ErrorCode::TaskCandidateMismatch,
        format!(
            "working tree changed or verify trace became stale for '{}' before submit lock acquisition",
            node.node_id
        ),
    ))
}

type PersistPostSaveOutcome = (Option<String>, Option<CommitOid>, Option<String>);

#[allow(clippy::too_many_arguments)]
fn execute_post_save_git(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    profile: &ResolvedGateProfile,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    changed_paths: &[String],
    current_tree: &TreeOid,
    expected_scoped_tree: &TreeOid,
    now_utc: &str,
    req: &SubmitReq,
    policy: &WorkspacePathPolicy,
) -> Result<PersistPostSaveOutcome, StateError> {
    let should_commit = machine_gate.save.post == SaveAction::Commit;

    if machine_gate.save.archive && !snapshot.run.is_ephemeral {
        let spec_rel = snapshot.run.spec_path.as_deref().ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "non-ephemeral run missing spec_path for archive",
            )
        })?;
        let archive_rel =
            compute_archive_relpath(config, snapshot.run.collection.as_deref(), spec_rel)?;
        let original_raw = read_confined_utf8(target_worktree, spec_rel)
            .or_else(|_| read_confined_utf8(primary, spec_rel))?;

        let provisional_receipt = build_provisional_receipt(
            snapshot,
            node,
            machine_gate,
            cursor,
            current_tree,
            None,
            now_utc,
            req,
        );
        let blackboard_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
        let render_input = build_archive_render_input(
            snapshot,
            Some(&provisional_receipt),
            &blackboard_items,
            None,
            now_utc,
            req.summary.as_deref(),
        );
        let (rendered_md, rendered_digest) = render_archive_markdown(&original_raw, &render_input)?;

        if should_commit {
            let branch = current_branch_name(target_worktree)?;
            if is_protected_main_branch(branch.as_deref()) && !req.allow_main {
                return Err(StateError::new(
                    ErrorCode::MainBranchProtected,
                    format!(
                        "refusing archive commit onto protected branch '{}' without --allow-main",
                        branch.as_deref().unwrap_or("main")
                    ),
                ));
            }
        }

        let arch_resp = archive_spec_with_git(&ArchiveCommitReq {
            worktree: target_worktree.to_path_buf(),
            run_id: snapshot.run.run_id.clone(),
            node_id: node.node_id.clone(),
            claim_revision: node.claim_revision,
            run_base_head: snapshot.run.target_base_commit.clone(),
            spec_relpath: spec_rel.to_owned(),
            archive_relpath: archive_rel.clone(),
            rendered_archive_markdown: rendered_md.clone(),
            rendered_archive_digest: rendered_digest,
            should_commit,
            commit_mode: profile.git.commit_mode,
            allow_main: req.allow_main,
            commit_message: format!(
                "acdd(archive): close {} ({})",
                snapshot.run.run_name, cursor
            ),
        })?;

        acdd_core::failpoint::hit("submit_archive_after_git");

        if let (Some(oid), Some(note_key)) = (
            arch_resp.committed_oid.as_ref(),
            machine_gate.save.git_commit_note.as_deref(),
        ) {
            attach_best_effort_git_note(
                target_worktree,
                profile,
                note_key,
                oid,
                snapshot,
                node,
                cursor,
                changed_paths,
                now_utc,
            );
        }

        return Ok((None, arch_resp.committed_oid, Some(arch_resp.archive_path)));
    }

    match machine_gate.save.post {
        SaveAction::None => Ok((None, None, None)),
        SaveAction::Commit => {
            let branch = current_branch_name(target_worktree)?;
            if is_protected_main_branch(branch.as_deref())
                && !req.allow_main
                && profile.git.commit_mode != CommitMode::Manual
            {
                return Err(StateError::new(
                    ErrorCode::MainBranchProtected,
                    format!(
                        "refusing scoped commit onto protected branch '{}' without --allow-main",
                        branch.as_deref().unwrap_or("main")
                    ),
                ));
            }
            let base_head = match node.baseline_commit.clone() {
                Some(c) => c,
                None => current_head_commit(target_worktree)?,
            };
            let commit_scope = if admitted_scope.is_empty() {
                vec!["**".to_owned()]
            } else {
                admitted_scope.to_vec()
            };
            let commit_resp = create_scoped_commit_with_expected(
                &ScopedCommitReq {
                    worktree: target_worktree.to_path_buf(),
                    run_id: snapshot.run.run_id.clone(),
                    node_id: node.node_id.clone(),
                    claim_revision: node.claim_revision,
                    base_head,
                    commit_mode: profile.git.commit_mode,
                    admitted_scope: commit_scope,
                    path_policy: policy.clone(),
                    message: format!("acdd({cursor}): accept gate"),
                    allow_main: req.allow_main,
                },
                Some(expected_scoped_tree),
            )?;

            acdd_core::failpoint::hit("submit_commit_after_git");

            if let (Some(oid), Some(note_key)) = (
                commit_resp.committed_oid.as_ref(),
                machine_gate.save.git_commit_note.as_deref(),
            ) {
                attach_best_effort_git_note(
                    target_worktree,
                    profile,
                    note_key,
                    oid,
                    snapshot,
                    node,
                    cursor,
                    changed_paths,
                    now_utc,
                );
            }

            Ok((commit_resp.snapshot_ref, commit_resp.committed_oid, None))
        }
    }
}

fn make_submit_pass_input(
    cursor: &NodeCursor,
    req: &SubmitReq,
    expected_execution_digest: Option<String>,
    snapshot_ref: Option<String>,
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
        snapshot_ref,
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

struct RecoveredArchiveGitState {
    archive_rel: String,
    committed_oid: Option<CommitOid>,
    already_committed: bool,
    current_tree: TreeOid,
}

#[allow(clippy::too_many_arguments)]
fn probe_interrupted_archive(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    req: &SubmitReq,
    policy: &WorkspacePathPolicy,
) -> Result<Option<RecoveredArchiveGitState>, StateError> {
    if !machine_gate.save.archive || snapshot.run.is_ephemeral {
        return Ok(None);
    }
    let Some(ref spec_rel) = snapshot.run.spec_path else {
        return Ok(None);
    };
    let archive_rel =
        compute_archive_relpath(config, snapshot.run.collection.as_deref(), spec_rel)?;
    let Ok(archive_abs) = checked_child(target_worktree, Path::new(&archive_rel)) else {
        return Ok(None);
    };
    if !archive_abs.is_file() {
        return Ok(None);
    }

    // Archive file already exists on disk; validate it strictly against run, claim, tree, and receipts.
    let existing_archive_bytes = fs::read(&archive_abs).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "failed reading existing archive '{}': {err}",
                archive_abs.display()
            ),
        )
    })?;
    let existing_digest = sha256_hex(&existing_archive_bytes);
    let existing_str = std::str::from_utf8(&existing_archive_bytes).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' is not valid UTF-8: {err}",
                archive_abs.display()
            ),
        )
    })?;
    let existing_doc = parse_spec(existing_str).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' failed spec validation: {err}",
                archive_abs.display()
            ),
        )
    })?;

    if existing_doc.status != SpecStatus::Completed {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' has non-completed status '{:?}'",
                archive_abs.display(),
                existing_doc.status
            ),
        ));
    }
    let Some(ref closure) = existing_doc.closure else {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' is missing required closure metadata",
                archive_abs.display()
            ),
        ));
    };
    if closure.status != SpecStatus::Completed {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' closure status '{:?}' is not completed",
                archive_abs.display(),
                closure.status
            ),
        ));
    }
    if let Some(ref expected_spec_id) = snapshot.run.spec_id {
        if &existing_doc.id != expected_spec_id {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "existing archive '{}' spec_id '{}' does not match active run spec_id '{}'",
                    archive_abs.display(),
                    existing_doc.id,
                    expected_spec_id
                ),
            ));
        }
    }
    if closure.target_base != snapshot.run.target_base
        || closure.target_base_commit != snapshot.run.target_base_commit
    {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' closure target_base does not match active run '{}'",
                archive_abs.display(),
                snapshot.run.run_id
            ),
        ));
    }

    if node.status != NodeStatus::InProgress || node.claim_revision == 0 {
        return Err(StateError::new(
            ErrorCode::StaleClaimRevision,
            format!(
                "node '{}' is not in_progress with an active claim (status: {})",
                node.node_id, node.status
            ),
        ));
    }
    if let Some(expected_rev) = req.claim_revision {
        if expected_rev != node.claim_revision {
            return Err(StateError::new(
                ErrorCode::StaleClaimRevision,
                format!(
                    "submit claim_revision {expected_rev} does not match active claim_revision {} on '{}'",
                    node.claim_revision, node.node_id
                ),
            ));
        }
    }
    if node.worker_id.as_ref() != Some(&req.worker_id) {
        return Err(StateError::new(
            ErrorCode::TaskAlreadyClaimed,
            format!(
                "worker '{}' does not hold the active claim on '{}'",
                req.worker_id, node.node_id
            ),
        ));
    }

    let current_tree = current_scope_tree(target_worktree, None, policy)?;
    if machine_gate.save.immutable && node.claim_tree.as_ref() != Some(&current_tree) {
        return Err(StateError::new(
            ErrorCode::TaskCandidateMismatch,
            format!(
                "immutable gate '{}' detected working-tree drift from claim_tree ({:?} != {})",
                node.node_id, node.claim_tree, current_tree
            ),
        ));
    }

    let trailer_needle = format!(
        "Acdd-Claim: {}/{}/{}",
        snapshot.run.run_id, node.node_id, node.claim_revision
    );
    let has_claim_commit = find_claim_commit_in_range(
        target_worktree,
        snapshot.run.target_base_commit.as_ref(),
        &trailer_needle,
    )?;

    let should_commit = machine_gate.save.post == SaveAction::Commit;
    if should_commit {
        let Some(ref commit_oid) = has_claim_commit else {
            if checked_child(target_worktree, Path::new(spec_rel)).is_ok_and(|p| p.is_file()) {
                return Ok(None);
            }
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "existing archive '{}' has no matching Acdd-Claim commit and source spec '{}' is missing",
                    archive_abs.display(),
                    spec_rel
                ),
            ));
        };
        let committed_blob = read_git_blob_utf8(target_worktree, commit_oid.as_str(), &archive_rel)
            .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
        if committed_blob.as_deref() != Some(existing_str) {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "existing archive '{}' does not match committed archive blob at '{}'",
                    archive_abs.display(),
                    commit_oid
                ),
            ));
        }
    }

    let provisional = build_provisional_receipt(
        snapshot,
        node,
        machine_gate,
        cursor,
        &current_tree,
        None,
        &closure.closed_at,
        req,
    );
    let blackboard_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    let render_input = build_archive_render_input(
        snapshot,
        Some(&provisional),
        &blackboard_items,
        None,
        &closure.closed_at,
        req.summary.as_deref(),
    );

    if closure.task_receipts != render_input.closure.task_receipts
        || closure
            .task_receipts
            .values()
            .any(|r| r.run_id != snapshot.run.run_id)
        || closure.metrics_summary != render_input.closure.metrics_summary
        || closure.rollup != render_input.closure.rollup
        || !archive_tasks_match_expected(&existing_doc, &render_input.tasks)
    {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "existing archive '{}' closure or task receipts do not match active run '{}' state",
                archive_abs.display(),
                snapshot.run.run_id
            ),
        ));
    }

    if !should_commit {
        let in_git = target_worktree.join(".git").exists()
            || acdd_core::git::git_common_dir(target_worktree).is_ok();
        let spec_raw_opt = match read_confined_utf8(target_worktree, spec_rel) {
            Ok(raw) => Some(raw),
            Err(_) if in_git => read_git_blob_utf8(target_worktree, "HEAD", spec_rel)
                .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?,
            Err(_) => None,
        };
        if let Some(ref spec_raw) = spec_raw_opt {
            let (_, expected_dig) = render_archive_markdown(spec_raw, &render_input)?;
            if expected_dig != existing_digest {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "existing archive '{}' digest '{}' does not match active run '{}'",
                        archive_abs.display(),
                        existing_digest,
                        snapshot.run.run_id
                    ),
                ));
            }
        } else if closure.task_receipts.is_empty() {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "cannot verify existing archive '{}' in no-commit mode without source spec '{}' or run-bound task receipts",
                    archive_abs.display(),
                    spec_rel
                ),
            ));
        }
    }

    let already_committed = should_commit && has_claim_commit.is_some();
    Ok(Some(RecoveredArchiveGitState {
        archive_rel,
        committed_oid: has_claim_commit,
        already_committed,
        current_tree,
    }))
}

fn archive_tasks_match_expected(
    existing_doc: &SpecDocument,
    expected: &BTreeMap<acdd_core::ids::TaskRef, ArchiveTaskUpdate>,
) -> bool {
    if existing_doc.tasks.len() != expected.len() {
        return false;
    }
    for task in &existing_doc.tasks {
        let Some(exp) = expected.get(&task.task_ref) else {
            return false;
        };
        if task.status != exp.status
            || task.extended_scope != exp.extended_scope
            || task.subtasks != exp.subtasks
            || task.receipt != exp.receipt
        {
            return false;
        }
    }
    true
}

fn relax_need_for_node(profile: &mut MachineProfileInput, gate_id: &str) {
    for item in &mut profile.gates {
        if let crate::machine::MachineGraphItem::Gate(g) = item {
            if g.id.as_str() == gate_id {
                g.need.clear();
                g.save.immutable = false;
            }
        }
    }
}

fn load_spec_for_tombstone(
    primary: &Path,
    target_worktree: &Path,
    archive_path: Option<&str>,
    closed_commit: Option<&CommitOid>,
    spec_path: Option<&str>,
) -> Option<crate::spec::SpecDocument> {
    if let (Some(arch_rel), Some(commit_oid)) = (archive_path, closed_commit) {
        if let Ok(Some(raw)) = read_git_blob_utf8(target_worktree, commit_oid.as_str(), arch_rel) {
            if let Ok(doc) = parse_spec(&raw) {
                return Some(doc);
            }
        }
    }
    let candidates = [
        archive_path.map(|p| (target_worktree, p)),
        archive_path.map(|p| (primary, p)),
        spec_path.map(|p| (target_worktree, p)),
        spec_path.map(|p| (primary, p)),
    ];
    for (root, rel) in candidates.into_iter().flatten() {
        if let Ok(raw) = read_confined_utf8(root, rel) {
            if let Ok(doc) = parse_spec(&raw) {
                return Some(doc);
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn finalize_with_prune(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    plan: &TransitionPlan,
    archive_path: Option<String>,
    closed_commit: Option<CommitOid>,
    now_utc: &str,
) -> Result<(), StateError> {
    let mut cleanup_manifest = build_tombstone_cleanup_manifest_from_snapshot(config, snapshot, primary);
    if archive_path.is_none() {
        cleanup_manifest.retain(|e| e.kind != "spec_file");
    } else if closed_commit.is_none() {
        if let Some(ref spec_rel) = snapshot.run.spec_path {
            let canon_target = acdd_core::fs::canonicalize_macos_var(target_worktree);
            if canon_target != acdd_core::fs::canonicalize_macos_var(primary)
                && checked_child(target_worktree, Path::new(spec_rel)).is_ok_and(|p| p.exists())
            {
                let ws_str = canon_target.to_string_lossy().into_owned();
                if !cleanup_manifest.iter().any(|e| {
                    e.kind == "spec_file"
                        && e.path_or_ref == *spec_rel
                        && e.workspace.as_deref() == Some(ws_str.as_str())
                }) {
                    cleanup_manifest.push(crate::types::ArchiveCleanupEntry {
                        kind: "spec_file".to_owned(),
                        path_or_ref: spec_rel.clone(),
                        workspace: Some(ws_str),
                    });
                }
            }
        }
    }
    let spec_doc = load_spec_for_tombstone(
        primary,
        target_worktree,
        archive_path.as_deref(),
        closed_commit.as_ref(),
        snapshot.run.spec_path.as_deref(),
    );
    let spec_id = spec_doc
        .as_ref()
        .map(|d| d.id.clone())
        .or_else(|| snapshot.run.spec_id.clone());
    let title = spec_doc
        .as_ref()
        .map(|d| d.title.clone())
        .unwrap_or_else(|| snapshot.run.run_name.as_str().to_owned());
    let closure = spec_doc.as_ref().and_then(|d| d.closure.as_ref());

    let new_gen = snapshot.run.generation + u64::from(plan.bump_run_generation);
    let tombstone_seed = RunTombstoneRecord {
        run_id: snapshot.run.run_id.clone(),
        run_name: snapshot.run.run_name.clone(),
        collection: snapshot.run.collection.clone(),
        workspace: snapshot.run.workspace.clone(),
        spec_id,
        title,
        profile_id: snapshot.run.profile_id.clone(),
        is_ephemeral: snapshot.run.is_ephemeral,
        generation: new_gen,
        terminal_status: TerminalStatus::Completed,
        target_base: snapshot.run.target_base.clone(),
        target_base_commit: snapshot.run.target_base_commit.clone(),
        archive_path,
        closed_commit,
        cleanup_pending: !cleanup_manifest.is_empty(),
        cleanup_manifest,
        summary_json: "{}".to_owned(),
        metrics_json: "{}".to_owned(),
        closed_at: now_utc.to_owned(),
    };

    let (_, summary_json, metrics_json) =
        build_completed_run_digest(snapshot, &tombstone_seed, closure)?;
    let tombstone = RunTombstoneRecord {
        summary_json,
        metrics_json,
        ..tombstone_seed
    };

    let post_plan_preconditions = StoragePreconditions {
        expected_run_generation: new_gen,
        expected_node_versions: plan
            .preconditions
            .expected_node_versions
            .iter()
            .map(|ev| {
                let bumped = if plan.node_patches.iter().any(|p| p.node_id == ev.node_id) {
                    ev.expected_version + 1
                } else {
                    ev.expected_version
                };
                NodeVersionExpectation {
                    node_id: ev.node_id.clone(),
                    expected_version: bumped,
                }
            })
            .collect(),
        expected_claim_revision: None,
        expected_contract_revision: None,
        expected_worker_id: None,
        required_predecessor_trees: Vec::new(),
    };

    exec_tx_with(backend, |tx| {
        tx.apply_transition_plan(&snapshot.run.run_id, plan)?;
        tx.archive_and_prune_run(&post_plan_preconditions, &tombstone)?;
        Ok(())
    })?;

    acdd_core::failpoint::hit("submit_archive_after_sqlite");

    let _ = resume_pending_tombstone_cleanups_dyn(config, backend, layout)?;
    Ok(())
}

fn list_all_blackboard_items(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    run_id: &RunId,
) -> Result<Vec<BlackboardRecord>, StateError> {
    if !config.modules.is_enabled("blackboard") {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = backend.list_blackboard(
            run_id,
            &BlackboardListReq {
                limit: 100,
                offset,
                ..BlackboardListReq::default()
            },
        )?;
        let fetched = page.items.len();
        items.extend(page.items);
        if fetched == 0 || items.len() >= page.total {
            break;
        }
        offset += fetched;
    }
    Ok(items)
}

fn attach_container_blackboard_cleanup(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    plan: &mut TransitionPlan,
) -> Result<(), StateError> {
    if !machine_gate.receipt || !config.modules.is_enabled("blackboard") {
        return Ok(());
    }
    let Some(ref container_id) = node.parent_id else {
        return Ok(());
    };
    let all_items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    let patches = plan_container_blackboard_cleanup(snapshot, container_id, &all_items);

    let mut seen_patch_ids: BTreeSet<i64> =
        plan.blackboard_patches.iter().map(|p| p.id).collect();
    for patch in patches {
        if seen_patch_ids.insert(patch.id) {
            plan.blackboard_patches.push(patch);
        }
    }

    // Track already-existing run-level CarriedOver Deferred payloads to prevent duplication on retry.
    let mut existing_carried: BTreeSet<(Option<SubtaskRef>, String)> = all_items
        .iter()
        .filter(|item| {
            item.node_id.is_none()
                && item.topic == BlackboardTopic::Deferred
                && item.status == BlackboardStatus::CarriedOver
        })
        .map(|item| (item.subtask_ref.clone(), item.payload.clone()))
        .collect();
    for bb in &plan.new_blackboard {
        if bb.node_id.is_none()
            && bb.topic == BlackboardTopic::Deferred
            && bb.status == BlackboardStatus::CarriedOver
        {
            existing_carried.insert((bb.subtask_ref.clone(), bb.payload.clone()));
        }
    }

    // Promote carried-over deferred items at run level by inserting carried-over notice if needed.
    for p in &plan.blackboard_patches {
        if p.status == BlackboardStatus::CarriedOver {
            if let Some(src) = all_items.iter().find(|i| i.id == Some(p.id)) {
                let key = (src.subtask_ref.clone(), src.payload.clone());
                if existing_carried.insert(key) {
                    plan.new_blackboard.push(BlackboardRecord {
                        id: None,
                        run_id: snapshot.run.run_id.clone(),
                        node_id: None,
                        recipient_node_id: None,
                        subtask_ref: src.subtask_ref.clone(),
                        gate: src.gate.clone(),
                        author: src.author.clone(),
                        topic: BlackboardTopic::Deferred,
                        status: BlackboardStatus::CarriedOver,
                        payload: src.payload.clone(),
                        created_at: acdd_core::telemetry::now_utc_iso_ms(),
                    });
                }
            }
        }
    }
    Ok(())
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

#[allow(clippy::too_many_arguments)]
fn enforce_blueprint_max_net_loc(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    target_worktree: &Path,
    current_tree: &TreeOid,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<(), StateError> {
    if snapshot.run.is_ephemeral {
        return Ok(());
    }
    let (Some(ref baseline_commit), Some(ref task_ref)) = (&node.baseline_commit, &node.task_ref)
    else {
        return Ok(());
    };
    let Some(max_net_loc) =
        crate::usecases::queries::extract_pre_task_slice(snapshot, task_ref.as_str())
            .and_then(|slice| slice.get("max_net_loc").and_then(serde_json::Value::as_i64))
    else {
        return Ok(());
    };

    let net_loc = compute_scoped_prod_net_loc(
        target_worktree,
        baseline_commit.as_str(),
        current_tree,
        admitted_scope,
        policy,
    )?;
    if net_loc <= max_net_loc {
        return Ok(());
    }

    if has_container_budget_decision(config, backend, snapshot, node, net_loc)? {
        return Ok(());
    }

    Err(StateError::new(
        ErrorCode::GateVerificationFailed,
        format!(
            "task '{task_ref}' production net LOC delta (+{net_loc}) exceeds blueprint budget max_net_loc={max_net_loc}; reuse existing canonical owners or simplify the implementation, or record a `BUDGET_OVERRIDE: <reason>` decision in `acdd_blackboard` (topic: decisions) / submit with `--action escalate`"
        ),
    ))
}

fn compute_scoped_prod_net_loc(
    worktree: &Path,
    baseline_commit: &str,
    current_tree: &TreeOid,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<i64, StateError> {
    let output = git_command(
        worktree,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            baseline_commit,
            current_tree.as_str(),
        ],
    )
    .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "git diff --numstat {} {} failed in '{}': {stderr}",
                baseline_commit,
                current_tree,
                worktree.display()
            ),
        ));
    }
    let mut net_loc: i64 = 0;
    for raw_record in output.stdout.split(|b| *b == 0) {
        if raw_record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(raw_record);
        let mut parts = record.splitn(3, '\t');
        let (Some(added_raw), Some(deleted_raw), Some(path_raw)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if added_raw == "-" || deleted_raw == "-" {
            continue;
        }
        let Ok(added) = added_raw.parse::<i64>() else {
            continue;
        };
        let Ok(deleted) = deleted_raw.parse::<i64>() else {
            continue;
        };
        let Some(rel) = normalize_rel_path(path_raw) else {
            continue;
        };
        if !policy.is_candidate_included(&rel) {
            continue;
        }
        if !admitted_scope.is_empty() && !policy.is_scope_admitted(&rel, admitted_scope) {
            continue;
        }
        if is_test_path(&rel) {
            continue;
        }
        net_loc += added - deleted;
    }
    Ok(net_loc)
}

fn is_test_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower
        .split('/')
        .any(|seg| matches!(seg, "test" | "tests" | "__tests__"))
    {
        return true;
    }
    let file_name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    file_name == "tests.rs"
        || file_name == "test.rs"
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
        || file_name.ends_with("_test.go")
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_spec.rb")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
}

fn has_container_budget_decision(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    net_loc: i64,
) -> Result<bool, StateError> {
    if !config.modules.is_enabled("blackboard") {
        return Ok(false);
    }
    let container_id = node.parent_id.as_deref().unwrap_or(&node.node_id);
    let container_node_ids: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| n.node_id == container_id || n.parent_id.as_deref() == Some(container_id))
        .map(|n| n.node_id.as_str())
        .collect();
    let items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    Ok(items.into_iter().any(|item| {
        item.topic == BlackboardTopic::Decisions
            && item
                .node_id
                .as_deref()
                .is_some_and(|nid| container_node_ids.contains(nid))
            && is_explicit_budget_override(&item.payload, net_loc)
    }))
}

fn is_explicit_budget_override(payload: &str, net_loc: i64) -> bool {
    let trimmed = payload.trim();
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if val.get("budget_override").and_then(serde_json::Value::as_bool) == Some(true) {
            return true;
        }
        if val
            .get("approved_max_net_loc")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|approved| approved >= net_loc)
        {
            return true;
        }
        return false;
    }
    trimmed.lines().any(|raw_line| {
        let line = raw_line.trim();
        let rest = ["budget_override:", "approved_budget:"]
            .iter()
            .find_map(|prefix| {
                line.get(..prefix.len())
                    .filter(|head| head.eq_ignore_ascii_case(prefix))
                    .map(|_| (prefix, line[prefix.len()..].trim()))
            });
        match rest {
            Some((&"approved_budget:", value)) => {
                if let Ok(approved) = value.parse::<i64>() {
                    approved >= net_loc
                } else {
                    !value.is_empty()
                }
            }
            Some((_, value)) => !value.is_empty(),
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::memory::MemoryStateBackend;
    use acdd_core::ids::{GateId, SubtaskRef};
    use acdd_core::response::ResponsePolicy;
    use acdd_core::subsystems::SubsystemRegistry;

    #[test]
    fn blackboard_cleanup_paginates_beyond_100_without_duplicating_carried_over() {
        let run_id = RunId::new("run-bb-page").expect("run_id");
        let snapshot = crate::usecases::archive_notes::tests::sample_snapshot(&run_id);
        let backend = MemoryStateBackend::new().expect("memory backend");
        let config = StateEngineConfig {
            workspace_root: Path::new("/tmp/ws").to_path_buf(),
            worktree: Path::new("/tmp/ws").to_path_buf(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: BTreeMap::new(),
            profiles_dirs: Vec::new(),
            linked_workspaces: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            response_policy: ResponsePolicy::default(),
            retain_traces: 5,
        };
        let s1 = SubtaskRef::new("S1").expect("subtask_ref");
        exec_tx_with(&backend, |tx| {
            tx.insert_run_graph(&snapshot)?;
            // Insert >100 container draft notes plus an already carried-over deferred item.
            for idx in 0..105 {
                tx.insert_blackboard(&BlackboardRecord {
                    id: None,
                    run_id: run_id.clone(),
                    node_id: Some("task:T1".to_owned()),
                    recipient_node_id: None,
                    subtask_ref: None,
                    gate: None,
                    author: "w1".to_owned(),
                    topic: BlackboardTopic::Draft,
                    status: BlackboardStatus::Open,
                    payload: format!("draft-{idx}"),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                })?;
            }
            tx.insert_blackboard(&BlackboardRecord {
                id: None,
                run_id: run_id.clone(),
                node_id: Some("task:T1".to_owned()),
                recipient_node_id: None,
                subtask_ref: Some(s1.clone()),
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Deferred,
                status: BlackboardStatus::Open,
                payload: "defer-once".to_owned(),
                created_at: "2026-01-01T00:00:01Z".to_owned(),
            })?;
            tx.insert_blackboard(&BlackboardRecord {
                id: None,
                run_id: run_id.clone(),
                node_id: None,
                recipient_node_id: None,
                subtask_ref: Some(s1),
                gate: None,
                author: "w1".to_owned(),
                topic: BlackboardTopic::Deferred,
                status: BlackboardStatus::CarriedOver,
                payload: "defer-once".to_owned(),
                created_at: "2026-01-01T00:00:02Z".to_owned(),
            })?;
            Ok(())
        })
        .expect("seed >100 bb");

        let mut plan = TransitionPlan {
            preconditions: StoragePreconditions {
                expected_run_generation: 1,
                expected_node_versions: vec![],
                expected_claim_revision: None,
                expected_contract_revision: None,
                expected_worker_id: None,
                required_predecessor_trees: vec![],
            },
            bump_run_generation: false,
            new_run_status: None,
            new_root_cycle_count: None,
            completed_at: None,
            node_patches: vec![],
            dependency_patches: vec![],
            claim_patches: vec![],
            new_artifacts: vec![],
            new_blackboard: vec![],
            blackboard_patches: vec![],
            new_subtasks: vec![],
            significant_transition: false,
        };
        let deliver = &snapshot.nodes[1];
        let mut gate = MachineGateDef::minimal(GateId::new("deliver").expect("gate_id"));
        gate.receipt = true;

        attach_container_blackboard_cleanup(&config, &backend, &snapshot, deliver, &gate, &mut plan)
            .expect("cleanup pagination");

        // All 105 drafts + 1 deferred container item are patched (> 100!).
        assert_eq!(plan.blackboard_patches.len(), 106);
        // Existing run-level CarriedOver record prevented duplication.
        assert!(plan.new_blackboard.is_empty());
    }
}
