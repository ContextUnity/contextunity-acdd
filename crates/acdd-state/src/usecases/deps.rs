//! Single canonical external, cross-spec, and archive dependency resolver (`INV-SINGLE-OWNER`).

use crate::git_ops::snapshot::{list_git_tree_entries_multi, resolve_git_tree_oid};
use crate::machine::MachineTaskDep;
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunGraphSnapshot, RunRecord, RunTombstoneRecord, TerminalStatus};
use crate::usecases::lifecycle::{
    effective_spec_collections, list_all_runs, list_all_tombstones, primary_workspace_root,
    read_confined_utf8,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::{
    load_adapter, resolve_effective_state_db_path, resolve_state_db_path, Adapter,
    SpecCollectionConfig,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_bytes;
use acdd_core::fs::{canonical_root, checked_child};
use acdd_core::git::{
    list_owned_worktrees, read_git_blob_utf8, read_git_blobs_at_paths_batch,
    read_git_blobs_utf8_batch,
};
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// Diagnostic emitted by [`validate_spec_dependencies`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepValidationIssue {
    /// Error code classifying the dependency violation.
    pub code: ErrorCode,
    /// Human-readable description of the violation.
    pub message: String,
}

impl DepValidationIssue {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Parsed syntactic form of a `depends_on` entry (`INV-SINGLE-OWNER`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedDepSyntax {
    LocalTask(String),
    LocalSpec {
        spec_id: String,
        task_ref: Option<String>,
    },
    LinkedSpec {
        ws_alias: String,
        spec_id: String,
        task_ref: Option<String>,
    },
}

/// Parse raw `depends_on` syntax into [`ParsedDepSyntax`].
pub(crate) fn parse_dep_syntax(
    raw: &str,
    local_task_refs: &BTreeSet<&str>,
) -> Result<ParsedDepSyntax, StateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "depends_on entry cannot be empty",
        ));
    }
    if let Some((left, task_part)) = trimmed.split_once(':') {
        let tref = TaskRef::new(task_part)?;
        if let Some((ws_alias, run_part)) = left.split_once('/') {
            Ok(ParsedDepSyntax::LinkedSpec {
                ws_alias: ws_alias.to_owned(),
                spec_id: run_part.to_owned(),
                task_ref: Some(tref.as_str().to_owned()),
            })
        } else {
            Ok(ParsedDepSyntax::LocalSpec {
                spec_id: left.to_owned(),
                task_ref: Some(tref.as_str().to_owned()),
            })
        }
    } else if let Some((ws_alias, spec_part)) = trimmed.split_once('/') {
        Ok(ParsedDepSyntax::LinkedSpec {
            ws_alias: ws_alias.to_owned(),
            spec_id: spec_part.to_owned(),
            task_ref: None,
        })
    } else if local_task_refs.contains(trimmed) {
        Ok(ParsedDepSyntax::LocalTask(trimmed.to_owned()))
    } else {
        Ok(ParsedDepSyntax::LocalSpec {
            spec_id: trimmed.to_owned(),
            task_ref: None,
        })
    }
}

/// Parse a `TaskSpec.depends_on` entry (`<task_ref>`, `<spec_id>`, `<ws>/<spec_id>`, `<spec_id>:<task_ref>`, or `<ws>/<spec_id>:<task_ref>`).
pub fn parse_task_dependency(
    raw: &str,
    current_workspace: &str,
    current_run_id: &RunId,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    local_task_refs: &BTreeSet<&str>,
) -> Result<MachineTaskDep, StateError> {
    parse_task_dependency_with_db(
        raw,
        current_workspace,
        current_run_id,
        linked_workspaces,
        spec_repos,
        local_task_refs,
        None,
    )
}

pub(crate) fn parse_task_dependency_with_db(
    raw: &str,
    current_workspace: &str,
    current_run_id: &RunId,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    local_task_refs: &BTreeSet<&str>,
    local_db: Option<(&Path, &Path)>,
) -> Result<MachineTaskDep, StateError> {
    match parse_dep_syntax(raw, local_task_refs)? {
        ParsedDepSyntax::LocalTask(t_ref) => Ok(MachineTaskDep {
            depends_on_workspace: current_workspace.to_owned(),
            depends_on_run_id: current_run_id.clone(),
            depends_on_ref: TaskRef::new(&t_ref)?,
        }),
        ParsedDepSyntax::LocalSpec { spec_id, task_ref } => {
            let rid = resolve_external_run_id(Path::new(current_workspace), &spec_id, local_db, false)?;
            let tref = match task_ref {
                Some(t) => TaskRef::new(&t)?,
                None => TaskRef::whole_spec(),
            };
            Ok(MachineTaskDep {
                depends_on_workspace: ".".to_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        }
        ParsedDepSyntax::LinkedSpec {
            ws_alias,
            spec_id,
            task_ref,
        } => {
            let canon_ws = resolve_linked_workspace_root(
                &ws_alias,
                raw.trim(),
                linked_workspaces,
                spec_repos,
            )?;
            let primary = local_db
                .map(|(root, _)| root.to_path_buf())
                .or_else(|| acdd_core::git::primary_worktree_root(Path::new(current_workspace)));
            let scoped = primary.is_some_and(|root| {
                acdd_core::git::primary_worktree_root(&canon_ws).as_ref() == Some(&root)
            });
            let rid = resolve_external_run_id(&canon_ws, &spec_id, local_db, scoped)?;
            let tref = match task_ref {
                Some(t) => TaskRef::new(&t)?,
                None => TaskRef::whole_spec(),
            };
            Ok(MachineTaskDep {
                depends_on_workspace: canon_ws.to_string_lossy().into_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        }
    }
}

pub(crate) fn resolve_linked_workspace_root(
    ws_alias: &str,
    raw_dep: &str,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
) -> Result<PathBuf, StateError> {
    let ws_path = linked_workspaces
        .get(ws_alias)
        .or_else(|| spec_repos.get(ws_alias))
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "dependency '{raw_dep}' references unknown linked workspace '{ws_alias}'"
                ),
            )
        })?;
    canonical_root(ws_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "linked workspace '{ws_alias}' path '{}' is invalid or confined-check failed: {err}",
                ws_path.display()
            ),
        )
    })
}

