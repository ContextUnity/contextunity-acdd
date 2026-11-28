//! Workflow inheritance (`inherit`), `gates` (replace) vs `gates_patch` (patch) resolution, SHA-256 pin verification, and template expansion.

use super::schema::{
    parse_workflow, GateDef, GateTemplate, GitPolicy, GitPolicyPatch, PatchField, ProofDef,
    RawGateDef, RawGateItem, ResolvedWorkflow, ResourceLimits, RoleDef, RolePatch, SavePolicy,
    SavePolicyPatch, SubsystemRegistryPatch, TasksBlock, WorkflowDef, DEFAULT_MAX_ATTEMPTS,
};
use super::when::{normalize_optional_when, WhenExpr};
use crate::types::SaveAction;
use acdd_core::config::{
    parse_confined_ref, read_confined_ref_bytes, resolve_confined_ref_path,
    verify_confined_sha256_prefix,
};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::fs::checked_child;
use acdd_core::ids::GateId;
use acdd_core::subsystems::SubsystemRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const BASE_YAML: &str = include_str!("embedded/base.yaml");
const CODE_YAML: &str = include_str!("embedded/code.yaml");
const DOCUMENTATION_YAML: &str = include_str!("embedded/documentation.yaml");

/// Return the raw embedded YAML for a built-in core workflow (`"base"`, `"code"`, or `"documentation"`).
pub fn embedded_workflow(name: &str) -> Option<&'static str> {
    match name {
        "base" => Some(BASE_YAML),
        "code" => Some(CODE_YAML),
        "documentation" => Some(DOCUMENTATION_YAML),
        _ => None,
    }
}

/// Intermediate resolved state tracking explicit `tools:` presence across `inherit`/`patch` layers.
#[derive(Debug, Clone)]
struct WorkingWorkflow {
    resolved: ResolvedWorkflow,
    gates: Vec<WorkingGate>,
}

#[derive(Debug, Clone)]
struct WorkingGate {
    gate: GateDef,
    has_explicit_tools: bool,
    has_explicit_max_attempts: bool,
    pending_tools_add: Vec<String>,
    pending_tools_remove: Vec<String>,
    pending_template_gates_patch: Vec<RawGateItem>,
    children: Vec<WorkingGate>,
    tasks_children: Option<WorkingTasksBlock>,
}

#[derive(Debug, Clone)]
struct WorkingTasksBlock {
    when: Option<String>,
    gates: Vec<WorkingGate>,
}

impl WorkingGate {
    fn finalize(mut self, default_tools: &[String]) -> GateDef {
        if self.has_explicit_tools
            || !self.pending_tools_add.is_empty()
            || !self.pending_tools_remove.is_empty()
        {
            let mut effective_tools = if self.has_explicit_tools {
                self.gate.tools.take().unwrap_or_default()
            } else {
                default_tools.to_vec()
            };
            for tool in self.pending_tools_add {
                if !effective_tools.contains(&tool) {
                    effective_tools.push(tool);
                }
            }
            if !self.pending_tools_remove.is_empty() {
                effective_tools.retain(|t| !self.pending_tools_remove.contains(t));
                self.gate
                    .tools_removed_for_validation
                    .extend(self.pending_tools_remove);
            }
            self.gate.tools = if !self.has_explicit_tools && effective_tools == default_tools {
                None
            } else {
                Some(effective_tools)
            };
        } else {
            self.gate.tools = None;
        }
        self.gate.gates = self
            .children
            .into_iter()
            .map(|c| c.finalize(default_tools))
            .collect();
        self.gate.tasks = self.tasks_children.map(|tb| TasksBlock {
            when: tb.when,
            gates: tb
                .gates
                .into_iter()
                .map(|c| c.finalize(default_tools))
                .collect(),
        });
        self.gate
    }
}

/// Resolve a workflow YAML string using an explicit `workflows_dirs` search list for `inherit:`.
pub fn resolve_workflow_from_str_with_dirs(
    yaml: &str,
    workflow_id: &str,
    source_rel_path: Option<&str>,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
) -> Result<ResolvedWorkflow, AcddError> {
    let mut stack = Vec::new();
    if let Some(rel) = source_rel_path {
        stack.push(path_cycle_identity(workspace_root, rel)?);
    } else {
        stack.push(format!("inline:{workflow_id}"));
    }
    let working = resolve_recursive(
        yaml,
        workflow_id,
        source_rel_path,
        workspace_root,
        acdd_modules,
        workflows_dirs,
        &mut stack,
    )?;
    finalize_working_workflow(working, &BTreeSet::new())
}

/// Load and resolve a built-in or workspace workflow reference using an explicit `workflows_dirs` list.
pub fn resolve_workflow_ref_with_dirs(
    workflow_ref: &str,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
) -> Result<ResolvedWorkflow, AcddError> {
    if let Some(yaml) = embedded_workflow(workflow_ref) {
        return resolve_builtin(
            workflow_ref,
            yaml,
            workspace_root,
            acdd_modules,
            workflows_dirs,
            false,
        );
    }
    let root = workspace_root.ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("cannot resolve workflow path '{workflow_ref}' without workspace root"),
        )
    })?;
    let (raw_path, expected_pin) = parse_path_and_pin(workflow_ref)?;
    let rel_path = resolve_workflow_path_in_workspace(root, &raw_path, workflows_dirs)?;
    let bytes = read_confined_profile_bytes(root, &rel_path)?;
    if let Some(pin) = expected_pin {
        verify_sha256_prefix(&bytes, &pin, &rel_path)?;
    }
    let yaml = std::str::from_utf8(&bytes).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("workflow '{rel_path}' is not valid UTF-8: {err}"),
        )
    })?;
    resolve_workflow_from_str_with_dirs(
        yaml,
        &rel_path,
        Some(&rel_path),
        Some(root),
        acdd_modules,
        workflows_dirs,
    )
}

