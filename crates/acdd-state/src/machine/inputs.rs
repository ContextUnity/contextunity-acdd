//! Pure input and output models consumed and produced by the `acdd_state::machine` core.

use crate::types::{
    ExtendedScopeEntry, GateClaimRecord, GateNodeRecord, IsolationMode, RejectionFindings,
    ReviewEvidence, SaveAction, StoragePreconditions, SubmitAction, SubtaskStatus,
    TaskDependencyRecord, TransitionPlan, WorktreeLifecycle,
};
use acdd_core::errors::StateError;
use acdd_core::ids::{CommitOid, GateId, NodeCursor, RunId, SubtaskRef, TaskRef, TreeOid, WorkerId};
use acdd_core::schema::SchemaDef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Pure `git:` policy projection supplied to the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineGitPolicy {
    /// Workspace isolation strategy (`in_place` or `worktree`).
    pub isolation: IsolationMode,
    /// Worktree lifecycle ownership (`manual` or `managed`).
    pub worktrees: WorktreeLifecycle,
}

impl Default for MachineGitPolicy {
    fn default() -> Self {
        Self {
            isolation: IsolationMode::Worktree,
            worktrees: WorktreeLifecycle::Manual,
        }
    }
}

/// Pure `save:` policy projection for a single gate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineSavePolicy {
    /// Persistence action taken on gate acceptance (`none` or `commit`).
    pub post: SaveAction,
    /// Whether the working tree must remain identical to `claim_tree` at submission.
    pub immutable: bool,
    /// Whether accepting this root gate archives the run specification.
    pub archive: bool,
    /// Whether accepting this root gate prunes active run tables into `run_tombstones`.
    pub prune: bool,
    /// Optional workspace-relative report path rendered after gate acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
}

/// Pure proof kind configured on a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineProofKind {
    /// No proof required beyond submission.
    None,
    /// Zero-exit command/test verification proof.
    Command,
    /// DomainProvider action call proof.
    Call,
    /// Independent structured contour review proof.
    Review,
    /// Custom schema-validated proof catalog entry.
    CustomSchema(String),
}

/// Domain check reference and parameters configured on a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineCheckDef {
    /// Qualified `<domain>.<check>` identifier.
    pub qualified_name: String,
    /// JSON parameters validated against the domain check schema.
    pub params: Value,
}

/// Pure gate definition used by the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineGateDef {
    /// Gate identifier within its container.
    pub id: GateId,
    /// Optional role name responsible for executing this gate.
    pub role: Option<String>,
    /// Proof policy required for gate acceptance.
    pub proof: MachineProofKind,
    /// Optional configured command slot ID.
    pub command: Option<String>,
    /// Optional qualified `<domain>.<action>` call.
    pub call: Option<String>,
    /// JSON `with:` parameters for `call:`.
    pub with_params: Value,
    /// Domain checks evaluated during `verify`.
    pub checks: Vec<MachineCheckDef>,
    /// Required review contour keys for `proof: review`.
    pub contours: Vec<String>,
    /// Gate selectors whose closing workers must differ from this gate's worker.
    pub independent_from: Vec<acdd_core::ids::GateSelector>,
    /// Predecessor gate selectors whose `accepted_tree` must match at submission.
    pub need: Vec<acdd_core::ids::GateSelector>,
    /// Optional successor gate ID on `pass`.
    pub pass: Option<String>,
    /// Optional target gate ID on `reject` / mechanical failure.
    pub fail: Option<String>,
    /// Optional target gate ID on `architectural_drift` rejection.
    pub fail_drift: Option<String>,
    /// Maximum attempts or cycles allowed before transitioning to `Escalated` (`>= 1`).
    pub max_attempts: u32,
    /// Whether passing this gate closes its owning task/run container with a receipt.
    pub receipt: bool,
    /// Gate persistence policy.
    pub save: MachineSavePolicy,
    /// Optional explicit scope globs (for root gates such as `polish`).
    pub scope: Vec<String>,
    /// Optional boolean `when:` tag expression (only valid on task gates).
    pub when: Option<String>,
    /// Child gates if this gate is a structural root container (e.g. `blueprint_synthesis`).
    pub children: Vec<MachineGateDef>,
    /// Canonical SHA-256 digest of this gate definition.
    pub definition_digest: String,
}

impl MachineGateDef {
    /// Construct a minimal `MachineGateDef` with sensible defaults for testing or builders.
    #[cfg(any(test, feature = "test-support"))]
    pub fn minimal(id: GateId) -> Self {
        let digest = format!("def:{id}");
        Self {
            id,
            role: None,
            proof: MachineProofKind::None,
            command: None,
            call: None,
            with_params: Value::Object(Default::default()),
            checks: Vec::new(),
            contours: Vec::new(),
            independent_from: Vec::new(),
            need: Vec::new(),
            pass: None,
            fail: None,
            fail_drift: None,
            max_attempts: 3,
            receipt: false,
            save: MachineSavePolicy::default(),
            scope: Vec::new(),
            when: None,
            children: Vec::new(),
            definition_digest: digest,
        }
    }
}

