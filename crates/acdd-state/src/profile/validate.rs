//! Fail-closed `GateProfile` validation, command pattern selection, and guidance budgeting.

use super::schema::{
    CommandDef, DetailedCommandDef, GateDef, GateGuidanceRef, ProofDef, ProofKind,
    ResolvedGateProfile, ResourceLimits, TestFilterConfig,
};
use super::shell_template::validate_shell_template_placeholders;
use super::when::WhenExpr;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, GateSelector};
use acdd_core::ports::{is_builtin_mcp_tool, CompositeDomainProvider};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

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

/// Options controlling fail-closed `validate_profile` execution.
#[derive(Default)]
pub struct ProfileValidationOptions<'a> {
    /// Registered host and external `DomainProvider`s (`None` validates against zero providers).
    pub domain_providers: Option<&'a CompositeDomainProvider>,
    /// Workspace `acdd.yaml` `modules:` override combined with `profile.modules`.
    pub acdd_modules: Option<&'a SubsystemRegistry>,
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
}

/// Validate a `ResolvedGateProfile` fail-closed against `DomainProvider` capabilities, `modules:`, and all schema invariants.
pub fn validate_profile(
    profile: &ResolvedGateProfile,
    opts: &ProfileValidationOptions<'_>,
) -> Result<(), StateError> {
    if profile.version != 2 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid profile version {}; expected 2", profile.version),
        ));
    }

    profile.modules.validate().map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid profile.modules: {err}"),
        )
    })?;
    if let Some(acdd_mods) = opts.acdd_modules {
        acdd_mods.validate().map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid workspace modules: {err}"),
            )
        })?;
    }

    for req in &profile.modules.required {
        if !is_subsystem_effectively_enabled(req, &profile.modules, opts.acdd_modules) {
            return Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("required subsystem '{req}' is disabled in effective modules configuration"),
            ));
        }
    }

    validate_limits(&profile.limits)?;
    validate_protected_paths(profile)?;
    validate_proof_catalog(profile)?;
    validate_roles(profile)?;
    validate_commands(profile)?;

    let empty_providers = CompositeDomainProvider::default();
    let providers = opts.domain_providers.unwrap_or(&empty_providers);

    for tool in &profile.default_tools {
        validate_tool_name(tool, &profile.modules, opts.acdd_modules, providers, "default_tools")?;
    }

    let task_gate_ids = collect_all_task_gate_ids(&profile.gates);
    validate_gate_container(
        profile,
        &profile.gates,
        GateScopeKind::Root,
        &task_gate_ids,
        providers,
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
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "terminal root_gate '{}' must not declare 'pass:'",
                    last_root_gate.id
                ),
            ));
        }
    }

    Ok(())
}

fn validate_limits(limits: &ResourceLimits) -> Result<(), StateError> {
    if limits.max_guidance_bytes == 0
        || limits.max_log_bytes == 0
        || limits.max_capture_bytes == 0
        || limits.max_line_bytes == 0
        || limits.diagnostic_inline_bytes == 0
        || limits.max_metadata_bytes == 0
        || limits.max_trace_summary_bytes == 0
        || limits.max_command_timeout_seconds == 0
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "all ResourceLimits byte and timeout bounds must be > 0",
        ));
    }
    Ok(())
}

fn validate_protected_paths(profile: &ResolvedGateProfile) -> Result<(), StateError> {
    for pat in &profile.protected_paths {
        if !acdd_core::path_policy::is_valid_scope_glob(pat) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("protected_paths contains invalid or non-normalized scope glob '{pat}'"),
            ));
        }
    }
    Ok(())
}

fn validate_proof_catalog(profile: &ResolvedGateProfile) -> Result<(), StateError> {
    for (name, def) in &profile.proofs {
        if matches!(
            name.as_str(),
            "none" | "contract" | "command" | "call" | "review" | "seam_test" | "delivery"
        ) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("proof catalog entry '{name}' uses a reserved or legacy proof name"),
            ));
        }
        def.schema.validate_schema(&format!("proofs.{name}.schema"))?;
    }
    Ok(())
}

