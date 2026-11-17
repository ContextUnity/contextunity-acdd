//! In-memory SQLite-backed backend (`:memory:`) for fast (`< 5 ms`) test matrices.

use crate::store::sqlite::migrations::SqliteMeta;
use crate::store::sqlite::{reader, InMemorySqliteStore};
use crate::store::traits::{StateBackend, StateReader, StateTx};
use crate::types::{
    ArtifactKind, BlackboardListReq, BlackboardPage, GateArtifactRecord, GateClaimRecord,
    GateNodeRecord, GateTraceRecord, ListRunsPage, ListRunsReq, ObservabilitySnapshot,
    RunGraphSnapshot, RunRecord, RunTombstoneRecord,
};
use acdd_core::errors::StateError;
use acdd_core::ids::RunId;

/// In-memory [`StateBackend`] backed by a shared SQLite `:memory:` connection.
///
/// Executes the exact same v1 DDL, `CHECK`/`FOREIGN KEY` constraints, partial unique indexes,
/// CAS preconditions, and SQL queries as [`crate::store::sqlite::SqliteStateBackend`] without
/// touching the filesystem.
pub struct MemoryStateBackend {
    inner: InMemorySqliteStore,
}

impl MemoryStateBackend {
    /// Create a fresh in-memory SQLite state backend initialized to `schema_version = 1`.
    pub fn new() -> Result<Self, StateError> {
        Ok(Self {
            inner: InMemorySqliteStore::new()?,
        })
    }

    /// Return current `_sqlite_meta` counters (`schema_version`, `write_seq`, `last_backup_seq`).
    pub fn read_meta(&self) -> Result<SqliteMeta, StateError> {
        self.inner.read_meta()
    }

    /// Return all non-internal SQLite table names currently present in the schema.
    pub fn table_names(&self) -> Result<Vec<String>, StateError> {
        self.inner.table_names()
    }

    /// Execute a raw SQL statement batch (intended for negative DDL / constraint harness tests).
    #[cfg(test)]
    pub(crate) fn execute_raw_sql(&self, sql: &str) -> Result<(), StateError> {
        self.inner.execute_raw_sql(sql)
    }
}

impl StateReader for MemoryStateBackend {
    fn load_run_graph(&self, run_id: &RunId) -> Result<Option<RunGraphSnapshot>, StateError> {
        self.inner
            .with_reader(|conn| reader::load_run_graph(conn, run_id))
    }

    fn list_runs(&self, req: &ListRunsReq) -> Result<ListRunsPage, StateError> {
        self.inner.with_reader(|conn| reader::list_runs(conn, req))
    }

    fn resolve_active_run_by_worktree(&self, worktree: &str) -> Result<Option<RunRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::resolve_active_run_by_worktree(conn, worktree))
    }

    fn load_node(&self, run_id: &RunId, node_id: &str) -> Result<Option<GateNodeRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::load_node(conn, run_id, node_id))
    }

    fn latest_claim(
        &self,
        run_id: &RunId,
        node_id: &str,
    ) -> Result<Option<GateClaimRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::latest_claim(conn, run_id, node_id))
    }

    fn latest_trace(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
    ) -> Result<Option<GateTraceRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::latest_trace(conn, run_id, node_id, claim_revision))
    }

    fn load_artifact(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
        kind: ArtifactKind,
    ) -> Result<Option<GateArtifactRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::load_artifact(conn, run_id, node_id, claim_revision, kind))
    }

    fn list_blackboard(
        &self,
        run_id: &RunId,
        req: &BlackboardListReq,
    ) -> Result<BlackboardPage, StateError> {
        self.inner
            .with_reader(|conn| reader::list_blackboard(conn, run_id, req))
    }

    fn load_tombstone(&self, run_id: &RunId) -> Result<Option<RunTombstoneRecord>, StateError> {
        self.inner
            .with_reader(|conn| reader::load_tombstone(conn, run_id))
    }

    fn max_spec_number(&self, collection: &str, prefix: &str) -> Result<Option<u32>, StateError> {
        self.inner
            .with_reader(|conn| reader::max_spec_number(conn, collection, prefix))
    }

    fn observability_snapshot(&self) -> Result<ObservabilitySnapshot, StateError> {
        self.inner.with_reader(reader::observability_snapshot)
    }
}

impl StateBackend for MemoryStateBackend {
    fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), StateError>,
    ) -> Result<(), StateError> {
        self.inner.exec_tx_dyn(f)
    }

    fn after_commit_maintenance(&self) -> Result<(), StateError> {
        // Backups are intentionally skipped for `:memory:` test backends.
        Ok(())
    }
}
