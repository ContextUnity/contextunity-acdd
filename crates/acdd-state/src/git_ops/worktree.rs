//! Repository common-dir/worktree layout resolution, managed worktree lifecycle, and ref cleanup.

use super::{run_git_checked, run_git_raw, run_git_stdout};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{atomic_write, checked_child, ensure_confined_dir_all, open_confined};
use acdd_core::ids::{CommitOid, RunId};
use acdd_core::lock::{try_acquire_exclusive, FileLockGuard};
use acdd_core::path_policy::normalize_rel_path;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

pub use acdd_core::fs::canonicalize_macos_var;

/// Resolved repository and common-dir lock/backup paths across primary and linked worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoWorktreeLayout {
    /// Canonical primary worktree root (first entry in `git worktree list --porcelain`).
    pub primary_worktree: PathBuf,
    /// Shared `.git` common directory resolved via `--git-common-dir`.
    pub git_common_dir: PathBuf,
    /// Canonical path of the requested target worktree.
    pub target_worktree: PathBuf,
    /// Shared `<git_common_dir>/acdd` directory holding locks and backup SQLite.
    pub acdd_common_dir: PathBuf,
    /// `<git_common_dir>/acdd/repo.lock`.
    pub repo_lock_path: PathBuf,
    /// `<git_common_dir>/acdd/backup.lock`.
    pub backup_lock_path: PathBuf,
    /// `<git_common_dir>/acdd/traces.lock`.
    pub traces_lock_path: PathBuf,
    /// `<git_common_dir>/acdd/state.bak.sqlite`.
    pub state_bak_path: PathBuf,
}

/// Request parameters for creating a managed run or task worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorktreeReq {
    /// Primary workspace root.
    pub primary_worktree: PathBuf,
    /// Target filesystem path for the new worktree.
    pub worktree_path: PathBuf,
    /// New branch name to create at `base_ref`.
    pub branch_name: String,
    /// Base branch, ref, or commit OID.
    pub base_ref: String,
    /// Optional uncommitted relative paths (e.g. `spec_path`) to copy into the new worktree.
    pub copy_uncommitted_paths: Vec<String>,
}

/// Metadata describing a newly created managed worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    /// Canonical created worktree path.
    pub worktree: PathBuf,
    /// Branch checked out in the worktree.
    pub branch: String,
    /// Initial `HEAD` commit OID of the new worktree.
    pub head_commit: CommitOid,
}

/// Resolve primary worktree, `--git-common-dir`, and shared lock/backup paths for `worktree`.
pub fn resolve_repo_layout(worktree: &Path) -> Result<RepoWorktreeLayout, StateError> {
    if !worktree.exists() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("worktree path '{}' does not exist", worktree.display()),
        ));
    }

    let top_raw = run_git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let target_worktree = canonicalize_macos_var(Path::new(&top_raw));

    let common_raw = run_git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let git_common_dir = canonicalize_macos_var(Path::new(&common_raw));

    let porcelain = run_git_stdout(
        worktree,
        &["worktree", "list", "--porcelain"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let primary_worktree = porcelain
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(|raw| canonicalize_macos_var(Path::new(raw.trim())))
        .unwrap_or_else(|| {
            if git_common_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
                git_common_dir
                    .parent()
                    .map(canonicalize_macos_var)
                    .unwrap_or_else(|| target_worktree.clone())
            } else {
                target_worktree.clone()
            }
        });

    let acdd_common_dir = git_common_dir.join("acdd");
    Ok(RepoWorktreeLayout {
        primary_worktree,
        git_common_dir,
        target_worktree,
        repo_lock_path: acdd_common_dir.join("repo.lock"),
        backup_lock_path: acdd_common_dir.join("backup.lock"),
        traces_lock_path: acdd_common_dir.join("traces.lock"),
        state_bak_path: acdd_common_dir.join("state.bak.sqlite"),
        acdd_common_dir,
    })
}

/// Create a managed run-level worktree under `.worktrees/<run_name>`.
pub fn create_run_worktree(req: &CreateWorktreeReq) -> Result<CreatedWorktree, StateError> {
    create_managed_worktree(req, None)
}

/// Create a managed task-level worktree under `.worktrees/<run_name>-<task_ref>`.
pub fn create_task_worktree(req: &CreateWorktreeReq) -> Result<CreatedWorktree, StateError> {
    create_managed_worktree(req, None)
}

