//! Single-pass dependency snapshot preloading and workspace/archive spec scanning.

use crate::git_ops::snapshot::{list_git_tree_entries_multi, resolve_git_tree_oid};
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunRecord, RunTombstoneRecord, TerminalStatus};
use crate::usecases::lifecycle::{list_all_runs, list_all_tombstones, read_confined_utf8};
use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_bytes;
use acdd_core::fs::checked_child;
use acdd_core::git::read_git_blobs_at_paths_batch;
use acdd_core::git::read_git_blobs_utf8_batch;
use acdd_core::ids::{CommitOid, RunId};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) type CollectedArchiveScan = (Vec<(String, String, SpecDocument)>, Vec<String>);

pub(crate) enum TombstoneArchiveError {
    ExternalGitDrift(String),
    NotInClosedCommit { path: String, commit: CommitOid },
    Unreadable { path: String, err: String },
    Invalid { path: String, err: String },
}

impl TombstoneArchiveError {
    pub(super) fn to_state_error(&self, dep_label: &str) -> StateError {
        match self {
            Self::ExternalGitDrift(err) => {
                StateError::new(ErrorCode::ExternalGitDrift, err.clone())
            }
            Self::NotInClosedCommit { path, commit } => StateError::new(
                ErrorCode::GateVerificationFailed,
                format!(
                    "spec dependency '{dep_label}' archive '{path}' not found in closed commit '{commit}'"
                ),
            ),
            Self::Unreadable { path, err } => StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("spec dependency '{dep_label}' archive '{path}' is unreadable: {err}"),
            ),
            Self::Invalid { path, err } => StateError::new(
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

pub(crate) struct CollectedWorkspaceSpec {
    pub rel_path: String,
    pub collection: Option<SpecCollectionConfig>,
    pub doc: SpecDocument,
}

pub(crate) struct WorkspaceSpecScanError {
    pub code: ErrorCode,
    pub diag_code: &'static str,
    pub path: String,
    pub message: String,
}

pub(crate) struct WorkspaceSpecScan {
    pub specs: Vec<CollectedWorkspaceSpec>,
    pub errors: Vec<WorkspaceSpecScanError>,
}

pub(super) fn is_git_repo(ws_path: &Path) -> bool {
    ws_path.join(".git").exists() || acdd_core::git::git_common_dir(ws_path).is_ok()
}

pub(super) fn is_completed_archive_doc(doc: &SpecDocument) -> bool {
    doc.status == SpecStatus::Completed
        && doc
            .closure
            .as_ref()
            .is_none_or(|c| c.status == SpecStatus::Completed)
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
) -> Result<BTreeMap<(RunId, String), bool>, StateError> {
    Ok(reader
        .load_task_statuses()?
        .into_iter()
        .map(|(run, node, status)| ((run, node), status == NodeStatus::Completed))
        .collect())
}

pub(crate) fn load_tombstone_archives(
    ws: &Path,
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
            parse_spec(&raw).map_err(|err| TombstoneArchiveError::Invalid {
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
    collections: &BTreeMap<String, SpecCollectionConfig>,
    reader: &dyn StateReader,
) -> Result<(DepSnapshot, Vec<String>), StateError> {
    let (archives, warnings) = collect_archived_specs(ws, collections, false, None)?;
    let runs = list_all_runs(reader, None)?;
    let tombstones = list_all_tombstones(reader)?;
    let task_statuses = load_task_status_map(reader)?;
    let tombstone_archives = load_tombstone_archives(ws, &tombstones);
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

fn literal_git_prefix(pat: &str) -> String {
    let mut prefix_parts = Vec::new();
    for seg in pat.split('/') {
        if seg.contains('*') || seg.contains('?') || seg.contains('{') || seg.contains('[') {
            break;
        }
        prefix_parts.push(seg);
    }
    prefix_parts.join("/")
}

fn probe_disk_archive_dirs(
    ws_path: &Path,
    col: &SpecCollectionConfig,
    warnings: &mut Vec<String>,
) -> Result<Vec<(String, PathBuf)>, StateError> {
    let resolved_dirs = col.resolve_dirs(ws_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec collection failed confinement check: {err}"),
        )
    })?;
    let mut out = Vec::new();
    for dir in resolved_dirs {
        let Ok(rel_dir) = dir.strip_prefix(ws_path) else {
            continue;
        };
        let arch_rel_join = rel_dir.join("archive");
        match fs::symlink_metadata(ws_path.join(&arch_rel_join)) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!(
                    "{}: unable to stat archive directory: {err}",
                    arch_rel_join.display()
                ));
                continue;
            }
            Ok(_) => {}
        }
        let arch_dir = checked_child(ws_path, &arch_rel_join).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "archive directory '{}' failed confinement check: {err}",
                    arch_rel_join.display()
                ),
            )
        })?;
        if !arch_dir.is_dir() {
            continue;
        }
        let read_dir = match fs::read_dir(&arch_dir) {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!(
                    "{}: unable to read archive directory: {err}",
                    arch_rel_join.display()
                ));
                continue;
            }
        };
        let mut entries = Vec::new();
        for item in read_dir {
            let entry = item.map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "{}: unable to read archive entry: {err}",
                        arch_rel_join.display()
                    ),
                )
            })?;
            entries.push(entry);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            let ft = entry.file_type().map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("archive entry '{}' file_type failed: {err}", p.display()),
                )
            })?;
            if ft.is_symlink() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("archive entry '{}' must not be a symlink", p.display()),
                ));
            }
            let Ok(rel_native) = p.strip_prefix(ws_path) else {
                continue;
            };
            let checked = checked_child(ws_path, rel_native).map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "archive entry '{}' failed confinement check: {err}",
                        rel_native.display()
                    ),
                )
            })?;
            let Some(rel_utf8) = rel_native.to_str() else {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("archive entry '{}' is not valid UTF-8", rel_native.display()),
                ));
            };
            let rel_posix = rel_utf8.replace('\\', "/");
            if !col.is_direct_archive_file(&rel_posix) {
                continue;
            }
            out.push((rel_posix, checked));
        }
    }
    Ok(out)
}

