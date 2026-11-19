//! Strictly-typed Request/Response DTOs and command/query enums for `StateEngine` and `StateQueryEngine`.

use crate::context::ClaimResp;
use crate::runner::WaveExecutionReport;
use crate::spec::{Receipt, SpecStatus};
use crate::types::{
    BlackboardRecord, BlackboardStatus, BlackboardTopic, CheckResultRecord, GateNodeRecord,
    IsolationMode, ListRunsPage, ObservabilitySnapshot, RejectionFindings, ReviewEvidence,
    RunRecord, RunStatus, RunTombstoneRecord, SubmitAction, SubtaskRecord, SubtaskStatus,
    TraceStatus,
};
use acdd_core::ids::{CommitOid, NodeCursor, RunId, RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_core::ports::DomainToolOutput;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Request payload for starting a new `Run` from a spec file or ephemeral `--goal`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartReq {
    /// Path to an existing spec Markdown file (mutually exclusive with `goal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_path: Option<String>,
    /// Ephemeral goal text materializing `.acdd/run/<run_id>/spec.md` (`is_ephemeral = true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Optional explicit run name override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<RunName>,
    /// Optional profile override (`base | code | audit | <path>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional initial scope globs (used for `--goal` ephemeral runs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Target base branch/ref for worktree isolation runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_base: Option<String>,
    /// Create a managed run worktree under `.worktrees/<run_name>`.
    #[serde(default)]
    pub worktree: bool,
    /// Force `in_place` isolation for this run.
    #[serde(default)]
    pub in_place: bool,
    /// Explicit worktree path override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
}

/// Response payload returned by `StateCommand::Start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartResp {
    /// Created `Run` record.
    pub run: RunRecord,
    /// Initial ready node cursors.
    pub ready_nodes: Vec<String>,
    /// Materialized spec path (`spec_path` or `.acdd/run/<run_id>/spec.md` when ephemeral).
    pub materialized_spec_path: Option<String>,
}

/// Request payload for drafting the next numbered spec document in a configured collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftReq {
    /// Target collection key from `acdd.yaml` `specs:` (e.g. `"milestones"`, `"plans"`).
    pub collection: String,
    /// URL/file slug for the drafted spec document.
    pub slug: String,
    /// Optional prefix override (must belong to `collection.prefixes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Optional human-readable title (defaults to formatted `slug`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional profile override written into frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Response payload returned by `StateCommand::Draft`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftResp {
    /// Generated sequential spec ID (e.g. `"AC-008"`).
    pub spec_id: String,
    /// Relative filesystem path of the created spec Markdown file.
    pub spec_path: String,
    /// Collection key that owns the file.
    pub collection: String,
}

/// Request payload for claiming or re-claiming a ready/active node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`, or `None` to auto-select next ready node).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Optional explicit session fingerprint override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_fingerprint: Option<String>,
    /// Force take-over of an existing active claim.
    #[serde(default)]
    pub force: bool,
    /// Explicitly authorize stealing an active claim from another host/remote session.
    #[serde(default)]
    pub steal_remote: bool,
    /// Explicit worktree path override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
}

/// Request payload for running gate verification (`command`, `call`, `checks`, scope barrier).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target node cursor.
    pub node: String,
    /// Optional claiming worker identity check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<WorkerId>,
    /// Optional `--test` filter target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_target: Option<String>,
    /// Optional expected `claim_revision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_revision: Option<u32>,
    /// Explicit worktree path override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
}

/// Response payload returned by `StateCommand::Verify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Verified node cursor.
    pub node_cursor: NodeCursor,
    /// Active claim revision.
    pub claim_revision: u32,
    /// Recorded trace status.
    pub trace_status: TraceStatus,
    /// Computed RFC-8785 SHA-256 execution digest.
    pub execution_digest: String,
    /// Process exit code if a command ran.
    pub exit_code: Option<i32>,
    /// Domain check outcomes recorded.
    pub check_results: Vec<CheckResultRecord>,
    /// Log path relative to `.acdd/run/<run_id>/`.
    pub log_relpath: String,
    /// Whether verification succeeded (`true` means trace is valid for `submit --action pass`).
    pub passed: bool,
}

