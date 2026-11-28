//! Roadmap DTO assembly, filtering, sorting, and per-task drill-down rendering.

use super::discovery::materialize_checkout_spec;
use super::types::{
    checkout_matches_run, ActiveRunMap, DependencyGraphData, DiscoveredWorkspace, SpecInfo,
};
use super::waves::{
    apply_spec_overlaps, apply_task_overlaps, compute_spec_waves, compute_task_waves,
    compute_unblocks_counts,
};
use crate::deps::ParsedDepSyntax;
use crate::overlay::{RuntimeActiveRun, SpecRuntimeOverlay};
use crate::spec::{SpecDocument, SpecStatus};
use crate::types::{
    RoadmapBadge, RoadmapDepEntry, RoadmapGroup, RoadmapReq, RoadmapResp, RoadmapSpecEntry,
    RoadmapTaskEntry, RoadmapWave,
};
use acdd_core::errors::AcddError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub(super) fn resolve_spec_id_selector(
    discovered: &DiscoveredWorkspace,
    query: &str,
) -> Option<String> {
    let mut all_ids: BTreeSet<&str> = discovered
        .specs_by_id
        .keys()
        .chain(discovered.archived_by_id.keys())
        .map(String::as_str)
        .collect();
    for checkout in discovered.checkout_specs.values() {
        all_ids.extend(checkout.active_docs.iter().map(|doc| doc.id.as_str()));
    }
    if all_ids.contains(query) {
        return Some(query.to_owned());
    }
    let prefix = format!("{query}-");
    let mut matches = all_ids.into_iter().filter(|id| id.starts_with(&prefix));
    let first = matches.next()?;
    matches.next().is_none().then(|| first.to_owned())
}

