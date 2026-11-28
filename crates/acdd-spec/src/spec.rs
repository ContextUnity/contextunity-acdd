//! Markdown/YAML `SpecDocument` parser, ephemeral `--goal` materializer, and deterministic archive renderer.

use crate::deltas::{validate_spec_deltas, DeltaSpecView, SpecDelta};
pub use crate::deltas::is_safe_path_segment_id;
use crate::frontmatter::split_frontmatter_required;
use crate::schema::{
    builtin_contract_schema, builtin_spec_schema, resolve_spec_schema, SpecItemSchemaDef,
    SpecSchemaDef,
};
use crate::types::{ExtendedScopeEntry, InvariantsState, SubtaskStatus};
use acdd_core::config::SpecConfig;
use acdd_core::errors::{AcddError, ErrorCode, DIAG_SPEC_ITEM_UNKNOWN};
use acdd_core::ids::{
    CommitOid, GateId, NodeCursor, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId,
};
use acdd_core::schema::suggest_similar_key;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const CORE_SPEC_FIELDS: &[&str] = &[
    "id", "title", "spec", "goal", "doc_type", "workflow", "status",
    "cancellation_reason", "depends_on", "linked_to", "closure", "spec_deltas",
];
pub(crate) const CORE_ITEM_FIELDS: &[&str] = &[
    "ref", "task_ref", "depends_on", "tags", "status", "receipt",
];

/// Lifecycle status of a specification document or task block.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SpecStatus {
    /// Drafted or planned, not yet started.
    #[default]
    Planned,
    /// Actively executing inside a `Run`.
    Active,
    /// Completed and archived.
    Completed,
    /// Explicitly cancelled with a mandatory `cancellation_reason`.
    Cancelled,
}

/// Repository binding entry inside `SpecDocument.repositories`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    /// Logical repository alias (`linked_workspaces` key or primary repo name).
    pub name: String,
    /// Required workspace path or linked workspace reference.
    pub path: String,
}

/// Aggregated invariant, decision, deferred finding, and review summary stored in receipts/closures.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRollup {
    /// Applicable and verified invariants captured from `gate_nodes.invariants_json`.
    #[serde(default)]
    pub verified_invariants: InvariantsState,
    /// Architectural or implementation decisions recorded during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    /// Findings explicitly deferred at closure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_findings: Vec<String>,
    /// Concise review acceptance summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_summary: Option<String>,
}

/// Durable task or root-gate delivery receipt stored in archive Markdown and `gate_artifacts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Owning `Run` identifier.
    pub run_id: RunId,
    /// Cursor of the accepting delivery/handoff gate (serialized as canonical `task:<t>:<g>` / `root:<g>`).
    #[serde(serialize_with = "serialize_node_cursor")]
    #[schemars(with = "String")]
    pub node_cursor: NodeCursor,
    /// Accepting gate identifier.
    pub gate_id: GateId,
    /// Worker identity that submitted the receipt.
    pub worker_id: WorkerId,
    /// Active claim revision at acceptance.
    pub claim_revision: u32,
    /// Active contract/scope revision at acceptance.
    pub contract_revision: u32,
    /// Candidate tree OID accepted by the gate.
    pub candidate_tree: TreeOid,
    /// Scoped commit OID created on delivery (`None` for `snapshot`/`manual`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_oid: Option<CommitOid>,
    /// UTC ISO-8601 acceptance timestamp.
    pub accepted_at: String,
    /// Proof summaries gathered from `need:` predecessor gates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub need_proofs: BTreeMap<String, Value>,
    /// Rolled-up invariants, decisions, deferred findings, and review summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollup: Option<ReceiptRollup>,
}

fn serialize_node_cursor<S>(cursor: &NodeCursor, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let encoded = cursor.to_node_id().map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&encoded)
}

/// Run-level closure metadata written into archived spec frontmatter (`closure:`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosureSummary {
    /// Terminal spec status (must be `completed`).
    pub status: SpecStatus,
    /// UTC ISO-8601 timestamp when the run archived/closed.
    pub closed_at: String,
    /// Target integration base branch/ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_base: Option<String>,
    /// Target integration base commit OID captured at run start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_base_commit: Option<CommitOid>,
    /// Final closing commit OID (`None` when `save.post != commit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_commit: Option<CommitOid>,
    /// Deterministic map of per-task delivery receipts keyed by `TaskRef`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_receipts: BTreeMap<TaskRef, Receipt>,
    /// Run-wide rollup across all tasks and root gates.
    #[serde(default)]
    pub rollup: ReceiptRollup,
    /// Aggregate run metrics summary.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics_summary: BTreeMap<String, Value>,
}

/// Subtask checklist item inside a `TaskSpec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subtask {
    /// Validated subtask identifier.
    pub subtask_ref: SubtaskRef,
    /// Concise subtask title.
    pub title: String,
    /// Current subtask status.
    #[serde(default = "default_subtask_status")]
    pub status: SubtaskStatus,
    /// Mandatory non-empty evidence when `status` is `completed` or `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

fn default_subtask_status() -> SubtaskStatus {
    SubtaskStatus::Pending
}

impl Subtask {
    /// Validate terminal-state invariants (`completed` / `cancelled` require non-empty `evidence`).
    pub fn validate(&self, task_ref: &TaskRef) -> Result<(), AcddError> {
        if matches!(
            self.status,
            SubtaskStatus::Completed | SubtaskStatus::Cancelled
        ) && self
            .evidence
            .as_deref()
            .is_none_or(|ev| ev.trim().is_empty())
        {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "task '{task_ref}' subtask '{}' has terminal status '{}' without non-empty evidence",
                    self.subtask_ref, self.status
                ),
            ));
        }
        Ok(())
    }
}

/// Parsed task block (`### task: <ref>` + fenced YAML).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Unique task reference within the spec document.
    pub task_ref: TaskRef,
    /// Concise objective/target of this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Optional detailed task specification prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Admitted write scope paths/globs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Approved scope extensions recorded during execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extended_scope: Vec<ExtendedScopeEntry>,
    /// Prerequisite task references (local `<task_ref>` or external).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Task tags for profile `when:` selection (defaults to `["default"]`).
    #[serde(default = "default_task_tags")]
    pub tags: Vec<String>,
    /// Applicable invariant descriptions or IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<String>,
    /// Task acceptance criteria.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// Monotonic contract revision (`>= 1`).
    #[serde(default = "default_contract_revision")]
    pub contract_revision: u32,
    /// Current task lifecycle status.
    #[serde(default)]
    pub status: SpecStatus,
    /// Ordered subtask items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Subtask>,
    /// Delivery receipt recorded upon completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}

