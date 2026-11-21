//! Cross-spec, active-run, and linked-workspace dependency graph resolution for roadmap synthesis.

use super::discovery::{checkout_spec_id, checkout_specs_from, scan_checkout_specs};
use super::{
    ActiveRunMap, CheckoutSpecs, DependencyGraphData, DiscoveredWorkspace, LinkedWsCache, SpecInfo,
};
use crate::machine::schedule::is_dep_satisfied_against_statuses;
use crate::spec::SpecDocument;
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{NodeKind, NodeStatus, RunGraphSnapshot, ValidatedObservations};
use crate::usecases::deps::{
    checkout_scoped_target_selector, collect_external_satisfied_deps_cached,
    effective_active_run_config, load_dep_snapshot, load_task_status_map, load_tombstone_archives,
    load_ws_adapter, parse_dep_syntax, resolve_dep_target, resolve_linked_workspace_root,
    resolve_spec_repositories, resolve_ws_context_canon, run_in_checkout, run_matches_selector,
    DepTargetResolution, ParsedDepSyntax, PreloadedDepContext,
};
use crate::usecases::dto::RoadmapDepEntry;
use crate::usecases::lifecycle::{effective_spec_collections, list_all_runs, list_all_tombstones};
use crate::usecases::StateEngineConfig;
use acdd_core::config::SpecCollectionConfig;
use acdd_core::errors::StateError;
use acdd_core::fs::canonical_root;
use acdd_core::ids::{RunId, TaskRef};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[allow(clippy::too_many_arguments)]
fn process_dep_edge(
    raw: &str,
    source: &SpecInfo,
    source_task_ref: Option<&str>,
    local_refs: &BTreeSet<&str>,
    annotation_run: Option<&RunId>,
    discovered: &DiscoveredWorkspace,
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    linked_cache: &mut LinkedWsCache,
    graph: &mut DependencyGraphData,
    warnings: &mut Vec<String>,
) -> Result<(Option<ParsedDepSyntax>, RoadmapDepEntry), StateError> {
    let source_spec_id = &source.spec_id;
    let record_in_graph = !source.is_archived && annotation_run.is_none();
    let mut parsed = match parse_dep_syntax(raw, local_refs) {
        Ok(p) => p,
        Err(err) => {
            let ctx = match source_task_ref {
                Some(t) => format!("spec '{source_spec_id}' task '{t}'"),
                None => format!("spec '{source_spec_id}'"),
            };
            warnings.push(format!("{ctx} malformed depends_on '{raw}': {err}"));
            if record_in_graph && source_task_ref.is_none() {
                graph
                    .has_unsatisfied_external_blocker
                    .insert(source_spec_id.to_owned(), true);
            }
            return Ok((
                None,
                RoadmapDepEntry {
                    raw: raw.to_owned(),
                    status: "invalid".to_owned(),
                    satisfied: false,
                },
            ));
        }
    };

    let resolution = resolve_dep_entry(
        raw,
        &mut parsed,
        source,
        root,
        annotation_run.is_some(),
        collections,
        config,
        reader,
        discovered,
        linked_cache,
        warnings,
    );
    let (entry, active_target) = match (resolution, annotation_run) {
        (Ok(resolved), _) => resolved,
        (Err(err), Some(run_id)) => {
            warnings.push(format!(
                "spec '{source_spec_id}' run '{run_id}' depends_on '{raw}': {err}"
            ));
            return Ok((
                None,
                RoadmapDepEntry {
                    raw: raw.to_owned(),
                    status: "invalid".to_owned(),
                    satisfied: false,
                },
            ));
        }
        (Err(err), None) => return Err(err),
    };

    if record_in_graph {
        record_reverse_block(
            &parsed,
            source_spec_id,
            source_task_ref,
            &mut graph.spec_blocks,
            &mut graph.task_blocks,
        );
        if let Some(target_run) = &active_target {
            let label = source_task_ref.map_or_else(
                || source_spec_id.to_owned(),
                |source_task| format!("{source_spec_id}:{source_task}"),
            );
            graph
                .active_spec_blocks
                .entry(target_run.clone())
                .or_default()
                .insert(label);
        }
        if let (
            Some(target_run),
            ParsedDepSyntax::LocalSpec {
                task_ref: Some(task_ref),
                ..
            }
            | ParsedDepSyntax::LinkedSpec {
                task_ref: Some(task_ref),
                ..
            },
        ) = (active_target, &parsed)
        {
            let label = source_task_ref.map_or_else(
                || source_spec_id.to_owned(),
                |source_task| format!("{source_spec_id}:{source_task}"),
            );
            graph
                .active_task_blocks
                .entry((target_run, TaskRef::new(task_ref)?))
                .or_default()
                .insert(label);
        }
        if !entry.satisfied && !matches!(parsed, ParsedDepSyntax::LocalTask(_)) {
            match &parsed {
                ParsedDepSyntax::LocalSpec {
                    spec_id: target_spec,
                    ..
                } if discovered.specs_by_id.contains_key(target_spec)
                    && (target_spec != source_spec_id || source_task_ref.is_none()) =>
                {
                    graph
                        .forward_spec_edges
                        .entry(target_spec.clone())
                        .or_default()
                        .insert(source_spec_id.to_owned());
                    if source_task_ref.is_none() {
                        graph
                            .unsatisfied_blockers
                            .entry(source_spec_id.to_owned())
                            .or_default()
                            .insert(target_spec.clone());
                    }
                }
                _ => {
                    if source_task_ref.is_none() {
                        graph
                            .has_unsatisfied_external_blocker
                            .insert(source_spec_id.to_owned(), true);
                    }
                }
            }
        }
    }

    Ok((Some(parsed), entry))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_dependency_graph(
    discovered: &DiscoveredWorkspace,
    root: &Path,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    active_runs_by_spec: &ActiveRunMap,
    include_archived: bool,
    warnings: &mut Vec<String>,
) -> Result<DependencyGraphData, StateError> {
    let mut graph = DependencyGraphData {
        spec_deps_resolved: BTreeMap::new(),
        active_spec_deps_resolved: BTreeMap::new(),
        task_deps_resolved: BTreeMap::new(),
        forward_spec_edges: BTreeMap::new(),
        unsatisfied_blockers: BTreeMap::new(),
        has_unsatisfied_external_blocker: BTreeMap::new(),
        spec_blocks: BTreeMap::new(),
        active_spec_blocks: BTreeMap::new(),
        task_blocks: BTreeMap::new(),
        active_task_blocks: BTreeMap::new(),
        scoped_planned: BTreeSet::new(),
        scoped_spec_blocks: BTreeMap::new(),
        scoped_task_blocks: BTreeMap::new(),
        active_ext_deps: BTreeMap::new(),
    };
    let mut linked_cache = LinkedWsCache {
        runs: list_all_runs(reader, None)?,
        tombstones: list_all_tombstones(reader)?,
        ..Default::default()
    };
    let has_task_deps = discovered
        .specs_by_id
        .values()
        .chain(discovered.archived_by_id.values().filter(|_| include_archived))
        .chain(discovered.specs_by_run.values())
        .any(|info| {
            info.doc.depends_on.iter().any(|d| d.contains(':'))
                || info
                    .doc
                    .tasks
                    .iter()
                    .any(|t| t.depends_on.iter().any(|d| d.contains(':')))
        });
    linked_cache.task_statuses = load_task_status_map(reader)?;
    if has_task_deps {
        linked_cache.tombstone_archives = load_tombstone_archives(root, &linked_cache.tombstones);
    }
    let empty_refs = BTreeSet::new();

    let planned = discovered
        .specs_by_id
        .values()
        .filter(|info| !active_runs_by_spec.contains_key(&info.spec_id))
        .chain(
            discovered
                .archived_by_id
                .values()
                .filter(|_| include_archived),
        )
        .map(|info| (info, None));
    let active = active_runs_by_spec.iter().flat_map(|(id, runs)| {
        runs.iter().filter_map(move |(run, _)| {
            let info = discovered
                .specs_by_run
                .get(&run.run_id)
                .or_else(|| discovered.specs_by_id.get(id))?;
            Some((info, Some(run)))
        })
    });
    for (info, run) in planned.chain(active) {
        let effective = run
            .filter(|_| !info.doc.depends_on.is_empty())
            .map(|run| {
                effective_active_run_config(config, run, root, Some(&discovered.owned_worktrees))
            })
            .transpose()?;
        let spec_config = effective.as_deref().unwrap_or(config);
        let spec_collections = effective
            .as_ref()
            .map(|cfg| effective_spec_collections(cfg));
        let mut deps_out = Vec::with_capacity(info.doc.depends_on.len());
        for raw in &info.doc.depends_on {
            let (_, entry) = process_dep_edge(
                raw,
                info,
                None,
                &empty_refs,
                run.map(|run| &run.run_id),
                discovered,
                root,
                spec_collections.as_ref().unwrap_or(collections),
                spec_config,
                reader,
                &mut linked_cache,
                &mut graph,
                warnings,
            )?;
            deps_out.push(entry);
        }
        if let Some(run) = run {
            graph
                .active_spec_deps_resolved
                .insert(run.run_id.clone(), deps_out);
            continue;
        }
        graph
            .spec_deps_resolved
            .insert(info.spec_id.clone(), deps_out);

        let local_refs: BTreeSet<&str> =
            info.doc.tasks.iter().map(|t| t.task_ref.as_str()).collect();
        for task in &info.doc.tasks {
            let t_ref = task.task_ref.as_str();
            let mut t_entries = Vec::with_capacity(task.depends_on.len());
            for raw in &task.depends_on {
                let (parsed_opt, entry) = process_dep_edge(
                    raw,
                    info,
                    Some(t_ref),
                    &local_refs,
                    None,
                    discovered,
                    root,
                    collections,
                    config,
                    reader,
                    &mut linked_cache,
                    &mut graph,
                    warnings,
                )?;
                t_entries.push((parsed_opt, entry));
            }
            graph
                .task_deps_resolved
                .insert((info.spec_id.clone(), t_ref.to_owned()), t_entries);
        }
    }

    record_active_blocks(config, discovered, root, active_runs_by_spec, &mut graph)?;
    Ok(graph)
}

fn record_active_blocks(
    config: &StateEngineConfig,
    discovered: &DiscoveredWorkspace,
    root: &Path,
    runs: &ActiveRunMap,
    graph: &mut DependencyGraphData,
) -> Result<(), StateError> {
    let snapshots: BTreeMap<_, _> = runs
        .iter()
        .flat_map(|(spec_id, runs)| {
            runs.iter().filter_map(move |(run, graph)| {
                graph.as_ref().map(|graph| (&run.run_id, (spec_id, graph)))
            })
        })
        .collect();
    let mut external_cache = BTreeMap::new();
    for (&source_run, &(source_spec, source)) in &snapshots {
        let ext_deps = collect_external_satisfied_deps_cached(
            config,
            source,
            Some(&discovered.owned_worktrees),
            &mut external_cache,
        )?;
        graph
            .active_ext_deps
            .insert(source_run.clone(), ext_deps.clone());
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            ..ValidatedObservations::default()
        };
        let completed_refs: BTreeSet<TaskRef> = source
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Task && node.status == NodeStatus::Completed)
            .filter_map(|node| node.task_ref.clone())
            .collect();
        for dep in &source.dependencies {
            let key = (dep.depends_on_run_id.clone(), dep.depends_on_ref.clone());
            if &dep.depends_on_run_id == source_run {
                graph
                    .active_task_blocks
                    .entry(key)
                    .or_default()
                    .insert(dep.task_ref.to_string());
                continue;
            }
            let checkout_raw = PathBuf::from(&dep.depends_on_workspace);
            let checkout = if checkout_raw.as_path() == Path::new(".") {
                checkout_raw
            } else {
                canonical_root(&checkout_raw).unwrap_or(checkout_raw)
            };
            if checkout.as_path() != Path::new(".")
                && checkout.as_path() != root
                && !discovered
                    .owned_worktrees
                    .iter()
                    .any(|owned| owned == &checkout)
            {
                continue;
            }
            let label = format!("{source_spec}:{}", dep.task_ref);
            let persisted = snapshots.get(&dep.depends_on_run_id).copied();
            let scoped = (checkout.as_path() != Path::new(".")).then_some(checkout.as_path());
            let selector = checkout_scoped_target_selector(
                persisted.map(|(_, target)| &target.run),
                dep.depends_on_run_id.as_str(),
                scoped,
            );
            let in_scope = |target: &RunGraphSnapshot| {
                scoped.is_none_or(|checkout| run_in_checkout(&target.run, checkout))
            };
            let target = persisted
                .filter(|(_, target)| in_scope(target))
                .or_else(|| {
                    snapshots.values().copied().find(|(spec_id, target)| {
                        (spec_id.as_str() == selector
                            || run_matches_selector(&target.run, selector))
                            && in_scope(target)
                    })
                });
            let edge_satisfied = is_dep_satisfied_against_statuses(
                &source.run.run_id,
                dep,
                |task_ref| completed_refs.contains(task_ref),
                &obs,
            );
            let (target_spec, completed) = if let Some((target_spec, target)) = target {
                if !dep.depends_on_ref.is_whole_spec() {
                    graph
                        .active_task_blocks
                        .entry((target.run.run_id.clone(), dep.depends_on_ref.clone()))
                        .or_default()
                        .insert(label.clone());
                }
                let completed = target.nodes.iter().any(|node| {
                    node.kind == NodeKind::Task
                        && node.status == NodeStatus::Completed
                        && node.task_ref.as_ref() == Some(&dep.depends_on_ref)
                });
                graph
                    .active_spec_blocks
                    .entry(target.run.run_id.clone())
                    .or_default()
                    .insert(label.clone());
                (target_spec.clone(), completed)
            } else if let Some((target_spec, _)) = discovered
                .specs_by_id
                .get_key_value(selector)
                .filter(|(_, info)| {
                    checkout.as_path() == Path::new(".") || info.checkout == checkout
                })
            {
                if !dep.depends_on_ref.is_whole_spec() {
                    graph
                        .task_blocks
                        .entry((target_spec.clone(), dep.depends_on_ref.to_string()))
                        .or_default()
                        .insert(label.clone());
                }
                graph
                    .spec_blocks
                    .entry(target_spec.clone())
                    .or_default()
                    .insert(label.clone());
                (target_spec.clone(), false)
            } else if let Some(checkout) =
                scoped.filter(|checkout| checkout_spec_id(discovered, checkout, selector).is_some())
            {
                let spec_id = checkout_spec_id(discovered, checkout, selector)
                    .expect("planned checkout spec");
                record_scoped_planned_blocks(
                    graph,
                    checkout,
                    &spec_id,
                    &dep.depends_on_ref,
                    &label,
                );
                (spec_id, false)
            } else {
                continue;
            };
            if !edge_satisfied && !completed && *source_spec != target_spec {
                graph
                    .forward_spec_edges
                    .entry(target_spec.clone())
                    .or_default()
                    .insert(source_spec.clone());
            }
        }
    }
    Ok(())
}

