---
id: AR-007-host-sqlite-engine-isolation-and-workspace-facade
title: "Confine host SQLite/Tree-sitter access inside acdd-index and unify CLI/MCP/HTTP runtime wiring behind WorkspaceServices"
doc_type: contract
status: planned
depends_on:
  - AC-005-unified-swarm-gates-and-peer-board-coordination
guidance:
  hitl: pro
owners:
  - Cargo.toml
  - acdd.yaml
  - crates/acdd-index/Cargo.toml
  - crates/acdd-index/src/
  - crates/acdd-state/src/usecases/
  - src/engine/
  - src/state_bridge/
  - src/cli/
  - src/mcp/
  - tests/core/
  - tests/mcp/
  - tests/acdd/
  - tests/commitment_integrity/
  - docs/adr/0001-crate-dag-and-layer-boundaries.md
  - docs/architecture/
  - src/AGENTS.md
  - crates/acdd-index/AGENTS.md
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AR-007-host-sqlite-engine-isolation-and-workspace-facade.md
invariants:
  - 'INV-HOST-SQLITE-CONFINED: Production modules in host src/** never import rusqlite directly (ARCH-HOST-SQLITE-CONFINED); all code-map SQLite access routes through typed acdd-index::db facades (CodeMapReader / CodeMapStore) and all state DB reads route through acdd-state use cases.'
  - 'INV-HOST-PARSER-FREE: Root Cargo.toml drops direct rusqlite, tree-sitter, and pulldown-cmark dependencies; AST pattern search and cold/delta indexing pipeline internals live in crates/acdd-index, keeping src/ a thin transport and domain-wiring host.'
  - 'INV-WORKSPACE-SERVICES-SINGLE-OWNER: One canonical WorkspaceServices facade in src/state_bridge/ owns workspace/worktree adapter resolution, CodeMapReader access, DomainProvider assembly, and acdd-spec/acdd-state dispatch for CLI, MCP, and AC-007 HTTP acdd serve, eliminating duplicated CliStateCtx / StateCallContext wiring and direct SqliteStateReader bypasses in src/state_bridge/mod.rs.'
  - 'INV-PROTECTED-PATHS-AUTHORIZED: This milestone explicitly authorizes root Cargo.toml, crates/acdd-index/Cargo.toml, and acdd.yaml architectural rule updates with zero new third-party dependencies.'
  - 'INV-BEHAVIOR-PRESERVING-REFACTOR: Strictly structural boundary hardening with zero CLI/MCP wire-contract regressions, zero SQLite schema changes, and full parity across core, mcp, acdd, and commitment_integrity suites before AC-007 launches acdd serve.'
acceptance_criteria:
  - 'AC-01: GIVEN typed CodeMapReader / CodeMapStore query and write facades in crates/acdd-index/src/db/ plus AST search moved into crates/acdd-index/src/ast/, WHEN src/state_bridge/, src/mcp/, and src/cli/ query code-map symbols, docs, diagnostics, seams, or AST matches, THEN every host caller uses typed acdd-index APIs with zero direct rusqlite or tree_sitter imports.'
  - 'AC-02: GIVEN the cold and incremental delta indexing pipeline and layered reader moved from src/engine/ into crates/acdd-index/src/pipeline/ (preserving ARCH-PIPELINE-BOUNDARY-ENTRY and PipelineTestHooks seam tests), WHEN tests/core/ and tests/mcp/freshness/ run, THEN cold build, delta invalidation, staged WAL publication, and reader-generation locking pass with zero regressions.'
  - 'AC-03: GIVEN WorkspaceServices in src/state_bridge/ unifying adapter loading, CodeMapReader lifecycle, DomainProvider wiring, and acdd-spec / acdd-state dispatch (replacing duplicated CliStateCtx, StateCallContext, and direct SqliteStateReader opens in src/state_bridge/mod.rs), WHEN CLI and MCP commands/queries execute across primary and linked worktrees, THEN every command/query passes through one canonical owner ready for AC-007 read-only acdd serve reuse.'
  - 'AC-04: GIVEN root Cargo.toml free of direct rusqlite, tree-sitter, and pulldown-cmark dependencies and acdd.yaml enforcing ARCH-HOST-SQLITE-CONFINED + ARCH-HOST-PARSER-FREE, WHEN acdd lint --categories arch and cargo test --test commitment_integrity run, THEN any direct rusqlite, tree_sitter, or pulldown_cmark import in host src/** fails closed.'
  - 'AC-05: GIVEN the completed AR-007 tree, WHEN acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --workspace --all-targets run, THEN ADR-0001, docs/architecture/, src/AGENTS.md, crates/acdd-index/AGENTS.md, ROADMAP.md, and CHANGELOG.md pass with zero errors and zero regressions.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# AR-007: Confine host SQLite/Tree-sitter access inside `acdd-index` and unify CLI/MCP/HTTP runtime wiring behind `WorkspaceServices`

