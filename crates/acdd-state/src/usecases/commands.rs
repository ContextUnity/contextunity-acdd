//! Mutating and plugin command handlers (`Start`, `Draft`, `Claim`, `ExtendScope`, `Reopen`,
//! `Sync`, `Subtask`, `BlackboardPost`, `Snapshot`, `Rollback`, `Integrate`, `Clear`, `CallPluginTool`).

use crate::context::{build_claim_context, ClaimContextRequest};
use crate::git_ops::commit::{current_branch_name, current_head_commit, is_protected_main_branch};
use crate::git_ops::integrate::{
    abort_in_progress_git_ops, integrate_task_branch, verify_tasks_integrated, IntegrateTaskReq,
};
use crate::git_ops::snapshot::{
    capture_snapshot, current_scope_tree, restore_rollback, CaptureSnapshotReq,
    RollbackReq as GitRollbackReq,
};
use crate::git_ops::worktree::{
    canonicalize_macos_var, cleanup_run_refs, create_run_worktree, create_task_worktree,
    remove_worktree_and_branch, CreateWorktreeReq, RepoWorktreeLayout,
};
use crate::machine::graph::{build_preconditions, is_post_task_root_node};
use crate::machine::{
    build_initial_run_graph, compute_schedule, container_scope, evaluate_transition,
    BuildGraphInput, ClaimDecision, ClaimInput, ExtendScopeInput, MachineSpecInput,
    MachineSubtaskInput, MachineTaskSpec, ReopenInput, SyncInput, TransitionInput,
    TransitionOutcome,
};
use crate::observability::build_completed_run_digest;
use crate::profile::{resolve_profile_ref, ResolvedGateProfile};
use crate::spec::{
    extract_spec_number, format_next_spec_id, materialize_ephemeral_goal_spec, parse_spec,
    SpecDocument, SpecStatus,
};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    ArchiveCleanupEntry, ArtifactKind, BlackboardListReq, BlackboardRecord, BlackboardStatus,
    BlackboardTopic, GateNodeRecord, IntegrateStrategy, IsolationMode, NodeKind, NodeStatus,
    NodeStatusPatch, NodeVersionExpectation, RejectionFindings, RunGraphSnapshot,
    RunTombstoneRecord, SaveAction, StoragePreconditions, SubtaskRecord, SubtaskStatus,
    TerminalStatus, TraceStatus, TransitionPlan, ValidatedObservations, WorktreeLifecycle,
};
use crate::usecases::dto::{
    BlackboardPostReq, BlackboardPostResp, CallPluginToolReq, CallPluginToolResp, ClaimReq,
    ClearReq, ClearResp, DraftReq, DraftResp, ExtendScopeReq, ExtendScopeResp, IntegrateReq,
    IntegrateResp, ReopenReq, ReopenResp, RollbackReq, RollbackResp, SnapshotReq, SnapshotResp,
    StartReq, StartResp, SubtaskReq, SubtaskResp, SyncReq, SyncResp,
};
use crate::usecases::lifecycle::{
    append_trace_event, auto_select_ready_cursor, build_path_policy,
    collect_external_satisfied_deps, collect_integration_inputs, compute_unintegrated_tasks,
    current_session_fingerprint, effective_spec_collections, ensure_blackboard_enabled,
    ensure_state_enabled, generate_run_id, is_local_pid_alive, match_spec_collection, now_utc_iso,
    parse_cursor_for_snapshot, parse_profile_from_run, parse_task_dependency,
    primary_workspace_root, prune_old_run_trace_dirs, read_confined_utf8, resolve_active_run,
    resolve_node_worktree, resume_pending_tombstone_cleanups_dyn, sorted_dir_paths,
    to_machine_profile, validate_profile_fail_closed, verify_run_level_depends_on,
    with_repo_lock_and_maintenance, with_repo_lock_and_maintenance_unchecked,
};
use crate::usecases::queries::{
    collect_dependency_receipts, extract_pre_task_slice, find_profile_gate_def, read_spec_readonly,
};
use crate::usecases::submit::build_cleanup_manifest;
use crate::usecases::StateEngineConfig;
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::telemetry::{SpanId, TraceId};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId, RunName, TaskRef};
use acdd_core::ports::{CompositeDomainProvider, DomainToolReq};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Start a new `Run` from a spec file or ephemeral `--goal`.
pub fn handle_start<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: StartReq,
) -> Result<StartResp, StateError> {
    handle_start_dyn(config, backend, provider, req)
}

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
    let created_at = now_utc_iso();
    let trace_id = TraceId::new().to_string();

    let (
        run_id,
        run_name,
        collection,
        spec_path_rel,
        is_ephemeral,
        spec_doc,
        resolved_profile,
        materialized_spec_path,
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
            .or(col_profile)
            .unwrap_or("workspace");

        let resolved = resolve_profile_ref(profile_ref, Some(primary), Some(&config.modules))?;
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
        )
    } else {
        let goal_text = req.goal.as_deref().unwrap_or("");
        let profile_ref = req.profile.as_deref().unwrap_or("workspace");
        let resolved = resolve_profile_ref(profile_ref, Some(primary), Some(&config.modules))?;
        validate_profile_fail_closed(config, &resolved, provider, true)?;

        let rid = generate_run_id("goal")?;
        let rname = match req.run_name.clone() {
            Some(name) => name,
            None => RunName::new(rid.as_str())?,
        };
        let rendered_spec = materialize_ephemeral_goal_spec(
            &rid,
            &rname,
            goal_text,
            &resolved.profile_id,
            &req.scope,
            &[],
        )?;
        let eph_rel = format!(".acdd/run/{}/spec.md", rid);
        let eph_abs = primary.join(&eph_rel);
        atomic_write(&eph_abs, rendered_spec.as_bytes(), false).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to materialize ephemeral spec '{}': {err}", eph_abs.display()),
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
        )
    };

    // Persist resolved profile snapshot into `.acdd/run/<run_id>/profile.resolved.yaml`.
    let resolved_yaml = serde_yaml::to_string(&resolved_profile).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize profile.resolved.yaml: {err}"),
        )
    })?;
    let profile_snapshot_path = primary
        .join(".acdd/run")
        .join(run_id.as_str())
        .join("profile.resolved.yaml");
    atomic_write(&profile_snapshot_path, resolved_yaml.as_bytes(), false).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "failed to write resolved profile snapshot '{}': {err}",
                profile_snapshot_path.display()
            ),
        )
    })?;

    let effective_isolation = if req.in_place {
        IsolationMode::InPlace
    } else if req.worktree || req.worktree_path.is_some() {
        IsolationMode::Worktree
    } else {
        resolved_profile.git.isolation
    };
    let effective_worktrees = if req.in_place {
        WorktreeLifecycle::Manual
    } else if req.worktree || req.worktree_path.is_some() {
        WorktreeLifecycle::Managed
    } else {
        resolved_profile.git.worktrees
    };

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

    let (run_worktree, run_branch) = resolve_start_worktree(
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

    let mut machine_profile = to_machine_profile(&resolved_profile)?;
    machine_profile.git.isolation = effective_isolation;
    machine_profile.git.worktrees = effective_worktrees;

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
            branch: run_branch,
            worktree: run_worktree,
            trace_id,
            created_at,
            spec: machine_spec,
        },
    )?;

    // Check external dependencies at start so already-completed external tasks unlock wave-0 tasks immediately.
    let ext_satisfied =
        collect_external_satisfied_deps(config, &run_id, &graph.dependencies)?;
    apply_initial_external_dep_satisfaction(&mut graph, &ext_satisfied);

    exec_tx_with(backend, |tx| tx.insert_run_graph(&graph))?;

    let obs = ValidatedObservations {
        external_satisfied_deps: ext_satisfied,
        now_utc: now_utc_iso(),
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

#[allow(clippy::too_many_arguments)]
fn resolve_start_worktree(
    config: &StateEngineConfig,
    primary: &Path,
    has_git: bool,
    run_name: &RunName,
    isolation: IsolationMode,
    worktrees: WorktreeLifecycle,
    req: &StartReq,
    target_base: Option<&str>,
    spec_path_rel: Option<&str>,
) -> Result<(Option<String>, Option<String>), StateError> {
    if isolation == IsolationMode::InPlace {
        let branch = if has_git {
            current_branch_name(primary)?
        } else {
            None
        };
        return Ok((Some(primary.to_string_lossy().into_owned()), branch));
    }

    if let Some(ref explicit) = req.worktree_path {
        let canon = canonicalize_macos_var(explicit);
        let branch = if has_git && canon.exists() {
            current_branch_name(&canon)?
        } else {
            None
        };
        return Ok((Some(canon.to_string_lossy().into_owned()), branch));
    }

    if worktrees == WorktreeLifecycle::Managed && has_git {
        let wt_path = primary.join(".worktrees").join(run_name.as_str());
        let branch_name = format!("run/{run_name}");
        let base_ref = target_base.unwrap_or("HEAD").to_owned();
        let copy_paths: Vec<String> = spec_path_rel.into_iter().map(str::to_owned).collect();
        let created = create_run_worktree(&CreateWorktreeReq {
            primary_worktree: primary.to_path_buf(),
            worktree_path: wt_path,
            branch_name,
            base_ref,
            copy_uncommitted_paths: copy_paths,
        })?;
        return Ok((
            Some(created.worktree.to_string_lossy().into_owned()),
            Some(created.branch),
        ));
    }

    let caller_wt = canonicalize_macos_var(&config.worktree);
    let branch = if has_git {
        current_branch_name(&caller_wt)?
    } else {
        None
    };
    Ok((Some(caller_wt.to_string_lossy().into_owned()), branch))
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

/// Draft the next uniquely numbered specification document in a configured collection.
pub fn handle_draft<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: DraftReq,
) -> Result<DraftResp, StateError> {
    handle_draft_dyn(config, backend, req)
}

pub(crate) fn handle_draft_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: DraftReq,
) -> Result<DraftResp, StateError> {
    ensure_state_enabled(config)?;
    let collections = effective_spec_collections(config);
    let col = collections.get(&req.collection).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("unknown spec collection '{}'", req.collection),
        )
    })?;

    let slug = sanitize_slug(&req.slug)?;
    let prefix = match req.prefix {
        Some(ref p) => {
            if !col.prefixes.is_empty() && !col.prefixes.contains(p) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "prefix '{p}' is not allowed for collection '{}' (allowed: {:?})",
                        req.collection, col.prefixes
                    ),
                ));
            }
            p.clone()
        }
        None => col
            .prefixes
            .first()
            .cloned()
            .unwrap_or_else(|| "SPEC".to_owned()),
    };

    let primary = primary_workspace_root(config);
    let mut max_num = backend.max_spec_number(&req.collection, &prefix)?;

    // Scan `<collection.path>/*.md` and `<collection.path>/archive/*.md`.
    for dir_rel in [col.path.clone(), format!("{}/archive", col.path)] {
        let Ok(dir_abs) = checked_child(&primary, Path::new(&dir_rel)) else {
            continue;
        };
        for p in sorted_dir_paths(&dir_abs) {
            if p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if let Some(num) = extract_spec_number(stem, &prefix) {
                    max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
                }
            }
            if let Ok(rel) = p.strip_prefix(&primary) {
                if let Ok(raw) = read_confined_utf8(&primary, &rel.to_string_lossy()) {
                    if let Ok(doc) = parse_spec(&raw) {
                        if let Some(num) = extract_spec_number(&doc.id, &prefix) {
                            max_num = Some(max_num.map_or(num, |cur| cur.max(num)));
                        }
                    }
                }
            }
        }
    }

    let spec_id = format_next_spec_id(&prefix, col.digits, max_num);
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| slug.replace('-', " "));
    let profile_line = req
        .profile
        .as_deref()
        .or(col.profile.as_deref())
        .map(|p| format!("profile: {p}\n"))
        .unwrap_or_default();

    let rendered = format!(
        "---\nid: {spec_id}\ntitle: \"{}\"\ndoc_type: contract\n{profile_line}status: planned\ninvariants: []\nacceptance_criteria: []\n---\n\n# {spec_id}: {title}\n\n### task: T1\n\n```yaml\ntarget: \"Describe task T1 target\"\nscope: []\ntags: [default]\ninvariants: []\nacceptance_criteria: []\n```\n",
        title.replace('"', "\\\"")
    );
    let _ = parse_spec(&rendered)?;

    let spec_rel = format!("{}/{spec_id}-{slug}.md", col.path.trim_end_matches('/'));
    let spec_abs = checked_child(&primary, Path::new(&spec_rel)).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid draft path '{spec_rel}': {err}"),
        )
    })?;
    if spec_abs.exists() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("draft target '{spec_rel}' already exists"),
        ));
    }
    atomic_write(&spec_abs, rendered.as_bytes(), false).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to write drafted spec '{spec_rel}': {err}"),
        )
    })?;

    Ok(DraftResp {
        spec_id,
        spec_path: spec_rel,
        collection: req.collection,
    })
}

