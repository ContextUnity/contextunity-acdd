---
title: "ADR 0008: Merkle Commitments and Compressed Fact Storage"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0008: Merkle Commitments and Compressed Fact Storage

## Status

Accepted.

## Context

Code-map storage requires cryptographic verification across cold builds and incremental deltas while keeping persistent SQLite databases compact and extraction RSS bounded. Persisting raw compiler scope graphs inside `nodes.details`, double-compressing fact payloads, wrapping single-file facts in temporary `BTreeMap`s, or allocating unpooled kind/language strings per symbol inflates cold-build latency and heap pressure.

## Decision

### 1. Deterministic Merkle Tree Commitments (`acdd-owner-sha256-v2`)

- [`crates/acdd-index/src/commitments.rs`](../../crates/acdd-index/src/commitments.rs) computes deterministic SHA-256 commitments (`ALGORITHM = "acdd-owner-sha256-v2"`, renamed from pre-`0.5.3` `forge-owner-sha256-v2`) over all 13 logical table domains (`source_inventory`, `files`, `nodes`, `edges`, `local_facts`, `edge_occurrences`, `dependencies`, `shared_owners`, `errors`, `resolution_coverage`, `coverage_owner_language`, `coverage_language_counts`, `doc_sections`) and sorted FTS5 postings (`(term, docid, col, pos)`).
- Dictionary lookups (`path_map`, `node_map`, `key_map`) resolve IDs in memory without per-row allocations; collections exceeding 8,192 records hash chunks in parallel via Rayon while preserving lexicographical sort order.
- Cold builds and incremental deltas over identical inputs produce bit-identical `output_root` commitments enforced by `tests/commitment_integrity`.

### 2. Compressed Fact Store (`ACDD-LOCAL-FACTS`) & Lean Projections

- **`nodes.details`**: Retains only navigation and direct-query fields; temporary value-flow graphs and raw scope maps stay out of `nodes.details` and FTS5 tables.
- **Compressed `local_facts.facts_blob`**: Stores typed AST facts compressed with Zstandard (`LOCAL_FACTS_BINARY_MAGIC = b"ACDD-LOCAL-FACTS\0\x03"`, pre-rename `FORGE-LOCAL-FACTS\0\x03`) in [`crates/acdd-index/src/typed_facts.rs`](../../crates/acdd-index/src/typed_facts.rs), preserving deterministic decode and delta hydration without duplicating uncompressed source text in SQLite.
- **Bulk Ingestion**: Multi-value batch inserts (`COLD_LOCAL_FACTS_BATCH_ROWS`) and borrowed serializers (`NavigationDetails`) avoid row-at-a-time SQL overhead and ingestion node clones, while target-scoped queries avoid workspace-wide table scans.

### 3. Single-Pass Positioned Spooling & `CompactTag` Pooling (`PF-002`)

Building on the archived [PF-002](../../docs/milestones/archive/PF-002-indexing-delta-and-query-acceleration.md) design:

- **Single-pass positioned spooling (`payloads.bin`)**: Cold extraction borrows `&TypedFacts` directly without allocating single-file `BTreeMap` wrappers or running redundant double Zstd compression passes, writing/reading bounded spooled payloads via positioned I/O (`write_at`/`read_at`) while preserving `MemoryPermit` bounds (`INV-PERF-STAGE-PAYLOAD-SPOOLING`).
- **`CompactTag` and string pooling**: [`CompactTag`](../../crates/acdd-index/src/models/compact_graph.rs) static known variants and shared `Arc<str>` path/module pooling deduplicate repeated node/reference `kind`, `language`, and file-path allocations across AST extraction, `TypedFacts` binary decode, and `LinkerInterner` pre-hashing without altering public `Node`/`Reference` compatibility or `output_root` commitment bytes.

## Consequences

- Tamper-evident index verification completes in milliseconds with identical Merkle roots across cold builds, extraction cache hits, and deltas.
- Persistent navigation tables remain compact while full typed facts stay losslessly recoverable for incremental relinking.
- Single-pass spooling and `CompactTag` pooling eliminate redundant heap and compression overhead without changing deterministic outputs.
