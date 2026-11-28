//! Canonical domain enums, database record models, query/pagination DTOs, and transition plan contracts.

use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ids::{CommitOid, RunId, RunName, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_spec::{ExtendedScopeEntry, InvariantsState, SubtaskStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

macro_rules! define_string_enum {
    ($(#[$meta:meta])* $name:ident, { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(
                $(#[$vmeta])*
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl $name {
            /// Return the canonical lowercase wire/SQL string representation.
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = AcddError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($wire => Ok(Self::$variant),)+
                    other => Err(AcddError::new(
                        ErrorCode::WorkflowSchemaInvalid,
                        format!("invalid {} value '{}'", stringify!($name), other),
                    )),
                }
            }
        }
    };
}

define_string_enum!(
    /// Lifecycle status of a `Run` row in `runs`.
    RunStatus,
    {
        /// Run is actively accepting gate claims, verifications, and submissions.
        Active => "active",
        /// Run completed all gates and archived cleanly.
        Completed => "completed",
        /// Run was cancelled or cleared before completion.
        Cancelled => "cancelled",
    }
);

define_string_enum!(
    /// Structural kind of a node in `gate_nodes`.
    NodeKind,
    {
        /// Structural container node (`root:<container>` or `tasks#<idx>`).
        Container => "container",
        /// Root-level gate executed outside individual tasks.
        RootGate => "root_gate",
        /// Task container node (`task:<task_ref>`, `gate_id = "__task__"`).
        Task => "task",
        /// Gate nested inside a task container (`task:<task_ref>:<gate>`).
        TaskGate => "task_gate",
    }
);

define_string_enum!(
    /// Execution status of a node in `gate_nodes`.
    NodeStatus,
    {
        /// Waiting on predecessors or dependencies.
        Pending => "pending",
        /// Ready to be claimed or executed.
        Ready => "ready",
        /// Actively claimed or executing.
        InProgress => "in_progress",
        /// Accepted and complete.
        Completed => "completed",
        /// Invalidated by candidate drift, reopen cascade, or rollback.
        Stale => "stale",
        /// Exceeded `max_attempts` or cycle budget; requires operator intervention/reopen.
        Escalated => "escalated",
        /// Cancelled by spec sync or operator action.
        Cancelled => "cancelled",
    }
);

define_string_enum!(
    /// Outcome of a `gate_claims` lease record.
    ClaimOutcome,
    {
        /// Currently active lease (`ended_at IS NULL`).
        Active => "active",
        /// Claim closed via passing submission.
        Passed => "passed",
        /// Claim closed via rejection submission.
        Rejected => "rejected",
        /// Claim replaced via `--force` take-over.
        Superseded => "superseded",
        /// Claim invalidated by `acdd reopen`.
        Reopened => "reopened",
        /// Claim cancelled by spec sync or run clear.
        Cancelled => "cancelled",
    }
);

define_string_enum!(
    /// Execution outcome of a `gate_traces` verification or action run.
    TraceStatus,
    {
        /// Command or mutating domain action is currently executing (`finished_at IS NULL`).
        Running => "running",
        /// Command/checks satisfied `proof: command` or `call` contract.
        Passed => "passed",
        /// Command/contract exited with an unexpected status or missing target test.
        Failed => "failed",
        /// Process exceeded `max_command_timeout_seconds` and was killed.
        TimedOut => "timed_out",
        /// One or more `checks:` domain verifiers failed.
        CheckFailed => "check_failed",
        /// Working-tree changes violated `is_agent_writable` for the admitted scope.
        ScopeViolation => "scope_violation",
        /// Process aborted externally or interrupted before normal completion.
        Aborted => "aborted",
    }
);

define_string_enum!(
    /// Artifact payload category stored in `gate_artifacts`.
    ArtifactKind,
    {
        /// Typed proof JSON payload submitted at gate acceptance.
        Proof => "proof",
        /// Structured rejection findings linked to a rejected submission.
        Findings => "findings",
        /// Gate or closure receipt rollup payload.
        Receipt => "receipt",
        /// Immutable accepted `GateDef` snapshot preserved across `sync --rebind-profile`.
        AcceptedSnapshot => "accepted_snapshot",
    }
);

define_string_enum!(
    /// Collaboration topic for `blackboard` messages.
    BlackboardTopic,
    {
        /// Exploratory design or scratch note cleared at container receipt.
        Draft => "draft",
        /// Non-obvious technical context note cleared at container receipt.
        Notes => "notes",
        /// Review or verification findings payload.
        Findings => "findings",
        /// Blocker requiring resolution before progress.
        Blockers => "blockers",
        /// Architectural or implementation decision record.
        Decisions => "decisions",
        /// Deferred follow-up carried over from container to run closure.
        Deferred => "deferred",
    }
);

define_string_enum!(
    /// Resolution lifecycle status of a `blackboard` entry.
    BlackboardStatus,
    {
        /// Newly posted and unresolved.
        Open => "open",
        /// Seen by a recipient without full resolution yet.
        Acknowledged => "acknowledged",
        /// Resolved or closed at container receipt.
        Resolved => "resolved",
        /// Deferred item promoted to run level or archive closure.
        CarriedOver => "carried_over",
    }
);

define_string_enum!(
    /// Severity level for structured review findings.
    FindingSeverity,
    {
        /// Release-blocking correctness, security, or invariant breach.
        Critical => "critical",
        /// Significant defect requiring remediation before acceptance.
        High => "high",
        /// Moderate defect or maintainability concern.
        Medium => "medium",
        /// Minor improvement or polish item.
        Low => "low",
    }
);

define_string_enum!(
    /// Workspace isolation strategy configured in `git.isolation`.
    IsolationMode,
    {
        /// Tasks execute directly in the primary workspace without separate task worktrees.
        InPlace => "in_place",
        /// Tasks execute in dedicated Git worktrees.
        Worktree => "worktree",
    }
);

define_string_enum!(
    /// Git worktree lifecycle ownership configured in `git.worktrees`.
    WorktreeLifecycle,
    {
        /// Operator or external tool manages worktrees manually.
        Manual => "manual",
        /// Engine creates and cleans up run/task worktrees automatically.
        Managed => "managed",
    }
);

define_string_enum!(
    /// Persistence action configured in `save` / `save.post`.
    SaveAction,
    {
        /// No Git commit is created.
        None => "none",
        /// Create a scoped Git commit on the node's branch.
        Commit => "commit",
        /// Save a parentless scoped Git checkpoint.
        Snapshot => "snapshot",
    }
);

#[allow(clippy::derivable_impls)]
impl Default for SaveAction {
    fn default() -> Self {
        Self::None
    }
}

define_string_enum!(
    /// Gate submission decision passed to `acdd submit --action`.
    SubmitAction,
    {
        /// Accept the gate and advance downstream transitions.
        Pass => "pass",
        /// Reject the gate with structured findings and route to `fail:`.
        Reject => "reject",
    }
);

define_string_enum!(
    /// Final disposition recorded in `run_tombstones.terminal_status`.
    TerminalStatus,
    {
        /// Run closed cleanly through its terminal archive/handoff gate.
        Completed => "completed",
        /// Run was force-cleared before completion; never satisfies dependencies.
        Cleared => "cleared",
    }
);

/// Individual structured finding item required when rejecting a review gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindingItem {
    /// Finding severity classification.
    pub severity: FindingSeverity,
    /// Optional repository-relative path affected by the finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Concrete defect description.
    pub message: String,
    /// Actionable remediation required before re-submission.
    pub remediation: String,
}

/// Classification of a gate rejection finding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectionKind {
    /// Localized implementation or verification defect fixed by returning to the target gate.
    #[default]
    Defect,
    /// Structural/architectural mismatch requiring re-architecture and a fresh builder context.
    ArchitecturalDrift,
}

