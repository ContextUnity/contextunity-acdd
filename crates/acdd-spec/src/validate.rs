//! Pure workspace, adapter, and specification validation over [`SpecRuntimeOverlay`].

use crate::deps::validate_spec_dependencies;
use crate::discovery::{
    collect_workspace_specs, effective_spec_collections,
};
use crate::overlay::{RuntimeTaskStatus, SpecRuntimeOverlay};
use crate::spec::{SpecDocument, SpecStatus};
use crate::types::{ValidateDiagnostic, ValidateResp, ValidateSummary};
use acdd_core::config::{load_adapter_report, Adapter, SpecConfig, ACDD_CONFIG_FILE};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::path_policy::is_valid_scope_glob;
use serde_json::{json, Value};
use std::path::Path;

/// Parsed active specification paired with its matched collection config.
#[derive(Debug, Clone)]
pub struct ScannedSpec {
    /// Workspace-relative spec path.
    pub rel_path: String,
    /// Owning collection config if matched.
    pub col: Option<SpecConfig>,
    /// Parsed spec document.
    pub doc: SpecDocument,
}

/// Construct a passing [`ValidateDiagnostic`].
pub fn pass(path: &str, code: &str, message: String) -> ValidateDiagnostic {
    ValidateDiagnostic {
        status: "pass".to_owned(),
        code: code.to_owned(),
        path: path.to_owned(),
        line: 1,
        column: 1,
        message,
    }
}

/// Construct a failing [`ValidateDiagnostic`].
pub fn fail(
    code: &str,
    path: &str,
    line: usize,
    column: usize,
    message: String,
) -> ValidateDiagnostic {
    ValidateDiagnostic {
        status: "fail".to_owned(),
        code: code.to_owned(),
        path: path.to_owned(),
        line,
        column,
        message,
    }
}

/// Convert a [`AcddError`] into a failing [`ValidateDiagnostic`].
pub fn fail_state_err(
    path: &str,
    err: &AcddError,
    line: usize,
    column: usize,
) -> ValidateDiagnostic {
    fail(err.code.as_str(), path, line, column, err.to_string())
}

/// Build a [`ValidateResp`] summary from collected diagnostics and warnings.
pub fn build_resp(
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

/// Strictly load `acdd.yaml` and fail closed on invalid `linked_workspaces` entries.
pub fn load_strict_adapter(
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
                    ErrorCode::WorkflowSchemaInvalid.as_str(),
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
        Err(err) => Err(fail_state_err(&config_label, &err, 1, 1)),
    }
}

/// Scan all configured spec collections under `root`, pushing any scan errors into `diagnostics`.
pub fn scan_collection_specs(
    root: &Path,
    adapter: &Adapter,
    diagnostics: &mut Vec<ValidateDiagnostic>,
) -> (Vec<ScannedSpec>, usize) {
    let specs_map = effective_spec_collections(&adapter.specs);
    let scan = collect_workspace_specs(root, &specs_map);
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

/// Validate a parsed `SpecDocument`'s static invariants, collection ID, planned task statuses,
/// optional task scope globs, empty guidance warning, and dependency graph via `overlay`.
///
/// Returns `true` if any failure diagnostic was emitted for this spec.
#[allow(clippy::too_many_arguments)]
pub fn validate_spec_static_checks(
    root: &Path,
    adapter: &Adapter,
    overlay: &dyn SpecRuntimeOverlay,
    col: Option<&SpecConfig>,
    rel_path: &str,
    spec: &SpecDocument,
    active_docs: &[SpecDocument],
    validate_task_scopes: bool,
    diagnostics: &mut Vec<ValidateDiagnostic>,
    warnings: &mut Vec<Value>,
) -> bool {
    let mut doc_failed = false;

    if matches!(spec.status, SpecStatus::Completed | SpecStatus::Cancelled) {
        diagnostics.push(fail(
            ErrorCode::SpecSchemaInvalid.as_str(),
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
            root,
            overlay,
            rel_path,
            spec,
            diagnostics,
            &mut doc_failed,
        );
    }

    if validate_task_scopes {
        validate_task_scope_globs(rel_path, spec, diagnostics, &mut doc_failed);
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

    for issue in validate_spec_dependencies(root, adapter, overlay, spec, active_docs) {
        diagnostics.push(fail(issue.code.as_str(), rel_path, 1, 1, issue.message));
        doc_failed = true;
    }

    doc_failed
}

fn validate_task_scope_globs(
    rel_path: &str,
    spec: &SpecDocument,
    diagnostics: &mut Vec<ValidateDiagnostic>,
    doc_failed: &mut bool,
) {
    for task in &spec.tasks {
        for pat in task
            .scope
            .iter()
            .chain(task.extended_scope.iter().map(|e| &e.path))
        {
            if !is_valid_scope_glob(pat) {
                diagnostics.push(fail(
                    ErrorCode::SpecSchemaInvalid.as_str(),
                    rel_path,
                    1,
                    1,
                    format!(
                        "task '{}' has invalid or non-normalized scope glob '{pat}'",
                        task.task_ref
                    ),
                ));
                *doc_failed = true;
            }
        }
    }
}

fn validate_spec_id_against_collection(
    rel_path: &str,
    spec_id: &str,
    col: &SpecConfig,
    diagnostics: &mut Vec<ValidateDiagnostic>,
    warnings: &mut Vec<Value>,
    doc_failed: &mut bool,
) {
    if col.prefixes.is_empty() {
        return;
    }
    let Some((prefix, rest)) = spec_id.split_once('-') else {
        diagnostics.push(fail(
            ErrorCode::SpecSchemaInvalid.as_str(),
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
            ErrorCode::SpecSchemaInvalid.as_str(),
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
            ErrorCode::SpecSchemaInvalid.as_str(),
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
            ErrorCode::SpecSchemaInvalid.as_str(),
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
    root: &Path,
    overlay: &dyn SpecRuntimeOverlay,
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

    let reconciled_statuses = overlay.active_task_statuses_for_spec(root, rel_path, &spec.id);

    for task in advanced {
        let reconciled = matches!(
            (
                task.status,
                reconciled_statuses.get(task.task_ref.as_str()).copied(),
            ),
            (SpecStatus::Completed, Some(RuntimeTaskStatus::Completed))
                | (
                    SpecStatus::Active,
                    Some(RuntimeTaskStatus::InProgress | RuntimeTaskStatus::Ready)
                )
        );
        if !reconciled {
            diagnostics.push(fail(
                ErrorCode::SpecSchemaInvalid.as_str(),
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