/// Resolve an optional explicit workflow reference, defaulting to `.acdd/workflows/default.yaml` when present
/// in `workspace_root`, or embedded `"code"` (stripping disabled domain hooks) otherwise.
pub fn resolve_optional_workflow_ref_with_dirs(
    workflow_ref: Option<&str>,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
) -> Result<ResolvedWorkflow, AcddError> {
    if let Some(explicit) = workflow_ref {
        return resolve_workflow_ref_with_dirs(
            explicit,
            workspace_root,
            acdd_modules,
            workflows_dirs,
        );
    }
    if let Some(root) = workspace_root {
        if std::fs::symlink_metadata(root.join(".acdd/workflows/default.yaml")).is_ok() {
            return resolve_workflow_ref_with_dirs(
                ".acdd/workflows/default.yaml",
                Some(root),
                acdd_modules,
                workflows_dirs,
            );
        }
    }
    resolve_builtin(
        "code",
        CODE_YAML,
        workspace_root,
        acdd_modules,
        workflows_dirs,
        true,
    )
}

/// Resolve an active run's workflow identifier during `sync --rebind-workflow`, matching `start`'s
/// disabled-domain stripping behavior when rebinding the default embedded `"code"` workflow.
pub(crate) fn resolve_workflow_for_rebind(
    workflow_id: &str,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
) -> Result<ResolvedWorkflow, AcddError> {
    if workflow_id == "code" {
        return resolve_builtin(
            "code",
            CODE_YAML,
            workspace_root,
            acdd_modules,
            workflows_dirs,
            true,
        );
    }
    resolve_workflow_ref_with_dirs(workflow_id, workspace_root, acdd_modules, workflows_dirs)
}

fn resolve_builtin(
    name: &str,
    yaml: &str,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
    strip_self_domain: bool,
) -> Result<ResolvedWorkflow, AcddError> {
    let mut stack = vec![format!("builtin:{name}")];
    let working = resolve_recursive(
        yaml,
        name,
        None,
        workspace_root,
        acdd_modules,
        workflows_dirs,
        &mut stack,
    )?;
    let disabled_domains = if strip_self_domain {
        collect_disabled_domains(name, acdd_modules, true)
    } else {
        BTreeSet::new()
    };
    finalize_working_workflow(working, &disabled_domains)
}

fn finalize_working_workflow(
    mut working: WorkingWorkflow,
    disabled_domains: &BTreeSet<String>,
) -> Result<ResolvedWorkflow, AcddError> {
    expand_templates_in_working(&mut working)?;
    strip_disabled_domain_capabilities(&mut working, disabled_domains);
    let default_tools = working.resolved.default_tools.clone();
    working.resolved.gates = working
        .gates
        .into_iter()
        .map(|g| g.finalize(&default_tools))
        .collect();
    Ok(working.resolved)
}

fn path_cycle_identity(
    workspace_root: Option<&Path>,
    rel_path: &str,
) -> Result<String, AcddError> {
    let norm_rel = rel_path.trim().replace('\\', "/");
    let norm_rel = norm_rel.strip_prefix("./").unwrap_or(&norm_rel);
    let Some(root) = workspace_root else {
        return Ok(format!("path:{norm_rel}"));
    };
    let checked = checked_child(root, Path::new(norm_rel)).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("profile path '{norm_rel}' failed confinement check: {err}"),
        )
    })?;
    let resolved = match checked.canonicalize() {
        Ok(canon) => canon,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            canon_root.join(norm_rel)
        }
        Err(err) => {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("failed to canonicalize profile path '{norm_rel}': {err}"),
            ));
        }
    };
    Ok(format!("path:{}", resolved.display()))
}

fn effective_workflows_dirs(
    root: &Path,
    explicit_dirs: Option<&[String]>,
) -> Result<Vec<String>, AcddError> {
    if let Some(dirs) = explicit_dirs {
        return Ok(dirs.to_vec());
    }
    acdd_core::config::load_adapter(root, None).map(|a| a.workflows_dirs)
}

fn resolve_workflow_path_in_workspace(
    root: &Path,
    raw_rel: &str,
    explicit_dirs: Option<&[String]>,
) -> Result<String, AcddError> {
    let workflows_dirs = effective_workflows_dirs(root, explicit_dirs)?;
    resolve_confined_ref_path(
        root,
        raw_rel,
        &workflows_dirs,
        ErrorCode::WorkflowSchemaInvalid,
        "workflow",
    )
}

pub(crate) fn workflow_lineage_paths(
    root: &Path,
    workflow_id: &str,
    workflows_dirs: &[String],
) -> Result<Vec<String>, AcddError> {
    let mut reference = workflow_id.to_owned();
    let mut paths = Vec::new();
    while reference != "none" && embedded_workflow(&reference).is_none() {
        let (raw, pin) = parse_path_and_pin(&reference)?;
        let rel = resolve_workflow_path_in_workspace(root, &raw, Some(workflows_dirs))?;
        if paths.contains(&rel) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "cyclic workflow inheritance",
            ));
        }
        let bytes = read_confined_profile_bytes(root, &rel)?;
        if let Some(pin) = pin {
            verify_sha256_prefix(&bytes, &pin, &rel)?;
        }
        let yaml = std::str::from_utf8(&bytes)
            .map_err(|err| AcddError::new(ErrorCode::WorkflowSchemaInvalid, err.to_string()))?;
        reference = parse_workflow(yaml)?
            .inherit
            .unwrap_or_else(|| "none".to_owned());
        paths.push(rel);
    }
    Ok(paths)
}

