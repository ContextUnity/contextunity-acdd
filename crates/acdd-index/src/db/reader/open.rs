use super::super::cache;
use super::super::commitments::{
    read_metadata, read_metadata_optional, MetadataKey, MAX_SQLITE_VALUE_BYTES,
};
use super::types::IndexRebuildRequired;
use anyhow::{bail, ensure, Context, Result};
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use std::{
    io::Read,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

/// Validates SQLite database header magic using a confined `O_NOFOLLOW` open.
pub fn validate_database_header(path: &Path) -> Result<()> {
    let confined = acdd_core::fs::confined_database_open_path(path)?;
    let parent = confined.parent().context("database path needs a parent")?;
    let name = confined
        .file_name()
        .context("database path needs a file name")?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    let mut file = acdd_core::fs::open_confined(parent, Path::new(name), &mut opts)?;
    ensure!(
        file.metadata()?.is_file(),
        "database path must be a regular file"
    );
    let mut header = [0; 16];
    file.read_exact(&mut header)?;
    if &header != b"SQLite format 3\0" {
        bail!("database has an invalid SQLite header");
    }
    Ok(())
}

struct IntegritySnapshot {
    _dir: acdd_core::fs::ScopedTempDir,
    path: PathBuf,
}

impl IntegritySnapshot {
    fn create() -> Result<Self> {
        let dir = acdd_core::fs::ScopedTempDir::create("contextunity-acdd-integrity")?;
        let path = dir.write_private_file("snapshot.sqlite", &[])?;
        Ok(Self { _dir: dir, path })
    }
}

/// Validates SQLite database quick_check integrity pragma.
pub fn validate_database_integrity(conn: &Connection) -> Result<()> {
    let source_integrity: String =
        conn.query_row("PRAGMA integrity_check(sqlite_schema)", [], |row| {
            row.get(0)
        })?;
    if source_integrity != "ok" {
        bail!("database integrity check failed: {source_integrity}");
    }
    let snapshot = IntegritySnapshot::create()?;
    let snapshot_open_path = acdd_core::fs::confined_database_open_path(&snapshot.path)?;
    let mut copy = Connection::open_with_flags(
        &snapshot_open_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    {
        let backup = Backup::new(conn, &mut copy)?;
        backup.run_to_completion(32, Duration::from_millis(5), None)?;
    }
    copy.execute_batch("PRAGMA cache_size=-4096; PRAGMA temp_store=FILE;")?;
    let integrity: String = copy.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("database integrity check failed: {integrity}");
    }
    Ok(())
}

/// Read connection held together with its generation guard lock.
pub struct LockedConnection {
    connection: Connection,
    _lock: acdd_core::lock::FileLockGuard,
}

impl Deref for LockedConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

fn wal_is_absent_or_empty(db_path: &Path) -> bool {
    let mut wal_name = db_path.as_os_str().to_owned();
    wal_name.push("-wal");
    match std::fs::symlink_metadata(PathBuf::from(wal_name)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Ok(meta) => meta.is_file() && !meta.file_type().is_symlink() && meta.len() == 0,
        Err(_) => false,
    }
}

fn sqlite_immutable_uri(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    let mut encoded = String::with_capacity(raw.len() + 24);
    encoded.push_str("file:");
    for ch in raw.chars() {
        match ch {
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            _ => encoded.push(ch),
        }
    }
    encoded.push_str("?immutable=1");
    Some(encoded)
}

/// Opens an existing SQLite database in confined read-only mode.
///
/// Opens with `READ_ONLY | NOFOLLOW` first so SQLite reports a true read-only
/// handle (`sqlite3_db_readonly == 1`) when `-shm` is present or journal mode
/// is `DELETE`. When a WAL-header snapshot without `-shm` fails with
/// `SQLITE_CANTOPEN (14)`:
/// - If no non-empty `-wal` sidecar exists, retries read-only with `?immutable=1`
///   (`READ_ONLY | URI | NOFOLLOW`), working in read-only directories without
///   mutating files or creating `-shm`.
/// - If a non-empty `-wal` sidecar exists, falls back to `READ_WRITE | NOFOLLOW`
///   with `PRAGMA query_only = ON` so uncheckpointed WAL frames are recovered
///   rather than silently skipped.
pub fn open_raw_confined_readonly(db_path: &Path) -> Result<Connection> {
    let confined = acdd_core::fs::confined_database_open_path(db_path)?;
    let configure = |conn: &Connection| -> rusqlite::Result<()> {
        conn.execute_batch(
            "PRAGMA query_only = ON; PRAGMA trusted_schema = OFF; PRAGMA busy_timeout = 5000;",
        )
    };
    let ro_attempt = Connection::open_with_flags(
        &confined,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .and_then(|conn| {
        configure(&conn)?;
        conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
        Ok(conn)
    });
    match ro_attempt {
        Ok(conn) => Ok(conn),
        Err(rusqlite::Error::SqliteFailure(err, _))
            if matches!(
                err.code,
                rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::ReadOnly
            ) =>
        {
            if wal_is_absent_or_empty(&confined) {
                if let Some(uri) = sqlite_immutable_uri(&confined) {
                    let conn = Connection::open_with_flags(
                        &uri,
                        OpenFlags::SQLITE_OPEN_READ_ONLY
                            | OpenFlags::SQLITE_OPEN_URI
                            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                    )?;
                    configure(&conn)?;
                    conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
                    return Ok(conn);
                }
            }
            let conn = Connection::open_with_flags(
                &confined,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            configure(&conn)?;
            conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
            Ok(conn)
        }
        Err(err) => Err(err.into()),
    }
}

/// Opens a database connection held under an exclusive/shared lock guard.
pub fn open_locked(path: &Path, root: &Path) -> Result<LockedConnection> {
    let lock = cache::shared_lock(path)?;
    Ok(LockedConnection {
        connection: open(path, root)?,
        _lock: lock,
    })
}

/// Opens a read-only SQLite snapshot after validating its workspace and Merkle seal.
///
/// The connection starts a deferred transaction so subsequent queries see one
/// generation. A verified file identity may use the cached integrity receipt.
///
/// # Errors
///
/// Rejects incompatible, corrupt, changed, or foreign workspace databases.
pub fn open(path: &Path, root: &Path) -> Result<Connection> {
    open_with_diagnostics(path, root, &mut |message| eprintln!("{message}"))
}

/// Opens a reader and reports recoverable receipt diagnostics to the supplied sink.
pub fn open_with_diagnostics(
    path: &Path,
    root: &Path,
    diagnostic: &mut dyn FnMut(String),
) -> Result<Connection> {
    let open_path = acdd_core::fs::confined_database_open_path(path)?;
    let before = cache::identity(&open_path)?;
    validate_database_header(&open_path)?;
    let conn = open_raw_confined_readonly(&open_path)?;
    conn.execute_batch("PRAGMA mmap_size=268435456;")?;
    conn.execute_batch("BEGIN DEFERRED")?;
    let has_metadata_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='metadata')",
        [],
        |r| r.get(0),
    )?;
    if !has_metadata_table {
        validate_database_integrity(&conn)?;
        return Err(
            IndexRebuildRequired("legacy database missing metadata table; rebuild index").into(),
        );
    }
    let version = read_metadata(&conn, MetadataKey::SchemaVersion)?;
    let engine = read_metadata(&conn, MetadataKey::IndexerEngine)?;
    if version != acdd_core::config::ENGINE_SCHEMA_VERSION || engine != "contextunity-acdd-rust" {
        validate_database_integrity(&conn)?;
        return Err(IndexRebuildRequired("incompatible database schema or engine").into());
    }
    validate_workspace(&conn, root)?;
    let semantics = read_metadata_optional(&conn, MetadataKey::IndexSemanticsVersion)?;
    if semantics.as_deref() != Some(crate::models::INDEX_SEMANTICS_VERSION) {
        validate_database_integrity(&conn)?;
        return Err(IndexRebuildRequired("incompatible index semantics; rebuild index").into());
    }
    let algorithm = read_metadata(&conn, MetadataKey::CommitmentAlgorithm)?;
    if algorithm != crate::db::commitments::ALGORITHM {
        validate_database_integrity(&conn)?;
        return Err(
            IndexRebuildRequired("incompatible commitment algorithm; rebuild index").into(),
        );
    }
    let seal = read_metadata(&conn, MetadataKey::OutputRoot)?;
    let receipt_missing = match cache::matches_with_reason(path, root, &seal, &before) {
        Ok(()) => false,
        Err(error) => {
            diagnostic(format!(
                "verified receipt unavailable for {}: {error:#}",
                path.display()
            ));
            true
        }
    };
    if receipt_missing {
        crate::db::commitments::verify(&conn)?;
    }
    let current = cache::identity(path)?;
    if current != before {
        bail!(
            "database changed during reader admission ({})",
            cache::describe_diff(&before, &current)
        );
    }
    if receipt_missing {
        match cache::publish_verified_identity(path, root, &seal, &before) {
            Ok(true) => {}
            Ok(false) => diagnostic(format!(
                "verified receipt publication refused for {}: generation changed or WAL is present",
                path.display()
            )),
            Err(error) => diagnostic(format!(
                "verified receipt publication failed for {}: {error:#}",
                path.display()
            )),
        }
    }
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_SQLITE_VALUE_BYTES,
    );
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024);
    Ok(conn)
}