pub(crate) fn load_ws_adapter(ws_root: &Path) -> Result<Adapter, StateError> {
    load_adapter(ws_root, None).map_err(|err| {
        if let Some(se) = err.get_ref().and_then(|inner| inner.downcast_ref::<StateError>()) {
            se.clone()
        } else {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to load acdd.yaml in '{}': {err}", ws_root.display()),
            )
        }
    })
}

/// Resolve the explicit or owning-primary state database without guessing on invalid configuration.
pub fn try_resolve_effective_state_db(
    root: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf, StateError> {
    let primary = acdd_core::git::primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    if let Some(path) = acdd_core::config::resolve_custom_db_path(&primary, explicit) {
        return Ok(path);
    }
    Ok(resolve_state_db_path(&primary, &load_ws_adapter(&primary)?))
}

pub(crate) fn same_repository(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    acdd_core::git::primary_worktree_root(left)
        .zip(acdd_core::git::primary_worktree_root(right))
        .is_some_and(|(a, b)| a == b)
}

pub(crate) fn resolve_ws_context_canon(
    ws_path: &Path,
    local_db: Option<(&Path, &Path)>,
) -> Result<(PathBuf, BTreeMap<String, SpecCollectionConfig>), StateError> {
    resolve_ws_context_canon_with_primary(ws_path, local_db, None)
}

fn resolve_ws_context_canon_with_primary(
    ws_path: &Path,
    local_db: Option<(&Path, &Path)>,
    known_primary: Option<&Path>,
) -> Result<(PathBuf, BTreeMap<String, SpecCollectionConfig>), StateError> {
    let repo_primary = known_primary.map_or_else(
        || acdd_core::git::primary_worktree_root(ws_path).unwrap_or_else(|| ws_path.to_path_buf()),
        Path::to_path_buf,
    );
    let adapter = load_ws_adapter(ws_path)?;
    let db_path = match local_db {
        Some((local_root, db)) if repo_primary == local_root => db.to_path_buf(),
        _ => resolve_effective_state_db_path(&repo_primary, None, &adapter),
    };
    Ok((db_path, adapter.specs))
}

pub(crate) fn effective_active_run_config<'a>(
    config: &'a StateEngineConfig,
    run: &RunRecord,
    primary: &Path,
    owned: Option<&[PathBuf]>,
) -> Result<Cow<'a, StateEngineConfig>, StateError> {
    let checkout = run
        .worktree
        .as_deref()
        .and_then(|path| canonical_root(Path::new(path)).ok())
        .filter(|path| canonical_root(&config.worktree).ok().as_ref() != Some(path))
        .filter(|path| match owned {
            Some(list) => list.contains(path),
            None => list_owned_worktrees(primary).is_ok_and(|list| list.contains(path)),
        });
    let Some(checkout) = checkout else {
        return Ok(Cow::Borrowed(config));
    };
    let adapter = load_ws_adapter(&checkout)?;
    let mut effective = config.clone();
    effective.state_db_override = Some(config.resolved_state_db_path().0);
    effective.worktree = checkout;
    effective.spec_collections = adapter.specs;
    effective.linked_workspaces = adapter
        .linked_workspaces
        .into_iter()
        .map(|linked| (linked.name, linked.path))
        .collect();
    Ok(Cow::Owned(effective))
}

fn resolve_observation_context(
    ws_path: &Path,
    primary_root: &Path,
    config: &StateEngineConfig,
    owned: Option<&[PathBuf]>,
) -> Result<(PathBuf, PathBuf, BTreeMap<String, SpecCollectionConfig>), StateError> {
    let checkout = canonical_root(&config.worktree).unwrap_or_else(|_| config.worktree.clone());
    let db_path = config.resolved_state_db_path().0;
    if ws_path == checkout {
        return Ok((checkout, db_path, effective_spec_collections(config)));
    }
    let known_primary = owned
        .filter(|list| list.iter().any(|p| p == ws_path))
        .map(|_| primary_root);
    let (db_path, collections) = resolve_ws_context_canon_with_primary(
        ws_path,
        Some((primary_root, &db_path)),
        known_primary,
    )?;
    Ok((ws_path.to_path_buf(), db_path, collections))
}

fn find_latest_matching_tombstone<'a>(
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

fn tombstone_superseded_by_cleared(
    tombs: &[RunTombstoneRecord],
    tomb: &RunTombstoneRecord,
) -> bool {
    let Some(ref spec_id) = tomb.spec_id else {
        return false;
    };
    find_latest_matching_tombstone(tombs, spec_id)
        .is_some_and(|latest| latest.terminal_status == TerminalStatus::Cleared)
}

pub(crate) fn run_in_checkout(run: &RunRecord, checkout: &Path) -> bool {
    run.worktree
        .as_deref()
        .and_then(|path| canonical_root(Path::new(path)).ok())
        .is_some_and(|path| path == checkout)
}

pub(crate) fn run_matches_selector(run: &RunRecord, selector: &str) -> bool {
    run.run_id.as_str() == selector
        || run.run_name.as_str() == selector
        || run.spec_id.as_deref() == Some(selector)
}

/// Persisted run id, or that run's spec id when the run lies outside an explicit checkout.
pub(crate) fn checkout_scoped_target_selector<'a>(
    persisted: Option<&'a RunRecord>,
    persisted_id: &'a str,
    checkout: Option<&Path>,
) -> &'a str {
    persisted
        .filter(|run| checkout.is_some_and(|checkout| !run_in_checkout(run, checkout)))
        .and_then(|run| run.spec_id.as_deref())
        .unwrap_or(persisted_id)
}

fn resolve_external_run_id(
    ws_root: &Path,
    run_sel: &str,
    local_db: Option<(&Path, &Path)>,
    scoped: bool,
) -> Result<RunId, StateError> {
    let db_path = if let Some((_, db)) = local_db.filter(|(root, _)| *root == ws_root) {
        db.to_path_buf()
    } else {
        resolve_ws_context_canon(ws_root, local_db)?.0
    };
    let reader = SqliteStateReader::new(db_path);
    let runs = list_all_runs(&reader, None)?;
    if let Some(found) = runs
        .into_iter()
        .find(|r| (!scoped || run_in_checkout(r, ws_root)) && run_matches_selector(r, run_sel))
    {
        return Ok(found.run_id);
    }
    let tombs = list_all_tombstones(&reader)?;
    if let Some(found) = find_latest_matching_tombstone(&tombs, run_sel) {
        return Ok(found.run_id.clone());
    }
    RunId::new(run_sel).map_err(StateError::from)
}

