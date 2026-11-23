//! Leaf Git subprocess execution, trailer formatting, and failpoint helpers for `git_ops`.

use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{canonicalize_macos_var, ScopedTempDir};
use acdd_core::path_policy::normalize_rel_path;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Output;

pub(super) fn create_scoped_temp_dir(prefix: &str) -> Result<ScopedTempDir, StateError> {
    ScopedTempDir::create(prefix).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed creating temporary directory '{prefix}': {err}"),
        )
    })
}

pub(super) fn run_git_raw(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    extra_env: &[(&str, &str)],
    stdin_bytes: Option<&[u8]>,
) -> Result<Output, StateError> {
    let output = if index_file.is_none() && extra_env.is_empty() && stdin_bytes.is_none() {
        acdd_core::git::git_command(worktree, args)
    } else {
        acdd_core::git::run_git_raw(worktree, args, index_file, extra_env, stdin_bytes)
    };
    output.map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))
}

pub(super) fn run_git_stdout(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    error_code: ErrorCode,
) -> Result<String, StateError> {
    let output = run_git_checked(worktree, args, index_file, None, error_code)?;
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
        return Err(StateError::new(
            error_code,
            acdd_core::git::format_git_failure(args, worktree, &output),
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
    Ok(canonicalize_macos_var(Path::new(&raw)))
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

pub(super) fn format_message_with_trailer(message: &str, trailer: &str) -> String {
    let trimmed = message.trim_end();
    if trimmed.lines().any(|line| line.trim() == trailer) {
        return format!("{trimmed}\n");
    }
    if trimmed.is_empty() {
        format!("{trailer}\n")
    } else {
        format!("{trimmed}\n\n{trailer}\n")
    }
}

pub(super) fn failpoint_hit(step: &str) {
    acdd_core::failpoint::hit(step);
    #[cfg(all(test, not(feature = "failpoints")))]
    acdd_core::failpoint::hit_from_env(step);
}

pub(super) fn ensure_non_empty_git_identity<'a>(worktree: &Path) -> Vec<(&'a str, &'a str)> {
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

pub(super) fn collect_included_z_paths(
    raw_z: &[u8],
    path_policy: &acdd_core::path_policy::WorkspacePathPolicy,
    changed: &mut BTreeSet<String>,
) {
    for raw in raw_z.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let s = String::from_utf8_lossy(raw);
        if let Some(norm) = normalize_rel_path(&s) {
            if path_policy.is_candidate_included(&norm) {
                changed.insert(norm);
            }
        }
    }
}
