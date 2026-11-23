//! Version-2 `GateProfile` parser, inheritance/patch resolver, and fail-closed validator.

pub mod inherit;
pub mod schema;
pub mod shell_template;
pub mod validate;
pub mod when;

pub use inherit::{
    embedded_profile, resolve_optional_profile_ref_with_dirs, resolve_profile_from_str_with_dirs,
    resolve_profile_ref_with_dirs,
};
pub use schema::{
    CommandDef, GateDef, GateDispatchInfo, ProofDef, ProofKind, ResolvedGateProfile,
    ResourceLimits, RoleDef, RoleExecutionMode,
};
pub use shell_template::{render_command_with_scope_target, render_command_with_test_filter};
pub use validate::{
    check_guidance_budget, resolve_and_budget_guidance, resolve_command_for_scope,
    validate_profile, ProfileValidationOptions, ResolvedCommandInvocation,
};
