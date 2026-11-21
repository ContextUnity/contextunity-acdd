---
id: PF-003-delta-seal-cold-pipeline-and-storage-acceleration
title: "Delta FTS sealing fast path, single-encode RAM-staged cold pipeline, and parallel linker preparation"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - src/engine/pipeline.rs
  - crates/acdd-index/src/db/commitments.rs
  - crates/acdd-index/src/db/commitments/
  - crates/acdd-index/src/db/writer/
  - crates/acdd-index/src/linker/mod.rs
  - crates/acdd-index/src/linker/indexing.rs
  - crates/acdd-index/src/scanner/
  - benchmarks/delta_benchmark.py
  - tests/commitment_integrity/
  - tests/core/
  - tests/incremental/
  - tests/linker/
  - docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md
  - docs/adr/0009-incremental-delta-and-transitive-relinking.md
  - docs/architecture/indexing.md
  - docs/milestones/PF-003-delta-seal-cold-pipeline-and-storage-acceleration.md
invariants:
  - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Cold builds and incremental deltas over identical inputs produce zero lost code nodes, zero lost exact call pairs, zero lost resolved references, and bit-identical acdd-owner-sha256-v2 output_root commitments across cold build, extraction cache hit, and incremental delta.'
  - 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing skips FTS5 posting re-hashing for any FTS domain (node_search or doc_search) whose underlying indexed rows did not change, and never re-hashes unchanged FTS domains a second time inside verify_owners within the same write transaction.'
  - 'INV-PF003-BOUNDED-RAM-STAGING: Cold-build in-memory payload staging stays strictly bounded by a deterministic fraction of memory_budget under MemoryPermit and spills automatically to positioned payloads.bin when cumulative staged bytes exceed the in-RAM threshold.'
  - 'INV-PF003-SINGLE-ENCODE-EXTRACT: Cold extraction encodes each file TypedFacts payload at most once per worker pass without redundant triple serialization or unrequested disk fan-out.'
  - 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallelized linker preparation produces bit-identical sorted SymbolId assignments, graph edges, and Merkle commitments regardless of Rayon thread count.'
acceptance_criteria:
  - 'AC-1: Incremental 1-file delta (edit_1_py and edit_1_md) on commerce-release-update with a warm verified receipt completes in <= 750 ms wall median (down from ~3,000 ms warm / ~5,500 ms uncached benchmark) with 0 commitment mismatches and bit-identical output_root vs fresh cold build.'
  - 'AC-2: benchmarks/delta_benchmark.py preserves a valid verified receipt for cloned base databases so benchmark deltas measure real warm incremental latency rather than uncached full-DB verification.'
  - 'AC-3: Cold-build FTS sealing (seal_fts_ms) on commerce-release-update drops from ~911-1,009 ms to <= 250 ms while passing tests/commitment_integrity bit-for-bit against an independently forced fts5vocab digest and commitments::verify(&conn).'
  - 'AC-4: Cold build on commerce-release-update drops from 12.26 s wall median to <= 8.5 s (targeting ~6.5-8.0 s) by eliminating triple TypedFacts encoding, unnecessary payloads.bin disk round-trips within memory_budget, redundant double filesystem walks, and redundant full-file PRAGMA quick_check scans (retaining fast PRAGMA integrity_check(sqlite_schema) structural validation) on freshly created private single-process DBs.'
  - 'AC-5: Parallelized linker preparation and doc-scoped by_suffix indexing reduce link_ms on commerce-release-update while keeping deterministic output_root and zero regressions across cargo test --workspace --all-targets.'
  - 'AC-6: ADR 0008, ADR 0009, and docs/architecture/indexing.md reflect the updated delta sealing, RAM-first bounded staging, and parallel linker invariants and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
repositories: []
linked_to: []
---

# PF-003: Delta FTS Sealing Fast Path, Single-Encode RAM-Staged Cold Pipeline, and Parallel Linker Preparation

## Goal & Measured Baseline

Empirical profiling of v0.6.2 on the control workspace (`commerce-release-update`: `4,088` files, `69,948` nodes, `214,857` edges, `190.81 MiB` SQLite DB, 16-core `performance` governor; [v062-commerce-release-update.md](../../benchmarks/results/v062-commerce-release-update.md)) confirms that **76.3% of cold-build wall time (`~9.3 s` of `12.26 s`, `3.65×` average parallelism across 16 cores)** and **>80% of single-file incremental delta time (`3.0–5.5 s`)** remain in serial sealing, redundant serialization, and full-table maintenance scans:

