//! Atomic SQLite backup (`state.bak.sqlite`) under `backup.lock` and restore reconciliation
//! against committed `<collection>/archive/*.md`.

use super::migrations::{ensure_schema, read_meta};
use crate::types::{ArchiveCleanupEntry, RunTombstoneRecord, TerminalStatus};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{
    checked_child, confined_database_create_path, confined_database_open_path,
    ensure_confined_dir_all,
};
use acdd_core::git::{git_status, git_stdout};
use acdd_core::ids::{CommitOid, RunId, RunName};
use acdd_core::lock::acquire_exclusive;
use acdd_core::telemetry::now_utc_iso_ms;
use rusqlite::{params, Connection, OpenFlags};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static BACKUP_NONCE: AtomicU64 = AtomicU64::new(0);

fn now_utc_iso() -> String {
    now_utc_iso_ms()
}

fn ensure_confined_dir(dir: &Path) -> Result<(), StateError> {
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| StateError::new(ErrorCode::ProfileSchemaInvalid, err.to_string()))?
            .join(dir)
    };
    ensure_confined_dir_all(&absolute, &absolute)
        .map(|_| ())
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to prepare confined database directory '{}': {err}",
                    dir.display()
                ),
            )
        })
}

/// Perform an atomic `rusqlite::backup` to `<backup_dir>/state.bak.sqlite` under `backup.lock`
/// when `significant_dirty` is set and `write_seq > last_backup_seq`.
///
/// MUST be invoked after releasing `<git_common_dir>/acdd/repo.lock`.
pub(crate) fn perform_backup_if_needed(
    conn: &mut Connection,
    backup_dir: &Path,
    significant_dirty: bool,
) -> Result<bool, StateError> {
    if !significant_dirty {
        return Ok(false);
    }

    let meta = read_meta(conn)?;
    if meta.write_seq <= meta.last_backup_seq {
        return Ok(false);
    }

    ensure_confined_dir(backup_dir)?;
    let lock_path = backup_dir.join("backup.lock");
    let _lock = acquire_exclusive(&lock_path).map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to acquire backup.lock '{}': {err}", lock_path.display()),
        )
    })?;

    // Re-read metadata under backup.lock before writing backup.
    let meta = read_meta(conn)?;
    if meta.write_seq <= meta.last_backup_seq {
        return Ok(false);
    }

    let backup_path = backup_dir.join("state.bak.sqlite");
    let confined_dst = confined_database_create_path(&backup_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid backup path '{}': {err}", backup_path.display()),
        )
    })?;

    let nonce = BACKUP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp_path = backup_dir.join(format!(
        "state.bak.sqlite.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let confined_tmp = confined_database_create_path(&tmp_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid temporary backup path '{}': {err}", tmp_path.display()),
        )
    })?;

    let backup_res = (|| -> Result<(), StateError> {
        {
            let mut dst_conn = Connection::open_with_flags(
                &confined_tmp,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    format!("failed to open backup target '{}': {err}", confined_tmp.display()),
                )
            })?;
            {
                let backup = rusqlite::backup::Backup::new(conn, &mut dst_conn).map_err(|err| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!("failed to initialize SQLite backup: {err}"),
                    )
                })?;
                backup
                    .run_to_completion(256, Duration::from_millis(5), None)
                    .map_err(|err| {
                        StateError::new(
                            ErrorCode::BackendConflict,
                            format!("failed to run SQLite backup to completion: {err}"),
                        )
                    })?;
            }
            dst_conn
                .execute_batch("PRAGMA journal_mode = DELETE;")
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!("failed to finalize backup journal mode: {err}"),
                    )
                })?;
        }

        fs::rename(&confined_tmp, &confined_dst).map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "failed to atomically rename backup '{}' -> '{}': {err}",
                    confined_tmp.display(),
                    confined_dst.display()
                ),
            )
        })?;
        Ok(())
    })();

    if backup_res.is_err() {
        let _ = fs::remove_file(&confined_tmp);
    }
    backup_res?;

    let write_seq_i64 = i64::try_from(meta.write_seq).map_err(|_| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("write_seq exceeds i64::MAX: {}", meta.write_seq),
        )
    })?;
    conn.execute(
        "UPDATE _sqlite_meta SET last_backup_seq = ?1 WHERE id = 1",
        params![write_seq_i64],
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to update last_backup_seq: {err}"),
        )
    })?;

    Ok(true)
}