fn is_admitted_git_collection_root(
    col: &SpecCollectionConfig,
    root_bytes: &[u8],
) -> Result<bool, StateError> {
    if root_bytes.is_empty()
        || root_bytes
            .split(|&b| b == b'/')
            .any(|seg| seg.is_empty() || seg == b"." || seg == b".." || seg.contains(&b'\\'))
    {
        return Ok(false);
    }
    match std::str::from_utf8(root_bytes) {
        Ok(root_str) => Ok(col.is_direct_archive_file(&format!("{root_str}/archive/_probe.md"))),
        Err(_) => {
            let lossy_root = String::from_utf8_lossy(root_bytes);
            if col.is_direct_archive_file(&format!("{lossy_root}/archive/_probe.md")) {
                Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "committed Git archive directory '{lossy_root}/archive' is not valid UTF-8"
                    ),
                ))
            } else {
                Ok(false)
            }
        }
    }
}

pub(super) fn visit_archived_specs<T>(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    disk_only: bool,
    head_oid: Option<&str>,
    warnings: &mut Vec<String>,
    mut visitor: impl FnMut(&str, String, &str) -> Option<T>,
) -> Result<Option<T>, StateError> {
    let in_git = !disk_only && (head_oid.is_some() || is_git_repo(ws_path));
    let all_git_entries = if in_git {
        let tree_oid = match head_oid {
            Some(oid) => oid.to_owned(),
            None => resolve_git_tree_oid(ws_path, "HEAD")?,
        };
        let mut prefixes = Vec::new();
        for col in collections.values() {
            for pat in col.patterns() {
                let is_literal = !pat.contains(['*', '?', '{', '[']);
                let tree_prefix = if is_literal {
                    pat.to_owned()
                } else {
                    literal_git_prefix(pat)
                };
                if !prefixes.contains(&tree_prefix) {
                    prefixes.push(tree_prefix);
                }
            }
        }
        let prefix_refs: Vec<&str> = prefixes.iter().map(String::as_str).collect();
        Some(list_git_tree_entries_multi(ws_path, &tree_oid, &prefix_refs)?)
    } else {
        None
    };
    let mut seen_paths = BTreeSet::new();
    let mut git_entries = Vec::new();
    for (col_name, col) in collections {
        let disk_entries = probe_disk_archive_dirs(ws_path, col, warnings)?;
        if let Some(tree_entries) = all_git_entries.as_deref() {
            for entry in tree_entries {
                let Some(slash_pos) = entry.raw_path.iter().rposition(|&b| b == b'/') else {
                    continue;
                };
                let (parent_bytes, file_bytes) = (
                    &entry.raw_path[..slash_pos],
                    &entry.raw_path[slash_pos + 1..],
                );
                if file_bytes == b"archive" {
                    if is_admitted_git_collection_root(col, parent_bytes)?
                        && entry.mode == "120000"
                    {
                        let raw_lossy = String::from_utf8_lossy(&entry.raw_path);
                        return Err(StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!(
                                "archive directory '{raw_lossy}' in Git tree must not be a symlink"
                            ),
                        ));
                    }
                    continue;
                }
                if !parent_bytes.ends_with(b"/archive") {
                    continue;
                }
                let root_bytes = &parent_bytes[..parent_bytes.len() - b"/archive".len()];
                if !is_admitted_git_collection_root(col, root_bytes)? {
                    continue;
                }
                let Ok(file_str) = std::str::from_utf8(file_bytes) else {
                    if file_bytes.ends_with(b".md") || entry.mode == "120000" {
                        return Err(StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!(
                                "committed Git archive entry '{}' is not valid UTF-8",
                                String::from_utf8_lossy(&entry.raw_path)
                            ),
                        ));
                    }
                    continue;
                };
                let raw_str = std::str::from_utf8(&entry.raw_path).map_err(|_| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "committed Git archive entry '{}' is not valid UTF-8",
                            String::from_utf8_lossy(&entry.raw_path)
                        ),
                    )
                })?;
                if entry.mode == "120000"
                    && (file_str.trim_end().ends_with(".md")
                        || (file_str.contains('\\') && file_str.ends_with(".md")))
                {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "archive entry '{raw_str}' in Git tree must not be a symlink"
                        ),
                    ));
                }
                if file_str.contains('\\') && file_str.ends_with(".md") {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "archive entry '{raw_str}' in Git tree has invalid or unsupported path characters"
                        ),
                    ));
                }
                if !col.is_direct_archive_file(raw_str) {
                    continue;
                }
                if !seen_paths.insert(raw_str.to_owned()) {
                    continue;
                }
                git_entries.push((col_name.clone(), raw_str.to_owned(), entry.oid.clone()));
            }
            continue;
        }

        for (rel_posix, _) in disk_entries {
            if !seen_paths.insert(rel_posix.clone()) {
                continue;
            }
            let raw = read_confined_utf8(ws_path, &rel_posix)?;
            if let Some(res) = visitor(col_name, rel_posix, &raw) {
                return Ok(Some(res));
            }
        }
    }
    if !git_entries.is_empty() {
        let oids: Vec<_> = git_entries.iter().map(|(_, _, oid)| oid.as_str()).collect();
        let blobs = read_git_blobs_utf8_batch(ws_path, &oids)
            .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
        for ((col, path, oid), blob) in git_entries.into_iter().zip(blobs) {
            match blob {
                Ok(Some(raw)) => {
                    if let Some(res) = visitor(&col, path, &raw) {
                        return Ok(Some(res));
                    }
                }
                Ok(None) => warnings.push(format!(
                    "enumerated committed archive '{path}' blob '{oid}' disappeared from HEAD"
                )),
                Err(err) => warnings.push(format!("{path}: failed reading git blob: {err}")),
            }
        }
    }
    Ok(None)
}

