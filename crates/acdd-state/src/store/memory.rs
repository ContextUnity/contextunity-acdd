//! In-memory SQLite-backed backend (`:memory:`) for fast (`< 5 ms`) test matrices.

use crate::store::sqlite::migrations::SqliteMeta;
use crate::store::sqlite::InMemorySqliteStore;
use crate::store::traits::{StateBackend, StateTx};
use acdd_core::errors::AcddError;

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
    pub fn new() -> Result<Self, AcddError> {
        Ok(Self {
            inner: InMemorySqliteStore::new()?,
        })
    }

    /// Return current `_sqlite_meta` counters (`schema_version`, `write_seq`, `last_backup_seq`).
    pub fn read_meta(&self) -> Result<SqliteMeta, AcddError> {
        self.inner.read_meta()
    }

    /// Return all non-internal SQLite table names currently present in the schema.
    #[cfg(test)]
    pub(crate) fn table_names(&self) -> Result<Vec<String>, AcddError> {
        self.inner.table_names()
    }

    /// Execute a raw SQL statement batch (intended for negative DDL / constraint harness tests).
    #[cfg(test)]
    pub(crate) fn execute_raw_sql(&self, sql: &str) -> Result<(), AcddError> {
        self.inner.execute_raw_sql(sql)
    }
}

crate::store::impl_state_reader_delegate!(MemoryStateBackend, with_reader | s | s.inner);

impl StateBackend for MemoryStateBackend {
    fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), AcddError>,
    ) -> Result<(), AcddError> {
        self.inner.exec_tx_dyn(f)
    }

    fn after_commit_maintenance(&self) -> Result<(), AcddError> {
        // Backups are intentionally skipped for `:memory:` test backends.
        Ok(())
    }
}
