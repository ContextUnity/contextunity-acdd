//! Gate submission orchestration (`submit`): freshness checks, `need:` stale-marking,
//! `save.post` (`none | snapshot | commit`), `git_commit_notes`, stateless archive retry,
//! and atomic `archive_and_prune_run` + post-commit cleanup.

use crate::git_ops::commit::{
    archive_spec_with_git, create_scoped_commit, current_branch_name, current_head_commit,
    find_claim_commit_in_range, is_protected_main_branch, write_git_note, ArchiveCommitReq,
    GitNoteReq, ScopedCommitReq,
};
use crate::git_ops::integrate::verify_tasks_integrated;
use crate::git_ops::snapshot::{
    capture_snapshot, current_scope_tree, diff_scope_changed_paths, remove_confined_file,
    CaptureSnapshotReq,
};
use crate::git_ops::worktree::{canonicalize_macos_var, RepoWorktreeLayout};
use crate::machine::graph::is_post_task_root_node;
use crate::machine::{
    compute_execution_digest, container_scope, evaluate_submit, plan_container_blackboard_cleanup,
    ExecutionDigestInput, MachineGateDef, MachineProfileInput, MachineProofKind, SubmitEvaluation,
    SubmitInput,
};
use crate::observability::build_completed_run_digest;
use crate::profile::{GitCommitNoteFormat, GitCommitNotePolicy, ResolvedGateProfile};
use crate::spec::{
    parse_spec, render_archive_markdown, ArchiveRenderInput, ArchiveTaskUpdate, ClosureSummary,
    Receipt, ReceiptRollup, SpecStatus, Subtask,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArchiveCleanupEntry, ArtifactKind, BlackboardListReq, BlackboardStatus, BlackboardTopic,
    CommitMode, GateNodeRecord, InvariantsState, NodeKind, NodeStatus, NodeVersionExpectation,
    RunGraphSnapshot, RunStatus, RunTombstoneRecord, SaveAction, StoragePreconditions,
    SubmitAction, TerminalStatus, TraceStatus, TransitionPlan, ValidatedObservations,
};
use crate::usecases::dto::{SubmitReq, SubmitResp, VerifyReq};
use crate::usecases::lifecycle::{
    append_trace_event, build_path_policy, collect_external_satisfied_deps,
    collect_integration_inputs, effective_spec_collections, match_spec_collection, now_utc_iso,
    parse_cursor_for_snapshot, parse_profile_from_run, primary_workspace_root, read_confined_utf8,
    resolve_active_run, resolve_node_worktree, resume_pending_tombstone_cleanups_dyn,
    to_machine_profile, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::verify::{find_machine_gate, resolve_commands_and_output_spec, verify_holding_repo_lock};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::{sha256_hex, utf8_floor_boundary};
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::ids::{CommitOid, NodeCursor, TreeOid};
use acdd_core::path_policy::WorkspacePathPolicy;
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::collections::BTreeMap;
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

pub(crate) fn handle_submit_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SubmitReq,
) -> Result<SubmitResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, layout| {
        submit_holding_repo_lock(config, backend, provider, layout, req)
    })
}