fn resolve_recursive(
    yaml: &str,
    workflow_id: &str,
    source_rel_path: Option<&str>,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    workflows_dirs: Option<&[String]>,
    stack: &mut Vec<String>,
) -> Result<WorkingWorkflow, AcddError> {
    let parsed = parse_workflow(yaml)?;

    let mut base = match parsed.inherit.as_deref() {
        None | Some("none") => WorkingWorkflow {
            resolved: ResolvedWorkflow {
                workflow_id: workflow_id.to_owned(),
                ..ResolvedWorkflow::default()
            },
            gates: Vec::new(),
        },
        Some("") => {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                "inherit field cannot be an empty string",
            ));
        }
        Some(builtin) if embedded_workflow(builtin).is_some() => {
            let identity = format!("builtin:{builtin}");
            if stack.contains(&identity) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("cyclic workflow inheritance detected at '{identity}'"),
                ));
            }
            let parent_yaml = embedded_workflow(builtin).ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("unknown built-in workflow '{builtin}'"),
                )
            })?;
            stack.push(identity);
            let res = resolve_recursive(
                parent_yaml,
                builtin,
                None,
                workspace_root,
                acdd_modules,
                workflows_dirs,
                stack,
            )?;
            stack.pop();
            res
        }
        Some(raw_ref) => {
            let (raw_path, expected_pin) = parse_path_and_pin(raw_ref)?;
            let root = workspace_root.ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("cannot inherit workflow path '{raw_path}' without workspace root"),
                )
            })?;
            let rel_path = resolve_workflow_path_in_workspace(root, &raw_path, workflows_dirs)?;
            let identity = path_cycle_identity(Some(root), &rel_path)?;
            if stack.contains(&identity) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("cyclic workflow inheritance detected at '{rel_path}'"),
                ));
            }
            let bytes = read_confined_profile_bytes(root, &rel_path)?;
            if let Some(pin) = expected_pin {
                verify_sha256_prefix(&bytes, &pin, &rel_path)?;
            }
            let parent_yaml = std::str::from_utf8(&bytes).map_err(|err| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("inherited workflow '{rel_path}' is not valid UTF-8: {err}"),
                )
            })?;
            stack.push(identity);
            let res = resolve_recursive(
                parent_yaml,
                &rel_path,
                Some(&rel_path),
                Some(root),
                acdd_modules,
                workflows_dirs,
                stack,
            )?;
            stack.pop();
            res
        }
    };

    base.resolved.workflow_id = workflow_id.to_owned();
    let parent_declared_tags = base.resolved.tags.clone();
    let parent_when_tags = collect_when_tags(&base.gates);
    let explicit_tags_override = !matches!(parsed.tags, PatchField::Unset);
    apply_parsed_layer(&mut base, parsed)?;
    if explicit_tags_override {
        let surviving_when_tags = collect_when_tags(&base.gates);
        for tag in parent_when_tags
            .intersection(&surviving_when_tags)
            .filter(|t| parent_declared_tags.contains(*t))
        {
            base.resolved.tags.insert(tag.clone());
        }
    }
    if source_rel_path.is_none() && embedded_workflow(workflow_id).is_some() {
        let disabled_domains = collect_disabled_domains(workflow_id, acdd_modules, false);
        strip_disabled_domain_capabilities(&mut base, &disabled_domains);
    }
    Ok(base)
}

fn collect_when_tags(gates: &[WorkingGate]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for gate in gates {
        if let Some(ref w) = gate.gate.when {
            if let Ok(expr) = WhenExpr::parse(w) {
                out.extend(expr.referenced_tags());
            }
        }
        out.extend(collect_when_tags(&gate.children));
        if let Some(ref tb) = gate.tasks_children {
            if let Some(ref w) = tb.when {
                if let Ok(expr) = WhenExpr::parse(w) {
                    out.extend(expr.referenced_tags());
                }
            }
            out.extend(collect_when_tags(&tb.gates));
        }
    }
    out
}

fn collect_disabled_domains(
    profile_id: &str,
    acdd_modules: Option<&SubsystemRegistry>,
    strip_self_domain: bool,
) -> BTreeSet<String> {
    let mut disabled = BTreeSet::new();
    let Some(mods) = acdd_modules else {
        return disabled;
    };
    for builtin_domain in ["code", "docs"] {
        if (strip_self_domain || builtin_domain != profile_id) && !mods.is_enabled(builtin_domain) {
            disabled.insert(builtin_domain.to_owned());
        }
    }
    for (domain, &enabled) in &mods.domains {
        if !enabled && (strip_self_domain || domain != profile_id) {
            disabled.insert(domain.clone());
        }
    }
    disabled
}

fn tool_belongs_to_domain(tool: &str, domain: &str) -> bool {
    if SubsystemRegistry::subsystem_for_tool(tool) == Some(domain) {
        return true;
    }
    if !acdd_core::ports::is_builtin_mcp_tool(tool) {
        if let Some((prefix, _)) = tool.split_once('_') {
            return prefix == domain;
        }
    }
    false
}

fn has_domain_prefix(qualified: &str, domain: &str) -> bool {
    qualified
        .split_once('.')
        .is_some_and(|(prefix, _)| prefix == domain)
}

fn context_belongs_to_domain(qualified: &str, domain: &str) -> bool {
    has_domain_prefix(qualified, domain)
        || (domain == "docs"
            && (has_domain_prefix(qualified, "paths") || has_domain_prefix(qualified, "specs")))
}

fn strip_disabled_domain_capabilities(
    working: &mut WorkingWorkflow,
    disabled_domains: &BTreeSet<String>,
) {
    if disabled_domains.is_empty() {
        return;
    }
    working.resolved.default_tools.retain(|tool| {
        !disabled_domains
            .iter()
            .any(|d| tool_belongs_to_domain(tool, d))
    });
    strip_disabled_domains_from_gates(&mut working.gates, disabled_domains);
}

