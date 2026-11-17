//! Disk-backed SQLite state engine (`rusqlite` isolated strictly inside `store/sqlite/`).
//!
//! Enforces `ADR-0002` symlink confinement and `SQLITE_OPEN_NOFOLLOW` on every connection open,
//! configures `PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;` prior to the first query,
//! keeps `StateReader` strictly read-only (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only = ON`)
//! without creating missing files, and manages WAL writes + atomic `state.bak.sqlite` backups.

pub mod backup;
pub mod migrations;
pub mod reader;
pub mod schema;
pub mod tx;

use crate::store::traits::{StateBackend, StateReader, StateTx};
use crate::types::{
    ArtifactKind, BlackboardListReq, BlackboardPage, GateArtifactRecord, GateClaimRecord,
    GateNodeRecord, GateTraceRecord, ListRunsPage, ListRunsReq, ObservabilitySnapshot,
    RunGraphSnapshot, RunRecord, RunTombstoneRecord,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{
    confined_database_create_path, confined_database_open_path, ensure_confined_dir_all,
};
use acdd_core::ids::RunId;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Configuration options for opening [`SqliteStateBackend`].
#[derive(Debug, Clone)]
pub struct SqliteBackendOptions {
    /// Filesystem path to `state.sqlite`.
    pub db_path: PathBuf,
    /// Optional backup/lock directory (`<git_common_dir>/acdd`).
    pub backup_dir: Option<PathBuf>,
    /// Optional primary workspace root for restore reconciliation against `<collection>/archive/*.md`.
    pub workspace_root: Option<PathBuf>,
    /// Configured `specs.*.path` relative paths used during archive restore reconciliation.
    pub spec_collection_paths: Vec<String>,
    /// Marks custom test / `--state-db` paths so automatic `state.bak.sqlite` backup is skipped.
    pub is_custom_db: bool,
}

impl SqliteBackendOptions {
    /// Construct options for a custom/standalone database path with automatic backup disabled.
    pub fn custom(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            backup_dir: None,
            workspace_root: None,
            spec_collection_paths: Vec::new(),
            is_custom_db: true,
        }
    }
}

fn resolve_absolute_db_path(path: &Path) -> Result<PathBuf, StateError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|err| StateError::new(ErrorCode::ProfileSchemaInvalid, err.to_string()))?
            .join(path))
    }
}

fn check_ancestors_no_symlinks(path: &Path) -> Result<(), StateError> {
    let absolute = resolve_absolute_db_path(path)?;
    let parent = absolute.parent().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("database path '{}' has no parent directory", path.display()),
        )
    })?;
    acdd_core::fs::check_ancestors_no_symlinks(parent, &absolute).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("database parent must not traverse a symlink: {err}"),
        )
    })
}

fn ensure_parent_dir_no_symlinks(db_path: &Path) -> Result<(), StateError> {
    let absolute = resolve_absolute_db_path(db_path)?;
    let parent = absolute.parent().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("database path '{}' has no parent directory", db_path.display()),
        )
    })?;
    ensure_confined_dir_all(parent, parent)
        .map(|_| ())
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to prepare confined database directory '{}': {err}",
                    parent.display()
                ),
            )
        })
}

/// Open an existing SQLite database strictly read-only (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW`
/// + `PRAGMA query_only = ON`) without ever creating missing files or parent directories.
fn open_read_only_if_exists(db_path: &Path) -> Result<Option<Connection>, StateError> {
    check_ancestors_no_symlinks(db_path)?;
    match fs::symlink_metadata(db_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("database must not be a symlink: {}", db_path.display()),
            ));
        }
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("database must be a regular file: {}", db_path.display()),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to inspect database '{}': {err}", db_path.display()),
            ));
        }
    }

    let confined = confined_database_open_path(db_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("database path confinement check failed: {err}"),
        )
    })?;

    let conn = match Connection::open_with_flags(
        &confined,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ) {
        Ok(conn) => conn,
        Err(rusqlite::Error::SqliteFailure(err, _))
            if matches!(
                err.code,
                rusqlite::ErrorCode::ReadOnly
                    | rusqlite::ErrorCode::PermissionDenied
                    | rusqlite::ErrorCode::CannotOpen
            ) =>
        {
            Connection::open_with_flags(
                &confined,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|ro_err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "failed to open read-only SQLite database '{}': {ro_err}",
                        confined.display()
                    ),
                )
            })?
        }
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to open read-only SQLite database '{}': {err}",
                    confined.display()
                ),
            ));
        }
    };

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA query_only = ON;",
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to configure read-only SQLite pragmas: {err}"),
        )
    })?;

    Ok(Some(conn))
}