/// Request payload for submitting a gate decision (`pass` or `reject`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target node cursor.
    pub node: String,
    /// Submission action (`pass` or `reject`).
    pub action: SubmitAction,
    /// Submitting worker identity.
    pub worker_id: WorkerId,
    /// Optional expected `claim_revision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_revision: Option<u32>,
    /// Optional `--test` target forwarded if `submit` auto-triggers `verify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_target: Option<String>,
    /// Typed review evidence JSON/struct for `proof: review`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_evidence: Option<ReviewEvidence>,
    /// Structured rejection findings required for `action == Reject`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_findings: Option<RejectionFindings>,
    /// Custom schema JSON evidence (`proof: blueprint` or custom `proofs:` schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_evidence: Option<Value>,
    /// Verified invariant identifiers attested by this submission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_invariants: Vec<String>,
    /// Optional human-readable summary for receipt rollups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Explicitly permit committing onto `main` / `master`.
    #[serde(default)]
    pub allow_main: bool,
    /// Explicit worktree path override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
}

/// Response payload returned by `StateCommand::Submit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmitResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Submitted node cursor.
    pub node_cursor: NodeCursor,
    /// Applied submission action.
    pub action: SubmitAction,
    /// Updated node status after submission.
    pub new_node_status: crate::types::NodeStatus,
    /// Updated run status (`Completed` if terminal archive/handoff gate closed).
    pub run_status: RunStatus,
    /// Created/recovered commit OID if `save.post == commit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_oid: Option<CommitOid>,
    /// Receipt emitted if `receipt: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// Archive path if archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    /// Repository-relative path to the materialized gate report when `save.report` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_path: Option<String>,
    /// Newly unblocked/ready node cursors.
    pub unblocked_nodes: Vec<String>,
}

/// Request payload for extending a task's admitted write scope (`extend_scope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtendScopeReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target task or task-gate cursor.
    pub node: String,
    /// Additional repository-relative paths or globs to admit.
    pub paths: Vec<String>,
    /// Non-empty justification for the scope extension.
    pub reason: String,
    /// Optional worker identity check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<WorkerId>,
}

/// Response payload returned by `StateCommand::ExtendScope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtendScopeResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Target node cursor.
    pub node_cursor: NodeCursor,
    /// New `contract_revision`.
    pub contract_revision: u32,
    /// Effective full admitted scope (`scope ∪ extended_scope`).
    pub admitted_scope: Vec<String>,
}

/// Request payload for reopening a gate or task container (`reopen`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReopenReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target gate or task container cursor.
    pub node: String,
    /// Cascade-stale `in_progress`/`completed` downstream dependent tasks.
    #[serde(default)]
    pub cascade: bool,
    /// Optional operator reason recorded on the blackboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Response payload returned by `StateCommand::Reopen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReopenResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Reopened node cursor.
    pub node_cursor: NodeCursor,
    /// Nodes transitioned or reset by the reopen operation.
    pub affected_nodes: Vec<String>,
}

/// Request payload for synchronizing spec edits (and optional profile rebind) into an active `Run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Rebind non-completed gates against the latest resolved profile (`--rebind-profile`).
    #[serde(default)]
    pub rebind_profile: bool,
}

/// Response payload returned by `StateCommand::Sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Updated `spec_digest`.
    pub spec_digest: String,
    /// Whether `rebind_profile` was applied.
    pub rebound_profile: bool,
    /// Number of newly added nodes.
    pub added_nodes: usize,
    /// Number of updated/affected nodes.
    pub updated_nodes: usize,
}

/// Request payload for updating a subtask status and mandatory terminal evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubtaskReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Owning task reference.
    pub task_ref: TaskRef,
    /// Target subtask reference.
    pub subtask_ref: SubtaskRef,
    /// New subtask status (`pending`, `in_progress`, `completed`, `cancelled`).
    pub status: SubtaskStatus,
    /// Required non-empty evidence when `status` is `completed` or `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Response payload returned by `StateCommand::Subtask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubtaskResp {
    /// Updated subtask row.
    pub subtask: SubtaskRecord,
}