/// `- tasks:` block inside a profile graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTasksBlockDef {
    /// Optional boolean `when:` tag expression selecting tasks for this block.
    pub when: Option<String>,
    /// Ordered gate definitions instantiated for each matching task.
    pub gates: Vec<MachineGateDef>,
}

/// Top-level element in a profile's `gates:` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineGraphItem {
    /// Root-level gate or container of root gates.
    Gate(Box<MachineGateDef>),
    /// Task fan-out block instantiated per matching task.
    TasksBlock(MachineTasksBlockDef),
}

/// Pure resolved profile projection consumed by the state machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineProfileInput {
    /// Profile identifier.
    pub profile_id: String,
    /// Canonical resolved profile JSON persisted on `runs.profile_json`.
    pub profile_json: String,
    /// Effective `git:` policy.
    pub git: MachineGitPolicy,
    /// Default terminal root gate save action.
    pub default_save: SaveAction,
    /// Project-level protected paths forbidden from agent writes.
    pub protected_paths: Vec<String>,
    /// Allowed tag vocabulary (`"default"` is always implicitly permitted).
    pub tags: BTreeSet<String>,
    /// Named custom proof JSON schemas from `profile.proofs`.
    pub proof_schemas: BTreeMap<String, SchemaDef>,
    /// Ordered top-level root gates and `- tasks:` blocks.
    pub gates: Vec<MachineGraphItem>,
}

/// Pure task dependency specification input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTaskDep {
    /// Target workspace path/alias of the prerequisite task.
    pub depends_on_workspace: String,
    /// Prerequisite run ID (equals the current run ID for intra-run dependencies).
    pub depends_on_run_id: RunId,
    /// Prerequisite task reference.
    pub depends_on_ref: TaskRef,
}

/// Pure subtask specification input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSubtaskInput {
    /// Subtask identifier within the task.
    pub subtask_ref: SubtaskRef,
    /// Human-readable subtask title.
    pub title: String,
    /// Initial or synced subtask status.
    pub status: SubtaskStatus,
    /// Required evidence when `status` is `Completed` or `Cancelled`.
    pub evidence: Option<String>,
}

/// Pure task specification input for graph construction and `sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTaskSpec {
    /// Unique task reference within the run.
    pub task_ref: TaskRef,
    /// Task tags (empty defaults to `["default"]`).
    pub tags: Vec<String>,
    /// Declared write scope globs/paths.
    pub scope: Vec<String>,
    /// Previously admitted extended scope entries.
    pub extended_scope: Vec<ExtendedScopeEntry>,
    /// Applicable invariant identifiers.
    pub invariants: Vec<String>,
    /// Task prerequisites.
    pub depends_on: Vec<MachineTaskDep>,
    /// Task subtasks.
    pub subtasks: Vec<MachineSubtaskInput>,
}

/// Specification snapshot fed into initial graph creation or `plan_sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpecInput {
    /// Tasks currently declared in the spec document.
    pub tasks: Vec<MachineTaskSpec>,
    /// SHA-256 digest of the spec source text.
    pub spec_digest: String,
}

/// Input parameters for constructing an initial `RunGraphSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildGraphInput {
    /// New immutable run identifier.
    pub run_id: RunId,
    /// Human-readable run name.
    pub run_name: acdd_core::ids::RunName,
    /// Collection name (`None` for ephemeral runs).
    pub collection: Option<String>,
    /// Canonical primary workspace path.
    pub workspace: String,
    /// Relative path to spec file (`None` when `is_ephemeral == true`).
    pub spec_path: Option<String>,
    /// Whether this is an ephemeral `--goal` run.
    pub is_ephemeral: bool,
    /// Optional target integration base branch name.
    pub target_base: Option<String>,
    /// Optional target base commit OID.
    pub target_base_commit: Option<CommitOid>,
    /// Optional run branch name.
    pub branch: Option<String>,
    /// Optional run worktree path.
    pub worktree: Option<String>,
    /// Root W3C 32-hex trace ID.
    pub trace_id: String,
    /// UTC ISO-8601 creation timestamp.
    pub created_at: String,
    /// Parsed tasks and spec digest.
    pub spec: MachineSpecInput,
}

