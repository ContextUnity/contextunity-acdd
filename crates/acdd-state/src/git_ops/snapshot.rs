//! Temporary `GIT_INDEX_FILE` scoped trees, candidate hashing, and diffing.

use super::{run_git_checked, run_git_stdout, ScopedTempDir};
use crate::types::IsolationMode;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{atomic_replace, checked_child, open_confined};
use acdd_core::ids::{CommitOid, RunId, TaskRef, TreeOid};
use acdd_core::path_policy::{is_under_dir_case_insensitive, normalize_rel_path, WorkspacePathPolicy};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_SNAPSHOT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8192;

/// Isolated temporary Git index file (`GIT_INDEX_FILE`) leaving `.git/index` untouched.
#[derive(Debug)]
pub struct TemporaryGitIndex {
    /// Path to the temporary index file.
    pub index_path: PathBuf,
    /// Worktree root bound to this temporary index.
    pub worktree: PathBuf,
}

impl Drop for TemporaryGitIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.index_path);
    }
}

impl TemporaryGitIndex {
    /// Allocate a new temporary index file path for `worktree`.
    pub fn new(worktree: impl Into<PathBuf>, index_path: impl Into<PathBuf>) -> Self {
        Self {
            index_path: index_path.into(),
            worktree: worktree.into(),
        }
    }

    pub(super) fn create_temp(worktree: &Path) -> Result<(Self, ScopedTempDir), StateError> {
        let dir = super::create_scoped_temp_dir("acdd-idx")?;
        let index_path = dir.path().join("index");
        Ok((Self::new(worktree.to_path_buf(), index_path), dir))
    }

    /// Populate this temporary index from `tree_or_head` (`git read-tree`).
    pub fn read_tree(&self, tree_or_head: &str) -> Result<(), StateError> {
        if let Some(parent) = self.index_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "failed to prepare temporary index directory '{}': {err}",
                        parent.display()
                    ),
                )
            })?;
        }
        // Git rejects a 0-byte existing file in GIT_INDEX_FILE; ensure clean start.
        if let Ok(meta) = fs::symlink_metadata(&self.index_path) {
            if meta.len() == 0 {
                let _ = fs::remove_file(&self.index_path);
            }
        }
        let args: Vec<&str> = if tree_or_head == "--empty" {
            vec!["read-tree", "--empty"]
        } else {
            vec!["read-tree", tree_or_head]
        };
        run_git_checked(
            &self.worktree,
            &args,
            Some(&self.index_path),
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        Ok(())
    }

    /// Write the current temporary index contents to a Git tree object (`git write-tree`).
    pub fn write_tree(&self) -> Result<TreeOid, StateError> {
        let oid = run_git_stdout(
            &self.worktree,
            &["write-tree"],
            Some(&self.index_path),
            ErrorCode::ExternalGitDrift,
        )?;
        TreeOid::new(oid).map_err(StateError::from)
    }

    pub(super) fn stage_paths(&self, paths: &[String]) -> Result<(), StateError> {
        for chunk in paths.chunks(200) {
            let mut args: Vec<String> = vec![
                "add".to_owned(),
                "-f".to_owned(),
                "--".to_owned(),
            ];
            for path in chunk {
                args.push(format!(":(literal){path}"));
            }
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_git_checked(
                &self.worktree,
                &arg_refs,
                Some(&self.index_path),
                None,
                ErrorCode::ExternalGitDrift,
            )?;
        }
        Ok(())
    }

    pub(super) fn remove_paths(&self, paths: &[String]) -> Result<(), StateError> {
        for chunk in paths.chunks(200) {
            let mut args: Vec<String> = vec![
                "rm".to_owned(),
                "--cached".to_owned(),
                "--quiet".to_owned(),
                "--ignore-unmatch".to_owned(),
                "--".to_owned(),
            ];
            for path in chunk {
                args.push(format!(":(literal){path}"));
            }
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_git_checked(
                &self.worktree,
                &arg_refs,
                Some(&self.index_path),
                None,
                ErrorCode::ExternalGitDrift,
            )?;
        }
        Ok(())
    }
}

