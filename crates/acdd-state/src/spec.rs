//! Markdown/YAML `SpecDocument` parser, ephemeral `--goal` materializer, and deterministic archive renderer.

use crate::types::{ExtendedScopeEntry, InvariantsState, SubtaskStatus};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Lifecycle status of a specification document or task block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Owning `Run` identifier.
    pub run_id: RunId,
    /// Cursor of the accepting delivery/handoff gate (serialized as canonical `task:<t>:<g>` / `root:<g>`).
    #[serde(serialize_with = "serialize_node_cursor")]
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
    pub fn validate(&self, task_ref: &TaskRef) -> Result<(), StateError> {
        if matches!(
            self.status,
            SubtaskStatus::Completed | SubtaskStatus::Cancelled
        ) && self
            .evidence
            .as_deref()
            .is_none_or(|ev| ev.trim().is_empty())
        {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
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
    pub target: Option<String>,
    /// Optional detailed task specification prose.
    pub spec: Option<String>,
    /// Admitted write scope paths/globs.
    pub scope: Vec<String>,
    /// Approved scope extensions recorded during execution.
    pub extended_scope: Vec<ExtendedScopeEntry>,
    /// Prerequisite task references (local `<task_ref>` or external).
    pub depends_on: Vec<String>,
    /// Task tags for profile `when:` selection (defaults to `["default"]`).
    pub tags: Vec<String>,
    /// Applicable invariant descriptions or IDs.
    pub invariants: Vec<String>,
    /// Task acceptance criteria.
    pub acceptance_criteria: Vec<String>,
    /// Monotonic contract revision (`>= 1`).
    pub contract_revision: u32,
    /// Current task lifecycle status.
    pub status: SpecStatus,
    /// Ordered subtask items.
    pub subtasks: Vec<Subtask>,
    /// Delivery receipt recorded upon completion.
    pub receipt: Option<Receipt>,
}

/// Strictly deserialized YAML frontmatter of a `SpecDocument`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecFrontmatter {
    pub id: String,
    pub title: String,
    #[serde(default = "default_doc_type")]
    pub doc_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub status: SpecStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<RepoEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<ClosureSummary>,
}

fn default_doc_type() -> String {
    "contract".to_owned()
}

/// Strictly deserialized YAML inside a `### task: <ref>` block (rejects `acdd_profile` and legacy fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskSpecYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extended_scope: Vec<ExtendedScopeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default = "default_task_tags")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default = "default_contract_revision")]
    pub contract_revision: u32,
    #[serde(default)]
    pub status: SpecStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Subtask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}

fn default_task_tags() -> Vec<String> {
    vec!["default".to_owned()]
}

fn default_contract_revision() -> u32 {
    1
}