/// Strictly read-only `StateReader` over a SQLite file path.
///
/// Never creates missing database files or directories (safe for `modules.state: false` and
/// read-only inspection commands).
#[derive(Debug, Clone)]
pub struct SqliteStateReader {
    db_path: PathBuf,
}

impl SqliteStateReader {
    /// Create a read-only reader bound to `db_path` without opening or creating files eagerly.
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    /// Return the configured database path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn with_conn_or_default<T>(
        &self,
        default: T,
        f: impl FnOnce(&Connection) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let Some(conn) = open_read_only_if_exists(&self.db_path)? else {
            return Ok(default);
        };
        f(&conn)
    }
}

impl StateReader for SqliteStateReader {
    fn load_run_graph(&self, run_id: &RunId) -> Result<Option<RunGraphSnapshot>, StateError> {
        self.with_conn_or_default(None, |conn| reader::load_run_graph(conn, run_id))
    }

    fn list_runs(&self, req: &ListRunsReq) -> Result<ListRunsPage, StateError> {
        let limit = req.limit.clamp(1, 100);
        let default = ListRunsPage {
            runs: Vec::new(),
            tombstones: Vec::new(),
            total_runs: 0,
            total_tombstones: 0,
            limit,
            offset: req.offset,
        };
        if let Some(conn) = open_read_only_if_exists(&self.db_path)? {
            reader::list_runs(&conn, req)
        } else if let Some(expected_gen) = req.generation {
            if expected_gen != 0 {
                return Err(StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "runs generation mismatch on missing database: expected {expected_gen}, observed 0"
                    ),
                ));
            }
            Ok(default)
        } else {
            Ok(default)
        }
    }

    fn resolve_active_run_by_worktree(&self, worktree: &str) -> Result<Option<RunRecord>, StateError> {
        self.with_conn_or_default(None, |conn| {
            reader::resolve_active_run_by_worktree(conn, worktree)
        })
    }

    fn load_node(&self, run_id: &RunId, node_id: &str) -> Result<Option<GateNodeRecord>, StateError> {
        self.with_conn_or_default(None, |conn| reader::load_node(conn, run_id, node_id))
    }

    fn latest_claim(
        &self,
        run_id: &RunId,
        node_id: &str,
    ) -> Result<Option<GateClaimRecord>, StateError> {
        self.with_conn_or_default(None, |conn| reader::latest_claim(conn, run_id, node_id))
    }

    fn latest_trace(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
    ) -> Result<Option<GateTraceRecord>, StateError> {
        self.with_conn_or_default(None, |conn| {
            reader::latest_trace(conn, run_id, node_id, claim_revision)
        })
    }

    fn load_artifact(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
        kind: ArtifactKind,
    ) -> Result<Option<GateArtifactRecord>, StateError> {
        self.with_conn_or_default(None, |conn| {
            reader::load_artifact(conn, run_id, node_id, claim_revision, kind)
        })
    }

    fn list_blackboard(
        &self,
        run_id: &RunId,
        req: &BlackboardListReq,
    ) -> Result<BlackboardPage, StateError> {
        let limit = req.limit.clamp(1, 100);
        if let Some(conn) = open_read_only_if_exists(&self.db_path)? {
            reader::list_blackboard(&conn, run_id, req)
        } else if let Some(expected_gen) = req.generation {
            Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "blackboard run generation mismatch on missing database: expected {expected_gen}"
                ),
            ))
        } else {
            Ok(BlackboardPage {
                items: Vec::new(),
                total: 0,
                generation: 0,
                limit,
                offset: req.offset,
            })
        }
    }

    fn load_tombstone(&self, run_id: &RunId) -> Result<Option<RunTombstoneRecord>, StateError> {
        self.with_conn_or_default(None, |conn| reader::load_tombstone(conn, run_id))
    }

    fn max_spec_number(&self, collection: &str, prefix: &str) -> Result<Option<u32>, StateError> {
        self.with_conn_or_default(None, |conn| {
            reader::max_spec_number(conn, collection, prefix)
        })
    }

    fn observability_snapshot(&self) -> Result<ObservabilitySnapshot, StateError> {
        self.with_conn_or_default(ObservabilitySnapshot::default(), |conn| {
            reader::observability_snapshot(conn)
        })
    }
}

/// Disk-backed WAL SQLite [`StateBackend`] with symlink confinement, CAS preconditions,
/// and post-commit atomic `state.bak.sqlite` maintenance.
pub struct SqliteStateBackend {
    options: SqliteBackendOptions,
    write_conn: Mutex<Connection>,
    reader: SqliteStateReader,
    significant_dirty: AtomicBool,
}

