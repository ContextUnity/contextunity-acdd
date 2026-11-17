//! Profile inheritance (`inherit`), `gates_merge: replace | patch` resolution, SHA-256 pin verification, and template expansion.

use super::schema::{
    parse_profile, GateDef, GateTemplate, GatesMergeMode, GitPolicy, GitPolicyPatch,
    ParsedProfileFile, PatchField, ProofDef, RawGateDef, RawGateItem, ResolvedGateProfile,
    ResourceLimits, RoleDef, RolePatch, SavePolicy, SavePolicyPatch, SubsystemRegistryPatch,
    TasksBlock, DEFAULT_MAX_ATTEMPTS,
};
use super::when::normalize_optional_when;
use crate::types::SaveAction;
use acdd_core::fs::{is_normalized_relative, open_confined};
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{is_valid_short_hex_prefix, GateId};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

const DEFAULT_YAML: &str = include_str!("embedded/default.yaml");
const CODE_YAML: &str = include_str!("embedded/code.yaml");
const IN_PLACE_YAML: &str = include_str!("embedded/in_place.yaml");

/// Return the raw embedded YAML for a built-in profile (`"default"`, `"code"`, or `"in_place"`).
pub fn embedded_profile(name: &str) -> Option<&'static str> {
    match name {
        "default" => Some(DEFAULT_YAML),
        "code" => Some(CODE_YAML),
        "in_place" => Some(IN_PLACE_YAML),
        _ => None,
    }
}

/// Intermediate resolved state tracking explicit `tools:` presence across `inherit`/`patch` layers.
#[derive(Debug, Clone)]
struct WorkingProfile {
    resolved: ResolvedGateProfile,
    gates: Vec<WorkingGate>,
}

#[derive(Debug, Clone)]
struct WorkingGate {
    gate: GateDef,
    has_explicit_tools: bool,
    pending_tools_add: Vec<String>,
    pending_tools_remove: Vec<String>,
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
        let mut effective_tools = if self.has_explicit_tools {
            self.gate.tools
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
        self.gate.tools = effective_tools;
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

/// Resolve a profile YAML string (with optional source relative path and workspace root for `inherit:`).
pub fn resolve_profile_from_str(
    yaml: &str,
    profile_id: &str,
    source_rel_path: Option<&str>,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
) -> Result<ResolvedGateProfile, StateError> {
    let mut stack = Vec::new();
    if let Some(rel) = source_rel_path {
        stack.push(format!("path:{rel}"));
    } else {
        stack.push(format!("inline:{profile_id}"));
    }
    let mut working = resolve_recursive(
        yaml,
        profile_id,
        source_rel_path,
        workspace_root,
        acdd_modules,
        &mut stack,
    )?;
    expand_templates_in_working(&mut working)?;
    let default_tools = working.resolved.default_tools.clone();
    working.resolved.gates = working
        .gates
        .into_iter()
        .map(|g| g.finalize(&default_tools))
        .collect();
    Ok(working.resolved)
}

/// Load and resolve a built-in (`default | code | in_place`) or workspace-relative profile path.
pub fn resolve_profile_ref(
    profile_ref: &str,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
) -> Result<ResolvedGateProfile, StateError> {
    if let Some(yaml) = embedded_profile(profile_ref) {
        return resolve_builtin(profile_ref, yaml, workspace_root, acdd_modules);
    }
    if profile_ref == "workspace" {
        let yaml = "version: 2\ninherit: workspace\n";
        return resolve_profile_from_str(yaml, "workspace", None, workspace_root, acdd_modules);
    }
    let root = workspace_root.ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("cannot resolve profile path '{profile_ref}' without workspace root"),
        )
    })?;
    let (rel_path, expected_pin) = parse_path_and_pin(profile_ref)?;
    let bytes = read_confined_profile_bytes(root, &rel_path)?;
    if let Some(pin) = expected_pin {
        verify_sha256_prefix(&bytes, &pin, &rel_path)?;
    }
    let yaml = std::str::from_utf8(&bytes).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("profile '{rel_path}' is not valid UTF-8: {err}"),
        )
    })?;
    resolve_profile_from_str(yaml, &rel_path, Some(&rel_path), Some(root), acdd_modules)
}

fn resolve_builtin(
    name: &str,
    yaml: &str,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
) -> Result<ResolvedGateProfile, StateError> {
    let mut stack = vec![format!("builtin:{name}")];
    let mut working = resolve_recursive(yaml, name, None, workspace_root, acdd_modules, &mut stack)?;
    expand_templates_in_working(&mut working)?;
    let default_tools = working.resolved.default_tools.clone();
    working.resolved.gates = working
        .gates
        .into_iter()
        .map(|g| g.finalize(&default_tools))
        .collect();
    Ok(working.resolved)
}