/// Structured findings payload required for `submit --action reject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectionFindings {
    /// Classification of the rejection (`defect` by default, or `architectural_drift`).
    #[serde(default)]
    pub kind: RejectionKind,
    /// Concise summary of why the gate was rejected.
    pub summary: String,
    /// Optional blackboard message ID referencing the posted discussion/findings thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blackboard_message_id: Option<i64>,
    /// One or more structured finding items.
    pub items: Vec<FindingItem>,
}

/// Per-contour review attestation inside `ReviewEvidence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewContourResult {
    /// Whether this contour applied to the reviewed diff/artifacts.
    pub applicable: bool,
    /// Concrete verification evidence (must be non-empty on `pass`).
    pub evidence: String,
}

/// Typed evidence payload required for `proof: review` submissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewEvidence {
    /// Review outcome matching the submission action.
    pub decision: SubmitAction,
    /// Map keyed 1:1 by the gate's declared `contours`.
    pub contours: BTreeMap<String, ReviewContourResult>,
    /// Concise review summary.
    pub summary: String,
    /// Optional non-blocking notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Recorded outcome of a single `DomainProvider::run_check` invocation inside a trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckResultRecord {
    /// Domain identifier (e.g. `"code"`, `"docs"`).
    pub domain: String,
    /// Check identifier within the domain (e.g. `"seam"`, `"links"`).
    pub check: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable check summary.
    pub summary: String,
    /// Diagnostic lines when `passed == false`.
    #[serde(default)]
    pub diagnostics: Vec<String>,
    /// Optional live salt folded into `execution_digest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_digest_salt: Option<String>,
}

/// Compact metadata summary for a single named item inside an attached specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachedSpecItemSummary {
    /// Singular Markdown heading token (`### <item>: <ref>`).
    pub item: String,
    /// Unique item reference within the attached spec.
    pub item_ref: String,
    /// Lifecycle status string (`planned`, `active`, `completed`, `cancelled`).
    pub status: String,
}

