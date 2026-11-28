---
id: UX-003-knapsack-context-rrf-search-and-sql-callers
title: 0-1 Knapsack ClaimResp packing, RRF search ranking, SQL callers, selector/get_doc DB fixes & <4 KiB Markdown compaction
doc_type: contract
status: completed
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
guidance:
  hitl: pro
required_skills:
- acdd
- test-suite-rules
- agent-instructions
closure:
  status: completed
  closed_at: 2026-11-25T21:30:54.611Z
  target_base: main
  target_base_commit: 25b3620c9eb861ee1d454fcdf26d9925968e0aa0
  task_receipts:
    global-envelope-discovery-and-traversal-compaction:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:global-envelope-discovery-and-traversal-compaction.deliver
      gate_id: deliver
      worker_id: ux003-w3-compaction-reviewer
      claim_revision: 1
      contract_revision: 5
      candidate_tree: 8a4a5a898984d01c7f02d60ba86fabe98e064332
      committed_oid: b4624e32e6ada7e11dcf2610ba8acfdea6d4b4d1
      accepted_at: 2026-11-25T17:02:58.560Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    knapsack-claim-context-packing:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:knapsack-claim-context-packing.deliver
      gate_id: deliver
      worker_id: ux003-w1-knapsack-reviewer
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 3a95302aa23fa2416850ee0b914794dab20a94c9
      committed_oid: ff338a21adb61642a74d56a45bc4829b82caa4e1
      accepted_at: 2026-11-25T15:27:31.967Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    rrf-hybrid-symbol-search:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:rrf-hybrid-symbol-search.deliver
      gate_id: deliver
      worker_id: ux003-w1-rrf-reviewer
      claim_revision: 1
      contract_revision: 1
      candidate_tree: ebfe83c295c2d0830906bc399986da4efd7e325b
      committed_oid: 01504feff193ca26a321dd1b2cd88bdc5fd85f60
      accepted_at: 2026-11-25T15:22:29.675Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    selector-getdoc-and-read-engine-compaction:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:selector-getdoc-and-read-engine-compaction.deliver
      gate_id: deliver
      worker_id: ux003-w1-selector-reviewer
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 8493265fbe3343653b6824aba08b78cfc8d43a23
      committed_oid: a89bccabbd20acd73c33a50764b3aa58c45fb9c0
      accepted_at: 2026-11-25T15:30:10.180Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Delivered selector-getdoc-and-read-engine-compaction (AC-5 / INV-SELECTOR-AND-GETDOC-BOUNDARY) after independent 6-contour review pass.
    sql-table-external-callers:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:sql-table-external-callers.deliver
      gate_id: deliver
      worker_id: ux003-w2-sql-reviewer
      claim_revision: 1
      contract_revision: 1
      candidate_tree: b341a34bd5593d730dafa19da0e2cc4676e13481
      committed_oid: 1c54f39b6e5ce88f5d4d965288f7c999886bae3c
      accepted_at: 2026-11-25T15:51:54.394Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    ux003-core-contracts-and-seam-registry:
      run_id: UX-003-knapsack-cont-20261125131925-3733606-1
      node_cursor: root:ux003-core-contracts-and-seam-registry.deliver
      gate_id: deliver
      worker_id: ux003-w0-reviewer-3
      claim_revision: 1
      contract_revision: 2
      candidate_tree: fe8e093a3a72c338474c32ea00f7d7decfb5ffd2
      committed_oid: 0c35f80a544f83d94e7415cfc104725a459deb46
      accepted_at: 2026-11-25T14:44:19.134Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    decisions:
    - 'UX-003 architecture: keep bounded deterministic knapsack in acdd-core::response and share it through the existing DomainProvider/ClaimResp assembly; rank only bounded search candidates in acdd-index with exact-search graph isolation; consume canonical SQL table nodes/edges for informational external callers while keeping signature_scope callable-only; reuse reader selector/document resolution, QueryOptions/DTOs and existing MCP/Markdown producers. Preserve ADR-0001/0003/0006/0012 layer, budget, confinement and fail-closed boundaries; no parallel parsers/resolvers, per-item DB or subprocess loops, or JSON field loss. Live owner correction: budget_items is src/state_bridge/common.rs:314, not mod.rs; knapsack task must extend admitted scope to common.rs inside the declared src/state_bridge root before editing. Public interface/schema changes explicitly specified by AC-1, AC-5 and AC-6 are escalated here: GetDoc and OverviewInput currently reside in src/mcp/tools/inputs.rs and have no include_candidates/path fields; propagate through existing owners and autonomously extend task scopes within admitted roots as needed, including code_tools for overview and any actual bridge/query owner, without altering protected manifests/profile. Preserve dependencies for shared files. Exact byte sizes include JSON envelope and separators; quantization must never exceed capacity. Verification must exercise real production seams, SQL deleted/unindexed cases, 2s doc query boundary, stable paging, exact authorizer, and representative Markdown <4 KiB with JSON parity.'
    - The scoped implementation and four executable preregistered module seams currently total approximately 195 net lines (132 tracked additions plus 63 new-file lines), above the architectural slice max_net_loc of 180. The core implementation is about 71 lines, with the remainder dedicated to the required exhaustive oracle, edge cases, and real canonical seam tests. Request architect/coordinator decision on the budget; I will not self-approve BUDGET_OVERRIDE.
    - 'Correction to #497: the budget guard compute_scoped_prod_net_loc in crates/acdd-state/src/usecases/submit/budget.rs excludes test paths. Current production-path diff is +140 lines (139 response.rs including its unit test and 1 lib.rs), below max_net_loc=180; no BUDGET_OVERRIDE is needed on the current diff. The separate lint/consumer dependency blocker #498 remains.'
    - 'Decision resolving blocker #498: extended ux003-core-contracts-and-seam-registry scope to src/state_bridge/common.rs. Wire budget_items in common.rs directly to acdd_core::pack_knapsack_indices (positional descending relevance priority until dependent task knapsack-claim-context-packing adds budget_scored_items and explicit lens scores). Verify zero unused_public_api/phantom_tested/dead_tails errors while staying within max_net_loc=180, then run acdd_verify and submit pass.'
    - '{"summary":"Authorize polish repair scope for UX-003 integration_review finding #507 (all 9 items)","items":["Extend polish scope to remediate all 9 items in #507: (1) knapsack recursive tie-breaking in crates/acdd-core/src/response.rs, (2) shared exact-fit envelope byte accounting consolidating src/state_bridge/common.rs and crates/acdd-state/src/context/tiers.rs, (3) pre-slice snippetless/module ranking stability in crates/acdd-index/src/db/symbols/search.rs, (4) exact/include_pulse=false graph isolation before search_graph_pulse across MCP/CLI/enrichment, (5) canonical quote-aware acdd_index::languages::sql::identifier_keys reuse in signature_scope.rs + index-only caller proof, (6) canonical import/export one-hop selector matching + # suffix boundary in selectors.rs, (7) ADR-0006 JSON field parity in preview.rs with Markdown-only noise suppression, (8) canonical relative path validation in reader/search.rs path-overview, (9) explicit scope authority & renewed independent integration_review + doc reconciliation."]}'
    review_summary: 'UX-003 completed: all 6 tasks delivered, #507 and #509 remediated, run_tests rev 4 (Linux + macOS + 0 lint errors / 60 rules) and integration_review rev 3 (Codex gpt-6.2-sol high, all 6 contours) passed.'
  metrics_summary:
    decisions_count: 5
    deferred_findings_count: 0
    failed_traces: 5
    passed_traces: 18
    task_receipts_count: 6
    tasks_cancelled: 0
    tasks_completed: 6
    tasks_total: 6
    total_claims: 36
    total_review_cycles: 2
    total_traces: 23
