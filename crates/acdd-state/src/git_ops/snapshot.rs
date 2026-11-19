//! Temporary `GIT_INDEX_FILE` scoped trees, candidate hashing, and diffing.

use super::{run_git_checked, run_git_stdout, ScopedTempDir};
use crate::types::IsolationMode;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{checked_child, open_confined};
use acdd_core::ids::{CommitOid, TreeOid};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
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
    if index_mode == Some("160000") {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("submodule gitlink '{rel_path}' is not permitted in snapshot/commit scope"),
        ));
    }
    let mut ancestor = worktree.to_path_buf();
    if let Some(parent_rel) = Path::new(rel_path).parent() {
        for comp in parent_rel.components() {
            ancestor.push(comp);
            if ancestor.join(".git").exists() {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "nested git repository/submodule ancestor for '{rel_path}' is not permitted in snapshot/commit scope"
                    ),
                ));
            }
        }
    }
    let abs = worktree.join(rel_path);
    let meta = match fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed inspecting candidate path '{rel_path}': {err}"),
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        if abs.join(".git").exists() {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "nested git repository/submodule '{rel_path}' is not permitted in snapshot/commit scope"
                ),
            ));
        }
        return Ok(());
    }
    if meta.is_file() {
        if meta.len() > MAX_SNAPSHOT_FILE_BYTES {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "candidate file '{rel_path}' ({} bytes) exceeds 16 MiB snapshot/commit limit",
                    meta.len()
                ),
            ));
        }
        let mut opts = OpenOptions::new();
        opts.read(true);
        let mut file = open_confined(worktree, Path::new(rel_path), &mut opts).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed opening candidate file '{rel_path}': {err}"),
            )
        })?;
        let mut buf = [0u8; BINARY_SNIFF_BYTES];
        let n = file.read(&mut buf).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed reading candidate file '{rel_path}': {err}"),
            )
        })?;
        if buf[..n].contains(&0) {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!(
                    "binary file '{rel_path}' containing NUL bytes is not permitted in snapshot/commit scope"
                ),
            ));
        }
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

pub(crate) fn remove_confined_file(worktree: &Path, rel_path: &str) -> Result<(), StateError> {
    let target = checked_child(worktree, Path::new(rel_path)).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("delete path '{rel_path}' failed confinement: {err}"),
        )
    })?;
    match fs::symlink_metadata(&target) {
        Ok(meta) => {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("refusing to delete directory '{rel_path}' as file"),
                ));
            }
            fs::remove_file(&target).map_err(|err| {
                StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("failed deleting '{rel_path}': {err}"),
                )
            })?;
            let mut parent = target.parent();
            while let Some(dir) = parent {
                if dir == worktree || !dir.starts_with(worktree) {
                    break;
                }
                match fs::remove_dir(dir) {
                    Ok(()) => parent = dir.parent(),
                    Err(_) => break,
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed inspecting '{rel_path}' for deletion: {err}"),
            ));
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
pub fn list_git_tree_files(
    worktree: &Path,
    tree_or_commit: &str,
    dir_rel: &str,
) -> Result<Vec<String>, StateError> {
    let trimmed = tree_or_commit.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid tree/commit reference '{tree_or_commit}'"),
        ));
    }
    let trimmed_dir = dir_rel.trim();
    let dir_prefix = if trimmed_dir.is_empty() || trimmed_dir == "." {
        None
    } else {
        let Some(norm_dir) = normalize_rel_path(trimmed_dir) else {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid repository-relative directory '{dir_rel}'"),
            ));
        };
        Some(format!("{norm_dir}/"))
    };
    let tree_spec = format!("{trimmed}^{{tree}}");
    run_git_stdout(
        worktree,
        &["rev-parse", "--verify", &tree_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let out = match dir_prefix {
        Some(ref prefix) => run_git_checked(
            worktree,
            &["ls-tree", "-r", "-z", "--full-tree", trimmed, "--", prefix],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?,
        None => run_git_checked(
            worktree,
            &["ls-tree", "-r", "-z", "--full-tree", trimmed],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?,
    };
    let mut files = Vec::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let kind = header.split_whitespace().nth(1);
        if kind != Some("blob") {
            continue;
        }
        let path_raw = String::from_utf8_lossy(&record[tab + 1..]);
        if let Some(norm) = normalize_rel_path(&path_raw) {
            files.push(norm);
        }
    }
    files.sort();
    Ok(files)
}
