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
use crate::usecases::StateEngineConfig;
use acdd_core::config::{load_adapter_report, Adapter, SpecCollectionConfig};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::checked_child;
use acdd_core::path_policy::{is_valid_scope_glob, scopes_overlap};
use acdd_core::ports::CompositeDomainProvider;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;
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

    let has_config = explicit_config.is_some() || root.join("acdd.yaml").is_file();
    if has_config {
        let config_path_label = explicit_config
            .and_then(|p| p.strip_prefix(root).ok().or(Some(p)))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| "acdd.yaml".to_owned());
        diagnostics.push(pass(
            &config_path_label,
            "CONFIG_VALID",
            format!(
                "validated acdd.yaml ({} spec collections, {} linked workspaces)",
                adapter.specs.len(),
                adapter.linked_workspaces.len()
            ),
        ));
    }

    let require_bound = adapter.modules.is_enabled("state");
    let has_workspace_profile = root.join(".acdd/profile.yaml").is_file();
    let profile_diag_path = if let Some(explicit) = explicit_profile {
        explicit.to_owned()
    } else if has_workspace_profile {
        ".acdd/profile.yaml".to_owned()
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
    if file_name == "acdd.yaml" {
        match load_strict_adapter(root, Some(&abs_path)) {
            Ok(loaded) => diagnostics.push(pass(
                &rel_display,
                "CONFIG_VALID",
                format!(
                    "validated acdd.yaml ({} spec collections, {} linked workspaces)",
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

    match path_kind(&abs_path) {
        Some(DocumentKind::Profile) => {
            let require_bound = adapter.modules.is_enabled("state")
                && (rel_display == ".acdd/profile.yaml" || explicit_profile.is_some());
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
            let has_workspace_profile = root.join(".acdd/profile.yaml").is_file();
            let profile_diag_path = if let Some(explicit) = explicit_profile {
                explicit
            } else if has_workspace_profile {
                ".acdd/profile.yaml"
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
        .unwrap_or_else(|| "acdd.yaml".to_owned());
    match load_adapter_report(root, explicit_config) {
        Ok(report) => {
            if !report.linked_workspace_errors.is_empty() {
                return Err(fail(
                    ErrorCode::ProfileSchemaInvalid.as_str(),
                    &config_label,
                    1,
                    1,
                    format!(
                        "invalid linked_workspaces in acdd.yaml: {}",
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
                    format!("invalid acdd.yaml: {err}"),
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
                    require_bound_commands: effective_mods.is_enabled("state"),
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
    let mut reconciled_statuses: BTreeMap<String, NodeStatus> = BTreeMap::new();
    if let Some(db_path) = state_db_path.filter(|p| p.is_file()) {
        let reader = SqliteStateReader::new(db_path);
        if let Ok(runs) = crate::usecases::lifecycle::list_all_runs(&reader, Some(RunStatus::Active))
        {
            if let Some(run) = runs.into_iter().find(|r| {
                r.spec_id.as_deref() == Some(spec.id.as_str())
                    || r.spec_path.as_deref() == Some(rel_path)
            }) {
                if let Ok(Some(snap)) = reader.load_run_graph(&run.run_id) {
                    for node in snap.nodes {
                        if let Some(tref) = node.task_ref {
                            if node.kind == crate::types::NodeKind::Task {
                                reconciled_statuses.insert(tref.as_str().to_owned(), node.status);
                            }
                        }
                    }
                }
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::RunHarness;
    use crate::types::{
        GateNodeRecord, IsolationMode, NodeKind, RunGraphSnapshot, RunRecord, RunTombstoneRecord,
        StoragePreconditions, TerminalStatus,
    };
    use acdd_core::ids::{RunId, RunName, TaskRef};
    use acdd_core::ports::{DomainCapabilities, DomainProvider};
    use acdd_core::subsystems::SubsystemRegistry;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    struct DummyCodeDocsProvider(&'static str);

    impl DomainProvider for DummyCodeDocsProvider {
        fn domain_id(&self) -> &str {
            self.0
        }

        fn capabilities(&self) -> DomainCapabilities {
            match self.0 {
                "code" => DomainCapabilities {
                    checks: vec![acdd_core::ports::CheckCapability {
                        name: "seam".to_owned(),
                        input_schema: acdd_core::schema::SchemaDef::object(),
                    }],
                    context_lenses: BTreeSet::from([
                        "symbols".to_owned(),
                        "tests".to_owned(),
                        "external_callers".to_owned(),
                    ]),
                    ..DomainCapabilities::default()
                },
                "docs" => DomainCapabilities {
                    context_lenses: BTreeSet::from([
                        "instructions".to_owned(),
                        "adrs".to_owned(),
                        "invariants".to_owned(),
                    ]),
                    ..DomainCapabilities::default()
                },
                _ => DomainCapabilities::default(),
            }
        }

        fn execute_action(
            &self,
            _action: &str,
            _params: &Value,
            _idempotency_key: &str,
            _req: &acdd_core::ports::DomainActionReq,
        ) -> anyhow::Result<acdd_core::ports::DomainActionResult> {
            Ok(acdd_core::ports::DomainActionResult {
                summary: "ok".to_owned(),
                output: json!({}),
            })
        }

        fn run_check(
            &self,
            _check: &str,
            _params: &Value,
            _req: &acdd_core::ports::DomainCheckReq,
        ) -> anyhow::Result<acdd_core::ports::DomainCheckOutcome> {
            Ok(acdd_core::ports::DomainCheckOutcome::Passed {
                summary: "ok".to_owned(),
                execution_digest_salt: None,
            })
        }

        fn provide_context(
            &self,
            _lens: &str,
            _req: &acdd_core::ports::DomainContextReq,
        ) -> anyhow::Result<Value> {
            Ok(json!({}))
        }

        fn call_tool(
            &self,
            _tool: &str,
            _args: &Value,
            _req: &acdd_core::ports::DomainToolReq,
        ) -> anyhow::Result<acdd_core::ports::DomainToolOutput> {
            Ok(acdd_core::ports::DomainToolOutput::Json(json!({})))
        }
    }

    fn full_providers() -> CompositeDomainProvider {
        CompositeDomainProvider::new(vec![
            Arc::new(DummyCodeDocsProvider("code")),
            Arc::new(DummyCodeDocsProvider("docs")),
        ])
        .unwrap()
    }

    fn write_bound_profile(root: &Path, extra: &str) {
        fs::create_dir_all(root.join(".acdd")).unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            format!(
                "version: 2\ninherit: code\ncommands:\n  test: \"cargo test {{test_filter}}\"\n  build_check: \"cargo check\"\n  verify_all: \"cargo test\"\n{extra}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn validate_workspace_config_profile_and_spec_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let providers = full_providers();

        // 1. Legacy 0.5 acdd.yaml triggers CONFIG_MIGRATION_REQUIRED.
        fs::write(root.join("acdd.yaml"), "milestones:\n  path: docs/milestones\n").unwrap();
        let res_legacy = validate_workspace(root, None, None, &providers).unwrap();
        assert!(!res_legacy.valid);
        assert_eq!(res_legacy.diagnostics[0].code, "CONFIG_MIGRATION_REQUIRED");

        // 2. Invalid schema acdd.yaml triggers ACDD_PROFILE_SCHEMA_INVALID.
        fs::write(root.join("acdd.yaml"), "bogus_unknown: true\n").unwrap();
        let res_schema = validate_workspace(root, None, None, &providers).unwrap();
        assert!(!res_schema.valid);
        assert_eq!(
            res_schema.diagnostics[0].code,
            ErrorCode::ProfileSchemaInvalid.as_str()
        );

        // 3. Valid acdd.yaml + unbound commands with modules.state: true -> diagnostics[0] = CONFIG_VALID, diagnostics[1] = COMMAND_UNBOUND.
        fs::write(
            root.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: true\n",
        )
        .unwrap();
        fs::create_dir_all(root.join(".acdd")).unwrap();
        fs::write(root.join(".acdd/profile.yaml"), "version: 2\ninherit: code\n").unwrap();
        let res_unbound = validate_workspace(root, None, None, &providers).unwrap();
        assert!(!res_unbound.valid);
        assert_eq!(res_unbound.diagnostics[0].code, "CONFIG_VALID");
        assert_eq!(res_unbound.diagnostics[1].code, "COMMAND_UNBOUND");

        // 4. Unbound command profile with modules.state: false passes (require_bound_commands = false).
        fs::write(
            root.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: false\n",
        )
        .unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\ninherit: none\ndefault_tools: [acdd_read]\ncommands:\n  t:\n    unbound: true\ngates:\n  - id: verify\n    command: t\n    proof: command\n",
        )
        .unwrap();
        let res_state_off = validate_workspace(root, None, None, &providers).unwrap();
        assert!(res_state_off.valid, "{res_state_off:?}");

        // 5. Spec validation matrix (all invalid specs fail and NEVER emit SPEC_VALID).
        fs::write(
            root.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: true\n",
        )
        .unwrap();
        write_bound_profile(root, "protected_paths: [Cargo.toml, .acdd/profile.yaml]\n");
        fs::create_dir_all(root.join("docs/milestones")).unwrap();
        fs::create_dir_all(root.join("docs/plans")).unwrap();

        let bad_specs: &[(&str, &str)] = &[
            (
                "docs/milestones/AC-001-terminal.md",
                "---\nid: AC-001-terminal\ntitle: Done\nstatus: completed\n---\n",
            ),
            (
                "docs/milestones/BAD-002.md",
                "---\nid: BAD-002\ntitle: Wrong prefix\nstatus: planned\n---\n",
            ),
            (
                "docs/milestones/AC-1-short.md",
                "---\nid: AC-1-short\ntitle: Short digits\nstatus: planned\n---\n",
            ),
            (
                "docs/milestones/AC-003-planned-active.md",
                "---\nid: AC-003-planned-active\ntitle: Unreconciled\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: active\n```\n",
            ),
            (
                "docs/milestones/AC-004-bad-glob.md",
                "---\nid: AC-004-bad-glob\ntitle: Bad glob\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"src/{a,b\"]\n```\n",
            ),
            (
                "docs/milestones/AC-005-protected.md",
                "---\nid: AC-005-protected\ntitle: Protected\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [Cargo.toml]\n```\n",
            ),
            (
                "docs/milestones/AC-006-ext-dep.md",
                "---\nid: AC-006-ext-dep\ntitle: Missing external\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\ndepends_on: [\"AC-999:missing\"]\n```\n",
            ),
            (
                "docs/milestones/AC-007-spec-dep.md",
                "---\nid: AC-007-spec-dep\ntitle: Missing spec dep\nstatus: planned\ndepends_on: [AC-999]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
            ),
        ];

        for &(rel, content) in bad_specs {
            fs::write(root.join(rel), content).unwrap();
            let doc_res = validate_document(root, Path::new(rel), None, None, &providers).unwrap();
            assert!(!doc_res.valid, "{rel} should fail: {doc_res:?}");
            assert!(
                !doc_res.diagnostics.iter().any(|d| d.code == "SPEC_VALID"),
                "{rel} must never emit SPEC_VALID when failing: {doc_res:?}"
            );
            fs::remove_file(root.join(rel)).unwrap();
        }

        // 6. Valid spec with lowercase plan prefix warning + cross-spec planned dependency passes.
        fs::write(
            root.join("docs/milestones/AC-010-base.md"),
            "---\nid: AC-010-base\ntitle: Base\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"src/{a,b}.rs\"]\n```\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/plans/plan-001-case.md"),
            "---\nid: plan-001-case\ntitle: Lowercase plan\nstatus: planned\ndepends_on: [\"AC-010-base:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [docs/]\n```\n",
        )
        .unwrap();
        let ok_res = validate_workspace(root, None, None, &providers).unwrap();
        assert!(ok_res.valid, "{ok_res:?}");
        assert!(!ok_res.warnings.is_empty(), "expected case advisory warning");

        // 7. Reconciled planned spec with completed task in state.sqlite + legacy 0.5 archive + tombstone precedence + validate_spec_for_run.
        let archived_05_dir = root.join("docs/milestones/archive");
        fs::create_dir_all(&archived_05_dir).unwrap();
        fs::write(
            archived_05_dir.join("AC-090-legacy.md"),
            "---\nid: AC-090\ntitle: Legacy 0.5\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: completed\n```\n",
        )
        .unwrap();
        let dep_05_doc = parse_spec(
            "---\nid: AC-091\ntitle: Consumer\nstatus: planned\ndepends_on: [\"AC-090:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        )
        .unwrap();

        let cfg = StateEngineConfig {
            workspace_root: root.to_path_buf(),
            worktree: root.to_path_buf(),
            state_db_override: None,
            code_db_path: None,
            spec_collections: acdd_core::config::default_specs(),
            profiles_dirs: vec![".acdd/profiles".to_owned()],
            linked_workspaces: BTreeMap::new(),
            modules: SubsystemRegistry::default(),
            response_policy: acdd_core::response::ResponsePolicy::default(),
            retain_traces: 5,
        };
        let resolved_p =
            resolve_optional_profile_ref_with_dirs(None, Some(root), Some(&cfg.modules), None)
                .unwrap();
        let harness = RunHarness::in_memory().unwrap();
        validate_spec_for_run(&cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();

        // Tombstone precedence: Completed superseded by newer Cleared fails; Cleared superseded by newer Completed passes.
        let r_done = RunId::new("run-tomb-1").unwrap();
        let r_clear = RunId::new("run-tomb-2").unwrap();
        let g_done = RunGraphSnapshot {
            run: RunRecord {
                run_id: r_done.clone(),
                run_name: RunName::new("tomb-done").unwrap(),
                collection: Some("milestones".to_owned()),
                workspace: "default".to_owned(),
                profile_id: "default".to_owned(),
                profile_json: resolved_p.to_canonical_json().unwrap(),
                spec_path: Some("docs/milestones/AC-092.md".to_owned()),
                spec_id: Some("AC-092".to_owned()),
                spec_digest: "aa".repeat(32),
                is_ephemeral: false,
                isolation: IsolationMode::InPlace,
                target_base: None,
                target_base_commit: None,
                branch: None,
                worktree: None,
                trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
                status: RunStatus::Active,
                cancellation_reason: None,
                root_cycle_count: 0,
                generation: 1,
                created_at: "2026-11-18T00:00:00.000Z".to_owned(),
                completed_at: None,
            },
            nodes: vec![GateNodeRecord {
                run_id: r_done.clone(),
                node_id: "task:t1".to_owned(),
                parent_id: None,
                kind: NodeKind::Task,
                task_ref: Some(TaskRef::new("t1").unwrap()),
                gate_id: "__task__".to_owned(),
                seq_order: 0,
                status: NodeStatus::Completed,
                node_version: 1,
                isolation: IsolationMode::InPlace,
                attempt: 0,
                cycle_count: 0,
                max_attempts: 1,
                claim_revision: 0,
                contract_revision: 1,
                worker_id: None,
                session_fingerprint: None,
                branch: None,
                worktree: None,
                baseline_commit: None,
                claim_tree: None,
                accepted_tree: None,
                accepted_definition_digest: None,
                tags: vec!["default".to_owned()],
                invariants: Default::default(),
                scope: vec!["src/".to_owned()],
                extended_scope: Vec::new(),
                active_span_id: None,
            }],
            dependencies: Vec::new(),
            subtasks: Vec::new(),
            claims: Vec::new(),
            traces: Vec::new(),
            artifacts: Vec::new(),
        };
        let mut g_clear = g_done.clone();
        g_clear.run.run_id = r_clear.clone();
        g_clear.run.run_name = RunName::new("tomb-clear").unwrap();
        g_clear.nodes[0].run_id = r_clear.clone();
        let tomb_done = RunTombstoneRecord {
            run_id: r_done,
            run_name: RunName::new("tomb-done").unwrap(),
            collection: Some("milestones".to_owned()),
            workspace: "default".to_owned(),
            spec_id: Some("AC-092".to_owned()),
            title: "Done".to_owned(),
            profile_id: "default".to_owned(),
            is_ephemeral: false,
            generation: 1,
            terminal_status: TerminalStatus::Completed,
            target_base: None,
            target_base_commit: None,
            archive_path: Some("docs/milestones/archive/AC-090-legacy.md".to_owned()),
            closed_commit: None,
            cleanup_pending: false,
            cleanup_manifest: Vec::new(),
            summary_json: "{}".to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: "2026-11-18T01:00:00.000Z".to_owned(),
        };
        let tomb_clear = RunTombstoneRecord {
            run_id: r_clear,
            run_name: RunName::new("tomb-clear").unwrap(),
            terminal_status: TerminalStatus::Cleared,
            archive_path: None,
            closed_at: "2026-11-18T02:00:00.000Z".to_owned(),
            ..tomb_done.clone()
        };
        let precond = StoragePreconditions {
            expected_run_generation: 1,
            expected_node_versions: Vec::new(),
            expected_claim_revision: None,
            expected_contract_revision: None,
            expected_worker_id: None,
            required_predecessor_trees: Vec::new(),
        };
        crate::store::traits::exec_tx_with(&harness, |tx| {
            tx.insert_run_graph(&g_done)?;
            tx.archive_and_prune_run(&precond, &tomb_done)?;
            tx.insert_run_graph(&g_clear)?;
            tx.archive_and_prune_run(&precond, &tomb_clear)
        })
        .unwrap();
        let dep_cleared = parse_spec(
            "---\nid: AC-093\ntitle: Superseded by cleared\nstatus: planned\ndepends_on: [\"AC-092:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        )
        .unwrap();
        let err_cleared =
            validate_spec_for_run(&cfg, &harness, &providers, &dep_cleared, &resolved_p)
                .unwrap_err();
        assert_eq!(err_cleared.code, ErrorCode::GateVerificationFailed);

        // Git HEAD verification for uncommitted 0.5 archive inside a Git repo.
        let git_dir = tempfile::tempdir().unwrap();
        let groot = git_dir.path();
        acdd_core::git::git_command(groot, &["init", "-b", "main"]).unwrap();
        acdd_core::git::git_command(groot, &["config", "user.email", "t@t.local"]).unwrap();
        acdd_core::git::git_command(groot, &["config", "user.name", "T"]).unwrap();
        fs::write(groot.join("README.md"), "# R\n").unwrap();
        acdd_core::git::git_command(groot, &["add", "README.md"]).unwrap();
        acdd_core::git::git_command(groot, &["commit", "-m", "init"]).unwrap();
        fs::create_dir_all(groot.join("docs/milestones/archive")).unwrap();
        fs::write(
            groot.join("docs/milestones/archive/AC-090-legacy.md"),
            "---\nid: AC-090\ntitle: Uncommitted\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: completed\n```\n",
        )
        .unwrap();
        let mut g_cfg = cfg.clone();
        g_cfg.workspace_root = groot.to_path_buf();
        g_cfg.worktree = groot.to_path_buf();
        let uncommitted_err =
            validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p)
                .unwrap_err();
        assert_eq!(uncommitted_err.code, ErrorCode::ProfileSchemaInvalid);
        let direct_dep_err =
            crate::usecases::deps::verify_run_level_depends_on(&g_cfg, &harness, &dep_05_doc)
                .unwrap_err();
        assert_eq!(direct_dep_err.code, ErrorCode::GateVerificationFailed);
        acdd_core::git::git_command(groot, &["add", "docs/milestones/archive/AC-090-legacy.md"])
            .unwrap();
        acdd_core::git::git_command(groot, &["commit", "-m", "archive"]).unwrap();
        validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();

        // Pinned HEAD blob check: unstaged working-tree edits on committed archive must NOT alter resolution.
        fs::write(
            groot.join("docs/milestones/archive/AC-090-legacy.md"),
            "---\nid: AC-090\ntitle: Unstaged Corrupt\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: done\nscope: [src/]\nstatus: planned\n```\n",
        )
        .unwrap();
        validate_spec_for_run(&g_cfg, &harness, &providers, &dep_05_doc, &resolved_p).unwrap();
        crate::usecases::deps::verify_run_level_depends_on(&g_cfg, &harness, &dep_05_doc).unwrap();
        // And an archive committed as `planned` with unstaged working-tree edit to `completed` must fail closed.
        fs::write(
            groot.join("docs/milestones/archive/AC-091-planned.md"),
            "---\nid: AC-091\ntitle: Planned In Head\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: p\nscope: [src/]\nstatus: planned\n```\n",
        )
        .unwrap();
        acdd_core::git::git_command(groot, &["add", "docs/milestones/archive/AC-091-planned.md"])
            .unwrap();
        acdd_core::git::git_command(groot, &["commit", "-m", "planned archive"]).unwrap();
        fs::write(
            groot.join("docs/milestones/archive/AC-091-planned.md"),
            "---\nid: AC-091\ntitle: Spoofed On Disk\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: p\nscope: [src/]\nstatus: completed\n```\n",
        )
        .unwrap();
        let dep_spoofed_doc = parse_spec(
            "---\nid: AC-094\ntitle: Spoofed Dep\nstatus: planned\ndepends_on: [\"AC-091:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        )
        .unwrap();
        let spoof_err =
            crate::usecases::deps::verify_run_level_depends_on(&g_cfg, &harness, &dep_spoofed_doc)
                .unwrap_err();
        assert_eq!(spoof_err.code, ErrorCode::GateVerificationFailed);

        // Reconciled planned spec with completed task in disk state.sqlite passes validate_workspace.
        let disk_harness = RunHarness::on_disk(vec!["docs/milestones".to_owned()]).unwrap();
        let disk_root = disk_harness.workspace_root().unwrap();
        write_bound_profile(disk_root, "");
        fs::write(
            disk_root.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: true\n",
        )
        .unwrap();
        fs::create_dir_all(disk_root.join("docs/milestones")).unwrap();
        fs::write(
            disk_root.join("docs/milestones/AC-020-reconciled.md"),
            "---\nid: AC-020-reconciled\ntitle: Reconciled\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: completed\n```\n",
        )
        .unwrap();
        let mut g_rec = g_done.clone();
        g_rec.run.run_id = RunId::new("run-reconcile").unwrap();
        g_rec.run.run_name = RunName::new("ac-020").unwrap();
        g_rec.run.spec_path = Some("docs/milestones/AC-020-reconciled.md".to_owned());
        g_rec.run.spec_id = Some("AC-020-reconciled".to_owned());
        g_rec.nodes[0].run_id = g_rec.run.run_id.clone();
        crate::store::traits::exec_tx_with(&disk_harness, |tx| tx.insert_run_graph(&g_rec)).unwrap();
        let rec_res = validate_workspace(disk_root, None, None, &providers).unwrap();
        assert!(rec_res.valid, "reconciled planned spec should pass: {rec_res:?}");

        // Linked workspace custom specs + custom state_db resolution in validate_spec_for_run.
        let lw_dir = tempfile::tempdir().unwrap();
        let lw_root = lw_dir.path();
        fs::write(
            lw_root.join("acdd.yaml"),
            "state_db: .acdd/custom-lw.sqlite\nspecs:\n  rfcs:\n    path: docs/rfcs\n    prefixes: [RFC]\n    digits: 3\n",
        )
        .unwrap();
        fs::create_dir_all(lw_root.join("docs/rfcs/archive")).unwrap();
        fs::write(
            lw_root.join("docs/rfcs/archive/RFC-001.md"),
            "---\nid: RFC-001\ntitle: Custom collection\nstatus: completed\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\nstatus: completed\n```\n",
        )
        .unwrap();
        let mut lw_cfg = cfg.clone();
        lw_cfg
            .linked_workspaces
            .insert("lib".to_owned(), lw_root.to_path_buf());
        let lw_consumer = parse_spec(
            "---\nid: AC-099\ntitle: LW Consumer\nstatus: planned\ndepends_on: [\"lib/RFC-001:t1\"]\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        )
        .unwrap();
        validate_spec_for_run(&lw_cfg, &harness, &providers, &lw_consumer, &resolved_p).unwrap();

        // 8. Audit remediation regressions:
        // (a) validate_spec_for_run rejects non-normalized globs identically to validate_workspace.
        for bad_scope in ["../escape/**", "/abs/path", "C:/win/path", "src/{a"] {
            let bad_doc = parse_spec(&format!(
                "---\nid: AC-095\ntitle: Bad scope\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [\"{bad_scope}\"]\n```\n"
            ))
            .unwrap();
            let err = validate_spec_for_run(&cfg, &harness, &providers, &bad_doc, &resolved_p)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{bad_scope}");
        }

        // (b) plan_extend_scope rejects malformed brace globs.
        let mut g_ext = g_done.clone();
        g_ext.nodes[0].status = NodeStatus::Ready;
        let machine_prof = crate::usecases::lifecycle::to_machine_profile(&resolved_p).unwrap();
        let ext_err = crate::machine::plan_extend_scope(
            &g_ext,
            &machine_prof,
            &crate::machine::ExtendScopeInput {
                node_cursor: acdd_core::ids::NodeCursor::task(
                    TaskRef::new("t1").unwrap(),
                    Vec::new(),
                ),
                paths: vec!["src/{a".to_owned()],
                reason: "need malformed".to_owned(),
                worker_id: None,
            },
        )
        .unwrap_err();
        assert_eq!(ext_err.code, ErrorCode::OutOfScopeChanges);

        // (c) validate_document surfaces broken .acdd/profile.yaml rather than swallowing with .ok().
        let broken_ws = tempfile::tempdir().unwrap();
        fs::create_dir_all(broken_ws.path().join(".acdd")).unwrap();
        fs::create_dir_all(broken_ws.path().join("docs/milestones")).unwrap();
        fs::write(
            broken_ws.path().join(".acdd/profile.yaml"),
            "version: 2\ninherit: missing_parent\n",
        )
        .unwrap();
        fs::write(
            broken_ws.path().join("docs/milestones/AC-001.md"),
            "---\nid: AC-001\ntitle: S\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: x\nscope: [src/]\n```\n",
        )
        .unwrap();
        let doc_broken_prof = validate_document(
            broken_ws.path(),
            Path::new("docs/milestones/AC-001.md"),
            None,
            None,
            &providers,
        )
        .unwrap();
        assert!(!doc_broken_prof.valid);
        assert!(
            doc_broken_prof
                .diagnostics
                .iter()
                .any(|d| d.status == "fail" && d.path == ".acdd/profile.yaml"),
            "expected .acdd/profile.yaml diagnostic: {doc_broken_prof:?}"
        );

        // (d) Cross-spec task-level cycle detection & non-cyclic cross-spec task refs.
        let spec_a = parse_spec(
            "---\nid: AC-101\ntitle: A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"AC-102:t2\"]\n```\n",
        )
        .unwrap();
        let spec_b = parse_spec(
            "---\nid: AC-102\ntitle: B\nstatus: planned\n---\n### task: t2\n```yaml\ntarget: b\nscope: [src/b.rs]\ndepends_on: [\"AC-101:t1\"]\n```\n",
        )
        .unwrap();
        let cyc_issues = validate_spec_dependencies(
            root,
            &Adapter::default(),
            Some(&harness),
            None,
            &spec_a,
            &[spec_a.clone(), spec_b],
        );
        assert!(
            cyc_issues
                .iter()
                .any(|i| i.code == ErrorCode::ProfileSchemaInvalid && i.message.contains("cross-spec")),
            "expected cross-spec task cycle diagnostic: {cyc_issues:?}"
        );

        let noncyc_a = parse_spec(
            "---\nid: AC-103\ntitle: A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"AC-104:t1\"]\n```\n### task: t2\n```yaml\ntarget: a2\nscope: [src/a2.rs]\n```\n",
        )
        .unwrap();
        let noncyc_b = parse_spec(
            "---\nid: AC-104\ntitle: B\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: b\nscope: [src/b.rs]\n```\n### task: t2\n```yaml\ntarget: b2\nscope: [src/b2.rs]\ndepends_on: [\"AC-103:t2\"]\n```\n",
        )
        .unwrap();
        let noncyc_issues = validate_spec_dependencies(
            root,
            &Adapter::default(),
            Some(&harness),
            None,
            &noncyc_a,
            &[noncyc_a.clone(), noncyc_b],
        );
        assert!(noncyc_issues.is_empty(), "non-cyclic cross-spec task edges should pass: {noncyc_issues:?}");

        // Self-alias pointing back to primary root MUST participate in cycle & self-dep detection.
        let mut self_alias_adapter = Adapter::default();
        self_alias_adapter.linked_workspaces.push(acdd_core::config::LinkedWorkspace {
            name: "self_ws".to_owned(),
            path: root.canonicalize().unwrap(),
            roots: vec![root.canonicalize().unwrap()],
            ignored_names: Default::default(),
        });
        let alias_cyc_a = parse_spec(
            "---\nid: AC-105\ntitle: Alias A\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"self_ws/AC-106:t2\"]\n```\n",
        )
        .unwrap();
        let alias_cyc_b = parse_spec(
            "---\nid: AC-106\ntitle: Alias B\nstatus: planned\n---\n### task: t2\n```yaml\ntarget: b\nscope: [src/b.rs]\ndepends_on: [\"AC-105:t1\"]\n```\n",
        )
        .unwrap();
        let alias_cyc_issues = validate_spec_dependencies(
            root,
            &self_alias_adapter,
            Some(&harness),
            None,
            &alias_cyc_a,
            &[alias_cyc_a.clone(), alias_cyc_b],
        );
        assert!(
            alias_cyc_issues
                .iter()
                .any(|i| i.code == ErrorCode::ProfileSchemaInvalid && i.message.contains("cross-spec")),
            "self-alias cross-spec cycle must fail closed: {alias_cyc_issues:?}"
        );

        let self_dep_alias = parse_spec(
            "---\nid: AC-107\ntitle: Self Alias\nstatus: planned\n---\n### task: t1\n```yaml\ntarget: a\nscope: [src/a.rs]\ndepends_on: [\"self_ws/AC-107:t1\"]\n```\n",
        )
        .unwrap();
        let self_dep_issues = validate_spec_dependencies(
            root,
            &self_alias_adapter,
            Some(&harness),
            None,
            &self_dep_alias,
            std::slice::from_ref(&self_dep_alias),
        );
        assert!(
            self_dep_issues
                .iter()
                .any(|i| i.code == ErrorCode::ProfileSchemaInvalid && i.message.contains("cannot depend on itself")),
            "self-alias self-dependency must fail closed: {self_dep_issues:?}"
        );

        // (e) Corrupt state DB in linked workspace propagates reader error instead of swallowing.
        fs::create_dir_all(lw_root.join(".acdd")).unwrap();
        fs::write(lw_root.join(".acdd/custom-lw.sqlite"), b"not a sqlite db").unwrap();
        let corrupt_db_err = validate_spec_for_run(&lw_cfg, &harness, &providers, &lw_consumer, &resolved_p)
            .unwrap_err();
        assert_ne!(corrupt_db_err.code, ErrorCode::GateVerificationFailed);
    }
}