---

# UX-003: 0-1 Knapsack `ClaimResp` packing, RRF search ranking, SQL callers, selector/`get_doc` DB fixes & `<4 KiB` Markdown compaction

## Outcome and purpose

Upgrade context selection, search ranking, SQL consumer awareness, DB-backed selector/`get_doc` reliability, and Markdown payload density across the `db/`, `engine/reader.rs`, and `src/format/` modules that align with `AR-005` typed DTOs (while non-conflicting `ast_grep_search` fixes, MCP JSON schema docstring compaction, cross-tool dispatch hints, and CLI defaults ship separately in [ast-inference-schema-compaction-and-cli-parity.md](../../plans/archive/ast-inference-schema-compaction-and-cli-parity.md)):

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
extended_scope:
- path: src/state_bridge/common.rs
  reason: 'Resolve unused_public_api blocker #498 by wiring existing budget_items in src/state_bridge/common.rs to pack_knapsack_indices in Wave 0; dependent task knapsack-claim-context-packing extends this with budget_scored_items and explicit scores'
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: acdd-core-knapsack-solver
  title: Implement KnapsackItem and pack_knapsack_indices with exact/quantized bounded DP, rank tie-breaking, and unit tests in crates/acdd-core/src/response.rs and re-export in crates/acdd-core/src/lib.rs
  status: completed
  evidence: Verified KnapsackItem and pack_knapsack_indices in crates/acdd-core/src/response.rs, re-exported in lib.rs, wired to budget_items in src/state_bridge/common.rs; acdd-core --lib 49/49, --test acdd 13/13, budget_items test 1/1, clippy clean, lint 0.
