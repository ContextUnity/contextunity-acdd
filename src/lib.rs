//! ContextUnity ACDD code graph, persistence, language analysis, and tool APIs.
#![warn(missing_docs)]

/// CLI argument grammar, command dispatch, and output writing.
pub mod cli;
/// Indexing pipeline, AST search, and documentation link verification engine.
pub mod engine;
/// Shared operation presentation and metadata compaction.
pub mod format;
/// MCP stdio server, workspace reader admission, and tool routing.
pub mod mcp;
/// Host DomainProvider implementations (`code`, `docs`) and state engine bridge.
pub mod state_bridge;
/// Telemetry and debug logging.
pub mod telemetry;