/// Claim or idempotently re-claim a ready/active gate node, assembling `ClaimResp`.
pub fn handle_claim<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: ClaimReq,
) -> Result<crate::context::ClaimResp, StateError> {
    handle_claim_dyn(config, backend, provider, req)
}

pub(crate) fn handle_claim_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: ClaimReq,
) -> Result<crate::context::ClaimResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, layout| {
        claim_holding_repo_lock(config, backend, provider, layout, primary, req)
    })
}

fn claim_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    req: ClaimReq,
) -> Result<crate::context::ClaimResp, StateError> {
    let run = resolve_active_run(
        config,
        backend,
        req.run.as_deref(),
        req.worktree.as_deref(),
    )?;
    let snapshot = backend
        .load_run_graph(&run.run_id)?
        .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run graph missing"))?;
    let resolved_profile = parse_profile_from_run(&snapshot.run)?;
    let machine_profile = to_machine_profile(&resolved_profile)?;
    let ext_deps =
        collect_external_satisfied_deps(config, &snapshot.run.run_id, &snapshot.dependencies)?;
    let unintegrated_tasks = compute_unintegrated_tasks(config, &snapshot)?;
    let now_utc = now_utc_iso();

    let sched_obs = ValidatedObservations {
        external_satisfied_deps: ext_deps.clone(),
        unintegrated_tasks,
        now_utc: now_utc.clone(),
        ..ValidatedObservations::default()
    };

    let cursor = match req.node {
        Some(ref raw) => parse_cursor_for_snapshot(&snapshot, raw)?,
        None => auto_select_ready_cursor(&snapshot, &sched_obs)?,
    };
    let node_id = cursor.to_node_id()?;
    let node = snapshot
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node '{cursor}' does not exist in run '{}'", run.run_id),
            )
        })?;
    let gate_def = find_profile_gate_def(&resolved_profile, &cursor).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("no gate definition found for '{cursor}'"),
        )
    })?;

    let (target_worktree, target_branch) = resolve_or_create_claim_worktree(
        config,
        primary,
        layout.is_some(),
        &snapshot,
        &resolved_profile,
        node,
        req.worktree.as_deref(),
    )?;

    let policy = build_path_policy(config, backend, &[])?;
    let claim_tree = current_scope_tree(&target_worktree, None, &policy)?;
    let baseline_commit = if layout.is_some() {
        current_head_commit(&target_worktree).ok()
    } else {
        None
    };

    let session_fp = req
        .session_fingerprint
        .clone()
        .or_else(|| Some(current_session_fingerprint()));
    let previous_local_pid_alive = if node.status == NodeStatus::InProgress {
        is_local_pid_alive(node.session_fingerprint.as_deref())
    } else {
        None
    };
    let span_id = SpanId::new().to_string();

    let claim_input = ClaimInput {
        node_cursor: cursor.clone(),
        worker_id: req.worker_id.clone(),
        session_fingerprint: session_fp,
        worktree: target_worktree.to_string_lossy().into_owned(),
        branch: target_branch,
        baseline_commit,
        claim_tree: claim_tree.clone(),
        span_id,
        force: req.force,
        steal_remote: req.steal_remote,
        previous_local_pid_alive,
    };

    let decision = evaluate_transition(
        &snapshot,
        &machine_profile,
        &sched_obs,
        &TransitionInput::Claim(claim_input),
    )?;
    let TransitionOutcome::Claim(decision) = decision else {
        unreachable!("Claim input always yields Claim outcome");
    };

    let (active_claim_rev, effective_tree, effective_node) = match *decision {
        ClaimDecision::Idempotent(existing) => (existing.claim_revision, existing.claim_tree, node.clone()),
        ClaimDecision::Admit { claim, transition } => {
            if gate_def.save.pre == Some(SaveAction::Snapshot) {
                let admitted = container_scope(&snapshot, node);
                let _ = capture_snapshot(&CaptureSnapshotReq {
                    worktree: target_worktree.clone(),
                    run_id: snapshot.run.run_id.clone(),
                    namespace: format!("pre-{}", node.gate_id),
                    label: format!("rev-{}", claim.claim_revision),
                    scope: if admitted.is_empty() {
                        None
                    } else {
                        Some(admitted)
                    },
                    path_policy: policy,
                    message: format!("acdd pre-claim snapshot {} rev-{}", cursor, claim.claim_revision),
                })?;
            }

            let parent_patches: Vec<NodeStatusPatch> = transition
                .node_patches
                .iter()
                .filter(|p| p.node_id != claim.node_id)
                .cloned()
                .collect();

            exec_tx_with(backend, |tx| {
                tx.admit_claim(&snapshot.run.run_id, &transition.preconditions, &claim)?;
                if !parent_patches.is_empty() {
                    let parent_versions: Vec<NodeVersionExpectation> = parent_patches
                        .iter()
                        .filter_map(|p| {
                            snapshot.nodes.iter().find(|n| n.node_id == p.node_id).map(|n| {
                                NodeVersionExpectation {
                                    node_id: n.node_id.clone(),
                                    expected_version: n.node_version,
                                }
                            })
                        })
                        .collect();
                    let parent_plan = TransitionPlan {
                        preconditions: StoragePreconditions {
                            expected_run_generation: snapshot.run.generation + 1,
                            expected_node_versions: parent_versions,
                            expected_claim_revision: None,
                            expected_contract_revision: None,
                            expected_worker_id: None,
                            required_predecessor_trees: Vec::new(),
                        },
                        bump_run_generation: false,
                        new_run_status: None,
                        new_root_cycle_count: None,
                        completed_at: None,
                        node_patches: parent_patches,
                        dependency_patches: Vec::new(),
                        claim_patches: Vec::new(),
                        new_artifacts: Vec::new(),
                        new_blackboard: Vec::new(),
                        blackboard_patches: Vec::new(),
                        new_subtasks: Vec::new(),
                        significant_transition: true,
                    };
                    tx.apply_transition_plan(&snapshot.run.run_id, &parent_plan)?;
                }
                Ok(())
            })?;

            let refreshed = backend
                .load_node(&snapshot.run.run_id, &node_id)?
                .unwrap_or_else(|| node.clone());
            (claim.claim_revision, claim.claim_tree, refreshed)
        }
    };

    let latest_snap = backend
        .load_run_graph(&snapshot.run.run_id)?
        .unwrap_or(snapshot);
    let spec = read_spec_readonly(config, &latest_snap.run);
    let unresolved_findings = find_latest_rejection_findings(&latest_snap, &node_id);
    let failed_trace_log_tail =
        read_latest_failed_trace_tail(&latest_snap, &node_id, primary, resolved_profile.limits.diagnostic_inline_bytes);
    let pre_task_slice = effective_node
        .task_ref
        .as_ref()
        .and_then(|tr| extract_pre_task_slice(&latest_snap, tr.as_str()));
    let dependency_receipts = collect_dependency_receipts(&latest_snap, &effective_node);
    let blackboard_items = if config.modules.is_enabled("blackboard") {
        backend
            .list_blackboard(
                &latest_snap.run.run_id,
                &BlackboardListReq {
                    limit: 15,
                    offset: 0,
                    ..BlackboardListReq::default()
                },
            )?
            .items
    } else {
        Vec::new()
    };

    let ctx_req = ClaimContextRequest {
        run: latest_snap.run.clone(),
        node: effective_node,
        gate: gate_def,
        profile: resolved_profile,
        spec,
        claim_revision: active_claim_rev,
        worker_id: req.worker_id,
        worktree: target_worktree,
        candidate_tree: effective_tree,
        unresolved_findings,
        failed_trace_log_tail,
        pre_task_slice,
        dependency_receipts,
        blackboard_items,
        response_max_bytes: config.response_policy.max_output_bytes,
    };

    let claim_resp = build_claim_context(&ctx_req, provider)?;

    let _ = append_trace_event(
        layout,
        primary,
        &latest_snap.run.run_id,
        &json!({
            "kind": "gate_claim",
            "run_id": latest_snap.run.run_id.as_str(),
            "node_cursor": claim_resp.node_cursor.to_string(),
            "claim_revision": claim_resp.claim_revision,
            "worker_id": claim_resp.worker_id.as_str(),
            "worktree": claim_resp.worktree,
            "timestamp": now_utc,
        }),
    );

    Ok(claim_resp)
}

