---
title: "ADR 0001: Crate DAG and Layer Boundaries"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0001: Crate DAG and Layer Boundaries

## Status

Accepted.

## Context

Coupling workflow state mechanics directly to AST parsing, SQLite code-map tables, CLI argument parsers, or MCP transport handlers creates cyclic build dependencies, bloats leaf crates with heavy C/async toolchains, and risks cross-layer leakage between pure state transitions and filesystem/SQLite side effects. The workspace requires an explicit, compiler-enforced unidirectional crate DAG and modular host boundaries.

## Decision

### 1. Three-Library-Crate Workspace DAG

The workspace is structured as **three library crates** plus **one root host binary/library crate** (`contextunity-acdd` in `src/`):

```text
  src/cli (clap) ─────────┐
  src/mcp (19 static      ├──► src/state_bridge/ (CodeDomainProvider, DocsDomainProvider)
       + plugin tools)    │        │                          │
  src/format/ (Markdown)  │        ▼                          ▼
                          └──► crates/acdd-state          crates/acdd-index
                                ├── machine/ (100% Pure)   (rusqlite code-map,
                                ├── usecases/               tree-sitter, linker, docs)
                                ├── git_ops/, runner/         │
                                └── store/sqlite/             │
                                       │                      │
                                       └──────────┬───────────┘
                                                  ▼
                                        crates/acdd-core
                         (config, ids, errors, path_policy, schema, ports,
                          fs, git, lock, response, subsystems, telemetry)
```

- **`crates/acdd-core`**: Strict leaf crate containing decoded identifier newtypes (`ids`), canonical `ErrorCode` / `StateError` (`errors`), normalized `WorkspacePathPolicy` predicates (`path_policy`), self-contained `SchemaDef` (`schema`), `DomainProvider` SPI (`ports`), workspace `Adapter` configuration (`config`), subsystem toggles (`subsystems`), response budgets (`response`), confined filesystem/lock/Git wrappers (`fs`, `lock`, `git`), Linux cgroup memory calculation (`sys`), and telemetry wire DTOs (`telemetry`).
- **`crates/acdd-index`**: Depends on `acdd-core`. Owns Tree-sitter extraction (`ast`), language profiles and declarative framework manifests (`languages`), cross-file linking (`linker`), documentation section/invariant extraction (`docs`), Merkle commitment sealing (`commitments`), compact graph/fact encoding (`models`, `typed_facts`, `interner`), and `code-map.sqlite` storage (`db`).
- **`crates/acdd-state`**: Depends on `acdd-core`. Owns `GateProfile` (`version: 2`) and `SpecDocument` parsing (`profile`, `spec`), the pure state-transition core (`machine/`), application use cases (`usecases/`), budgeted `ClaimResp` assembly (`context`), Git snapshot/commit/integration operations (`git_ops/`), wave/runner execution (`runner/`), `StateBackend` persistence (`store/`), and `RunHarness` (`harness`).
- **`src/` (`contextunity-acdd`)**: Host crate wiring `acdd-core`, `acdd-index`, and `acdd-state`. Owns `src/state_bridge/`, `src/engine/` (indexing pipeline and layered reader), `src/format/` (pure Markdown/JSON presentation), `src/cli/`, and `src/mcp/`.

### Core Leaf Isolation

> [!IMPORTANT]
> Invariant: `crates/acdd-core` is a strict leaf crate. It never depends on `acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `zstd`, `rayon`, `hashbrown`, `clap`, `rmcp`, or `tokio`. `crates/acdd-state` never depends on `acdd-index`, `tree-sitter`, `clap`, `rmcp`, or `tokio`.

- Inside `crates/acdd-state`, `machine/` is a 100% pure functional core: zero imports or references to `std::fs`, `std::process`, `std::net`, or `rusqlite`.
- Direct `rusqlite` imports inside `crates/acdd-state` are confined exclusively to `crates/acdd-state/src/store/sqlite/`.
- `src/state_bridge/` is imported by `src/cli/` and `src/mcp/`, and never by `src/engine/` or any workspace library crate (`crates/*`). Proxy re-export modules (`src/core/*`, `src/db/*`) are banned.
- These dependency fences are verified automatically by [`state_boundaries.rs`](../../tests/commitment_integrity/state_boundaries.rs).

### 2. Subsystem Disablement (`modules:`)

[`SubsystemRegistry`](../../crates/acdd-core/src/subsystems.rs) governs runtime availability for `code`, `docs`, `state`, `blackboard`, `memory`, and `domains.<id>` under presets (`full`, `navigator`, `orchestrator`, `minimal`) or boolean overrides in `acdd.yaml`:

- Disabling `state` (`modules.state: false`) physically disables `blackboard`, prevents `.acdd/state.sqlite` creation or directory initialization, and removes state tools from MCP `list_tools`.
- Disabling `code` or `docs` removes the corresponding MCP tools and fails closed on disabled tool/read-mode requests with `FEATURE_DISABLED`.
- Core tools `acdd_read` and `acdd_guide` remain available even under `profile: minimal`.

### 3. Balanced Modularity

Split modules strictly when responsibilities diverge (for example protocol ingress vs. business logic vs. persistence), never by arbitrary line-count caps. Cohesive multi-pass algorithms keep their linear structure in place; proven duplicate logic is extracted into a single canonical owner in `acdd-core` (`fs`, `git`, `lock`, `config`, `sys`).

## Consequences

- External `DomainProvider` shim-crates compile against `acdd-core` alone without pulling `rusqlite`, `tree-sitter`, `rmcp`, or `clap`.
- Pure state-machine transitions run deterministically in-memory without filesystem, Git, or SQLite side effects.
- `tests/commitment_integrity/state_boundaries.rs` fails the build immediately if any forbidden cross-crate or pure-core dependency is introduced.
