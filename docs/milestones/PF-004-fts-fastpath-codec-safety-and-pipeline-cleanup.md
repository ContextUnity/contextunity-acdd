---
id: PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
title: "FTS5 fast-path signed-rowid fix, unified batch ingest, shared graph projection, typed RAM staging, and linker parallelism"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-index/src/commitments.rs
  - crates/acdd-index/src/db/commitments.rs
  - crates/acdd-index/src/db/commitments/fts5_fastpath.rs
  - crates/acdd-index/src/db/commitments/snapshot.rs
  - crates/acdd-index/src/db/writer/cache.rs
  - crates/acdd-index/src/db/writer/cache/tests.rs
  - crates/acdd-index/src/db/writer/delta.rs
  - crates/acdd-index/src/db/writer/ingest.rs
  - crates/acdd-index/src/db/writer/mod.rs
  - crates/acdd-index/src/db/writer/payload_store.rs
  - crates/acdd-index/src/db/writer/seal.rs
  - crates/acdd-index/src/linker/indexing.rs
  - crates/acdd-index/src/linker/mod.rs
  - crates/acdd-index/src/linker/python/receivers.rs
  - crates/acdd-index/src/models/details.rs
  - crates/acdd-index/src/typed_facts/binary.rs
  - src/engine/pipeline/cold.rs
  - src/engine/pipeline/delta.rs
  - src/engine/pipeline/hooks.rs
  - src/engine/pipeline/mod.rs
  - src/engine/pipeline/staging.rs
  - tests/commitment_integrity/parallel_sealing.rs
  - tests/core/build_query_roundtrip.rs
  - tests/linker/early_interning_and_compact_facts.rs
  - docs/milestones/PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup.md
invariants:
  - 'INV-PF004-FTS5-SIGNED-ROWID-FASTPATH: fts_digest_fastpath succeeds (returns Some) on real pipeline::build indexes whose node_search rowids span full signed i64 stable_hash64 values, decodes docsize blobs in-place without per-document Vec<u8> allocations or redundant fts5vocab DDL on the fast path, and matches fts_digest_vocab byte-for-byte.'
  - 'INV-PF004-CLEAN-DELTA-FTS-NOOP: Incremental delta skips node_search and doc_search FTS5 mutations when the file search projection is unchanged, preventing avoidable FTS5 tombstone fragmentation, and caches path_dictionary once across all 13 commitment domains.'
  - 'INV-PF004-SINGLE-INGEST-AND-SHARED-GRAPH-PROJECTION: persist_files_impl uses one unified MultiValueBatch<..., BorrowedSqlValue> path for cold and delta builds, persist_graph_prepared and seal::logical_domain_leaves share one canonical pre-projected graph (unique_edges, dependencies, node lookup) with Rayon-parallel domain leaf hashing, and in-memory staged catalog storage stays typed without serialize-then-deserialize round-trips.'
  - 'INV-PF004-LINKER-SINGLE-ENTRY-AND-PARALLEL-PREP: linker/mod.rs collapses duplicated Facts/TypedFacts/SymbolCatalog flow-interning loops and drops the borrowed BTreeMap clone in required_full_typed_facts, avoids mid-linker libc::malloc_trim(0) page-fault thrashing, and runs children_by_parent, package_exports, and PythonReceivers::build data-parallel across worker threads.'
  - 'INV-PF004-ZERO-PF003-BLOAT: Superseded parallel enums (FtsCommitmentTable), pass-through wrappers (populate_owned_with_store), manual TypedFacts byte mirroring, dead zstd prepared-storage paths, and BuildMode dead_tails are removed with zero new duplicates or dead_tails.'
