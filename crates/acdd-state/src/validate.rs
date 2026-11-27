//! Single canonical workspace, specification, profile, and dependency validator.
//!
//! Owned by `acdd-state` so `acdd validate`, `acdd start`, `acdd draft`, `acdd sync`,
//! `StateQuery::Validate`, and `docs check` share structural, profile, and state-backed checks.

use crate::machine::{effective_task_tags, matches_when};
use crate::workflow::{
    resolve_optional_workflow_ref_with_dirs, resolve_workflow_from_str_with_dirs,
    resolve_workflow_ref_with_dirs, validate_workflow, ResolvedWorkflow, WorkflowValidationOptions,
};
use crate::store::traits::StateReader;
use crate::usecases::config::StateEngineConfig;
use crate::usecases::deps::try_resolve_effective_state_db;
use crate::usecases::dto::{ValidateDiagnostic, ValidateResp};
use crate::usecases::lifecycle::{
    effective_modules_for_profile, effective_spec_collections, validate_profile_fail_closed,
};
use crate::usecases::overlay::{validate_spec_dependencies, StateRuntimeOverlay};
use acdd_core::config::{Adapter, SpecConfig, ACDD_CONFIG_FILE, ACDD_DIR};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::fs::checked_child;
use acdd_core::path_policy::{is_valid_scope_glob, scopes_overlap};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
use acdd_spec::discovery::{match_spec_collection, read_confined_utf8};
use acdd_spec::overlay::{NoopRuntimeOverlay, SpecRuntimeOverlay};
use acdd_spec::validate::{
    build_resp, fail, fail_state_err, load_strict_adapter, pass, scan_collection_specs,
    validate_spec_static_checks, ScannedSpec,
};
use acdd_spec::{parse_spec, SpecDocument, SpecStatus};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn resolve_validation_state_db(
    root: &Path,
    explicit_config: Option<&Path>,
) -> Result<Option<PathBuf>, AcddError> {
    if explicit_config.is_some() {
        return Ok(None);
    }
    match try_resolve_effective_state_db(root, None) {
        Ok(path) => Ok(Some(path)),
        Err(err) if err.code == ErrorCode::WorkflowSchemaInvalid => {
            let root_canon = acdd_core::fs::canonical_root(root).unwrap_or_else(|_| root.to_path_buf());
            let primary = acdd_core::git::primary_worktree_root(&root_canon)
                .unwrap_or_else(|| root_canon.clone());
            if primary != root_canon {
                Ok(None)
            } else {
                Err(err)
            }
        }
        Err(err) => Err(err),
    }
}

