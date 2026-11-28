use crate::errors::{AcddError, ErrorCode};
use crate::ids::{GateId, NodeCursor, RunId, WorkerId};
use crate::path_policy::WorkspacePathPolicy;
use crate::schema::{AdditionalPropertiesDef, SchemaDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// Canonical list of the 18 built-in static MCP tools that plugin tools must never collide with.
const BUILTIN_MCP_TOOLS: [&str; 18] = [
    // 9 code tools
    "code_map_overview",
    "code_map_search",
    "code_map_inspect",
    "code_map_impact",
    "code_map_analyze",
    "code_map_explain",
    "code_map_query",
    "code_map_lint",
    "ast_grep_search",
    // 2 docs tools
    "get_doc",
    "search_docs",
    // 2 core tools
    "acdd_read",
    "acdd_guide",
    // 5 state tools
    "acdd_state",
    "acdd_claim",
    "acdd_verify",
    "acdd_submit",
    "acdd_blackboard",
];

/// Retired static MCP tool names kept permanently reserved against external plugin registration.
const RETIRED_MCP_TOOLS: [&str; 2] = ["code_map_tests", "code_map_prove_removal"];

/// Return `true` if `name` matches one of the 18 built-in static MCP tools.
pub fn is_builtin_mcp_tool(name: &str) -> bool {
    BUILTIN_MCP_TOOLS.contains(&name)
}

/// Capability descriptor for a gate `call: <domain>.<action>` step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActionCapability {
    /// Unqualified action name within the domain (`<action>`).
    pub name: String,
    /// Schema validating `with:` parameters passed to `execute_action`.
    pub input_schema: SchemaDef,
    /// Whether this action mutates external or domain state.
    pub mutating: bool,
    /// Whether `lookup_action_result` can recover an uncertain outcome after a crash.
    pub supports_result_lookup: bool,
}

/// Capability descriptor for a gate `checks: [<domain>.<check>]` verifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CheckCapability {
    /// Unqualified check name within the domain (`<check>`).
    pub name: String,
    /// Schema validating optional check parameters.
    pub input_schema: SchemaDef,
}

/// Parsed canonical `context:` reference (`paths.<name>`, `specs.<name>`, or `<domain>.<name>`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextRef {
    /// Named path group lens (`paths.<name>`).
    Paths(String),
    /// Spec collection or built-in base selector lens (`specs.<name>`).
    Specs(String),
    /// Qualified domain lens (`<domain>.<name>`).
    Domain {
        /// Owning domain identifier.
        domain: String,
        /// Unqualified lens name within `domain`.
        name: String,
    },
}

impl ContextRef {
    /// Parse a raw `<prefix>.<name>` context reference fail-closed.
    pub fn parse(raw: &str) -> std::result::Result<Self, AcddError> {
        let (domain, item) = split_qualified(raw, "context lens")?;
        Ok(match domain {
            "paths" => Self::Paths(item.to_owned()),
            "specs" => Self::Specs(item.to_owned()),
            _ => Self::Domain {
                domain: domain.to_owned(),
                name: item.to_owned(),
            },
        })
    }

    /// Render the canonical `<prefix>.<name>` wire string.
    pub fn as_qualified_string(&self) -> String {
        match self {
            Self::Paths(name) => format!("paths.{name}"),
            Self::Specs(name) => format!("specs.{name}"),
            Self::Domain { domain, name } => format!("{domain}.{name}"),
        }
    }
}

impl fmt::Display for ContextRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paths(name) => write!(f, "paths.{name}"),
            Self::Specs(name) => write!(f, "specs.{name}"),
            Self::Domain { domain, name } => write!(f, "{domain}.{name}"),
        }
    }
}

/// Scope selection policy for a [`ContextCapability`] when assembling `DomainContextReq.admitted_scope`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScopeMode {
    /// Use full `admitted_scope` (including `extended_scope` and task fallback).
    #[default]
    Admitted,
    /// Prefer `node.scope` when non-empty, falling back to `admitted_scope`.
    NodeOnly,
}

/// Capability descriptor for a gate `context:` lens exported by a `DomainProvider`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContextCapability {
    /// Unqualified context lens name within the domain (`<lens>`).
    pub name: String,
    /// Scope selection policy (`admitted` or `node_only`).
    #[serde(default)]
    pub scope_mode: ContextScopeMode,
    /// Optional default `profile.paths.<key>` lookup used to populate `lens_globs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_paths_key: Option<String>,
    /// Schema validating optional `with:` parameters passed to `provide_context`.
    #[serde(default = "SchemaDef::object")]
    pub input_schema: SchemaDef,
}

impl ContextCapability {
    /// Construct a default `Admitted`-scope `ContextCapability` with an open object `input_schema`.
    pub fn simple(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scope_mode: ContextScopeMode::Admitted,
            default_paths_key: None,
            input_schema: SchemaDef::object(),
        }
    }
}

/// Optional MCP/CLI plugin tool exposed by an external `DomainProvider` as `<domain>_<name>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DomainToolDef {
    /// Unqualified tool suffix; registered globally as `<domain>_<name>`.
    pub name: String,
    /// Concise human/agent description shown in MCP `list_tools`.
    pub description: String,
    /// JSON Schema subset validating incoming tool arguments.
    pub input_schema: SchemaDef,
    /// Whether invoking this tool is read-only.
    pub read_only: bool,
}

/// Output returned by `DomainProvider::call_tool`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DomainToolOutput {
    /// Markdown response suitable for `ADR-0006` presentation.
    Markdown(String),
    /// Structured JSON payload compacted by the host response policy.
    Json(Value),
}

/// Complete capability manifest exported by a `DomainProvider`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DomainCapabilities {
    /// Actions usable via `call: <domain>.<action>` and `with:`.
    #[serde(default)]
    pub actions: Vec<ActionCapability>,
    /// Checks usable via `checks: [<domain>.<check>]`.
    #[serde(default)]
    pub checks: Vec<CheckCapability>,
    /// Context capabilities usable via `context: [<domain>.<lens>]` or `{ context, with }`.
    #[serde(default)]
    pub context: Vec<ContextCapability>,
    /// Whether this provider handles `paths.<name>` and `specs.<name>` collection context lenses.
    #[serde(default)]
    pub supports_collection_context: bool,
    /// Optional plugin tools exposed as `<domain>_<name>`.
    #[serde(default)]
    pub tools: Vec<DomainToolDef>,
}