fn is_git_repo(ws_path: &Path) -> bool {
    ws_path.join(".git").exists() || acdd_core::git::git_common_dir(ws_path).is_ok()
}

fn is_completed_archive_doc(doc: &SpecDocument) -> bool {
    doc.status == SpecStatus::Completed
        && doc
            .closure
            .as_ref()
            .is_none_or(|c| c.status == SpecStatus::Completed)
}

fn verify_archived_task_completed(
    ws_path: &Path,
    arch_rel: &str,
    closed_commit: Option<&CommitOid>,
    task_ref: &str,
    dep_label: &str,
) -> Result<(), StateError> {
    let raw = if is_git_repo(ws_path) {
        if let Some(commit_oid) = closed_commit {
            read_git_blob_utf8(ws_path, commit_oid.as_str(), arch_rel)
                .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "spec dependency '{dep_label}' archive '{arch_rel}' not found in closed commit '{commit_oid}'"
                        ),
                    )
                })?
        } else if let Some(head_blob) = read_git_blob_utf8(ws_path, "HEAD", arch_rel)
            .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?
        {
            head_blob
        } else {
            read_confined_utf8(ws_path, arch_rel).map_err(|err| {
                StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("spec dependency '{dep_label}' archive '{arch_rel}' is unreadable: {err}"),
                )
            })?
        }
    } else {
        read_confined_utf8(ws_path, arch_rel).map_err(|err| {
            StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("spec dependency '{dep_label}' archive '{arch_rel}' is unreadable: {err}"),
            )
        })?
    };
    let parsed = parse_spec(&raw).map_err(|err| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("spec dependency '{dep_label}' archive '{arch_rel}' is invalid: {err}"),
        )
    })?;
    let Some(task) = parsed.tasks.iter().find(|t| t.task_ref.as_str() == task_ref) else {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "spec dependency '{dep_label}' does not contain task '{task_ref}' in '{arch_rel}'"
            ),
        ));
    };
    if task.status != SpecStatus::Completed {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("spec dependency '{dep_label}' task '{task_ref}' is not completed"),
        ));
    }
    Ok(())
}

/// Evaluate external cross-run/cross-workspace dependencies via strictly read-only [`SqliteStateReader`].
///
/// Cleared runs (`terminal_status == 'cleared'`) NEVER satisfy dependencies.
pub fn collect_external_satisfied_deps(
    config: &StateEngineConfig,
    graph: &RunGraphSnapshot,
) -> Result<Vec<(String, RunId, TaskRef)>, StateError> {
    collect_external_satisfied_deps_with_owned(config, graph, None)
}

pub(crate) fn collect_external_satisfied_deps_with_owned(
    config: &StateEngineConfig,
    graph: &RunGraphSnapshot,
    owned: Option<&[PathBuf]>,
) -> Result<Vec<(String, RunId, TaskRef)>, StateError> {
    collect_external_satisfied_deps_cached(config, graph, owned, &mut BTreeMap::new())
}

pub(crate) type ExternalDepCache =
    BTreeMap<(PathBuf, String), (PathBuf, SqliteStateReader, BTreeMap<String, SpecCollectionConfig>, DepSnapshot)>;

pub(crate) fn collect_external_satisfied_deps_cached(
    config: &StateEngineConfig,
    graph: &RunGraphSnapshot,
    owned: Option<&[PathBuf]>,
    contexts: &mut ExternalDepCache,
) -> Result<Vec<(String, RunId, TaskRef)>, StateError> {
    let primary = primary_workspace_root(config);
    let owned_buf;
    let owned = match owned {
        Some(cached) => cached,
        None => {
            owned_buf = list_owned_worktrees(&primary).unwrap_or_default();
            &owned_buf
        }
    };
    let config = effective_active_run_config(config, &graph.run, &primary, Some(owned))?;
    let mut satisfied = Vec::new();

    for dep in &graph.dependencies {
        let requested = Path::new(&dep.depends_on_workspace);
        let scoped = requested != Path::new(".") && owned.iter().any(|checkout| checkout == requested);
        let is_satisfied = if dep.satisfied && !scoped {
            true
        } else if dep.depends_on_run_id == graph.run.run_id {
            false
        } else {
            let key = (
                if dep.depends_on_workspace == "." { config.worktree.clone() } else { PathBuf::new() },
                dep.depends_on_workspace.clone(),
            );
            if !contexts.contains_key(&key) {
                let ws_path = if dep.depends_on_workspace == "." {
                    config.worktree.as_path()
                } else {
                    Path::new(&dep.depends_on_workspace)
                };
                let (checkout, db_path, collections) =
                    resolve_observation_context(ws_path, &primary, &config, Some(owned))?;
                let reader = SqliteStateReader::new(db_path);
                let (snapshot, _) = load_dep_snapshot(&checkout, &collections, &reader)?;
                contexts.insert(
                    key.clone(),
                    (checkout, reader, collections, snapshot),
                );
            }
            let (checkout, reader, collections, snapshot) = &contexts[&key];
            let preloaded = snapshot.context();

            is_external_task_satisfied(
                reader,
                checkout,
                collections,
                &dep.depends_on_run_id,
                &dep.depends_on_ref,
                scoped.then_some(checkout.as_path()),
                &preloaded,
            )?
        };
        if is_satisfied {
            satisfied.push((
                dep.depends_on_workspace.clone(),
                dep.depends_on_run_id.clone(),
                dep.depends_on_ref.clone(),
            ));
        }
    }

    Ok(satisfied)
}

