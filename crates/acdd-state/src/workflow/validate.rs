//! Fail-closed `ResolvedWorkflow` validation, command pattern selection, and guidance budgeting.

use super::schema::{
    CommandDef, ContextEntry, DetailedCommandDef, GateDef, GateGuidanceRef, ProofDef, ProofKind,
    ResolvedWorkflow, ResourceLimits, TestFilterConfig,
};
use super::shell_template::{glob_specificity, validate_shell_template_placeholders};
use super::when::WhenExpr;
use crate::types::SaveAction;
use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::ids::{GateId, GateSelector};
use acdd_core::path_policy::{
    is_valid_scope_glob, normalize_rel_path, should_skip_walk_dir, WorkspacePathPolicy,
};
use acdd_core::ports::{is_builtin_mcp_tool, CompositeDomainProvider, ContextRef};
use acdd_core::subsystems::SubsystemRegistry;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Legacy 0.5 tool names explicitly rejected during v2 profile validation.
const LEGACY_TOOL_NAMES: &[&str] = &[
    "task_list",
    "task_claim",
    "task_submit",
    "task_manage",
    "task_blackboard",
    "milestone_init",
    "milestone_verify",
    "milestone_handoff",
    "session_checkpoint",
    "forge_guide",
    "check-write",
];

/// Options controlling fail-closed `validate_workflow` execution.
#[derive(Default)]
pub struct WorkflowValidationOptions<'a> {
    /// Registered host and external `DomainProvider`s (`None` validates against zero providers).
    pub domain_providers: Option<&'a CompositeDomainProvider>,
    /// Workspace `acdd.yaml` `modules:` override combined with `profile.modules`.
    pub acdd_modules: Option<&'a SubsystemRegistry>,
    /// Effective `acdd.yaml` `specs:` collection registry (`None` defaults to built-in `milestones`/`plans`).
    pub spec_collections: Option<&'a BTreeMap<String, SpecConfig>>,
    /// When `true`, any gate referencing an `unbound: true` command fails with `CommandUnbound`.
    pub require_bound_commands: bool,
}

/// Selected command invocation after resolving `Pattern` specificity and YAML order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommandInvocation {
    /// Command template string (still before `{test_filter}` expansion).
    pub command: String,
    /// Optional `--test` filter configuration.
    pub test_filter: Option<TestFilterConfig>,
    /// Optional timeout override in seconds.
    pub timeout_seconds: Option<u64>,
    /// Path-pattern to `{scope_target}` argument overrides.
    pub scope_args_by_pattern: BTreeMap<String, String>,
    /// Sorted `scope` paths that matched this invocation (empty for non-pattern commands).
    pub matched_paths: Vec<String>,
    /// When `true`, runs verbatim without `{scope_target}` or `{test_filter}` expansion.
    pub post_always: bool,
}

/// Validate a `ResolvedWorkflow` fail-closed against `DomainProvider` capabilities, `modules:`, and all schema invariants.
pub fn validate_workflow(
    profile: &ResolvedWorkflow,
    opts: &WorkflowValidationOptions<'_>,
) -> Result<(), AcddError> {
    if profile.version != 2 {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid profile version {}; expected 2", profile.version),
        ));
    }

    profile.modules.validate().map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid workflow modules: {err}"),
        )
    })?;
    if let Some(acdd_mods) = opts.acdd_modules {
        acdd_mods.validate().map_err(|err| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("invalid workspace modules: {err}"),
            )
        })?;
    }

    for req in &profile.modules.required {
        if !is_subsystem_effectively_enabled(req, &profile.modules, opts.acdd_modules) {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!(
                    "required subsystem '{req}' is disabled in effective modules configuration"
                ),
            ));
        }
    }

    if profile.save == SaveAction::Snapshot {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "snapshot save is only allowed on leaf task gates",
        ));
    }
    validate_limits(&profile.limits)?;
    validate_protected_paths(profile)?;
    validate_paths_map(profile)?;
    validate_proof_catalog(profile)?;
    validate_roles(profile)?;
    validate_commands(profile)?;

    let empty_providers = CompositeDomainProvider::default();
    let providers = opts.domain_providers.unwrap_or(&empty_providers);

    for tool in &profile.default_tools {
        validate_tool_name(
            tool,
            &profile.modules,
            opts.acdd_modules,
            providers,
            "default_tools",
        )?;
    }

    let task_gate_ids = collect_all_task_gate_ids(&profile.gates);
    validate_gate_container(
        profile,
        &profile.gates,
        GateScopeKind::Root,
        &task_gate_ids,
        opts,
    )?;

    // Terminal root_gate check: last root_gate (and any gate with archive/prune) must not have `pass:`.
    if let Some(last_root_gate) = profile
        .gates
        .iter()
        .rev()
        .find(|g| !g.is_tasks_block() && g.gates.is_empty())
    {
        if last_root_gate.pass.is_some() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "terminal root_gate '{}' must not declare 'pass:'",
                    last_root_gate.id
                ),
            ));
        }
    }

    Ok(())
}