/// Strictly deserialized fixed core YAML frontmatter of a `SpecDocument`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecCoreFrontmatter {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default)]
    pub status: SpecStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<ClosureSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_deltas: Vec<SpecDelta>,
}

/// Strictly deserialized fixed core YAML inside a `### <item>: <ref>` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCoreYaml {
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub item_ref: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default = "default_task_tags")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub status: SpecStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}

fn default_task_tags() -> Vec<String> {
    vec!["default".to_owned()]
}

fn default_contract_revision() -> u32 {
    1
}

/// Parsed Markdown + YAML specification document (milestone, plan, memory, custom schema, or `--goal` spec).
#[derive(Debug, Clone, PartialEq)]
pub struct SpecDocument {
    /// Specification document identifier (e.g. `AC-008`, `PLAN-001`, `GOAL-r1`).
    pub id: String,
    /// Human-readable specification title.
    pub title: String,
    /// Document classification (`contract` by default, or `spec` type when `doc_type` is omitted).
    pub doc_type: String,
    /// Optional workflow name/path override declared in frontmatter.
    pub workflow: Option<String>,
    /// Document lifecycle status.
    pub status: SpecStatus,
    /// Mandatory reason when `status == Cancelled`.
    pub cancellation_reason: Option<String>,
    /// Spec type selector declared in frontmatter `spec:` (defaults to owning `SpecConfig` or `"contract"`).
    pub spec: Option<String>,
    /// Ephemeral `--goal` narrative text stored in frontmatter `goal:`.
    pub goal: Option<String>,
    /// External/cross-spec dependencies.
    pub depends_on: Vec<String>,
    /// Owning paths or team identifiers (projected from `fields["owners"]`).
    pub owners: Vec<String>,
    /// Document-level invariants (projected from `fields["invariants"]`).
    pub invariants: Vec<String>,
    /// Document-level acceptance criteria (projected from `fields["acceptance_criteria"]`).
    pub acceptance_criteria: Vec<String>,
    /// Optional advisory guidance metadata (projected from `fields["guidance"]`).
    pub guidance: Option<serde_yaml::Value>,
    /// Recommended/required skills (projected from `fields["required_skills"]`).
    pub required_skills: Vec<String>,
    /// Multi-repo bindings (`linked_workspaces`, projected from `fields["repositories"]`).
    pub repositories: Vec<RepoEntry>,
    /// Related documents or plans.
    pub linked_to: Vec<String>,
    /// Archive closure summary populated on terminal completion.
    pub closure: Option<ClosureSummary>,
    /// Declarative `spec_deltas` entries targeting another Spec.
    pub spec_deltas: Vec<SpecDelta>,
    /// `SchemaDef`-validated extra frontmatter fields beyond fixed core fields.
    pub fields: BTreeMap<String, Value>,
    /// Ordered task specifications extracted from `### task: <ref>` sections.
    pub tasks: Vec<TaskSpec>,
    /// Named non-task item collections extracted from `### <item>: <ref>` sections, keyed by collection name.
    pub items: BTreeMap<String, Vec<SpecItem>>,
    /// Markdown body following the YAML frontmatter.
    pub body: String,
    /// Lowercase SHA-256 hex digest of the raw spec source bytes.
    pub digest: String,
}

/// Parsed named item block (`### <item>: <ref>` + fenced YAML) for non-task collections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecItem {
    /// Singular item token declared in the Markdown heading (`### <item>: <ref>`).
    pub item: String,
    /// Unique item reference within the specification document.
    pub item_ref: TaskRef,
    /// Optional prerequisite references declared inside the item block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Item tags (defaults to `["default"]` when omitted).
    #[serde(default = "default_task_tags")]
    pub tags: Vec<String>,
    /// Current item lifecycle status.
    #[serde(default)]
    pub status: SpecStatus,
    /// Optional completion receipt recorded on the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// `SchemaDef`-validated extra YAML fields beyond fixed core item keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
}

impl SpecDocument {
    /// Collect every item reference across `tasks` and named `items` collections.
    pub fn all_item_refs(&self) -> BTreeSet<&str> {
        self.tasks
            .iter()
            .map(|t| t.task_ref.as_str())
            .chain(self.items.values().flatten().map(|i| i.item_ref.as_str()))
            .collect()
    }

    /// Return `true` when `item_ref` matches any task in `tasks` or any item in `items`.
    pub fn has_item(&self, item_ref: &str) -> bool {
        self.item_status(item_ref).is_some()
    }

    /// Return the lifecycle status of a matching task or named item, if present.
    pub fn item_status(&self, item_ref: &str) -> Option<SpecStatus> {
        if let Some(task) = self.tasks.iter().find(|t| t.task_ref.as_str() == item_ref) {
            return Some(task.status);
        }
        self.items
            .values()
            .flatten()
            .find(|item| item.item_ref.as_str() == item_ref)
            .map(|item| item.status)
    }

    /// Return the `depends_on` entries for a matching task or named item, if present.
    pub fn item_depends_on(&self, item_ref: &str) -> Option<&[String]> {
        if let Some(task) = self.tasks.iter().find(|t| t.task_ref.as_str() == item_ref) {
            return Some(&task.depends_on);
        }
        self.items
            .values()
            .flatten()
            .find(|item| item.item_ref.as_str() == item_ref)
            .map(|item| item.depends_on.as_slice())
    }
}

/// Per-task runtime state applied when rendering a final archive Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveTaskUpdate {
    /// Final task lifecycle status.
    pub status: SpecStatus,
    /// Final extended scope entries accumulated via `extend_scope`.
    pub extended_scope: Vec<ExtendedScopeEntry>,
    /// Final subtasks with terminal evidence.
    pub subtasks: Vec<Subtask>,
    /// Accepted delivery receipt (if completed).
    pub receipt: Option<Receipt>,
}

/// Input payload for `render_archive_markdown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRenderInput {
    /// Closure summary written into frontmatter `closure:`.
    pub closure: ClosureSummary,
    /// Per-task updates keyed by `TaskRef`.
    pub tasks: BTreeMap<TaskRef, ArchiveTaskUpdate>,
}

