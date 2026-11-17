---
title: "ADR 0007: SQLite Code-Map Concurrency and Staged Publication"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0007: SQLite Code-Map Concurrency and Staged Publication

## Status

Accepted.

## Context

Serving concurrent MCP JSON-RPC queries and CLI reads alongside cold index rebuilds, incremental deltas, and multi-worktree state updates risks exposing partially populated tables, dangling `-wal`/`-shm` sidecars after publication, symlink attacks on database files, or macOS/APFS read-only permission edge cases.

## Decision

### 1. Code-Map Reader Admission & WAL Mode

- Code-map reader connections initialize with `PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA synchronous = NORMAL; PRAGMA mmap_size = 268435456;`.
- **Snapshot Admission & Cross-Process Fencing**: The server reuses one reader connection behind a mutex, verifying canonical workspace, adapter digest/roots, and main database + non-empty WAL file identity on admission under a shared filesystem lock that fences against exclusive rebuild/delta writers.
- After executing a query, the server re-checks database identity while the read transaction remains active; a changed or unverifiable snapshot invalidates the cached connection and fails the stale request cleanly.
- Verified snapshot receipts bind canonical workspace/database paths, Merkle output root, and database file identity (no `meta` generation counter or event-only cache invalidation).

### 2. Two-Phase Staged Indexing & Atomic Publication

- **Temporary Staging**: Cold builds populate all tables, FTS5 virtual tables, and Merkle commitments inside `<path>.sqlite.tmp` using fast private pragmas (`synchronous = OFF; cache_size = -64000;`).
- **Pre-Publication Verification**: Merkle root commitments, schema versions, and vacuum state are verified before any replacement occurs.
- **WAL Checkpoint & Atomic Swap**: Under an exclusive filesystem lock (`flock`), the builder checkpoints WAL (`TRUNCATE`), switches `PRAGMA journal_mode = DELETE;`, closes the staging handle, unlinks any regular-file `-wal`/`-shm`/`-journal` sidecars, and publishes via atomic `std::fs::rename`. Build failures leave the previous generation untouched.

### 3. State DB `NOFOLLOW`, macOS WAL, and `DELETE` Backups

- All SQLite opens across `acdd-index` and `acdd-state` ([`crates/acdd-state/src/store/sqlite/mod.rs`](../../crates/acdd-state/src/store/sqlite/mod.rs)) validate symlink-free ancestors (`confined_database_open_path` / `confined_database_create_path`) and pass `SQLITE_OPEN_NOFOLLOW` (complementing `O_NOFOLLOW` on lock/cache files).
- Read-only state queries handle read-only filesystems or restricted permissions cleanly by falling back to `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` with `PRAGMA query_only = ON;` without creating missing files or directories.
- State backups (`<git_common_dir>/acdd/state.bak.sqlite` under `backup.lock`, [`backup.rs`](../../crates/acdd-state/src/store/sqlite/backup.rs)) run `rusqlite::backup::Backup` into a staged temp file, finalize with `PRAGMA journal_mode = DELETE;` so backups are self-contained single files without lingering `-wal`/`-shm` sidecars across macOS/Linux, and publish via atomic `rename`.

## Consequences

- Readers see only complete, Merkle-verified database generations with zero partial-write visibility.
- Stale `-wal`/`-shm` sidecars never corrupt newly published generations or backup files.
- Symlinked SQLite targets fail closed across Linux and macOS.
