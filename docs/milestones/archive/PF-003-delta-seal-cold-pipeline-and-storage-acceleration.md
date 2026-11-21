---
id: PF-003-delta-seal-cold-pipeline-and-storage-acceleration
title: Delta FTS sealing fast path, single-encode RAM-staged cold pipeline, and parallel linker preparation
doc_type: contract
status: completed
owners:
- src/engine/pipeline.rs
- src/engine/pipeline/
- src/AGENTS.md
- crates/acdd-index/AGENTS.md
- crates/acdd-index/src/commitments.rs
- crates/acdd-index/src/models/
- crates/acdd-index/src/db/commitments.rs
- crates/acdd-index/src/db/commitments/
- crates/acdd-index/src/db/reader.rs
- crates/acdd-index/src/db/traversal.rs
- crates/acdd-index/src/db/writer/
- crates/acdd-index/src/linker/mod.rs
- crates/acdd-index/src/linker/indexing.rs
- crates/acdd-index/src/linker/traits.rs
- crates/acdd-index/src/languages/rust/linker.rs
- crates/acdd-index/src/languages/python/linker.rs
- crates/acdd-index/src/languages/typescript/linker.rs
- crates/acdd-index/src/scanner/
- benchmarks/delta_benchmark.py
- crates/acdd-state/src/store/sqlite/tx.rs
- src/cli/mod.rs
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
- 'INV-PF003-TYPED-CORE-WIRE-PARITY: Typed graph enums (EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId), NodeDetailFlags, CommitmentDomain/FtsDomain, and PayloadStore preserve 100% SQLite schema, serde JSON wire compatibility, and bit-identical acdd-owner-sha256-v2 output_root parity.'
- 'INV-PF003-MODULAR-SINGLE-OWNER: Every indexer subsystem keeps one canonical owner (INV-SINGLE-OWNER) — single &dyn FactsLookup LanguageLinker contract with zero _borrowed duplicates, canonical TempTableGuard and PayloadStore helpers in db::writer, and modular src/engine/pipeline/ decomposition with zero >= 10-line clones or dead_tails.'
acceptance_criteria:
- 'AC-1: Incremental 1-file delta (edit_1_py and edit_1_md) on commerce-release-update with a warm verified receipt completes in <= 750 ms wall median (down from ~3,000 ms warm / ~5,500 ms uncached benchmark) with 0 commitment mismatches and bit-identical output_root vs fresh cold build.'
- 'AC-2: benchmarks/delta_benchmark.py preserves a valid verified receipt for cloned base databases so benchmark deltas measure real warm incremental latency rather than uncached full-DB verification.'
- 'AC-3: Cold-build FTS sealing (seal_fts_ms) on commerce-release-update drops from ~911-1,009 ms to <= 250 ms while passing tests/commitment_integrity bit-for-bit against an independently forced fts5vocab digest and commitments::verify(&conn).'
- 'AC-4: Cold build on commerce-release-update drops from 12.26 s wall median to <= 8.5 s (targeting ~6.5-8.0 s) by eliminating triple TypedFacts encoding, unnecessary payloads.bin disk round-trips within memory_budget, redundant double filesystem walks, and redundant full-file PRAGMA quick_check scans (retaining fast PRAGMA integrity_check(sqlite_schema) structural validation) on freshly created private single-process DBs.'
- 'AC-5: Parallelized linker preparation and doc-scoped by_suffix indexing reduce link_ms on commerce-release-update while keeping deterministic output_root and zero regressions across cargo test --workspace --all-targets.'
- 'AC-6: ADR 0008, ADR 0009, docs/architecture/indexing.md, crates/acdd-index/AGENTS.md, and src/AGENTS.md reflect the updated delta sealing, RAM-first bounded staging, parallel linker, typed Layer-1 core, and modular pipeline invariants and pass acdd docs check and acdd validate.'
- 'AC-7: CompactTag::known is decomposed into canonical typed enums (EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId) plus NodeDetailFlags; LanguageLinker eliminates dual owned vs _borrowed methods via unified &dyn FactsLookup; and commitments/writer use typed CommitmentDomain, FtsDomain, TempTableGuard, and PayloadStore without breaking wire compatibility or output_root.'
- 'AC-8: src/engine/pipeline.rs (4,223 LOC) is decomposed into an idiomatic src/engine/pipeline/ directory module (budget, hooks, staging, cold, invalidation, hydration, delta, tests) reusing db::writer::TempTableGuard and PayloadStore with zero >= 10-line clones (eliminating the delta_inner HTML frontier duplication) and typed BuildMode/ColdScanResult seams.'
guidance:
  hitl: pro