fn record_scoped_planned_blocks(
    graph: &mut DependencyGraphData,
    checkout: &Path,
    spec_id: &str,
    task_ref: &TaskRef,
    label: &str,
) {
    let key = (checkout.to_path_buf(), spec_id.to_owned());
    graph.scoped_planned.insert(key.clone());
    if !task_ref.is_whole_spec() {
        graph
            .scoped_task_blocks
            .entry((key.0.clone(), key.1.clone(), task_ref.to_string()))
            .or_default()
            .insert(label.to_owned());
    }
    graph
        .scoped_spec_blocks
        .entry(key)
        .or_default()
        .insert(label.to_owned());
}

fn record_reverse_block(
    parsed: &ParsedDepSyntax,
    source_spec_id: &str,
    source_task_ref: Option<&str>,
    spec_blocks: &mut BTreeMap<String, BTreeSet<String>>,
    task_blocks: &mut BTreeMap<(String, String), BTreeSet<String>>,
) {
    match parsed {
        ParsedDepSyntax::LocalTask(target_task) => {
            if let Some(t_ref) = source_task_ref {
                task_blocks
                    .entry((source_spec_id.to_owned(), target_task.clone()))
                    .or_default()
                    .insert(t_ref.to_owned());
            }
        }
        ParsedDepSyntax::LocalSpec { spec_id, task_ref } => {
            let label = match source_task_ref {
                Some(t_ref) => format!("{source_spec_id}:{t_ref}"),
                None => source_spec_id.to_owned(),
            };
            if let Some(target_task) = task_ref {
                task_blocks
                    .entry((spec_id.clone(), target_task.clone()))
                    .or_default()
                    .insert(label.clone());
            }
            spec_blocks
                .entry(spec_id.clone())
                .or_default()
                .insert(label);
        }
        ParsedDepSyntax::LinkedSpec { .. } => {}
    }
}