/// Restore `db_path` from `<backup_dir>/state.bak.sqlite` when `db_path` is missing and
/// reconcile any restored active runs whose `<collection>/archive/*.md` is already committed.
pub(crate) fn restore_from_backup_if_missing(
    db_path: &Path,
    backup_dir: &Path,
    workspace_root: Option<&Path>,
    spec_collection_paths: &[String],
) -> Result<bool, StateError> {
    match fs::symlink_metadata(db_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("database must not be a symlink: {}", db_path.display()),
            ));
        }
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to inspect database '{}': {err}", db_path.display()),
            ));
        }
    }

    let backup_path = backup_dir.join("state.bak.sqlite");
    match fs::symlink_metadata(&backup_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "backup database must not be a symlink: {}",
                    backup_path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "failed to inspect backup database '{}': {err}",
                    backup_path.display()
                ),
            ));
        }
    }

    ensure_confined_dir(backup_dir)?;
    let lock_path = backup_dir.join("backup.lock");
    let _lock = acquire_exclusive(&lock_path).map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to acquire backup.lock '{}': {err}", lock_path.display()),
        )
    })?;

    // Re-check after acquiring backup.lock.
    if db_path.exists() {
        return Ok(false);
    }

    let confined_backup = confined_database_open_path(&backup_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid backup path '{}': {err}", backup_path.display()),
        )
    })?;

    let bak_conn = Connection::open_with_flags(
        &confined_backup,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "failed to open backup database '{}': {err}",
                confined_backup.display()
            ),
        )
    })?;
    bak_conn
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA query_only = ON;")
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to configure backup read connection: {err}"),
            )
        })?;

    let integrity: String = bak_conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("backup quick_check failed: {err}"),
            )
        })?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("backup database failed integrity check: {integrity}"),
        ));
    }
    let _ = read_meta(&bak_conn)?;

    let parent = db_path.parent().ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("database path '{}' has no parent directory", db_path.display()),
        )
    })?;
    ensure_confined_dir(parent)?;
    let confined_dst = confined_database_create_path(db_path).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid database path '{}': {err}", db_path.display()),
        )
    })?;

    let nonce = BACKUP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp_restore = parent.join(format!(
        ".state.restore.{}.{}.sqlite.tmp",
        std::process::id(),
        nonce
    ));
    let confined_tmp = confined_database_create_path(&tmp_restore).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!(
                "invalid temporary restore path '{}': {err}",
                tmp_restore.display()
            ),
        )
    })?;

    let restore_res = (|| -> Result<(), StateError> {
        {
            let mut dst_conn = Connection::open_with_flags(
                &confined_tmp,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::BackendConflict,
                    format!(
                        "failed to open temporary restore DB '{}': {err}",
                        confined_tmp.display()
                    ),
                )
            })?;
            {
                let backup = rusqlite::backup::Backup::new(&bak_conn, &mut dst_conn).map_err(|err| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!("failed to initialize restore backup copy: {err}"),
                    )
                })?;
                backup
                    .run_to_completion(256, Duration::from_millis(5), None)
                    .map_err(|err| {
                        StateError::new(
                            ErrorCode::BackendConflict,
                            format!("failed to copy backup database: {err}"),
                        )
                    })?;
            }

            dst_conn
                .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::ProfileSchemaInvalid,
                        format!("failed to configure restored database: {err}"),
                    )
                })?;
            ensure_schema(&mut dst_conn)?;
            reconcile_restored_with_archives(
                &mut dst_conn,
                workspace_root,
                spec_collection_paths,
            )?;
            // Ensure restored file uses rollback journal mode before atomic rename so no
            // temporary -wal/-shm sidecars remain tied to the tmp filename.
            let _: String = dst_conn
                .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::BackendConflict,
                        format!("failed to finalize restored journal mode: {err}"),
                    )
                })?;
        }

        fs::rename(&confined_tmp, &confined_dst).map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!(
                    "failed to atomically rename restored DB '{}' -> '{}': {err}",
                    confined_tmp.display(),
                    confined_dst.display()
                ),
            )
        })?;
        Ok(())
    })();

    if restore_res.is_err() {
        let _ = fs::remove_file(&confined_tmp);
    }
    restore_res?;

    Ok(true)
}