required_skills:
- acdd
- contract-planning
- test-suite-rules
closure:
  status: completed
  closed_at: 2026-11-20T05:17:11.054Z
  target_base: run/PF-003
  target_base_commit: 24890e02bcd400dde37153e0086c705bf5463fed
  task_receipts:
    cold-fts-seal-and-verify-fastpath:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:cold-fts-seal-and-verify-fastpath.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 2
      candidate_tree: acf8ebc3d39dc8964378eff55d5b95e056e825ae
      committed_oid: 7c2d844e5072ac072cabd081151f519b244ea1a7
      accepted_at: 2026-11-20T01:26:46.839Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Fast-path cold FTS commitment digests match an independently forced fts5vocab digest and commitments::verify(&conn) byte-for-byte on single- and multi-language corpora including Unicode identifiers, separators, and contentless deletes.'
          verified: []
    delta-seal-gc-and-receipt:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:delta-seal-gc-and-receipt.deliver
      gate_id: deliver
      worker_id: coordinator
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 4126d565a71d4ff2b403c25d0715ef8be121757a
      committed_oid: 7d3e02a5e0bff481b67b58497cf7852c5a306228
      accepted_at: 2026-11-19T22:36:59.464Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing re-hashes an FTS domain only when rows in that FTS domain were mutated and performs zero redundant second-pass FTS re-hashing inside verify_owners for the same transaction.'
          - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every incremental delta produces the exact same output_root as a fresh cold build and passes full commitments::verify(&conn).'
          verified: []
    linker-parallel-prep-and-details-diet:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:linker-parallel-prep-and-details-diet.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 431c9b4fa612431c84a84a58f672ff3b7426f458
      committed_oid: 47f81fa69dae3c1dac4349c1ba4c5c09228d3623
      accepted_at: 2026-11-19T23:29:56.540Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallel linker preparation yields identical edge ordering, resolution counts, and output_root across 1-thread and N-thread Rayon pools.'
          - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every SQL reader, MCP tool, and delta hydration path preserves identical query outputs after any nodes.details compaction.'
          verified: []
    modularize-pipeline-and-payload-store:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:modularize-pipeline-and-payload-store.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 3
      candidate_tree: 7d153396122bc4b936d130a1417c5b94b7e3c86d
      committed_oid: d94f3ab073678db01ed54e0667e7a58cf644bd2c
      accepted_at: 2026-11-20T04:50:41.264Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-BOUNDED-RAM-STAGING: PayloadStore enforces bounded in-RAM staging under MemoryPermit and transparently spills overflow to positioned payloads.bin via StagedPayload::Spilled(SpoolSlice) with zero bit-packed offset sentinels.'
          - 'INV-PF003-MODULAR-SINGLE-OWNER: src/engine/pipeline/ delegates all temporary frontier SQL tables to db::writer::TempTableGuard, decomposes delta_inner into explicit phase helpers, and leaves zero >= 10-line clones or dead_tails.'
          verified: []
    single-encode-ram-staging-and-single-walk:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:single-encode-ram-staging-and-single-walk.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 2
      candidate_tree: d08eb20a3fc3138c7f3f5f169d24fd93cf8a6cee
      committed_oid: 32c9cedaa6d5da10e578881847b08920fe4178ba
      accepted_at: 2026-11-20T00:05:55.439Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-BOUNDED-RAM-STAGING: Staged payloads remain in RAM only while cumulative bytes stay within the bounded budget slice and automatically spill to positioned payloads.bin once exceeded.'
          - 'INV-PF003-SINGLE-ENCODE-EXTRACT: Each cold-extracted file encodes TypedFacts once per worker pass rather than serializing separately for extraction cache, storage_rows, and local_facts.'
          verified: []
    typed-commitments-and-sqlite-writer-guards:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:typed-commitments-and-sqlite-writer-guards.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 5801b32d20f86ce507a2118350b4f1a20d2d77ef
      committed_oid: dd62e1e4dee3b12df6cb02fce09ab7685e3bcaf6
      accepted_at: 2026-11-20T02:37:22.531Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-TYPED-CORE-WIRE-PARITY: CommitmentDomain, FtsDomain, FtsDirtySet, and CatalogStorageView preserve exact acdd-owner-sha256-v2 domain names, leaf keys, binary cache encoding, and bit-identical cold-vs-delta output_root.'
          - 'INV-PF003-MODULAR-SINGLE-OWNER: SQLite temporary table lifecycle (create, clear, batch populate, RAII drop) has one canonical owner in db::writer::temp_table::TempTableGuard consumed by writer/delta.rs and writer/entrypoints.rs, and the FTS5 binary segment decoder lives in db/commitments/fts5_fastpath.rs rather than bloating db/commitments.rs.'
          verified: []
    typed-graph-models-and-linker-catalog:
      run_id: PF-003-delta-seal-co-20261119212841-747613-1
      node_cursor: root:typed-graph-models-and-linker-catalog.deliver
      gate_id: deliver
      worker_id: coordinator-pf003
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 7eddde242e2786aeb1563f28282723e08825bd49
      committed_oid: 20193de5e975faca431a034b094fb93cd1e6d219
      accepted_at: 2026-11-20T02:36:19.620Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-PF003-TYPED-CORE-WIRE-PARITY: EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId, and NodeDetailFlags preserve exact JSON/SQLite string representations and bit-identical acdd-owner-sha256-v2 output_root commitments.'
          - 'INV-PF003-MODULAR-SINGLE-OWNER: LanguageLinker exposes one canonical package_exports and one canonical required_full_facts taking &dyn FactsLookup with zero _borrowed duplicate trait methods and zero anonymous boolean flag tuples in db/traversal.rs.'
          verified: []
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing re-hashes an FTS domain only when rows in that FTS domain were mutated and performs zero redundant second-pass FTS re-hashing inside verify_owners for the same transaction.'
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every incremental delta produces the exact same output_root as a fresh cold build and passes full commitments::verify(&conn).'
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Fast-path cold FTS commitment digests match an independently forced fts5vocab digest and commitments::verify(&conn) byte-for-byte on single- and multi-language corpora including Unicode identifiers, separators, and contentless deletes.'
      - 'INV-PF003-BOUNDED-RAM-STAGING: Staged payloads remain in RAM only while cumulative bytes stay within the bounded budget slice and automatically spill to positioned payloads.bin once exceeded.'
      - 'INV-PF003-SINGLE-ENCODE-EXTRACT: Each cold-extracted file encodes TypedFacts once per worker pass rather than serializing separately for extraction cache, storage_rows, and local_facts.'
      - 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallel linker preparation yields identical edge ordering, resolution counts, and output_root across 1-thread and N-thread Rayon pools.'
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every SQL reader, MCP tool, and delta hydration path preserves identical query outputs after any nodes.details compaction.'
      - 'INV-PF003-TYPED-CORE-WIRE-PARITY: EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId, and NodeDetailFlags preserve exact JSON/SQLite string representations and bit-identical acdd-owner-sha256-v2 output_root commitments.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: LanguageLinker exposes one canonical package_exports and one canonical required_full_facts taking &dyn FactsLookup with zero _borrowed duplicate trait methods and zero anonymous boolean flag tuples in db/traversal.rs.'
      - 'INV-PF003-TYPED-CORE-WIRE-PARITY: CommitmentDomain, FtsDomain, FtsDirtySet, and CatalogStorageView preserve exact acdd-owner-sha256-v2 domain names, leaf keys, binary cache encoding, and bit-identical cold-vs-delta output_root.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: SQLite temporary table lifecycle (create, clear, batch populate, RAII drop) has one canonical owner in db::writer::temp_table::TempTableGuard consumed by writer/delta.rs and writer/entrypoints.rs, and the FTS5 binary segment decoder lives in db/commitments/fts5_fastpath.rs rather than bloating db/commitments.rs.'
      - 'INV-PF003-BOUNDED-RAM-STAGING: PayloadStore enforces bounded in-RAM staging under MemoryPermit and transparently spills overflow to positioned payloads.bin via StagedPayload::Spilled(SpoolSlice) with zero bit-packed offset sentinels.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: src/engine/pipeline/ delegates all temporary frontier SQL tables to db::writer::TempTableGuard, decomposes delta_inner into explicit phase helpers, and leaves zero >= 10-line clones or dead_tails.'
      verified: []
    decisions:
    - |-
      PF-003 Architectural Blueprint Decision (ADR-0008 & ADR-0009 Reconciliation):

      1. ADR-0008 & ADR-0009 Reconciliation (No Merkle format change; adr_required=false, docs reconciled in AC-6):
      - ALGORITHM='acdd-owner-sha256-v2' remains 100% bit-identical across cold builds, extraction cache hits, and incremental deltas.
      - Task 1 (delta-seal-gc-and-receipt, Wave 0):
        * Dirty FTS tracking in crates/acdd-index/src/db/writer/delta.rs: Compare per-file node_search rows (computed via existing pub(super) ingest::write_node_search_text on old local_facts cross-checked against DB nodes count/ids vs new facts) and doc_sections rows before/after replace_files. Because Markdown files also emit document/invariant nodes into node_search, body-only .md edits and comment-only .py edits yield identical (search_id, search_text) sets -> FtsDirtyDomains { node_search: false, doc_search: true/false }, skipping unchanged FTS5 posting re-hashing while failing closed (re-hashing) whenever headings/symbols/rows actually change or stored domain_commitments rows are absent. Zero edits to writer/ingest.rs (keeps Wave 1 disjoint).
        * Eliminate redundant in-transaction double-hash in crates/acdd-index/src/db/commitments.rs + delta.rs: Add pub(crate) FtsDirtyDomains, seal_owners_with_dirty_fts (loading domain_commitments once across domains instead of 13 full scans), and verify_delta_seal (verifying metadata.commitment_algorithm, metadata.output_root, all 17 owner_id=0 domain_commitments digests, and root(&tx)==seal without re-running fts_digest or re-scanning leaves in the same IMMEDIATE transaction). Public seal_owners/verify_owners/verify retain full verification semantics.
        * Scoped coverage_evidence orphan GC in delta.rs: Capture candidate_orphan_evidence(evidence_id) from affected edges/edge_occurrences/resolution_coverage before deletes; prune IDs re-inserted for affected rows after persist_graph + INSERT INTO edges; if empty (common case), skip all table scans (0ms); only if non-empty, filter against remaining references and delete true orphans (0 schema/index additions).
        * Warm receipt in benchmarks/delta_benchmark.py: Re-bind base_db.verified.json to each cloned run_db (updating database path and identity.main from os.lstat with wal=None, mode 0o600) so delta measures real warm incremental latency rather than uncached full-DB verify.

      2. Task 2 (cold-fts-seal-and-verify-fastpath, Wave 1, depends on Task 1):
      - Accelerate canonical fts_digest in crates/acdd-index/src/db/commitments.rs (+ snapshot.rs) via a pure-Rust SQLite FTS5 *_data B-tree segment reader + deterministic k-way term merge emitting postings in exact canonical (term, identity, col, offset) order, eliminating ~3.5M per-posting fts5vocab FFI transitions while still validating *_config and *_docsize via validate_fts_docsize.
      - Fail closed to existing fts5vocab streaming/sorted path whenever FTS5 version/config/detail/delete-tombstone structure is unsupported or unexpected.
      - Because the fast path lives in canonical fts_digest (INV-SINGLE-OWNER), cold builds (seal_fts_ms <= 250ms), dirty-FTS deltas, and full commitments::verify all accelerate while remaining bit-identical.
      - Note: Extend scope to tests/commitment_integrity/cold_sealing.rs (observe_read only) to map b'node_search_data' -> 'node_search_postings' and b'doc_search_data' -> 'doc_search_postings' alongside *_commit_vocab without weakening any assertion.

      3. Task 3 (single-encode-ram-staging-and-single-walk, Wave 1, depends on Task 1):
      - Single-encode TypedFacts (INV-PF003-SINGLE-ENCODE-EXTRACT): Add MAGIC_V5 ('ACDD-EXTRACTION-FACTS\0\x05') + store_encoded/load_encoded to ExtractionCache (cache.rs) storing the already Zstd-compressed local_facts payload (preserving MAGIC_V4 read/test compatibility). In extraction_worker (pipeline.rs), encode TypedFacts once via encode_facts(&facts), reuse for local_facts staging and extraction_cache.store_encoded, and avoid cloning all Node structs in prepare_catalog_rows (writer/mod.rs).
      - Bounded RAM-first payload staging (INV-PF003-BOUNDED-RAM-STAGING): Keep staged payloads in RAM up to a deterministic fraction of memory_budget (e.g. (memory_budget / 16).min(128 MiB), no floor above budget/16 so low-budget tests spill cleanly) and lazily create/spill overflow to positioned payloads.bin via write_at/read_at under MemoryPermit. Preserve public StagedFilePayload/StagedPayloadBlob compatibility for existing tests (tests/incremental/single_pass_spool_and_parallel_delta.rs) and preserve INV-PERF-BOUNDED-STAGING-AND-TRIM (malloc_trim at boundaries, <800 MiB RSS).
      - Single-walk cold discovery (scanner/mod.rs + pipeline.rs): Add scanner::collect_for_build sharing walk_candidate_paths to return (Vec<BuildCandidate>, Vec<PathBuf>, BuildStreamReport) in one walk, eliminating double workspace traversal while preserving the 3-thread ByteBudget producer-worker-consumer pipeline and PipelineTestHooks::overlap. Also skip unnecessary language() calls inside manifests-only dependency_manifests_for_build.
      - Trim redundant cold PRAGMA quick_check on freshly created private stage DB in atomic_build (report verify_ms=0.0); preserve validate_database_integrity on untrusted open/recovery paths.

      4. Task 4 (linker-parallel-prep-and-details-diet, Wave 1, depends on Task 1):
      - Doc-scoped by_suffix (linker/mod.rs): Collect wanted_suffixes from BOTH doc.referenced_symbols AND document node.details['symbol_claims'][*]['name'] (normalized :: -> .) for active owners, and populate by_suffix only for matching qualname suffixes on non-doc/invariant/component nodes.
      - Deterministic parallel linker preparation (linker/mod.rs + linker/indexing.rs): Parallelize independent read-only index builds (by_name/by_qual vs by_id/by_module/by_suffix vs node_bindings/node_rebindings via rayon::join), per-file normalized_imports via par_iter alongside modules_by_namespace via rayon::join, and PythonReceivers::build alongside RustMembers::build via rayon::join. Preserve deterministic BTree/slice ordering and INV-PERF-NO-NESTED-PAR-ITER (do not touch ValueFlowIndex::build_parallel initial evaluate).
      - Audited nodes.details diet (writer/ingest.rs): Live consumer audit proves param_types (db/symbols/search.rs:76, pipeline.rs:1884) and bindings/rebindings (delta hydration pipeline.rs:3678-3711 -> push_node -> linker shadowing checks, plus tests/core/storage_batches/catalog_semantics.rs:386-458) ARE actively consumed on the read/delta path and MUST NOT be stripped (INV-PF003-ZERO-COVERAGE-REGRESSION). Fix the real memory/allocation waste in writer/ingest.rs (reusable_detail_buffers accumulates flushed Vec<u8> buffers without ever reusing or dropping them) and preserve ingest::persist_catalog_files/LoadedCatalogFile signatures for Wave 1 disjointness with Task 3.
    - |-
      PF-003 Architectural Blueprint Decision v2 (Remediating Review Findings #128; supersedes #126):

      1. Finding 1 Remediation (Bundled SQLite FTS5 rowids & independent fts5vocab oracle in cold-fts-seal-and-verify-fastpath):
      - Bundled SQLite FTS5 stores the averages record at _data.id = 1 (FTS5_AVERAGES_ROWID = 1) and the structure record at _data.id = 10 (FTS5_STRUCTURE_ROWID = 10), with segment leaves addressed via FTS5_SEGMENT_ROWID(segid, pgno) and _idx.
      - Because both sealing and public commitments::verify call fts_digest, self-agreement does not prove compatibility. Keep fts_digest_vocab(conn, table, order_by) (the canonical fts5vocab('instance') implementation) as an explicit independent oracle and expose a fast-path execution probe so tests/commitment_integrity/parallel_sealing.rs asserts BOTH: (a) the _data/_idx fast path actually executes (returns Some(digest) without falling back) on valid indexes, and (b) its digest matches the independently forced fts_digest_vocab byte-for-byte across Unicode identifiers, custom separators (._-/), multi-column doc_search, contentless deletes (node_search tombstones), external-content deletes (doc_search), multi-page overflow posting lists, malformed _data/_docsize blobs (fail-closed rejection/fallback), and token-count-preserving posting mutations (e.g. swapping equal-count terms while preserving *_docsize totals).

      2. Finding 2 Remediation (Markdown document/invariant nodes in node_search & delta-seal-gc-and-receipt AC-1 alignment):
      - Reconciled docs/milestones/PF-003-delta-seal-cold-pipeline-and-storage-acceleration.md (and ran acdd sync --run pf-003 + acdd validate): Markdown files emit kind='document' and kind='invariant' nodes into node_search. Therefore, delta skips node_search_postings hashing when an edit leaves node_search postings unchanged (e.g. a body-only Markdown edit where only doc_sections content/size/mtime change, or a code edit where per-node search_text is unchanged), re-hashes node_search_postings when Markdown heading/invariant edits or code edits mutate node_search postings, and skips doc_search_postings on code-only edits.
      - Verification seam in tests/incremental/single_pass_spool_and_parallel_delta.rs and tests/commitment_integrity/cold_sealing.rs proves both the body-only Markdown skip of node_search_postings and the heading/invariant Markdown rehash of node_search_postings, verifying 100% cold output_root parity, full commitments::verify(&conn), and independent fts_digest_vocab agreement.

      3. Finding 3 Remediation (Bounded Zstd decompression on MAGIC_V5 ExtractionCache in single-encode-ram-staging-and-single-walk):
      - In crates/acdd-index/src/db/writer/cache.rs and crates/acdd-index/src/typed_facts/mod.rs, do not pass size-limited compressed MAGIC_V5 payloads to an unbounded read_to_end decompression. Record and validate the uncompressed byte length in MAGIC_V5 and enforce an explicit expanded-size cap decode_facts_bounded(blob, max_payload_bytes) using decoder.take(max_payload_bytes.saturating_add(1)).read_to_end(&mut raw), treating any oversized, truncated, checksum-mismatched, or invalid payload as a clean cache miss (None).
      - Verification seam covers MAGIC_V4 backward compatibility, MAGIC_V5 cold/delta output_root parity, Markdown section.mtime refresh on MAGIC_V5 cache hits (re-encoding local_facts when file.is_doc so doc_sections and local_facts leaves match uncached cold builds), and oversized decompressed payload rejection.

      4. Finding 4 Remediation (Structural integrity check before cold verified-receipt publication in single-encode-ram-staging-and-single-walk):
      - In src/engine/pipeline.rs (atomic_build), do not drop structural B-tree verification completely before publishing a verified receipt. Replace the 416ms full-file PRAGMA quick_check with PRAGMA integrity_check(sqlite_schema) (plus verifying non-zero page_count and journal_mode=DELETE transition after checkpoint) on the freshly built single-process candidate DB before acquiring the generation lock and calling cache::publish_verified. Since CREATE INDEX (all 16 secondary indexes), fts_commitment_digests, and install_snapshot have just traversed the table and FTS B-trees in the same private stage connection, PRAGMA integrity_check(sqlite_schema) verifies root/schema B-tree integrity in <1ms while failing closed ('candidate database failed integrity check: ...') on corrupted candidate headers/schemas before publication or receipt emission.
      - Verification seam in tests/core/build_query_roundtrip.rs exercises a structurally corrupted candidate/published DB through publication and reader::open admission, proving fail-closed rejection and refusal to trust or retain a verified receipt.

      5. Finding 5 Remediation (Canonical warm receipt admission in benchmarks/delta_benchmark.py):
      - Forbid manual Python copying or rewriting of .verified.json file identities (ino/mtime/size) in benchmarks/delta_benchmark.py. After shutil.copy2(base_db, run_db) and BEFORE mutating target files or timing acdd delta, execute an untimed canonical ACDD reader-admission command (run_cli_json with subcommand=['query', 'overview', '--aspects', 'metadata'], db=run_db) which runs reader::open -> commitments::verify(&conn) -> cache::publish_verified_identity, and assert that f'{run_db}.verified.json' exists before starting the timed delta pass.

      6. Finding 6 Remediation (Concrete timing/benchmark protocol & >=2,048-node parallel linker determinism seam):
      - Set explicit parallel linker preparation thresholds PARALLEL_LINKER_PREP_MIN_NODES = 2048 and PARALLEL_LINKER_PREP_MIN_FILES = 32 in crates/acdd-index/src/linker/{mod,indexing}.rs, and test 1-thread vs 4-thread rayon::ThreadPoolBuilder determinism in tests/linker/early_interning_and_compact_facts.rs on a fixture with >= 2,048 nodes across >= 32 files (covering Python, Rust, and Markdown referenced_symbols + symbol_claims), asserting bit-identical SymbolId assignments, sorted graph.edges, sorted graph.coverage, and output_root.
      - Bind AC-1, AC-3, AC-4, and AC-5 timing criteria explicitly to their production operations, fixture cardinality (commerce-release-update: 4,088 files, 69,948 nodes, 214,857 edges, 2,874 doc sections), warmup policy, sample method (repeats=5, wall/phase median), receipt preconditions, and regression mutations.
    - 'PF-003 Coordinator Authority Decision: crates/acdd-state/src/store/sqlite/tx.rs (FK ordering fix for acdd sync) and src/cli/mod.rs (legacy snapshot test reconciliation) are officially admitted into PF-003 run scope and recorded in owners list (commit af13b8a6).'
  metrics_summary:
    decisions_count: 3
    deferred_findings_count: 0
    failed_traces: 4
    passed_traces: 21
    task_receipts_count: 7
    tasks_cancelled: 0
    tasks_completed: 7
    tasks_total: 7
    total_claims: 50
    total_review_cycles: 8
    total_traces: 27
---

# PF-003: Delta FTS Sealing Fast Path, Single-Encode RAM-Staged Cold Pipeline, and Parallel Linker Preparation

## Goal & Measured Baseline

Empirical profiling of v0.6.2 on the control workspace (`commerce-release-update`: `4,088` files, `69,948` nodes, `214,857` edges, `190.81 MiB` SQLite DB, 16-core `performance` governor; [v062-commerce-release-update.md](../../../benchmarks/results/v062-commerce-release-update.md)) confirms that **76.3% of cold-build wall time (`~9.3 s` of `12.26 s`, `3.65×` average parallelism across 16 cores)** and **>80% of single-file incremental delta time (`3.0–5.5 s`)** remain in serial sealing, redundant serialization, and full-table maintenance scans:

1. **Incremental delta (`edit_1_md`: `5,551 ms`; `edit_1_py`: `5,416 ms`):**
   - `seal_owners` (`1,228 ms`) and `verify_owners` (`1,232 ms`) both run single-threaded `fts_digest` across all `node_search_postings` (`14.56 MiB`) and `doc_search_postings` **twice in one transaction** even when a body-only `.md` edit modifies zero `node_search` postings (or a `.py` edit modifies zero doc sections), and both re-read all `domain_commitments` rows across 12 domains.
   - `DELETE FROM coverage_evidence WHERE evidence_id NOT IN (...)` in [`persist_delta`](../../../crates/acdd-index/src/db/writer/delta.rs) runs a 5-way full-table `UNION` scan (`~197 ms`) on every delta.
   - `benchmarks/delta_benchmark.py` copies `base_db` without a matching verified receipt, forcing [`open`](../../../crates/acdd-index/src/db/reader.rs) to run a `~2.48 s` full-database `commitments::verify(&conn)` before delta even starts.
2. **Cold build (`12,257 ms` wall, `44.75 s` CPU):**
   - `seal_fts_ms` (`911–1,009 ms`) reads every posting row-by-row through `fts5vocab` FFI — **3.6–4× slower** than inserting the FTS5 index itself (`254 ms`).
   - `extraction_worker` serializes each file's `TypedFacts` **three times** (`extraction_cache.store` creating `4,088` individual disk files + `prepare_catalog_storage_with_facts` cloning every node + `encode_facts` Zstd), writes both blobs to `payloads.bin`, and reads/decodes them back (`restore_payload_ms = 246 ms`) even though all `local_facts` total `15.88 MiB` (`0.34%` of the `4.65 GiB` budget).
   - `dependency_manifests_for_build` and `stream_for_build` walk the entire workspace tree twice sequentially, and full-file `PRAGMA quick_check` (`416 ms`) re-scans all `190.8 MiB` of leaf pages on a freshly created private DB after index creation and FTS sealing have already traversed the B-trees.
   - Linker preparation (`link_ms = 2,538 ms`) builds interner tables, `by_name`/`by_qual`/`by_suffix`/`by_id`/`by_module`, namespace/inheritance/semantic indexes single-threaded, and populates `by_suffix` for every dot-segment of every node even when docs reference only a small symbol subset.

## Non-Goals & Negative Boundaries

- Do **not** change symbol extraction rules, language linkers, or resolution semantics (all coverage changes belong to `LS-003`).
- Do **not** weaken Merkle tamper detection or break cold-vs-delta `output_root` parity ([ADR 0008](../../adr/0008-merkle-commitments-and-compressed-fact-storage.md), [ADR 0009](../../adr/0009-incremental-delta-and-transitive-relinking.md)).
- Do **not** remove bounded `MemoryPermit` spilling to `payloads.bin` when staged bytes exceed the RAM budget threshold (`INV-PERF-STAGE-PAYLOAD-SPOOLING`).
- Do **not** run full benchmark suites autonomously without explicit user approval (`benchmarks/AGENTS.md`).

## Architectural Reconciliation

- **[ADR 0008](../../adr/0008-merkle-commitments-and-compressed-fact-storage.md)**: Keep `ALGORITHM = "acdd-owner-sha256-v2"` bit-identical across cold builds and deltas. If FTS sealing acceleration requires any commitment-format change rather than preserving exact posting-stream digests, escalate an explicit ADR-0008 amendment decision on `acdd_blackboard` before merging.
- **[ADR 0009](../../adr/0009-incremental-delta-and-transitive-relinking.md)**: Preserve closed-perimeter transitive invalidation, `TEMP TABLE` frontier joins, and atomic WAL-to-`DELETE` delta publication.
- **Layer 1 Architectural Foundation (Feeds `LS-003` Layer 2)**: Immediately after Wave 1 lands, Waves 2 and 3 establish typed, single-owner core primitives (`models::{tags, details}`, unified `&dyn FactsLookup` `LanguageLinker`, `CommitmentDomain`/`FtsDomain`, `TempTableGuard`, `PayloadStore`, and modular `src/engine/pipeline/`) so `LS-003` can extend `LanguageId`, `NodeKind`, and `LanguageLinker` directly without duplicating stringly-typed tables or dual `_borrowed` methods. Protected files (`Cargo.toml`, `crates/*/Cargo.toml`, `.acdd/profile.yaml`) remain strictly out of scope for `PF-003`.

## Tasks in this milestone

### task: delta-seal-gc-and-receipt

```yaml
task_ref: delta-seal-gc-and-receipt
target: Eliminate redundant FTS5 re-hashing and 5-way coverage_evidence full scans on incremental deltas while preserving bit-identical acdd-owner-sha256-v2 output_root parity and fixing warm receipt cloning in delta_benchmark.py
scope:
- crates/acdd-index/src/db/commitments.rs
- crates/acdd-index/src/db/commitments/snapshot.rs
- crates/acdd-index/src/db/writer/delta.rs
- benchmarks/delta_benchmark.py
- crates/acdd-state/src/store/sqlite/tx.rs
- src/cli/mod.rs
- tests/commitment_integrity/cold_sealing.rs
- tests/core/fact_persistence.rs
- tests/incremental/single_pass_spool_and_parallel_delta.rs
tags:
- default
invariants:
- 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing re-hashes an FTS domain only when rows in that FTS domain were mutated and performs zero redundant second-pass FTS re-hashing inside verify_owners for the same transaction.'
- 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every incremental delta produces the exact same output_root as a fresh cold build and passes full commitments::verify(&conn).'
acceptance_criteria:
- 'AC-1: Incremental delta skips node_search_postings hashing when an edit (such as a body-only Markdown edit) leaves node_search postings unchanged, re-hashes node_search_postings when Markdown heading/invariant or code edits change node_search postings, skips doc_search_postings hashing on code-only edits, and passes full post-commit commitments::verify(&conn) with zero mismatch.'
- 'AC-2: Scoped coverage_evidence cleanup inspects only evidence_ids removed from affected owners rather than running a 5-table full UNION scan, leaving zero orphan rows referenced nowhere.'
- 'AC-3: delta_benchmark.py establishes a valid verified receipt for cloned benchmark DBs via canonical reader admission before mutating targets and timing delta runs.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: dirty-fts-domain-tracking-in-delta
  title: Track whether node_search and doc_search rows changed during persist_delta and pass dirty FTS domain flags into seal_owners
  status: completed
  evidence: FtsDirtyDomains in crates/acdd-index/src/db/writer/delta.rs & commitments.rs; verified by cold_and_delta_match_commitments_after_spool_and_parallel_delta
- subtask_ref: eliminate-redundant-verify-owners-double-hash
  title: Avoid duplicate in-transaction FTS re-hashing and redundant domain_commitments full scans between seal_owners and verify_owners while preserving tamper-evident verification
  status: completed
  evidence: seal_owners_with_dirty_fts (canonical decode_stored_digest) + verify_delta_seal in commitments.rs; passed R2 review
- subtask_ref: scoped-coverage-evidence-orphan-gc
  title: Replace 5-way full-table DELETE FROM coverage_evidence with affected-owner candidate evidence_id cleanup
  status: completed
  evidence: candidate_orphan_evidence pruned by affected owner_path_id rows first with empty short-circuit in writer/delta.rs
- subtask_ref: delta-benchmark-warm-receipt-and-seam-tests
  title: Update delta_benchmark.py receipt setup and extend table-driven commitment/delta parity tests proving bit-identical cold-vs-delta output_root
  status: completed
  evidence: delta_benchmark.py warm receipt via query overview + table-driven matrix in single_pass_spool_and_parallel_delta.rs
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:delta-seal-gc-and-receipt.deliver
  gate_id: deliver
  worker_id: coordinator
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 4126d565a71d4ff2b403c25d0715ef8be121757a
  committed_oid: 7d3e02a5e0bff481b67b58497cf7852c5a306228
  accepted_at: 2026-11-19T22:36:59.464Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-INCREMENTAL-FTS-SKIP: Delta sealing re-hashes an FTS domain only when rows in that FTS domain were mutated and performs zero redundant second-pass FTS re-hashing inside verify_owners for the same transaction.'
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every incremental delta produces the exact same output_root as a fresh cold build and passes full commitments::verify(&conn).'
      verified: []
```

### task: cold-fts-seal-and-verify-fastpath

```yaml
task_ref: cold-fts-seal-and-verify-fastpath
target: Accelerate cold-build FTS5 commitment hashing without row-by-row fts5vocab FFI overhead while proving bit-for-bit acdd-owner-sha256-v2 parity against commitments::verify
scope:
- crates/acdd-index/src/db/commitments.rs
- crates/acdd-index/src/db/commitments/snapshot.rs
- crates/acdd-index/src/db/commitments/domain_leaves.rs
- crates/acdd-index/src/db/commitments/coverage_leaves.rs
- tests/commitment_integrity/parallel_sealing.rs
- docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md
extended_scope:
- path: tests/commitment_integrity/cold_sealing.rs
  reason: 'Observe FTS5 *_data and *_idx fast-path shadow table reads in cold_sealing.rs SQLite authorizer observer per blueprint #126/#129'
depends_on:
- delta-seal-gc-and-receipt
tags:
- default
invariants:
- 'INV-PF003-ZERO-COVERAGE-REGRESSION: Fast-path cold FTS commitment digests match an independently forced fts5vocab digest and commitments::verify(&conn) byte-for-byte on single- and multi-language corpora including Unicode identifiers, separators, and contentless deletes.'
acceptance_criteria:
- 'AC-1: Cold FTS sealing eliminates per-posting fts5vocab FFI cursor bottleneck while matching an independently forced fts5vocab digest and fts_digest output byte-for-byte across tests/commitment_integrity.'
- 'AC-2: ADR 0008 documents the fast-path FTS sealing invariant and verification boundary.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: fast-canonical-fts-digest-path
  title: Implement high-throughput canonical FTS posting digest computation (parallelized or direct tokenizer/batch stream) producing bit-identical digests to fts_digest
  status: completed
  evidence: Fast digest matches vocabulary and falls back on tombstones and malformed data; integrity suite passed 18 tests.
- subtask_ref: commitment-integrity-parity-matrix
  title: Add table-driven parity tests in tests/commitment_integrity/parallel_sealing.rs verifying exact digest equality across Unicode, punctuation separators, deletions, and multi-column doc_search
  status: completed
  evidence: Integrity suite passed 18 tests including Unicode, columns, segments, overflow, tombstones and malformed blobs.
- subtask_ref: update-adr-0008-fts-sealing
  title: Reconcile docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md with the fast-path FTS commitment architecture
  status: completed
  evidence: ADR 0008 documents canonical FTS sealing and fallback.
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:cold-fts-seal-and-verify-fastpath.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 2
  candidate_tree: acf8ebc3d39dc8964378eff55d5b95e056e825ae
  committed_oid: 7c2d844e5072ac072cabd081151f519b244ea1a7
  accepted_at: 2026-11-20T01:26:46.839Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Fast-path cold FTS commitment digests match an independently forced fts5vocab digest and commitments::verify(&conn) byte-for-byte on single- and multi-language corpora including Unicode identifiers, separators, and contentless deletes.'
      verified: []
```

### task: single-encode-ram-staging-and-single-walk

```yaml
task_ref: single-encode-ram-staging-and-single-walk
target: Eliminate triple TypedFacts serialization, unnecessary disk spool round-trips within memory_budget, duplicate workspace filesystem walks, and redundant cold PRAGMA quick_check scans
scope:
- src/engine/pipeline.rs
- crates/acdd-index/src/db/writer/mod.rs
- crates/acdd-index/src/db/writer/cache.rs
- crates/acdd-index/src/scanner/mod.rs
- crates/acdd-index/src/scanner/discovery.rs
- tests/core/build_query_roundtrip.rs
- tests/core/scanner_guard_limits.rs
- docs/architecture/indexing.md
extended_scope:
- path: crates/acdd-index/src/db/writer/cache/tests.rs
  reason: Allow extending existing unit test matrix for ExtractionCache MAGIC_V4/MAGIC_V5 bounded decompression in cache/tests.rs
depends_on:
- delta-seal-gc-and-receipt
tags:
- default
invariants:
- 'INV-PF003-BOUNDED-RAM-STAGING: Staged payloads remain in RAM only while cumulative bytes stay within the bounded budget slice and automatically spill to positioned payloads.bin once exceeded.'
- 'INV-PF003-SINGLE-ENCODE-EXTRACT: Each cold-extracted file encodes TypedFacts once per worker pass rather than serializing separately for extraction cache, storage_rows, and local_facts.'
acceptance_criteria:
- 'AC-1: Cold builds under default memory_budget avoid payloads.bin disk write/read round-trips while low-memory-budget tests still exercise positioned payloads.bin spooling cleanly.'
- 'AC-2: Workspace manifest discovery and file streaming avoid redundant double tree walks during cold builds, and single-pass extraction avoids triple serialization.'
- 'AC-3: Freshly constructed single-process cold DBs replace redundant full-file PRAGMA quick_check scans with fast schema-root structural verification (PRAGMA integrity_check(sqlite_schema)) before receipt publication while preserving corruption detection on candidate/open/recovery paths and zero leaked stage directories.'
- 'AC-4: docs/architecture/indexing.md documents the RAM-first bounded staging threshold and single-walk cold pipeline.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: unify-typed-facts-encoding-and-cache-store
  title: Encode TypedFacts once per cold-extracted file and reuse the canonical encoded payload across local_facts and bounded-decompression extraction cache persistence
  status: completed
  evidence: V4 and bounded V5 cache tests pass; warm cache delta matches cold rebuild.
- subtask_ref: ram-first-bounded-payload-staging
  title: Keep staged file payloads in RAM up to a bounded memory_budget threshold and spill overflow to positioned payloads.bin under MemoryPermit
  status: completed
  evidence: Budget matrix proves disk spill and RAM staging produce equal verified output roots.
- subtask_ref: single-walk-manifest-discovery-and-quick-check-trim
  title: Consolidate cold dependency manifest discovery with file scanning and replace full-file cold PRAGMA quick_check with PRAGMA integrity_check(sqlite_schema) on freshly committed private candidate DBs
  status: completed
  evidence: Scanner parity and limits pass; candidate schema integrity, page count and DELETE mode checks are in place.
- subtask_ref: staging-spill-and-roundtrip-tests
  title: Extend table-driven tests in tests/core/build_query_roundtrip.rs and tests/core/scanner_guard_limits.rs covering both RAM-staged and budget-spilled cold builds
  status: completed
  evidence: Extended in-process RAM-vs-spill matrix and sqlite_schema corruption tests in tests/core/build_query_roundtrip.rs, tests/core/scanner_guard_limits.rs, and crates/acdd-index/src/db/writer/cache/tests.rs; verified in review R3 (#60).
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:single-encode-ram-staging-and-single-walk.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 2
  candidate_tree: d08eb20a3fc3138c7f3f5f169d24fd93cf8a6cee
  committed_oid: 32c9cedaa6d5da10e578881847b08920fe4178ba
  accepted_at: 2026-11-20T00:05:55.439Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-BOUNDED-RAM-STAGING: Staged payloads remain in RAM only while cumulative bytes stay within the bounded budget slice and automatically spill to positioned payloads.bin once exceeded.'
      - 'INV-PF003-SINGLE-ENCODE-EXTRACT: Each cold-extracted file encodes TypedFacts once per worker pass rather than serializing separately for extraction cache, storage_rows, and local_facts.'
      verified: []
```

### task: linker-parallel-prep-and-details-diet

```yaml
task_ref: linker-parallel-prep-and-details-diet
target: Parallelize read-only linker preparation stages, restrict by_suffix indexing to referenced doc symbols, and trim redundant linker-only JSON fields from persisted nodes.details without breaking read queries or Merkle parity
scope:
- crates/acdd-index/src/linker/mod.rs
- crates/acdd-index/src/linker/indexing.rs
- crates/acdd-index/src/db/writer/ingest.rs
- crates/acdd-index/src/db/writer/batch.rs
- tests/linker/early_interning_and_compact_facts.rs
- tests/core/compact_schema.rs
- docs/adr/0009-incremental-delta-and-transitive-relinking.md
depends_on:
- delta-seal-gc-and-receipt
tags:
- default
invariants:
- 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallel linker preparation yields identical edge ordering, resolution counts, and output_root across 1-thread and N-thread Rayon pools.'
- 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every SQL reader, MCP tool, and delta hydration path preserves identical query outputs after any nodes.details compaction.'
acceptance_criteria:
- 'AC-1: Independent linker preparation structures build concurrently with deterministic sorted assembly and pass 1-thread vs multi-thread determinism tests.'
- 'AC-2: by_suffix indexes only candidate suffixes needed by active doc.referenced_symbols while preserving 100% of verified documentation edges.'
- 'AC-3: Any trimmed linker-only keys in nodes.details are audited against all db/reader and MCP consumers and verified with identical cold-vs-delta output_root.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: parallel-linker-index-preparation
  title: Parallelize read-only node/namespace/inheritance preparation passes in linker/mod.rs and linker/indexing.rs with deterministic sorted merges
  status: completed
  evidence: Added PARALLEL_LINKER_PREP_MIN_NODES=2048 and PARALLEL_LINKER_PREP_MIN_FILES=32 deterministic rayon::join / par_iter preparation in linker/mod.rs and linker/indexing.rs
- subtask_ref: doc-scoped-by-suffix-index
  title: Collect distinct doc.referenced_symbols suffixes upfront and populate by_suffix only for matching candidates
  status: completed
  evidence: Precollected wanted_suffixes from active owners referenced_symbols and document symbol_claims and populated by_suffix only for matching candidates
- subtask_ref: audit-and-trim-nodes-details-json
  title: Audit all live SQL readers of nodes.details and remove verified linker-only JSON bloat (such as redundant bindings/rebindings if unused on read path) in writer/ingest.rs
  status: completed
  evidence: Retained live delta-hydration bindings/rebindings/param_types and eliminated unbounded reusable_detail_buffers accumulation in writer/ingest.rs
- subtask_ref: determinism-and-schema-seam-tests
  title: Add table-driven determinism and schema-consumer tests in tests/linker/early_interning_and_compact_facts.rs and tests/core/compact_schema.rs and update ADR 0009
  status: completed
  evidence: Extended 1-thread vs 4-thread >=2048-node determinism matrix in early_interning_and_compact_facts.rs, schema tests in compact_schema.rs, and reconciled ADR-0009
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:linker-parallel-prep-and-details-diet.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 431c9b4fa612431c84a84a58f672ff3b7426f458
  committed_oid: 47f81fa69dae3c1dac4349c1ba4c5c09228d3623
  accepted_at: 2026-11-19T23:29:56.540Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-DETERMINISTIC-PARALLEL-LINK: Parallel linker preparation yields identical edge ordering, resolution counts, and output_root across 1-thread and N-thread Rayon pools.'
      - 'INV-PF003-ZERO-COVERAGE-REGRESSION: Every SQL reader, MCP tool, and delta hydration path preserves identical query outputs after any nodes.details compaction.'
      verified: []
```

### task: typed-graph-models-and-linker-catalog

```yaml
task_ref: typed-graph-models-and-linker-catalog
target: Decompose CompactTag::known into canonical wire-compatible graph/node/reference/language enums, centralize NodeDetailFlags and NodeDetails projection across models and db readers, and collapse dual owned-vs-borrowed LanguageLinker methods onto unified FactsLookup
scope:
- crates/acdd-index/src/models/
- crates/acdd-index/src/linker/traits.rs
- crates/acdd-index/src/linker/mod.rs
- crates/acdd-index/src/linker/indexing.rs
- crates/acdd-index/src/languages/rust/linker.rs
- crates/acdd-index/src/languages/python/linker.rs
- crates/acdd-index/src/languages/typescript/linker.rs
- crates/acdd-index/src/db/reader.rs
- crates/acdd-index/src/db/traversal.rs
- tests/core/compact_schema.rs
- tests/linker/early_interning_and_compact_facts.rs
extended_scope:
- path: crates/acdd-index/src/lib.rs
  reason: Public FactsLookup re-export required by task contract
depends_on:
- linker-parallel-prep-and-details-diet
tags:
- default
invariants:
- 'INV-PF003-TYPED-CORE-WIRE-PARITY: EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId, and NodeDetailFlags preserve exact JSON/SQLite string representations and bit-identical acdd-owner-sha256-v2 output_root commitments.'
- 'INV-PF003-MODULAR-SINGLE-OWNER: LanguageLinker exposes one canonical package_exports and one canonical required_full_facts taking &dyn FactsLookup with zero _borrowed duplicate trait methods and zero anonymous boolean flag tuples in db/traversal.rs.'
acceptance_criteria:
- 'AC-1: crates/acdd-index/src/models/tags.rs owns EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, and LanguageId with CompactTag::known delegating to them and CompactGraph providing typed push/reference helpers.'
- 'AC-2: crates/acdd-index/src/models/details.rs owns NodeDetailKey constants and NodeDetailFlags, replacing db/traversal.rs::parse_node_detail_flags 5-tuple parsing and linker/traits.rs::is_stub_or_overload ad-hoc JSON lookups.'
- 'AC-3: FactsLookup is promoted as the single catalog trait across LanguageLinker (removing package_exports_borrowed and required_full_facts_borrowed) and linker/mod.rs entrypoints, passing all linker and schema contract tests with zero clones or dead_tails.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: typed-graph-and-language-enums
  title: Create crates/acdd-index/src/models/tags.rs with EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, and LanguageId, wire CompactTag::known and CompactGraph typed helpers onto them
  status: completed
  evidence: Created models/tags.rs with EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId; wired to CompactTag::known.
- subtask_ref: typed-node-details-projection-and-flags
  title: Create crates/acdd-index/src/models/details.rs with NodeDetailKey constants and NodeDetailFlags and migrate db/traversal.rs, db/reader.rs, and linker/traits.rs off ad-hoc JSON probes
  status: completed
  evidence: Created models/details.rs with NodeDetailKey and NodeDetailFlags; migrated db/traversal.rs, db/reader.rs, and linker/traits.rs.
- subtask_ref: unify-facts-lookup-and-language-linker
  title: Promote FactsLookup and collapse LanguageLinker package_exports/package_exports_borrowed and required_full_facts/required_full_facts_borrowed into single &dyn FactsLookup methods across linker and Tier-1 linkers
  status: completed
  evidence: Unified LanguageLinker trait around &dyn FactsLookup; removed package_exports_borrowed and required_full_facts_borrowed.
- subtask_ref: typed-models-and-linker-seam-tests
  title: Extend table-driven wire-parity and linker catalog tests in tests/core/compact_schema.rs and tests/linker/early_interning_and_compact_facts.rs
  status: completed
  evidence: Extended wire-parity and catalog tests in tests/core/compact_schema.rs and tests/linker/early_interning_and_compact_facts.rs.
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:typed-graph-models-and-linker-catalog.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 7eddde242e2786aeb1563f28282723e08825bd49
  committed_oid: 20193de5e975faca431a034b094fb93cd1e6d219
  accepted_at: 2026-11-20T02:36:19.620Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-TYPED-CORE-WIRE-PARITY: EdgeKind, EdgeConfidence, CoverageStatus, NodeKind, ReferenceKind, LanguageId, and NodeDetailFlags preserve exact JSON/SQLite string representations and bit-identical acdd-owner-sha256-v2 output_root commitments.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: LanguageLinker exposes one canonical package_exports and one canonical required_full_facts taking &dyn FactsLookup with zero _borrowed duplicate trait methods and zero anonymous boolean flag tuples in db/traversal.rs.'
      verified: []
```

### task: typed-commitments-and-sqlite-writer-guards

```yaml
task_ref: typed-commitments-and-sqlite-writer-guards
target: Replace stringly-typed commitment/FTS domain dispatch with typed CommitmentDomain and FtsDomain enums, extract the FTS5 binary segment decoder into db/commitments/fts5_fastpath.rs, extract canonical TempTableGuard and typed CatalogMetadata/CatalogStorageView helpers in db::writer, consolidate MultiValueBatch usage, and co-evolve crates/acdd-index/AGENTS.md
scope:
- crates/acdd-index/src/commitments.rs
- crates/acdd-index/src/db/commitments.rs
- crates/acdd-index/src/db/commitments/
- crates/acdd-index/src/db/writer/mod.rs
- crates/acdd-index/src/db/writer/batch.rs
- crates/acdd-index/src/db/writer/cache.rs
- crates/acdd-index/src/db/writer/delta.rs
- crates/acdd-index/src/db/writer/entrypoints.rs
- crates/acdd-index/src/db/writer/ingest.rs
- crates/acdd-index/src/db/writer/seal.rs
- crates/acdd-index/src/db/writer/temp_table.rs
- crates/acdd-index/AGENTS.md
- tests/commitment_integrity/
- tests/core/fact_persistence.rs
depends_on:
- cold-fts-seal-and-verify-fastpath
- single-encode-ram-staging-and-single-walk
- linker-parallel-prep-and-details-diet
tags:
- default
invariants:
- 'INV-PF003-TYPED-CORE-WIRE-PARITY: CommitmentDomain, FtsDomain, FtsDirtySet, and CatalogStorageView preserve exact acdd-owner-sha256-v2 domain names, leaf keys, binary cache encoding, and bit-identical cold-vs-delta output_root.'
- 'INV-PF003-MODULAR-SINGLE-OWNER: SQLite temporary table lifecycle (create, clear, batch populate, RAII drop) has one canonical owner in db::writer::temp_table::TempTableGuard consumed by writer/delta.rs and writer/entrypoints.rs, and the FTS5 binary segment decoder lives in db/commitments/fts5_fastpath.rs rather than bloating db/commitments.rs.'
acceptance_criteria:
- 'AC-1: CommitmentDomain (all 13 logical domains), FtsDomain (NodeSearch, DocSearch), and FtsDirtySet replace raw &str domain/FTS table switching in commitments.rs, db/commitments*, and db/writer/seal.rs, and the pure-Rust FTS5 segment decoder (`fts_digest_fastpath` + cursor/poslist/config helpers) is extracted cleanly into crates/acdd-index/src/db/commitments/fts5_fastpath.rs with zero digest regressions.'
- 'AC-2: crates/acdd-index/src/db/writer/temp_table.rs provides TempTableGuard with deterministic RAII cleanup and typed text/int batch population, replacing ad-hoc CREATE TEMP TABLE blocks in writer/delta.rs and writer/entrypoints.rs, and typed CatalogMetadata + CatalogStorageView eliminate the anonymous 3-tuple and manual inline binary field duplication in writer/cache.rs and writer/mod.rs.'
- 'AC-3: crates/acdd-index/AGENTS.md records canonical ownership for models::{tags, details}, commitments::{CommitmentDomain, FtsDomain}, db::commitments::fts5_fastpath, db::writer::{TempTableGuard, MultiValueBatch}, and linker::traits::FactsLookup.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: typed-commitment-and-fts-domains
  title: Introduce CommitmentDomain, FtsDomain, FtsCommitmentTable, and FtsDirtySet in commitments.rs, db/commitments*, and db/writer/seal.rs, and extract the pure-Rust FTS5 binary decoder into db/commitments/fts5_fastpath.rs while keeping DOMAINS and leaf keys wire-identical
  status: completed
  evidence: Introduced CommitmentDomain, FtsDomain, FtsDirtySet and extracted FTS5 segment decoder into db/commitments/fts5_fastpath.rs.
- subtask_ref: canonical-temp-table-guard-and-batch-consolidation
  title: Extract TempTableGuard in db/writer/temp_table.rs, migrate writer/delta.rs and writer/entrypoints.rs onto it, and consolidate multi-value batch loops onto MultiValueBatch
  status: completed
  evidence: Extracted TempTableGuard in db/writer/temp_table.rs, migrated writer/delta.rs and entrypoints.rs; consolidated MultiValueBatch.
- subtask_ref: typed-catalog-storage-view
  title: Replace the anonymous CatalogMetadata 3-tuple and manual inline TypedFacts binary field encoding in writer/mod.rs with typed CatalogMetadata (keyed by CommitmentDomain) and canonical borrowed CatalogStorageView encoding in writer/cache.rs
  status: completed
  evidence: Replaced anonymous 3-tuple with typed CatalogMetadata and canonical borrowed CatalogStorageView encoder in writer/cache.rs.
- subtask_ref: commitment-and-writer-guard-tests-and-agents-md
  title: Extend table-driven commitment/persistence parity tests in tests/commitment_integrity/ and tests/core/fact_persistence.rs and co-evolve crates/acdd-index/AGENTS.md
  status: completed
  evidence: Extended tests in tests/commitment_integrity/ and tests/core/fact_persistence.rs; co-evolved crates/acdd-index/AGENTS.md.
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:typed-commitments-and-sqlite-writer-guards.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 5801b32d20f86ce507a2118350b4f1a20d2d77ef
  committed_oid: dd62e1e4dee3b12df6cb02fce09ab7685e3bcaf6
  accepted_at: 2026-11-20T02:37:22.531Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-TYPED-CORE-WIRE-PARITY: CommitmentDomain, FtsDomain, FtsDirtySet, and CatalogStorageView preserve exact acdd-owner-sha256-v2 domain names, leaf keys, binary cache encoding, and bit-identical cold-vs-delta output_root.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: SQLite temporary table lifecycle (create, clear, batch populate, RAII drop) has one canonical owner in db::writer::temp_table::TempTableGuard consumed by writer/delta.rs and writer/entrypoints.rs, and the FTS5 binary segment decoder lives in db/commitments/fts5_fastpath.rs rather than bloating db/commitments.rs.'
      verified: []
```

### task: modularize-pipeline-and-payload-store

```yaml
task_ref: modularize-pipeline-and-payload-store
target: Replace bit-packed RAM-spool sentinel offsets with typed StagedPayload/PayloadStore, decompose 4,223-LOC src/engine/pipeline.rs into modular src/engine/pipeline/ submodules reusing TempTableGuard, introduce typed BuildMode/ColdScanResult, and co-evolve src/AGENTS.md and crates/acdd-index/AGENTS.md
scope:
- src/engine/pipeline.rs
- src/engine/pipeline/
- src/AGENTS.md
- crates/acdd-index/src/db/writer/mod.rs
- crates/acdd-index/src/db/writer/payload_store.rs
- crates/acdd-index/src/scanner/
- crates/acdd-index/AGENTS.md
- docs/architecture/indexing.md
- tests/core/build_query_roundtrip.rs
- tests/core/scanner_guard_limits.rs
- tests/incremental/single_pass_spool_and_parallel_delta.rs
extended_scope:
- path: crates/acdd-index/src/db/commitments/fts5_fastpath.rs
  reason: Investigate and fix extra vocabulary read in FTS fastpath during incremental delta per Defect 4
- path: crates/acdd-index/src/db/commitments.rs
  reason: Investigate and fix extra vocabulary read in FTS fastpath during incremental delta per Defect 4
- path: tests/commitment_integrity/cold_sealing.rs
  reason: Reconcile denied FTS reader error chain assertion with fastpath authorization error
depends_on:
- typed-graph-models-and-linker-catalog
- typed-commitments-and-sqlite-writer-guards
tags:
- default
invariants:
- 'INV-PF003-BOUNDED-RAM-STAGING: PayloadStore enforces bounded in-RAM staging under MemoryPermit and transparently spills overflow to positioned payloads.bin via StagedPayload::Spilled(SpoolSlice) with zero bit-packed offset sentinels.'
- 'INV-PF003-MODULAR-SINGLE-OWNER: src/engine/pipeline/ delegates all temporary frontier SQL tables to db::writer::TempTableGuard, decomposes delta_inner into explicit phase helpers, and leaves zero >= 10-line clones or dead_tails.'
acceptance_criteria:
- 'AC-1: crates/acdd-index/src/db/writer/payload_store.rs owns StagedPayload (InMemory | Spilled(SpoolSlice)) and PayloadStore, eliminating the 1 << 63 offset sentinel bit and raw RamStagedPayloads mutex alias across db/writer/mod.rs and src/engine/pipeline/.'
- 'AC-2: src/engine/pipeline.rs is decomposed into src/engine/pipeline/{mod,budget,hooks,staging,cold,invalidation,hydration,delta,tests}.rs with typed BuildMode/ColdScanResult, deduplicated expand_html_include_frontier helper (eliminating the L3299/L3424 clone), and TempTableGuard reuse.'
- 'AC-3: src/AGENTS.md, crates/acdd-index/AGENTS.md, and docs/architecture/indexing.md document the modular pipeline layout, PayloadStore, and TempTableGuard canonical ownership, and all cold/spill/delta parity tests pass with zero output_root drift.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: typed-payload-store-and-scanner-result
  title: Implement StagedPayload and PayloadStore in db/writer/payload_store.rs, wire db/writer/mod.rs onto it, and introduce typed ColdScanResult/BuildMode seams in scanner/ and pipeline/
  status: completed
  evidence: Implemented StagedPayload and PayloadStore in db/writer/payload_store.rs; introduced BuildMode and ColdScanResult.
- subtask_ref: decompose-engine-pipeline-modules
  title: Split src/engine/pipeline.rs into src/engine/pipeline/{mod,budget,hooks,staging,cold,invalidation,hydration,delta,tests}.rs, reuse db::writer::TempTableGuard, and deduplicate HTML include frontier expansion
  status: completed
  evidence: Decomposed pipeline.rs into pipeline/mod.rs and 8 submodules with 4 delta_inner phase helpers and TempTableGuard reuse.
- subtask_ref: pipeline-and-payload-store-seam-tests
  title: Extend table-driven RAM-vs-spill and delta parity tests in tests/core/build_query_roundtrip.rs, tests/core/scanner_guard_limits.rs, and tests/incremental/single_pass_spool_and_parallel_delta.rs
  status: completed
  evidence: Passed build_query_roundtrip, scanner_guard_limits, single_pass_spool_and_parallel_delta, and 18 commitment_integrity tests.
- subtask_ref: coevolve-agents-md-and-indexing-docs
  title: Co-evolve src/AGENTS.md, crates/acdd-index/AGENTS.md, and docs/architecture/indexing.md with PayloadStore, TempTableGuard, and modular src/engine/pipeline/ ownership
  status: completed
  evidence: Co-evolved src/AGENTS.md, crates/acdd-index/AGENTS.md, and docs/architecture/indexing.md.
receipt:
  run_id: PF-003-delta-seal-co-20261119212841-747613-1
  node_cursor: root:modularize-pipeline-and-payload-store.deliver
  gate_id: deliver
  worker_id: coordinator-pf003
  claim_revision: 1
  contract_revision: 3
  candidate_tree: 7d153396122bc4b936d130a1417c5b94b7e3c86d
  committed_oid: d94f3ab073678db01ed54e0667e7a58cf644bd2c
  accepted_at: 2026-11-20T04:50:41.264Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-PF003-BOUNDED-RAM-STAGING: PayloadStore enforces bounded in-RAM staging under MemoryPermit and transparently spills overflow to positioned payloads.bin via StagedPayload::Spilled(SpoolSlice) with zero bit-packed offset sentinels.'
      - 'INV-PF003-MODULAR-SINGLE-OWNER: src/engine/pipeline/ delegates all temporary frontier SQL tables to db::writer::TempTableGuard, decomposes delta_inner into explicit phase helpers, and leaves zero >= 10-line clones or dead_tails.'
      verified: []
```