/// Request payload for posting a message to the run blackboard (`blackboard post`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlackboardPostReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Optional source node cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Optional target recipient node cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_node: Option<String>,
    /// Optional subtask reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtask_ref: Option<SubtaskRef>,
    /// Optional gate label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// Author identity.
    pub author: String,
    /// Blackboard topic (`draft | notes | findings | blockers | decisions | deferred`).
    pub topic: BlackboardTopic,
    /// Message text or JSON payload.
    pub payload: String,
}

/// Response payload returned by `StateCommand::BlackboardPost`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardPostResp {
    /// Persisted blackboard entry with assigned `id`.
    pub entry: BlackboardRecord,
}

/// Request payload for clearing/cancelling an active `Run` (`acdd clear`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClearReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Optional cancellation reason recorded in the tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Force cleanup of dirty managed worktrees.
    #[serde(default)]
    pub force: bool,
}

/// Response payload returned by `StateCommand::Clear`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClearResp {
    /// Recorded tombstone with `terminal_status = cleared`.
    pub tombstone: RunTombstoneRecord,
    /// Snapshot refs and worktrees cleaned up.
    pub cleaned_items: Vec<String>,
}

/// Request payload for automated wave orchestration (`acdd run`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Maximum concurrent wave jobs (`--jobs N`; clamped to `1` for `in_place`).
    #[serde(default)]
    pub jobs: Option<usize>,
    /// Explicitly permit commits/integration onto `main` / `master`.
    #[serde(default)]
    pub allow_main: bool,
    /// Dry-run mode computing waves without mutating state.
    #[serde(default)]
    pub dry_run: bool,
}

/// Response payload returned by `StateCommand::Run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Wave execution report.
    pub report: WaveExecutionReport,
}

/// Request payload for invoking an external `DomainProvider` plugin tool (`acdd tool <domain>_<name>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallPluginToolReq {
    /// Full plugin tool name (`<domain>_<name>`).
    pub tool_name: String,
    /// JSON arguments validated against `DomainToolDef.input_schema`.
    #[serde(default)]
    pub args: Value,
    /// Optional active run selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Optional active node cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Optional target worktree override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
}

/// Response payload returned by `StateCommand::CallPluginTool`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallPluginToolResp {
    /// Qualified plugin tool name invoked.
    pub tool_name: String,
    /// Output emitted by the `DomainProvider`.
    pub output: DomainToolOutput,
}

/// Read-only query request for `StateQuery::Status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// Read-only response returned by `StateQuery::Status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResp {
    /// Resolved `Run` record (`None` if no active run or DB missing).
    pub run: Option<RunRecord>,
    /// All `gate_nodes` in the run.
    pub nodes: Vec<GateNodeRecord>,
    /// Ready node cursors.
    pub ready_nodes: Vec<String>,
    /// Spec status summary if available.
    pub spec_status: Option<SpecStatus>,
}

/// Read-only query request for `StateQuery::List`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListReq {
    /// Optional workspace filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Optional run status filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    /// Include closed tombstones.
    #[serde(default)]
    pub include_tombstones: bool,
    /// Page size (`1..=100`, default `50`).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Zero-based page offset.
    #[serde(default)]
    pub offset: usize,
    /// Optional `runs.generation` pin for stable pagination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

/// Read-only response returned by `StateQuery::List`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListResp {
    /// Paginated runs and tombstones.
    pub page: ListRunsPage,
}

/// Read-only query request for `StateQuery::Show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShowReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target node cursor (`None` returns run-level summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// Read-only response returned by `StateQuery::Show`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowResp {
    /// Resolved `Run` record.
    pub run: RunRecord,
    /// Requested node record (if `node` specified).
    pub node: Option<GateNodeRecord>,
    /// Subtasks belonging to the node (if task).
    pub subtasks: Vec<SubtaskRecord>,
    /// Optional budgeted context preview when showing a gate node.
    pub context_preview: Option<Value>,
}