/// Validate all specs, workflows, `acdd.yaml`, and active constraints across a workspace root.
pub fn validate_workspace(
    root: &Path,
    explicit_config: Option<&Path>,
    explicit_profile: Option<&str>,
    providers: &CompositeDomainProvider,
) -> Result<ValidateResp, AcddError> {
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();

    let adapter = match load_strict_adapter(root, explicit_config) {
        Ok(adapter) => adapter,
        Err(diag) => {
            diagnostics.push(diag);
            return Ok(build_resp("workspace", diagnostics, warnings, 0));
        }
    };

    let has_config = explicit_config.is_some() || root.join(ACDD_CONFIG_FILE).is_file();
    if has_config {
        let config_path_label = explicit_config
            .and_then(|p| p.strip_prefix(root).ok().or(Some(p)))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| ACDD_CONFIG_FILE.to_owned());
        diagnostics.push(pass(
            &config_path_label,
            "CONFIG_VALID",
            format!(
                "validated {ACDD_CONFIG_FILE} ({} spec collections, {} linked workspaces)",
                adapter.specs.len(),
                adapter.linked_workspaces.len()
            ),
        ));
    }

    let require_bound = adapter.modules.is_enabled(SubsystemId::State.as_str());
    let workflow_rel = format!("{ACDD_DIR}/workflows/default.yaml");
    let has_workspace_workflow = root.join(&workflow_rel).is_file();
    let profile_diag_path = if let Some(explicit) = explicit_profile {
        explicit.to_owned()
    } else if has_workspace_workflow {
        workflow_rel
    } else {
        "embedded:code".to_owned()
    };

    let active_profile = if require_bound || has_workspace_workflow || explicit_profile.is_some() {
        match resolve_optional_workflow_ref_with_dirs(
            explicit_profile,
            Some(root),
            Some(&adapter.modules),
            Some(&adapter.workflows_dirs),
        ) {
            Ok(resolved) => {
                let opts = WorkflowValidationOptions {
                    domain_providers: Some(providers),
                    acdd_modules: Some(&adapter.modules),
                    require_bound_commands: require_bound,
                };
                match validate_workflow(&resolved, &opts) {
                    Ok(()) => {
                        diagnostics.push(pass(
                            &profile_diag_path,
                            "WORKFLOW_VALID",
                            format!(
                                "validated {} gates and {} commands",
                                resolved.gates.len(),
                                resolved.commands.len()
                            ),
                        ));
                        Some(resolved)
                    }
                    Err(err) => {
                        diagnostics.push(fail_state_err(&profile_diag_path, &err, 1, 1));
                        None
                    }
                }
            }
            Err(err) => {
                diagnostics.push(fail_state_err(&profile_diag_path, &err, 1, 1));
                None
            }
        }
    } else {
        None
    };

    let noop = NoopRuntimeOverlay;
    let state_overlay;
    let overlay: &dyn SpecRuntimeOverlay = if require_bound {
        let state_db_override = resolve_validation_state_db(root, explicit_config)?;
        state_overlay = StateRuntimeOverlay::for_validation(
            root,
            &adapter,
            None,
            state_db_override.as_deref(),
        );
        &state_overlay
    } else {
        &noop
    };
    let (scanned_specs, scan_failed_count) =
        scan_collection_specs(root, &adapter, &mut diagnostics);
    let active_docs: Vec<SpecDocument> = scanned_specs.iter().map(|s| s.doc.clone()).collect();
    let docs_count = scanned_specs.len() + scan_failed_count;

    for entry in &scanned_specs {
        validate_parsed_spec(
            root,
            &adapter,
            overlay,
            providers,
            active_profile.as_ref(),
            entry.col.as_ref(),
            &entry.rel_path,
            &entry.doc,
            &active_docs,
            &mut diagnostics,
            &mut warnings,
        );
    }

    Ok(build_resp("workspace", diagnostics, warnings, docs_count))
}