/// Compute lowercase hex SHA-256 digest of raw specification bytes.
pub(crate) fn compute_spec_digest(raw: &str) -> String {
    acdd_core::format::sha256_hex(raw.as_bytes())
}

impl DeltaSpecView for SpecDocument {
    fn delta_spec_id(&self) -> &str {
        &self.id
    }

    fn delta_spec_digest(&self) -> &str {
        &self.digest
    }

    fn delta_spec_deltas(&self) -> &[SpecDelta] {
        &self.spec_deltas
    }

    fn delta_field(&self, field: &str) -> Option<&Value> {
        self.fields.get(field)
    }

    fn delta_named_entries(&self, field: &str) -> Option<Vec<(&str, Value)>> {
        if let Some(items) = self.items.get(field) {
            return Some(
                items
                    .iter()
                    .map(|item| {
                        let mut map: serde_json::Map<String, Value> =
                            item.fields.clone().into_iter().collect();
                        map.insert("status".to_owned(), serde_json::json!(item.status));
                        map.insert("tags".to_owned(), serde_json::json!(item.tags));
                        map.insert("depends_on".to_owned(), serde_json::json!(item.depends_on));
                        (item.item_ref.as_str(), Value::Object(map))
                    })
                    .collect(),
            );
        }
        if field == "tasks"
            && (!self.tasks.is_empty()
                || self.spec.as_deref().unwrap_or("contract") == "contract")
        {
            return Some(
                self.tasks
                    .iter()
                    .map(|task| {
                        (
                            task.task_ref.as_str(),
                            serde_json::json!({
                                "target": task.target,
                                "spec": task.spec,
                                "scope": task.scope,
                                "status": task.status,
                                "tags": task.tags,
                                "depends_on": task.depends_on,
                                "invariants": task.invariants,
                                "subtasks": task.subtasks,
                                "acceptance_criteria": task.acceptance_criteria,
                            }),
                        )
                    })
                    .collect(),
            );
        }
        None
    }
}

fn parse_yaml_mapping(raw_yaml: &str, context_label: &str) -> Result<serde_yaml::Mapping, AcddError> {
    let val: serde_yaml::Value = serde_yaml::from_str(raw_yaml).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("invalid YAML in {context_label}: {err}"),
        )
    })?;
    match val {
        serde_yaml::Value::Mapping(map) => Ok(map),
        _ => Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("{context_label} YAML must be a mapping"),
        )),
    }
}

fn split_yaml_mapping(
    raw_yaml: &str,
    core_keys: &[&str],
    context_label: &str,
) -> Result<(serde_yaml::Mapping, BTreeMap<String, Value>), AcddError> {
    let mapping = parse_yaml_mapping(raw_yaml, context_label)?;
    let mut core_map = serde_yaml::Mapping::new();
    let mut extra = BTreeMap::new();
    for (k, v) in mapping {
        let Some(key_str) = k.as_str() else {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("{context_label} YAML keys must be strings"),
            ));
        };
        if core_keys.contains(&key_str) {
            core_map.insert(k, v);
        } else {
            let json_val = serde_json::to_value(&v).map_err(|err| {
                AcddError::new(
                    ErrorCode::SpecSchemaInvalid,
                    format!("invalid value for '{key_str}' in {context_label}: {err}"),
                )
            })?;
            extra.insert(key_str.to_owned(), json_val);
        }
    }
    Ok((core_map, extra))
}

#[derive(Default, Deserialize)]
struct ContractProjectedFields {
    #[serde(default)]
    owners: Vec<String>,
    #[serde(default)]
    invariants: Vec<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    guidance: Option<serde_yaml::Value>,
    #[serde(default)]
    required_skills: Vec<String>,
    #[serde(default)]
    repositories: Vec<RepoEntry>,
}

#[derive(Default, Deserialize)]
struct TaskProjectedFields {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    extended_scope: Vec<ExtendedScopeEntry>,
    #[serde(default)]
    invariants: Vec<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default = "default_contract_revision")]
    contract_revision: u32,
    #[serde(default)]
    subtasks: Vec<Subtask>,
}

fn spec_err(msg: impl Into<String>) -> AcddError {
    AcddError::new(ErrorCode::SpecSchemaInvalid, msg)
}

fn deserialize_projected<T: serde::de::DeserializeOwned>(
    extra: &BTreeMap<String, Value>,
    context_label: &str,
) -> Result<T, AcddError> {
    let obj: serde_json::Map<String, Value> = extra.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    serde_json::from_value(Value::Object(obj))
        .map_err(|err| spec_err(format!("invalid fields in {context_label}: {err}")))
}

fn normalize_task_extra_for_schema(extra: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut normalized = extra.clone();
    if let Some(Value::Array(subtasks)) = normalized.get_mut("subtasks") {
        for sub in subtasks {
            if let Some(obj) = sub.as_object_mut().filter(|o| o.get("evidence").is_some_and(Value::is_null)) {
                obj.remove("evidence");
            }
        }
    }
    normalized
}

/// Parse a Markdown + YAML frontmatter `SpecDocument` strictly under built-in `contract`/`memory` bases
/// (defaulting omitted `spec:` to built-in `contract`).
pub fn parse_spec(raw: &str) -> Result<SpecDocument, AcddError> {
    parse_spec_inner(raw, None, None, &BTreeMap::new(), true)
}

/// Parse a Markdown + YAML frontmatter `SpecDocument` strictly, validating non-core frontmatter and item
/// fields against `schema_override` (or the built-in `contract`/`memory` schema when `None`).
pub fn parse_spec_with_schema(
    raw: &str,
    schema_override: Option<&SpecSchemaDef>,
) -> Result<SpecDocument, AcddError> {
    parse_spec_inner(raw, schema_override, None, &BTreeMap::new(), true)
}

/// Parse a Markdown + YAML frontmatter `SpecDocument` against the active workspace `specs:` registry
/// and optional owning collection `(collection_key, SpecConfig)` in strict 3-step validation order.
pub fn parse_spec_with_collections(
    raw: &str,
    owning_col: Option<(&str, &SpecConfig)>,
    collections: &BTreeMap<String, SpecConfig>,
) -> Result<SpecDocument, AcddError> {
    parse_spec_inner(raw, None, owning_col, collections, true)
}

