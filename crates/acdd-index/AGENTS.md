# `acdd-index` — Extraction, Linking & Code-Map SQLite

Read the [root instructions](../../AGENTS.md), [source architecture](../../src/AGENTS.md), and [indexing architecture](../../docs/architecture/indexing.md) before editing this crate. Read [language extractors](src/languages/AGENTS.md) before modifying `src/languages/`.

## Boundaries & Ownership

- `crates/acdd-index` owns AST extraction (`ast/`), language profiles and framework manifests (`src/languages/`), cross-file symbol linking (`linker/`), directory scanning (`scanner/`), documentation parsing (`docs.rs`), graph/fact encoding (`models/`, `typed_facts/`, `interner.rs`, `semantic.rs`), Merkle commitments (`commitments.rs`), and code-map SQLite persistence (`db/`).
- Depend only on `acdd-core` inside this workspace; never import `acdd-state`.

## Production Rust Invariants

- **Reuse `acdd-core` primitives**: Use `acdd_core::{lock, fs, format, git, telemetry}`. Do not duplicate `libc::flock`, `Sha256` + `hex::encode`, `JsonSizeCounter`, or symlink/mkdir loops.
- **Intern & pool early**: Pool repeated tags, paths, and identifiers with `CompactTag`, `Arc<str>`, `Cow<'_, str>`, and `ACDD-STRTAB\x01` string tables (`typed_facts/binary.rs`).
- **Stream hashes**: Use `acdd_core::format::sha256_serde_json` instead of allocating intermediate `serde_json::to_vec` buffers.
- **Determinism (`INV-PERF-DETERMINISTIC-MERKLE-SEALING`)**: Never assign `SymbolId` or emit rows, edges, or commitments from unsorted `hashbrown::HashMap`/`HashSet` or unsorted `fs::read_dir` iteration; sort keys explicitly or use `BTreeMap`/`BTreeSet`.
- **De-monomorphize hot paths**: Pass `&Facts`, `&mut dyn std::io::Write`, or non-generic inner functions instead of duplicating large generic bodies across `Facts`/`TypedFacts` or writers.

## SQLite & Cross-Platform Invariants

- Always pass `SQLITE_OPEN_NOFOLLOW` paths through `acdd_core::fs::confined_database_open_path` (or `confined_database_create_path`) before opening.
- Read-only code-map opening (`db::reader::open_confined_readonly`) must handle WAL-mode databases without a `-shm` file (`OPEN_READ_WRITE` + `PRAGMA query_only = ON` with `OPEN_READ_ONLY` fallback) and enforce `PRAGMA trusted_schema = OFF;` plus `busy_timeout`.
- Staged publications checkpoint WAL (`TRUNCATE`), switch `PRAGMA journal_mode = DELETE;`, close the connection, and unlink regular-file `-wal`/`-shm`/`-journal` sidecars under exclusive lock.

## Verification

```bash
cargo clippy -p acdd-index --all-targets --all-features -- -D warnings
cargo test -p acdd-index --lib
```
