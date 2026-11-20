---
id: AR-002-modularize-db-reader-and-traversal
title: "Decompose db/reader.rs and db/traversal.rs monoliths, eliminate query duplication, and bound query-time allocations"
doc_type: contract
status: planned
depends_on:
  - PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
guidance:
  hitl: pro
owners:
  - crates/acdd-index/AGENTS.md
  - crates/acdd-index/src/db/mod.rs
  - crates/acdd-index/src/db/paging.rs
  - crates/acdd-index/src/db/reader.rs
  - crates/acdd-index/src/db/reader/
  - crates/acdd-index/src/db/traversal.rs
  - crates/acdd-index/src/db/traversal/
  - docs/architecture/indexing.md
  - docs/milestones/AR-002-modularize-db-reader-and-traversal.md
  - tests/query_context/
  - tests/mcp/
invariants:
  - 'INV-AR002-MODULAR-READER-TRAVERSAL: db/reader and db/traversal are decomposed into single-responsibility submodules under crates/acdd-index/src/db/{reader,traversal}/ preserving all public re-exports on db::{reader,traversal} with zero >= 5-line clones or newly introduced dead_tails.'
  - 'INV-AR002-SINGLE-OWNER-PATH-AND-INVARIANTS: Selector URI/path prefix normalization and governing_invariants SQL/projection logic each have one canonical owner, and phantom-tested non-_with_path convenience wrappers (inspect_paged, explain_with_options, traverse_with_options, removal_paged, query_with_options) are retired onto canonical *_with_path entrypoints.'
  - 'INV-AR002-BOUNDED-QUERY-EXECUTION: paging::query avoids redundant double CTE execution where safe, directory_boundary avoids ORDER BY json_object(...), rows_with_mode counts serialized bytes without building and re-serializing intermediate JSON maps, and dead_tails_paged/duplicates_paged materialize JSON Value items only for the requested page slice.'
acceptance_criteria:
  - 'AC-1: crates/acdd-index/src/db/reader.rs (2,380 LOC) is decomposed into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs; governing_invariants and governing_invariants_for_scope share one unified invariant-projection helper (eliminating all 9 duplicate blocks / 45 duplicated lines); VALID_OVERVIEW_ASPECTS is derived without duplicating DATABASE_OVERVIEW_ASPECTS + DISCOVERY_OVERVIEW_ASPECTS; and rows_with_mode avoids redundant serde_json::to_writer row re-serialization.'
  - 'AC-2: crates/acdd-index/src/db/traversal.rs (3,521 LOC) is decomposed into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs reusing models::details::NodeDetailFlags and one shared selector URI/path normalizer across reader and traversal.'
  - 'AC-3: dead_tails_paged and duplicates_paged build Value items only for the requested [offset..offset+limit] page (and avoid per-line String cloning when normalizing duplicate slices); directory_boundary drops ORDER BY json_object(...); paging::query avoids redundant double execution of complex CTEs; and non-_with_path phantom-tested wrappers are collapsed onto *_with_path (with tests/query_context/ and tests/mcp/ migrated).'
  - 'AC-4: crates/acdd-index/AGENTS.md and docs/architecture/indexing.md document the db/reader/ and db/traversal/ module ownership, and cargo test --test query_context --test mcp plus code_map_query(duplicates, dead_tails) pass cleanly.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
repositories: []
linked_to: []
---

# AR-002: Modularize `db/reader.rs` & `db/traversal.rs`, Unify Query Projections, and Bound Query Memory

## Goal & Structural Problems Identified

`crates/acdd-index/src/db/reader.rs` (`2,380` LOC) and `crates/acdd-index/src/db/traversal.rs` (`3,521` LOC) form a `5,901`-LOC read/query monolith outside the write/indexing scope of `PF-004` and `LS-003`. Code and graph audits identified four concrete architectural and performance defects:

1. **Multi-domain god-files (`5,901` LOC across 2 files):**
   - `db/reader.rs` mixes 5 unrelated subsystems: DB opening/confinement/WAL-fallback/integrity backup (`open*`, `validate_*`, `QueryBudget`), low-level SQL execution & 8 MiB byte-budget serialization (`rows_with_mode`, `sql_query_paged`), symbol/doc search (`search*`, `search_docs*`, `get_doc*`), inspect/explain/governing invariants, and diagnostics/analysis.
   - `db/traversal.rs` mixes 5 unrelated subsystems: BFS impact/implementations/unwired/untested/removal, directory `slice_paged` + boundary CTEs, Git diff `change_impact`, disk-reading `duplicates_paged`, and the `~1,100`-LOC workspace liveness `dead_tails` / `phantom_tested` / `dangling_calls` engine (including its own C-style comment/string lexer and callback scanner).
2. **Concrete duplication & wrapper bloat:**
   - `governing_invariants` and `governing_invariants_for_scope` (`db/reader.rs:1761-1876`) contain **9 duplicate blocks (`45` duplicated lines)**.
   - URI/path prefix stripping (`file://`, `file:`, `./`, trailing `/`) is duplicated across `reader.rs::directory_path`, `reader/selectors.rs::select_detail_with_path`, and `traversal.rs::slice_paged`.
   - `VALID_OVERVIEW_ASPECTS` (`reader.rs:45-61`) manually repeats `DATABASE_OVERVIEW_ASPECTS` + `DISCOVERY_OVERVIEW_ASPECTS`.
   - Legacy non-`_with_path` wrappers (`inspect_paged`, `explain_with_options`, `traverse_with_options`, `removal_paged`, `query_with_options`) exist solely for integration tests while production callers use `*_with_path`.
3. **Query-time CPU & allocation overhead:**
   - `paging::query` (`db/paging.rs:85-97`) executes every SQL query **twice** (`SELECT count(*) FROM (<sql>)` then `SELECT * FROM (<sql>) LIMIT ? OFFSET ?`), doubling cost for multi-join CTEs such as `boundary_modules` and `untested_paged`.
   - `directory_boundary` (`db/traversal.rs:1448-1495`) executes 4 separate `CROSS JOIN` queries and sorts `internal` via `ORDER BY json_object(...)`, forcing per-row JSON construction solely to sort.
   - `rows_with_mode` (`db/reader.rs:679-786`) constructs an intermediate `serde_json::Map` per row and runs `serde_json::to_writer(&mut JsonByteCounter, &value)` for byte accounting instead of counting during direct streaming/serialization.
   - `dead_tails_paged` (`db/traversal.rs:3250-3391`) and `duplicates_paged` (`db/traversal.rs:1928-2349`) materialize full `Vec<Value>` item lists across the entire repository before `page_slice` keeps only `offset..offset+limit`, and `duplicates_paged` allocates `Vec<String>` line copies for every candidate file.

## Non-Goals & Negative Boundaries

- Do **not** alter indexing write paths (`db/writer/`, `db/commitments/`, `linker/`, `src/engine/pipeline/`).
- Preserve all CLI, MCP, and JSON response schemas, deterministic sort orders, `8 MiB` result bounds, `2s` query budgets, and SQLite read-only/confinement invariants (`SQLITE_OPEN_NOFOLLOW`, `PRAGMA trusted_schema = OFF`).

## Tasks in this milestone

### task: modularize-db-reader-and-unify-query-projections

