//! Single canonical external, cross-spec, and archive dependency resolver (`INV-SINGLE-OWNER`).

use crate::git_ops::list_git_tree_files;
use acdd_core::git::read_git_blob_utf8;
use crate::machine::MachineTaskDep;
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunTombstoneRecord, TaskDependencyRecord, TerminalStatus};
use crate::usecases::lifecycle::{
    effective_spec_collections, list_all_runs, list_all_tombstones, primary_workspace_root,
    read_confined_utf8, sorted_dir_paths,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::{load_adapter, resolve_state_db_path, Adapter, SpecCollectionConfig};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{canonical_root, checked_child};
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use acdd_core::path_policy::normalize_rel_path;
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

/// Parse a `TaskSpec.depends_on` entry (`<task_ref>`, `<run_id>:<task_ref>`, or `<ws_alias>/<run_id>:<task_ref>`).
pub fn parse_task_dependency(
    raw: &str,
    current_workspace: &str,
    current_run_id: &RunId,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
) -> Result<MachineTaskDep, StateError> {
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
            let canon_ws = resolve_linked_workspace_root(
                ws_alias,
                trimmed,
                linked_workspaces,
                spec_repos,
            )?;
            let rid = resolve_external_run_id(&canon_ws, run_part)?;
            Ok(MachineTaskDep {
                depends_on_workspace: canon_ws.to_string_lossy().into_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        } else {
            let rid = resolve_external_run_id(Path::new(current_workspace), left)?;
            Ok(MachineTaskDep {
                depends_on_workspace: current_workspace.to_owned(),
                depends_on_run_id: rid,
                depends_on_ref: tref,
            })
        }
    } else {
        Ok(MachineTaskDep {
            depends_on_workspace: current_workspace.to_owned(),
            depends_on_run_id: current_run_id.clone(),
            depends_on_ref: TaskRef::new(trimmed)?,
        })
    }
}

fn resolve_linked_workspace_root(
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

fn load_ws_adapter(ws_root: &Path) -> Result<Adapter, StateError> {
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

fn resolve_ws_context(
    ws_path: &Path,
    primary_root: &Path,
    config: &StateEngineConfig,
) -> Result<(PathBuf, BTreeMap<String, SpecCollectionConfig>), StateError> {
    if ws_path == primary_root {
        return Ok((
            config.resolved_state_db_path().0,
            effective_spec_collections(config),
        ));
    }
    let adapter = load_ws_adapter(ws_path)?;
    let db_path = resolve_state_db_path(ws_path, &adapter);
    Ok((db_path, adapter.specs))
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
        .max_by(|a, b| {
            a.closed_at
                .cmp(&b.closed_at)
                .then_with(|| {
                    let a_cleared = a.terminal_status == TerminalStatus::Cleared;
                    let b_cleared = b.terminal_status == TerminalStatus::Cleared;
                    a_cleared.cmp(&b_cleared)
                })
                .then_with(|| a.run_id.as_str().cmp(b.run_id.as_str()))
        })
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

fn resolve_external_run_id(ws_root: &Path, run_sel: &str) -> Result<RunId, StateError> {
    let db_path = if fs::symlink_metadata(ws_root.join("acdd.yaml")).is_ok() {
        let adapter = load_ws_adapter(ws_root)?;
        resolve_state_db_path(ws_root, &adapter)
    } else {
        ws_root.join(".acdd/state.sqlite")
    };
    let reader = SqliteStateReader::new(db_path);
    let runs = list_all_runs(&reader, None)?;
    if let Some(found) = runs.into_iter().find(|r| {
        r.run_id.as_str() == run_sel
            || r.run_name.as_str() == run_sel
            || r.spec_id.as_deref() == Some(run_sel)
    }) {
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
    current_run_id: &RunId,
    deps: &[TaskDependencyRecord],
) -> Result<Vec<(String, RunId, TaskRef)>, StateError> {
    let primary = primary_workspace_root(config);
    let primary_str = primary.to_string_lossy().into_owned();
    let mut satisfied = Vec::new();
    let mut contexts: BTreeMap<String, (SqliteStateReader, BTreeMap<String, SpecCollectionConfig>)> =
        BTreeMap::new();

    for dep in deps {
        if dep.satisfied {
            satisfied.push((
                dep.depends_on_workspace.clone(),
                dep.depends_on_run_id.clone(),
                dep.depends_on_ref.clone(),
            ));
            continue;
        }
        if dep.depends_on_run_id == *current_run_id && dep.depends_on_workspace == primary_str {
            continue;
        }
        let ws_path = PathBuf::from(&dep.depends_on_workspace);
        if !contexts.contains_key(&dep.depends_on_workspace) {
            let (db_path, collections) = resolve_ws_context(&ws_path, &primary, config)?;
            contexts.insert(
                dep.depends_on_workspace.clone(),
                (SqliteStateReader::new(db_path), collections),
            );
        }
        let (reader, collections) = &contexts[&dep.depends_on_workspace];

        if is_external_task_satisfied(
            reader,
            &ws_path,
            collections,
            &dep.depends_on_run_id,
            &dep.depends_on_ref,
        )? {
            satisfied.push((
                dep.depends_on_workspace.clone(),
                dep.depends_on_run_id.clone(),
                dep.depends_on_ref.clone(),
            ));
        }
    }

    Ok(satisfied)
}

enum DepTargetResolution {
    ActiveSpec {
        cancelled: bool,
        task_exists: bool,
    },
    ActiveRun {
        run_id: RunId,
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

#[allow(clippy::too_many_arguments)]
fn resolve_dep_target(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    reader_opt: Option<&dyn StateReader>,
    active_specs: Option<&[SpecDocument]>,
    run_sel: &str,
    task_sel: Option<&str>,
    dep_label: &str,
) -> Result<DepTargetResolution, StateError> {
    if let Some(specs) = active_specs {
        if let Some(target_doc) = specs.iter().find(|s| s.id == run_sel) {
            let cancelled = target_doc.status == SpecStatus::Cancelled;
            let task_exists = task_sel.is_none_or(|t_ref| {
                target_doc.tasks.iter().any(|t| t.task_ref.as_str() == t_ref)
            });
            return Ok(DepTargetResolution::ActiveSpec {
                cancelled,
                task_exists,
            });
        }
    }

    if let Some(reader) = reader_opt {
        let runs = list_all_runs(reader, None)?;
        if let Some(run) = runs.into_iter().find(|r| {
            r.run_id.as_str() == run_sel
                || r.run_name.as_str() == run_sel
                || r.spec_id.as_deref() == Some(run_sel)
        }) {
            let (task_exists, task_completed) = match task_sel {
                Some(t_ref) => match reader.load_node(&run.run_id, &format!("task:{t_ref}"))? {
                    Some(node) => (true, node.status == NodeStatus::Completed),
                    None => (false, false),
                },
                None => (true, false),
            };
            return Ok(DepTargetResolution::ActiveRun {
                run_id: run.run_id,
                task_exists,
                task_completed,
            });
        }

        let tombs = list_all_tombstones(reader)?;
        if let Some(latest) = find_latest_matching_tombstone(&tombs, run_sel) {
            let cleared = latest.terminal_status != TerminalStatus::Completed
                || tombstone_superseded_by_cleared(&tombs, latest);
            let task_outcome = if cleared {
                Ok(())
            } else if let Some(t_ref) = task_sel {
                if let Some(ref arch_rel) = latest.archive_path {
                    verify_archived_task_completed(
                        ws_path,
                        arch_rel,
                        latest.closed_commit.as_ref(),
                        t_ref,
                        dep_label,
                    )
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

    let in_git = is_git_repo(ws_path);
    for col in collections.values() {
        let arch_rel_join = Path::new(&col.path).join("archive");
        let arch_rel_dir = normalize_rel_path(&arch_rel_join.to_string_lossy()).ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("archive directory '{}' is not a normalized relative path", col.path),
            )
        })?;
        if in_git {
            if fs::symlink_metadata(ws_path.join(&arch_rel_dir)).is_ok() {
                let _ = checked_child(ws_path, Path::new(&arch_rel_dir)).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "archive directory '{}' failed confinement check: {err}",
                            col.path
                        ),
                    )
                })?;
            }
            for rel_posix in list_git_tree_files(ws_path, "HEAD", &arch_rel_dir)? {
                if !rel_posix.ends_with(".md") {
                    continue;
                }
                let Some(raw) = read_git_blob_utf8(ws_path, "HEAD", &rel_posix).ok().flatten()
                else {
                    continue;
                };
                let Ok(parsed) = parse_spec(&raw) else {
                    continue;
                };
                if parsed.id == run_sel {
                    let completed_doc = is_completed_archive_doc(&parsed);
                    let (task_exists, task_completed) = match task_sel {
                        Some(t_ref) => {
                            match parsed.tasks.iter().find(|t| t.task_ref.as_str() == t_ref) {
                                Some(t) => (true, t.status == SpecStatus::Completed),
                                None => (false, false),
                            }
                        }
                        None => (true, true),
                    };
                    return Ok(DepTargetResolution::ArchivedSpec {
                        rel_path: rel_posix,
                        completed_doc,
                        task_exists,
                        task_completed,
                    });
                }
            }
            continue;
        }

        let arch_dir = match checked_child(ws_path, &arch_rel_join) {
            Ok(d) if d.is_dir() => d,
            Ok(_) => continue,
            Err(err) => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "archive directory '{}' failed confinement check: {err}",
                        col.path
                    ),
                ));
            }
        };
        for p in sorted_dir_paths(&arch_dir) {
            if p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(rel) = p.strip_prefix(ws_path) else {
                continue;
            };
            let rel_posix = rel.to_string_lossy().replace('\\', "/");
            let Ok(raw) = read_confined_utf8(ws_path, &rel_posix) else {
                continue;
            };
            let Ok(parsed) = parse_spec(&raw) else {
                continue;
            };
            if parsed.id == run_sel {
                let completed_doc = is_completed_archive_doc(&parsed);
                let (task_exists, task_completed) = match task_sel {
                    Some(t_ref) => match parsed.tasks.iter().find(|t| t.task_ref.as_str() == t_ref)
                    {
                        Some(t) => (true, t.status == SpecStatus::Completed),
                        None => (false, false),
                    },
                    None => (true, true),
                };
                return Ok(DepTargetResolution::ArchivedSpec {
                    rel_path: rel_posix,
                    completed_doc,
                    task_exists,
                    task_completed,
                });
            }
        }
    }

    Ok(DepTargetResolution::NotFound)
}

