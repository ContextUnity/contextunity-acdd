//! Git worktree, snapshot, scoped commit, integration, and stateless crash-recovery operations.

pub mod commit;
pub mod integrate;
pub mod snapshot;
pub mod worktree;

#[cfg(test)]
mod tests;

pub use commit::{
    archive_spec_with_git, create_scoped_commit, current_branch_name, current_head_commit,
    is_protected_main_branch, write_git_note, ArchiveCommitReq, ArchiveCommitResp, GitNoteReq,
    ScopedCommitReq, ScopedCommitResp,
};
pub use integrate::{
    abort_in_progress_git_ops, integrate_task_branch, verify_tasks_integrated, IntegrateTaskReq,
    IntegrateTaskResp, TaskIntegrationCheckInput,
};
pub use snapshot::{
    blob_oid_at, capture_snapshot, current_scope_tree, diff_scope_changed_paths, restore_rollback,
    CaptureSnapshotReq, CaptureSnapshotResp, GitGcProtection, RollbackReq, RollbackResp,
    TemporaryGitIndex,
};
pub use worktree::{
    canonicalize_macos_var, cleanup_run_refs, create_run_worktree, create_task_worktree,
    remove_worktree_and_branch, resolve_repo_layout, CreateWorktreeReq, CreatedWorktree,
    RepoWorktreeLayout,
};

use acdd_core::errors::{ErrorCode, StateError};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) struct ScopedTempDir(pub(super) PathBuf);

impl ScopedTempDir {
    pub(super) fn create(prefix: &str) -> Result<Self, StateError> {
        let base = std::env::temp_dir();
        loop {
            let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("{prefix}-{}-{seq}", std::process::id()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self(candidate)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(StateError::new(
                        ErrorCode::ExternalGitDrift,
                        format!(
                            "failed creating temporary directory '{}': {err}",
                            candidate.display()
                        ),
                    ));
                }
            }
        }
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn run_git_raw(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    extra_env: &[(&str, &str)],
    stdin_bytes: Option<&[u8]>,
) -> Result<Output, StateError> {
    acdd_core::git::run_git_raw(worktree, args, index_file, extra_env, stdin_bytes)
        .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))
}

pub(super) fn run_git_stdout(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    error_code: ErrorCode,
) -> Result<String, StateError> {
    let output = run_git_raw(worktree, args, index_file, &[], None)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(StateError::new(
            error_code,
            format!("git {:?} failed in '{}': {stderr}", args, worktree.display()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(super) fn run_git_checked(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    stdin_bytes: Option<&[u8]>,
    error_code: ErrorCode,
) -> Result<Output, StateError> {
    let output = run_git_raw(worktree, args, index_file, &[], stdin_bytes)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(StateError::new(
            error_code,
            format!("git {:?} failed in '{}': {detail}", args, worktree.display()),
        ));
    }
    Ok(output)
}

pub(super) fn git_dir_path(worktree: &Path) -> Result<std::path::PathBuf, StateError> {
    let raw = run_git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    Ok(worktree::canonicalize_macos_var(Path::new(&raw)))
}

pub(super) fn commit_has_exact_trailer(
    worktree: &Path,
    commit_oid: &str,
    trailer_line: &str,
) -> Result<bool, StateError> {
    let msg = run_git_stdout(
        worktree,
        &["show", "-s", "--format=%B", commit_oid],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    Ok(msg.lines().any(|line| line.trim() == trailer_line))
}

pub(super) fn sync_default_index_paths(
    worktree: &Path,
    target_commit: &str,
    rel_paths: &[String],
) -> Result<(), StateError> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let mut pathspecs = Vec::new();
    for path in rel_paths {
        pathspecs.extend_from_slice(b":(literal)");
        pathspecs.extend_from_slice(path.as_bytes());
        pathspecs.push(0);
    }
    run_git_checked(
        worktree,
        &[
            "reset",
            "--quiet",
            target_commit,
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        None,
        Some(&pathspecs),
        ErrorCode::ExternalGitDrift,
    )?;
    Ok(())
}

pub(super) fn failpoint_hit(step: &str) {
    acdd_core::failpoint::hit(step);
    #[cfg(all(test, not(feature = "failpoints")))]
    {
        let Ok(spec) = std::env::var("ACDD_FAILPOINT") else {
            return;
        };
        for entry in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (target, action) = entry
                .split_once(':')
                .map_or((entry, "abort"), |(t, a)| (t.trim(), a.trim()));
            if target != step {
                continue;
            }
            match action {
                "panic" => panic!("failpoint hit: {step}"),
                "exit" => std::process::exit(137),
                _ => {
                    #[cfg(unix)]
                    unsafe {
                        // Terminate immediately without Drop/atexit/stdio flush or SIGABRT core dumps.
                        libc::_exit(134);
                    }
                    #[cfg(not(unix))]
                    std::process::exit(134);
                }
            }
        }
    }
}

pub(super) fn ensure_non_empty_git_identity<'a>(
    worktree: &Path,
) -> Vec<(&'a str, &'a str)> {
    let has_name = run_git_raw(worktree, &["config", "user.name"], None, &[], None)
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    let has_email = run_git_raw(worktree, &["config", "user.email"], None, &[], None)
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    let mut env = Vec::new();
    if !has_name {
        env.push(("GIT_AUTHOR_NAME", "acdd-agent"));
        env.push(("GIT_COMMITTER_NAME", "acdd-agent"));
    }
    if !has_email {
        env.push(("GIT_AUTHOR_EMAIL", "acdd@contextunity.local"));
        env.push(("GIT_COMMITTER_EMAIL", "acdd@contextunity.local"));
    }
    env
}