pub(crate) fn create_managed_worktree(
    req: &CreateWorktreeReq,
    copy_source: Option<&Path>,
) -> Result<CreatedWorktree, StateError> {
    let primary = canonicalize_macos_var(&req.primary_worktree);
    if !primary.exists() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("primary worktree '{}' does not exist", primary.display()),
        ));
    }

    if req.branch_name.trim().is_empty() || req.branch_name.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid worktree branch name '{}'", req.branch_name),
        ));
    }
    if req.base_ref.trim().is_empty() || req.base_ref.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid base ref '{}'", req.base_ref),
        ));
    }

    run_git_checked(
        &primary,
        &["check-ref-format", "--branch", &req.branch_name],
        None,
        None,
        ErrorCode::ProfileSchemaInvalid,
    )?;

    let base_check = format!("{}^{{commit}}", req.base_ref);
    run_git_stdout(&primary, &["rev-parse", "--verify", &base_check], None, ErrorCode::ExternalGitDrift)?;

    let raw_target = if req.worktree_path.is_absolute() {
        req.worktree_path.clone()
    } else {
        primary.join(&req.worktree_path)
    };
    let target_wt = canonicalize_macos_var(&raw_target);
    if target_wt.exists() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("worktree path '{}' already exists", target_wt.display()),
        ));
    }

    let parent = target_wt.parent().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("worktree path '{}' requires a parent directory", target_wt.display()),
        )
    })?;
    ensure_confined_dir_exists(parent)?;

    let target_str = target_wt.to_string_lossy().into_owned();
    run_git_checked(
        &primary,
        &["worktree", "add", "-b", &req.branch_name, &target_str, &req.base_ref],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;

    let copy_source = copy_source.unwrap_or(&primary);
    if let Err(copy_err) = copy_uncommitted_files(copy_source, &target_wt, &req.copy_uncommitted_paths) {
        let _ = remove_worktree_and_branch(&primary, Some(&target_wt), Some(&req.branch_name), true);
        return Err(copy_err);
    }

    let canonical_wt = canonicalize_macos_var(&target_wt);
    let head_commit = super::commit::current_head_commit(&canonical_wt)?;
    Ok(CreatedWorktree {
        worktree: canonical_wt,
        branch: req.branch_name.clone(),
        head_commit,
    })
}

fn ensure_confined_dir_exists(dir: &Path) -> Result<(), StateError> {
    ensure_confined_dir_all(dir, dir).map(|_| ()).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed preparing confined directory '{}': {err}", dir.display()),
        )
    })
}

fn copy_uncommitted_files(
    primary: &Path,
    target_wt: &Path,
    paths: &[String],
) -> Result<(), StateError> {
    for raw in paths {
        let Some(norm) = normalize_rel_path(raw) else {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid uncommitted relative path '{raw}'"),
            ));
        };
        let rel = Path::new(&norm);
        let src = checked_child(primary, rel).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("uncommitted path '{norm}' failed confinement check: {err}"),
            )
        })?;
        match fs::symlink_metadata(&src) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("refusing to copy symlink '{norm}' into managed worktree"),
                ));
            }
            Ok(meta) if meta.is_file() => {
                let mut open_opts = OpenOptions::new();
                open_opts.read(true);
                let mut file = open_confined(primary, rel, &mut open_opts).map_err(|err| {
                    StateError::new(
                        ErrorCode::ExternalGitDrift,
                        format!("failed reading '{norm}': {err}"),
                    )
                })?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|err| {
                    StateError::new(
                        ErrorCode::ExternalGitDrift,
                        format!("failed reading bytes from '{norm}': {err}"),
                    )
                })?;
                let dst = checked_child(target_wt, rel).map_err(|err| {
                    StateError::new(
                        ErrorCode::ExternalGitDrift,
                        format!("destination path '{norm}' failed confinement check: {err}"),
                    )
                })?;
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                atomic_write(&dst, &bytes, executable).map_err(|err| {
                    StateError::new(
                        ErrorCode::ExternalGitDrift,
                        format!("failed writing '{norm}' into managed worktree: {err}"),
                    )
                })?;
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("failed inspecting '{norm}': {err}"),
                ));
            }
        }
    }
    Ok(())
}

/// Return `true` when `branch` exists and is an ancestor of `target_ref` (e.g. `HEAD` or a `closed_commit`).
pub fn is_branch_merged_into_ref(
    primary_worktree: &Path,
    branch_ref: &str,
    target_ref: &str,
) -> Result<bool, StateError> {
    let primary = canonicalize_macos_var(primary_worktree);
    let full_ref = if branch_ref.starts_with("refs/") {
        branch_ref.to_owned()
    } else {
        format!("refs/heads/{branch_ref}")
    };
    let exists = run_git_raw(
        &primary,
        &["show-ref", "--verify", "--quiet", &full_ref],
        None,
        &[],
        None,
    )?;
    if !exists.status.success() {
        return Ok(false);
    }
    let out = run_git_raw(
        &primary,
        &["merge-base", "--is-ancestor", &full_ref, target_ref],
        None,
        &[],
        None,
    )?;
    Ok(out.status.success())
}

