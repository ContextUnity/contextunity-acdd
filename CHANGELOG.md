# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.3] - 2026-11-15

This release completes the unified `contextunity-acdd` ecosystem transition (Milestone IN-002), brings extensive cross-platform macOS/Linux hardening (Milestone AR-001), strengthens orchestrator and multi-agent wave lifecycles, and resolves release publishing and test maintenance bottlenecks.

### Breaking changes

- **One-Pass Ecosystem Rename (`contextunity-acdd`)**:
  - Renamed the distribution to `contextunity-acdd`, the primary executable and companion skill to `acdd`, and the MCP single-shot reader to `acdd_read`.
  - Renamed workspace crates to `acdd-core`, `acdd-index`, and `acdd-tasks`.
  - Configuration and runtime stores now use `acdd.yaml` and `.acdd/` (with `.acdd/code-map.sqlite` and `.acdd/tasks.sqlite`).
  - Environment variables now use the `ACDD_` prefix (`ACDD_WORKSPACE_ROOT`, `ACDD_RELEASE_CHANNEL`, etc.).
  - Release archives are now published as `contextunity-acdd-<target>.tar.gz`.
  - Standardized managed marker comments across documentation and agent instructions to `<!-- acdd:<name>:start -->` / `<!-- acdd:<name>:end -->`.
  - All legacy aliases, shims, and fallback flags have been retired per `INV-ONE-PASS-IDENTITY`.

### Fixed

- **CI Rolling Nightly Publish**: Resolved HTTP 422 `Reference does not exist` during the `publish` stage in `.github/workflows/release.yml` by checking ref existence via GitHub API, using `POST` for missing refs and `PATCH` for existing ones, and decoupling initial release creation from rolling tag updates.
- **macOS & Cross-Platform Boundaries**:
  - Fixed path canonicalization, `/private/var/folders` symlink resolution, and case-insensitive filesystem boundaries on macOS (`crates/acdd-core/src/fs.rs`).
  - Confined SQLite database opening (`confined_database_open_path`) to prevent alias and symlink traversal escapes outside the workspace root.
  - Fixed process signaling and discovery (`SIGUSR1` / `SIGTERM`) in `acdd reload` across Darwin and Linux.
  - Hardened scanner limits and symlink cycle detection during deep directory traversal.
- **Orchestrator, Wave Concurrency & Workflow Parity**:
  - Fixed parallel execution races, role runner environment propagation, and dependency resolution in multi-agent wave dispatchers (`engine/orchestrator/waves.rs`, `spawner.rs`).
  - Hardened candidate snapshot verification and evidence compilation to prevent drift and enforce strict gate transition contracts.
  - Fixed worktree isolation tracking and candidate snapshot recovery in concurrent task workflows.
- **Test Invariant Hardening & Performance**:
  - Resolved SQLite WAL/SHM sidecar publication hazard in `src/engine/pipeline.rs::atomic_build` by checkpointing WAL (`TRUNCATE`), switching to `PRAGMA journal_mode=DELETE;`, and cleaning regular-file `-wal`/`-shm`/`-journal` sidecars under `exclusive_lock` before final publication (verified via `tests/core/database_guard.rs::output_sidecar_hazards_preserve_generation`).
  - Strengthened repository test-suite authoring invariants (shared read-only indexed fixtures, in-memory `TasksStore` vs `WaveHarness` subprocess separation, integration binary budget, and zero tautological/meta-test bloat) across `AGENTS.md`, `TESTS.md`, `tests/AGENTS.md`, `tests/acdd/AGENTS.md`, and `docs/architecture/test-harness.md`, while keeping shared skills (`skills/test-suite-refactor/SKILL.md`, `skills/test-contract-seam/SKILL.md`) repository-agnostic.
  - Expanded Milestone `AR-001` (`AR-001-borrowed-data-and-composable-execution.md`), authored Milestone `PF-002` (`PF-002-indexing-delta-and-query-acceleration.md`), and created `docs/plans/test-suite-decomposition-and-shared-fixtures.md`.
  - Cleaned up redundant and fragile test theater: eliminated tautological enum self-comparisons, struct setter echoes, test-harness meta-tests, repetitive Cargo.toml string assertions, and brittle CI YAML inspection tests.
  - Replaced cloned heavy test harnesses with table-driven test matrices across `incremental`, `mcp/freshness`, and `installer` test suites, drastically reducing test runtime and unnecessary subprocess spawns.
  - Hardened SQLite VM budget test (`compact_summary_call_counts_obey_vm_budget_and_reset_connection`) by expanding recursive fanout bounds from 10M to 50M rows, ensuring deterministic `QueryBudget` interrupt across ultra-fast Apple Silicon CPUs.
- **Documentation Clarification**: Cleaned up duplicated ACDD acronym definitions in `README.md` and `INSTALL.md` to reference the ACDD lifecycle cleanly.

## [0.5.2] - 2026-11-13