/// Request context passed to `DomainProvider::execute_action`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainActionReq {
    /// Active run identifier.
    pub run_id: RunId,
    /// Cursor of the gate invoking the action.
    pub node_cursor: NodeCursor,
    /// Gate identifier.
    pub gate_id: GateId,
    /// Active claiming worker if claimed.
    pub worker_id: Option<WorkerId>,
    /// Primary repository workspace root.
    pub workspace_root: PathBuf,
    /// Worktree directory where the gate runs.
    pub worktree: PathBuf,
    /// Admitted write scope globs/paths for the active node.
    pub admitted_scope: Vec<String>,
}

/// Result returned by `DomainProvider::execute_action` and `lookup_action_result`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DomainActionResult {
    /// Short summary recorded in trace/artifact logs.
    pub summary: String,
    /// Structured action output payload.
    #[serde(default)]
    pub output: Value,
}

/// Request context passed to `DomainProvider::run_check`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainCheckReq {
    /// Active run identifier.
    pub run_id: RunId,
    /// Cursor of the gate executing the check.
    pub node_cursor: NodeCursor,
    /// Gate identifier.
    pub gate_id: GateId,
    /// Primary repository workspace root.
    pub workspace_root: PathBuf,
    /// Worktree directory where live files are inspected.
    pub worktree: PathBuf,
    /// Admitted scope globs/paths for the active node.
    pub admitted_scope: Vec<String>,
    /// Candidate changed paths relative to `worktree`.
    pub changed_paths: Vec<String>,
    /// Optional test target supplied via `--test` or prior `contract` proof.
    pub test_target: Option<String>,
    /// Workspace path policy for candidate/scope/engine-managed filtering.
    #[serde(default)]
    pub path_policy: WorkspacePathPolicy,
    /// Optional commit or tree OID representing claim-time baseline (`HEAD` when absent).
    #[serde(default)]
    pub baseline_ref: Option<String>,
    /// Effective task/node tags for domain checks (e.g. `["docs"]`).
    #[serde(default)]
    pub node_tags: Vec<String>,
}

/// Fail-closed check verdict (`Passed | Failed`, never `NotApplicable`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DomainCheckOutcome {
    /// The check passed; optional salt is folded into `execution_digest`, and optional
    /// non-blocking diagnostics (e.g. Diff X-Ray hints) are forwarded to `VerifyResp`.
    Passed {
        /// Human-readable verification summary.
        summary: String,
        /// Optional non-blocking diagnostics surfaced on passing verification.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<String>,
        /// Optional live-index generation or file-digest salt bound into `execution_digest`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_digest_salt: Option<String>,
    },
    /// The check failed with concrete diagnostics.
    Failed {
        /// Failure summary.
        summary: String,
        /// Detailed failure diagnostics.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<String>,
    },
}

/// Typed live freshness observation returned by [`DomainProvider::observe_freshness`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FreshnessObservation {
    /// Live observation/check passed; optional index generation and digest salt are folded into `execution_digest`.
    Passed {
        /// Optional live-index generation identifier.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index_generation: Option<String>,
        /// Optional live-index or file-digest salt matching `DomainCheckOutcome::Passed::execution_digest_salt`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_digest_salt: Option<String>,
    },
    /// Live check failed (`DomainCheckOutcome::Failed`), preventing stale trace reuse.
    Failed {
        /// Failure summary from `DomainCheckOutcome::Failed`.
        summary: String,
    },
}

/// Request context passed to `DomainProvider::provide_context`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainContextReq {
    /// Active run identifier.
    pub run_id: RunId,
    /// Cursor of the claimed/inspected node.
    pub node_cursor: NodeCursor,
    /// Gate identifier.
    pub gate_id: GateId,
    /// Primary repository workspace root.
    pub workspace_root: PathBuf,
    /// Worktree directory for the claimed node.
    pub worktree: PathBuf,
    /// Admitted scope globs/paths for the node.
    pub admitted_scope: Vec<String>,
    /// Optional task target identifier/path.
    pub target: Option<String>,
    /// Remaining response byte budget available for this lens.
    pub max_bytes: usize,
    /// Workspace path policy for candidate/scope/engine-managed filtering.
    #[serde(default)]
    pub path_policy: WorkspacePathPolicy,
    /// Optional commit or tree OID representing claim-time baseline (`HEAD` when absent).
    #[serde(default)]
    pub baseline_ref: Option<String>,
    /// Resolved path globs for `paths.<name>` or `docs.invariants` (`paths.docs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lens_globs: Vec<String>,
    /// Owning Spec body and citation context used for Tier 1 direct ID/title matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_text: Option<String>,
    /// Optional `with:` parameters validated against `ContextCapability.input_schema`.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// Request context passed to `DomainProvider::call_tool`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainToolReq {
    /// Primary repository workspace root.
    pub workspace_root: PathBuf,
    /// Optional target worktree if scoped to an active run/task.
    pub worktree: Option<PathBuf>,
    /// Optional active run identifier.
    pub run_id: Option<RunId>,
    /// Optional active node cursor.
    pub node_cursor: Option<NodeCursor>,
}

/// Service Provider Interface implemented by host (`code`, `docs`) and external domain plugins.
pub trait DomainProvider: Send + Sync {
    /// Unique lowercase/alphanumeric domain identifier (`"code"`, `"docs"`, `"http"`, etc.).
    fn domain_id(&self) -> &str;

    /// Declare all actions, checks, context lenses, and optional plugin tools supported by this provider.
    fn capabilities(&self) -> DomainCapabilities;

    /// Execute a gate `call: <domain>.<action>` step.
    fn execute_action(
        &self,
        action: &str,
        params: &Value,
        idempotency_key: &str,
        req: &DomainActionReq,
    ) -> Result<DomainActionResult>;

    /// Look up the outcome of a previously started mutating action after crash recovery.
    fn lookup_action_result(
        &self,
        _action: &str,
        _idempotency_key: &str,
    ) -> Result<Option<DomainActionResult>> {
        Ok(None)
    }