fn strip_disabled_domains_from_gates(
    gates: &mut [WorkingGate],
    disabled_domains: &BTreeSet<String>,
) {
    for gate in gates {
        if let Some(tools) = &mut gate.gate.tools {
            tools.retain(|tool| {
                !disabled_domains
                    .iter()
                    .any(|d| tool_belongs_to_domain(tool, d))
            });
        }
        gate.pending_tools_add.retain(|tool| {
            !disabled_domains
                .iter()
                .any(|d| tool_belongs_to_domain(tool, d))
        });
        gate.gate
            .context
            .retain(|ctx| !disabled_domains.iter().any(|d| context_belongs_to_domain(ctx, d)));
        gate.gate.guidance.retain(|g| match g {
            super::schema::GateGuidanceRef::Slot(slot) => {
                !disabled_domains.iter().any(|d| has_domain_prefix(slot, d))
            }
            super::schema::GateGuidanceRef::Inline(_) => true,
        });
        gate.gate.checks.retain(|chk| {
            !disabled_domains
                .iter()
                .any(|d| has_domain_prefix(&chk.check, d))
        });
        if gate
            .gate
            .call
            .as_deref()
            .is_some_and(|c| disabled_domains.iter().any(|d| has_domain_prefix(c, d)))
        {
            gate.gate.call = None;
        }
        strip_disabled_domains_from_gates(&mut gate.children, disabled_domains);
        if let Some(tb) = &mut gate.tasks_children {
            strip_disabled_domains_from_gates(&mut tb.gates, disabled_domains);
        }
    }
}

fn parse_path_and_pin(raw: &str) -> Result<(String, Option<String>), AcddError> {
    let base_name = raw.rsplit_once(':').map_or(raw, |(p, _)| p);
    if embedded_workflow(base_name).is_some() || base_name == "none" {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("built-in workflow reference '{base_name}' cannot carry a SHA-256 pin"),
        ));
    }
    parse_confined_ref(raw, ErrorCode::WorkflowSchemaInvalid, "workflow")
}

fn read_confined_profile_bytes(root: &Path, rel_path: &str) -> Result<Vec<u8>, AcddError> {
    read_confined_ref_bytes(root, rel_path, ErrorCode::WorkflowSchemaInvalid, "workflow")
}

fn verify_sha256_prefix(
    raw_bytes: &[u8],
    expected_prefix_lower: &str,
    rel_path: &str,
) -> Result<(), AcddError> {
    verify_confined_sha256_prefix(
        raw_bytes,
        expected_prefix_lower,
        rel_path,
        ErrorCode::WorkflowSchemaInvalid,
        "workflow",
    )
}

fn apply_parsed_layer(
    working: &mut WorkingWorkflow,
    parsed: WorkflowDef,
) -> Result<(), AcddError> {
    let res = &mut working.resolved;

    if let Some(git_patch) = parsed.git {
        merge_git_policy(&mut res.git, git_patch);
    }

    match parsed.save {
        PatchField::Unset => {}
        PatchField::Clear => res.save = SaveAction::Commit,
        PatchField::Set(s) => res.save = s,
    }

    match parsed.tags {
        PatchField::Unset => {
            res.tags.insert("default".to_owned());
        }
        PatchField::Clear => {
            res.tags = BTreeSet::from(["default".to_owned()]);
        }
        PatchField::Set(mut tags) => {
            tags.insert("default".to_owned());
            res.tags = tags;
        }
    }

    match parsed.protected_paths {
        PatchField::Unset => {}
        PatchField::Clear => res.protected_paths.clear(),
        PatchField::Set(paths) => res.protected_paths = paths,
    }

    match parsed.default_tools {
        PatchField::Unset => {}
        PatchField::Clear => res.default_tools.clear(),
        PatchField::Set(tools) => res.default_tools = tools,
    }

    for (name, patch_opt) in parsed.roles {
        match patch_opt {
            None => {
                res.roles.remove(&name);
            }
            Some(patch) => {
                let entry = res.roles.entry(name).or_default();
                merge_role(entry, patch);
            }
        }
    }

    for (name, cmd_opt) in parsed.commands {
        match cmd_opt {
            None => {
                res.commands.remove(&name);
            }
            Some(cmd) => {
                res.commands.insert(name, cmd);
            }
        }
    }

    for (pat, arg_opt) in parsed.scope_args_by_pattern {
        match arg_opt {
            None => {
                res.scope_args_by_pattern.remove(&pat);
            }
            Some(arg) => {
                res.scope_args_by_pattern.insert(pat, arg);
            }
        }
    }

    for (name, proof_opt) in parsed.proofs {
        match proof_opt {
            None => {
                res.proofs.remove(&name);
            }
            Some(proof) => {
                res.proofs.insert(name, proof);
            }
        }
    }

    for (slot, guidance_opt) in parsed.guidance {
        match guidance_opt {
            None => {
                res.guidance.remove(&slot);
            }
            Some(g) => {
                res.guidance.insert(slot, g);
            }
        }
    }

    for (name, globs_opt) in parsed.paths {
        match globs_opt {
            None => {
                res.paths.remove(&name);
            }
            Some(globs) => {
                res.paths.insert(name, globs);
            }
        }
    }

    for (name, contour_opt) in parsed.contours {
        match contour_opt {
            None => {
                res.contours.remove(&name);
            }
            Some(c) => {
                res.contours.insert(name, c);
            }
        }
    }

    for (name, tpl_opt) in parsed.gate_templates {
        match tpl_opt {
            None => {
                res.gate_templates.remove(&name);
            }
            Some(tpl) => {
                res.gate_templates.insert(name, tpl);
            }
        }
    }

    if let Some(mod_patch) = parsed.modules {
        merge_modules(&mut res.modules, mod_patch);
    }

    if let Some(lim_patch) = parsed.limits {
        merge_limits(&mut res.limits, lim_patch);
    }

    if let Some(gate_items) = parsed.gates {
        working.gates = build_gates_replace(gate_items)?;
    }
    if let Some(patch_items) = parsed.gates_patch {
        apply_gates_patch(&mut working.gates, patch_items)?;
    }

    Ok(())
}

