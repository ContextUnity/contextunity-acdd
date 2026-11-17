---
title: "ADR 0002: Workspace Confinement and Path Policy"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0002: Workspace Confinement and Path Policy

## Status

Accepted.

## Context

Scanning or executing file/SQLite/Git operations against broad roots (`/`, `/home`, `..`), symlink escapes, or conflated worktree paths risks reading sensitive host files, exhausting memory, corrupting databases via symlinked sidecars, or either falsely triggering `TASK_CANDIDATE_MISMATCH` on runtime SQLite files or letting agents overwrite `.acdd/profile.yaml` and active `SpecDocument` contracts.

## Decision

### 1. Root Scope Guard and Symlink Confinement (`acdd-core`)

1. **Root Scope Guard**: Workspace initialization rejects forbidden broad roots (`/`, `~`, `/home/*`, `/Users/*`, `/usr`, `/var`, `/etc`, and multi-repository parent containers) unless explicitly overridden where permitted (`allow_broad_root`).
2. **Strict Symlink Confinement ([`fs.rs`](../../crates/acdd-core/src/fs.rs) & [`lock.rs`](../../crates/acdd-core/src/lock.rs))**:
   - Every workspace path resolves through `canonical_root`, `checked_child`, and `safe_relative`, rejecting parent-directory escapes and recursive symlink cycles.
   - Unix file opens for locks, extraction caches, and database preparation enforce `O_NOFOLLOW`.
   - SQLite opens validate symlink-free ancestors (`confined_database_open_path` / `confined_database_create_path`) and pass `SQLITE_OPEN_NOFOLLOW` so symlinks on the final database file fail closed.
   - macOS system-root aliases (`/var` $\to$ `/private/var`, `/tmp` $\to$ `/private/tmp`) are handled safely via `is_system_root_symlink` and canonicalization without permitting user-directory symlink escapes.
   - `atomic_write` (explicit `0o600` / `0o755` modes) and `atomic_replace` (mode-preserving replacement for specifications) share `commit_atomic_temp` and reject symlink targets.

### 2. Four Non-Conflated `WorkspacePathPolicy` Predicates

[`WorkspacePathPolicy`](../../crates/acdd-core/src/path_policy.rs) enforces four strictly separate lexical predicates:

1. **`is_engine_managed(rel_path)`**: Matches active `Run` `spec_path` and `archive_path` files owned by the state engine. Human spec edits during an active run are reconciled via `acdd sync`.
2. **`is_candidate_included(rel_path)`**: Defines which files enter reproducible Git `TreeOid` candidate/snapshot calculations:
   - **Excludes**: `.git/**`, `.worktrees/**`, `is_engine_managed(rel_path)`, configured `state_db` / `code_db` (plus `-wal`, `-shm`, `.backup` sidecars), and `.acdd/**` **except** the tracked configuration whitelist.
   - **Includes**: Tracked repository artifacts (`Cargo.lock`, `tests/**/*.jsonl`) and the tracked `.acdd/` whitelist (`.acdd/profile.yaml`, `.acdd/profiles/**`, `.acdd/frameworks/**`).
3. **`is_scope_admitted(rel_path, scope)`**: Component-wise normalized glob or directory match against the node's `admitted_scope` (`scope` + `extended_scope`).
4. **`is_agent_writable(rel_path, admitted_scope, protected_paths)`**: Returns `true` strictly when `is_candidate_included(rel_path)` AND `!is_scope_admitted(rel_path, protected_paths)` AND `is_scope_admitted(rel_path, admitted_scope)`. Consequently, `.acdd/{profile.yaml,profiles/**,frameworks/**}` participate in `TreeOid` snapshots while remaining un-writable by agents unless explicitly admitted in `scope` and not in `protected_paths`.

## Consequences

- Indexing, SQLite storage, advisory locking, and Git operations stay strictly confined within authorized repository perimeters across Linux and macOS.
- Runtime state files (`.acdd/state.sqlite*`, `.acdd/code-map.sqlite*`, `.acdd/run/**`, `.acdd/tmp/**`) never cause spurious `TASK_CANDIDATE_MISMATCH` drift, while profile/framework config changes remain cryptographically bound to `TreeOid`.
- Out-of-scope agent edits fail closed with `OUT_OF_SCOPE_CHANGES` before verification or submission.
