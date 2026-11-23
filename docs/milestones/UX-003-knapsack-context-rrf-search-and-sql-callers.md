---
id: UX-003-knapsack-context-rrf-search-and-sql-callers
title: "0-1 Knapsack ClaimResp packing, Reciprocal Rank Fusion search, and SQL table external-caller lens"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - acdd.yaml
  - crates/acdd-core/src/
  - crates/acdd-index/src/db/symbols/
  - crates/acdd-state/src/context/
  - src/state_bridge/
  - tests/acdd/
  - tests/query_context/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/architecture/indexing.md
  - docs/architecture/performance.md
  - docs/reference/tasks.md
  - ROADMAP.md
  - docs/milestones/UX-003-knapsack-context-rrf-search-and-sql-callers.md
invariants:
  - 'INV-CORE-LEAF: Pure 0-1 knapsack selection contracts (KnapsackItem, pack_knapsack_indices) live in crates/acdd-core/src/response.rs free of JSON, SQLite, async, or filesystem dependencies.'
  - 'INV-KNAPSACK-BOUNDED-DETERMINISM: budget_scored_items and apply_tier6_context_lenses never exceed ResponsePolicy.max_output_bytes, cap DP table size (at most 65,536 cells via quantized weight bucketing when needed), break equal-score ties cleanly by original candidate rank, prevent early oversized items from terminating smaller high-value fits, and preserve ClaimResp.omitted_sections accounting.'
  - 'INV-RRF-RANKING-CONTRACT: code_map_search (search_with_options) scores non-exact, non-type-filter queries with deterministic Reciprocal Rank Fusion sum(weight / (60 + rank)) over structural tier rank, FTS5 BM25 lexical rank, and bounded non-containment graph-degree rank without unbounded hardcoded score offsets or whole-table degree scans, preserving match_reason, 1001-row search_total horizon, generation pagination, and QueryBudget.'
  - 'INV-EXACT-SEARCH-GRAPH-ISOLATION: exact: true queries MUST NOT read edges or shared_owners tables (preserving AuthAction::Read authorizer isolation), and returns:/takes: type filters plus unranked arbitrary wildcard queries retain existing behavior.'
  - 'INV-SQL-TABLE-EXTERNAL-CALLERS: code.external_callers surfaces non-test external consumers of SQL table nodes (defined in scoped .sql files or deleted relative to baseline_ref) across defines_table, queries_table, mutates_table, and references_table incoming edges, while code.signature_scope continues enforcing breaking callable signatures without false positives on SQL schema changes.'
  - 'INV-DOCS-AND-ADR-COEVOLUTION: UX-003 updates ADR-0006, docs/architecture/indexing.md, docs/architecture/performance.md, docs/reference/tasks.md, and ROADMAP.md in the same milestone passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: acdd-core::response exposes pack_knapsack_indices with exact byte DP for tight item/capacity products, quantized DP scaling above the 65,536-cell cap, deterministic rank tie-breaking, and property/edge-case tests (empty items, zero capacity, oversized-first item skipped in favor of smaller fits, overflowing subset weights).'
  - 'AC-2: src/state_bridge/mod.rs (budget_scored_items / budget_items), code_domain/context.rs (provide_symbols, provide_tests, provide_external_callers), docs_domain.rs, and crates/acdd-state/src/context/tiers.rs (apply_tier6_context_lenses) select relevance-scored context items via 0-1 knapsack packing under tight ClaimResp byte budgets instead of truncating after the first oversized candidate.'
  - 'AC-3: crates/acdd-index/src/db/symbols/search.rs replaces hardcoded additive score constants and binary graph_group partitioning with weighted RRF (k = 60) over structural tier rank, FTS5 BM25 rank, and bounded non-containment graph-degree centrality, while keeping exact: true isolated from edges/shared_owners and preserving generation paging and match_reason output.'
  - 'AC-4: src/state_bridge/code_domain/context.rs, signature_scope.rs, and mod.rs extend code.external_callers to include SQL table nodes (including baseline-deleted .sql tables) linked via defines_table, queries_table, mutates_table, and references_table incoming edges (plus baseline fallback for unindexed .sql tables), without regressing code.signature_scope.'
  - 'AC-5: ADR-0006, docs/architecture/indexing.md, docs/architecture/performance.md, docs/reference/tasks.md, and ROADMAP.md reflect the shipped behavior and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - test-suite-rules
  - agent-instructions