/// Parse a spec for roadmap/archive discovery so malformed `depends_on` entries can surface as graph warnings
/// while retaining strict validation on all direct spec parsing entrypoints.
pub(crate) fn parse_spec_for_roadmap(
    raw: &str,
    schema_override: Option<&SpecSchemaDef>,
    owning_col: Option<(&str, &SpecConfig)>,
    collections: &BTreeMap<String, SpecConfig>,
) -> Result<SpecDocument, AcddError> {
    parse_spec_inner(raw, schema_override, owning_col, collections, false)
}

struct ParsedStep1Item {
    item_token: String,
    heading_ref: TaskRef,
    core: TaskCoreYaml,
    extra: BTreeMap<String, Value>,
}

fn parse_step1_items(
    body: &str,
    strict_dep_syntax: bool,
) -> Result<Vec<ParsedStep1Item>, AcddError> {
    let blocks = locate_item_blocks(body)?;
    let mut seen_refs = BTreeSet::new();
    let mut parsed_items = Vec::with_capacity(blocks.len());

    for block in blocks {
        if !seen_refs.insert(block.heading_ref.clone()) {
            return Err(spec_err(format!(
                "duplicate item block '### {}: {}' (item ref must be unique across the spec)",
                block.item_token, block.heading_ref
            )));
        }
        let block_label = format!("'### {}: {}'", block.item_token, block.heading_ref);
        let (item_core_map, item_extra) =
            split_yaml_mapping(block.yaml_slice, CORE_ITEM_FIELDS, &block_label)?;
        let item_core: TaskCoreYaml = serde_yaml::from_value(serde_yaml::Value::Mapping(item_core_map))
            .map_err(|err| spec_err(format!("invalid YAML in {block_label}: {err}")))?;

        if let (Some(r), Some(tr)) = (&item_core.item_ref, &item_core.task_ref) {
            if r != tr {
                return Err(spec_err(format!(
                    "{} '{}' has conflicting 'ref' and 'task_ref'",
                    block.item_token, block.heading_ref
                )));
            }
        }
        if let Some(yaml_ref) = item_core.task_ref.as_ref().or(item_core.item_ref.as_ref()) {
            if yaml_ref != &block.heading_ref {
                return Err(spec_err(format!(
                    "item block heading '### {}: {}' disagrees with YAML ref '{yaml_ref}'",
                    block.item_token, block.heading_ref
                )));
            }
        }
        if strict_dep_syntax {
            for dep in &item_core.depends_on {
                crate::deps::validate_dep_entry_syntax(dep)?;
            }
        }

        parsed_items.push(ParsedStep1Item {
            item_token: block.item_token,
            heading_ref: block.heading_ref,
            core: item_core,
            extra: item_extra,
        });
    }

    Ok(parsed_items)
}

fn parse_spec_inner(
    raw: &str,
    schema_override: Option<&SpecSchemaDef>,
    owning_col: Option<(&str, &SpecConfig)>,
    collections: &BTreeMap<String, SpecConfig>,
    strict_dep_syntax: bool,
) -> Result<SpecDocument, AcddError> {
    // Step 1: Core parsing (frontmatter YAML, item blocks, item ref uniqueness).
    let digest = compute_spec_digest(raw);
    let (frontmatter_str, body) = split_frontmatter_required(raw)?;
    let (core_map, extra) = split_yaml_mapping(frontmatter_str, CORE_SPEC_FIELDS, "spec frontmatter")?;
    let fm: SpecCoreFrontmatter = serde_yaml::from_value(serde_yaml::Value::Mapping(core_map))
        .map_err(|err| spec_err(format!("invalid spec frontmatter YAML: {err}")))?;

    if fm.id.trim().is_empty() {
        return Err(spec_err("spec frontmatter 'id' cannot be empty"));
    }
    if !is_safe_path_segment_id(&fm.id) {
        return Err(spec_err(format!(
            "spec frontmatter 'id' must be a single safe identifier without path separators or traversal, got '{}'",
            fm.id
        )));
    }
    if fm.title.trim().is_empty() {
        return Err(spec_err("spec frontmatter 'title' cannot be empty"));
    }
    if fm.status == SpecStatus::Cancelled
        && fm.cancellation_reason.as_deref().is_none_or(|r| r.trim().is_empty())
    {
        return Err(spec_err("cancelled spec requires non-empty 'cancellation_reason'"));
    }
    if fm.closure.as_ref().is_some_and(|c| c.status != SpecStatus::Completed) {
        return Err(spec_err("spec closure.status must be 'completed'"));
    }
    validate_spec_deltas(&fm.spec_deltas)?;
    if strict_dep_syntax {
        for dep in &fm.depends_on {
            crate::deps::validate_dep_entry_syntax(dep)?;
        }
    }

    let parsed_items = parse_step1_items(body, strict_dep_syntax)?;

    // Step 2: Active SchemaDef validation (frontmatter fields, declared item types, item fields).
    let resolved_schema = match schema_override {
        Some(schema) => schema.clone(),
        None if owning_col.is_none() && collections.is_empty() => match fm.spec.as_deref() {
            Some(sel) => builtin_spec_schema(sel)
                .unwrap_or_else(builtin_contract_schema)
                .clone(),
            None => builtin_contract_schema().clone(),
        },
        None => resolve_spec_schema(fm.spec.as_deref(), owning_col, collections)?,
    };
    resolved_schema
        .fields
        .validate_spec_fields(&extra, CORE_SPEC_FIELDS, "spec")?;

    let proj: ContractProjectedFields = deserialize_projected(&extra, "spec frontmatter")?;
    if proj
        .repositories
        .iter()
        .any(|r| r.name.trim().is_empty() || r.path.trim().is_empty())
    {
        return Err(spec_err(
            "spec repositories entries require non-empty 'name' and 'path'",
        ));
    }

    let mut token_to_col: BTreeMap<&str, (&str, &SpecItemSchemaDef)> = BTreeMap::new();
    for (col_key, item_def) in &resolved_schema.items {
        let token = item_def.item.as_deref().unwrap_or(col_key.as_str());
        if token_to_col.insert(token, (col_key.as_str(), item_def)).is_some() {
            return Err(spec_err(format!(
                "spec schema '{}' declares duplicate item token '{token}'",
                resolved_schema.name
            )));
        }
    }

    let mut tasks = Vec::new();
    let mut items: BTreeMap<String, Vec<SpecItem>> = resolved_schema
        .items
        .keys()
        .filter(|col_key| col_key.as_str() != "tasks")
        .map(|col_key| (col_key.clone(), Vec::new()))
        .collect();

    for parsed in parsed_items {
        let Some(&(col_key, item_schema)) = token_to_col.get(parsed.item_token.as_str()) else {
            let mut err = AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!(
                    "spec type '{}' does not declare item type '{}'",
                    fm.spec.as_deref().unwrap_or(&resolved_schema.name),
                    parsed.item_token
                ),
            )
            .with_diag_code(DIAG_SPEC_ITEM_UNKNOWN);
            if let Some(suggestion) =
                suggest_similar_key(&parsed.item_token, token_to_col.keys().copied())
            {
                err = err.with_hint(format!("did you mean '{suggestion}'?"));
            }
            return Err(err);
        };

        let normalized = normalize_task_extra_for_schema(&parsed.extra);
        item_schema.fields.validate_spec_fields(
            &normalized,
            CORE_ITEM_FIELDS,
            &format!("{} '{}'", parsed.item_token, parsed.heading_ref),
        )?;

        let tags = if parsed.core.tags.is_empty() {
            default_task_tags()
        } else {
            parsed.core.tags
        };

        if col_key == "tasks" {
            let block_label = format!("'### {}: {}'", parsed.item_token, parsed.heading_ref);
            let tp: TaskProjectedFields = deserialize_projected(&parsed.extra, &block_label)?;
            if tp.contract_revision < 1 {
                return Err(spec_err(format!(
                    "task '{}' contract_revision must be >= 1",
                    parsed.heading_ref
                )));
            }
            let mut seen_subtasks = BTreeSet::new();
            for sub in &tp.subtasks {
                if !seen_subtasks.insert(sub.subtask_ref.clone()) {
                    return Err(spec_err(format!(
                        "duplicate subtask_ref '{}' in task '{}'",
                        sub.subtask_ref, parsed.heading_ref
                    )));
                }
                sub.validate(&parsed.heading_ref)?;
            }
            tasks.push(TaskSpec {
                task_ref: parsed.heading_ref,
                target: tp.target,
                spec: tp.spec,
                scope: tp.scope,
                extended_scope: tp.extended_scope,
                depends_on: parsed.core.depends_on,
                tags,
                invariants: tp.invariants,
                acceptance_criteria: tp.acceptance_criteria,
                contract_revision: tp.contract_revision,
                status: parsed.core.status,
                subtasks: tp.subtasks,
                receipt: parsed.core.receipt,
            });
        } else {
            items.entry(col_key.to_owned()).or_default().push(SpecItem {
                item: parsed.item_token,
                item_ref: parsed.heading_ref,
                depends_on: parsed.core.depends_on,
                tags,
                status: parsed.core.status,
                receipt: parsed.core.receipt,
                fields: parsed.extra,
            });
        }
    }

    let doc_type = fm
        .doc_type
        .clone()
        .unwrap_or_else(|| fm.spec.clone().unwrap_or_else(|| "contract".to_owned()));

    Ok(SpecDocument {
        id: fm.id,
        title: fm.title,
        doc_type,
        workflow: fm.workflow,
        status: fm.status,
        cancellation_reason: fm.cancellation_reason,
        spec: fm.spec,
        goal: fm.goal,
        depends_on: fm.depends_on,
        owners: proj.owners,
        invariants: proj.invariants,
        acceptance_criteria: proj.acceptance_criteria,
        guidance: proj.guidance,
        required_skills: proj.required_skills,
        repositories: proj.repositories,
        linked_to: fm.linked_to,
        closure: fm.closure,
        spec_deltas: fm.spec_deltas,
        fields: extra,
        tasks,
        items,
        body: body.to_owned(),
        digest,
    })
}