fn merge_git_policy(target: &mut GitPolicy, patch: GitPolicyPatch) {
    if let Some(v) = patch.isolation {
        target.isolation = v;
    }
    if let Some(v) = patch.worktrees {
        target.worktrees = v;
    }
}

fn merge_role(target: &mut RoleDef, patch: RolePatch) {
    if let PatchField::Set(v) = patch.mode {
        target.mode = v;
    } else if matches!(patch.mode, PatchField::Clear) {
        target.mode = Default::default();
    }
    if let PatchField::Set(v) = patch.reuse_on_reject {
        target.reuse_on_reject = v;
    } else if matches!(patch.reuse_on_reject, PatchField::Clear) {
        target.reuse_on_reject = false;
    }
    if let PatchField::Set(v) = patch.models {
        target.models = v;
    } else if matches!(patch.models, PatchField::Clear) {
        target.models.clear();
    }
    patch
        .guidance
        .apply_to_option(&mut target.guidance, Clone::clone);
    patch
        .runner
        .apply_to_option(&mut target.runner, Clone::clone);
    patch
        .prompt_template
        .apply_to_option(&mut target.prompt_template, Clone::clone);
}

fn merge_modules(target: &mut SubsystemRegistry, patch: SubsystemRegistryPatch) {
    if let Some(preset) = patch.preset {
        target.preset = preset;
    }
    if let PatchField::Set(v) = patch.enabled {
        target.enabled = v;
    } else if matches!(patch.enabled, PatchField::Clear) {
        target.enabled.clear();
    }
    if let PatchField::Set(v) = patch.disabled {
        target.disabled = v;
    } else if matches!(patch.disabled, PatchField::Clear) {
        target.disabled.clear();
    }
    if let PatchField::Set(v) = patch.required {
        target.required = v;
    } else if matches!(patch.required, PatchField::Clear) {
        target.required.clear();
    }
    for (domain, val) in patch.domains {
        match val {
            None => {
                target.domains.remove(&domain);
            }
            Some(b) => {
                target.domains.insert(domain, b);
            }
        }
    }
}

fn merge_limits(target: &mut ResourceLimits, patch: super::schema::ResourceLimitsPatch) {
    if let Some(v) = patch.max_guidance_bytes {
        target.max_guidance_bytes = v;
    }
    if let Some(v) = patch.max_log_bytes {
        target.max_log_bytes = v;
    }
    if let Some(v) = patch.max_capture_bytes {
        target.max_capture_bytes = v;
    }
    if let Some(v) = patch.max_line_bytes {
        target.max_line_bytes = v;
    }
    if let Some(v) = patch.diagnostic_inline_bytes {
        target.diagnostic_inline_bytes = v;
    }
    if let Some(v) = patch.max_metadata_bytes {
        target.max_metadata_bytes = v;
    }
    if let Some(v) = patch.max_trace_summary_bytes {
        target.max_trace_summary_bytes = v;
    }
    if let Some(v) = patch.max_command_timeout_seconds {
        target.max_command_timeout_seconds = v;
    }
}

fn build_gates_replace(items: Vec<RawGateItem>) -> Result<Vec<WorkingGate>, AcddError> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            RawGateItem::Delete(sel) => {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate deletion '{sel}: null' is only allowed inside 'gates_patch'"),
                ));
            }
            RawGateItem::Gate(raw) => {
                if raw.after.is_some() || raw.before.is_some() {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        "'after'/'before' gate positioning is only allowed inside 'gates_patch'",
                    ));
                }
                if raw.id.as_deref().is_some_and(|id| id.contains('.')) {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!(
                            "dotted gate cursor '{}' is only allowed inside 'gates_patch'",
                            raw.id.as_deref().unwrap_or_default()
                        ),
                    ));
                }
                out.push(raw_to_working_gate(*raw)?);
            }
        }
    }
    Ok(out)
}

fn raw_to_working_gate(raw: RawGateDef) -> Result<WorkingGate, AcddError> {
    let mut gate = GateDef::default();
    if let Some(id) = raw.id {
        GateId::new(&id)?;
        gate.id = id;
    }
    let has_explicit_tools = !matches!(raw.tools, PatchField::Unset);
    let has_explicit_max_attempts = matches!(raw.max_attempts, PatchField::Set(_));
    let has_template = matches!(raw.use_template, PatchField::Set(_));
    let mut pending_tools_add = Vec::new();
    let mut pending_tools_remove = Vec::new();
    let mut pending_template_gates_patch = Vec::new();

    if let PatchField::Set(v) = raw.role {
        gate.role = Some(v);
    }
    if let PatchField::Set(v) = raw.command {
        gate.command = Some(v);
    }
    if let PatchField::Set(v) = raw.call {
        gate.call = Some(v);
    }
    if let PatchField::Set(v) = raw.with {
        gate.with = Some(v);
    }
    if let PatchField::Set(v) = raw.checks {
        gate.checks = v;
    }
    if let PatchField::Set(v) = raw.context {
        gate.context = v;
    }
    let children = match raw.gates {
        PatchField::Set(items) => {
            if has_template {
                pending_template_gates_patch = items;
                Vec::new()
            } else {
                build_gates_replace(items)?
            }
        }
        PatchField::Unset | PatchField::Clear => Vec::new(),
    };
    let tasks_children = match raw.tasks {
        Some(tb) => {
            let when = match tb.when {
                PatchField::Set(w) => Some(w),
                PatchField::Unset | PatchField::Clear => None,
            };
            let gates = build_gates_replace(tb.gates)?;
            Some(WorkingTasksBlock { when, gates })
        }
        None => None,
    };
    if let PatchField::Set(v) = raw.use_template {
        gate.use_template = Some(v);
    }
    if let PatchField::Set(v) = raw.when {
        gate.when = Some(v);
    }
    if let PatchField::Set(v) = raw.proof {
        gate.proof = v;
    }
    if let PatchField::Set(v) = raw.tools {
        gate.tools = Some(v);
    }
    pending_tools_add.extend(raw.tools_add);
    pending_tools_remove.extend(raw.tools_remove);
    if let PatchField::Set(v) = raw.guidance {
        gate.guidance = v;
    }
    if let PatchField::Set(v) = raw.contours {
        gate.contours = v;
    }
    if let PatchField::Set(v) = raw.scope {
        gate.scope = v;
    }
    if let Some(save_patch) = raw.save {
        merge_save_policy(&mut gate.save, save_patch);
    }
    if let PatchField::Set(v) = raw.max_attempts {
        gate.max_attempts = v;
    }
    if let PatchField::Set(v) = raw.receipt {
        gate.receipt = v;
    }
    if let PatchField::Set(v) = raw.independent_from {
        gate.independent_from = v;
    }
    if let PatchField::Set(v) = raw.need {
        gate.need = v;
    }
    if let PatchField::Set(v) = raw.pass {
        gate.pass = Some(v);
    }
    if let PatchField::Set(v) = raw.fail {
        gate.fail = Some(v);
    }
    if let PatchField::Set(v) = raw.fail_drift {
        gate.fail_drift = Some(v);
    }

    if gate.id.is_empty() && tasks_children.is_none() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "every gate entry must declare either 'id' or 'tasks'",
        ));
    }

    Ok(WorkingGate {
        gate,
        has_explicit_tools,
        has_explicit_max_attempts,
        pending_tools_add,
        pending_tools_remove,
        pending_template_gates_patch,
        children,
        tasks_children,
    })
}