/// Parsed Markdown + YAML specification document (milestone, plan, or ephemeral `--goal` spec).
#[derive(Debug, Clone, PartialEq)]
pub struct SpecDocument {
    /// Specification document identifier (e.g. `AC-008`, `PLAN-001`, `GOAL-r1`).
    pub id: String,
    /// Human-readable specification title.
    pub title: String,
    /// Document classification (`contract` by default).
    pub doc_type: String,
    /// Optional profile name/path override declared in frontmatter.
    pub profile: Option<String>,
    /// Document lifecycle status.
    pub status: SpecStatus,
    /// Mandatory reason when `status == Cancelled`.
    pub cancellation_reason: Option<String>,
    /// Root specification narrative text (from frontmatter `spec:` or body).
    pub spec: Option<String>,
    /// External/cross-spec dependencies.
    pub depends_on: Vec<String>,
    /// Owning paths or team identifiers.
    pub owners: Vec<String>,
    /// Document-level invariants.
    pub invariants: Vec<String>,
    /// Document-level acceptance criteria.
    pub acceptance_criteria: Vec<String>,
    /// Optional advisory guidance metadata.
    pub guidance: Option<serde_yaml::Value>,
    /// Recommended/required skills.
    pub required_skills: Vec<String>,
    /// Multi-repo bindings (`linked_workspaces`).
    pub repositories: Vec<RepoEntry>,
    /// Related documents or plans.
    pub linked_to: Vec<String>,
    /// Archive closure summary populated on terminal completion.
    pub closure: Option<ClosureSummary>,
    /// Ordered task specifications extracted from `### task: <ref>` sections.
    pub tasks: Vec<TaskSpec>,
    /// Markdown body following the YAML frontmatter.
    pub body: String,
    /// Lowercase SHA-256 hex digest of the raw spec source bytes.
    pub digest: String,
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
pub fn compute_spec_digest(raw: &str) -> String {
    acdd_core::format::sha256_hex(raw.as_bytes())
}

/// Parse a Markdown + YAML frontmatter `SpecDocument` strictly.
pub fn parse_spec(raw: &str) -> Result<SpecDocument, StateError> {
    let digest = compute_spec_digest(raw);
    let (frontmatter_str, body) = split_frontmatter(raw)?;
    let fm: SpecFrontmatter = serde_yaml::from_str(frontmatter_str).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid spec frontmatter YAML: {err}"),
        )
    })?;

    if fm.id.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "spec frontmatter 'id' cannot be empty",
        ));
    }
    if fm.title.trim().is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "spec frontmatter 'title' cannot be empty",
        ));
    }
    if fm.status == SpecStatus::Cancelled
        && fm
            .cancellation_reason
            .as_deref()
            .is_none_or(|r| r.trim().is_empty())
    {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "cancelled spec requires non-empty 'cancellation_reason'",
        ));
    }
    for repo in &fm.repositories {
        if repo.name.trim().is_empty() || repo.path.trim().is_empty() {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "spec repositories entries require non-empty 'name' and 'path'",
            ));
        }
    }
    if let Some(closure) = &fm.closure {
        if closure.status != SpecStatus::Completed {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                "spec closure.status must be 'completed'",
            ));
        }
    }

    let blocks = locate_task_blocks(body)?;
    let mut seen_tasks = BTreeSet::new();
    let mut tasks = Vec::with_capacity(blocks.len());

    for block in blocks {
        if !seen_tasks.insert(block.heading_ref.clone()) {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("duplicate task block '### task: {}'", block.heading_ref),
            ));
        }
        let parsed_yaml: TaskSpecYaml = serde_yaml::from_str(block.yaml_slice).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "invalid YAML in '### task: {}': {err}",
                    block.heading_ref
                ),
            )
        })?;
        if let Some(yaml_ref) = &parsed_yaml.task_ref {
            if yaml_ref != &block.heading_ref {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "task block heading '### task: {}' disagrees with YAML task_ref '{yaml_ref}'",
                        block.heading_ref
                    ),
                ));
            }
        }
        if parsed_yaml.contract_revision < 1 {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "task '{}' contract_revision must be >= 1",
                    block.heading_ref
                ),
            ));
        }
        let mut seen_subtasks = BTreeSet::new();
        for sub in &parsed_yaml.subtasks {
            if !seen_subtasks.insert(sub.subtask_ref.clone()) {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "duplicate subtask_ref '{}' in task '{}'",
                        sub.subtask_ref, block.heading_ref
                    ),
                ));
            }
            sub.validate(&block.heading_ref)?;
        }
        let tags = if parsed_yaml.tags.is_empty() {
            default_task_tags()
        } else {
            parsed_yaml.tags
        };

        tasks.push(TaskSpec {
            task_ref: block.heading_ref,
            target: parsed_yaml.target,
            spec: parsed_yaml.spec,
            scope: parsed_yaml.scope,
            extended_scope: parsed_yaml.extended_scope,
            depends_on: parsed_yaml.depends_on,
            tags,
            invariants: parsed_yaml.invariants,
            acceptance_criteria: parsed_yaml.acceptance_criteria,
            contract_revision: parsed_yaml.contract_revision,
            status: parsed_yaml.status,
            subtasks: parsed_yaml.subtasks,
            receipt: parsed_yaml.receipt,
        });
    }

    Ok(SpecDocument {
        id: fm.id,
        title: fm.title,
        doc_type: fm.doc_type,
        profile: fm.profile,
        status: fm.status,
        cancellation_reason: fm.cancellation_reason,
        spec: fm.spec,
        depends_on: fm.depends_on,
        owners: fm.owners,
        invariants: fm.invariants,
        acceptance_criteria: fm.acceptance_criteria,
        guidance: fm.guidance,
        required_skills: fm.required_skills,
        repositories: fm.repositories,
        linked_to: fm.linked_to,
        closure: fm.closure,
        tasks,
        body: body.to_owned(),
        digest,
    })
}