/// Materialize an ephemeral `--goal` specification document with optional `### task: goal` emission.
///
/// Writes narrative goal text to frontmatter `goal:` and sets `spec: contract` explicitly.
pub fn materialize_ephemeral_goal_spec_with_options(
    run_id: &RunId,
    run_name: &RunName,
    goal: &str,
    workflow_id: &str,
    scope: &[String],
    tags: &[String],
    include_goal_task: bool,
) -> Result<String, AcddError> {
    let trimmed_goal = goal.trim();
    if trimmed_goal.is_empty() {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            "ephemeral --goal text cannot be empty",
        ));
    }
    let title = format!("{run_name}: {trimmed_goal}");
    let fm = SpecCoreFrontmatter {
        id: format!("GOAL-{run_id}"),
        title: title.clone(),
        spec: Some("contract".to_owned()),
        goal: Some(trimmed_goal.to_owned()),
        doc_type: None,
        workflow: Some(workflow_id.to_owned()),
        status: SpecStatus::Active,
        cancellation_reason: None,
        depends_on: Vec::new(),
        linked_to: Vec::new(),
        closure: None,
        spec_deltas: Vec::new(),
    };
    let mut fm_val = serde_yaml::to_value(&fm).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("failed to serialize ephemeral spec frontmatter: {err}"),
        )
    })?;
    if !include_goal_task && !scope.is_empty() {
        if let serde_yaml::Value::Mapping(ref mut map) = fm_val {
            map.insert(
                serde_yaml::Value::String("owners".to_owned()),
                serde_yaml::to_value(scope).map_err(|err| {
                    AcddError::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!("failed to serialize ephemeral spec owners: {err}"),
                    )
                })?,
            );
        }
    }
    let fm_yaml = serde_yaml::to_string(&fm_val).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("failed to serialize ephemeral spec frontmatter: {err}"),
        )
    })?;

    let rendered = if include_goal_task {
        let effective_tags = if tags.is_empty() {
            default_task_tags()
        } else {
            tags.to_vec()
        };
        let task_ref = TaskRef::new("goal")?;
        let task_spec = TaskSpec {
            task_ref: task_ref.clone(),
            target: Some(trimmed_goal.to_owned()),
            spec: Some(trimmed_goal.to_owned()),
            scope: scope.to_vec(),
            extended_scope: Vec::new(),
            depends_on: Vec::new(),
            tags: effective_tags,
            invariants: Vec::new(),
            acceptance_criteria: Vec::new(),
            contract_revision: 1,
            status: SpecStatus::Planned,
            subtasks: Vec::new(),
            receipt: None,
        };
        let task_yaml_str = serialize_task_spec_yaml(&task_spec, false)?;

        format!(
            "---\n{}---\n\n# {}\n\n{}\n\n### task: {}\n\n```yaml\n{}```\n",
            fm_yaml, title, trimmed_goal, task_ref, task_yaml_str
        )
    } else {
        format!("---\n{}---\n\n# {}\n\n{}\n", fm_yaml, title, trimmed_goal)
    };
    let _ = parse_spec(&rendered)?;
    Ok(rendered)
}

