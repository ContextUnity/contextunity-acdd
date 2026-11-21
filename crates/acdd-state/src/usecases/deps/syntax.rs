//! Dependency syntax parsing, linked-workspace resolution, and active-run context selection.

use super::snapshot::find_latest_matching_tombstone;
use crate::machine::MachineTaskDep;
use crate::spec::SpecDocument;
use crate::store::sqlite::SqliteStateReader;
use crate::types::{RunRecord, RunTombstoneRecord};
use crate::usecases::lifecycle::{effective_spec_collections, list_all_runs, list_all_tombstones};
use crate::usecases::StateEngineConfig;
use acdd_core::config::{
    load_adapter, resolve_effective_state_db_path, resolve_state_db_path, Adapter,
    SpecCollectionConfig, ACDD_CONFIG_FILE,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::canonical_root;
use acdd_core::git::list_owned_worktrees;
use acdd_core::ids::{RunId, TaskRef};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

pub(crate) fn load_ws_adapter(ws_root: &Path) -> Result<Adapter, StateError> {
    load_adapter(ws_root, None).map_err(|err| {
        if let Some(se) = err.get_ref().and_then(|inner| inner.downcast_ref::<StateError>()) {
            se.clone()
        } else {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to load {ACDD_CONFIG_FILE} in '{}': {err}", ws_root.display()),
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

pub(super) fn same_repository(left: &Path, right: &Path) -> bool {
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

pub(super) fn resolve_observation_context(
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
    let tombs: Vec<RunTombstoneRecord> = list_all_tombstones(&reader)?;
    if let Some(found) = find_latest_matching_tombstone(&tombs, run_sel) {
        return Ok(found.run_id.clone());
    }
    RunId::new(run_sel).map_err(StateError::from)
}