/// Snapshot of a primary or attached secondary specification bound to a `RunRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachedSpecRecord {
    /// Specification ID (e.g. `AC-006`, `RES-001`, `MEM-001`).
    pub spec_id: String,
    /// Specification type selector (resolved `spec:` or collection/doc type).
    pub spec_type: String,
    /// Owning collection key (`specs:` registry key), if matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Normalized workspace-relative path to the specification file.
    pub path: String,
    /// Human-readable specification title.
    pub title: String,
    /// Lowercase hex SHA-256 digest of the specification bytes.
    pub digest: String,
    /// Compact schema-backed frontmatter field summaries (`SpecDocument.fields`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// Named non-task items (`items` collections) declared in the attached spec.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, Vec<AttachedSpecItemSummary>>,
}

/// Row representation for the `runs` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunRecord {
    /// Immutable run identifier.
    pub run_id: RunId,
    /// Unique human-readable run name within `workspace`.
    pub run_name: RunName,
    /// Spec collection name (`milestones`, `plans`, etc.) or `None` for ephemeral runs.
    pub collection: Option<String>,
    /// Canonical primary workspace path.
    pub workspace: String,
    /// Resolved workflow identifier.
    pub profile_id: String,
    /// Canonical resolved `ResolvedWorkflow` JSON snapshot (omitted in compact node `show` responses unless requested).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile_json: String,
    /// Relative path to source spec file (`None` when `is_ephemeral == true`).
    pub spec_path: Option<String>,
    /// Source spec ID (e.g. `AC-005`) preserved without truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// SHA-256 digest of the source spec content.
    pub spec_digest: String,
    /// Attached primary/secondary specifications bound to this run (`attached_specs_json`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_specs: Vec<AttachedSpecRecord>,
    /// Whether this run was created ephemerally via `--goal`.
    pub is_ephemeral: bool,
    /// Run-level isolation mode (`in_place` or `worktree`).
    pub isolation: IsolationMode,
    /// Target integration base branch/ref name.
    pub target_base: Option<String>,
    /// Target integration base commit OID captured at run start.
    pub target_base_commit: Option<CommitOid>,
    /// Run branch name when using worktree isolation.
    pub branch: Option<String>,
    /// Run worktree path when active.
    pub worktree: Option<String>,
    /// Root W3C 32-hex trace ID for this run.
    pub trace_id: String,
    /// Current run lifecycle status.
    pub status: RunStatus,
    /// Cancellation reason when `status == Cancelled`.
    pub cancellation_reason: Option<String>,
    /// Number of root-level fail-route cycles executed so far.
    pub root_cycle_count: u32,
    /// Optimistic concurrency generation counter (`>= 1`).
    pub generation: u64,
    /// UTC ISO-8601 timestamp when the run was created.
    pub created_at: String,
    /// UTC ISO-8601 timestamp when the run completed/cancelled.
    pub completed_at: Option<String>,
}

/// Row representation for the `gate_nodes` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateNodeRecord {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Canonical node ID (`root:<g>`, `tasks#<idx>`, `task:<t>`, `task:<t>:<g>`).
    pub node_id: String,
    /// Parent node ID (`None` for top-level root nodes/containers).
    pub parent_id: Option<String>,
    /// Structural node kind.
    pub kind: NodeKind,
    /// Owning task reference (`None` for `Container` and `RootGate`).
    pub task_ref: Option<TaskRef>,
    /// Gate identifier (`"__task__"` for `Task`, block key for `Container`).
    pub gate_id: String,
    /// Deterministic topological/declaration sequence order.
    pub seq_order: u32,
    /// Current node status.
    pub status: NodeStatus,
    /// Optimistic concurrency node version (`>= 1`).
    pub node_version: u64,
    /// Effective isolation mode for this node.
    pub isolation: IsolationMode,
    /// Rejection/failure attempt counter.
    pub attempt: u32,
    /// Container fail-route cycle counter.
    pub cycle_count: u32,
    /// Maximum attempts/cycles allowed before transitioning to `Escalated`.
    pub max_attempts: u32,
    /// Monotonic claim revision (`0` if never claimed).
    pub claim_revision: u32,
    /// Monotonic contract/scope revision (`>= 1`).
    pub contract_revision: u32,
    /// Active or last claiming worker ID.
    pub worker_id: Option<WorkerId>,
    /// Active or last claiming session fingerprint (`host_id:pid:start_time`).
    pub session_fingerprint: Option<String>,
    /// Branch associated with this node/task worktree.
    pub branch: Option<String>,
    /// Worktree directory associated with this node/task.
    pub worktree: Option<String>,
    /// Baseline commit OID for scope diffing and scoped commits.
    pub baseline_commit: Option<CommitOid>,
    /// Candidate tree OID captured when claimed.
    pub claim_tree: Option<TreeOid>,
    /// Candidate tree OID captured when accepted.
    pub accepted_tree: Option<TreeOid>,
    /// Canonical `GateDef` digest at the time the node was accepted.
    pub accepted_definition_digest: Option<String>,
    /// Effective tags for `when:` evaluation.
    pub tags: Vec<String>,
    /// Applicable and verified invariants.
    pub invariants: InvariantsState,
    /// Declared write/candidate scope globs/paths.
    pub scope: Vec<String>,
    /// Approved scope extensions with reasons.
    pub extended_scope: Vec<ExtendedScopeEntry>,
    /// Active W3C 16-hex span ID for the current claim.
    pub active_span_id: Option<String>,
}

