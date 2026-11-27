//! Shared task-spec conversion, worktree collision checks, and slug/path helpers for commands.

use crate::git_ops::worktree::canonicalize_macos_var;
use crate::machine::{MachineSpecInput, MachineSubtaskInput, MachineTaskSpec};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::{parse_task_dependency_with_db, resolve_spec_repositories};
use acdd_core::config::WORKTREES_DIR;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ids::{RunId, RunName};
use acdd_spec::SpecDocument;
use std::path::Path;

pub(super) fn ensure_no_managed_worktree_collision(
    primary: &Path,
    run_name: &str,
    candidate_wt: &Path,
    candidate_branch: Option<&str>,
    existing_wt: Option<&str>,
) -> Result<(), AcddError> {
    let matches_existing = existing_wt
        .is_some_and(|existing| canonicalize_macos_var(Path::new(existing)) == candidate_wt);
    let managed_wt_root = canonicalize_macos_var(&primary.join(WORKTREES_DIR));
    let collides_managed_path = candidate_wt.parent() == Some(managed_wt_root.as_path())
        && candidate_wt
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == run_name || name.starts_with(&format!("{run_name}-")));
    let collides_managed_branch = candidate_branch.is_some_and(|b| {
        b == format!("run/{run_name}") || b.starts_with(&format!("task/{run_name}-"))
    });
    if !matches_existing && (collides_managed_path || collides_managed_branch) {
        return Err(AcddError::new(
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
    config: &StateEngineConfig,
) -> Result<MachineSpecInput, AcddError> {
    let spec_repos = resolve_spec_repositories(&config.worktree, doc);
    let state_db = config.resolved_state_db_path().0;
    let local_task_refs: std::collections::BTreeSet<&str> =
        doc.tasks.iter().map(|t| t.task_ref.as_str()).collect();
    let mut tasks = Vec::with_capacity(doc.tasks.len());
    for t in &doc.tasks {
        let mut deps = Vec::with_capacity(t.depends_on.len());
        for raw_dep in &t.depends_on {
            deps.push(parse_task_dependency_with_db(
                raw_dep,
                primary_workspace,
                run_id,
                &config.linked_workspaces,
                &spec_repos,
                &local_task_refs,
                Some((Path::new(primary_workspace), &state_db)),
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

pub(super) fn normalize_spec_relpath(raw: &str) -> Result<String, AcddError> {
    let normalized = acdd_core::path_policy::normalize_rel_path(raw).ok_or_else(|| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("spec_path '{raw}' must be a normalized workspace-relative path"),
        )
    })?;
    Ok(normalized)
}

pub(super) fn derive_run_name(spec_id: &str, fallback: &str) -> Result<RunName, AcddError> {
    let clean: String = spec_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(40)
        .collect();
    if let Ok(name) = RunName::new(clean) {
        return Ok(name);
    }
    RunName::new(fallback).map_err(AcddError::from)
}