acceptance_criteria:
  - 'AC-1: fts5_fastpath decodes signed i64 rowid deltas via monotonic wrapping_add_unsigned, validates u32::try_from and checked_add bounds, uses a typed Fts5DecodeState struct, decodes docsize blobs in-place without 72k Vec<u8> allocations or redundant ensure_fts_vocab queries when fastpath succeeds, and proves fts_digest_fastpath(conn, "node_search_postings") == Ok(Some(vocab)) on a real pipeline::build index in tests/commitment_integrity/parallel_sealing.rs.'
  - 'AC-2: FtsCommitmentTable is merged into FtsDomain, CommitmentDomain::from_name uses direct match, db/writer/delta.rs skips redundant FTS5 delete/reinsert when dirty_fts is false and removes dead commitments_ms/_verify_elapsed, and db/commitments.rs caches path_dictionary once per seal_owners_with_dirty_fts call and short-circuits empty domain batches.'
  - 'AC-3: persist_files_impl unifies cold and delta inserts onto MultiValueBatch<..., BorrowedSqlValue> (removing ~150+ LOC of if bulk_search else stmt_* duplication), persist_graph_prepared and seal::logical_domain_leaves share one PreparedGraph projection with Rayon-parallel domain hashing (removing duplicate unique_edges/dependencies/node_by_id passes), StagedPreparedStorage stays typed in RAM and uses a compile-time-safe projection codec only on disk spill (eliminating restore_payload_ms for in-budget builds and dead zstd decoder code), and typed_facts exposes uncompressed length directly so cold.rs stops slicing hardcoded b"ACDD-LOCAL-FACTS\0\x03".'
  - 'AC-4: populate_owned_with_store collapses into one populate_owned entrypoint skipping empty-table DELETE FROM storms when schema_ready is true, BuildMode dead tails and identical if/else branches in sanitize_extracted_facts are removed, PipelineTestHooks builds plain Self without Arc::get_mut().expect(), models/details.rs derives its JSON-key probe list from NodeDetailKey, required_full_typed_facts passes &dyn FactsLookup directly without cloning a borrowed BTreeMap, link_compact_* entrypoints share one flow-interning helper, mid-linker/mid-projection malloc_trim(0) calls are removed, and children_by_parent + package_exports + PythonReceivers::build run data-parallel across worker threads.'
required_skills:
  - acdd
  - close-review-findings
  - test-suite-rules
repositories: []
linked_to: []
---

# PF-004: FTS5 Fast-Path Signed-Rowid Fix, Unified Batch Ingest, Shared Graph Projection, Typed RAM Staging, and Linker Parallelism

## Goal & Empirical Benchmark Root Causes

Benchmarking `PF-003` on `commerce-release-update` (`4,153` files, `72,318` nodes, `221,672` edges) showed why cold-build wall time only moved from `12,257 ms` to `11,797 ms` (`-3.7%`, missing `<= 8,500 ms`) and `seal_fts_ms` regressed `+10%` (`1,009 ms -> 1,114 ms`, missing `<= 250 ms`), while revealing several redundant boundaries that can be deleted outright (`~500–650` net LOC reduction):

1. **`seal_fts_ms` (`1,009 ms -> 1,114 ms`, `+105 ms` regression instead of `-800 ms` speedup):**
   - `node_search` uses `rowid = stable_hash64(&node.id)` (full-range signed `i64`). In `fts5_fastpath.rs:299, 324`, `i64::try_from(delta)` fails whenever adjacent postings cross zero by `> i64::MAX` (`25%` per 2-doc term), aborting `fts5_fastpath` with `Ok(None)` **after** running `load_fts_documents` (`72,318` rows) and reading `node_search_data`, then falling back to full `fts_digest_vocab` (`105 ms + 1,009 ms = 1,114 ms`).
   - Even when fastpath succeeds, `snapshot.rs:75` and `db/commitments.rs:432` execute `ensure_fts_vocab` (`CREATE VIRTUAL TABLE temp.*_commit_vocab`) + `SELECT term FROM {vocab} LIMIT 1`, `load_fts_documents` allocates `72,318` `Vec<u8>` `size_blob`s instead of decoding `decode_fts5_size_array` in-place from `ValueRef::Blob`, and `fts5_fastpath` double-allocates every unique term (`to_vec()` + `.clone()`).