/// Row representation for the `task_dependencies` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskDependencyRecord {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Dependent task reference inside `run_id`.
    pub task_ref: TaskRef,
    /// Target workspace path/alias of the prerequisite task.
    pub depends_on_workspace: String,
    /// Prerequisite run identifier.
    pub depends_on_run_id: RunId,
    /// Prerequisite task reference.
    pub depends_on_ref: TaskRef,
    /// Whether this dependency edge has been satisfied (`completed`).
    pub satisfied: bool,
}

/// Row representation for the `subtasks` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubtaskRecord {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Owning task container `node_id` (`task:<task_ref>`).
    pub node_id: String,
    /// Subtask identifier within the task.
    pub subtask_ref: SubtaskRef,
    /// Declaration order index.
    pub seq_order: u32,
    /// Human-readable subtask title.
    pub title: String,
    /// Subtask lifecycle status.
    pub status: SubtaskStatus,
    /// Required non-empty completion/cancellation evidence.
    pub evidence: Option<String>,
    /// UTC ISO-8601 update timestamp.
    pub updated_at: String,
}

/// Row representation for the `gate_claims` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateClaimRecord {
    /// Surrogate database primary key (`None` before insertion).
    pub claim_id: Option<i64>,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Claimed node ID.
    pub node_id: String,
    /// Claim revision (`>= 1`).
    pub claim_revision: u32,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Claiming session fingerprint (`host_id:pid:start_time`).
    pub session_fingerprint: Option<String>,
    /// Worktree path where the claim executes.
    pub worktree: String,
    /// W3C 16-hex span ID created for this claim.
    pub span_id: String,
    /// Candidate tree OID captured at claim admission.
    pub claim_tree: TreeOid,
    /// UTC ISO-8601 claim start timestamp.
    pub claimed_at: String,
    /// UTC ISO-8601 claim end timestamp (`None` while `outcome == Active`).
    pub ended_at: Option<String>,
    /// Current claim outcome.
    pub outcome: ClaimOutcome,
}

/// Row representation for the `gate_traces` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateTraceRecord {
    /// Surrogate database primary key (`None` before insertion).
    pub trace_row_id: Option<i64>,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Target node ID.
    pub node_id: String,
    /// Claim revision under which this trace ran (`>= 1`).
    pub claim_revision: u32,
    /// Contract revision under which this trace ran (`>= 1`).
    pub contract_revision: u32,
    /// RFC-8785 SHA-256 execution fingerprint.
    pub execution_digest: String,
    /// Idempotency key for mutating `call:` actions.
    pub idempotency_key: Option<String>,
    /// Configured command slot ID if a command ran.
    pub command_id: Option<String>,
    /// Rendered command line executed (if any).
    pub command_line: Option<String>,
    /// Domain action identifier `<domain>.<action>` if `call:` ran.
    pub action_id: Option<String>,
    /// Optional `--test` target passed to verification.
    pub test_target: Option<String>,
    /// Trace execution status.
    pub status: TraceStatus,
    /// Process exit code if a command ran.
    pub exit_code: Option<i32>,
    /// Names of individual passing tests observed.
    pub passed_tests: Vec<String>,
    /// Names of individual failing tests observed.
    pub failed_tests: Vec<String>,
    /// Domain check results observed during this trace.
    pub check_results: Vec<CheckResultRecord>,
    /// Candidate tree OID before command/check execution.
    pub candidate_tree_before: TreeOid,
    /// Candidate tree OID after execution (`None` while `status == Running`).
    pub candidate_tree_after: Option<TreeOid>,
    /// Relative log path inside `.acdd/run/<run_id>/`.
    pub log_relpath: String,
    /// UTC ISO-8601 start timestamp.
    pub started_at: String,
    /// UTC ISO-8601 finish timestamp (`None` while `status == Running`).
    pub finished_at: Option<String>,
}

/// Row representation for the `gate_artifacts` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateArtifactRecord {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Target node ID.
    pub node_id: String,
    /// Claim revision associated with the artifact (`>= 0`).
    pub claim_revision: u32,
    /// Artifact category.
    pub kind: ArtifactKind,
    /// Canonical JSON payload string.
    pub payload_json: String,
    /// Optional Git commit OID.
    pub snapshot_commit: Option<CommitOid>,
    /// UTC ISO-8601 creation timestamp.
    pub created_at: String,
}

