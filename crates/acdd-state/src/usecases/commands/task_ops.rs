//! Task/node mutation and plugin handlers (`extend_scope`, `reopen`, `subtask`, `blackboard_post`, `call_plugin_tool`).

use crate::git_ops::snapshot::{
    current_scope_tree, restore_task_snapshot, save_task_snapshot, snapshot_prefix,
};
use crate::git_ops::worktree::{canonicalize_macos_var, cleanup_refs, try_lock_secondary_repo};
use crate::machine::{
    container_scope, evaluate_transition, ExtendScopeInput, ReopenInput, TransitionInput,
    TransitionOutcome,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    BlackboardRecord, BlackboardStatus,
    BlackboardTopic, IsolationMode, NodeKind, SubtaskRecord, SubtaskStatus, ValidatedObservations,
};
use crate::usecases::dto::{
    BlackboardPostReq, BlackboardPostResp, CallPluginToolReq, CallPluginToolResp, ExtendScopeReq, ExtendScopeResp, ReopenReq,
    ReopenResp, SnapshotAction, SnapshotReq, SnapshotResp, SubtaskReq, SubtaskResp,
};
use crate::usecases::lifecycle::{
    build_path_policy, ensure_blackboard_enabled, ensure_state_enabled, parse_cursor_for_snapshot, snapshot_worktree_layout,
    parse_profile_from_run, primary_workspace_root, resolve_active_run, resolve_node_worktree,
    to_machine_profile, with_repo_lock_and_maintenance, with_repo_lock_and_maintenance_unchecked,
};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::check_ancestors_no_symlinks;
use acdd_core::ports::{CompositeDomainProvider, DomainToolReq};
use std::path::Path;


pub(crate) fn handle_snapshot_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: SnapshotReq,
) -> Result<SnapshotResp, StateError> {
    use SnapshotAction::{Clear, Save};
    let invalid = |message| StateError::new(ErrorCode::ProfileSchemaInvalid, message);
    if (req.all && (req.action != Clear || req.task_ref.is_some()))
        || (req.action == Clear && req.revision.is_some())
    {
        return Err(invalid("snapshot --all requires clear without a task, and clear does not accept a revision"));
    }
    with_repo_lock_and_maintenance(config, backend, |_, locked| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let prefix = snapshot_prefix(&run.run_id, req.task_ref.as_ref());
        let clear = |prefix| cleanup_refs(locked, config.linked_workspaces.values(), &run.run_id, Some(prefix))
            .map(|refs| SnapshotResp { refs });
        if req.all {
            return clear(prefix);
        }
        let task = req.task_ref.ok_or_else(|| invalid("snapshot requires a task or --all"))?;
        let snapshot = backend.load_run_graph(&run.run_id)?.ok_or_else(|| invalid("run graph missing"))?;
        let node = snapshot.nodes.iter()
            .find(|n| n.kind == NodeKind::Task && n.task_ref.as_ref() == Some(&task))
            .ok_or_else(|| invalid("snapshot task does not exist"))?;
        if req.action == Clear {
            return clear(prefix);
        }
        if node.isolation == IsolationMode::Worktree && node.worktree.is_none() {
            return Err(invalid("snapshot task has no assigned worktree"));
        }
        let literal = node.worktree.as_deref().or(run.worktree.as_deref())
            .map(Path::new).unwrap_or(&config.worktree);
        check_ancestors_no_symlinks(literal, Path::new(".git"))
            .map_err(|err| StateError::new(ErrorCode::ExternalGitDrift, err.to_string()))?;
        let worktree = resolve_node_worktree(config, &run, node, None);
        let target = snapshot_worktree_layout(config, locked, &worktree)?;
        let _guard = try_lock_secondary_repo(locked, &target)?;
        let scope = container_scope(&snapshot, node);
        let policy = build_path_policy(config, backend, &[])?;
        let reference = if req.action == Save {
            let rev = req.revision.unwrap_or(node.claim_revision);
            let reference = format!("{prefix}r{rev}");
            let tree = current_scope_tree(&worktree, Some(&scope), &policy)?;
            save_task_snapshot(&worktree, &reference, &tree)?;
            reference
        } else {
            restore_task_snapshot(&worktree, &prefix, req.revision, &scope, &policy)?
        };
        Ok(SnapshotResp { refs: vec![reference] })
    })
}