pub(super) fn list_stage_entries(
    worktree: &Path,
    index_file: Option<&Path>,
) -> Result<BTreeMap<String, (String, String)>, StateError> {
    let out = run_git_checked(
        worktree,
        &["ls-files", "--cached", "--stage", "-z"],
        index_file,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let mut entries = BTreeMap::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let path_raw = String::from_utf8_lossy(&record[tab + 1..]);
        let mut parts = header.split_whitespace();
        let (Some(mode), Some(oid), _stage) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let Some(norm) = normalize_rel_path(&path_raw) {
            entries.insert(norm, (mode.to_owned(), oid.to_owned()));
        }
    }
    Ok(entries)
}

pub(super) fn list_untracked_entries(worktree: &Path) -> Result<Vec<String>, StateError> {
    let out = run_git_checked(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let mut paths = Vec::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let raw = String::from_utf8_lossy(record);
        paths.push(raw.into_owned());
    }
    Ok(paths)
}

pub(super) fn guard_candidate_path(
    worktree: &Path,
    rel_path: &str,
    index_mode: Option<&str>,
) -> Result<(), StateError> {
    let relative = Path::new(rel_path);
    let invalid = |reason| StateError::new(ErrorCode::ExternalGitDrift, format!("candidate '{rel_path}': {reason}"));
    if index_mode == Some("160000") {
        return Err(invalid("submodule gitlink is not permitted"));
    }
    let metadata = match worktree.join(relative).symlink_metadata() {
        Ok(meta) => Some(meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(snapshot_path_error(rel_path, err)),
    };
    let skip_leaf = !metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    for prefix in relative.ancestors().skip(usize::from(skip_leaf)).take_while(|p| !p.as_os_str().is_empty()) {
        match worktree.join(prefix).join(".git").symlink_metadata() {
            Ok(_) => return Err(invalid("nested git repository/submodule is not permitted")),
            Err(err) if matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {}
            Err(err) => return Err(snapshot_path_error(rel_path, err)),
        }
    }
    let Some(metadata) = metadata.filter(fs::Metadata::is_file) else {
        return Ok(());
    };
    if metadata.len() > MAX_SNAPSHOT_FILE_BYTES {
        return Err(invalid("file exceeds 16 MiB snapshot/commit limit"));
    }
    let mut file = open_confined(worktree, relative, OpenOptions::new().read(true))
        .map_err(|err| snapshot_path_error(rel_path, err))?;
    let mut buf = [0; BINARY_SNIFF_BYTES];
    let size = file.read(&mut buf).map_err(|err| snapshot_path_error(rel_path, err))?;
    if buf[..size].contains(&0) {
        return Err(invalid("binary file contains NUL bytes"));
    }
    Ok(())
}

pub(super) fn collect_literal_scope_paths(
    worktree: &Path,
    scope: &[String],
    candidates: &mut BTreeMap<String, Option<String>>,
) {
    for pattern in scope {
        if pattern.contains('*') || pattern.contains('?') {
            continue;
        }
        let Some(norm) = normalize_rel_path(pattern) else {
            continue;
        };
        if !candidates.contains_key(&norm) && fs::symlink_metadata(worktree.join(&norm)).is_ok() {
            candidates.entry(norm).or_insert(None);
        }
    }
}

/// Compute the deterministic `TreeOid` over `scope` (or full `is_candidate_included`) in `worktree`.
pub fn current_scope_tree(
    worktree: &Path,
    scope: Option<&[String]>,
    path_policy: &WorkspacePathPolicy,
) -> Result<TreeOid, StateError> {
    let staged = list_stage_entries(worktree, None)?;
    let untracked = list_untracked_entries(worktree)?;

    let mut candidates: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (path, (mode, _oid)) in staged {
        candidates.insert(path, Some(mode));
    }
    for raw in untracked {
        let is_dir_entry = raw.ends_with('/');
        let trimmed = raw.trim_end_matches('/');
        let Some(norm) = normalize_rel_path(trimmed) else {
            continue;
        };
        if is_dir_entry && worktree.join(&norm).join(".git").exists() {
            let admitted = path_policy.is_candidate_included(&norm)
                && scope.is_none_or(|s| path_policy.is_scope_admitted(&norm, s));
            if admitted {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "nested git repository/submodule '{norm}' is not permitted in snapshot/commit scope"
                    ),
                ));
            }
            continue;
        }
        candidates.entry(norm).or_insert(None);
    }
    if let Some(scope_patterns) = scope {
        collect_literal_scope_paths(worktree, scope_patterns, &mut candidates);
    }

    let mut existing_paths = Vec::new();
    for (norm, index_mode) in candidates {
        if !path_policy.is_candidate_included(&norm) {
            continue;
        }
        if let Some(scope_patterns) = scope {
            if !path_policy.is_scope_admitted(&norm, scope_patterns) {
                continue;
            }
        }
        guard_candidate_path(worktree, &norm, index_mode.as_deref())?;
        if let Ok(meta) = fs::symlink_metadata(worktree.join(&norm)) {
            if !meta.is_dir() {
                existing_paths.push(norm);
            }
        }
    }

    let (temp_index, _dir) = TemporaryGitIndex::create_temp(worktree)?;
    temp_index.read_tree("--empty")?;
    temp_index.stage_paths(&existing_paths)?;
    temp_index.write_tree()
}

