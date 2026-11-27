# `acdd-state` — Crate Router

Read [root instructions](../../AGENTS.md), [source router](../../src/AGENTS.md), [ADR 0001](../../docs/adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../../docs/adr/0003-unified-profile-graph-and-state-machine.md), [cross-platform](../../docs/architecture/cross-platform.md), and [state reference](../../docs/reference/tasks.md) before editing.

## Local Rules & Ownership

- Enforce `INV-STATE-NO-INDEX`, `INV-STATE-PURE-MACHINE`, `INV-STATE-SQLITE-CONFINED`, and `INV-STATE-SINGLE-VALIDATOR`: depend only on `acdd-core` and `acdd-spec`, keep `machine/` free of filesystem/SQLite/Git/clock/subprocess calls, confine `rusqlite` strictly to `src/store/sqlite/`, and route persistence through `&dyn StateBackend` (`exec_tx_dyn`).
- Reuse `acdd_core::{config, errors, path_policy, fs, lock, git, sys, format, telemetry}` and `acdd_spec` (`INV-SINGLE-OWNER`, `INV-GIT-SINGLE-OWNER`), keep validation in `validate.rs` + `usecases/deps/` and `when:` in `workflow/when.rs`, and preload query snapshots in `roadmap/`/`deps/`/`validate.rs` without `N+1` archive fallbacks.
- Pass DB/worktree paths through `confined_database_open_path`, `canonicalize_macos_var`, and `check_ancestors_no_symlinks`; keep `open_read_only_if_exists` strictly read-only without creating missing DB files.
- Keep integration suites in `crates/acdd-state/tests/state_integration.rs` (using `test-support` / `RunHarness::in_memory()`), and escalate new subsystems or `~80-100`+ net LOC defect fixes via `acdd_blackboard`.

## Verification

```bash
cargo clippy -p acdd-state --all-targets --all-features -- -D warnings
cargo test -p acdd-state
cargo test -p acdd-state --features failpoints --lib
cargo test --test acdd
```