fn validate_roles(profile: &ResolvedGateProfile) -> Result<(), StateError> {
    for (name, role) in &profile.roles {
        if name.trim().is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "role name cannot be empty",
            ));
        }
        if let Some(runner) = &role.runner {
            if runner.trim().is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    for (pat, arg) in scope_args {
        validate_shell_template_placeholders(
            arg,
            &["{file_stem}", "{path}"],
            &format!("{ctx}[{pat}]"),
        )?;
    }
    Ok(())
}

fn validate_commands(profile: &ResolvedGateProfile) -> Result<(), StateError> {
    validate_scope_args_map(&profile.scope_args_by_pattern, "scope_args_by_pattern")?;
    for (name, cmd) in &profile.commands {
        if name.trim().is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "command slot name cannot be empty",
            ));
        }
        match cmd {
            CommandDef::Unbound { unbound, .. } => {
                if !unbound {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
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
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("commands.{name}: pattern command list cannot be empty"),
                    ));
                }
                for (idx, entry) in entries.iter().enumerate() {
                    let path = format!("commands.{name}[{idx}]");
                    if entry.pattern.trim().is_empty() {
                        return Err(StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!("{path}.pattern cannot be empty"),
                        ));
                    }
                    if entry.timeout_seconds == Some(0) {
                        return Err(StateError::new(
                            ErrorCode::ProfileSchemaInvalid,
                            format!("{path}.timeout_seconds must be > 0"),
                        ));
                    }
                    validate_command_string(
                        &entry.command,
                        entry.test_filter.as_ref(),
                        &path,
                    )?;
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