fn is_external_task_satisfied(
    reader: &dyn StateReader,
    ws_path: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    target_run_id: &RunId,
    target_task_ref: &TaskRef,
) -> Result<bool, StateError> {
    match resolve_dep_target(
        ws_path,
        collections,
        Some(reader),
        None,
        target_run_id.as_str(),
        Some(target_task_ref.as_str()),
        target_run_id.as_str(),
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
    let spec_repos = spec_repo_map(&primary, doc);

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
            let (db_path, collections) = resolve_ws_context(&canon, &primary, config)?;
            (canon, collections, rest, Some(SqliteStateReader::new(db_path)))
        } else {
            (
                primary.clone(),
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
    let spec_repos = spec_repo_map(root, doc);

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
                if !trimmed.contains(':') {
                    issues.push(DepValidationIssue::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "task '{}' external dependency '{trimmed}' must include ':<task_ref>'",
                            t.task_ref
                        ),
                    ));
                    continue;
                }
                if let Err(err) = validate_external_dep_ref(
                    root,
                    adapter,
                    local_reader,
                    state_db_override,
                    &linked_workspaces,
                    &spec_repos,
                    active_specs,
                    &doc.id,
                    trimmed,
                    true,
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
                issues.push(DepValidationIssue::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "task '{}' depends on unknown local task '{trimmed}'",
                        t.task_ref
                    ),
                ));
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
            state_db_override,
            &linked_workspaces,
            &spec_repos,
            active_specs,
            &doc.id,
            trimmed,
            false,
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
enum DepGraphNode<'a> {
    Spec(&'a str),
    Task(&'a str, &'a str),
}