/// Validate a single specification Markdown, profile YAML, or `acdd.yaml` document within a workspace root.
pub fn validate_document(
    root: &Path,
    relative: &Path,
    explicit_config: Option<&Path>,
    explicit_profile: Option<&str>,
    providers: &CompositeDomainProvider,
) -> Result<ValidateResp, AcddError> {
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();

    let Some(raw_rel) = relative.to_str() else {
        diagnostics.push(fail(
            "VALIDATION_PATH_INVALID",
            "<path>",
            1,
            1,
            "validation path must be valid UTF-8".to_owned(),
        ));
        return Ok(build_resp("document:<invalid>", diagnostics, warnings, 1));
    };
    let rel_norm = raw_rel.replace('\\', "/");
    let rel_display = rel_norm.strip_prefix("./").unwrap_or(&rel_norm).to_owned();
    let scope_label = format!("document:{rel_display}");

    let abs_path = match checked_child(root, Path::new(&rel_display)) {
        Ok(p) => p,
        Err(err) => {
            diagnostics.push(fail(
                "VALIDATION_PATH_INVALID",
                &rel_display,
                1,
                1,
                format!("path confinement check failed: {err}"),
            ));
            return Ok(build_resp(&scope_label, diagnostics, warnings, 1));
        }
    };

    let file_name = abs_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if file_name == ACDD_CONFIG_FILE {
        match load_strict_adapter(root, Some(&abs_path)) {
            Ok(loaded) => diagnostics.push(pass(
                &rel_display,
                "CONFIG_VALID",
                format!(
                    "validated {ACDD_CONFIG_FILE} ({} spec collections, {} linked workspaces)",
                    loaded.specs.len(),
                    loaded.linked_workspaces.len()
                ),
            )),
            Err(diag) => diagnostics.push(diag),
        }
        return Ok(build_resp(&scope_label, diagnostics, warnings, 1));
    }

    let adapter = match load_strict_adapter(root, explicit_config) {
        Ok(adapter) => adapter,
        Err(diag) => {
            diagnostics.push(diag);
            return Ok(build_resp(&scope_label, diagnostics, warnings, 1));
        }
    };

    let content = match read_confined_utf8(root, &rel_display) {
        Ok(c) => c,
        Err(err) => {
            diagnostics.push(fail(
                "VALIDATION_IO",
                &rel_display,
                1,
                1,
                format!("unable to read validation input: {err}"),
            ));
            return Ok(build_resp(&scope_label, diagnostics, warnings, 1));
        }
    };

    let workflow_rel = format!("{ACDD_DIR}/workflows/default.yaml");
    match path_kind(&abs_path) {
        Some(DocumentKind::Profile) => {
            let require_bound = adapter.modules.is_enabled(SubsystemId::State.as_str())
                && (rel_display == workflow_rel || explicit_profile.is_some());
            validate_profile_doc(
                root,
                &rel_display,
                &content,
                &adapter,
                providers,
                require_bound,
                &mut diagnostics,
            );
        }
        Some(DocumentKind::Spec) => {
            let doc = match parse_spec(&content) {
                Ok(doc) => doc,
                Err(err) => {
                    diagnostics.push(fail_state_err(&rel_display, &err, 1, 1));
                    return Ok(build_resp(&scope_label, diagnostics, warnings, 1));
                }
            };
            let state_enabled = adapter.modules.is_enabled(SubsystemId::State.as_str());
            let has_workspace_profile = root.join(&workflow_rel).is_file();
            let profile_diag_path = if let Some(explicit) = explicit_profile {
                explicit
            } else if has_workspace_profile {
                workflow_rel.as_str()
            } else {
                "embedded:code"
            };
            let active_profile =
                if state_enabled || has_workspace_profile || explicit_profile.is_some() {
                    match resolve_optional_workflow_ref_with_dirs(
                        explicit_profile,
                        Some(root),
                        Some(&adapter.modules),
                        Some(&adapter.workflows_dirs),
                    ) {
                        Ok(p) => Some(p),
                        Err(err) => {
                            diagnostics.push(fail_state_err(profile_diag_path, &err, 1, 1));
                            None
                        }
                    }
                } else {
                    None
                };
            let mut sink = Vec::new();
            let (mut scanned_specs, _) = scan_collection_specs(root, &adapter, &mut sink);
            if let Some(pos) = scanned_specs.iter().position(|e| e.rel_path == rel_display) {
                scanned_specs[pos].doc = doc.clone();
            } else {
                scanned_specs.push(ScannedSpec {
                    rel_path: rel_display.clone(),
                    col: match_spec_collection(&adapter.specs, &rel_display)
                        .and_then(|k| adapter.specs.get(&k).cloned()),
                    doc: doc.clone(),
                });
            }
            let active_docs: Vec<SpecDocument> = scanned_specs.into_iter().map(|s| s.doc).collect();
            let col = match_spec_collection(&adapter.specs, &rel_display)
                .and_then(|k| adapter.specs.get(&k));
            let noop = NoopRuntimeOverlay;
            let state_overlay;
            let overlay: &dyn SpecRuntimeOverlay =
                if adapter.modules.is_enabled(SubsystemId::State.as_str()) {
                    let state_db_override = resolve_validation_state_db(root, explicit_config)?;
                    state_overlay = StateRuntimeOverlay::for_validation(
                        root,
                        &adapter,
                        None,
                        state_db_override.as_deref(),
                    );
                    &state_overlay
                } else {
                    &noop
                };
            validate_parsed_spec(
                root,
                &adapter,
                overlay,
                providers,
                active_profile.as_ref(),
                col,
                &rel_display,
                &doc,
                &active_docs,
                &mut diagnostics,
                &mut warnings,
            );
        }
        None => diagnostics.push(fail(
            "VALIDATION_KIND_UNSUPPORTED",
            &rel_display,
            1,
            1,
            "expected a .md, .yaml, .yml, .yaml.example, or .yml.example document".to_owned(),
        )),
    }

    Ok(build_resp(&scope_label, diagnostics, warnings, 1))
}

