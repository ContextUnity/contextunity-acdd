//! Git execution and repository/worktree root resolution helpers.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
#[cfg(test)]
pub fn git_status(worktree: &Path, args: &[&str]) -> Result<std::process::ExitStatus> {
    Ok(git_command(worktree, args)?.status)
}

fn bytes_to_path_exact(raw: &[u8]) -> Result<PathBuf> {
    if raw.is_empty() {
        bail!("TASK_GIT_DIR_INVALID: empty Git path response");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(raw)))
    }
    #[cfg(not(unix))]
    {
        let s = std::str::from_utf8(raw)
            .context("TASK_GIT_DIR_INVALID: non-UTF-8 Git path response")?;
        Ok(PathBuf::from(s))
    }
}

fn git_rev_parse_path_exact(worktree: &Path, flag: &str) -> Result<PathBuf> {
    let output = git_command(worktree, &["rev-parse", "--path-format=absolute", flag])?;
    if !output.status.success() {
        bail!(
            "TASK_GIT_DIR_INVALID: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(windows)]
    let bytes = output
        .stdout
        .strip_suffix(b"\r\n")
        .or_else(|| output.stdout.strip_suffix(b"\n"))
        .unwrap_or(&output.stdout);
    #[cfg(not(windows))]
    let bytes = output
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(&output.stdout);
    let path = bytes_to_path_exact(bytes)?;
    Ok(path.canonicalize().unwrap_or(path))
}

/// Resolves the repository common `.git` directory via `git rev-parse --path-format=absolute --git-common-dir`
/// without trimming whitespace from path segments.
pub fn git_common_dir(worktree: &Path) -> Result<PathBuf> {
    git_rev_parse_path_exact(worktree, "--git-common-dir")
}

/// Resolves the exact repository top-level directory via `git rev-parse --path-format=absolute --show-toplevel`
/// without trimming whitespace from path segments.
pub fn git_toplevel_exact(worktree: &Path) -> Result<PathBuf> {
    git_rev_parse_path_exact(worktree, "--show-toplevel")
}

/// Lists all existing canonical worktree roots owned by the repository at `root`,
/// preserving exact trailing whitespace and rejecting stale or foreign directories.
pub fn list_owned_worktrees(root: &Path) -> Result<Vec<PathBuf>> {
    let expected_common = git_common_dir(root)?;
    let output = git_command(root, &["worktree", "list", "--porcelain", "-z"])?;
    if !output.status.success() {
        bail!(
            "ACDD_GIT_ERROR: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for rec in output.stdout.split(|b| *b == 0) {
        let Some(raw_bytes) = rec.strip_prefix(b"worktree ") else {
            continue;
        };
        let Ok(p) = bytes_to_path_exact(raw_bytes) else {
            continue;
        };
        if !p.is_dir() {
            continue;
        }
        let Ok(canon_p) = p.canonicalize() else {
            continue;
        };
        if git_toplevel_exact(&canon_p).ok().as_ref() != Some(&canon_p) {
            continue;
        }
        if git_common_dir(&canon_p).ok().as_ref() != Some(&expected_common) {
            continue;
        }
        if seen.insert(canon_p.clone()) {
            out.push(canon_p);
        }
    }
    Ok(out)
}

/// Resolves the primary worktree root for a repository or linked worktree.
pub fn primary_worktree_root(root: &Path) -> Option<PathBuf> {
    if root.join(".git").is_dir() {
        return root.canonicalize().ok();
    }
    if !root.join(".git").exists() {
        return None;
    }
    if let Ok(common_dir) = git_common_dir(root) {
        if common_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
            if let Some(parent) = common_dir.parent().filter(|p| p.is_dir()) {
                return Some(parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf()));
            }
        }
        return list_owned_worktrees(root)
            .ok()
            .and_then(|v| v.into_iter().next())
            .or(Some(common_dir));
    }
    None
}

fn validate_git_rev(rev: &str) -> Result<&str> {
    let trimmed = rev.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        bail!("invalid git revision '{rev}'");
    }
    Ok(trimmed)
}

/// Reads a Git blob at `{rev}:{rel_path}` as UTF-8 text.
///
/// Returns `Ok(None)` when the path is absent in `rev` or resolves to a non-blob object
/// (such as a tree, commit, or submodule). Returns `Err` when `rev` or `rel_path` is invalid,
/// `git` cannot run in `worktree`, or the blob bytes are not valid UTF-8.
pub fn read_git_blob_utf8(worktree: &Path, rev: &str, rel_path: &str) -> Result<Option<String>> {
    let rev = validate_git_rev(rev)?;
    let norm_rel = crate::path_policy::normalize_rel_path(rel_path)
        .with_context(|| format!("invalid relative git path '{rel_path}'"))?;
    let spec = format!("{rev}:./{norm_rel}");
    read_git_blob_utf8_spec(worktree, &spec)
}

