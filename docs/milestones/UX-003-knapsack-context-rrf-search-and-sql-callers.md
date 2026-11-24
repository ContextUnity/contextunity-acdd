---
id: UX-003-knapsack-context-rrf-search-and-sql-callers
title: "0-1 Knapsack ClaimResp packing, RRF search ranking, SQL callers, selector/get_doc DB fixes & <4 KiB Markdown compaction"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - acdd.yaml
  - crates/acdd-core/src/
  - crates/acdd-index/src/db/reader/
  - crates/acdd-index/src/db/symbols/
  - crates/acdd-state/src/context/
  - src/engine/reader.rs
  - src/format/
  - src/mcp/server.rs
  - src/mcp/tools/code_tools.rs
  - src/state_bridge/
  - tests/acdd/
  - tests/mcp/
  - tests/query_context/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/architecture/indexing.md
  - docs/architecture/performance.md
  - docs/reference/mcp-markdown-contracts.md
  - docs/reference/tasks.md
  - ROADMAP.md
  - docs/milestones/UX-003-knapsack-context-rrf-search-and-sql-callers.md
invariants:
  - 'INV-CORE-LEAF: Pure 0-1 knapsack selection contracts (KnapsackItem, pack_knapsack_indices) live in crates/acdd-core/src/response.rs free of JSON, SQLite, async, or filesystem dependencies.'
  - 'INV-KNAPSACK-BOUNDED-DETERMINISM: budget_scored_items and apply_tier6_context_lenses never exceed ResponsePolicy.max_output_bytes, cap DP table size (at most 65,536 cells via quantized weight bucketing when needed), break equal-score ties cleanly by original candidate rank, prevent early oversized items from terminating smaller high-value fits, and preserve ClaimResp.omitted_sections accounting.'
  - 'INV-RRF-RANKING-CONTRACT: code_map_search (search_with_options) scores non-exact, non-type-filter queries with deterministic Reciprocal Rank Fusion sum(weight / (60 + rank)) over structural tier rank, FTS5 BM25 lexical rank, and bounded non-containment graph-degree rank without unbounded hardcoded score offsets or whole-table degree scans, preserving match_reason, 1001-row search_total horizon, generation pagination, and QueryBudget.'
  - 'INV-EXACT-SEARCH-GRAPH-ISOLATION: exact: true queries MUST NOT read edges or shared_owners tables (preserving AuthAction::Read authorizer isolation), and returns:/takes: type filters plus unranked arbitrary wildcard queries retain existing behavior.'
  - 'INV-SQL-TABLE-EXTERNAL-CALLERS: code.external_callers surfaces non-test external consumers of SQL table nodes (defined in scoped .sql files or deleted relative to baseline_ref) across defines_table, queries_table, mutates_table, and references_table incoming edges, while code.signature_scope continues enforcing breaking callable signatures without false positives on SQL schema changes.'
  - 'INV-SELECTOR-AND-GETDOC-BOUNDARY: Symbol selectors normalize Type::method separators to .-boundary suffix matches and honor explicit file scopes without failing in apply_suffix_fallback, resolve or suggest 1-hop imported/re-exported symbols for (path, selector), and get_doc gates ambiguous reference candidates behind include_candidates: false without hitting SQLite timeouts.'
  - 'INV-MARKDOWN-PAYLOAD-HYGIENE: Compact Markdown responses target <4 KiB inline thresholds by omitting unrequested file:// URIs, no-op freshness {status: matched, refresh: none}, unrequested ambiguous doc candidates, redundant id/hop breadcrumbs, and 0-count diagnostic rows, shortening all continuing generation hashes to >=8 chars while preserving JSON schema parity.'
  - 'INV-DOCS-AND-ADR-COEVOLUTION: UX-003 updates ADR-0006, docs/architecture/indexing.md, docs/architecture/performance.md, docs/reference/{mcp-markdown-contracts,tasks}.md, and ROADMAP.md in the same milestone passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: acdd-core::response exposes pack_knapsack_indices with exact byte DP for tight item/capacity products, quantized DP scaling above the 65,536-cell cap, deterministic rank tie-breaking, and property/edge-case tests (empty items, zero capacity, oversized-first item skipped in favor of smaller fits, overflowing subset weights).'
  - 'AC-2: src/state_bridge/mod.rs (budget_scored_items / budget_items), code_domain/context.rs (provide_symbols, provide_tests, provide_external_callers), docs_domain.rs, and crates/acdd-state/src/context/tiers.rs (apply_tier6_context_lenses) select relevance-scored context items via 0-1 knapsack packing under tight ClaimResp byte budgets instead of truncating after the first oversized candidate.'
  - 'AC-3: crates/acdd-index/src/db/symbols/search.rs replaces hardcoded additive score constants and binary graph_group partitioning with weighted RRF (k = 60) over structural tier rank, FTS5 BM25 rank, and bounded non-containment graph-degree centrality, populates result["pattern"] reliably, deprioritizes snippetless module indexed_text rows, keeps exact: true isolated from edges/shared_owners, and preserves generation paging and match_reason output.'
  - 'AC-4: src/state_bridge/code_domain/context.rs, signature_scope.rs, and mod.rs extend code.external_callers to include SQL table nodes (including baseline-deleted .sql tables) linked via defines_table, queries_table, mutates_table, and references_table incoming edges (plus baseline fallback for unindexed .sql tables), without regressing code.signature_scope.'
  - 'AC-5: crates/acdd-index/src/db/reader/selectors.rs fixes Type::method prefix misbinding to n.kind, allows apply_suffix_fallback inside scoped paths, and resolves/suggests 1-hop imported and barrel re-exported symbols for acdd_read(path, selector); crates/acdd-index/src/db/reader/docs.rs decouples document_reference_candidate_page from Detail::Full onto include_candidates (eliminating candidate bloat and 2s SQLite timeouts on large docs); src/engine/reader.rs sets mode="search" default context=0 with ## <path> grouping, **/archive/** default exclusion, default line_count=100, and concise enclosing hints; and src/format/coordination.rs strips redundant metadata/excerpts and candidate stubs.'
  - 'AC-6: crates/acdd-core/src/response.rs defaults ResponsePolicy::page_size to 12, SourceContextPolicy::max_body_lines to 100, and ResponsePolicy::file_uris to false; src/format/{presentation,discovery,traversal}.rs, src/mcp/server.rs, src/mcp/tools/code_tools.rs (wiring OverviewInput.path, include_candidates, and include_pulse), and crates/acdd-index/src/db/symbols/{preview,test_discovery}.rs suppress matched/none freshness in Markdown, shorten all continuing generation hashes to >=8 chars, omit file:// URIs in compact Markdown, compact acdd_read snippet headers, code_map_tests breadcrumbs (~130 B/item compact), code_map_search 2-line cards (with "1000+" total instead of null), code_map_overview (skip 0-count rows, inline profiles, cap hubs, render path overview), code_map_inspect/impact/explain, and fix trim_items_page_tail double-quoting.'
  - 'AC-7: ADR-0006, docs/architecture/{indexing,performance}.md, docs/reference/{mcp-markdown-contracts,tasks}.md, and ROADMAP.md reflect the shipped behavior and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - test-suite-rules
  - agent-instructions