/// Row representation for the `blackboard` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardRecord {
    /// Surrogate database primary key (`None` before insertion).
    pub id: Option<i64>,
    /// Owning run identifier.
    pub run_id: RunId,
    /// Optional source node ID.
    pub node_id: Option<String>,
    /// Optional targeted recipient node ID.
    pub recipient_node_id: Option<String>,
    /// Optional subtask reference.
    pub subtask_ref: Option<SubtaskRef>,
    /// Optional gate cursor/label.
    pub gate: Option<String>,
    /// Author identifier (`worker_id` or role/user).
    pub author: String,
    /// Blackboard topic category.
    pub topic: BlackboardTopic,
    /// Blackboard item status.
    pub status: BlackboardStatus,
    /// Message text or JSON payload.
    pub payload: String,
    /// UTC ISO-8601 creation timestamp.
    pub created_at: String,
}

/// Row representation for the `run_tombstones` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunTombstoneRecord {
    /// Closed run identifier.
    pub run_id: RunId,
    /// Closed run name.
    pub run_name: RunName,
    /// Collection name (`None` if ephemeral).
    pub collection: Option<String>,
    /// Canonical primary workspace path.
    pub workspace: String,
    /// Specification document ID (e.g. `AC-001`) if present.
    pub spec_id: Option<String>,
    /// Run/spec title.
    pub title: String,
    /// Profile identifier used by the run.
    pub profile_id: String,
    /// Whether the run was ephemeral (`--goal`).
    pub is_ephemeral: bool,
    /// Final run generation counter at closure.
    pub generation: u64,
    /// Terminal disposition (`completed` or `cleared`).
    pub terminal_status: TerminalStatus,
    /// Target integration base branch/ref.
    pub target_base: Option<String>,
    /// Target integration base commit OID.
    pub target_base_commit: Option<CommitOid>,
    /// Archive file path relative to workspace (`None` if ephemeral or cleared).
    pub archive_path: Option<String>,
    /// Closing commit OID if committed.
    pub closed_commit: Option<CommitOid>,
    /// Whether post-commit worktree/ref/spec cleanup is still pending.
    pub cleanup_pending: bool,
    /// Pending cleanup items manifest (worktrees, branches, snapshot refs, old spec path).
    pub cleanup_manifest: Vec<ArchiveCleanupEntry>,
    /// Canonical JSON closure summary payload.
    pub summary_json: String,
    /// Canonical JSON closure metrics payload.
    pub metrics_json: String,
    /// UTC ISO-8601 closure timestamp.
    pub closed_at: String,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Cleanup item recorded inside `run_tombstones.cleanup_manifest_json` for crash-safe resumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCleanupEntry {
    /// Resource kind (`spec_file`, `worktree`, `branch`, `snapshot_ref`).
    pub kind: String,
    /// Path or Git ref/branch name to remove.
    pub path_or_ref: String,
    /// Optional linked workspace root if different from primary workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Whether removal was explicitly authorized with `--force` (defaults to `false`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

/// Complete hydrated snapshot of an active `Run` and all its child rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunGraphSnapshot {
    /// Top-level `runs` row.
    pub run: RunRecord,
    /// All `gate_nodes` rows belonging to `run`.
    pub nodes: Vec<GateNodeRecord>,
    /// All `task_dependencies` rows belonging to `run`.
    pub dependencies: Vec<TaskDependencyRecord>,
    /// All `subtasks` rows belonging to `run`.
    pub subtasks: Vec<SubtaskRecord>,
    /// All `gate_claims` rows belonging to `run`.
    pub claims: Vec<GateClaimRecord>,
    /// All `gate_traces` rows belonging to `run`.
    pub traces: Vec<GateTraceRecord>,
    /// All `gate_artifacts` rows belonging to `run`.
    pub artifacts: Vec<GateArtifactRecord>,
}

/// Optimistic concurrency check on a single `gate_nodes` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeVersionExpectation {
    /// Target `node_id`.
    pub node_id: String,
    /// Expected `gate_nodes.node_version` prior to mutation.
    pub expected_version: u64,
}

/// Predecessor accepted-tree invariant checked atomically or prior to transition commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PredecessorTreeExpectation {
    /// Predecessor `node_id`.
    pub node_id: String,
    /// Expected `accepted_tree` recorded on the predecessor.
    pub expected_accepted_tree: TreeOid,
}