/// Reads a Git blob by exact object ID (`oid`) as UTF-8 text without path-normalization aliasing.
pub fn read_git_blob_utf8_by_oid(worktree: &Path, oid: &str) -> Result<Option<String>> {
    if oid.is_empty() || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid git blob oid '{oid}'");
    }
    read_git_blob_utf8_spec(worktree, oid)
}

fn read_git_blob_utf8_spec(worktree: &Path, spec: &str) -> Result<Option<String>> {
    let kind_out = run_git_raw(worktree, &["cat-file", "-t", spec], None, &[], None)?;
    if !kind_out.status.success() {
        return Ok(None);
    }
    if String::from_utf8_lossy(&kind_out.stdout).trim() != "blob" {
        return Ok(None);
    }
    let show_out = run_git_raw(worktree, &["cat-file", "blob", spec], None, &[], None)?;
    if !show_out.status.success() {
        let stderr = String::from_utf8_lossy(&show_out.stderr).trim().to_owned();
        bail!("git cat-file blob '{spec}' failed in '{}': {stderr}", worktree.display());
    }
    let text = String::from_utf8(show_out.stdout)
        .with_context(|| format!("git blob '{spec}' is not valid UTF-8"))?;
    Ok(Some(text))
}

/// Computes a unified diff (`-U{context_lines}`) for a confined relative path against `baseline_rev`.
///
/// Supports tracked changes, untracked live files (absent or present in `baseline_rev`),
/// deleted files, and executable mode changes (`100644`/`100755`) without `/tmp` collisions.
pub fn diff_worktree_file_against_rev(
    worktree: &Path,
    baseline_rev: &str,
    rel_path: &str,
    context_lines: usize,
) -> Result<String> {
    let rev = validate_git_rev(baseline_rev)?;
    let norm_rel = crate::path_policy::normalize_rel_path_exact(rel_path)
        .with_context(|| format!("invalid relative delta path '{rel_path}'"))?;
    let live_path = crate::fs::checked_child(worktree, Path::new(&norm_rel))?;

    let tree_spec = format!("{rev}^{{tree}}");
    let tree_oid = run_git_stdout(worktree, &["rev-parse", "--verify", &tree_spec], None, &[], None)
        .context("delta baseline must resolve to a valid git commit or tree")?;
    let unified_arg = format!("--unified={context_lines}");

    let live_meta = match std::fs::symlink_metadata(&live_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                bail!("delta path '{}' must name a regular file", live_path.display());
            }
            Some(meta)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    let is_untracked = if live_meta.is_some() {
        let untracked = run_git_raw(
            worktree,
            &[
                "--literal-pathspecs",
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                &norm_rel,
            ],
            None,
            &[],
            None,
        )?;
        if !untracked.status.success() {
            bail!(
                "git ls-files failed in '{}': {}",
                worktree.display(),
                String::from_utf8_lossy(&untracked.stderr).trim()
            );
        }
        !untracked.stdout.is_empty()
    } else {
        false
    };

    if !is_untracked {
        let out = run_git_raw(
            worktree,
            &[
                "--no-pager",
                "--literal-pathspecs",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                &unified_arg,
                &tree_oid,
                "--",
                &norm_rel,
            ],
            None,
            &[],
            None,
        )?;
        if !out.status.success() {
            bail!(
                "git delta failed in '{}': {}",
                worktree.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        return String::from_utf8(out.stdout)
            .with_context(|| format!("git delta output for '{norm_rel}' is not valid UTF-8"));
    }

    let live_meta = live_meta.expect("untracked file must exist on disk");
    let abs_str = live_path
        .to_str()
        .context("delta file path is not valid UTF-8")?;
    let baseline_entry = baseline_blob_entry(worktree, &tree_oid, &norm_rel)?;
    let Some((baseline_mode, blob_oid)) = baseline_entry else {
        let out = run_no_index_diff(worktree, &unified_arg, None, abs_str)?;
        return String::from_utf8(out)
            .with_context(|| format!("git untracked delta output for '{norm_rel}' is not valid UTF-8"));
    };

    let baseline_blob = run_git_raw(worktree, &["cat-file", "blob", &blob_oid], None, &[], None)?;
    if !baseline_blob.status.success() {
        bail!(
            "git cat-file baseline blob failed: {}",
            String::from_utf8_lossy(&baseline_blob.stderr).trim()
        );
    }
    let current_bytes = std::fs::read(&live_path)?;
    #[cfg(unix)]
    let live_mode = {
        use std::os::unix::fs::PermissionsExt;
        if live_meta.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        }
    };
    #[cfg(not(unix))]
    let live_mode = {
        let _ = live_meta;
        baseline_mode.as_str()
    };
    let mode_changed =
        baseline_mode != live_mode && matches!(baseline_mode.as_str(), "100644" | "100755");
    if current_bytes == baseline_blob.stdout {
        return Ok(if mode_changed {
            format!("diff --git a/{norm_rel} b/{norm_rel}\nold mode {baseline_mode}\nnew mode {live_mode}\n")
        } else {
            String::new()
        });
    }

    let tmp_dir = crate::fs::ScopedTempDir::create("contextunity-delta")?;
    let tmp_path = tmp_dir.write_private_file("baseline", &baseline_blob.stdout)?;
    let tmp_str = tmp_path.to_str().context("temp delta path is not UTF-8")?;
    let raw = run_no_index_diff(worktree, &unified_arg, Some(tmp_str), abs_str)?;
    let text = String::from_utf8(raw)
        .with_context(|| format!("git baseline untracked delta output for '{norm_rel}' is not UTF-8"))?;
    Ok(normalize_no_index_mode_headers(
        &text,
        &baseline_mode,
        live_mode,
        mode_changed,
    ))
}

fn baseline_blob_entry(worktree: &Path, tree_oid: &str, norm_rel: &str) -> Result<Option<(String, String)>> {
    let out = run_git_raw(
        worktree,
        &["--literal-pathspecs", "ls-tree", "-z", tree_oid, "--", norm_rel],
        None,
        &[],
        None,
    )?;
    if !out.status.success() {
        return Ok(None);
    }
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let mut parts = header.split_whitespace();
        if let (Some(mode), Some("blob"), Some(oid)) = (parts.next(), parts.next(), parts.next()) {
            return Ok(Some((mode.to_owned(), oid.to_owned())));
        }
    }
    Ok(None)
}

