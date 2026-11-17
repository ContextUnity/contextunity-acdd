//! Fail-closed `GateProfile` validation, command pattern selection, `{test_filter}` substitution, and guidance budgeting.

use super::schema::{
    CommandDef, CommandOutputSpec, CompileErrorsSpec, DetailedCommandDef, GateDef, GateGuidanceRef,
    ProofDef, ProofKind, ResolvedGateProfile, ResourceLimits, TestFilterConfig,
};
use super::when::WhenExpr;
use crate::types::SaveAction;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{GateId, GateSelector};
use acdd_core::ports::{is_builtin_mcp_tool, CompositeDomainProvider};
use regex::Regex;
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
    /// Optional output parser specification.
    pub output: Option<CommandOutputSpec>,
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
    validate_git_commit_notes(profile)?;
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

fn validate_git_commit_notes(profile: &ResolvedGateProfile) -> Result<(), StateError> {
    for (name, note) in &profile.git_commit_notes {
        if name.trim().is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "git_commit_notes key cannot be empty",
            ));
        }
        if !note.ref_name.starts_with("refs/") || note.ref_name.trim().len() <= 5 {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "git_commit_notes.{name}.ref_name '{}' must start with 'refs/'",
                    note.ref_name
                ),
            ));
        }
        if note.max_bytes == 0 {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("git_commit_notes.{name}.max_bytes must be > 0"),
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

fn validate_commands(profile: &ResolvedGateProfile) -> Result<(), StateError> {
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
                    if let Some(output) = &entry.output {
                        validate_command_output_spec(output, &format!("{path}.output"))?;
                    }
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
    if let Some(output) = &d.output {
        validate_command_output_spec(output, &format!("{ctx}.output"))?;
    }
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
    }
    Ok(())
}

fn validate_command_output_spec(spec: &CommandOutputSpec, ctx: &str) -> Result<(), StateError> {
    let mut simple_regexes: Vec<(String, &str)> = Vec::new();
    for (idx, p) in spec.noise.iter().enumerate() {
        simple_regexes.push((format!("{ctx}.noise[{idx}]"), p.as_str()));
    }
    match &spec.compile_errors {
        CompileErrorsSpec::Patterns(pats) => {
            for (idx, p) in pats.iter().enumerate() {
                simple_regexes.push((format!("{ctx}.compile_errors[{idx}]"), p.as_str()));
            }
        }
        CompileErrorsSpec::IncludeExclude(filter) => {
            for (idx, p) in filter.include.iter().enumerate() {
                simple_regexes.push((format!("{ctx}.compile_errors.include[{idx}]"), p.as_str()));
            }
            for (idx, p) in filter.exclude.iter().enumerate() {
                simple_regexes.push((format!("{ctx}.compile_errors.exclude[{idx}]"), p.as_str()));
            }
        }
    }
    for (field, pat) in simple_regexes {
        Regex::new(pat).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid regex at {field}: {err}"),
            )
        })?;
    }

    let mut group_regexes: Vec<(String, &str)> = Vec::new();
    if let Some(counts) = &spec.counts {
        group_regexes.push((format!("{ctx}.counts.passed"), counts.passed.as_str()));
        group_regexes.push((format!("{ctx}.counts.failed"), counts.failed.as_str()));
    }
    if let Some(tests) = &spec.tests {
        group_regexes.push((format!("{ctx}.tests.passed"), tests.passed.as_str()));
        if let Some(failed) = &tests.failed {
            group_regexes.push((format!("{ctx}.tests.failed"), failed.as_str()));
        }
        if let Some(ignored) = &tests.ignored {
            group_regexes.push((format!("{ctx}.tests.ignored"), ignored.as_str()));
        }
        if let Some(target) = &tests.target {
            group_regexes.push((format!("{ctx}.tests.target"), target.as_str()));
        }
    }
    for (field, pat) in group_regexes {
        let re = Regex::new(pat).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid regex at {field}: {err}"),
            )
        })?;
        if re.captures_len() < 2 {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{field} regex requires capture group 1"),
            ));
        }
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
    if gate.save.pre == Some(SaveAction::Commit) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: 'save.pre' only allows 'none' or 'snapshot'"),
        ));
    }
    if let Some(note_key) = &gate.save.git_commit_note {
        if !profile.git_commit_notes.contains_key(note_key) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: unknown git_commit_note '{note_key}'"),
            ));
        }
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
    if let Some(fail_target) = &gate.fail {
        if fail_target.as_str() == gate.id {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: 'fail:' cannot point to itself"),
            ));
        }
        if !sibling_ids.contains(fail_target.as_str()) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("{ctx}: 'fail:' references unknown sibling gate '{fail_target}'"),
            ));
        }
    }
    if gate.pass.is_some() && gate.pass == gate.fail {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("{ctx}: 'pass:' and 'fail:' cannot target the same gate"),
        ));
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

    for tool in &gate.tools {
        validate_tool_name(tool, &profile.modules, opts.acdd_modules, providers, &ctx)?;
    }
    for tool in &gate.tools_removed_for_validation {
        validate_tool_name(tool, &profile.modules, opts.acdd_modules, providers, &ctx)?;
    }

    validate_gate_proof(profile, gate, &ctx)?;
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
        ProofDef::Kind(ProofKind::None) => {
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Contract) => {
            let Some(cmd_name) = &gate.command else {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: contract' requires 'command'"),
                ));
            };
            let Some(ef) = &gate.expected_failure else {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: contract' requires 'expected_failure'"),
                ));
            };
            if ef.exit_codes.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure.exit_codes' cannot be empty"),
                ));
            }
            if let Some(cmd) = profile.commands.get(cmd_name) {
                validate_contract_command_filter_support(cmd, ctx)?;
            }
        }
        ProofDef::Kind(ProofKind::Command) => {
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
            if gate.command.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: command' requires 'command'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Call) => {
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
            if gate.call.is_none() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'proof: call' requires 'call'"),
                ));
            }
        }
        ProofDef::Kind(ProofKind::Review) => {
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
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
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
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
            if gate.expected_failure.is_some() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("{ctx}: 'expected_failure' is only allowed when proof is 'contract'"),
                ));
            }
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

