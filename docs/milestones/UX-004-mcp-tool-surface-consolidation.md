---
id: UX-004-mcp-tool-surface-consolidation
title: "Lean MCP surface (20 -> 18 tools, active-run-only acdd_state) and read-only code_map_analyze diagnostics (diff_impact, boundary, path, entrypoints, PageRank hotspots, cochange)"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/git.rs
  - crates/acdd-core/src/ports.rs
  - crates/acdd-core/src/subsystems.rs
  - crates/acdd-index/src/db/
  - crates/acdd-state/src/profile/embedded/
  - src/engine/guide.rs
  - src/engine/reader.rs
  - src/state_bridge/mod.rs
  - src/format/
  - src/cli/
  - src/mcp/tools/
  - src/mcp/server/
  - benchmarks/profiles/
  - tests/query_context/
  - tests/mcp/
  - tests/acdd/
  - AGENTS.md
  - src/AGENTS.md
  - README.md
  - INSTALL.md
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/UX-004-mcp-tool-surface-consolidation.md
invariants:
  - 'INV-LEAN-STATIC-MCP-SURFACE: Static MCP tool count drops cleanly from 20 to 18 (9 code, 2 docs, 2 core, 5 active-run state) across Server::tool_router() and crates/acdd-core/src/ports.rs::BUILTIN_MCP_TOOLS by folding code_map_prove_removal and code_map_tests into code_map_query(operation="prove_removal" | "tests") with zero new fields on code_map_query and zero new MCP tools.'
  - 'INV-ACTIVE-RUN-MCP-STATE: MCP acdd_state focuses strictly on active-run lifecycle operations; out-of-workflow spec planning/authoring actions (roadmap, draft) are removed from McpStateAction and StateInput with fail-closed redirect hints to CLI acdd roadmap and acdd draft.'
  - 'INV-COMPACT-ANALYZE-EXTENSION: All six new analysis modes (diff_impact, boundary, path, entrypoints, hotspots, cochange) live inside existing code_map_analyze adding strictly 3 compact optional schema properties (mode, to, base), preserving default mode="summary", include_cycles, lint, and read-only SQL compatibility.'
  - 'INV-READONLY-OVER-INDEX: UX-004 analyzers stay a pure read-only layer over already-indexed code-map.sqlite facts (nodes, edges, details.entrypoint, doc_sections, is_test) without modifying language extractors, linkers, or lint pipelines (Rust rmcp/clap/inline-mod extraction belongs to LS-004).'
  - 'INV-SINGLE-OWNER-DIFF-IMPACT: code_map_analyze(mode="diff_impact") delegates strictly to the canonical crates/acdd-index/src/db/traversal/change_impact.rs engine, computing signature_touched from indexed declaration line bounds (no cat-file blob reparsing), verified call-graph uncovered_symbols, and governed INV-* affected_docs.'
  - 'INV-DETERMINISTIC-PAGERANK: PageRank centrality in hotspots computes deterministically over sorted node_hash graphs loaded from code-map.sqlite (damping 0.85, bounded iterations and graph limits within the 2-second query budget) with zero SQLite schema changes and zero new third-party dependencies, reusable by AC-007 GET /api/map.'
  - 'INV-GIT-SINGLE-OWNER: Bounded Git history for cochange (--name-status -z --find-renames, zero --find-copies-harder) and diff_impact invoke Git exclusively through crates/acdd-core/src/git.rs and exclude sensitive/internal/DB paths via WorkspacePathPolicy and shared sensitive-path matching.'
