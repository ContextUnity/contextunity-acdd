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

### 1. Four-Library-Crate Workspace DAG

The workspace is structured as **four library crates** plus **one root host binary/library crate** (`contextunity-acdd` in `src/`):

```text
  src/cli (clap) ─────────┐
  src/mcp (18 static      ├──► src/state_bridge/ (CodeDomainProvider, DocsDomainProvider)
       + plugin tools)    │        │                          │
  src/format/ (Markdown)  │        ▼                          ▼
                          └──► crates/acdd-state          crates/acdd-index
                                ├── machine/ (100% Pure)   (rusqlite code-map,
                                ├── usecases/               tree-sitter, linker, docs)
                                ├── git_ops/, runner/         │
                                └── store/sqlite/             │
                                       │                      │
                                       └────┬────────┬────────┘
                                            ▼        ▼
                                      crates/acdd-spec (frontmatter, spec parsing,
                                            │           draft, validate, roadmap)
                                            ▼
                                      crates/acdd-core
                         (config, ids, errors, path_policy, schema, ports,
                          fs, git, lock, response, subsystems, telemetry)
```

- **`crates/acdd-core`**: Strict leaf crate containing decoded identifier newtypes (`ids`), canonical `ErrorCode` / `AcddError` (`errors`), normalized `WorkspacePathPolicy` predicates (`path_policy`), self-contained `SchemaDef` (`schema`), `DomainProvider` SPI (`ports`), workspace `Adapter` configuration (`config`), subsystem toggles (`subsystems`), response budgets (`response`), confined filesystem/lock/Git wrappers (`fs`, `lock`, `git`), Linux cgroup memory calculation (`sys`), and telemetry wire DTOs (`telemetry`).
- **`crates/acdd-spec`**: Depends strictly on `acdd-core` plus serialization/schema crates (`serde`, `serde_json`, `serde_yaml`, `schemars`). Owns canonical YAML frontmatter extraction (`frontmatter`), specification document parsing/rendering (`spec`), specification drafting (`draft`), confined workspace/archive discovery (`discovery`), static dependency resolution/validation (`deps`, `validate`), static roadmap DAG construction (`roadmap/`), and the [`SpecRuntimeOverlay`](../../crates/acdd-spec/src/overlay.rs) seam (`overlay`) decoupling pure spec behavior from SQLite runtime state.
- **`crates/acdd-index`**: Depends on `acdd-core` and `acdd-spec::frontmatter` (`crates/acdd-spec/src/frontmatter.rs`). Owns Tree-sitter extraction (`ast`), language profiles and declarative framework manifests (`languages`), cross-file linking (`linker`), documentation section/invariant extraction (`docs`), Merkle commitment sealing (`commitments`), compact graph/fact encoding (`models`, `typed_facts`, `interner`), and `code-map.sqlite` storage (`db`).
- **`crates/acdd-state`**: Depends on `acdd-core` and `acdd-spec`. Owns `WorkflowDef` (`version: 2`), the pure state-transition core (`machine/`), application use cases (`usecases/`, supplying SQLite `StateRuntimeOverlay`), budgeted `ClaimResp` assembly (`context`), Git snapshot/commit/integration operations (`git_ops/`), wave/runner execution (`runner/`), `StateBackend` persistence (`store/`), and `RunHarness` (`harness`).
- **`src/` (`contextunity-acdd`)**: Host crate wiring `acdd-core`, `acdd-spec`, `acdd-index`, and `acdd-state`. Owns `src/state_bridge/`, `src/engine/` (indexing pipeline and layered reader), `src/format/` (pure Markdown/JSON presentation), `src/cli/`, and `src/mcp/`.

### Core Leaf Isolation

