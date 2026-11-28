//! Host `SpecDomainProvider` (`domain_id = "spec"`) implementing `spec.deltas_applied`.

use super::common::{
    collect_spec_selectors, compute_specs_digest, resolve_worktree_adapter,
    unsupported_domain_action,
};
use acdd_core::config::{Adapter, SpecConfig};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities, DomainCheckOutcome,
    DomainCheckReq, DomainContextReq, DomainProvider, DomainToolOutput, DomainToolReq,
    FreshnessObservation,
};
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use acdd_core::subsystems::SubsystemId;
use acdd_spec::deltas::{DeltaSpecView, SpecDelta};
use acdd_spec::discovery::{collect_workspace_specs, CollectedWorkspaceSpec};
use acdd_spec::schema::resolve_spec_schema;
use acdd_spec::{
    effective_spec_collections, is_safe_path_segment_id, verify_spec_deltas, SpecDocument,
    SpecStatus,
};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Host `spec` domain provider exposing `spec.deltas_applied` verification and freshness observation.
pub struct SpecDomainProvider {
    root: PathBuf,
    config: Adapter,
}

impl SpecDomainProvider {
    /// Construct a `SpecDomainProvider` bound to `root` and workspace `config`.
    pub fn new(root: PathBuf, config: Adapter) -> Self {
        Self { root, config }
    }

    fn run_deltas_applied_check(
        &self,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        let parsed = parse_spec_deltas_params(params)?;
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let effective_cols = effective_spec_collections(&worktree_adapter.specs);
        let live_digest = compute_specs_digest(
            &req.worktree,
            &worktree_adapter,
            params,
            req,
            "__spec_deltas_meta__",
        );
        let scan = collect_workspace_specs(&req.worktree, &effective_cols);
        if !scan.errors.is_empty() {
            let diagnostics = scan
                .errors
                .iter()
                .map(|e| format!("[{}] {}: {}", e.diag_code, e.path, e.message))
                .collect::<Vec<_>>();
            return Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "spec.deltas_applied found {} workspace spec scan error(s)",
                    diagnostics.len()
                ),
                diagnostics,
            });
        }
        let views: Vec<WorkspaceDeltaSpec<'_>> = scan
            .specs
            .iter()
            .map(|item| WorkspaceDeltaSpec::from_scanned(item, &effective_cols))
            .collect();
        let sources = select_delta_sources(&parsed, &scan.specs, &views, &effective_cols)?;
        let all_specs: Vec<&WorkspaceDeltaSpec<'_>> = scan
            .specs
            .iter()
            .zip(views.iter())
            .filter(|(s, _)| s.doc.status != SpecStatus::Cancelled)
            .map(|(_, view)| view)
            .collect();
        let report = verify_spec_deltas(&sources, &all_specs, parsed.target.as_deref());
        if !report.is_ok() {
            return Ok(DomainCheckOutcome::Failed {
                summary: format!(
                    "spec.deltas_applied found {} mismatch(es) across {} delta(s)",
                    report.diagnostics.len(),
                    report.deltas_checked
                ),
                diagnostics: report.diagnostics,
            });
        }
        Ok(DomainCheckOutcome::Passed {
            summary: format!(
                "spec.deltas_applied verified {} delta(s) across {} spec(s)",
                report.deltas_checked, report.specs_checked
            ),
            diagnostics: Vec::new(),
            execution_digest_salt: Some(format!("spec_deltas_live={live_digest}")),
        })
    }

    fn observe_deltas_freshness(
        &self,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        let _ = parse_spec_deltas_params(params)?;
        let worktree_adapter = resolve_worktree_adapter(&self.root, &req.worktree, &self.config)?;
        let digest = compute_specs_digest(
            &req.worktree,
            &worktree_adapter,
            params,
            req,
            "__spec_deltas_meta__",
        );
        Ok(FreshnessObservation::Passed {
            index_generation: None,
            execution_digest_salt: Some(format!("spec_deltas_live={digest}")),
        })
    }
}

impl DomainProvider for SpecDomainProvider {
    fn domain_id(&self) -> &str {
        SubsystemId::Spec.as_str()
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: Vec::new(),
            checks: vec![CheckCapability {
                name: "deltas_applied".to_owned(),
                input_schema: deltas_applied_input_schema(),
            }],
            context: Vec::new(),
            supports_collection_context: false,
            tools: Vec::new(),
        }
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        unsupported_domain_action(SubsystemId::Spec.as_str(), action)
    }

    fn run_check(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "deltas_applied" => self.run_deltas_applied_check(params, req),
            _ => Err(unknown_spec_check(check)),
        }
    }

    fn observe_freshness(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        match check {
            "deltas_applied" => self.observe_deltas_freshness(params, req),
            _ => Err(unknown_spec_check(check)),
        }
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> Result<Value> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("unknown spec context lens 'spec.{lens}'"),
        )
        .into())
    }

    fn call_tool(
        &self,
        tool: &str,
        _args: &Value,
        _req: &DomainToolReq,
    ) -> Result<DomainToolOutput> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("spec domain does not expose tool '{tool}'"),
        )
        .into())
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecDeltasParams {
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    specs: Option<Vec<String>>,
    #[serde(default)]
    target: Option<String>,
}