acceptance_criteria:
  - 'AC-01: GIVEN an indexed workspace, WHEN code_map_analyze(mode="hotspots" | "boundary" | "path" | "entrypoints") runs, THEN hotspots returns deterministic PageRank centrality, fan-in/fan-out, LOC/complexity proxy, and call-based test-gap rankings within the 2-second query budget; boundary reports inbound external callers, grouped outbound dependencies, and pub symbols with zero external callers; path returns shortest/top-K typed edge chains (failing closed when to is omitted); and entrypoints traces reverse paths to stored nodes.details.entrypoint roots plus uncalled public root symbols.'
  - 'AC-02: GIVEN a Git workspace with uncommitted changes or commit history, WHEN code_map_analyze(mode="diff_impact" | "cochange") runs, THEN diff_impact delegates through canonical change_impact.rs and returns changed symbols, signature_touched flags (by declaration header overlap, zero cat-file reparsing), governed INV-* affected_docs, reaching test_case selectors, and verified uncovered_symbols (excluding lexical fallback); cochange returns bounded Git co-change pairs lacking direct static edges while excluding WorkspacePathPolicy/sensitive paths and binding continuations to history+policy fingerprint; non-Git roots fail closed cleanly.'
  - 'AC-03: GIVEN the updated MCP server under full preset, WHEN tools/list is queried, THEN exactly 18 built-in static tools are exposed matching BUILTIN_MCP_TOOLS (code_map_prove_removal and code_map_tests removed; code_map_analyze preserved with only mode/to/base added; zero new fields on code_map_query; net tools/list schema bytes shrink), plugin registration reserves retired tool names, embedded profiles and reader/guide hints omit removed tool names, and code_map_query(operation="prove_removal" | "tests") matches former dedicated tool output 1:1.'
  - 'AC-04: GIVEN direct MCP calls to removed tools (code_map_prove_removal, code_map_tests) or removed acdd_state actions (roadmap, draft), WHEN executed, THEN every call fails closed with an explicit redirect hint (code_map_query(operation=...) or CLI acdd roadmap / acdd draft), while CLI acdd query remove, acdd query tests, acdd query analyze --mode, acdd roadmap, and acdd draft remain fully functional.'
  - 'AC-05: GIVEN the completed UX-004 tree, WHEN tests/query_context/, tests/mcp/, tests/acdd/, acdd docs check, acdd validate, and acdd lint --categories arch,docs run, THEN active docs, ADRs, routers, skills/acdd/, benchmarks/profiles/, ROADMAP.md, and CHANGELOG.md reflect the 18 static MCP tools, active-run-only acdd_state, and code_map_analyze modes with zero errors.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# UX-004: Lean MCP surface (20 → 18 tools, active-run-only `acdd_state`) and read-only `code_map_analyze` diagnostics

## Goal & Non-goals

**Goal.** Shrink static MCP schema overhead and give active coding, review, and planning agents single-call structural diagnostics strictly as a **pure read-only layer over already-indexed facts**:
1. **Lean MCP surface (20 → 18 static tools, zero new tools):** Fold `code_map_prove_removal` and `code_map_tests` into `code_map_query(operation="prove_removal" | "tests")` reusing existing fields (`0` new schema properties on `code_map_query`), and remove out-of-workflow `roadmap` and `draft` actions from MCP `acdd_state` (keeping `acdd_state` strictly for active runs while CLI `acdd roadmap` / `acdd draft` stay first-class).
2. **Read-only `code_map_analyze` modes:** Extend existing `code_map_analyze` with 3 compact optional fields (`mode`, `to`, `base`) delivering deterministic PageRank `hotspots` (reusable by [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md) `GET /api/map`), `boundary`, `path`, `entrypoints`, canonical `diff_impact` (reusing `change_impact.rs` with header-line `signature_touched` and zero `cat-file` blob reparsing), and bounded Git `cochange`.

**Non-goals.**
- Modifying Rust/language AST extractors, cross-file Clap/rmcp linkers, or inline `mod` export publication (owned by [LS-004](archive/LS-004-rust-entrypoints-and-inline-module-exports.md)).
- Adding OID-pinned `git cat-file` blob streaming or quadratic `--find-copies-harder` scans.
- Adding any `acdd_spec` MCP tool or new third-party dependencies in `Cargo.toml`.
- Changing SQLite `code-map.sqlite` schema version or altering CLI `acdd query remove` / `acdd query tests` / `acdd roadmap` / `acdd draft`.

## Architecture & ownership map

### 1. MCP surface consolidation (`20 -> 18` static tools)