> [!IMPORTANT]
> Invariant: `crates/acdd-core` is a strict leaf crate, `crates/acdd-spec` depends only on `crates/acdd-core` and pure serialization/schema crates, and `crates/acdd-state` remains isolated from `crates/acdd-index` and host `src/`.
> - INV-CORE-LEAF: `crates/acdd-core` never depends on `acdd-spec`, `acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `zstd`, `rayon`, `hashbrown`, `clap`, `rmcp`, or `tokio`. [`filter_subsystem_tools`](../../crates/acdd-core/src/subsystems.rs)
> - INV-SPEC-CRATE-BOUNDARY: `crates/acdd-spec` depends strictly on `acdd-core` and `serde`/`serde_json`/`serde_yaml`/`schemars` without SQLite, index, state, regex, walkdir, or async runtimes; `crates/acdd-index` enters `crates/acdd-spec` exclusively through `crates/acdd-spec/src/frontmatter.rs`. [`SpecRuntimeOverlay`](../../crates/acdd-spec/src/overlay.rs)
> - INV-STATE-NO-INDEX: `crates/acdd-state` depends only on `acdd-core` and `acdd-spec`, never on `acdd-index`, `src/`, `tree-sitter`, `clap`, `rmcp`, or `tokio`. [`execute_on_dyn`](../../crates/acdd-state/src/usecases/mod.rs)
> - INV-STATE-BRIDGE-BOUNDARY: `src/state_bridge/` is imported by `src/cli/` and `src/mcp/`, and never by `src/engine/` or any workspace library crate (`crates/*`); proxy re-export modules (`src/core/*`, `src/db/*`) are banned. [`build_domain_providers`](../../src/state_bridge/mod.rs)

```acdd-rule
id: INV-CORE-LEAF
invariant: INV-CORE-LEAF
severity: error
kind: forbid_dependency
from:
  zones: [core]
to:
  zones: [spec, index, state, host_format, host_engine, host_state_bridge, host_telemetry, host_cli, host_mcp]
  external_symbols: ["acdd_spec", "acdd_index", "acdd_state", "rusqlite", "tree_sitter", "tokio", "rmcp", "clap", "rayon", "zstd", "hashbrown", "petgraph", "chrono"]
include_tests: true
```

```acdd-rule
id: INV-SPEC-CRATE-BOUNDARY
invariant: INV-SPEC-CRATE-BOUNDARY
severity: error
kind: allow_only_dependencies
from:
  zones: [spec]
to:
  zones: [core]
include_tests: true
```

```acdd-rule
id: INV-SPEC-CRATE-EXTERNALS
invariant: INV-SPEC-CRATE-BOUNDARY
severity: error
kind: forbid_dependency
from:
  zones: [spec]
to:
  external_symbols: ["acdd_index", "acdd_state", "rusqlite", "tree_sitter", "zstd", "rayon", "hashbrown", "petgraph", "pulldown_cmark", "walkdir", "ignore", "clap", "rmcp", "tokio", "libc", "regex"]
include_tests: true
```

```acdd-rule
id: INV-STATE-NO-INDEX
invariant: INV-STATE-NO-INDEX
severity: error
kind: allow_only_dependencies
from:
  zones: [state]
to:
  zones: [core, spec]
```

```acdd-rule
id: INV-STATE-NO-INDEX-EXTERNALS
invariant: INV-STATE-NO-INDEX
severity: error
kind: forbid_dependency
from:
  zones: [state]
to:
  external_symbols: ["acdd_index", "tree_sitter", "tokio", "rmcp", "clap", "rayon", "zstd", "hashbrown", "petgraph"]
```

```acdd-rule
id: INV-STATE-BRIDGE-BOUNDARY
invariant: INV-STATE-BRIDGE-BOUNDARY
severity: error
kind: forbid_dependency
from:
  zones: [core, spec, index, state, host_engine, host_format]
to:
  zones: [host_state_bridge]
```

- Inside `crates/acdd-state`, `machine/` is a 100% pure functional core (`INV-STATE-PURE-MACHINE`), and direct `rusqlite` imports are confined to `crates/acdd-state/src/store/sqlite/` (`INV-STATE-SQLITE-CONFINED`); see [ADR 0003](0003-unified-profile-graph-and-state-machine.md).
- These dependency fences are also verified by [`state_boundaries.rs`](../../tests/commitment_integrity/state_boundaries.rs).

### 2. Subsystem Disablement (`preset:` & `modules:`)

[`SubsystemRegistry`](../../crates/acdd-core/src/subsystems.rs) governs runtime availability for `code`, `docs`, `spec`, `state`, `blackboard`, `memory`, and `domains.<id>` under top-level `preset:` (`full`, `index`, `spec`, `state`, `minimal`) or boolean overrides in `acdd.yaml`:

- Disabling `state` (`modules.state: false`, or `preset: index` / `preset: spec`) physically disables `blackboard`, prevents `.acdd/state.sqlite` creation or directory initialization, and removes state tools from MCP `list_tools`.
- `preset: spec` enables `spec` and `docs` (CLI `acdd validate`/`roadmap`/`draft` + MCP `get_doc`, `search_docs`, `acdd_read`, `acdd_guide`) with zero `.acdd/state.sqlite` creation and zero new MCP tools.
- Disabling `code` or `docs` removes the corresponding MCP tools and fails closed on disabled tool/read-mode requests with `FEATURE_DISABLED`.
- Core tools `acdd_read` and `acdd_guide` remain available even under `preset: minimal`.

### 3. Balanced Modularity

Split modules strictly when responsibilities diverge (for example protocol ingress vs. business logic vs. persistence), never by arbitrary line-count caps. Cohesive multi-pass algorithms keep their linear structure in place; proven duplicate logic is extracted into a single canonical owner in `acdd-core` (`fs`, `git`, `lock`, `config`, `sys`).

### 4. Typed Graph Tags and Canonical Errors

- `acdd-core::errors::AcddError` is the sole canonical structured diagnostic type across all workspace crates (`StateError` is removed); `ErrorCode`, its wire names, and CLI exit-code mapping remain stable, with `ACDD_WORKFLOW_SCHEMA_INVALID` separated from `ACDD_SPEC_SCHEMA_INVALID`.
- `acdd-index::models::tags` owns the closed `Copy` enums for node kinds, language IDs, edge kinds, edge confidence, coverage status, and reference kinds. Each enum uses a one-byte representation and exposes its stable wire names through the existing parse and formatting helpers.
- Public edge tags remain strings at the compatibility boundary. Conversion into compact typed graph storage is fallible and rejects unknown tags instead of inventing enum values.

## Consequences

- External `DomainProvider` shim-crates compile against `acdd-core` alone without pulling `rusqlite`, `tree-sitter`, `rmcp`, or `clap`.
- Pure state-machine transitions run deterministically in-memory without filesystem, Git, or SQLite side effects.
- `tests/commitment_integrity/state_boundaries.rs` fails the build immediately if any forbidden cross-crate or pure-core dependency is introduced.