impl<'a> DepGraphNode<'a> {
    fn spec_id(&self) -> &'a str {
        match self {
            Self::Spec(s) | Self::Task(s, _) => s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DfsColor {
    Visiting,
    Visited,
}

fn spec_repo_map(primary_root: &Path, spec_doc: &SpecDocument) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();
    for r in &spec_doc.repositories {
        let p = Path::new(&r.path);
        map.insert(
            r.name.clone(),
            if p.is_relative() {
                primary_root.join(p)
            } else {
                p.to_path_buf()
            },
        );
    }
    map
}

fn parse_dep_node<'a>(
    primary_canon: Option<&Path>,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    default_spec_id: &'a str,
    raw: &'a str,
    allow_bare_task: bool,
) -> Option<DepGraphNode<'a>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (target_str, allow_bare) = if let Some((ws_alias, rest)) = trimmed.split_once('/') {
        let primary = primary_canon?;
        let canon =
            resolve_linked_workspace_root(ws_alias, trimmed, linked_workspaces, spec_repos).ok()?;
        if canon.as_path() != primary {
            return None;
        }
        (rest.trim(), false)
    } else {
        (trimmed, allow_bare_task)
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
        Some(DepGraphNode::Task(s, t))
    } else if allow_bare {
        Some(DepGraphNode::Task(default_spec_id, target_str))
    } else {
        Some(DepGraphNode::Spec(target_str))
    }
}

