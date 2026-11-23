//! Task branch integration (`rebase-ff` landing) and completeness checks.

use super::commit::{current_branch_name, current_head_commit, is_protected_main_branch};
use super::snapshot::tree_entry_at;
use super::exec::{
    commit_has_exact_trailer, create_scoped_temp_dir, ensure_non_empty_git_identity, failpoint_hit,
    format_message_with_trailer, git_dir_path, run_git_checked, run_git_raw, run_git_stdout,
};
use acdd_core::fs::ScopedTempDir;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{CommitOid, RunId, TaskRef, TreeOid};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parameters for integrating a completed task branch into the target run worktree.
#[derive(Debug, Clone)]
pub struct IntegrateTaskReq {
    /// Target run worktree where the integrated result lands.
    pub target_worktree: PathBuf,
    /// Optional task worktree path (must be clean of dangling merge/rebase markers).
    pub source_worktree: Option<PathBuf>,
    /// Source task branch or commit ref to integrate.
    pub source_branch: String,
    /// Owning run identifier for the `Acdd-Integrated` trailer.
    pub run_id: RunId,
    /// Completed task reference for the `Acdd-Integrated` trailer.
    pub task_ref: TaskRef,
    /// Accepted candidate tree OID for the `Acdd-Integrated` trailer.
    pub accepted_tree: TreeOid,
    /// Explicitly permit integrating onto `main` / `master`.
    pub allow_main: bool,
}

/// Result of integrating a task branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrateTaskResp {
    /// Created/landed commit OID.
    pub integrated_commit: Option<CommitOid>,
    /// Resulting `HEAD` commit OID in `target_worktree`.
    pub target_head: CommitOid,
}

/// Metadata for checking whether a completed task's scoped delta is present in `run_worktree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskIntegrationCheckInput {
    /// Completed task reference.
    pub task_ref: TaskRef,
    /// Initial `claim_tree` captured when the task was first claimed.
    pub initial_claim_tree: TreeOid,
    /// Final `accepted_tree` captured at task delivery.
    pub accepted_tree: TreeOid,
    /// Effective admitted scope (`scope ∪ extended_scope`) of the task.
    pub admitted_scope: Vec<String>,
    /// Transitive downstream completed tasks that may legitimately overwrite paths in `admitted_scope`.
    pub downstream_tasks: Vec<TaskRef>,
}

