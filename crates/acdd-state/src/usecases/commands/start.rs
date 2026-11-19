//! `start` command handler and initial run worktree/dependency resolution.

use crate::git_ops::commit::{current_branch_name, current_head_commit};
use crate::git_ops::worktree::{
    canonicalize_macos_var, create_run_worktree, remove_worktree_and_branch, CreateWorktreeReq,
    RepoWorktreeLayout,
};
use crate::machine::{
    build_initial_run_graph, compute_schedule,
    BuildGraphInput,
};
use crate::profile::resolve_optional_profile_ref_with_dirs;
use crate::spec::{
    materialize_ephemeral_goal_spec_with_options, parse_spec, SpecStatus,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot, ValidatedObservations, WorktreeLifecycle,
};
use crate::usecases::dto::{
    StartReq, StartResp,
};
use crate::usecases::lifecycle::{
    append_trace_event, apply_effective_isolation, collect_external_satisfied_deps,
    effective_spec_collections, ensure_state_enabled, generate_run_id, match_spec_collection,
    read_confined_utf8, resume_pending_tombstone_cleanups_dyn, to_machine_profile,
    validate_profile_fail_closed, verify_run_level_depends_on,
    with_repo_lock_and_maintenance_unchecked,
};
use crate::usecases::StateEngineConfig;
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::telemetry::TraceId;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId, RunName, TaskRef};
use acdd_core::ports::CompositeDomainProvider;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    build_machine_spec_input, derive_run_name, ensure_no_managed_worktree_collision,
    normalize_spec_relpath,
};

pub(crate) fn handle_start_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: StartReq,
) -> Result<StartResp, StateError> {
    ensure_state_enabled(config)?;
    if req.spec_path.is_some() == req.goal.is_some() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "start requires exactly one of 'spec_path' or 'goal'",
        ));
    }
    if req.in_place && (req.worktree || req.worktree_path.is_some()) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "conflicting isolation flags: cannot combine '--in-place' with '--worktree' or '--worktree-path'",
        ));
    }

    with_repo_lock_and_maintenance_unchecked(config, backend, |primary, layout| {
        start_holding_repo_lock(config, backend, provider, layout, primary, req)
    })
}