#[allow(dead_code)]
pub(crate) enum DepTargetResolution {
    ActiveSpec {
        cancelled: bool,
        task_exists: bool,
    },
    ActiveRun {
        run_id: RunId,
        spec_id: Option<String>,
        worktree: Option<String>,
        task_exists: bool,
        task_completed: bool,
    },
    Tombstone {
        cleared: bool,
        task_outcome: Result<(), StateError>,
    },
    ArchivedSpec {
        rel_path: String,
        completed_doc: bool,
        task_exists: bool,
        task_completed: bool,
    },
    NotFound,
}

fn match_archived_spec_content(
    raw: &str,
    rel_posix: String,
    run_sel: &str,
    task_sel: Option<&str>,
) -> Option<DepTargetResolution> {
    let parsed = parse_spec(raw).ok()?;
    if parsed.id != run_sel {
        return None;
    }
    Some(archived_resolution(&parsed, rel_posix, task_sel))
}

fn archived_resolution(
    parsed: &SpecDocument,
    rel_posix: String,
    task_sel: Option<&str>,
) -> DepTargetResolution {
    let completed_doc = is_completed_archive_doc(parsed);
    let (task_exists, task_completed) = match task_sel {
        Some(t_ref) => match parsed.tasks.iter().find(|t| t.task_ref.as_str() == t_ref) {
            Some(t) => (true, t.status == SpecStatus::Completed),
            None => (false, false),
        },
        None => (true, true),
    };
    DepTargetResolution::ArchivedSpec {
        rel_path: rel_posix,
        completed_doc,
        task_exists,
        task_completed,
    }
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

fn visit_archived_specs<T>(
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

pub(crate) type CollectedArchiveScan = (Vec<(String, String, SpecDocument)>, Vec<String>);

pub(crate) enum TombstoneArchiveError {
    ExternalGitDrift(String),
    NotInClosedCommit { path: String, commit: CommitOid },
    Unreadable { path: String, err: String },
    Invalid { path: String, err: String },
}

impl TombstoneArchiveError {
    fn to_state_error(&self, dep_label: &str) -> StateError {
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

fn archived_task_outcome(
    doc: &SpecDocument,
    path: &str,
    task: &str,
    label: &str,
) -> Result<(), StateError> {
    match doc.tasks.iter().find(|t| t.task_ref.as_str() == task) {
        None => Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("spec dependency '{label}' does not contain task '{task}' in '{path}'"),
        )),
        Some(t) if t.status != SpecStatus::Completed => Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("spec dependency '{label}' task '{task}' is not completed"),
        )),
        Some(_) => Ok(()),
    }
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_dep_target(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    reader_opt: Option<&dyn StateReader>,
    active_specs: Option<&[SpecDocument]>,
    run_sel: &str,
    task_sel: Option<&str>,
    dep_label: &str,
    run_checkout: Option<&Path>,
    preloaded: Option<&PreloadedDepContext<'_>>,
) -> Result<DepTargetResolution, StateError> {
    if let Some(reader) = reader_opt {
        let owned_runs;
        let runs = if let Some(ctx) = preloaded {
            ctx.runs
        } else {
            owned_runs = list_all_runs(reader, None)?;
            &owned_runs
        };
        let run = runs.iter().find(|r| {
            run_checkout.is_none_or(|checkout| run_in_checkout(r, checkout))
                && run_matches_selector(r, run_sel)
        });
        if let Some(run) = run {
            let (task_exists, task_completed) = match task_sel {
                Some(t_ref) => {
                    if let Some(ctx) = preloaded {
                        ctx.task_statuses
                            .get(&(run.run_id.clone(), format!("task:{t_ref}")))
                            .map_or((false, false), |completed| (true, *completed))
                    } else {
                        match reader.load_node(&run.run_id, &format!("task:{t_ref}"))? {
                            Some(node) => (true, node.status == NodeStatus::Completed),
                            None => (false, false),
                        }
                    }
                }
                None => (true, false),
            };
            return Ok(DepTargetResolution::ActiveRun {
                run_id: run.run_id.clone(),
                spec_id: run.spec_id.clone(),
                worktree: run.worktree.clone(),
                task_exists,
                task_completed,
            });
        }
    }

    if let Some(target_doc) = active_specs.and_then(|specs| specs.iter().find(|s| s.id == run_sel))
    {
        let cancelled = target_doc.status == SpecStatus::Cancelled;
        let task_exists = task_sel.is_none_or(|t_ref| {
            target_doc
                .tasks
                .iter()
                .any(|t| t.task_ref.as_str() == t_ref)
        });
        return Ok(DepTargetResolution::ActiveSpec {
            cancelled,
            task_exists,
        });
    }

    if let Some(reader) = reader_opt {
        let owned_tombs;
        let tombs = if let Some(ctx) = preloaded {
            ctx.tombstones
        } else {
            owned_tombs = list_all_tombstones(reader)?;
            &owned_tombs
        };
        if let Some(latest) = find_latest_matching_tombstone(tombs, run_sel) {
            let cleared = latest.terminal_status != TerminalStatus::Completed
                || tombstone_superseded_by_cleared(tombs, latest);
            let task_outcome = if cleared {
                Ok(())
            } else if let Some(t_ref) = task_sel {
                if let Some(ref arch_rel) = latest.archive_path {
                    if let Some(ctx) = preloaded {
                        let checkout_doc = latest.closed_commit.is_none().then(|| {
                            ctx.archived_specs
                                .values()
                                .find(|(rel, _)| rel == arch_rel)
                                .map(|(_, doc)| doc)
                        }).flatten();
                        let preloaded_entry = (ws_path == ctx.workspace
                            || latest.closed_commit.is_some())
                            .then(|| ctx.tombstone_archives.get(&latest.run_id))
                            .flatten();
                        if let Some(doc) = checkout_doc {
                            archived_task_outcome(doc, arch_rel, t_ref, dep_label)
                        } else if let Some(entry) = preloaded_entry {
                            match entry {
                                Ok(doc) => archived_task_outcome(doc, arch_rel, t_ref, dep_label),
                                Err(err) => Err(err.to_state_error(dep_label)),
                            }
                        } else {
                            verify_archived_task_completed(
                                ws_path,
                                arch_rel,
                                latest.closed_commit.as_ref(),
                                t_ref,
                                dep_label,
                            )
                        }
                    } else {
                        verify_archived_task_completed(
                            ws_path,
                            arch_rel,
                            latest.closed_commit.as_ref(),
                            t_ref,
                            dep_label,
                        )
                    }
                } else if latest.is_ephemeral && t_ref == "goal" {
                    Ok(())
                } else {
                    Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "dependency '{dep_label}' has no archive to verify task '{t_ref}'"
                        ),
                    ))
                }
            } else {
                Ok(())
            };
            return Ok(DepTargetResolution::Tombstone {
                cleared,
                task_outcome,
            });
        }
    }

    if let Some(ctx) = preloaded {
        return Ok(ctx.archived_specs.get(run_sel).map_or(
            DepTargetResolution::NotFound,
            |(path, doc)| archived_resolution(doc, path.clone(), task_sel),
        ));
    }
    let mut scratch_warnings = Vec::new();
    if let Some(found) = visit_archived_specs(
        ws_path,
        collections,
        false,
        None,
        &mut scratch_warnings,
        |_, rel_posix, raw| match_archived_spec_content(raw, rel_posix, run_sel, task_sel),
    )? {
        return Ok(found);
    }
    if !scratch_warnings.is_empty() {
        let joined = scratch_warnings.join("; ");
        let code = if joined.contains("HEAD") {
            ErrorCode::ExternalGitDrift
        } else {
            ErrorCode::ProfileSchemaInvalid
        };
        return Err(StateError::new(code, joined));
    }

    Ok(DepTargetResolution::NotFound)
}

