# `acdd-state` — Unified Profile, State Machine & Run Store

Read the [root instructions](../../AGENTS.md), [source architecture](../../src/AGENTS.md), and [state reference](../../docs/reference/tasks.md) before editing this crate.

## Boundaries & Ownership

- `crates/acdd-state` owns `profile/` (`GateProfile` `version: 2`, inheritance, `when::WhenExpr`), `spec.rs` (`SpecDocument`), the 100% pure I/O-free state reducer `machine/`, `validate.rs` + `usecases/deps.rs` (the sole workspace/spec/profile/dep validator), `store/` (`StateBackend` traits + SQLite/memory backends), `git_ops/`, `runner/`, 6-tier `context.rs`, `observability/`, `usecases/`, and `harness.rs` (`RunHarness`).
- Depend only on `acdd-core` inside this workspace; never depend on `acdd-index` or host `src/`.
- Keep `machine/` free of filesystem, SQLite, Git, clock, and subprocess calls.
- Confine all `rusqlite` usage strictly to `src/store/sqlite/`. Use `&dyn StateBackend` via `exec_tx_dyn` in `usecases/` to avoid generic monomorphization bloat across `SqliteStateBackend` and `MemoryStateBackend`.

## Production Rust & Single-Source Invariants (`INV-SINGLE-OWNER`)

- **Single-Owner Primitives**: Use `acdd_core::{config, errors, path_policy, fs, lock, git, sys, format, telemetry}` — zero local copies of glob/overlap matchers, `read_process_start_time`, `acdd.yaml` / `0.5` detection, `ErrorCode`/`StateError`, symlink ancestor checks, `mkdir` loops, Git env scrubbing, `sha256_hex`, `canonical_json_sha256`, `utf8_tail`, or UTC timestamp formatting.
- **Reuse-First & Git Boundary**: Search `acdd_core` and `acdd_state` with `code_map_search` / `ast_grep_search` before authoring helpers; route all Git subprocess calls in `git_ops/` and `runner/` through `acdd_core::git` (ban raw `Command::new("git")`).
- **Complexity Escalation**: Escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before introducing any new subsystem, parallel profile/scheduler pipeline, or defect fix exceeding `~80-100` net production LOC.
- **Single Validator & `WhenExpr`**: Route all workspace/spec/profile/dependency validation through `crate::validate` + `crate::usecases::deps`; reuse `crate::profile::when::WhenExpr` inside `machine/graph.rs` with zero duplicate parsers or evaluators.
- **Borrow & budget cleanly**: Borrow `GateProfile`, `RoleProfile`, and `&str`/`&Path` across `runner/` until owned DTOs or spawned processes require ownership. Cache static regexes with `LazyLock<Regex>` and size 6-tier `context.rs` payloads in $O(N)$ using `acdd_core::format::estimate_json_bytes`.
- **Determinism**: Sort all `fs::read_dir` entries before scanning specs, profiles, or archives.

## SQLite & Cross-Platform Invariants

- Route all DB path confinement through `acdd_core::fs::confined_database_open_path` / `confined_database_create_path`.
- Read-only state queries (`open_read_only_if_exists`) strictly open `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` first (handling WAL-mode databases with missing `-shm` recovery without unintended writes) and never create missing DB files.
- SQLite backup/restore (`store/sqlite/backup.rs`) switches staged databases to `PRAGMA journal_mode = DELETE;` before atomic rename.

## Verification

```bash
cargo clippy -p acdd-state --all-targets --all-features -- -D warnings
cargo test -p acdd-state --lib
cargo test -p acdd-state --features failpoints --lib
cargo test --test acdd
```