fn start_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    req: StartReq,
) -> Result<StartResp, StateError> {
    let _ = resume_pending_tombstone_cleanups_dyn(config, backend, layout)?;

    let collections = effective_spec_collections(config);
    let created_at = acdd_core::telemetry::now_utc_iso_ms();
    let trace_id = TraceId::new().to_string();

    let (
        run_id,
        run_name,
        collection,
        spec_path_rel,
        is_ephemeral,
        spec_doc,
        mut resolved_profile,
        materialized_spec_path,
        ephemeral_spec_to_write,
    ) = if let Some(ref raw_spec_path) = req.spec_path {
        let norm_rel = normalize_spec_relpath(raw_spec_path)?;
        let raw_content = read_confined_utf8(primary, &norm_rel)?;
        let doc = parse_spec(&raw_content)?;
        if matches!(doc.status, SpecStatus::Completed | SpecStatus::Cancelled) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "cannot start run from spec '{}' with terminal status '{:?}'",
                    doc.id, doc.status
                ),
            ));
        }
        verify_run_level_depends_on(config, backend, &doc)?;

        let col = match_spec_collection(&collections, &norm_rel);
        let col_profile = col
            .as_deref()
            .and_then(|k| collections.get(k))
            .and_then(|c| c.profile.as_deref());
        let profile_ref = req
            .profile
            .as_deref()
            .or(doc.profile.as_deref())
            .or(col_profile);

        let resolved = resolve_optional_profile_ref_with_dirs(
            profile_ref,
            Some(primary),
            Some(&config.modules),
            Some(&config.profiles_dirs),
        )?;
        validate_profile_fail_closed(config, &resolved, provider, true)?;

        let rid = generate_run_id(&doc.id)?;
        let rname = match req.run_name.clone() {
            Some(name) => name,
            None => derive_run_name(&doc.id, rid.as_str())?,
        };
        let sp_display = norm_rel.clone();
        (
            rid,
            rname,
            col,
            Some(norm_rel),
            false,
            doc,
            resolved,
            Some(sp_display),
            None,
        )
    } else {
        let goal_text = req.goal.as_deref().unwrap_or("");
        let resolved = resolve_optional_profile_ref_with_dirs(
            req.profile.as_deref(),
            Some(primary),
            Some(&config.modules),
            Some(&config.profiles_dirs),
        )?;
        validate_profile_fail_closed(config, &resolved, provider, true)?;

        let rid = generate_run_id("goal")?;
        let rname = match req.run_name.clone() {
            Some(name) => name,
            None => RunName::new(rid.as_str())?,
        };
        let has_tasks_block = resolved.gates.iter().any(|g| g.tasks.is_some());
        if !has_tasks_block {
            for pat in &req.scope {
                if !acdd_core::path_policy::is_valid_scope_glob(pat) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("goal run has invalid or non-normalized scope glob '{pat}'"),
                    ));
                }
            }
        }
        let rendered_spec = materialize_ephemeral_goal_spec_with_options(
            &rid,
            &rname,
            goal_text,
            &resolved.profile_id,
            &req.scope,
            &[],
            has_tasks_block,
        )?;
        let eph_rel = format!(".acdd/run/{}/spec.md", rid);
        let eph_abs = checked_child(primary, Path::new(&eph_rel)).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid ephemeral spec path '{eph_rel}': {err}"),
            )
        })?;
        let doc = parse_spec(&rendered_spec)?;
        (
            rid,
            rname,
            None,
            None,
            true,
            doc,
            resolved,
            Some(eph_rel),
            Some((eph_abs, rendered_spec)),
        )
    };

    crate::validate::validate_spec_for_run(
        config,
        backend,
        provider,
        &spec_doc,
        &resolved_profile,
    )?;

    let effective_isolation = if req.in_place {
        IsolationMode::InPlace
    } else if req.worktree || req.worktree_path.is_some() {
        IsolationMode::Worktree
    } else {
        resolved_profile.git.isolation
    };
    let effective_worktrees = if effective_isolation == IsolationMode::InPlace {
        WorktreeLifecycle::Manual
    } else if req.worktree || req.worktree_path.is_some() {
        WorktreeLifecycle::Managed
    } else {
        resolved_profile.git.worktrees
    };
    apply_effective_isolation(&mut resolved_profile, effective_isolation, effective_worktrees);

    let resolved_yaml = serde_yaml::to_string(&resolved_profile).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize profile.resolved.yaml: {err}"),
        )
    })?;
    let profile_snapshot_rel = format!(".acdd/run/{}/profile.resolved.yaml", run_id.as_str());
    let profile_snapshot_path =
        checked_child(primary, Path::new(&profile_snapshot_rel)).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid profile snapshot path '{profile_snapshot_rel}': {err}"),
            )
        })?;

    let target_base = match req.target_base {
        Some(ref b) => Some(b.clone()),
        None if layout.is_some() => current_branch_name(primary)?,
        None => None,
    };
    let target_base_commit = if layout.is_some() {
        current_head_commit(primary).ok()
    } else {
        None
    };

    let (planned_worktree, planned_branch, managed_wt_req) = plan_start_worktree(
        config,
        primary,
        layout.is_some(),
        &run_name,
        effective_isolation,
        effective_worktrees,
        &req,
        target_base.as_deref(),
        spec_path_rel.as_deref(),
    )?;

    let machine_profile = to_machine_profile(&resolved_profile)?;

    let mut spec_repos = BTreeMap::new();
    for r in &spec_doc.repositories {
        spec_repos.insert(r.name.clone(), PathBuf::from(&r.path));
    }
    let primary_str = primary.to_string_lossy().into_owned();
    let machine_spec = build_machine_spec_input(
        &spec_doc,
        &primary_str,
        &run_id,
        &config.linked_workspaces,
        &spec_repos,
    )?;

    let mut graph = build_initial_run_graph(
        &machine_profile,
        &BuildGraphInput {
            run_id: run_id.clone(),
            run_name,
            collection,
            workspace: primary_str,
            spec_path: spec_path_rel,
            is_ephemeral,
            target_base,
            target_base_commit,
            branch: planned_branch,
            worktree: planned_worktree,
            trace_id,
            created_at,
            spec: machine_spec,
        },
    )?;
    graph.run.spec_id = Some(spec_doc.id.clone());

    // Check external dependencies at start so already-completed external tasks unlock wave-0 tasks immediately.
    let ext_satisfied =
        collect_external_satisfied_deps(config, &run_id, &graph.dependencies)?;
    apply_initial_external_dep_satisfaction(&mut graph, &ext_satisfied);

    // Create managed run worktree (if requested) ONLY after all spec/profile/graph validation succeeds,
    // and roll back worktree/branch/disk artifacts cleanly if SQL persistence fails.
    let created_managed = if let Some(ref wt_req) = managed_wt_req {
        let created = create_run_worktree(wt_req)?;
        graph.run.worktree = Some(created.worktree.to_string_lossy().into_owned());
        graph.run.branch = Some(created.branch.clone());
        Some((created.worktree, created.branch))
    } else {
        None
    };

    if let Err(err) = exec_tx_with(backend, |tx| tx.insert_run_graph(&graph)) {
        if let Some((wt, br)) = created_managed {
            let _ = remove_worktree_and_branch(primary, Some(&wt), Some(&br), true);
        }
        return Err(err);
    }

    if let Some((eph_abs, rendered_spec)) = ephemeral_spec_to_write {
        atomic_write(&eph_abs, rendered_spec.as_bytes(), false).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to materialize ephemeral spec '{}': {err}", eph_abs.display()),
            )
        })?;
    }
    atomic_write(&profile_snapshot_path, resolved_yaml.as_bytes(), false).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "failed to write resolved profile snapshot '{}': {err}",
                profile_snapshot_path.display()
            ),
        )
    })?;

    let obs = ValidatedObservations {
        external_satisfied_deps: ext_satisfied,
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };
    let schedule = compute_schedule(&graph, &obs)?;
    let mut ready_nodes = Vec::new();
    for nid in &schedule.ready_root_nodes {
        ready_nodes.push(NodeCursor::from_node_id(nid)?.to_string());
    }
    for tref in &schedule.ready_tasks {
        let parent_id = format!("task:{tref}");
        if let Some(g) = graph
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.parent_id.as_deref() == Some(parent_id.as_str())
                    && n.status == NodeStatus::Ready
            })
            .min_by_key(|n| n.seq_order)
        {
            ready_nodes.push(NodeCursor::from_node_id(&g.node_id)?.to_string());
        }
    }

    let _ = append_trace_event(
        layout,
        primary,
        &run_id,
        &json!({
            "kind": "run_start",
            "run_id": run_id.as_str(),
            "run_name": graph.run.run_name.as_str(),
            "profile_id": graph.run.profile_id,
            "isolation": graph.run.isolation.as_str(),
            "is_ephemeral": graph.run.is_ephemeral,
            "timestamp": graph.run.created_at,
        }),
    );

    Ok(StartResp {
        run: graph.run,
        ready_nodes,
        materialized_spec_path,
    })
}

