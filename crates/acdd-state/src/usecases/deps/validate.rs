//! Intra-spec task DAG validation and cross-spec/cross-workspace cycle checks.

use super::external::{resolve_dep_target, DepTargetResolution};
use super::snapshot::{collect_workspace_specs, load_dep_snapshot, DepSnapshot};
use super::syntax::{
    load_ws_adapter, resolve_linked_workspace_root, resolve_spec_repositories,
    resolve_ws_context_canon,
};
use crate::spec::SpecDocument;
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use acdd_core::config::{resolve_effective_state_db_path, Adapter, SpecCollectionConfig};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::canonical_root;
use acdd_core::ids::TaskRef;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