fn validate_detailed_command(d: &DetailedCommandDef, ctx: &str) -> Result<(), StateError> {
    if d.timeout_seconds == Some(0) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    if command.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}.command cannot be empty"),
        ));
    }
    validate_shell_template_placeholders(command, &["{scope_target}", "{test_filter}"], ctx)?;
    let placeholder_count = command.matches("{test_filter}").count();
    if placeholder_count > 1 {
        return Err(StateError::new(
            ErrorCode::CommandTestFilterUnsupported,
            format!("{ctx}: command template may contain at most one '{{test_filter}}' placeholder (found {placeholder_count})"),
        ));
    }
    if let Some(TestFilterConfig::Template(tpl)) = test_filter {
        let count = tpl.matches("{test}").count();
        if tpl.trim().is_empty() || count != 1 {
            return Err(StateError::new(
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
    profile: &ResolvedGateProfile,
    gates: &[GateDef],
    scope_kind: GateScopeKind,
    task_gate_ids: &BTreeSet<String>,
    providers: &CompositeDomainProvider,
    opts: &ProfileValidationOptions<'_>,
) -> Result<(), StateError> {
    let mut sibling_ids = BTreeSet::new();
    for gate in gates {
        if gate.is_tasks_block() {
            continue;
        }
        if gate.id.is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "gate id cannot be empty outside '- tasks:' blocks",
            ));
        }
        GateId::new(&gate.id)?;
        if !sibling_ids.insert(gate.id.clone()) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("duplicate gate id '{}' in container", gate.id),
            ));
        }
    }

    for gate in gates {
        if let Some(tb) = &gate.tasks {
            if scope_kind != GateScopeKind::Root {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "'- tasks:' block is only permitted at top level of 'gates:'",
                ));
            }
            if !gate.id.is_empty() {
                GateId::new(&gate.id)?;
                if !sibling_ids.insert(gate.id.clone()) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("duplicate gate id '{}' on tasks block", gate.id),
                    ));
                }
            }
            if let Some(when_str) = tb.when.as_deref().or(gate.when.as_deref()) {
                let expr = WhenExpr::parse(when_str)?;
                expr.validate_tags(&profile.tags, "tasks.when")?;
            }
            if tb.gates.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "'- tasks:' block must declare at least one task gate",
                ));
            }
            if gate.save.archive || gate.save.prune {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "archive/prune are only allowed on leaf root_gate nodes",
                ));
            }
            validate_gate_container(
                profile,
                &tb.gates,
                GateScopeKind::TaskGate,
                task_gate_ids,
                providers,
                opts,
            )?;
            continue;
        }

        let is_container = !gate.gates.is_empty();
        if scope_kind != GateScopeKind::TaskGate && gate.when.is_some() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
            validate_gate_container(
                profile,
                &gate.gates,
                child_kind,
                task_gate_ids,
                providers,
                opts,
            )?;
        }

        validate_single_gate(
            profile,
            gate,
            scope_kind,
            is_container,
            &sibling_ids,
            task_gate_ids,
            providers,
            opts,
        )?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_single_gate(
    profile: &ResolvedGateProfile,
    gate: &GateDef,
    scope_kind: GateScopeKind,
    is_container: bool,
    sibling_ids: &BTreeSet<String>,
    task_gate_ids: &BTreeSet<String>,
    providers: &CompositeDomainProvider,
    opts: &ProfileValidationOptions<'_>,
) -> Result<(), StateError> {
    let ctx = format!("gate '{}'", gate.id);

    if gate.max_attempts < 1 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: max_attempts must be >= 1"),
        ));
    }

    if (gate.save.archive || gate.save.prune) && (scope_kind != GateScopeKind::Root || is_container)
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: 'save.archive' and 'save.prune' are only permitted on root_gate"),
        ));
    }
    if (gate.save.archive || gate.save.prune) && gate.pass.is_some() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: terminal root_gate with archive/prune must not declare 'pass:'"),
        ));
    }
    if let Some(ref tpl) = gate.save.report {
        validate_report_path_template(&ctx, tpl, &profile.protected_paths)?;
    }

    if let Some(pass_target) = &gate.pass {
        if pass_target.as_str() == gate.id {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: 'pass:' cannot point to itself"),
            ));
        }
        if !sibling_ids.contains(pass_target.as_str()) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: 'pass:' references unknown sibling gate '{pass_target}'"),
            ));
        }
    }
    for (label, target_opt) in [("fail", &gate.fail), ("fail_drift", &gate.fail_drift)] {
        if let Some(fail_target) = target_opt {
            if fail_target.as_str() == gate.id {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: '{label}:' cannot point to itself"),
                ));
            }
            if !sibling_ids.contains(fail_target.as_str()) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: '{label}:' references unknown sibling gate '{fail_target}'"),
                ));
            }
        }
        if gate.pass.is_some() && gate.pass.as_ref() == target_opt.as_ref() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: 'pass:' and '{label}:' cannot target the same gate"),
            ));
        }
    }

    if gate.receipt && gate.need.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: 'receipt: true' requires non-empty 'need:'"),
        ));
    }

    for sel in &gate.need {
        validate_gate_selector(sel, &gate.id, scope_kind, sibling_ids, task_gate_ids, "need")?;
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
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: unknown role '{role_name}'"),
            ));
        }
    }

    if let Some(cmd_name) = &gate.command {
        let cmd = profile.commands.get(cmd_name).ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: unknown command '{cmd_name}'"),
            )
        })?;
        if opts.require_bound_commands && matches!(cmd, CommandDef::Unbound { .. }) {
            return Err(StateError::new(
                ErrorCode::CommandUnbound,
                format!("{ctx}: command slot '{cmd_name}' is unbound"),
            )
            .with_hint(format!(
                "Override commands.{cmd_name} in .acdd/profile.yaml before starting or running a workflow"
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

    for lens in &gate.context {
        let domain = lens.split('.').next().unwrap_or("");
        ensure_domain_enabled(domain, &profile.modules, opts.acdd_modules, &ctx)?;
        providers.validate_context_lens(lens)?;
    }

    for g_ref in &gate.guidance {
        match g_ref {
            GateGuidanceRef::Slot(slot) => {
                if !profile.guidance.contains_key(slot) {
                    return Err(StateError::new(
                        ErrorCode::UnknownGuidanceSlot,
                        format!("{ctx}: unknown guidance slot '{slot}'"),
                    ));
                }
            }
            GateGuidanceRef::Inline(inline) => {
                if inline.inline.trim().is_empty() {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("{ctx}: inline guidance cannot be empty"),
                    ));
                }
            }
        }
    }

    for contour in &gate.contours {
        if !profile.contours.contains_key(contour) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
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

fn validate_report_path_template(
    ctx: &str,
    raw: &str,
    protected_paths: &[String],
) -> Result<(), StateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed != raw {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "{ctx}: unsupported placeholder '{{{token}}}' in 'save.report'; expected {{run_id}}, {{run_name}}, {{gate_id}}, or {{spec_id}}"
                    ),
                ));
            }
            substituted.push('x');
        } else if ch == '}' {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: unmatched '}}' in 'save.report' ('{raw}')"),
            ));
        } else {
            substituted.push(ch);
        }
    }
    if substituted.contains('\\')
        || !acdd_core::fs::is_normalized_relative(std::path::Path::new(&substituted))
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "{ctx}: 'save.report' must be a normalized workspace-relative path, got '{raw}'"
            ),
        ));
    }
    let default_policy = acdd_core::path_policy::WorkspacePathPolicy::default();
    if !default_policy.is_valid_report_path(&substituted, protected_paths) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    match sel {
        GateSelector::Local(parts) => {
            if parts.len() == 1 && parts[0].as_str() == self_id {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("gate '{self_id}': '{field}' cannot reference itself"),
                ));
            }
            let target = parts[0].as_str();
            if !sibling_ids.contains(target) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("gate '{self_id}': '{field}' references unknown gate '{sel}'"),
                ));
            }
        }
        GateSelector::AllTasksGate(parts) => {
            if scope_kind != GateScopeKind::Root {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("gate '{self_id}': '{field}' selector '{sel}' is only valid on root gates"),
                ));
            }
            let leaf = parts.last().map(|g| g.as_str()).unwrap_or("");
            if !task_gate_ids.contains(leaf) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
    profile: &ResolvedGateProfile,
    gate: &GateDef,
    ctx: &str,
) -> Result<(), StateError> {
    match &gate.proof {
        ProofDef::Kind(ProofKind::None) => {}
        ProofDef::Kind(ProofKind::Command) => {
            if gate.command.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: command' requires 'command'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Call) => {
            if gate.call.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: call' requires 'call'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Review) => {
            if gate.command.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: review' cannot be combined with 'command'"),
                ));
            }
            if gate.role.is_none() || gate.contours.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: review' requires 'role' and non-empty 'contours'"),
                ));
            }
        }
        ProofDef::Named(name) | ProofDef::ResolvedNamed { name, .. } => {
            let Some(catalog_entry) = profile.proofs.get(name) else {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: unknown proof catalog '{name}'"),
                ));
            };
            catalog_entry
                .schema
                .validate_schema(&format!("{ctx}.proof.{name}"))?;
            if gate.call.is_none() && gate.role.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: catalog proof '{name}' requires 'call' or 'role'"),
                ));
            }
        }
        ProofDef::Scheme { scheme } => {
            scheme.validate_schema(&format!("{ctx}.proof.scheme"))?;
            if gate.call.is_none() && gate.role.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    if LEGACY_TOOL_NAMES.contains(&tool)
        || tool.starts_with("task_")
        || tool.starts_with("milestone_")
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: legacy 0.5 tool '{tool}' is not permitted in v2 profiles"),
        ));
    }
    if is_builtin_mcp_tool(tool) {
        if let Some(subsystem) = SubsystemRegistry::subsystem_for_tool(tool) {
            if !is_subsystem_effectively_enabled(subsystem, profile_modules, acdd_modules) {
                return Err(StateError::new(
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
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    if matches!(domain, "code" | "docs") {
        if !is_subsystem_effectively_enabled(domain, profile_modules, acdd_modules) {
            return Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("{ctx}: domain '{domain}' is disabled in modules configuration"),
            ));
        }
        return Ok(());
    }
    if profile_modules.domains.get(domain) == Some(&false)
        || acdd_modules.is_some_and(|m| m.domains.get(domain) == Some(&false))
    {
        return Err(StateError::new(
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
) -> Result<Vec<ResolvedCommandInvocation>, StateError> {
    match cmd {
        CommandDef::Unbound { .. } => Err(StateError::new(
            ErrorCode::CommandUnbound,
            "cannot resolve unbound command slot",
        )
        .with_hint("Override the command slot in .acdd/profile.yaml")),
        CommandDef::Script(script) => Ok(vec![ResolvedCommandInvocation {
            command: script.clone(),
            test_filter: None,
            timeout_seconds: None,
            scope_args_by_pattern: BTreeMap::new(),
            matched_paths: Vec::new(),
        }]),
        CommandDef::Detailed(d) => Ok(vec![ResolvedCommandInvocation {
            command: d.command.clone(),
            test_filter: d.test_filter.clone(),
            timeout_seconds: d.timeout_seconds,
            scope_args_by_pattern: d.scope_args_by_pattern.clone(),
            matched_paths: Vec::new(),
        }]),
        CommandDef::Pattern(entries) => {
            if entries.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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

            let out = selected_order
                .into_iter()
                .map(|idx| {
                    let entry = &entries[idx];
                    ResolvedCommandInvocation {
                        command: entry.command.clone(),
                        test_filter: entry.test_filter.clone(),
                        timeout_seconds: entry.timeout_seconds,
                        scope_args_by_pattern: entry.scope_args_by_pattern.clone(),
                        matched_paths: matched_by_idx.remove(&idx).unwrap_or_default(),
                    }
                })
                .collect();
            Ok(out)
        }
    }
}

/// Glob specificity ordering: `(literal_segments, single_wildcard_segments, !globstar_count, literal_len)`.
pub fn glob_specificity(pattern: &str) -> (usize, usize, usize, usize) {
    let norm = pattern.trim().replace('\\', "/");
    let mut literal_segs = 0usize;
    let mut single_wild_segs = 0usize;
    let mut globstar_segs = 0usize;
    let mut literal_chars = 0usize;
    for seg in norm.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if seg == "**" {
            globstar_segs += 1;
        } else if seg.contains('*') || seg.contains('?') {
            single_wild_segs += 1;
        } else {
            literal_segs += 1;
            literal_chars += seg.len();
        }
    }
    (
        literal_segs,
        single_wild_segs,
        usize::MAX - globstar_segs,
        literal_chars,
    )
}

/// Resolve all `guidance` items on a `GateDef` against `profile.guidance`, enforcing the mandatory budget cap.
pub fn resolve_and_budget_guidance(
    profile: &ResolvedGateProfile,
    gate: &GateDef,
    contract_bytes: usize,
    response_max_bytes: usize,
) -> Result<Vec<String>, StateError> {
    let mut rendered = Vec::with_capacity(gate.guidance.len());
    let mut guidance_bytes = 0usize;
    for item in &gate.guidance {
        let text = match item {
            GateGuidanceRef::Slot(slot) => profile
                .guidance
                .get(slot)
                .ok_or_else(|| {
                    StateError::new(
                        ErrorCode::UnknownGuidanceSlot,
                        format!("gate '{}': unknown guidance slot '{slot}'", gate.id),
                    )
                })?
                .render(),
            GateGuidanceRef::Inline(inline) => inline.inline.clone(),
        };
        guidance_bytes += text.len();
        rendered.push(text);
    }
    check_guidance_budget(contract_bytes, guidance_bytes, &profile.limits, response_max_bytes)?;
    Ok(rendered)
}

/// Enforce the fail-closed mandatory `contract + guidance` byte reservation cap.
pub fn check_guidance_budget(
    contract_bytes: usize,
    guidance_bytes: usize,
    limits: &ResourceLimits,
    response_max_bytes: usize,
) -> Result<usize, StateError> {
    let total = contract_bytes.saturating_add(guidance_bytes);
    let cap = limits.max_guidance_bytes.min(response_max_bytes);
    if total > cap {
        return Err(StateError::new(
            ErrorCode::GuidanceBudgetExceeded,
            format!(
                "mandatory contract + guidance ({total} bytes) exceeds reserved budget ({cap} bytes)"
            ),
        )
        .with_hint("Reduce inline guidance or task spec/invariants size so mandatory instructions fit without truncation"));
    }
    Ok(cap - total)
}