```yaml
task_ref: modularize-db-reader-and-unify-query-projections
target: "Decompose db/reader.rs into focused db/reader/ submodules, unify governing_invariants and selector normalization, optimize paging::query and rows_with_mode, and retire non-_with_path reader wrappers"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/db/mod.rs
  - crates/acdd-index/src/db/paging.rs
  - crates/acdd-index/src/db/reader.rs
  - crates/acdd-index/src/db/reader/
  - tests/query_context/
  - tests/mcp/
invariants:
  - 'INV-AR002-MODULAR-READER-TRAVERSAL: db/reader/ submodules isolate open/confinement, SQL budget execution, search, inspect/explain, docs, diagnostics, and selectors behind db::reader re-exports with zero clones or dead_tails.'
  - 'INV-AR002-SINGLE-OWNER-PATH-AND-INVARIANTS: governing_invariants and governing_invariants_for_scope share one canonical query/projection helper, and selector URI normalization lives in one helper.'
  - 'INV-AR002-BOUNDED-QUERY-EXECUTION: paging::query avoids redundant double execution of CTEs via COUNT(*) OVER() or single-pass counting, and rows_with_mode avoids redundant row re-serialization for byte accounting.'
acceptance_criteria:
  - 'AC-1: db/reader.rs is split into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs with all public signatures re-exported cleanly from db::reader.'
  - 'AC-2: governing_invariants and governing_invariants_for_scope share one unified helper (0 duplicate blocks in code_map_query duplicates), VALID_OVERVIEW_ASPECTS is composed without duplicating aspect lists, and reader/selectors share one strip_selector_uri_prefix helper.'
  - 'AC-3: paging::query eliminates double query execution on heavy CTEs while preserving exact total/has_more/generation semantics, rows_with_mode eliminates double serde_json::to_writer overhead, and inspect_paged / explain_with_options are retired in favor of *_with_path (with tests/query_context/ migrated).'
status: planned
subtasks:
  - subtask_ref: split-db-reader-submodules
    title: "Decompose db/reader.rs into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs preserving db::reader exports"
  - subtask_ref: unify-invariants-overview-and-selector-normalization
    title: "Deduplicate governing_invariants/governing_invariants_for_scope, VALID_OVERVIEW_ASPECTS, and selector URI normalization, and migrate tests off inspect_paged/explain_with_options"
  - subtask_ref: optimize-paging-count-and-rows-byte-accounting
    title: "Optimize paging::query double-execution and rows_with_mode byte counting while preserving exact 8 MiB / 2s budgets"
```

### task: modularize-db-traversal-and-bound-analysis-queries

```yaml
task_ref: modularize-db-traversal-and-bound-analysis-queries
target: "Decompose db/traversal.rs into focused db/traversal/ submodules, bound dead_tails_paged and duplicates_paged page materialization, optimize directory_boundary, retire non-_with_path wrappers, and co-evolve docs"
contract_revision: 1
depends_on:
  - modularize-db-reader-and-unify-query-projections
tags:
  - default
scope:
  - crates/acdd-index/src/db/traversal.rs
  - crates/acdd-index/src/db/traversal/
  - crates/acdd-index/AGENTS.md
  - docs/architecture/indexing.md
  - tests/query_context/
  - tests/mcp/
invariants:
  - 'INV-AR002-MODULAR-READER-TRAVERSAL: db/traversal/ submodules isolate impact/BFS, directory slice, Git change_impact, duplicates, and dead_tails engines cleanly behind db::traversal re-exports.'
  - 'INV-AR002-BOUNDED-QUERY-EXECUTION: dead_tails_paged and duplicates_paged materialize JSON Value payloads only for the requested page slice, duplicates avoids per-line String cloning, and directory_boundary sorts without ORDER BY json_object(...).'
acceptance_criteria:
  - 'AC-1: db/traversal.rs is split into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs reusing reader::strip_selector_uri_prefix and models::details::NodeDetailFlags.'
  - 'AC-2: dead_tails_paged and duplicates_paged sort lightweight metadata structs and construct serde_json::Value objects only for [offset..offset+limit] items; duplicates avoids per-line Vec<String> cloning; directory_boundary replaces ORDER BY json_object(...) with native column sorting.'
  - 'AC-3: traverse_with_options, removal_paged, and query_with_options are collapsed onto *_with_path (with tests/query_context/ and tests/mcp/ migrated), crates/acdd-index/AGENTS.md and docs/architecture/indexing.md are updated, and cargo test --test query_context --test mcp + code_map_query(duplicates, dead_tails) pass.'
status: planned
subtasks:
  - subtask_ref: split-db-traversal-submodules-and-retire-wrappers
    title: "Decompose db/traversal.rs into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs, reuse shared selector normalizer/NodeDetailFlags, and collapse *_with_options wrappers"
  - subtask_ref: bound-dead-tails-duplicates-and-directory-boundary
    title: "Materialize JSON items only for paged slices in dead_tails_paged and duplicates_paged, drop per-line String clones in duplicates, and remove ORDER BY json_object(...) in directory_boundary"
  - subtask_ref: update-db-query-docs-and-verify-parity
    title: "Update crates/acdd-index/AGENTS.md and docs/architecture/indexing.md and verify query_context, mcp, duplicates, and dead_tails"
```
