//! Sequential SQLite schema migration runner seeded at `schema_version = 1`.

use super::schema::{CURRENT_SCHEMA_VERSION, DOMAIN_TABLES, SCHEMA_V1_SQL};
use acdd_core::errors::{ErrorCode, StateError};
use rusqlite::{params, Connection, OptionalExtension};

/// Metadata snapshot read from SQLite-private `_sqlite_meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteMeta {
    /// Current persisted schema version.
    pub schema_version: u32,
    /// Monotonic write sequence incremented on every `StateTx` mutation.
    pub write_seq: u64,
    /// Last `write_seq` captured into `state.bak.sqlite`.
    pub last_backup_seq: u64,
}

#[derive(Debug, Clone, Copy)]
struct MigrationStep {
    target_version: u32,
    apply: fn(&Connection) -> Result<(), StateError>,
}

const MIGRATIONS: &[MigrationStep] = &[MigrationStep {
    target_version: 1,
    apply: apply_v1,
}];

fn apply_v1(conn: &Connection) -> Result<(), StateError> {
    conn.execute_batch(SCHEMA_V1_SQL).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to initialize v1 state schema: {err}"),
        )
    })?;
    conn.execute(
        "INSERT OR IGNORE INTO _sqlite_meta (id, schema_version, write_seq, last_backup_seq)
         VALUES (1, 1, 0, 0)",
        [],
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to seed _sqlite_meta: {err}"),
        )
    })?;
    Ok(())
}

/// Ensure the SQLite database has been migrated to `CURRENT_SCHEMA_VERSION`.
pub fn ensure_schema(conn: &mut Connection) -> Result<(), StateError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to begin schema migration transaction: {err}"),
            )
        })?;

    let mut current_version = read_schema_version_in_conn(&tx)?;
    if current_version == 0 {
        // Guard against opening an unrelated or legacy 0.5 database without migration.
        let foreign_tables: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlite_meta'",
                [],
                |row| row.get(0),
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to inspect sqlite_schema before initialization: {err}"),
                )
            })?;
        if foreign_tables > 0 {
            return Err(StateError::new(
                ErrorCode::ConfigMigrationRequired,
                "state database contains existing tables without _sqlite_meta; migrate or recreate state.sqlite",
            )
            .with_hint("Follow docs/runbooks/migration-0.6.md if upgrading from 0.5.x"));
        }
    }

    if current_version > CURRENT_SCHEMA_VERSION {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "state database schema_version {current_version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            ),
        ));
    }

    for step in MIGRATIONS {
        if current_version < step.target_version {
            (step.apply)(&tx)?;
            tx.execute(
                "UPDATE _sqlite_meta SET schema_version = ?1 WHERE id = 1",
                params![i64::from(step.target_version)],
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!(
                        "failed to update _sqlite_meta to schema_version {}: {err}",
                        step.target_version
                    ),
                )
            })?;
            current_version = step.target_version;
        }
    }

    verify_required_tables(&tx)?;
    tx.commit().map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to commit schema migration transaction: {err}"),
        )
    })?;
    Ok(())
}

/// Read `_sqlite_meta` from an initialized connection.
pub fn read_meta(conn: &Connection) -> Result<SqliteMeta, StateError> {
    conn.query_row(
        "SELECT schema_version, write_seq, last_backup_seq FROM _sqlite_meta WHERE id = 1",
        [],
        |row| {
            let schema_version: i64 = row.get(0)?;
            let write_seq: i64 = row.get(1)?;
            let last_backup_seq: i64 = row.get(2)?;
            Ok((schema_version, write_seq, last_backup_seq))
        },
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to read _sqlite_meta: {err}"),
        )
    })
    .and_then(|(schema_version, write_seq, last_backup_seq)| {
        let schema_version = u32::try_from(schema_version).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid negative schema_version in _sqlite_meta: {schema_version}"),
            )
        })?;
        let write_seq = u64::try_from(write_seq).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid negative write_seq in _sqlite_meta: {write_seq}"),
            )
        })?;
        let last_backup_seq = u64::try_from(last_backup_seq).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("invalid negative last_backup_seq in _sqlite_meta: {last_backup_seq}"),
            )
        })?;
        Ok(SqliteMeta {
            schema_version,
            write_seq,
            last_backup_seq,
        })
    })
}

fn read_schema_version_in_conn(conn: &Connection) -> Result<u32, StateError> {
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = '_sqlite_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to check _sqlite_meta existence: {err}"),
            )
        })?;
    if !has_meta {
        return Ok(0);
    }
    let version: Option<i64> = conn
        .query_row(
            "SELECT schema_version FROM _sqlite_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to read schema_version from _sqlite_meta: {err}"),
            )
        })?;
    let Some(raw) = version else {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            "_sqlite_meta exists without id = 1 metadata row",
        ));
    };
    u32::try_from(raw).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid negative schema_version in _sqlite_meta: {raw}"),
        )
    })
}

fn verify_required_tables(conn: &Connection) -> Result<(), StateError> {
    for table in DOMAIN_TABLES {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                params![table],
                |row| row.get(0),
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to verify table '{table}': {err}"),
                )
            })?;
        if !exists {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("state database is missing required domain table '{table}'"),
            ));
        }
    }
    Ok(())
}