pub(crate) fn snapshot_prefix(run: &RunId, task: Option<&TaskRef>) -> String {
    let mut prefix = format!("refs/acdd/snapshots/{run}/");
    if let Some(task) = task {
        prefix.push_str(&format!("{task}/"));
    }
    prefix
}

pub(crate) fn validate_snapshot_label(raw: &str) -> Result<(), StateError> {
    ((1..=64).contains(&raw.len()) && raw != "_adhoc" && !raw.starts_with('.') && !raw.ends_with('.') && !raw.ends_with(".lock") && !raw.contains("..") && raw.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))).then_some(()).ok_or_else(|| StateError::new(ErrorCode::ProfileSchemaInvalid, format!("invalid snapshot name '{raw}'")))
}

pub(crate) fn parse_older_than(raw: &str) -> Result<u64, StateError> {
    let bad = || StateError::new(ErrorCode::ProfileSchemaInvalid, format!("invalid duration '{raw}'")); let (n, unit) = (!raw.is_empty() && raw.is_ascii()).then(|| raw.split_at(raw.len() - 1)).ok_or_else(bad)?;
    let mult = match unit { "s" => 1, "m" => 60, "h" => 3600, "d" => 86400, "w" => 604800, _ => return Err(bad()) };
    n.parse::<u64>().ok().filter(|v| *v > 0).and_then(|v| v.checked_mul(mult)).ok_or_else(bad)
}

pub(crate) fn list_snapshot_refs(worktree: &Path, prefix: &str) -> Result<Vec<(String, i64)>, StateError> {
    Ok(run_git_stdout(worktree, &["for-each-ref", "--format=%(refname)%09%(committerdate:unix)", prefix], None, ErrorCode::ExternalGitDrift)?.lines().filter(|line| !line.is_empty()).map(|line| { let (name, ts) = line.split_once('\t').unwrap_or((line, "0")); (name.to_owned(), ts.parse().unwrap_or(0)) }).collect())
}

pub(crate) fn parse_canonical_revision(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?;
    let num = rest.strip_prefix('r')?;
    num.parse::<u32>().ok().filter(|v| v.to_string() == num)
}

pub(crate) fn save_task_snapshot(worktree: &Path, reference: &str, tree: &TreeOid, create_only: bool) -> Result<(), StateError> {
    let code = ErrorCode::ExternalGitDrift;
    let commit = run_git_stdout(worktree, &["-c", "user.name=acdd-agent", "-c", "user.email=acdd@contextunity.local", "commit-tree", tree.as_str(), "--no-gpg-sign", "-m", reference], None, code)?;
    let zero = "0".repeat(commit.trim().len());
    let args = if create_only { vec!["update-ref", "--no-deref", reference, &commit, &zero] } else { vec!["update-ref", "--no-deref", reference, &commit] };
    run_git_checked(worktree, &args, None, None, code).map(|_| ())
}

