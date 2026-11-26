---
id: LS-004-rust-entrypoints-and-inline-module-exports
title: "Language-agnostic local-mutation resolution fix, per-language manifest folders, declarative Rust entrypoints (rmcp/axum/actix, cross-file clap), and inline module exports"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-index/build_manifests.rs
  - crates/acdd-index/src/ast/visitor.rs
  - crates/acdd-index/src/models/details.rs
  - crates/acdd-index/src/languages/rust/
  - crates/acdd-index/src/languages/manifests/
  - crates/acdd-index/src/linker/
  - crates/acdd-index/src/db/lint/unused_public_api.rs
  - crates/acdd-index/src/db/reader/
  - src/format/
  - tests/languages/
  - tests/linker/
  - tests/query_context/
  - tests/mcp/context/entrypoints_and_delta.rs
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/LS-004-rust-entrypoints-and-inline-module-exports.md
invariants:
  - 'INV-LOCAL-MUTATES-NOT-UNRESOLVED-CALL: Local variable and parameter EdgeKind::Mutates references in crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs are handled distinctly from callable Calls/Handles across ALL languages so mutating a local binding never emits false unresolved shadowed-callee diagnostics (preserving true shadowed callable diagnostics and real cross-symbol Mutates edges).'
  - 'INV-MANIFESTS-BY-LANGUAGE: Built-in framework manifests under crates/acdd-index/src/languages/manifests/ are organized into per-language subdirectories (rust/, python/, typescript/, web/) discovered deterministically by build_manifests.rs with zero hardcoded framework package names inside language parsers.'
  - 'INV-PRODUCER-OWNED-ENTRYPOINTS: Declarative Rust framework packs (rmcp.toml, axum.toml, actix.toml) and cross-file clap command_dispatch project through canonical extractor/linker facts into persisted nodes.details.entrypoint (mcp_tool, http_route, cli_command) via db/writer/entrypoints.rs with zero downstream string heuristics.'
  - 'INV-SHADOW-SAFE-FRAMEWORK-PROJECTION: Package-gated activation and shadow checks prevent non-rmcp/non-axum/non-actix/non-clap shadows from emitting entrypoint projections, and incremental delta re-indexing clears stale categories when attributes or imports are removed.'
  - 'INV-INLINE-MOD-EXPORTS-SAFE: Scoped inline mod_item bodies publish visibility-aware EXPORTS facts cleanly without regressing existing db/lint/unused_public_api.rs liveness lint behavior.'
acceptance_criteria:
  - 'AC-01: GIVEN multi-language sources (Rust, Python, TypeScript) mutating local variables or parameters alongside true shadowed callable calls and real static/field mutations, WHEN linked by crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs, THEN local-binding EdgeKind::Mutates no longer emit false unresolved shadowed-callee coverage rows, true shadowed calls still report unresolved, real cross-symbol Mutates edges remain intact, and code_map_overview reports production vs test resolution health cleanly.'
  - 'AC-02: GIVEN built-in manifests reorganized into crates/acdd-index/src/languages/manifests/{rust,python,typescript,web}/, WHEN build_manifests.rs compiles and tests/languages/ runs, THEN all 27+ packs load deterministically in sorted order with zero activation regressions.'
  - 'AC-03: GIVEN declarative Rust manifests (manifests/rust/{rmcp,axum,actix}.toml) backed by generic attribute/route syntax operations, WHEN Rust files importing rmcp/axum/actix are indexed (cold and delta), THEN db/writer/entrypoints.rs persists nodes.details.entrypoint = "mcp_tool" | "http_route" while ignoring shadowed attributes or inactive packages.'
  - 'AC-04: GIVEN a multi-file Rust clap CLI (struct Cli #[derive(Parser)] in args.rs, Cli::parse() dispatch in mod.rs), WHEN indexed (cold and delta), THEN cross-file linker resolution emits exact cli_command_registration -> handles edges projecting nodes.details.entrypoint = "cli_command", and removing #[derive(Parser)] or shadowing clap clears the projection on delta.'
  - 'AC-05: GIVEN Rust files containing public and private inline mod blocks with named and wildcard pub use re-exports, WHEN indexed, THEN scoped ModuleDeclaration nodes publish accurate EXPORTS facts reachable from crate roots while existing unused_public_api lint suites pass with zero regressions.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