/// Fail-closed gate validation invoked by `acdd start` and `acdd sync` before run mutation.
pub fn validate_spec_for_run(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    provider: &CompositeDomainProvider,
    doc: &SpecDocument,
    profile: &ResolvedWorkflow,
) -> Result<(), AcddError> {
    if matches!(doc.status, SpecStatus::Completed | SpecStatus::Cancelled) {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "cannot start or sync spec '{}' with terminal status '{:?}'",
                doc.id, doc.status
            ),
        ));
    }

    validate_profile_fail_closed(config, profile, provider, true)?;

    let source = &config.worktree;
    let adapter = Adapter {
        specs: effective_spec_collections(config),
        workflows_dirs: config.workflows_dirs.clone(),
        modules: config.modules.clone(),
        linked_workspaces: config
            .linked_workspaces
            .iter()
            .map(|(name, path)| acdd_core::config::LinkedWorkspace {
                name: name.clone(),
                path: path.clone(),
                roots: Vec::new(),
                ignored_names: Default::default(),
            })
            .collect(),
        ..Adapter::default()
    };

    let mut sink = Vec::new();
    let (scanned_specs, _) = scan_collection_specs(source, &adapter, &mut sink);
    let mut active_docs: Vec<SpecDocument> = scanned_specs.into_iter().map(|s| s.doc).collect();
    if !active_docs.iter().any(|d| d.id == doc.id) {
        active_docs.push(doc.clone());
    }

    let dep_issues = validate_spec_dependencies(
        source,
        &adapter,
        Some(reader),
        Some(&config.resolved_state_db_path().0),
        doc,
        &active_docs,
    );
    if let Some(first) = dep_issues.into_iter().next() {
        return Err(AcddError::new(first.code, first.message));
    }

    crate::usecases::deps::verify_run_level_depends_on(config, reader, doc)?;

    validate_tasks_against_profile(doc, profile)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum DocumentKind {
    Spec,
    Profile,
}

fn path_kind(path: &Path) -> Option<DocumentKind> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".yaml.example") || name.ends_with(".yml.example") {
        return Some(DocumentKind::Profile);
    }
    match path.extension()?.to_str()? {
        "md" => Some(DocumentKind::Spec),
        "yaml" | "yml" => Some(DocumentKind::Profile),
        _ => None,
    }
}

