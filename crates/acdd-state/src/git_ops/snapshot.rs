//! Temporary `GIT_INDEX_FILE` snapshots, GC keep refs, candidate tree hashing, diffing, and rollback.

use super::{
    ensure_non_empty_git_identity, run_git_checked, run_git_raw, run_git_stdout, ScopedTempDir,
};
use crate::types::IsolationMode;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{atomic_write, checked_child, ensure_confined_dir_all, open_confined};
use acdd_core::ids::{CommitOid, RunId, TreeOid};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_SNAPSHOT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8192;
static KEEP_REF_SEQ: AtomicU64 = AtomicU64::new(0);

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
        let dir = ScopedTempDir::create("acdd-idx")?;
        let index_path = dir.0.join("index");
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

/// Short-lived GC keep-ref guard preventing `git gc` from pruning unreferenced snapshot objects.
#[derive(Debug)]
pub struct GitGcProtection {
    /// Worktree/repository path owning the keep ref.
    pub worktree: PathBuf,
    /// Temporary ref under `refs/acdd/keep/<run_id>/...`.
    pub keep_ref: String,
}

impl GitGcProtection {
    /// Protect `commit_oid` under `refs/acdd/keep/<run_id>/...` until this guard is dropped.
    pub fn acquire(
        worktree: impl Into<PathBuf>,
        run_id: &RunId,
        commit_oid: &CommitOid,
    ) -> Result<Self, StateError> {
        let wt = worktree.into();
        let seq = KEEP_REF_SEQ.fetch_add(1, Ordering::Relaxed);
        let keep_ref = format!(
            "refs/acdd/keep/{}/{}-{}",
            run_id.as_str(),
            std::process::id(),
            seq
        );
        run_git_checked(
            &wt,
            &["update-ref", &keep_ref, commit_oid.as_str()],
            None,
            None,
            ErrorCode::ExternalGitDrift,
        )?;
        Ok(Self {
            worktree: wt,
            keep_ref,
        })
    }
}

impl Drop for GitGcProtection {
    fn drop(&mut self) {
        let _ = run_git_raw(
            &self.worktree,
            &["update-ref", "-d", &self.keep_ref],
            None,
            &[],
            None,
        );
    }
}

/// Parameters for capturing a parentless snapshot under `refs/acdd/snapshots/<run_id>/...`.
#[derive(Debug, Clone)]
pub struct CaptureSnapshotReq {
    /// Target worktree directory.
    pub worktree: PathBuf,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Node slug or `"manual"` namespace segment.
    pub namespace: String,
    /// Snapshot label or `"rev-<N>"`.
    pub label: String,
    /// Optional scope restriction (`None` captures full `is_candidate_included` tree).
    pub scope: Option<Vec<String>>,
    /// Workspace path policy controlling `is_candidate_included`.
    pub path_policy: WorkspacePathPolicy,
    /// Human-readable commit message for the parentless snapshot commit.
    pub message: String,
}

/// Result of capturing a Git snapshot ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSnapshotResp {
    /// Full Git reference written (`refs/acdd/snapshots/...`).
    pub snapshot_ref: String,
    /// Parentless commit OID pointed to by `snapshot_ref`.
    pub commit_oid: CommitOid,
    /// Tree OID captured by the snapshot.
    pub tree_oid: TreeOid,
}

/// Parameters for restoring files from a snapshot ref or commit/tree OID without moving `HEAD`.
#[derive(Debug, Clone)]
pub struct RollbackReq {
    /// Target worktree directory.
    pub worktree: PathBuf,
    /// Snapshot ref, commit OID, or tree OID to restore from.
    pub target_ref_or_oid: String,
    /// Optional active node write scope (`None` uses full `is_candidate_included`).
    pub admitted_scope: Option<Vec<String>>,
    /// Workspace path policy filtering allowed files.
    pub path_policy: WorkspacePathPolicy,
}

/// Summary of a confined rollback restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackResp {
    /// Resolved target tree OID restored into the working tree.
    pub target_tree: TreeOid,
    /// Repository-relative paths restored or deleted.
    pub restored_paths: Vec<String>,
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