/// Storage-level CAS preconditions produced by the pure state machine and enforced by `StateTx`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoragePreconditions {
    /// Expected `runs.generation` before committing the transaction.
    pub expected_run_generation: u64,
    /// Expected `node_version` for every node read or mutated by the plan.
    pub expected_node_versions: Vec<NodeVersionExpectation>,
    /// Expected active `claim_revision` if the transition operates on a claim.
    pub expected_claim_revision: Option<u32>,
    /// Expected `contract_revision` if the transition validates contract/scope freshness.
    pub expected_contract_revision: Option<u32>,
    /// Expected active `worker_id` if the transition requires an active claim owner.
    pub expected_worker_id: Option<WorkerId>,
    /// Expected predecessor `accepted_tree` values from `need:`.
    pub required_predecessor_trees: Vec<PredecessorTreeExpectation>,
}

/// Partial field updates applied atomically to a `gate_nodes` row inside `apply_transition_plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeStatusPatch {
    /// Target `node_id` to update.
    pub node_id: String,
    /// New node status.
    pub new_status: NodeStatus,
    /// Updated `attempt` counter if changed.
    pub attempt: Option<u32>,
    /// Updated `cycle_count` counter if changed.
    pub cycle_count: Option<u32>,
    /// Updated `claim_revision` if changed.
    pub claim_revision: Option<u32>,
    /// Updated `contract_revision` if changed.
    pub contract_revision: Option<u32>,
    /// Updated `worker_id` (`Some(None)` clears it).
    pub worker_id: Option<Option<WorkerId>>,
    /// Updated `session_fingerprint` (`Some(None)` clears it).
    pub session_fingerprint: Option<Option<String>>,
    /// Updated `branch` (`Some(None)` clears it).
    pub branch: Option<Option<String>>,
    /// Updated `worktree` (`Some(None)` clears it).
    pub worktree: Option<Option<String>>,
    /// Updated `baseline_commit` (`Some(None)` clears it).
    pub baseline_commit: Option<Option<CommitOid>>,
    /// Updated `claim_tree` (`Some(None)` clears it).
    pub claim_tree: Option<Option<TreeOid>>,
    /// Updated `accepted_tree` (`Some(None)` clears it).
    pub accepted_tree: Option<Option<TreeOid>>,
    /// Updated `accepted_definition_digest` (`Some(None)` clears it).
    pub accepted_definition_digest: Option<Option<String>>,
    /// Updated `tags` if changed (e.g., on `sync`).
    pub tags: Option<Vec<String>>,
    /// Updated `invariants` if changed.
    pub invariants: Option<InvariantsState>,
    /// Updated `scope` if changed (e.g., on `sync`).
    pub scope: Option<Vec<String>>,
    /// Updated `extended_scope` if changed.
    pub extended_scope: Option<Vec<ExtendedScopeEntry>>,
    /// Updated `active_span_id` (`Some(None)` clears it).
    pub active_span_id: Option<Option<String>>,
}

impl NodeStatusPatch {
    /// Construct a minimal patch that transitions `node_id` to `new_status` without changing other fields.
    pub fn status_only(node_id: impl Into<String>, new_status: NodeStatus) -> Self {
        Self {
            node_id: node_id.into(),
            new_status,
            attempt: None,
            cycle_count: None,
            claim_revision: None,
            contract_revision: None,
            worker_id: None,
            session_fingerprint: None,
            branch: None,
            worktree: None,
            baseline_commit: None,
            claim_tree: None,
            accepted_tree: None,
            accepted_definition_digest: None,
            tags: None,
            invariants: None,
            scope: None,
            extended_scope: None,
            active_span_id: None,
        }
    }
}

/// Update to `task_dependencies.satisfied` when a prerequisite task completes or reopens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DependencySatisfiedPatch {
    /// Dependent task reference (or prerequisite reference depending on direction; targets `depends_on_ref` inside the run).
    pub depends_on_ref: TaskRef,
    /// Optional specific dependent `task_ref` (`None` updates all dependents of `depends_on_ref` inside this run).
    pub dependent_task_ref: Option<TaskRef>,
    /// New `satisfied` flag.
    pub satisfied: bool,
}

/// Outcome update applied to an existing `gate_claims` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimOutcomePatch {
    /// Target `node_id`.
    pub node_id: String,
    /// Target `claim_revision`.
    pub claim_revision: u32,
    /// Terminal outcome for this claim revision.
    pub outcome: ClaimOutcome,
    /// UTC ISO-8601 end timestamp.
    pub ended_at: String,
}

/// Status update applied to an existing `blackboard` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardStatusPatch {
    /// Target `blackboard.id`.
    pub id: i64,
    /// New blackboard status.
    pub status: BlackboardStatus,
}

