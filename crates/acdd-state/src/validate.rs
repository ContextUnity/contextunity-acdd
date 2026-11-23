//! Single canonical workspace, specification, profile, and dependency validator.
//!
//! Owned by `acdd-state` so `acdd validate`, `acdd start`, `acdd draft`, `acdd sync`,
//! `StateQuery::Validate`, and `docs check` share one set of structural and fail-closed checks.

use crate::machine::{effective_task_tags, matches_when};
use crate::profile::{
    resolve_optional_profile_ref_with_dirs, resolve_profile_from_str_with_dirs,
    resolve_profile_ref_with_dirs, validate_profile, ProfileValidationOptions,
    ResolvedGateProfile,
};
use crate::spec::{parse_spec, SpecDocument, SpecStatus};
use crate::store::sqlite::SqliteStateReader;
use crate::store::traits::StateReader;
use crate::types::{NodeStatus, RunStatus};
use crate::usecases::deps::{collect_workspace_specs, validate_spec_dependencies};
use crate::usecases::dto::{ValidateDiagnostic, ValidateResp, ValidateSummary};
use crate::usecases::lifecycle::{
    effective_modules_for_profile, effective_spec_collections, match_spec_collection,
    read_confined_utf8, validate_profile_fail_closed,
};
use crate::usecases::config::StateEngineConfig;
use acdd_core::config::{load_adapter_report, Adapter, SpecCollectionConfig, ACDD_CONFIG_FILE, ACDD_DIR};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::checked_child;
use acdd_core::path_policy::{is_valid_scope_glob, scopes_overlap};
use acdd_core::ports::CompositeDomainProvider;
use acdd_core::subsystems::SubsystemId;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Validate all specs, profiles, `acdd.yaml`, and active constraints across a workspace root.
pub fn validate_workspace(
    root: &Path,
    explicit_config: Option<&Path>,
    explicit_profile: Option<&str>,
    providers: &CompositeDomainProvider,
) -> Result<ValidateResp, StateError> {
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
    let profile_rel = format!("{ACDD_DIR}/profile.yaml");
    let has_workspace_profile = root.join(&profile_rel).is_file();
    let profile_diag_path = if let Some(explicit) = explicit_profile {
        explicit.to_owned()
    } else if has_workspace_profile {
        profile_rel
    } else {
        "embedded:code".to_owned()
    };

    let active_profile = match resolve_optional_profile_ref_with_dirs(
        explicit_profile,
        Some(root),
        Some(&adapter.modules),
        Some(&adapter.profiles_dirs),
    ) {
        Ok(resolved) => {
            let opts = ProfileValidationOptions {
                domain_providers: Some(providers),
                acdd_modules: Some(&adapter.modules),
                require_bound_commands: require_bound,
            };
            match validate_profile(&resolved, &opts) {
                Ok(()) => {
                    diagnostics.push(pass(
                        &profile_diag_path,
                        "PROFILE_VALID",
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
    };

    let state_db_path = crate::usecases::deps::try_resolve_effective_state_db(root, None)?;
    let (scanned_specs, scan_failed_count) = scan_collection_specs(root, &adapter, &mut diagnostics);
    let active_docs: Vec<SpecDocument> = scanned_specs.iter().map(|s| s.doc.clone()).collect();
    let docs_count = scanned_specs.len() + scan_failed_count;

    for entry in &scanned_specs {
        validate_parsed_spec(
            root,
            &adapter,
            Some(&state_db_path),
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
) -> Result<ValidateResp, StateError> {
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

    let profile_rel = format!("{ACDD_DIR}/profile.yaml");
    match path_kind(&abs_path) {
        Some(DocumentKind::Profile) => {
            let require_bound = adapter.modules.is_enabled(SubsystemId::State.as_str())
                && (rel_display == profile_rel || explicit_profile.is_some());
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
            let has_workspace_profile = root.join(&profile_rel).is_file();
            let profile_diag_path = if let Some(explicit) = explicit_profile {
                explicit
            } else if has_workspace_profile {
                profile_rel.as_str()
            } else {
                "embedded:code"
            };
            let active_profile = match resolve_optional_profile_ref_with_dirs(
                explicit_profile,
                Some(root),
                Some(&adapter.modules),
                Some(&adapter.profiles_dirs),
            ) {
                Ok(p) => Some(p),
                Err(err) => {
                    diagnostics.push(fail_state_err(profile_diag_path, &err, 1, 1));
                    None
                }
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
            let active_docs: Vec<SpecDocument> =
                scanned_specs.into_iter().map(|s| s.doc).collect();
            let col = match_spec_collection(&adapter.specs, &rel_display)
                .and_then(|k| adapter.specs.get(&k));
            let state_db_path = crate::usecases::deps::try_resolve_effective_state_db(root, None)?;
            validate_parsed_spec(
                root,
                &adapter,
                Some(&state_db_path),
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
    profile: &ResolvedGateProfile,
) -> Result<(), StateError> {
    if matches!(doc.status, SpecStatus::Completed | SpecStatus::Cancelled) {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
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
        profiles_dirs: config.profiles_dirs.clone(),
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
        return Err(StateError::new(first.code, first.message));
    }

    crate::usecases::deps::verify_run_level_depends_on(config, reader, doc)?;

    validate_tasks_against_profile(doc, profile)?;
    Ok(())
}

fn load_strict_adapter(
    root: &Path,
    explicit_config: Option<&Path>,
) -> Result<Adapter, ValidateDiagnostic> {
    let config_label = explicit_config
        .and_then(|p| p.strip_prefix(root).ok().or(Some(p)))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| ACDD_CONFIG_FILE.to_owned());
    match load_adapter_report(root, explicit_config) {
        Ok(report) => {
            if !report.linked_workspace_errors.is_empty() {
                return Err(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    &config_label,
                    1,
                    1,
                    format!(
                        "invalid linked_workspaces in {ACDD_CONFIG_FILE}: {}",
                        report.linked_workspace_errors.join("; ")
                    ),
                ));
            }
            Ok(report.adapter)
        }
        Err(err) => {
            if let Some(se) = err.get_ref().and_then(|e| e.downcast_ref::<StateError>()) {
                Err(fail_state_err(&config_label, se, 1, 1))
            } else {
                Err(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    &config_label,
                    1,
                    1,
                    format!("invalid {ACDD_CONFIG_FILE}: {err}"),
                ))
            }
        }
    }
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
        .unwrap_or("profile");
    let resolved = match resolve_profile_from_str_with_dirs(
        content,
        profile_id,
        Some(path),
        Some(root),
        Some(&adapter.modules),
        Some(&adapter.profiles_dirs),
    ) {
        Ok(resolved) => resolved,
        Err(err) => {
            diagnostics.push(fail_state_err(path, &err, 1, 1));
            return;
        }
    };
    let opts = ProfileValidationOptions {
        domain_providers: Some(providers),
        acdd_modules: Some(&adapter.modules),
        require_bound_commands,
    };
    match validate_profile(&resolved, &opts) {
        Ok(()) => diagnostics.push(pass(
            path,
            "PROFILE_VALID",
            format!(
                "validated {} gates and {} registered commands",
                resolved.gates.len(),
                resolved.commands.len()
            ),
        )),
        Err(err) => diagnostics.push(fail_state_err(path, &err, 1, 1)),
    }
}

struct ScannedSpec {
    rel_path: String,
    col: Option<SpecCollectionConfig>,
    doc: SpecDocument,
}

fn scan_collection_specs(
    root: &Path,
    adapter: &Adapter,
    diagnostics: &mut Vec<ValidateDiagnostic>,
) -> (Vec<ScannedSpec>, usize) {
    let scan = collect_workspace_specs(root, &adapter.specs);
    let failed_count = scan.errors.len();
    for err in scan.errors {
        diagnostics.push(fail(err.diag_code, &err.path, 1, 1, err.message));
    }
    let specs = scan
        .specs
        .into_iter()
        .map(|s| ScannedSpec {
            rel_path: s.rel_path,
            col: s.collection,
            doc: s.doc,
        })
        .collect();
    (specs, failed_count)
}

#[allow(clippy::too_many_arguments)]
fn validate_parsed_spec(
    root: &Path,
    adapter: &Adapter,
    state_db_path: Option<&Path>,
    providers: &CompositeDomainProvider,
    workspace_profile: Option<&ResolvedGateProfile>,
    col: Option<&SpecCollectionConfig>,
    rel_path: &str,
    spec: &SpecDocument,
    active_docs: &[SpecDocument],
    diagnostics: &mut Vec<ValidateDiagnostic>,
    warnings: &mut Vec<Value>,
) {
    let mut doc_failed = false;

    if matches!(spec.status, SpecStatus::Completed | SpecStatus::Cancelled) {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            rel_path,
            1,
            1,
            format!(
                "spec '{}' has terminal status '{:?}' in active collection directory; move to archive/",
                spec.id, spec.status
            ),
        ));
        doc_failed = true;
    }

    if let Some(col_cfg) = col {
        validate_spec_id_against_collection(
            rel_path,
            &spec.id,
            col_cfg,
            diagnostics,
            warnings,
            &mut doc_failed,
        );
    }

    if spec.status == SpecStatus::Planned {
        validate_planned_spec_task_statuses(
            state_db_path,
            rel_path,
            spec,
            diagnostics,
            &mut doc_failed,
        );
    }

    let effective_profile_ref = spec
        .profile
        .as_deref()
        .or_else(|| col.and_then(|c| c.profile.as_deref()));

    let resolved_owned;
    let effective_profile = if let Some(pref) = effective_profile_ref {
        match resolve_profile_ref_with_dirs(
            pref,
            Some(root),
            Some(&adapter.modules),
            Some(&adapter.profiles_dirs),
        ) {
            Ok(p) => {
                let effective_mods =
                    effective_modules_for_profile(&adapter.modules, &p.modules);
                let opts = ProfileValidationOptions {
                    domain_providers: Some(providers),
                    acdd_modules: Some(&effective_mods),
                    require_bound_commands: effective_mods.is_enabled(SubsystemId::State.as_str()),
                };
                if let Err(err) = validate_profile(&p, &opts) {
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

    if let Some(profile) = effective_profile {
        if let Err(err) = validate_tasks_against_profile(spec, profile) {
            diagnostics.push(fail_state_err(rel_path, &err, 1, 1));
            doc_failed = true;
        }
    } else {
        // Still validate task scope syntax even when no profile resolved.
        for task in &spec.tasks {
            for pat in task
                .scope
                .iter()
                .chain(task.extended_scope.iter().map(|e| &e.path))
            {
                if !is_valid_scope_glob(pat) {
                    diagnostics.push(fail(
                        ErrorCode::ProfileSchemaInvalid.as_str(),
                        rel_path,
                        1,
                        1,
                        format!(
                            "task '{}' has invalid or non-normalized scope glob '{pat}'",
                            task.task_ref
                        ),
                    ));
                    doc_failed = true;
                }
            }
        }
    }

    if let Some(ref guidance) = spec.guidance {
        let empty_guidance = matches!(guidance, serde_yaml::Value::Null)
            || guidance.as_str().is_some_and(|s| s.trim().is_empty())
            || guidance.as_mapping().is_some_and(|m| m.is_empty())
            || guidance.as_sequence().is_some_and(|s| s.is_empty());
        if empty_guidance {
            warnings.push(json!({"path": rel_path, "message": "spec guidance block is empty"}));
        }
    }

    let state_db_override = state_db_path.map(PathBuf::from);
    for issue in validate_spec_dependencies(
        root,
        adapter,
        None,
        state_db_override.as_deref(),
        spec,
        active_docs,
    ) {
        diagnostics.push(fail(
            issue.code.as_str(),
            rel_path,
            1,
            1,
            issue.message,
        ));
        doc_failed = true;
    }

    if !doc_failed {
        diagnostics.push(pass(
            rel_path,
            "SPEC_VALID",
            format!("validated spec '{}' ({} tasks)", spec.id, spec.tasks.len()),
        ));
    }
}

fn validate_spec_id_against_collection(
    rel_path: &str,
    spec_id: &str,
    col: &SpecCollectionConfig,
    diagnostics: &mut Vec<ValidateDiagnostic>,
    warnings: &mut Vec<Value>,
    doc_failed: &mut bool,
) {
    if col.prefixes.is_empty() {
        return;
    }
    let Some((prefix, rest)) = spec_id.split_once('-') else {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            rel_path,
            1,
            1,
            format!(
                "spec id '{spec_id}' must start with one of {:?}-<NNN>",
                col.prefixes
            ),
        ));
        *doc_failed = true;
        return;
    };

    if !col
        .prefixes
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(prefix))
    {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            rel_path,
            1,
            1,
            format!(
                "spec id '{spec_id}' prefix '{prefix}' does not match collection prefixes {:?}",
                col.prefixes
            ),
        ));
        *doc_failed = true;
        return;
    }

    let digit_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_len == 0 {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            rel_path,
            1,
            1,
            format!("spec id '{spec_id}' is missing numeric suffix after prefix '{prefix}-'"),
        ));
        *doc_failed = true;
        return;
    }
    if digit_len != col.digits {
        diagnostics.push(fail(
            ErrorCode::ProfileSchemaInvalid.as_str(),
            rel_path,
            1,
            1,
            format!(
                "spec id '{spec_id}' numeric padding has {digit_len} digit(s), expected {}",
                col.digits
            ),
        ));
        *doc_failed = true;
        return;
    }

    if !col.prefixes.iter().any(|allowed| allowed == prefix) {
        warnings.push(json!({
            "path": rel_path,
            "code": "SPEC_PREFIX_CASE",
            "message": format!("spec id '{spec_id}' prefix '{prefix}' differs in case from {:?}", col.prefixes)
        }));
    }
}

fn load_reconciled_task_statuses(
    state_db_path: Option<&Path>,
    rel_path: &str,
    spec_id: &str,
) -> BTreeMap<String, NodeStatus> {
    let mut reconciled_statuses = BTreeMap::new();
    let Some(db_path) = state_db_path.filter(|p| p.is_file()) else {
        return reconciled_statuses;
    };
    let reader = SqliteStateReader::new(db_path);
    let Ok(runs) = crate::usecases::lifecycle::list_all_runs(&reader, Some(RunStatus::Active))
    else {
        return reconciled_statuses;
    };
    let Some(run) = runs
        .into_iter()
        .find(|r| r.spec_id.as_deref() == Some(spec_id) || r.spec_path.as_deref() == Some(rel_path))
    else {
        return reconciled_statuses;
    };
    let Ok(Some(snap)) = reader.load_run_graph(&run.run_id) else {
        return reconciled_statuses;
    };
    for node in snap.nodes {
        if let Some(tref) = node.task_ref {
            if node.kind == crate::types::NodeKind::Task {
                reconciled_statuses.insert(tref.as_str().to_owned(), node.status);
            }
        }
    }
    reconciled_statuses
}

fn validate_planned_spec_task_statuses(
    state_db_path: Option<&Path>,
    rel_path: &str,
    spec: &SpecDocument,
    diagnostics: &mut Vec<ValidateDiagnostic>,
    doc_failed: &mut bool,
) {
    let advanced: Vec<_> = spec
        .tasks
        .iter()
        .filter(|t| matches!(t.status, SpecStatus::Active | SpecStatus::Completed))
        .collect();
    if advanced.is_empty() {
        return;
    }

    // Check if an active Run in state.sqlite reconciles these task statuses.
    let reconciled_statuses = load_reconciled_task_statuses(state_db_path, rel_path, &spec.id);

    for task in advanced {
        let reconciled = matches!(
            (
                task.status,
                reconciled_statuses.get(task.task_ref.as_str()).copied(),
            ),
            (SpecStatus::Completed, Some(NodeStatus::Completed))
                | (SpecStatus::Active, Some(NodeStatus::InProgress | NodeStatus::Ready))
        );
        if !reconciled {
            diagnostics.push(fail(
                ErrorCode::ProfileSchemaInvalid.as_str(),
                rel_path,
                1,
                1,
                format!(
                    "planned spec '{}' has task '{}' with unreconciled status '{:?}'",
                    spec.id, task.task_ref, task.status
                ),
            ));
            *doc_failed = true;
        }
    }
}

fn validate_tasks_against_profile(
    spec: &SpecDocument,
    profile: &ResolvedGateProfile,
) -> Result<(), StateError> {
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "task '{}' matched {matched_blocks} '- tasks:' blocks in profile '{}' (expected 1)",
                        task.task_ref, profile.profile_id
                    ),
                ));
            }
            if matched_gates == 0 {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "task '{}' matched zero task gates in profile '{}'",
                        task.task_ref, profile.profile_id
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "task '{}' has invalid or non-normalized scope glob '{pat}'",
                        task.task_ref
                    ),
                ));
            }
            if !profile.protected_paths.is_empty()
                && scopes_overlap(std::slice::from_ref(pat), &profile.protected_paths)
            {
                return Err(StateError::new(
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

fn pass(path: &str, code: &str, message: String) -> ValidateDiagnostic {
    ValidateDiagnostic {
        status: "pass".to_owned(),
        code: code.to_owned(),
        path: path.to_owned(),
        line: 1,
        column: 1,
        message,
    }
}

fn fail(code: &str, path: &str, line: usize, column: usize, message: String) -> ValidateDiagnostic {
    ValidateDiagnostic {
        status: "fail".to_owned(),
        code: code.to_owned(),
        path: path.to_owned(),
        line,
        column,
        message,
    }
}

fn fail_state_err(path: &str, err: &StateError, line: usize, column: usize) -> ValidateDiagnostic {
    fail(err.code.as_str(), path, line, column, err.to_string())
}

fn build_resp(
    scope: &str,
    diagnostics: Vec<ValidateDiagnostic>,
    warnings: Vec<Value>,
    documents_validated: usize,
) -> ValidateResp {
    let failed = diagnostics.iter().filter(|d| d.status == "fail").count();
    let passed = diagnostics.iter().filter(|d| d.status == "pass").count();
    ValidateResp {
        valid: failed == 0,
        scope: scope.to_owned(),
        summary: ValidateSummary {
            passed,
            failed,
            warnings: warnings.len(),
            documents_validated,
        },
        diagnostics,
        warnings,
    }
}