pub(crate) fn handle_extend_scope_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ExtendScopeReq,
) -> Result<ExtendScopeResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let profile = parse_profile_from_run(&snapshot.run)?;
        let machine_profile = to_machine_profile(&profile)?;
        let cursor = parse_cursor_for_snapshot(&snapshot, &req.node)?;
        let policy = build_path_policy(config, backend, &[])?;
        let obs = ValidatedObservations {
            path_policy: Some(policy),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &snapshot,
            &machine_profile,
            &obs,
            &TransitionInput::ExtendScope(ExtendScopeInput {
                node_cursor: cursor.clone(),
                paths: req.paths,
                reason: req.reason,
                worker_id: req.worker_id,
            }),
        )?;
        let TransitionOutcome::Plan(plan) = outcome else {
            unreachable!();
        };

        let contract_revision = plan
            .node_patches
            .first()
            .and_then(|p| p.contract_revision)
            .unwrap_or(1);
        let mut admitted_scope = Vec::new();
        let node_id = cursor.to_node_id()?;
        if let Some(orig) = snapshot.nodes.iter().find(|n| n.node_id == node_id) {
            let mut merged = container_scope(&snapshot, orig);
            if let Some(ext) = plan
                .node_patches
                .first()
                .and_then(|p| p.extended_scope.as_ref())
            {
                for e in ext {
                    if !merged.contains(&e.path) {
                        merged.push(e.path.clone());
                    }
                }
            }
            admitted_scope = merged;
        }

        exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;

        Ok(ExtendScopeResp {
            run_id: snapshot.run.run_id,
            node_cursor: cursor,
            contract_revision,
            admitted_scope,
        })
    })
}

pub(crate) fn handle_reopen_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ReopenReq,
) -> Result<ReopenResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let profile = parse_profile_from_run(&snapshot.run)?;
        let machine_profile = to_machine_profile(&profile)?;
        let cursor = parse_cursor_for_snapshot(&snapshot, &req.node)?;
        let obs = ValidatedObservations {
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &snapshot,
            &machine_profile,
            &obs,
            &TransitionInput::Reopen(ReopenInput {
                node_cursor: cursor.clone(),
                cascade: req.cascade,
                reason: req.reason.clone(),
            }),
        )?;
        let TransitionOutcome::Plan(mut plan) = outcome else {
            unreachable!();
        };

        if let Some(ref reason) = req.reason {
            if !reason.trim().is_empty() && config.modules.is_enabled("blackboard") {
                plan.new_blackboard.push(BlackboardRecord {
                    id: None,
                    run_id: snapshot.run.run_id.clone(),
                    node_id: Some(cursor.to_node_id()?),
                    recipient_node_id: None,
                    subtask_ref: None,
                    gate: None,
                    author: "operator".to_owned(),
                    topic: BlackboardTopic::Notes,
                    status: BlackboardStatus::Open,
                    payload: format!("Reopened {cursor}: {}", reason.trim()),
                    created_at: obs.now_utc.clone(),
                });
            }
        }

        let affected_nodes: Vec<String> = plan
            .node_patches
            .iter()
            .map(|p| p.node_id.clone())
            .collect();
        exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;

        Ok(ReopenResp {
            run_id: snapshot.run.run_id,
            node_cursor: cursor,
            affected_nodes,
        })
    })
}

pub(crate) fn handle_subtask_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: SubtaskReq,
) -> Result<SubtaskResp, StateError> {
    ensure_state_enabled(config)?;
    if matches!(req.status, SubtaskStatus::Completed | SubtaskStatus::Cancelled)
        && req
            .evidence
            .as_deref()
            .is_none_or(|e| e.trim().is_empty())
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "subtask '{}' status '{}' requires non-empty evidence",
                req.subtask_ref, req.status
            ),
        ));
    }

    with_repo_lock_and_maintenance_unchecked(config, backend, |_primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let task_node_id = format!("task:{}", req.task_ref);
        if !snapshot.nodes.iter().any(|n| n.node_id == task_node_id) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("task '{}' does not exist in run '{}'", req.task_ref, run.run_id),
            ));
        }

        let existing = snapshot
            .subtasks
            .iter()
            .find(|s| s.node_id == task_node_id && s.subtask_ref == req.subtask_ref);
        let (seq_order, title) = match existing {
            Some(s) => (s.seq_order, s.title.clone()),
            None => {
                let next_seq = snapshot
                    .subtasks
                    .iter()
                    .filter(|s| s.node_id == task_node_id)
                    .map(|s| s.seq_order + 1)
                    .max()
                    .unwrap_or(0);
                (next_seq, req.subtask_ref.as_str().to_owned())
            }
        };

        let record = SubtaskRecord {
            run_id: snapshot.run.run_id,
            node_id: task_node_id,
            subtask_ref: req.subtask_ref,
            seq_order,
            title,
            status: req.status,
            evidence: req.evidence,
            updated_at: acdd_core::telemetry::now_utc_iso_ms(),
        };

        exec_tx_with(backend, |tx| tx.upsert_subtask(&record))?;
        Ok(SubtaskResp { subtask: record })
    })
}

