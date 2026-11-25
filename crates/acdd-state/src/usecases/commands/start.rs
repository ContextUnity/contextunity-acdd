//! `start` command handler and initial run worktree/dependency resolution.

use crate::git_ops::commit::{current_branch_name, current_head_commit};
use crate::git_ops::worktree::{
    canonicalize_macos_var, create_managed_worktree, create_run_worktree,
    remove_worktree_and_branch, CreateWorktreeReq, RepoWorktreeLayout,
};
use crate::machine::{build_initial_run_graph, BuildGraphInput};
use crate::profile::{resolve_optional_profile_ref_with_dirs, ResolvedGateProfile};
use crate::spec::{materialize_ephemeral_goal_spec_with_options, parse_spec, SpecStatus};
use crate::store::traits::{exec_tx_with, StateBackend};
use crate::types::{
    IsolationMode, NodeKind, NodeStatus, RunGraphSnapshot, ValidatedObservations, WorktreeLifecycle,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::{collect_external_satisfied_deps, verify_run_level_depends_on};
use crate::usecases::dto::{StartReq, StartResp};
use crate::usecases::lifecycle::{
    append_trace_event, apply_effective_isolation, checkout_config, effective_spec_collections,
    ensure_state_enabled, generate_run_id, match_spec_collection, read_confined_utf8,
    resolve_source_checkout, resume_pending_tombstone_cleanups_dyn, to_machine_profile,
    validate_profile_fail_closed, with_repo_lock_and_maintenance_unchecked,
};
use acdd_core::config::{Adapter, ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{atomic_write, checked_child};
use acdd_core::ids::{RunId, RunName, TaskRef};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::telemetry::TraceId;
use serde_json::json;
use std::path::Path;

use super::common::{
    build_machine_spec_input, derive_run_name, ensure_no_managed_worktree_collision,
    normalize_spec_relpath,
};

pub(crate) fn handle_start_dyn(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    req: StartReq,
    source_override: Option<&Adapter>,
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
        start_holding_repo_lock(
            config,
            backend,
            provider,
            layout,
            primary,
            req,
            source_override,
        )
    })
}