/// Collect all archived spec documents across `collections` (from committed `HEAD` in Git, or disk when `disk_only` / non-Git),
/// returning `(parsed_specs, io_warnings)` while ignoring unparseable legacy frontmatter in archive files.
pub(crate) fn collect_archived_specs(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    disk_only: bool,
    head_oid: Option<&str>,
) -> Result<CollectedArchiveScan, StateError> {
    let mut out = Vec::new();
    let mut io_warnings = Vec::new();
    visit_archived_specs(
        ws_path,
        collections,
        disk_only,
        head_oid,
        &mut io_warnings,
        |col_name, rel_posix, raw| {
            if let Ok(doc) = parse_spec(raw) {
                out.push((col_name.to_owned(), rel_posix, doc));
            }
            None::<()>
        },
    )?;
    Ok((out, io_warnings))
}

pub(crate) fn collect_workspace_specs(
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
) -> WorkspaceSpecScan {
    collect_workspace_specs_cached(root, collections, &mut BTreeMap::new())
}

pub(crate) fn collect_workspace_specs_cached(
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, StateError>>,
) -> WorkspaceSpecScan {
    fn walk(
        root: &Path,
        dir: &Path,
        col: &SpecCollectionConfig,
        found: &mut BTreeMap<String, CollectedWorkspaceSpec>,
        errors: &mut Vec<WorkspaceSpecScanError>,
        parsed: &mut BTreeMap<[u8; 32], Result<SpecDocument, StateError>>,
    ) {
        let entries_iter = match fs::read_dir(dir) {
            Ok(iter) => iter,
            Err(err) => {
                let rel_str = dir
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| dir.to_string_lossy().replace('\\', "/"));
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: "VALIDATION_IO",
                    path: rel_str,
                    message: format!("unable to read spec directory: {err}"),
                });
                return;
            }
        };
        let mut entries = Vec::new();
        for item in entries_iter {
            match item {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    let rel_str = dir
                        .strip_prefix(root)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| dir.to_string_lossy().replace('\\', "/"));
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::ProfileSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to read spec directory entry: {err}"),
                    });
                }
            }
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel_str = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(err) => {
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::ProfileSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to inspect spec collection entry: {err}"),
                    });
                    continue;
                }
            };
            if ft.is_symlink() {
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: ErrorCode::ProfileSchemaInvalid.as_str(),
                    path: rel_str.clone(),
                    message: format!("spec collection entry '{rel_str}' must not be a symlink"),
                });
                continue;
            }
            if ft.is_dir() {
                if entry.file_name() != "archive" {
                    walk(root, &path, col, found, errors, parsed);
                }
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !ft.is_file()
                || !name.ends_with(".md")
                || matches!(name.as_str(), "README.md" | "AGENTS.md" | "CLAUDE.md")
            {
                continue;
            }
            match read_confined_utf8(root, &rel_str) {
                Ok(content) => match parsed
                    .entry(sha256_bytes(content.as_bytes()))
                    .or_insert_with(|| parse_spec(&content))
                    .clone()
                {
                    Ok(doc) => {
                        found.insert(
                            rel_str.clone(),
                            CollectedWorkspaceSpec {
                                rel_path: rel_str,
                                collection: Some(col.clone()),
                                doc,
                            },
                        );
                    }
                    Err(err) => {
                        errors.push(WorkspaceSpecScanError {
                            code: err.code,
                            diag_code: err.code.as_str(),
                            path: rel_str,
                            message: err.to_string(),
                        });
                    }
                },
                Err(err) => {
                    errors.push(WorkspaceSpecScanError {
                        code: ErrorCode::ProfileSchemaInvalid,
                        diag_code: "VALIDATION_IO",
                        path: rel_str,
                        message: format!("unable to read spec manifest: {err}"),
                    });
                }
            }
        }
    }

    let mut found = BTreeMap::new();
    let mut errors = Vec::new();
    for col in collections.values() {
        let dirs = match col.resolve_dirs(root) {
            Ok(d) => d,
            Err(err) => {
                let label = col.patterns().join(", ");
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: ErrorCode::ProfileSchemaInvalid.as_str(),
                    path: label.clone(),
                    message: format!("spec collection '{label}' failed confinement check: {err}"),
                });
                continue;
            }
        };
        for dir in dirs {
            walk(root, &dir, col, &mut found, &mut errors, parsed);
        }
    }
    WorkspaceSpecScan {
        specs: found.into_values().collect(),
        errors,
    }
}
