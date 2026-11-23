//! Blueprint `max_net_loc` enforcement and container budget override checks.

use acdd_core::git::git_command;
use crate::store::traits::StateBackend;
use crate::types::{
    BlackboardStatus,
    BlackboardTopic,
    GateNodeRecord,
    RunGraphSnapshot,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::TreeOid;
use acdd_core::path_policy::{normalize_rel_path, WorkspacePathPolicy};
use acdd_core::subsystems::SubsystemId;
use std::collections::BTreeSet;
use std::path::Path;

use super::post_save::list_all_blackboard_items;

#[allow(clippy::too_many_arguments)]
pub(super) fn enforce_blueprint_max_net_loc(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate_role: Option<&str>,
    target_worktree: &Path,
    current_tree: &TreeOid,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<(), StateError> {
    if snapshot.run.is_ephemeral {
        return Ok(());
    }
    let (Some(ref baseline_commit), Some(ref task_ref)) = (&node.baseline_commit, &node.task_ref)
    else {
        return Ok(());
    };
    let Some(max_net_loc) =
        crate::context::slices::extract_pre_task_slice(snapshot, task_ref.as_str())
            .and_then(|slice| slice.get("max_net_loc").and_then(serde_json::Value::as_i64))
    else {
        return Ok(());
    };

    let net_loc = compute_scoped_prod_net_loc(
        target_worktree,
        baseline_commit.as_str(),
        current_tree,
        admitted_scope,
        policy,
    )?;
    if net_loc <= max_net_loc {
        return Ok(());
    }

    if has_container_budget_decision(
        config,
        backend,
        snapshot,
        node,
        gate_role,
        task_ref.as_str(),
        net_loc,
    )? {
        return Ok(());
    }

    Err(StateError::new(
        ErrorCode::GateVerificationFailed,
        format!(
            "task '{task_ref}' production net LOC delta (+{net_loc}) exceeds blueprint budget max_net_loc={max_net_loc}; reuse existing canonical owners or simplify the implementation, or escalate (`submit --action escalate`) for an independent `BUDGET_OVERRIDE: <reason>` decision from architect/coordinator/operator in `acdd_blackboard` (topic: decisions)"
        ),
    ))
}

fn compute_scoped_prod_net_loc(
    worktree: &Path,
    baseline_commit: &str,
    current_tree: &TreeOid,
    admitted_scope: &[String],
    policy: &WorkspacePathPolicy,
) -> Result<i64, StateError> {
    let output = git_command(
        worktree,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            baseline_commit,
            current_tree.as_str(),
        ],
    )
    .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(StateError::new(
            ErrorCode::ExternalGitDrift,
            format!(
                "git diff --numstat {} {} failed in '{}': {stderr}",
                baseline_commit,
                current_tree,
                worktree.display()
            ),
        ));
    }
    let mut net_loc: i64 = 0;
    for raw_record in output.stdout.split(|b| *b == 0) {
        if raw_record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(raw_record);
        let mut parts = record.splitn(3, '\t');
        let (Some(added_raw), Some(deleted_raw), Some(path_raw)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if added_raw == "-" || deleted_raw == "-" {
            continue;
        }
        let Ok(added) = added_raw.parse::<i64>() else {
            continue;
        };
        let Ok(deleted) = deleted_raw.parse::<i64>() else {
            continue;
        };
        let Some(rel) = normalize_rel_path(path_raw) else {
            continue;
        };
        if !policy.is_candidate_included(&rel) {
            continue;
        }
        if !admitted_scope.is_empty() && !policy.is_scope_admitted(&rel, admitted_scope) {
            continue;
        }
        if is_test_path(&rel) {
            continue;
        }
        net_loc += added - deleted;
    }
    Ok(net_loc)
}

fn is_test_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower
        .split('/')
        .any(|seg| matches!(seg, "test" | "tests" | "__tests__"))
    {
        return true;
    }
    let file_name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    file_name == "tests.rs"
        || file_name == "test.rs"
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
        || file_name.ends_with("_test.go")
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_spec.rb")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
}

fn has_container_budget_decision(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    snapshot: &RunGraphSnapshot,
    node: &GateNodeRecord,
    gate_role: Option<&str>,
    task_ref: &str,
    net_loc: i64,
) -> Result<bool, StateError> {
    if !config.modules.is_subsystem_enabled(SubsystemId::Blackboard) {
        return Ok(false);
    }
    let container_id = node.parent_id.as_deref().unwrap_or(&node.node_id);
    let container_node_ids: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| n.node_id == container_id || n.parent_id.as_deref() == Some(container_id))
        .map(|n| n.node_id.as_str())
        .collect();
    let mut self_authors: BTreeSet<&str> = BTreeSet::new();
    if let Some(ref w) = node.worker_id {
        self_authors.insert(w.as_str());
    }
    for claim in &snapshot.claims {
        if claim.node_id == node.node_id {
            self_authors.insert(claim.worker_id.as_str());
        }
    }
    let is_self_author = |author: &str| -> bool {
        gate_role.is_some_and(|role| author.eq_ignore_ascii_case(role))
            || self_authors.contains(author)
    };

    let items = list_all_blackboard_items(config, backend, &snapshot.run.run_id)?;
    let mut saw_self_approved = None;
    for item in items {
        if item.status != BlackboardStatus::Open
            || item.topic != BlackboardTopic::Decisions
            || !item
                .node_id
                .as_deref()
                .is_some_and(|nid| container_node_ids.contains(nid))
            || !is_explicit_budget_override(&item.payload, net_loc)
        {
            continue;
        }
        if !is_self_author(item.author.trim()) {
            return Ok(true);
        }
        saw_self_approved = Some(item.author);
    }
    if let Some(author) = saw_self_approved {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "BUDGET_OVERRIDE for task '{task_ref}' (+{net_loc} net LOC) cannot be self-approved by implementer '{author}'; escalate (`submit --action escalate`) and record approval from architect, coordinator, or operator"
            ),
        ));
    }
    Ok(false)
}

fn is_explicit_budget_override(payload: &str, net_loc: i64) -> bool {
    let trimmed = payload.trim();
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if val.get("budget_override").and_then(serde_json::Value::as_bool) == Some(true) {
            return true;
        }
        if val
            .get("approved_max_net_loc")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|approved| approved >= net_loc)
        {
            return true;
        }
        return false;
    }
    trimmed.lines().any(|raw_line| {
        let line = raw_line.trim();
        let rest = ["budget_override:", "approved_budget:"]
            .iter()
            .find_map(|prefix| {
                line.get(..prefix.len())
                    .filter(|head| head.eq_ignore_ascii_case(prefix))
                    .map(|_| (prefix, line[prefix.len()..].trim()))
            });
        match rest {
            Some((&"approved_budget:", value)) => {
                if let Ok(approved) = value.parse::<i64>() {
                    approved >= net_loc
                } else {
                    !value.is_empty()
                }
            }
            Some((_, value)) => !value.is_empty(),
            None => false,
        }
    })
}
