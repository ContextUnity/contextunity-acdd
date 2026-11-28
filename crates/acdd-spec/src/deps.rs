//! Static dependency syntax parsing, target resolution, and cycle validation (`INV-SINGLE-OWNER`).

use crate::discovery::{
    collect_archived_specs, collect_workspace_specs, is_completed_archive_doc, load_ws_adapter,
    resolve_linked_workspace_root, resolve_spec_repositories,
};
use crate::overlay::{RuntimeDepResolution, SpecRuntimeOverlay};
use crate::spec::{is_safe_path_segment_id, SpecDocument, SpecStatus};
use acdd_core::config::{Adapter, SpecConfig};
use acdd_core::errors::{AcddError, ErrorCode, DIAG_ITEM_DEPENDENCY_UNKNOWN};
use acdd_core::fs::canonical_root;
use acdd_core::ids::TaskRef;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// Parsed syntactic form of a `depends_on` entry (`INV-SINGLE-OWNER`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedDepSyntax {
    /// Intra-spec item/task dependency (`<item_ref>`).
    LocalTask(String),
    /// Cross-spec dependency within the primary/current workspace (`<spec_id>[:<task_ref>|#<item_ref>]`).
    LocalSpec {
        /// Target spec/run selector.
        spec_id: String,
        /// Optional target task reference (`<spec_id>:<task_ref>`).
        task_ref: Option<String>,
        /// Optional target named item reference (`<spec_id>#<item_ref>`).
        item_ref: Option<String>,
    },
    /// Cross-workspace dependency (`<ws_alias>/<spec_id>[:<task_ref>|#<item_ref>]`).
    LinkedSpec {
        /// Linked workspace or repository alias.
        ws_alias: String,
        /// Target spec/run selector.
        spec_id: String,
        /// Optional target task reference (`<ws_alias>/<spec_id>:<task_ref>`).
        task_ref: Option<String>,
        /// Optional target named item reference (`<ws_alias>/<spec_id>#<item_ref>`).
        item_ref: Option<String>,
    },
}

fn dep_syntax_err(msg: impl Into<String>) -> AcddError {
    AcddError::new(ErrorCode::SpecSchemaInvalid, msg).with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN)
}

fn validate_dep_ref_token(raw: &str, full_entry: &str, label: &str) -> Result<(), AcddError> {
    if raw.is_empty()
        || raw.contains(char::is_whitespace)
        || !is_safe_path_segment_id(raw)
        || raw.contains([':', '#'])
    {
        return Err(dep_syntax_err(format!(
            "invalid {label} '{raw}' in depends_on entry '{full_entry}'"
        )));
    }
    Ok(())
}

/// Validate pure `depends_on` entry syntax without requiring local item context.
pub(crate) fn validate_dep_entry_syntax(raw: &str) -> Result<(), AcddError> {
    let empty = BTreeSet::new();
    parse_dep_syntax(raw, &empty).map(|_| ())
}