1. **Incremental delta (`edit_1_md`: `5,551 ms`; `edit_1_py`: `5,416 ms`):**
   - `seal_owners` (`1,228 ms`) and `verify_owners` (`1,232 ms`) both run single-threaded `fts_digest` across all `node_search_postings` (`14.56 MiB`) and `doc_search_postings` **twice in one transaction** even when a body-only `.md` edit modifies zero `node_search` postings (or a `.py` edit modifies zero doc sections), and both re-read all `domain_commitments` rows across 12 domains.
   - `DELETE FROM coverage_evidence WHERE evidence_id NOT IN (...)` in [`persist_delta`](../../crates/acdd-index/src/db/writer/delta.rs) runs a 5-way full-table `UNION` scan (`~197 ms`) on every delta.
   - `benchmarks/delta_benchmark.py` copies `base_db` without a matching verified receipt, forcing [`open`](../../crates/acdd-index/src/db/reader.rs) to run a `~2.48 s` full-database `commitments::verify(&conn)` before delta even starts.
2. **Cold build (`12,257 ms` wall, `44.75 s` CPU):**
   - `seal_fts_ms` (`911–1,009 ms`) reads every posting row-by-row through `fts5vocab` FFI — **3.6–4× slower** than inserting the FTS5 index itself (`254 ms`).
   - `extraction_worker` serializes each file's `TypedFacts` **three times** (`extraction_cache.store` creating `4,088` individual disk files + `prepare_catalog_storage_with_facts` cloning every node + `encode_facts` Zstd), writes both blobs to `payloads.bin`, and reads/decodes them back (`restore_payload_ms = 246 ms`) even though all `local_facts` total `15.88 MiB` (`0.34%` of the `4.65 GiB` budget).
   - `dependency_manifests_for_build` and `stream_for_build` walk the entire workspace tree twice sequentially, and full-file `PRAGMA quick_check` (`416 ms`) re-scans all `190.8 MiB` of leaf pages on a freshly created private DB after index creation and FTS sealing have already traversed the B-trees.
   - Linker preparation (`link_ms = 2,538 ms`) builds interner tables, `by_name`/`by_qual`/`by_suffix`/`by_id`/`by_module`, namespace/inheritance/semantic indexes single-threaded, and populates `by_suffix` for every dot-segment of every node even when docs reference only a small symbol subset.

## Non-Goals & Negative Boundaries

- Do **not** change symbol extraction rules, language linkers, or resolution semantics (all coverage changes belong to `LS-003`).
- Do **not** weaken Merkle tamper detection or break cold-vs-delta `output_root` parity ([ADR 0008](../adr/0008-merkle-commitments-and-compressed-fact-storage.md), [ADR 0009](../adr/0009-incremental-delta-and-transitive-relinking.md)).
- Do **not** remove bounded `MemoryPermit` spilling to `payloads.bin` when staged bytes exceed the RAM budget threshold (`INV-PERF-STAGE-PAYLOAD-SPOOLING`).
- Do **not** run full benchmark suites autonomously without explicit user approval (`benchmarks/AGENTS.md`).

## Architectural Reconciliation

- **[ADR 0008](../adr/0008-merkle-commitments-and-compressed-fact-storage.md)**: Keep `ALGORITHM = "acdd-owner-sha256-v2"` bit-identical across cold builds and deltas. If FTS sealing acceleration requires any commitment-format change rather than preserving exact posting-stream digests, escalate an explicit ADR-0008 amendment decision on `acdd_blackboard` before merging.
- **[ADR 0009](../adr/0009-incremental-delta-and-transitive-relinking.md)**: Preserve closed-perimeter transitive invalidation, `TEMP TABLE` frontier joins, and atomic WAL-to-`DELETE` delta publication.

## Tasks in this milestone

### task: delta-seal-gc-and-receipt