fn run_no_index_diff(
    worktree: &Path,
    unified_arg: &str,
    baseline_tmp: Option<&str>,
    live_abs: &str,
) -> Result<Vec<u8>> {
    #[cfg(not(unix))]
    let _empty_guard = if baseline_tmp.is_none() {
        let dir = crate::fs::ScopedTempDir::create("contextunity-delta")?;
        let empty = dir.write_private_file("empty", b"")?;
        Some((dir, empty))
    } else {
        None
    };
    let lhs = match baseline_tmp {
        Some(path) => path,
        None => {
            #[cfg(unix)]
            {
                "/dev/null"
            }
            #[cfg(not(unix))]
            {
                _empty_guard
                    .as_ref()
                    .and_then(|(_, p)| p.to_str())
                    .context("empty delta path is not UTF-8")?
            }
        }
    };
    let out = run_git_raw(
        worktree,
        &[
            "--no-pager",
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            unified_arg,
            "--",
            lhs,
            live_abs,
        ],
        None,
        &[],
        None,
    )?;
    if !matches!(out.status.code(), Some(0 | 1)) {
        bail!(
            "git no-index delta failed in '{}': {}",
            worktree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn normalize_no_index_mode_headers(
    text: &str,
    baseline_mode: &str,
    live_mode: &str,
    mode_changed: bool,
) -> String {
    let mut out = String::with_capacity(text.len() + 48);
    let mut in_header = true;
    let mut inserted = false;
    for line in text.split_inclusive('\n') {
        if in_header {
            if line.starts_with("--- ")
                || line.starts_with("@@ ")
                || line.starts_with("Binary files ")
                || line.starts_with("GIT binary patch")
            {
                if mode_changed && !inserted {
                    out.push_str(&format!("old mode {baseline_mode}\nnew mode {live_mode}\n"));
                    inserted = true;
                }
                in_header = false;
            } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
                continue;
            } else if mode_changed && !inserted && line.starts_with("index ") {
                out.push_str(&format!("old mode {baseline_mode}\nnew mode {live_mode}\n"));
                inserted = true;
            }
        }
        out.push_str(line);
    }
    if mode_changed && !inserted {
        if let Some(nl) = out.find('\n') {
            out.insert_str(
                nl + 1,
                &format!("old mode {baseline_mode}\nnew mode {live_mode}\n"),
            );
        }
    }
    out
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

        // Verify read_git_blob_utf8 across present, absent, non-blob, and invalid UTF-8 inputs.
        assert_eq!(
            read_git_blob_utf8(&repo, "HEAD", "README.md").unwrap().as_deref(),
            Some("hi\n")
        );
        assert_eq!(read_git_blob_utf8(&repo, "HEAD", "missing.txt").unwrap(), None);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub/file.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("bin.dat"), [0xff_u8, 0xfe, 0x00]).unwrap();
        assert!(git_status(&repo, &["add", "sub/file.txt", "bin.dat"]).unwrap().success());
        assert!(git_status(&repo, &["commit", "-m", "add sub and bin"]).unwrap().success());
        assert_eq!(read_git_blob_utf8(&repo, "HEAD", "sub").unwrap(), None);
        assert!(read_git_blob_utf8(&repo, "HEAD", "bin.dat").is_err());

        // Verify diff_worktree_file_against_rev for tracked, untracked, pre-baseline untracked, deleted, and mode diffs.
        assert_eq!(
            diff_worktree_file_against_rev(&repo, "HEAD", "sub/file.txt", 3).unwrap(),
            ""
        );
        std::fs::write(repo.join("sub/file.txt"), "one\ntwo-edited\n").unwrap();
        let tracked_diff = diff_worktree_file_against_rev(&repo, "HEAD", "sub/file.txt", 3).unwrap();
        assert!(tracked_diff.contains("-two\n+two-edited\n"), "{tracked_diff}");

        std::fs::write(repo.join("untracked.txt"), "fresh\n").unwrap();
        let untracked_diff = diff_worktree_file_against_rev(&repo, "HEAD", "untracked.txt", 3).unwrap();
        assert!(untracked_diff.contains("--- /dev/null"), "{untracked_diff}");
        assert!(untracked_diff.contains("+fresh\n"), "{untracked_diff}");

        let head_tree = run_git_stdout(&repo, &["rev-parse", "HEAD^{tree}"], None, &[], None).unwrap();
        std::fs::remove_file(repo.join("sub/file.txt")).unwrap();
        let deleted_diff = diff_worktree_file_against_rev(&repo, &head_tree, "sub/file.txt", 3).unwrap();
        assert!(deleted_diff.contains("-one\n-two\n"), "{deleted_diff}");

        // Restore as untracked identical to baseline tree -> empty diff; mode change -> mode diff.
        assert!(git_status(&repo, &["rm", "--cached", "sub/file.txt"]).unwrap().success());
        std::fs::write(repo.join("sub/file.txt"), "one\ntwo\n").unwrap();
        assert_eq!(
            diff_worktree_file_against_rev(&repo, &head_tree, "sub/file.txt", 3).unwrap(),
            ""
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = repo.join("sub/file.txt");
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
            let mode_only = diff_worktree_file_against_rev(&repo, &head_tree, "sub/file.txt", 3).unwrap();
            assert_eq!(mode_only.matches("old mode 100644").count(), 1, "{mode_only}");
            assert_eq!(mode_only.matches("new mode 100755").count(), 1, "{mode_only}");
        }

        // Preserve leading/trailing spaces in actual OS filenames while rejecting invalid delta paths.
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let spaced_cases = [
            ("src/ leading.rs", "pub fn leading_space() -> i32 { 1 }\n", "pub fn leading_space() -> i32 { 2 }\n"),
            ("src/leading.rs", "pub fn plain() -> i32 { 10 }\n", "pub fn plain() -> i32 { 20 }\n"),
            ("src/trailing.rs ", "pub fn trailing_space() -> i32 { 3 }\n", "pub fn trailing_space() -> i32 { 4 }\n"),
        ];
        for (rel, base_text, _) in spaced_cases {
            std::fs::write(repo.join(rel), base_text).unwrap();
            assert!(git_status(&repo, &["add", "--", rel]).unwrap().success());
        }
        assert!(git_status(&repo, &["commit", "-m", "add spaced filenames"]).unwrap().success());
        for (rel, _, live_text) in spaced_cases {
            std::fs::write(repo.join(rel), live_text).unwrap();
        }
        for (rel, expected_del, expected_add, forbidden) in [
            ("src/ leading.rs", "-pub fn leading_space() -> i32 { 1 }", "+pub fn leading_space() -> i32 { 2 }", "plain()"),
            ("./src/ leading.rs", "-pub fn leading_space() -> i32 { 1 }", "+pub fn leading_space() -> i32 { 2 }", "plain()"),
            ("src/leading.rs", "-pub fn plain() -> i32 { 10 }", "+pub fn plain() -> i32 { 20 }", "leading_space()"),
            ("src/trailing.rs ", "-pub fn trailing_space() -> i32 { 3 }", "+pub fn trailing_space() -> i32 { 4 }", "plain()"),
        ] {
            let diff = diff_worktree_file_against_rev(&repo, "HEAD", rel, 3).unwrap();
            assert!(diff.contains(expected_del), "{rel}: {diff}");
            assert!(diff.contains(expected_add), "{rel}: {diff}");
            assert!(!diff.contains(forbidden), "{rel} leaked neighbor diff: {diff}");
        }
        for invalid in ["", ".", "./", "../escape.rs", "/abs.rs", "\\win.rs", "C:drive.rs", "bad\0.rs"] {
            assert!(
                diff_worktree_file_against_rev(&repo, "HEAD", invalid, 3).is_err(),
                "expected invalid delta path '{invalid:?}' to error"
            );
        }
    }
}
