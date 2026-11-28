//! Runtime `SpecRuntimeOverlay` implementations bridging `StateReader` / `StateBackend` into `acdd-spec`.

use crate::context::slices::read_spec_readonly;
use crate::machine::graph::container_scope;
use crate::machine::schedule::{
    claimed_unfinished_scopes, is_dep_satisfied_against_statuses, is_task_schedule_eligible,
    task_dependencies_satisfied,
};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::{StateBackend, StateReader};
use crate::types::{
    GateNodeRecord, NodeKind, NodeStatus, RunGraphSnapshot, RunRecord, RunStatus,
    ValidatedObservations,
};
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::{
    checkout_scoped_target_selector, collect_external_satisfied_deps_cached, load_dep_snapshot,
    resolve_active_run_dep, resolve_tombstone_dep, resolve_ws_context_canon, DepSnapshot,
    ExternalDepCache,
};
use crate::usecases::lifecycle::list_all_runs;
use acdd_core::config::{resolve_effective_state_db_path, Adapter, SpecConfig};
use acdd_core::errors::AcddError;
use acdd_core::fs::canonical_root;
use acdd_core::ids::{RunId, TaskRef};
use acdd_spec::overlay::{
    RuntimeActiveExternalDep, RuntimeActiveRun, RuntimeActiveTask, RuntimeDepResolution,
    RuntimeTaskStatus, SpecRuntimeOverlay,
};
use acdd_spec::{RoadmapActiveGateEntry, RoadmapDepEntry, SpecStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct CachedWsDepState {
    reader: Option<SqliteStateReader>,
    snapshot: DepSnapshot,
}

#[derive(Default)]
struct RoadmapOverlayCache {
    loaded: bool,
    runs: BTreeMap<RunId, (RunRecord, Option<RunGraphSnapshot>)>,
    external_cache: ExternalDepCache,
}

/// Live `SpecRuntimeOverlay` backed by `StateReader` (and cached per-workspace `DepSnapshot`s).
pub(crate) struct StateRuntimeOverlay<'a> {
    local_primary: PathBuf,
    local_checkout: PathBuf,
    local_db: PathBuf,
    local_reader: Option<&'a dyn StateReader>,
    roadmap_config: Option<&'a StateEngineConfig>,
    cache: Mutex<BTreeMap<PathBuf, CachedWsDepState>>,
    roadmap_cache: Mutex<RoadmapOverlayCache>,
}

impl<'a> StateRuntimeOverlay<'a> {
    pub(crate) fn for_validation(
        root: &Path,
        adapter: &Adapter,
        local_reader: Option<&'a dyn StateReader>,
        state_db_override: Option<&Path>,
    ) -> Self {
        let local_checkout = canonical_root(root).unwrap_or_else(|_| root.to_path_buf());
        let local_primary = acdd_core::git::primary_worktree_root(&local_checkout)
            .unwrap_or_else(|| local_checkout.clone());
        let local_db = resolve_effective_state_db_path(&local_primary, state_db_override, adapter);
        Self {
            local_primary,
            local_checkout,
            local_db,
            local_reader,
            roadmap_config: None,
            cache: Mutex::new(BTreeMap::new()),
            roadmap_cache: Mutex::new(RoadmapOverlayCache::default()),
        }
    }

    pub(crate) fn for_roadmap(
        config: &'a StateEngineConfig,
        reader: &'a dyn StateReader,
    ) -> Self {
        let local_checkout =
            canonical_root(&config.worktree).unwrap_or_else(|_| config.worktree.clone());
        let local_primary = acdd_core::git::primary_worktree_root(&local_checkout)
            .unwrap_or_else(|| local_checkout.clone());
        let local_db = config.resolved_state_db_path().0;
        Self {
            local_primary,
            local_checkout,
            local_db,
            local_reader: Some(reader),
            roadmap_config: Some(config),
            cache: Mutex::new(BTreeMap::new()),
            roadmap_cache: Mutex::new(RoadmapOverlayCache::default()),
        }
    }

