//! Single canonical external, cross-spec, and archive dependency resolver (`INV-SINGLE-OWNER`).

mod external;
mod snapshot;
mod syntax;
mod validate;

pub use external::{collect_external_satisfied_deps, verify_run_level_depends_on};
pub(crate) use external::{
    collect_external_satisfied_deps_cached, collect_external_satisfied_deps_with_owned,
    resolve_dep_target, DepTargetResolution,
};
pub(crate) use snapshot::{
    collect_archived_specs, collect_workspace_specs, collect_workspace_specs_cached,
    load_dep_snapshot, load_task_status_map, load_tombstone_archives, CollectedWorkspaceSpec,
    DepSnapshot, PreloadedDepContext, TombstoneArchiveError,
};
pub use syntax::try_resolve_effective_state_db;
pub(crate) use syntax::{
    checkout_scoped_target_selector, effective_active_run_config, load_ws_adapter,
    parse_dep_syntax, parse_task_dependency_with_db, resolve_linked_workspace_root,
    resolve_spec_repositories, resolve_ws_context_canon, run_in_checkout, run_matches_selector,
    ParsedDepSyntax,
};
pub use validate::validate_spec_dependencies;