fn resolve_recursive(
    yaml: &str,
    profile_id: &str,
    source_rel_path: Option<&str>,
    workspace_root: Option<&Path>,
    acdd_modules: Option<&SubsystemRegistry>,
    stack: &mut Vec<String>,
) -> Result<WorkingProfile, StateError> {
    let parsed = parse_profile(yaml)?;

    let mut base = match parsed.inherit.as_deref() {
        None | Some("none") => WorkingProfile {
            resolved: ResolvedGateProfile {
                profile_id: profile_id.to_owned(),
                ..ResolvedGateProfile::default()
            },
            gates: Vec::new(),
        },
        Some("") => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "inherit field cannot be an empty string",
            ));
        }
        Some("default") | Some("code") | Some("in_place") => {
            let builtin = parsed.inherit.as_deref().unwrap_or("default");
            let identity = format!("builtin:{builtin}");
            if stack.contains(&identity) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("cyclic profile inheritance detected at '{identity}'"),
                ));
            }
            let parent_yaml = embedded_profile(builtin).ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("unknown built-in profile '{builtin}'"),
                )
            })?;
            stack.push(identity);
            let res = resolve_recursive(
                parent_yaml,
                builtin,
                None,
                workspace_root,
                acdd_modules,
                stack,
            )?;
            stack.pop();
            res
        }
        Some("workspace") => {
            if source_rel_path == Some(".acdd/profile.yaml") {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "'.acdd/profile.yaml' cannot inherit 'workspace' (self-inheritance)",
                ));
            }
            let has_workspace_file = workspace_root
                .map(|root| root.join(".acdd/profile.yaml").exists())
                .unwrap_or(false);
            if has_workspace_file {
                let root = workspace_root.expect("checked above");
                let identity = "path:.acdd/profile.yaml".to_owned();
                if stack.contains(&identity) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "cyclic profile inheritance detected at '.acdd/profile.yaml'",
                    ));
                }
                let bytes = read_confined_profile_bytes(root, ".acdd/profile.yaml")?;
                let parent_yaml = std::str::from_utf8(&bytes).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("'.acdd/profile.yaml' is not valid UTF-8: {err}"),
                    )
                })?;
                stack.push(identity);
                let res = resolve_recursive(
                    parent_yaml,
                    ".acdd/profile.yaml",
                    Some(".acdd/profile.yaml"),
                    Some(root),
                    acdd_modules,
                    stack,
                )?;
                stack.pop();
                res
            } else {
                let fallback = if acdd_modules.is_some_and(|m| m.is_enabled("code")) {
                    "code"
                } else {
                    "default"
                };
                let identity = format!("builtin:{fallback}");
                if stack.contains(&identity) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("cyclic profile inheritance detected at '{identity}'"),
                    ));
                }
                let parent_yaml = embedded_profile(fallback).expect("builtin exists");
                stack.push(identity);
                let res = resolve_recursive(
                    parent_yaml,
                    fallback,
                    None,
                    workspace_root,
                    acdd_modules,
                    stack,
                )?;
                stack.pop();
                res
            }
        }
        Some(raw_ref) => {
            let (rel_path, expected_pin) = parse_path_and_pin(raw_ref)?;
            let identity = format!("path:{rel_path}");
            if stack.contains(&identity) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("cyclic profile inheritance detected at '{rel_path}'"),
                ));
            }
            let root = workspace_root.ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("cannot inherit profile path '{rel_path}' without workspace root"),
                )
            })?;
            let bytes = read_confined_profile_bytes(root, &rel_path)?;
            if let Some(pin) = expected_pin {
                verify_sha256_prefix(&bytes, &pin, &rel_path)?;
            }
            let parent_yaml = std::str::from_utf8(&bytes).map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("inherited profile '{rel_path}' is not valid UTF-8: {err}"),
                )
            })?;
            stack.push(identity);
            let res = resolve_recursive(
                parent_yaml,
                &rel_path,
                Some(&rel_path),
                Some(root),
                acdd_modules,
                stack,
            )?;
            stack.pop();
            res
        }
    };

    base.resolved.profile_id = profile_id.to_owned();
    apply_parsed_layer(&mut base, parsed)?;
    Ok(base)
}

