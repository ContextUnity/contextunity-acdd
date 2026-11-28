//! Pure specification frontmatter, parsing, drafting, validation, and static roadmap DAG engine.

#![warn(missing_docs)]

pub mod deltas;
pub mod deps;
pub mod discovery;
pub mod draft;
pub mod frontmatter;
pub mod overlay;
pub mod roadmap;
pub mod schema;
pub mod spec;
pub mod types;
pub mod validate;

pub use deltas::verify_spec_deltas;

pub use discovery::{
    effective_spec_collections, match_spec_collection, matching_spec_collections,
    next_spec_id_for_prefix, read_confined_utf8,
};
pub use draft::draft_spec;
pub use spec::{
    is_safe_path_segment_id, materialize_ephemeral_goal_spec_with_options, parse_spec,
    parse_spec_with_collections, render_archive_markdown_with_collections, ArchiveRenderInput,
    ArchiveTaskUpdate, ClosureSummary, Receipt, ReceiptRollup, SpecDocument, SpecStatus, Subtask,
};
pub use types::{
    DraftReq, DraftResp, ExtendedScopeEntry, InvariantsState, RoadmapActiveGateEntry,
    RoadmapDepEntry, RoadmapReq, RoadmapResp, RoadmapSpecEntry, RoadmapTaskEntry, RoadmapWave,
    SubtaskStatus, ValidateDiagnostic, ValidateResp,
};