impl SqliteStateBackend {
    /// Open (or initialize) a custom/standalone SQLite database at `db_path` without backups.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, StateError> {
        Self::open_inner(db_path.as_ref())
    }

    fn open_inner(db_path: &Path) -> Result<Self, StateError> {
        Self::open_with_options(SqliteBackendOptions::custom(db_path))
    }

    /// Open (or initialize/restore) a SQLite state backend with full backup and reconciliation options.
    pub fn open_with_options(options: SqliteBackendOptions) -> Result<Self, StateError> {
        if !options.is_custom_db {
            if let Some(ref backup_dir) = options.backup_dir {
                let _ = backup::restore_from_backup_if_missing(
                    &options.db_path,
                    backup_dir,
                    options.workspace_root.as_deref(),
                    &options.spec_collection_paths,
                )?;
            }
        }

        ensure_parent_dir_no_symlinks(&options.db_path)?;
        let confined_path = confined_database_create_path(&options.db_path).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "database confinement check failed for '{}': {err}",
                    options.db_path.display()
                ),
            )
        })?;

        let mut conn = Connection::open_with_flags(
            &confined_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to open SQLite state database '{}': {err}",
                    confined_path.display()
                ),
            )
        })?;

        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to configure writer SQLite pragmas: {err}"),
            )
        })?;

        migrations::ensure_schema(&mut conn)?;

        let reader = SqliteStateReader::new(confined_path.clone());
        let mut opts = options;
        opts.db_path = confined_path;

        Ok(Self {
            options: opts,
            write_conn: Mutex::new(conn),
            reader,
            significant_dirty: AtomicBool::new(false),
        })
    }

    /// Return the canonical confined path of `state.sqlite`.
    pub fn db_path(&self) -> &Path {
        &self.options.db_path
    }

    /// Return current `_sqlite_meta` counters (`schema_version`, `write_seq`, `last_backup_seq`).
    pub fn read_meta(&self) -> Result<migrations::SqliteMeta, StateError> {
        let guard = self.write_conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "SQLite write connection mutex poisoned",
            )
        })?;
        migrations::read_meta(&guard)
    }

    /// Execute a read-only SQLite closure against a fresh `SQLITE_OPEN_READ_ONLY` reader connection.
    pub fn inspect_read_only<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let Some(conn) = open_read_only_if_exists(&self.options.db_path)? else {
            return Err(StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "state database '{}' does not exist",
                    self.options.db_path.display()
                ),
            ));
        };
        f(&conn)
    }

    /// Execute a read-only closure against the writer connection (e.g., to inspect WAL pragmas).
    pub fn inspect_writer<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let guard = self.write_conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "SQLite write connection mutex poisoned",
            )
        })?;
        f(&guard)
    }
}

impl StateReader for SqliteStateBackend {
    fn load_run_graph(&self, run_id: &RunId) -> Result<Option<RunGraphSnapshot>, StateError> {
        self.reader.load_run_graph(run_id)
    }

    fn list_runs(&self, req: &ListRunsReq) -> Result<ListRunsPage, StateError> {
        self.reader.list_runs(req)
    }

    fn resolve_active_run_by_worktree(&self, worktree: &str) -> Result<Option<RunRecord>, StateError> {
        self.reader.resolve_active_run_by_worktree(worktree)
    }

    fn load_node(&self, run_id: &RunId, node_id: &str) -> Result<Option<GateNodeRecord>, StateError> {
        self.reader.load_node(run_id, node_id)
    }

    fn latest_claim(
        &self,
        run_id: &RunId,
        node_id: &str,
    ) -> Result<Option<GateClaimRecord>, StateError> {
        self.reader.latest_claim(run_id, node_id)
    }

    fn latest_trace(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
    ) -> Result<Option<GateTraceRecord>, StateError> {
        self.reader.latest_trace(run_id, node_id, claim_revision)
    }

    fn load_artifact(
        &self,
        run_id: &RunId,
        node_id: &str,
        claim_revision: u32,
        kind: ArtifactKind,
    ) -> Result<Option<GateArtifactRecord>, StateError> {
        self.reader
            .load_artifact(run_id, node_id, claim_revision, kind)
    }

    fn list_blackboard(
        &self,
        run_id: &RunId,
        req: &BlackboardListReq,
    ) -> Result<BlackboardPage, StateError> {
        self.reader.list_blackboard(run_id, req)
    }

