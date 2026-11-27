//! Single canonical external, cross-spec, and archive dependency resolver (`INV-SINGLE-OWNER`).

mod external;
mod snapshot;
mod syntax;

pub(crate) use acdd_spec::discovery::resolve_spec_repositories;
pub use external::{collect_external_satisfied_deps, verify_run_level_depends_on};
pub(crate) use external::{
    collect_external_satisfied_deps_cached, resolve_active_run_dep, resolve_tombstone_dep,
    ExternalDepCache,
};
pub(crate) use snapshot::{load_dep_snapshot, DepSnapshot};
pub use syntax::try_resolve_effective_state_db;
pub(crate) use syntax::{
    checkout_scoped_target_selector, parse_task_dependency_with_db, resolve_ws_context_canon,
};
