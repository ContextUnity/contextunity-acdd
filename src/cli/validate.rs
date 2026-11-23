//! Thin CLI adapter delegating workspace and document validation to `acdd_state::validate`.

use crate::state_bridge::build_domain_providers;
use acdd_core::config::{load_adapter_report, Adapter};
use acdd_state::usecases::dto::ValidateResp;
use acdd_state::validate::{validate_document, validate_workspace};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

/// Validate one document or workspace with explicit `--config` and `--profile` overrides.
pub fn run_with_options(
    root: &Path,
    relative: Option<&Path>,
    explicit_config: Option<&Path>,
    explicit_profile: Option<&str>,
) -> Result<Value> {
    // Fall back to `Adapter::default()` only when loading `acdd.yaml` fails so
    // `acdd_state::validate` can emit structured config diagnostics; propagate provider errors.
    let adapter = match load_adapter_report(root, explicit_config) {
        Ok(report) => report.adapter,
        Err(_) => Adapter::default(),
    };
    let providers = build_domain_providers(root, &adapter, None, Vec::new())?;
    let resp: ValidateResp = match relative {
        Some(rel) => validate_document(root, rel, explicit_config, explicit_profile, &providers)?,
        None => validate_workspace(root, explicit_config, explicit_profile, &providers)?,
    };
    let mut value = serde_json::to_value(&resp)?;
    if let Some(obj) = value.as_object_mut() {
        obj.entry("diagnostics").or_insert_with(|| json!([]));
        obj.entry("warnings").or_insert_with(|| json!([]));
    }
    Ok(value)
}
