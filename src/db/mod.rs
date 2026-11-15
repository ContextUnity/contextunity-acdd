//! ContextUnity ACDD SQLite database persistence and querying, re-exported from acdd-index.

pub use acdd_index::db::*;

/// Implements writer support with pipeline re-exports.
pub mod writer;

/// Implements tasks store support.
pub use acdd_tasks::store as tasks_store;