2. **`restore_payload_ms` (`+264 ms`) + duplicated cold/delta ingest + double graph projection (`persist_ms = 5,834 ms` = 50% of cold build):**
   - `extraction_worker` serializes all `72,318` `Node`s into `CatalogStorageView::encode` bytes (`storage_rows`), calls `shrink_to_fit()` twice, holds them in RAM (`StagedPayload::InMemory`), and `decode_prepared_storage` deserializes them back (`264 ms`). Keeping `PreparedCatalogStorage` typed in RAM and encoding only on disk spill eliminates `restore_payload_ms` for in-budget builds and removes the fragile `5x 0u32` manual `TypedFacts` layout mirroring.
   - `persist_files_impl` (`db/writer/ingest.rs:252-724`) duplicates 6 table inserts across `if bulk_search { ... } else { stmt_*.execute(...) }` (~150–180 LOC), while `node_batch`/`file_batch` bypass `MultiValueBatch<..., BorrowedSqlValue>` and `populate_owned_with_store` deletes and re-inserts all `4,153` `source_inventory` rows that `staging.rs` just wrote.
   - `persist_graph_prepared` (`ingest.rs`) and `seal::logical_domain_leaves` (`seal.rs`) **independently** build `72,318`-entry `node_by_id`/`node_paths` maps, walk all `221,672` `graph.edges`, deduplicate and sort `unique_edges`, collect and sort `dependencies` (including Python `template_contexts`), and `seal.rs` hashes all 4 graph domains (`edges`, `edge_occurrences`, `coverage`, `dependencies`, `763 ms`) serially on 1 thread. Sharing one `PreparedGraph` + Rayon-parallel domain leaf hashing removes both code duplication and redundant work.
3. **`link_ms` (`2,538 ms -> 2,506 ms`, only `-1.3%`) + `linker/mod.rs` triplication:**
   - `PF-003` left 6 mid-build `libc::malloc_trim(0)` calls (`linker/mod.rs:1094, 1131, 1157`, `db/writer/mod.rs:472, 490`, `seal.rs:493`, plus duplicate `cold.rs:678`) that cause glibc arena lock contention and kernel `MADV_DONTNEED` zero-page fault thrashing during active allocation.
   - `InheritanceIndexes::build` runs `rayon::join(build_python, RustMembers::build)` — on non-Rust repos (`54,932` Python nodes, `0` Rust nodes), 15 of 16 cores sit idle during `PythonReceivers::build`; `package_exports` iterates languages serially; `children_by_parent` runs serially outside the prep `rayon::join`.
   - `linker/mod.rs` repeats flow/uncertainty/fault interning across 3 entrypoints (`link_compact_catalog_with_registry`, `link_compact_with_registry`, `link_compact_with_typed_registry`) and `required_full_typed_facts` (`L194-L197`) clones a full `BTreeMap<String, &Facts>` even though `BTreeMap<String, TypedFacts>` already implements `FactsLookup`.

## Explicit Removal & Consolidation Targets

Builders MUST delete/collapse the following redundant code and boundaries (do not leave superseded paths behind):