fn validate_limits(limits: &ResourceLimits) -> Result<(), AcddError> {
    if limits.max_guidance_bytes == 0
        || limits.max_log_bytes == 0
        || limits.max_capture_bytes == 0
        || limits.max_line_bytes == 0
        || limits.diagnostic_inline_bytes == 0
        || limits.max_metadata_bytes == 0
        || limits.max_trace_summary_bytes == 0
        || limits.max_command_timeout_seconds == 0
    {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "all ResourceLimits byte and timeout bounds must be > 0",
        ));
    }
    Ok(())
}

fn validate_protected_paths(profile: &ResolvedWorkflow) -> Result<(), AcddError> {
    for pat in &profile.protected_paths {
        if !is_valid_scope_glob(pat) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("protected_paths contains invalid or non-normalized scope glob '{pat}'"),
            ));
        }
    }
    Ok(())
}

fn validate_paths_map(profile: &ResolvedWorkflow) -> Result<(), AcddError> {
    for (name, globs) in &profile.paths {
        if name.trim().is_empty() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "workflow paths: entry name cannot be empty",
            ));
        }
        for pat in globs {
            if !is_valid_scope_glob(pat) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("paths.{name} contains invalid or non-normalized scope glob '{pat}'"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_proof_catalog(profile: &ResolvedWorkflow) -> Result<(), AcddError> {
    for (name, def) in &profile.proofs {
        if matches!(
            name.as_str(),
            "none" | "contract" | "command" | "call" | "review" | "seam_test" | "delivery"
        ) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("proof catalog entry '{name}' uses a reserved or legacy proof name"),
            ));
        }
        def.schema
            .validate_schema(&format!("proofs.{name}.schema"))?;
    }
    Ok(())
}

fn validate_roles(profile: &ResolvedWorkflow) -> Result<(), AcddError> {
    for (name, role) in &profile.roles {
        if name.trim().is_empty() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "role name cannot be empty",
            ));
        }
        if let Some(runner) = &role.runner {
            if runner.trim().is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("roles.{name}.runner cannot be empty when specified"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_scope_args_map(
    scope_args: &BTreeMap<String, String>,
    ctx: &str,
) -> Result<(), AcddError> {
    for (pat, arg) in scope_args {
        validate_shell_template_placeholders(
            arg,
            &["{file_stem}", "{path}"],
            &format!("{ctx}[{pat}]"),
        )?;
    }
    Ok(())
}

fn validate_commands(profile: &ResolvedWorkflow) -> Result<(), AcddError> {
    validate_scope_args_map(&profile.scope_args_by_pattern, "scope_args_by_pattern")?;
    for (name, cmd) in &profile.commands {
        if name.trim().is_empty() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "command slot name cannot be empty",
            ));
        }
        match cmd {
            CommandDef::Unbound { unbound, .. } => {
                if !unbound {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("commands.{name}: unbound must be true when no command is set"),
                    ));
                }
            }
            CommandDef::Script(script) => {
                validate_command_string(script, None, &format!("commands.{name}"))?;
            }
            CommandDef::Detailed(d) => {
                validate_detailed_command(d, &format!("commands.{name}"))?;
            }
            CommandDef::Pattern(entries) => {
                if entries.is_empty() {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("commands.{name}: pattern command list cannot be empty"),
                    ));
                }
                for (idx, entry) in entries.iter().enumerate() {
                    let path = format!("commands.{name}[{idx}]");
                    if entry.pattern.trim().is_empty() {
                        return Err(AcddError::new(
                            ErrorCode::WorkflowSchemaInvalid,
                            format!("{path}.pattern cannot be empty"),
                        ));
                    }
                    if entry.timeout_seconds == Some(0) {
                        return Err(AcddError::new(
                            ErrorCode::WorkflowSchemaInvalid,
                            format!("{path}.timeout_seconds must be > 0"),
                        ));
                    }
                    validate_command_string(&entry.command, entry.test_filter.as_ref(), &path)?;
                    validate_scope_args_map(
                        &entry.scope_args_by_pattern,
                        &format!("{path}.scope_args_by_pattern"),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_detailed_command(d: &DetailedCommandDef, ctx: &str) -> Result<(), AcddError> {
    if d.timeout_seconds == Some(0) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}.timeout_seconds must be > 0"),
        ));
    }
    validate_command_string(&d.command, d.test_filter.as_ref(), ctx)?;
    validate_scope_args_map(
        &d.scope_args_by_pattern,
        &format!("{ctx}.scope_args_by_pattern"),
    )?;
    Ok(())
}

