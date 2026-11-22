/// AST search and language resolution shared by CLI and MCP.
pub mod ast;
/// Markdown link and anchor integrity checker.
pub(crate) mod docs_check;
/// Built-in CLI and MCP guidance topics and workspace initialization (`acdd init`).
pub mod guide;
/// Indexing pipeline orchestration.
pub mod pipeline;
/// Shared layered file and symbol reader for CLI and MCP.
pub mod reader;