/// Read-only query request for `StateQuery::Context`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target gate cursor (`None` auto-selects the single active claimed or ready gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Optional worker filter when auto-selecting or validating an active claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<WorkerId>,
}

/// Read-only query request for `StateQuery::Next`.

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NextReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Optional limit on returned ready cursors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Read-only response returned by `StateQuery::Next`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NextResp {
    /// Active run identifier (`None` when no active run exists).
    pub run_id: Option<RunId>,
    /// Effective isolation mode.
    pub isolation: Option<IsolationMode>,
    /// Ready node cursors ordered by topological wave and `seq_order`.
    pub ready_cursors: Vec<String>,
    /// Parallel waves grouped for non-overlapping execution.
    pub waves: Vec<Vec<String>>,
}

/// Read-only query request for `StateQuery::BlackboardRead`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlackboardReadReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Optional filter by source node cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Optional filter by recipient node cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_node: Option<String>,
    /// Optional topic filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<BlackboardTopic>,
    /// Optional status filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<BlackboardStatus>,
    /// Page size (`1..=100`, default `50`).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Zero-based page offset.
    #[serde(default)]
    pub offset: usize,
    /// Optional `runs.generation` pin for stable pagination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

/// Read-only response returned by `StateQuery::BlackboardRead`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardReadResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Blackboard items on the page.
    pub items: Vec<BlackboardRecord>,
    /// Total matching items.
    pub total: usize,
    /// Observed run generation.
    pub generation: u64,
    /// Applied page limit.
    pub limit: usize,
    /// Applied page offset.
    pub offset: usize,
}

/// Read-only query request for `StateQuery::Observability`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityReq {
    /// Whether to include Prometheus 0.0.4 text output.
    #[serde(default)]
    pub include_prometheus: bool,
}

/// Read-only response returned by `StateQuery::Observability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservabilityResp {
    /// Aggregate state metrics snapshot.
    pub snapshot: ObservabilitySnapshot,
    /// Optional Prometheus 0.0.4 text exposition payload.
    pub prometheus_text: Option<String>,
}

/// Snapshot operation on an active run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotAction {
    /// Capture the task's current files.
    Save,
    /// Restore a task checkpoint.
    Restore,
    /// Delete checkpoint refs.
    Clear,
}

/// Request for an active run's task checkpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotReq {
    /// Optional active run ID or name.
    pub run: Option<String>,
    /// Operation to execute.
    pub action: SnapshotAction,
    /// Task to address for task-scoped operations.
    pub task_ref: Option<TaskRef>,
    /// Clear all checkpoints belonging to the run.
    #[serde(default)]
    pub all: bool,
    /// Override the save revision or select an exact restore revision.
    pub revision: Option<u32>,
}

/// Outcome of a checkpoint operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotResp {
    /// Refs saved, restored, or deleted.
    pub refs: Vec<String>,
}

/// Mutating and plugin commands executed by [`super::StateEngine`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum StateCommand {
    /// Start a new `Run` from a spec file or ephemeral `--goal`.
    Start(StartReq),
    /// Draft a sequentially numbered spec document in a configured collection.
    Draft(DraftReq),
    /// Claim or re-claim a ready/active node.
    Claim(ClaimReq),
    /// Execute gate verification (`command`, `call`, `checks`, scope barrier).
    Verify(VerifyReq),
    /// Submit `pass` or `reject` for a claimed node.
    Submit(Box<SubmitReq>),
    /// Extend a task's admitted write scope.
    ExtendScope(ExtendScopeReq),
    /// Reopen a gate or task container (with optional `--cascade`).
    Reopen(ReopenReq),
    /// Synchronize spec edits (and optional `--rebind-profile`) into an active `Run`.
    Sync(SyncReq),
    /// Update a task's subtask status and terminal evidence.
    Subtask(SubtaskReq),
    /// Post a message to the run blackboard.
    BlackboardPost(BlackboardPostReq),
    /// Clear/cancel an active `Run` into `run_tombstones(terminal_status = 'cleared')`.
    Clear(ClearReq),
    /// Save, restore, or clear task checkpoints.
    Snapshot(SnapshotReq),
    /// Execute automated DAG waves (`acdd run`).
    Run(RunReq),
    /// Invoke an external `DomainProvider` plugin tool (`<domain>_<name>`).
    CallPluginTool(CallPluginToolReq),
}

