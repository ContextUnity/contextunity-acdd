//! Indexing pipeline orchestration for cold builds and incremental delta runs.

mod budget;
mod cold;
mod delta;
/// Test fault-injection hooks.
pub mod hooks;
mod hydration;
mod invalidation;
mod runtime;
/// Shared pipeline build utilities.
pub mod shared;
mod staging;

pub use cold::{
    build, build_with_cache_dir, build_with_selection_and_guard, validate_code_map_target,
};
pub use delta::delta_with_selection;
pub(crate) use delta::delta_with_selection_and_guard;
pub(crate) use shared::SourceSnapshotMismatch;

// ---------------------------------------------------------------------------
// Incremental Delta Indexing
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "lang-rust"))]
mod tests;
