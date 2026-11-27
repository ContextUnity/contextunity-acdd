//! Cross-spec, active-run, and linked-workspace dependency graph resolution for roadmap synthesis.

use super::discovery::{checkout_spec_id, checkout_specs_from, scan_checkout_specs};
use super::types::{
    ActiveRunMap, CheckoutSpecs, DependencyGraphData, DiscoveredWorkspace, LinkedWsCache,
    RoadmapConfig, SpecInfo,
};
use crate::deps::{parse_dep_syntax, resolve_dep_target, DepTargetResolution, ParsedDepSyntax};
use crate::discovery::{
    collect_archived_specs, effective_spec_collections, load_ws_adapter,
    resolve_linked_workspace_root, resolve_spec_repositories,
};
use crate::overlay::{RuntimeActiveRun, SpecRuntimeOverlay};
use crate::spec::{SpecDocument, SpecStatus};
use crate::types::RoadmapDepEntry;
use acdd_core::config::SpecConfig;
use acdd_core::errors::AcddError;
use acdd_core::fs::canonical_root;
use acdd_core::ids::{RunId, TaskRef};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

struct DependencyGraphBuilder<'a> {
    discovered: &'a DiscoveredWorkspace,
    root: &'a Path,
    overlay: &'a dyn SpecRuntimeOverlay,
    linked_cache: LinkedWsCache,
    graph: DependencyGraphData,
    warnings: Vec<String>,
}