- subtask_ref: pre-register-ux003-seam-tests
  title: Register tests/acdd/claim_knapsack_and_sql_callers.rs in tests/acdd/mod.rs, tests/query_context/{rrf_hybrid_search,selector_ergonomics}.rs in tests/query_context.rs, and tests/mcp/freshness/output_compaction.rs in tests/mcp/freshness.rs
  status: completed
  evidence: Verified pre-registered canonical seam test modules in tests/acdd/claim_knapsack_and_sql_callers.rs, tests/query_context/{rrf_hybrid_search,selector_ergonomics}.rs, and tests/mcp/freshness/output_compaction.rs; all compile and pass.
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:ux003-core-contracts-and-seam-registry.deliver
  gate_id: deliver
  worker_id: ux003-w0-reviewer-3
  claim_revision: 1
  contract_revision: 2
  candidate_tree: fe8e093a3a72c338474c32ea00f7d7decfb5ffd2
  committed_oid: 0c35f80a544f83d94e7415cfc104725a459deb46
  accepted_at: 2026-11-25T14:44:19.134Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

### task: knapsack-claim-context-packing

```yaml
task_ref: knapsack-claim-context-packing
target: Wire relevance-scored 0-1 knapsack packing into budget_scored_items/budget_items in src/state_bridge/, code_domain/context.rs, docs_domain.rs, and Tier 6 apply_tier6_context_lenses in crates/acdd-state/src/context/tiers.rs
scope:
- crates/acdd-state/src/context/tiers.rs
- src/state_bridge/mod.rs
- src/state_bridge/code_domain/context.rs
- src/state_bridge/docs_domain.rs
- tests/acdd/claim_knapsack_and_sql_callers.rs
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
extended_scope:
- path: src/state_bridge/common.rs
  reason: 'budget_items canonical owner resides in src/state_bridge/common.rs inside admitted src/state_bridge/ root (blackboard #495, pre_task_slice)'
depends_on:
- ux003-core-contracts-and-seam-registry
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: state-bridge-knapsack-budgeter
  title: Implement budget_scored_items over pack_knapsack_indices in src/state_bridge/mod.rs and emit explicit relevance scores in code_domain/context.rs and docs_domain.rs
  status: completed
  evidence: Verified budget_scored_items/budget_items in src/state_bridge/common.rs over pack_knapsack_indices and explicit relevance scoring in src/state_bridge/code_domain/context.rs and src/state_bridge/docs_domain.rs; budget_items_packs_ordered_candidates_within_budget (1/1) and claim_knapsack_and_sql_callers (2/2) passed.
- subtask_ref: tier6-knapsack-budget-flow
  title: Tighten Tier 6 ClaimResp byte allocation and lens fallback handling in crates/acdd-state/src/context/tiers.rs, add seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs, and update docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  status: completed
  evidence: Verified apply_tier6_context_lenses, pack_oversized_items_lens, and omit_context_lens in crates/acdd-state/src/context/tiers.rs, seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs (2/2), acdd-state context::tests (2/2), and ADR-0006 update.
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:knapsack-claim-context-packing.deliver
  gate_id: deliver
  worker_id: ux003-w1-knapsack-reviewer
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 3a95302aa23fa2416850ee0b914794dab20a94c9
  committed_oid: ff338a21adb61642a74d56a45bc4829b82caa4e1
  accepted_at: 2026-11-25T15:27:31.967Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

### task: rrf-hybrid-symbol-search

```yaml
task_ref: rrf-hybrid-symbol-search
target: Replace additive score constants and binary graph_group boosts in crates/acdd-index/src/db/symbols/search.rs with bounded RRF (k = 60) ranking, reliable pattern reporting, and module indexed_text deprioritization
scope:
- crates/acdd-index/src/db/symbols/search.rs
- tests/query_context/rrf_hybrid_search.rs
- docs/architecture/indexing.md
- docs/architecture/performance.md
depends_on:
- ux003-core-contracts-and-seam-registry
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: sqlite-rrf-symbol-ranking
  title: Implement weighted RRF (k = 60) fusion across structural tier, BM25 rank, and bounded non-containment edge degree in crates/acdd-index/src/db/symbols/search.rs, populate pattern metadata consistently, and deprioritize snippetless module indexed_text hits while preserving exact-search isolation and generation paging
  status: completed
  evidence: Verified by ux003-w1-rrf-builder (build_check pass, code.seam 1 hop, +94 net LOC) and ux003-w1-rrf-reviewer (review pass)
