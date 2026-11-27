//! Strictly-typed Request/Response DTOs and command/query enums for `StateEngine` and `StateQueryEngine`.

use crate::context::ClaimResp;
use crate::workflow::GateDispatchInfo;
use crate::runner::WaveExecutionReport;
use crate::types::{
    ArtifactKind, BlackboardRecord, BlackboardStatus, BlackboardTopic, CheckResultRecord,
    GateNodeRecord, IsolationMode, ListRunsPage, ObservabilitySnapshot, RejectionFindings,
    ReviewEvidence, RunRecord, RunStatus, RunTombstoneRecord, SubmitAction, SubtaskRecord,
    TraceStatus,
};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::ids::{CommitOid, NodeCursor, RunId, RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_core::ports::DomainToolOutput;
use acdd_spec::types::ValidateReq;
use acdd_spec::{Receipt, SpecStatus, SubtaskStatus};
pub use acdd_spec::{
    DraftReq, DraftResp, RoadmapDepEntry, RoadmapReq, RoadmapResp, RoadmapSpecEntry,
    RoadmapTaskEntry, RoadmapWave, ValidateDiagnostic, ValidateResp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// Optional workflow override (`base | code | documentation | <path>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
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
    /// Sequential coordinator dispatch plan across all executable workflow gates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_plan: Vec<GateDispatchInfo>,
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
    /// Context detail budget (`compact` = 10,000 bytes default, `full` = 32 KiB).
    #[serde(default)]
    pub detail: acdd_core::response::Detail,
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

/// Request payload for synchronizing spec edits (and optional workflow rebind) into an active `Run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Rebind non-completed gates against the latest resolved workflow (`--rebind-workflow`).
    #[serde(default)]
    pub rebind_workflow: bool,
}

/// Response payload returned by `StateCommand::Sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncResp {
    /// Run identifier.
    pub run_id: RunId,
    /// Updated `spec_digest`.
    pub spec_digest: String,
    /// Whether `rebind_workflow` was applied.
    pub rebound_workflow: bool,
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
    /// Purge an already-cleared tombstone (`terminal_status = 'cleared'`) instead of clearing an active run.
    #[serde(default)]
    pub purge: bool,
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

/// Artifact filter for `StateQuery::Show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSelector {
    /// Submitted proof payload (`proof`).
    Proof,
    /// Structured rejection findings payload (`findings`).
    Findings,
    /// Acceptance/closure receipt payload (`receipt`).
    Receipt,
    /// All persisted proof, findings, and receipt artifacts (`all`).
    All,
}

impl ArtifactSelector {
    /// Whether this selector includes `kind`.
    pub const fn matches_kind(self, kind: ArtifactKind) -> bool {
        match self {
            Self::Proof => matches!(kind, ArtifactKind::Proof),
            Self::Findings => matches!(kind, ArtifactKind::Findings),
            Self::Receipt => matches!(kind, ArtifactKind::Receipt),
            Self::All => matches!(
                kind,
                ArtifactKind::Proof | ArtifactKind::Findings | ArtifactKind::Receipt
            ),
        }
    }
}

impl std::str::FromStr for ArtifactSelector {
    type Err = AcddError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "proof" => Ok(Self::Proof),
            "findings" => Ok(Self::Findings),
            "receipt" => Ok(Self::Receipt),
            "all" => Ok(Self::All),
            other => Err(AcddError::new(
                ErrorCode::WorkflowSchemaInvalid,
                format!(
                    "invalid artifact selector '{other}'; expected one of: proof, findings, receipt, all"
                ),
            )),
        }
    }
}

/// Summary (and optionally parsed JSON payload) of a persisted gate artifact in [`ShowResp`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GateArtifactEntry {
    /// Node ID that produced this artifact.
    pub node_id: String,
    /// Artifact category (`proof`, `findings`, or `receipt`).
    pub kind: ArtifactKind,
    /// Claim revision associated with this artifact.
    pub claim_revision: u32,
    /// UTC ISO-8601 creation timestamp.
    pub created_at: String,
    /// Optional Git commit OID recorded when persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_commit: Option<CommitOid>,
    /// Canonical JSON payload length in bytes.
    pub payload_bytes: usize,
    /// Full parsed JSON payload (populated only when `artifact` matches).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// Read-only query request for `StateQuery::Show`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShowReq {
    /// Optional run ID or name selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Target node cursor (`None` returns run-level summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Optional artifact selector (`proof | findings | receipt | all`) to include parsed payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactSelector>,
    /// Include `run.profile_json` on node-level `show` (omitted by default to avoid ~25 KiB bloat).
    #[serde(default)]
    pub include_profile: bool,
}

/// Read-only response returned by `StateQuery::Show`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowResp {
    /// Resolved `Run` record (`profile_json` is omitted on node-level `show` unless `include_profile` is true).
    pub run: RunRecord,
    /// Requested node record (if `node` specified).
    pub node: Option<GateNodeRecord>,
    /// Subtasks belonging to the node (if task).
    pub subtasks: Vec<SubtaskRecord>,
    /// Persisted `proof` / `findings` / `receipt` artifact metadata (and parsed payloads when selected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_artifacts: Vec<GateArtifactEntry>,
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
    /// Response detail budget (`compact` default <= 10,000 bytes, or `full` up to 32 KiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<acdd_core::response::Detail>,
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
    /// Coordinator dispatch metadata for each cursor in `ready_cursors`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ready_gates: Vec<GateDispatchInfo>,
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
    /// List checkpoint refs.
    List,
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
    /// Override the save revision or select an exact restore / delete revision.
    pub revision: Option<u32>,
    /// Standalone label under `refs/acdd/snapshots/_adhoc/<name>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Git worktree that owns the snapshot refs and file restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Delete refs older than `<N><s|m|h|d|w>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<String>,
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
    /// Synchronize spec edits (and optional `--rebind-workflow`) into an active `Run`.
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