---

# LS-004: Language-agnostic local-mutation resolution fix, per-language manifest folders, declarative Rust entrypoints (`rmcp`/`axum`/`actix`, cross-file `clap`), and inline module exports

## Goal & Non-goals

**Goal.** Fix a high-impact language-agnostic resolution accounting defect (~16K false `unresolved` `shadowed_binding` rows caused by local-variable `EdgeKind::Mutates` in `dispatch.rs`, lifting workspace Resolution Health from ~66% to ~79%+ and prod to ~85%+), organize built-in framework manifests cleanly by language, and close three concrete Rust semantic extraction gaps in `crates/acdd-index`:
1. **Language-agnostic local-binding `EdgeKind::Mutates` resolution (`dispatch.rs`) + `prod`/`tests` health breakdown (`code_map_overview`):** Stop counting local variable/parameter mutations as unresolved shadowed callees across all languages while preserving real cross-symbol `Mutates` edges and true shadowed-call diagnostics.
2. **Per-language manifest folders:** Reorganize the 27 flat `crates/acdd-index/src/languages/manifests/*.toml` files into `rust/`, `python/`, `typescript/`, `web/` with deterministic recursive discovery in `build_manifests.rs`.
3. **Declarative Rust entrypoints:** Add generic Rust attribute/route syntax operations with standard packs (`manifests/rust/{rmcp,axum,actix}.toml`) and extend declarative `command_dispatch` (`manifests/rust/clap.toml`) across split files (`args.rs` + `mod.rs`) on cold and delta builds.
4. **Inline module exports:** Publish visibility-aware `EXPORTS` facts on inline `mod foo { ... }` declarations (`pub use` named and wildcard re-exports) without regressing existing `unused_public_api` lint behavior.

