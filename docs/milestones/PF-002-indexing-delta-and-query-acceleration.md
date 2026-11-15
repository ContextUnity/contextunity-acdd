---
id: PF-002-indexing-delta-and-query-acceleration
title: "Indexing single-pass spooling, early token interning, and AST prefilter acceleration"
doc_type: contract
status: planned
depends_on:
  - AR-001-borrowed-data-and-composable-execution
guidance:
  hitl: pro
owners:
  - src/engine/pipeline.rs
  - crates/acdd-index/src/db/writer.rs
  - crates/acdd-core/src/models.rs
  - crates/acdd-core/src/models/compact_graph.rs
  - crates/acdd-core/src/typed_facts.rs
  - crates/acdd-core/src/interner.rs
  - crates/acdd-index/src/ast/visitor.rs
  - crates/acdd-index/src/linker/mod.rs
  - crates/acdd-index/src/ast/search.rs
  - src/cli/ast.rs
  - tests/
  - docs/architecture/
  - docs/reference/
  - docs/milestones/PF-002-indexing-delta-and-query-acceleration.md
invariants:
  - 'INV-SCOPE-INTEGRITY: Exclude every Cargo.toml, Cargo.lock, build.rs, .acdd/**, adapter/profile configuration, and dependency change from implementation scopes; no protected-file exception is authorized.'
  - 'INV-CORE-LEAF: Preserve existing crate dependency directions, core leaf isolation, and the frozen engine/database boundary; keep linker/extraction optimizations within existing crate boundaries.'
  - 'INV-PERF-STAGE-PAYLOAD-SPOOLING: Keep full extracted file payloads spooled on disk between parallel extraction and SQLite ingestion rather than retaining full extracted maps or full catalog rows in RAM.'
  - 'INV-PERF-BINARY-CATALOG-STORAGE: Preserve typed binary Zstd local-facts storage and deterministic encoding parity without introducing redundant double serialization or double compression passes.'
  - 'INV-PERF-COMPACT-EDGE-COVERAGE-MODELS: Preserve compact static/interned string representations across graph and facts representations while keeping binary cache decode and Merkle commitment bytes identical.'
  - 'INV-PERF-TYPED-SIDECAR-FACTS: Preserve deterministic extraction cache keys, FORGE-LOCAL-FACTS binary compatibility or clean version parity, and lossless typed fact roundtrips.'
  - 'INV-PERF-DETERMINISTIC-MERKLE-SEALING: Guarantee identical output_root Merkle commitments and graph/edge/coverage query results between cold builds, warm cache hits, and parallel delta updates.'
  - 'INV-AST-PREFILTER-RECALL: Structural AST prefiltering must never drop matching files; any pattern lacking a restrictive literal identifier anchor falls back to full-language Tree-sitter scanning with identical continuation offsets and order.'
  - 'INV-PROOF-BOUNDARY: Prove behavioral parity and regression sensitivity through real cold, delta, linker, and AST query seams; cite only observed behavior and never fabricate benchmark claims.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
related_plans: []
---

# Indexing single-pass spooling, early token interning, and AST prefilter acceleration

## Outcome and admission

Accelerate cold indexing, incremental delta refreshes, symbol interning, and
structural AST queries by eliminating three measured pipeline bottlenecks after
`AR-001` stabilizes single-owner borrowing and publication safety:

1. **Single-pass spooled serialization & parallel delta**: Remove the single-file
   `BTreeMap` wrapper and redundant double Zstd compression pass on the cold
   build worker path, adopt positioned single-file spool I/O (`write_at`/`read_at`),
   and parallelize delta modified-file extraction, batch Zstd hydration, and
   temp-table frontier lookup inside `delta_inner`.
2. **Early string interning & compact fact tags**: Extend static/interned string
   reuse (`CompactTag` and shared `Arc<str>` paths/modules) into AST extraction,
   `TypedFacts` serialization/decoding, and `LinkerInterner` pre-hashing while
   preserving existing public DTO field signatures and deterministic commitments.