- subtask_ref: rrf-search-seams-and-docs
  title: Add multi-signal RRF, pattern heading, and exact-search isolation contract tests in tests/query_context/rrf_hybrid_search.rs and document RRF ranking in docs/architecture/indexing.md and docs/architecture/performance.md
  status: completed
  evidence: Verified by tests/query_context/rrf_hybrid_search.rs (3/3), search_and_paging (8/8), ranking_and_coverage (12/12), docs check pass
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:rrf-hybrid-symbol-search.deliver
  gate_id: deliver
  worker_id: ux003-w1-rrf-reviewer
  claim_revision: 1
  contract_revision: 1
  candidate_tree: ebfe83c295c2d0830906bc399986da4efd7e325b
  committed_oid: 01504feff193ca26a321dd1b2cd88bdc5fd85f60
  accepted_at: 2026-11-25T15:22:29.675Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

### task: sql-table-external-callers

```yaml
task_ref: sql-table-external-callers
target: Extend code.external_callers to surface non-test external consumers of SQL table nodes across SQL table edges without changing code.signature_scope strictness
scope:
- src/state_bridge/code_domain/mod.rs
- src/state_bridge/code_domain/signature_scope.rs
- src/state_bridge/code_domain/context.rs
- tests/acdd/claim_knapsack_and_sql_callers.rs
- docs/reference/tasks.md
depends_on:
- knapsack-claim-context-packing
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: sql-table-caller-edge-resolution
  title: Extend node/edge selection in src/state_bridge/code_domain/{mod.rs,signature_scope.rs,context.rs} so active and deleted .sql table nodes resolve external consumers via defines_table, queries_table, mutates_table, and references_table incoming edges (and baseline fallback)
  status: completed
  evidence: Batched SQL table node/edge resolution across defines_table, queries_table, mutates_table, references_table and baseline fallback in context.rs and signature_scope.rs while keeping signature_scope callable-only
- subtask_ref: sql-callers-tests-and-docs
  title: Add SQL schema consumer ClaimResp seam tests in tests/acdd/claim_knapsack_and_sql_callers.rs and update docs/reference/tasks.md
  status: completed
  evidence: Added sql_table_external_callers_and_signature_scope_seam_matrix in tests/acdd/claim_knapsack_and_sql_callers.rs and documented SQL table external_callers in docs/reference/tasks.md
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:sql-table-external-callers.deliver
  gate_id: deliver
  worker_id: ux003-w2-sql-reviewer
  claim_revision: 1
  contract_revision: 1
  candidate_tree: b341a34bd5593d730dafa19da0e2cc4676e13481
  committed_oid: 1c54f39b6e5ce88f5d4d965288f7c999886bae3c
  accepted_at: 2026-11-25T15:51:54.394Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

### task: selector-getdoc-and-read-engine-compaction

```yaml
task_ref: selector-getdoc-and-read-engine-compaction
target: Fix Type::method and 1-hop import selector resolution, decouple get_doc candidates from Detail::Full to eliminate candidate bloat and 2s SQLite timeouts, compact acdd_read search/lines defaults and doc Markdown sections
scope:
- crates/acdd-index/src/db/reader/selectors.rs
- crates/acdd-index/src/db/reader/docs.rs
- crates/acdd-index/src/db/symbols/preview.rs
- src/engine/reader.rs
- src/format/coordination.rs
- src/mcp/tools/code_tools.rs
- tests/query_context/selector_ergonomics.rs
- tests/mcp/freshness/document_responses.rs
extended_scope:
- path: src/mcp/tools/inputs.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: src/cli/args.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: src/cli/dispatch_code.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/query_context/symbol_context.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/query_context/search_and_paging.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/incremental/doc_parity.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/mcp/freshness/reference_identity.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/mcp/freshness/reference_responses.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
- path: tests/mcp/test_read.rs
  reason: 'Propagate GetDoc.include_candidates through MCP/CLI get_doc_paged callers and reconcile existing candidate and search/footer test seams per #495 and #496'