    fn ensure_roadmap_runs_loaded(
        &self,
        cache: &mut RoadmapOverlayCache,
    ) -> Result<(), AcddError> {
        if cache.loaded {
            return Ok(());
        }
        let Some(reader) = self.local_reader else {
            cache.loaded = true;
            return Ok(());
        };
        let active_runs = list_all_runs(reader, Some(RunStatus::Active))?;
        for run in active_runs {
            let graph = reader.load_run_graph(&run.run_id)?;
            cache.runs.insert(run.run_id.clone(), (run, graph));
        }
        cache.loaded = true;
        Ok(())
    }

    fn with_ws_snapshot<T>(
        &self,
        ws_path: &Path,
        collections: &BTreeMap<String, SpecConfig>,
        f: impl FnOnce(&dyn StateReader, &DepSnapshot) -> Result<T, AcddError>,
    ) -> Result<T, AcddError> {
        let ws_canon = canonical_root(ws_path).unwrap_or_else(|_| ws_path.to_path_buf());
        let mut guard = self.cache.lock().expect("runtime overlay cache lock");
        if !guard.contains_key(&ws_canon) {
            let db_path = if ws_canon == self.local_checkout || ws_canon == self.local_primary {
                self.local_db.clone()
            } else {
                resolve_ws_context_canon(&ws_canon, Some((&self.local_primary, &self.local_db)))?.0
            };
            let use_local = self.local_reader.is_some() && db_path == self.local_db;
            let owned_reader = (!use_local).then(|| SqliteStateReader::new(db_path));
            let reader = if use_local {
                self.local_reader.expect("checked is_some")
            } else {
                owned_reader.as_ref().expect("owned reader constructed")
            };
            let (snapshot, _) = load_dep_snapshot(&ws_canon, collections, reader)?;
            guard.insert(
                ws_canon.clone(),
                CachedWsDepState {
                    reader: owned_reader,
                    snapshot,
                },
            );
        }
        let cached = &guard[&ws_canon];
        let reader = cached
            .reader
            .as_ref()
            .map(|r| r as &dyn StateReader)
            .or(self.local_reader)
            .expect("reader available");
        f(reader, &cached.snapshot)
    }
}