3. **AST grep multi-token prefilter**: Extract restrictive literal identifier
   anchors from structural AST patterns (beyond declaration-only
   `pattern_symbol_name`) to narrow SQLite candidate files before Tree-sitter
   parsing, falling back to full-language scan whenever no safe anchor exists.

`PF` owns engine indexing, delta, and query throughput. Sequence inventory
inspection confirms `PF-001` is archived at
[PF-001-cold-build-rss-bound-and-linker-diet](archive/PF-001-cold-build-rss-bound-and-linker-diet.md);
`PF-002` is the next unused sequence number. This contract depends on
[AR-001](AR-001-borrowed-data-and-composable-execution.md) so candidate move
ownership, single `Adapter` reuse, and clean WAL checkpoint/sidecar publication
land before parallel pipeline throughput changes.

## Architectural viability and negative boundaries

Respect every existing invariant in [performance architecture](../architecture/performance.md):
- Never reintroduce in-memory full-fact `BTreeMap` retention across cold build
  (`INV-PERF-STAGE-PAYLOAD-SPOOLING`). Positioned spool I/O on `payloads.bin`
  must retain the bounded spool footprint and `MemoryPermit` release semantics.
- Never replace `delta_inner`'s read transaction with un-fenced concurrent SQLite
  mutations, and keep the `TEMP TABLE` optimization strictly scoped to small
  frontier key lookups (never full catalog staging tables).
- Keep `Node`, `Edge`, `Reference`, and `Facts` public field signatures
  compatible (`symbols: []`) or internalize compact pooling behind `TypedFacts`,
  `CompactGraph`, `ast::visitor`, and `LinkerInterner` without breaking
  downstream callers or adding `Cargo.toml` dependencies.
- Never allow AST prefiltering to return false negatives: literals inside
  comments/strings/metavariables (`$META`, `$$$`) or language keywords are never
  treated as required symbol anchors, and unsupported/generic patterns always
  fall back to full language file scan in `search_paged`.

## Grounded symbols and current bottlenecks

| Verified declaration | Current bottleneck / observation | Admitted optimization |
| --- | --- | --- |
| [`extraction_worker`](../../src/engine/pipeline.rs) & [`prepare_catalog_storage_with_facts`](../../crates/acdd-index/src/db/writer.rs) | Allocates a per-file `BTreeMap::from([(file.path.clone(), facts)])` wrapper solely for storage row prep, compressing facts both in `prepare_catalog_storage_with_facts` and in `encode_facts`; spooling re-opens/seeks sequentially. | Borrow `&TypedFacts` directly without single-file map cloning, avoid double Zstd compression on the same fact payload, and use positioned `FileExt::write_at` / `read_at` spooling on `payloads.bin` under `consume_pipeline_results` / `populate_owned`. |
| `delta_inner` & `load_hydrated_catalog_entries` ([`pipeline.rs`](../../src/engine/pipeline.rs)) | Re-extracts modified files sequentially instead of using Rayon with `ExtractionCache`, decodes Zstd blobs sequentially, and serializes frontier lists via `serde_json::to_string` + `json_each(?1)` across six repeated frontier queries. | Run Rayon parallel extraction over modified files (reusing `ExtractionCache`), decode fetched Zstd blobs in parallel after SQLite batch fetch, and populate a connection-scoped `TEMP TABLE` for the frontier key queries inside the existing read transaction. |
| [`CompactTag`](../../crates/acdd-core/src/models/compact_graph.rs), [`TypedFacts`](../../crates/acdd-core/src/typed_facts.rs), [`ast::visitor`](../../crates/acdd-index/src/ast/visitor.rs), [`LinkerInterner`](../../crates/acdd-core/src/interner.rs) | `Edge`/`Coverage` already use `CompactTag` + `Arc<str>`, while AST extraction and binary decode still heap-allocate repeated `kind`, `language`, and file-path strings per node/reference before linking. | Share static tag lookups and per-file `Arc<str>` path strings during AST extraction, binary `TypedFacts` decode, and `LinkerInterner` symbol/path ingestion while keeping public DTO signatures (`Node`/`Reference`) and `output_root` commitments unchanged. |
| [`search_paged`](../../src/cli/ast.rs) & [`pattern_symbol_name`](../../crates/acdd-index/src/ast/search.rs) | Only pre-filters SQLite files when `pattern_symbol_name` extracts a single declaration identifier; call-site or expression structural queries (e.g. `foo($X)` or `bar.baz()`) scan up to `500000` files of that language with Tree-sitter. | Add restrictive literal identifier anchor extraction in `crates/acdd-index/src/ast/search.rs` and prefilter candidate file paths in `src/cli/ast.rs::search_paged` via indexed `nodes` / `node_search` / `shared_keys` tables before Tree-sitter scan, with automatic fallback to full-language scan when unconstrained. |

