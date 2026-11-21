//! Task/node mutation and plugin handlers (`extend_scope`, `reopen`, `subtask`, `blackboard_post`, `call_plugin_tool`).

use crate::git_ops::snapshot::{
    current_scope_tree, list_snapshot_refs, parse_canonical_revision, parse_older_than, restore_task_snapshot, save_task_snapshot, snapshot_prefix, validate_snapshot_label,
};
use crate::git_ops::worktree::{canonicalize_macos_var, cleanup_locked_refs, lock_cleanup_repos, resolve_repo_layout, try_lock_secondary_repo};
use crate::machine::{
    container_scope, evaluate_transition, ExtendScopeInput, ReopenInput, TransitionInput,
    TransitionOutcome,
};
use crate::store::traits::{exec_tx_with, StateBackend, StateReader};
use crate::types::{
    BlackboardRecord, BlackboardStatus,
    BlackboardTopic, IsolationMode, NodeKind, SubtaskRecord, SubtaskStatus, ValidatedObservations,
};
use crate::usecases::dto::{
    BlackboardPostReq, BlackboardPostResp, CallPluginToolReq, CallPluginToolResp, ExtendScopeReq, ExtendScopeResp, ReopenReq,
    ReopenResp, SnapshotAction, SnapshotReq, SnapshotResp, SubtaskReq, SubtaskResp,
};
use crate::usecases::lifecycle::{
    acquire_repo_lock, build_path_policy, ensure_blackboard_enabled, ensure_state_enabled,
    load_active_run_graph, parse_cursor_for_snapshot, snapshot_worktree_layout,
    primary_workspace_root, resolve_active_run, resolve_node_worktree, resolve_repo_layout_optional, resolve_run_optional,
    with_repo_lock_and_maintenance, with_repo_lock_and_maintenance_unchecked,
};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::check_ancestors_no_symlinks;
use acdd_core::ids::RunId;
use acdd_core::ports::{CompositeDomainProvider, DomainToolReq};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn handle_snapshot_dyn(
    config: &StateEngineConfig, reader: &dyn StateReader, req: SnapshotReq,
) -> Result<SnapshotResp, StateError> {
    use SnapshotAction::{Clear, List, Restore, Save};
    let bad = |m: &str| StateError::new(ErrorCode::ProfileSchemaInvalid, m.to_owned());
    check_ancestors_no_symlinks(&config.workspace_root, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    let primary = primary_workspace_root(config); check_ancestors_no_symlinks(&primary, Path::new(".git/acdd/repo.lock")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
    let target_str = req.task_ref.as_ref().map(|t| t.as_str().to_owned()).or_else(|| req.name.clone());
    let layout = resolve_repo_layout_optional(&primary)?;
    let primary_lock = req.run.is_some().then(|| acquire_repo_lock(layout.as_ref())).transpose()?.flatten();
        let explicit = req.run.is_some(); let run = if explicit { Some(resolve_active_run(config, reader, req.run.as_deref(), None)?) } else {
            match resolve_run_optional(config, reader, None, None, true) {
                Ok(run) => run, Err(err) if err.code == ErrorCode::RunSelectorAmbiguous => None, Err(err) => return Err(err),
            }
        };
        let adhoc_only = !explicit && req.action == Clear && (req.older_than.is_some() || req.revision.is_some());
        let graph = run.as_ref().map(|run| reader.load_run_graph(&run.run_id)).transpose()?.flatten();
        if (explicit || (run.is_some() && !adhoc_only)) && graph.is_none() { return Err(bad("run graph missing")); }
        let mut picked = (!adhoc_only).then_some(target_str.as_deref()).flatten().and_then(|want| graph.as_ref()?.nodes.iter().position(|n| n.kind == NodeKind::Task && n.task_ref.as_ref().is_some_and(|t| t.as_str() == want)));
        if explicit && target_str.is_some() {
            let raw = target_str.as_deref().unwrap();
            acdd_core::ids::TaskRef::new(raw).map_err(|e| bad(&e.to_string()))?;
            if picked.is_none() { return Err(bad("snapshot task does not exist")); }
        }
        let (mut run, mut graph) = (run, graph); let primary_lock = if !adhoc_only && picked.is_some() && primary_lock.is_none() { let lock = acquire_repo_lock(layout.as_ref())?; run = Some(resolve_active_run(config, reader, req.run.as_deref(), None)?); graph = run.as_ref().map(|run| reader.load_run_graph(&run.run_id)).transpose()?.flatten(); picked = (!adhoc_only).then_some(target_str.as_deref()).flatten().and_then(|want| graph.as_ref()?.nodes.iter().position(|n| n.kind == NodeKind::Task && n.task_ref.as_ref().is_some_and(|t| t.as_str() == want))); lock } else { primary_lock }; let task_run = (!adhoc_only && (explicit || picked.is_some())).then(|| run.clone()).flatten();
        let (prefix, task_node, scope) = if let Some(run) = task_run.as_ref() {
            let g = graph.as_ref().ok_or_else(|| bad("run graph missing"))?;
            match picked {
                Some(i) => { let node = g.nodes[i].clone(); (snapshot_prefix(&run.run_id, node.task_ref.as_ref()), Some(node), Some(container_scope(g, &g.nodes[i]))) }
                None => (snapshot_prefix(&run.run_id, None), None, None),
            }
        } else if let Some(name) = &target_str {
            validate_snapshot_label(name)?;
            (format!("refs/acdd/snapshots/_adhoc/{name}/"), None, None)
        } else { ("refs/acdd/snapshots/_adhoc/".into(), None, None) };
        if task_run.is_none() { drop(primary_lock); }
        if (matches!(req.action, Save | Restore) && target_str.is_none()) || (explicit && matches!(req.action, Save | Restore) && picked.is_none())
            || (req.all && (req.action != Clear || target_str.is_some())) || (req.action == Clear && target_str.is_none() && !req.all && req.revision.is_none() && req.older_than.is_none())
        { return Err(bad("snapshot flags do not match save, restore, list, or clear")); }
        let io = matches!(req.action, Save | Restore);
        let worktree = if let (Some(run), Some(node)) = (&task_run, &task_node) {
            if io && node.isolation == IsolationMode::Worktree && node.worktree.is_none() { return Err(bad("snapshot task has no assigned worktree")); }
            let literal = node.worktree.as_deref().or(run.worktree.as_deref()).map(Path::new).unwrap_or(&config.worktree);
            check_ancestors_no_symlinks(literal, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?;
            if let Some(raw) = req.worktree.as_deref() { check_ancestors_no_symlinks(raw, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?; }
            let assigned = resolve_node_worktree(config, run, node, None);
            if req.worktree.as_ref().is_some_and(|path| canonicalize_macos_var(path) != assigned) { return Err(StateError::new(ErrorCode::ExternalGitDrift, "explicit worktree does not match assigned task worktree")); }
            if io || req.action == List { assigned } else { req.worktree.clone().unwrap_or_else(|| config.worktree.clone()) }
        } else {
            let raw = req.worktree.as_deref().map(Path::new).unwrap_or(&config.worktree);
            check_ancestors_no_symlinks(raw, Path::new(".git")).map_err(|e| StateError::new(ErrorCode::ExternalGitDrift, e.to_string()))?; raw.to_path_buf()
        };
        let target = if (io || req.action == List) && task_node.is_some() { snapshot_worktree_layout(config, layout.as_ref(), &worktree)? } else { resolve_repo_layout(&worktree)? };
        if (req.worktree.is_some() || task_node.is_none()) && canonicalize_macos_var(&worktree) != target.target_worktree {
            return Err(StateError::new(ErrorCode::ExternalGitDrift, "snapshot worktree must be a git worktree root".to_owned()));
        }
        let wt = target.target_worktree.clone();
        let _standalone_guard = (task_run.is_none() && req.action != Clear).then(|| acquire_repo_lock(Some(&target))).transpose()?.flatten();
        let _held = (task_run.is_some() && req.action != Clear).then(|| try_lock_secondary_repo(layout.as_ref(), &target)).transpose()?.flatten();
        if req.action == List {
            let refs = list_snapshot_refs(&wt, &prefix)?.into_iter().map(|(r, _)| r).filter(|r| {
                let tail = match r.strip_prefix(&prefix) { Some(t) => t, None => return false };
                let (valid_label, rev) = match target_str { Some(_) => (true, parse_canonical_revision(tail, "")), None => { let mut p = tail.split('/'); match (p.next(), p.next(), p.next()) { (Some(label), Some(r), None) => (validate_snapshot_label(label).is_ok(), parse_canonical_revision(r, "")), _ => (false, None) } } };
                valid_label && rev.is_some_and(|rev| req.revision.is_none_or(|want| want == rev))
            }).collect();
            return Ok(SnapshotResp { refs });
        }
        if req.action == Clear {
            let exact = req.revision.filter(|_| target_str.is_some()).map(|rev| format!("{prefix}r{rev}"));
            let suffix = req.revision.filter(|_| target_str.is_none()).map(|rev| format!("/r{rev}"));
            let cutoff = req.older_than.as_deref().map(|raw| {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| bad(&e.to_string()))?.as_secs();
                now.checked_sub(parse_older_than(raw)?).filter(|c| *c <= i64::MAX as u64).ok_or_else(|| bad("duration exceeds unix epoch")).map(|c| c as i64)
            }).transpose()?;
            if task_run.is_some() && req.worktree.is_some() { snapshot_worktree_layout(config, layout.as_ref(), &wt)?; }
            let repos = if task_run.is_none() { lock_cleanup_repos(None, std::iter::once(&wt))? } else { lock_cleanup_repos(layout.as_ref(), config.linked_workspaces.values())? };
            return Ok(SnapshotResp { refs: cleanup_locked_refs(&repos, &RunId::new("adhoc").expect("id"), Some(prefix), exact.as_deref(), suffix.as_deref(), cutoff)? });
        }
        let policy = build_path_policy(
            config,
            reader,
            &[],
            task_run.as_ref(),
            Some(&wt),
        )?;
        let reference = if req.action == Save {
            let rev = if task_node.is_none() && req.revision.is_none() {
                list_snapshot_refs(&wt, &prefix)?.iter().filter_map(|(n, _)| parse_canonical_revision(n, &prefix)).max().map(|n| n.checked_add(1).ok_or_else(|| bad("snapshot revision overflow"))).transpose()?.unwrap_or(1)
            } else { req.revision.unwrap_or_else(|| task_node.as_ref().map(|n| n.claim_revision).unwrap_or(1)) };
            let reference = format!("{prefix}r{rev}");
            save_task_snapshot(&wt, &reference, &current_scope_tree(&wt, scope.as_deref(), &policy)?, task_node.is_none() && req.revision.is_none())?;
            reference
        } else { restore_task_snapshot(&wt, &prefix, req.revision, scope.as_deref(), &policy)? };
        Ok(SnapshotResp { refs: vec![reference] })
}

pub(crate) fn handle_extend_scope_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ExtendScopeReq,
) -> Result<ExtendScopeResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, _layout| {
        let loaded = load_active_run_graph(config, backend, req.run.as_deref(), None)?;
        let cursor = parse_cursor_for_snapshot(&loaded.snapshot, &req.node)?;
        let run_wt = loaded
            .snapshot
            .run
            .worktree
            .as_deref()
            .map(Path::new)
            .unwrap_or(&config.worktree);
        let policy = loaded.path_policy(config, backend, Some(run_wt))?;
        let obs = ValidatedObservations {
            path_policy: Some(policy),
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &loaded.snapshot,
            &loaded.machine_profile,
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
        if let Some(orig) = loaded.snapshot.nodes.iter().find(|n| n.node_id == node_id) {
            let mut merged = container_scope(&loaded.snapshot, orig);
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

        exec_tx_with(backend, |tx| tx.apply_transition_plan(&loaded.snapshot.run.run_id, &plan))?;

        Ok(ExtendScopeResp {
            run_id: loaded.snapshot.run.run_id,
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
        let loaded = load_active_run_graph(config, backend, req.run.as_deref(), None)?;
        let cursor = parse_cursor_for_snapshot(&loaded.snapshot, &req.node)?;
        let obs = ValidatedObservations {
            now_utc: acdd_core::telemetry::now_utc_iso_ms(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &loaded.snapshot,
            &loaded.machine_profile,
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
                    run_id: loaded.snapshot.run.run_id.clone(),
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
        exec_tx_with(backend, |tx| tx.apply_transition_plan(&loaded.snapshot.run.run_id, &plan))?;

        Ok(ReopenResp {
            run_id: loaded.snapshot.run.run_id,
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
