//! Git execution and repository/worktree root resolution helpers.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

/// Git environment variables cleared before invoking `git` in an explicit worktree.
///
/// Prevents inherited hook or worktree variables (such as `GIT_DIR` or `GIT_INDEX_FILE`
/// during `pre-commit` hooks) from redirecting subprocesses to the wrong repository.
pub const STRIPPED_GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// Removes inherited Git repository-override environment variables from `cmd`.
pub fn scrub_git_env(cmd: &mut Command) {
    for key in STRIPPED_GIT_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// Builds a `git` [`Command`] bound to `worktree` with inherited Git override variables cleared.
pub fn base_git_command(worktree: &Path) -> Result<Command> {
    if !worktree.exists() {
        bail!(
            "TASK_GIT_WORKTREE_MISSING: worktree '{}' does not exist on disk",
            worktree.display()
        );
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(worktree);
    scrub_git_env(&mut cmd);
    Ok(cmd)
}

/// Executes `git` in `worktree` with scrubbed Git env, optional `GIT_INDEX_FILE`, extra env vars, and optional `stdin` bytes.
pub fn run_git_raw(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    extra_env: &[(&str, &str)],
    stdin_bytes: Option<&[u8]>,
) -> Result<Output> {
    let mut cmd = base_git_command(worktree)?;
    cmd.args(args);
    if let Some(index) = index_file {
        cmd.env("GIT_INDEX_FILE", index);
    }
    for &(k, v) in extra_env {
        cmd.env(k, v);
    }
    if let Some(bytes) = stdin_bytes {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn git {:?} in '{}'",
                args,
                worktree.display()
            )
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(bytes).with_context(|| {
                format!("failed to write stdin to git {:?} in '{}'", args, worktree.display())
            })?;
        }
        child.wait_with_output().with_context(|| {
            format!(
                "failed waiting on git {:?} in '{}'",
                args,
                worktree.display()
            )
        })
    } else {
        cmd.output().with_context(|| {
            format!(
                "failed to execute git {:?} in '{}'",
                args,
                worktree.display()
            )
        })
    }
}

/// Executes `git` via [`run_git_raw`] and returns trimmed UTF-8-lossy stdout on exit code 0.
pub fn run_git_stdout(
    worktree: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    extra_env: &[(&str, &str)],
    stdin_bytes: Option<&[u8]>,
) -> Result<String> {
    let output = run_git_raw(worktree, args, index_file, extra_env, stdin_bytes)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        bail!(
            "git {:?} failed in '{}': {detail}",
            args,
            worktree.display()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs a git command in the specified working directory with Git override env vars scrubbed.
pub fn git_command(worktree: &Path, args: &[&str]) -> Result<Output> {
    run_git_raw(worktree, args, None, &[], None)
}

/// Reads non-empty UTF-8-lossy standard output from a successful Git command.
pub fn git_stdout(worktree: &Path, args: &[&str], error_code: &str) -> Result<String> {
    let output = git_command(worktree, args)?;
    if !output.status.success() {
        bail!("{error_code}: {}", String::from_utf8_lossy(&output.stderr));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        bail!("{error_code}: empty Git response");
    }
    Ok(value)
}

/// Returns whether a Git command completed successfully.
pub fn git_status(worktree: &Path, args: &[&str]) -> Result<ExitStatus> {
    Ok(git_command(worktree, args)?.status)
}

/// Resolves the repository common `.git` directory via `git rev-parse --path-format=absolute --git-common-dir`.
pub fn git_common_dir(worktree: &Path) -> Result<PathBuf> {
    let resolved = git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "TASK_GIT_DIR_INVALID",
    )?;
    let path = PathBuf::from(&resolved);
    Ok(path.canonicalize().unwrap_or(path))
}

/// Resolves the primary worktree root for a repository or linked worktree.
pub fn primary_worktree_root(root: &Path) -> Option<PathBuf> {
    if root.join(".git").is_dir() {
        return root.canonicalize().ok();
    }
    if !root.join(".git").exists() {
        return None;
    }
    if let Ok(output) = git_command(root, &["worktree", "list", "--porcelain"]) {
        if output.status.success() {
            if let Ok(text) = std::str::from_utf8(&output.stdout) {
                if let Some(first) = text.lines().find_map(|line| line.strip_prefix("worktree ")) {
                    let path = PathBuf::from(first.trim());
                    return Some(path.canonicalize().unwrap_or(path));
                }
            }
        }
    }
    let common_dir = git_common_dir(root).ok()?;
    if common_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
        common_dir.parent().map(Path::to_path_buf)
    } else {
        Some(common_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_common_dir_and_primary_worktree_resolve_in_repo_and_linked_worktree() {
        let base = std::env::temp_dir().join(format!("acdd_core_git_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(base.clone());

        let repo = base.join("primary");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git_status(&repo, &["init"]).unwrap().success());
        assert!(git_status(&repo, &["config", "user.email", "test@example.com"])
            .unwrap()
            .success());
        assert!(git_status(&repo, &["config", "user.name", "Test"])
            .unwrap()
            .success());
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        assert!(git_status(&repo, &["add", "README.md"]).unwrap().success());
        assert!(git_status(&repo, &["commit", "-m", "init"])
            .unwrap()
            .success());

        let wt = base.join("linked");
        assert!(git_status(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "wt-branch",
                wt.to_str().unwrap(),
            ],
        )
        .unwrap()
        .success());

        let can_repo = repo.canonicalize().unwrap();
        assert_eq!(git_common_dir(&repo).unwrap(), can_repo.join(".git"));
        assert_eq!(git_common_dir(&wt).unwrap(), can_repo.join(".git"));
        assert_eq!(primary_worktree_root(&repo), Some(can_repo.clone()));
        assert_eq!(primary_worktree_root(&wt), Some(can_repo));
        assert_eq!(primary_worktree_root(&base), None);

        let hashed = run_git_stdout(&repo, &["hash-object", "--stdin"], None, &[], Some(b"hi\n")).unwrap();
        assert_eq!(hashed.len(), 40);
        let mut cmd = Command::new("git");
        cmd.env("GIT_DIR", "/nonexistent");
        scrub_git_env(&mut cmd);
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "GIT_DIR" && v.is_none()));
    }
}
