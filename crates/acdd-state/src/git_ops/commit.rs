//! Scoped plumbing Git commits and crash-safe archive commits.

use super::snapshot::{
    blob_oid_at, collect_literal_scope_paths, guard_candidate_path, list_stage_entries,
    list_untracked_entries, remove_confined_file, TemporaryGitIndex,
};
use super::{
    commit_has_exact_trailer, ensure_non_empty_git_identity, run_git_checked, run_git_raw,
    run_git_stdout, sync_default_index_paths, ScopedTempDir,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::fs::{atomic_write, checked_child, open_confined};
use acdd_core::ids::{CommitOid, RunId, TreeOid};
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

/// Parameters for creating a scoped commit with `Acdd-Claim` trailer.
#[derive(Debug, Clone)]
pub struct ScopedCommitReq {
    /// Worktree where the commit executes.
    pub worktree: std::path::PathBuf,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Node identifier embedded in the `Acdd-Claim` trailer.
    pub node_id: String,
    /// Active claim revision embedded in the `Acdd-Claim` trailer.
    pub claim_revision: u32,
    /// Expected parent commit OID (`baseline_commit` / `HEAD`).
    pub base_head: CommitOid,
    /// Scoped paths (`scope ∪ extended_scope` + optional spec/archive paths) to include.
    pub admitted_scope: Vec<String>,
    /// Workspace path policy.
    pub path_policy: WorkspacePathPolicy,
    /// Commit subject/message body (excluding trailer).
    pub message: String,
    /// Explicitly permit committing on `main` / `master`.
    pub allow_main: bool,
}

/// Result of creating or idempotently recovering a scoped commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedCommitResp {
    /// New or recovered HEAD commit OID.
    pub committed_oid: Option<CommitOid>,
    /// Tree OID of the committed scoped state.
    pub committed_tree: TreeOid,
    /// `true` if a pre-existing `Acdd-Claim` commit was recovered idempotently after a crash.
    pub recovered_idempotently: bool,
}

/// Parameters for writing and optionally committing the final archive Markdown document.
#[derive(Debug, Clone)]
pub struct ArchiveCommitReq {
    /// Primary or run worktree root.
    pub worktree: std::path::PathBuf,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Closing root gate node ID.
    pub node_id: String,
    /// Closing claim revision.
    pub claim_revision: u32,
    /// Base commit captured at run start (`run_base_head..HEAD` idempotency scan).
    pub run_base_head: Option<CommitOid>,
    /// Source specification path relative to `worktree` (removed on archive).
    pub spec_relpath: String,
    /// Target archive path relative to `worktree`.
    pub archive_relpath: String,
    /// Rendered archive Markdown bytes.
    pub rendered_archive_markdown: String,
    /// Expected SHA-256 digest of `rendered_archive_markdown`.
    pub rendered_archive_digest: String,
    /// Whether `save.post == SaveAction::Commit`.
    pub should_commit: bool,
    /// Explicitly permit committing on `main` / `master`.
    pub allow_main: bool,
    /// Commit message used when `should_commit == true`.
    pub commit_message: String,
}

/// Result of writing/committing the archive Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveCommitResp {
    /// Archive relative path written on disk.
    pub archive_path: String,
    /// SHA-256 digest of the archive contents.
    pub archive_digest: String,
    /// Committed OID if committed.
    pub committed_oid: Option<CommitOid>,
    /// `true` if recovered idempotently from an interrupted archive commit.
    pub recovered_idempotently: bool,
}

/// Read the current `HEAD` commit OID in `worktree`.
pub fn current_head_commit(worktree: &Path) -> Result<CommitOid, StateError> {
    let oid = run_git_stdout(
        worktree,
        &["rev-parse", "--verify", "HEAD"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    CommitOid::new(oid).map_err(StateError::from)
}

/// Read the current symbolic branch name (`None` when detached).
pub fn current_branch_name(worktree: &Path) -> Result<Option<String>, StateError> {
    run_git_stdout(
        worktree,
        &["rev-parse", "--git-dir"],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let out = run_git_raw(worktree, &["symbolic-ref", "-q", "HEAD"], None, &[], None)?;
    if out.status.success() {
        let full = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let short = full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned();
        if short.is_empty() {
            Ok(None)
        } else {
            Ok(Some(short))
        }
    } else if out.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "git symbolic-ref failed in '{}': {}",
                worktree.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ))
    }
}