---

# UX-003: 0-1 Knapsack `ClaimResp` packing, RRF search ranking, SQL callers, selector/`get_doc` DB fixes & `<4 KiB` Markdown compaction

## Outcome and purpose

Upgrade context selection, search ranking, SQL consumer awareness, DB-backed selector/`get_doc` reliability, and Markdown payload density across the `db/`, `engine/reader.rs`, and `src/format/` modules that align with `AR-005` typed DTOs (while non-conflicting `ast_grep_search` fixes, MCP JSON schema docstring compaction, cross-tool dispatch hints, and CLI defaults ship separately in [ast-inference-schema-compaction-and-cli-parity.md](../plans/ast-inference-schema-compaction-and-cli-parity.md)):

1. **Deterministic 0-1 Knapsack `ClaimResp` Packing (`budget_scored_items` & Tier 6 Lenses)**:
   Replace first-fit sequential break truncation in `src/state_bridge/mod.rs::budget_items` with relevance-weighted 0-1 knapsack packing backed by a pure bounded DP routine in `crates/acdd-core/src/response.rs` (`pack_knapsack_indices`). Maximize cumulative relevance inside `ResponsePolicy.max_output_bytes` instead of dropping trailing high-signal items after an oversized candidate.
2. **Reciprocal Rank Fusion (`RRF`, `k = 60`) & Search Signal Fixes in `code_map_search`**:
   Replace hardcoded additive constants (`1000`, `500`, `250`, `+50`) and binary `graph_group` separation in `crates/acdd-index/src/db/symbols/search.rs` with scale-invariant RRF combining structural tier rank, FTS5 BM25 rank, and bounded non-containment graph-degree centrality. Populate `result["pattern"]` consistently and deprioritize snippetless `module` `indexed_text` hits.