fn is_external_task_satisfied(
    reader: &dyn StateReader,
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    target_run_id: &RunId,
    target_task_ref: &TaskRef,
    run_checkout: Option<&Path>,
    preloaded: &PreloadedDepContext<'_>,
) -> Result<bool, StateError> {
    let task_sel = if target_task_ref.is_whole_spec() {
        None
    } else {
        Some(target_task_ref.as_str())
    };
    let persisted = preloaded
        .runs
        .iter()
        .find(|r| r.run_id == *target_run_id);
    let target = checkout_scoped_target_selector(persisted, target_run_id.as_str(), run_checkout);
    match resolve_dep_target(
        ws_path,
        collections,
        Some(reader),
        None,
        target,
        task_sel,
        target_run_id.as_str(),
        run_checkout,
        Some(preloaded),
    )? {
        DepTargetResolution::ActiveSpec { .. } | DepTargetResolution::NotFound => Ok(false),
        DepTargetResolution::ActiveRun { task_completed, .. } => Ok(task_completed),
        DepTargetResolution::Tombstone {
            cleared,
            task_outcome,
        } => Ok(!cleared && task_outcome.is_ok()),
        DepTargetResolution::ArchivedSpec {
            completed_doc,
            task_exists,
            task_completed,
            ..
        } => Ok(completed_doc && task_exists && task_completed),
    }
}

/// Verify run-level `SpecDocument.depends_on` entries are satisfied by completed tombstones or committed archives.
pub fn verify_run_level_depends_on(
    config: &StateEngineConfig,
    local_reader: &dyn StateReader,
    doc: &SpecDocument,
) -> Result<(), StateError> {
    if doc.depends_on.is_empty() {
        return Ok(());
    }
    let primary = primary_workspace_root(config);
    let source = &config.worktree;
    let spec_repos = resolve_spec_repositories(source, doc);

    for dep_raw in &doc.depends_on {
        let trimmed = dep_raw.trim();
        if trimmed.is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "spec depends_on entry cannot be empty",
            ));
        }
        let (ws_path, ws_collections, run_sel, reader_owned): (
            PathBuf,
            BTreeMap<String, SpecCollectionConfig>,
            &str,
            Option<SqliteStateReader>,
        ) = if let Some((ws_alias, rest)) = trimmed.split_once('/') {
            let canon = resolve_linked_workspace_root(
                ws_alias,
                trimmed,
                &config.linked_workspaces,
                &spec_repos,
            )?;
            if same_repository(&primary, &canon) {
                let collections = if canonical_root(source).ok().as_ref() == Some(&canon) {
                    effective_spec_collections(config)
                } else {
                    load_ws_adapter(&canon)?.specs
                };
                (canon, collections, rest, None)
            } else {
                let (checkout, db_path, collections) =
                    resolve_observation_context(&canon, &primary, config, None)?;
                (
                    checkout,
                    collections,
                    rest,
                    Some(SqliteStateReader::new(db_path)),
                )
            }
        } else {
            (
                config.worktree.clone(),
                effective_spec_collections(config),
                trimmed,
                None,
            )
        };

        let reader: &dyn StateReader = match reader_owned {
            Some(ref r) => r,
            None => local_reader,
        };

        let (run_sel, task_sel) = match run_sel.split_once(':') {
            Some((r, t)) => {
                let _ = TaskRef::new(t)?;
                (r, Some(t))
            }
            None => (run_sel, None),
        };

        match resolve_dep_target(
            &ws_path,
            &ws_collections,
            Some(reader),
            None,
            run_sel,
            task_sel,
            trimmed,
            (trimmed.contains('/')
                && acdd_core::git::primary_worktree_root(&ws_path).as_deref() == Some(&primary))
            .then_some(ws_path.as_path()),
            None,
        )? {
            DepTargetResolution::Tombstone {
                cleared: false,
                task_outcome,
            } => {
                task_outcome?;
            }
            DepTargetResolution::Tombstone { cleared: true, .. } => {
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!(
                        "spec dependency '{trimmed}' was cleared (terminal_status='cleared') and cannot satisfy depends_on"
                    ),
                ));
            }
            DepTargetResolution::ArchivedSpec {
                rel_path,
                completed_doc,
                task_exists,
                task_completed,
            } => {
                if !completed_doc {
                    return Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "spec dependency '{trimmed}' targets non-completed archive '{rel_path}'"
                        ),
                    ));
                }
                if let Some(t_ref) = task_sel {
                    if !task_exists {
                        return Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!(
                                "spec dependency '{trimmed}' does not contain task '{t_ref}' in '{rel_path}'"
                            ),
                        ));
                    }
                    if !task_completed {
                        return Err(StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!("spec dependency '{trimmed}' task '{t_ref}' is not completed"),
                        ));
                    }
                }
            }
            DepTargetResolution::ActiveSpec { .. }
            | DepTargetResolution::ActiveRun { .. }
            | DepTargetResolution::NotFound => {
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("spec dependency '{trimmed}' is not completed"),
                ));
            }
        }
    }

    Ok(())
}

