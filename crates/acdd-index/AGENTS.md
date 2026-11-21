# `acdd-index` — Extraction, Linking & Code-Map SQLite

Read the [root instructions](../../AGENTS.md), [source architecture](../../src/AGENTS.md), and [indexing architecture](../../docs/architecture/indexing.md) before editing this crate. Read [language extractors](src/languages/AGENTS.md) before modifying `src/languages/`.

## Boundaries & Ownership

- `crates/acdd-index` owns AST extraction (`ast/`), language profiles and framework manifests (`src/languages/`), cross-file symbol linking (`linker/`), directory scanning (`scanner/`), documentation parsing (`docs.rs`), graph/fact encoding (`models/`, `typed_facts/`, `interner.rs`, `semantic.rs`), Merkle commitments (`commitments.rs`), and code-map SQLite persistence (`db/`).
- Depend only on `acdd-core` inside this workspace; never import `acdd-state`.
- `models::{tags, details}` own typed model tags and node detail accessors; `linker::traits::FactsLookup` owns borrowed fact lookup.
- `commitments::{CommitmentDomain, FtsDomain}` own commitment and FTS domain identities; `db::commitments::fts5_fastpath` owns the FTS5 segment decoder.
- `db::writer::temp_table::TempTableGuard` owns temporary-table lifetime and `db::writer::batch::MultiValueBatch` owns multi-value insertion batching.

## Production Rust Invariants

- **Single-Owner Primitives (`INV-SINGLE-OWNER`)**: Reuse `acdd_core::{lock, fs, format, git, sys, telemetry, config, errors, path_policy}`. Do not duplicate `libc::flock`, `Sha256` + `hex::encode`, `JsonSizeCounter`, symlink/mkdir loops, `acdd.yaml` parsing, or glob engines.
- **Confined & Digest-Verified Disk Reads**: Read workspace source files through `super::symbols::verified_source` (in `db/`) or confined `acdd_core::fs` helpers; ban raw `std::fs::read_to_string` on workspace paths.
- **Zero Dead-Code Suppression, Shims, or Duplicates**: Ban crate/module-wide `#![allow(dead_code, unused_imports)]` and legacy `core`/`engine` module shims; run `code_map_query(operation="duplicates", path="crates/acdd-index")` and `code_map_query(operation="dead_tails", path="crates/acdd-index")` before delivery.
- **Reuse-First & Zero Heuristics**: Search `acdd_core` and `acdd_index` with `code_map_search` / `ast_grep_search` before adding helpers; ban narrow language string/path heuristics where tree-sitter AST, typed `RuleIR` manifests, or linker graph data exists, and ban raw `Command::new("git")` (`acdd_core::git` only).
- **Complexity Escalation**: Escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before introducing a new subsystem, parallel extraction/linker pipeline, or defect fix exceeding `~80-100` net production LOC.
- **Intern & pool early**: Pool repeated tags, paths, and identifiers with `CompactTag`, `Arc<str>`, `Cow<'_, str>`, and `ACDD-STRTAB\x01` string tables (`typed_facts/binary.rs`).
- **Stream hashes**: Use `acdd_core::format::sha256_serde_json` instead of allocating intermediate `serde_json::to_vec` buffers.
- **Determinism (`INV-PERF-DETERMINISTIC-MERKLE-SEALING`)**: Never assign `SymbolId` or emit rows, edges, or commitments from unsorted `hashbrown::HashMap`/`HashSet` or unsorted `fs::read_dir` iteration; sort keys explicitly or use `BTreeMap`/`BTreeSet`.
- **De-monomorphize hot paths**: Pass `&Facts`, `&mut dyn std::io::Write`, or non-generic inner functions instead of duplicating large generic bodies across `Facts`/`TypedFacts` or writers.
- **Compile-time codec coupling**: Never hand-encode raw bytes pretending to match another struct's `BinaryDecode` layout without compile-time field destructuring or a shared `struct_codecs!`; never duplicate binary magic constants (`LOCAL_MAGIC`, etc.) outside their canonical owning module.
- **Non-fallback fast-path proofs**: Any fast path with a silent fallback (`Ok(None) => slow_path`, such as `fts5_fastpath`) must have a production-seam test asserting `Some(...)` on a real `pipeline::build` index (covering full-range signed `stable_hash64` rowids), not only synthetic sequential rowid fixtures or transparent `commitments::verify`.
- **Zero `N+1` SQLite / Disk / Git Loops**: Never execute per-node, per-edge, or per-file `rusqlite` queries, `verified_source` reads, or `acdd_core::git` spawns inside loops (`db/{reader,traversal}`, `commitments`, `pipeline`); preload into `BTreeMap` / `IN (...)` / temp tables or batched `git cat-file --batch` reads.
- **Single-Pass Parse & Shared Query Helpers**: Never re-parse ASTs, source bodies, or manifests inside the same build/query pass, and consolidate shared SQL predicates/row decoders/pagination across `db/reader` and `db/traversal` into single canonical helpers.
- **Release Hot-Path Measurement**: Verify changes in `pipeline`, `linker`, `db/reader`, and `db/traversal` with `--release` timing (`wall` + `sys`) and peak RSS, not only debug `--lib` unit tests.

## SQLite & Cross-Platform Invariants

- Always pass `SQLITE_OPEN_NOFOLLOW` paths through `acdd_core::fs::confined_database_open_path` (or `confined_database_create_path`) before opening.
- Read-only code-map opening (`db::reader::open_raw_confined_readonly`) opens `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` first (handling WAL-mode databases without a `-shm` file without unintended writes) and enforces `PRAGMA trusted_schema = OFF;` plus `busy_timeout`.
- Staged publications checkpoint WAL (`TRUNCATE`), switch `PRAGMA journal_mode = DELETE;`, close the connection, and unlink regular-file `-wal`/`-shm`/`-journal` sidecars under exclusive lock.

## Verification

```bash
cargo clippy -p acdd-index --all-targets --all-features -- -D warnings
cargo test -p acdd-index --lib
```