3. **SQL Table External-Caller Visibility in `ClaimResp` (`code.external_callers`)**:
   Extend `src/state_bridge/code_domain/` so `code.external_callers` inspects scoped `.sql` `table` nodes (active and baseline-deleted) and reports external non-test files across `defines_table`, `queries_table`, `mutates_table`, and `references_table` incoming edges without breaking `code.signature_scope`.
4. **DB Selector Resolution, `get_doc` Candidate Decoupling & `acdd_read` Engine Compaction**:
   - Fix `Type::method` (`n.kind` misbinding) and `path + Type.method` (`apply_suffix_fallback` `scope` bailout) in `crates/acdd-index/src/db/reader/selectors.rs`, plus add 1-hop `imports`/`references`/barrel re-export lookup when `acdd_read(path, selector)` has zero direct hits in `scope`.
   - Decouple `document_reference_candidate_page` in `crates/acdd-index/src/db/reader/docs.rs` from `Detail::Full` onto `include_candidates` (`false` default, eliminating ~79–92% candidate bloat and 2s SQLite timeout on large docs), clean `render_section_page`/`render_search_section_page` in `src/format/coordination.rs`, and switch `src/engine/reader.rs` search default to `context = 0` with `## <path>` grouping and `**/archive/**` exclusion plus default `line_count = 100`.
5. **Global Markdown/URI Envelope & Discovery/Traversal Compaction (`<4 KiB` Target)**:
   - Lower `ResponsePolicy::default().page_size` to `12` (`acdd.yaml` + `crates/acdd-core/src/response.rs`), `SourceContextPolicy::max_body_lines` to `100`, and opt-in `file_uris: false` so `src/format/presentation.rs` and `src/mcp/server.rs` omit verbose `file://` URIs in compact Markdown, suppress `{status: "matched", refresh: "none"}` `freshness` across all Markdown tools, shorten continuing `generation` hashes to `>=8` chars, fix `trim_items_page_tail` double-quoting, and compact `render_acdd_read` headers (`~1 KB -> ~110 B`), `src/format/discovery.rs` (`code_map_search`, `code_map_overview` including `path`-scoped overview & zero-count pruning, `code_map_inspect`, `operation_scope`), and `src/format/traversal.rs` (`code_map_tests` `~130 B/item` compact arrow breadcrumb, `code_map_impact`/`explain`).

## Global Negative Boundaries