#[allow(clippy::too_many_arguments)]
fn resolve_or_create_claim_worktree(
    config: &StateEngineConfig,
    primary: &Path,
    has_git: bool,
    snapshot: &RunGraphSnapshot,
    profile: &ResolvedGateProfile,
    node: &GateNodeRecord,
    override_wt: Option<&Path>,
) -> Result<(PathBuf, Option<String>), StateError> {
    if let Some(wt) = override_wt {
        let canon = canonicalize_macos_var(wt);
        let branch = if has_git && canon.exists() {
            current_branch_name(&canon)?
        } else {
            node.branch.clone()
        };
        return Ok((canon, branch));
    }

    if let Some(ref wt) = node.worktree {
        let p = canonicalize_macos_var(Path::new(wt));
        return Ok((p, node.branch.clone()));
    }

    if node.kind == NodeKind::TaskGate {
        if let Some(ref pid) = node.parent_id {
            if let Some(parent_task) = snapshot.nodes.iter().find(|n| &n.node_id == pid) {
                if let Some(ref pwt) = parent_task.worktree {
                    let p = canonicalize_macos_var(Path::new(pwt));
                    return Ok((p, parent_task.branch.clone()));
                }
                if has_git
                    && snapshot.run.isolation == IsolationMode::Worktree
                    && profile.git.worktrees == WorktreeLifecycle::Managed
                {
                    if let Some(ref tref) = parent_task.task_ref {
                        let wt_path = primary
                            .join(".worktrees")
                            .join(format!("{}-{tref}", snapshot.run.run_name));
                        let branch_name = format!("task/{}-{tref}", snapshot.run.run_name);
                        let base_ref = snapshot
                            .run
                            .branch
                            .as_deref()
                            .or(snapshot.run.target_base.as_deref())
                            .unwrap_or("HEAD")
                            .to_owned();
                        let copy_paths: Vec<String> =
                            snapshot.run.spec_path.iter().cloned().collect();
                        let created = create_task_worktree(&CreateWorktreeReq {
                            primary_worktree: primary.to_path_buf(),
                            worktree_path: wt_path,
                            branch_name,
                            base_ref,
                            copy_uncommitted_paths: copy_paths,
                        })?;
                        return Ok((created.worktree, Some(created.branch)));
                    }
                }
            }
        }
    }

    let wt = resolve_node_worktree(config, &snapshot.run, node, None);
    let branch = if has_git && wt.exists() {
        current_branch_name(&wt)?
    } else {
        snapshot.run.branch.clone()
    };
    Ok((wt, branch))
}