depends_on:
- ux003-core-contracts-and-seam-registry
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: selector-colon-and-import-resolution
  title: Normalize Type::method to .-boundary suffix matching, remove the scope bailout in apply_suffix_fallback, and resolve or suggest 1-hop imported/re-exported symbols for acdd_read(path, selector) in crates/acdd-index/src/db/reader/selectors.rs, plus seam tests in tests/query_context/selector_ergonomics.rs
  status: completed
  evidence: Verified Type::method normalization (split_kind_and_dotted + is_double_colon), scoped apply_suffix_fallback with .-boundary guard, and 1-hop imports/references/barrel resolution with fail-closed ambiguity in crates/acdd-index/src/db/reader/selectors.rs; selector_ergonomics (2/2 passed).
- subtask_ref: getdoc-decoupling-and-read-engine
  title: 'Gate document_reference_candidate_page strictly on include_candidates in crates/acdd-index/src/db/reader/docs.rs and src/mcp/tools/code_tools.rs (fixing 2s large-doc timeout), switch mode="search" default context to 0 with ## <path> grouping and **/archive/** exclusion, lower default line_count to 100 and trim footer/source_preview defaults in src/engine/reader.rs and crates/acdd-index/src/db/symbols/preview.rs, strip doc DB metadata/stubs and search path duplication in src/format/coordination.rs, and update docs tests in tests/mcp/freshness/document_responses.rs'
  status: completed
  evidence: 'Verified get_doc_paged include_candidates decoupling across MCP/CLI (large 80-section doc <2s), acdd_read search defaults (context=0, ## <path>, **/archive/** exclusion, line_count=100, concise footer), source_preview compaction, and coordination.rs Markdown compaction; document_responses (6/6), symbol_context (11/11), test_read (9/9), doc_parity (4/4), reference_identity (2/2), reference_responses (2/2), search_and_paging (8/8), preview (2/2), reader (1/1), commitment_integrity (9/9), clippy clean, code_map_lint 0 violations.'
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:selector-getdoc-and-read-engine-compaction.deliver
  gate_id: deliver
  worker_id: ux003-w1-selector-reviewer
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 8493265fbe3343653b6824aba08b78cfc8d43a23
  committed_oid: a89bccabbd20acd73c33a50764b3aa58c45fb9c0
  accepted_at: 2026-11-25T15:30:10.180Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Delivered selector-getdoc-and-read-engine-compaction (AC-5 / INV-SELECTOR-AND-GETDOC-BOUNDARY) after independent 6-contour review pass.
```

### task: global-envelope-discovery-and-traversal-compaction

```yaml
task_ref: global-envelope-discovery-and-traversal-compaction
target: Compact Markdown discovery, traversal, and global envelopes to hold routine navigation payloads under the 4 KiB inline threshold while preserving JSON parity and opt-in full detail/file_uris
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
extended_scope:
- path: src/mcp/tools/inputs.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: src/mcp/tools/code_tools.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: src/mcp/tools/mod.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: src/state_bridge/overview.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: crates/acdd-index/src/db/reader/types.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: crates/acdd-index/src/db/reader/search.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: crates/acdd-index/src/db/symbols/search.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: src/cli/mod.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: src/cli/dispatch_code.rs
  reason: 'Wire OverviewInput.path, PageInput.file_uris, SearchSymbols.include_pulse, code_map_overview tool description, path-scoped overview reader/bridge, and CLI call-site parity per #495/#496 and AC-6'
- path: crates/acdd-index/src/db/reader/mod.rs
  reason: Re-export overview_for_path from db::reader::search for path-scoped code_map_overview
- path: tests/mcp/context/query_and_policy.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/context/sibling_worktrees.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/freshness/governance_responses.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/freshness/reference_responses.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/freshness/manifest_freshness.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/freshness/incremental_freshness.rs
  reason: Reconcile existing tests/mcp assertions with AC-6 defaults (page_size=12, opt-in file_uris=false)