impl StateCommandOutput {
    /// Serialize the inner command payload directly to an untagged [`serde_json::Value`].
    pub fn into_untagged_json(self) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::Start(resp) => serde_json::to_value(resp),
            Self::Draft(resp) => serde_json::to_value(resp),
            Self::Claim(resp) => serde_json::to_value(resp),
            Self::Verify(resp) => serde_json::to_value(resp),
            Self::Submit(resp) => serde_json::to_value(resp),
            Self::ExtendScope(resp) => serde_json::to_value(resp),
            Self::Reopen(resp) => serde_json::to_value(resp),
            Self::Sync(resp) => serde_json::to_value(resp),
            Self::Subtask(resp) => serde_json::to_value(resp),
            Self::BlackboardPost(resp) => serde_json::to_value(resp),
            Self::Clear(resp) => serde_json::to_value(resp),
            Self::Snapshot(resp) => serde_json::to_value(resp),
            Self::Run(resp) => serde_json::to_value(resp),
            Self::CallPluginTool(resp) => serde_json::to_value(resp),
        }
    }
}

/// Typed output returned by [`super::StateEngine::execute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StateCommandOutput {
    /// Output of `Start`.
    Start(Box<StartResp>),
    /// Output of `Draft`.
    Draft(DraftResp),
    /// Output of `Claim`.
    Claim(Box<ClaimResp>),
    /// Output of `Verify`.
    Verify(VerifyResp),
    /// Output of `Submit`.
    Submit(Box<SubmitResp>),
    /// Output of `ExtendScope`.
    ExtendScope(ExtendScopeResp),
    /// Output of `Reopen`.
    Reopen(ReopenResp),
    /// Output of `Sync`.
    Sync(SyncResp),
    /// Output of `Subtask`.
    Subtask(SubtaskResp),
    /// Output of `BlackboardPost`.
    BlackboardPost(BlackboardPostResp),
    /// Output of `Clear`.
    Clear(Box<ClearResp>),
    /// Output of `Snapshot`.
    Snapshot(SnapshotResp),
    /// Output of `Run`.
    Run(RunResp),
    /// Output of `CallPluginTool`.
    CallPluginTool(CallPluginToolResp),
}

/// Read-only request payload for `StateQuery::Validate`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateReq {
    /// Optional workspace-relative document path; `None` validates the whole workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional explicit config path override (`--config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_config: Option<PathBuf>,
    /// Optional explicit profile override (`--profile`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_profile: Option<String>,
}

/// Single diagnostic entry emitted by `StateQuery::Validate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateDiagnostic {
    /// Outcome status (`"pass"` or `"fail"`).
    pub status: String,
    /// Stable diagnostic code string.
    pub code: String,
    /// Workspace-relative file path.
    pub path: String,
    /// One-based line number (0 when unknown).
    pub line: usize,
    /// One-based column number (0 when unknown).
    pub column: usize,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Summary counters returned by `StateQuery::Validate`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateSummary {
    /// Passed diagnostic checks count.
    pub passed: usize,
    /// Failed diagnostic checks count.
    pub failed: usize,
    /// Warnings count.
    pub warnings: usize,
    /// Validated documents count.
    pub documents_validated: usize,
}

/// Typed response payload returned by `StateQuery::Validate`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateResp {
    /// Overall validation pass (`failed == 0`).
    pub valid: bool,
    /// Validation scope description (`"workspace"` or `"document:<path>"`).
    pub scope: String,
    /// Aggregate summary counts.
    pub summary: ValidateSummary,
    /// Diagnostic list ordered deterministically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ValidateDiagnostic>,
    /// Structured advisory warnings (JSON objects).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Value>,
}

