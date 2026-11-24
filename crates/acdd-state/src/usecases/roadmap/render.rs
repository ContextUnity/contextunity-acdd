//! Roadmap DTO assembly, filtering, sorting, and per-task drill-down rendering.

use super::discovery::materialize_checkout_spec;
use super::types::{ActiveRunMap, DependencyGraphData, DiscoveredWorkspace, SpecInfo};
use super::waves::{
    apply_spec_overlaps, apply_task_overlaps, compute_spec_waves, compute_task_waves,
};
use crate::spec::SpecStatus;
use crate::types::{
    GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::{
    collect_external_satisfied_deps_with_owned, run_in_checkout, ParsedDepSyntax,
};
use crate::usecases::dto::{
    RoadmapActiveGateEntry, RoadmapDepEntry, RoadmapReq, RoadmapResp, RoadmapSpecEntry,
    RoadmapTaskEntry, RoadmapWave,
};
use acdd_core::errors::StateError;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn assemble_roadmap_output(
    config: &StateEngineConfig,
    discovered: &DiscoveredWorkspace,
    graph: &DependencyGraphData,
    active_runs_by_spec: &ActiveRunMap,
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    req: &RoadmapReq,
) -> Result<RoadmapResp, StateError> {
    let mut in_progress_ids = Vec::new();
    let mut planned_ids = Vec::new();
    for (spec_id, info) in &discovered.specs_by_id {
        if active_runs_by_spec.contains_key(spec_id) {
            in_progress_ids.push(spec_id.clone());
        } else if info.doc.status != SpecStatus::Cancelled
            && info.doc.status != SpecStatus::Completed
        {
            planned_ids.push(spec_id.clone());
        }
    }

    let (wave_layers, cycles) = compute_spec_waves(
        &planned_ids,
        &graph.unsatisfied_blockers,
        &graph.has_unsatisfied_external_blocker,
    );
    let want_tasks = |spec_id: &str| req.include_tasks || req.spec_id.as_deref() == Some(spec_id);

    let mut in_progress_entries: Vec<RoadmapSpecEntry> = Vec::new();
    for spec_id in &in_progress_ids {
        if let Some(runs) = active_runs_by_spec.get(spec_id) {
            for active_run in runs {
                in_progress_entries.push(build_spec_entry(
                    config,
                    discovered
                        .specs_by_run
                        .get(&active_run.0.run_id)
                        .unwrap_or(&discovered.specs_by_id[spec_id]),
                    "in_progress",
                    Some(active_run),
                    graph,
                    unblocks_counts,
                    want_tasks(spec_id),
                )?);
            }
        }
    }
    sort_spec_entries(&mut in_progress_entries);
    let active_info = |entry: &RoadmapSpecEntry| {
        entry
            .run_id
            .as_deref()
            .and_then(|run_id| discovered.specs_by_run.get(run_id))
            .unwrap_or(&discovered.specs_by_id[&entry.spec_id])
    };
    apply_spec_overlaps(&mut in_progress_entries, active_info);
    in_progress_entries.retain(|e| passes_display_filter(active_info(e), req));

    let mut waves_out = Vec::with_capacity(wave_layers.len());
    for (wave_idx, layer_ids) in wave_layers {
        let status = if wave_idx == 0 { "ready" } else { "planned" };
        let mut entries: Vec<RoadmapSpecEntry> = layer_ids
            .iter()
            .map(|spec_id| {
                build_spec_entry(
                    config,
                    &discovered.specs_by_id[spec_id],
                    status,
                    None,
                    graph,
                    unblocks_counts,
                    want_tasks(spec_id),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_spec_entries(&mut entries);
        apply_spec_overlaps(&mut entries, |e| &discovered.specs_by_id[&e.spec_id]);
        entries.retain(|e| passes_display_filter(&discovered.specs_by_id[&e.spec_id], req));
        if !entries.is_empty() {
            waves_out.push(RoadmapWave {
                wave: wave_idx,
                specs: entries,
            });
        }
    }

    let mut scoped_entries = Vec::new();
    for (checkout, spec_id) in &graph.scoped_planned {
        let active_here = active_runs_by_spec
            .get(spec_id)
            .is_some_and(|runs| runs.iter().any(|(run, _)| run_in_checkout(run, checkout)));
        if active_here {
            continue;
        }
        let Some(info) = materialize_checkout_spec(discovered, checkout, spec_id) else {
            continue;
        };
        if !passes_display_filter(&info, req) {
            continue;
        }
        scoped_entries.push(build_spec_entry(
            config,
            &info,
            "planned",
            None,
            graph,
            unblocks_counts,
            want_tasks(spec_id),
        )?);
    }
    if !scoped_entries.is_empty() {
        sort_spec_entries(&mut scoped_entries);
        let wave = waves_out
            .last()
            .map(|item| item.wave.saturating_add(1))
            .unwrap_or(0);
        waves_out.push(RoadmapWave {
            wave,
            specs: scoped_entries,
        });
    }

    let mut archived_entries = Vec::new();
    if req.archived
        || req
            .spec_id
            .as_deref()
            .is_some_and(|id| discovered.archived_by_id.contains_key(id))
    {
        let empty_counts = BTreeMap::new();
        for (spec_id, info) in &discovered.archived_by_id {
            if !passes_display_filter(info, req) {
                continue;
            }
            archived_entries.push(build_spec_entry(
                config,
                info,
                "archived",
                None,
                graph,
                &empty_counts,
                want_tasks(spec_id),
            )?);
        }
        archived_entries.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
    }

    if req.ready_only {
        in_progress_entries.clear();
        waves_out.retain(|w| w.wave == 0);
        archived_entries.clear();
    } else if req.active_only {
        waves_out.clear();
        archived_entries.clear();
    }

    Ok(RoadmapResp {
        in_progress: in_progress_entries,
        waves: waves_out,
        archived: archived_entries,
        archived_count: discovered.archived_by_id.len(),
        cycles,
        warnings: discovered.warnings.clone(),
    })
}

fn passes_display_filter(info: &SpecInfo, req: &RoadmapReq) -> bool {
    if let Some(ref target_id) = req.spec_id {
        if info.spec_id != *target_id {
            return false;
        }
    }
    if let Some(ref target_col) = req.collection {
        if info.collection != *target_col {
            return false;
        }
    }
    true
}

fn sort_spec_entries(entries: &mut [RoadmapSpecEntry]) {
    entries.sort_by(|a, b| {
        b.unblocks_specs
            .cmp(&a.unblocks_specs)
            .then_with(|| b.unblocks_tasks.cmp(&a.unblocks_tasks))
            .then_with(|| b.spec_id.cmp(&a.spec_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
}

fn build_spec_entry(
    config: &StateEngineConfig,
    info: &SpecInfo,
    status: &str,
    active_run: Option<&(RunRecord, Option<RunGraphSnapshot>)>,
    deps: &DependencyGraphData,
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    include_tasks: bool,
) -> Result<RoadmapSpecEntry, StateError> {
    let mut total_tasks = info.doc.tasks.len();
    let (run_id, worktree, completed_tasks, active_gates) = match active_run {
        Some((run, graph_opt)) => {
            let (completed, gates) = match graph_opt {
                Some(graph) => {
                    total_tasks = graph
                        .nodes
                        .iter()
                        .filter(|n| n.kind == NodeKind::Task)
                        .count();
                    let comp = graph
                        .nodes
                        .iter()
                        .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
                        .count();
                    let active = graph
                        .nodes
                        .iter()
                        .filter(|n| {
                            matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                                && n.status == NodeStatus::InProgress
                        })
                        .map(to_active_gate_entry)
                        .collect();
                    (comp, active)
                }
                None => (0, Vec::new()),
            };
            (
                Some(run.run_id.as_str().to_owned()),
                run.worktree.clone(),
                completed,
                gates,
            )
        }
        None => {
            let comp = info
                .doc
                .tasks
                .iter()
                .filter(|t| t.status == SpecStatus::Completed)
                .count();
            (None, None, comp, Vec::new())
        }
    };

    let (unblocks_specs, unblocks_tasks) = unblocks_counts
        .get(&info.spec_id)
        .copied()
        .unwrap_or((0, 0));

    let tasks = if include_tasks {
        build_task_entries(config, info, active_run.and_then(|(_, g)| g.as_ref()), deps)?
    } else {
        Vec::new()
    };

    Ok(RoadmapSpecEntry {
        spec_id: info.spec_id.clone(),
        title: info.doc.title.clone(),
        collection: info.collection.clone(),
        path: info.path.clone(),
        doc_type: info.doc.doc_type.clone(),
        status: status.to_owned(),
        run_id,
        worktree,
        source_worktree: info.source_worktree.clone(),
        divergence: info.divergence.clone(),
        completed_tasks,
        total_tasks,
        active_gates,
        unblocks_specs,
        unblocks_tasks,
        parallel_safe: true,
        scope_overlaps: Vec::new(),
        depends_on: match active_run {
            Some((run, _)) => deps.active_spec_deps_resolved.get(&run.run_id),
            None => deps.spec_deps_resolved.get(&info.spec_id),
        }
        .cloned()
        .unwrap_or_default(),
        blocks: {
            let scoped_key = (info.checkout.clone(), info.spec_id.clone());
            let blocks = if let Some((run, _)) = active_run {
                deps.active_spec_blocks.get(&run.run_id)
            } else if deps.scoped_planned.contains(&scoped_key) {
                deps.scoped_spec_blocks.get(&scoped_key)
            } else {
                deps.spec_blocks.get(&info.spec_id)
            };
            blocks
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default()
        },
        tasks,
    })
}

fn to_active_gate_entry(node: &GateNodeRecord) -> RoadmapActiveGateEntry {
    let cursor = match (&node.kind, &node.task_ref) {
        (NodeKind::TaskGate, Some(t_ref)) => format!("{t_ref}.{}", node.gate_id),
        _ => node.gate_id.clone(),
    };
    RoadmapActiveGateEntry {
        cursor,
        status: node.status.as_str().to_owned(),
        worker_id: node.worker_id.as_ref().map(|w| w.as_str().to_owned()),
    }
}

fn resolve_task_status_and_gates(
    info: &SpecInfo,
    t_ref: &str,
    task_status: SpecStatus,
    all_deps_ok: bool,
    graph_opt: Option<&RunGraphSnapshot>,
    claimed_scopes: &[(acdd_core::ids::TaskRef, Vec<String>)],
) -> (&'static str, Vec<RoadmapActiveGateEntry>) {
    if let Some(graph) = graph_opt {
        let task_node = graph.nodes.iter().find(|n| {
            n.kind == NodeKind::Task && n.task_ref.as_ref().map(|t| t.as_str()) == Some(t_ref)
        });
        let gates: Vec<RoadmapActiveGateEntry> = graph
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.task_ref.as_ref().map(|t| t.as_str()) == Some(t_ref)
                    && n.status == NodeStatus::InProgress
            })
            .map(to_active_gate_entry)
            .collect();
        let st = match task_node {
            Some(node) if node.status == NodeStatus::Completed => "completed",
            Some(node) if node.status == NodeStatus::Cancelled => "cancelled",
            Some(node) if node.status == NodeStatus::InProgress => "in_progress",
            Some(node)
                if crate::machine::schedule::is_task_schedule_eligible(
                    graph,
                    node,
                    claimed_scopes,
                    all_deps_ok,
                ) =>
            {
                "ready"
            }
            _ => "planned",
        };
        (st, gates)
    } else if task_status == SpecStatus::Cancelled {
        ("cancelled", Vec::new())
    } else if info.is_archived || task_status == SpecStatus::Completed {
        ("completed", Vec::new())
    } else if all_deps_ok {
        ("ready", Vec::new())
    } else {
        ("planned", Vec::new())
    }
}

fn build_active_task_entries(
    config: &StateEngineConfig,
    info: &SpecInfo,
    graph: &RunGraphSnapshot,
    projection: &DependencyGraphData,
) -> Result<Vec<RoadmapTaskEntry>, StateError> {
    let ext_deps = match projection.active_ext_deps.get(&graph.run.run_id) {
        Some(cached) => cached.clone(),
        None => collect_external_satisfied_deps_with_owned(config, graph, None)?,
    };
    let obs = ValidatedObservations {
        external_satisfied_deps: ext_deps,
        external_deps_authoritative: true,
        ..Default::default()
    };
    let task_nodes: Vec<&GateNodeRecord> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Task && n.task_ref.is_some())
        .collect();
    let completed_refs: BTreeSet<acdd_core::ids::TaskRef> = task_nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Completed)
        .filter_map(|n| n.task_ref.clone())
        .collect();
    let task_ids: Vec<String> = task_nodes
        .iter()
        .filter_map(|n| n.task_ref.as_ref().map(|t| t.as_str().to_owned()))
        .collect();

    let mut deps_by_task: BTreeMap<String, Vec<RoadmapDepEntry>> = BTreeMap::new();
    let mut unsatisfied_local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut has_unsatisfied_ext: BTreeMap<String, bool> = BTreeMap::new();
    for node in &task_nodes {
        let Some(ref task_ref) = node.task_ref else {
            continue;
        };
        let t_ref = task_ref.as_str().to_owned();
        let mut entries = Vec::new();
        for dep in graph
            .dependencies
            .iter()
            .filter(|d| &d.task_ref == task_ref)
        {
            let satisfied = crate::machine::schedule::is_dep_satisfied_against_statuses(
                &graph.run.run_id,
                dep,
                |r| completed_refs.contains(r),
                &obs,
            );
            let raw = if dep.depends_on_run_id == graph.run.run_id {
                dep.depends_on_ref.as_str().to_owned()
            } else if dep.depends_on_workspace == "." {
                format!("{}:{}", dep.depends_on_run_id.as_str(), dep.depends_on_ref.as_str())
            } else {
                format!(
                    "{}/{}:{}",
                    dep.depends_on_workspace,
                    dep.depends_on_run_id.as_str(),
                    dep.depends_on_ref.as_str()
                )
            };
            if !satisfied {
                if dep.depends_on_run_id == graph.run.run_id {
                    unsatisfied_local
                        .entry(t_ref.clone())
                        .or_default()
                        .insert(dep.depends_on_ref.as_str().to_owned());
                } else {
                    has_unsatisfied_ext.insert(t_ref.clone(), true);
                }
            }
            entries.push(RoadmapDepEntry {
                raw,
                status: if satisfied { "satisfied" } else { "planned" }.to_owned(),
                satisfied,
            });
        }
        deps_by_task.insert(t_ref, entries);
    }

    let task_wave = compute_task_waves(&task_ids, &unsatisfied_local, &has_unsatisfied_ext);
    let claimed_scopes = crate::machine::schedule::claimed_unfinished_scopes(graph);
    let mut out = Vec::with_capacity(task_nodes.len());
    for node in &task_nodes {
        let Some(ref task_ref) = node.task_ref else {
            continue;
        };
        let t_ref = task_ref.as_str().to_owned();
        let disk_task = info.doc.tasks.iter().find(|t| t.task_ref.as_str() == t_ref);
        let deps = deps_by_task.remove(&t_ref).unwrap_or_default();
        let all_deps_ok = crate::machine::schedule::task_dependencies_satisfied(graph, task_ref, &obs);
        let (status, active_gates) = resolve_task_status_and_gates(
            info,
            &t_ref,
            disk_task.map_or(SpecStatus::Planned, |t| t.status),
            all_deps_ok,
            Some(graph),
            &claimed_scopes,
        );
        let mut scope = crate::machine::graph::container_scope(graph, node);
        if scope.is_empty() {
            if let Some(dt) = disk_task {
                scope.clone_from(&dt.scope);
                for ext in &dt.extended_scope {
                    if !scope.contains(&ext.path) {
                        scope.push(ext.path.clone());
                    }
                }
            }
        }
        out.push(RoadmapTaskEntry {
            task_ref: t_ref.clone(),
            target: disk_task.map(|t| t.target.clone()).unwrap_or_default(),
            status: status.to_owned(),
            wave: task_wave.get(&t_ref).copied().unwrap_or(0),
            depends_on: deps,
            blocks: projection
                .active_task_blocks
                .get(&(graph.run.run_id.clone(), task_ref.clone()))
                .map(|blocks| blocks.iter().cloned().collect())
                .unwrap_or_default(),
            scope,
            scope_overlaps: Vec::new(),
            active_gates,
        });
    }
    apply_task_overlaps(&mut out);
    Ok(out)
}

fn build_task_entries(
    config: &StateEngineConfig,
    info: &SpecInfo,
    graph_opt: Option<&RunGraphSnapshot>,
    projection: &DependencyGraphData,
) -> Result<Vec<RoadmapTaskEntry>, StateError> {
    if let Some(graph) = graph_opt {
        return build_active_task_entries(config, info, graph, projection);
    }

    let mut deps_by_task: BTreeMap<String, Vec<RoadmapDepEntry>> = BTreeMap::new();
    let mut unsatisfied_local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut has_unsatisfied_ext: BTreeMap<String, bool> = BTreeMap::new();

    for task in &info.doc.tasks {
        let t_ref = task.task_ref.as_str().to_owned();
        if let Some(resolved) = projection
            .task_deps_resolved
            .get(&(info.spec_id.clone(), t_ref.clone()))
        {
            let mut entries = Vec::with_capacity(resolved.len());
            for (parsed_opt, entry) in resolved {
                if !entry.satisfied {
                    if let Some(ParsedDepSyntax::LocalTask(blocker)) = parsed_opt {
                        unsatisfied_local
                            .entry(t_ref.clone())
                            .or_default()
                            .insert(blocker.clone());
                    } else {
                        has_unsatisfied_ext.insert(t_ref.clone(), true);
                    }
                }
                entries.push(entry.clone());
            }
            deps_by_task.insert(t_ref, entries);
        }
    }

    let task_ids: Vec<String> = info
        .doc
        .tasks
        .iter()
        .map(|t| t.task_ref.as_str().to_owned())
        .collect();
    let task_wave = compute_task_waves(&task_ids, &unsatisfied_local, &has_unsatisfied_ext);
    let mut out = Vec::with_capacity(info.doc.tasks.len());
    for task in &info.doc.tasks {
        let t_ref = task.task_ref.as_str().to_owned();
        let deps = deps_by_task.remove(&t_ref).unwrap_or_default();
        let all_deps_ok = deps.iter().all(|d| d.satisfied);
        let (status, active_gates) =
            resolve_task_status_and_gates(info, &t_ref, task.status, all_deps_ok, None, &[]);

        let mut scope: Vec<String> = task.scope.clone();
        for ext in &task.extended_scope {
            if !scope.contains(&ext.path) {
                scope.push(ext.path.clone());
            }
        }

        out.push(RoadmapTaskEntry {
            task_ref: t_ref.clone(),
            target: task.target.clone(),
            status: status.to_owned(),
            wave: task_wave.get(&t_ref).copied().unwrap_or(0),
            depends_on: deps,
            blocks: {
                let scoped_key = (info.checkout.clone(), info.spec_id.clone());
                let blocks = if projection.scoped_planned.contains(&scoped_key) {
                    projection.scoped_task_blocks.get(&(
                        info.checkout.clone(),
                        info.spec_id.clone(),
                        t_ref.clone(),
                    ))
                } else {
                    projection
                        .task_blocks
                        .get(&(info.spec_id.clone(), t_ref.clone()))
                };
                blocks
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default()
            },
            scope,
            scope_overlaps: Vec::new(),
            active_gates,
        });
    }

    apply_task_overlaps(&mut out);
    Ok(out)
}
