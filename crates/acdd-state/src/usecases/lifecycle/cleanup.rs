//! Canonical `run_tombstones.cleanup_manifest` construction (`INV-SINGLE-OWNER`).

use crate::git_ops::worktree::canonicalize_macos_var;
use crate::types::ArchiveCleanupEntry;
use acdd_core::config::WORKTREES_DIR;
use acdd_core::fs::checked_child;
use std::collections::BTreeSet;
use std::path::Path;

/// Input fields for building a canonical `run_tombstones.cleanup_manifest`.
pub(super) struct CleanupManifestInput<'a> {
    pub spec_path: Option<&'a str>,
    pub run_worktree: Option<&'a str>,
    pub run_branch: Option<&'a str>,
    pub task_worktrees_and_branches: &'a [(Option<&'a str>, Option<&'a str>)],
}

fn is_managed_worktree_entry(
    primary: &Path,
    wt: &str,
    run_name: Option<&str>,
    is_run_worktree: bool,
) -> bool {
    let canon_primary = canonicalize_macos_var(primary);
    let canon_wt = canonicalize_macos_var(Path::new(wt));
    if canon_wt == canon_primary {
        return false;
    }
    let managed_root = canonicalize_macos_var(&primary.join(WORKTREES_DIR));
    if canon_wt.parent() != Some(managed_root.as_path()) {
        return false;
    }
    let Some(name) = canon_wt.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match (run_name, is_run_worktree) {
        (Some(rn), true) => name == rn,
        (Some(rn), false) => name.starts_with(&format!("{rn}-")),
        (None, _) => !name.is_empty(),
    }
}

fn is_managed_task_branch_entry(
    br: &str,
    run_branch: Option<&str>,
    run_name: Option<&str>,
) -> bool {
    if run_branch == Some(br) {
        return false;
    }
    match run_name {
        Some(rn) => br.starts_with(&format!("task/{rn}-")),
        None => br.starts_with("task/"),
    }
}

/// Build the canonical `run_tombstones.cleanup_manifest` (`INV-SINGLE-OWNER`).
///
/// Includes `spec_file` (when present under `primary`), managed task worktrees/branches, and any
/// managed run worktree distinct from `primary`. Never includes `run.branch`
/// or manual `--worktree` checkouts/branches.
pub(super) fn build_tombstone_cleanup_manifest(
    input: &CleanupManifestInput<'_>,
    primary: &Path,
) -> Vec<ArchiveCleanupEntry> {
    build_tombstone_cleanup_manifest_with_policy(input, primary, true, None)
}

pub(super) fn build_tombstone_cleanup_manifest_with_policy(
    input: &CleanupManifestInput<'_>,
    primary: &Path,
    worktrees_managed: bool,
    run_name: Option<&str>,
) -> Vec<ArchiveCleanupEntry> {
    let mut entries = Vec::new();

    if let Some(sp) = input.spec_path {
        if checked_child(primary, Path::new(sp)).is_ok_and(|p| p.exists()) {
            entries.push(ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: sp.to_owned(),
                workspace: None,
                force: false,
            });
        }
    }

    let mut seen_wt = BTreeSet::new();
    let mut seen_br = BTreeSet::new();

    if worktrees_managed {
        for &(wt_opt, br_opt) in input.task_worktrees_and_branches {
            if let Some(wt) = wt_opt {
                if is_managed_worktree_entry(primary, wt, run_name, false)
                    && seen_wt.insert(wt.to_owned())
                {
                    entries.push(ArchiveCleanupEntry {
                        kind: "worktree".to_owned(),
                        path_or_ref: wt.to_owned(),
                        workspace: None,
                        force: false,
                    });
                }
            }
            if let Some(br) = br_opt {
                if is_managed_task_branch_entry(br, input.run_branch, run_name)
                    && seen_br.insert(br.to_owned())
                {
                    entries.push(ArchiveCleanupEntry {
                        kind: "branch".to_owned(),
                        path_or_ref: br.to_owned(),
                        workspace: None,
                        force: false,
                    });
                }
            }
        }

        if let Some(run_wt) = input.run_worktree {
            if is_managed_worktree_entry(primary, run_wt, run_name, true)
                && seen_wt.insert(run_wt.to_owned())
            {
                entries.push(ArchiveCleanupEntry {
                    kind: "worktree".to_owned(),
                    path_or_ref: run_wt.to_owned(),
                    workspace: None,
                    force: false,
                });
            }
        }
    }

    entries
}