fn parse_path_and_pin(raw: &str) -> Result<(String, Option<String>), StateError> {
    let (path_part, pin_part) = match raw.rsplit_once(':') {
        Some((p, pin)) if !p.is_empty() && !pin.is_empty() => (p, Some(pin)),
        Some((_, _)) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("malformed profile reference '{raw}'"),
            ));
        }
        None => (raw, None),
    };
    if embedded_profile(path_part).is_some() || matches!(path_part, "workspace" | "none") {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("built-in profile reference '{path_part}' cannot carry a SHA-256 pin"),
        ));
    }
    if !is_normalized_relative(Path::new(path_part)) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("inherited profile path '{path_part}' must be a normalized workspace-relative path"),
        ));
    }
    let normalized_pin = match pin_part {
        Some(pin) => {
            if !is_valid_short_hex_prefix(pin) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "inherit SHA-256 prefix '{pin}' must be 7..=64 hexadecimal characters"
                    ),
                ));
            }
            Some(pin.to_ascii_lowercase())
        }
        None => None,
    };
    Ok((path_part.to_owned(), normalized_pin))
}

fn read_confined_profile_bytes(root: &Path, rel_path: &str) -> Result<Vec<u8>, StateError> {
    let mut file = open_confined(root, Path::new(rel_path), OpenOptions::new().read(true))
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to open inherited profile '{rel_path}': {err}"),
            )
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to read inherited profile '{rel_path}': {err}"),
        )
    })?;
    Ok(bytes)
}

fn verify_sha256_prefix(
    raw_bytes: &[u8],
    expected_prefix_lower: &str,
    rel_path: &str,
) -> Result<(), StateError> {
    let actual = acdd_core::format::sha256_hex(raw_bytes);
    if !actual.starts_with(expected_prefix_lower) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "inherit SHA-256 pin mismatch for '{rel_path}': expected prefix '{expected_prefix_lower}', got '{actual}'"
            ),
        ));
    }
    Ok(())
}