/// Return `true` when `branch` is `main` or `master` (`refs/heads/{main,master}`).
pub fn is_protected_main_branch(branch: Option<&str>) -> bool {
    matches!(
        branch,
        Some("main" | "master" | "refs/heads/main" | "refs/heads/master")
    )
}

/// Create or idempotently recover a scoped plumbing commit.
pub fn create_scoped_commit(req: &ScopedCommitReq) -> Result<ScopedCommitResp, StateError> {
    create_scoped_commit_with_expected(req, None)
}

/// Create or idempotently recover a scoped plumbing commit while verifying the staged scoped subset matches `expected_scoped_tree`.
pub fn create_scoped_commit_with_expected(
    req: &ScopedCommitReq,
    expected_scoped_tree: Option<&TreeOid>,
) -> Result<ScopedCommitResp, StateError> {
    let claim_trailer = format!(
        "Acdd-Claim: {}/{}/{}",
        req.run_id.as_str(),
        req.node_id,
        req.claim_revision
    );

    ensure_branch_writable(&req.worktree, req.allow_main)?;

    let (temp_index, idx_dir) = TemporaryGitIndex::create_temp(&req.worktree)?;
    temp_index.read_tree(req.base_head.as_str())?;

    let staged = list_stage_entries(&req.worktree, Some(&temp_index.index_path))?;
    let untracked = list_untracked_entries(&req.worktree)?;

    let mut candidates: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (path, (mode, _oid)) in staged {
        candidates.insert(path, Some(mode));
    }
    for raw in untracked {
        let is_dir_entry = raw.ends_with('/');
        let Some(norm) = normalize_rel_path(raw.trim_end_matches('/')) else {
            continue;
        };
        if is_dir_entry && req.worktree.join(&norm).join(".git").exists() {
            let admitted = req.path_policy.is_candidate_included(&norm)
                && req.path_policy.is_scope_admitted(&norm, &req.admitted_scope);
            if admitted {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "nested git repository/submodule '{norm}' is not permitted in scoped commit"
                    ),
                ));
            }
            continue;
        }
        candidates.entry(norm).or_insert(None);
    }
    collect_literal_scope_paths(&req.worktree, &req.admitted_scope, &mut candidates);

    let mut to_stage = Vec::new();
    let mut to_remove = Vec::new();
    for (norm, index_mode) in candidates {
        if !req.path_policy.is_candidate_included(&norm)
            || !req.path_policy.is_scope_admitted(&norm, &req.admitted_scope)
        {
            continue;
        }
        guard_candidate_path(&req.worktree, &norm, index_mode.as_deref())?;
        match fs::symlink_metadata(req.worktree.join(&norm)) {
            Ok(meta) => {
                if !meta.is_dir() {
                    to_stage.push(norm);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if index_mode.is_some() {
                    to_remove.push(norm);
                }
            }
            Err(err) => {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!("failed inspecting scoped path '{norm}': {err}"),
                ));
            }
        }
    }

    temp_index.remove_paths(&to_remove)?;
    temp_index.stage_paths(&to_stage)?;
    let expected_tree = temp_index.write_tree()?;

    if let Some(expected_scoped) = expected_scoped_tree {
        let staged_entries = list_stage_entries(&req.worktree, Some(&temp_index.index_path))?;
        let mut index_info = String::new();
        for (path, (mode, oid)) in staged_entries {
            if !req.path_policy.is_candidate_included(&path)
                || !req.path_policy.is_scope_admitted(&path, &req.admitted_scope)
            {
                continue;
            }
            index_info.push_str(&mode);
            index_info.push_str(" blob ");
            index_info.push_str(&oid);
            index_info.push('\t');
            index_info.push_str(&path);
            index_info.push('\0');
        }
        let (scoped_idx, _scoped_dir) = TemporaryGitIndex::create_temp(&req.worktree)?;
        scoped_idx.read_tree("--empty")?;
        if !index_info.is_empty() {
            let out = run_git_raw(
                &req.worktree,
                &["update-index", "-z", "--index-info"],
                Some(&scoped_idx.index_path),
                &[],
                Some(index_info.as_bytes()),
            )?;
            if !out.status.success() {
                return Err(StateError::new(
                    ErrorCode::ExternalGitDrift,
                    format!(
                        "failed verifying staged scoped tree via update-index: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                ));
            }
        }
        let staged_scoped_tree = scoped_idx.write_tree()?;
        if &staged_scoped_tree != expected_scoped {
            return Err(StateError::new(
                ErrorCode::TaskCandidateMismatch,
                format!(
                    "working tree changed before scoped commit (expected scoped tree {}, got {})",
                    expected_scoped, staged_scoped_tree
                ),
            ));
        }
    }

    let actual_head = current_head_commit(&req.worktree)?;
    if actual_head != req.base_head {
        if let Ok((head_tree, head_parent)) =
            commit_tree_and_single_parent(&req.worktree, actual_head.as_str())
        {
            if head_tree == expected_tree
                && head_parent == req.base_head
                && commit_has_exact_trailer(&req.worktree, actual_head.as_str(), &claim_trailer)?
            {
                let changed =
                    diff_tree_paths(&req.worktree, req.base_head.as_str(), actual_head.as_str())?;
                sync_default_index_paths(&req.worktree, actual_head.as_str(), &changed)?;
                return Ok(ScopedCommitResp {
                    committed_oid: Some(actual_head),
                    committed_tree: expected_tree,
                    recovered_idempotently: true,
                });
            }
        }
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "HEAD '{}' drifted from expected base_head '{}' without a matching Acdd-Claim commit",
                actual_head, req.base_head
            ),
        ));
    }

    let full_msg = format_message_with_trailer(&req.message, &claim_trailer);
    let committed_oid = commit_temp_index(
        &req.worktree,
        temp_index,
        idx_dir,
        &req.base_head,
        &expected_tree,
        &full_msg,
    )?;

    Ok(ScopedCommitResp {
        committed_oid: Some(committed_oid),
        committed_tree: expected_tree,
        recovered_idempotently: false,
    })
}