    /// Execute a gate `checks: [<domain>.<check>]` verifier.
    fn run_check(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome>;

    /// Observe freshness/digest inputs for a `checks: [<domain>.<check>]` verifier.
    ///
    /// By default, delegates to [`DomainProvider::run_check`] and distinguishes a passing
    /// check ([`FreshnessObservation::Passed`]) from a failing check ([`FreshnessObservation::Failed`])
    /// so `acdd submit` never confuses a failed check with an unsalted pass.
    fn observe_freshness(
        &self,
        check: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        match self.run_check(check, params, req)? {
            DomainCheckOutcome::Passed {
                execution_digest_salt,
                ..
            } => Ok(FreshnessObservation::Passed {
                index_generation: None,
                execution_digest_salt,
            }),
            DomainCheckOutcome::Failed { summary, .. } => {
                Ok(FreshnessObservation::Failed { summary })
            }
        }
    }

    /// Materialize a `context: [<domain>.<lens>]` section for `ClaimResp` / `ShowResp`.
    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value>;

    /// Invoke an optional plugin tool registered under `<domain>_<name>`.
    fn call_tool(
        &self,
        _tool: &str,
        _args: &Value,
        _req: &DomainToolReq,
    ) -> Result<DomainToolOutput> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!(
                "domain '{}' does not implement plugin tools",
                self.domain_id()
            ),
        )
        .into())
    }
}

/// Fail-closed domain provider stub that exposes no capabilities and rejects every invocation.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct NoopDomainProvider {
    domain_id: String,
}

#[cfg(test)]
impl Default for NoopDomainProvider {
    fn default() -> Self {
        Self {
            domain_id: "noop".to_owned(),
        }
    }
}

#[cfg(test)]
impl DomainProvider for NoopDomainProvider {
    fn domain_id(&self) -> &str {
        &self.domain_id
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities::default()
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!(
                "action '{}.{}' is unavailable on noop domain provider",
                self.domain_id, action
            ),
        )
        .into())
    }

    fn run_check(
        &self,
        check: &str,
        _params: &Value,
        _req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!(
                "check '{}.{}' is unavailable on noop domain provider",
                self.domain_id, check
            ),
        )
        .into())
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> Result<Value> {
        Err(AcddError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!(
                "context lens '{}.{}' is unavailable on noop domain provider",
                self.domain_id, lens
            ),
        )
        .into())
    }
}

struct RegisteredProvider {
    provider: Arc<dyn DomainProvider>,
    capabilities: DomainCapabilities,
    enabled: bool,
}

#[derive(Clone)]
struct RegisteredPluginTool {
    domain_id: String,
    def: DomainToolDef,
}

/// Fail-closed registry and dispatcher composing host and external `DomainProvider`s.
///
/// Rejects duplicate `domain_id`, duplicate `<domain>_<name>` plugin tool names, invalid capability schemas,
/// and collisions with the 20 built-in MCP tools at construction time.
#[derive(Default)]
pub struct CompositeDomainProvider {
    providers: BTreeMap<String, RegisteredProvider>,
    plugin_tools: BTreeMap<String, RegisteredPluginTool>,
    collection_context_domain: Option<String>,
}

impl CompositeDomainProvider {
    /// Register domain providers while honoring explicit `modules:` enablement flags (`domain_id -> bool`).
    ///
    /// Built-in `"code"`, `"docs"`, and `"spec"` default to enabled (`true`) when absent from `enabled_flags`;
    /// external domain providers default to disabled (`false`) unless explicitly enabled.
    pub fn with_module_flags(
        providers: Vec<Arc<dyn DomainProvider>>,
        enabled_flags: BTreeMap<String, bool>,
    ) -> std::result::Result<Self, AcddError> {
        let mut composite = Self::default();
        for provider in providers {
            let domain_id = provider.domain_id().to_owned();
            validate_component_name(&domain_id, "domain_id")?;
            if composite.providers.contains_key(&domain_id) {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
                    format!("duplicate DomainProvider registration for domain '{domain_id}'"),
                ));
            }