- path: tests/mcp/query_ranking.rs
  reason: Opt into file_uris=true on linked-URI assertion in tests/mcp/query_ranking.rs per AC-6
depends_on:
- knapsack-claim-context-packing
- rrf-hybrid-symbol-search
- sql-table-external-callers
- selector-getdoc-and-read-engine-compaction
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: policy-defaults-and-global-envelope-hygiene
  title: Lower default ResponsePolicy::page_size to 12 and SourceContextPolicy::max_body_lines to 100 with opt-in file_uris (false default) in crates/acdd-core/src/response.rs and acdd.yaml; suppress matched/none freshness across all Markdown tools, shorten continuing generation hashes to >=8 chars, omit compact file:// URIs, compact render_acdd_read headers, and fix trim_items_page_tail double-quoting in src/format/presentation.rs and src/mcp/server.rs
  status: completed
  evidence: Verified ResponsePolicy::page_size=12, SourceContextPolicy::max_body_lines=100, ResponsePolicy::file_uris=false in crates/acdd-core/src/response.rs and acdd.yaml; matched/none freshness suppression across navigation Markdown tools, 8-char continuing generation hashes with >=8-char prefix replay, opt-in file:// URIs (file_uris=true or detail=full), compact render_acdd_read header, and trim_items_page_tail double-quoting fix in src/format/presentation.rs, src/mcp/tools/code_tools.rs, src/mcp/tools/inputs.rs, and src/cli/mod.rs.
- subtask_ref: discovery-and-traversal-compact-renderers
  title: 'Compact code_map_search cards (1000+ total badge, capped signature, 1-line pulse), code_map_overview (omit 0-count health rows, inline compiled profiles, cap hubs, render path overview), operation_scope, and code_map_inspect in src/format/discovery.rs; compact code_map_tests breadcrumbs (~130 B/item) and impact/explain items without per-hop rs: IDs in src/format/traversal.rs and crates/acdd-index/src/db/symbols/test_discovery.rs'
  status: completed
  evidence: 'Verified code_map_search 1000+ badge, 2-line compact cards, 1-line pulse, and include_pulse=false; code_map_overview path parameter + tool description reconciliation + reader::overview_for_path + inline compiled profiles + 0-count diagnostic row omission + capped hubs (8); compact operation_scope and code_map_inspect preview flags in src/format/discovery.rs; and compact code_map_tests breadcrumbs and code_map_impact/explain items omitting per-hop rs: IDs in src/format/traversal.rs while preserving JSON parity.'
- subtask_ref: compaction-seams-and-canonical-docs
  title: Add <4 KiB payload budget regression tests in tests/mcp/freshness/output_compaction.rs, reconcile tests/mcp/freshness/document_responses.rs, tests/mcp/lint_surface.rs, tests/mcp/tool_evolution/{cli_and_relations.rs,mcp_boundaries/ast_and_search.rs}, and tests/query_context/{search_and_paging.rs,diagnostics_and_source.rs}, and update docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md, docs/reference/mcp-markdown-contracts.md, and ROADMAP.md
  status: completed
  evidence: Verified <4 KiB payload budget regression test compact_markdown_envelopes_stay_under_4kib_with_json_parity_and_opt_ins in tests/mcp/freshness/output_compaction.rs, reconciled tests across tests/mcp (163/163), tests/query_context diagnostics_and_source (9/9) and search_and_paging (8/8), tests/commitment_integrity (9/9), acdd-core response unit tests (2/2), strict clippy clean, code_map_lint 0 violations, duplicates 0, dead_tails 0 introduced, docs check pass, acdd validate pass, and updated docs/adr/0006, docs/reference/mcp-markdown-contracts.md, and ROADMAP.md.
receipt:
  run_id: UX-003-knapsack-cont-20261125131925-3733606-1
  node_cursor: root:global-envelope-discovery-and-traversal-compaction.deliver
  gate_id: deliver
  worker_id: ux003-w3-compaction-reviewer
  claim_revision: 1
  contract_revision: 5
  candidate_tree: 8a4a5a898984d01c7f02d60ba86fabe98e064332
  committed_oid: b4624e32e6ada7e11dcf2610ba8acfdea6d4b4d1
  accepted_at: 2026-11-25T17:02:58.560Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```