This release introduces reviewed pre-task `blueprint_synthesis` planning, recursive `inherit: default` profile overlays, conditional cross-task seam verification, and integrated post-task milestone closure (`verify` -> `polish` <-> `milestone_review` -> `handoff`). It also finalizes schema `v7` task-store alignment, removes legacy profile keywords, consolidates ACDD documentation under [`docs/reference/acdd.md`](docs/reference/acdd.md), and wires automated GitHub release notes from this file.

### Added

- **Reviewed `blueprint_synthesis` Pre-Task Gate**: Added the two-stage root `blueprint_synthesis` container (`synthesize` -> `review`) with reusable `proofs.blueprint` schema, grounded symbol/invariant citations, per-task slice fan-out into `gate_transitions` (`subject = <full_task_id>`), and automatic task-scoped injection on `task claim`.
- **Recursive Profile Inheritance (`inherit: default` vs `inherit: none`)**: Repository overlays now merge `gates:` recursively by gate `id` and `- tasks:` container onto `acdd.default.yaml` before normalization, allowing compact project-level command bindings.
- **Conditional Seam & Post-Task Closure Pipeline**: Added the language-neutral `seam` container (`when: seam`: `seam.integrate` -> `seam.review`) inside `- tasks:` and aligned post-task milestone closure on `verify` -> `polish` <-> `milestone_review` -> `handoff`.
- **Standalone Artifact Authoring Workflows**: Added taskless root-gate workflows (`author-milestone.workflow.yaml`, `author-plan.workflow.yaml`, `author-workflow.workflow.yaml`) under `docs/workflows/` (with portable copies in `skills/contextunity-forge/references/acdd-profile-examples/`).
- **ACDD Architecture & Walkthrough Documentation**: Added full documentation in [ACDD reference](docs/reference/acdd.md) and symlinked [canonical profile template](docs/reference/acdd_profile.yaml.example) to the Forge skill reference.
- **Release Notes Automation**: Added `CHANGELOG.md` and automated release-notes extraction in [`.github/workflows/release.yml`](.github/workflows/release.yml) for `v*` tag releases.

### Changed

- **Version `0.5.2`**: Bumped workspace crates (`contextunity-forge-mcp`, `forge-core`, `forge-index`, `forge-tasks`), installation docs, and workspace contract tests to `0.5.2`.
- **Lean Repository Overlay**: Simplified `.forge/acdd/profile.yaml` to an `inherit: default` overlay binding local `commands` and `scope_args_by_pattern`.
- **Modularized Task Lifecycle Engine**: Split task lifecycle handling into `crates/forge-tasks/src/engine/lifecycle/` modules and standardized CLI guide gate lists and validation probes (`src/cli/guide.rs`, `src/cli/validate.rs`).
- **Canonical Documentation Alignment**: Shortened the `README.md` ACDD summary in favor of [docs/reference/acdd.md](docs/reference/acdd.md), deduplicated [docs/reference/profiles.md](docs/reference/profiles.md), and unified canonical profile terminology (`save`, `fail`, `loop`, `need`, `absorb`) across docs and skills.

### Removed

- **Legacy Profile & Runtime Compatibility**: Removed legacy profile keywords (`contract_synthesis`, `persistence`, `reject_routing`, `reject_to`, `review_sources`, `receipt_review`, `sha_snapshot`, `absorb_working_tree`), unused `doc_writer` / `milestone.doc` defaults from `acdd.default.yaml`, pre-v7 SQLite runtime compatibility fallbacks, and duplicate `docs/future/references/acdd_profile.v2.yaml.example`.

## [0.5.1-alpha.2] - 2026-11-11

A targeted stabilization release hardening host environment isolation, non-Git workspace ignore handling, and CLI stdio transport behavior for CI and packaged installations.

### Fixed

- **CI, Host Isolation & Stdio Transport**: Fixed host environment isolation edge cases, added non-git workspace ignore handling, and resolved dash stdio transport handling in CLI tests.

## [0.5.1-alpha.1] - 2026-11-11

Introduced streamlined cross-platform distribution via `install.sh`, automated GitHub release artifacts with checksums, companion ACDD onboarding skills, and hardened SQLite checkpoint recovery.

### Added

- **Cross-Platform Installer & Release Channels**: Added `install.sh` with baked-in release channel support (`latest` / `--nightly`), CI/CD cross-compilation matrix (Linux x86_64/arm64, macOS x86_64/arm64), and `SHA256SUMS` verification.
- **ACDD Onboarding Skill**: Bundled ACDD onboarding guidance and domain profile examples in `skills/contextunity-forge/`.

### Fixed

- **Checkpoint Recovery & Lifecycle Segregation**: Improved `milestone db-recover` to restore active checkpoints while respecting task scopes, enforced fail-closed artifact ownership, and unified state clearing.

## [0.5.0-alpha.1] - 2026-11-10

Delivered the headless orchestrator and isolated role-runner architecture (`workflow run`, `plan run`, wave execution), explicit worker-ID separation without host session scraping, and matching documentation updates.

### Added

- **Headless Orchestrator & Role Runners**: Implemented headless task/wave execution (`workflow run`, `plan run`), milestone wave runner, isolated role process spawner, and worker separation independent of host session scraping.
- **Direct Symbol Read Alias & Guide Enhancements**: Added `symbol` selector alias on `forge_read` and exposed lifecycle gates and subtasks in `forge_guide`.