/// Core `submit` implementation executed while `repo.lock` is held.
pub(crate) fn submit_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    req: SubmitReq,
) -> Result<SubmitResp, StateError> {
    let run = resolve_active_run(
        config,
        backend,
        req.run.as_deref(),
        req.worktree.as_deref(),
    )?;
    let mut snapshot = backend
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

    // Check stateless archive retry path before trace/need checks when archive was already written/committed before crash.
    if machine_gate.save.archive && !snapshot.run.is_ephemeral {
        if let Some(resp) = try_recover_interrupted_archive(
            config,
            backend,
            layout,
            &primary,
            &target_worktree,
            &snapshot,
            &resolved_profile,
            &machine_profile,
            &node,
            &machine_gate,
            &cursor,
            &req,
            &policy,
        )? {
            return Ok(resp);
        }
    }

    // Auto-verify if required trace is missing/stale (or fail closed with TASK_PROOF_UNOBSERVED for `proof: contract`).
    let admitted_scope = container_scope(&snapshot, &node);
    let current_tree = current_scope_tree(&target_worktree, None, &policy)?;
    let expected_digest = ensure_fresh_trace_or_auto_verify(
        config,
        backend,
        provider,
        layout,
        &mut snapshot,
        &resolved_profile,
        &machine_profile,
        &node,
        &machine_gate,
        &cursor,
        &admitted_scope,
        &current_tree,
        &req,
    )?;

    // Re-read node & current_tree after potential auto-verify.
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| StateError::new(ErrorCode::BackendConflict, "node disappeared"))?
        .clone();
    let current_tree = current_scope_tree(&target_worktree, None, &policy)?;
    let changed_paths = diff_scope_changed_paths(
        &target_worktree,
        node.isolation,
        node.baseline_commit.as_ref(),
        node.claim_tree.as_ref(),
        &policy,
    )?;

    let unintegrated_tasks = if machine_gate.receipt && is_post_task_root_node(&snapshot, &node) {
        let inputs = collect_integration_inputs(&snapshot);
        verify_tasks_integrated(&snapshot.run.run_id, &target_worktree, &inputs)?
    } else {
        Vec::new()
    };
    let external_satisfied_deps =
        collect_external_satisfied_deps(config, &snapshot.run.run_id, &snapshot.dependencies)?;

    let obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        container_tree: if admitted_scope.is_empty() {
            None
        } else {
            Some(current_scope_tree(&target_worktree, Some(&admitted_scope), &policy)?)
        },
        changed_paths,
        out_of_scope_paths: Vec::new(),
        unintegrated_tasks,
        external_satisfied_deps,
        now_utc: now_utc_iso(),
    };

    // Pre-save pure evaluation: checks scope barrier, immutable, need: drift (marks stale!), trace freshness, proof evidence, independent_from.
    let pre_input = SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        worker_id: req.worker_id.clone(),
        claim_revision: req.claim_revision,
        expected_execution_digest: expected_digest.clone(),
        review_evidence: req.review_evidence.clone(),
        rejection_findings: None,
        custom_evidence: req.custom_evidence.clone(),
        verified_invariants: req.verified_invariants.clone(),
        snapshot_ref: None,
        snapshot_commit: None,
        summary: req.summary.clone(),
    };

    let pre_eval = evaluate_submit(&snapshot, &machine_profile, &obs, &pre_input)?;
    if let SubmitEvaluation::MarkStaleAndReject {
        stale_node_ids,
        preconditions,
        error,
    } = pre_eval
    {
        exec_tx_with(backend, |tx| {
            tx.mark_nodes_stale(&snapshot.run.run_id, &preconditions, &stale_node_ids, &obs.now_utc)
        })?;
        return Err(error);
    }

    // Execute Git `save.post` / `archive` OUTSIDE any SQLite transaction.
    let (snapshot_ref, committed_oid, archive_path_written) = execute_post_save_git(
        config,
        &primary,
        &target_worktree,
        &snapshot,
        &resolved_profile,
        &node,
        &machine_gate,
        &cursor,
        &admitted_scope,
        &current_tree,
        &obs.now_utc,
        &req,
        &policy,
    )?;

    // Build final transition plan with snapshot/commit metadata + container blackboard cleanup.
    let final_input = SubmitInput {
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        worker_id: req.worker_id.clone(),
        claim_revision: req.claim_revision,
        expected_execution_digest: expected_digest,
        review_evidence: req.review_evidence.clone(),
        rejection_findings: None,
        custom_evidence: req.custom_evidence.clone(),
        verified_invariants: req.verified_invariants.clone(),
        snapshot_ref: snapshot_ref.clone(),
        snapshot_commit: committed_oid.clone(),
        summary: req.summary.clone(),
    };

    let mut plan = match evaluate_submit(&snapshot, &machine_profile, &obs, &final_input)? {
        SubmitEvaluation::Apply(plan) => plan,
        SubmitEvaluation::MarkStaleAndReject { error, .. } => return Err(error),
    };

    attach_container_blackboard_cleanup(config, backend, &snapshot, &node, &machine_gate, &mut plan)?;

    let emitted_receipt = extract_receipt_from_plan(&plan);

    // Commit atomically (and archive+prune if `save.prune == true`).
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
        now_utc: now_utc_iso(),
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

