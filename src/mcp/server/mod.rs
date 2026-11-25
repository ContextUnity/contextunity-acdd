//! MCP server handle, workspace adapter resolution, and submodule decomposition.

mod admission;
mod session;
mod types;
mod worktree;

pub use session::serve_with_state_config;
pub use types::Server;
pub(crate) use types::{primary_workspace_root_for, RequestAdapter};