/// Parse raw `depends_on` syntax into [`ParsedDepSyntax`].
pub fn parse_dep_syntax(
    raw: &str,
    local_item_refs: &BTreeSet<&str>,
) -> Result<ParsedDepSyntax, AcddError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(dep_syntax_err("depends_on entry cannot be empty"));
    }
    if trimmed.contains(char::is_whitespace) || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(dep_syntax_err(format!(
            "invalid depends_on entry '{trimmed}' (whitespace or path traversal is forbidden)"
        )));
    }

    let (left_part, task_ref, item_ref) = if let Some((left, hash_part)) = trimmed.split_once('#') {
        if left.contains(':') {
            return Err(dep_syntax_err(format!(
                "depends_on entry '{trimmed}' cannot mix ':' and '#'"
            )));
        }
        validate_dep_ref_token(hash_part, trimmed, "item_ref")?;
        let _ = TaskRef::new(hash_part).map_err(|err| {
            dep_syntax_err(format!(
                "invalid item_ref '{hash_part}' in depends_on entry '{trimmed}': {err}"
            ))
        })?;
        (left, None, Some(hash_part.to_owned()))
    } else if let Some((left, task_part)) = trimmed.split_once(':') {
        if left.contains('#') {
            return Err(dep_syntax_err(format!(
                "depends_on entry '{trimmed}' cannot mix ':' and '#'"
            )));
        }
        validate_dep_ref_token(task_part, trimmed, "task_ref")?;
        let tref = TaskRef::new(task_part).map_err(|err| {
            dep_syntax_err(format!(
                "invalid task_ref '{task_part}' in depends_on entry '{trimmed}': {err}"
            ))
        })?;
        (left, Some(tref.as_str().to_owned()), None)
    } else {
        (trimmed, None, None)
    };

    if left_part.is_empty() {
        return Err(dep_syntax_err(format!(
            "depends_on entry '{trimmed}' is missing target run or spec ID"
        )));
    }

    if let Some((ws_alias, spec_part)) = left_part.split_once('/') {
        if ws_alias.is_empty() || spec_part.is_empty() || spec_part.contains('/') {
            return Err(dep_syntax_err(format!(
                "invalid linked workspace dependency '{trimmed}'"
            )));
        }
        validate_dep_ref_token(ws_alias, trimmed, "workspace alias")?;
        validate_dep_ref_token(spec_part, trimmed, "spec_id")?;
        Ok(ParsedDepSyntax::LinkedSpec {
            ws_alias: ws_alias.to_owned(),
            spec_id: spec_part.to_owned(),
            task_ref,
            item_ref,
        })
    } else if task_ref.is_some() || item_ref.is_some() {
        validate_dep_ref_token(left_part, trimmed, "spec_id")?;
        Ok(ParsedDepSyntax::LocalSpec {
            spec_id: left_part.to_owned(),
            task_ref,
            item_ref,
        })
    } else if local_item_refs.contains(trimmed) {
        Ok(ParsedDepSyntax::LocalTask(trimmed.to_owned()))
    } else {
        validate_dep_ref_token(trimmed, trimmed, "spec_id")?;
        Ok(ParsedDepSyntax::LocalSpec {
            spec_id: trimmed.to_owned(),
            task_ref: None,
            item_ref: None,
        })
    }
}

/// Resolution outcome for an external `depends_on` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepTargetResolution {
    /// Matched an active/planned/cancelled spec file on disk.
    ActiveSpec {
        /// Whether the matched spec has `status: cancelled`.
        cancelled: bool,
        /// Whether the requested `task_ref` exists (always `true` when `task_sel` is `None`).
        task_exists: bool,
    },
    /// Matched an active `Run` supplied by [`SpecRuntimeOverlay`].
    ActiveRun {
        /// Active run ID.
        run_id: acdd_core::ids::RunId,
        /// Bound spec ID if present.
        spec_id: Option<String>,
        /// Whether the requested task exists on the active run.
        task_exists: bool,
        /// Whether the requested task is completed.
        task_completed: bool,
    },
    /// Matched a terminal `RunTombstoneRecord` supplied by [`SpecRuntimeOverlay`].
    Tombstone {
        /// Whether the tombstone was cleared or superseded by a cleared run.
        cleared: bool,
        /// Task completion outcome when not cleared.
        task_outcome: Result<(), AcddError>,
    },
    /// Matched a committed or disk archive specification document.
    ArchivedSpec {
        /// Archive relative path.
        rel_path: String,
        /// Whether the archive document is `completed`.
        completed_doc: bool,
        /// Whether the requested task exists in the archive.
        task_exists: bool,
        /// Whether the requested task is `completed` in the archive.
        task_completed: bool,
    },
    /// No matching run, spec, tombstone, or archive was found.
    NotFound,
}