fn apply_gates_patch(
    target: &mut Vec<WorkingGate>,
    items: Vec<RawGateItem>,
) -> Result<(), AcddError> {
    for item in items {
        match item {
            RawGateItem::Delete(selector) => {
                delete_gate_by_selector(target, &selector)?;
            }
            RawGateItem::Gate(raw) => {
                if raw.after.is_some() && raw.before.is_some() {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        "gate patch entry cannot specify both 'after' and 'before'",
                    ));
                }
                if let Some(anchor) = raw.after.clone() {
                    insert_gate_relative(target, *raw, &anchor, true)?;
                } else if let Some(anchor) = raw.before.clone() {
                    insert_gate_relative(target, *raw, &anchor, false)?;
                } else if raw.tasks.is_some() {
                    patch_tasks_block(target, *raw)?;
                } else if let Some(id_or_cursor) = raw.id.clone() {
                    if let Some((head, rest)) = id_or_cursor.split_once('.') {
                        if head == "tasks" {
                            patch_via_tasks_cursor(target, rest, *raw)?;
                        } else {
                            let matched = target
                                .iter_mut()
                                .find(|g| g.gate.id == head)
                                .ok_or_else(|| {
                                    AcddError::new(
                                        ErrorCode::WorkflowSchemaInvalid,
                                        format!("patch target gate '{id_or_cursor}' not found (missing container '{head}')"),
                                    )
                                })?;
                            let mut nested = *raw;
                            nested.id = Some(rest.to_owned());
                            apply_gates_patch(
                                &mut matched.children,
                                vec![RawGateItem::Gate(Box::new(nested))],
                            )?;
                        }
                    } else {
                        let matched = target
                            .iter_mut()
                            .find(|g| g.gate.id == id_or_cursor)
                            .ok_or_else(|| {
                                AcddError::new(
                                    ErrorCode::WorkflowSchemaInvalid,
                                    format!(
                                        "patch target gate '{id_or_cursor}' does not exist in inherited profile; use 'after:'/'before:' to insert a new gate"
                                    ),
                                )
                            })?;
                        patch_existing_working_gate(matched, *raw)?;
                    }
                } else {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        "gate patch entry must declare 'id' or 'tasks'",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn delete_gate_by_selector(
    target: &mut Vec<WorkingGate>,
    selector: &str,
) -> Result<(), AcddError> {
    if let Some((head, rest)) = selector.split_once('.') {
        if head == "tasks" {
            let mut matching_blocks: Vec<&mut WorkingTasksBlock> = target
                .iter_mut()
                .filter_map(|g| g.tasks_children.as_mut())
                .filter(|tb| contains_gate_path(&tb.gates, rest))
                .collect();
            if matching_blocks.len() != 1 {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "gate deletion target '{selector}' matched {} tasks blocks (expected exactly 1)",
                        matching_blocks.len()
                    ),
                ));
            }
            return delete_gate_by_selector(&mut matching_blocks[0].gates, rest);
        }
        let parent = target
            .iter_mut()
            .find(|g| g.gate.id == head)
            .ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("gate deletion target '{selector}' not found"),
                )
            })?;
        return delete_gate_by_selector(&mut parent.children, rest);
    }

    let before_len = target.len();
    target.retain(|g| g.gate.id != selector);
    if target.len() == before_len {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("gate deletion target '{selector}' does not exist"),
        ));
    }
    Ok(())
}

fn contains_gate_path(gates: &[WorkingGate], path: &str) -> bool {
    match path.split_once('.') {
        Some((head, rest)) => gates
            .iter()
            .find(|g| g.gate.id == head)
            .is_some_and(|g| contains_gate_path(&g.children, rest)),
        None => gates.iter().any(|g| g.gate.id == path),
    }
}