pub(super) fn assemble_roadmap_output(
    discovered: &DiscoveredWorkspace,
    graph: &DependencyGraphData,
    overlay: &dyn SpecRuntimeOverlay,
    active_runs_by_spec: &ActiveRunMap,
    req: &RoadmapReq,
) -> Result<RoadmapResp, AcddError> {
    let visible_specs: BTreeMap<&str, &SpecInfo> = discovered
        .specs_by_id
        .iter()
        .filter(|(spec_id, info)| {
            passes_schema_visibility(info, req)
                || active_runs_by_spec.get(spec_id.as_str()).is_some_and(|runs| {
                    runs.iter().any(|active_run| {
                        discovered
                            .active_run_spec(active_run, spec_id)
                            .is_some_and(|run_info| passes_schema_visibility(run_info, req))
                    })
                })
        })
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let unblocks_counts = compute_unblocks_counts(&visible_specs, &graph.forward_spec_edges);

    let mut planned_ids = Vec::new();
    for (spec_id, info) in &discovered.specs_by_id {
        let has_active_run = active_runs_by_spec
            .get(spec_id.as_str())
            .is_some_and(|runs| discovered.has_resolved_active_run(spec_id, runs));
        if !has_active_run
            && passes_schema_visibility(info, req)
            && info.doc.status != SpecStatus::Cancelled
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
    for (spec_id, runs) in active_runs_by_spec {
        for active_run in runs {
            let Some(info) = discovered.active_run_spec(active_run, spec_id) else {
                continue;
            };
            if !passes_schema_visibility(info, req) {
                continue;
            }
            in_progress_entries.push(build_spec_entry(
                info,
                "in_progress",
                Some(active_run),
                graph,
                overlay,
                &discovered.owned_worktrees,
                &unblocks_counts,
                &req.badges,
                want_tasks(spec_id)
                    && passes_display_filter(info, Some("in_progress"), req),
            )?);
        }
    }
    sort_spec_entries(&mut in_progress_entries);
    apply_spec_overlaps(&mut in_progress_entries, |e| {
        discovered.entry_spec_info(e, active_runs_by_spec, false)
    });
    in_progress_entries.retain(|e| {
        discovered
            .entry_spec_info(e, active_runs_by_spec, false)
            .is_some_and(|info| passes_display_filter(info, Some("in_progress"), req))
    });

    let mut waves_out = Vec::with_capacity(wave_layers.len());
    for (wave_idx, layer_ids) in wave_layers {
        let status = if wave_idx == 0 { "ready" } else { "planned" };
        let mut entries: Vec<RoadmapSpecEntry> = layer_ids
            .iter()
            .filter_map(|spec_id| {
                let info = &discovered.specs_by_id[spec_id];
                passes_schema_visibility(info, req).then(|| {
                    build_spec_entry(
                        info,
                        status,
                        None,
                        graph,
                        overlay,
                        &discovered.owned_worktrees,
                        &unblocks_counts,
                        &req.badges,
                        want_tasks(spec_id)
                            && passes_display_filter(info, Some(status), req),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_spec_entries(&mut entries);
        apply_spec_overlaps(&mut entries, |e| discovered.specs_by_id.get(&e.spec_id));
        entries.retain(|e| {
            passes_display_filter(
                &discovered.specs_by_id[&e.spec_id],
                Some(status),
                req,
            )
        });
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
                checkout_matches_run(
                    run.worktree.as_deref(),
                    run.branch.as_deref(),
                    checkout,
                    &discovered.worktree_branches,
                )
            })
        });
        if active_here {
            continue;
        }
        let Some(info) = materialize_checkout_spec(discovered, checkout, spec_id) else {
            continue;
        };
        if !passes_display_filter(&info, Some("planned"), req) {
            continue;
        }
        scoped_entries.push(build_spec_entry(
            &info,
            "planned",
            None,
            graph,
            overlay,
            &discovered.owned_worktrees,
            &unblocks_counts,
            &req.badges,
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
            if !passes_display_filter(info, Some("archived"), req) {
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
                &req.badges,
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

    let group_by = req
        .group_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let groups = match group_by.as_deref() {
        Some(axis) if axis != "wave" => build_roadmap_groups(
            axis,
            &in_progress_entries,
            &waves_out,
            &archived_entries,
            discovered,
            active_runs_by_spec,
        ),
        _ => Vec::new(),
    };

    let mut warnings = discovered.warnings.clone();
    for (spec_id, warning) in &discovered.divergence_warnings {
        if req
            .spec_id
            .as_deref()
            .is_none_or(|target| target == spec_id)
        {
            warnings.push(warning.clone());
        }
    }

    Ok(RoadmapResp {
        in_progress: in_progress_entries,
        waves: waves_out,
        archived: archived_entries,
        archived_count: discovered.archived_by_id.len(),
        cycles,
        group_by,
        groups,
        warnings,
    })
}

fn passes_schema_visibility(info: &SpecInfo, req: &RoadmapReq) -> bool {
    req.all
        || req.spec.as_deref() == Some(info.effective_spec.as_str())
        || req.spec_id.as_deref() == Some(info.spec_id.as_str())
        || info.roadmap_visible
}

fn scalar_field_str(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(if *b { "true" } else { "false" }.to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn format_field_value(val: &Value) -> Option<String> {
    if let Some(s) = scalar_field_str(val) {
        return (!s.trim().is_empty()).then_some(s);
    }
    if let Value::Array(items) = val {
        let joined: Vec<String> = items
            .iter()
            .filter_map(scalar_field_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        return (!joined.is_empty()).then(|| joined.join(", "));
    }
    None
}

fn collect_spec_tags(doc: &SpecDocument) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    if let Some(val) = doc.fields.get("tags") {
        match val {
            Value::String(s) if !s.trim().is_empty() => {
                tags.insert(s.clone());
            }
            Value::Array(items) => {
                for item in items {
                    if let Some(s) = item.as_str().filter(|s| !s.trim().is_empty()) {
                        tags.insert(s.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    for task in &doc.tasks {
        tags.extend(task.tags.iter().cloned());
    }
    for item in doc.items.values().flatten() {
        tags.extend(item.tags.iter().cloned());
    }
    tags
}

fn spec_status_str(status: SpecStatus) -> &'static str {
    match status {
        SpecStatus::Planned => "planned",
        SpecStatus::Active => "active",
        SpecStatus::Completed => "completed",
        SpecStatus::Cancelled => "cancelled",
    }
}

fn readiness_label(status: &str) -> &str {
    if status == "planned" {
        "blocked"
    } else {
        status
    }
}

fn resolve_spec_field_value(
    info: &SpecInfo,
    effective_spec: &str,
    status: Option<&str>,
    key: &str,
) -> Option<String> {
    match key {
        "readiness" => status.map(|s| readiness_label(s).to_owned()),
        "spec" => Some(effective_spec.to_owned()),
        "status" => Some(
            status
                .map(str::to_owned)
                .unwrap_or_else(|| spec_status_str(info.doc.status).to_owned()),
        ),
        "doc_type" => Some(info.doc.doc_type.clone()),
        "workflow" => info.doc.workflow.clone(),
        "id" => Some(info.spec_id.clone()),
        "tags" => {
            let joined: Vec<String> = collect_spec_tags(&info.doc).into_iter().collect();
            (!joined.is_empty()).then(|| joined.join(", "))
        }
        other => info.doc.fields.get(other).and_then(format_field_value),
    }
}

fn spec_matches_field(
    info: &SpecInfo,
    effective_spec: &str,
    status: Option<&str>,
    key: &str,
    expected: &str,
) -> bool {
    match key {
        "readiness" => status.map(readiness_label) == Some(expected),
        "spec" => effective_spec == expected,
        "status" => status.unwrap_or_else(|| spec_status_str(info.doc.status)) == expected,
        "doc_type" => info.doc.doc_type == expected,
        "workflow" => info.doc.workflow.as_deref() == Some(expected),
        "id" => info.spec_id == expected,
        "tags" => collect_spec_tags(&info.doc).contains(expected),
        other => match info.doc.fields.get(other) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(scalar_field_str)
                .any(|s| s == expected),
            Some(val) => scalar_field_str(val).as_deref() == Some(expected),
            None => false,
        },
    }
}

fn project_badges(
    info: &SpecInfo,
    effective_spec: &str,
    status: &str,
    badge_names: &[String],
) -> Vec<RoadmapBadge> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in badge_names {
        let key = raw.trim();
        if key.is_empty() || !seen.insert(key.to_owned()) {
            continue;
        }
        if let Some(value) = resolve_spec_field_value(info, effective_spec, Some(status), key) {
            out.push(RoadmapBadge {
                name: key.to_owned(),
                value,
            });
        }
    }
    out
}

/// Group all entries in a [`RoadmapResp`] along `axis` (`spec`, `status`, `doc_type`,
/// `readiness`, or a projected badge name), placing entries without a value into `(none)`.
pub fn group_roadmap_resp(resp: &RoadmapResp, axis: &str) -> Vec<RoadmapGroup> {
    group_roadmap_entries_with(
        axis,
        &resp.in_progress,
        &resp.waves,
        &resp.archived,
        |_, _| None,
    )
}

fn group_roadmap_entries_with<F>(
    axis: &str,
    in_progress: &[RoadmapSpecEntry],
    waves: &[RoadmapWave],
    archived: &[RoadmapSpecEntry],
    mut resolve_custom: F,
) -> Vec<RoadmapGroup>
where
    F: FnMut(&RoadmapSpecEntry, &str) -> Option<String>,
{
    let mut buckets: BTreeMap<String, Vec<RoadmapSpecEntry>> = BTreeMap::new();
    let mut none_bucket: Vec<RoadmapSpecEntry> = Vec::new();
    let all_entries = in_progress
        .iter()
        .chain(waves.iter().flat_map(|w| &w.specs))
        .chain(archived.iter());

    for entry in all_entries {
        let group_val = match axis {
            "spec" => Some(entry.spec.clone()),
            "status" => Some(entry.status.clone()),
            "doc_type" => Some(entry.doc_type.clone()),
            "readiness" => Some(readiness_label(&entry.status).to_owned()),
            other => resolve_custom(entry, other).or_else(|| {
                entry
                    .badges
                    .iter()
                    .find(|b| b.name == other)
                    .map(|b| b.value.clone())
            }),
        };
        match group_val.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(label) => {
                buckets
                    .entry(label.to_owned())
                    .or_default()
                    .push(entry.clone());
            }
            None => none_bucket.push(entry.clone()),
        }
    }

    let mut groups: Vec<RoadmapGroup> = buckets
        .into_iter()
        .map(|(group, specs)| RoadmapGroup { group, specs })
        .collect();
    if !none_bucket.is_empty() {
        groups.push(RoadmapGroup {
            group: "(none)".to_owned(),
            specs: none_bucket,
        });
    }
    groups
}

fn build_roadmap_groups(
    axis: &str,
    in_progress: &[RoadmapSpecEntry],
    waves: &[RoadmapWave],
    archived: &[RoadmapSpecEntry],
    discovered: &DiscoveredWorkspace,
    active_runs_by_spec: &ActiveRunMap,
) -> Vec<RoadmapGroup> {
    group_roadmap_entries_with(axis, in_progress, waves, archived, |entry, other| {
        discovered
            .entry_spec_info(entry, active_runs_by_spec, true)
            .and_then(|info| resolve_spec_field_value(info, &entry.spec, Some(&entry.status), other))
    })
}

fn passes_display_filter(
    info: &SpecInfo,
    status: Option<&str>,
    req: &RoadmapReq,
) -> bool {
    let effective_spec = info.effective_spec.as_str();
    if let Some(ref target_id) = req.spec_id {
        if info.spec_id != *target_id {
            return false;
        }
    }
    if let Some(ref target_col) = req.spec {
        if effective_spec != *target_col {
            return false;
        }
    }
    if !passes_schema_visibility(info, req) {
        return false;
    }
    if !req.tags.is_empty() {
        let doc_tags = collect_spec_tags(&info.doc);
        if !req.tags.iter().all(|t| doc_tags.contains(t.trim())) {
            return false;
        }
    }
    for raw in &req.fields {
        let Some((key, val)) = raw.split_once('=') else {
            return false;
        };
        if !spec_matches_field(info, effective_spec, status, key.trim(), val.trim()) {
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
    badge_names: &[String],
    include_tasks: bool,
) -> Result<RoadmapSpecEntry, AcddError> {
    let effective_spec = &info.effective_spec;
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
        spec: effective_spec.clone(),
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
        badges: project_badges(info, effective_spec, status, badge_names),
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
