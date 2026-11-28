//! Single-pass runtime dependency snapshot preloading backed by `acdd_spec::discovery`.

use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunRecord, RunTombstoneRecord, TerminalStatus};
use crate::usecases::lifecycle::{list_all_runs, list_all_tombstones};
use acdd_core::config::SpecConfig;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::git::read_git_blobs_at_paths_batch;
use acdd_core::ids::{CommitOid, RunId};
use acdd_spec::discovery::{
    collect_archived_specs, is_git_repo, parse_spec_at_relpath, read_confined_utf8,
};
use acdd_spec::SpecDocument;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) enum TombstoneArchiveError {
    ExternalGitDrift(String),
    NotInClosedCommit { path: String, commit: CommitOid },
    Unreadable { path: String, err: String },
    Invalid { path: String, err: String },
}

impl TombstoneArchiveError {
    pub(super) fn to_state_error(&self, dep_label: &str) -> AcddError {
        match self {
            Self::ExternalGitDrift(err) => {
                AcddError::new(ErrorCode::ExternalGitDrift, err.clone())
            }
            Self::NotInClosedCommit { path, commit } => AcddError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "spec dependency '{dep_label}' archive '{path}' not found in closed commit '{commit}'"
                ),
            ),
            Self::Unreadable { path, err } => AcddError::new(
                ErrorCode::GateVerificationFailed,
                format!("spec dependency '{dep_label}' archive '{path}' is unreadable: {err}"),
            ),
            Self::Invalid { path, err } => AcddError::new(
                ErrorCode::GateVerificationFailed,
                format!("spec dependency '{dep_label}' archive '{path}' is invalid: {err}"),
            ),
        }
    }
}

pub(crate) struct DepSnapshot {
    pub workspace: PathBuf,
    pub runs: Vec<RunRecord>,
    pub task_statuses: BTreeMap<(RunId, String), bool>,
    pub tombstones: Vec<RunTombstoneRecord>,
    pub tombstone_archives: BTreeMap<RunId, Result<SpecDocument, TombstoneArchiveError>>,
    pub archived_specs: BTreeMap<String, (String, SpecDocument)>,
}

impl DepSnapshot {
    pub(crate) fn context(&self) -> PreloadedDepContext<'_> {
        PreloadedDepContext {
            workspace: &self.workspace,
            runs: &self.runs,
            task_statuses: &self.task_statuses,
            tombstones: &self.tombstones,
            tombstone_archives: &self.tombstone_archives,
            archived_specs: &self.archived_specs,
        }
    }
}

pub(crate) struct PreloadedDepContext<'a> {
    pub workspace: &'a Path,
    pub runs: &'a [RunRecord],
    pub task_statuses: &'a BTreeMap<(RunId, String), bool>,
    pub tombstones: &'a [RunTombstoneRecord],
    pub tombstone_archives: &'a BTreeMap<RunId, Result<SpecDocument, TombstoneArchiveError>>,
    pub archived_specs: &'a BTreeMap<String, (String, SpecDocument)>,
}

pub(super) fn find_latest_matching_tombstone<'a>(
    tombs: &'a [RunTombstoneRecord],
    sel: &str,
) -> Option<&'a RunTombstoneRecord> {
    tombs
        .iter()
        .filter(|t| {
            t.run_id.as_str() == sel
                || t.run_name.as_str() == sel
                || t.spec_id.as_deref() == Some(sel)
        })
        .max_by(|a, b| compare_tombstones(a, b))
}

fn compare_tombstones(a: &RunTombstoneRecord, b: &RunTombstoneRecord) -> std::cmp::Ordering {
    a.closed_at
        .cmp(&b.closed_at)
        .then_with(|| {
            (a.terminal_status == TerminalStatus::Cleared)
                .cmp(&(b.terminal_status == TerminalStatus::Cleared))
        })
        .then_with(|| a.run_id.as_str().cmp(b.run_id.as_str()))
}

pub(super) fn tombstone_superseded_by_cleared(
    tombs: &[RunTombstoneRecord],
    tomb: &RunTombstoneRecord,
) -> bool {
    let Some(ref spec_id) = tomb.spec_id else {
        return false;
    };
    find_latest_matching_tombstone(tombs, spec_id)
        .is_some_and(|latest| latest.terminal_status == TerminalStatus::Cleared)
}

pub(crate) fn load_task_status_map(
    reader: &dyn StateReader,
) -> Result<BTreeMap<(RunId, String), bool>, AcddError> {
    Ok(reader
        .load_task_statuses()?
        .into_iter()
        .map(|(run, node, status)| ((run, node), status == NodeStatus::Completed))
        .collect())
}

pub(crate) fn load_tombstone_archives(
    ws: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    tombs: &[RunTombstoneRecord],
) -> BTreeMap<RunId, Result<SpecDocument, TombstoneArchiveError>> {
    let mut out = BTreeMap::new();
    let archives: Vec<_> = tombs
        .iter()
        .filter_map(|t| t.archive_path.as_deref().map(|p| (t, p)))
        .collect();
    if archives.is_empty() {
        return out;
    }
    let paths: Vec<_> = archives
        .iter()
        .map(|(t, p)| (t.closed_commit.as_ref().map_or("HEAD", |c| c.as_str()), *p))
        .collect();
    let mut blobs = if is_git_repo(ws) {
        match read_git_blobs_at_paths_batch(ws, &paths) {
            Ok(blobs) => Some(blobs.into_iter()),
            Err(err) => {
                for (t, _) in &archives {
                    out.insert(
                        t.run_id.clone(),
                        Err(TombstoneArchiveError::ExternalGitDrift(err.to_string())),
                    );
                }
                return out;
            }
        }
    } else {
        None
    };
    for (tomb, path) in archives {
        let raw = match blobs
            .as_mut()
            .map(|it| it.next().expect("batch preserves input length"))
        {
            Some(Err(err)) => Err(TombstoneArchiveError::ExternalGitDrift(err.to_string())),
            Some(Ok(Some(raw))) => Ok(raw),
            Some(Ok(None)) if tomb.closed_commit.is_some() => {
                Err(TombstoneArchiveError::NotInClosedCommit {
                    path: path.to_owned(),
                    commit: tomb.closed_commit.clone().unwrap(),
                })
            }
            _ => read_confined_utf8(ws, path).map_err(|err| TombstoneArchiveError::Unreadable {
                path: path.to_owned(),
                err: err.to_string(),
            }),
        };
        let parsed = raw.and_then(|raw| {
            parse_spec_at_relpath(&raw, path, collections)
                .map(|(_, doc)| doc)
                .map_err(|err| TombstoneArchiveError::Invalid {
                    path: path.to_owned(),
                    err: err.to_string(),
                })
        });
        out.insert(tomb.run_id.clone(), parsed);
    }
    out
}

pub(crate) fn load_dep_snapshot(
    ws: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    reader: &dyn StateReader,
) -> Result<(DepSnapshot, Vec<String>), AcddError> {
    let (archives, warnings) = collect_archived_specs(ws, collections, false, None)?;
    let runs = list_all_runs(reader, None)?;
    let tombstones = list_all_tombstones(reader)?;
    let task_statuses = load_task_status_map(reader)?;
    let tombstone_archives = load_tombstone_archives(ws, collections, &tombstones);
    let archived_specs = archives
        .into_iter()
        .map(|(_, path, doc)| (doc.id.clone(), (path, doc)))
        .collect();
    Ok((
        DepSnapshot {
            workspace: ws.to_path_buf(),
            runs,
            task_statuses,
            tombstones,
            tombstone_archives,
            archived_specs,
        },
        warnings,
    ))
}