pub(crate) fn restore_task_snapshot(
    worktree: &Path, prefix: &str, revision: Option<u32>, scope: Option<&[String]>, policy: &WorkspacePathPolicy,
) -> Result<String, StateError> {
    let code = ErrorCode::ExternalGitDrift;
    let reference = if let Some(rev) = revision {
        format!("{prefix}r{rev}")
    } else {
        list_snapshot_refs(worktree, prefix)?.into_iter().filter_map(|(name, _)| parse_canonical_revision(&name, prefix).map(|r| (r, name)))
            .max_by_key(|(rev, _)| *rev).map(|(_, name)| name).unwrap_or_default()
    };
    if reference.is_empty() { return Err(StateError::new(code, format!("no snapshot ref found under {prefix}"))); }
    let tree = run_git_stdout(worktree, &["rev-parse", "--verify", &format!("{reference}^{{tree}}")], None, code)?;
    let current = current_scope_tree(worktree, scope, policy)?;
    let args = ["diff-tree", "-r", "--no-renames", "--raw", "-z", current.as_str(), &tree];
    let diff = run_git_checked(worktree, &args, None, None, code)?;
    let (index, backup_dir) = TemporaryGitIndex::create_temp(worktree)?;
    index.read_tree(&tree)?;
    let mut writes = Vec::new();
    let mut removals = Vec::new();
    let mut originals = Vec::new();
    let mut absent_dirs = BTreeSet::new();
    let mut records = diff.stdout.split(|b| *b == 0).filter(|record| !record.is_empty());
    while let Some(header) = records.next() {
        let raw = records.next().and_then(|p| std::str::from_utf8(p).ok())
            .ok_or_else(|| StateError::new(code, "snapshot path must be UTF-8"))?;
        if normalize_rel_path(raw).as_deref() != Some(raw) {
            return Err(StateError::new(code, format!("snapshot path '{raw}' is not normalized")));
        }
        if !policy.is_candidate_included(raw) || is_under_dir_case_insensitive(raw, ".acdd") || scope.is_some_and(|scope| !policy.is_scope_admitted(raw, scope)) {
            continue;
        }
        let path = checked_child(worktree, Path::new(raw)).map_err(|err| snapshot_path_error(raw, err))?;
        guard_candidate_path(worktree, raw, None)?;
        let dest_meta = match fs::symlink_metadata(&path) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(snapshot_path_error(raw, err)),
        };
        if dest_meta.as_ref().is_some_and(|m| !m.is_file()) {
            return Err(StateError::new(code, format!("snapshot destination '{raw}' is not a regular file")));
        }
        match header.split(|b| *b == b' ').nth(1) {
            Some(b"000000") if dest_meta.is_some() => removals.push(raw.to_owned()),
            Some(b"000000") => continue,
            Some(b"100644" | b"100755") => {
                writes.extend_from_slice(raw.as_bytes());
                writes.push(0);
            }
            _ => return Err(StateError::new(code, format!("snapshot entry '{raw}' is not a regular file"))),
        }
        let backup = if dest_meta.is_some() {
            let backup = backup_dir.path().join(originals.len().to_string());
            fs::copy(&path, &backup).map_err(|err| snapshot_path_error(raw, err))?;
            Some(backup)
        } else {
            absent_dirs.extend(path.ancestors().skip(1).take_while(|p| !p.exists()).map(Path::to_path_buf));
            None
        };
        originals.push((raw, backup));
    }
    let result = if writes.is_empty() {
        Ok(())
    } else {
        let args = ["checkout-index", "--force", "-z", "--stdin"];
        run_git_checked(worktree, &args, Some(&index.index_path), Some(&writes), code).map(|_| ())
    }
    .and_then(|()| {
        removals
            .iter()
            .try_for_each(|rel| remove_confined_file(worktree, rel))
    });
    if let Err(mut error) = result {
        for (raw, backup) in originals {
            if let Err(err) = rollback_snapshot_path(worktree, raw, backup.as_deref()) {
                error.message.push_str(&format!(". Rollback of '{raw}' failed: {err}"));
            }
        }
        for dir in absent_dirs.iter().rev().filter(|dir| dir.exists()) {
            if let Err(err) = fs::remove_dir(dir) {
                error.message.push_str(&format!(". Rollback of '{}' failed: {err}", dir.display()));
            }
        }
        return Err(error);
    }
    Ok(reference)
}