fn validate_command_string(
    command: &str,
    test_filter: Option<&TestFilterConfig>,
    ctx: &str,
) -> Result<(), AcddError> {
    if command.trim().is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}.command cannot be empty"),
        ));
    }
    validate_shell_template_placeholders(command, &["{scope_target}", "{test_filter}"], ctx)?;
    let placeholder_count = command.matches("{test_filter}").count();
    if placeholder_count > 1 {
        return Err(AcddError::new(
            ErrorCode::CommandTestFilterUnsupported,
            format!("{ctx}: command template may contain at most one '{{test_filter}}' placeholder (found {placeholder_count})"),
        ));
    }
    if let Some(TestFilterConfig::Template(tpl)) = test_filter {
        let count = tpl.matches("{test}").count();
        if tpl.trim().is_empty() || count != 1 {
            return Err(AcddError::new(
                ErrorCode::CommandTestFilterUnsupported,
                format!("{ctx}.test_filter template must contain '{{test}}' exactly once"),
            ));
        }
        validate_shell_template_placeholders(tpl, &["{test}"], &format!("{ctx}.test_filter"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateScopeKind {
    Root,
    RootContainerChild,
    TaskGate,
}

fn collect_all_task_gate_ids(gates: &[GateDef]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for g in gates {
        if let Some(tb) = &g.tasks {
            collect_gate_ids_recursive(&tb.gates, &mut out);
        }
    }
    out
}

fn collect_gate_ids_recursive(gates: &[GateDef], out: &mut BTreeSet<String>) {
    for g in gates {
        if !g.id.is_empty() {
            out.insert(g.id.clone());
        }
        collect_gate_ids_recursive(&g.gates, out);
    }
}

fn validate_gate_container(
    profile: &ResolvedWorkflow,
    gates: &[GateDef],
    scope_kind: GateScopeKind,
    task_gate_ids: &BTreeSet<String>,
    opts: &WorkflowValidationOptions<'_>,
) -> Result<(), AcddError> {
    let mut sibling_ids = BTreeSet::new();
    for gate in gates {
        if gate.is_tasks_block() {
            continue;
        }
        if gate.id.is_empty() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "gate id cannot be empty outside '- tasks:' blocks",
            ));
        }
        GateId::new(&gate.id)?;
        if !sibling_ids.insert(gate.id.clone()) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("duplicate gate id '{}' in container", gate.id),
            ));
        }
    }

    for gate in gates {
        if gate.save.post == Some(SaveAction::Snapshot)
            && (scope_kind != GateScopeKind::TaskGate
                || gate.is_tasks_block()
                || !gate.gates.is_empty())
        {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "gate '{}': snapshot save is only allowed on leaf task gates",
                    gate.id
                ),
            ));
        }
        if let Some(tb) = &gate.tasks {
            if scope_kind != GateScopeKind::Root {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "'- tasks:' block is only permitted at top level of 'gates:'",
                ));
            }
            if !gate.id.is_empty() {
                GateId::new(&gate.id)?;
                if !sibling_ids.insert(gate.id.clone()) {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("duplicate gate id '{}' on tasks block", gate.id),
                    ));
                }
            }
            if let Some(when_str) = tb.when.as_deref().or(gate.when.as_deref()) {
                let expr = WhenExpr::parse(when_str)?;
                expr.validate_tags(&profile.tags, "tasks.when")?;
            }
            if tb.gates.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "'- tasks:' block must declare at least one task gate",
                ));
            }
            if gate.save.archive || gate.save.prune {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "archive/prune are only allowed on leaf root_gate nodes",
                ));
            }
            validate_gate_container(
                profile,
                &tb.gates,
                GateScopeKind::TaskGate,
                task_gate_ids,
                opts,
            )?;
            continue;
        }

        let is_container = !gate.gates.is_empty();
        if scope_kind != GateScopeKind::TaskGate && gate.when.is_some() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "gate '{}': 'when:' is forbidden on root_gate and root containers",
                    gate.id
                ),
            ));
        }
        if let Some(when_str) = &gate.when {
            let expr = WhenExpr::parse(when_str)?;
            expr.validate_tags(&profile.tags, &format!("gates.{}", gate.id))?;
        }

        if is_container {
            if gate.save.archive || gate.save.prune {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "container gate '{}': archive and prune are only allowed on leaf root_gate nodes",
                        gate.id
                    ),
                ));
            }
            let child_kind = match scope_kind {
                GateScopeKind::Root | GateScopeKind::RootContainerChild => {
                    GateScopeKind::RootContainerChild
                }
                GateScopeKind::TaskGate => GateScopeKind::TaskGate,
            };
            validate_gate_container(profile, &gate.gates, child_kind, task_gate_ids, opts)?;
        }

        validate_single_gate(
            profile,
            gate,
            scope_kind,
            is_container,
            &sibling_ids,
            task_gate_ids,
            opts,
        )?;
    }

    Ok(())
}

