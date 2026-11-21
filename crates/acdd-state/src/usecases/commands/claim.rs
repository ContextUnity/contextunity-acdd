//! `claim` command handler, worktree materialization, and context-reset checks.

use super::ensure_no_managed_worktree_collision;
use crate::context::{GateContextAssembler, SnapshotContextInput};
use crate::git_ops::commit::{current_branch_name, current_head_commit};
use crate::git_ops::snapshot::current_scope_tree;
use crate::git_ops::worktree::{
    canonicalize_macos_var, create_task_worktree, remove_worktree_and_branch, CreateWorktreeReq,
    RepoWorktreeLayout,
};
use crate::machine::{
    evaluate_transition, ClaimDecision, ClaimInput, TransitionInput,
    TransitionOutcome,
};
use crate::profile::ResolvedGateProfile;
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    GateNodeRecord, IsolationMode, NodeKind, NodeStatus, NodeStatusPatch, NodeVersionExpectation,
    RunGraphSnapshot, StoragePreconditions, TransitionPlan, ValidatedObservations,
    WorktreeLifecycle,
};
use crate::usecases::dto::ClaimReq;
use crate::usecases::lifecycle::{
    append_trace_event, auto_select_ready_cursor, collect_external_satisfied_deps,
    compute_unintegrated_tasks, current_session_fingerprint, is_local_pid_alive,
    load_active_run_graph, parse_cursor_for_snapshot, resolve_node_worktree, with_repo_lock_and_maintenance,
};
use crate::usecases::queries::find_profile_gate_def;
use crate::usecases::StateEngineConfig;
use acdd_core::telemetry::SpanId;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::TreeOid;
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::path::{Path, PathBuf};


pub(crate) fn handle_claim_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: ClaimReq,
) -> Result<crate::context::ClaimResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        claim_holding_repo_lock(config, backend, provider, layout, primary, req)
    })
}