- **Remove `FtsCommitmentTable`** (`crates/acdd-index/src/commitments.rs`) -> merge into `FtsDomain`; replace linear `ALL.iter().find(...)` in `CommitmentDomain::from_name` with `match`.
- **Remove redundant fast-path `ensure_fts_vocab` / `LIMIT 1`** (`db/commitments/snapshot.rs:75`, `db/commitments.rs:431-438`) and per-row `Vec<u8>` `size_blob` allocations in `load_fts_documents` (decode `d.sz` in-place from `ValueRef::Blob`).
- **Remove dead timing & double-write churn** (`commitments_ms`, `_verify_elapsed` in `db/writer/delta.rs:436, 461` and `pipeline/delta.rs:1424`; empty-table `DELETE FROM` storm + `source_inventory` delete/re-insert when `schema_ready == true` in `db/writer/mod.rs:267-285`).
- **Remove dual `if bulk_search { ... } else { stmt_* }` loops** in `db/writer/ingest.rs:252-724` -> unify cold & delta onto `MultiValueBatch<..., BorrowedSqlValue>`.
- **Remove duplicate graph materialization** between `db/writer/ingest.rs` and `db/writer/seal.rs` -> share one canonical `PreparedGraph` (like `PreparedCoverage`).
- **Remove manual `5x 0u32` `CatalogStorageView::encode` layout mirroring** and **dead zstd decoder branch** (`db/writer/cache.rs:56-138`), plus **hardcoded `b"ACDD-LOCAL-FACTS\0\x03"` slice** (`pipeline/cold.rs:512`).
- **Remove pass-through `populate_owned_with_store` wrapper** (`db/writer/mod.rs:200-237`), **`BuildMode` dead tails** (`is_cold`/`is_delta`/`is_fast_verify` and unused `FastVerifyOnly`), **identical `if/else` in `sanitize_extracted_facts`** (`pipeline/cold.rs:132-144`), **`Arc::get_mut().expect()` builder chains** (`pipeline/hooks.rs`), **redundant `shrink_to_fit()`** (`payload_store.rs:40`), and **duplicated JSON key array** (`models/details.rs:54`).
- **Remove `borrowed: BTreeMap<String, &Facts>` clone** (`linker/mod.rs:194-197`), **3x duplicated flow/uncertainty interning loops** (`linker/mod.rs:365-584`), and **6 mid-build `libc::malloc_trim(0)` calls** (`linker/mod.rs:1094, 1131, 1157`, `db/writer/mod.rs:472, 490`, `seal.rs:493`; keep only phase-boundary trims at `cold.rs:413` and `db/writer/mod.rs:561`).

## Non-Goals & Negative Boundaries

- Do **not** extract language-specific import/inheritance grammar rules out of `linker/indexing.rs` into `LanguageLinker` traits or refactor `linker/resolution/mod.rs` here; that belongs to `LS-003` Wave 1 (`universal-declarative-engine-and-generic-linker`).
- Do **not** refactor query/read-side modules (`db/reader.rs`, `db/traversal.rs`).
- Preserve bit-identical `acdd-owner-sha256-v2` `output_root` commitments across cold and delta builds.

## Tasks in this milestone

### task: fts5-fastpath-signed-rowid-and-delta-seal-hygiene

```yaml
task_ref: fts5-fastpath-signed-rowid-and-delta-seal-hygiene
target: "Fix FTS5 fast-path signed i64 rowid delta decoding, eliminate fast-path fts5vocab DDL and 72k docsize Vec<u8> allocations, merge FtsCommitmentTable into FtsDomain, skip no-op delta FTS5 rewrites, and cache path_dictionary across commitment domains"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/commitments.rs
  - crates/acdd-index/src/db/commitments/fts5_fastpath.rs
  - crates/acdd-index/src/db/commitments/snapshot.rs
  - crates/acdd-index/src/db/commitments.rs
  - crates/acdd-index/src/db/writer/delta.rs
  - src/engine/pipeline/delta.rs
  - tests/commitment_integrity/parallel_sealing.rs
invariants:
  - 'INV-PF004-FTS5-SIGNED-ROWID-FASTPATH: fts_digest_fastpath returns Some matching fts_digest_vocab byte-for-byte on real pipeline::build indexes spanning negative and positive signed stable_hash64 rowids without executing fts5vocab DDL on the fast path.'
  - 'INV-PF004-CLEAN-DELTA-FTS-NOOP: Incremental delta skips unchanged FTS5 mutations and caches path_dictionary once per seal_owners_with_dirty_fts call.'
acceptance_criteria:
  - 'AC-1: fts5_fastpath.rs uses monotonic wrapping_add_unsigned for signed i64 rowid deltas, u32::try_from in decode_fts5_varint32, checked_add in load_term, and a typed Fts5DecodeState struct without double term allocation; snapshot.rs and db/commitments.rs skip ensure_fts_vocab when fts_digest_fastpath succeeds and decode docsize blobs in-place from ValueRef::Blob without per-row Vec<u8> allocation.'
  - 'AC-2: tests/commitment_integrity/parallel_sealing.rs proves fts_digest_fastpath(conn, "node_search_postings") == Ok(Some(vocab)) on a real pipeline::build index containing both negative and positive stable_hash64 node_hash rowids.'
  - 'AC-3: FtsCommitmentTable is deleted and unified into FtsDomain, CommitmentDomain::from_name uses direct match, db/writer/delta.rs skips FTS5 delete+reinsert when dirty_fts is false and removes dead commitments_ms/_verify_elapsed, and db/commitments.rs hoists path_dictionary loading outside the 13-domain loop with early return on empty entries.'
status: planned
subtasks:
  - subtask_ref: fix-fts5-fastpath-signed-deltas-and-inplace-docsize
    title: "Fix signed i64 delta decoding, varint32/term bounds checks, Fts5DecodeState, in-place docsize decoding, and fast-path vocab skip in db/commitments/"
  - subtask_ref: unify-fts-domain-and-optimize-delta-seal
    title: "Delete FtsCommitmentTable in favor of FtsDomain, skip FTS5 rewrites when dirty_fts is false, remove dead commitments_ms/_verify_elapsed, and cache path_dictionary once across DOMAINS"
  - subtask_ref: real-pipeline-fts5-fastpath-non-fallback-test
    title: "Add production-seam non-fallback test in tests/commitment_integrity/parallel_sealing.rs proving fts_digest_fastpath == Some on a real pipeline::build index with signed node_hash rowids"
```