#[allow(clippy::too_many_arguments)]
fn ensure_fresh_trace_or_auto_verify(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    snapshot: &mut RunGraphSnapshot,
    resolved_profile: &ResolvedGateProfile,
    _machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    current_tree: &TreeOid,
    req: &SubmitReq,
) -> Result<Option<String>, StateError> {
    if machine_gate.save.immutable {
        if let Some(ref claim_tree) = node.claim_tree {
            if claim_tree != current_tree {
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

    let gate_def = find_profile_gate_def(resolved_profile, cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;
    let (resolved_commands, output_spec) = resolve_commands_and_output_spec(
        resolved_profile,
        &gate_def,
        admitted_scope,
        req.test_target.as_deref(),
    )?;
    let checks_params = serde_json::Value::Array(
        machine_gate
            .checks
            .iter()
            .map(|c| json!({ "name": c.qualified_name, "params": c.params }))
            .collect(),
    );

    let required_status = match machine_gate.proof {
        MachineProofKind::Contract => TraceStatus::ExpectedFailure,
        _ => TraceStatus::Passed,
    };

    let existing_fresh = snapshot
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
        .cloned();

    if let Some(ref tr) = existing_fresh {
        let effective_test_target = req.test_target.as_deref().or(tr.test_target.as_deref());
        let (trace_commands, trace_output_spec) =
            if effective_test_target == req.test_target.as_deref() {
                (resolved_commands.clone(), output_spec.clone())
            } else {
                resolve_commands_and_output_spec(
                    resolved_profile,
                    &gate_def,
                    admitted_scope,
                    effective_test_target,
                )?
            };
        let salt_from_trace = extract_live_files_digest(&tr.check_results);
        let expected_digest = compute_execution_digest(&ExecutionDigestInput {
            contract_revision: node.contract_revision,
            gate_def_digest: machine_gate.definition_digest.clone(),
            admitted_scope: admitted_scope.to_vec(),
            resolved_commands: trace_commands,
            test_target: effective_test_target.map(ToOwned::to_owned),
            output_spec: trace_output_spec,
            call_with: machine_gate.with_params.clone(),
            checks_params: checks_params.clone(),
            index_generation: None,
            live_files_digest: salt_from_trace,
        })?;
        if tr.execution_digest == expected_digest {
            return Ok(Some(expected_digest));
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

    let verify_resp = verify_holding_repo_lock(
        config,
        backend,
        provider,
        layout,
        VerifyReq {
            run: Some(snapshot.run.run_id.as_str().to_owned()),
            node: cursor.to_string(),
            worker_id: Some(req.worker_id.clone()),
            test_target: req.test_target.clone(),
            claim_revision: Some(node.claim_revision),
            worktree: req.worktree.clone(),
        },
    )?;

    *snapshot = backend
        .load_run_graph(&snapshot.run.run_id)?
        .ok_or_else(|| StateError::new(ErrorCode::BackendConflict, "run missing after verify"))?;

    Ok(Some(verify_resp.execution_digest))
}

fn extract_live_files_digest(checks: &[crate::types::CheckResultRecord]) -> Option<String> {
    let salts: Vec<String> = checks
        .iter()
        .filter_map(|c| {
            c.execution_digest_salt
                .as_ref()
                .map(|s| format!("{}.{}:{s}", c.domain, c.check))
        })
        .collect();
    if salts.is_empty() {
        None
    } else {
        Some(salts.join("|"))
    }
}

type PersistPostSaveOutcome = (Option<String>, Option<CommitOid>, Option<String>);

#[allow(clippy::too_many_arguments)]
fn execute_post_save_git(
    config: &StateEngineConfig,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    profile: &ResolvedGateProfile,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    admitted_scope: &[String],
    current_tree: &TreeOid,
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
        let archive_rel = compute_archive_relpath(config, snapshot, spec_rel)?;
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
        let render_input = build_archive_render_input(
            snapshot,
            Some(&provisional_receipt),
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

        sync_archive_to_primary(
            primary,
            target_worktree,
            &arch_resp.archive_path,
            rendered_md.as_bytes(),
        )?;

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
            );
        }

        return Ok((None, arch_resp.committed_oid, Some(arch_resp.archive_path)));
    }

    match machine_gate.save.post {
        SaveAction::None => Ok((None, None, None)),
        SaveAction::Snapshot => {
            let snap = capture_snapshot(&CaptureSnapshotReq {
                worktree: target_worktree.to_path_buf(),
                run_id: snapshot.run.run_id.clone(),
                namespace: sanitize_ref_segment(&node.node_id),
                label: format!("rev-{}", node.claim_revision),
                scope: if admitted_scope.is_empty() {
                    None
                } else {
                    Some(admitted_scope.to_vec())
                },
                path_policy: policy.clone(),
                message: format!(
                    "acdd snapshot {} rev-{}",
                    cursor, node.claim_revision
                ),
            })?;
            Ok((Some(snap.snapshot_ref), Some(snap.commit_oid), None))
        }
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
            let commit_resp = create_scoped_commit(&ScopedCommitReq {
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
                manual_snapshot_namespace: Some(sanitize_ref_segment(&node.node_id)),
            })?;

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
                );
            }

            Ok((commit_resp.snapshot_ref, commit_resp.committed_oid, None))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn try_recover_interrupted_archive(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    target_worktree: &Path,
    snapshot: &RunGraphSnapshot,
    profile: &ResolvedGateProfile,
    machine_profile: &MachineProfileInput,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    req: &SubmitReq,
    policy: &WorkspacePathPolicy,
) -> Result<Option<SubmitResp>, StateError> {
    let Some(spec_rel) = snapshot.run.spec_path.as_deref() else {
        return Ok(None);
    };
    let archive_rel = compute_archive_relpath(config, snapshot, spec_rel)?;
    let Ok(archive_abs) = checked_child(target_worktree, Path::new(&archive_rel)) else {
        return Ok(None);
    };
    if !archive_abs.is_file() {
        return Ok(None);
    }

    // Archive file already exists on disk; check if it matches an interrupted archive commit or digest.
    let existing_archive_bytes = fs::read(&archive_abs).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed reading existing archive '{}': {err}", archive_abs.display()),
        )
    })?;
    let existing_digest = sha256_hex(&existing_archive_bytes);

    let existing_doc = std::str::from_utf8(&existing_archive_bytes)
        .ok()
        .and_then(|s| parse_spec(s).ok());

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
    if has_claim_commit.is_none() && should_commit {
        return Ok(None);
    }

    // Also confirm if spec_path is already gone or archive matches when !should_commit.
    if !should_commit {
        if let Ok(spec_raw) = read_confined_utf8(target_worktree, spec_rel) {
            let recovery_closed_at = existing_doc
                .as_ref()
                .and_then(|d| d.closure.as_ref())
                .map(|c| c.closed_at.clone())
                .unwrap_or_else(now_utc_iso);
            let current_tree = current_scope_tree(target_worktree, None, policy)?;
            let provisional = build_provisional_receipt(
                snapshot,
                node,
                machine_gate,
                cursor,
                &current_tree,
                None,
                &recovery_closed_at,
                req,
            );
            let render_input = build_archive_render_input(
                snapshot,
                Some(&provisional),
                None,
                &recovery_closed_at,
                req.summary.as_deref(),
            );
            if let Ok((_, expected_dig)) = render_archive_markdown(&spec_raw, &render_input) {
                if expected_dig != existing_digest {
                    return Ok(None);
                }
            }
        }
    }

    if checked_child(target_worktree, Path::new(spec_rel)).is_ok_and(|p| p.exists()) {
        remove_confined_file(target_worktree, spec_rel)?;
    }
    sync_archive_to_primary(primary, target_worktree, &archive_rel, &existing_archive_bytes)?;

    // Stateless recovery: skip `need:` check and skip re-committing; directly finalize SQLite + cleanup.
    let current_tree = current_scope_tree(target_worktree, None, policy)?;
    let now_utc = now_utc_iso();
    let obs = ValidatedObservations {
        current_tree: Some(current_tree.clone()),
        now_utc: now_utc.clone(),
        ..ValidatedObservations::default()
    };

    // Bypass `need:` by building a relaxed machine gate copy only for final artifact/plan construction.
    let mut relaxed_profile = machine_profile.clone();
    relax_need_for_node(&mut relaxed_profile, &node.gate_id);

    let submit_input = SubmitInput {
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
        snapshot_commit: has_claim_commit.clone(),
        summary: req.summary.clone(),
    };

    let mut plan = match evaluate_submit(snapshot, &relaxed_profile, &obs, &submit_input)? {
        SubmitEvaluation::Apply(p) => p,
        SubmitEvaluation::MarkStaleAndReject { error, .. } => return Err(error),
    };
    attach_container_blackboard_cleanup(config, backend, snapshot, node, machine_gate, &mut plan)?;
    let emitted_receipt = extract_receipt_from_plan(&plan);

    if machine_gate.save.prune {
        finalize_with_prune(
            config,
            backend,
            layout,
            primary,
            target_worktree,
            snapshot,
            &plan,
            Some(archive_rel.clone()),
            has_claim_commit.clone(),
            &now_utc,
        )?;
    } else {
        exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;
    }

    let unblocked_nodes = collect_unblocked_cursors(&plan, &node.node_id)?;
    let _ = profile;
    Ok(Some(SubmitResp {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        action: SubmitAction::Pass,
        new_node_status: NodeStatus::Completed,
        run_status: if machine_gate.save.prune {
            RunStatus::Completed
        } else {
            snapshot.run.status
        },
        committed_oid: has_claim_commit,
        snapshot_ref: None,
        receipt: emitted_receipt,
        archive_path: Some(archive_rel),
        unblocked_nodes,
    }))
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

fn sync_archive_to_primary(
    primary: &Path,
    target_worktree: &Path,
    archive_rel: &str,
    archive_bytes: &[u8],
) -> Result<(), StateError> {
    if canonicalize_macos_var(target_worktree) == canonicalize_macos_var(primary) {
        return Ok(());
    }
    let primary_archive = checked_child(primary, Path::new(archive_rel)).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("primary archive path '{archive_rel}' failed confinement check: {err}"),
        )
    })?;
    atomic_write(&primary_archive, archive_bytes, false).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed syncing archive '{archive_rel}' to primary workspace: {err}"),
        )
    })?;
    Ok(())
}