fn claim_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    req: ClaimReq,
) -> Result<crate::context::ClaimResp, StateError> {
    let loaded = load_active_run_graph(
        config,
        backend,
        req.run.as_deref(),
        req.worktree.as_deref(),
    )?;
    let run_id = loaded.snapshot.run.run_id.clone();
    let ext_deps = collect_external_satisfied_deps(config, &loaded.snapshot)?;
    let unintegrated_tasks = compute_unintegrated_tasks(config, &loaded.snapshot)?;
    let now_utc = acdd_core::telemetry::now_utc_iso_ms();

    let sched_obs = ValidatedObservations {
        external_satisfied_deps: ext_deps.clone(),
        external_deps_authoritative: true,
        unintegrated_tasks,
        now_utc: now_utc.clone(),
        ..ValidatedObservations::default()
    };

    let cursor = match req.node {
        Some(ref raw) => parse_cursor_for_snapshot(&loaded.snapshot, raw)?,
        None => auto_select_ready_cursor(&loaded.snapshot, &sched_obs)?,
    };
    let node_id = cursor.to_node_id()?;
    let node = loaded
        .snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .cloned()
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node '{cursor}' does not exist in run '{run_id}'"),
            )
        })?;
    let gate_def = find_profile_gate_def(&loaded.profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;

    let session_fp = req
        .session_fingerprint
        .clone()
        .or_else(|| Some(current_session_fingerprint()));
    let previous_local_pid_alive = if node.status == NodeStatus::InProgress {
        is_local_pid_alive(node.session_fingerprint.as_deref())
    } else {
        None
    };
    let span_id = SpanId::new().to_string();

    let (planned_wt, planned_branch, create_wt_req) = plan_claim_worktree(
        config,
        primary,
        layout.is_some(),
        &loaded.snapshot,
        &loaded.profile,
        &node,
        req.worktree.as_deref(),
    )?;

    // Run pure `admit_claim` validation BEFORE creating any managed task worktree/branch
    // so rejected claims never leak Git worktrees or branches.
    if create_wt_req.is_some() {
        let precheck_input = ClaimInput {
            node_cursor: cursor.clone(),
            worker_id: req.worker_id.clone(),
            session_fingerprint: session_fp.clone(),
            worktree: planned_wt.to_string_lossy().into_owned(),
            branch: planned_branch.clone(),
            baseline_commit: None,
            claim_tree: TreeOid::new("0".repeat(40))?,
            span_id: span_id.clone(),
            force: req.force,
            steal_remote: req.steal_remote,
            previous_local_pid_alive,
        };
        let _ = evaluate_transition(
            &loaded.snapshot,
            &loaded.machine_profile,
            &sched_obs,
            &TransitionInput::Claim(precheck_input),
        )?;
    }

    let (target_worktree, target_branch, created_managed) = if let Some(ref wt_req) = create_wt_req
    {
        let created = create_task_worktree(wt_req)?;
        (
            created.worktree.clone(),
            Some(created.branch.clone()),
            Some((created.worktree, created.branch)),
        )
    } else {
        (planned_wt, planned_branch, None)
    };

    let result = (|| -> Result<crate::context::ClaimResp, StateError> {
        let policy = loaded.path_policy(config, backend, Some(&target_worktree))?;
        let claim_tree = current_scope_tree(&target_worktree, None, &policy)?;
        let baseline_commit = if layout.is_some() {
            current_head_commit(&target_worktree).ok()
        } else {
            None
        };

        let claim_input = ClaimInput {
            node_cursor: cursor.clone(),
            worker_id: req.worker_id.clone(),
            session_fingerprint: session_fp,
            worktree: target_worktree.to_string_lossy().into_owned(),
            branch: target_branch,
            baseline_commit,
            claim_tree,
            span_id,
            force: req.force,
            steal_remote: req.steal_remote,
            previous_local_pid_alive,
        };

        let decision = evaluate_transition(
            &loaded.snapshot,
            &loaded.machine_profile,
            &sched_obs,
            &TransitionInput::Claim(claim_input),
        )?;
        let TransitionOutcome::Claim(decision) = decision else {
            unreachable!("Claim input always yields Claim outcome");
        };

        match *decision {
            ClaimDecision::Idempotent(existing) => {
                let ctx_req = GateContextAssembler::build_request_from_snapshot(
                    SnapshotContextInput {
                        config,
                        reader: backend,
                        snapshot: &loaded.snapshot,
                        node: &node,
                        gate: gate_def,
                        profile: loaded.profile,
                        claim_revision: existing.claim_revision,
                        worker_id: req.worker_id,
                        worktree: target_worktree,
                        candidate_tree: existing.claim_tree,
                        path_policy: policy,
                        primary,
                    },
                )?;
                GateContextAssembler::assemble(&ctx_req, provider)
            }
            ClaimDecision::Admit { claim, transition } => {
                // Assemble full gate context BEFORE committing irreversible claim state so
                // budget/provider failures leave SQLite untouched and trigger worktree rollback.
                let mut candidate_node = node.clone();
                candidate_node.status = NodeStatus::InProgress;
                candidate_node.claim_revision = claim.claim_revision;
                candidate_node.worker_id = Some(claim.worker_id.clone());
                candidate_node.session_fingerprint = claim.session_fingerprint.clone();
                candidate_node.worktree = Some(claim.worktree.clone());
                candidate_node.branch = claim.branch.clone();
                candidate_node.baseline_commit = claim.baseline_commit.clone();
                candidate_node.claim_tree = Some(claim.claim_tree.clone());
                candidate_node.active_span_id = Some(claim.span_id.clone());

                let ctx_req = GateContextAssembler::build_request_from_snapshot(
                    SnapshotContextInput {
                        config,
                        reader: backend,
                        snapshot: &loaded.snapshot,
                        node: &candidate_node,
                        gate: gate_def,
                        profile: loaded.profile,
                        claim_revision: claim.claim_revision,
                        worker_id: req.worker_id,
                        worktree: target_worktree,
                        candidate_tree: claim.claim_tree.clone(),
                        path_policy: policy,
                        primary,
                    },
                )?;
                let claim_resp = GateContextAssembler::assemble(&ctx_req, provider)?;

                let parent_patches: Vec<NodeStatusPatch> = transition
                    .node_patches
                    .iter()
                    .filter(|p| p.node_id != claim.node_id)
                    .cloned()
                    .collect();

                exec_tx_with(backend, |tx| {
                    tx.admit_claim(&loaded.snapshot.run.run_id, &transition.preconditions, &claim)?;
                    if !parent_patches.is_empty() {
                        let parent_versions: Vec<NodeVersionExpectation> = parent_patches
                            .iter()
                            .filter_map(|p| {
                                loaded
                                    .snapshot
                                    .nodes
                                    .iter()
                                    .find(|n| n.node_id == p.node_id)
                                    .map(|n| NodeVersionExpectation {
                                        node_id: n.node_id.clone(),
                                        expected_version: n.node_version,
                                    })
                            })
                            .collect();
                        let parent_plan = TransitionPlan {
                            preconditions: StoragePreconditions {
                                expected_run_generation: loaded.snapshot.run.generation + 1,
                                expected_node_versions: parent_versions,
                                expected_claim_revision: None,
                                expected_contract_revision: None,
                                expected_worker_id: None,
                                required_predecessor_trees: Vec::new(),
                            },
                            bump_run_generation: false,
                            new_run_status: None,
                            new_root_cycle_count: None,
                            completed_at: None,
                            node_patches: parent_patches,
                            dependency_patches: Vec::new(),
                            claim_patches: Vec::new(),
                            new_artifacts: Vec::new(),
                            new_blackboard: Vec::new(),
                            blackboard_patches: Vec::new(),
                            new_subtasks: Vec::new(),
                            significant_transition: true,
                        };
                        tx.apply_transition_plan(&loaded.snapshot.run.run_id, &parent_plan)?;
                    }
                    Ok(())
                })?;

                Ok(claim_resp)
            }
        }
    })();

    let claim_resp = match result {
        Ok(resp) => resp,
        Err(err) => {
            if let Some((wt, br)) = created_managed {
                let _ = remove_worktree_and_branch(primary, Some(&wt), Some(&br), true);
            }
            return Err(err);
        }
    };

    let _ = append_trace_event(
        layout,
        primary,
        &run_id,
        &json!({
            "kind": "gate_claim",
            "run_id": run_id.as_str(),
            "node_cursor": claim_resp.node_cursor.to_string(),
            "claim_revision": claim_resp.claim_revision,
            "worker_id": claim_resp.worker_id.as_str(),
            "worktree": claim_resp.worktree,
            "timestamp": now_utc,
        }),
    );

    Ok(claim_resp)
}