fn apply_parsed_layer(
    working: &mut WorkingProfile,
    parsed: ParsedProfileFile,
) -> Result<(), StateError> {
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

    for (name, note_opt) in parsed.git_commit_notes {
        match note_opt {
            None => {
                res.git_commit_notes.remove(&name);
            }
            Some(patch) => {
                let entry = res.git_commit_notes.entry(name).or_default();
                merge_git_commit_note(entry, patch);
            }
        }
    }

    if let Some(gate_items) = parsed.gates {
        match parsed.gates_merge {
            GatesMergeMode::Replace => {
                working.gates = build_gates_replace(gate_items)?;
            }
            GatesMergeMode::Patch => {
                apply_gates_patch(&mut working.gates, gate_items)?;
            }
        }
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
    if let Some(v) = patch.commit_mode {
        target.commit_mode = v;
    }
    if let Some(v) = patch.integrate {
        target.integrate = v;
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
    patch.guidance.apply_to_option(&mut target.guidance, Clone::clone);
    patch.runner.apply_to_option(&mut target.runner, Clone::clone);
    patch
        .prompt_template
        .apply_to_option(&mut target.prompt_template, Clone::clone);
}

fn merge_modules(target: &mut SubsystemRegistry, patch: SubsystemRegistryPatch) {
    if let Some(preset) = patch.profile {
        target.profile = preset;
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

fn merge_git_commit_note(
    target: &mut super::schema::GitCommitNotePolicy,
    patch: super::schema::GitCommitNotePolicyPatch,
) {
    match patch.ref_name {
        PatchField::Unset => {}
        PatchField::Clear => target.ref_name = "refs/notes/acdd".to_owned(),
        PatchField::Set(v) => target.ref_name = v,
    }
    match patch.max_bytes {
        PatchField::Unset => {}
        PatchField::Clear => target.max_bytes = 2048,
        PatchField::Set(v) => target.max_bytes = v,
    }
    match patch.format {
        PatchField::Unset => {}
        PatchField::Clear => target.format = Default::default(),
        PatchField::Set(v) => target.format = v,
    }
    match patch.sections {
        PatchField::Unset => {}
        PatchField::Clear => target.sections.clear(),
        PatchField::Set(v) => target.sections = v,
    }
}

fn build_gates_replace(items: Vec<RawGateItem>) -> Result<Vec<WorkingGate>, StateError> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            RawGateItem::Delete(sel) => {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("gate deletion '{sel}: null' is only allowed with 'gates_merge: patch'"),
                ));
            }
            RawGateItem::Gate(raw) => {
                if raw.after.is_some() || raw.before.is_some() {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "'after'/'before' gate positioning is only allowed with 'gates_merge: patch'",
                    ));
                }
                if raw.id.as_deref().is_some_and(|id| id.contains('.')) {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!(
                            "dotted gate cursor '{}' is only allowed with 'gates_merge: patch'",
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

fn raw_to_working_gate(raw: RawGateDef) -> Result<WorkingGate, StateError> {
    let mut gate = GateDef::default();
    if let Some(id) = raw.id {
        GateId::new(&id)?;
        gate.id = id;
    }
    let has_explicit_tools = matches!(raw.tools, PatchField::Set(_));
    let mut pending_tools_add = Vec::new();
    let mut pending_tools_remove = Vec::new();

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
        PatchField::Set(items) => build_gates_replace(items)?,
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
        gate.tools = v;
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
    if let PatchField::Set(ef_patch) = raw.expected_failure {
        let mut ef = Default::default();
        merge_expected_failure(&mut ef, ef_patch);
        gate.expected_failure = Some(ef);
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

    if gate.id.is_empty() && tasks_children.is_none() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "every gate entry must declare either 'id' or 'tasks'",
        ));
    }

    Ok(WorkingGate {
        gate,
        has_explicit_tools,
        pending_tools_add,
        pending_tools_remove,
        children,
        tasks_children,
    })
}

fn apply_gates_patch(
    target: &mut Vec<WorkingGate>,
    items: Vec<RawGateItem>,
) -> Result<(), StateError> {
    for item in items {
        match item {
            RawGateItem::Delete(selector) => {
                delete_gate_by_selector(target, &selector)?;
            }
            RawGateItem::Gate(raw) => {
                if raw.after.is_some() && raw.before.is_some() {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
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
                                    StateError::new(
                                        ErrorCode::ProfileSchemaInvalid,
                                        format!("patch target gate '{id_or_cursor}' not found (missing container '{head}')"),
                                    )
                                })?;
                            let mut nested = *raw;
                            nested.id = Some(rest.to_owned());
                            apply_gates_patch(&mut matched.children, vec![RawGateItem::Gate(Box::new(nested))])?;
                        }
                    } else {
                        let matched = target
                            .iter_mut()
                            .find(|g| g.gate.id == id_or_cursor)
                            .ok_or_else(|| {
                                StateError::new(
                                    ErrorCode::ProfileSchemaInvalid,
                                    format!(
                                        "patch target gate '{id_or_cursor}' does not exist in inherited profile; use 'after:'/'before:' to insert a new gate"
                                    ),
                                )
                            })?;
                        patch_existing_working_gate(matched, *raw)?;
                    }
                } else {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        "gate patch entry must declare 'id' or 'tasks'",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn delete_gate_by_selector(target: &mut Vec<WorkingGate>, selector: &str) -> Result<(), StateError> {
    if let Some((head, rest)) = selector.split_once('.') {
        if head == "tasks" {
            let mut matching_blocks: Vec<&mut WorkingTasksBlock> = target
                .iter_mut()
                .filter_map(|g| g.tasks_children.as_mut())
                .filter(|tb| contains_gate_path(&tb.gates, rest))
                .collect();
            if matching_blocks.len() != 1 {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("gate deletion target '{selector}' not found"),
                )
            })?;
        return delete_gate_by_selector(&mut parent.children, rest);
    }

    let before_len = target.len();
    target.retain(|g| g.gate.id != selector);
    if target.len() == before_len {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("insertion anchor '{anchor}' not found"),
                )
            })?;
        return insert_gate_relative(&mut parent.children, raw, rest, insert_after);
    }

    if raw.id.as_deref().is_some_and(|id| id.contains('.')) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("insertion anchor gate '{anchor}' does not exist"),
            )
        })?;

    let new_gate = raw_to_working_gate(raw)?;
    if !new_gate.gate.id.is_empty() && target.iter().any(|g| g.gate.id == new_gate.gate.id) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("inserted gate '{}' already exists at target level", new_gate.gate.id),
        ));
    }
    let idx = if insert_after { pos + 1 } else { pos };
    target.insert(idx, new_gate);
    Ok(())
}