fn serialize_task_spec_yaml(task: &TaskSpec, use_ref_key: bool) -> Result<String, AcddError> {
    let mut val = serde_yaml::to_value(task).map_err(yaml_ser_err)?;
    if use_ref_key {
        if let serde_yaml::Value::Mapping(ref mut map) = val {
            if let Some(v) = map.remove(serde_yaml::Value::String("task_ref".to_owned())) {
                map.insert(serde_yaml::Value::String("ref".to_owned()), v);
            }
        }
    }
    serde_yaml::to_string(&val).map_err(yaml_ser_err)
}

fn yaml_ser_err(err: serde_yaml::Error) -> AcddError {
    AcddError::new(
        ErrorCode::SpecSchemaInvalid,
        format!("failed to serialize spec YAML: {err}"),
    )
}

/// Deterministically render the final archive Markdown document against the active workspace `specs:`
/// registry and optional owning collection, updating `status`, `closure:`, and task entries.
pub fn render_archive_markdown_with_collections(
    original_raw: &str,
    input: &ArchiveRenderInput,
    owning_col: Option<(&str, &SpecConfig)>,
    collections: &BTreeMap<String, SpecConfig>,
) -> Result<(String, String), AcddError> {
    if input.closure.status != SpecStatus::Completed {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            "archive closure.status must be 'completed'",
        ));
    }
    let parsed = parse_spec_with_collections(original_raw, owning_col, collections)?;
    let (fm_str, body) = split_frontmatter_required(original_raw)?;
    let mut fm_map = parse_yaml_mapping(fm_str, "spec frontmatter")?;
    fm_map.insert(
        serde_yaml::Value::String("status".to_owned()),
        serde_yaml::to_value(SpecStatus::Completed).map_err(yaml_ser_err)?,
    );
    fm_map.insert(
        serde_yaml::Value::String("closure".to_owned()),
        serde_yaml::to_value(&input.closure).map_err(yaml_ser_err)?,
    );
    let new_fm_yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(fm_map)).map_err(|err| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("failed to serialize archive frontmatter: {err}"),
        )
    })?;

    let task_token = crate::schema::resolve_spec_schema(
        parsed.spec.as_deref(),
        owning_col,
        collections,
    )
    .ok()
    .and_then(|s| s.items.get("tasks").and_then(|t| t.item.clone()))
    .unwrap_or_else(|| "task".to_owned());
    let blocks: Vec<LocatedItemBlock<'_>> = locate_item_blocks(body)?
        .into_iter()
        .filter(|b| b.item_token == task_token)
        .collect();
    let mut new_body = String::with_capacity(body.len() + 1024);
    let mut cursor = 0usize;

    for (task_idx, block) in blocks.iter().enumerate() {
        let existing = &parsed.tasks[task_idx];
        let mut updated = existing.clone();
        if let Some(patch) = input.tasks.get(&existing.task_ref) {
            for sub in &patch.subtasks {
                sub.validate(&existing.task_ref)?;
            }
            updated.status = patch.status;
            updated.extended_scope = patch.extended_scope.clone();
            updated.subtasks = patch.subtasks.clone();
            updated.receipt = patch.receipt.clone();
        }
        let orig_map = parse_yaml_mapping(block.yaml_slice, &format!("'### task: {}'", existing.task_ref))?;
        let use_ref_key = orig_map.contains_key(serde_yaml::Value::String("ref".to_owned()))
            && !orig_map.contains_key(serde_yaml::Value::String("task_ref".to_owned()));
        let serialized_task = serialize_task_spec_yaml(&updated, use_ref_key)?;

        new_body.push_str(&body[cursor..block.yaml_byte_start]);
        new_body.push_str(&serialized_task);
        if !serialized_task.ends_with('\n') {
            new_body.push('\n');
        }
        cursor = block.yaml_byte_end;
    }
    new_body.push_str(&body[cursor..]);

    let body_trimmed = new_body.trim_start_matches('\n');
    let rendered = format!("---\n{new_fm_yaml}---\n\n{body_trimmed}");
    let _ = parse_spec_with_collections(&rendered, owning_col, collections)?;
    let digest = compute_spec_digest(&rendered);
    Ok((rendered, digest))
}

/// Extract numeric suffix from a specification ID matching `<PREFIX>-<NNN>...` (case-sensitive prefix).
pub fn extract_spec_number(spec_id: &str, prefix: &str) -> Option<u32> {
    let rest = spec_id.strip_prefix(prefix)?.strip_prefix('-')?;
    let digits_end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return None;
    }
    rest[..digits_end].parse::<u32>().ok()
}

/// Format the next unique `<PREFIX>-<NNN>` identifier given the highest observed number.
pub fn format_next_spec_id(prefix: &str, digits: usize, max_existing: Option<u32>) -> String {
    let next = max_existing.unwrap_or(0).saturating_add(1);
    format!("{prefix}-{next:0digits$}")
}

struct LocatedItemBlock<'a> {
    item_token: String,
    heading_ref: TaskRef,
    yaml_slice: &'a str,
    yaml_byte_start: usize,
    yaml_byte_end: usize,
}

fn is_valid_item_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn section_contains_yaml_fence(lines: &[(usize, &str)], mut idx: usize) -> bool {
    while idx < lines.len() {
        let (_, sub_line) = lines[idx];
        let trimmed = sub_line.trim_end_matches(['\r', '\n']).trim();
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") || trimmed.starts_with("#### ") {
            return false;
        }
        if trimmed == "```yaml" || trimmed == "```yml" {
            return true;
        }
        if trimmed.starts_with("```") {
            idx += 1;
            while idx < lines.len() {
                if lines[idx].1.trim().starts_with("```") {
                    break;
                }
                idx += 1;
            }
        }
        idx += 1;
    }
    false
}