---

# UX-003: 0-1 Knapsack `ClaimResp` packing, Reciprocal Rank Fusion search, and SQL table external-caller lens

## Outcome and purpose

Upgrade context selection, search ranking, and SQL consumer awareness without adding external dependencies or ML models:

1. **Deterministic 0-1 Knapsack `ClaimResp` Packing (`budget_scored_items` & Tier 6 Lenses)**:
   Replace first-fit sequential break truncation in `src/state_bridge/mod.rs::budget_items` (`for item in items { ... break; }`) with relevance-weighted 0-1 knapsack packing backed by a pure bounded DP routine in `crates/acdd-core/src/response.rs`. When a large low-priority symbol, test, or caller sits ahead of smaller high-relevance entries—or when tight `ClaimResp` Tier 6 remaining headroom constrains lens payloads—ACDD maximizes cumulative relevance inside the byte cap instead of dropping trailing high-signal items.
2. **Reciprocal Rank Fusion (`RRF`, `k = 60`) in `code_map_search`**:
   Replace fragile hardcoded additive constants (`1000`, `500`, `250`, `+50`) and binary `graph_group` separation in `crates/acdd-index/src/db/symbols/search.rs` with scale-invariant Reciprocal Rank Fusion combining structural tier rank, FTS5 BM25 rank, and bounded non-containment graph-degree centrality over the top candidate window. This rewards multi-signal consensus (e.g., heavily referenced core symbols matching both name and text) without letting extreme BM25 scores or binary 1-edge presence override exact/central symbols.
3. **SQL Table External-Caller Visibility in `ClaimResp` (`code.external_callers`)**:
   ACDD already indexes `.sql` `table` declarations, embedded SQL literals in Rust/TypeScript/Python, and schema/query edges (`defines_table`, `queries_table`, `mutates_table`, `references_table`) for `code_map_impact` and `code_map_explain`. Extend `src/state_bridge/code_domain/` so `code.external_callers` inspects scoped `.sql` `table` nodes (both active and baseline-deleted) and reports external non-test files querying, mutating, defining, or referencing those tables.

## Global Negative Boundaries

- Do NOT introduce floating-point nondeterminism or unseeded randomness in either DP tie-breaking or SQLite RRF ordering; ties must resolve deterministically by `(rrf_score DESC, match_rank ASC, pd.path ASC, n.line ASC, n.id ASC)`.
- `exact: true` searches in `crates/acdd-index/src/db/symbols/search.rs` MUST NOT touch `edges` or `shared_owners` (preserving existing SQLite authorizer read-isolation tests).
- `acdd-core` MUST remain a dependency-free leaf (`INV-CORE-LEAF`)—no `serde_json`, `rusqlite`, or filesystem I/O inside knapsack types.
- `code.signature_scope` verification MUST NOT fail solely because a `.sql` schema table has external callers; SQL schema consumer surfacing belongs to the informational `code.external_callers` context lens.
- Do NOT break existing `tests/acdd/` or `tests/query_context/` test binaries; keep all new integration tests inside existing canonical binaries (`tests/acdd.rs`, `tests/query_context.rs`).

## Tasks in this milestone

### task: ux003-core-contracts-and-seam-registry

