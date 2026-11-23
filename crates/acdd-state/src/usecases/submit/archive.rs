//! Interrupted archive crash-recovery probing and atomic `archive_and_prune_run` finalization.

use crate::git_ops::commit::find_claim_commit_in_range;
use crate::git_ops::snapshot::current_scope_tree;
use acdd_core::git::read_git_blob_utf8;
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::machine::{
    MachineGateDef, MachineProfileInput,
};
use crate::observability::build_completed_run_digest;
use crate::spec::{parse_spec, render_archive_markdown, ArchiveTaskUpdate, SpecDocument, SpecStatus};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    GateNodeRecord, NodeStatus, NodeVersionExpectation,
    RunGraphSnapshot, RunTombstoneRecord, SaveAction, StoragePreconditions, TerminalStatus, TransitionPlan,
};
use crate::usecases::archive_notes::{build_archive_render_input, build_provisional_receipt};
use crate::usecases::dto::SubmitReq;
use crate::usecases::lifecycle::{
    build_tombstone_cleanup_manifest_from_snapshot, compute_archive_relpath, read_confined_utf8,
    resume_pending_tombstone_cleanups_dyn,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::config::CLAIM_TRAILER_KEY;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::checked_child;
use acdd_core::ids::{CommitOid, NodeCursor, TreeOid};
use acdd_core::path_policy::WorkspacePathPolicy;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::post_save::list_all_blackboard_items;

pub(super) struct RecoveredArchiveGitState {
    pub(super) archive_rel: String,
    pub(super) committed_oid: Option<CommitOid>,
    pub(super) already_committed: bool,
    pub(super) current_tree: TreeOid,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn probe_interrupted_archive(
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
        "{CLAIM_TRAILER_KEY}: {}/{}/{}",
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

pub(super) fn relax_need_for_node(profile: &mut MachineProfileInput, gate_id: &str) {
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

fn is_git_tracked(worktree: &Path, rel_path: &str) -> bool {
    acdd_core::git::git_command(worktree, &["ls-files", "--cached", "--error-unmatch", "--", rel_path])
        .is_ok_and(|out| out.status.success())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_with_prune(
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
    } else if let Some(ref spec_rel) = snapshot.run.spec_path {
        let canon_primary = acdd_core::fs::canonicalize_macos_var(primary);
        let canon_target = acdd_core::fs::canonicalize_macos_var(target_worktree);
        // When an isolated worktree run archived via a commit (`closed_commit`), never delete a
        // Git-tracked `spec_rel` from `primary` before the run branch is merged — doing so leaves
        // `main` dirty with an unstaged deletion (`D <spec_rel>`).
        if closed_commit.is_some() && canon_target != canon_primary && is_git_tracked(primary, spec_rel) {
            cleanup_manifest.retain(|e| !(e.kind == "spec_file" && e.workspace.is_none()));
        }
        // Only remove UNTRACKED `spec_rel` copies from managed worktrees before `git worktree remove`.
        // Deleting a Git-tracked file inside a task worktree makes it dirty (`D <spec_rel>`) and
        // causes non-force `git worktree remove` to fail.
        let mut extra_spec_entries: Vec<crate::types::ArchiveCleanupEntry> = Vec::new();
        let mut candidate_wts: Vec<std::path::PathBuf> = cleanup_manifest
            .iter()
            .filter(|e| e.kind == "worktree")
            .map(|e| std::path::PathBuf::from(&e.path_or_ref))
            .collect();
        if closed_commit.is_none() && canon_target != canon_primary {
            candidate_wts.push(canon_target);
        }
        for wt_path in candidate_wts {
            let should_remove = checked_child(&wt_path, Path::new(spec_rel)).is_ok_and(|p| p.exists())
                && (closed_commit.is_none() || !is_git_tracked(&wt_path, spec_rel));
            if should_remove {
                let canon_ws = acdd_core::fs::canonicalize_macos_var(&wt_path)
                    .to_string_lossy()
                    .into_owned();
                if !cleanup_manifest
                    .iter()
                    .chain(extra_spec_entries.iter())
                    .any(|e| {
                        e.kind == "spec_file"
                            && e.path_or_ref == *spec_rel
                            && e.workspace.as_deref() == Some(canon_ws.as_str())
                    })
                {
                    extra_spec_entries.push(crate::types::ArchiveCleanupEntry {
                        kind: "spec_file".to_owned(),
                        path_or_ref: spec_rel.clone(),
                        workspace: Some(canon_ws),
                        force: false,
                    });
                }
            }
        }
        if !extra_spec_entries.is_empty() {
            extra_spec_entries.extend(cleanup_manifest);
            cleanup_manifest = extra_spec_entries;
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
        cleanup_pending: true,
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