fn parse_item_heading_candidate<'a>(
    trimmed: &'a str,
    lines: &'a [(usize, &'a str)],
    next_idx: usize,
) -> Option<(&'static str, &'a str, &'a str)> {
    let rest = trimmed.strip_prefix("### ")?;
    let (raw_token, raw_ref) = rest.split_once(':')?;
    let token = raw_token.trim();
    if !is_valid_item_token(token) {
        return None;
    }
    let ref_str = raw_ref.trim();
    if token == "task" {
        return Some(("task", token, ref_str));
    }
    if ref_str.is_empty() || ref_str.starts_with("//") || ref_str.chars().any(char::is_whitespace) {
        return None;
    }
    if section_contains_yaml_fence(lines, next_idx) {
        return Some(("item", token, ref_str));
    }
    None
}

fn scan_item_yaml_block(
    lines: &[(usize, &str)],
    idx: &mut usize,
    item_token: &str,
    item_ref: &TaskRef,
) -> Result<Option<(usize, usize)>, AcddError> {
    let mut found_yaml: Option<(usize, usize)> = None;
    let mut fence_open_byte: Option<usize> = None;

    while *idx < lines.len() {
        let (sub_start, sub_line) = lines[*idx];
        let sub_trimmed = sub_line.trim_end_matches(['\r', '\n']).trim();
        if fence_open_byte.is_none()
            && (sub_trimmed.starts_with("## ")
                || sub_trimmed.starts_with("### ")
                || sub_trimmed.starts_with("#### "))
        {
            break;
        }
        if let Some(yaml_start) = fence_open_byte {
            if sub_trimmed == "```" {
                found_yaml = Some((yaml_start, sub_start));
                *idx += 1;
                break;
            }
        } else if sub_trimmed == "```yaml" || sub_trimmed == "```yml" {
            fence_open_byte = Some(sub_start + sub_line.len());
        } else if sub_trimmed.starts_with("```") {
            *idx += 1;
            while *idx < lines.len() {
                let (_, inner) = lines[*idx];
                *idx += 1;
                if inner.trim().starts_with("```") {
                    break;
                }
            }
            continue;
        }
        *idx += 1;
    }

    if fence_open_byte.is_some() && found_yaml.is_none() {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("unclosed ```yaml block under '### {item_token}: {item_ref}'"),
        ));
    }
    if found_yaml.is_none() && item_token == "task" {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("task '### task: {item_ref}' is missing fenced ```yaml block"),
        ));
    }
    Ok(found_yaml)
}