fn find_latest_rejection_findings(
    snapshot: &RunGraphSnapshot,
    node_id: &str,
) -> Option<RejectionFindings> {
    snapshot
        .artifacts
        .iter()
        .filter(|a| a.node_id == node_id && a.kind == ArtifactKind::Findings)
        .max_by_key(|a| a.claim_revision)
        .and_then(|a| serde_json::from_str::<RejectionFindings>(&a.payload_json).ok())
}

fn read_latest_failed_trace_tail(
    snapshot: &RunGraphSnapshot,
    node_id: &str,
    primary: &Path,
    max_bytes: usize,
) -> Option<String> {
    let trace = snapshot
        .traces
        .iter()
        .filter(|t| {
            t.node_id == node_id
                && !matches!(t.status, TraceStatus::Running | TraceStatus::Passed | TraceStatus::ExpectedFailure)
        })
        .max_by_key(|t| t.trace_row_id.unwrap_or(0))?;
    let rel = Path::new(".acdd/run")
        .join(snapshot.run.run_id.as_str())
        .join(&trace.log_relpath);
    let mut file = acdd_core::fs::open_confined(primary, &rel, fs::OpenOptions::new().read(true)).ok()?;
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    if content.len() <= max_bytes {
        Some(content)
    } else {
        let mut start = content.len().saturating_sub(max_bytes);
        while start < content.len() && !content.is_char_boundary(start) {
            start += 1;
        }
        Some(content[start..].to_owned())
    }
}

