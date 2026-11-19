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
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, StateError> {
        Self::open_inner(db_path.as_ref())
    }

    fn open_inner(db_path: &Path) -> Result<Self, StateError> {
        Self::open_with_options(SqliteBackendOptions::custom(db_path))
    }

    /// Open (or initialize/restore) a SQLite state backend with full backup and reconciliation options
    /// (crate-internal so external callers must use `open` or `StateEngine`-wired reconciliation).
    pub(crate) fn open_with_options(options: SqliteBackendOptions) -> Result<Self, StateError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::traits::exec_tx_with;
    use crate::types::{ListRunsReq, TaskDependencyRecord, TerminalStatus};
    use acdd_core::git::git_command;
    use tempfile::TempDir;

    #[test]
    fn sqlite_v1_to_v2_migration_and_readonly_wal_without_shm() {
        let ws = TempDir::new().expect("tempdir");
        let db_dir = ws.path().join(".acdd");
        fs::create_dir_all(&db_dir).expect("create .acdd");
        let db_path = db_dir.join("state.sqlite");

        // 1. Seed a genuine schema v1 database without `runs.spec_id`, plus an active run row,
        //    and verify SqliteStateReader reads v1 cleanly without `no such column: spec_id`.
        {
            let mut conn = Connection::open(&db_path).expect("open v1 seed db");
            conn.execute_batch(schema::SCHEMA_V1_SQL)
                .expect("apply v1 sql");
            conn.execute(
                "INSERT INTO _sqlite_meta (id, schema_version, write_seq, last_backup_seq)
                 VALUES (1, 1, 5, 0)",
                [],
            )
            .expect("seed meta v1");
            conn.execute(
                "INSERT INTO runs (
                    run_id, run_name, workspace, profile_id, profile_json,
                    spec_digest, is_ephemeral, isolation, worktree, trace_id, status, created_at
                ) VALUES (
                    'run-v1-ro', 'run-v1-ro', '/tmp/ws', 'code', '{}',
                    'digest-v1', 0, 'in_place', '/tmp/ws-v1', 'trace-v1', 'active', '2026-01-01T00:00:00Z'
                )",
                [],
            )
            .expect("seed v1 run row");
            let has_spec_id: bool = conn
                .prepare("PRAGMA table_info(runs)")
                .expect("pragma")
                .query_map([], |r| r.get::<_, String>(1))
                .expect("query")
                .filter_map(Result::ok)
                .any(|c| c == "spec_id");
            assert!(!has_spec_id, "v1 schema should not have runs.spec_id yet");
            let _ = &mut conn;
        }
        {
            let v1_reader = SqliteStateReader::new(&db_path);
            let v1_page = v1_reader
                .list_runs(&ListRunsReq {
                    limit: 10,
                    ..ListRunsReq::default()
                })
                .expect("v1 list_runs must succeed read-only");
            assert_eq!(v1_page.runs.len(), 1);
            assert_eq!(v1_page.runs[0].run_id.as_str(), "run-v1-ro");
            assert_eq!(v1_page.runs[0].spec_id, None);
            let v1_graph = v1_reader
                .load_run_graph(&RunId::new("run-v1-ro").expect("run_id"))
                .expect("v1 load_run_graph must succeed")
                .expect("v1 run present");
            assert_eq!(v1_graph.run.spec_id, None);
            let v1_by_wt = v1_reader
                .resolve_active_run_by_worktree("/tmp/ws-v1")
                .expect("v1 resolve_active_run_by_worktree must succeed")
                .expect("v1 run matched by worktree");
            assert_eq!(v1_by_wt.spec_id, None);

            // Clean up seeded v1 run row before v2 migration test below, and verify newer schema fails closed.
            let conn = Connection::open(&db_path).expect("reopen v1 seed");
            conn.execute("DELETE FROM runs WHERE run_id = 'run-v1-ro'", [])
                .expect("delete v1 seed row");
            conn.execute(
                "UPDATE _sqlite_meta SET schema_version = ?1 WHERE id = 1",
                [i64::from(schema::CURRENT_SCHEMA_VERSION + 1)],
            )
            .expect("bump schema_version to future");
            drop(conn);

            let future_err = v1_reader
                .list_runs(&ListRunsReq::default())
                .expect_err("future schema_version must fail closed on read-only open");
            assert_eq!(future_err.code, ErrorCode::ProfileSchemaInvalid);

            let conn = Connection::open(&db_path).expect("restore v1 version");
            conn.execute("UPDATE _sqlite_meta SET schema_version = 1 WHERE id = 1", [])
                .expect("reset schema_version to 1");
        }

        // 2. Open with SqliteStateBackend -> migrates v1 -> v2, persists and reads spec_id.
        {
            let backend = SqliteStateBackend::open(&db_path)
                .expect("open backend migrates v1 -> v2");
            assert_eq!(backend.read_meta().expect("meta").schema_version, 2);
            // Reopen is idempotent when schema is already v2.
            drop(backend);
            let backend = SqliteStateBackend::open(&db_path)
                .expect("reopen idempotent v2");
            assert_eq!(backend.read_meta().expect("meta2").schema_version, 2);

            backend
                .inspect_writer(|conn| {
                    conn.execute(
                        "INSERT INTO runs (
                            run_id, run_name, workspace, profile_id, profile_json,
                            spec_digest, is_ephemeral, isolation, trace_id, status, created_at
                        ) VALUES (
                            'run-1', 'run-1', '/tmp/ws', 'code', '{}',
                            'digest-1', 0, 'in_place', 'trace-1', 'active', '2026-01-01T00:00:00Z'
                        )",
                        [],
                    )
                    .map_err(|e| StateError::new(ErrorCode::BackendConflict, e.to_string()))?;
                    Ok(())
                })
                .expect("seed run row");

            exec_tx_with(&backend, |tx| {
                tx.update_run_spec_id(&RunId::new("run-1")?, Some("AC-V2-SPEC"))
            })
            .expect("update_run_spec_id");

            let loaded = backend
                .load_run_graph(&RunId::new("run-1").expect("run_id"))
                .expect("load_run_graph")
                .expect("run present");
            assert_eq!(loaded.run.spec_id.as_deref(), Some("AC-V2-SPEC"));
        }

        // 3. Ensure SQLite is in WAL mode, remove any `-shm` sidecar, make path read-only on Unix,
        // and verify SqliteStateReader reads cleanly without mutating files or creating `-shm`.
        let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
        if shm_path.exists() {
            fs::remove_file(&shm_path).expect("remove shm");
        }
        assert!(!shm_path.exists());

        let reader = SqliteStateReader::new(&db_path);
        {
            #[cfg(unix)]
            let _perm_guard = {
                use std::os::unix::fs::PermissionsExt;
                struct PermGuard {
                    dir: PathBuf,
                    file: PathBuf,
                }
                impl Drop for PermGuard {
                    fn drop(&mut self) {
                        let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
                        let _ = fs::set_permissions(&self.file, fs::Permissions::from_mode(0o644));
                    }
                }
                fs::set_permissions(&db_path, fs::Permissions::from_mode(0o444)).expect("ro file");
                fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o555)).expect("ro dir");
                PermGuard {
                    dir: db_dir.clone(),
                    file: db_path.clone(),
                }
            };

            let before_bytes = fs::read(&db_path).expect("read db bytes before");
            let page = reader
                .list_runs(&ListRunsReq {
                    limit: 10,
                    ..ListRunsReq::default()
                })
                .expect("reader opens read-only WAL db without -shm");
            assert_eq!(page.runs.len(), 1);
            assert_eq!(page.runs[0].spec_id.as_deref(), Some("AC-V2-SPEC"));
            assert!(!shm_path.exists(), "read-only path must not create -shm");
            assert_eq!(
                fs::read(&db_path).expect("read db bytes after"),
                before_bytes,
                "read-only open must not mutate db bytes"
            );
        }

        // 4. Non-empty `-wal` on a read-only path without `-shm` (where only `?immutable=1`
        //    would be possible) MUST fail closed instead of silently returning stale data.
        #[cfg(unix)]
        {
            let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
            fs::write(&wal_path, b"uncheckpointed-wal-frame-bytes").expect("write non-empty wal");

            use std::os::unix::fs::PermissionsExt;
            struct PermGuard {
                dir: PathBuf,
                file: PathBuf,
                wal: PathBuf,
            }
            impl Drop for PermGuard {
                fn drop(&mut self) {
                    let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
                    let _ = fs::set_permissions(&self.file, fs::Permissions::from_mode(0o644));
                    let _ = fs::remove_file(&self.wal);
                }
            }
            fs::set_permissions(&db_path, fs::Permissions::from_mode(0o444)).expect("ro file");
            fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o555)).expect("ro dir");
            let _perm_guard_wal = PermGuard {
                dir: db_dir.clone(),
                file: db_path.clone(),
                wal: wal_path.clone(),
            };

            let wal_err = reader
                .list_runs(&ListRunsReq {
                    limit: 10,
                    ..ListRunsReq::default()
                })
                .expect_err("non-empty WAL on immutable read-only path must fail closed");
            assert_eq!(wal_err.code, ErrorCode::BackendConflict);
        }

        // 5. Writer open MUST reject a newer-schema DB (`PRAGMA user_version > CURRENT_SCHEMA_VERSION`)
        //    BEFORE issuing `PRAGMA journal_mode = WAL` or mutating the database file.
        {
            let future_db = db_dir.join("future-user-version.sqlite");
            {
                let seed = Connection::open(&future_db).expect("seed future db");
                seed.execute_batch(&format!(
                    "PRAGMA journal_mode = DELETE; PRAGMA user_version = {};",
                    schema::CURRENT_SCHEMA_VERSION + 1
                ))
                .expect("set future user_version");
                seed.execute("CREATE TABLE sentinel(x INTEGER)", [])
                    .expect("create sentinel");
            }
            let bytes_before = fs::read(&future_db).expect("read future db before");
            let err = match SqliteStateBackend::open(&future_db) {
                Ok(_) => panic!("writer open on newer user_version must fail closed"),
                Err(err) => err,
            };
            assert_eq!(err.code, ErrorCode::BackendConflict);
            assert_eq!(
                fs::read(&future_db).expect("read future db after"),
                bytes_before,
                "writer open must not mutate newer-schema DB bytes before rejecting"
            );
            assert!(!PathBuf::from(format!("{}-wal", future_db.display())).exists());
            assert!(!PathBuf::from(format!("{}-shm", future_db.display())).exists());
        }

        // 6. #8 Backup restore reconciliation boundaries + #17/#18 apply_sync_plan + #21 atomic read tx.
        {
            let git_ws = ws.path().join("backup-scope-repo");
            fs::create_dir_all(git_ws.join("docs/milestones/archive")).unwrap();
            fs::create_dir_all(git_ws.join("docs/plans/archive")).unwrap();
            for args in [
                vec!["init"],
                vec!["config", "user.email", "test@example.com"],
                vec!["config", "user.name", "Test"],
            ] {
                assert!(git_command(&git_ws, &args).unwrap().status.success());
            }
            fs::write(
                git_ws.join("docs/plans/archive/AC-101.md"),
                "---\nid: AC-101\ntitle: Other Collection\nstatus: completed\n---\n",
            ).unwrap();
            fs::write(
                git_ws.join("docs/milestones/archive/AC-103.md"),
                "---\nid: AC-999\ntitle: Mismatched Spec ID\nstatus: completed\n---\n",
            ).unwrap();
            for args in [
                vec!["add", "."],
                vec!["commit", "-m", "seed head"],
            ] {
                assert!(git_command(&git_ws, &args).unwrap().status.success());
            }
            // Uncommitted working-tree archive must NOT be trusted in a Git repo.
            fs::write(
                git_ws.join("docs/milestones/archive/AC-102.md"),
                "---\nid: AC-102\ntitle: Uncommitted Disk\nstatus: completed\n---\n",
            ).unwrap();
            // Branch-only committed archive on `milestone/AC-104` MUST reconcile when `row.branch` matches.
            assert!(git_command(&git_ws, &["checkout", "-b", "milestone/AC-104"]).unwrap().status.success());
            fs::write(
                git_ws.join("docs/milestones/archive/AC-104.md"),
                "---\nid: AC-104\ntitle: Branch Archive\nstatus: completed\n---\n",
            ).unwrap();
            assert!(git_command(&git_ws, &["add", "docs/milestones/archive/AC-104.md"]).unwrap().status.success());
            assert!(git_command(&git_ws, &["commit", "-m", "branch archive"]).unwrap().status.success());
            assert!(git_command(&git_ws, &["checkout", "-"]).unwrap().status.success());

            let b_db = git_ws.join(".acdd/state.sqlite");
            let b_bak = git_ws.join(".git/acdd");
            let b_opts = SqliteBackendOptions {
                db_path: b_db.clone(),
                backup_dir: Some(b_bak),
                workspace_root: Some(git_ws.clone()),
                spec_collection_paths: vec!["docs/milestones".to_owned(), "docs/plans".to_owned()],
                is_custom_db: false,
            };
            let b_backend = SqliteStateBackend::open_with_options(b_opts.clone()).unwrap();
            let mut r101 = crate::harness::RunHarness::build_sample_graph("run-101", "AC-101", git_ws.to_str().unwrap(), None, &["T1"]).unwrap();
            r101.run.spec_path = Some("docs/milestones/AC-101.md".to_owned());
            r101.run.spec_id = Some("AC-101".to_owned());
            let mut r102 = crate::harness::RunHarness::build_sample_graph("run-102", "AC-102", git_ws.to_str().unwrap(), None, &["T1"]).unwrap();
            r102.run.spec_path = Some("docs/milestones/AC-102.md".to_owned());
            r102.run.spec_id = Some("AC-102".to_owned());
            let mut r103 = crate::harness::RunHarness::build_sample_graph("run-103", "AC-103", git_ws.to_str().unwrap(), None, &["T1"]).unwrap();
            r103.run.spec_path = Some("docs/milestones/AC-103.md".to_owned());
            r103.run.spec_id = Some("AC-103".to_owned());
            let mut r104 = crate::harness::RunHarness::build_sample_graph("run-104", "AC-104", git_ws.to_str().unwrap(), None, &["T1"]).unwrap();
            r104.run.spec_path = Some("docs/milestones/AC-104.md".to_owned());
            r104.run.spec_id = Some("AC-104".to_owned());
            r104.run.branch = Some("milestone/AC-104".to_owned());

            // Test #17/#18 apply_sync_plan removes dependencies and updates seq_order in SQLite.
            r101.dependencies.push(TaskDependencyRecord {
                run_id: r101.run.run_id.clone(),
                task_ref: acdd_core::ids::TaskRef::new("T1").unwrap(),
                depends_on_workspace: "/ext".to_owned(),
                depends_on_run_id: RunId::new("ext-run").unwrap(),
                depends_on_ref: acdd_core::ids::TaskRef::new("E1").unwrap(),
                satisfied: false,
            });
            b_backend.exec_tx(|tx| {
                tx.insert_run_graph(&r101)?;
                tx.insert_run_graph(&r102)?;
                tx.insert_run_graph(&r103)?;
                tx.insert_run_graph(&r104)
            }).unwrap();

            let sync_plan = crate::machine::SyncPlan {
                transition: crate::types::TransitionPlan {
                    preconditions: crate::types::StoragePreconditions {
                        expected_run_generation: 1,
                        expected_node_versions: Vec::new(),
                        expected_claim_revision: None,
                        expected_contract_revision: None,
                        expected_worker_id: None,
                        required_predecessor_trees: Vec::new(),
                    },
                    bump_run_generation: true,
                    new_run_status: None,
                    new_root_cycle_count: None,
                    completed_at: None,
                    node_patches: Vec::new(),
                    dependency_patches: Vec::new(),
                    claim_patches: Vec::new(),
                    new_artifacts: Vec::new(),
                    new_blackboard: Vec::new(),
                    blackboard_patches: Vec::new(),
                    new_subtasks: Vec::new(),
                    significant_transition: true,
                },
                added_nodes: Vec::new(),
                added_dependencies: Vec::new(),
                removed_dependencies: r101.dependencies.clone(),
                seq_order_updates: vec![("task:T1".to_owned(), 9)],
                spec_digest: "synced-digest".to_owned(),
            };
            b_backend.exec_tx(|tx| tx.apply_sync_plan(&r101.run.run_id, &sync_plan, None)).unwrap();
            let r101_after = b_backend.load_run_graph(&r101.run.run_id).unwrap().unwrap();
            assert!(r101_after.dependencies.is_empty());
            assert_eq!(
                r101_after.nodes.iter().find(|n| n.node_id == "task:T1").unwrap().seq_order,
                9
            );

            // Verify #21 atomic BEGIN DEFERRED / ROLLBACK read snapshot under interleaved writer DELETE.
            {
                let (ro_conn, sv) = open_read_only_if_exists(&b_db).unwrap().unwrap();
                let deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let deleted_hook = std::sync::Arc::clone(&deleted);
                let db_hook = b_db.clone();
                ro_conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
                    if let rusqlite::hooks::AuthAction::Read { table_name, .. } = ctx.action {
                        if table_name == "gate_nodes"
                            && !deleted_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
                        {
                            if let Ok(wconn) = Connection::open(&db_hook) {
                                let _ = wconn.execute_batch(
                                    "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
                                );
                                let _ = wconn.execute("DELETE FROM runs WHERE run_id = 'run-101'", []);
                            }
                        }
                    }
                    rusqlite::hooks::Authorization::Allow
                }));
                let snap_during_delete = reader::load_run_graph_with_version(
                    &ro_conn,
                    &RunId::new("run-101").unwrap(),
                    sv,
                )
                .unwrap()
                .expect("snapshot should remain atomic across interleaved DELETE");
                assert!(deleted.load(std::sync::atomic::Ordering::SeqCst));
                assert!(!snap_during_delete.nodes.is_empty());
                assert!(ro_conn.is_autocommit(), "read tx must roll back on exit");
                assert!(b_backend.load_run_graph(&r101.run.run_id).unwrap().is_none());
                // Re-insert r101 cleanly for restore test below.
                b_backend.exec_tx(|tx| tx.insert_run_graph(&r101)).unwrap();
            }

            b_backend.after_commit_maintenance().unwrap();
            drop(b_backend);

            let _ = fs::remove_file(&b_db);
            let _ = fs::remove_file(git_ws.join(".acdd/state.sqlite-wal"));
            let _ = fs::remove_file(git_ws.join(".acdd/state.sqlite-shm"));
            let b_restored = SqliteStateBackend::open_with_reconciler(b_opts, |runs| {
                crate::usecases::lifecycle::reconcile_restored_runs_with_archives(
                    runs,
                    Some(&git_ws),
                    &["docs/milestones".to_owned(), "docs/plans".to_owned()],
                )
            })
            .unwrap();
            assert!(b_restored.load_run_graph(&RunId::new("run-101").unwrap()).unwrap().is_some());
            assert!(b_restored.load_run_graph(&RunId::new("run-102").unwrap()).unwrap().is_some());
            assert!(b_restored.load_run_graph(&RunId::new("run-103").unwrap()).unwrap().is_some());
            assert!(b_restored.load_run_graph(&RunId::new("run-104").unwrap()).unwrap().is_none());
            let tomb_104 = b_restored.load_tombstone(&RunId::new("run-104").unwrap()).unwrap().unwrap();
            assert_eq!(tomb_104.terminal_status, TerminalStatus::Completed);
            assert!(tomb_104.closed_commit.is_some());
        }
    }
}
