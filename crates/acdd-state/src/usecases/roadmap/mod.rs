//! Read-only roadmap builder delegating graph discovery and rendering to `acdd-spec`
//! over [`crate::usecases::overlay::StateRuntimeOverlay`].

use crate::store::traits::StateReader;
use crate::usecases::config::StateEngineConfig;
use crate::usecases::dto::{RoadmapReq, RoadmapResp};
use crate::usecases::overlay::StateRuntimeOverlay;
use acdd_core::errors::AcddError;
use acdd_spec::roadmap::RoadmapConfig;

/// Execute `StateQuery::Roadmap` strictly over `dyn StateReader` without creating `state.sqlite`.
pub(crate) fn handle_roadmap(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: RoadmapReq,
) -> Result<RoadmapResp, AcddError> {
    let overlay = StateRuntimeOverlay::for_roadmap(config, reader);
    let rm_cfg = RoadmapConfig {
        workspace_root: &config.workspace_root,
        worktree: &config.worktree,
        spec_collections: &config.spec_collections,
        linked_workspaces: &config.linked_workspaces,
    };
    acdd_spec::roadmap::build_roadmap(&rm_cfg, &overlay, req)
}