impl DependencyGraphBuilder<'_> {
    fn process_dep_edge(
        &mut self,
        raw: &str,
        source: &SpecInfo,
        source_task_ref: Option<&str>,
        local_refs: &BTreeSet<&str>,
        annotation_run: Option<&RunId>,
        config: &RoadmapConfig<'_>,
    ) -> Result<(Option<ParsedDepSyntax>, RoadmapDepEntry), AcddError> {
        let discovered = self.discovered;
        let source_spec_id = &source.spec_id;
        let record_in_graph = !source.is_archived && annotation_run.is_none();
        let mut parsed = match parse_dep_syntax(raw, local_refs) {
            Ok(p) => p,
            Err(err) => {
                let ctx = match source_task_ref {
                    Some(t) => format!("spec '{source_spec_id}' task '{t}'"),
                    None => format!("spec '{source_spec_id}'"),
                };
                self.warnings
                    .push(format!("{ctx} malformed depends_on '{raw}': {err}"));
                if record_in_graph && source_task_ref.is_none() {
                    self.graph
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

        let resolution =
            self.resolve_dep_entry(raw, &mut parsed, source, annotation_run.is_some(), config);
        let (entry, active_target) = match (resolution, annotation_run) {
            (Ok(resolved), _) => resolved,
            (Err(err), Some(run_id)) => {
                self.warnings.push(format!(
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
                &mut self.graph.spec_blocks,
                &mut self.graph.task_blocks,
            );
            if let Some(target_run) = &active_target {
                let label = source_task_ref.map_or_else(
                    || source_spec_id.to_owned(),
                    |source_task| format!("{source_spec_id}:{source_task}"),
                );
                self.graph
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
                self.graph
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
                        self.graph
                            .forward_spec_edges
                            .entry(target_spec.clone())
                            .or_default()
                            .insert(source_spec_id.to_owned());
                        if source_task_ref.is_none() {
                            self.graph
                                .unsatisfied_blockers
                                .entry(source_spec_id.to_owned())
                                .or_default()
                                .insert(target_spec.clone());
                        }
                    }
                    _ => {
                        if source_task_ref.is_none() {
                            self.graph
                                .has_unsatisfied_external_blocker
                                .insert(source_spec_id.to_owned(), true);
                        }
                    }
                }
            }
        }

        Ok((Some(parsed), entry))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_dependency_graph(
    discovered: &DiscoveredWorkspace,
    root: &Path,
    current_root: &Path,
    current_only: bool,
    config: &RoadmapConfig<'_>,
    overlay: &dyn SpecRuntimeOverlay,
    active_runs_by_spec: &ActiveRunMap,
    include_archived: bool,
    warnings: &mut Vec<String>,
) -> Result<DependencyGraphData, AcddError> {
    let graph = DependencyGraphData {
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
    };
    let mut builder = DependencyGraphBuilder {
        discovered,
        root,
        overlay,
        linked_cache: LinkedWsCache::default(),
        graph,
        warnings: Vec::new(),
    };

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
        runs.iter().filter_map(move |run| {
            let info = discovered
                .specs_by_run
                .get(&run.run_id)
                .or_else(|| discovered.specs_by_id.get(id))?;
            Some((info, Some(run)))
        })
    });
    for (info, run) in planned.chain(active) {
        let checkout_wt = run
            .filter(|_| !info.doc.depends_on.is_empty())
            .and_then(|r| r.worktree.as_deref())
            .and_then(|p| canonical_root(Path::new(p)).ok())
            .filter(|wt| {
                canonical_root(config.worktree).ok().as_ref() != Some(wt)
                    && discovered.owned_worktrees.contains(wt)
            });
        let checkout_override = checkout_wt
            .as_deref()
            .map(|wt| -> Result<_, AcddError> {
                let adapter = load_ws_adapter(wt)?;
                let linked: BTreeMap<String, PathBuf> = adapter
                    .linked_workspaces
                    .into_iter()
                    .map(|l| (l.name, l.path))
                    .collect();
                Ok((adapter.specs, linked))
            })
            .transpose()?;
        let effective_cols = checkout_override
            .as_ref()
            .map_or(config.spec_collections, |(cols, _)| cols);
        let effective_linked = checkout_override
            .as_ref()
            .map_or(config.linked_workspaces, |(_, linked)| linked);
        let spec_config = RoadmapConfig {
            workspace_root: config.workspace_root,
            worktree: checkout_wt.as_deref().unwrap_or(config.worktree),
            spec_collections: effective_cols,
            linked_workspaces: effective_linked,
        };
        let empty_refs = BTreeSet::new();
        let mut deps_out = Vec::with_capacity(info.doc.depends_on.len());
        for raw in &info.doc.depends_on {
            let (_, entry) = builder.process_dep_edge(
                raw,
                info,
                None,
                &empty_refs,
                run.map(|run| &run.run_id),
                &spec_config,
            )?;
            deps_out.push(entry);
        }
        if let Some(run) = run {
            builder
                .graph
                .active_spec_deps_resolved
                .insert(run.run_id.clone(), deps_out);
            continue;
        }
        builder
            .graph
            .spec_deps_resolved
            .insert(info.spec_id.clone(), deps_out);

        let local_refs: BTreeSet<&str> =
            info.doc.tasks.iter().map(|t| t.task_ref.as_str()).collect();
        for task in &info.doc.tasks {
            let t_ref = task.task_ref.as_str();
            let mut t_entries = Vec::with_capacity(task.depends_on.len());
            for raw in &task.depends_on {
                let (parsed_opt, entry) =
                    builder.process_dep_edge(raw, info, Some(t_ref), &local_refs, None, config)?;
                t_entries.push((parsed_opt, entry));
            }
            builder
                .graph
                .task_deps_resolved
                .insert((info.spec_id.clone(), t_ref.to_owned()), t_entries);
        }
    }

    warnings.extend(builder.warnings);
    let mut graph = builder.graph;
    record_active_blocks(
        discovered,
        root,
        current_root,
        current_only,
        overlay,
        active_runs_by_spec,
        &mut graph,
    )?;
    Ok(graph)
}

fn record_active_blocks(
    discovered: &DiscoveredWorkspace,
    root: &Path,
    current_root: &Path,
    current_only: bool,
    overlay: &dyn SpecRuntimeOverlay,
    runs: &ActiveRunMap,
    graph: &mut DependencyGraphData,
) -> Result<(), AcddError> {
    let snapshots: BTreeMap<_, _> = runs
        .iter()
        .flat_map(|(spec_id, runs)| {
            runs.iter()
                .map(move |run| (&run.run_id, (spec_id, run)))
        })
        .collect();
    for (&source_run, &(source_spec, source)) in &snapshots {
        for (target_task, source_task) in &source.local_task_blocks {
            graph
                .active_task_blocks
                .entry((source_run.clone(), target_task.clone()))
                .or_default()
                .insert(source_task.to_string());
        }
        let run_wt = source
            .worktree
            .as_deref()
            .map(|p| canonical_root(Path::new(p)).unwrap_or_else(|_| PathBuf::from(p)));
        let in_scope = !current_only
            || discovered.specs_by_run.contains_key(source_run)
            || run_wt.as_deref().unwrap_or(root) == current_root;
        if !in_scope || !source.has_external_deps {
            continue;
        }
        let ext_deps = overlay.active_run_external_deps(source_run, &discovered.owned_worktrees)?;
        for dep in &ext_deps {
            let key = (dep.target_run_id.clone(), dep.target_task_ref.clone());
            if &dep.target_run_id == source_run {
                graph
                    .active_task_blocks
                    .entry(key)
                    .or_default()
                    .insert(dep.source_task_ref.to_string());
                continue;
            }
            let checkout_raw = PathBuf::from(&dep.target_workspace);
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
            let label = format!("{source_spec}:{}", dep.source_task_ref);
            let scoped = (checkout.as_path() != Path::new(".")).then_some(checkout.as_path());
            let in_scope = |target: &RuntimeActiveRun| {
                scoped.is_none_or(|checkout| {
                    target
                        .worktree
                        .as_deref()
                        .and_then(|path| canonical_root(Path::new(path)).ok())
                        .is_some_and(|wt| wt == checkout)
                })
            };
            let selector = dep.target_selector.as_str();
            let target = snapshots
                .get(&dep.target_run_id)
                .copied()
                .filter(|(_, target)| in_scope(target))
                .or_else(|| {
                    snapshots.values().copied().find(|(spec_id, target)| {
                        (spec_id.as_str() == selector
                            || target.run_id.as_str() == selector
                            || target.spec_id.as_deref() == Some(selector))
                            && in_scope(target)
                    })
                });
            let (target_spec, completed) = if let Some((target_spec, target)) = target {
                if !dep.target_task_ref.is_whole_spec() {
                    graph
                        .active_task_blocks
                        .entry((target.run_id.clone(), dep.target_task_ref.clone()))
                        .or_default()
                        .insert(label.clone());
                }
                let completed = target.completed_task_refs.contains(&dep.target_task_ref);
                graph
                    .active_spec_blocks
                    .entry(target.run_id.clone())
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
                if !dep.target_task_ref.is_whole_spec() {
                    graph
                        .task_blocks
                        .entry((target_spec.clone(), dep.target_task_ref.to_string()))
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
                    &dep.target_task_ref,
                    &label,
                );
                (spec_id, false)
            } else {
                continue;
            };
            if !dep.satisfied && !completed && *source_spec != target_spec {
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

impl DependencyGraphBuilder<'_> {
    fn resolve_dep_entry(
        &mut self,
        raw: &str,
        parsed: &mut ParsedDepSyntax,
        source: &SpecInfo,
        active_annotation: bool,
        config: &RoadmapConfig<'_>,
    ) -> Result<(RoadmapDepEntry, Option<RunId>), AcddError> {
        let collections = effective_spec_collections(config.spec_collections);
        let to_entry = |(status, satisfied): (&str, bool)| RoadmapDepEntry {
            raw: raw.to_owned(),
            status: status.to_owned(),
            satisfied,
        };
        let local_checkout = if active_annotation {
            config.worktree
        } else {
            self.root
        };
        let mut qualified = None;
        let (spec_id, task_ref) = match parsed {
            ParsedDepSyntax::LocalTask(t_ref) => {
                let completed = source
                    .doc
                    .tasks
                    .iter()
                    .any(|task| task.task_ref.as_str() == t_ref && task.status == SpecStatus::Completed);
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
                    config.worktree.to_path_buf(),
                    ws_alias.clone(),
                );
                if !self.linked_cache.aliases.contains_key(&key) {
                    let spec_repos = resolve_spec_repositories(&source.checkout, &source.doc);
                    let checkout = if config.linked_workspaces.contains_key(ws_alias)
                        || spec_repos.contains_key(ws_alias)
                    {
                        Some(resolve_linked_workspace_root(
                            ws_alias,
                            raw,
                            config.linked_workspaces,
                            &spec_repos,
                        )?)
                    } else {
                        None
                    };
                    self.linked_cache.aliases.insert(key.clone(), checkout);
                }
                let Some(checkout) = self.linked_cache.aliases[&key].clone() else {
                    return Ok((to_entry(("missing", false)), None));
                };
                if checkout != self.root && !self.discovered.owned_worktrees.contains(&checkout) {
                    let outcome = resolve_linked_dep(
                        raw,
                        ws_alias,
                        spec_id,
                        task_ref.as_deref(),
                        checkout,
                        self.overlay,
                        &mut self.linked_cache,
                        &mut self.warnings,
                    )?;
                    return Ok((to_entry(outcome), None));
                }
                qualified = Some((checkout, ws_alias.as_str()));
                (spec_id, task_ref)
            }
        };
        if let Some((checkout, alias)) = &qualified {
            if !self.discovered.checkout_specs.contains_key(checkout) {
                self.linked_cache.workspace(
                    checkout,
                    alias,
                    Some((local_checkout, &collections)),
                    &mut self.warnings,
                )?;
            }
        }
        let (checkout, target_collections, active_docs): (
            &Path,
            &BTreeMap<String, SpecConfig>,
            &[SpecDocument],
        ) = if let Some((checkout, _)) = &qualified {
            let scoped = match self.discovered.checkout_specs.get(checkout) {
                Some(scoped) => scoped,
                None => &self.linked_cache.workspaces[checkout],
            };
            (checkout.as_path(), &scoped.collections, &scoped.active_docs)
        } else {
            (
                local_checkout,
                &collections,
                self.discovered
                    .checkout_specs
                    .get(local_checkout)
                    .map_or(&[][..], |scoped| scoped.active_docs.as_slice()),
            )
        };
        let empty_archives = BTreeMap::new();
        let archived_specs = self
            .discovered
            .checkout_archives
            .get(checkout)
            .unwrap_or(&empty_archives);
        let mut resolved = resolve_dep_target(
            checkout,
            target_collections,
            self.overlay,
            Some(active_docs),
            Some(archived_specs),
            spec_id,
            task_ref.as_deref(),
            raw,
            qualified.as_ref().map(|(checkout, _)| checkout.as_path()),
        )?;
        if qualified.is_none() && matches!(resolved, DepTargetResolution::NotFound) {
            if let Some(sibling) = self
                .discovered
                .sibling_candidates
                .get(spec_id)
                .and_then(|paths| paths.iter().find(|path| path.as_path() != checkout))
            {
                let scoped = &self.discovered.checkout_specs[sibling];
                let sibling_archives = self
                    .discovered
                    .checkout_archives
                    .get(sibling)
                    .unwrap_or(&empty_archives);
                resolved = resolve_dep_target(
                    sibling,
                    &scoped.collections,
                    self.overlay,
                    Some(&scoped.active_docs),
                    Some(sibling_archives),
                    spec_id,
                    task_ref.as_deref(),
                    raw,
                    None,
                )?;
            }
        }
        let mut active_target = None;
        let target_in_checkout = match &resolved {
            DepTargetResolution::ActiveRun {
                run_id,
                spec_id: resolved_spec,
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
            && self
                .discovered
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
}

#[allow(clippy::too_many_arguments)]
fn resolve_linked_dep(
    raw: &str,
    ws_alias: &str,
    spec_id: &str,
    task_ref: Option<&str>,
    ws_path: PathBuf,
    overlay: &dyn SpecRuntimeOverlay,
    linked_cache: &mut LinkedWsCache,
    warnings: &mut Vec<String>,
) -> Result<(&'static str, bool), AcddError> {
    let cache_key = (
        ws_path.clone(),
        spec_id.to_owned(),
        task_ref.map(str::to_owned),
    );
    if let Some(&cached) = linked_cache.outcomes.get(&cache_key) {
        return Ok(cached);
    }

    if !linked_cache.linked_archives.contains_key(&ws_path) {
        let scoped = linked_cache.workspace(&ws_path, ws_alias, None, warnings)?;
        let (archives, archive_warnings) =
            collect_archived_specs(&ws_path, &scoped.collections, false, None)?;
        warnings.extend(archive_warnings);
        let archived_map = archives
            .into_iter()
            .map(|(_, path, doc)| (doc.id.clone(), (path, doc)))
            .collect();
        linked_cache
            .linked_archives
            .insert(ws_path.clone(), archived_map);
    }
    let scoped = &linked_cache.workspaces[&ws_path];
    let archived_specs = &linked_cache.linked_archives[&ws_path];
    let outcome = map_dep_resolution(resolve_dep_target(
        &ws_path,
        &scoped.collections,
        overlay,
        Some(&scoped.active_docs),
        Some(archived_specs),
        spec_id,
        task_ref,
        raw,
        None,
    )?);
    linked_cache.outcomes.insert(cache_key, outcome);
    Ok(outcome)
}

impl LinkedWsCache {
    fn workspace(
        &mut self,
        checkout: &Path,
        alias: &str,
        local: Option<(&Path, &BTreeMap<String, SpecConfig>)>,
        warnings: &mut Vec<String>,
    ) -> Result<&CheckoutSpecs, AcddError> {
        use std::collections::btree_map::Entry;
        match self.workspaces.entry(checkout.to_path_buf()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let collections = match local {
                    Some((root, collections)) if root == checkout => collections.clone(),
                    Some(_) | None => load_ws_adapter(checkout)?.specs,
                };
                let scanned = scan_checkout_specs(
                    checkout,
                    alias,
                    &collections,
                    warnings,
                    &mut BTreeMap::new(),
                )?;
                Ok(entry.insert(checkout_specs_from(&collections, &scanned)))
            }
        }
    }
}