            let caps = provider.capabilities();
            validate_capabilities(&domain_id, &caps)?;
            if caps.supports_collection_context {
                if let Some(ref existing) = composite.collection_context_domain {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!(
                            "duplicate collection context provider registered by domains '{existing}' and '{domain_id}'"
                        ),
                    ));
                }
                composite.collection_context_domain = Some(domain_id.clone());
            }

            for tool in &caps.tools {
                let full_name = format!("{domain_id}_{}", tool.name);
                if is_builtin_mcp_tool(&full_name) {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!(
                            "plugin tool '{full_name}' from domain '{domain_id}' collides with built-in MCP tool"
                        ),
                    ));
                }
                if RETIRED_MCP_TOOLS.contains(&full_name.as_str()) {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!(
                            "plugin tool '{full_name}' from domain '{domain_id}' collides with reserved retired MCP tool"
                        ),
                    ));
                }
                if let Some(existing) = composite.plugin_tools.get(&full_name) {
                    return Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!(
                            "duplicate plugin tool '{full_name}' registered by domains '{}' and '{domain_id}'",
                            existing.domain_id
                        ),
                    ));
                }
                composite.plugin_tools.insert(
                    full_name,
                    RegisteredPluginTool {
                        domain_id: domain_id.clone(),
                        def: tool.clone(),
                    },
                );
            }

            let default_enabled = matches!(domain_id.as_str(), "code" | "docs" | "spec");
            let enabled = enabled_flags
                .get(&domain_id)
                .copied()
                .unwrap_or(default_enabled);
            composite.providers.insert(
                domain_id,
                RegisteredProvider {
                    provider,
                    capabilities: caps,
                    enabled,
                },
            );
        }
        Ok(composite)
    }

    /// Return whether `domain_id` is registered and enabled.
    pub fn is_domain_enabled(&self, domain_id: &str) -> bool {
        self.providers
            .get(domain_id)
            .is_some_and(|entry| entry.enabled)
    }

    /// Return all active plugin tools as `(full_name, DomainToolDef)` sorted by full name (`<domain>_<name>`).
    pub fn plugin_tools(&self) -> Vec<(String, DomainToolDef)> {
        self.plugin_tools
            .iter()
            .filter(|(_, tool)| self.is_domain_enabled(&tool.domain_id))
            .map(|(full_name, tool)| (full_name.clone(), tool.def.clone()))
            .collect()
    }

    /// Return `true` if `full_name` is a registered plugin tool on an enabled domain.
    pub fn has_enabled_plugin_tool(&self, full_name: &str) -> bool {
        self.plugin_tools
            .get(full_name)
            .is_some_and(|tool| self.is_domain_enabled(&tool.domain_id))
    }

    fn resolve_provider_capability<'s, 'q, T>(
        &'s self,
        qualified: &'q str,
        kind: &'static str,
        items: impl FnOnce(&'s DomainCapabilities) -> &'s [T],
        name_of: impl Fn(&T) -> &str,
    ) -> std::result::Result<(&'s RegisteredProvider, &'q str, &'s T), AcddError> {
        let (domain, cap_name) = split_qualified(qualified, kind)?;
        let entry = self.active_provider(domain)?;
        let cap = items(&entry.capabilities)
            .iter()
            .find(|item| name_of(item) == cap_name)
            .ok_or_else(|| {
                AcddError::new(
                    ErrorCode::DomainCapabilityUnavailable,
                    format!("{kind} '{qualified}' is not provided by domain '{domain}'"),
                )
            })?;
        Ok((entry, cap_name, cap))
    }

    fn resolve_action_capability<'s, 'q>(
        &'s self,
        qualified: &'q str,
    ) -> std::result::Result<(&'s RegisteredProvider, &'q str, &'s ActionCapability), AcddError>
    {
        self.resolve_provider_capability(qualified, "action", |c| &c.actions, |a| &a.name)
    }

    fn resolve_check_capability<'s, 'q>(
        &'s self,
        qualified: &'q str,
    ) -> std::result::Result<(&'s RegisteredProvider, &'q str, &'s CheckCapability), AcddError>
    {
        self.resolve_provider_capability(qualified, "check", |c| &c.checks, |c| &c.name)
    }

    /// Validate a qualified action reference (`<domain>.<action>`) and its `with:` parameters fail-closed.
    pub fn validate_action_ref(
        &self,
        qualified: &str,
        params: &Value,
    ) -> std::result::Result<(), AcddError> {
        let (_, _, cap) = self.resolve_action_capability(qualified)?;
        cap.input_schema
            .validate_value(params, &format!("call.{qualified}.with"))
    }

    /// Validate a qualified check reference (`<domain>.<check>`) and its parameters fail-closed.
    pub fn validate_check_ref(
        &self,
        qualified: &str,
        params: &Value,
    ) -> std::result::Result<(), AcddError> {
        let (_, _, cap) = self.resolve_check_capability(qualified)?;
        cap.input_schema
            .validate_value(params, &format!("checks.{qualified}"))
    }

    /// Return the `domain_id` of the registered provider declaring `supports_collection_context: true`, if any.
    pub fn collection_context_domain(&self) -> Option<&str> {
        self.collection_context_domain.as_deref()
    }

    fn active_collection_provider<'s>(
        &'s self,
        ctx_ref: &ContextRef,
    ) -> std::result::Result<&'s RegisteredProvider, AcddError> {
        let Some(ref domain) = self.collection_context_domain else {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!(
                    "no registered domain provider supports collection context '{ctx_ref}'"
                ),
            ));
        };
        self.active_provider(domain)
    }

    /// Look up the [`ContextCapability`] metadata for a [`ContextRef`] (`Ok(None)` for `paths.*` / `specs.*`).
    pub fn context_capability(
        &self,
        ctx_ref: &ContextRef,
    ) -> std::result::Result<Option<&ContextCapability>, AcddError> {
        match ctx_ref {
            ContextRef::Paths(_) | ContextRef::Specs(_) => {
                let _ = self.active_collection_provider(ctx_ref)?;
                Ok(None)
            }
            ContextRef::Domain { domain, name } => {
                let qualified = ctx_ref.as_qualified_string();
                let (_, _, cap) = self.resolve_provider_capability(
                    &qualified,
                    "context lens",
                    |c| &c.context,
                    |c| &c.name,
                )?;
                let _ = (domain, name);
                Ok(Some(cap))
            }
        }
    }

    /// Validate a [`ContextRef`] and its optional `with:` parameters fail-closed.
    pub fn validate_context_ref(
        &self,
        ctx_ref: &ContextRef,
        params: &Value,
    ) -> std::result::Result<(), AcddError> {
        match self.context_capability(ctx_ref)? {
            None => {
                let schema = collection_context_input_schema();
                validate_context_params(&schema, params, &format!("context.{ctx_ref}.with"))
            }
            Some(cap) => {
                validate_context_params(&cap.input_schema, params, &format!("context.{ctx_ref}.with"))
            }
        }
    }

    /// Validate that a plugin tool name `<domain>_<name>` is registered and its domain is enabled.
    pub fn validate_plugin_tool_name(
        &self,
        full_name: &str,
    ) -> std::result::Result<(), AcddError> {
        let Some(tool) = self.plugin_tools.get(full_name) else {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("plugin tool '{full_name}' is not registered"),
            ));
        };
        if !self.is_domain_enabled(&tool.domain_id) {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!(
                    "plugin tool '{full_name}' is unavailable because domain '{}' is disabled",
                    tool.domain_id
                ),
            ));
        }
        Ok(())
    }

    /// Return the `ActionCapability` metadata for a qualified `<domain>.<action>`.
    pub fn action_capability(
        &self,
        qualified: &str,
    ) -> std::result::Result<&ActionCapability, AcddError> {
        let (_, _, cap) = self.resolve_action_capability(qualified)?;
        Ok(cap)
    }

    /// Validate and dispatch `call: <domain>.<action>` to the owning enabled provider.
    pub fn execute_action(
        &self,
        qualified: &str,
        params: &Value,
        idempotency_key: &str,
        req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        let (entry, action_name, cap) = self.resolve_action_capability(qualified)?;
        cap.input_schema
            .validate_value(params, &format!("call.{qualified}.with"))?;
        entry
            .provider
            .execute_action(action_name, params, idempotency_key, req)
    }

    /// Look up a previously started mutating action result on the owning enabled provider.
    pub fn lookup_action_result(
        &self,
        qualified: &str,
        idempotency_key: &str,
    ) -> Result<Option<DomainActionResult>> {
        let (entry, action_name, _cap) = self.resolve_action_capability(qualified)?;
        entry
            .provider
            .lookup_action_result(action_name, idempotency_key)
    }

    /// Validate and execute `checks: [<domain>.<check>]` on the owning enabled provider.
    pub fn run_check(
        &self,
        qualified: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        let (entry, check_name, cap) = self.resolve_check_capability(qualified)?;
        cap.input_schema
            .validate_value(params, &format!("checks.{qualified}"))?;
        entry.provider.run_check(check_name, params, req)
    }

    /// Validate and observe freshness inputs for `checks: [<domain>.<check>]` on the owning enabled provider.
    pub fn observe_freshness(
        &self,
        qualified: &str,
        params: &Value,
        req: &DomainCheckReq,
    ) -> Result<FreshnessObservation> {
        let (entry, check_name, cap) = self.resolve_check_capability(qualified)?;
        cap.input_schema
            .validate_value(params, &format!("checks.{qualified}"))?;
        entry.provider.observe_freshness(check_name, params, req)
    }

    /// Validate and fetch `context:` (`paths.<name>`, `specs.<name>`, or `<domain>.<lens>`) from the owning enabled provider.
    pub fn provide_context(&self, qualified: &str, req: &DomainContextReq) -> Result<Value> {
        let ctx_ref = ContextRef::parse(qualified)?;
        self.validate_context_ref(&ctx_ref, &req.params)?;
        match &ctx_ref {
            ContextRef::Paths(_) | ContextRef::Specs(_) => {
                let entry = self.active_collection_provider(&ctx_ref)?;
                entry.provider.provide_context(qualified, req)
            }
            ContextRef::Domain { domain, name } => {
                let entry = self.active_provider(domain)?;
                entry.provider.provide_context(name, req)
            }
        }
    }

    /// Validate and invoke an external plugin tool `<domain>_<name>`.
    pub fn call_tool(
        &self,
        full_name: &str,
        args: &Value,
        req: &DomainToolReq,
    ) -> Result<DomainToolOutput> {
        let Some(tool) = self.plugin_tools.get(full_name) else {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("plugin tool '{full_name}' is not registered"),
            )
            .into());
        };
        let entry = self.active_provider(&tool.domain_id)?;
        tool.def
            .input_schema
            .validate_value(args, &format!("tool.{full_name}.args"))?;
        entry.provider.call_tool(&tool.def.name, args, req)
    }

    fn active_provider(
        &self,
        domain: &str,
    ) -> std::result::Result<&RegisteredProvider, AcddError> {
        let Some(entry) = self.providers.get(domain) else {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("domain provider '{domain}' is not registered"),
            ));
        };
        if !entry.enabled {
            return Err(AcddError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("domain provider '{domain}' is disabled in modules configuration"),
            ));
        }
        Ok(entry)
    }
}