### task: linker-entry-unification-and-parallel-prep

```yaml
task_ref: linker-entry-unification-and-parallel-prep
target: "Unify duplicated linker flow-interning entrypoints, eliminate the borrowed BTreeMap clone in required_full_typed_facts, drop mid-linker malloc_trim(0) thrashing, and parallelize children_by_parent, package_exports, and PythonReceivers::build across worker threads"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/linker/mod.rs
  - crates/acdd-index/src/linker/indexing.rs
  - crates/acdd-index/src/linker/python/receivers.rs
  - tests/linker/early_interning_and_compact_facts.rs
invariants:
  - 'INV-PF004-LINKER-SINGLE-ENTRY-AND-PARALLEL-PREP: required_full_typed_facts passes &dyn FactsLookup directly without cloning BTreeMap<String, &Facts>, linker entrypoints share one flow/uncertainty interning helper, mid-linker malloc_trim(0) calls are removed, and package_exports + PythonReceivers::build execute data-parallel with bit-identical deterministic output.'
acceptance_criteria:
  - 'AC-1: required_full_typed_facts passes facts directly as &dyn FactsLookup (deleting the borrowed BTreeMap clone), and link_compact_catalog_with_registry / link_compact_with_registry / link_compact_with_typed_registry share one canonical flow/uncertainty/fault interning helper.'
  - 'AC-2: The 3 mid-linker libc::malloc_trim(0) calls in linker/mod.rs (L1094, L1131, L1157) are deleted; children_by_parent joins the parallel prep wave; package_exports runs parallel across languages; and PythonReceivers::build resolves class hierarchies data-parallel with Rayon (no single-threaded bottleneck on Python-heavy repositories) with deterministic ordering verified in tests/linker/early_interning_and_compact_facts.rs.'
status: planned
subtasks:
  - subtask_ref: unify-linker-entrypoints-and-drop-borrowed-clone
    title: "Delete the borrowed BTreeMap clone in required_full_typed_facts and consolidate the 3 link_compact_* flow/uncertainty interning loops into one helper in linker/mod.rs"
  - subtask_ref: remove-mid-linker-malloc-trim-and-parallelize-prep
    title: "Delete mid-linker malloc_trim(0) calls and parallelize children_by_parent, package_exports, and PythonReceivers::build with deterministic ordering"
  - subtask_ref: verify-parallel-linker-determinism
    title: "Extend tests/linker/early_interning_and_compact_facts.rs verifying deterministic parity across single-thread and multi-thread linker prep"
```

### task: unified-batch-ingest-shared-graph-seal-and-ram-staging