/// Capture an orphan snapshot commit and atomically update `refs/acdd/snapshots/<run_id>/...`.
pub fn capture_snapshot(req: &CaptureSnapshotReq) -> Result<CaptureSnapshotResp, StateError> {
    if req.namespace.trim().is_empty()
        || req.label.trim().is_empty()
        || req.namespace.contains("..")
        || req.label.contains("..")
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "invalid snapshot namespace '{}' or label '{}'",
                req.namespace, req.label
            ),
        ));
    }
    let snapshot_ref = format!(
        "refs/acdd/snapshots/{}/{}/{}",
        req.run_id.as_str(),
        req.namespace,
        req.label
    );
    run_git_checked(
        &req.worktree,
        &["check-ref-format", &snapshot_ref],
        None,
        None,
        ErrorCode::ProfileSchemaInvalid,
    )?;

    let tree_oid = current_scope_tree(&req.worktree, req.scope.as_deref(), &req.path_policy)?;
    let identity_env = ensure_non_empty_git_identity(&req.worktree);
    let commit_out = run_git_raw(
        &req.worktree,
        &["commit-tree", tree_oid.as_str(), "-F", "-"],
        None,
        &identity_env,
        Some(req.message.as_bytes()),
    )?;
    if !commit_out.status.success() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "git commit-tree failed for snapshot: {}",
                String::from_utf8_lossy(&commit_out.stderr).trim()
            ),
        ));
    }
    let commit_oid = CommitOid::new(String::from_utf8_lossy(&commit_out.stdout).trim())?;
    let _gc = GitGcProtection::acquire(&req.worktree, &req.run_id, &commit_oid)?;
    run_git_checked(
        &req.worktree,
        &["update-ref", &snapshot_ref, commit_oid.as_str()],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;

    Ok(CaptureSnapshotResp {
        snapshot_ref,
        commit_oid,
        tree_oid,
    })
}

/// Confined-restore working tree paths from a snapshot without mutating `HEAD`.
pub fn restore_rollback(req: &RollbackReq) -> Result<RollbackResp, StateError> {
    let target_trimmed = req.target_ref_or_oid.trim();
    if target_trimmed.is_empty() || target_trimmed.starts_with('-') {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid rollback target '{}'", req.target_ref_or_oid),
        ));
    }
    let head_before = run_git_raw(&req.worktree, &["rev-parse", "--verify", "HEAD"], None, &[], None)?
        .status
        .success()
        .then(|| super::commit::current_head_commit(&req.worktree))
        .transpose()?;

    let tree_spec = format!("{target_trimmed}^{{tree}}");
    let target_tree_raw = run_git_stdout(
        &req.worktree,
        &["rev-parse", "--verify", &tree_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let target_tree = TreeOid::new(target_tree_raw)?;

    let is_eligible = |norm: &str| -> bool {
        req.path_policy.is_candidate_included(norm)
            && req
                .admitted_scope
                .as_ref()
                .is_none_or(|scope| req.path_policy.is_scope_admitted(norm, scope))
    };

    let tree_out = run_git_checked(
        &req.worktree,
        &["ls-tree", "-r", "-z", "--full-tree", target_tree.as_str()],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let mut target_entries: BTreeMap<String, (String, String)> = BTreeMap::new();
    for record in tree_out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let path_raw = String::from_utf8_lossy(&record[tab + 1..]);
        let mut parts = header.split_whitespace();
        let (Some(mode), Some(_kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Some(norm) = normalize_rel_path(&path_raw) else {
            continue;
        };
        if !is_eligible(&norm) {
            continue;
        }
        if mode == "160000" {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("submodule entry '{norm}' cannot be restored by rollback"),
            ));
        }
        target_entries.insert(norm, (mode.to_owned(), oid.to_owned()));
    }

    let mut current_eligible = BTreeSet::new();
    for path in list_stage_entries(&req.worktree, None)?.into_keys() {
        if is_eligible(&path) {
            current_eligible.insert(path);
        }
    }
    for raw in list_untracked_entries(&req.worktree)? {
        if let Some(norm) = normalize_rel_path(raw.trim_end_matches('/')) {
            if is_eligible(&norm) {
                current_eligible.insert(norm);
            }
        }
    }

    let mut restored_paths = Vec::new();
    for current in &current_eligible {
        if !target_entries.contains_key(current) {
            remove_confined_file(&req.worktree, current)?;
            restored_paths.push(current.clone());
        }
    }

    for (rel_path, (mode, expected_oid)) in &target_entries {
        if working_entry_matches(&req.worktree, rel_path, mode, expected_oid)? {
            continue;
        }
        restore_blob_confined(&req.worktree, rel_path, mode, expected_oid)?;
        restored_paths.push(rel_path.clone());
    }

    if let Some(before) = head_before {
        let head_after = super::commit::current_head_commit(&req.worktree)?;
        if head_after != before {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                "HEAD moved unexpectedly during rollback",
            ));
        }
    }

    restored_paths.sort();
    restored_paths.dedup();
    Ok(RollbackResp {
        target_tree,
        restored_paths,
    })
}