/// Input parameters for `admit_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimInput {
    /// Target node cursor.
    pub node_cursor: NodeCursor,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Claiming session fingerprint (`host_id:pid:start_time`).
    pub session_fingerprint: Option<String>,
    /// Worktree path bound to the claim.
    pub worktree: String,
    /// Optional branch bound to the claim.
    pub branch: Option<String>,
    /// Optional baseline commit OID for diffing.
    pub baseline_commit: Option<CommitOid>,
    /// Candidate tree OID observed at claim time.
    pub claim_tree: TreeOid,
    /// W3C 16-hex span ID generated for this claim.
    pub span_id: String,
    /// Whether `--force` takeover was requested.
    pub force: bool,
    /// Whether `--steal-remote` was explicitly authorized by the operator.
    pub steal_remote: bool,
    /// Liveness probe for the previous local claim PID (`Some(false)` means dead).
    pub previous_local_pid_alive: Option<bool>,
}

/// Pure result returned by `admit_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimDecision {
    /// Re-claim from the identical `(worker_id, session_fingerprint, worktree)` is idempotent.
    Idempotent(GateClaimRecord),
    /// New active claim admission with CAS preconditions and node/claim updates.
    Admit {
        /// New claim row to insert via `StateTx::admit_claim`.
        claim: crate::types::ClaimInsertInput,
        /// Equivalent transition plan capturing preconditions and node updates.
        transition: Box<TransitionPlan>,
    },
}

/// Outcome of a `DomainProvider` action execution observed by the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionExecutionOutcome {
    /// Action succeeded cleanly.
    Succeeded,
    /// Action failed cleanly with an error summary.
    Failed(String),
    /// Mutating action crashed mid-execution and could not be recovered via lookup.
    UncertainCrash,
}

/// Execution observations produced by running commands/actions/checks in the imperative shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyExecutionObservation {
    /// Whether the process exceeded `max_command_timeout_seconds`.
    pub timed_out: bool,
    /// Whether the process was aborted externally.
    pub aborted: bool,
    /// Exit code returned by the command, if a command ran.
    pub exit_code: Option<i32>,
    /// Tail of stderr captured for diagnostics.
    pub stderr_tail: String,
    /// Domain check results observed.
    pub check_results: Vec<crate::types::CheckResultRecord>,
    /// Optional `DomainProvider` action outcome.
    pub action_outcome: Option<ActionExecutionOutcome>,
    /// Candidate tree OID captured immediately before execution.
    pub candidate_tree_before: TreeOid,
    /// Candidate tree OID captured immediately after execution.
    pub candidate_tree_after: Option<TreeOid>,
}

/// Input parameters for `evaluate_verify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyInput {
    /// Target claimed node cursor.
    pub node_cursor: NodeCursor,
    /// Optional caller worker identity (enforced when supplied).
    pub worker_id: Option<WorkerId>,
    /// Optional caller claim revision (enforced when supplied).
    pub claim_revision: Option<u32>,
    /// Optional `--test` target supplied by caller.
    pub test_target: Option<String>,
    /// Configured command slot ID.
    pub command_id: Option<String>,
    /// Rendered command line executed.
    pub command_line: Option<String>,
    /// Deterministic resolved command strings hashed into `execution_digest`.
    pub resolved_commands: Vec<String>,
    /// Qualified `<domain>.<action>` identifier if `call:` ran.
    pub action_id: Option<String>,
    /// Optional live index generation salt hashed into `execution_digest`.
    pub index_generation: Option<String>,
    /// Optional live worktree files digest salt hashed into `execution_digest`.
    pub live_files_digest: Option<String>,
    /// Relative log path under `.acdd/run/<run_id>/`.
    pub log_relpath: String,
    /// UTC ISO-8601 start timestamp.
    pub started_at: String,
    /// Shell execution results.
    pub exec_obs: VerifyExecutionObservation,
}

/// Pure result returned by `evaluate_verify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyEvaluation {
    /// Computed RFC-8785 SHA-256 execution digest.
    pub execution_digest: String,
    /// Trace start record fields.
    pub trace_start: crate::types::TraceStartInput,
    /// Trace completion fields (`status != Running`).
    pub trace_finish: crate::types::TraceFinishInput,
    /// CAS storage preconditions verified before recording the trace/transition.
    pub preconditions: StoragePreconditions,
    /// `Ok(())` when verification satisfied all gate contracts, or the exact `StateError`.
    pub outcome: Result<(), StateError>,
    /// Optional follow-up transition plan when a mechanical (`role: None`) gate with `fail:` fails.
    pub failure_transition: Option<TransitionPlan>,
}