/// Read-only request payload for `StateQuery::Roadmap`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoadmapReq {
    /// Optional spec ID filter / drill-down selector (including archived specs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// Optional spec collection display filter applied after building the full cross-collection DAG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Show only Wave 0 ready specs.
    #[serde(default)]
    pub ready_only: bool,
    /// Show only in-progress specs (active runs).
    #[serde(default)]
    pub active_only: bool,
    /// Include intra-spec task drill-down for every returned spec.
    #[serde(default)]
    pub include_tasks: bool,
    /// Include archived specs in `archived` output (archived specs always count in `archived_count` and dependency resolution).
    #[serde(default)]
    pub archived: bool,
    /// Restrict spec discovery strictly to the current workspace/worktree (`--current`).
    #[serde(default)]
    pub current_only: bool,
}

/// Worktree copy summary when a spec diverges across Git worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapWorktreeCopy {
    /// Worktree label/path (e.g. `"main"` or `".worktrees/feat-x"`).
    pub worktree: String,
    /// Lifecycle status in that worktree (`"archived"`, `"active"`, `"planned"`, `"cancelled"`).
    pub status: String,
    /// Whether this copy was selected as canonical.
    pub canonical: bool,
}

/// Resolved dependency status entry in [`RoadmapSpecEntry`] or [`RoadmapTaskEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapDepEntry {
    /// Raw `depends_on` string as written in the spec.
    pub raw: String,
    /// Resolved status (`satisfied`, `archived`, `active`, `planned`, `cancelled`, `cleared`, `missing`).
    pub status: String,
    /// Whether the dependency is currently satisfied.
    pub satisfied: bool,
}

/// Active gate cursor and worker entry on an in-progress run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapActiveGateEntry {
    /// Node cursor string (e.g. `T-02.build`).
    pub cursor: String,
    /// Gate node status (e.g. `active`).
    pub status: String,
    /// Claiming worker ID if claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// Task-level drill-down entry inside [`RoadmapSpecEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapTaskEntry {
    /// Task reference (`T-01`, etc.).
    pub task_ref: String,
    /// Task target summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Live or planned task status (`planned`, `ready`, `in_progress`, `completed`, `cancelled`).
    pub status: String,
    /// Intra-spec topological wave (`0..M`).
    pub wave: usize,
    /// Task dependencies with resolved satisfaction status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RoadmapDepEntry>,
    /// Reverse dependencies (`<task_ref>` or `<spec_id>:<task_ref>`) blocked by this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    /// Effective write scope globs for this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Other tasks in the same intra-spec wave whose scopes overlap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_overlaps: Vec<String>,
    /// Active gate cursors on this task (when part of an active run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_gates: Vec<RoadmapActiveGateEntry>,
}

/// Spec entry in `RoadmapResp` (`in_progress`, `waves`, or `archived`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapSpecEntry {
    /// Specification document ID.
    pub spec_id: String,
    /// Specification title.
    pub title: String,
    /// Spec collection name (`plans`, `milestones`, `specs`, etc.).
    pub collection: String,
    /// Workspace-relative spec file path.
    pub path: String,
    /// Document type (`contract`, `plan`, etc.).
    pub doc_type: String,
    /// Effective spec status (`planned`, `ready`, `in_progress`, `completed`, `cancelled`, `archived`).
    pub status: String,
    /// Active run ID if this spec has an active run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Active run worktree path if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Source worktree label when discovered only in a secondary worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_worktree: Option<String>,
    /// Divergent worktree copies when the same `spec_id` differs across Git worktrees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub divergence: Vec<RoadmapWorktreeCopy>,
    /// Number of completed tasks (from active run or spec document).
    pub completed_tasks: usize,
    /// Total number of tasks in this spec.
    pub total_tasks: usize,
    /// Active gate cursors on the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_gates: Vec<RoadmapActiveGateEntry>,
    /// Transitive count of downstream specs unblocked by this spec.
    pub unblocks_specs: usize,
    /// Transitive count of downstream tasks unblocked by this spec.
    pub unblocks_tasks: usize,
    /// True when no other spec in the same wave/active tier overlaps this spec's write scope.
    pub parallel_safe: bool,
    /// Spec IDs in the same wave/active tier whose write scopes overlap this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_overlaps: Vec<String>,
    /// Spec-level dependencies with resolved satisfaction status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RoadmapDepEntry>,
    /// Reverse dependencies (`<spec_id>` or `<spec_id>:<task_ref>`) blocked by this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    /// Task drill-down entries (populated when `spec_id` is requested or `include_tasks` is true).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<RoadmapTaskEntry>,
}