fn patch_tasks_block(target: &mut [WorkingGate], raw: RawGateDef) -> Result<(), StateError> {
    let Some(raw_tb) = raw.tasks else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "patch '- tasks:' target with when={child_when:?} does not match any inherited tasks block"
            ),
        ));
    }
    if matches.len() > 1 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
    let mut matching_blocks: Vec<&mut WorkingTasksBlock> = target
        .iter_mut()
        .filter_map(|g| g.tasks_children.as_mut())
        .filter(|tb| contains_gate_path(&tb.gates, rest))
        .collect();
    if matching_blocks.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("patch target 'tasks.{rest}' not found in any inherited tasks block"),
        ));
    }
    if matching_blocks.len() > 1 {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
) -> Result<(), StateError> {
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
    match raw.gates {
        PatchField::Unset => {}
        PatchField::Clear => target.children.clear(),
        PatchField::Set(items) => apply_gates_patch(&mut target.children, items)?,
    }
    raw.use_template
        .apply_to_option(&mut g.use_template, Clone::clone);
    raw.when.apply_to_option(&mut g.when, Clone::clone);

    match raw.proof {
        PatchField::Unset => {}
        PatchField::Clear => g.proof = ProofDef::default(),
        PatchField::Set(v) => g.proof = v,
    }
    match raw.tools {
        PatchField::Unset => {}
        PatchField::Clear => {
            target.has_explicit_tools = false;
            g.tools.clear();
        }
        PatchField::Set(v) => {
            target.has_explicit_tools = true;
            g.tools = v;
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
    match raw.expected_failure {
        PatchField::Unset => {}
        PatchField::Clear => g.expected_failure = None,
        PatchField::Set(ef_patch) => {
            let ef = g.expected_failure.get_or_insert_with(Default::default);
            merge_expected_failure(ef, ef_patch);
        }
    }
    match raw.max_attempts {
        PatchField::Unset => {}
        PatchField::Clear => g.max_attempts = DEFAULT_MAX_ATTEMPTS,
        PatchField::Set(v) => g.max_attempts = v,
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

    Ok(())
}

fn merge_save_policy(target: &mut SavePolicy, patch: SavePolicyPatch) {
    patch.pre.apply_to_option(&mut target.pre, |a| *a);
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
        .git_commit_note
        .apply_to_option(&mut target.git_commit_note, Clone::clone);
}

fn merge_expected_failure(
    target: &mut super::schema::ExpectedFailureSpec,
    patch: super::schema::ExpectedFailurePatch,
) {
    match patch.exit_codes {
        PatchField::Unset => {}
        PatchField::Clear => target.exit_codes.clear(),
        PatchField::Set(v) => target.exit_codes = v,
    }
    patch
        .stderr_contains
        .apply_to_option(&mut target.stderr_contains, Clone::clone);
}

fn expand_templates_in_working(working: &mut WorkingProfile) -> Result<(), StateError> {
    let templates = working.resolved.gate_templates.clone();
    let mut stack = Vec::new();
    expand_gate_list(&mut working.gates, &templates, &mut stack)
}

fn expand_gate_list(
    gates: &mut [WorkingGate],
    templates: &BTreeMap<String, GateTemplate>,
    stack: &mut Vec<String>,
) -> Result<(), StateError> {
    for gate in gates.iter_mut() {
        if let Some(tpl_name) = gate.gate.use_template.clone() {
            if stack.contains(&tpl_name) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("cyclic gate_templates expansion detected at '{tpl_name}'"),
                ));
            }
            let tpl = templates.get(&tpl_name).ok_or_else(|| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
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
                if child.gate.command.is_none() {
                    child.gate.command = tpl.command.clone();
                }
                if child.gate.guidance.is_empty() && !tpl.guidance.is_empty() {
                    child.gate.guidance = tpl.guidance.clone();
                }
                if let Some(max_att) = tpl.max_attempts {
                    if child.gate.max_attempts == DEFAULT_MAX_ATTEMPTS {
                        child.gate.max_attempts = max_att;
                    }
                }
            }
            expand_gate_list(&mut tpl_children, templates, stack)?;
            if gate.children.is_empty() {
                gate.children = tpl_children;
            }
            if gate.gate.command.is_none() {
                gate.gate.command = tpl.command.clone();
            }
            if gate.gate.guidance.is_empty() && !tpl.guidance.is_empty() {
                gate.gate.guidance = tpl.guidance.clone();
            }
            if let Some(max_att) = tpl.max_attempts {
                if gate.gate.max_attempts == DEFAULT_MAX_ATTEMPTS {
                    gate.gate.max_attempts = max_att;
                }
            }
            stack.pop();
        }
        expand_gate_list(&mut gate.children, templates, stack)?;
        if let Some(tb) = &mut gate.tasks_children {
            expand_gate_list(&mut tb.gates, templates, stack)?;
        }
    }
    Ok(())
}