/// Extend a task's admitted write scope in SQLite without rewriting the spec file until archive.
pub fn handle_extend_scope<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: ExtendScopeReq,
) -> Result<ExtendScopeResp, StateError> {
    handle_extend_scope_dyn(config, backend, req)
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
        let obs = ValidatedObservations {
            now_utc: now_utc_iso(),
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

/// Reopen a gate or task container (with optional `--cascade`).
pub fn handle_reopen<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: ReopenReq,
) -> Result<ReopenResp, StateError> {
    handle_reopen_dyn(config, backend, req)
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
            now_utc: now_utc_iso(),
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

/// Synchronize specification edits (and optional `--rebind-profile`) into an active `Run`.
pub fn handle_sync<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: SyncReq,
) -> Result<SyncResp, StateError> {
    handle_sync_dyn(config, backend, provider, req)
}

pub(crate) fn handle_sync_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: SyncReq,
) -> Result<SyncResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
        let current_resolved = parse_profile_from_run(&snapshot.run)?;
        let current_machine = to_machine_profile(&current_resolved)?;

        let spec_rel = if let Some(ref sp) = snapshot.run.spec_path {
            sp.clone()
        } else {
            format!(".acdd/run/{}/spec.md", snapshot.run.run_id)
        };
        let raw_spec = read_confined_utf8(primary, &spec_rel)?;
        let doc = parse_spec(&raw_spec)?;

        let (new_machine_profile, new_profile_pair_owned) = if req.rebind_profile {
            let refreshed = resolve_profile_ref(
                &snapshot.run.profile_id,
                Some(primary),
                Some(&config.modules),
            )?;
            validate_profile_fail_closed(config, &refreshed, provider, true)?;
            let m = to_machine_profile(&refreshed)?;
            let pid = m.profile_id.clone();
            let pjson = m.profile_json.clone();
            (Some(m), Some((pid, pjson)))
        } else {
            (None, None)
        };

        let mut spec_repos = BTreeMap::new();
        for r in &doc.repositories {
            spec_repos.insert(r.name.clone(), PathBuf::from(&r.path));
        }
        let primary_str = primary.to_string_lossy().into_owned();
        let machine_spec = build_machine_spec_input(
            &doc,
            &primary_str,
            &snapshot.run.run_id,
            &config.linked_workspaces,
            &spec_repos,
        )?;
        let ext_deps =
            collect_external_satisfied_deps(config, &snapshot.run.run_id, &snapshot.dependencies)?;
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            now_utc: now_utc_iso(),
            ..ValidatedObservations::default()
        };

        let outcome = evaluate_transition(
            &snapshot,
            &current_machine,
            &obs,
            &TransitionInput::Sync(SyncInput {
                spec: machine_spec,
                new_profile: new_machine_profile,
                rebind_profile: req.rebind_profile,
            }),
        )?;
        let TransitionOutcome::Sync(sync_plan) = outcome else {
            unreachable!();
        };

        let added_nodes = sync_plan.added_nodes.len();
        let updated_nodes = sync_plan.transition.node_patches.len();
        let spec_digest = sync_plan.spec_digest.clone();

        exec_tx_with(backend, |tx| {
            let pair = new_profile_pair_owned
                .as_ref()
                .map(|(id, json)| (id.as_str(), json.as_str()));
            tx.apply_sync_plan(&snapshot.run.run_id, &sync_plan, pair)
        })?;

        Ok(SyncResp {
            run_id: snapshot.run.run_id,
            spec_digest,
            rebound_profile: req.rebind_profile,
            added_nodes,
            updated_nodes,
        })
    })
}