/// Input parameters for `evaluate_submit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitInput {
    /// Target claimed node cursor.
    pub node_cursor: NodeCursor,
    /// Submission action (`pass` or `reject`).
    pub action: SubmitAction,
    /// Submitting worker identity.
    pub worker_id: WorkerId,
    /// Optional caller-asserted claim revision.
    pub claim_revision: Option<u32>,
    /// Expected `execution_digest` computed from current gate inputs for trace freshness.
    pub expected_execution_digest: Option<String>,
    /// Typed review evidence for `proof: review` gates.
    pub review_evidence: Option<ReviewEvidence>,
    /// Structured rejection findings required when `action == SubmitAction::Reject`.
    pub rejection_findings: Option<RejectionFindings>,
    /// Custom schema JSON evidence for `proof: CustomSchema(...)` or `proof: blueprint` gates.
    pub custom_evidence: Option<Value>,
    /// Invariant identifiers verified by this submission.
    pub verified_invariants: Vec<String>,
    /// Optional commit OID created by `save.post`.
    pub snapshot_commit: Option<CommitOid>,
    /// Optional closure summary text for receipt rollups.
    pub summary: Option<String>,
}

/// Pure result returned by `evaluate_submit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmitEvaluation {
    /// Transition plan ready for atomic `StateTx::apply_transition_plan`.
    Apply(TransitionPlan),
    /// Predecessor `need:` nodes drifted and must be marked `stale` before failing with `TASK_CANDIDATE_MISMATCH`.
    MarkStaleAndReject {
        /// Predecessor node IDs whose `accepted_tree` drifted from `current_tree`.
        stale_node_ids: Vec<String>,
        /// CAS preconditions for `StateTx::mark_nodes_stale`.
        preconditions: StoragePreconditions,
        /// Canonical `TASK_CANDIDATE_MISMATCH` error returned after marking stale.
        error: StateError,
    },
}

/// Input parameters for `plan_extend_scope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtendScopeInput {
    /// Target task or task-gate cursor.
    pub node_cursor: NodeCursor,
    /// New scope paths or glob patterns to admit.
    pub paths: Vec<String>,
    /// Non-empty justification recorded alongside the extension.
    pub reason: String,
    /// Optional worker identity check.
    pub worker_id: Option<WorkerId>,
}

/// Input parameters for `plan_reopen`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReopenInput {
    /// Target gate or task container cursor to reopen.
    pub node_cursor: NodeCursor,
    /// Whether to cascade-stale `in_progress`/`completed` downstream dependent tasks.
    pub cascade: bool,
    /// Optional reason posted to blackboard / diagnostic context.
    pub reason: Option<String>,
}

/// Input parameters for `plan_sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncInput {
    /// Updated parsed specification tasks and digest.
    pub spec: MachineSpecInput,
    /// Updated profile (used when `rebind_profile == true`).
    pub new_profile: Option<MachineProfileInput>,
    /// Whether to rebind non-completed gates to `new_profile`.
    pub rebind_profile: bool,
}

/// Pure synchronization plan emitted by `plan_sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    /// Atomic patches to existing nodes, dependencies, subtasks, and claims.
    pub transition: TransitionPlan,
    /// Brand-new `gate_nodes` rows introduced by newly added tasks or rebinding.
    pub added_nodes: Vec<GateNodeRecord>,
    /// Brand-new `task_dependencies` rows introduced by sync.
    pub added_dependencies: Vec<TaskDependencyRecord>,
    /// Existing `task_dependencies` rows removed from the spec during sync.
    #[serde(default)]
    pub removed_dependencies: Vec<TaskDependencyRecord>,
    /// Updated `seq_order` values for existing nodes reordered by new gates or tasks.
    #[serde(default)]
    pub seq_order_updates: Vec<(String, u32)>,
    /// New `spec_digest` when updated.
    pub spec_digest: String,
}

/// Unified command enum dispatching into the pure state machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransitionInput {
    /// Claim or re-claim a node.
    Claim(ClaimInput),
    /// Evaluate verification results for a claimed node.
    Verify(VerifyInput),
    /// Submit `pass` or `reject` for a claimed node.
    Submit(SubmitInput),
    /// Extend a task's admitted write scope.
    ExtendScope(ExtendScopeInput),
    /// Reopen a gate or task node.
    Reopen(ReopenInput),
    /// Synchronize spec changes (and optional profile rebind) into active state.
    Sync(SyncInput),
}

/// Unified pure output returned by `evaluate_transition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionOutcome {
    /// Result of `admit_claim`.
    Claim(Box<ClaimDecision>),
    /// Result of `evaluate_verify`.
    Verify(Box<VerifyEvaluation>),
    /// Result of `evaluate_submit`.
    Submit(Box<SubmitEvaluation>),
    /// Result of `plan_extend_scope` or `plan_reopen`.
    Plan(Box<TransitionPlan>),
    /// Result of `plan_sync`.
    Sync(Box<SyncPlan>),
}