```yaml
task_ref: unified-batch-ingest-shared-graph-seal-and-ram-staging
target: "Unify cold and delta file inserts onto MultiValueBatch, share one PreparedGraph projection between ingest.rs and seal.rs with parallel domain hashing, keep in-memory catalog storage typed with a compile-safe spill codec, and remove PF-003 pipeline bloat and dead_tails"
contract_revision: 1
depends_on:
  - fts5-fastpath-signed-rowid-and-delta-seal-hygiene
tags:
  - default
scope:
  - crates/acdd-index/src/db/writer/cache.rs
  - crates/acdd-index/src/db/writer/cache/tests.rs
  - crates/acdd-index/src/db/writer/ingest.rs
  - crates/acdd-index/src/db/writer/mod.rs
  - crates/acdd-index/src/db/writer/payload_store.rs
  - crates/acdd-index/src/db/writer/seal.rs
  - crates/acdd-index/src/models/details.rs
  - crates/acdd-index/src/typed_facts/binary.rs
  - src/engine/pipeline/cold.rs
  - src/engine/pipeline/hooks.rs
  - src/engine/pipeline/mod.rs
  - src/engine/pipeline/staging.rs
  - tests/core/build_query_roundtrip.rs
invariants:
  - 'INV-PF004-SINGLE-INGEST-AND-SHARED-GRAPH-PROJECTION: persist_files_impl uses one unified MultiValueBatch<..., BorrowedSqlValue> path for cold and delta builds, persist_graph_prepared and seal::logical_domain_leaves share one canonical PreparedGraph with parallel domain leaf hashing, and in-memory staged storage avoids serialize/deserialize round-trips while disk-spilled storage uses one compile-time-checked codec.'
  - 'INV-PF004-ZERO-PF003-BLOAT: Zero pass-through populate_owned wrappers, zero dead zstd prepared-storage branches, zero hardcoded LOCAL_MAGIC slices outside typed_facts, and zero BuildMode dead_tails.'
acceptance_criteria:
  - 'AC-1: PreparedCatalogStorage stays typed in RAM when within PayloadStore budget and encodes via a compile-time-safe projection codec only when spilling to payloads.bin (removing manual 5x 0u32 TypedFacts mirroring and the dead zstd decoder branch), and typed_facts exposes uncompressed length from encoding so src/engine/pipeline/cold.rs removes hardcoded b"ACDD-LOCAL-FACTS\0\x03".'
  - 'AC-2: persist_files_impl collapses its duplicated if bulk_search / else stmt_* loops onto MultiValueBatch<..., BorrowedSqlValue>, populate_owned_with_store collapses into one populate_owned entrypoint skipping empty-table DELETE FROM storms when schema_ready is true (and avoiding double source_inventory writes), and persist_graph_prepared + seal::logical_domain_leaves share one PreparedGraph projection with Rayon-parallel domain leaf hashing and zero mid-projection malloc_trim(0) calls.'
  - 'AC-3: BuildMode dead_tails (is_cold, is_delta, is_fast_verify, and unused FastVerifyOnly) are removed, sanitize_extracted_facts collapses its identical if/else branches, PipelineTestHooks builds plain Self without Arc::get_mut().expect(), PayloadStore::stage avoids redundant shrink_to_fit, and models/details.rs derives its JSON key list from NodeDetailKey.'
status: planned
subtasks:
  - subtask_ref: typed-in-ram-staging-and-compile-safe-spill-codec
    title: "Keep PreparedCatalogStorage typed in RAM when within budget, use a compile-time-safe codec for disk spill, remove dead zstd decoder branch, and return uncompressed_len from typed_facts encoding"
  - subtask_ref: unify-batch-ingest-and-shared-graph-projection
    title: "Unify cold/delta persist_files_impl onto MultiValueBatch, skip redundant schema_ready DELETE FROM/source_inventory rewrites, collapse populate_owned_with_store, share PreparedGraph between ingest.rs and seal.rs, parallelize seal.rs graph domain hashing, and remove mid-projection malloc_trim(0)"
  - subtask_ref: cleanup-pipeline-dead-tails-hooks-and-details
    title: "Remove BuildMode dead_tails, simplify sanitize_extracted_facts, clean up PipelineTestHooks and PayloadStore::stage, deduplicate NodeDetailKey probes, and reconcile build_query_roundtrip.rs"
```