fn start_holding_repo_lock(
    config: &StateEngineConfig,
    backend: &dyn StateBackend,
    provider: &CompositeDomainProvider,
    layout: Option<&RepoWorktreeLayout>,
    primary: &Path,
    req: StartReq,
    source_override: Option<&Adapter>,
) -> Result<StartResp, StateError> {
    let _ = resume_pending_tombstone_cleanups_dyn(config, backend, layout)?;

    let caller = resolve_source_checkout(primary, layout, &config.worktree, false)?;
    let explicit = req
        .worktree_path
        .as_deref()
        .map(|path| resolve_source_checkout(primary, layout, path, true))
        .transpose()?;
    let source = explicit
        .as_deref()
        .filter(|path| path.is_dir())
        .unwrap_or(&caller);
    let spec_source = if req.spec_path.is_some() {
        source
    } else {
        primary
    };
    let checkout = checkout_config(config, primary, spec_source, source_override)?;
    let created_at = acdd_core::telemetry::now_utc_iso_ms();
    let trace_id = TraceId::new().to_string();

    let run_id;
    let run_name;
    let collection;
    let spec_path_rel;
    let is_ephemeral;
    let spec_doc;
    let mut resolved_profile;
    let materialized_spec_path;
    let ephemeral_spec_to_write;
    if let Some(raw_spec_path) = req.spec_path.as_deref() {
        let collections = effective_spec_collections(&checkout);
        let norm_rel = normalize_spec_relpath(raw_spec_path)?;
        let raw_content = read_confined_utf8(source, &norm_rel)?;
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
        verify_run_level_depends_on(&checkout, backend, &doc)?;
        let col = match_spec_collection(&collections, &norm_rel);
        let col_profile = col
            .as_deref()
            .and_then(|key| collections.get(key))
            .and_then(|value| value.profile.as_deref());
        let profile_ref = req
            .profile
            .as_deref()
            .map(|reference| {
                Path::new(reference)
                    .strip_prefix(source)
                    .ok()
                    .and_then(Path::to_str)
                    .unwrap_or(reference)
            })
            .or(doc.profile.as_deref())
            .or(col_profile);
        let resolved = resolve_optional_profile_ref_with_dirs(
            profile_ref,
            Some(source),
            Some(&config.modules),
            Some(&checkout.profiles_dirs),
        )?;
        validate_profile_fail_closed(config, &resolved, provider, true)?;
        run_id = generate_run_id(&doc.id)?;
        run_name = match req.run_name {
            Some(name) => name,
            None => derive_run_name(&doc.id, run_id.as_str())?,
        };
        collection = col;
        spec_path_rel = Some(norm_rel.clone());
        is_ephemeral = false;
        spec_doc = doc;
        resolved_profile = resolved;
        materialized_spec_path = Some(norm_rel);
        ephemeral_spec_to_write = None;
    } else {
        let requested_scope = req.scope;
        let goal_text = req.goal.as_deref().unwrap_or("");
        let mut resolved = resolve_optional_profile_ref_with_dirs(
            req.profile.as_deref(),
            Some(primary),
            Some(&config.modules),
            Some(&config.profiles_dirs),
        )?;
        validate_profile_fail_closed(config, &resolved, provider, true)?;
        run_id = generate_run_id("goal")?;
        run_name = match req.run_name {
            Some(name) => name,
            None => RunName::new(run_id.as_str())?,
        };
        let has_tasks_block = resolved.gates.iter().any(|gate| gate.tasks.is_some());
        let effective_scope = if !has_tasks_block {
            for pattern in &requested_scope {
                if !acdd_core::path_policy::is_valid_scope_glob(pattern) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("goal run has invalid or non-normalized scope glob '{pattern}'"),
                    ));
                }
            }
            if requested_scope.is_empty() && resolved.profile_id == "spec_planning" {
                let docs_roots = source_override
                    .map(|adapter| adapter.docs.clone())
                    .unwrap_or_else(|| {
                        if primary.join(ACDD_CONFIG_FILE).is_file() {
                            acdd_core::config::load_adapter(primary, None)
                                .map(|adapter| adapter.docs)
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        }
                    });
                derive_spec_planning_scope(primary, &checkout, &docs_roots)
            } else {
                requested_scope
            }
        } else {
            requested_scope
        };
        if !has_tasks_block && !effective_scope.is_empty() {
            assign_root_gate_scopes(&mut resolved.gates, &effective_scope);
        }
        let rendered_spec = materialize_ephemeral_goal_spec_with_options(
            &run_id,
            &run_name,
            goal_text,
            &resolved.profile_id,
            &effective_scope,
            &[],
            has_tasks_block,
        )?;
        let ephemeral_path = format!("{ACDD_DIR}/run/{}/spec.md", run_id);
        let ephemeral_abs = checked_child(primary, Path::new(&ephemeral_path)).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid ephemeral spec path '{ephemeral_path}': {err}"),
            )
        })?;
        spec_doc = parse_spec(&rendered_spec)?;
        collection = None;
        spec_path_rel = None;
        is_ephemeral = true;
        resolved_profile = resolved;
        materialized_spec_path = Some(ephemeral_path);
        ephemeral_spec_to_write = Some((ephemeral_abs, rendered_spec));
    }

    crate::validate::validate_spec_for_run(
        &checkout,
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
    apply_effective_isolation(
        &mut resolved_profile,
        effective_isolation,
        effective_worktrees,
    );

    let resolved_yaml = serde_yaml::to_string(&resolved_profile).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize profile.resolved.yaml: {err}"),
        )
    })?;
    let profile_snapshot_rel = format!("{ACDD_DIR}/run/{}/profile.resolved.yaml", run_id.as_str());
    let profile_snapshot_path =
        checked_child(primary, Path::new(&profile_snapshot_rel)).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid profile snapshot path '{profile_snapshot_rel}': {err}"),
            )
        })?;

    let target_base = match req.target_base {
        Some(b) => Some(b),
        None if layout.is_some() => current_branch_name(source)?,
        None => None,
    };
    let target_base_commit = if layout.is_some() {
        current_head_commit(source).ok()
    } else {
        None
    };

    let worktree_base = target_base
        .as_deref()
        .or(target_base_commit.as_ref().map(|commit| commit.as_str()));
    let (planned_worktree, planned_branch, mut managed_wt_req) = if effective_isolation
        == IsolationMode::Worktree
        && effective_worktrees == WorktreeLifecycle::Managed
        && layout.is_some()
        && explicit.as_deref() != Some(source)
    {
        let wt_path = explicit.as_deref().map_or_else(
            || primary.join(WORKTREES_DIR).join(run_name.as_str()),
            Path::to_path_buf,
        );
        let canon_wt = canonicalize_macos_var(&wt_path);
        let branch_name = format!("run/{run_name}");
        let base_ref = worktree_base.unwrap_or("HEAD").to_owned();
        let copy_paths: Vec<String> = spec_path_rel.iter().cloned().collect();
        (
            Some(canon_wt.to_string_lossy().into_owned()),
            Some(branch_name.clone()),
            Some(CreateWorktreeReq {
                primary_worktree: primary.to_path_buf(),
                worktree_path: wt_path,
                branch_name,
                base_ref,
                copy_uncommitted_paths: copy_paths,
            }),
        )
    } else {
        let branch = if layout.is_some() {
            current_branch_name(source)?
        } else {
            None
        };
        if explicit.is_some() && effective_worktrees == WorktreeLifecycle::Managed {
            ensure_no_managed_worktree_collision(
                primary,
                run_name.as_str(),
                source,
                branch.as_deref(),
                None,
            )?;
        }
        (Some(source.to_string_lossy().into_owned()), branch, None)
    };
    if let Some(worktree) = managed_wt_req.as_mut().filter(|_| !is_ephemeral) {
        worktree
            .copy_uncommitted_paths
            .extend(crate::profile::inherit::profile_lineage_paths(
                spec_source,
                &resolved_profile.profile_id,
                &checkout.profiles_dirs,
            )?);
        if source != primary {
            worktree
                .copy_uncommitted_paths
                .push(ACDD_CONFIG_FILE.to_owned());
        }
    }

    let primary_str = primary.to_string_lossy().into_owned();
    let machine_spec = build_machine_spec_input(&spec_doc, &primary_str, &run_id, &checkout)?;
    let machine_profile = to_machine_profile(&resolved_profile)?;
    let mut graph = build_initial_run_graph(
        &machine_profile,
        &BuildGraphInput {
            run_id: run_id.clone(),
            run_name,
            collection,
            workspace: primary_str.clone(),
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

    let ext_satisfied = collect_external_satisfied_deps(&checkout, &graph)?;
    apply_initial_external_dep_satisfaction(&mut graph, &ext_satisfied);
    let obs = ValidatedObservations {
        external_satisfied_deps: ext_satisfied,
        external_deps_authoritative: true,
        now_utc: acdd_core::telemetry::now_utc_iso_ms(),
        ..ValidatedObservations::default()
    };
    let ready_nodes =
        crate::usecases::queries::collect_ready_cursors_with_fallback(&graph, &obs, false)?;

    let created_managed = if let Some(ref wt_req) = managed_wt_req {
        let created = if spec_source == primary {
            create_run_worktree(wt_req)?
        } else {
            create_managed_worktree(wt_req, Some(spec_source))?
        };
        graph.run.worktree = Some(created.worktree.to_string_lossy().into_owned());
        graph.run.branch = Some(created.branch.clone());
        let checked = (|| {
            if !graph.run.is_ephemeral {
                let destination =
                    checkout_config(config, primary, &created.worktree, source_override)?;
                let mut dest_profile = resolve_optional_profile_ref_with_dirs(
                    Some(&resolved_profile.profile_id),
                    Some(&created.worktree),
                    Some(&config.modules),
                    Some(&destination.profiles_dirs),
                )?;
                apply_effective_isolation(
                    &mut dest_profile,
                    effective_isolation,
                    effective_worktrees,
                );
                if to_machine_profile(&dest_profile)?.profile_json != machine_profile.profile_json {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "managed checkout profile differs from source",
                    ));
                }
                let dest_doc = parse_spec(&read_confined_utf8(
                    &created.worktree,
                    graph.run.spec_path.as_deref().unwrap_or_default(),
                )?)?;
                crate::validate::validate_spec_for_run(
                    &destination,
                    backend,
                    provider,
                    &dest_doc,
                    &dest_profile,
                )?;
                let dest_spec = build_machine_spec_input(
                    &dest_doc,
                    &primary_str,
                    &graph.run.run_id,
                    &destination,
                )?;
                let source_spec = build_machine_spec_input(
                    &spec_doc,
                    &primary_str,
                    &graph.run.run_id,
                    &checkout,
                )?;
                if dest_spec != source_spec {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "managed checkout dependency bindings differ from source",
                    ));
                }
                let satisfied_before_check: Vec<bool> =
                    graph.dependencies.iter().map(|dep| dep.satisfied).collect();
                for dep in &mut graph.dependencies {
                    dep.satisfied = false;
                }
                let dest_satisfied = collect_external_satisfied_deps(&destination, &graph)?;
                for (dep, was_satisfied) in graph.dependencies.iter_mut().zip(satisfied_before_check) {
                    dep.satisfied = was_satisfied;
                }
                if dest_satisfied != obs.external_satisfied_deps {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "managed checkout dependency satisfaction differs from source",
                    ));
                }
            }
            Ok::<(), StateError>(())
        })();
        if let Err(err) = checked {
            remove_worktree_and_branch(
                primary,
                Some(&created.worktree),
                Some(&created.branch),
                true,
            )?;
            return Err(err);
        }
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
                format!(
                    "failed to materialize ephemeral spec '{}': {err}",
                    eph_abs.display()
                ),
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

    let gate_plan = ResolvedGateProfile::dispatch_plan(&resolved_profile);
    Ok(StartResp {
        run: graph.run,
        ready_nodes,
        materialized_spec_path,
        gate_plan,
    })
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