/// Pure transition plan computed by `acdd_state::machine` and persisted atomically by `StateTx`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransitionPlan {
    /// Optimistic concurrency preconditions checked before mutating state.
    pub preconditions: StoragePreconditions,
    /// Whether `runs.generation` must be incremented by `1`.
    pub bump_run_generation: bool,
    /// Optional updated `runs.status`.
    pub new_run_status: Option<RunStatus>,
    /// Optional updated `runs.root_cycle_count`.
    pub new_root_cycle_count: Option<u32>,
    /// Optional updated `runs.completed_at`.
    pub completed_at: Option<String>,
    /// Node updates applied within the same SQL transaction.
    pub node_patches: Vec<NodeStatusPatch>,
    /// Dependency satisfaction updates applied within the same SQL transaction.
    pub dependency_patches: Vec<DependencySatisfiedPatch>,
    /// Claim outcome updates applied within the same SQL transaction.
    pub claim_patches: Vec<ClaimOutcomePatch>,
    /// New artifacts inserted within the same SQL transaction.
    pub new_artifacts: Vec<GateArtifactRecord>,
    /// New blackboard entries inserted within the same SQL transaction.
    pub new_blackboard: Vec<BlackboardRecord>,
    /// Existing blackboard status updates applied within the same SQL transaction.
    pub blackboard_patches: Vec<BlackboardStatusPatch>,
    /// Subtasks inserted or updated within the same SQL transaction.
    pub new_subtasks: Vec<SubtaskRecord>,
    /// Whether `after_commit_maintenance` should trigger a SQLite backup on commit.
    pub significant_transition: bool,
}

/// Validated imperative-shell observations fed into the pure state machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidatedObservations {
    /// Current working tree OID across `is_candidate_included` paths.
    pub current_tree: Option<TreeOid>,
    /// Current working tree OID restricted to the active node's `container_scope`.
    pub container_tree: Option<TreeOid>,
    /// Candidate changed paths relative to the worktree.
    pub changed_paths: Vec<String>,
    /// Changed paths that violated `is_agent_writable`.
    pub out_of_scope_paths: Vec<String>,
    /// Tasks whose scoped deltas are missing from `run_worktree` (`TASK_NOT_INTEGRATED`).
    pub unintegrated_tasks: Vec<TaskRef>,
    /// External cross-workspace dependencies confirmed completed (`(workspace, run_id, task_ref)`).
    pub external_satisfied_deps: Vec<(String, RunId, TaskRef)>,
    /// Whether external dependency observations replace persisted satisfaction flags.
    #[serde(default)]
    pub external_deps_authoritative: bool,
    /// Optional workspace path policy resolved by the shell (incorporating custom `state_db`/`code_db` overrides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_policy: Option<acdd_core::path_policy::WorkspacePathPolicy>,
    /// Current UTC ISO-8601 timestamp supplied by the shell.
    pub now_utc: String,
}

/// Input parameters for inserting a new `gate_claims` row via `StateTx::admit_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimInsertInput {
    /// Target `node_id`.
    pub node_id: String,
    /// New `claim_revision` (`previous + 1`).
    pub claim_revision: u32,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Claiming session fingerprint (`host_id:pid:start_time`).
    pub session_fingerprint: Option<String>,
    /// Assigned worktree path.
    pub worktree: String,
    /// Assigned branch name (if worktree-isolated).
    pub branch: Option<String>,
    /// Baseline commit OID recorded at claim.
    pub baseline_commit: Option<CommitOid>,
    /// W3C 16-hex span ID generated for this claim.
    pub span_id: String,
    /// Candidate tree OID captured at claim admission.
    pub claim_tree: TreeOid,
    /// UTC ISO-8601 timestamp when the claim was admitted.
    pub claimed_at: String,
}

/// Input parameters for inserting a `running` trace via `StateTx::start_trace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceStartInput {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Target `node_id`.
    pub node_id: String,
    /// Active claim revision (`>= 1`).
    pub claim_revision: u32,
    /// Active contract revision (`>= 1`).
    pub contract_revision: u32,
    /// RFC-8785 SHA-256 execution digest.
    pub execution_digest: String,
    /// Idempotency key for mutating `call:` actions.
    pub idempotency_key: Option<String>,
    /// Configured command slot ID.
    pub command_id: Option<String>,
    /// Rendered command line.
    pub command_line: Option<String>,
    /// Domain action identifier `<domain>.<action>`.
    pub action_id: Option<String>,
    /// Optional `--test` target.
    pub test_target: Option<String>,
    /// Candidate tree OID captured before execution.
    pub candidate_tree_before: TreeOid,
    /// Relative log file path under `.acdd/run/<run_id>/`.
    pub log_relpath: String,
    /// UTC ISO-8601 start timestamp.
    pub started_at: String,
}

/// Input parameters for finalizing a `gate_traces` row via `StateTx::finish_trace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceFinishInput {
    /// Primary key of the running trace row.
    pub trace_row_id: i64,
    /// Terminal trace status (must not be `Running`).
    pub status: TraceStatus,
    /// Process exit code if a command ran.
    pub exit_code: Option<i32>,
    /// Passing test target names.
    pub passed_tests: Vec<String>,
    /// Failing test target names.
    pub failed_tests: Vec<String>,
    /// Domain check results recorded during the trace.
    pub check_results: Vec<CheckResultRecord>,
    /// Candidate tree OID captured after execution.
    pub candidate_tree_after: Option<TreeOid>,
    /// UTC ISO-8601 finish timestamp.
    pub finished_at: String,
}