/// Materialize an ephemeral `--goal` specification document (`spec.md`) for `.acdd/run/<run_id>/spec.md`.
pub fn materialize_ephemeral_goal_spec(
    run_id: &RunId,
    run_name: &RunName,
    goal: &str,
    profile_id: &str,
    scope: &[String],
    tags: &[String],
) -> Result<String, StateError> {
    let trimmed_goal = goal.trim();
    if trimmed_goal.is_empty() {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "ephemeral --goal text cannot be empty",
        ));
    }
    let fm = SpecFrontmatter {
        id: format!("GOAL-{run_id}"),
        title: format!("{run_name}: {trimmed_goal}"),
        doc_type: default_doc_type(),
        profile: Some(profile_id.to_owned()),
        status: SpecStatus::Active,
        cancellation_reason: None,
        spec: Some(trimmed_goal.to_owned()),
        depends_on: Vec::new(),
        owners: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        guidance: None,
        required_skills: Vec::new(),
        repositories: Vec::new(),
        linked_to: Vec::new(),
        closure: None,
    };
    let fm_yaml = serde_yaml::to_string(&fm).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize ephemeral spec frontmatter: {err}"),
        )
    })?;

    let effective_tags = if tags.is_empty() {
        default_task_tags()
    } else {
        tags.to_vec()
    };
    let task_ref = TaskRef::new("goal")?;
    let task_yaml = TaskSpecYaml {
        task_ref: Some(task_ref.clone()),
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
    let task_yaml_str = serde_yaml::to_string(&task_yaml).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize ephemeral task YAML: {err}"),
        )
    })?;

    let rendered = format!(
        "---\n{}---\n\n# {}\n\n{}\n\n### task: {}\n\n```yaml\n{}```\n",
        fm_yaml,
        fm.title,
        trimmed_goal,
        task_ref,
        task_yaml_str
    );
    // Self-verify round-trip invariants.
    let _ = parse_spec(&rendered)?;
    Ok(rendered)
}