fn validate_gate_transitions(
    gate: &GateDef,
    ctx: &str,
    sibling_ids: &BTreeSet<String>,
) -> Result<(), AcddError> {
    if let Some(pass_target) = &gate.pass {
        if pass_target.as_str() == gate.id {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: 'pass:' cannot point to itself"),
            ));
        }
        if !sibling_ids.contains(pass_target.as_str()) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: 'pass:' references unknown sibling gate '{pass_target}'"),
            ));
        }
    }
    for (label, target_opt) in [("fail", &gate.fail), ("fail_drift", &gate.fail_drift)] {
        if let Some(fail_target) = target_opt {
            if fail_target.as_str() == gate.id {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: '{label}:' cannot point to itself"),
                ));
            }
            if !sibling_ids.contains(fail_target.as_str()) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: '{label}:' references unknown sibling gate '{fail_target}'"),
                ));
            }
        }
        if gate.pass.is_some() && gate.pass.as_ref() == target_opt.as_ref() {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: 'pass:' and '{label}:' cannot target the same gate"),
            ));
        }
    }
    Ok(())
}

fn validate_single_gate(
    profile: &ResolvedWorkflow,
    gate: &GateDef,
    scope_kind: GateScopeKind,
    is_container: bool,
    sibling_ids: &BTreeSet<String>,
    task_gate_ids: &BTreeSet<String>,
    opts: &WorkflowValidationOptions<'_>,
) -> Result<(), AcddError> {
    let empty_providers = CompositeDomainProvider::default();
    let providers = opts.domain_providers.unwrap_or(&empty_providers);
    let ctx = format!("gate '{}'", gate.id);

    if gate.max_attempts < 1 {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: max_attempts must be >= 1"),
        ));
    }

    if (gate.save.archive || gate.save.prune) && (scope_kind != GateScopeKind::Root || is_container)
    {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: 'save.archive' and 'save.prune' are only permitted on root_gate"),
        ));
    }
    if (gate.save.archive || gate.save.prune) && gate.pass.is_some() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: terminal root_gate with archive/prune must not declare 'pass:'"),
        ));
    }
    if let Some(ref tpl) = gate.save.report {
        validate_report_path_template(&ctx, tpl, &profile.protected_paths)?;
    }

    validate_gate_transitions(gate, &ctx, sibling_ids)?;

    if gate.receipt && gate.need.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: 'receipt: true' requires non-empty 'need:'"),
        ));
    }

    for sel in &gate.need {
        validate_gate_selector(
            sel,
            &gate.id,
            scope_kind,
            sibling_ids,
            task_gate_ids,
            "need",
        )?;
    }
    for sel in &gate.independent_from {
        validate_gate_selector(
            sel,
            &gate.id,
            scope_kind,
            sibling_ids,
            task_gate_ids,
            "independent_from",
        )?;
    }

    if let Some(role_name) = &gate.role {
        if !profile.roles.contains_key(role_name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: unknown role '{role_name}'"),
            ));
        }
    }

    if let Some(cmd_name) = &gate.command {
        let cmd = profile.commands.get(cmd_name).ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: unknown command '{cmd_name}'"),
            )
        })?;
        if opts.require_bound_commands && matches!(cmd, CommandDef::Unbound { .. }) {
            return Err(AcddError::new(
                ErrorCode::CommandUnbound,
                format!("{ctx}: command slot '{cmd_name}' is unbound"),
            )
            .with_hint(format!(
                "Override commands.{cmd_name} in .acdd/workflows/default.yaml before starting or running a workflow"
            )));
        }
    }

    if let Some(call_ref) = &gate.call {
        let domain = call_ref.split('.').next().unwrap_or("");
        ensure_domain_enabled(domain, &profile.modules, opts.acdd_modules, &ctx)?;
        let default_params = json!({});
        let params = gate.with.as_ref().unwrap_or(&default_params);
        providers.validate_action_ref(call_ref, params)?;
    }

    for chk in &gate.checks {
        let domain = chk.check.split('.').next().unwrap_or("");
        ensure_domain_enabled(domain, &profile.modules, opts.acdd_modules, &ctx)?;
        providers.validate_check_ref(&chk.check, &chk.with)?;
    }

    for entry in &gate.context {
        validate_gate_context_entry(entry, profile, opts, &ctx)?;
    }

    validate_gate_scope(gate, profile, opts, &ctx)?;

    for g_ref in &gate.guidance {
        match g_ref {
            GateGuidanceRef::Slot(slot) => {
                if !profile.guidance.contains_key(slot) {
                    return Err(AcddError::new(
                        ErrorCode::UnknownGuidanceSlot,
                        format!("{ctx}: unknown guidance slot '{slot}'"),
                    ));
                }
            }
            GateGuidanceRef::Inline(inline) => {
                if inline.inline.trim().is_empty() {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("{ctx}: inline guidance cannot be empty"),
                    ));
                }
            }
        }
    }

    for contour in &gate.contours {
        if !profile.contours.contains_key(contour) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: unknown contour '{contour}'"),
            ));
        }
    }

    for tool in profile.effective_tools(gate) {
        validate_tool_name(tool, &profile.modules, opts.acdd_modules, providers, &ctx)?;
    }
    for tool in &gate.tools_removed_for_validation {
        validate_tool_name(tool, &profile.modules, opts.acdd_modules, providers, &ctx)?;
    }

    validate_gate_proof(profile, gate, &ctx)?;
    Ok(())
}

