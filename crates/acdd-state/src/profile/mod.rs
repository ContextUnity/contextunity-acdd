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
    parse_profile, CheckRef, CommandDef, ContourDef, DetailedCommandDef, GateDef, GateDispatchInfo,
    GateGuidanceRef, GateTemplate, GitPolicy, GitPolicyPatch, GuidanceEntry, InlineGuidance,
    ModelSpec, ParsedProfileFile, PatchField, PatternCommandEntry, ProofCatalogDef, ProofDef,
    ProofKind, RawGateDef, RawGateItem, RawTasksBlock, ResolvedGateProfile, ResourceLimits,
    ResourceLimitsPatch, RoleDef, RoleExecutionMode, RolePatch, SavePolicy, SavePolicyPatch,
    StructuredGuidance, SubsystemRegistryPatch, TasksBlock, TestFilterConfig, DEFAULT_MAX_ATTEMPTS,
};
pub use shell_template::{
    ends_inside_shell_comment, posix_single_quote, render_command_with_scope_target,
    render_command_with_test_filter, validate_shell_template_placeholders,
};
pub use validate::{
    check_guidance_budget, glob_specificity, resolve_and_budget_guidance,
    resolve_command_for_scope, validate_profile, ProfileValidationOptions,
    ResolvedCommandInvocation,
};
pub use when::{normalize_optional_when, WhenExpr};
