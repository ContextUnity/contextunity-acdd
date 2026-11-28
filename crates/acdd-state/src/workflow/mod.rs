//! Version-2 `WorkflowDef` parser, inheritance/patch resolver, and fail-closed validator.

pub mod inherit;
pub mod schema;
pub mod shell_template;
pub mod validate;
pub mod when;

pub use inherit::{
    embedded_workflow, resolve_optional_workflow_ref_with_dirs, resolve_workflow_from_str_with_dirs,
    resolve_workflow_ref_with_dirs,
};
pub use schema::{
    CommandDef, ContextEntry, GateDef, GateDispatchInfo, ProofDef, ProofKind, ResolvedWorkflow,
    ResourceLimits, RoleDef, RoleExecutionMode,
};
pub use shell_template::{render_command_with_scope_target, render_command_with_test_filter};
pub use validate::{
    check_guidance_budget, collect_workflow_glob_warnings, resolve_and_budget_guidance,
    resolve_command_for_scope, validate_workflow, ResolvedCommandInvocation,
    WorkflowValidationOptions,
};