fn has_cross_spec_cycle(
    primary_root: &Path,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    doc: &SpecDocument,
    active_specs: &[SpecDocument],
) -> bool {
    let primary_canon = canonical_root(primary_root).ok();
    let mut by_id: BTreeMap<&str, &SpecDocument> = BTreeMap::new();
    for s in active_specs {
        by_id.insert(s.id.as_str(), s);
    }
    by_id.insert(doc.id.as_str(), doc);

    let mut color: BTreeMap<DepGraphNode<'_>, DfsColor> = BTreeMap::new();
    let mut stack: Vec<DepGraphNode<'_>> = Vec::new();

    #[allow(clippy::too_many_arguments)]
    fn dfs<'a>(
        node: DepGraphNode<'a>,
        primary_root: &Path,
        primary_canon: Option<&Path>,
        linked_workspaces: &BTreeMap<String, PathBuf>,
        by_id: &BTreeMap<&'a str, &'a SpecDocument>,
        color: &mut BTreeMap<DepGraphNode<'a>, DfsColor>,
        stack: &mut Vec<DepGraphNode<'a>>,
    ) -> bool {
        if let Some(&c) = color.get(&node) {
            if c == DfsColor::Visiting {
                if let Some(pos) = stack.iter().position(|n| n == &node) {
                    let cycle_slice = &stack[pos..];
                    let first_spec = cycle_slice[0].spec_id();
                    if cycle_slice.iter().any(|n| n.spec_id() != first_spec) {
                        return true;
                    }
                }
            }
            return false;
        }
        color.insert(node.clone(), DfsColor::Visiting);
        stack.push(node.clone());

        if let Some(spec_doc) = by_id.get(node.spec_id()) {
            let repos = spec_repo_map(primary_root, spec_doc);
            let mut neighbors: Vec<DepGraphNode<'a>> = Vec::new();
            for raw in &spec_doc.depends_on {
                if let Some(target) = parse_dep_node(
                    primary_canon,
                    linked_workspaces,
                    &repos,
                    spec_doc.id.as_str(),
                    raw,
                    false,
                ) {
                    neighbors.push(target);
                }
            }
            match node {
                DepGraphNode::Spec(_) => {
                    for t in &spec_doc.tasks {
                        neighbors.push(DepGraphNode::Task(
                            spec_doc.id.as_str(),
                            t.task_ref.as_str(),
                        ));
                    }
                }
                DepGraphNode::Task(_, task_ref) => {
                    if let Some(task) = spec_doc
                        .tasks
                        .iter()
                        .find(|t| t.task_ref.as_str() == task_ref)
                    {
                        for raw in &task.depends_on {
                            if let Some(target) = parse_dep_node(
                                primary_canon,
                                linked_workspaces,
                                &repos,
                                spec_doc.id.as_str(),
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
                if dfs(
                    next,
                    primary_root,
                    primary_canon,
                    linked_workspaces,
                    by_id,
                    color,
                    stack,
                ) {
                    return true;
                }
            }
        }

        stack.pop();
        color.insert(node, DfsColor::Visited);
        false
    }

    if dfs(
        DepGraphNode::Spec(doc.id.as_str()),
        primary_root,
        primary_canon.as_deref(),
        linked_workspaces,
        &by_id,
        &mut color,
        &mut stack,
    ) {
        return true;
    }
    for t in &doc.tasks {
        if dfs(
            DepGraphNode::Task(doc.id.as_str(), t.task_ref.as_str()),
            primary_root,
            primary_canon.as_deref(),
            linked_workspaces,
            &by_id,
            &mut color,
            &mut stack,
        ) {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn validate_external_dep_ref(
    primary_root: &Path,
    primary_adapter: &Adapter,
    local_reader: Option<&dyn StateReader>,
    state_db_override: Option<&Path>,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
    primary_active_specs: &[SpecDocument],
    self_spec_id: &str,
    trimmed: &str,
    require_task: bool,
) -> Result<(), StateError> {
    let primary_canon = canonical_root(primary_root).ok();
    let (ws_path, is_primary, ws_collections, ws_db_path, run_and_task) =
        if let Some((ws_alias, rest)) = trimmed.split_once('/') {
            let canon =
                resolve_linked_workspace_root(ws_alias, trimmed, linked_workspaces, spec_repos)?;
            if primary_canon.as_deref() == Some(canon.as_path()) {
                let db_path = match state_db_override {
                    Some(p) if p.is_absolute() => p.to_path_buf(),
                    Some(rel) => primary_root.join(rel),
                    None => resolve_state_db_path(primary_root, primary_adapter),
                };
                (
                    primary_root.to_path_buf(),
                    true,
                    primary_adapter.specs.clone(),
                    db_path,
                    rest,
                )
            } else {
                let ws_adapter = load_ws_adapter(&canon)?;
                let db_path = resolve_state_db_path(&canon, &ws_adapter);
                (canon, false, ws_adapter.specs, db_path, rest)
            }
        } else {
            let db_path = match state_db_override {
                Some(p) if p.is_absolute() => p.to_path_buf(),
                Some(rel) => primary_root.join(rel),
                None => resolve_state_db_path(primary_root, primary_adapter),
            };
            (
                primary_root.to_path_buf(),
                true,
                primary_adapter.specs.clone(),
                db_path,
                trimmed,
            )
        };

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

    let active_storage: Vec<SpecDocument>;
    let active_list: &[SpecDocument] = if is_primary {
        primary_active_specs
    } else {
        let scan = collect_workspace_specs(&ws_path, &ws_collections);
        if let Some(first_err) = scan.errors.into_iter().next() {
            return Err(StateError::new(first_err.code, first_err.message));
        }
        active_storage = scan.specs.into_iter().map(|s| s.doc).collect();
        &active_storage
    };

    let owned_reader = if is_primary && local_reader.is_some() {
        None
    } else if ws_db_path.is_file() {
        Some(SqliteStateReader::new(&ws_db_path))
    } else {
        None
    };
    let reader_opt: Option<&dyn StateReader> = if is_primary && local_reader.is_some() {
        local_reader
    } else {
        owned_reader.as_ref().map(|r| r as &dyn StateReader)
    };

    match resolve_dep_target(
        &ws_path,
        &ws_collections,
        reader_opt,
        Some(active_list),
        run_sel,
        task_sel,
        trimmed,
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
    fn walk(
        root: &Path,
        dir: &Path,
        col: &SpecCollectionConfig,
        found: &mut BTreeMap<String, CollectedWorkspaceSpec>,
        errors: &mut Vec<WorkspaceSpecScanError>,
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
                    walk(root, &path, col, found, errors);
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
                Ok(content) => match parse_spec(&content) {
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
        let dir = match checked_child(root, Path::new(&col.path)) {
            Ok(d) => d,
            Err(err) => {
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: ErrorCode::ProfileSchemaInvalid.as_str(),
                    path: col.path.clone(),
                    message: format!(
                        "spec collection '{}' failed confinement check: {err}",
                        col.path
                    ),
                });
                continue;
            }
        };
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: ErrorCode::ProfileSchemaInvalid.as_str(),
                    path: col.path.clone(),
                    message: format!("spec collection '{}' must not be a symlink", col.path),
                });
            }
            Ok(meta) if meta.is_dir() => {
                walk(root, &dir, col, &mut found, &mut errors);
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                errors.push(WorkspaceSpecScanError {
                    code: ErrorCode::ProfileSchemaInvalid,
                    diag_code: "VALIDATION_IO",
                    path: col.path.clone(),
                    message: format!("unable to inspect spec collection '{}': {err}", col.path),
                });
            }
        }
    }
    WorkspaceSpecScan {
        specs: found.into_values().collect(),
        errors,
    }
}