fn reject_legacy_lens_or_scope(raw: &str, ctx: &str, field: &str) -> Result<(), AcddError> {
    match raw {
        "docs.instructions" => Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: legacy '{raw}' in {field} is no longer supported"),
        )
        .with_hint("Replace 'docs.instructions' with 'paths.instructions' and declare 'paths.instructions' (or inherit 'base')")),
        "docs.adrs" => Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: legacy '{raw}' in {field} is no longer supported"),
        )
        .with_hint("Replace 'docs.adrs' with 'paths.adrs' and declare 'paths.adrs' (or inherit 'base')")),
        _ => Ok(()),
    }
}

fn validate_named_spec_ref<'a>(
    raw: &str,
    effective_specs: &'a BTreeMap<String, SpecConfig>,
    ctx: &str,
    field: &str,
) -> Result<Vec<(&'a String, &'a SpecConfig)>, AcddError> {
    let name = raw.strip_prefix("specs.").unwrap_or("");
    if name.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "{ctx}: {field} entry '{raw}' must name a registered spec (e.g. 'specs.memories')"
            ),
        ));
    }
    let matched = acdd_spec::matching_spec_collections(effective_specs, name);
    if matched.is_empty() {
        let available: Vec<&str> = effective_specs.keys().map(String::as_str).collect();
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: unknown spec reference '{raw}' in {field}"),
        )
        .with_hint(format!(
            "Register 'specs.{name}' in acdd.yaml (registered specs: {available:?})"
        )));
    }
    Ok(matched)
}

fn validate_named_path_ref<'a>(
    raw: &str,
    profile: &'a ResolvedWorkflow,
    ctx: &str,
    field: &str,
) -> Result<&'a [String], AcddError> {
    let name = raw.strip_prefix("paths.").unwrap_or("");
    if name.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "{ctx}: {field} entry '{raw}' must name a declared path group (e.g. 'paths.docs')"
            ),
        ));
    }
    profile.paths.get(name).map(Vec::as_slice).ok_or_else(|| {
        let available: Vec<&str> = profile.paths.keys().map(String::as_str).collect();
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: unknown path reference '{raw}' in {field}"),
        )
        .with_hint(format!(
            "Declare 'paths.{name}: [...]' in the workflow (declared paths: {available:?})"
        ))
    })
}

fn validate_gate_context_entry(
    entry: &ContextEntry,
    profile: &ResolvedWorkflow,
    opts: &WorkflowValidationOptions<'_>,
    ctx: &str,
) -> Result<(), AcddError> {
    let lens = entry.as_str();
    reject_legacy_lens_or_scope(lens, ctx, "context:")?;
    let empty_providers = CompositeDomainProvider::default();
    let providers = opts.domain_providers.unwrap_or(&empty_providers);
    let empty_specs = BTreeMap::new();
    let effective_specs =
        acdd_spec::effective_spec_collections(opts.spec_collections.unwrap_or(&empty_specs));

    let ctx_ref = ContextRef::parse(lens)?;
    match &ctx_ref {
        ContextRef::Paths(_) => {
            let _ = validate_named_path_ref(lens, profile, ctx, "context:")?;
            if let Some(col_domain) = providers.collection_context_domain() {
                ensure_domain_enabled(col_domain, &profile.modules, opts.acdd_modules, ctx)?;
            }
            providers.validate_context_ref(&ctx_ref, &entry.with)
        }
        ContextRef::Specs(_) => {
            let _ = validate_named_spec_ref(lens, &effective_specs, ctx, "context:")?;
            if let Some(col_domain) = providers.collection_context_domain() {
                ensure_domain_enabled(col_domain, &profile.modules, opts.acdd_modules, ctx)?;
            }
            providers.validate_context_ref(&ctx_ref, &entry.with)
        }
        ContextRef::Domain { domain, .. } => {
            ensure_domain_enabled(domain, &profile.modules, opts.acdd_modules, ctx)?;
            providers.validate_context_ref(&ctx_ref, &entry.with)
        }
    }
}