/// Deterministically render the final archive Markdown document updating `status`, `closure:`,
/// and every task's `status`, `extended_scope`, `subtasks`, and `receipt`, returning `(markdown, sha256)`.
pub fn render_archive_markdown(
    original_raw: &str,
    input: &ArchiveRenderInput,
) -> Result<(String, String), StateError> {
    if input.closure.status != SpecStatus::Completed {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "archive closure.status must be 'completed'",
        ));
    }
    let parsed = parse_spec(original_raw)?;
    let (fm_str, body) = split_frontmatter(original_raw)?;
    let mut fm: SpecFrontmatter = serde_yaml::from_str(fm_str).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to re-parse spec frontmatter: {err}"),
        )
    })?;

    fm.status = SpecStatus::Completed;
    fm.closure = Some(input.closure.clone());

    let new_fm_yaml = serde_yaml::to_string(&fm).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize archive frontmatter: {err}"),
        )
    })?;

    let blocks = locate_task_blocks(body)?;
    let mut new_body = String::with_capacity(body.len() + 1024);
    let mut cursor = 0usize;

    for (task_idx, block) in blocks.iter().enumerate() {
        let existing = &parsed.tasks[task_idx];
        let mut updated_yaml = TaskSpecYaml {
            task_ref: Some(existing.task_ref.clone()),
            target: existing.target.clone(),
            spec: existing.spec.clone(),
            scope: existing.scope.clone(),
            extended_scope: existing.extended_scope.clone(),
            depends_on: existing.depends_on.clone(),
            tags: existing.tags.clone(),
            invariants: existing.invariants.clone(),
            acceptance_criteria: existing.acceptance_criteria.clone(),
            contract_revision: existing.contract_revision,
            status: existing.status,
            subtasks: existing.subtasks.clone(),
            receipt: existing.receipt.clone(),
        };

        if let Some(patch) = input.tasks.get(&existing.task_ref) {
            for sub in &patch.subtasks {
                sub.validate(&existing.task_ref)?;
            }
            updated_yaml.status = patch.status;
            updated_yaml.extended_scope = patch.extended_scope.clone();
            updated_yaml.subtasks = patch.subtasks.clone();
            updated_yaml.receipt = patch.receipt.clone();
        }

        let serialized_task = serde_yaml::to_string(&updated_yaml).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to serialize archive task '{}': {err}",
                    existing.task_ref
                ),
            )
        })?;

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
    let _ = parse_spec(&rendered)?;
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

fn split_frontmatter(raw: &str) -> Result<(&str, &str), StateError> {
    let normalized = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let after_open = if let Some(rest) = normalized.strip_prefix("---\n") {
        rest
    } else if let Some(rest) = normalized.strip_prefix("---\r\n") {
        rest
    } else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "spec document must start with YAML frontmatter '---'",
        ));
    };

    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed == "---" {
            let fm = &after_open[..offset];
            let body = &after_open[offset + line.len()..];
            return Ok((fm, body));
        }
        offset += line.len();
    }

    Err(StateError::new(
        ErrorCode::ProfileSchemaInvalid,
        "spec document frontmatter is missing closing '---'",
    ))
}

struct LocatedTaskBlock<'a> {
    heading_ref: TaskRef,
    yaml_slice: &'a str,
    yaml_byte_start: usize,
    yaml_byte_end: usize,
}