```yaml
task_ref: delta-seal-gc-and-receipt
target: "Eliminate redundant FTS5 re-hashing and 5-way coverage_evidence full scans on incremental deltas while preserving bit-identical acdd-owner-sha256-v2 output_root parity and fixing warm receipt cloning in delta_benchmark.py"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/db/commitments.rs
  - crates/acdd-index/src/db/commitments/snapshot.rs
  - crates/acdd-index/src/db/writer/delta.rs
  - benchmarks/delta_benchmark.py
  - tests/commitment_integrity/cold_sealing.rs
  - tests/core/fact_persistence.rs
  - tests/incremental/single_pass_spool_and_parallel_delta.rs
invariants:
  - 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing re-hashes an FTS domain only when rows in that FTS domain were mutated and performs zero redundant second-pass FTS re-hashing inside verify_owners for the same transaction.'
  - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every incremental delta produces the exact same output_root as a fresh cold build and passes full commitments::verify(&conn).'
acceptance_criteria:
  - 'AC-1: Incremental delta skips node_search_postings hashing when an edit (such as a body-only Markdown edit) leaves node_search postings unchanged, re-hashes node_search_postings when Markdown heading/invariant or code edits change node_search postings, skips doc_search_postings hashing on code-only edits, and passes full post-commit commitments::verify(&conn) with zero mismatch.'
  - 'AC-2: Scoped coverage_evidence cleanup inspects only evidence_ids removed from affected owners rather than running a 5-table full UNION scan, leaving zero orphan rows referenced nowhere.'
  - 'AC-3: delta_benchmark.py establishes a valid verified receipt for cloned benchmark DBs via canonical reader admission before mutating targets and timing delta runs.'
status: planned
subtasks:
  - subtask_ref: dirty-fts-domain-tracking-in-delta
    title: "Track whether node_search and doc_search rows changed during persist_delta and pass dirty FTS domain flags into seal_owners"
  - subtask_ref: eliminate-redundant-verify-owners-double-hash
    title: "Avoid duplicate in-transaction FTS re-hashing and redundant domain_commitments full scans between seal_owners and verify_owners while preserving tamper-evident verification"
  - subtask_ref: scoped-coverage-evidence-orphan-gc
    title: "Replace 5-way full-table DELETE FROM coverage_evidence with affected-owner candidate evidence_id cleanup"
  - subtask_ref: delta-benchmark-warm-receipt-and-seam-tests
    title: "Update delta_benchmark.py receipt setup and extend table-driven commitment/delta parity tests proving bit-identical cold-vs-delta output_root"
```

### task: cold-fts-seal-and-verify-fastpath

```yaml
task_ref: cold-fts-seal-and-verify-fastpath
target: "Accelerate cold-build FTS5 commitment hashing without row-by-row fts5vocab FFI overhead while proving bit-for-bit acdd-owner-sha256-v2 parity against commitments::verify"
contract_revision: 1
depends_on:
  - delta-seal-gc-and-receipt
tags:
  - default
scope:
  - crates/acdd-index/src/db/commitments.rs
  - crates/acdd-index/src/db/commitments/snapshot.rs
  - crates/acdd-index/src/db/commitments/domain_leaves.rs
  - crates/acdd-index/src/db/commitments/coverage_leaves.rs
  - tests/commitment_integrity/parallel_sealing.rs
  - docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md
invariants:
  - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Fast-path cold FTS commitment digests match an independently forced fts5vocab digest and commitments::verify(&conn) byte-for-byte on single- and multi-language corpora including Unicode identifiers, separators, and contentless deletes.'
acceptance_criteria:
  - 'AC-1: Cold FTS sealing eliminates per-posting fts5vocab FFI cursor bottleneck while matching an independently forced fts5vocab digest and fts_digest output byte-for-byte across tests/commitment_integrity.'
  - 'AC-2: ADR 0008 documents the fast-path FTS sealing invariant and verification boundary.'
status: planned
subtasks:
  - subtask_ref: fast-canonical-fts-digest-path
    title: "Implement high-throughput canonical FTS posting digest computation (parallelized or direct tokenizer/batch stream) producing bit-identical digests to fts_digest"
  - subtask_ref: commitment-integrity-parity-matrix
    title: "Add table-driven parity tests in tests/commitment_integrity/parallel_sealing.rs verifying exact digest equality across Unicode, punctuation separators, deletions, and multi-column doc_search"
  - subtask_ref: update-adr-0008-fts-sealing
    title: "Reconcile docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md with the fast-path FTS commitment architecture"
```

### task: single-encode-ram-staging-and-single-walk