fn archived_resolution(
    parsed: &SpecDocument,
    rel_posix: String,
    task_sel: Option<&str>,
) -> DepTargetResolution {
    let completed_doc = is_completed_archive_doc(parsed);
    let (task_exists, task_completed) = match task_sel {
        Some(t_ref) => match parsed.item_status(t_ref) {
            Some(status) => (true, status == SpecStatus::Completed),
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

/// Resolve an external `depends_on` target across [`SpecRuntimeOverlay`], active specs, and archives.
#[allow(clippy::too_many_arguments)]
pub fn resolve_dep_target(
    ws_path: &Path,
    collections: &BTreeMap<String, SpecConfig>,
    overlay: &dyn SpecRuntimeOverlay,
    active_specs: Option<&[SpecDocument]>,
    archived_specs: Option<&BTreeMap<String, (String, SpecDocument)>>,
    run_sel: &str,
    task_sel: Option<&str>,
    dep_label: &str,
    run_checkout: Option<&Path>,
) -> Result<DepTargetResolution, AcddError> {
    if let Some(res) =
        overlay.resolve_active_run_dep(ws_path, collections, run_sel, task_sel, run_checkout)?
    {
        return Ok(match res {
            RuntimeDepResolution::ActiveRun {
                run_id,
                spec_id,
                task_exists,
                task_completed,
            } => DepTargetResolution::ActiveRun {
                run_id,
                spec_id,
                task_exists,
                task_completed,
            },
            RuntimeDepResolution::Tombstone {
                cleared,
                task_outcome,
            } => DepTargetResolution::Tombstone {
                cleared,
                task_outcome,
            },
        });
    }

    if let Some(target_doc) = active_specs.and_then(|specs| specs.iter().find(|s| s.id == run_sel))
    {
        let cancelled = target_doc.status == SpecStatus::Cancelled;
        let task_exists = task_sel.is_none_or(|item_ref| target_doc.has_item(item_ref));
        return Ok(DepTargetResolution::ActiveSpec {
            cancelled,
            task_exists,
        });
    }

    if let Some(res) =
        overlay.resolve_tombstone_dep(ws_path, collections, run_sel, task_sel, dep_label)?
    {
        return Ok(match res {
            RuntimeDepResolution::ActiveRun {
                run_id,
                spec_id,
                task_exists,
                task_completed,
            } => DepTargetResolution::ActiveRun {
                run_id,
                spec_id,
                task_exists,
                task_completed,
            },
            RuntimeDepResolution::Tombstone {
                cleared,
                task_outcome,
            } => DepTargetResolution::Tombstone {
                cleared,
                task_outcome,
            },
        });
    }

    if let Some(preloaded) = archived_specs {
        return Ok(preloaded
            .get(run_sel)
            .map_or(DepTargetResolution::NotFound, |(path, doc)| {
                archived_resolution(doc, path.clone(), task_sel)
            }));
    }

    let mut scratch_warnings = Vec::new();
    if let Some(found) = crate::discovery::visit_archived_specs(
        ws_path,
        collections,
        false,
        None,
        &mut scratch_warnings,
        |col_name, rel_posix, raw| {
            let owning = collections
                .get_key_value(col_name)
                .map(|(k, v)| (k.as_str(), v));
            let parsed = crate::spec::parse_spec_with_collections(raw, owning, collections).ok()?;
            if parsed.id != run_sel {
                return None;
            }
            Some(archived_resolution(&parsed, rel_posix, task_sel))
        },
    )? {
        return Ok(found);
    }
    if !scratch_warnings.is_empty() {
        let joined = scratch_warnings.join("; ");
        let code = if joined.contains("HEAD") {
            ErrorCode::ExternalGitDrift
        } else {
            ErrorCode::SpecSchemaInvalid
        };
        return Err(AcddError::new(code, joined));
    }

    Ok(DepTargetResolution::NotFound)
}

/// Diagnostic emitted by [`validate_spec_dependencies`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepValidationIssue {
    /// Error code classifying the dependency violation.
    pub code: ErrorCode,
    /// Stable diagnostic code (`ITEM_DEPENDENCY_UNKNOWN` when applicable, else `code.as_str()`).
    pub diag_code: &'static str,
    /// Human-readable description of the violation.
    pub message: String,
}

impl DepValidationIssue {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            diag_code: code.as_str(),
            message: message.into(),
        }
    }

    fn from_err(err: &AcddError, message: impl Into<String>) -> Self {
        Self {
            code: err.code,
            diag_code: static_dep_diag_code(err),
            message: message.into(),
        }
    }

    fn with_diag_code(mut self, diag_code: &'static str) -> Self {
        self.diag_code = diag_code;
        self
    }
}

