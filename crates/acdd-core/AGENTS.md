# `acdd-core` — Leaf Contracts & Shared Primitives

Read the [root instructions](../../AGENTS.md) and [source architecture](../../src/AGENTS.md) before editing this crate.

## Boundaries (`INV-CORE-LEAF`)

- Keep `crates/acdd-core` a strict leaf crate: zero dependencies on `acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `chrono`, `rayon`, `zstd`, `hashbrown`, `clap`, `rmcp`, or `tokio`.
- Preserve all existing public signatures and DTO contracts (`INV-SURFACE-COMPATIBILITY`).
- Keep thin generic wrappers (`impl AsRef<Path>`, `impl serde::Serialize`) delegating immediately to non-generic inner functions (`fn inner(path: &Path)`) to prevent monomorphization bloat.

## Single-Source Workspace Ownership

- **Contracts & Policy**: `ports::DomainProvider`, `ids`, `errors`, `schema`, `config` (`Adapter`, `ScannerLimits`, `load_adapter`), `subsystems`, `response`, and `path_policy::WorkspacePathPolicy`.
- **Filesystem (`fs`)**: `atomic_write` (`0o600`/`0o755`), `atomic_replace`, `confined_database_open_path` / `confined_database_create_path`, `canonicalize_macos_var`, `ensure_confined_dir_all`, `check_ancestors_no_symlinks`, `checked_child`, and `open_confined`.
- **Advisory Locks (`lock`)**: bounded-retry and blocking shared/exclusive OS file locks via `std::fs::File::try_lock*` / `lock*` with `O_NOFOLLOW` regular-file opening (`0o600`). Do not introduce `unsafe` or `libc::flock`.
- **Git (`git`)**: `scrub_git_env` (`STRIPPED_GIT_ENV_VARS`) and `run_git_*` / `git_*` subprocess helpers.
- **Format & Hashing (`format`)**: `stable_hash64`, `sha256_hex` / `sha256_bytes`, streaming `sha256_serde_json` / `sha256_serde_json_hex` (zero `Vec<u8>`), `canonical_json_sha256` / `canonical_json_string`, `JsonByteCounter` / `estimate_json_bytes`, and `utf8_floor_boundary` / `utf8_tail`.
- **Telemetry & Time (`telemetry`)**: `TraceId`, `SpanId`, `TraceParent`, `now_utc_iso_ms`, and `unix_epoch_ms`.

## Verification

```bash
cargo clippy -p acdd-core --all-targets --all-features -- -D warnings
cargo test -p acdd-core --lib
```