## Governing invariants and producer-consumer contract

Preserve [ADR 0003](../adr/0003-two-phase-staged-publication.md),
[ADR 0012](../adr/0012-zero-regression-serialization-and-cold-build-latency.md),
[ADR 0018](../adr/0018-layer-dependency-boundaries-and-core-leaf-isolation.md),
and [performance invariants](../architecture/performance.md#model-and-agent-invariants-immutable-guidelines).

| Boundary / owner task | Input → output and consumers | Verification and parity criteria |
| --- | --- | --- |
| pipeline-and-delta | Scanned cold candidates / delta modified set + read transaction → single-pass positioned spool + parallel delta facts → published SQLite index | Bitwise identical `output_root` Merkle commitment and persisted catalog/graph rows across cold builds, warm extraction cache hits, and parallel delta rebuilds; consumer failure cleanup preserves previous generation. |
| early-interning | Source + AST syntax tree → compact-pooled `TypedFacts` → `LinkerInterner` → `CompactGraph` | Zero change to resolved/unresolved/ambiguous edge counts, diagnostic totals, binary cache roundtrips (`FORGE-LOCAL-FACTS\0\x03`), or `output_root`. |
| ast-prefilter | Structural AST pattern + language + optional path → SQLite candidate prefilter → `ast::search_page` → bounded paged JSON/Markdown results | 100% recall parity against unfiltered full-language scan across declarations, calls, member accesses, wildcard metavariables, macro invocations, and non-matching patterns; deterministic ordering and continuation identity unchanged. |

## Waves, proof policy, and harness allocation

| Wave | Tasks | Dependency |
| --- | --- | --- |
| 0 | `freeze-performance-seams-and-contracts` | none (runs after `AR-001` lands) |
| 1 | `single-pass-spool-and-parallel-delta`, `intern-extraction-tokens-and-kinds`, `accelerate-ast-grep-candidate-prefilter` | each depends only on Wave 0 |

Wave 0 owns shared test index updates (`tests/incremental.rs`, `tests/linker.rs`,
`tests/query_context.rs`) and registers compiling empty stub files for the
three Wave 1 domain tasks. Wave 1 task scopes are strictly disjoint across code,
test stubs, and documentation pages.

Use `direct-proof`: establish behavioral equivalence baselines and sensitivity
mutations (stale spool offset, corrupted temp-table frontier, dropped
unresolved-ref parity, false-negative literal filter) to verify every optimization
against real indexing and query seams.

## Tasks in this milestone

### task: freeze-performance-seams-and-contracts

```yaml
task_ref: freeze-performance-seams-and-contracts
target: "Freeze PF-002 performance contracts, verify disjoint Wave 1 boundaries, and pre-register compiling test stubs in tests/incremental.rs, tests/linker.rs, and tests/query_context.rs without editing production code or Cargo.toml"
proof_policy: direct-proof
contract_revision: 1
depends_on: []
scope:
  - docs/milestones/PF-002-indexing-delta-and-query-acceleration.md
  - tests/incremental.rs
  - tests/linker.rs
  - tests/query_context.rs
  - tests/incremental/single_pass_spool_and_parallel_delta.rs
  - tests/linker/early_interning_and_compact_facts.rs
  - tests/query_context/ast_grep_candidate_prefilter.rs
symbols: []
status: ready
subtasks:
  - subtask_ref: freeze-pf-seams-and-invariants
    title: "Verify disjoint Wave 1 scopes, AR-001 baseline assumptions, and exact performance invariants in this manifest"
  - subtask_ref: register-pf-test-stubs
    title: "Create compiling empty seam modules and register them only in tests/incremental.rs, tests/linker.rs, and tests/query_context.rs"
  - subtask_ref: validate-pf-manifest-and-compile
    title: "Run acdd validate on PF-002 and strict Clippy across registered stubs"
```

Blueprint slice:

- Implementation steps: re-inspect `src/engine/pipeline.rs`,
  `crates/acdd-index/src/db/writer.rs`, `crates/acdd-core/src/typed_facts.rs`,
  `crates/acdd-index/src/ast/search.rs`, and `src/cli/ast.rs` after `AR-001`
  lands; register the three new test stubs (`single_pass_spool_and_parallel_delta`,
  `early_interning_and_compact_facts`, `ast_grep_candidate_prefilter`); run
  manifest validation and Clippy.
- Symbol signatures: none modified.
- Test seam: `acdd validate docs/milestones/PF-002-indexing-delta-and-query-acceleration.md --format json`
  and `cargo clippy --test incremental --test linker --test query_context --all-features -- -D warnings`.
- Incoming seams: completed `AR-001` codebase, existing test suite index files.
- Outgoing seams: compiling empty seam files consumed exclusively by their
  owning Wave 1 task.
- Negative boundaries: no production code edit, no `Cargo.toml` edit, no fake
  assertions in stubs.

### task: single-pass-spool-and-parallel-delta

```yaml
task_ref: single-pass-spool-and-parallel-delta
target: "Eliminate single-file map wrapping and redundant double Zstd compression in extraction/storage prep, use positioned single-file spool I/O on payloads.bin, and parallelize delta extraction/hydration with TEMP TABLE frontier queries"
proof_policy: direct-proof
contract_revision: 1
depends_on:
  - freeze-performance-seams-and-contracts
scope:
  - src/engine/pipeline.rs
  - crates/acdd-index/src/db/writer.rs
  - docs/architecture/performance.md
  - tests/incremental/single_pass_spool_and_parallel_delta.rs
symbols: []
status: planned
subtasks:
  - subtask_ref: eliminate-single-file-map-and-double-zstd
    title: "Pass borrowed &TypedFacts directly in extraction_worker / writer without BTreeMap cloning and encode Zstd local facts once per file"
  - subtask_ref: positioned-spool-io
    title: "Use positioned read_at/write_at spool I/O on payloads.bin across consume_pipeline_results and populate_owned while preserving MemoryPermit bounds"
  - subtask_ref: parallelize-delta-and-temp-frontier
    title: "Parallelize delta_inner modified extraction with ExtractionCache and Zstd decoding after SQLite batch fetch, and replace 6x json_each frontier queries with a TEMP TABLE frontier"
```

Blueprint slice:

- Implementation steps: in `src/engine/pipeline.rs` (`extraction_worker`,
  `consume_pipeline_results`, `delta_inner`) and
  `crates/acdd-index/src/db/writer.rs` (`prepare_catalog_storage_with_facts`,
  `populate_owned`, `read_staged_payload`), remove per-file `BTreeMap` creation
  for single-file catalog row preparation, share a single Zstd compression pass
  between fact payload spooling and catalog restoration, switch spool read/write
  offsets to positioned `std::os::unix::fs::FileExt` (`read_at`/`write_at` with
  portable `#[cfg(not(unix))]` seek fallback), parallelize modified-file
  extraction (`rayon`) sharing `ExtractionCache` plus parallel `decode_facts`
  hydration after batch SQLite blob read, and replace the six `json_each(?1)`
  frontier queries with one transaction-local `TEMP TABLE` of frontier keys.
- Symbol signatures: public `build`, `delta`, and `populate_owned` signatures
  keep existing external compatibility; internal helpers remain crate/file-scoped.
- Test seam: `tests/incremental/single_pass_spool_and_parallel_delta.rs::cold_and_delta_match_commitments_after_spool_and_parallel_delta`,
  run with `cargo test --test incremental single_pass_spool_and_parallel_delta`;
  also run `cargo test --test incremental` and `cargo test --test commitment_integrity`.
- Sensitivity: corrupt a positioned spool offset or drop a transitive frontier
  neighbor from the `TEMP TABLE`; tests must immediately catch catalog corruption
  or cold-vs-delta `output_root` divergence.
- Incoming seams: moved `BuildCandidate` and clean WAL handling from `AR-001`.
- Outgoing seams: deterministic SQLite catalog, edges, commitments, and `docs/architecture/performance.md`.
- Negative boundaries: never buffer full extracted `Facts` or full catalog rows
  in RAM during cold build (`INV-PERF-STAGE-PAYLOAD-SPOOLING`); never use
  `TEMP TABLE` to stage catalog rows; no schema version breaking migration
  without clean fallback.

### task: intern-extraction-tokens-and-kinds

```yaml
task_ref: intern-extraction-tokens-and-kinds
target: "Extend CompactTag static tag tables and shared Arc<str>/slice pooling across AST extraction, TypedFacts serialization, and LinkerInterner ingestion without changing public Node/Reference field compatibility or commitment bytes"
proof_policy: direct-proof
contract_revision: 1
depends_on:
  - freeze-performance-seams-and-contracts
scope:
  - crates/acdd-core/src/models.rs
  - crates/acdd-core/src/models/compact_graph.rs
  - crates/acdd-core/src/typed_facts.rs
  - crates/acdd-core/src/interner.rs
  - crates/acdd-index/src/ast/visitor.rs
  - crates/acdd-index/src/linker/mod.rs
  - docs/architecture/indexing.md
  - tests/linker/early_interning_and_compact_facts.rs
symbols: []
status: planned
subtasks:
  - subtask_ref: expand-static-compact-tags
    title: "Extend CompactTag static known variants to cover common AST node/reference kinds and languages alongside edge/coverage tags"
  - subtask_ref: pool-extraction-and-binary-decode-strings
    title: "Pool repeated path/kind/language allocations during ast::visitor extraction, TypedFacts binary serialization/decode, and LinkerInterner pre-hashing"
  - subtask_ref: prove-roundtrip-and-linker-parity
    title: "Verify identical TypedFacts binary roundtrips, LinkerInterner ID resolution, Graph edge/coverage parity, and Merkle commitments"
```

Blueprint slice:

- Implementation steps: expand `CompactTag` (`crates/acdd-core/src/models/compact_graph.rs`
  / `models.rs`) to cover frequent node kinds (`function`, `struct`, `method`,
  `class`, `module`, `trait`, `enum`, `impl`, `field`, `constant`, etc.),
  reference kinds (`call`, `import`, `type`, `route`, `member`), and language
  tags; thread shared string/path interning (`InternedSymbol` / `Arc<str>` path
  pool / static `&'static str` literals) inside `crates/acdd-index/src/ast/visitor.rs`,
  `crates/acdd-core/src/typed_facts.rs` (binary decode string table deduplication),
  and `crates/acdd-index/src/linker/mod.rs` (`LinkerInterner` pre-interning)
  while preserving `Node` / `Reference` public struct fields and exact
  `encode_facts` / `decode_facts` roundtrips.
- Symbol signatures: public struct fields on `Node`, `Reference`, `Facts`, and
  `TypedFacts` preserve API compatibility (`symbols: []`).
- Test seam: `tests/linker/early_interning_and_compact_facts.rs::interned_extraction_and_linker_preserve_exact_graph_parity`,
  run with `cargo test --test linker early_interning_and_compact_facts`; also run
  `cargo test --test linker` and `cargo test -p acdd-core --lib`.
- Sensitivity: map an unknown tag or collide two distinct symbols in pre-hashing;
  the parity test must catch mismatches in serialized facts, resolved edge count,
  or diagnostic output.
- Incoming seams: raw AST syntax nodes, `TypedFacts`, `LinkerInterner`.
- Outgoing seams: deduplicated facts and deterministic `CompactGraph` outputs.
- Negative boundaries: no breaking public DTO field changes across workspace
  tests, no `Cargo.toml` edits, no resolution heuristic algorithm change.

### task: accelerate-ast-grep-candidate-prefilter

```yaml
task_ref: accelerate-ast-grep-candidate-prefilter
target: "Extract restrictive literal identifier anchors from structural AST patterns in ast::search and pre-filter SQLite candidate files in ast::search_paged while guaranteeing 100% recall parity via automatic full-language fallback"
proof_policy: direct-proof
contract_revision: 1
depends_on:
  - freeze-performance-seams-and-contracts
scope:
  - src/cli/ast.rs
  - crates/acdd-index/src/ast/search.rs
  - docs/reference/acdd.md
  - tests/query_context/ast_grep_candidate_prefilter.rs
symbols: []
status: planned
subtasks:
  - subtask_ref: extract-pattern-literal-anchors
    title: "Extract restrictive non-metavariable literal identifier tokens from prepared structural AST patterns alongside pattern_symbol_name"
  - subtask_ref: prefilter-candidate-files-in-sqlite
    title: "Intersect indexed file candidates in src/cli/ast.rs::search_paged using node/search indexes when anchors exist, falling back cleanly when unrestricted"
  - subtask_ref: prove-ast-prefilter-recall-parity
    title: "Prove 100% match, ordering, continuation offset, and diagnostic parity against unfiltered full-language scans across languages and metavariable patterns"
```

Blueprint slice:

- Implementation steps: in `crates/acdd-index/src/ast/search.rs`, add an
  internal/crate helper returning safe literal identifier anchors extracted
  from the normalized Tree-sitter pattern (ignoring `$VAR`/`$$$` metavariables,
  string/comment tokens if ambiguous, and language keywords, and keeping
  `pattern_symbol_name` intact). In `src/cli/ast.rs::search_paged`, query SQLite
  for distinct file paths containing those indexed anchors (`nodes` /
  `node_search` / `shared_keys` scoped by `language` and optional `path`),
  returning the union or intersection safely according to guarantee of zero false
  negatives, and fall back directly to the existing full-language ordered query
  whenever no restrictive literal anchor is extracted. Keep continuation offset,
  generation check, `SearchBudget`, and diagnostics identical.
- Symbol signatures: `ast::search_paged`, `ast::search`, and
  `ast::search::pattern_symbol_name` keep existing public signatures.
- Test seam: `tests/query_context/ast_grep_candidate_prefilter.rs::prefiltered_ast_search_matches_full_scan_with_100_percent_recall`,
  run with `cargo test --test query_context ast_grep_candidate_prefilter`; also
  run `cargo test --test query_context search_and_paging` and
  `cargo test --test mcp test_read` / AST tests.
- Sensitivity: inject an overly aggressive filter (e.g., filtering by `$META`
  placeholder name or omitting a call/method site whose name is in `references`
  / `shared_keys` rather than `nodes.name`); the recall-parity matrix must fail
  immediately by comparing filtered vs unfiltered `search_paged` results.
- Incoming seams: single loaded `&scanner::Adapter` (after `AR-001`), SQLite
  read connection, structural pattern & language.
- Outgoing seams: deterministic bounded `ast_grep_search` results matching the
  exact unfiltered result set in less scanned file volume.
- Negative boundaries: zero false negatives permitted (`INV-AST-PREFILTER-RECALL`);
  no change to pattern syntax, diagnostic error codes, paging limit, or budget.

## Verification and delivery

Before each task Build, load `code-quality` and `test-contract-seam`, relevant
owner instructions, and returned workflow guidance. Execute `task verify` with the
named seam and run strict `cargo clippy --all-targets --all-features -- -D warnings`.
Record measured file/allocation/SQL differences and test sensitivity results
honestly per `INV-PROOF-BOUNDARY`. At milestone handoff, run the documentation
scan agent across the modified performance/indexing/reference pages and complete
all [TESTS.md](../../TESTS.md#milestone-handoff-gate) checks before archival.