/// Validate intra-spec task DAG (`unknown local task_ref`, self-dependency, and cycles)
/// plus external task and spec-level `depends_on` references for `validate` and `validate_spec_for_run`.
pub fn validate_spec_dependencies(
    root: &Path,
    adapter: &Adapter,
    local_reader: Option<&dyn StateReader>,
    state_db_override: Option<&Path>,
    doc: &SpecDocument,
    active_specs: &[SpecDocument],
) -> Vec<DepValidationIssue> {
    let mut issues = Vec::new();
    let linked_workspaces: BTreeMap<String, PathBuf> = adapter
        .linked_workspaces
        .iter()
        .map(|lw| (lw.name.clone(), lw.path.clone()))
        .collect();
    let spec_repos = resolve_spec_repositories(root, doc);
    let local_checkout = canonical_root(root).unwrap_or_else(|_| root.to_path_buf());
    let local_primary = acdd_core::git::primary_worktree_root(&local_checkout)
        .unwrap_or_else(|| local_checkout.clone());
    let local_db = resolve_effective_state_db_path(&local_primary, state_db_override, adapter);
    let mut contexts = ValidationContexts {
        local_checkout,
        local_primary,
        local_db,
        workspaces: BTreeMap::new(),
    };

    // 1. Validate intra-spec task DAG and external task.depends_on entries.
    let task_refs: BTreeSet<&str> = doc.tasks.iter().map(|t| t.task_ref.as_str()).collect();
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in &doc.tasks {
        indegree.insert(t.task_ref.as_str(), 0);
        adj.entry(t.task_ref.as_str()).or_default();
    }

    for t in &doc.tasks {
        for dep_raw in &t.depends_on {
            let trimmed = dep_raw.trim();
            if trimmed.is_empty() {
                issues.push(DepValidationIssue::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task '{}' has an empty depends_on entry", t.task_ref),
                ));
                continue;
            }
            if trimmed.contains(':') || trimmed.contains('/') {
                let require_task = trimmed.contains(':');
                if let Err(err) = validate_external_dep_ref(
                    root,
                    adapter,
                    local_reader,
                    &linked_workspaces,
                    &spec_repos,
                    active_specs,
                    &doc.id,
                    trimmed,
                    require_task,
                    &mut contexts,
                ) {
                    issues.push(DepValidationIssue::new(err.code, err.to_string()));
                }
                continue;
            }
            if let Err(err) = TaskRef::new(trimmed) {
                issues.push(DepValidationIssue::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task '{}' depends_on '{trimmed}' is invalid: {err}", t.task_ref),
                ));
                continue;
            }
            if trimmed == t.task_ref.as_str() {
                issues.push(DepValidationIssue::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task '{}' cannot depend on itself", t.task_ref),
                ));
                continue;
            }
            if !task_refs.contains(trimmed) {
                if let Err(err) = validate_external_dep_ref(
                    root,
                    adapter,
                    local_reader,
                    &linked_workspaces,
                    &spec_repos,
                    active_specs,
                    &doc.id,
                    trimmed,
                    false,
                    &mut contexts,
                ) {
                    issues.push(DepValidationIssue::new(
                        err.code,
                        format!(
                            "task '{}' depends on unknown local task or external spec '{trimmed}': {err}",
                            t.task_ref
                        ),
                    ));
                }
                continue;
            }
            adj.entry(trimmed).or_default().push(t.task_ref.as_str());
            *indegree.entry(t.task_ref.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&k, _)| k)
        .collect();
    let mut visited = 0usize;
    while let Some(cur) = queue.pop_front() {
        visited += 1;
        if let Some(nexts) = adj.get(cur) {
            for &nxt in nexts {
                if let Some(deg) = indegree.get_mut(nxt) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(nxt);
                    }
                }
            }
        }
    }
    if visited != task_refs.len() {
        issues.push(DepValidationIssue::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec '{}' task dependency graph contains a cycle", doc.id),
        ));
    }

    // 2. Validate spec-level `doc.depends_on` entries and cross-spec cycles.
    for dep_raw in &doc.depends_on {
        let trimmed = dep_raw.trim();
        if trimmed.is_empty() {
            issues.push(DepValidationIssue::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("spec '{}' has an empty depends_on entry", doc.id),
            ));
            continue;
        }
        if let Err(err) = validate_external_dep_ref(
            root,
            adapter,
            local_reader,
            &linked_workspaces,
            &spec_repos,
            active_specs,
            &doc.id,
            trimmed,
            false,
            &mut contexts,
        ) {
            issues.push(DepValidationIssue::new(err.code, err.to_string()));
        }
    }

    if has_cross_spec_cycle(root, &linked_workspaces, doc, active_specs) {
        issues.push(DepValidationIssue::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec '{}' depends_on forms a cross-spec dependency cycle", doc.id),
        ));
    }

    issues
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DepGraphNode {
    ws_root: PathBuf,
    spec_id: String,
    task_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DfsColor {
    Visiting,
    Visited,
}

struct WsGraphEntry {
    linked_workspaces: BTreeMap<String, PathBuf>,
    specs: BTreeMap<String, SpecDocument>,
}

pub(crate) fn resolve_spec_repositories(
    primary_root: &Path,
    spec_doc: &SpecDocument,
) -> BTreeMap<String, PathBuf> {
    let base = acdd_core::git::primary_worktree_root(primary_root)
        .filter(|primary| {
            primary != primary_root && {
                let rels: Vec<&Path> = spec_doc
                    .repositories
                    .iter()
                    .map(|r| Path::new(&r.path))
                    .filter(|p| p.is_relative())
                    .collect();
                !rels.is_empty()
                    && rels.iter().all(|p| primary.join(p).exists())
                    && !rels.iter().all(|p| primary_root.join(p).exists())
            }
        })
        .unwrap_or_else(|| primary_root.to_path_buf());
    let mut map = BTreeMap::new();
    for r in &spec_doc.repositories {
        let p = Path::new(&r.path);
        map.insert(
            r.name.clone(),
            if p.is_relative() {
                base.join(p)
            } else {
                p.to_path_buf()
            },
        );
    }
    map
}

fn parse_dep_node(
    current_ws: &Path,
    ws_linked: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    default_spec: &SpecDocument,
    raw: &str,
    allow_bare_task: bool,
) -> Option<DepGraphNode> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (target_ws, target_str, allow_bare) = if let Some((ws_alias, rest)) = trimmed.split_once('/')
    {
        let resolved =
            resolve_linked_workspace_root(ws_alias, trimmed, ws_linked, spec_repos).ok()?;
        let canon = canonical_root(&resolved).unwrap_or(resolved);
        (canon, rest.trim(), false)
    } else {
        (current_ws.to_path_buf(), trimmed, allow_bare_task)
    };
    if target_str.is_empty() {
        return None;
    }
    if let Some((spec_part, task_part)) = target_str.split_once(':') {
        let s = spec_part.trim();
        let t = task_part.trim();
        if s.is_empty() || t.is_empty() {
            return None;
        }
        Some(DepGraphNode {
            ws_root: target_ws,
            spec_id: s.to_owned(),
            task_ref: Some(t.to_owned()),
        })
    } else if allow_bare
        && default_spec
            .tasks
            .iter()
            .any(|t| t.task_ref.as_str() == target_str)
    {
        Some(DepGraphNode {
            ws_root: target_ws,
            spec_id: default_spec.id.clone(),
            task_ref: Some(target_str.to_owned()),
        })
    } else {
        Some(DepGraphNode {
            ws_root: target_ws,
            spec_id: target_str.to_owned(),
            task_ref: None,
        })
    }
}