fn deltas_applied_input_schema() -> SchemaDef {
    let mut non_empty = SchemaDef::string();
    non_empty.min_length = Some(1);
    let mut specs_array = SchemaDef::array(non_empty.clone());
    specs_array.min_items = Some(1);

    let mut schema = SchemaDef::object();
    schema.properties.insert("spec".to_owned(), non_empty.clone());
    schema.properties.insert("specs".to_owned(), specs_array);
    schema.properties.insert("target".to_owned(), non_empty);
    schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
    schema
}

struct WorkspaceDeltaSpec<'a> {
    doc: &'a SpecDocument,
    declared_items: Option<BTreeSet<String>>,
}

impl<'a> WorkspaceDeltaSpec<'a> {
    fn from_scanned(
        item: &'a CollectedWorkspaceSpec,
        effective_cols: &BTreeMap<String, SpecConfig>,
    ) -> Self {
        let owning = item
            .collection_key
            .as_deref()
            .zip(item.collection.as_ref());
        let declared_items = resolve_spec_schema(item.doc.spec.as_deref(), owning, effective_cols)
            .ok()
            .map(|schema| schema.items.into_keys().collect());
        Self {
            doc: &item.doc,
            declared_items,
        }
    }
}

impl DeltaSpecView for WorkspaceDeltaSpec<'_> {
    fn delta_spec_id(&self) -> &str {
        self.doc.delta_spec_id()
    }

    fn delta_spec_digest(&self) -> &str {
        self.doc.delta_spec_digest()
    }

    fn delta_spec_deltas(&self) -> &[SpecDelta] {
        self.doc.delta_spec_deltas()
    }

    fn delta_field(&self, field: &str) -> Option<&Value> {
        self.doc.delta_field(field)
    }

    fn delta_named_entries(&self, field: &str) -> Option<Vec<(&str, Value)>> {
        if self.doc.items.contains_key(field) || (field == "tasks" && !self.doc.tasks.is_empty()) {
            return self.doc.delta_named_entries(field);
        }
        match &self.declared_items {
            Some(declared) => declared.contains(field).then(Vec::new),
            None => self.doc.delta_named_entries(field),
        }
    }
}

fn parse_spec_deltas_params(params: &Value) -> Result<SpecDeltasParams> {
    if params.is_null() {
        return Ok(SpecDeltasParams::default());
    }
    let parsed: SpecDeltasParams = serde_json::from_value(params.clone()).map_err(|err| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid spec.deltas_applied check parameters: {err}"),
        )
    })?;
    if let Some(ref target) = parsed.target {
        if !is_safe_path_segment_id(target) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "invalid spec.deltas_applied target '{target}': must be a safe non-empty spec identifier"
                ),
            )
            .into());
        }
    }
    Ok(parsed)
}

fn select_delta_sources<'a>(
    params: &SpecDeltasParams,
    scanned: &'a [CollectedWorkspaceSpec],
    views: &'a [WorkspaceDeltaSpec<'a>],
    effective_cols: &BTreeMap<String, SpecConfig>,
) -> Result<Vec<&'a WorkspaceDeltaSpec<'a>>> {
    let selectors = collect_spec_selectors(&params.spec, &params.specs);
    if selectors.is_empty() {
        return Ok(scanned
            .iter()
            .zip(views.iter())
            .filter(|(s, _)| s.doc.status != SpecStatus::Cancelled)
            .map(|(_, view)| view)
            .collect());
    }
    let mut selected = Vec::new();
    let mut seen_paths = BTreeSet::new();
    for sel in selectors {
        let mut matched_any = effective_cols
            .iter()
            .any(|(k, cfg)| k == sel || cfg.effective_builtin_base(Some(k)) == Some(sel));
        for (item, view) in scanned.iter().zip(views.iter()) {
            let exact_match = item.doc.id == sel || item.rel_path == sel;
            let matches_col = acdd_spec::schema::spec_matches_selector(
                item.doc.spec.as_deref(),
                item.collection_key.as_deref(),
                sel,
                effective_cols,
            );
            if exact_match || (matches_col && item.doc.status != SpecStatus::Cancelled) {
                matched_any = true;
                if seen_paths.insert(item.rel_path.as_str()) {
                    selected.push(view);
                }
            }
        }
        if !matched_any {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("spec.deltas_applied references unknown spec or collection '{sel}'"),
            )
            .into());
        }
    }
    Ok(selected)
}

fn unknown_spec_check(check: &str) -> anyhow::Error {
    AcddError::new(
        ErrorCode::DomainCapabilityUnavailable,
        format!("unknown spec check 'spec.{check}'"),
    )
    .into()
}