fn rollback_snapshot_path(worktree: &Path, raw: &str, backup: Option<&Path>) -> anyhow::Result<()> {
    let path = checked_child(worktree, Path::new(raw))?;
    if let Some(backup) = backup {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = fs::read(backup)?;
        let permissions = fs::metadata(backup)?.permissions();
        if !fs::read(&path).is_ok_and(|actual| actual == bytes) {
            atomic_replace(&path, &bytes)?;
        }
        if fs::metadata(&path)?.permissions() != permissions {
            fs::set_permissions(&path, permissions)?;
        }
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn snapshot_path_error(path: impl std::fmt::Display, err: std::io::Error) -> StateError {
    StateError::new(ErrorCode::ExternalGitDrift, format!("cannot update '{path}': {err}"))
}

pub(crate) fn remove_confined_file(worktree: &Path, rel_path: &str) -> Result<(), StateError> {
    let target = checked_child(worktree, Path::new(rel_path)).map_err(|err| snapshot_path_error(rel_path, err))?;
    match fs::remove_file(&target) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => result.map_err(|err| snapshot_path_error(rel_path, err))?,
    }
    for dir in target.ancestors().skip(1).take_while(|dir| *dir != worktree && dir.starts_with(worktree)) {
        if fs::remove_dir(dir).is_err() {
            break;
        }
    }
    Ok(())
}

/// Compute candidate changed paths in `worktree` according to `isolation` (`baseline_commit` vs `claim_tree`).
pub fn diff_scope_changed_paths(
    worktree: &Path,
    isolation: IsolationMode,
    baseline_commit: Option<&CommitOid>,
    claim_tree: Option<&TreeOid>,
    path_policy: &WorkspacePathPolicy,
) -> Result<Vec<String>, StateError> {
    let mut changed = BTreeSet::new();
    match isolation {
        IsolationMode::Worktree => {
            let base = baseline_commit.ok_or_else(|| {
                StateError::new(
                    ErrorCode::ExternalGitDrift,
                    "missing baseline_commit for worktree scope diff",
                )
            })?;
            let diff_out = run_git_checked(
                worktree,
                &["diff", "--name-only", "--no-renames", "-z", base.as_str()],
                None,
                None,
                ErrorCode::ExternalGitDrift,
            )?;
            for raw in diff_out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                let s = String::from_utf8_lossy(raw);
                if let Some(norm) = normalize_rel_path(&s) {
                    if path_policy.is_candidate_included(&norm) {
                        changed.insert(norm);
                    }
                }
            }
            for raw in list_untracked_entries(worktree)? {
                if let Some(norm) = normalize_rel_path(raw.trim_end_matches('/')) {
                    if path_policy.is_candidate_included(&norm) {
                        changed.insert(norm);
                    }
                }
            }
        }
        IsolationMode::InPlace => {
            let base_tree = claim_tree.ok_or_else(|| {
                StateError::new(
                    ErrorCode::ExternalGitDrift,
                    "missing claim_tree for in_place scope diff",
                )
            })?;
            let current_tree = current_scope_tree(worktree, None, path_policy)?;
            if current_tree != *base_tree {
                let diff_out = run_git_checked(
                    worktree,
                    &[
                        "diff-tree",
                        "--no-commit-id",
                        "--name-only",
                        "--no-renames",
                        "-r",
                        "-z",
                        base_tree.as_str(),
                        current_tree.as_str(),
                    ],
                    None,
                    None,
                    ErrorCode::ExternalGitDrift,
                )?;
                for raw in diff_out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                    let s = String::from_utf8_lossy(raw);
                    if let Some(norm) = normalize_rel_path(&s) {
                        if path_policy.is_candidate_included(&norm) {
                            changed.insert(norm);
                        }
                    }
                }
            }
        }
    }
    Ok(changed.into_iter().collect())
}