## Goal & Non-goals

**Goal.** Eliminate host-layer leakage of `rusqlite`, `tree-sitter`, and `pulldown-cmark` across `src/` (~20 files spanning `src/engine/`, `src/state_bridge/`, `src/cli/`, and `src/mcp/`), move low-level indexing pipeline and AST search internals into `crates/acdd-index`, and consolidate duplicated CLI/MCP workspace wiring behind one canonical `WorkspaceServices` facade in `src/state_bridge/`. Executed strictly after [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) and immediately before [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md) so `acdd mcp` and read-only `acdd serve` mount on a clean, SQLite-free host transport layer with zero duplicated runtime wiring.

**Non-goals.**
- Introducing HTTP serving, Web UI, SSE, Prometheus `/metrics`, or OTLP trace export (all owned by [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)).
- Multi-project `WorkspaceRegistry` or authenticated ingress (deferred to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)).
- Adding third-party crates, changing SQLite table schemas, or altering CLI/MCP wire formats.

## Architecture & ownership map

### 1. Host boundary target (`ADR 0001` hardening)

```text
  src/cli/ (clap)  ─────┐
  src/mcp/ (rmcp)  ─────┼──► src/state_bridge/WorkspaceServices (single runtime facade)
  src/observability/ ───┘        │                 │                 │
    (AC-007 next)                ▼                 ▼                 ▼
                           acdd-state          acdd-spec         acdd-index
                         (workflow/state      (specs/DAG)     (ast, pipeline,
                          SQLite confined)                     db::CodeMapReader
                                 │                 │           SQLite + TS confined)
                                 └────────┬────────┘                 │
                                          ▼                          ▼
                                    crates/acdd-core (strict leaf)
```

### 2. Consumer & execution path map

| Leakage surface today | Canonical owner after `AR-007` | Host consumers updated | Fail-closed enforcement |
|---|---|---|---|
| Direct `rusqlite::Connection` in `src/mcp/server/{admission,types}.rs`, `src/mcp/response.rs`, `src/cli/dispatch_code.rs`, `src/mcp/tools/code_tools.rs` | `crates/acdd-index/src/db/` (`CodeMapReader` + typed `IndexError` classification for SQLite interrupts/limits) | `src/mcp/`, `src/cli/`, `src/state_bridge/` | `ARCH-HOST-SQLITE-CONFINED` in `acdd.yaml` + root `Cargo.toml` drops `rusqlite` |
| Direct SQL queries in `src/state_bridge/{common,docs_domain,overview}.rs` and `src/state_bridge/code_domain/*.rs` | Typed query methods on `acdd_index::db::reader` (`CodeMapReader`) | `src/state_bridge/` | `ARCH-HOST-SQLITE-CONFINED` + `tests/commitment_integrity/` |
| `src/engine/ast.rs` (`tree_sitter::Node`), `src/engine/reader.rs`, and `src/engine/pipeline/*.rs` (~7.4K LOC) | `crates/acdd-index/src/{ast,reader,pipeline}/` (preserving `ARCH-PIPELINE-BOUNDARY-ENTRY`) | `src/cli/`, `src/mcp/`, `src/state_bridge/`, `tests/core/` | `ARCH-HOST-PARSER-FREE` + root `Cargo.toml` drops `tree-sitter` & `pulldown-cmark` |
| Duplicated `CliStateCtx` + `StateCallContext` + `src/cli/validate.rs` + direct `SqliteStateReader::new` in `src/state_bridge/mod.rs` | `src/state_bridge::WorkspaceServices` + read-only `acdd-state` query entrypoint for active worktree node lookup | `src/cli/`, `src/mcp/`, and upcoming `AC-007` `src/observability/` | `INV-WORKSPACE-SERVICES-SINGLE-OWNER` + `tests/commitment_integrity/` |

## Tasks in this milestone

### task: index-codemap-facade-and-ast-migration