fn locate_task_blocks(body: &str) -> Result<Vec<LocatedTaskBlock<'_>>, StateError> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
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
        let (line_start, line) = lines[idx];
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]).trim();
        if trimmed.starts_with("```") {
            in_outer_fence = !in_outer_fence;
            idx += 1;
            continue;
        }
        if !in_outer_fence {
            if let Some(raw_ref) = trimmed.strip_prefix("### task:") {
                let ref_str = raw_ref.trim();
                let task_ref = TaskRef::new(ref_str).map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("invalid task heading '### task: {ref_str}': {err}"),
                    )
                })?;

                // Scan forward until the next `### task:` heading or EOF to find ```yaml ... ```
                idx += 1;
                let mut found_yaml: Option<(usize, usize)> = None;
                let mut fence_open_byte: Option<usize> = None;

                while idx < lines.len() {
                    let (sub_start, sub_line) = lines[idx];
                    let sub_trimmed = sub_line.trim_end_matches(&['\r', '\n'][..]).trim();
                    if fence_open_byte.is_none() && sub_trimmed.starts_with("### task:") {
                        break;
                    }
                    if let Some(yaml_start) = fence_open_byte {
                        if sub_trimmed == "```" {
                            found_yaml = Some((yaml_start, sub_start));
                            idx += 1;
                            break;
                        }
                    } else if sub_trimmed == "```yaml" || sub_trimmed == "```yml" {
                        fence_open_byte = Some(sub_start + sub_line.len());
                    } else if sub_trimmed.starts_with("```") {
                        // Skip non-yaml fenced block before yaml block if any, or close it
                        idx += 1;
                        while idx < lines.len() {
                            let (_, inner) = lines[idx];
                            idx += 1;
                            if inner.trim().starts_with("```") {
                                break;
                            }
                        }
                        continue;
                    }
                    idx += 1;
                }

                if fence_open_byte.is_some() && found_yaml.is_none() {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("unclosed ```yaml block under '### task: {task_ref}'"),
                    ));
                }
                let Some((yaml_start, yaml_end)) = found_yaml else {
                    return Err(StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("task '### task: {task_ref}' is missing fenced ```yaml block"),
                    ));
                };
                blocks.push(LocatedTaskBlock {
                    heading_ref: task_ref,
                    yaml_slice: &body[yaml_start..yaml_end],
                    yaml_byte_start: yaml_start,
                    yaml_byte_end: yaml_end,
                });
                pos = yaml_end;
                continue;
            }
        }
        let _ = (line_start, pos);
        idx += 1;
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
profile: code
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
            (
                "missing_frontmatter",
                "# No frontmatter\n",
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
            assert_eq!(err.code, ErrorCode::ProfileSchemaInvalid, "{label}: {err}");
        }
    }

    #[test]
    fn ephemeral_goal_and_archive_renderer_roundtrip() {
        let run_id = RunId::new("run-001").unwrap();
        let run_name = RunName::new("fix-auth").unwrap();
        let ephemeral = materialize_ephemeral_goal_spec(
            &run_id,
            &run_name,
            "Fix token expiry bug",
            "code",
            &["src/auth.rs".to_owned()],
            &[],
        )
        .unwrap();

        let parsed = parse_spec(&ephemeral).unwrap();
        assert_eq!(parsed.id, "GOAL-run-001");
        assert_eq!(parsed.profile.as_deref(), Some("code"));
        assert_eq!(parsed.status, SpecStatus::Active);
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].task_ref.as_str(), "goal");
        assert_eq!(parsed.tasks[0].scope, vec!["src/auth.rs"]);

        let task_ref = TaskRef::new("goal").unwrap();
        let receipt = Receipt {
            run_id: run_id.clone(),
            node_cursor: NodeCursor::task(
                task_ref.clone(),
                vec![GateId::new("deliver").unwrap()],
            ),
            gate_id: GateId::new("deliver").unwrap(),
            worker_id: WorkerId::new("reviewer-1").unwrap(),
            claim_revision: 1,
            contract_revision: 2,
            candidate_tree: TreeOid::new(oid_40('a')).unwrap(),
            committed_oid: Some(CommitOid::new(oid_40('b')).unwrap()),
            accepted_at: "2026-11-17T10:00:00.000Z".to_owned(),
            need_proofs: BTreeMap::from([("review".to_owned(), serde_json::json!({"decision": "pass"}))]),
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
                metrics_summary: BTreeMap::from([("tasks_completed".to_owned(), serde_json::json!(1))]),
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

        let (rendered_1, digest_1) = render_archive_markdown(&ephemeral, &archive_input).unwrap();
        let (rendered_2, digest_2) = render_archive_markdown(&ephemeral, &archive_input).unwrap();
        assert_eq!(rendered_1, rendered_2);
        assert_eq!(digest_1, digest_2);
        assert_eq!(digest_1, compute_spec_digest(&rendered_1));

        let archived = parse_spec(&rendered_1).unwrap();
        assert_eq!(archived.status, SpecStatus::Completed);
        assert_eq!(archived.closure.as_ref().unwrap().target_base.as_deref(), Some("main"));
        assert_eq!(archived.tasks[0].status, SpecStatus::Completed);
        assert_eq!(archived.tasks[0].extended_scope.len(), 1);
        assert_eq!(archived.tasks[0].subtasks.len(), 1);
        assert!(archived.tasks[0].receipt.is_some());

        // Helper spec ID numbering checks
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