impl SpecRuntimeOverlay for StateRuntimeOverlay<'_> {
    fn active_task_statuses_for_spec(
        &self,
        root: &Path,
        rel_path: &str,
        spec_id: &str,
    ) -> BTreeMap<String, RuntimeTaskStatus> {
        let mut reconciled_statuses = BTreeMap::new();
        let owned_reader;
        let reader: &dyn StateReader = if let Some(r) = self.local_reader {
            r
        } else {
            let root_canon = canonical_root(root).unwrap_or_else(|_| root.to_path_buf());
            let db_path = if root_canon == self.local_checkout {
                self.local_db.clone()
            } else {
                let primary = acdd_core::git::primary_worktree_root(&root_canon)
                    .unwrap_or(root_canon);
                primary.join(".acdd/state.sqlite")
            };
            if !db_path.is_file() {
                return reconciled_statuses;
            }
            owned_reader = SqliteStateReader::new(db_path);
            &owned_reader
        };
        let Ok(runs) = list_all_runs(reader, Some(RunStatus::Active)) else {
            return reconciled_statuses;
        };
        let Some(run) = runs.into_iter().find(|r| {
            r.spec_id.as_deref() == Some(spec_id) || r.spec_path.as_deref() == Some(rel_path)
        }) else {
            return reconciled_statuses;
        };
        let Ok(Some(snap)) = reader.load_run_graph(&run.run_id) else {
            return reconciled_statuses;
        };
        for node in snap.nodes {
            if node.kind == NodeKind::Task {
                if let Some(tref) = node.task_ref {
                    let mapped = match node.status {
                        NodeStatus::Ready => RuntimeTaskStatus::Ready,
                        NodeStatus::InProgress => RuntimeTaskStatus::InProgress,
                        NodeStatus::Completed => RuntimeTaskStatus::Completed,
                        _ => RuntimeTaskStatus::Other,
                    };
                    reconciled_statuses.insert(tref.as_str().to_owned(), mapped);
                }
            }
        }
        reconciled_statuses
    }

    fn resolve_active_run_dep(
        &self,
        ws_path: &Path,
        collections: &BTreeMap<String, SpecConfig>,
        run_sel: &str,
        task_sel: Option<&str>,
        run_checkout: Option<&Path>,
    ) -> Result<Option<RuntimeDepResolution>, AcddError> {
        self.with_ws_snapshot(ws_path, collections, |reader, snapshot| {
            let preloaded = snapshot.context();
            resolve_active_run_dep(reader, run_sel, task_sel, run_checkout, Some(&preloaded))
        })
    }

    fn resolve_tombstone_dep(
        &self,
        ws_path: &Path,
        collections: &BTreeMap<String, SpecConfig>,
        run_sel: &str,
        task_sel: Option<&str>,
        dep_label: &str,
    ) -> Result<Option<RuntimeDepResolution>, AcddError> {
        self.with_ws_snapshot(ws_path, collections, |reader, snapshot| {
            let preloaded = snapshot.context();
            resolve_tombstone_dep(
                ws_path,
                collections,
                reader,
                run_sel,
                task_sel,
                dep_label,
                Some(&preloaded),
            )
        })
    }

    fn load_roadmap_active_runs(&self) -> Result<BTreeMap<String, Vec<RuntimeActiveRun>>, AcddError> {
        if self.roadmap_config.is_none() || self.local_reader.is_none() {
            return Ok(BTreeMap::new());
        }
        let mut guard = self.roadmap_cache.lock().expect("roadmap cache lock");
        self.ensure_roadmap_runs_loaded(&mut guard)?;

        let mut out: BTreeMap<String, Vec<RuntimeActiveRun>> = BTreeMap::new();
        for (run, graph_opt) in guard.runs.values() {
            let Some(ref spec_id) = run.spec_id else {
                continue;
            };
            let (
                total_tasks,
                completed_tasks,
                active_gates,
                completed_task_refs,
                local_task_blocks,
                has_external_deps,
            ) = match graph_opt {
                Some(ref graph) => {
                    let task_nodes: Vec<&GateNodeRecord> = graph
                        .nodes
                        .iter()
                        .filter(|n| n.kind == NodeKind::Task && n.task_ref.is_some())
                        .collect();
                    let completed_refs: BTreeSet<TaskRef> = task_nodes
                        .iter()
                        .filter(|n| n.status == NodeStatus::Completed)
                        .filter_map(|n| n.task_ref.clone())
                        .collect();
                    let active_gates = graph
                        .nodes
                        .iter()
                        .filter(|n| {
                            matches!(n.kind, NodeKind::RootGate | NodeKind::TaskGate)
                                && n.status == NodeStatus::InProgress
                        })
                        .map(to_active_gate_entry)
                        .collect();
                    let local_blocks: Vec<(TaskRef, TaskRef)> = graph
                        .dependencies
                        .iter()
                        .filter(|d| d.depends_on_run_id == graph.run.run_id)
                        .map(|d| (d.depends_on_ref.clone(), d.task_ref.clone()))
                        .collect();
                    let has_ext = graph
                        .dependencies
                        .iter()
                        .any(|d| d.depends_on_run_id != graph.run.run_id);
                    (
                        task_nodes.len(),
                        completed_refs.len(),
                        active_gates,
                        completed_refs,
                        local_blocks,
                        has_ext,
                    )
                }
                None => (0, 0, Vec::new(), BTreeSet::new(), Vec::new(), false),
            };
            out.entry(spec_id.clone())
                .or_default()
                .push(RuntimeActiveRun {
                    run_id: run.run_id.clone(),
                    spec_id: run.spec_id.clone(),
                    spec_path: run.spec_path.clone(),
                    worktree: run.worktree.clone(),
                    branch: run.branch.clone(),
                    total_tasks,
                    completed_tasks,
                    active_gates,
                    completed_task_refs,
                    local_task_blocks,
                    has_external_deps,
                });
        }
        Ok(out)
    }

    fn active_run_external_deps(
        &self,
        run_id: &RunId,
        owned_worktrees: &[PathBuf],
    ) -> Result<Vec<RuntimeActiveExternalDep>, AcddError> {
        let Some(config) = self.roadmap_config else {
            return Ok(Vec::new());
        };
        let mut guard = self.roadmap_cache.lock().expect("roadmap cache lock");
        self.ensure_roadmap_runs_loaded(&mut guard)?;
        let Some(graph) = guard.runs.get(run_id).and_then(|(_, g)| g.clone()) else {
            return Ok(Vec::new());
        };
        let ext_deps = collect_external_satisfied_deps_cached(
            config,
            &graph,
            Some(owned_worktrees),
            &mut guard.external_cache,
        )?;
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            ..Default::default()
        };
        let completed_refs: BTreeSet<TaskRef> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task && n.status == NodeStatus::Completed)
            .filter_map(|n| n.task_ref.clone())
            .collect();
        let mut out = Vec::with_capacity(graph.dependencies.len());
        for dep in graph
            .dependencies
            .iter()
            .filter(|d| d.depends_on_run_id != graph.run.run_id)
        {
            let checkout_raw = PathBuf::from(&dep.depends_on_workspace);
            let checkout = if checkout_raw.as_path() == Path::new(".") {
                checkout_raw
            } else {
                canonical_root(&checkout_raw).unwrap_or(checkout_raw)
            };
            let persisted = guard.runs.get(&dep.depends_on_run_id).map(|(r, _)| r);
            let scoped = (checkout.as_path() != Path::new(".")).then_some(checkout.as_path());
            let target_selector = checkout_scoped_target_selector(
                persisted,
                dep.depends_on_run_id.as_str(),
                scoped,
            )
            .to_owned();
            let satisfied = is_dep_satisfied_against_statuses(
                &graph.run.run_id,
                dep,
                |r| completed_refs.contains(r),
                &obs,
            );
            out.push(RuntimeActiveExternalDep {
                source_task_ref: dep.task_ref.clone(),
                target_workspace: dep.depends_on_workspace.clone(),
                target_run_id: dep.depends_on_run_id.clone(),
                target_task_ref: dep.depends_on_ref.clone(),
                target_selector,
                satisfied,
            });
        }
        Ok(out)
    }

    fn project_active_run_tasks(
        &self,
        run_id: &RunId,
        owned_worktrees: &[PathBuf],
    ) -> Result<Vec<RuntimeActiveTask>, AcddError> {
        let Some(config) = self.roadmap_config else {
            return Ok(Vec::new());
        };
        let mut guard = self.roadmap_cache.lock().expect("roadmap cache lock");
        self.ensure_roadmap_runs_loaded(&mut guard)?;
        let Some((run, Some(graph))) = guard.runs.get(run_id).cloned() else {
            return Ok(Vec::new());
        };
        let ext_deps = collect_external_satisfied_deps_cached(
            config,
            &graph,
            Some(owned_worktrees),
            &mut guard.external_cache,
        )?;
        let obs = ValidatedObservations {
            external_satisfied_deps: ext_deps,
            external_deps_authoritative: true,
            ..Default::default()
        };
        let disk_doc = read_spec_readonly(config, &run);
        let task_nodes: Vec<&GateNodeRecord> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Task && n.task_ref.is_some())
            .collect();
        let completed_refs: BTreeSet<TaskRef> = task_nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .filter_map(|n| n.task_ref.clone())
            .collect();
        let claimed_scopes = claimed_unfinished_scopes(&graph);
        let mut projected_tasks = Vec::with_capacity(task_nodes.len());
        for node in &task_nodes {
            let Some(ref task_ref) = node.task_ref else {
                continue;
            };
            let t_ref = task_ref.as_str();
            let disk_task = disk_doc
                .as_ref()
                .and_then(|d| d.tasks.iter().find(|t| t.task_ref.as_str() == t_ref));
            let mut deps = Vec::new();
            let mut unsatisfied_local_blockers = BTreeSet::new();
            let mut has_unsatisfied_external_dep = false;
            for dep in graph
                .dependencies
                .iter()
                .filter(|d| &d.task_ref == task_ref)
            {
                let satisfied = is_dep_satisfied_against_statuses(
                    &graph.run.run_id,
                    dep,
                    |r| completed_refs.contains(r),
                    &obs,
                );
                let raw = if dep.depends_on_run_id == graph.run.run_id {
                    dep.depends_on_ref.as_str().to_owned()
                } else if dep.depends_on_workspace == "." {
                    format!(
                        "{}:{}",
                        dep.depends_on_run_id.as_str(),
                        dep.depends_on_ref.as_str()
                    )
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
                        unsatisfied_local_blockers.insert(dep.depends_on_ref.as_str().to_owned());
                    } else {
                        has_unsatisfied_external_dep = true;
                    }
                }
                deps.push(RoadmapDepEntry {
                    raw,
                    status: if satisfied { "satisfied" } else { "planned" }.to_owned(),
                    satisfied,
                });
            }
            let all_deps_ok = task_dependencies_satisfied(&graph, task_ref, &obs);
            let task_gates: Vec<RoadmapActiveGateEntry> = graph
                .nodes
                .iter()
                .filter(|n| {
                    n.kind == NodeKind::TaskGate
                        && n.task_ref.as_ref().is_some_and(|t| t.as_str() == t_ref)
                        && n.status == NodeStatus::InProgress
                })
                .map(to_active_gate_entry)
                .collect();
            let disk_status = disk_task.map_or(SpecStatus::Planned, |t| t.status);
            let status = match node.status {
                NodeStatus::Completed => "completed",
                NodeStatus::Cancelled => "cancelled",
                NodeStatus::InProgress => "in_progress",
                _ if is_task_schedule_eligible(&graph, node, &claimed_scopes, all_deps_ok) => {
                    "ready"
                }
                _ if disk_status == SpecStatus::Cancelled => "cancelled",
                _ => "planned",
            };
            projected_tasks.push(RuntimeActiveTask {
                task_ref: t_ref.to_owned(),
                status: status.to_owned(),
                active_gates: task_gates,
                scope: container_scope(&graph, node),
                depends_on: deps,
                unsatisfied_local_blockers,
                has_unsatisfied_external_dep,
            });
        }
        Ok(projected_tasks)
    }
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

/// Runtime overlay supplying `StateBackend::max_spec_number` for `acdd_spec::draft_spec`.
pub(crate) struct BackendDraftOverlay<'a>(pub(crate) &'a dyn StateBackend);

impl SpecRuntimeOverlay for BackendDraftOverlay<'_> {
    fn max_spec_number(&self, collection: &str, prefix: &str) -> Result<Option<u32>, AcddError> {
        self.0.max_spec_number(collection, prefix)
    }
}

/// Validate intra-spec task DAG and external `depends_on` references via [`StateRuntimeOverlay`].
pub fn validate_spec_dependencies(
    root: &Path,
    adapter: &Adapter,
    local_reader: Option<&dyn StateReader>,
    state_db_override: Option<&Path>,
    doc: &acdd_spec::SpecDocument,
    active_specs: &[acdd_spec::SpecDocument],
) -> Vec<acdd_spec::deps::DepValidationIssue> {
    let overlay = StateRuntimeOverlay::for_validation(root, adapter, local_reader, state_db_override);
    acdd_spec::deps::validate_spec_dependencies(root, adapter, &overlay, doc, active_specs)
}