/// Abort any dangling `.git/MERGE_HEAD`, `.git/rebase-merge`, or `.git/rebase-apply` state in `worktree`.
pub fn abort_in_progress_git_ops(worktree: &Path) -> Result<bool, StateError> {
    let git_dir = git_dir_path(worktree)?;
    let mut aborted = false;

    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        run_git_checked(
            worktree,
            &["rebase", "--abort"],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        aborted = true;
    }

    if git_dir.join("MERGE_HEAD").exists() {
        run_git_checked(
            worktree,
            &["merge", "--abort"],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        aborted = true;
    }

    if git_dir.join("SQUASH_MSG").exists() {
        let unmerged = run_git_checked(
            worktree,
            &["ls-files", "-u"],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        if !unmerged.stdout.iter().all(u8::is_ascii_whitespace) {
            run_git_checked(
                worktree,
                &["reset", "--merge"],
                None,
                None,
                ErrorCode::ExternalGitDrift,
            )?;
            let _ = fs::remove_file(git_dir.join("SQUASH_MSG"));
            aborted = true;
        }
    }

    Ok(aborted)
}

/// Integrate `source_branch` into `target_worktree` via `rebase-ff`, aborting cleanly on conflict.
pub fn integrate_task_branch(req: &IntegrateTaskReq) -> Result<IntegrateTaskResp, StateError> {
    abort_in_progress_git_ops(&req.target_worktree)?;
    if let Some(src_wt) = &req.source_worktree {
        abort_in_progress_git_ops(src_wt)?;
    }

    if req.source_branch.trim().is_empty() || req.source_branch.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid source branch '{}'", req.source_branch),
        ));
    }
    let src_spec = format!("{}^{{commit}}", req.source_branch);
    run_git_stdout(
        &req.target_worktree,
        &["rev-parse", "--verify", &src_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;

    let target_branch = current_branch_name(&req.target_worktree)?;
    if !req.allow_main && is_protected_main_branch(target_branch.as_deref()) {
        return Err(StateError::new(
            ErrorCode::MainBranchProtected,
            format!(
                "refusing to integrate onto protected branch '{}'; pass --allow-main to override",
                target_branch.as_deref().unwrap_or("main")
            ),
        ));
    }

    let trailer = format!(
        "Acdd-Integrated: {}/{}/{}",
        req.run_id.as_str(),
        req.task_ref.as_str(),
        req.accepted_tree.as_str()
    );
    let target_head_before = current_head_commit(&req.target_worktree)?;
    let identity_env = ensure_non_empty_git_identity(&req.target_worktree);

    let mut temp_wt_guard: Option<TempRebaseWorktree> = None;
    let existing_wt = match &req.source_worktree {
        Some(wt) => Some(wt.clone()),
        None => find_branch_worktree(&req.target_worktree, &req.source_branch),
    };
    let rebase_wt = match existing_wt.as_deref() {
        Some(wt) => {
            abort_in_progress_git_ops(wt)?;
            wt
        }
        None => {
            let wt = TempRebaseWorktree::create(&req.target_worktree, &req.source_branch)?;
            temp_wt_guard = Some(wt);
            temp_wt_guard
                .as_ref()
                .map(|g| g.worktree_dir.as_path())
                .unwrap()
        }
    };

    let rebase_out = run_git_raw(
        rebase_wt,
        &["rebase", target_head_before.as_str()],
        None,
        &identity_env,
        None,
    )?;
    if !rebase_out.status.success() {
        let conflicts = collect_conflicting_paths(rebase_wt);
        failpoint_hit("git_integrate_before_abort");
        let _ = run_git_raw(rebase_wt, &["rebase", "--abort"], None, &[], None);
        drop(temp_wt_guard);
        return Err(integration_conflict_error(
            "rebase-ff",
            &conflicts,
            &req.source_branch,
            target_branch.as_deref(),
        ));
    }

    let rebased_tip = current_head_commit(rebase_wt)?;
    if rebased_tip != target_head_before {
        let existing_msg = run_git_stdout(
            rebase_wt,
            &["show", "-s", "--format=%B", rebased_tip.as_str()],
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        let amended_msg = format_message_with_trailer(&existing_msg, &trailer);
        let amend_out = run_git_raw(
            rebase_wt,
            &["commit", "--amend", "--no-verify", "-F", "-"],
            None,
            &identity_env,
            Some(amended_msg.as_bytes()),
        )?;
        if !amend_out.status.success() {
            drop(temp_wt_guard);
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "failed appending Acdd-Integrated trailer on rebased tip: {}",
                    String::from_utf8_lossy(&amend_out.stderr).trim()
                ),
            ));
        }
        let final_tip = current_head_commit(rebase_wt)?;
        let ff_out = run_git_raw(
            &req.target_worktree,
            &["merge", "--ff-only", final_tip.as_str()],
            None,
            &identity_env,
            None,
        )?;
        if !ff_out.status.success() {
            let conflicts = collect_conflicting_paths(&req.target_worktree);
            let _ = abort_in_progress_git_ops(&req.target_worktree);
            drop(temp_wt_guard);
            return Err(integration_conflict_error(
                "rebase-ff",
                &conflicts,
                &req.source_branch,
                target_branch.as_deref(),
            ));
        }
    }
    drop(temp_wt_guard);
    let target_head = current_head_commit(&req.target_worktree)?;
    Ok(IntegrateTaskResp {
        integrated_commit: Some(target_head.clone()),
        target_head,
    })
}

/// Verify every completed task's scoped delta is present in `run_worktree`, returning unintegrated `TaskRef`s.
pub fn verify_tasks_integrated(
    run_id: &RunId,
    run_worktree: &Path,
    tasks: &[TaskIntegrationCheckInput],
) -> Result<Vec<TaskRef>, StateError> {
    let _head = current_head_commit(run_worktree)?;
    let policy = WorkspacePathPolicy::default();
    let tasks_by_ref: BTreeMap<&TaskRef, &TaskIntegrationCheckInput> =
        tasks.iter().map(|t| (&t.task_ref, t)).collect();
    let mut missing = Vec::new();

    for task in tasks {
        let diff_out = run_git_checked(
            run_worktree,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-r",
                "-z",
                task.initial_claim_tree.as_str(),
                task.accepted_tree.as_str(),
            ],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;

        let mut changed_paths = Vec::new();
        for raw in diff_out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let s = String::from_utf8_lossy(raw);
            if let Some(norm) = normalize_rel_path(&s) {
                if task.admitted_scope.is_empty()
                    || policy.is_scope_admitted(&norm, &task.admitted_scope)
                {
                    changed_paths.push(norm);
                }
            }
        }
        if changed_paths.is_empty() {
            continue;
        }

        let trailer = format!(
            "Acdd-Integrated: {}/{}/{}",
            run_id.as_str(),
            task.task_ref.as_str(),
            task.accepted_tree.as_str()
        );
        let integrated_commit = find_integrated_commit(run_worktree, &trailer)?;

        let mut task_ok = true;
        for path in &changed_paths {
            let head_entry = tree_entry_at(run_worktree, "HEAD", path)?;
            let accepted_entry = tree_entry_at(run_worktree, task.accepted_tree.as_str(), path)?;

            // Rule (b): exact `(mode, blob_oid)` tree entry match (or both absent) in run_worktree HEAD.
            if head_entry == accepted_entry {
                continue;
            }

            // Rule (c): transitive downstream completed task owning `path` modified its `(mode, blob_oid)`
            // and its accepted entry matches run_worktree HEAD.
            let mut covered_by_downstream = false;
            for downstream_ref in &task.downstream_tasks {
                let Some(downstream) = tasks_by_ref.get(downstream_ref) else {
                    continue;
                };
                if downstream.admitted_scope.is_empty()
                    || !policy.is_scope_admitted(path, &downstream.admitted_scope)
                {
                    continue;
                }
                let downstream_initial = tree_entry_at(
                    run_worktree,
                    downstream.initial_claim_tree.as_str(),
                    path,
                )?;
                let downstream_accepted =
                    tree_entry_at(run_worktree, downstream.accepted_tree.as_str(), path)?;
                if downstream_initial != downstream_accepted && downstream_accepted == head_entry {
                    covered_by_downstream = true;
                    break;
                }
            }
            if covered_by_downstream {
                continue;
            }

            // Rule (a): Acdd-Integrated trailer in HEAD history without later revert of `path` to initial_claim_tree.
            if let Some(ref int_oid) = integrated_commit {
                let initial_entry =
                    tree_entry_at(run_worktree, task.initial_claim_tree.as_str(), path)?;
                let range = format!("{int_oid}..HEAD");
                let later_commits = run_git_stdout(
                    run_worktree,
                    &["rev-list", &range, "--", path],
                    None,
                    ErrorCode::ExternalGitDrift,
                )?;
                let reverted_to_initial = !later_commits.trim().is_empty()
                    && head_entry == initial_entry
                    && initial_entry != accepted_entry;
                if !reverted_to_initial {
                    continue;
                }
            }

            task_ok = false;
            break;
        }

        if !task_ok {
            missing.push(task.task_ref.clone());
        }
    }

    Ok(missing)
}