fn insert_gate_relative(
    target: &mut Vec<WorkingGate>,
    mut raw: RawGateDef,
    anchor: &str,
    insert_after: bool,
) -> Result<(), AcddError> {
    raw.after = None;
    raw.before = None;
    if let Some((head, rest)) = anchor.split_once('.') {
        if head == "tasks" {
            let mut matching_blocks: Vec<&mut WorkingTasksBlock> = target
                .iter_mut()
                .filter_map(|g| g.tasks_children.as_mut())
                .filter(|tb| contains_gate_path(&tb.gates, rest))
                .collect();
            if matching_blocks.len() != 1 {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!(
                        "insertion anchor '{anchor}' matched {} tasks blocks (expected exactly 1)",
                        matching_blocks.len()
                    ),
                ));
            }
            return insert_gate_relative(&mut matching_blocks[0].gates, raw, rest, insert_after);
        }
        let parent = target
            .iter_mut()
            .find(|g| g.gate.id == head)
            .ok_or_else(|| {
                AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("insertion anchor '{anchor}' not found"),
                )
            })?;
        return insert_gate_relative(&mut parent.children, raw, rest, insert_after);
    }

    if raw.id.as_deref().is_some_and(|id| id.contains('.')) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "inserted gate id '{}' cannot contain '.'",
                raw.id.as_deref().unwrap_or_default()
            ),
        ));
    }

    let pos = target
        .iter()
        .position(|g| g.gate.id == anchor)
        .ok_or_else(|| {
            AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("insertion anchor gate '{anchor}' does not exist"),
            )
        })?;

    let new_gate = raw_to_working_gate(raw)?;
    if !new_gate.gate.id.is_empty() && target.iter().any(|g| g.gate.id == new_gate.gate.id) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "inserted gate '{}' already exists at target level",
                new_gate.gate.id
            ),
        ));
    }
    let idx = if insert_after { pos + 1 } else { pos };
    target.insert(idx, new_gate);
    Ok(())
}

fn patch_tasks_block(target: &mut [WorkingGate], raw: RawGateDef) -> Result<(), AcddError> {
    let Some(raw_tb) = raw.tasks else {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            "internal error: patch_tasks_block called without tasks",
        ));
    };
    let child_when = match &raw_tb.when {
        PatchField::Unset | PatchField::Clear => None,
        PatchField::Set(w) => normalize_optional_when(Some(w))?,
    };
    let mut matches: Vec<&mut WorkingGate> = if let Some(ref explicit_id) = raw.id {
        target
            .iter_mut()
            .filter(|g| g.tasks_children.is_some() && g.gate.id == *explicit_id)
            .collect()
    } else {
        let mut matched = Vec::new();
        for g in target.iter_mut() {
            if let Some(tb) = &g.tasks_children {
                let parent_when = normalize_optional_when(tb.when.as_deref())?;
                if parent_when == child_when {
                    matched.push(g);
                }
            }
        }
        matched
    };

    if matches.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "patch '- tasks:' target with when={child_when:?} does not match any inherited tasks block"
            ),
        ));
    }
    if matches.len() > 1 {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "ambiguous patch '- tasks:' target with when={child_when:?}: {} inherited blocks match; specify explicit id",
                matches.len()
            ),
        ));
    }

    let tb = matches[0]
        .tasks_children
        .as_mut()
        .expect("filtered for tasks_children");
    apply_gates_patch(&mut tb.gates, raw_tb.gates)
}

fn patch_via_tasks_cursor(
    target: &mut [WorkingGate],
    rest: &str,
    mut raw: RawGateDef,
) -> Result<(), AcddError> {
    let mut matching_blocks: Vec<&mut WorkingTasksBlock> = target
        .iter_mut()
        .filter_map(|g| g.tasks_children.as_mut())
        .filter(|tb| contains_gate_path(&tb.gates, rest))
        .collect();
    if matching_blocks.is_empty() {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("patch target 'tasks.{rest}' not found in any inherited tasks block"),
        ));
    }
    if matching_blocks.len() > 1 {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "ambiguous patch target 'tasks.{rest}' matches {} tasks blocks; use structural '- tasks:' with when:",
                matching_blocks.len()
            ),
        ));
    }
    raw.id = Some(rest.to_owned());
    apply_gates_patch(
        &mut matching_blocks[0].gates,
        vec![RawGateItem::Gate(Box::new(raw))],
    )
}