fn validate_gate_scope(
    gate: &GateDef,
    profile: &ResolvedWorkflow,
    opts: &WorkflowValidationOptions<'_>,
    ctx: &str,
) -> Result<(), AcddError> {
    if gate.scope.is_empty() {
        return Ok(());
    }
    let empty_specs = BTreeMap::new();
    let effective_specs =
        acdd_spec::effective_spec_collections(opts.spec_collections.unwrap_or(&empty_specs));
    for pat in &gate.scope {
        reject_legacy_lens_or_scope(pat, ctx, "scope:")?;
        if pat.starts_with("specs.") {
            let cols = validate_named_spec_ref(pat, &effective_specs, ctx, "scope:")?;
            let patterns: Vec<&str> = cols
                .into_iter()
                .flat_map(|(_, col)| col.patterns())
                .collect();
            if patterns.is_empty() || patterns.iter().any(|p| !is_valid_scope_glob(p)) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: spec reference '{pat}' in scope: resolves to empty or invalid globs {patterns:?}"),
                ));
            }
        } else if pat.starts_with("paths.") {
            let globs = validate_named_path_ref(pat, profile, ctx, "scope:")?;
            if globs.is_empty() || globs.iter().any(|g| !is_valid_scope_glob(g)) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: path reference '{pat}' in scope: resolves to empty or invalid globs {globs:?}"),
                ));
            }
        } else if !is_valid_scope_glob(pat) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: scope contains invalid or non-normalized scope glob '{pat}'"),
            ));
        }
    }
    Ok(())
}

fn collect_referenced_spec_lenses(
    gates: &[GateDef],
    spec_collections: &BTreeMap<String, SpecConfig>,
    out: &mut BTreeMap<String, Vec<String>>,
) {
    for gate in gates {
        for entry in gate
            .scope
            .iter()
            .map(String::as_str)
            .chain(gate.context.iter().map(ContextEntry::as_str))
        {
            if let Some(name) = entry.strip_prefix("specs.") {
                let cols = acdd_spec::matching_spec_collections(spec_collections, name);
                if !cols.is_empty() {
                    out.entry(entry.to_owned()).or_insert_with(|| {
                        cols.into_iter()
                            .flat_map(|(_, col)| col.patterns().into_iter().map(str::to_owned))
                            .collect()
                    });
                }
            }
        }
        if !gate.gates.is_empty() {
            collect_referenced_spec_lenses(&gate.gates, spec_collections, out);
        }
        if let Some(ref tb) = gate.tasks {
            collect_referenced_spec_lenses(&tb.gates, spec_collections, out);
        }
    }
}

/// Emit structured validation warnings for any declared `paths.<name>` or referenced `specs.<name>`
/// whose globs are empty or match zero non-symlink regular files under `root`.
pub fn collect_workflow_glob_warnings(
    root: &Path,
    profile_path: &str,
    profile: &ResolvedWorkflow,
    spec_collections: &BTreeMap<String, SpecConfig>,
    warnings: &mut Vec<Value>,
) {
    let mut checked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, globs) in &profile.paths {
        checked.insert(format!("paths.{name}"), globs.clone());
    }
    collect_referenced_spec_lenses(&profile.gates, spec_collections, &mut checked);
    if checked.is_empty() {
        return;
    }

    let mut pending: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (lens, globs) in checked {
        if globs.is_empty() {
            warnings.push(json!({
                "code": "EMPTY_GLOB_MATCH",
                "path": profile_path,
                "lens": lens,
                "globs": globs,
                "message": format!("workflow lens '{lens}' has an empty glob list and matches 0 files"),
            }));
        } else {
            pending.insert(lens, globs);
        }
    }
    if pending.is_empty() {
        return;
    }

    let policy = WorkspacePathPolicy::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if pending.is_empty() {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = entries.flatten().collect();
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if ft.is_dir() {
                if should_skip_walk_dir(dir.file_name().and_then(|p| p.to_str()), name_str.as_ref())
                {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                let Ok(rel_raw) = path.strip_prefix(root) else {
                    continue;
                };
                let Some(rel) = normalize_rel_path(&rel_raw.to_string_lossy()) else {
                    continue;
                };
                pending.retain(|_, globs| !policy.is_scope_admitted(&rel, globs));
                if pending.is_empty() {
                    break;
                }
            }
        }
    }

    for (lens, globs) in pending {
        warnings.push(json!({
            "code": "EMPTY_GLOB_MATCH",
            "path": profile_path,
            "lens": lens,
            "globs": globs,
            "message": format!("workflow lens '{lens}' globs {globs:?} matched 0 files"),
        }));
    }
}