fn ensure_ws_graph_entry<'a>(
    ws_cache: &'a mut BTreeMap<PathBuf, WsGraphEntry>,
    ws_root: &Path,
) -> Option<&'a WsGraphEntry> {
    if !ws_cache.contains_key(ws_root) {
        let adapter = load_ws_adapter(ws_root).ok()?;
        let linked = adapter
            .linked_workspaces
            .into_iter()
            .map(|lw| (lw.name, lw.path))
            .collect();
        let scan = collect_workspace_specs(ws_root, &adapter.specs);
        let mut specs = BTreeMap::new();
        for s in scan.specs {
            specs.insert(s.doc.id.clone(), s.doc);
        }
        ws_cache.insert(
            ws_root.to_path_buf(),
            WsGraphEntry {
                linked_workspaces: linked,
                specs,
            },
        );
    }
    ws_cache.get(ws_root)
}

fn has_cross_spec_cycle(
    primary_root: &Path,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    doc: &SpecDocument,
    active_specs: &[SpecDocument],
) -> bool {
    let primary_key = canonical_root(primary_root).unwrap_or_else(|_| primary_root.to_path_buf());
    let mut primary_specs = BTreeMap::new();
    for s in active_specs {
        primary_specs.insert(s.id.clone(), s.clone());
    }
    primary_specs.insert(doc.id.clone(), doc.clone());

    let mut ws_cache: BTreeMap<PathBuf, WsGraphEntry> = BTreeMap::new();
    ws_cache.insert(
        primary_key.clone(),
        WsGraphEntry {
            linked_workspaces: linked_workspaces.clone(),
            specs: primary_specs,
        },
    );

    let mut color: BTreeMap<DepGraphNode, DfsColor> = BTreeMap::new();
    let mut stack: Vec<DepGraphNode> = Vec::new();

    fn dfs(
        node: DepGraphNode,
        ws_cache: &mut BTreeMap<PathBuf, WsGraphEntry>,
        color: &mut BTreeMap<DepGraphNode, DfsColor>,
        stack: &mut Vec<DepGraphNode>,
    ) -> bool {
        if let Some(&c) = color.get(&node) {
            if c == DfsColor::Visiting {
                if let Some(pos) = stack.iter().position(|n| n == &node) {
                    let cycle_slice = &stack[pos..];
                    let first = &cycle_slice[0];
                    if cycle_slice
                        .iter()
                        .any(|n| (&n.ws_root, &n.spec_id) != (&first.ws_root, &first.spec_id))
                    {
                        return true;
                    }
                }
            }
            return false;
        }
        color.insert(node.clone(), DfsColor::Visiting);
        stack.push(node.clone());

        let loaded = ensure_ws_graph_entry(ws_cache, &node.ws_root).and_then(|entry| {
            entry
                .specs
                .get(&node.spec_id)
                .cloned()
                .map(|spec_doc| (entry.linked_workspaces.clone(), spec_doc))
        });
        if let Some((ws_linked, spec_doc)) = loaded {
            let repos = resolve_spec_repositories(&node.ws_root, &spec_doc);
            let mut neighbors: Vec<DepGraphNode> = Vec::new();
            for raw in &spec_doc.depends_on {
                if let Some(target) =
                    parse_dep_node(&node.ws_root, &ws_linked, &repos, &spec_doc, raw, false)
                {
                    neighbors.push(target);
                }
            }
            match &node.task_ref {
                None => {
                    for t in &spec_doc.tasks {
                        neighbors.push(DepGraphNode {
                            ws_root: node.ws_root.clone(),
                            spec_id: spec_doc.id.clone(),
                            task_ref: Some(t.task_ref.as_str().to_owned()),
                        });
                    }
                }
                Some(task_ref) => {
                    if let Some(task) = spec_doc
                        .tasks
                        .iter()
                        .find(|t| t.task_ref.as_str() == task_ref)
                    {
                        for raw in &task.depends_on {
                            if let Some(target) = parse_dep_node(
                                &node.ws_root,
                                &ws_linked,
                                &repos,
                                &spec_doc,
                                raw,
                                true,
                            ) {
                                neighbors.push(target);
                            }
                        }
                    }
                }
            }
            for next in neighbors {
                if dfs(next, ws_cache, color, stack) {
                    return true;
                }
            }
        }

        stack.pop();
        color.insert(node, DfsColor::Visited);
        false
    }

    if dfs(
        DepGraphNode {
            ws_root: primary_key.clone(),
            spec_id: doc.id.clone(),
            task_ref: None,
        },
        &mut ws_cache,
        &mut color,
        &mut stack,
    ) {
        return true;
    }
    for t in &doc.tasks {
        if dfs(
            DepGraphNode {
                ws_root: primary_key.clone(),
                spec_id: doc.id.clone(),
                task_ref: Some(t.task_ref.as_str().to_owned()),
            },
            &mut ws_cache,
            &mut color,
            &mut stack,
        ) {
            return true;
        }
    }
    false
}

