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
    #[cfg(any(test, feature = "test-support"))]
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

fn encode_sqlite_uri_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut encoded = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            other => encoded.push(other),
        }
    }
    encoded
}

fn configure_read_only_conn(conn: &Connection, db_path: &Path) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA query_only = ON;",
    )?;
    // Probe page 1 / WAL state now so missing `-shm` on a WAL DB is surfaced immediately.
    let _: i64 = conn.query_row("SELECT 1 FROM sqlite_schema LIMIT 1", [], |row| row.get(0)).or_else(|err| {
        if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
            Ok(1)
        } else {
            Err(err)
        }
    })?;
    let _ = db_path;
    Ok(())
}

fn path_or_parent_is_read_only(db_path: &Path) -> bool {
    let db_ro = fs::metadata(db_path).is_ok_and(|m| m.permissions().readonly());
    let dir_ro = db_path
        .parent()
        .and_then(|p| fs::metadata(p).ok())
        .is_some_and(|m| m.permissions().readonly());
    db_ro || dir_ro
}

fn non_empty_wal_exists(db_path: &Path) -> bool {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    fs::metadata(wal_path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn open_immutable_read_only(confined: &Path) -> Result<Connection, StateError> {
    let uri = format!("file:{}?immutable=1", encode_sqlite_uri_path(confined));
    let uri_conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|uri_err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "failed to open read-only SQLite database '{}': {uri_err}",
                confined.display()
            ),
        )
    })?;
    configure_read_only_conn(&uri_conn, confined).map_err(|pragma_err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to configure read-only SQLite pragmas: {pragma_err}"),
        )
    })?;
    Ok(uri_conn)
}

/// Open an existing SQLite database strictly read-only (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW`
/// + `PRAGMA query_only = ON`) without ever creating missing files or parent directories.
///
/// Returns `(Connection, schema_version)` when the database exists.
fn open_read_only_if_exists(db_path: &Path) -> Result<Option<(Connection, u32)>, StateError> {
    let confined = confined_database_create_path(db_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "database confinement check failed for '{}': {err}",
                db_path.display()
            ),
        )
    })?;
    match fs::symlink_metadata(&confined) {
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

    // Strictly attempt read-only first so writable directories/files are never opened read-write
    // unnecessarily.
    let ro_attempt = Connection::open_with_flags(
        &confined,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .and_then(|conn| {
        configure_read_only_conn(&conn, &confined)?;
        Ok(conn)
    });

    let conn = match ro_attempt {
        Ok(conn) => conn,
        Err(rusqlite::Error::SqliteFailure(err, _))
            if matches!(
                err.code,
                rusqlite::ErrorCode::CannotOpen
                    | rusqlite::ErrorCode::ReadOnly
                    | rusqlite::ErrorCode::PermissionDenied
            ) =>
        {
            // A WAL database whose `-shm` sidecar was cleaned up or resides on a read-only
            // filesystem returns `CannotOpen` when opened `READ_ONLY`.
            // - Never mutate DB/sidecar files when the path or parent directory is read-only, or
            //   when there is no uncheckpointed `-wal` to replay: open with `?immutable=1` ONLY
            //   if no non-empty `-wal` exists (otherwise fail closed rather than returning stale data).
            // - Only when the directory/DB are writable AND a non-empty `-wal` exists, recover
            //   via `READ_WRITE` + immediate `PRAGMA query_only = ON`.
            let has_wal = non_empty_wal_exists(&confined);
            if path_or_parent_is_read_only(&confined) {
                if has_wal {
                    return Err(StateError::new(
                        ErrorCode::BackendConflict,
                        format!(
                            "cannot open read-only SQLite database '{}' with immutable=1 while a non-empty WAL sidecar exists",
                            confined.display()
                        ),
                    )
                    .with_hint("Checkpoint the WAL or ensure the database directory is writable before reading."));
                }
                open_immutable_read_only(&confined)?
            } else if !has_wal {
                open_immutable_read_only(&confined)?
            } else {
                let rw_conn = Connection::open_with_flags(
                    &confined,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(|rw_err| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!(
                            "failed to open SQLite database '{}' to replay non-empty WAL: {rw_err}",
                            confined.display()
                        ),
                    )
                })?;
                configure_read_only_conn(&rw_conn, &confined).map_err(|pragma_err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to configure read-only SQLite pragmas: {pragma_err}"),
                    )
                })?;
                rw_conn
            }
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

    let schema_version = migrations::validate_read_schema_version(&conn)?;
    Ok(Some((conn, schema_version)))
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
        let Some((conn, _)) = open_read_only_if_exists(&self.db_path)? else {
            return Ok(default);
        };
        f(&conn)
    }

    fn with_versioned_conn_or_default<T>(
        &self,
        default: T,
        f: impl FnOnce(&Connection, u32) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let Some((conn, schema_version)) = open_read_only_if_exists(&self.db_path)? else {
            return Ok(default);
        };
        f(&conn, schema_version)
    }
}

