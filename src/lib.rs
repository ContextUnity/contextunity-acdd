//! ContextUnity ACDD code graph, persistence, language analysis, and tool APIs.
#![warn(missing_docs)]

/// Implements cli support.
pub mod cli;
/// Implements engine support.
pub mod engine;
/// Shared operation presentation and metadata compaction.
pub mod format;
/// Implements mcp support.
pub mod mcp;
/// Host DomainProvider implementations (`code`, `docs`) and state engine bridge.
pub mod state_bridge;
/// Telemetry and debug logging.
pub mod telemetry;

/// Re-exported acdd-core crate.
pub use acdd_core;
/// Re-exported acdd-index crate.
pub use acdd_index;
/// Re-exported acdd-state crate.
pub use acdd_state;
