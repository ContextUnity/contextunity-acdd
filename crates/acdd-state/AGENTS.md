# `acdd-state` — Unified Profile, State Machine & Run Store

Read the [root instructions](../../AGENTS.md), [source architecture](../../src/AGENTS.md), and [state reference](../../docs/reference/tasks.md) before editing this crate.

## Boundaries & Ownership

- `crates/acdd-state` owns `profile/` (`GateProfile` `version: 2`, inheritance, `when:` predicates), `spec.rs` (`SpecDocument`), the 100% pure I/O-free state reducer `machine/`, `store/` (`StateBackend` traits + SQLite/memory backends), `git_ops/`, `runner/`, 6-tier `context.rs`, `observability/`, `usecases/`, and `harness.rs` (`RunHarness`).
- Depend only on `acdd-core` inside this workspace; never depend on `acdd-index` or host `src/`.
- Keep `machine/` free of filesystem, SQLite, Git, clock, and subprocess calls.
- Confine all `rusqlite` usage strictly to `src/store/sqlite/`. Use `&dyn StateBackend` via `exec_tx_dyn` in `usecases/` to avoid generic monomorphization bloat across `SqliteStateBackend` and `MemoryStateBackend`.

## Production Rust & Single-Source Invariants

- **Reuse `acdd-core` primitives**: Use `acdd_core::{fs, lock, git, format, telemetry, config}` — zero local copies of symlink ancestor checks, `mkdir` loops, Git env scrubbing, `sha256_hex`, `canonical_json_sha256`, `utf8_tail`, or UTC timestamp formatting.
- **Borrow & budget cleanly**: Borrow `GateProfile`, `RoleProfile`, and `&str`/`&Path` across `runner/` until owned DTOs or spawned processes require ownership. Cache static regexes with `LazyLock<Regex>` and size 6-tier `context.rs` payloads in $O(N)$ using `acdd_core::format::estimate_json_bytes`.
- **Determinism**: Sort all `fs::read_dir` entries before scanning specs, profiles, or archives.

## SQLite & Cross-Platform Invariants

- Route all DB path confinement through `acdd_core::fs::confined_database_open_path` / `confined_database_create_path`.
- Read-only state queries use `open_read_only_if_exists` (trying `OPEN_READ_WRITE` + `PRAGMA query_only = ON` with `OPEN_READ_ONLY` fallback for WAL-without-`-shm` safety) and never create missing DB files.
- SQLite backup/restore (`store/sqlite/backup.rs`) switches staged databases to `PRAGMA journal_mode = DELETE;` before atomic rename.

## Verification

```bash
cargo clippy -p acdd-state --all-targets --all-features -- -D warnings
cargo test -p acdd-state --lib
cargo test -p acdd-state --features failpoints --lib
cargo test --test acdd
```