fn validate_contract_command_filter_support(
    cmd: &CommandDef,
    ctx: &str,
) -> Result<(), StateError> {
    match cmd {
        CommandDef::Unbound { .. } => Ok(()),
        CommandDef::Script(s) => check_filterable(s, None, ctx),
        CommandDef::Detailed(d) => check_filterable(&d.command, d.test_filter.as_ref(), ctx),
        CommandDef::Pattern(entries) => {
            for e in entries {
                check_filterable(&e.command, e.test_filter.as_ref(), ctx)?;
            }
            Ok(())
        }
    }
}

fn check_filterable(
    cmd: &str,
    test_filter: Option<&TestFilterConfig>,
    ctx: &str,
) -> Result<(), StateError> {
    if matches!(test_filter, Some(TestFilterConfig::Enabled(false))) {
        return Err(StateError::new(
            ErrorCode::CommandTestFilterUnsupported,
            format!("{ctx}: command disables test_filter but is used by 'proof: contract'"),
        ));
    }
    if !cmd.contains("{test_filter}") && has_unescaped_chain_operator(cmd) {
        return Err(StateError::new(
            ErrorCode::CommandTestFilterPlaceholderRequired,
            format!(
                "{ctx}: chained contract command '{cmd}' must declare '{{test_filter}}' placeholder"
            ),
        ));
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

/// POSIX single-quote escape (`'` -> `'\''`) wrapped in single quotes.
pub fn posix_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

/// Detect unescaped shell chain operators (`&&`, `||`, `;`, `|`) outside quotes.
pub fn has_unescaped_chain_operator(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b';' | b'|' => return true,
            b'&' if bytes.get(i + 1) == Some(&b'&') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Render a command template with optional `--test <test_target>` substitution.
pub fn render_command_with_test_filter(
    command_template: &str,
    test_filter: Option<&TestFilterConfig>,
    test_target: Option<&str>,
) -> Result<String, StateError> {
    let placeholder_count = command_template.matches("{test_filter}").count();
    if placeholder_count > 1 {
        return Err(StateError::new(
            ErrorCode::CommandTestFilterUnsupported,
            format!(
                "command template contains {placeholder_count} '{{test_filter}}' placeholders; at most 1 is allowed"
            ),
        ));
    }
    if let Some(TestFilterConfig::Template(tpl)) = test_filter {
        if tpl.trim().is_empty() || tpl.matches("{test}").count() != 1 {
            return Err(StateError::new(
                ErrorCode::CommandTestFilterUnsupported,
                "test_filter template must contain '{test}' exactly once",
            ));
        }
    }

    match test_target {
        None => {
            if placeholder_count == 1 {
                let replaced = command_template.replace("{test_filter}", "");
                Ok(collapse_placeholder_spaces(&replaced))
            } else {
                Ok(command_template.to_owned())
            }
        }
        Some(target) => {
            if target.trim().is_empty() {
                return Err(StateError::new(
                    ErrorCode::CommandTestFilterUnsupported,
                    "--test target cannot be empty",
                ));
            }
            if matches!(test_filter, Some(TestFilterConfig::Enabled(false))) {
                return Err(StateError::new(
                    ErrorCode::CommandTestFilterUnsupported,
                    "this command disables '--test' filtering (test_filter: false)",
                ));
            }
            let quoted = posix_single_quote(target);
            let rendered_filter = match test_filter {
                None | Some(TestFilterConfig::Enabled(true)) => quoted,
                Some(TestFilterConfig::Template(tpl)) => tpl.replace("{test}", &quoted),
                Some(TestFilterConfig::Enabled(false)) => unreachable!(),
            };
            if placeholder_count == 1 {
                Ok(command_template.replace("{test_filter}", &rendered_filter))
            } else if has_unescaped_chain_operator(command_template) {
                Err(StateError::new(
                    ErrorCode::CommandTestFilterPlaceholderRequired,
                    format!(
                        "chained command '{command_template}' requires '{{test_filter}}' when '--test' is passed"
                    ),
                ))
            } else {
                Ok(format!("{} {rendered_filter}", command_template.trim_end()))
            }
        }
    }
}

fn collapse_placeholder_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            out.push(ch);
        }
    }
    out.trim().to_owned()
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
            output: None,
            matched_paths: Vec::new(),
        }]),
        CommandDef::Detailed(d) => Ok(vec![ResolvedCommandInvocation {
            command: d.command.clone(),
            test_filter: d.test_filter.clone(),
            timeout_seconds: d.timeout_seconds,
            scope_args_by_pattern: d.scope_args_by_pattern.clone(),
            output: d.output.clone(),
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
                    if glob_pattern_matches(&entry.pattern, path) {
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
                        output: entry.output.clone(),
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

/// Match a workspace-relative path against a glob pattern (`**`, `*`, `?`, `{a,b}`, or directory prefix).
pub fn glob_pattern_matches(pattern: &str, path: &str) -> bool {
    let norm_pat = pattern.trim().replace('\\', "/");
    let norm_pat = norm_pat.strip_prefix("./").unwrap_or(&norm_pat);
    let norm_path = path.trim().replace('\\', "/");
    let norm_path = norm_path.strip_prefix("./").unwrap_or(&norm_path);

    if let Some(open) = norm_pat.find('{') {
        if let Some(close) = norm_pat[open + 1..].find('}') {
            let close = open + 1 + close;
            let prefix = &norm_pat[..open];
            let suffix = &norm_pat[close + 1..];
            return norm_pat[open + 1..close]
                .split(',')
                .any(|choice| glob_pattern_matches(&format!("{prefix}{choice}{suffix}"), norm_path));
        }
    }
    if !norm_pat.contains('*') && !norm_pat.contains('?') {
        let trimmed = norm_pat.trim_end_matches('/');
        if norm_path == trimmed {
            return true;
        }
        return norm_path.len() > trimmed.len()
            && norm_path.starts_with(trimmed)
            && norm_path.as_bytes()[trimmed.len()] == b'/';
    }
    glob_match_bytes(norm_pat.as_bytes(), norm_path.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], path: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    match pattern[0] {
        b'*' if pattern.get(1) == Some(&b'*') => {
            let rest = &pattern[2..];
            if rest.first() == Some(&b'/') && glob_match_bytes(&rest[1..], path) {
                return true;
            }
            (0..=path.len()).any(|idx| glob_match_bytes(rest, &path[idx..]))
        }
        b'*' => {
            let mut idx = 0usize;
            while idx <= path.len() {
                if glob_match_bytes(&pattern[1..], &path[idx..]) {
                    return true;
                }
                if idx < path.len() && path[idx] == b'/' {
                    break;
                }
                idx += 1;
            }
            false
        }
        b'?' => !path.is_empty() && path[0] != b'/' && glob_match_bytes(&pattern[1..], &path[1..]),
        byte => path.first() == Some(&byte) && glob_match_bytes(&pattern[1..], &path[1..]),
    }
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