fn map_dep_resolution(resolution: DepTargetResolution) -> (&'static str, bool) {
    match resolution {
        DepTargetResolution::ActiveSpec {
            cancelled,
            task_exists,
        } => {
            if !task_exists {
                ("missing", false)
            } else if cancelled {
                ("cancelled", false)
            } else {
                ("planned", false)
            }
        }
        DepTargetResolution::ActiveRun {
            task_exists,
            task_completed,
            ..
        } => {
            if !task_exists {
                ("missing", false)
            } else if task_completed {
                ("satisfied", true)
            } else {
                ("active", false)
            }
        }
        DepTargetResolution::Tombstone {
            cleared,
            task_outcome,
        } => {
            if cleared {
                ("cleared", false)
            } else if task_outcome.is_ok() {
                ("satisfied", true)
            } else {
                ("missing", false)
            }
        }
        DepTargetResolution::ArchivedSpec {
            completed_doc,
            task_exists,
            task_completed,
            ..
        } => {
            if !task_exists {
                ("missing", false)
            } else if completed_doc && task_completed {
                ("archived", true)
            } else {
                ("cancelled", false)
            }
        }
        DepTargetResolution::NotFound => ("missing", false),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_dep_entry(
    raw: &str,
    parsed: &mut ParsedDepSyntax,
    source: &SpecInfo,
    root: &Path,
    active_annotation: bool,
    collections: &BTreeMap<String, SpecCollectionConfig>,
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    discovered: &DiscoveredWorkspace,
    linked_cache: &mut LinkedWsCache,
    warnings: &mut Vec<String>,
) -> Result<(RoadmapDepEntry, Option<RunId>), StateError> {
    let to_entry = |(status, satisfied): (&str, bool)| RoadmapDepEntry {
        raw: raw.to_owned(),
        status: status.to_owned(),
        satisfied,
    };
    let local_checkout = if active_annotation {
        &config.worktree
    } else {
        root
    };
    let mut qualified = None;
    let (spec_id, task_ref) = match parsed {
        ParsedDepSyntax::LocalTask(t_ref) => {
            let completed = source.doc.tasks.iter().any(|task| {
                task.task_ref.as_str() == t_ref && task.status == crate::spec::SpecStatus::Completed
            });
            return Ok((
                to_entry((if completed { "satisfied" } else { "planned" }, completed)),
                None,
            ));
        }
        ParsedDepSyntax::LocalSpec { spec_id, task_ref } => (spec_id, task_ref),
        ParsedDepSyntax::LinkedSpec {
            ws_alias,
            spec_id,
            task_ref,
        } => {
            let key = (
                source.checkout.clone(),
                source.spec_id.clone(),
                config.worktree.clone(),
                ws_alias.clone(),
            );
            if !linked_cache.aliases.contains_key(&key) {
                let spec_repos = resolve_spec_repositories(&source.checkout, &source.doc);
                let checkout = if config.linked_workspaces.contains_key(ws_alias)
                    || spec_repos.contains_key(ws_alias)
                {
                    Some(resolve_linked_workspace_root(
                        ws_alias,
                        raw,
                        &config.linked_workspaces,
                        &spec_repos,
                    )?)
                } else {
                    None
                };
                linked_cache.aliases.insert(key.clone(), checkout);
            }
            let Some(checkout) = linked_cache.aliases[&key].clone() else {
                return Ok((to_entry(("missing", false)), None));
            };
            if checkout != root && !discovered.owned_worktrees.contains(&checkout) {
                let outcome = resolve_linked_dep(
                    raw,
                    ws_alias,
                    spec_id,
                    task_ref.as_deref(),
                    checkout,
                    linked_cache,
                    warnings,
                )?;
                return Ok((to_entry(outcome), None));
            }
            qualified = Some((checkout, ws_alias.as_str()));
            (spec_id, task_ref)
        }
    };
    if let Some((checkout, alias)) = &qualified {
        if !discovered.checkout_specs.contains_key(checkout) {
            linked_cache.workspace(
                checkout,
                alias,
                Some((local_checkout, collections)),
                warnings,
            )?;
        }
    }
    let (checkout, target_collections, active_docs): (
        &Path,
        &BTreeMap<String, SpecCollectionConfig>,
        &[SpecDocument],
    ) = if let Some((checkout, _)) = &qualified {
        let scoped = match discovered.checkout_specs.get(checkout) {
            Some(scoped) => scoped,
            None => &linked_cache.workspaces[checkout].0,
        };
        (checkout.as_path(), &scoped.collections, &scoped.active_docs)
    } else {
        (
            local_checkout,
            collections,
            discovered
                .checkout_specs
                .get(local_checkout)
                .map_or(&[][..], |scoped| scoped.active_docs.as_slice()),
        )
    };
    let empty_archives = BTreeMap::new();
    let archived_specs = discovered
        .checkout_archives
        .get(checkout)
        .unwrap_or(&empty_archives);
    let preloaded = PreloadedDepContext {
        workspace: root,
        runs: &linked_cache.runs,
        task_statuses: &linked_cache.task_statuses,
        tombstones: &linked_cache.tombstones,
        tombstone_archives: &linked_cache.tombstone_archives,
        archived_specs,
    };
    let mut resolved = resolve_dep_target(
        checkout,
        target_collections,
        Some(reader),
        Some(active_docs),
        spec_id,
        task_ref.as_deref(),
        raw,
        qualified.as_ref().map(|(checkout, _)| checkout.as_path()),
        Some(&preloaded),
    )?;
    if qualified.is_none() && matches!(resolved, DepTargetResolution::NotFound) {
        if let Some(sibling) = discovered
            .sibling_candidates
            .get(spec_id)
            .and_then(|paths| paths.iter().find(|path| path.as_path() != checkout))
        {
            let scoped = &discovered.checkout_specs[sibling];
            let sibling_context = PreloadedDepContext {
                archived_specs: discovered
                    .checkout_archives
                    .get(sibling)
                    .unwrap_or(&empty_archives),
                ..preloaded
            };
            resolved = resolve_dep_target(
                sibling,
                &scoped.collections,
                None,
                Some(&scoped.active_docs),
                spec_id,
                task_ref.as_deref(),
                raw,
                None,
                Some(&sibling_context),
            )?;
        }
    }
    let mut active_target = None;
    let target_in_checkout = match &resolved {
        DepTargetResolution::ActiveRun {
            run_id,
            spec_id: resolved_spec,
            worktree: _,
            task_exists,
            ..
        } => {
            let matches_checkout = qualified.is_none() || *task_exists;
            if matches_checkout {
                active_target = Some(run_id.clone());
            }
            if let Some(resolved_spec) = resolved_spec {
                spec_id.clone_from(resolved_spec);
            }
            matches_checkout
        }
        DepTargetResolution::ActiveSpec { task_exists, .. } => *task_exists,
        _ => false,
    };
    let local_vertex = qualified.is_some()
        && target_in_checkout
        && discovered
            .specs_by_id
            .get(spec_id)
            .is_some_and(|info| info.checkout == checkout);
    if local_vertex {
        *parsed = ParsedDepSyntax::LocalSpec {
            spec_id: std::mem::take(spec_id),
            task_ref: task_ref.take(),
        };
    }
    Ok((to_entry(map_dep_resolution(resolved)), active_target))
}

fn resolve_linked_dep(
    raw: &str,
    ws_alias: &str,
    spec_id: &str,
    task_ref: Option<&str>,
    ws_path: PathBuf,
    linked_cache: &mut LinkedWsCache,
    warnings: &mut Vec<String>,
) -> Result<(&'static str, bool), StateError> {
    let cache_key = (
        ws_path.clone(),
        spec_id.to_owned(),
        task_ref.map(str::to_owned),
    );
    if let Some(&cached) = linked_cache.outcomes.get(&cache_key) {
        return Ok(cached);
    }

    if !linked_cache.linked_deps.contains_key(&ws_path) {
        let (scoped, ext_reader) = linked_cache.workspace(&ws_path, ws_alias, None, warnings)?;
        let reader = ext_reader.as_ref().expect("linked workspace has a reader");
        let (data, archive_warnings) = load_dep_snapshot(&ws_path, &scoped.collections, reader)?;
        warnings.extend(archive_warnings);
        linked_cache.linked_deps.insert(ws_path.clone(), data);
    }
    let (scoped, ext_reader) = &linked_cache.workspaces[&ws_path];
    let preloaded = linked_cache.linked_deps[&ws_path].context();
    let outcome = map_dep_resolution(resolve_dep_target(
        &ws_path,
        &scoped.collections,
        ext_reader.as_ref().map(|reader| reader as &dyn StateReader),
        Some(&scoped.active_docs),
        spec_id,
        task_ref,
        raw,
        None,
        Some(&preloaded),
    )?);
    linked_cache.outcomes.insert(cache_key, outcome);
    Ok(outcome)
}

impl LinkedWsCache {
    fn workspace(
        &mut self,
        checkout: &Path,
        alias: &str,
        local: Option<(&Path, &BTreeMap<String, SpecCollectionConfig>)>,
        warnings: &mut Vec<String>,
    ) -> Result<&(CheckoutSpecs, Option<SqliteStateReader>), StateError> {
        use std::collections::btree_map::Entry;
        match self.workspaces.entry(checkout.to_path_buf()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let (collections, reader) = match local {
                    Some((root, collections)) if root == checkout => (collections.clone(), None),
                    Some(_) => (load_ws_adapter(checkout)?.specs, None),
                    None => {
                        let (db_path, specs) = resolve_ws_context_canon(checkout, None)?;
                        (specs, Some(SqliteStateReader::new(db_path)))
                    }
                };
                let scanned =
                    scan_checkout_specs(checkout, alias, &collections, warnings, &mut BTreeMap::new())?;
                Ok(entry.insert((checkout_specs_from(&collections, &scanned), reader)))
            }
        }
    }
}
