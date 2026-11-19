---
title: "ADR 0009: Incremental Delta and Transitive Relinking"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0009: Incremental Delta and Transitive Relinking

## Status

Accepted.

## Context

Re-indexing an entire repository after editing a handful of files wastes CPU and I/O, while naively re-parsing only directly modified files leaves cross-file symbol edges, importer bindings, and reverse-dependency callers stale. Sequential Zstd hydration and repeated `json_each` SQLite frontier scans also bottleneck medium-to-large delta refreshes.

## Decision

### 1. Scoped Invalidation Perimeter & Transitive Export Dirtying

When files change or are deleted, the delta pipeline identifies a closed invalidation perimeter:

1. Directly modified or deleted files.
2. Files importing changed public symbols or module exports.
3. Reverse dependencies affected by signature, type, or export changes.

Unaffected files retain their cached nodes, edges, compressed `local_facts`, and leaf digests, while files inside the dirty perimeter are relinked selectively to match a fresh cold build identically.

### 2. Parallel `ExtractionCache` Delta, Batch Hydration & `TEMP TABLE` Frontier (`PF-002`)

Within [`delta_inner`](../../src/engine/pipeline.rs), [`crates/acdd-index/src/db/writer/delta.rs`](../../crates/acdd-index/src/db/writer/delta.rs), and archived [PF-002](../milestones/archive/PF-002-indexing-delta-and-query-acceleration.md):

- **Parallel extraction**: Modified files extract concurrently via Rayon using the shared [`ExtractionCache`](../../crates/acdd-index/src/db/writer/cache.rs) (`ACDD-EXTRACTION-FACTS\0\x04` under `O_NOFOLLOW`).
- **Batch Zstd hydration**: Persisted `local_facts.facts_blob` rows for transitive frontier neighbors are batch-fetched from SQLite and decoded/hydrated in parallel rather than row-by-row.
- **Connection-scoped `TEMP TABLE` frontier**: Read-side frontier lookups on the `admitted` snapshot connection in `delta_inner` populate in-memory `TEMP TABLE`s (`delta_modified_paths`, `delta_changed_resolution`, `delta_frontier_text`, `delta_frontier_text_2`, `delta_frontier_int`, toggling `PRAGMA query_only = OFF` strictly during temp-table writes via an RAII guard that unconditionally restores `PRAGMA query_only = ON` before `SELECT`s), while write-side delta persistence in `crates/acdd-index/src/db/delta.rs` uses `changed_edge_keys` and `replaced_nodes` `TEMP TABLE`s without staging full catalog tables or repeating `json_each(?1)` frontier scans.

### 3. Atomic Delta Publication

Delta mutations commit in one fenced SQLite transaction and publish updated graph tables alongside the recomputed `acdd-owner-sha256-v2` Merkle `output_root`. Cached MCP readers observe the updated database identity on post-query verification and refresh automatically.

## Consequences

- Incremental delta updates maintain 100% `output_root` and graph-edge parity with full cold rebuilds.
- Parallel extraction, batch Zstd hydration, and `TEMP TABLE` frontier joins accelerate multi-file delta refreshes without compromising transactional isolation.