/// Update a task's subtask status and mandatory terminal evidence.
pub fn handle_subtask<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: SubtaskReq,
) -> Result<SubtaskResp, StateError> {
    handle_subtask_dyn(config, backend, req)
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
            updated_at: now_utc_iso(),
        };

        exec_tx_with(backend, |tx| tx.upsert_subtask(&record))?;
        Ok(SubtaskResp { subtask: record })
    })
}

/// Post a message to the run blackboard (`acdd_blackboard`).
pub fn handle_blackboard_post<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: BlackboardPostReq,
) -> Result<BlackboardPostResp, StateError> {
    handle_blackboard_post_dyn(config, backend, req)
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
            created_at: now_utc_iso(),
        };

        let stored = exec_tx_with(backend, |tx| tx.insert_blackboard(&item))?;
        Ok(BlackboardPostResp { entry: stored })
    })
}

/// Capture an explicit manual Git snapshot under `refs/acdd/snapshots/<run_id>/manual/<label>`.
pub fn handle_snapshot<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: SnapshotReq,
) -> Result<SnapshotResp, StateError> {
    handle_snapshot_dyn(config, backend, req)
}

pub(crate) fn handle_snapshot_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: SnapshotReq,
) -> Result<SnapshotResp, StateError> {
    ensure_state_enabled(config)?;
    let primary = primary_workspace_root(config);
    let layout = crate::usecases::lifecycle::resolve_repo_layout_optional(&primary)?;
    let lock_guard = crate::usecases::lifecycle::acquire_repo_lock(layout.as_ref())?;

    let result = (|| -> Result<SnapshotResp, StateError> {
        let run = resolve_active_run(
            config,
            backend,
            req.run.as_deref(),
            req.worktree.as_deref(),
        )?;
        let target_wt = req
            .worktree
            .as_deref()
            .or_else(|| run.worktree.as_deref().map(Path::new))
            .unwrap_or(&config.worktree);
        let canon_wt = canonicalize_macos_var(target_wt);
        let policy = build_path_policy(config, backend, &[])?;
        let label = req
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());

        let snap = capture_snapshot(&CaptureSnapshotReq {
            worktree: canon_wt,
            run_id: run.run_id.clone(),
            namespace: "manual".to_owned(),
            label: label.clone(),
            scope: None,
            path_policy: policy,
            message: format!("acdd manual snapshot {label}"),
        })?;

        Ok(SnapshotResp {
            run_id: run.run_id,
            snapshot_ref: snap.snapshot_ref,
            commit_oid: snap.commit_oid,
            tree_oid: snap.tree_oid,
        })
    })();

    drop(lock_guard);
    result
}

/// Confined-restore files from a snapshot ref or OID and mark drifted completed nodes stale.
pub fn handle_rollback<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: RollbackReq,
) -> Result<RollbackResp, StateError> {
    handle_rollback_dyn(config, backend, req)
}

pub(crate) fn handle_rollback_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: RollbackReq,
) -> Result<RollbackResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |_primary, _layout| {
        let run = resolve_active_run(
            config,
            backend,
            req.run.as_deref(),
            req.worktree.as_deref(),
        )?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;

        let (admitted_scope, target_wt) = if let Some(ref raw_node) = req.node {
            let cursor = parse_cursor_for_snapshot(&snapshot, raw_node)?;
            let nid = cursor.to_node_id()?;
            let node = snapshot
                .nodes
                .iter()
                .find(|n| n.node_id == nid)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("node '{raw_node}' does not exist in run '{}'", run.run_id),
                    )
                })?;
            let scope = container_scope(&snapshot, node);
            let wt = resolve_node_worktree(config, &snapshot.run, node, req.worktree.as_deref());
            (if scope.is_empty() { None } else { Some(scope) }, wt)
        } else {
            let wt = req
                .worktree
                .as_deref()
                .or_else(|| snapshot.run.worktree.as_deref().map(Path::new))
                .unwrap_or(&config.worktree);
            (None, canonicalize_macos_var(wt))
        };

        let policy = build_path_policy(config, backend, &[])?;
        let restored = restore_rollback(&GitRollbackReq {
            worktree: target_wt.clone(),
            target_ref_or_oid: req.target,
            admitted_scope,
            path_policy: policy.clone(),
        })?;

        let current_tree = current_scope_tree(&target_wt, None, &policy)?;
        let mut staled_nodes = Vec::new();
        let mut versions = Vec::new();
        for node in &snapshot.nodes {
            if matches!(node.kind, NodeKind::RootGate | NodeKind::TaskGate)
                && node.status == NodeStatus::Completed
                && node.accepted_tree.as_ref() != Some(&current_tree)
            {
                staled_nodes.push(node.node_id.clone());
                versions.push(NodeVersionExpectation {
                    node_id: node.node_id.clone(),
                    expected_version: node.node_version,
                });
            }
        }

        if !staled_nodes.is_empty() {
            let preconditions = StoragePreconditions {
                expected_run_generation: snapshot.run.generation,
                expected_node_versions: versions,
                expected_claim_revision: None,
                expected_contract_revision: None,
                expected_worker_id: None,
                required_predecessor_trees: Vec::new(),
            };
            let now_utc = now_utc_iso();
            exec_tx_with(backend, |tx| {
                tx.mark_nodes_stale(&snapshot.run.run_id, &preconditions, &staled_nodes, &now_utc)
            })?;
        }

        Ok(RollbackResp {
            run_id: snapshot.run.run_id,
            target_tree: restored.target_tree,
            restored_paths: restored.restored_paths,
            staled_nodes,
        })
    })
}