fn validate_profile_doc(
    root: &Path,
    path: &str,
    content: &str,
    adapter: &Adapter,
    providers: &CompositeDomainProvider,
    require_bound_commands: bool,
    diagnostics: &mut Vec<ValidateDiagnostic>,
) {
    let profile_id = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("workflow");
    let resolved = match resolve_workflow_from_str_with_dirs(
        content,
        profile_id,
        Some(path),
        Some(root),
        Some(&adapter.modules),
        Some(&adapter.workflows_dirs),
    ) {
        Ok(resolved) => resolved,
        Err(err) => {
            diagnostics.push(fail_state_err(path, &err, 1, 1));
            return;
        }
    };
    let opts = WorkflowValidationOptions {
        domain_providers: Some(providers),
        acdd_modules: Some(&adapter.modules),
        require_bound_commands,
    };
    match validate_workflow(&resolved, &opts) {
        Ok(()) => diagnostics.push(pass(
            path,
            "WORKFLOW_VALID",
            format!(
                "validated {} gates and {} registered commands",
                resolved.gates.len(),
                resolved.commands.len()
            ),
        )),
        Err(err) => diagnostics.push(fail_state_err(path, &err, 1, 1)),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_parsed_spec(
    root: &Path,
    adapter: &Adapter,
    overlay: &dyn SpecRuntimeOverlay,
    providers: &CompositeDomainProvider,
    workspace_profile: Option<&ResolvedWorkflow>,
    col: Option<&SpecConfig>,
    rel_path: &str,
    spec: &SpecDocument,
    active_docs: &[SpecDocument],
    diagnostics: &mut Vec<ValidateDiagnostic>,
    warnings: &mut Vec<Value>,
) {
    let effective_profile_ref = spec
        .workflow
        .as_deref()
        .or_else(|| col.and_then(|c| c.workflow.as_deref()));

    let mut doc_failed = false;
    let resolved_owned;
    let effective_profile = if let Some(pref) = effective_profile_ref {
        match resolve_workflow_ref_with_dirs(
            pref,
            Some(root),
            Some(&adapter.modules),
            Some(&adapter.workflows_dirs),
        ) {
            Ok(p) => {
                let effective_mods = effective_modules_for_profile(&adapter.modules, &p.modules);
                let opts = WorkflowValidationOptions {
                    domain_providers: Some(providers),
                    acdd_modules: Some(&effective_mods),
                    require_bound_commands: effective_mods.is_enabled(SubsystemId::State.as_str()),
                };
                if let Err(err) = validate_workflow(&p, &opts) {
                    diagnostics.push(fail_state_err(rel_path, &err, 1, 1));
                    doc_failed = true;
                }
                resolved_owned = p;
                Some(&resolved_owned)
            }
            Err(err) => {
                diagnostics.push(fail_state_err(rel_path, &err, 1, 1));
                doc_failed = true;
                workspace_profile
            }
        }
    } else {
        workspace_profile
    };

    doc_failed |= validate_spec_static_checks(
        root,
        adapter,
        overlay,
        col,
        rel_path,
        spec,
        active_docs,
        effective_profile.is_none(),
        diagnostics,
        warnings,
    );

    if let Some(profile) = effective_profile {
        if let Err(err) = validate_tasks_against_profile(spec, profile) {
            diagnostics.push(fail_state_err(rel_path, &err, 1, 1));
            doc_failed = true;
        }
    }

    if !doc_failed {
        diagnostics.push(pass(
            rel_path,
            "SPEC_VALID",
            format!("validated spec '{}' ({} tasks)", spec.id, spec.tasks.len()),
        ));
    }
}

fn validate_tasks_against_profile(
    spec: &SpecDocument,
    profile: &ResolvedWorkflow,
) -> Result<(), AcddError> {
    for task in &spec.tasks {
        let eff_tags = effective_task_tags(&task.tags, &profile.tags)?;

        // Verify task matches exactly one `- tasks:` block and at least one task gate when profile defines tasks blocks.
        let mut matched_blocks = 0usize;
        let mut matched_gates = 0usize;
        for item in &profile.gates {
            if let Some(ref tb) = item.tasks {
                if matches_when(tb.when.as_deref(), &eff_tags, &profile.tags)? {
                    matched_blocks += 1;
                    for g in &tb.gates {
                        if matches_when(g.when.as_deref(), &eff_tags, &profile.tags)? {
                            matched_gates += 1;
                        }
                    }
                }
            }
        }
        if profile.gates.iter().any(|g| g.tasks.is_some()) {
            if matched_blocks != 1 {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "task '{}' matched {matched_blocks} '- tasks:' blocks in workflow '{}' (expected 1)",
                        task.task_ref, profile.workflow_id
                    ),
                ));
            }
            if matched_gates == 0 {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "task '{}' matched zero task gates in workflow '{}'",
                        task.task_ref, profile.workflow_id
                    ),
                ));
            }
        }

        for pat in task
            .scope
            .iter()
            .chain(task.extended_scope.iter().map(|e| &e.path))
        {
            if !is_valid_scope_glob(pat) {
                return Err(AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!(
                        "task '{}' has invalid or non-normalized scope glob '{pat}'",
                        task.task_ref
                    ),
                ));
            }
            if !profile.protected_paths.is_empty()
                && scopes_overlap(std::slice::from_ref(pat), &profile.protected_paths)
            {
                return Err(AcddError::new(
                    ErrorCode::OutOfScopeChanges,
                    format!(
                        "task '{}' scope '{pat}' overlaps protected_paths {:?}",
                        task.task_ref, profile.protected_paths
                    ),
                ));
            }
        }
    }
    Ok(())
}
