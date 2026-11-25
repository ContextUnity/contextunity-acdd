//! Read-only roadmap builder: cross-collection & cross-worktree spec DAG,
//! topological waves, critical-path unblocks priority, scope-overlap detection,
//! and task drill-down.

mod discovery;
mod graph;
mod render;
mod types;
mod waves;

use self::discovery::discover_specs_across_worktrees;
use self::graph::build_dependency_graph;
use self::render::assemble_roadmap_output;
use self::types::*;
use self::waves::compute_unblocks_counts;
use crate::store::traits::StateReader;
use crate::types::RunStatus;
use crate::usecases::config::StateEngineConfig;
use crate::usecases::dto::{RoadmapReq, RoadmapResp};
use crate::usecases::lifecycle::{
    effective_spec_collections, list_all_runs, primary_workspace_root,
};
use acdd_core::errors::StateError;
use acdd_core::fs::canonical_root;
use std::collections::BTreeMap;

/// Execute `StateQuery::Roadmap` strictly over `dyn StateReader` without creating `state.sqlite`.
pub(crate) fn handle_roadmap(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: RoadmapReq,
) -> Result<RoadmapResp, StateError> {
    let root = primary_workspace_root(config);
    let current_root = canonical_root(&config.worktree).unwrap_or_else(|_| config.worktree.clone());
    let primary_canon = canonical_root(&root).unwrap_or_else(|_| root.clone());
    let collections = effective_spec_collections(config);

    let active_runs_by_spec = load_active_runs(reader)?;
    let mut discovered = discover_specs_across_worktrees(
        &current_root,
        &primary_canon,
        &collections,
        &active_runs_by_spec,
        req.current_only,
    )?;

    let mut graph_warnings = Vec::new();
    let graph = build_dependency_graph(
        &discovered,
        &primary_canon,
        config,
        reader,
        &active_runs_by_spec,
        req.archived
            || req
                .spec_id
                .as_ref()
                .is_some_and(|id| discovered.archived_by_id.contains_key(id)),
        &mut graph_warnings,
    )?;
    discovered.warnings.extend(graph_warnings);

    let unblocks_counts =
        compute_unblocks_counts(&discovered.specs_by_id, &graph.forward_spec_edges);
    assemble_roadmap_output(
        config,
        &discovered,
        &graph,
        &active_runs_by_spec,
        &unblocks_counts,
        &req,
    )
}

fn load_active_runs(reader: &dyn StateReader) -> Result<ActiveRunMap, StateError> {
    let active_runs = list_all_runs(reader, Some(RunStatus::Active))?;
    let mut active_runs_by_spec: ActiveRunMap = BTreeMap::new();
    for run in active_runs {
        let graph = reader.load_run_graph(&run.run_id)?;
        if let Some(ref spec_id) = run.spec_id {
            active_runs_by_spec
                .entry(spec_id.clone())
                .or_default()
                .push((run, graph));
        }
    }
    for runs in active_runs_by_spec.values_mut() {
        runs.sort_by(|a, b| a.0.run_id.as_str().cmp(b.0.run_id.as_str()));
    }
    Ok(active_runs_by_spec)
}