**Non-goals.**
- Speculative `dyn Trait` implementation fan-out (honest `trait_object_dispatch` analysis gaps remain preserved).
- Adding new MCP tools or modifying `code_map_analyze` presentation (already handled by [UX-004](UX-004-mcp-tool-surface-consolidation.md)).
- Adding framework packs for Tier-2 languages (Go/Java/C#/Kotlin/PHP/Ruby).

## Tasks in this milestone

### task: universal-local-mutates-and-overview-health

```yaml
task_ref: universal-local-mutates-and-overview-health
target: "Fix language-agnostic local-binding EdgeKind::Mutates handling in linker/resolution/value_flow_phase/dispatch.rs so local mutations do not emit false unresolved shadowed-callee rows across any language, and expose prod vs test resolution health breakdown in code_map_overview"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/linker/resolution/
  - crates/acdd-index/src/db/reader/
  - src/format/
  - tests/linker/
  - tests/query_context/
status: planned
subtasks:
  - subtask_ref: separate-local-mutates-from-shadowed-calls
    title: "Handle local variable/parameter EdgeKind::Mutates cleanly in dispatch.rs across all languages while preserving real static/field Mutates edges and true shadowed Calls/Handles unresolved diagnostics"
  - subtask_ref: prod-vs-tests-resolution-health-overview
    title: "Add prod vs tests resolution health breakdown in code_map_overview and verify multi-language linker/overview parity in tests/linker/ and tests/query_context/"
```

- **Wave**: `0` (concurrent with `organize-manifests-by-language-and-rust-packs`, strictly disjoint scopes) | **Budget**: `~260` net LOC.
- **Seam & mutation proof (`AC-01`)**: `tests/linker/` and `tests/query_context/` prove local `Mutates` across Rust/Python/TypeScript no longer inflate `unresolved` `shadowed_binding`, true shadowed callable calls still report `unresolved`, real cross-symbol `Mutates` edges remain, and `code_map_overview` distinguishes prod vs test health.

### task: organize-manifests-by-language-and-rust-packs

```yaml
task_ref: organize-manifests-by-language-and-rust-packs
target: "Reorganize built-in framework manifests into per-language subdirectories (rust/, python/, typescript/, web/) with deterministic build_manifests.rs discovery, add generic Rust attribute/route syntax operations, and ship manifests/rust/{rmcp,axum,actix}.toml"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/build_manifests.rs
  - crates/acdd-index/src/languages/manifests/
  - crates/acdd-index/src/languages/rust/
  - tests/languages/
  - tests/mcp/context/entrypoints_and_delta.rs
status: planned
subtasks:
  - subtask_ref: manifests-language-subfolders
    title: "Move flat manifests/*.toml into manifests/{rust,python,typescript,web}/ and update build_manifests.rs for deterministic recursive discovery with full manifest-activation test parity"
  - subtask_ref: declarative-rust-rmcp-axum-actix-packs
    title: "Wire generic declarative Rust attribute/route registration operations and add package-gated, shadow-safe manifests/rust/{rmcp,axum,actix}.toml packs verified on cold and delta builds"
```

- **Wave**: `0` (concurrent with `universal-local-mutates-and-overview-health`, strictly disjoint scopes) | **Budget**: `~350` net LOC.
- **Seam & mutation proof (`AC-02`, `AC-03`)**: `tests/languages/` and `tests/mcp/context/entrypoints_and_delta.rs` prove deterministic loading across language subfolders, `rmcp`/`axum`/`actix` `mcp_tool` & `http_route` persistence, package activation gating, and negative shadow rejection.

### task: crossfile-clap-and-inline-mod-exports

```yaml
task_ref: crossfile-clap-and-inline-mod-exports
target: "Extend declarative clap command_dispatch across split Rust files via linker framework relations and publish visibility-aware EXPORTS facts on scoped inline Rust ModuleDeclaration nodes while preserving unused_public_api lint"
contract_revision: 1
depends_on:
  - universal-local-mutates-and-overview-health
  - organize-manifests-by-language-and-rust-packs
tags:
  - default
scope:
  - crates/acdd-index/src/ast/visitor.rs
  - crates/acdd-index/src/models/details.rs
  - crates/acdd-index/src/languages/rust/
  - crates/acdd-index/src/linker/
  - crates/acdd-index/src/db/lint/unused_public_api.rs
  - tests/languages/
  - tests/linker/
  - tests/mcp/context/entrypoints_and_delta.rs
  - ROADMAP.md
  - CHANGELOG.md
status: planned
subtasks:
  - subtask_ref: crossfile-clap-linker-and-delta-proofs
    title: "Persist clap Parser metadata + cross-file cli_command_registration candidates, resolve exact handles edges in linker framework relations, and verify cold + incremental delta addition/removal/shadow cases"
  - subtask_ref: scoped-inline-mod-exports-and-lint-preservation
    title: "Publish inline mod_item pub use exports on scoped ModuleDeclaration nodes in ast/visitor.rs and languages/rust/, proving named/wildcard re-exports and unchanged unused_public_api lint results"
```

- **Wave**: `1` | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-04`, `AC-05`)**: `tests/languages/`, `tests/linker/`, and `tests/mcp/context/entrypoints_and_delta.rs` prove split-file `clap` cold/delta entrypoints, inline `mod` named/wildcard `pub use` publication, and unchanged `unused_public_api` lint results.

## Negative boundaries

- No new third-party dependencies in `Cargo.toml` and no SQLite schema version changes.
- No hardcoded framework package names inside Rust parser code (`INV-MANIFESTS-BY-LANGUAGE`).
- No suppression of true shadowed callable `Calls`/`Handles` unresolved diagnostics or deletion of real cross-symbol `Mutates` edges.
- No downstream string heuristics inside `db/analyze/` or `src/`.
- No regressions in existing `unused_public_api` lint findings or `entrypoints_and_delta` negative shadow tests.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Wave 0 (parallel disjoint: language-agnostic local `Mutates` + `overview` health vs. manifest subfolders + declarative Rust packs) and Wave 1 (cross-file `clap` + inline `mod` exports).
- **Correctness**: Preserves real `Mutates` edges and shadowed callable diagnostics, enforces deterministic recursive manifest discovery, package-gated shadow safety, cold + delta consistency, and non-regression of `unused_public_api`.
- **Coherence**: Fully independent of [UX-004](UX-004-mcp-tool-surface-consolidation.md) (`depends_on: []`), lifting multi-language resolution accuracy and automatically enriching `code_map_analyze(mode="entrypoints")`, `code_map_overview`, and `AC-007` `GET /api/map`.