#[derive(Debug)]
struct ActiveRunRow {
    run_id: String,
    run_name: String,
    collection: Option<String>,
    workspace: String,
    profile_id: String,
    spec_path: Option<String>,
    is_ephemeral: bool,
    generation: i64,
    target_base: Option<String>,
    target_base_commit: Option<String>,
}

fn candidate_archive_relpaths(spec_path: &str, spec_collection_paths: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    let path = Path::new(spec_path);
    let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
        return candidates;
    };

    if let Some(parent) = path.parent().and_then(|p| p.to_str()) {
        let normalized_parent = parent.trim_end_matches('/');
        if !normalized_parent.is_empty() {
            candidates.push(format!("{normalized_parent}/archive/{file_name}"));
        }
    }

    for col_path in spec_collection_paths {
        let trimmed = col_path.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            let rel = format!("{trimmed}/archive/{file_name}");
            if !candidates.contains(&rel) {
                candidates.push(rel);
            }
        }
    }

    candidates
}

fn is_archive_committed(ws_root: &Path, archive_rel: &str) -> bool {
    let Ok(child_path) = checked_child(ws_root, Path::new(archive_rel)) else {
        return false;
    };
    if ws_root.join(".git").exists() {
        let spec = format!("HEAD:{archive_rel}");
        return git_status(ws_root, &["cat-file", "-e", &spec])
            .is_ok_and(|status| status.success());
    }
    child_path.is_file()
}

fn parse_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let trimmed = content.trim_start_matches('\u{feff}');
    let rest = trimmed.strip_prefix("---")?;
    let end_idx = rest.find("\n---")?;
    let yaml_block = &rest[..end_idx];
    for line in yaml_block.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim() == key {
            let cleaned = v.trim().trim_matches('"').trim_matches('\'').trim();
            if !cleaned.is_empty() {
                return Some(cleaned.to_owned());
            }
        }
    }
    None
}