pub(crate) fn handle_blackboard_post_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: BlackboardPostReq,
) -> Result<BlackboardPostResp, StateError> {
    ensure_blackboard_enabled(config)?;
    if req.author.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "blackboard post invalid: empty 'author' (expected non-empty worker/agent identifier)",
        ));
    }
    if req.payload.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "blackboard post invalid: empty 'payload' (expected non-empty message text or JSON payload)",
        ));
    }

    with_repo_lock_and_maintenance_unchecked(config, backend, |_primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;

        let node_id = req
            .node
            .as_deref()
            .map(|raw| parse_cursor_for_snapshot(&snapshot, raw)?.to_node_id().map_err(StateError::from))
            .transpose()?;
        let recipient_node_id = req
            .recipient_node
            .as_deref()
            .map(|raw| parse_cursor_for_snapshot(&snapshot, raw)?.to_node_id().map_err(StateError::from))
            .transpose()?;

        let item = BlackboardRecord {
            id: None,
            run_id: snapshot.run.run_id,
            node_id,
            recipient_node_id,
            subtask_ref: req.subtask_ref,
            gate: req.gate,
            author: req.author,
            topic: req.topic,
            status: BlackboardStatus::Open,
            payload: req.payload,
            created_at: acdd_core::telemetry::now_utc_iso_ms(),
        };

        let stored = exec_tx_with(backend, |tx| tx.insert_blackboard(&item))?;
        Ok(BlackboardPostResp { entry: stored })
    })
}

pub(crate) fn handle_call_plugin_tool_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: CallPluginToolReq,
) -> Result<CallPluginToolResp, StateError> {
    if req.run.is_some() || req.node.is_some() {
        ensure_state_enabled(config)?;
    }
    let primary = primary_workspace_root(config);

    let (run_id, node_cursor, worktree) = if req.run.is_some() || req.node.is_some() {
        let run = resolve_active_run(
            config,
            backend,
            req.run.as_deref(),
            req.worktree.as_deref(),
        )?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let cursor = req
            .node
            .as_deref()
            .map(|raw| parse_cursor_for_snapshot(&snapshot, raw))
            .transpose()?;
        let wt = if let Some(ref c) = cursor {
            let node_id = c.to_node_id()?;
            let node = snapshot
                .nodes
                .iter()
                .find(|n| n.node_id == node_id)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("node '{c}' does not exist in run '{}'", run.run_id),
                    )
                })?;
            if req.worktree.is_none() && node.worktree.is_none() {
                if let Some(parent_wt) = node
                    .parent_id
                    .as_deref()
                    .and_then(|pid| snapshot.nodes.iter().find(|n| n.node_id == pid))
                    .and_then(|p| p.worktree.as_deref())
                {
                    Some(canonicalize_macos_var(Path::new(parent_wt)))
                } else {
                    Some(resolve_node_worktree(config, &snapshot.run, node, None))
                }
            } else {
                Some(resolve_node_worktree(
                    config,
                    &snapshot.run,
                    node,
                    req.worktree.as_deref(),
                ))
            }
        } else {
            req.worktree
                .map(|p| canonicalize_macos_var(&p))
                .or_else(|| run.worktree.as_deref().map(|p| canonicalize_macos_var(Path::new(p))))
        };
        (Some(run.run_id), cursor, wt)
    } else {
        (
            None,
            None,
            req.worktree.map(|p| canonicalize_macos_var(&p)),
        )
    };

    let tool_req = DomainToolReq {
        workspace_root: primary,
        worktree,
        run_id,
        node_cursor,
    };

    let output = provider.call_tool(&req.tool_name, &req.args, &tool_req).map_err(|err| {
        if let Some(se) = StateError::from_anyhow(&err) {
            se.clone()
        } else {
            StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("plugin tool '{}' failed: {err}", req.tool_name),
            )
        }
    })?;

    Ok(CallPluginToolResp {
        tool_name: req.tool_name,
        output,
    })
}
