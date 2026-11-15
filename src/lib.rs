//! ContextUnity ACDD code graph, persistence, language analysis, and tool APIs.
#![warn(missing_docs)]

/// Implements cli support.
pub mod cli;
/// Implements core support.
pub mod core;
/// Implements db support.
pub mod db;
/// Implements engine support.
pub mod engine;
/// Implements mcp support.
pub mod mcp;

/// Re-exported acdd-core crate.
pub use acdd_core;
/// Re-exported acdd-index crate.
pub use acdd_index;
/// Re-exported acdd-tasks crate.
pub use acdd_tasks;