```yaml
task_ref: index-codemap-facade-and-ast-migration
target: "Introduce typed CodeMapReader / CodeMapStore and IndexError interrupt helpers in crates/acdd-index/src/db/, move AST pattern search out of src/engine/ast.rs into crates/acdd-index/src/ast/, and expose typed query entrypoints covering all src/state_bridge and src/mcp code-map reads"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/db/
  - crates/acdd-index/src/ast/
  - crates/acdd-index/src/lib.rs
  - src/engine/ast.rs
  - src/engine/mod.rs
  - tests/query_context/
status: planned
subtasks:
  - subtask_ref: typed-codemap-reader-and-error-surface
    title: "Add typed CodeMapReader / CodeMapStore wrappers, read-only session handle, and SQLite interrupt/limit diagnostic classifiers in crates/acdd-index/src/db/ so callers never touch raw rusqlite::Connection or rusqlite::Error"
  - subtask_ref: move-ast-search-into-acdd-index
    title: "Relocate AST pattern search and tree_sitter::Node diagnostics from src/engine/ast.rs into crates/acdd-index/src/ast/ with full test parity in tests/query_context/"
```

- **Wave**: `0` | **Budget**: `~700` net LOC (typed wrappers + move).
- **Seam & mutation proof (`AC-01`)**: `tests/query_context/` and `crates/acdd-index` unit tests prove AST search, doc/seam/overview queries, and interrupt classification execute strictly via `CodeMapReader` with zero `rusqlite` or `tree_sitter` types exposed in public signatures.

### task: move-indexing-pipeline-into-acdd-index

```yaml
task_ref: move-indexing-pipeline-into-acdd-index
target: "Move cold/delta indexing pipeline and layered reader from src/engine/{pipeline,reader}.rs into crates/acdd-index/src/{pipeline,reader}/ while preserving ARCH-PIPELINE-BOUNDARY-ENTRY, staged WAL publication, and PipelineTestHooks seam tests"
contract_revision: 1
depends_on:
  - index-codemap-facade-and-ast-migration
tags:
  - default
scope:
  - crates/acdd-index/src/pipeline/
  - crates/acdd-index/src/reader.rs
  - crates/acdd-index/src/lib.rs
  - src/engine/
  - tests/core/
status: planned
subtasks:
  - subtask_ref: relocate-pipeline-and-reader-modules
    title: "Move src/engine/pipeline/*.rs and src/engine/reader.rs into crates/acdd-index/src/ preserving cold/delta/invalidation/staging/hydration invariants and PipelineTestHooks"
  - subtask_ref: slim-host-engine-wrappers
    title: "Reduce src/engine/ to thin host guide/docs-check orchestration free of rusqlite, tree_sitter, and pulldown_cmark imports; verify tests/core/ cold/delta/recovery suites pass"
```

- **Wave**: `1` (concurrent with `workspace-services-facade-and-bridge-cleanup`, strictly disjoint scopes) | **Budget**: `~650` net LOC (mostly mechanical moves).
- **Seam & mutation proof (`AC-02`)**: `tests/core/` cold build, delta invalidation, and database publication recovery suites pass with zero regressions and zero `rusqlite` imports remaining in `src/engine/`.

### task: workspace-services-facade-and-bridge-cleanup

```yaml
task_ref: workspace-services-facade-and-bridge-cleanup
target: "Introduce canonical WorkspaceServices facade in src/state_bridge/, migrate src/state_bridge/, src/cli/, and src/mcp/ off raw rusqlite::Connection and direct SqliteStateReader opens onto CodeMapReader and acdd-state use cases, and eliminate duplicated CliStateCtx / StateCallContext wiring"
contract_revision: 1
depends_on:
  - index-codemap-facade-and-ast-migration
tags:
  - default
scope:
  - crates/acdd-state/src/usecases/
  - src/state_bridge/
  - src/cli/
  - src/mcp/
  - tests/mcp/
  - tests/acdd/
status: planned
subtasks:
  - subtask_ref: workspace-services-and-state-bridge-migration
    title: "Implement WorkspaceServices in src/state_bridge/, replace direct SqliteStateReader instantiation in src/state_bridge/mod.rs with a read-only acdd-state usecase query, and migrate CodeDomainProvider / DocsDomainProvider / overview to typed CodeMapReader calls"
  - subtask_ref: unify-cli-and-mcp-runtime-contexts
    title: "Rewire src/cli/ (dispatch_code, dispatch_state, validate) and src/mcp/ (server, response, code_tools, state_tools) onto WorkspaceServices and CodeMapReader with zero direct rusqlite imports; verify tests/mcp/ and tests/acdd/ pass"
```

- **Wave**: `1` (concurrent with `move-indexing-pipeline-into-acdd-index`, strictly disjoint scopes) | **Budget**: `~750` net LOC.
- **Seam & mutation proof (`AC-03`)**: `tests/mcp/` (including freshness/admission suites) and `tests/acdd/` prove CLI and MCP dispatch through one `WorkspaceServices` owner with zero raw `rusqlite` or `SqliteStateReader` usage in `src/`.

### task: enforce-host-isolation-and-drop-root-deps