```yaml
task_ref: ux003-core-contracts-and-seam-registry
target: Add pure bounded 0-1 knapsack selection contracts (KnapsackItem, pack_knapsack_indices) to crates/acdd-core/src/response.rs and pre-register canonical seam test modules in tests/acdd/mod.rs and tests/query_context.rs
subtasks:
- subtask_ref: acdd-core-knapsack-solver
  title: Implement KnapsackItem and pack_knapsack_indices with exact/quantized bounded DP, rank tie-breaking, and unit tests in crates/acdd-core/src/response.rs and re-export in crates/acdd-core/src/lib.rs
- subtask_ref: pre-register-ux003-seam-tests
  title: Register tests/acdd/claim_knapsack_and_sql_callers.rs in tests/acdd/mod.rs and tests/query_context/rrf_hybrid_search.rs in tests/query_context.rs
contract_revision: 1
tags:
- default
depends_on: []
scope:
- crates/acdd-core/src/lib.rs
- crates/acdd-core/src/response.rs
- tests/acdd/mod.rs
- tests/acdd/claim_knapsack_and_sql_callers.rs
- tests/query_context.rs
- tests/query_context/rrf_hybrid_search.rs
```

### task: knapsack-claim-context-packing

```yaml
task_ref: knapsack-claim-context-packing
target: Wire relevance-scored 0-1 knapsack packing into budget_scored_items/budget_items in src/state_bridge/, code_domain/context.rs, docs_domain.rs, and Tier 6 apply_tier6_context_lenses in crates/acdd-state/src/context/tiers.rs
subtasks:
- subtask_ref: state-bridge-knapsack-budgeter
  title: Implement budget_scored_items over pack_knapsack_indices in src/state_bridge/mod.rs and emit explicit relevance scores in code_domain/context.rs and docs_domain.rs
- subtask_ref: tier6-knapsack-budget-flow
  title: Tighten Tier 6 ClaimResp byte allocation and lens fallback handling in crates/acdd-state/src/context/tiers.rs, add seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs, and update docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
contract_revision: 1
tags:
- default
depends_on:
- ux003-core-contracts-and-seam-registry
scope:
- crates/acdd-state/src/context/tiers.rs
- src/state_bridge/mod.rs
- src/state_bridge/code_domain/context.rs
- src/state_bridge/docs_domain.rs
- tests/acdd/claim_knapsack_and_sql_callers.rs
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
```

### task: rrf-hybrid-symbol-search

```yaml
task_ref: rrf-hybrid-symbol-search
target: Replace additive score constants and binary graph_group boosts in crates/acdd-index/src/db/symbols/search.rs with bounded RRF (k = 60) ranking over structural tier, FTS5 BM25 rank, and graph centrality
subtasks:
- subtask_ref: sqlite-rrf-symbol-ranking
  title: Implement weighted RRF (k = 60) fusion across structural tier, BM25 rank, and bounded non-containment edge degree in crates/acdd-index/src/db/symbols/search.rs while preserving exact-search edge isolation and generation paging
- subtask_ref: rrf-search-seams-and-docs
  title: Add multi-signal RRF contract tests and exact-search isolation checks in tests/query_context/rrf_hybrid_search.rs and document RRF ranking in docs/architecture/indexing.md and docs/architecture/performance.md
contract_revision: 1
tags:
- default
depends_on:
- ux003-core-contracts-and-seam-registry
scope:
- crates/acdd-index/src/db/symbols/search.rs
- tests/query_context/rrf_hybrid_search.rs
- docs/architecture/indexing.md
- docs/architecture/performance.md
```

### task: sql-table-external-callers

```yaml
task_ref: sql-table-external-callers
target: Extend code.external_callers to surface non-test external consumers of SQL table nodes across SQL table edges without changing code.signature_scope strictness
subtasks:
- subtask_ref: sql-table-caller-edge-resolution
  title: Extend node/edge selection in src/state_bridge/code_domain/{mod.rs,signature_scope.rs,context.rs} so active and deleted .sql table nodes resolve external consumers via defines_table, queries_table, mutates_table, and references_table incoming edges (and baseline fallback)
- subtask_ref: sql-callers-tests-and-docs
  title: Add SQL schema consumer ClaimResp seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs and update docs/reference/tasks.md and ROADMAP.md
contract_revision: 1
tags:
- default
depends_on:
- knapsack-claim-context-packing
scope:
- src/state_bridge/code_domain/mod.rs
- src/state_bridge/code_domain/signature_scope.rs
- src/state_bridge/code_domain/context.rs
- tests/acdd/claim_knapsack_and_sql_callers.rs
- docs/reference/tasks.md
- ROADMAP.md
```