/// Integrate completed task branches into the run worktree (`manual | rebase-ff | ff-only | no-ff | squash`).
pub fn handle_integrate<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: IntegrateReq,
) -> Result<IntegrateResp, StateError> {
    handle_integrate_dyn(config, backend, req)
}

pub(crate) fn handle_integrate_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: IntegrateReq,
) -> Result<IntegrateResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, _layout| {
        integrate_holding_repo_lock(config, backend, primary, req)
    })
}

pub(crate) fn integrate_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    primary: &Path,
    req: IntegrateReq,
) -> Result<IntegrateResp, StateError> {
    let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
    let snapshot = backend
        .load_run_graph(&run.run_id)?
        .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;
    let profile = parse_profile_from_run(&snapshot.run)?;

    let run_worktree = snapshot
        .run
        .worktree
        .as_deref()
        .map(|p| canonicalize_macos_var(Path::new(p)))
        .unwrap_or_else(|| primary.to_path_buf());
    let _ = abort_in_progress_git_ops(&run_worktree)?;

    let strategy = req.strategy.unwrap_or(profile.git.integrate);
    if strategy != IntegrateStrategy::Manual {
        let branch = current_branch_name(&run_worktree)?;
        if is_protected_main_branch(branch.as_deref()) && !req.allow_main {
            return Err(StateError::new(
                ErrorCode::MainBranchProtected,
                format!(
                    "refusing integration onto protected branch '{}' without --allow-main",
                    branch.as_deref().unwrap_or("main")
                ),
            ));
        }
    }

    let run_branch = current_branch_name(&run_worktree)?;
    let mut completed_tasks: Vec<&crate::types::GateNodeRecord> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
        .filter(|n| {
            req.task_ref
                .as_ref()
                .is_none_or(|target| n.task_ref.as_ref() == Some(target))
        })
        .collect();
    completed_tasks.sort_by_key(|n| n.seq_order);

    if let Some(ref target_ref) = req.task_ref {
        if completed_tasks.is_empty() {
            return Err(StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("completed task '{target_ref}' not found in run '{}'", run.run_id),
            ));
        }
    }

    let mut integrated_tasks = Vec::new();
    for task_node in completed_tasks {
        let Some(ref tref) = task_node.task_ref else {
            continue;
        };
        let Some(ref accepted_tree) = task_node.accepted_tree else {
            continue;
        };
        let Some(ref source_branch) = task_node.branch else {
            continue;
        };
        if run_branch.as_deref() == Some(source_branch.as_str()) {
            continue;
        }
        let source_wt = task_node.worktree.as_deref().map(PathBuf::from);
        if let Some(ref swt) = source_wt {
            if swt.exists() {
                let _ = abort_in_progress_git_ops(swt)?;
            }
        }

        let resp = integrate_task_branch(&IntegrateTaskReq {
            target_worktree: run_worktree.clone(),
            source_worktree: source_wt,
            source_branch: source_branch.clone(),
            run_id: snapshot.run.run_id.clone(),
            task_ref: tref.clone(),
            accepted_tree: accepted_tree.clone(),
            strategy,
            allow_main: req.allow_main,
        })?;
        if resp.integrated_commit.is_some() || strategy == IntegrateStrategy::FfOnly {
            integrated_tasks.push(tref.clone());
        }
    }

    // If all non-cancelled tasks are now completed & integrated, promote first post-task root gate Pending -> Ready.
    let all_tasks_done = crate::machine::schedule::all_non_cancelled_tasks_completed(&snapshot);
    if all_tasks_done {
        let inputs = collect_integration_inputs(&snapshot);
        let missing = verify_tasks_integrated(&snapshot.run.run_id, &run_worktree, &inputs)?;
        if missing.is_empty() {
            if let Some(first_post) = snapshot
                .nodes
                .iter()
                .filter(|n| is_post_task_root_node(&snapshot, n))
                .min_by_key(|n| n.seq_order)
            {
                if first_post.status == NodeStatus::Pending {
                    let plan = TransitionPlan {
                        preconditions: build_preconditions(&snapshot, &[&first_post.node_id], None),
                        bump_run_generation: true,
                        new_run_status: None,
                        new_root_cycle_count: None,
                        completed_at: None,
                        node_patches: vec![NodeStatusPatch::status_only(
                            &first_post.node_id,
                            NodeStatus::Ready,
                        )],
                        dependency_patches: Vec::new(),
                        claim_patches: Vec::new(),
                        new_artifacts: Vec::new(),
                        new_blackboard: Vec::new(),
                        blackboard_patches: Vec::new(),
                        new_subtasks: Vec::new(),
                        significant_transition: true,
                    };
                    exec_tx_with(backend, |tx| tx.apply_transition_plan(&snapshot.run.run_id, &plan))?;
                }
            }
        }
    }

    let target_head = current_head_commit(&run_worktree)?;
    Ok(IntegrateResp {
        run_id: snapshot.run.run_id,
        integrated_tasks,
        target_head,
    })
}

/// Clear/cancel an active `Run` into `run_tombstones(terminal_status = 'cleared')` preserving `backup.lock` order.
pub fn handle_clear<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    req: ClearReq,
) -> Result<ClearResp, StateError> {
    handle_clear_dyn(config, backend, req)
}