fn load_spec_for_tombstone(
    primary: &Path,
    target_worktree: &Path,
    archive_path: Option<&str>,
    spec_path: Option<&str>,
) -> Option<crate::spec::SpecDocument> {
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
    let cleanup_manifest = build_cleanup_manifest(config, snapshot, primary);
    let spec_doc = load_spec_for_tombstone(
        primary,
        target_worktree,
        archive_path.as_deref(),
        snapshot.run.spec_path.as_deref(),
    );
    let spec_id = spec_doc.as_ref().map(|d| d.id.clone());
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

pub(crate) fn build_cleanup_manifest(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
    primary: &Path,
) -> Vec<ArchiveCleanupEntry> {
    let mut entries = Vec::new();
    let canon_primary = canonicalize_macos_var(primary);

    if let Some(ref sp) = snapshot.run.spec_path {
        entries.push(ArchiveCleanupEntry {
            kind: "spec_file".to_owned(),
            path_or_ref: sp.clone(),
            workspace: None,
        });
    }

    let mut seen_wt = std::collections::BTreeSet::new();
    let mut seen_br = std::collections::BTreeSet::new();

    for node in &snapshot.nodes {
        if node.kind == NodeKind::Task {
            if let Some(ref wt) = node.worktree {
                let wt_path = canonicalize_macos_var(Path::new(wt));
                if wt_path != canon_primary && seen_wt.insert(wt.clone()) {
                    entries.push(ArchiveCleanupEntry {
                        kind: "worktree".to_owned(),
                        path_or_ref: wt.clone(),
                        workspace: None,
                    });
                }
            }
            if let Some(ref br) = node.branch {
                if snapshot.run.branch.as_ref() != Some(br) && seen_br.insert(br.clone()) {
                    entries.push(ArchiveCleanupEntry {
                        kind: "branch".to_owned(),
                        path_or_ref: br.clone(),
                        workspace: None,
                    });
                }
            }
        }
    }

    if let Some(ref run_wt) = snapshot.run.worktree {
        let wt_path = canonicalize_macos_var(Path::new(run_wt));
        if wt_path != canon_primary && seen_wt.insert(run_wt.clone()) {
            entries.push(ArchiveCleanupEntry {
                kind: "worktree".to_owned(),
                path_or_ref: run_wt.clone(),
                workspace: None,
            });
        }
    }

    for ws in config.linked_workspaces.values() {
        entries.push(ArchiveCleanupEntry {
            kind: "snapshot_ref".to_owned(),
            path_or_ref: format!("refs/acdd/snapshots/{}", snapshot.run.run_id),
            workspace: Some(canonicalize_macos_var(ws).to_string_lossy().into_owned()),
        });
    }
    entries.push(ArchiveCleanupEntry {
        kind: "snapshot_ref".to_owned(),
        path_or_ref: format!("refs/acdd/snapshots/{}", snapshot.run.run_id),
        workspace: None,
    });

    entries
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
    let page = backend.list_blackboard(
        &snapshot.run.run_id,
        &BlackboardListReq {
            limit: 100,
            offset: 0,
            ..BlackboardListReq::default()
        },
    )?;
    let patches = plan_container_blackboard_cleanup(snapshot, container_id, &page.items);
    plan.blackboard_patches.extend(patches);

    // Promote carried-over deferred items at run level by inserting carried-over notice if needed.
    for p in &plan.blackboard_patches {
        if p.status == BlackboardStatus::CarriedOver {
            if let Some(src) = page.items.iter().find(|i| i.id == Some(p.id)) {
                plan.new_blackboard.push(crate::types::BlackboardRecord {
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
                    created_at: now_utc_iso(),
                });
            }
        }
    }
    Ok(())
}

fn compute_archive_relpath(
    config: &StateEngineConfig,
    snapshot: &RunGraphSnapshot,
    spec_relpath: &str,
) -> Result<String, StateError> {
    let collections = effective_spec_collections(config);
    let col_key = snapshot
        .run
        .collection
        .clone()
        .or_else(|| match_spec_collection(&collections, spec_relpath));

    let file_name = Path::new(spec_relpath)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("spec_path '{spec_relpath}' has no file name"),
            )
        })?;

    if let Some(key) = col_key {
        if let Some(cfg) = collections.get(&key) {
            return Ok(format!("{}/archive/{file_name}", cfg.path.trim_end_matches('/')));
        }
    }
    let parent = Path::new(spec_relpath)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .trim_end_matches('/');
    if parent.is_empty() {
        Ok(format!("archive/{file_name}"))
    } else {
        Ok(format!("{parent}/archive/{file_name}"))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_provisional_receipt(
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    machine_gate: &MachineGateDef,
    cursor: &NodeCursor,
    current_tree: &TreeOid,
    committed_oid: Option<CommitOid>,
    now_utc: &str,
    req: &SubmitReq,
) -> Receipt {
    Receipt {
        run_id: snapshot.run.run_id.clone(),
        node_cursor: cursor.clone(),
        gate_id: machine_gate.id.clone(),
        worker_id: req.worker_id.clone(),
        claim_revision: node.claim_revision,
        contract_revision: node.contract_revision,
        candidate_tree: current_tree.clone(),
        committed_oid,
        accepted_at: now_utc.to_owned(),
        need_proofs: BTreeMap::new(),
        rollup: Some(ReceiptRollup {
            verified_invariants: InvariantsState {
                applicable: req.verified_invariants.clone(),
                verified: req.verified_invariants.clone(),
            },
            decisions: Vec::new(),
            deferred_findings: Vec::new(),
            review_summary: req.summary.clone(),
        }),
    }
}

fn build_archive_render_input(
    snapshot: &RunGraphSnapshot,
    closing_receipt: Option<&Receipt>,
    closed_commit: Option<CommitOid>,
    now_utc: &str,
    summary: Option<&str>,
) -> ArchiveRenderInput {
    let mut tasks = BTreeMap::new();
    let mut task_receipts = BTreeMap::new();
    let mut all_applicable = Vec::new();
    let mut all_verified = Vec::new();

    for node in &snapshot.nodes {
        if node.kind != NodeKind::Task {
            continue;
        }
        let Some(ref tref) = node.task_ref else {
            continue;
        };
        let status = match node.status {
            NodeStatus::Completed => SpecStatus::Completed,
            NodeStatus::Cancelled => SpecStatus::Cancelled,
            NodeStatus::InProgress => SpecStatus::Active,
            _ => SpecStatus::Planned,
        };
        for inv in &node.invariants.applicable {
            if !all_applicable.contains(inv) {
                all_applicable.push(inv.clone());
            }
        }
        for inv in &node.invariants.verified {
            if !all_verified.contains(inv) {
                all_verified.push(inv.clone());
            }
        }
        let subtasks: Vec<Subtask> = snapshot
            .subtasks
            .iter()
            .filter(|s| s.node_id == node.node_id)
            .map(|s| Subtask {
                subtask_ref: s.subtask_ref.clone(),
                title: s.title.clone(),
                status: s.status,
                evidence: s.evidence.clone(),
            })
            .collect();

        let prefix = format!("task:{tref}:");
        let task_receipt = snapshot
            .artifacts
            .iter()
            .filter(|a| a.kind == ArtifactKind::Receipt && a.node_id.starts_with(&prefix))
            .max_by_key(|a| a.claim_revision)
            .and_then(|a| serde_json::from_str::<Receipt>(&a.payload_json).ok());
        if let Some(ref rec) = task_receipt {
            task_receipts.insert(tref.clone(), rec.clone());
        }

        tasks.insert(
            tref.clone(),
            ArchiveTaskUpdate {
                status,
                extended_scope: node.extended_scope.clone(),
                subtasks,
                receipt: task_receipt,
            },
        );
    }

    let _ = closing_receipt;

    ArchiveRenderInput {
        closure: ClosureSummary {
            status: SpecStatus::Completed,
            closed_at: now_utc.to_owned(),
            target_base: snapshot.run.target_base.clone(),
            target_base_commit: snapshot.run.target_base_commit.clone(),
            closed_commit,
            task_receipts,
            rollup: ReceiptRollup {
                verified_invariants: InvariantsState {
                    applicable: all_applicable,
                    verified: all_verified,
                },
                decisions: Vec::new(),
                deferred_findings: Vec::new(),
                review_summary: summary.map(str::to_owned),
            },
            metrics_summary: BTreeMap::new(),
        },
        tasks,
    }
}

fn attach_best_effort_git_note(
    worktree: &Path,
    profile: &ResolvedGateProfile,
    note_key: &str,
    commit_oid: &CommitOid,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
) {
    let Some(policy) = profile.git_commit_notes.get(note_key) else {
        return;
    };
    let body = render_git_commit_note(policy, snapshot, node, cursor);
    let _ = write_git_note(&GitNoteReq {
        worktree: worktree.to_path_buf(),
        commit_oid: commit_oid.clone(),
        policy: policy.clone(),
        body,
    });
}

fn render_git_commit_note(
    policy: &GitCommitNotePolicy,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
) -> String {
    let raw = match policy.format {
        GitCommitNoteFormat::Json => json!({
            "run_id": snapshot.run.run_id.as_str(),
            "run_name": snapshot.run.run_name.as_str(),
            "node_cursor": cursor.to_string(),
            "claim_revision": node.claim_revision,
            "attempt": node.attempt,
            "isolation": snapshot.run.isolation.as_str(),
        })
        .to_string(),
        GitCommitNoteFormat::Markdown => format!(
            "# ACDD Gate Commit Note\n\n- Run: `{}` (`{}`)\n- Node: `{}` (rev {})\n- Isolation: `{}`\n",
            snapshot.run.run_name,
            snapshot.run.run_id,
            cursor,
            node.claim_revision,
            snapshot.run.isolation
        ),
    };
    if raw.len() <= policy.max_bytes {
        raw
    } else {
        let end = utf8_floor_boundary(&raw, policy.max_bytes);
        raw[..end].to_owned()
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

fn sanitize_ref_segment(node_id: &str) -> String {
    node_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