/// Remove a task/run worktree and delete its temporary branch.
pub fn remove_worktree_and_branch(
    primary_worktree: &Path,
    worktree: Option<&Path>,
    branch: Option<&str>,
    force: bool,
) -> Result<(), StateError> {
    let primary = canonicalize_macos_var(primary_worktree);

    if let Some(wt) = worktree {
        let can_wt = canonicalize_macos_var(wt);
        if can_wt == primary {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                "refusing to remove the primary repository worktree",
            ));
        }
        if wt.exists() {
            let _ = super::integrate::abort_in_progress_git_ops(wt);
            let wt_str = can_wt.to_string_lossy().into_owned();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&wt_str);
            run_git_checked(&primary, &args, None, None, ErrorCode::ExternalGitDrift)?;
        }
        let _ = run_git_raw(&primary, &["worktree", "prune"], None, &[], None);
    }

    if let Some(raw_branch) = branch {
        if super::commit::is_protected_main_branch(Some(raw_branch)) {
            return Err(StateError::new(
                ErrorCode::MainBranchProtected,
                format!("refusing to delete protected branch '{raw_branch}'"),
            ));
        }
        let short = raw_branch.strip_prefix("refs/heads/").unwrap_or(raw_branch);
        if short.trim().is_empty() || short.starts_with('-') {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid branch name '{raw_branch}'"),
            ));
        }
        let full_ref = format!("refs/heads/{short}");
        let exists = run_git_raw(
            &primary,
            &["show-ref", "--verify", "--quiet", &full_ref],
            None,
            &[],
            None,
        )?;
        if exists.status.success() {
            let flag = if force { "-D" } else { "-d" };
            run_git_checked(&primary, &["branch", flag, short], None, None, ErrorCode::ExternalGitDrift)?;
        }
    }

    Ok(())
}

/// Delete `refs/acdd/{runs,snapshots,keep}/<run_id>/*` across primary and linked workspaces.
pub fn cleanup_run_refs<'a>(
    locked: Option<&'a RepoWorktreeLayout>, workspaces: impl IntoIterator<Item = &'a PathBuf>,
    run_id: &RunId,
) -> Result<Vec<String>, StateError> {
    cleanup_refs(locked, workspaces, run_id, None)
}

pub(crate) fn try_lock_secondary_repo(
    locked: Option<&RepoWorktreeLayout>, target: &RepoWorktreeLayout,
) -> Result<Option<FileLockGuard>, StateError> {
    if locked.is_some_and(|primary| primary.git_common_dir == target.git_common_dir) {
        return Ok(None);
    }
    try_acquire_exclusive(&target.repo_lock_path)
        .map_err(|err| StateError::new(ErrorCode::BackendConflict, err.to_string()))?
        .map(Some).ok_or_else(|| StateError::new(ErrorCode::BackendConflict,
            format!("repository lock is busy: '{}'", target.repo_lock_path.display())))
}

pub(crate) fn cleanup_refs<'a>(
    locked: Option<&'a RepoWorktreeLayout>, workspaces: impl IntoIterator<Item = &'a PathBuf>,
    run_id: &RunId, snapshot: Option<String>,
) -> Result<Vec<String>, StateError> {
    let repos = lock_cleanup_repos(locked, workspaces)?;
    cleanup_locked_refs(&repos, run_id, snapshot)
}

pub(crate) fn lock_cleanup_repos<'a>(
    locked: Option<&'a RepoWorktreeLayout>, workspaces: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<Vec<(RepoWorktreeLayout, Option<FileLockGuard>)>, StateError> {
    let mut seen_common_dirs = BTreeSet::new();
    let mut repos = Vec::new();
    for ws in locked.map(|layout| &layout.primary_worktree).into_iter().chain(workspaces) {
        if !locked.is_some_and(|primary| primary.primary_worktree == *ws)
            && ws.is_dir()
            && ws.join(".git").symlink_metadata().is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
            && acdd_core::git::git_common_dir(ws).is_err()
        {
            continue;
        }
        let layout = resolve_repo_layout(ws)?;
        if seen_common_dirs.insert(layout.git_common_dir.clone()) {
            let guard = try_lock_secondary_repo(locked, &layout)?;
            repos.push((layout, guard));
        }
    }
    Ok(repos)
}

pub(crate) fn cleanup_locked_refs(
    repos: &[(RepoWorktreeLayout, Option<FileLockGuard>)], run_id: &RunId, snapshot: Option<String>,
) -> Result<Vec<String>, StateError> {
    let mut deleted = Vec::new();
    let prefixes = match snapshot {
        Some(prefix) => vec![prefix],
        None => ["runs", "snapshots", "keep"].map(|kind| format!("refs/acdd/{kind}/{run_id}/")).to_vec(),
    };
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(prefixes.iter().map(String::as_str));

    for (layout, _guard) in repos {
        let listed = run_git_stdout(
            &layout.primary_worktree,
            &args,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        if listed.is_empty() {
            continue;
        }
        let batch: String = listed.lines().map(|r| format!("delete {r}\n")).collect();
        run_git_checked(
            &layout.primary_worktree,
            &["update-ref", "--no-deref", "--stdin"],
            None,
            Some(batch.as_bytes()),
            ErrorCode::ExternalGitDrift,
        )?;
        deleted.extend(listed.lines().map(str::to_owned));
    }

    Ok(deleted)
}