fn locate_item_blocks(body: &str) -> Result<Vec<LocatedItemBlock<'_>>, AcddError> {
    let mut blocks = Vec::new();
    let mut in_outer_fence = false;

    let lines: Vec<(usize, &str)> = body
        .split_inclusive('\n')
        .scan(0usize, |acc, line| {
            let start = *acc;
            *acc += line.len();
            Some((start, line))
        })
        .collect();

    let mut idx = 0usize;
    while idx < lines.len() {
        let (_, line) = lines[idx];
        let trimmed = line.trim_end_matches(['\r', '\n']).trim();
        if trimmed.starts_with("```") {
            in_outer_fence = !in_outer_fence;
            idx += 1;
            continue;
        }
        if in_outer_fence {
            idx += 1;
            continue;
        }
        let Some((kind_label, token, ref_str)) = parse_item_heading_candidate(trimmed, &lines, idx + 1)
        else {
            idx += 1;
            continue;
        };
        let item_ref = TaskRef::new(ref_str).map_err(|err| {
            AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("invalid {kind_label} heading '### {token}: {ref_str}': {err}"),
            )
        })?;
        idx += 1;
        let yaml_range = scan_item_yaml_block(&lines, &mut idx, token, &item_ref)?;
        let (yaml_slice, yaml_byte_start, yaml_byte_end) = match yaml_range {
            Some((s, e)) => (&body[s..e], s, e),
            None => ("{}", 0, 0),
        };
        blocks.push(LocatedItemBlock {
            item_token: token.to_owned(),
            heading_ref: item_ref,
            yaml_slice,
            yaml_byte_start,
            yaml_byte_end,
        });
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid_40(ch: char) -> String {
        std::iter::repeat_n(ch, 40).collect()
    }

    #[test]
    fn spec_parser_validation_matrix() {
        let valid_spec = r#"---
id: AC-008
title: "Unified engine"
doc_type: contract
workflow: code
status: active
owners: [crates/acdd-state/]
invariants: [INV-1]
acceptance_criteria: [AC-1]
repositories:
  - name: main
    path: .
---

# Unified engine

## Tasks

### task: core-types

```yaml
target: "Define core types"
scope:
  - crates/acdd-state/src/types.rs
depends_on: []
invariants: [INV-1]
subtasks:
  - subtask_ref: s1
    title: "Add enums"
    status: completed
    evidence: "cargo test -p acdd-state passed"
```

### task: profile-parser

```yaml
task_ref: profile-parser
target: "Parse profiles"
depends_on: [core-types]
tags: [code]
contract_revision: 2
status: planned
```
"#;

        let doc = parse_spec(valid_spec).unwrap();
        assert_eq!(doc.id, "AC-008");
        assert_eq!(doc.status, SpecStatus::Active);
        assert_eq!(doc.tasks.len(), 2);
        assert_eq!(doc.tasks[0].tags, vec!["default"]);
        assert_eq!(doc.tasks[0].contract_revision, 1);
        assert_eq!(doc.tasks[1].tags, vec!["code"]);
        assert_eq!(doc.tasks[1].contract_revision, 2);
        assert_eq!(doc.digest, compute_spec_digest(valid_spec));

        let bad_cases: &[(&str, &str)] = &[
            ("missing_frontmatter", "# No frontmatter\n"),
            (
                "spec_id_parent_traversal",
                "---\nid: \"../escape\"\ntitle: t\n---\n",
            ),
            (
                "spec_id_multi_segment",
                "---\nid: \"sub/AC-1\"\ntitle: t\n---\n",
            ),
            (
                "spec_id_backslash",
                "---\nid: \"sub\\\\AC-1\"\ntitle: t\n---\n",
            ),
            (
                "cancelled_without_reason",
                "---\nid: AC-1\ntitle: t\nstatus: cancelled\n---\n",
            ),
            (
                "repo_empty_path",
                "---\nid: AC-1\ntitle: t\nrepositories:\n  - name: r\n    path: \"  \"\n---\n",
            ),
            (
                "per_task_acdd_profile_rejected",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\nacdd_profile: custom\n```\n",
            ),
            (
                "legacy_task_proof_policy_rejected",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\nproof_policy: seam-test-first\n```\n",
            ),
            (
                "mismatched_task_ref",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\ntask_ref: t2\n```\n",
            ),
            (
                "duplicate_task_ref",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\ntarget: a\n```\n\n### task: t1\n\n```yaml\ntarget: b\n```\n",
            ),
            (
                "subtask_completed_without_evidence",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\nsubtasks:\n  - subtask_ref: s1\n    title: do\n    status: completed\n```\n",
            ),
            (
                "subtask_cancelled_blank_evidence",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\nsubtasks:\n  - subtask_ref: s1\n    title: do\n    status: cancelled\n    evidence: \"   \"\n```\n",
            ),
            (
                "zero_contract_revision",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\n```yaml\ncontract_revision: 0\n```\n",
            ),
            (
                "missing_yaml_fence",
                "---\nid: AC-1\ntitle: t\n---\n\n### task: t1\n\nJust prose.\n",
            ),
        ];

        for &(label, raw) in bad_cases {
            let err = parse_spec(raw).expect_err("expected parse failure");
            assert_eq!(err.code, ErrorCode::SpecSchemaInvalid, "{label}: {err}");
        }
    }

    #[test]
    fn ephemeral_goal_and_archive_renderer_roundtrip() {
        let run_id = RunId::new("run-001").unwrap();
        let run_name = RunName::new("fix-auth").unwrap();
        let ephemeral = materialize_ephemeral_goal_spec_with_options(
            &run_id,
            &run_name,
            "Fix token expiry bug",
            "code",
            &["src/auth.rs".to_owned()],
            &[],
            true,
        )
        .unwrap();

        let parsed = parse_spec(&ephemeral).unwrap();
        assert_eq!(parsed.id, "GOAL-run-001");
        assert_eq!(parsed.spec.as_deref(), Some("contract"));
        assert_eq!(parsed.goal.as_deref(), Some("Fix token expiry bug"));
        assert_eq!(parsed.workflow.as_deref(), Some("code"));
        assert_eq!(parsed.status, SpecStatus::Active);
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].task_ref.as_str(), "goal");
        assert_eq!(parsed.tasks[0].scope, vec!["src/auth.rs"]);

        let task_ref = TaskRef::new("goal").unwrap();
        let receipt = Receipt {
            run_id: run_id.clone(),
            node_cursor: NodeCursor::task(task_ref.clone(), vec![GateId::new("deliver").unwrap()]),
            gate_id: GateId::new("deliver").unwrap(),
            worker_id: WorkerId::new("reviewer-1").unwrap(),
            claim_revision: 1,
            contract_revision: 2,
            candidate_tree: TreeOid::new(oid_40('a')).unwrap(),
            committed_oid: Some(CommitOid::new(oid_40('b')).unwrap()),
            accepted_at: "2026-11-17T10:00:00.000Z".to_owned(),
            need_proofs: BTreeMap::from([(
                "review".to_owned(),
                serde_json::json!({"decision": "pass"}),
            )]),
            rollup: Some(ReceiptRollup {
                verified_invariants: InvariantsState {
                    applicable: vec!["INV-1".to_owned()],
                    verified: vec!["INV-1".to_owned()],
                },
                decisions: vec!["Use constant-time compare".to_owned()],
                deferred_findings: vec![],
                review_summary: Some("All contours verified".to_owned()),
            }),
        };

        let mut task_receipts = BTreeMap::new();
        task_receipts.insert(task_ref.clone(), receipt.clone());

        let archive_input = ArchiveRenderInput {
            closure: ClosureSummary {
                status: SpecStatus::Completed,
                closed_at: "2026-11-17T10:30:00.000Z".to_owned(),
                target_base: Some("main".to_owned()),
                target_base_commit: Some(CommitOid::new(oid_40('c')).unwrap()),
                closed_commit: Some(CommitOid::new(oid_40('d')).unwrap()),
                task_receipts,
                rollup: ReceiptRollup {
                    verified_invariants: InvariantsState {
                        applicable: vec!["INV-1".to_owned()],
                        verified: vec!["INV-1".to_owned()],
                    },
                    decisions: vec!["Use constant-time compare".to_owned()],
                    deferred_findings: vec!["Follow-up docs nit".to_owned()],
                    review_summary: Some("Integration verified".to_owned()),
                },
                metrics_summary: BTreeMap::from([(
                    "tasks_completed".to_owned(),
                    serde_json::json!(1),
                )]),
            },
            tasks: BTreeMap::from([(
                task_ref.clone(),
                ArchiveTaskUpdate {
                    status: SpecStatus::Completed,
                    extended_scope: vec![ExtendedScopeEntry {
                        path: "tests/auth.rs".to_owned(),
                        reason: "Add regression test".to_owned(),
                    }],
                    subtasks: vec![Subtask {
                        subtask_ref: SubtaskRef::new("fix").unwrap(),
                        title: "Patch expiry check".to_owned(),
                        status: SubtaskStatus::Completed,
                        evidence: Some("cargo test auth passed".to_owned()),
                    }],
                    receipt: Some(receipt),
                },
            )]),
        };

        let (rendered_1, digest_1) = render_archive_markdown_with_collections(
            &ephemeral,
            &archive_input,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        let (rendered_2, digest_2) = render_archive_markdown_with_collections(
            &ephemeral,
            &archive_input,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(rendered_1, rendered_2);
        assert_eq!(digest_1, digest_2);
        assert_eq!(digest_1, compute_spec_digest(&rendered_1));

        let archived = parse_spec(&rendered_1).unwrap();
        assert_eq!(archived.status, SpecStatus::Completed);
        assert_eq!(
            archived.closure.as_ref().unwrap().target_base.as_deref(),
            Some("main")
        );
        assert_eq!(archived.tasks[0].status, SpecStatus::Completed);
        assert_eq!(archived.tasks[0].extended_scope.len(), 1);
        assert_eq!(archived.tasks[0].subtasks.len(), 1);
        assert!(archived.tasks[0].receipt.is_some());

        let id_cases: &[(&str, &str, Option<u32>)] = &[
            ("AC-007-foo", "AC", Some(7)),
            ("PLAN-012-bar", "PLAN", Some(12)),
            ("AC-bad", "AC", None),
        ];
        for &(raw_id, prefix, expected) in id_cases {
            assert_eq!(extract_spec_number(raw_id, prefix), expected);
        }
        assert_eq!(format_next_spec_id("AC", 3, Some(7)), "AC-008");
        assert_eq!(format_next_spec_id("PLAN", 3, None), "PLAN-001");
    }
}
