//! External cross-run/cross-workspace dependency resolution and run-level `depends_on` checks.

use super::snapshot::{
    find_latest_matching_tombstone, is_completed_archive_doc, is_git_repo, load_dep_snapshot,
    tombstone_superseded_by_cleared, visit_archived_specs, DepSnapshot, PreloadedDepContext,
};
use super::syntax::{
    checkout_scoped_target_selector, effective_active_run_config, load_ws_adapter,
    resolve_linked_workspace_root, resolve_observation_context, resolve_spec_repositories,
    run_in_checkout, run_matches_selector, same_repository,
};
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunGraphSnapshot, TerminalStatus};
use crate::usecases::lifecycle::{
    effective_spec_collections, list_all_runs, list_all_tombstones, primary_workspace_root,
    read_confined_utf8,
};
use crate::usecases::StateEngineConfig;
use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::canonical_root;
use acdd_core::git::{list_owned_worktrees, read_git_blob_utf8};
use acdd_core::ids::{CommitOid, RunId, TaskRef};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

pub(crate) enum DepTargetResolution {
    ActiveSpec {
        cancelled: bool,
        task_exists: bool,
    },
    ActiveRun {
        run_id: RunId,
        spec_id: Option<String>,
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