fn patch_existing_working_gate(
    target: &mut WorkingGate,
    raw: RawGateDef,
) -> Result<(), AcddError> {
    let g = &mut target.gate;

    raw.role.apply_to_option(&mut g.role, Clone::clone);
    raw.command.apply_to_option(&mut g.command, Clone::clone);
    raw.call.apply_to_option(&mut g.call, Clone::clone);
    raw.with.apply_to_option(&mut g.with, Clone::clone);

    match raw.checks {
        PatchField::Unset => {}
        PatchField::Clear => g.checks.clear(),
        PatchField::Set(v) => g.checks = v,
    }
    match raw.context {
        PatchField::Unset => {}
        PatchField::Clear => g.context.clear(),
        PatchField::Set(v) => g.context = v,
    }
    raw.use_template
        .apply_to_option(&mut g.use_template, Clone::clone);
    if g.use_template.is_none() {
        target.pending_template_gates_patch.clear();
    }
    match raw.gates {
        PatchField::Unset => {}
        PatchField::Clear => {
            target.children.clear();
            target.pending_template_gates_patch.clear();
        }
        PatchField::Set(items) => {
            if g.use_template.is_some() && target.children.is_empty() {
                target.pending_template_gates_patch.extend(items);
            } else {
                apply_gates_patch(&mut target.children, items)?;
            }
        }
    }
    raw.when.apply_to_option(&mut g.when, Clone::clone);

    match raw.proof {
        PatchField::Unset => {}
        PatchField::Clear => g.proof = ProofDef::default(),
        PatchField::Set(v) => g.proof = v,
    }
    match raw.tools {
        PatchField::Unset => {}
        PatchField::Clear => {
            target.has_explicit_tools = true;
            g.tools = Some(Vec::new());
            target.pending_tools_add.clear();
            target.pending_tools_remove.clear();
        }
        PatchField::Set(v) => {
            target.has_explicit_tools = true;
            g.tools = Some(v);
            target.pending_tools_add.clear();
            target.pending_tools_remove.clear();
        }
    }
    for add in raw.tools_add {
        target.pending_tools_remove.retain(|r| r != &add);
        if !target.pending_tools_add.contains(&add) {
            target.pending_tools_add.push(add);
        }
    }
    for rem in raw.tools_remove {
        target.pending_tools_add.retain(|a| a != &rem);
        if !target.pending_tools_remove.contains(&rem) {
            target.pending_tools_remove.push(rem);
        }
    }
    match raw.guidance {
        PatchField::Unset => {}
        PatchField::Clear => g.guidance.clear(),
        PatchField::Set(v) => g.guidance = v,
    }
    match raw.contours {
        PatchField::Unset => {}
        PatchField::Clear => g.contours.clear(),
        PatchField::Set(v) => g.contours = v,
    }
    match raw.scope {
        PatchField::Unset => {}
        PatchField::Clear => g.scope.clear(),
        PatchField::Set(v) => g.scope = v,
    }
    if let Some(save_patch) = raw.save {
        merge_save_policy(&mut g.save, save_patch);
    }
    match raw.max_attempts {
        PatchField::Unset => {}
        PatchField::Clear => {
            g.max_attempts = DEFAULT_MAX_ATTEMPTS;
            target.has_explicit_max_attempts = false;
        }
        PatchField::Set(v) => {
            g.max_attempts = v;
            target.has_explicit_max_attempts = true;
        }
    }
    match raw.receipt {
        PatchField::Unset => {}
        PatchField::Clear => g.receipt = false,
        PatchField::Set(v) => g.receipt = v,
    }
    match raw.independent_from {
        PatchField::Unset => {}
        PatchField::Clear => g.independent_from.clear(),
        PatchField::Set(v) => g.independent_from = v,
    }
    match raw.need {
        PatchField::Unset => {}
        PatchField::Clear => g.need.clear(),
        PatchField::Set(v) => g.need = v,
    }
    raw.pass.apply_to_option(&mut g.pass, Clone::clone);
    raw.fail.apply_to_option(&mut g.fail, Clone::clone);
    raw.fail_drift
        .apply_to_option(&mut g.fail_drift, Clone::clone);

    Ok(())
}

fn merge_save_policy(target: &mut SavePolicy, patch: SavePolicyPatch) {
    patch.post.apply_to_option(&mut target.post, |a| *a);
    match patch.immutable {
        PatchField::Unset => {}
        PatchField::Clear => target.immutable = false,
        PatchField::Set(v) => target.immutable = v,
    }
    match patch.archive {
        PatchField::Unset => {}
        PatchField::Clear => target.archive = false,
        PatchField::Set(v) => target.archive = v,
    }
    match patch.prune {
        PatchField::Unset => {}
        PatchField::Clear => target.prune = false,
        PatchField::Set(v) => target.prune = v,
    }
    patch
        .report
        .apply_to_option(&mut target.report, Clone::clone);
}

fn expand_templates_in_working(working: &mut WorkingWorkflow) -> Result<(), AcddError> {
    let templates = working.resolved.gate_templates.clone();
    let mut stack = Vec::new();
    expand_gate_list(&mut working.gates, &templates, &mut stack)
}

fn apply_template_defaults(gate: &mut WorkingGate, tpl: &GateTemplate) {
    if gate.gate.command.is_none() {
        gate.gate.command = tpl.command.clone();
    }
    if gate.gate.guidance.is_empty() && !tpl.guidance.is_empty() {
        gate.gate.guidance = tpl.guidance.clone();
    }
    if let Some(max_att) = tpl.max_attempts {
        if !gate.has_explicit_max_attempts {
            gate.gate.max_attempts = max_att;
        }
    }
}

fn expand_gate_template(
    gate: &mut WorkingGate,
    tpl_name: String,
    templates: &BTreeMap<String, GateTemplate>,
    stack: &mut Vec<String>,
) -> Result<(), AcddError> {
    if stack.contains(&tpl_name) {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("cyclic gate_templates expansion detected at '{tpl_name}'"),
        ));
    }
    let tpl = templates.get(&tpl_name).ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!(
                "gate '{}' references unknown gate_template '{}'",
                gate.gate.id, tpl_name
            ),
        )
    })?;
    stack.push(tpl_name);
    let mut tpl_children: Vec<WorkingGate> = tpl
        .gates
        .iter()
        .cloned()
        .map(raw_to_working_gate)
        .collect::<Result<Vec<_>, _>>()?;
    for child in &mut tpl_children {
        apply_template_defaults(child, tpl);
    }
    expand_gate_list(&mut tpl_children, templates, stack)?;
    let inline_patch = std::mem::take(&mut gate.pending_template_gates_patch);
    if !inline_patch.is_empty() {
        if tpl_children.is_empty() {
            tpl_children = build_gates_replace(inline_patch)?;
        } else {
            apply_gates_patch(&mut tpl_children, inline_patch)?;
        }
    }
    if gate.children.is_empty() {
        gate.children = tpl_children;
    }
    apply_template_defaults(gate, tpl);
    stack.pop();
    Ok(())
}

fn expand_gate_list(
    gates: &mut [WorkingGate],
    templates: &BTreeMap<String, GateTemplate>,
    stack: &mut Vec<String>,
) -> Result<(), AcddError> {
    for gate in gates.iter_mut() {
        if let Some(tpl_name) = gate.gate.use_template.clone() {
            expand_gate_template(gate, tpl_name, templates, stack)?;
        }
        expand_gate_list(&mut gate.children, templates, stack)?;
        if let Some(tb) = &mut gate.tasks_children {
            expand_gate_list(&mut tb.gates, templates, stack)?;
        }
    }
    Ok(())
}
