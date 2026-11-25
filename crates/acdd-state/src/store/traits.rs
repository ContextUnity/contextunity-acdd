//! Portable storage traits (`StateBackend`, `StateReader`, `StateTx`) decoupling state logic from SQLite.

use crate::machine::SyncPlan;
use crate::types::{
    ArtifactKind, BlackboardListReq, BlackboardPage, BlackboardRecord, BlackboardStatusPatch,
    ClaimInsertInput, GateArtifactRecord, GateClaimRecord, GateNodeRecord, GateTraceRecord,
    ListRunsPage, ListRunsReq, NodeStatus, ObservabilitySnapshot, RunGraphSnapshot, RunRecord,
    RunTombstoneRecord, StoragePreconditions, SubtaskRecord, TraceFinishInput, TraceStartInput,
    TransitionPlan,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::RunId;

/// Read-only query interface over persisted runs, nodes, claims, traces, artifacts, and tombstones.
///
/// Implementations on disk MUST open read-only (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only = ON`)
/// and return empty/absent results without creating missing database files.
pub trait StateReader: Send + Sync {
    /// Load task-node statuses for all active runs in one read.
    fn load_task_statuses(&self) -> Result<Vec<(RunId, String, NodeStatus)>, StateError>;

    /// Load the complete graph snapshot (`runs`, `gate_nodes`, `task_dependencies`, `subtasks`,
    /// `gate_claims`, `gate_traces`, `gate_artifacts`) for `run_id`.
    fn load_run_graph(&self, run_id: &RunId) -> Result<Option<RunGraphSnapshot>, StateError>;

    /// List runs (and optionally tombstones) with bounded pagination and optional generation pinning.
    fn list_runs(&self, req: &ListRunsReq) -> Result<ListRunsPage, StateError>;

    /// Resolve the single active `Run` bound to `worktree`, if any.
    fn resolve_active_run_by_worktree(
        &self,
        worktree: &str,
    ) -> Result<Option<RunRecord>, StateError>;

    /// Load a single `gate_nodes` row by `(run_id, node_id)`.
    fn load_node(
        &self,
        run_id: &RunId,
        node_id: &str,
    ) -> Result<Option<GateNodeRecord>, StateError>;

    /// Load the most recent `gate_claims` row (`MAX(claim_revision)`) for `(run_id, node_id)`.
    fn latest_claim(
        &self,
        run_id: &RunId,
        node_id: &str,
    ) -> Result<Option<GateClaimRecord>, StateError>;

    /// Load the most recent `gate_traces` row (`MAX(trace_row_id)`) for `(run_id, node_id, claim_revision)`.
    fn latest_trace(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
    ) -> Result<Option<GateTraceRecord>, StateError>;

    /// Load a specific artifact by `(run_id, node_id, claim_revision, kind)`.
    fn load_artifact(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
        kind: ArtifactKind,
    ) -> Result<Option<GateArtifactRecord>, StateError>;

    /// List paginated blackboard entries for `run_id`.
    fn list_blackboard(
        &self,
        run_id: &RunId,
        req: &BlackboardListReq,
    ) -> Result<BlackboardPage, StateError>;

    /// Load a closed run tombstone by `run_id`.
    fn load_tombstone(&self, run_id: &RunId) -> Result<Option<RunTombstoneRecord>, StateError>;

    /// Return the highest sequential spec number recorded in `runs`/`run_tombstones` for `(collection, prefix)`.
    fn max_spec_number(&self, collection: &str, prefix: &str) -> Result<Option<u32>, StateError>;

    /// Compute an aggregate read-only observability snapshot across active and closed runs.
    fn observability_snapshot(&self) -> Result<ObservabilitySnapshot, StateError>;
}

/// Atomic transactional write interface executed inside `StateBackend::exec_tx`.
///
/// Implementations MUST NOT perform external filesystem, Git, or network I/O inside `StateTx` methods.
pub trait StateTx {
    /// Insert a newly created `Run` and all initial `gate_nodes`, `task_dependencies`, and `subtasks`.
    fn insert_run_graph(&mut self, graph: &RunGraphSnapshot) -> Result<(), StateError>;

    /// Admit a new active claim (`gate_claims` + `gate_nodes` update) checking CAS `preconditions`.
    fn admit_claim(
        &mut self,
        run_id: &RunId,
        preconditions: &StoragePreconditions,
        claim: &ClaimInsertInput,
    ) -> Result<GateClaimRecord, StateError>;

    /// Insert a `running` trace row verifying CAS `preconditions`.
    fn start_trace(
        &mut self,
        preconditions: &StoragePreconditions,
        input: &TraceStartInput,
    ) -> Result<GateTraceRecord, StateError>;

    /// Finalize an existing `running` trace row (`status != Running`, non-null `finished_at`).
    fn finish_trace(&mut self, input: &TraceFinishInput) -> Result<(), StateError>;

    /// Mark the specified nodes `stale` under CAS `preconditions` and bump `runs.generation`.
    fn mark_nodes_stale(
        &mut self,
        run_id: &RunId,
        preconditions: &StoragePreconditions,
        node_ids: &[String],
        now_utc: &str,
    ) -> Result<(), StateError>;

    /// Atomically enforce `plan.preconditions` and apply all node, dependency, claim, artifact,
    /// blackboard, and subtask mutations in a single transaction.
    fn apply_transition_plan(
        &mut self,
        run_id: &RunId,
        plan: &TransitionPlan,
    ) -> Result<(), StateError>;

    /// Atomically apply a `SyncPlan` (transition updates, `runs.spec_digest`/optional profile rebind,
    /// newly added `gate_nodes`, and newly added `task_dependencies`).
    fn apply_sync_plan(
        &mut self,
        run_id: &RunId,
        plan: &SyncPlan,
        new_profile: Option<(&str, &str)>,
    ) -> Result<(), StateError>;

    /// Persist `runs.spec_id` for `run_id` inside the active transaction.
    fn update_run_spec_id(
        &mut self,
        run_id: &RunId,
        spec_id: Option<&str>,
    ) -> Result<(), StateError>;

    /// Insert a new blackboard entry and return the stored record with assigned `id`.
    fn insert_blackboard(
        &mut self,
        item: &BlackboardRecord,
    ) -> Result<BlackboardRecord, StateError>;

    /// Update the status of an existing blackboard entry scoped to `run_id`.
    fn update_blackboard_status(
        &mut self,
        run_id: &RunId,
        patch: &BlackboardStatusPatch,
    ) -> Result<(), StateError>;

    /// Insert or update a subtask row (enforcing terminal non-empty `evidence`).
    fn upsert_subtask(&mut self, subtask: &SubtaskRecord) -> Result<(), StateError>;

    /// Record `tombstone` in `run_tombstones` and prune the active `runs` graph atomically.
    fn archive_and_prune_run(
        &mut self,
        preconditions: &StoragePreconditions,
        tombstone: &RunTombstoneRecord,
    ) -> Result<(), StateError>;

    /// Clear `cleanup_pending = 0` on `run_tombstones` once post-commit file/ref/worktree cleanup completes.
    fn mark_tombstone_cleaned(&mut self, run_id: &RunId) -> Result<(), StateError>;
}

/// Execute `f` inside an atomic database transaction via an object-safe `&dyn StateBackend`
/// without monomorphizing callers over concrete backend types.
pub fn exec_tx_with<T>(
    backend: &dyn StateBackend,
    f: impl FnOnce(&mut dyn StateTx) -> Result<T, StateError>,
) -> Result<T, StateError> {
    let mut f_opt = Some(f);
    let mut out: Option<T> = None;
    backend.exec_tx_dyn(&mut |tx| {
        let func = f_opt.take().ok_or_else(|| {
            StateError::new(
                ErrorCode::BackendConflict,
                "transaction closure invoked more than once",
            )
        })?;
        out = Some(func(tx)?);
        Ok(())
    })?;
    out.ok_or_else(|| {
        StateError::new(
            ErrorCode::BackendConflict,
            "transaction completed without producing a result",
        )
    })
}

/// Full read/write storage backend supporting atomic transactions and post-commit backup maintenance.
pub trait StateBackend: StateReader + Send + Sync {
    /// Execute `f` inside an atomic database transaction, rolling back on error (object-safe).
    fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), StateError>,
    ) -> Result<(), StateError>;

    /// Execute `f` inside an atomic database transaction, rolling back on error.
    fn exec_tx<T, F>(&self, f: F) -> Result<T, StateError>
    where
        Self: Sized,
        F: FnOnce(&mut dyn StateTx) -> Result<T, StateError>,
    {
        exec_tx_with(self, f)
    }

    /// Perform post-commit maintenance (e.g. `<git_common_dir>/acdd/state.bak.sqlite` backup under `backup.lock`).
    ///
    /// MUST be invoked AFTER releasing `<git_common_dir>/acdd/repo.lock`.
    fn after_commit_maintenance(&self) -> Result<(), StateError>;
}