/// Read `(mode, object_id)` of `rel_path` at `tree_or_commit` (or `None` if absent).
pub fn tree_entry_at(
    worktree: &Path,
    tree_or_commit: &str,
    rel_path: &str,
) -> Result<Option<(String, String)>, StateError> {
    let trimmed = tree_or_commit.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid tree/commit reference '{tree_or_commit}'"),
        ));
    }
    let Some(norm) = normalize_rel_path(rel_path) else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid repository-relative path '{rel_path}'"),
        ));
    };
    let tree_spec = format!("{trimmed}^{{tree}}");
    run_git_stdout(
        worktree,
        &["rev-parse", "--verify", &tree_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let out = run_git_checked(
        worktree,
        &["ls-tree", "-z", "--full-tree", trimmed, "--", &norm],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let path_raw = String::from_utf8_lossy(&record[tab + 1..]);
        if normalize_rel_path(&path_raw).as_deref() == Some(norm.as_str()) {
            let mut parts = header.split_whitespace();
            let (mode, _kind, oid) = (parts.next(), parts.next(), parts.next());
            if let (Some(mode), Some(oid)) = (mode, oid) {
                return Ok(Some((mode.to_owned(), oid.to_owned())));
            }
        }
    }
    Ok(None)
}

/// Read the Git blob OID of `rel_path` at `tree_or_commit` (or `None` if absent).
pub fn blob_oid_at(
    worktree: &Path,
    tree_or_commit: &str,
    rel_path: &str,
) -> Result<Option<String>, StateError> {
    Ok(tree_entry_at(worktree, tree_or_commit, rel_path)?.map(|(_, oid)| oid))
}

/// List normalized relative file paths directly inside `dir_rel` at `tree_or_commit`.
/// Git tree entry returned by [`list_git_tree_entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeEntry {
    /// Git file mode (`"100644"`, `"100755"`, `"120000"`, etc.).
    pub mode: String,
    /// Git blob object ID from `ls-tree`.
    pub oid: String,
    /// Exact raw repository-relative path bytes from `git ls-tree -z` (without `\` -> `/` rewriting).
    pub raw_path: Vec<u8>,
}

/// Resolve `tree_or_commit` to its immutable Git tree object ID.
pub fn resolve_git_tree_oid(worktree: &Path, tree_or_commit: &str) -> Result<String, StateError> {
    let trimmed = tree_or_commit.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid tree/commit reference '{tree_or_commit}'"),
        ));
    }
    let tree_spec = format!("{trimmed}^{{tree}}");
    let oid = run_git_stdout(
        worktree,
        &["rev-parse", "--verify", &tree_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    Ok(oid.trim().to_owned())
}

/// List blob entries (`mode`, `path`) under `dir_rel` in `tree_or_commit`.
pub fn list_git_tree_entries(
    worktree: &Path,
    tree_or_commit: &str,
    dir_rel: &str,
) -> Result<Vec<GitTreeEntry>, StateError> {
    list_git_tree_entries_multi(worktree, tree_or_commit, &[dir_rel])
}

/// List blob entries (`mode`, `path`) under multiple `dir_rels` in `tree_or_commit` in one Git call.
pub fn list_git_tree_entries_multi(
    worktree: &Path,
    tree_or_commit: &str,
    dir_rels: &[&str],
) -> Result<Vec<GitTreeEntry>, StateError> {
    let trimmed = tree_or_commit.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid tree/commit reference '{tree_or_commit}'"),
        ));
    }
    let mut dir_prefixes = Vec::with_capacity(dir_rels.len());
    let mut full_tree = dir_rels.is_empty();
    for &dir_rel in dir_rels {
        if dir_rel.is_empty() || dir_rel == "." {
            full_tree = true;
            continue;
        }
        let stripped = dir_rel
            .strip_prefix("./")
            .unwrap_or(dir_rel)
            .trim_end_matches('/');
        if stripped.is_empty()
            || stripped
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == ".." || s.contains('\\'))
        {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid repository-relative directory '{dir_rel}'"),
            ));
        }
        dir_prefixes.push(format!("{stripped}/"));
    }
    let is_hex_oid = (trimmed.len() == 40 || trimmed.len() == 64)
        && trimmed.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_hex_oid {
        let tree_spec = format!("{trimmed}^{{tree}}");
        run_git_stdout(
            worktree,
            &["rev-parse", "--verify", &tree_spec],
            None,
            ErrorCode::ExternalGitDrift,
        )?;
    }
    let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", trimmed];
    if !full_tree {
        args.push("--");
        args.extend(dir_prefixes.iter().map(String::as_str));
    }
    let out = run_git_checked(worktree, &args, None, None, ErrorCode::ExternalGitDrift)?;
    let mut entries = Vec::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let mut parts = header.split_whitespace();
        let Some(mode) = parts.next() else {
            continue;
        };
        let kind = parts.next();
        if kind != Some("blob") {
            continue;
        }
        let Some(oid) = parts.next() else {
            continue;
        };
        entries.push(GitTreeEntry {
            mode: mode.to_owned(),
            oid: oid.to_owned(),
            raw_path: record[tab + 1..].to_vec(),
        });
    }
    entries.sort_by(|a, b| a.raw_path.cmp(&b.raw_path));
    entries.dedup_by(|a, b| a.raw_path == b.raw_path);
    Ok(entries)
}