/// Write `archive_relpath` (confined + atomic), optionally commit, or idempotently recover after crash.
pub fn archive_spec_with_git(req: &ArchiveCommitReq) -> Result<ArchiveCommitResp, StateError> {
    let Some(norm_spec) = normalize_rel_path(&req.spec_relpath) else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid spec relative path '{}'", req.spec_relpath),
        ));
    };
    let Some(norm_archive) = normalize_rel_path(&req.archive_relpath) else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid archive relative path '{}'", req.archive_relpath),
        ));
    };

    let expected_digest = sha256_hex(req.rendered_archive_markdown.as_bytes());
    if expected_digest != req.rendered_archive_digest {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "rendered_archive_digest mismatch: expected '{expected_digest}', got '{}'",
                req.rendered_archive_digest
            ),
        ));
    }

    let claim_trailer = format!(
        "Acdd-Claim: {}/{}/{}",
        req.run_id.as_str(),
        req.node_id,
        req.claim_revision
    );
    let archive_abs = checked_child(&req.worktree, Path::new(&norm_archive)).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("archive path '{norm_archive}' failed confinement check: {err}"),
        )
    })?;

    let will_commit = req.should_commit;
    if will_commit {
        if let Some(existing_oid) = find_claim_commit_in_range(
            &req.worktree,
            req.run_base_head.as_ref(),
            &claim_trailer,
        )? {
            let archive_on_disk = archive_abs.is_file();
            let archive_in_commit =
                blob_oid_at(&req.worktree, existing_oid.as_str(), &norm_archive)?.is_some();
            if archive_on_disk || archive_in_commit {
                if !archive_on_disk {
                    atomic_write(&archive_abs, req.rendered_archive_markdown.as_bytes(), false)
                        .map_err(|err| {
                            StateError::new(
                                ErrorCode::ExternalGitDrift,
                                format!("failed re-materializing '{norm_archive}' on retry: {err}"),
                            )
                        })?;
                }
                remove_confined_file(&req.worktree, &norm_spec)?;
                if existing_oid == current_head_commit(&req.worktree)? {
                    let changed = diff_tree_paths(
                        &req.worktree,
                        &format!("{existing_oid}^1"),
                        existing_oid.as_str(),
                    )
                    .unwrap_or_default();
                    let _ = sync_default_index_paths(&req.worktree, existing_oid.as_str(), &changed);
                }
                return Ok(ArchiveCommitResp {
                    archive_path: norm_archive,
                    archive_digest: req.rendered_archive_digest.clone(),
                    committed_oid: Some(existing_oid),
                    recovered_idempotently: true,
                });
            }
        }
    } else if matches!(fs::symlink_metadata(&archive_abs), Ok(m) if m.is_file()) {
        let mut opts = OpenOptions::new();
        opts.read(true);
        if let Ok(mut file) = open_confined(&req.worktree, Path::new(&norm_archive), &mut opts) {
            let mut bytes = Vec::new();
            if file.read_to_end(&mut bytes).is_ok()
                && sha256_hex(&bytes) == req.rendered_archive_digest
            {
                return Ok(ArchiveCommitResp {
                    archive_path: norm_archive,
                    archive_digest: req.rendered_archive_digest.clone(),
                    committed_oid: None,
                    recovered_idempotently: true,
                });
            }
        }
    }

    if will_commit {
        ensure_branch_writable(&req.worktree, req.allow_main)?;
    }

    let archive_existed_before = fs::symlink_metadata(&archive_abs).is_ok();
    let archive_tracked_before = if will_commit {
        blob_oid_at(&req.worktree, "HEAD", &norm_archive)?.is_some()
    } else {
        false
    };

    atomic_write(&archive_abs, req.rendered_archive_markdown.as_bytes(), false).map_err(|err| {
        StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("failed writing archive '{norm_archive}': {err}"),
        )
    })?;
    super::failpoint_hit("archive_after_write");

    if will_commit {
        let base_head = current_head_commit(&req.worktree)?;
        let (temp_index, idx_dir) = TemporaryGitIndex::create_temp(&req.worktree)?;
        if let Err(err) = (|| -> Result<TreeOid, StateError> {
            temp_index.read_tree(base_head.as_str())?;
            temp_index.stage_paths(std::slice::from_ref(&norm_archive))?;
            temp_index.remove_paths(std::slice::from_ref(&norm_spec))?;
            temp_index.write_tree()
        })() {
            if !archive_existed_before && !archive_tracked_before {
                let _ = remove_confined_file(&req.worktree, &norm_archive);
            }
            return Err(err);
        }
        let expected_tree = temp_index.write_tree()?;
        let full_msg = format_message_with_trailer(&req.commit_message, &claim_trailer);
        let committed_oid = match commit_temp_index(
            &req.worktree,
            temp_index,
            idx_dir,
            &base_head,
            &expected_tree,
            &full_msg,
        ) {
            Ok(oid) => oid,
            Err(err) => {
                if !archive_existed_before && !archive_tracked_before {
                    let _ = remove_confined_file(&req.worktree, &norm_archive);
                }
                return Err(err);
            }
        };
        super::failpoint_hit("archive_after_commit");
        remove_confined_file(&req.worktree, &norm_spec)?;
        return Ok(ArchiveCommitResp {
            archive_path: norm_archive,
            archive_digest: req.rendered_archive_digest.clone(),
            committed_oid: Some(committed_oid),
            recovered_idempotently: false,
        });
    }

    Ok(ArchiveCommitResp {
        archive_path: norm_archive,
        archive_digest: req.rendered_archive_digest.clone(),
        committed_oid: None,
        recovered_idempotently: false,
    })
}