/// Topological wave of planned specs ordered by critical-path priority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapWave {
    /// Zero-based wave index (`0` = ready now).
    pub wave: usize,
    /// Specs in this wave sorted descending by `(unblocks_specs, unblocks_tasks)` then `spec_id`.
    pub specs: Vec<RoadmapSpecEntry>,
}

/// Read-only response returned by `StateQuery::Roadmap`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoadmapResp {
    /// Specs with an active `Run`.
    pub in_progress: Vec<RoadmapSpecEntry>,
    /// Planned specs grouped into topological waves (`Wave 0` = ready).
    pub waves: Vec<RoadmapWave>,
    /// Archived specs (populated when `include_archived == true` or `spec_id` targets an archived spec).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived: Vec<RoadmapSpecEntry>,
    /// Total count of archived specs discovered across collections.
    pub archived_count: usize,
    /// Spec dependency cycles detected during topological layering, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycles: Vec<Vec<String>>,
    /// Cross-worktree divergence warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// All read-only state queries executed directly over `dyn StateReader` by [`super::StateQueryEngine`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StateQuery {
    /// Inspect status of an active run.
    Status(StatusReq),
    /// Paginate runs and closed tombstones.
    List(ListReq),
    /// Show details for a run or specific node cursor.
    Show(ShowReq),
    /// List ready node cursors and topological execution waves.
    Next(NextReq),
    /// Paginate blackboard entries for a run.
    BlackboardRead(BlackboardReadReq),
    /// Aggregate observability snapshot (and optional Prometheus text).
    Observability(ObservabilityReq),
    /// Validate workspace specs, config, profiles, and dependencies.
    Validate(ValidateReq),
    /// Assemble a budgeted `ClaimResp` without mutating claim state.
    Context(ContextReq),
    /// Compute cross-collection roadmap, dependency waves, priority, overlaps, and task drill-down.
    Roadmap(RoadmapReq),
}

/// Typed output returned by [`super::StateQueryEngine::query`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StateQueryOutput {
    /// Output of `Status`.
    Status(Box<StatusResp>),
    /// Output of `List`.
    List(ListResp),
    /// Output of `Show`.
    Show(Box<ShowResp>),
    /// Output of `Next`.
    Next(NextResp),
    /// Output of `BlackboardRead`.
    BlackboardRead(BlackboardReadResp),
    /// Output of `Observability`.
    Observability(ObservabilityResp),
    /// Output of `Validate`.
    Validate(ValidateResp),
    /// Output of `Context` (`ClaimResp` assembled read-only).
    Context(Box<crate::context::ClaimResp>),
    /// Output of `Roadmap`.
    Roadmap(Box<RoadmapResp>),
}

impl StateQueryOutput {
    /// Serialize the inner query payload directly to an untagged [`serde_json::Value`].
    pub fn into_untagged_json(self) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::Status(resp) => serde_json::to_value(resp),
            Self::List(resp) => serde_json::to_value(resp),
            Self::Show(resp) => serde_json::to_value(resp),
            Self::Next(resp) => serde_json::to_value(resp),
            Self::BlackboardRead(resp) => serde_json::to_value(resp),
            Self::Observability(resp) => serde_json::to_value(resp),
            Self::Validate(resp) => serde_json::to_value(resp),
            Self::Context(resp) => serde_json::to_value(resp),
            Self::Roadmap(resp) => serde_json::to_value(resp),
        }
    }
}

/// Map legacy `StateEngineConfig` / workspace fields helper placeholder.
pub type LinkedWorkspacesMap = BTreeMap<String, PathBuf>;