type PlannedStartWorktree = (Option<String>, Option<String>, Option<CreateWorktreeReq>);

#[allow(clippy::too_many_arguments)]
fn plan_start_worktree(
    config: &StateEngineConfig,
    primary: &Path,
    has_git: bool,
    run_name: &RunName,
    isolation: IsolationMode,
    worktrees: WorktreeLifecycle,
    req: &StartReq,
    target_base: Option<&str>,
    spec_path_rel: Option<&str>,
) -> Result<PlannedStartWorktree, StateError> {
    if isolation == IsolationMode::InPlace {
        let target_wt =
            canonicalize_macos_var(req.worktree_path.as_deref().unwrap_or(&config.worktree));
        let branch = if has_git && target_wt.exists() {
            current_branch_name(&target_wt)?
        } else {
            None
        };
        return Ok((Some(target_wt.to_string_lossy().into_owned()), branch, None));
    }

    if let Some(ref explicit) = req.worktree_path {
        let canon = canonicalize_macos_var(explicit);
        let branch = if has_git && canon.exists() {
            current_branch_name(&canon)?
        } else {
            None
        };
        if worktrees == WorktreeLifecycle::Managed {
            ensure_no_managed_worktree_collision(
                primary,
                run_name.as_str(),
                &canon,
                branch.as_deref(),
                None,
            )?;
        }
        return Ok((Some(canon.to_string_lossy().into_owned()), branch, None));
    }

    if worktrees == WorktreeLifecycle::Managed && has_git {
        let wt_path = primary.join(".worktrees").join(run_name.as_str());
        let canon_wt = canonicalize_macos_var(&wt_path);
        let branch_name = format!("run/{run_name}");
        let base_ref = target_base.unwrap_or("HEAD").to_owned();
        let copy_paths: Vec<String> = spec_path_rel.into_iter().map(str::to_owned).collect();
        return Ok((
            Some(canon_wt.to_string_lossy().into_owned()),
            Some(branch_name.clone()),
            Some(CreateWorktreeReq {
                primary_worktree: primary.to_path_buf(),
                worktree_path: wt_path,
                branch_name,
                base_ref,
                copy_uncommitted_paths: copy_paths,
            }),
        ));
    }

    let caller_wt = canonicalize_macos_var(&config.worktree);
    let branch = if has_git {
        current_branch_name(&caller_wt)?
    } else {
        None
    };
    Ok((Some(caller_wt.to_string_lossy().into_owned()), branch, None))
}

fn apply_initial_external_dep_satisfaction(
    graph: &mut RunGraphSnapshot,
    ext_satisfied: &[(String, RunId, TaskRef)],
) {
    for dep in &mut graph.dependencies {
        if ext_satisfied.iter().any(|(ws, rid, tref)| {
            ws == &dep.depends_on_workspace
                && rid == &dep.depends_on_run_id
                && tref == &dep.depends_on_ref
        }) {
            dep.satisfied = true;
        }
    }

    let pre_ok = crate::machine::schedule::pre_task_barrier_satisfied(graph);
    if !pre_ok {
        return;
    }

    let newly_ready_tasks: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Pending)
        .filter_map(|n| {
            let tref = n.task_ref.as_ref()?;
            let all_ok = graph
                .dependencies
                .iter()
                .filter(|d| &d.task_ref == tref)
                .all(|d| d.satisfied);
            all_ok.then(|| n.node_id.clone())
        })
        .collect();

    for task_node_id in newly_ready_tasks {
        if let Some(t) = graph.nodes.iter_mut().find(|n| n.node_id == task_node_id) {
            t.status = NodeStatus::Ready;
        }
        if let Some(first_gate) = graph
            .nodes
            .iter_mut()
            .filter(|n| n.parent_id.as_deref() == Some(task_node_id.as_str()))
            .min_by_key(|n| n.seq_order)
        {
            if first_gate.status == NodeStatus::Pending {
                first_gate.status = NodeStatus::Ready;
            }
        }
    }
}
