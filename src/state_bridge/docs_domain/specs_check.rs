//! `docs.specs` check parameter parsing, schema, and required-spec validation.

use super::super::common::{collect_spec_selectors, is_active_collection_spec_rel};
use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::path_policy::normalize_rel_path;
use acdd_core::ports::DomainCheckReq;
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocsSpecsParams {
    #[serde(default)]
    pub(super) spec: Option<String>,
    #[serde(default)]
    pub(super) specs: Option<Vec<String>>,
    #[serde(default)]
    pub(super) required: bool,
}

pub(super) fn docs_specs_input_schema() -> SchemaDef {
    let mut specs_array = SchemaDef::array(SchemaDef::string());
    specs_array.min_items = Some(1);
    let mut spec_str = SchemaDef::string();
    spec_str.min_length = Some(1);

    let mut schema = SchemaDef::object();
    schema.properties.insert("spec".to_owned(), spec_str);
    schema.properties.insert("specs".to_owned(), specs_array);
    schema
        .properties
        .insert("required".to_owned(), SchemaDef::of_type("boolean"));
    schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
    schema
}

pub(super) fn parse_docs_specs_params(params: &Value) -> Result<DocsSpecsParams> {
    if params.is_null() {
        return Ok(DocsSpecsParams::default());
    }
    serde_json::from_value(params.clone()).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid docs.specs check parameters: {err}"),
        )
        .into()
    })
}

pub(super) fn resolve_target_spec_collections<'a>(
    params: &DocsSpecsParams,
    effective_cols: &'a BTreeMap<String, SpecConfig>,
) -> Result<Vec<(&'a String, &'a SpecConfig)>> {
    let selectors = collect_spec_selectors(&params.spec, &params.specs);
    if selectors.is_empty() {
        return Ok(effective_cols.iter().collect());
    }
    let mut resolved = Vec::new();
    let mut seen = BTreeSet::new();
    for sel in selectors {
        let matches = acdd_spec::matching_spec_collections(effective_cols, sel);
        if matches.is_empty() {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("docs.specs references unknown spec type or collection '{sel}'"),
            )
            .into());
        }
        for (col_key, col_cfg) in matches {
            if seen.insert(col_key.as_str()) {
                resolved.push((col_key, col_cfg));
            }
        }
    }
    Ok(resolved)
}

pub(super) fn check_required_specs(
    worktree: &Path,
    req: &DomainCheckReq,
    target_cols: &[(&String, &SpecConfig)],
    effective_cols: &BTreeMap<String, SpecConfig>,
) -> Option<String> {
    let labels: Vec<&str> = target_cols.iter().map(|(k, _)| k.as_str()).collect();
    let changed_matches = req.changed_paths.iter().any(|p| {
        let Some(norm) = normalize_rel_path(p) else {
            return false;
        };
        if !req.admitted_scope.is_empty()
            && !req.path_policy.is_scope_admitted(&norm, &req.admitted_scope)
        {
            return false;
        }
        if !effective_cols
            .values()
            .any(|cfg| is_active_collection_spec_rel(&norm, cfg))
        {
            return false;
        }
        let Ok(content) = acdd_core::fs::read_confined_utf8(worktree, Path::new(&norm)) else {
            return false;
        };
        let owning_col_key = acdd_spec::match_spec_collection(effective_cols, &norm);
        let owning_col = owning_col_key
            .as_deref()
            .and_then(|k| effective_cols.get(k).map(|cfg| (k, cfg)));
        let doc_spec = acdd_spec::parse_spec_with_collections(&content, owning_col, effective_cols)
            .ok()
            .and_then(|d| d.spec);
        target_cols.iter().any(|(k, _)| {
            acdd_spec::schema::spec_matches_selector(
                doc_spec.as_deref(),
                owning_col_key.as_deref(),
                k.as_str(),
                effective_cols,
            )
        })
    });
    if !changed_matches {
        return Some(format!(
            "docs.specs (required: true) found no changed active spec file in candidate matching [{}]",
            labels.join(", ")
        ));
    }
    None
}