fn assign_root_gate_scopes(gates: &mut [crate::profile::GateDef], scope: &[String]) {
    for gate in gates {
        if gate.tasks.is_none() && gate.scope.is_empty() {
            gate.scope = scope.to_vec();
        }
        if !gate.gates.is_empty() {
            assign_root_gate_scopes(&mut gate.gates, scope);
        }
    }
}

fn normalize_scope_entry(raw: &str) -> String {
    raw.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

fn derive_spec_planning_scope(
    primary: &Path,
    checkout: &StateEngineConfig,
    docs_roots: &[String],
) -> Vec<String> {
    let mut scope = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push_entry = |raw: &str, check_file: bool| {
        let norm = normalize_scope_entry(raw);
        if norm.is_empty() {
            return;
        }
        let pat = if norm.contains(['*', '?', '[', '{'])
            || (check_file
                && (checked_child(primary, Path::new(&norm)).is_ok_and(|p| p.is_file())
                    || Path::new(&norm).extension().is_some()))
        {
            norm
        } else {
            format!("{norm}/**")
        };
        if acdd_core::path_policy::is_valid_scope_glob(&pat) && seen.insert(pat.clone()) {
            scope.push(pat);
        }
    };

    for col in effective_spec_collections(checkout).values() {
        for raw in col.patterns() {
            push_entry(raw, false);
        }
    }

    if docs_roots.is_empty() {
        push_entry("docs/**", false);
        push_entry("ROADMAP.md", true);
    } else {
        for raw in docs_roots {
            push_entry(raw, true);
        }
        if primary.join("ROADMAP.md").is_file() {
            push_entry("ROADMAP.md", true);
        }
    }

    for dir in &checkout.profiles_dirs {
        push_entry(dir, false);
    }
    push_entry(&format!("{ACDD_DIR}/profiles/**"), false);
    scope
}
