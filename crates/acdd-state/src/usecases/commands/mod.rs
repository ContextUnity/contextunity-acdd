//! Mutating and plugin command handlers (`Start`, `Draft`, `Claim`, `ExtendScope`, `Reopen`,
//! `Sync`, `Subtask`, `BlackboardPost`, `Clear`, `CallPluginTool`).

mod claim;
mod run_ops;
mod start;
mod task_ops;

pub(crate) use claim::handle_claim_dyn;
pub(crate) use crate::context::is_context_reset_required;
pub(crate) use run_ops::{handle_clear_dyn, handle_draft_dyn, handle_sync_dyn};
pub(crate) use start::handle_start_dyn;
pub(crate) use task_ops::{
    handle_blackboard_post_dyn, handle_call_plugin_tool_dyn, handle_extend_scope_dyn,
    handle_reopen_dyn, handle_subtask_dyn,
};

use crate::git_ops::worktree::canonicalize_macos_var;
use crate::machine::{MachineSpecInput, MachineSubtaskInput, MachineTaskSpec};
use crate::spec::SpecDocument;
use crate::usecases::lifecycle::parse_task_dependency;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{RunId, RunName};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) fn ensure_no_managed_worktree_collision(
    primary: &Path,
    run_name: &str,
    candidate_wt: &Path,
    candidate_branch: Option<&str>,
    existing_wt: Option<&str>,
) -> Result<(), StateError> {
    let matches_existing =
        existing_wt.is_some_and(|existing| canonicalize_macos_var(Path::new(existing)) == candidate_wt);
    let managed_wt_root = canonicalize_macos_var(&primary.join(".worktrees"));
    let collides_managed_path = candidate_wt.parent() == Some(managed_wt_root.as_path())
        && candidate_wt
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == run_name || name.starts_with(&format!("{run_name}-")));
    let collides_managed_branch = candidate_branch.is_some_and(|b| {
        b == format!("run/{run_name}") || b.starts_with(&format!("task/{run_name}-"))
    });
    if !matches_existing && (collides_managed_path || collides_managed_branch) {
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "explicit worktree '{}' collides with ACDD managed worktree/branch naming for run '{}' while worktrees: managed is active",
                candidate_wt.display(),
                run_name
            ),
        ));
    }
    Ok(())
}

pub(super) fn build_machine_spec_input(
    doc: &SpecDocument,
    primary_workspace: &str,
    run_id: &RunId,
    linked_workspaces: &BTreeMap<String, PathBuf>,
    spec_repos: &BTreeMap<String, PathBuf>,
) -> Result<MachineSpecInput, StateError> {
    let mut tasks = Vec::with_capacity(doc.tasks.len());
    for t in &doc.tasks {
        let mut deps = Vec::with_capacity(t.depends_on.len());
        for raw_dep in &t.depends_on {
            deps.push(parse_task_dependency(
                raw_dep,
                primary_workspace,
                run_id,
                linked_workspaces,
                spec_repos,
            )?);
        }
        let subtasks = t
            .subtasks
            .iter()
            .map(|s| MachineSubtaskInput {
                subtask_ref: s.subtask_ref.clone(),
                title: s.title.clone(),
                status: s.status,
                evidence: s.evidence.clone(),
            })
            .collect();
        tasks.push(MachineTaskSpec {
            task_ref: t.task_ref.clone(),
            tags: t.tags.clone(),
            scope: t.scope.clone(),
            extended_scope: t.extended_scope.clone(),
            invariants: t.invariants.clone(),
            depends_on: deps,
            subtasks,
        });
    }
    Ok(MachineSpecInput {
        tasks,
        spec_digest: doc.digest.clone(),
    })
}

pub(super) fn normalize_spec_relpath(raw: &str) -> Result<String, StateError> {
    let normalized = acdd_core::path_policy::normalize_rel_path(raw).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec_path '{raw}' must be a normalized workspace-relative path"),
        )
    })?;
    Ok(normalized)
}

pub(super) fn derive_run_name(spec_id: &str, fallback: &str) -> Result<RunName, StateError> {
    let clean: String = spec_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(40)
        .collect();
    if let Ok(name) = RunName::new(clean) {
        return Ok(name);
    }
    RunName::new(fallback).map_err(StateError::from)
}

fn sanitize_slug(raw: &str) -> Result<String, StateError> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let slug: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() || slug.len() > 64 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid draft slug '{raw}'"),
        ));
    }
    Ok(slug)
}