pub(crate) fn handle_clear_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    req: ClearReq,
) -> Result<ClearResp, StateError> {
    with_repo_lock_and_maintenance(config, backend, |primary, _layout| {
        let run = resolve_active_run(config, backend, req.run.as_deref(), None)?;
        let snapshot = backend
            .load_run_graph(&run.run_id)?
            .ok_or_else(|| StateError::new(ErrorCode::RunSelectorAmbiguous, "run missing"))?;

        // Never delete the user's non-ephemeral spec file on `clear`; only clean worktrees, branches (including `run.branch`), refs, and ephemeral spec.
        let mut cleanup_manifest: Vec<ArchiveCleanupEntry> =
            build_cleanup_manifest(config, &snapshot, primary)
                .into_iter()
                .filter(|e| e.kind != "spec_file")
                .collect();
        if let Some(ref run_br) = snapshot.run.branch {
            if snapshot.run.target_base.as_ref() != Some(run_br)
                && !cleanup_manifest
                    .iter()
                    .any(|e| e.kind == "branch" && e.path_or_ref == *run_br)
            {
                cleanup_manifest.push(ArchiveCleanupEntry {
                    kind: "branch".to_owned(),
                    path_or_ref: run_br.clone(),
                    workspace: None,
                });
            }
        }
        if snapshot.run.is_ephemeral {
            cleanup_manifest.push(ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: format!(".acdd/run/{}/spec.md", snapshot.run.run_id),
                workspace: None,
            });
        }

        let spec_doc = read_spec_readonly(config, &snapshot.run);
        let closed_at = now_utc_iso();
        let tombstone_seed = RunTombstoneRecord {
            run_id: snapshot.run.run_id.clone(),
            run_name: snapshot.run.run_name.clone(),
            collection: snapshot.run.collection.clone(),
            workspace: snapshot.run.workspace.clone(),
            spec_id: spec_doc.as_ref().map(|d| d.id.clone()),
            title: spec_doc
                .as_ref()
                .map(|d| d.title.clone())
                .unwrap_or_else(|| snapshot.run.run_name.as_str().to_owned()),
            profile_id: snapshot.run.profile_id.clone(),
            is_ephemeral: snapshot.run.is_ephemeral,
            generation: snapshot.run.generation,
            terminal_status: TerminalStatus::Cleared,
            target_base: snapshot.run.target_base.clone(),
            target_base_commit: snapshot.run.target_base_commit.clone(),
            archive_path: None,
            closed_commit: None,
            cleanup_pending: !cleanup_manifest.is_empty(),
            cleanup_manifest: cleanup_manifest.clone(),
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at,
        };

        let (_, mut summary_json, metrics_json) =
            build_completed_run_digest(&snapshot, &tombstone_seed, None)?;
        if let Some(ref reason) = req.reason {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&summary_json) {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("cancellation_reason".to_owned(), json!(reason));
                    summary_json = v.to_string();
                }
            }
        }
        let tombstone = RunTombstoneRecord {
            summary_json,
            metrics_json,
            ..tombstone_seed
        };

        let preconditions = StoragePreconditions {
            expected_run_generation: snapshot.run.generation,
            expected_node_versions: Vec::new(),
            expected_claim_revision: None,
            expected_contract_revision: None,
            expected_worker_id: None,
            required_predecessor_trees: Vec::new(),
        };

        exec_tx_with(backend, |tx| tx.archive_and_prune_run(&preconditions, &tombstone))?;

        let mut cleaned_items = Vec::new();
        let mut workspaces = BTreeSet::new();
        workspaces.insert(primary.to_path_buf());
        for ws in config.linked_workspaces.values() {
            workspaces.insert(canonicalize_macos_var(ws));
        }

        for entry in &cleanup_manifest {
            match entry.kind.as_str() {
                "spec_file" => {
                    if let Ok(p) = checked_child(primary, Path::new(&entry.path_or_ref)) {
                        if p.exists() && fs::remove_file(&p).is_ok() {
                            cleaned_items.push(entry.path_or_ref.clone());
                        }
                    }
                }
                "worktree" => {
                    remove_worktree_and_branch(
                        primary,
                        Some(Path::new(&entry.path_or_ref)),
                        None,
                        req.force,
                    )?;
                    cleaned_items.push(entry.path_or_ref.clone());
                }
                "branch" => {
                    remove_worktree_and_branch(
                        primary,
                        None,
                        Some(&entry.path_or_ref),
                        req.force,
                    )?;
                    cleaned_items.push(entry.path_or_ref.clone());
                }
                _ => {}
            }
        }
        let ws_vec: Vec<PathBuf> = workspaces.into_iter().collect();
        if let Ok(refs) = cleanup_run_refs(&ws_vec, &tombstone.run_id) {
            cleaned_items.extend(refs);
        }
        let _ = prune_old_run_trace_dirs(primary, backend, config.retain_traces);
        exec_tx_with(backend, |tx| tx.mark_tombstone_cleaned(&tombstone.run_id))?;

        let mut final_tombstone = tombstone;
        final_tombstone.cleanup_pending = false;
        Ok(ClearResp {
            tombstone: final_tombstone,
            cleaned_items,
        })
    })
}

/// Invoke an external `DomainProvider` plugin tool (`<domain>_<name>`) validating `DomainToolDef.input_schema`.
pub fn handle_call_plugin_tool<B: StateBackend>(
    config: &StateEngineConfig,
    backend: &B,
    provider: &CompositeDomainProvider,
    req: CallPluginToolReq,
) -> Result<CallPluginToolResp, StateError> {
    handle_call_plugin_tool_dyn(config, backend, provider, req)
}

pub(crate) fn handle_call_plugin_tool_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: CallPluginToolReq,
) -> Result<CallPluginToolResp, StateError> {
    ensure_state_enabled(config)?;
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
        let wt = req
            .worktree
            .map(|p| canonicalize_macos_var(&p))
            .or_else(|| run.worktree.as_deref().map(|p| canonicalize_macos_var(Path::new(p))));
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

fn build_machine_spec_input(
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

fn normalize_spec_relpath(raw: &str) -> Result<String, StateError> {
    let normalized = acdd_core::path_policy::normalize_rel_path(raw).ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("spec_path '{raw}' must be a normalized workspace-relative path"),
        )
    })?;
    Ok(normalized)
}

fn derive_run_name(spec_id: &str, fallback: &str) -> Result<RunName, StateError> {
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