struct ValidationWorkspace {
    path: PathBuf,
    db: PathBuf,
    collections: BTreeMap<String, SpecCollectionConfig>,
    active: Vec<SpecDocument>,
    snapshot: DepSnapshot,
    checkout_scoped: bool,
}

struct ValidationContexts {
    local_checkout: PathBuf,
    local_primary: PathBuf,
    local_db: PathBuf,
    workspaces: BTreeMap<String, ValidationWorkspace>,
}

#[allow(clippy::too_many_arguments)]
fn validate_external_dep_ref(
    _primary_root: &Path,
    primary_adapter: &Adapter,
    local_reader: Option<&dyn StateReader>,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    primary_active_specs: &[SpecDocument],
    self_spec_id: &str,
    trimmed: &str,
    require_task: bool,
    contexts: &mut ValidationContexts,
) -> Result<(), StateError> {
    let (alias, run_and_task) = trimmed.split_once('/').unwrap_or(("", trimmed));
    if !contexts.workspaces.contains_key(alias) {
        let ws_path = if alias.is_empty() {
            contexts.local_checkout.clone()
        } else {
            resolve_linked_workspace_root(alias, trimmed, linked_workspaces, spec_repos)?
        };
        let is_primary = ws_path == contexts.local_checkout;
        let (db, collections) = if is_primary {
            (contexts.local_db.clone(), primary_adapter.specs.clone())
        } else {
            resolve_ws_context_canon(
                &ws_path,
                Some((&contexts.local_primary, &contexts.local_db)),
            )?
        };
        let active = if is_primary {
            primary_active_specs.to_vec()
        } else {
            let scan = collect_workspace_specs(&ws_path, &collections);
            if let Some(first_err) = scan.errors.into_iter().next() {
                return Err(StateError::new(first_err.code, first_err.message));
            }
            scan.specs.into_iter().map(|s| s.doc).collect()
        };
        let owned_reader = SqliteStateReader::new(&db);
        let reader = local_reader
            .filter(|_| db == contexts.local_db)
            .unwrap_or(&owned_reader);
        let (snapshot, _) = load_dep_snapshot(&ws_path, &collections, reader)?;
        let checkout_scoped = !alias.is_empty()
            && acdd_core::git::primary_worktree_root(&ws_path).as_deref()
                == Some(&contexts.local_primary);
        contexts.workspaces.insert(
            alias.to_owned(),
            ValidationWorkspace {
                path: ws_path,
                db,
                collections,
                active,
                snapshot,
                checkout_scoped,
            },
        );
    }
    let workspace = &contexts.workspaces[alias];
    let ws_path = &workspace.path;
    let is_primary = *ws_path == contexts.local_checkout;

    let (run_sel, task_sel) = match run_and_task.split_once(':') {
        Some((r, t)) => {
            if r.trim().is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("dependency '{trimmed}' is missing target run or spec ID"),
                ));
            }
            let _ = TaskRef::new(t)?;
            (r.trim(), Some(t))
        }
        None => {
            if require_task {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("task dependency '{trimmed}' must specify ':<task_ref>'"),
                ));
            }
            (run_and_task.trim(), None)
        }
    };

    if is_primary && run_sel == self_spec_id {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec '{self_spec_id}' cannot depend on itself via '{trimmed}'"),
        ));
    }

    let owned_reader = SqliteStateReader::new(&workspace.db);
    let reader = local_reader
        .filter(|_| workspace.db == contexts.local_db)
        .unwrap_or(&owned_reader);
    let preloaded = workspace.snapshot.context();

    match resolve_dep_target(
        ws_path,
        &workspace.collections,
        Some(reader),
        Some(&workspace.active),
        run_sel,
        task_sel,
        trimmed,
        workspace.checkout_scoped.then_some(ws_path.as_path()),
        Some(&preloaded),
    )? {
        DepTargetResolution::ActiveSpec {
            cancelled,
            task_exists,
        } => {
            if cancelled {
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("dependency '{trimmed}' targets cancelled spec '{run_sel}'"),
                ));
            }
            if let Some(t_ref) = task_sel {
                if !task_exists {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "dependency '{trimmed}' references unknown task '{t_ref}' in spec '{run_sel}'"
                        ),
                    ));
                }
            }
            Ok(())
        }
        DepTargetResolution::ActiveRun {
            run_id,
            task_exists,
            ..
        } => {
            if let Some(t_ref) = task_sel {
                if !task_exists {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "dependency '{trimmed}' references unknown task '{t_ref}' in active run '{run_id}'"
                        ),
                    ));
                }
            }
            Ok(())
        }
        DepTargetResolution::Tombstone {
            cleared,
            task_outcome,
        } => {
            if cleared {
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!(
                        "dependency '{trimmed}' targets cleared run (terminal_status='cleared')"
                    ),
                ));
            }
            task_outcome
        }
        DepTargetResolution::ArchivedSpec {
            rel_path,
            completed_doc,
            task_exists,
            task_completed,
        } => {
            if !completed_doc {
                return Err(StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!(
                        "dependency '{trimmed}' targets non-completed archive '{rel_path}'"
                    ),
                ));
            }
            if let Some(t_ref) = task_sel {
                if !task_exists {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "dependency '{trimmed}' references unknown task '{t_ref}' in archive '{rel_path}'"
                        ),
                    ));
                }
                if !task_completed {
                    return Err(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("dependency '{trimmed}' task '{t_ref}' is not completed"),
                    ));
                }
            }
            Ok(())
        }
        DepTargetResolution::NotFound => Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("dependency '{trimmed}' target '{run_sel}' was not found"),
        )),
    }
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