fn static_dep_diag_code(err: &AcddError) -> &'static str {
    if err.effective_diag_code() == DIAG_ITEM_DEPENDENCY_UNKNOWN {
        DIAG_ITEM_DEPENDENCY_UNKNOWN
    } else {
        err.code.as_str()
    }
}

struct ValidationWorkspace {
    path: PathBuf,
    collections: BTreeMap<String, SpecConfig>,
    active: Vec<SpecDocument>,
    archived: BTreeMap<String, (String, SpecDocument)>,
    checkout_scoped: bool,
}

/// Validate intra-spec task DAG (`unknown local task_ref`, self-dependency, and cycles)
/// plus external task and spec-level `depends_on` references using `overlay`.
pub fn validate_spec_dependencies(
    root: &Path,
    adapter: &Adapter,
    overlay: &dyn SpecRuntimeOverlay,
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
    let mut workspaces: BTreeMap<String, ValidationWorkspace> = BTreeMap::new();

    let mut validate_external_dep_ref = |trimmed: &str, require_task: bool| -> Result<(), AcddError> {
        validate_dep_entry_syntax(trimmed)?;
        let self_spec_id = doc.id.as_str();
        let (alias, _) = trimmed.split_once('/').unwrap_or(("", trimmed));
        if !workspaces.contains_key(alias) {
            let ws_path = if alias.is_empty() {
                local_checkout.clone()
            } else {
                resolve_linked_workspace_root(alias, trimmed, &linked_workspaces, &spec_repos)?
            };
            let is_primary = ws_path == local_checkout;
            let collections = if is_primary {
                adapter.specs.clone()
            } else {
                load_ws_adapter(&ws_path)?.specs
            };
            let active = if is_primary {
                active_specs.to_vec()
            } else {
                let scan = collect_workspace_specs(&ws_path, &collections);
                if let Some(first_err) = scan.errors.into_iter().next() {
                    return Err(AcddError::new(first_err.code, first_err.message));
                }
                scan.specs.into_iter().map(|s| s.doc).collect()
            };
            let (archives, _) = collect_archived_specs(&ws_path, &collections, false, None)?;
            let archived = archives
                .into_iter()
                .map(|(_, path, doc)| (doc.id.clone(), (path, doc)))
                .collect();
            let checkout_scoped = !alias.is_empty()
                && acdd_core::git::primary_worktree_root(&ws_path).as_deref()
                    == Some(&local_primary);
            workspaces.insert(
                alias.to_owned(),
                ValidationWorkspace {
                    path: ws_path,
                    collections,
                    active,
                    archived,
                    checkout_scoped,
                },
            );
        }
        let workspace = &workspaces[alias];
        let ws_path = &workspace.path;
        let is_primary = *ws_path == local_checkout;

        let empty_refs = BTreeSet::new();
        let (run_sel, task_sel) = match parse_dep_syntax(trimmed, &empty_refs)? {
            ParsedDepSyntax::LocalSpec {
                spec_id,
                task_ref,
                item_ref,
            }
            | ParsedDepSyntax::LinkedSpec {
                spec_id,
                task_ref,
                item_ref,
                ..
            } => {
                let target_ref = task_ref.or(item_ref);
                if require_task && target_ref.is_none() {
                    return Err(dep_syntax_err(format!(
                        "task dependency '{trimmed}' must specify ':<task_ref>' or '#<item_ref>'"
                    )));
                }
                (spec_id, target_ref)
            }
            ParsedDepSyntax::LocalTask(task_ref) => {
                return Err(dep_syntax_err(format!(
                    "unexpected local task dependency '{task_ref}' in '{trimmed}'"
                )));
            }
        };

        if is_primary && run_sel == self_spec_id {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("spec '{self_spec_id}' cannot depend on itself via '{trimmed}'"),
            ));
        }

        match resolve_dep_target(
            ws_path,
            &workspace.collections,
            overlay,
            Some(&workspace.active),
            Some(&workspace.archived),
            &run_sel,
            task_sel.as_deref(),
            trimmed,
            workspace.checkout_scoped.then_some(ws_path.as_path()),
        )? {
            DepTargetResolution::ActiveSpec {
                cancelled,
                task_exists,
            } => {
                if cancelled {
                    return Err(AcddError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("dependency '{trimmed}' targets cancelled spec '{run_sel}'"),
                    ));
                }
                if let Some(ref t_ref) = task_sel {
                    if !task_exists {
                        return Err(AcddError::new(
                            ErrorCode::SpecSchemaInvalid,
                            format!(
                                "dependency '{trimmed}' references unknown item '{t_ref}' in spec '{run_sel}'"
                            ),
                        )
                        .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN));
                    }
                }
                Ok(())
            }
            DepTargetResolution::ActiveRun {
                run_id,
                task_exists,
                ..
            } => {
                if let Some(ref t_ref) = task_sel {
                    if !task_exists {
                        return Err(AcddError::new(
                            ErrorCode::SpecSchemaInvalid,
                            format!(
                                "dependency '{trimmed}' references unknown item '{t_ref}' in active run '{run_id}'"
                            ),
                        )
                        .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN));
                    }
                }
                Ok(())
            }
            DepTargetResolution::Tombstone {
                cleared,
                task_outcome,
            } => {
                if cleared {
                    return Err(AcddError::new(
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
                    return Err(AcddError::new(
                        ErrorCode::GateVerificationFailed,
                        format!(
                            "dependency '{trimmed}' targets non-completed archive '{rel_path}'"
                        ),
                    ));
                }
                if let Some(t_ref) = task_sel {
                    if !task_exists {
                        return Err(AcddError::new(
                            ErrorCode::SpecSchemaInvalid,
                            format!(
                                "dependency '{trimmed}' references unknown item '{t_ref}' in archive '{rel_path}'"
                            ),
                        )
                        .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN));
                    }
                    if !task_completed {
                        return Err(AcddError::new(
                            ErrorCode::GateVerificationFailed,
                            format!("dependency '{trimmed}' task '{t_ref}' is not completed"),
                        ));
                    }
                }
                Ok(())
            }
            DepTargetResolution::NotFound => Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("dependency '{trimmed}' target '{run_sel}' was not found"),
            )
            .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN)),
        }
    };

    // 1. Validate intra-spec item/task DAG and external item.depends_on entries.
    let all_items: Vec<(&str, &[String], &str)> = doc
        .tasks
        .iter()
        .map(|t| (t.task_ref.as_str(), t.depends_on.as_slice(), "task"))
        .chain(doc.items.values().flatten().map(|item| {
            (
                item.item_ref.as_str(),
                item.depends_on.as_slice(),
                item.item.as_str(),
            )
        }))
        .collect();
    let local_item_refs: BTreeSet<&str> = all_items.iter().map(|&(r, _, _)| r).collect();
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &(item_ref, _, _) in &all_items {
        indegree.insert(item_ref, 0);
        adj.entry(item_ref).or_default();
    }

    for &(item_ref, deps, kind) in &all_items {
        for dep_raw in deps {
            let trimmed = dep_raw.trim();
            if trimmed.is_empty() {
                issues.push(
                    DepValidationIssue::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!("{kind} '{item_ref}' has an empty depends_on entry"),
                    )
                    .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN),
                );
                continue;
            }
            if trimmed.contains(':')
                || trimmed.contains('#')
                || trimmed.contains('/')
                || trimmed.contains(char::is_whitespace)
                || trimmed.contains('\\')
                || trimmed.contains("..")
            {
                let require_item = trimmed.contains(':') || trimmed.contains('#');
                if let Err(err) = validate_external_dep_ref(trimmed, require_item) {
                    issues.push(DepValidationIssue::from_err(&err, err.to_string()));
                }
                continue;
            }
            if let Err(err) = TaskRef::new(trimmed) {
                issues.push(
                    DepValidationIssue::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!("{kind} '{item_ref}' depends_on '{trimmed}' is invalid: {err}"),
                    )
                    .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN),
                );
                continue;
            }
            if trimmed == item_ref {
                issues.push(DepValidationIssue::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("{kind} '{item_ref}' cannot depend on itself"),
                ));
                continue;
            }
            if !local_item_refs.contains(trimmed) {
                if let Err(err) = validate_external_dep_ref(trimmed, false) {
                    issues.push(DepValidationIssue::from_err(
                        &err,
                        format!(
                            "{kind} '{item_ref}' depends on unknown local item or external spec '{trimmed}': {err}"
                        ),
                    ));
                }
                continue;
            }
            adj.entry(trimmed).or_default().push(item_ref);
            *indegree.entry(item_ref).or_insert(0) += 1;
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
    if visited != local_item_refs.len() {
        issues.push(DepValidationIssue::new(
            ErrorCode::SpecSchemaInvalid,
            format!("spec '{}' task dependency graph contains a cycle", doc.id),
        ));
    }

    // 2. Validate spec-level `doc.depends_on` entries and cross-spec cycles.
    for dep_raw in &doc.depends_on {
        let trimmed = dep_raw.trim();
        if trimmed.is_empty() {
            issues.push(
                DepValidationIssue::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("spec '{}' has an empty depends_on entry", doc.id),
                )
                .with_diag_code(DIAG_ITEM_DEPENDENCY_UNKNOWN),
            );
            continue;
        }
        if let Err(err) = validate_external_dep_ref(trimmed, false) {
            issues.push(DepValidationIssue::from_err(&err, err.to_string()));
        }
    }

    if has_cross_spec_cycle(root, &linked_workspaces, doc, active_specs) {
        issues.push(DepValidationIssue::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "spec '{}' depends_on forms a cross-spec dependency cycle",
                doc.id
            ),
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
    let (target_ws, target_str, allow_bare) =
        if let Some((ws_alias, rest)) = trimmed.split_once('/') {
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
    if let Some((spec_part, item_part)) = target_str
        .split_once(':')
        .or_else(|| target_str.split_once('#'))
    {
        let s = spec_part.trim();
        let t = item_part.trim();
        if s.is_empty() || t.is_empty() {
            return None;
        }
        Some(DepGraphNode {
            ws_root: target_ws,
            spec_id: s.to_owned(),
            task_ref: Some(t.to_owned()),
        })
    } else if allow_bare && default_spec.has_item(target_str) {
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
            let matching_deps: Vec<&[String]> = match &node.task_ref {
                Some(t_ref) => spec_doc.item_depends_on(t_ref).into_iter().collect(),
                None => spec_doc
                    .tasks
                    .iter()
                    .map(|t| t.depends_on.as_slice())
                    .chain(
                        spec_doc
                            .items
                            .values()
                            .flatten()
                            .map(|item| item.depends_on.as_slice()),
                    )
                    .collect(),
            };
            for deps in matching_deps {
                for raw in deps {
                    if let Some(target) =
                        parse_dep_node(&node.ws_root, &ws_linked, &repos, &spec_doc, raw, true)
                    {
                        neighbors.push(target);
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

    let mut seeds = vec![DepGraphNode {
        ws_root: primary_key.clone(),
        spec_id: doc.id.clone(),
        task_ref: None,
    }];
    for item_ref in doc.all_item_refs() {
        seeds.push(DepGraphNode {
            ws_root: primary_key.clone(),
            spec_id: doc.id.clone(),
            task_ref: Some(item_ref.to_owned()),
        });
    }

    for seed in seeds {
        if dfs(seed, &mut ws_cache, &mut color, &mut stack) {
            return true;
        }
    }
    false
}