| Removed MCP surface | Replacement | Consumers updated | CLI parity |
|---|---|---|---|
| `code_map_prove_removal` | `code_map_query(operation="prove_removal", selector=..., path=...)` | `crates/acdd-core/src/ports.rs`, `src/mcp/tools/`, `crates/acdd-index/src/db/traversal/`, `crates/acdd-state/src/profile/embedded/`, `src/engine/guide.rs`, `benchmarks/profiles/`, `tests/mcp/` | `acdd query remove` (unchanged) |
| `code_map_tests` | `code_map_query(operation="tests", selector=..., path=...)` | Same + `src/engine/reader.rs` active-gate hint + `tests/query_context/ranking_and_coverage.rs` | `acdd query tests` (unchanged) |
| `acdd_state(action: roadmap \| draft)` | CLI `acdd roadmap` / `acdd draft` (fail-closed redirect hint) | `src/mcp/tools/{inputs,state_tools,dispatch}.rs`, `crates/acdd-state/src/profile/embedded/spec_planning.yaml`, `tests/mcp/`, docs | `acdd roadmap`, `acdd draft` (unchanged) |
| `code_map_analyze` *(extended)* | `+3` optional fields (`mode`, `to`, `base`), default `summary` unchanged | `src/mcp/tools/{inputs,code_tools}.rs`, `src/cli/{args,dispatch_code}.rs`, `src/format/` | `acdd query analyze --mode` |

### 2. Read-only `code_map_analyze` diagnostic modes

| `mode` | Canonical owner | Existing data reused (zero extractor/schema changes) |
|---|---|---|
| `hotspots` | `crates/acdd-index/src/db/analyze/hotspots.rs` | `nodes`, `edges`, `calls` test coverage (`db/symbols/test_discovery.rs`), deterministic PageRank (`damping = 0.85`) |
| `boundary` | `crates/acdd-index/src/db/analyze/boundary.rs` | `nodes`, `edges`, `path_dictionary`, `dependencies` (`is_public` with 0 external callers) |
| `path` | `crates/acdd-index/src/db/analyze/path.rs` | `nodes`, `edges`, `edge_occurrences` |
| `entrypoints` | `crates/acdd-index/src/db/analyze/entrypoints.rs` | Reverse `edges`, stored `nodes.details['entrypoint']` (auto-enriched later by `LS-004`), uncalled public root symbols |
| `diff_impact` | `crates/acdd-index/src/db/traversal/change_impact.rs` | Existing Git hunk ranges + `nodes.line..end_line` (`signature_touched` by declaration line overlap) + `doc_sections` (`INV-*`) + verified `calls` `test_case` reachability (`uncovered_symbols`) |
| `cochange` | `crates/acdd-index/src/db/analyze/cochange.rs` + `crates/acdd-core/src/git.rs` | Bounded `git log --no-merges --name-status -z --find-renames` + `edges` difference + `WorkspacePathPolicy` + shared sensitive-path matcher |

## Tasks in this milestone

### task: index-analyze-graph-and-pagerank-engine

```yaml
task_ref: index-analyze-graph-and-pagerank-engine
target: "Implement pure read-only graph analyzers (deterministic PageRank hotspots, boundary, path, and entrypoints) under crates/acdd-index/src/db/analyze/ plus shared verified calls test-coverage helper in db/symbols/test_discovery.rs within the 2-second query budget, with contract tests in tests/query_context/"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/db/analyze/
  - crates/acdd-index/src/db/mod.rs
  - crates/acdd-index/src/db/paging.rs
  - crates/acdd-index/src/db/reader/
  - crates/acdd-index/src/db/symbols/
  - tests/query_context/
status: planned
subtasks:
  - subtask_ref: pagerank-hotspots-and-boundary
    title: "Add shared verified calls-edge test-coverage helper in db/symbols/test_discovery.rs, implement deterministic PageRank (sorted node_hash, damping 0.85, bounded work/clock checks) hotspots and pure graph boundary analyzer (inbound callers, grouped outbound deps, pub symbols with 0 external callers)"
  - subtask_ref: shortest-path-and-entrypoints
    title: "Implement bounded shortest/top-K path tracer (failing closed when destination is missing) and reverse entrypoints analyzer over stored nodes.details.entrypoint plus uncalled public roots, with tests in tests/query_context/"
```