- Do NOT introduce floating-point nondeterminism or unseeded randomness in either DP tie-breaking or SQLite RRF ordering; ties must resolve deterministically by `(rrf_score DESC, match_rank ASC, pd.path ASC, n.line ASC, n.id ASC)`.
- `exact: true` searches in `crates/acdd-index/src/db/symbols/search.rs` MUST NOT touch `edges` or `shared_owners` (preserving existing SQLite authorizer read-isolation tests).
- `acdd-core` MUST remain a dependency-free leaf (`INV-CORE-LEAF`)—no `serde_json`, `rusqlite`, or filesystem I/O inside knapsack or policy types.
- `code.signature_scope` verification MUST NOT fail solely because a `.sql` schema table has external callers; SQL schema consumer surfacing belongs to the informational `code.external_callers` context lens.
- Markdown compaction MUST preserve parsed JSON schema parity (retaining canonical JSON fields where callers inspect them), 8-character `generation` validation, exact `ErrorCode` diagnostics, and `file://` URI opt-in when `detail: "full"` or `file_uris: true`.
- Do NOT break canonical test binary layout (`tests/acdd.rs`, `tests/mcp.rs`, `tests/query_context.rs`); keep all new integration tests inside pre-registered modules of those canonical binaries.

## Tasks in this milestone

### task: ux003-core-contracts-and-seam-registry

```yaml
task_ref: ux003-core-contracts-and-seam-registry
target: Add pure bounded 0-1 knapsack selection contracts (KnapsackItem, pack_knapsack_indices) to crates/acdd-core/src/response.rs and pre-register canonical UX-003 seam test modules across tests/acdd, tests/query_context, and tests/mcp
subtasks:
- subtask_ref: acdd-core-knapsack-solver
  title: Implement KnapsackItem and pack_knapsack_indices with exact/quantized bounded DP, rank tie-breaking, and unit tests in crates/acdd-core/src/response.rs and re-export in crates/acdd-core/src/lib.rs
- subtask_ref: pre-register-ux003-seam-tests
  title: Register tests/acdd/claim_knapsack_and_sql_callers.rs in tests/acdd/mod.rs, tests/query_context/{rrf_hybrid_search,selector_ergonomics}.rs in tests/query_context.rs, and tests/mcp/freshness/output_compaction.rs in tests/mcp/freshness.rs
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
- tests/query_context/selector_ergonomics.rs
- tests/mcp/freshness.rs
- tests/mcp/freshness/output_compaction.rs
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
target: Replace additive score constants and binary graph_group boosts in crates/acdd-index/src/db/symbols/search.rs with bounded RRF (k = 60) ranking, reliable pattern reporting, and module indexed_text deprioritization
subtasks:
- subtask_ref: sqlite-rrf-symbol-ranking
  title: Implement weighted RRF (k = 60) fusion across structural tier, BM25 rank, and bounded non-containment edge degree in crates/acdd-index/src/db/symbols/search.rs, populate pattern metadata consistently, and deprioritize snippetless module indexed_text hits while preserving exact-search isolation and generation paging
- subtask_ref: rrf-search-seams-and-docs
  title: Add multi-signal RRF, pattern heading, and exact-search isolation contract tests in tests/query_context/rrf_hybrid_search.rs and document RRF ranking in docs/architecture/indexing.md and docs/architecture/performance.md
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
  title: Add SQL schema consumer ClaimResp seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs and update docs/reference/tasks.md
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
```

### task: selector-getdoc-and-read-engine-compaction