fn validate_component_name(name: &str, kind: &'static str) -> std::result::Result<(), AcddError> {
    crate::ids::validate_standard_ident(name, kind).map_err(AcddError::from)
}

fn validate_capabilities(
    domain_id: &str,
    caps: &DomainCapabilities,
) -> std::result::Result<(), AcddError> {
    let mut seen_actions = BTreeSet::new();
    for action in &caps.actions {
        validate_component_name(&action.name, "action capability name")?;
        if !seen_actions.insert(&action.name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "duplicate action capability '{}.{}'",
                    domain_id, action.name
                ),
            ));
        }
        action
            .input_schema
            .validate_schema(&format!("domains.{domain_id}.actions.{}", action.name))?;
    }

    let mut seen_checks = BTreeSet::new();
    for check in &caps.checks {
        validate_component_name(&check.name, "check capability name")?;
        if !seen_checks.insert(&check.name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("duplicate check capability '{}.{}'", domain_id, check.name),
            ));
        }
        check
            .input_schema
            .validate_schema(&format!("domains.{domain_id}.checks.{}", check.name))?;
    }

    let mut seen_context = BTreeSet::new();
    for ctx_cap in &caps.context {
        validate_component_name(&ctx_cap.name, "context capability name")?;
        if !seen_context.insert(&ctx_cap.name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "duplicate context capability '{}.{}'",
                    domain_id, ctx_cap.name
                ),
            ));
        }
        if let Some(ref key) = ctx_cap.default_paths_key {
            validate_component_name(key, "default_paths_key")?;
        }
        ctx_cap
            .input_schema
            .validate_schema(&format!("domains.{domain_id}.context.{}", ctx_cap.name))?;
    }

    let mut seen_tools = BTreeSet::new();
    for tool in &caps.tools {
        validate_component_name(&tool.name, "plugin tool name")?;
        if !seen_tools.insert(&tool.name) {
            return Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!("duplicate tool '{}' in domain '{domain_id}'", tool.name),
            ));
        }
        tool.input_schema
            .validate_schema(&format!("domains.{domain_id}.tools.{}", tool.name))?;
    }

    Ok(())
}

fn split_qualified<'a>(
    qualified: &'a str,
    kind: &'static str,
) -> std::result::Result<(&'a str, &'a str), AcddError> {
    let Some((domain, item)) = qualified.split_once('.') else {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{kind} reference '{qualified}' must be qualified as '<domain>.<name>'"),
        ));
    };
    if domain.is_empty() || item.is_empty() || item.contains('.') {
        return Err(AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("{kind} reference '{qualified}' must be qualified as '<domain>.<name>'"),
        ));
    }
    Ok((domain, item))
}

fn collection_context_input_schema() -> SchemaDef {
    let mut schema = SchemaDef::object();
    schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
    schema
}