fn validate_report_path_template(
    ctx: &str,
    raw: &str,
    protected_paths: &[String],
) -> Result<(), AcddError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed != raw {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: 'save.report' must be a non-empty normalized workspace-relative path"),
        ));
    }
    let mut substituted = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            let mut token = String::new();
            let mut closed = false;
            for next in chars.by_ref() {
                if next == '}' {
                    closed = true;
                    break;
                }
                if next == '{' {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("{ctx}: malformed placeholder in 'save.report' ('{raw}')"),
                    ));
                }
                token.push(next);
            }
            if !closed
                || !matches!(
                    token.as_str(),
                    "run_id" | "run_name" | "gate_id" | "spec_id"
                )
            {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "{ctx}: unsupported placeholder '{{{token}}}' in 'save.report'; expected {{run_id}}, {{run_name}}, {{gate_id}}, or {{spec_id}}"
                    ),
                ));
            }
            substituted.push('x');
        } else if ch == '}' {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("{ctx}: unmatched '}}' in 'save.report' ('{raw}')"),
            ));
        } else {
            substituted.push(ch);
        }
    }
    if substituted.contains('\\')
        || !acdd_core::fs::is_normalized_relative(std::path::Path::new(&substituted))
    {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "{ctx}: 'save.report' must be a normalized workspace-relative path, got '{raw}'"
            ),
        ));
    }
    let default_policy = acdd_core::path_policy::WorkspacePathPolicy::default();
    if !default_policy.is_valid_report_path(&substituted, protected_paths) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "{ctx}: 'save.report' path '{raw}' must resolve under .acdd/reports/*.{{md,json}} without violating protected_paths"
            ),
        ));
    }
    Ok(())
}

fn validate_gate_selector(
    sel: &GateSelector,
    self_id: &str,
    scope_kind: GateScopeKind,
    sibling_ids: &BTreeSet<String>,
    task_gate_ids: &BTreeSet<String>,
    field: &str,
) -> Result<(), AcddError> {
    match sel {
        GateSelector::Local(parts) => {
            if parts.len() == 1 && parts[0].as_str() == self_id {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{self_id}': '{field}' cannot reference itself"),
                ));
            }
            let target = parts[0].as_str();
            if !sibling_ids.contains(target) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate '{self_id}': '{field}' references unknown gate '{sel}'"),
                ));
            }
        }
        GateSelector::AllTasksGate(parts) => {
            if scope_kind != GateScopeKind::Root {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "gate '{self_id}': '{field}' selector '{sel}' is only valid on root gates"
                    ),
                ));
            }
            let leaf = parts.last().map(|g| g.as_str()).unwrap_or("");
            if !task_gate_ids.contains(leaf) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "gate '{self_id}': '{field}' selector '{sel}' does not match any task gate"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_gate_proof(
    profile: &ResolvedWorkflow,
    gate: &GateDef,
    ctx: &str,
) -> Result<(), AcddError> {
    match &gate.proof {
        ProofDef::Kind(ProofKind::None) => {}
        ProofDef::Kind(ProofKind::Command) => {
            if gate.command.is_none() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: 'proof: command' requires 'command'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Call) => {
            if gate.call.is_none() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: 'proof: call' requires 'call'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Review) => {
            if gate.command.is_some() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: 'proof: review' cannot be combined with 'command'"),
                ));
            }
            if gate.role.is_none() || gate.contours.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: 'proof: review' requires 'role' and non-empty 'contours'"),
                ));
            }
        }
        ProofDef::Named(name) | ProofDef::ResolvedNamed { name, .. } => {
            let Some(catalog_entry) = profile.proofs.get(name) else {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: unknown proof catalog '{name}'"),
                ));
            };
            catalog_entry
                .schema
                .validate_schema(&format!("{ctx}.proof.{name}"))?;
            if gate.call.is_none() && gate.role.is_none() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: catalog proof '{name}' requires 'call' or 'role'"),
                ));
            }
        }
        ProofDef::Scheme { scheme } => {
            scheme.validate_schema(&format!("{ctx}.proof.scheme"))?;
            if gate.call.is_none() && gate.role.is_none() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("{ctx}: inline scheme proof requires 'call' or 'role'"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_tool_name(
    tool: &str,
    profile_modules: &SubsystemRegistry,
    acdd_modules: Option<&SubsystemRegistry>,
    providers: &CompositeDomainProvider,
    ctx: &str,
) -> Result<(), AcddError> {
    if LEGACY_TOOL_NAMES.contains(&tool)
        || tool.starts_with("task_")
        || tool.starts_with("milestone_")
    {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: legacy 0.5 tool '{tool}' is not permitted in v2 profiles"),
        ));
    }
    if is_builtin_mcp_tool(tool) {
        if let Some(subsystem) = SubsystemRegistry::subsystem_for_tool(tool) {
            if !is_subsystem_effectively_enabled(subsystem, profile_modules, acdd_modules) {
                return Err(AcddError::new(
                    ErrorCode::DomainCapabilityUnavailable,
                    format!(
                        "{ctx}: built-in tool '{tool}' is unavailable because subsystem '{subsystem}' is disabled"
                    ),
                ));
            }
        }
        return Ok(());
    }
    if !tool.contains('_') {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{ctx}: unknown tool '{tool}'"),
        ));
    }
    let domain = tool.split('_').next().unwrap_or("");
    ensure_domain_enabled(domain, profile_modules, acdd_modules, ctx)?;
    providers.validate_plugin_tool_name(tool)
}