fn working_entry_matches(
    worktree: &Path,
    rel_path: &str,
    expected_mode: &str,
    expected_oid: &str,
) -> Result<bool, StateError> {
    let abs = worktree.join(rel_path);
    let meta = match fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed inspecting '{rel_path}' during rollback: {err}"),
            ));
        }
    };
    if expected_mode == "120000" {
        if !meta.file_type().is_symlink() {
            return Ok(false);
        }
        let target = fs::read_link(&abs).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed reading symlink '{rel_path}': {err}"),
            )
        })?;
        let out = run_git_checked(
            worktree,
            &["hash-object", "--stdin"],
            None,
            Some(target.to_string_lossy().as_bytes()),
            ErrorCode::ExternalGitDrift,
        )?;
        return Ok(String::from_utf8_lossy(&out.stdout).trim() == expected_oid);
    }
    if !meta.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let is_exec = meta.permissions().mode() & 0o111 != 0;
        let want_exec = expected_mode == "100755";
        if is_exec != want_exec {
            return Ok(false);
        }
    }
    let out = run_git_checked(
        worktree,
        &["hash-object", "--", rel_path],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == expected_oid)
}

fn restore_blob_confined(
    worktree: &Path,
    rel_path: &str,
    mode: &str,
    blob_oid: &str,
) -> Result<(), StateError> {
    let rel = Path::new(rel_path);
    let target = checked_child(worktree, rel).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("rollback path '{rel_path}' failed confinement: {err}"),
        )
    })?;
    let rel_parent = rel
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_confined_dir_all(worktree, rel_parent).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "rollback failed preparing parent directory for '{rel_path}': {err}"
            ),
        )
    })?;
    let blob = run_git_checked(
        worktree,
        &["cat-file", "blob", blob_oid],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    if mode == "120000" {
        if fs::symlink_metadata(&target).is_ok() {
            fs::remove_file(&target).map_err(|err| {
                StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("failed replacing '{rel_path}' with symlink: {err}"),
                )
            })?;
        }
        let link_target = String::from_utf8(blob.stdout).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("non-UTF-8 symlink target in '{rel_path}': {err}"),
            )
        })?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&link_target, &target).map_err(|err| {
            StateError::new(
                ErrorCode::ExternalGitDrift,
                format!("failed creating symlink '{rel_path}': {err}"),
            )
        })?;
        #[cfg(not(unix))]
        let _ = link_target;
        return Ok(());
    }
    let executable = mode == "100755";
    atomic_write(&target, &blob.stdout, executable).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed restoring '{rel_path}': {err}"),
        )
    })?;
    Ok(())
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

/// Read the Git blob OID of `rel_path` at `tree_or_commit` (or `None` if absent).
pub fn blob_oid_at(
    worktree: &Path,
    tree_or_commit: &str,
    rel_path: &str,
) -> Result<Option<String>, StateError> {
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
            let (_mode, _kind, oid) = (parts.next(), parts.next(), parts.next());
            return Ok(oid.map(str::to_owned));
        }
    }
    Ok(None)
}
