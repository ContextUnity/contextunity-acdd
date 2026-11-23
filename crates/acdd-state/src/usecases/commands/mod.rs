//! Mutating and plugin command handlers (`Start`, `Draft`, `Claim`, `ExtendScope`, `Reopen`,
//! `Sync`, `Subtask`, `BlackboardPost`, `Clear`, `CallPluginTool`).

mod claim;
mod common;
mod run_ops;
mod start;
pub(super) mod task_ops;

pub(crate) use claim::handle_claim_dyn;
pub(crate) use crate::context::is_context_reset_required;
pub(crate) use run_ops::{handle_clear_dyn, handle_draft_dyn, handle_sync_dyn};
pub(crate) use start::handle_start_dyn;
pub(crate) use task_ops::{
    handle_blackboard_post_dyn, handle_call_plugin_tool_dyn, handle_extend_scope_dyn,
    handle_reopen_dyn, handle_subtask_dyn,
};
