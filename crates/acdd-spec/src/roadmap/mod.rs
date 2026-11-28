//! Read-only roadmap builder: cross-collection & cross-worktree spec DAG,
//! topological waves, critical-path unblocks priority, scope-overlap detection,
//! and task drill-down over [`SpecRuntimeOverlay`].

mod discovery;
mod graph;
mod render;
mod types;
mod waves;

use self::discovery::discover_specs_across_worktrees;
use self::graph::build_dependency_graph;
use self::render::{assemble_roadmap_output, resolve_spec_id_selector};
use crate::discovery::effective_spec_collections;
use crate::overlay::SpecRuntimeOverlay;
use crate::types::{RoadmapReq, RoadmapResp};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::fs::canonical_root;
use acdd_core::git::list_owned_worktrees_info;
use std::path::PathBuf;

pub use self::render::group_roadmap_resp;
pub use self::types::RoadmapConfig;

fn validate_roadmap_req(req: &RoadmapReq) -> Result<(), AcddError> {
    for tag in &req.tags {
        if tag.trim().is_empty() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "roadmap --tag filter must not be empty",
            ));
        }
    }
    for raw in &req.fields {
        let valid = raw
            .split_once('=')
            .is_some_and(|(k, _)| !k.trim().is_empty());
        if !valid {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("roadmap --field filter '{raw}' must be 'key=value' with a non-empty key"),
            ));
        }
    }
    if req.group_by.as_deref().is_some_and(|g| g.trim().is_empty()) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "roadmap --group-by axis must not be empty",
        ));
    }
    Ok(())
}

/// Build the cross-worktree roadmap DAG using `overlay` for live runs and tombstones.
pub fn build_roadmap(
    config: &RoadmapConfig<'_>,
    overlay: &dyn SpecRuntimeOverlay,
    req: RoadmapReq,
) -> Result<RoadmapResp, AcddError> {
    validate_roadmap_req(&req)?;
    let primary = acdd_core::git::primary_worktree_root(config.workspace_root)
        .unwrap_or_else(|| config.workspace_root.to_path_buf());
    let current_root = canonical_root(config.worktree).unwrap_or_else(|_| config.worktree.to_path_buf());
    let primary_canon = canonical_root(&primary).unwrap_or(primary);
    let collections = effective_spec_collections(config.spec_collections);

    let worktree_info = list_owned_worktrees_info(&primary_canon).unwrap_or_default();
    let owned_worktrees: Vec<PathBuf> = worktree_info.iter().map(|w| w.path.clone()).collect();
    let active_runs_by_spec = overlay.load_roadmap_active_runs()?;

    let mut discovered = discover_specs_across_worktrees(
        &current_root,
        &primary_canon,
        &collections,
        &worktree_info,
        owned_worktrees,
        &active_runs_by_spec,
        req.current_only,
    )?;

    let mut req = req;
    if let Some(ref query) = req.spec_id {
        if let Some(resolved) = resolve_spec_id_selector(&discovered, query) {
            req.spec_id = Some(resolved);
        }
    }

    let mut graph_warnings = Vec::new();
    let graph = build_dependency_graph(
        &discovered,
        &primary_canon,
        &current_root,
        req.current_only,
        config,
        overlay,
        &active_runs_by_spec,
        req.archived
            || req
                .spec_id
                .as_ref()
                .is_some_and(|id| discovered.archived_by_id.contains_key(id)),
        &mut graph_warnings,
    )?;
    discovered.warnings.extend(graph_warnings);

    assemble_roadmap_output(
        &discovered,
        &graph,
        overlay,
        &active_runs_by_spec,
        &req,
    )
}