```yaml
task_ref: selector-getdoc-and-read-engine-compaction
target: "Fix Type::method and 1-hop import selector resolution, decouple get_doc candidates from Detail::Full to eliminate candidate bloat and 2s SQLite timeouts, compact acdd_read search/lines defaults and doc Markdown sections"
subtasks:
- subtask_ref: selector-colon-and-import-resolution
  title: "Normalize Type::method to .-boundary suffix matching, remove the scope bailout in apply_suffix_fallback, and resolve or suggest 1-hop imported/re-exported symbols for acdd_read(path, selector) in crates/acdd-index/src/db/reader/selectors.rs, plus seam tests in tests/query_context/selector_ergonomics.rs"
- subtask_ref: getdoc-decoupling-and-read-engine
  title: 'Gate document_reference_candidate_page strictly on include_candidates in crates/acdd-index/src/db/reader/docs.rs and src/mcp/tools/code_tools.rs (fixing 2s large-doc timeout), switch mode="search" default context to 0 with ## <path> grouping and **/archive/** exclusion, lower default line_count to 100 and trim footer/source_preview defaults in src/engine/reader.rs and crates/acdd-index/src/db/symbols/preview.rs, strip doc DB metadata/stubs and search path duplication in src/format/coordination.rs, and update docs tests in tests/mcp/freshness/document_responses.rs'
contract_revision: 1
tags:
- default
depends_on:
- ux003-core-contracts-and-seam-registry
scope:
- crates/acdd-index/src/db/reader/selectors.rs
- crates/acdd-index/src/db/reader/docs.rs
- crates/acdd-index/src/db/symbols/preview.rs
- src/engine/reader.rs
- src/format/coordination.rs
- src/mcp/tools/code_tools.rs
- tests/query_context/selector_ergonomics.rs
- tests/mcp/freshness/document_responses.rs
```

### task: global-envelope-discovery-and-traversal-compaction

```yaml
task_ref: global-envelope-discovery-and-traversal-compaction
target: "Compact Markdown discovery, traversal, and global envelopes to hold routine navigation payloads under the 4 KiB inline threshold while preserving JSON parity and opt-in full detail/file_uris"
subtasks:
- subtask_ref: policy-defaults-and-global-envelope-hygiene
  title: "Lower default ResponsePolicy::page_size to 12 and SourceContextPolicy::max_body_lines to 100 with opt-in file_uris (false default) in crates/acdd-core/src/response.rs and acdd.yaml; suppress matched/none freshness across all Markdown tools, shorten continuing generation hashes to >=8 chars, omit compact file:// URIs, compact render_acdd_read headers, and fix trim_items_page_tail double-quoting in src/format/presentation.rs and src/mcp/server.rs"
- subtask_ref: discovery-and-traversal-compact-renderers
  title: 'Compact code_map_search cards (1000+ total badge, capped signature, 1-line pulse), code_map_overview (omit 0-count health rows, inline compiled profiles, cap hubs, render path overview), operation_scope, and code_map_inspect in src/format/discovery.rs; compact code_map_tests breadcrumbs (~130 B/item) and impact/explain items without per-hop rs: IDs in src/format/traversal.rs and crates/acdd-index/src/db/symbols/test_discovery.rs'
- subtask_ref: compaction-seams-and-canonical-docs
  title: "Add <4 KiB payload budget regression tests in tests/mcp/freshness/output_compaction.rs, reconcile tests/mcp/freshness/document_responses.rs, tests/mcp/lint_surface.rs, tests/mcp/tool_evolution/{cli_and_relations.rs,mcp_boundaries/ast_and_search.rs}, and tests/query_context/{search_and_paging.rs,diagnostics_and_source.rs}, and update docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md, docs/reference/mcp-markdown-contracts.md, and ROADMAP.md"
contract_revision: 1
tags:
- default
depends_on:
- knapsack-claim-context-packing
- rrf-hybrid-symbol-search
- sql-table-external-callers
- selector-getdoc-and-read-engine-compaction
scope:
- acdd.yaml
- crates/acdd-core/src/response.rs
- crates/acdd-index/src/db/symbols/test_discovery.rs
- src/mcp/server.rs
- src/format/presentation.rs
- src/format/discovery.rs
- src/format/traversal.rs
- tests/mcp/freshness/document_responses.rs
- tests/mcp/freshness/output_compaction.rs
- tests/mcp/lint_surface.rs
- tests/mcp/tool_evolution/cli_and_relations.rs
- tests/mcp/tool_evolution/mcp_boundaries/ast_and_search.rs
- tests/query_context/search_and_paging.rs
- tests/query_context/diagnostics_and_source.rs
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/reference/mcp-markdown-contracts.md
- ROADMAP.md
```