    fn load_tombstone(&self, run_id: &RunId) -> Result<Option<RunTombstoneRecord>, StateError> {
        self.reader.load_tombstone(run_id)
    }

    fn max_spec_number(&self, collection: &str, prefix: &str) -> Result<Option<u32>, StateError> {
        self.reader.max_spec_number(collection, prefix)
    }

    fn observability_snapshot(&self) -> Result<ObservabilitySnapshot, StateError> {
        self.reader.observability_snapshot()
    }
}

impl StateBackend for SqliteStateBackend {
    fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), StateError>,
    ) -> Result<(), StateError> {
        let mut guard = self.write_conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "SQLite write connection mutex poisoned",
            )
        })?;
        let sql_tx = guard
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|err| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    format!("failed to begin SQLite IMMEDIATE transaction: {err}"),
                )
            })?;

        let mut wrapper = tx::SqliteTx::new(&sql_tx);
        f(&mut wrapper)?;
        let significant = wrapper.is_significant_dirty();

        sql_tx.commit().map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to commit SQLite transaction: {err}"),
            )
        })?;

        if significant {
            self.significant_dirty.store(true, Ordering::Release);
        }

        Ok(())
    }

    fn after_commit_maintenance(&self) -> Result<(), StateError> {
        if self.options.is_custom_db {
            return Ok(());
        }
        let Some(ref backup_dir) = self.options.backup_dir else {
            return Ok(());
        };
        if !self.significant_dirty.load(Ordering::Acquire) {
            return Ok(());
        }

        let mut guard = self.write_conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "SQLite write connection mutex poisoned",
            )
        })?;
        let _backed_up =
            backup::perform_backup_if_needed(&mut guard, backup_dir, true)?;
        self.significant_dirty.store(false, Ordering::Release);
        Ok(())
    }
}

struct QueryOnlyGuard<'a>(&'a Connection);

impl Drop for QueryOnlyGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.execute_batch("PRAGMA query_only = OFF;");
    }
}

/// Shared `:memory:` SQLite connection wrapper keeping all `rusqlite` imports confined to `store/sqlite/`.
pub(crate) struct InMemorySqliteStore {
    conn: Mutex<Connection>,
}

impl InMemorySqliteStore {
    pub(crate) fn new() -> Result<Self, StateError> {
        let mut conn = Connection::open_in_memory().map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to open in-memory SQLite database: {err}"),
            )
        })?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to configure in-memory SQLite pragmas: {err}"),
                )
            })?;
        migrations::ensure_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn with_reader<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let guard = self.conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "in-memory SQLite mutex poisoned",
            )
        })?;
        guard.execute_batch("PRAGMA query_only = ON;").map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to set PRAGMA query_only = ON: {err}"),
            )
        })?;
        let _reset = QueryOnlyGuard(&guard);
        f(&guard)
    }

    pub(crate) fn exec_tx_dyn(
        &self,
        f: &mut dyn FnMut(&mut dyn StateTx) -> Result<(), StateError>,
    ) -> Result<(), StateError> {
        let mut guard = self.conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "in-memory SQLite mutex poisoned",
            )
        })?;
        let sql_tx = guard
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|err| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    format!("failed to begin in-memory SQLite transaction: {err}"),
                )
            })?;
        let mut wrapper = tx::SqliteTx::new(&sql_tx);
        f(&mut wrapper)?;
        sql_tx.commit().map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to commit in-memory SQLite transaction: {err}"),
            )
        })?;
        Ok(())
    }

    pub(crate) fn read_meta(&self) -> Result<migrations::SqliteMeta, StateError> {
        let guard = self.conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "in-memory SQLite mutex poisoned",
            )
        })?;
        migrations::read_meta(&guard)
    }

    #[cfg(test)]
    pub(crate) fn execute_raw_sql(&self, sql: &str) -> Result<(), StateError> {
        let guard = self.conn.lock().map_err(|_| {
            StateError::new(
                ErrorCode::BackendConflict,
                "in-memory SQLite mutex poisoned",
            )
        })?;
        guard.execute_batch(sql).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("raw SQL execution failed: {err}"),
            )
        })
    }

    pub(crate) fn table_names(&self) -> Result<Vec<String>, StateError> {
        self.with_reader(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_schema
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name ASC",
                )
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to query table names: {err}"),
                    )
                })?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to read table names: {err}"),
                    )
                })?;
            let mut names = Vec::new();
            for row in rows {
                names.push(row.map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to decode table name: {err}"),
                    )
                })?);
            }
            Ok(names)
        })
    }
}