- **Wave**: `0` | **Budget**: `~500` net LOC.
- **Seam & mutation proof (`AC-01`)**: `tests/query_context/` proves deterministic PageRank ordering and budget bounds, exact `boundary` inbound/outbound/internal-only `pub` counts, `path` shortest edge chain resolution + missing-`to` rejection, and `entrypoints` root tracing over stored entrypoint categories.

### task: git-diff-impact-and-cochange-analyzers

```yaml
task_ref: git-diff-impact-and-cochange-analyzers
target: "Extend canonical change_impact.rs with signature_touched (declaration line overlap, zero cat-file reparsing), verified uncovered_symbols, governed INV-* affected_docs, and optional base ref, implement bounded cochange in crates/acdd-index/src/db/analyze/cochange.rs via crates/acdd-core/src/git.rs + WorkspacePathPolicy, and add compact Markdown formatters in src/format/"
contract_revision: 1
depends_on:
  - index-analyze-graph-and-pagerank-engine
tags:
  - default
scope:
  - crates/acdd-core/src/git.rs
  - crates/acdd-index/src/db/source.rs
  - crates/acdd-index/src/db/traversal/change_impact.rs
  - crates/acdd-index/src/db/traversal/mod.rs
  - crates/acdd-index/src/db/analyze/
  - src/format/
  - tests/query_context/
status: planned
subtasks:
  - subtask_ref: canonical-diff-impact-enrichment-and-cochange
    title: "Add bounded git log --name-status -z --find-renames helper in crates/acdd-core/src/git.rs and shared sensitive/policy path filter in db/source.rs; extend change_impact.rs with signature_touched, verified uncovered_symbols (excluding lexical fallback), governed INV-* affected_docs, and base ref; implement cochange with history+policy continuation fingerprint"
  - subtask_ref: analyze-markdown-formatters-and-tests
    title: "Add compact Markdown presentation for all 6 code_map_analyze modes in src/format/ and verify diff_impact, cochange, sensitive/DB-path exclusion, continuation invalidation, and non-Git fail-closed behavior in tests/query_context/"
```

- **Wave**: `1` | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-02`)**: `tests/query_context/` proves `diff_impact` delegates through `change_impact.rs` and reports changed symbols, `signature_touched`, governed `INV-*` docs, reaching `test_case` nodes, and verified `uncovered_symbols` (excluding lexical fallback); `cochange` surfaces unlinked co-changed files while excluding sensitive/policy paths and failing closed outside Git repos.

### task: consolidate-mcp-surface-and-wire-analyze

```yaml
task_ref: consolidate-mcp-surface-and-wire-analyze
target: "Fold code_map_prove_removal and code_map_tests into code_map_query (20 -> 18 static MCP tools synced with ports.rs), remove out-of-workflow roadmap and draft actions from MCP acdd_state with fail-closed CLI redirect hints, update embedded profiles, reader/guide hints, and benchmark profiles, wire code_map_analyze mode/to/base across MCP and CLI, and update test suites"
contract_revision: 1
depends_on:
  - git-diff-impact-and-cochange-analyzers
tags:
  - default
scope:
  - crates/acdd-core/src/ports.rs
  - crates/acdd-core/src/subsystems.rs
  - crates/acdd-index/src/db/traversal/types.rs
  - crates/acdd-index/src/db/traversal/mod.rs
  - crates/acdd-state/src/profile/embedded/
  - src/engine/guide.rs
  - src/engine/reader.rs
  - src/state_bridge/mod.rs
  - src/format/
  - src/cli/
  - src/mcp/tools/
  - src/mcp/server/
  - benchmarks/profiles/
  - tests/mcp/
  - tests/query_context/
  - tests/acdd/
status: planned
subtasks:
  - subtask_ref: fold-tools-and-trim-mcp-state-actions
    title: "Add GraphQueryOp::{Tests, ProveRemoval}, route operation='prove_removal' and operation='tests' through code_map_query (zero new schema fields), sync BUILTIN_MCP_TOOLS (20 -> 18) and retired-name plugin reservation in ports.rs, unregister code_map_prove_removal and code_map_tests, update embedded profiles + src/engine/{guide,reader}.rs + benchmarks/profiles/, remove roadmap/draft from McpStateAction/StateInput, and fail closed with redirect hints"
  - subtask_ref: wire-analyze-modes-and-verify-mcp-schema
    title: "Add optional mode/to/base to Analyze and CLI acdd query analyze (passing resolved WorkspacePathPolicy via src/state_bridge) preserving default summary/include_cycles/lint/SQL compatibility, verify 18 static MCP tools and net tools/list schema byte reduction in tests/mcp/, and confirm CLI parity"