impl StateReader for SqliteStateReader {
    fn load_task_statuses(
        &self,
    ) -> Result<Vec<(RunId, String, crate::types::NodeStatus)>, StateError> {
        let Some((conn, _)) = open_read_only_if_exists(&self.db_path)? else {
            return Ok(Vec::new());
        };
        reader::load_task_statuses(&conn)
    }

    fn load_run_graph(&self, run_id: &RunId) -> Result<Option<RunGraphSnapshot>, StateError> {
        self.with_versioned_conn_or_default(None, |conn, schema_version| {
            reader::load_run_graph_with_version(conn, run_id, schema_version)
        })
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
        if let Some((conn, schema_version)) = open_read_only_if_exists(&self.db_path)? {
            reader::list_runs_with_version(&conn, req, schema_version)
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
        self.with_versioned_conn_or_default(None, |conn, schema_version| {
            reader::resolve_active_run_by_worktree_with_version(conn, worktree, schema_version)
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
        if let Some((conn, _)) = open_read_only_if_exists(&self.db_path)? {
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, StateError> {
        Self::open_inner(db_path.as_ref())
    }

    #[cfg(any(test, feature = "test-support"))]
    fn open_inner(db_path: &Path) -> Result<Self, StateError> {
        Self::open_with_options(SqliteBackendOptions::custom(db_path))
    }

    /// Open (or initialize/restore) a SQLite state backend with full backup and reconciliation options
    /// (gated on `test` / `test-support` so production callers use `StateEngine`-wired reconciliation).
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_with_options(options: SqliteBackendOptions) -> Result<Self, StateError> {
        Self::open_with_reconciler(options, |_| Ok(Vec::new()))
    }

    /// Open (or initialize/restore) a SQLite state backend, invoking `reconciler` if backup
    /// restoration occurs so higher-level lifecycle logic stays outside `store/sqlite`.
    pub(crate) fn open_with_reconciler<F>(
        options: SqliteBackendOptions,
        reconciler: F,
    ) -> Result<Self, StateError>
    where
        F: FnOnce(&[backup::RestoredActiveRun]) -> Result<Vec<RunTombstoneRecord>, StateError>,
    {
        let confined_path = confined_database_create_path(&options.db_path).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "database confinement check failed for '{}': {err}",
                    options.db_path.display()
                ),
            )
        })?;

        if !options.is_custom_db {
            if let Some(ref backup_dir) = options.backup_dir {
                let _ = backup::restore_from_backup_if_missing(
                    &confined_path,
                    backup_dir,
                    reconciler,
                )?;
            }
        }

        let parent = confined_path.parent().ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("database path '{}' has no parent directory", options.db_path.display()),
            )
        })?;
        ensure_confined_dir_all(parent, parent).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to prepare confined database directory '{}': {err}",
                    parent.display()
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
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to configure writer SQLite connection pragmas: {err}"),
            )
        })?;

        migrations::ensure_schema(&mut conn)?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to configure writer SQLite WAL pragmas: {err}"),
            )
        })?;

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
    #[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn inspect_read_only<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        let Some((conn, _)) = open_read_only_if_exists(&self.options.db_path)? else {
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
    #[cfg(any(test, feature = "test-support"))]
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

/// Open an existing `state.sqlite` strictly read-only for `test-support` integration tests.
#[cfg(any(test, feature = "test-support"))]
pub fn open_read_only_with_version(db_path: &Path) -> Result<Option<(Connection, u32)>, StateError> {
    open_read_only_if_exists(db_path)
}

/// Load a run graph snapshot from a read-only SQLite connection at an explicit `schema_version`.
#[cfg(any(test, feature = "test-support"))]
pub fn load_run_graph_with_version(
    conn: &Connection,
    run_id: &RunId,
    schema_version: u32,
) -> Result<Option<RunGraphSnapshot>, StateError> {
    reader::load_run_graph_with_version(conn, run_id, schema_version)
}

crate::store::impl_state_reader_delegate!(SqliteStateBackend, |s| s.reader);

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

#[cfg(any(test, feature = "test-support"))]
struct QueryOnlyGuard<'a>(&'a Connection);

#[cfg(any(test, feature = "test-support"))]
impl Drop for QueryOnlyGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.execute_batch("PRAGMA query_only = OFF;");
    }
}

/// Shared `:memory:` SQLite connection wrapper keeping all `rusqlite` imports confined to `store/sqlite/`.
#[cfg(any(test, feature = "test-support"))]
pub(crate) struct InMemorySqliteStore {
    conn: Mutex<Connection>,
}

#[cfg(any(test, feature = "test-support"))]
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