/// Reads the indexed files inventory snapshot from the database.
pub fn inventory_snapshot(conn: &Connection) -> Result<Vec<crate::models::FileEntry>> {
    // Sealed inventory metadata can exceed the public SQL cell limit; restore it before tool queries.
    let previous_limit = conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, i32::MAX);
    let inventory = read_metadata(conn, MetadataKey::InventorySnapshot);
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, previous_limit);
    serde_json::from_str(&inventory.context("cannot read inventory snapshot")?)
        .context("invalid inventory snapshot")
}

/// Validates workspace root matches database metadata.
pub fn validate_workspace(conn: &Connection, root: &Path) -> Result<()> {
    let stored = read_metadata(conn, MetadataKey::WorkspaceRoot)?;
    if Path::new(&stored) != root.canonicalize()? {
        bail!("database belongs to a different workspace: {stored}");
    }
    Ok(())
}

/// Reads adapter policy from the index SQLite `metadata` table.
pub fn read_policy(root: &Path, conn: &Connection) -> Result<acdd_core::config::Adapter> {
    let raw = read_metadata(conn, MetadataKey::Adapter)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    acdd_core::config::read_policy_from_value(root, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        commitments::{seal, write_metadata, ALGORITHM},
        schema::initialize_code_map_schema,
    };

    #[test]
    fn inventory_snapshot_reads_over_limit_and_restores_sqlite_cell_cap() {
        let dir = acdd_core::fs::ScopedTempDir::create("acdd-ut-large-inv").unwrap();
        let db = dir.path().join("index.sqlite");
        let conn = Connection::open(&db).unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(
            &conn,
            MetadataKey::SchemaVersion,
            acdd_core::config::ENGINE_SCHEMA_VERSION,
        )
        .unwrap();
        write_metadata(&conn, MetadataKey::IndexerEngine, "contextunity-acdd-rust").unwrap();
        write_metadata(
            &conn,
            MetadataKey::WorkspaceRoot,
            &dir.path().canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();
        write_metadata(
            &conn,
            MetadataKey::IndexSemanticsVersion,
            crate::models::INDEX_SEMANTICS_VERSION,
        )
        .unwrap();
        write_metadata(&conn, MetadataKey::CommitmentAlgorithm, ALGORITHM).unwrap();
        let padded = format!("[]{}", " ".repeat(MAX_SQLITE_VALUE_BYTES as usize + 4096));
        write_metadata(&conn, MetadataKey::InventorySnapshot, &padded).unwrap();
        seal(&conn).unwrap();
        drop(conn);

        let opened = open(&db, dir.path()).unwrap();
        assert_eq!(
            opened.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
            MAX_SQLITE_VALUE_BYTES
        );
        assert!(
            read_metadata(&opened, MetadataKey::InventorySnapshot).is_err(),
            "public SQL queries must stay capped at MAX_SQLITE_VALUE_BYTES"
        );
        assert!(inventory_snapshot(&opened).unwrap().is_empty());
        assert_eq!(
            opened.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
            MAX_SQLITE_VALUE_BYTES
        );
    }
}