```

- **Wave**: `2` | **Budget**: `~450` net LOC (deletions + dispatch wiring).
- **Seam & mutation proof (`AC-03`, `AC-04`)**: `tests/mcp/`, `tests/query_context/`, and `tests/acdd/` prove `tools/list` returns 18 built-in tools matching `BUILTIN_MCP_TOOLS` with net smaller schema bytes, `code_map_query` serves `prove_removal` and `tests` with full parity, `acdd_state` exposes active-run actions only, removed tool/action calls fail closed with redirect hints, and CLI commands work cleanly.

### task: reconcile-mcp-tool-catalog-docs-and-skills

```yaml
task_ref: reconcile-mcp-tool-catalog-docs-and-skills
target: "Update active AGENTS.md/src/AGENTS.md, ADRs, README/INSTALL, docs/architecture/, docs/reference/, docs/runbooks/, skills/acdd/, ROADMAP.md, and CHANGELOG.md for the 18 static MCP tools, active-run-only acdd_state, and code_map_analyze modes"
contract_revision: 1
depends_on:
  - consolidate-mcp-surface-and-wire-analyze
tags:
  - docs
scope:
  - AGENTS.md
  - src/AGENTS.md
  - README.md
  - INSTALL.md
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
status: planned
subtasks:
  - subtask_ref: update-mcp-reference-and-skills
    title: "Update active tool counts (20 -> 18), active-run-only acdd_state guidance, code_map_query prove_removal/tests examples, and code_map_analyze mode documentation across active docs, ADRs, routers, skills/acdd/, ROADMAP.md, and CHANGELOG.md"
  - subtask_ref: verify-docs-lint-and-suites
    title: "Verify acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --test mcp --test query_context --test acdd pass with zero errors"
```

- **Wave**: `3` | **Budget**: `~220` net LOC (docs/skills).
- **Seam & mutation proof (`AC-05`)**: `acdd docs check`, `acdd validate`, `acdd lint --categories arch,docs`, and `cargo test --test mcp --test query_context --test acdd` pass with zero stale active `20 static` / `code_map_prove_removal` / `code_map_tests` / `acdd_state(action: roadmap|draft)` instructions.

## Negative boundaries

- No new static MCP tools (`acdd_spec` excluded; net count drops `20 -> 18`) and no new third-party dependencies in `Cargo.toml`.
- No edits to language extractors (`languages/`), `ast/visitor.rs`, `linker/`, or `db/lint/unused_public_api.rs` inside `UX-004` (owned by [LS-004](archive/LS-004-rust-entrypoints-and-inline-module-exports.md)).
- No `git cat-file` blob reparsing or `--find-copies-harder` scans inside `UX-004`.
- No new schema properties on `code_map_query` (`Query` input struct) and no SQLite schema version bump.
- No raw `Command::new("git")` outside `crates/acdd-core/src/git.rs` (`INV-GIT-SINGLE-OWNER`).
- No exposure of sensitive or `WorkspacePathPolicy`-excluded paths in `diff_impact` or `cochange`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Waves 0–3 covering read-only graph/PageRank analyzers, canonical `change_impact.rs` + bounded Git `cochange`, `20 -> 18` MCP tool/action consolidation synced with `ports.rs`, and full active doc/router reconciliation.
- **Correctness**: Enforces deterministic PageRank within 2-second budgets, `INV-READONLY-OVER-INDEX`, `INV-SINGLE-OWNER-DIFF-IMPACT`, `INV-GIT-SINGLE-OWNER`, `WorkspacePathPolicy` filtering, and fail-closed redirect hints.
- **Coherence**: Ready immediately in Wave 0 (`depends_on: []`), cleanly separated from Rust extractor/linker upgrades in [LS-004](archive/LS-004-rust-entrypoints-and-inline-module-exports.md), ahead of [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md), and sharing deterministic PageRank `hotspots` with [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md).