fn ensure_branch_writable(worktree: &Path, allow_main: bool) -> Result<(), StateError> {
    let branch = current_branch_name(worktree)?;
    if !allow_main && is_protected_main_branch(branch.as_deref()) {
        return Err(StateError::new(
            ErrorCode::MainBranchProtected,
            format!(
                "refusing to commit onto protected branch '{}'; pass --allow-main to override",
                branch.as_deref().unwrap_or("main")
            ),
        ));
    }
    Ok(())
}

fn commit_temp_index(
    worktree: &Path,
    temp_index: TemporaryGitIndex,
    idx_dir: ScopedTempDir,
    base_head: &CommitOid,
    expected_tree: &TreeOid,
    commit_message: &str,
) -> Result<CommitOid, StateError> {
    let identity_env = ensure_non_empty_git_identity(worktree);
    drop((temp_index, idx_dir));
    let out = run_git_raw(
        worktree,
        &[
            "commit-tree",
            expected_tree.as_str(),
            "-p",
            base_head.as_str(),
            "-F",
            "-",
        ],
        None,
        &identity_env,
        Some(commit_message.as_bytes()),
    )?;
    if !out.status.success() {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "git commit-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    let new_head = CommitOid::new(String::from_utf8_lossy(&out.stdout).trim())?;
    run_git_checked(
        worktree,
        &["update-ref", "HEAD", new_head.as_str(), base_head.as_str()],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    super::failpoint_hit("git_commit_after_update_ref");

    let changed = diff_tree_paths(worktree, base_head.as_str(), new_head.as_str())?;
    sync_default_index_paths(worktree, new_head.as_str(), &changed)?;
    super::failpoint_hit("git_commit_after_index_sync");
    Ok(new_head)
}

fn commit_tree_and_single_parent(
    worktree: &Path,
    commit_oid: &str,
) -> Result<(TreeOid, CommitOid), StateError> {
    let parents_line = run_git_stdout(
        worktree,
        &["rev-list", "--parents", "-n", "1", commit_oid],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let fields: Vec<&str> = parents_line.split_whitespace().collect();
    if fields.len() != 2 {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!("commit '{commit_oid}' does not have exactly one parent"),
        ));
    }
    let tree_spec = format!("{commit_oid}^{{tree}}");
    let tree_raw = run_git_stdout(
        worktree,
        &["rev-parse", "--verify", &tree_spec],
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    Ok((TreeOid::new(tree_raw)?, CommitOid::new(fields[1])?))
}

fn diff_tree_paths(worktree: &Path, base: &str, new: &str) -> Result<Vec<String>, StateError> {
    let out = run_git_checked(
        worktree,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--no-renames",
            "-r",
            "-z",
            base,
            new,
        ],
        None,
        None,
        ErrorCode::ExternalGitDrift,
    )?;
    let mut paths = Vec::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let s = String::from_utf8_lossy(raw);
        if let Some(norm) = normalize_rel_path(&s) {
            paths.push(norm);
        }
    }
    Ok(paths)
}

pub(crate) fn find_claim_commit_in_range(
    worktree: &Path,
    run_base_head: Option<&CommitOid>,
    claim_trailer: &str,
) -> Result<Option<CommitOid>, StateError> {
    let rev_out = if let Some(base) = run_base_head {
        let range = format!("{base}..HEAD");
        run_git_raw(worktree, &["rev-list", &range], None, &[], None)?
    } else {
        run_git_raw(worktree, &["rev-list", "-n", "16", "HEAD"], None, &[], None)?
    };
    if !rev_out.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&rev_out.stdout);
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if commit_has_exact_trailer(worktree, line, claim_trailer)? {
            return Ok(Some(CommitOid::new(line)?));
        }
    }
    Ok(None)
}

fn format_message_with_trailer(message: &str, trailer: &str) -> String {
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