```yaml
task_ref: single-encode-ram-staging-and-single-walk
target: "Eliminate triple TypedFacts serialization, unnecessary disk spool round-trips within memory_budget, duplicate workspace filesystem walks, and redundant cold PRAGMA quick_check scans"
contract_revision: 1
depends_on:
  - delta-seal-gc-and-receipt
tags:
  - default
scope:
  - src/engine/pipeline.rs
  - crates/acdd-index/src/db/writer/mod.rs
  - crates/acdd-index/src/db/writer/cache.rs
  - crates/acdd-index/src/scanner/mod.rs
  - crates/acdd-index/src/scanner/discovery.rs
  - tests/core/build_query_roundtrip.rs
  - tests/core/scanner_guard_limits.rs
  - docs/architecture/indexing.md
invariants:
  - 'INV-PF003-BOUNDED-RAM-STAGING: Staged payloads remain in RAM only while cumulative bytes stay within the bounded budget slice and automatically spill to positioned payloads.bin once exceeded.'
  - 'INV-PF003-SINGLE-ENCODE-EXTRACT: Each cold-extracted file encodes TypedFacts once per worker pass rather than serializing separately for extraction cache, storage_rows, and local_facts.'
acceptance_criteria:
  - 'AC-1: Cold builds under default memory_budget avoid payloads.bin disk write/read round-trips while low-memory-budget tests still exercise positioned payloads.bin spooling cleanly.'
  - 'AC-2: Workspace manifest discovery and file streaming avoid redundant double tree walks during cold builds, and single-pass extraction avoids triple serialization.'
  - 'AC-3: Freshly constructed single-process cold DBs replace redundant full-file PRAGMA quick_check scans with fast schema-root structural verification (PRAGMA integrity_check(sqlite_schema)) before receipt publication while preserving corruption detection on candidate/open/recovery paths and zero leaked stage directories.'
  - 'AC-4: docs/architecture/indexing.md documents the RAM-first bounded staging threshold and single-walk cold pipeline.'
status: planned
subtasks:
  - subtask_ref: unify-typed-facts-encoding-and-cache-store
    title: "Encode TypedFacts once per cold-extracted file and reuse the canonical encoded payload across local_facts and bounded-decompression extraction cache persistence"
  - subtask_ref: ram-first-bounded-payload-staging
    title: "Keep staged file payloads in RAM up to a bounded memory_budget threshold and spill overflow to positioned payloads.bin under MemoryPermit"
  - subtask_ref: single-walk-manifest-discovery-and-quick-check-trim
    title: "Consolidate cold dependency manifest discovery with file scanning and replace full-file cold PRAGMA quick_check with PRAGMA integrity_check(sqlite_schema) on freshly committed private candidate DBs"
  - subtask_ref: staging-spill-and-roundtrip-tests
    title: "Extend table-driven tests in tests/core/build_query_roundtrip.rs and tests/core/scanner_guard_limits.rs covering both RAM-staged and budget-spilled cold builds"
```

### task: linker-parallel-prep-and-details-diet

```yaml
task_ref: linker-parallel-prep-and-details-diet
target: "Parallelize read-only linker preparation stages, restrict by_suffix indexing to referenced doc symbols, and trim redundant linker-only JSON fields from persisted nodes.details without breaking read queries or Merkle parity"
contract_revision: 1
depends_on:
  - delta-seal-gc-and-receipt
tags:
  - default
scope:
  - crates/acdd-index/src/linker/mod.rs
  - crates/acdd-index/src/linker/indexing.rs
  - crates/acdd-index/src/db/writer/ingest.rs
  - crates/acdd-index/src/db/writer/batch.rs
  - tests/linker/early_interning_and_compact_facts.rs
  - tests/core/compact_schema.rs
  - docs/adr/0009-incremental-delta-and-transitive-relinking.md
invariants:
  - 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallel linker preparation yields identical edge ordering, resolution counts, and output_root across 1-thread and N-thread Rayon pools.'
  - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every SQL reader, MCP tool, and delta hydration path preserves identical query outputs after any nodes.details compaction.'
acceptance_criteria:
  - 'AC-1: Independent linker preparation structures build concurrently with deterministic sorted assembly and pass 1-thread vs multi-thread determinism tests.'
  - 'AC-2: by_suffix indexes only candidate suffixes needed by active doc.referenced_symbols while preserving 100% of verified documentation edges.'
  - 'AC-3: Any trimmed linker-only keys in nodes.details are audited against all db/reader and MCP consumers and verified with identical cold-vs-delta output_root.'
status: planned
subtasks:
  - subtask_ref: parallel-linker-index-preparation
    title: "Parallelize read-only node/namespace/inheritance preparation passes in linker/mod.rs and linker/indexing.rs with deterministic sorted merges"
  - subtask_ref: doc-scoped-by-suffix-index
    title: "Collect distinct doc.referenced_symbols suffixes upfront and populate by_suffix only for matching candidates"
  - subtask_ref: audit-and-trim-nodes-details-json
    title: "Audit all live SQL readers of nodes.details and remove verified linker-only JSON bloat (such as redundant bindings/rebindings if unused on read path) in writer/ingest.rs"
  - subtask_ref: determinism-and-schema-seam-tests
    title: "Add table-driven determinism and schema-consumer tests in tests/linker/early_interning_and_compact_facts.rs and tests/core/compact_schema.rs and update ADR 0009"
```
