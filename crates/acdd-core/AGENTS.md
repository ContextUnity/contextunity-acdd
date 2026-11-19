# `acdd-core` — Leaf Contracts & Shared Primitives

Read the [root instructions](../../AGENTS.md) and [source architecture](../../src/AGENTS.md) before editing this crate.

## Boundaries (`INV-CORE-LEAF`)

- Keep `crates/acdd-core` a strict leaf crate: zero dependencies on `acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `chrono`, `rayon`, `zstd`, `hashbrown`, `clap`, `rmcp`, or `tokio`.
- Preserve all existing public signatures and DTO contracts (`INV-SURFACE-COMPATIBILITY`).
- Keep thin generic wrappers (`impl AsRef<Path>`, `impl serde::Serialize`) delegating immediately to non-generic inner functions (`fn inner(path: &Path)`) to prevent monomorphization bloat.

## Single-Source Workspace Ownership (`INV-SINGLE-OWNER`)

- **Configuration & Errors (`config`, `errors`)**: `config` is the sole `acdd.yaml` loader (`load_adapter`, `load_adapter_strict`, `load_adapter_report`, `resolve_state_db_path`, `resolve_code_db_path`) and legacy `0.5` key guard (`ConfigMigrationRequired`); `errors` is the sole owner of `ErrorCode` (`30` canonical variants) and `StateError`.
- **Path Policy & Glob Engine (`path_policy`)**: `WorkspacePathPolicy` (`from_workspace_config`, scope/candidate predicates) and brace-aware glob/overlap matching (`expand_glob_braces`, `glob_matches`, `patterns_overlap`, `scopes_overlap`). Never duplicate glob engines downstream.
- **Contracts & Policy**: `ports::DomainProvider`, `ids`, `schema`, `subsystems`, and `response`.
- **Filesystem (`fs`)**: `atomic_write` (`0o600`/`0o755`), `atomic_replace`, `confined_database_open_path` / `confined_database_create_path`, `canonicalize_macos_var`, `ensure_confined_dir_all`, `check_ancestors_no_symlinks`, `checked_child`, and `open_confined`.
- **Advisory Locks (`lock`)**: bounded-retry and blocking shared/exclusive OS file locks via `std::fs::File::try_lock*` / `lock*` with `O_NOFOLLOW` regular-file opening (`0o600`). Do not introduce `unsafe` or `libc::flock`.
- **Git (`git`)**: `scrub_git_env` (`STRIPPED_GIT_ENV_VARS`) and `run_git_*` / `git_*` subprocess helpers. `crates/acdd-core/src/git.rs` is the sole permitted owner of raw `Command::new("git")` invocations across production code.
- **Reuse-first & Complexity Escalation**: Run `code_map_search` / `ast_grep_search` before adding primitives so shared utilities stay consolidated; escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before introducing a new subsystem, expanding public `acdd-core` surfaces, or exceeding `~80-100` net production LOC for a defect fix.
- **System, Format & Telemetry (`sys`, `format`, `telemetry`)**: `sys` owns cgroup memory parsing and cross-platform `read_process_start_time`; `format` owns `stable_hash64`, `sha256_hex` / `sha256_bytes`, streaming `sha256_serde_json` / `sha256_serde_json_hex` (zero `Vec<u8>`), `canonical_json_sha256` / `canonical_json_string`, `JsonByteCounter` / `estimate_json_bytes`, and `utf8_floor_boundary` / `utf8_tail`; `telemetry` owns `TraceId`, `SpanId`, `TraceParent`, `now_utc_iso_ms`, and `unix_epoch_ms`.

## Verification

```bash
cargo clippy -p acdd-core --all-targets --all-features -- -D warnings
cargo test -p acdd-core --lib
cargo test --test commitment_integrity
```