### Changed

- **Documentation & Delivery Alignment**: Aligned `README.md`, `INSTALL.md`, and reference docs with headless execution and autonomous delivery behavior.

## [0.4.1] - 2026-11-09

Added structural duplicate detection, explicit verification targets (`--test` / `--seam`), deeper Rust ecosystem/trait resolution, and modular decomposition across the task engine and test suite.

### Added

- **Structural Duplicate Detection**: Added `duplicates` operation to `code_map_query` and tightened clone/duplication detection across the codebase.
- **Explicit Scoped Verification Targets**: Added explicit `--test` and `--seam` target flags to `task verify`, removing brittle heuristic target guessing.
- **Rust Ecosystem Manifest & Trait Resolution**: Added Rust std/prelude/Cargo ecosystem packs, closure/tuple inference, chained receiver coverage, and faster cold SQLite/FTS persistence.

### Changed

- **Modular Task Engine & Domain Test Suite**: Decomposed the task engine, gate graph, and profile parsers, and refactored test suites into focused domain modules; repaired framework indexing regressions and optimized indexing/linking pipelines.

## [0.4.0] - 2026-10-31

Split the codebase into a four-crate Cargo workspace (`forge-core`, `forge-index`, `forge-tasks`, `contextunity-forge-mcp`) alongside hierarchical ACDD gate AST compilation, declarative multi-language framework packs, SQL/ORM schema indexing, and 96 KiB bounded MCP payloads.

### Added

- **Hierarchical ACDD Gates & Profiles**: Implemented the hierarchical ACDD gate AST, modular catalogs, command-proof execution traces, atomic SQLite checkpoints, and scoped Git candidate snapshots.
- **Declarative Framework Packs**: Added language/framework semantic packs for Django/ORM, FastAPI, Starlette, Nuxt/Vue/Pinia, HTMX, Alpine.js, GridViewSpec, and Rust.
- **SQL Schema Indexing & Topology Queries**: Added SQL/ORM schema extraction and table linking, entrypoint classification, directory boundary slicing, and hierarchy/test-gap traversals.
- **Bounded 96 KiB MCP Payloads & Handoff Pruning**: Enforced bounded MCP response budgets with graceful pagination and SQLite milestone handoff pruning with `milestone_tombstones`.

### Changed

- **Workspace Crate Split**: Split the monolithic crate into a four-crate Cargo workspace (`forge-core`, `forge-index`, `forge-tasks`, and `contextunity-forge-mcp`).

## [0.3.1] - 2026-10-19

A focused bug-fix release resolving workspace manifest discovery during MCP admission.

### Fixed

- **MCP Workspace Manifest Admission**: Fixed manifest discovery during MCP server startup and workspace admission.

## [0.3.0] - 2026-10-11

Introduced declarative ACDD governance profiles, the `task_blackboard` SQLite store, zero-downtime `SIGHUP` MCP server reload, serde-backed framework manifests, and single MIT licensing.

### Added

- **Declarative ACDD Profiles & Task Blackboard**: Introduced declarative profile loading, `task_blackboard` SQLite store and MCP/CLI tooling, subtask lifecycles, direct in-memory JSON proof submission, and milestone delivery rollups.
- **SIGHUP Zero-Downtime Hot-Reload & Compaction**: Added SIGHUP reload support for running MCP servers and compacted response payloads across search, removal proofs, tasks, and checkpoints.
- **Serde Framework Manifests**: Added declarative serde-backed framework manifest loading.

### Changed

- **MIT License & Documentation Scaffold**: Adopted single MIT license and reorganized reference, architecture, and skill documentation.

## [0.2.0] - 2026-09-26

Initial public release of the pure-Rust `contextunity-forge-mcp` MCP server and CLI, unifying Tree-sitter AST code graphs, living documentation indexing, deterministic Merkle commitments, and local task/milestone lifecycles in a single binary.

### Added

- **Initial Native Rust Forge Engine**: Released the standalone `contextunity-forge-mcp` MCP server and CLI featuring Tree-sitter AST indexing (`.forge/code-map.sqlite`), single-shot digest-verified symbol reads (`forge_read`), `ast_grep_search`, living documentation indexing, deterministic Merkle commitments, and SQLite task/milestone lifecycle management.

[Unreleased]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.5.3...HEAD
[0.5.3]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/v0.5.1-alpha.2...v0.5.2
[0.5.1-alpha.2]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/v0.5.1-alpha.1...v0.5.1-alpha.2
[0.5.1-alpha.1]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/v0.5.0-alpha.1...v0.5.1-alpha.1
[0.5.0-alpha.1]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/a813f830c...v0.5.0-alpha.1
[0.4.1]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/be3733967...a813f830c
[0.4.0]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/1dc907d8c...be3733967
[0.3.1]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/7762ff86c...1dc907d8c
[0.3.0]: https://github.com/ContextUnity/contextunity-forge-mcp/compare/f502c69dd...7762ff86c
[0.2.0]: https://github.com/ContextUnity/contextunity-forge-mcp/commit/f502c69dd