/// Reconcile active non-ephemeral runs in `conn` against committed `<collection>/archive/*.md`.
///
/// Any run whose archive file is already committed on disk is transitioned to a `completed`
/// `run_tombstones` row and pruned from `runs` (cascading to all child tables).
pub(crate) fn reconcile_restored_with_archives(
    conn: &mut Connection,
    workspace_root: Option<&Path>,
    spec_collection_paths: &[String],
) -> Result<usize, StateError> {
    let active_runs: Vec<ActiveRunRow> = {
        let mut stmt = conn
            .prepare(
                "SELECT run_id, run_name, collection, workspace, profile_id, spec_path,
                        is_ephemeral, generation, target_base, target_base_commit
                 FROM runs",
            )
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to query runs during restore reconciliation: {err}"),
                )
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ActiveRunRow {
                    run_id: row.get(0)?,
                    run_name: row.get(1)?,
                    collection: row.get(2)?,
                    workspace: row.get(3)?,
                    profile_id: row.get(4)?,
                    spec_path: row.get(5)?,
                    is_ephemeral: row.get::<_, i64>(6)? != 0,
                    generation: row.get(7)?,
                    target_base: row.get(8)?,
                    target_base_commit: row.get(9)?,
                })
            })
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to read runs during restore reconciliation: {err}"),
                )
            })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to decode run row during restore reconciliation: {err}"),
                )
            })?);
        }
        items
    };

    let mut reconciled = 0usize;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to start restore reconciliation transaction: {err}"),
            )
        })?;

    for row in active_runs {
        if row.is_ephemeral {
            continue;
        }
        let Some(ref spec_path) = row.spec_path else {
            continue;
        };
        let ws_root = workspace_root.unwrap_or_else(|| Path::new(&row.workspace));
        let candidates = candidate_archive_relpaths(spec_path, spec_collection_paths);
        let Some(archive_rel) = candidates
            .into_iter()
            .find(|rel| is_archive_committed(ws_root, rel))
        else {
            continue;
        };

        let archive_content = checked_child(ws_root, Path::new(&archive_rel))
            .ok()
            .and_then(|p| fs::read_to_string(p).ok())
            .unwrap_or_default();
        let spec_id = parse_frontmatter_field(&archive_content, "id");
        let title = parse_frontmatter_field(&archive_content, "title")
            .unwrap_or_else(|| row.run_name.clone());

        let closed_commit = if ws_root.join(".git").exists() {
            git_stdout(
                ws_root,
                &["log", "-n", "1", "--format=%H", "--", &archive_rel],
                "GIT_LOG_FAILED",
            )
            .ok()
            .and_then(|oid| CommitOid::new(oid).ok())
        } else {
            None
        };

        let mut cleanup_manifest = Vec::new();
        if checked_child(ws_root, Path::new(spec_path))
            .is_ok_and(|p| p.exists())
        {
            cleanup_manifest.push(ArchiveCleanupEntry {
                kind: "spec_file".to_owned(),
                path_or_ref: spec_path.clone(),
                workspace: None,
            });
        }
        let cleanup_pending = !cleanup_manifest.is_empty();

        let run_id = RunId::new(row.run_id.clone())?;
        let run_name = RunName::new(row.run_name.clone())?;
        let generation = u64::try_from(row.generation).map_err(|_| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("negative generation on restored run '{}'", row.run_id),
            )
        })?;
        let target_base_commit = row
            .target_base_commit
            .map(CommitOid::new)
            .transpose()?;

        let tombstone = RunTombstoneRecord {
            run_id,
            run_name,
            collection: row.collection,
            workspace: row.workspace,
            spec_id,
            title,
            profile_id: row.profile_id,
            is_ephemeral: false,
            generation,
            terminal_status: TerminalStatus::Completed,
            target_base: row.target_base,
            target_base_commit,
            archive_path: Some(archive_rel),
            closed_commit,
            cleanup_pending,
            cleanup_manifest,
            summary_json: r#"{"reconciled_from_archive":true}"#.to_owned(),
            metrics_json: "{}".to_owned(),
            closed_at: now_utc_iso(),
        };

        let cleanup_manifest_json = serde_json::to_string(&tombstone.cleanup_manifest)
            .map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("failed to serialize cleanup_manifest: {err}"),
                )
            })?;

        tx.execute(
            "INSERT OR REPLACE INTO run_tombstones (
                run_id, run_name, collection, workspace, spec_id, title, profile_id,
                is_ephemeral, generation, terminal_status, target_base, target_base_commit,
                archive_path, closed_commit, cleanup_pending, cleanup_manifest_json,
                summary_json, metrics_json, closed_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16,
                ?17, ?18, ?19
            )",
            params![
                tombstone.run_id.as_str(),
                tombstone.run_name.as_str(),
                tombstone.collection,
                tombstone.workspace,
                tombstone.spec_id,
                tombstone.title,
                tombstone.profile_id,
                i64::from(tombstone.is_ephemeral),
                i64::try_from(tombstone.generation).unwrap_or(i64::MAX),
                tombstone.terminal_status.as_str(),
                tombstone.target_base,
                tombstone.target_base_commit.as_ref().map(|c| c.as_str()),
                tombstone.archive_path,
                tombstone.closed_commit.as_ref().map(|c| c.as_str()),
                i64::from(tombstone.cleanup_pending),
                cleanup_manifest_json,
                tombstone.summary_json,
                tombstone.metrics_json,
                tombstone.closed_at,
            ],
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to insert reconciled tombstone: {err}"),
            )
        })?;

        tx.execute(
            "DELETE FROM runs WHERE run_id = ?1",
            params![tombstone.run_id.as_str()],
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to prune reconciled run '{}': {err}", tombstone.run_id),
            )
        })?;

        tx.execute(
            "UPDATE _sqlite_meta SET write_seq = write_seq + 1 WHERE id = 1",
            [],
        )
        .map_err(|err| {
            StateError::new(
                ErrorCode::BackendConflict,
                format!("failed to advance write_seq during reconciliation: {err}"),
            )
        })?;

        reconciled += 1;
    }

    tx.commit().map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to commit restore reconciliation transaction: {err}"),
        )
    })?;

    Ok(reconciled)
}