fn is_subsystem_effectively_enabled(
    subsystem: &str,
    profile_modules: &SubsystemRegistry,
    acdd_modules: Option<&SubsystemRegistry>,
) -> bool {
    let profile_ok = profile_modules.is_enabled(subsystem);
    let acdd_ok = acdd_modules.is_none_or(|m| m.is_enabled(subsystem));
    profile_ok && acdd_ok
}

fn ensure_domain_enabled(
    domain: &str,
    profile_modules: &SubsystemRegistry,
    acdd_modules: Option<&SubsystemRegistry>,
    ctx: &str,
) -> Result<(), AcddError> {
    if matches!(domain, "code" | "docs" | "spec") {
        if !is_subsystem_effectively_enabled(domain, profile_modules, acdd_modules) {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("{ctx}: domain '{domain}' is disabled in modules configuration"),
            ));
        }
        return Ok(());
    }
    if profile_modules.domains.get(domain) == Some(&false)
        || acdd_modules.is_some_and(|m| m.domains.get(domain) == Some(&false))
    {
        return Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("{ctx}: external domain '{domain}' is disabled in modules configuration"),
        ));
    }
    Ok(())
}

/// Resolve a `CommandDef` into deduplicated invocations for `scope_paths`.
///
/// For `CommandDef::Pattern`, scope paths are sorted lexicographically, each path selects the
/// highest-specificity matching glob (breaking ties by YAML declaration order), and invocations
/// are deduplicated in first-selected order.
pub fn resolve_command_for_scope(
    cmd: &CommandDef,
    scope_paths: &[String],
) -> Result<Vec<ResolvedCommandInvocation>, AcddError> {
    match cmd {
        CommandDef::Unbound { .. } => Err(AcddError::new(
            ErrorCode::CommandUnbound,
            "cannot resolve unbound command slot",
        )
        .with_hint("Override the command slot in .acdd/workflows/default.yaml")),
        CommandDef::Script(script) => Ok(vec![ResolvedCommandInvocation {
            command: script.clone(),
            test_filter: None,
            timeout_seconds: None,
            scope_args_by_pattern: BTreeMap::new(),
            matched_paths: Vec::new(),
            post_always: false,
        }]),
        CommandDef::Detailed(d) => Ok(vec![ResolvedCommandInvocation {
            command: d.command.clone(),
            test_filter: d.test_filter.clone(),
            timeout_seconds: d.timeout_seconds,
            scope_args_by_pattern: d.scope_args_by_pattern.clone(),
            matched_paths: Vec::new(),
            post_always: false,
        }]),
        CommandDef::Pattern(entries) => {
            if entries.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    "pattern command has zero entries",
                ));
            }
            let mut sorted_scope: Vec<String> = scope_paths.to_vec();
            sorted_scope.sort();
            sorted_scope.dedup();

            let mut selected_order: Vec<usize> = Vec::new();
            let mut matched_by_idx: BTreeMap<usize, Vec<String>> = BTreeMap::new();

            for path in &sorted_scope {
                let mut best: Option<((usize, usize, usize, usize), usize)> = None;
                for (idx, entry) in entries.iter().enumerate() {
                    if entry.post_always {
                        continue;
                    }
                    if acdd_core::path_policy::glob_matches(&entry.pattern, path) {
                        let spec = glob_specificity(&entry.pattern);
                        let better = match best {
                            None => true,
                            Some((best_spec, best_idx)) => {
                                spec > best_spec || (spec == best_spec && idx < best_idx)
                            }
                        };
                        if better {
                            best = Some((spec, idx));
                        }
                    }
                }
                if let Some((_, win_idx)) = best {
                    matched_by_idx
                        .entry(win_idx)
                        .or_insert_with(|| {
                            selected_order.push(win_idx);
                            Vec::new()
                        })
                        .push(path.clone());
                }
            }

            let mut out: Vec<ResolvedCommandInvocation> = selected_order
                .into_iter()
                .map(|idx| {
                    let entry = &entries[idx];
                    ResolvedCommandInvocation {
                        command: entry.command.clone(),
                        test_filter: entry.test_filter.clone(),
                        timeout_seconds: entry.timeout_seconds,
                        scope_args_by_pattern: entry.scope_args_by_pattern.clone(),
                        matched_paths: matched_by_idx.remove(&idx).unwrap_or_default(),
                        post_always: false,
                    }
                })
                .collect();
            if !out.is_empty() {
                for entry in entries.iter().filter(|e| e.post_always) {
                    out.push(ResolvedCommandInvocation {
                        command: entry.command.clone(),
                        test_filter: None,
                        timeout_seconds: entry.timeout_seconds,
                        scope_args_by_pattern: BTreeMap::new(),
                        matched_paths: Vec::new(),
                        post_always: true,
                    });
                }
            }
            Ok(out)
        }
    }
}

pub use super::shell_template::{check_guidance_budget, resolve_and_budget_guidance};