/// Paginated query parameters for `StateReader::list_runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListRunsReq {
    /// Optional workspace filter.
    pub workspace: Option<String>,
    /// Optional active/completed/cancelled status filter.
    pub status: Option<RunStatus>,
    /// Whether to include closed runs from `run_tombstones`.
    pub include_tombstones: bool,
    /// Page size (`1..=100`, default `50`).
    pub limit: usize,
    /// Zero-based page offset.
    pub offset: usize,
    /// Optional expected `runs.generation` for stable pagination (`BackendConflict` on drift).
    pub generation: Option<u64>,
}

impl Default for ListRunsReq {
    fn default() -> Self {
        Self {
            workspace: None,
            status: None,
            include_tombstones: false,
            limit: 50,
            offset: 0,
            generation: None,
        }
    }
}

/// Paginated response returned by `StateReader::list_runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListRunsPage {
    /// Active/persisted runs in the requested page.
    pub runs: Vec<RunRecord>,
    /// Closed run tombstones if `include_tombstones == true`.
    pub tombstones: Vec<RunTombstoneRecord>,
    /// Total matching active/persisted run count.
    pub total_runs: usize,
    /// Total matching tombstone count.
    pub total_tombstones: usize,
    /// Applied page limit.
    pub limit: usize,
    /// Applied page offset.
    pub offset: usize,
}

/// Paginated query parameters for `StateReader::list_blackboard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardListReq {
    /// Optional filter by source `node_id`.
    pub node_id: Option<String>,
    /// Optional filter by `recipient_node_id`.
    pub recipient_node_id: Option<String>,
    /// Optional filter by topic.
    pub topic: Option<BlackboardTopic>,
    /// Optional filter by item status.
    pub status: Option<BlackboardStatus>,
    /// Page size (`1..=100`, default `50`).
    pub limit: usize,
    /// Zero-based page offset.
    pub offset: usize,
    /// Optional expected `runs.generation` for stable pagination (`BackendConflict` on drift).
    pub generation: Option<u64>,
}

impl Default for BlackboardListReq {
    fn default() -> Self {
        Self {
            node_id: None,
            recipient_node_id: None,
            topic: None,
            status: None,
            limit: 50,
            offset: 0,
            generation: None,
        }
    }
}

/// Paginated response returned by `StateReader::list_blackboard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlackboardPage {
    /// Blackboard items in the requested page.
    pub items: Vec<BlackboardRecord>,
    /// Total matching items for the run/filter.
    pub total: usize,
    /// Run generation observed when reading the page.
    pub generation: u64,
    /// Applied page limit.
    pub limit: usize,
    /// Applied page offset.
    pub offset: usize,
}

/// Read-only aggregate metrics snapshot returned by `StateReader::observability_snapshot`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservabilitySnapshot {
    /// Number of active runs in `runs`.
    pub active_runs: usize,
    /// Number of completed runs in `run_tombstones` (plus any completed rows in `runs`).
    pub completed_runs: usize,
    /// Number of cleared runs in `run_tombstones`.
    pub cleared_runs: usize,
    /// Gate node counts keyed by `NodeStatus::as_str()`.
    pub nodes_by_status: BTreeMap<String, usize>,
    /// Number of active claims (`outcome == 'active'`).
    pub active_claims: usize,
    /// Trace counts keyed by `TraceStatus::as_str()`.
    pub traces_by_status: BTreeMap<String, usize>,
    /// Number of open blackboard items (`status == 'open'`).
    pub open_blackboard_items: usize,
}

/// Compact summary digest of a completed/cleared run stored in `run_tombstones`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompletedRunDigest {
    /// Closed run identifier.
    pub run_id: RunId,
    /// Closed run name.
    pub run_name: RunName,
    /// Collection name (`None` if ephemeral).
    pub collection: Option<String>,
    /// Specification document ID if applicable.
    pub spec_id: Option<String>,
    /// Run/spec title.
    pub title: String,
    /// Terminal disposition (`completed` or `cleared`).
    pub terminal_status: TerminalStatus,
    /// Archive file path relative to workspace (`None` if ephemeral or cleared).
    pub archive_path: Option<String>,
    /// Closing commit OID if committed.
    pub closed_commit: Option<CommitOid>,
    /// UTC ISO-8601 closure timestamp.
    pub closed_at: String,
}

/// Active `in_progress` gate claim summary returned by `StateReader::active_worker_claims`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorkerClaim {
    /// Active run identifier owning the claimed gate.
    pub run_id: RunId,
    /// Claimed gate node identifier (`root:<gate>` or `task:<ref>:<gate>`).
    pub node_id: String,
    /// Effective worktree path resolved from node, parent task, or run (`None` when unset).
    pub worktree: Option<String>,
}