fn find_integrated_commit(
    worktree: &Path,
    trailer: &str,
) -> Result<Option<CommitOid>, StateError> {
    let out = run_git_checked(
        worktree,
        &["log", "--fixed-strings", "--grep", trailer, "--format=%H", "HEAD"],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for oid in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if commit_has_exact_trailer(worktree, oid, trailer)? {
            return Ok(Some(CommitOid::new(oid)?));
        }
    }
    Ok(None)
}

fn collect_conflicting_paths(worktree: &Path) -> Vec<String> {
    let Ok(out) = run_git_raw(
        worktree,
        &["diff", "--name-only", "--diff-filter=U", "-z"],
        None,
        &[],
        None,
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    out.stdout
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .filter_map(|r| normalize_rel_path(&String::from_utf8_lossy(r)))
        .collect()
}

fn integration_conflict_error(
    strategy: &str,
    conflicting_paths: &[String],
    source_branch: &str,
    target_branch: Option<&str>,
) -> StateError {
    let target = target_branch.unwrap_or("HEAD");
    let paths_display = if conflicting_paths.is_empty() {
        "none (non-fast-forward or structural conflict)".to_owned()
    } else {
        conflicting_paths.join(", ")
    };
    StateError::new(
        ErrorCode::GitIntegrationConflict,
        format!(
            "GitIntegrationConflict {{ strategy: \"{strategy}\", source_branch: \"{source_branch}\", target_branch: \"{target}\", conflicting_paths: [{paths_display}] }}"
        ),
    )
    .with_hint("Resolve conflicts manually or rebase the task branch before re-running integration.")
}

fn find_branch_worktree(repo_worktree: &Path, branch: &str) -> Option<PathBuf> {
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let expected_ref = format!("refs/heads/{short}");
    let out = run_git_stdout(
        repo_worktree,
        &["worktree", "list", "--porcelain"],
        None,
        ErrorCode::ExternalGitDrift,
    )
    .ok()?;
    let mut current_wt: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_wt = Some(PathBuf::from(path.trim()));
        } else if let Some(br) = line.strip_prefix("branch ") {
            if br.trim() == expected_ref {
                return current_wt;
            }
        } else if line.trim().is_empty() {
            current_wt = None;
        }
    }
    None
}

struct TempRebaseWorktree {
    primary_worktree: PathBuf,
    worktree_dir: PathBuf,
    _temp_dir: ScopedTempDir,
}

impl TempRebaseWorktree {
    fn create(primary_worktree: &Path, branch: &str) -> Result<Self, StateError> {
        let temp_dir = create_scoped_temp_dir("acdd-rebase")?;
        let worktree_dir = temp_dir.path().join("wt");
        let wt_str = worktree_dir.to_string_lossy().into_owned();
        run_git_checked(
            primary_worktree,
            &["worktree", "add", &wt_str, branch],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        Ok(Self {
            primary_worktree: primary_worktree.to_path_buf(),
            worktree_dir,
            _temp_dir: temp_dir,
        })
    }
}

impl Drop for TempRebaseWorktree {
    fn drop(&mut self) {
        let _ = abort_in_progress_git_ops(&self.worktree_dir);
        let wt_str = self.worktree_dir.to_string_lossy().into_owned();
        let _ = run_git_raw(
            &self.primary_worktree,
            &["worktree", "remove", "--force", &wt_str],
            None,
            &[],
            None,
        );
        let _ = run_git_raw(&self.primary_worktree, &["worktree", "prune"], None, &[], None);
    }
}