fn validate_context_params(
    schema: &SchemaDef,
    params: &Value,
    path: &str,
) -> std::result::Result<(), AcddError> {
    if params.is_null() && schema.schema_type == "object" {
        schema.validate_value(&json!({}), path)
    } else {
        schema.validate_value(params, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AdditionalPropertiesDef;
    use serde_json::json;

    struct MockProvider {
        domain: String,
        caps: DomainCapabilities,
    }

    impl DomainProvider for MockProvider {
        fn domain_id(&self) -> &str {
            &self.domain
        }

        fn capabilities(&self) -> DomainCapabilities {
            self.caps.clone()
        }

        fn execute_action(
            &self,
            action: &str,
            params: &Value,
            idempotency_key: &str,
            _req: &DomainActionReq,
        ) -> Result<DomainActionResult> {
            Ok(DomainActionResult {
                summary: format!("{}.{}:{idempotency_key}", self.domain, action),
                output: params.clone(),
            })
        }

        fn lookup_action_result(
            &self,
            action: &str,
            idempotency_key: &str,
        ) -> Result<Option<DomainActionResult>> {
            Ok(Some(DomainActionResult {
                summary: format!("lookup:{}.{}:{idempotency_key}", self.domain, action),
                output: json!({"recovered": true}),
            }))
        }

        fn run_check(
            &self,
            check: &str,
            params: &Value,
            _req: &DomainCheckReq,
        ) -> Result<DomainCheckOutcome> {
            if params.get("fail") == Some(&json!(true)) {
                return Ok(DomainCheckOutcome::Failed {
                    summary: format!("{}.{check} failed", self.domain),
                    diagnostics: vec!["forced failure".to_owned()],
                });
            }
            let salt = if params.get("unsalted") == Some(&json!(true)) {
                None
            } else {
                Some("gen-42".to_owned())
            };
            Ok(DomainCheckOutcome::Passed {
                summary: format!("{}.{check} ok", self.domain),
                diagnostics: Vec::new(),
                execution_digest_salt: salt,
            })
        }

        fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
            if req.params.is_null() {
                Ok(json!({ "lens": format!("{}.{lens}", self.domain) }))
            } else {
                Ok(json!({ "lens": format!("{}.{lens}", self.domain), "params": req.params }))
            }
        }

        fn call_tool(
            &self,
            tool: &str,
            args: &Value,
            _req: &DomainToolReq,
        ) -> Result<DomainToolOutput> {
            Ok(DomainToolOutput::Markdown(format!(
                "called {}_{tool} with {args}",
                self.domain
            )))
        }
    }

    fn strict_url_schema() -> SchemaDef {
        let mut s = SchemaDef::object();
        s.properties.insert("url".to_owned(), SchemaDef::string());
        s.required = vec!["url".to_owned()];
        s.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
        s
    }

    fn sample_provider(domain: &str, tool_names: &[&str]) -> Arc<dyn DomainProvider> {
        Arc::new(MockProvider {
            domain: domain.to_owned(),
            caps: DomainCapabilities {
                actions: vec![ActionCapability {
                    name: "probe".to_owned(),
                    input_schema: strict_url_schema(),
                    mutating: true,
                    supports_result_lookup: true,
                }],
                checks: vec![CheckCapability {
                    name: "health".to_owned(),
                    input_schema: SchemaDef::object(),
                }],
                context: vec![ContextCapability::simple("endpoints")],
                supports_collection_context: false,
                tools: tool_names
                    .iter()
                    .map(|name| DomainToolDef {
                        name: (*name).to_owned(),
                        description: "test plugin tool".to_owned(),
                        input_schema: strict_url_schema(),
                        read_only: true,
                    })
                    .collect(),
            },
        })
    }

    fn sample_action_req() -> DomainActionReq {
        DomainActionReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: NodeCursor::root(vec![GateId::new("build").unwrap()]).unwrap(),
            gate_id: GateId::new("build").unwrap(),
            worker_id: Some(WorkerId::new("w1").unwrap()),
            workspace_root: PathBuf::from("/repo"),
            worktree: PathBuf::from("/repo"),
            admitted_scope: vec!["src/**".to_owned()],
        }
    }

    fn sample_check_req() -> DomainCheckReq {
        DomainCheckReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: NodeCursor::root(vec![GateId::new("build").unwrap()]).unwrap(),
            gate_id: GateId::new("build").unwrap(),
            workspace_root: PathBuf::from("/repo"),
            worktree: PathBuf::from("/repo"),
            admitted_scope: vec!["src/**".to_owned()],
            changed_paths: vec![],
            test_target: None,
            path_policy: WorkspacePathPolicy::default(),
            baseline_ref: None,
            node_tags: vec![],
        }
    }

    #[test]
    fn composite_registration_rejects_duplicates_and_builtin_collisions() {
        assert_eq!(BUILTIN_MCP_TOOLS.len(), 18);
        assert!(!is_builtin_mcp_tool("code_map_tests"));
        assert!(!is_builtin_mcp_tool("code_map_prove_removal"));

        let collision_provider = sample_provider("acdd", &["read"]);
        let lint_collision_provider = sample_provider("code", &["map_lint"]);
        let retired_tests_provider = sample_provider("code", &["map_tests"]);
        let retired_removal_provider = sample_provider("code", &["map_prove_removal"]);
        let dup_domain_a = sample_provider("http", &["request"]);
        let dup_domain_b = sample_provider("http", &["inspect"]);
        let cross_tool_a = sample_provider("foo", &["bar_baz"]);
        let cross_tool_b = sample_provider("foo_bar", &["baz"]);

        type RegCase<'a> = (&'a str, Vec<Arc<dyn DomainProvider>>, Option<ErrorCode>);
        let cases: &[RegCase<'_>] = &[
            (
                "valid_distinct",
                vec![
                    sample_provider("http", &["request"]),
                    sample_provider("sql", &["explain"]),
                ],
                None,
            ),
            (
                "collides_with_builtin_acdd_read",
                vec![collision_provider],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
            (
                "collides_with_builtin_code_map_lint",
                vec![lint_collision_provider],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
            (
                "collides_with_retired_code_map_tests",
                vec![retired_tests_provider],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
            (
                "collides_with_retired_code_map_prove_removal",
                vec![retired_removal_provider],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
            (
                "duplicate_domain_id",
                vec![dup_domain_a, dup_domain_b],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
            (
                "duplicate_full_tool_name_across_domains",
                vec![cross_tool_a, cross_tool_b],
                Some(ErrorCode::WorkflowSchemaInvalid),
            ),
        ];

        for (label, providers, expected_err) in cases {
            let flags = providers
                .iter()
                .map(|p| (p.domain_id().to_owned(), true))
                .collect();
            let res = CompositeDomainProvider::with_module_flags(providers.clone(), flags);
            match expected_err {
                None => assert!(res.is_ok(), "{label}: unexpected err {:?}", res.err()),
                Some(code) => {
                    let err = res.err().expect("expected error");
                    assert_eq!(err.code, *code, "{label}: {err}");
                }
            }
        }
    }

    #[test]
    fn composite_dispatch_enforces_enablement_capabilities_and_schemas_fail_closed() {
        let flags = BTreeMap::from([("http".to_owned(), true), ("disabled".to_owned(), false)]);
        let composite = CompositeDomainProvider::with_module_flags(
            vec![
                sample_provider("code", &["seam"]),
                sample_provider("http", &["request"]),
                sample_provider("disabled", &["ping"]),
                sample_provider("unlisted", &["check"]),
            ],
            flags,
        )
        .unwrap();
        assert!(composite.is_domain_enabled("code"));
        assert!(!composite.is_domain_enabled("unlisted"));

        let action_req = sample_action_req();
        let check_req = sample_check_req();
        let ctx_req = DomainContextReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: NodeCursor::root(vec![GateId::new("build").unwrap()]).unwrap(),
            gate_id: GateId::new("build").unwrap(),
            workspace_root: PathBuf::from("/repo"),
            worktree: PathBuf::from("/repo"),
            admitted_scope: vec!["src/**".to_owned()],
            target: None,
            max_bytes: 4096,
            path_policy: WorkspacePathPolicy::default(),
            baseline_ref: None,
            lens_globs: Vec::new(),
            citation_text: None,
            params: Value::Null,
        };
        let tool_req = DomainToolReq {
            workspace_root: PathBuf::from("/repo"),
            worktree: None,
            run_id: None,
            node_cursor: None,
        };

        // Plugin tools list includes default-on built-in + enabled domains, excludes disabled/unlisted
        let tools: Vec<String> = composite
            .plugin_tools()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(tools, vec!["code_seam", "http_request"]);

        // Happy path across all 4 SPI surfaces
        let act = composite
            .execute_action(
                "http.probe",
                &json!({"url": "https://example.test"}),
                "k1",
                &action_req,
            )
            .unwrap();
        assert_eq!(act.summary, "http.probe:k1");
        assert!(composite
            .lookup_action_result("http.probe", "k1")
            .unwrap()
            .is_some());
        assert!(matches!(
            composite
                .run_check("http.health", &json!({}), &check_req)
                .unwrap(),
            DomainCheckOutcome::Passed { .. }
        ));
        assert_eq!(
            composite
                .observe_freshness("http.health", &json!({}), &check_req)
                .unwrap(),
            FreshnessObservation::Passed {
                index_generation: None,
                execution_digest_salt: Some("gen-42".to_owned()),
            }
        );
        assert_eq!(
            composite
                .observe_freshness("http.health", &json!({"unsalted": true}), &check_req)
                .unwrap(),
            FreshnessObservation::Passed {
                index_generation: None,
                execution_digest_salt: None,
            }
        );
        assert_eq!(
            composite
                .observe_freshness("http.health", &json!({"fail": true}), &check_req)
                .unwrap(),
            FreshnessObservation::Failed {
                summary: "http.health failed".to_owned(),
            }
        );
        assert_eq!(
            composite
                .provide_context("http.endpoints", &ctx_req)
                .unwrap(),
            json!({"lens": "http.endpoints"})
        );
        assert!(matches!(
            composite
                .call_tool(
                    "http_request",
                    &json!({"url": "https://example.test"}),
                    &tool_req
                )
                .unwrap(),
            DomainToolOutput::Markdown(_)
        ));

        // Fail-closed matrix (no NotApplicable!)
        let fail_cases: &[(&str, anyhow::Result<()>, ErrorCode)] = &[
            (
                "disabled_domain_action",
                composite
                    .execute_action("disabled.probe", &json!({"url": "x"}), "k", &action_req)
                    .map(|_| ()),
                ErrorCode::DomainCapabilityUnavailable,
            ),
            (
                "unregistered_domain_check",
                composite
                    .run_check("missing.health", &json!({}), &check_req)
                    .map(|_| ()),
                ErrorCode::DomainCapabilityUnavailable,
            ),
            (
                "unknown_lens",
                composite
                    .provide_context("http.unknown", &ctx_req)
                    .map(|_| ()),
                ErrorCode::DomainCapabilityUnavailable,
            ),
            (
                "disabled_plugin_tool",
                composite
                    .call_tool("disabled_ping", &json!({"url": "x"}), &tool_req)
                    .map(|_| ()),
                ErrorCode::DomainCapabilityUnavailable,
            ),
            (
                "unlisted_external_domain_default_off",
                composite
                    .call_tool("unlisted_check", &json!({"url": "x"}), &tool_req)
                    .map(|_| ()),
                ErrorCode::DomainCapabilityUnavailable,
            ),
            (
                "action_schema_violation",
                composite
                    .execute_action("http.probe", &json!({"wrong": 1}), "k", &action_req)
                    .map(|_| ()),
                ErrorCode::WorkflowSchemaInvalid,
            ),
            (
                "tool_schema_violation",
                composite
                    .call_tool("http_request", &json!({"url": 123}), &tool_req)
                    .map(|_| ()),
                ErrorCode::WorkflowSchemaInvalid,
            ),
        ];

        for (label, res, expected_code) in fail_cases {
            let err = res.as_ref().unwrap_err();
            let acdd_err = AcddError::from_anyhow(err).expect("expected AcddError");
            assert_eq!(acdd_err.code, *expected_code, "{label}: {acdd_err}");
        }

        // Noop provider fails closed with DomainCapabilityUnavailable
        let noop = NoopDomainProvider::default();
        assert_eq!(noop.domain_id(), "noop");
        let noop_err = noop.run_check("x", &json!({}), &check_req).unwrap_err();
        assert_eq!(
            AcddError::from_anyhow(&noop_err).unwrap().code,
            ErrorCode::DomainCapabilityUnavailable
        );
    }

    #[test]
    fn resolve_capability_parity_across_validation_inspection_and_dispatch() {
        let composite = CompositeDomainProvider::with_module_flags(
            vec![sample_provider("http", &["request"])],
            BTreeMap::from([("http".to_owned(), true)]),
        )
        .unwrap();
        let cap = composite.action_capability("http.probe").unwrap();
        assert_eq!(cap.name, "probe");
        assert!(cap.mutating);
        assert!(cap.supports_result_lookup);

        let action_req = sample_action_req();
        let check_req = sample_check_req();

        for (qualified, expected) in [
            ("http.probe", Ok(())),
            ("http.missing", Err(ErrorCode::DomainCapabilityUnavailable)),
            ("missing.probe", Err(ErrorCode::DomainCapabilityUnavailable)),
            ("malformed", Err(ErrorCode::WorkflowSchemaInvalid)),
        ] {
            let val_res = composite
                .validate_action_ref(qualified, &json!({"url": "https://example.test"}))
                .map_err(|e| e.code);
            let cap_res = composite
                .action_capability(qualified)
                .map(|_| ())
                .map_err(|e| e.code);
            let exec_res = composite
                .execute_action(
                    qualified,
                    &json!({"url": "https://example.test"}),
                    "k1",
                    &action_req,
                )
                .map(|_| ())
                .map_err(|e| AcddError::from_anyhow(&e).unwrap().code);
            let lookup_res = composite
                .lookup_action_result(qualified, "k1")
                .map(|_| ())
                .map_err(|e| AcddError::from_anyhow(&e).unwrap().code);
            assert_eq!(val_res, expected, "validate_action_ref({qualified})");
            assert_eq!(cap_res, expected, "action_capability({qualified})");
            assert_eq!(exec_res, expected, "execute_action({qualified})");
            assert_eq!(lookup_res, expected, "lookup_action_result({qualified})");
        }

        for (qualified, expected) in [
            ("http.health", Ok(())),
            ("http.missing", Err(ErrorCode::DomainCapabilityUnavailable)),
            (
                "missing.health",
                Err(ErrorCode::DomainCapabilityUnavailable),
            ),
            ("malformed", Err(ErrorCode::WorkflowSchemaInvalid)),
        ] {
            let val_res = composite
                .validate_check_ref(qualified, &json!({}))
                .map_err(|e| e.code);
            let run_res = composite
                .run_check(qualified, &json!({}), &check_req)
                .map(|_| ())
                .map_err(|e| AcddError::from_anyhow(&e).unwrap().code);
            let obs_res = composite
                .observe_freshness(qualified, &json!({}), &check_req)
                .map(|_| ())
                .map_err(|e| AcddError::from_anyhow(&e).unwrap().code);
            assert_eq!(val_res, expected, "validate_check_ref({qualified})");
            assert_eq!(run_res, expected, "run_check({qualified})");
            assert_eq!(obs_res, expected, "observe_freshness({qualified})");
        }
    }

    #[test]
    fn context_input_schema_validation_and_param_forwarding() {
        let docs_provider: Arc<dyn DomainProvider> = Arc::new(MockProvider {
            domain: "docs".to_owned(),
            caps: DomainCapabilities {
                supports_collection_context: true,
                ..DomainCapabilities::default()
            },
        });
        let custom_provider: Arc<dyn DomainProvider> = Arc::new(MockProvider {
            domain: "http".to_owned(),
            caps: DomainCapabilities {
                context: vec![ContextCapability {
                    name: "endpoints".to_owned(),
                    scope_mode: ContextScopeMode::Admitted,
                    default_paths_key: None,
                    input_schema: strict_url_schema(),
                }],
                ..DomainCapabilities::default()
            },
        });
        let composite = CompositeDomainProvider::with_module_flags(
            vec![docs_provider, custom_provider],
            BTreeMap::from([("docs".to_owned(), true), ("http".to_owned(), true)]),
        )
        .unwrap();

        // (a) Unknown `with:` keys fail closed on `specs.*` and `paths.*`.
        for lens in ["specs.memories", "paths.docs"] {
            let ctx_ref = ContextRef::parse(lens).unwrap();
            assert!(
                composite.validate_context_ref(&ctx_ref, &json!({})).is_ok(),
                "{lens} should accept empty params"
            );
            let err = composite
                .validate_context_ref(&ctx_ref, &json!({"unexpected": true}))
                .unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::WorkflowSchemaInvalid,
                "{lens} must reject unknown with: keys"
            );
        }

        // (b) Custom `ContextCapability.input_schema` rejects bad `with:` params and
        // forwards valid `with:` params into `DomainContextReq.params`.
        let custom_ref = ContextRef::parse("http.endpoints").unwrap();
        let bad_err = composite
            .validate_context_ref(&custom_ref, &json!({"wrong": 1}))
            .unwrap_err();
        assert_eq!(bad_err.code, ErrorCode::WorkflowSchemaInvalid);
        let valid_params = json!({"url": "https://example.test"});
        composite
            .validate_context_ref(&custom_ref, &valid_params)
            .expect("valid custom context params must pass");

        let mut ctx_req = DomainContextReq {
            run_id: RunId::new("r1").unwrap(),
            node_cursor: NodeCursor::root(vec![GateId::new("build").unwrap()]).unwrap(),
            gate_id: GateId::new("build").unwrap(),
            workspace_root: PathBuf::from("/repo"),
            worktree: PathBuf::from("/repo"),
            admitted_scope: vec!["src/**".to_owned()],
            target: None,
            max_bytes: 4096,
            path_policy: WorkspacePathPolicy::default(),
            baseline_ref: None,
            lens_globs: vec![],
            citation_text: None,
            params: valid_params.clone(),
        };
        let out = composite
            .provide_context("http.endpoints", &ctx_req)
            .unwrap();
        assert_eq!(out["params"], valid_params);
        ctx_req.params = json!({});
        let missing_url_err = composite
            .provide_context("http.endpoints", &ctx_req)
            .unwrap_err();
        assert_eq!(
            AcddError::from_anyhow(&missing_url_err).unwrap().code,
            ErrorCode::WorkflowSchemaInvalid
        );
    }
}