```yaml
task_ref: enforce-host-isolation-and-drop-root-deps
target: "Remove direct rusqlite, tree-sitter, and pulldown-cmark dependencies from root Cargo.toml, add ARCH-HOST-SQLITE-CONFINED and ARCH-HOST-PARSER-FREE rules to acdd.yaml, update ARCH-PIPELINE-BOUNDARY-ENTRY paths, and add commitment_integrity host boundary fences"
contract_revision: 1
depends_on:
  - move-indexing-pipeline-into-acdd-index
  - workspace-services-facade-and-bridge-cleanup
tags:
  - default
scope:
  - Cargo.toml
  - crates/acdd-index/Cargo.toml
  - acdd.yaml
  - tests/commitment_integrity/
status: planned
subtasks:
  - subtask_ref: drop-root-sqlite-and-parser-deps
    title: "Remove direct rusqlite, tree-sitter, and pulldown-cmark entries from root Cargo.toml (keeping them confined inside member library crates) and verify cargo check --workspace --all-targets"
  - subtask_ref: add-arch-rules-and-boundary-tests
    title: "Add ARCH-HOST-SQLITE-CONFINED and ARCH-HOST-PARSER-FREE rules in acdd.yaml, retarget ARCH-PIPELINE-BOUNDARY-ENTRY to crates/acdd-index/src/pipeline/, and add compiler/lint boundary assertions in tests/commitment_integrity/"
```

- **Wave**: `2` | **Budget**: `~200` net LOC.
- **Seam & mutation proof (`AC-04`)**: `acdd lint --categories arch` and `cargo test --test commitment_integrity` pass and fail closed if any host `src/**` file adds a `rusqlite`, `tree_sitter`, or `pulldown_cmark` import.

### task: docs-adr-and-roadmap-reconciliation

```yaml
task_ref: docs-adr-and-roadmap-reconciliation
target: "Reconcile ADR 0001, docs/architecture/, src/AGENTS.md, crates/acdd-index/AGENTS.md, ROADMAP.md, and CHANGELOG.md with the SQLite/parser-free host boundary and WorkspaceServices facade"
contract_revision: 1
depends_on:
  - enforce-host-isolation-and-drop-root-deps
tags:
  - docs
scope:
  - docs/adr/0001-crate-dag-and-layer-boundaries.md
  - docs/architecture/
  - src/AGENTS.md
  - crates/acdd-index/AGENTS.md
  - ROADMAP.md
  - CHANGELOG.md
status: planned
subtasks:
  - subtask_ref: update-adr0001-and-routers
    title: "Update ADR 0001 layer diagram/invariants, docs/architecture/indexing.md + concurrency.md, src/AGENTS.md, crates/acdd-index/AGENTS.md, ROADMAP.md, and CHANGELOG.md"
  - subtask_ref: verify-docs-lint-and-workspace-suite
    title: "Verify acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --workspace --all-targets pass with zero errors"
```

- **Wave**: `3` | **Budget**: `~250` net LOC.
- **Seam & mutation proof (`AC-05`)**: `acdd docs check`, `acdd validate`, `acdd lint --categories arch,docs`, and `cargo test --workspace --all-targets` pass with zero errors.

## Negative boundaries

- No new third-party dependencies in any `Cargo.toml`.
- No HTTP server, SSE, Prometheus `/metrics`, OTLP shipper, or Web UI code inside `AR-007` (strictly owned by [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)).
- No SQLite schema changes or CLI/MCP wire-contract regressions.
- No `pub use rusqlite::*` re-exports from `crates/acdd-index` or `crates/acdd-state` to bypass `ARCH-HOST-SQLITE-CONFINED`.
- No `acdd-state` imports inside `crates/acdd-index` (`INV-STATE-NO-INDEX`).

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Waves 0–3 covering typed `CodeMapReader` facades, AST/pipeline relocation into `crates/acdd-index`, `WorkspaceServices` unification across CLI/MCP, root `Cargo.toml` cleanup + `acdd.yaml` rules, and ADR/router reconciliation.
- **Correctness**: Every `AC-*` names concrete fail-closed architectural rules (`ARCH-HOST-SQLITE-CONFINED`, `ARCH-HOST-PARSER-FREE`) and real seam suites (`tests/core/`, `tests/mcp/`, `tests/acdd/`, `tests/query_context/`, `tests/commitment_integrity/`).
- **Coherence**: Sequences strictly after [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) and immediately before [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md), keeps concurrent Wave 1 write scopes strictly disjoint (`crates/acdd-index/src/pipeline/` + `src/engine/` vs. `src/state_bridge/` + `src/cli/` + `src/mcp/`), and prepares a clean multi-transport host for `AC-007` and `AC-009`.
