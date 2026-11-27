//! Roadmap DTO assembly, filtering, sorting, and per-task drill-down rendering.

use super::discovery::materialize_checkout_spec;
use super::types::{ActiveRunMap, DependencyGraphData, DiscoveredWorkspace, SpecInfo};
use super::waves::{
    apply_spec_overlaps, apply_task_overlaps, compute_spec_waves, compute_task_waves,
};
use crate::deps::ParsedDepSyntax;
use crate::overlay::{RuntimeActiveRun, SpecRuntimeOverlay};
use crate::spec::SpecStatus;
use crate::types::{
    RoadmapDepEntry, RoadmapReq, RoadmapResp, RoadmapSpecEntry, RoadmapTaskEntry, RoadmapWave,
};
use acdd_core::errors::AcddError;
use acdd_core::fs::canonical_root;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(super) fn assemble_roadmap_output(
    discovered: &DiscoveredWorkspace,
    graph: &DependencyGraphData,
    overlay: &dyn SpecRuntimeOverlay,
    active_runs_by_spec: &ActiveRunMap,
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    req: &RoadmapReq,
) -> Result<RoadmapResp, AcddError> {
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
                let info = discovered
                    .specs_by_run
                    .get(&active_run.run_id)
                    .unwrap_or(&discovered.specs_by_id[spec_id]);
                in_progress_entries.push(build_spec_entry(
                    info,
                    "in_progress",
                    Some(active_run),
                    graph,
                    overlay,
                    &discovered.owned_worktrees,
                    unblocks_counts,
                    want_tasks(spec_id) && passes_display_filter(info, req),
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
                let info = &discovered.specs_by_id[spec_id];
                build_spec_entry(
                    info,
                    status,
                    None,
                    graph,
                    overlay,
                    &discovered.owned_worktrees,
                    unblocks_counts,
                    want_tasks(spec_id) && passes_display_filter(info, req),
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
        let active_here = active_runs_by_spec.get(spec_id).is_some_and(|runs| {
            runs.iter().any(|run| {
                run.worktree
                    .as_deref()
                    .and_then(|p| canonical_root(Path::new(p)).ok())
                    .is_some_and(|wt| &wt == checkout)
            })
        });
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
            &info,
            "planned",
            None,
            graph,
            overlay,
            &discovered.owned_worktrees,
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
                info,
                "archived",
                None,
                graph,
                overlay,
                &discovered.owned_worktrees,
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
    if let Some(ref target_col) = req.spec {
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

#[allow(clippy::too_many_arguments)]
fn build_spec_entry(
    info: &SpecInfo,
    status: &str,
    active_run: Option<&RuntimeActiveRun>,
    deps: &DependencyGraphData,
    overlay: &dyn SpecRuntimeOverlay,
    owned_worktrees: &[PathBuf],
    unblocks_counts: &BTreeMap<String, (usize, usize)>,
    include_tasks: bool,
) -> Result<RoadmapSpecEntry, AcddError> {
    let (total_tasks, run_id, worktree, completed_tasks, active_gates) = match active_run {
        Some(run) => (
            if run.total_tasks == 0 && !info.doc.tasks.is_empty() {
                info.doc.tasks.len()
            } else {
                run.total_tasks
            },
            Some(run.run_id.as_str().to_owned()),
            run.worktree.clone(),
            run.completed_tasks,
            run.active_gates.clone(),
        ),
        None => {
            let comp = info
                .doc
                .tasks
                .iter()
                .filter(|t| t.status == SpecStatus::Completed)
                .count();
            (info.doc.tasks.len(), None, None, comp, Vec::new())
        }
    };

    let (unblocks_specs, unblocks_tasks) = unblocks_counts
        .get(&info.spec_id)
        .copied()
        .unwrap_or((0, 0));

    let tasks = if include_tasks {
        build_task_entries(info, active_run, deps, overlay, owned_worktrees)?
    } else {
        Vec::new()
    };

    Ok(RoadmapSpecEntry {
        spec_id: info.spec_id.clone(),
        title: info.doc.title.clone(),
        spec: info.collection.clone(),
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
            Some(run) => deps.active_spec_deps_resolved.get(&run.run_id),
            None => deps.spec_deps_resolved.get(&info.spec_id),
        }
        .cloned()
        .unwrap_or_default(),
        blocks: {
            let scoped_key = (info.checkout.clone(), info.spec_id.clone());
            let blocks = if let Some(run) = active_run {
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

fn build_active_task_entries(
    info: &SpecInfo,
    run: &RuntimeActiveRun,
    projection: &DependencyGraphData,
    overlay: &dyn SpecRuntimeOverlay,
    owned_worktrees: &[PathBuf],
) -> Result<Vec<RoadmapTaskEntry>, AcddError> {
    let projected_tasks = overlay.project_active_run_tasks(&run.run_id, owned_worktrees)?;
    let task_ids: Vec<String> = projected_tasks.iter().map(|t| t.task_ref.clone()).collect();
    let mut unsatisfied_local: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut has_unsatisfied_ext: BTreeMap<String, bool> = BTreeMap::new();
    for task in &projected_tasks {
        if !task.unsatisfied_local_blockers.is_empty() {
            unsatisfied_local.insert(
                task.task_ref.clone(),
                task.unsatisfied_local_blockers.clone(),
            );
        }
        if task.has_unsatisfied_external_dep {
            has_unsatisfied_ext.insert(task.task_ref.clone(), true);
        }
    }
    let task_wave = compute_task_waves(&task_ids, &unsatisfied_local, &has_unsatisfied_ext);
    let mut out = Vec::with_capacity(projected_tasks.len());
    for task in &projected_tasks {
        let t_ref = &task.task_ref;
        let disk_task = info
            .doc
            .tasks
            .iter()
            .find(|t| t.task_ref.as_str() == t_ref.as_str());
        let mut scope = task.scope.clone();
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
        let task_ref_id = acdd_core::ids::TaskRef::new(t_ref)?;
        out.push(RoadmapTaskEntry {
            task_ref: t_ref.clone(),
            target: disk_task.map(|t| t.target.clone()).unwrap_or_default(),
            status: task.status.clone(),
            wave: task_wave.get(t_ref).copied().unwrap_or(0),
            depends_on: task.depends_on.clone(),
            blocks: projection
                .active_task_blocks
                .get(&(run.run_id.clone(), task_ref_id))
                .map(|blocks| blocks.iter().cloned().collect())
                .unwrap_or_default(),
            scope,
            scope_overlaps: Vec::new(),
            active_gates: task.active_gates.clone(),
        });
    }
    apply_task_overlaps(&mut out);
    Ok(out)
}

fn build_task_entries(
    info: &SpecInfo,
    active_run: Option<&RuntimeActiveRun>,
    projection: &DependencyGraphData,
    overlay: &dyn SpecRuntimeOverlay,
    owned_worktrees: &[PathBuf],
) -> Result<Vec<RoadmapTaskEntry>, AcddError> {
    if let Some(run) = active_run {
        return build_active_task_entries(info, run, projection, overlay, owned_worktrees);
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
        let status = if task.status == SpecStatus::Cancelled {
            "cancelled"
        } else if info.is_archived || task.status == SpecStatus::Completed {
            "completed"
        } else if all_deps_ok {
            "ready"
        } else {
            "planned"
        };

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
            active_gates: Vec::new(),
        });
    }

    apply_task_overlaps(&mut out);
    Ok(out)
}