#[allow(clippy::too_many_arguments)]
fn plan_claim_worktree(
    config: &StateEngineConfig,
    primary: &Path,
    has_git: bool,
    snapshot: &RunGraphSnapshot,
    profile: &ResolvedGateProfile,
    node: &GateNodeRecord,
    override_wt: Option<&Path>,
) -> Result<(PathBuf, Option<String>, Option<CreateWorktreeReq>), StateError> {
    if let Some(wt) = override_wt {
        let canon = canonicalize_macos_var(wt);
        let branch = if has_git && canon.exists() {
            current_branch_name(&canon)?
        } else {
            node.branch.clone()
        };
        if profile.git.worktrees == WorktreeLifecycle::Managed {
            let parent_wt = node.parent_id.as_deref().and_then(|pid| {
                snapshot
                    .nodes
                    .iter()
                    .find(|n| n.node_id == pid)
                    .and_then(|p| p.worktree.as_deref())
            });
            let existing = node
                .worktree
                .as_deref()
                .or(parent_wt)
                .or(snapshot.run.worktree.as_deref());
            ensure_no_managed_worktree_collision(
                primary,
                snapshot.run.run_name.as_str(),
                &canon,
                branch.as_deref(),
                existing,
            )?;
        }
        return Ok((canon, branch, None));
    }

    if let Some(ref wt) = node.worktree {
        let p = canonicalize_macos_var(Path::new(wt));
        return Ok((p, node.branch.clone(), None));
    }

    if node.kind == NodeKind::TaskGate {
        if let Some(ref pid) = node.parent_id {
            if let Some(parent_task) = snapshot.nodes.iter().find(|n| &n.node_id == pid) {
                if let Some(ref pwt) = parent_task.worktree {
                    let p = canonicalize_macos_var(Path::new(pwt));
                    return Ok((p, parent_task.branch.clone(), None));
                }
                if has_git
                    && snapshot.run.isolation == IsolationMode::Worktree
                    && profile.git.worktrees == WorktreeLifecycle::Managed
                {
                    if let Some(ref tref) = parent_task.task_ref {
                        let wt_path = primary
                            .join(".worktrees")
                            .join(format!("{}-{tref}", snapshot.run.run_name));
                        let canon_wt = canonicalize_macos_var(&wt_path);
                        let branch_name = format!("task/{}-{tref}", snapshot.run.run_name);
                        let base_ref = snapshot
                            .run
                            .branch
                            .as_deref()
                            .or(snapshot.run.target_base.as_deref())
                            .unwrap_or("HEAD")
                            .to_owned();
                        let copy_paths: Vec<String> =
                            snapshot.run.spec_path.iter().cloned().collect();
                        return Ok((
                            canon_wt,
                            Some(branch_name.clone()),
                            Some(CreateWorktreeReq {
                                primary_worktree: primary.to_path_buf(),
                                worktree_path: wt_path,
                                branch_name,
                                base_ref,
                                copy_uncommitted_paths: copy_paths,
                            }),
                        ));
                    }
                }
            }
        }
    }

    let wt = resolve_node_worktree(config, &snapshot.run, node, None);
    let branch = if has_git && wt.exists() {
        current_branch_name(&wt)?
    } else {
        snapshot.run.branch.clone()
    };
    Ok((wt, branch, None))
}
