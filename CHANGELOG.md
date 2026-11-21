# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0] - 2026-11-21

### Added

- **`acdd roadmap` CLI and MCP parity**: Read-only `acdd roadmap [spec_id] [--collection NAME] [--ready] [--active] [--tasks]` delegates to `StateQuery::Roadmap` via `execute_state_query`, renders compact Markdown through `src/format/roadmap.rs`, and exposes `acdd_state(action: "roadmap")` with matching JSON output.
- **Task Git checkpoints (`save.post: snapshot`)**:
  - `acdd snapshot save|restore|clear` and `acdd_state(action: "snapshot")` manage confined per-task refs under `refs/acdd/snapshots/<run_id>/<task_ref>/r<rev>` without moving `HEAD`.
  - Embedded `code` `build` gates auto-save checkpoints on pass; deliver, run `clear`, and `handoff` delete matching refs.
- **Coordinator Gate Dispatch Plans & Swarm Role Models**:
  - Added coordinator gate dispatch planning and swarm role model configuration across embedded `code` / `audit` profiles and `acdd-state` execution context.

### Changed

- **Cold & Delta Indexing Pipeline Acceleration and Memory Reduction (`PF-003`, `PF-004`)**:
  - Replaced duplicate cold walks and multi-pass encoders with single-walk streaming extraction, `PayloadStore` RAM/spool staging, `LocalFactsBatchWriter` direct SQLite streaming, and delta-only `PreparedCatalogStorage` with contiguous `PackedStrings` search texts.
  - Unified cold/delta SQLite ingest onto `MultiValueBatch` and shared `PreparedGraph` projections with Rayon-parallel domain hashing, typed `CompactGraph` (`CompactTag`, `NodeDetails`, `Arc<str>` interning pools), and thread-local Zstd graph scratch reuse/release.
  - Consolidated linker entry onto `link_compact_catalog_with_registry`, parallelized linker prep (`children_by_parent`, `PythonReceivers::build`), pruned persistent `FlowStore` facts early, centralized glibc page trimming in `acdd_core::sys::trim_allocator()`, and restored phase-boundary trims inside the linker.
  - Reorganized `src/engine/pipeline/` and `crates/acdd-index/src/db/writer/` into focused submodules with `TempTableGuard` lifetime management and early-drop cold FTS/verify fast-paths.
- **Roadmap & Dependency Resolution Hot-Path Consolidation (`PF-005`)**:
  - Batched multi-blob Git reads through single-subprocess `git cat-file --batch` inspection in `crates/acdd-core/src/git.rs` and removed redundant `git rev-parse` checks in `list_git_tree_entries` and `list_owned_worktrees`.
  - Consolidated `handle_roadmap` and `validate_spec_dependencies` to preload SQLite runs, tombstones, and archive summaries once per invocation instead of rescanning archives per edge across sibling worktrees.

### Fixed

- **FTS5 Fast-Path Signed Rowid Decoding & Delta Seal Hygiene (`PF-004`, `PF-003`)**:
  - Fixed signed `i64` rowid decoding and eliminated per-document buffer allocations in `crates/acdd-index/src/db/commitments/fts5_fastpath.rs`, and skipped no-op delta FTS5 mutations when file projections stay clean alongside `delta-seal` GC pruning.
- **State Checkout Isolation, Handoff Cleanup & `LoadedRunGraph` Unification**:
  - Isolated checkout path policy boundaries, hardened handoff and snapshot cleanup, and unified `LoadedRunGraph` handling in `crates/acdd-state`.
- **Roadmap Cross-Worktree and Archive Noise**:
  - Suppressed schema and worktree divergence warnings for archived specs in `acdd roadmap`.
  - Secondary worktrees now overlay onto `main` only for active runs (`status: active` or active run in SQLite), preventing unmerged/draft branch plans from polluting the root roadmap.

## [0.6.2] - 2026-11-18

This release streamlines the execution lifecycle onto `blueprint_synthesis -> tasks: build -> review -> deliver -> verify -> integration_review -> handoff`, embeds mandatory graph-hygiene checks (`dead_tails`, `duplicates`) and ACDD MCP tool rules into the `code` profile, unifies MCP DTO schemas via `schemars`, compresses Markdown formatting, isolates `RunHarness` behind `test-support`, and modernizes the `benchmarks/` suite.

### Added

- **Graph Hygiene (`dead_tails`), Taskless `--goal` & Audit/Report Pipeline**:
  - Added `code_map_query(operation="dead_tails")` and `acdd query run dead_tails` computing workspace-wide production liveness across entrypoints, trait impls, `#[tool]` handlers, nested `contains` chains, and same-file bare-identifier/const-table callbacks before reporting scoped `phantom_tested`, `dead_tails`, and `dangling_calls`.
  - Embedded `acdd.code_tools` across the `code` profile (`crates/acdd-state/src/profile/embedded/code.yaml`) so synthesis, build, polish, and review gates enforce ACDD MCP navigation, `duplicates`/`dead_tails` hygiene, and blast-radius checks.
  - Added `gates_patch:` profile inheritance overrides, taskless `--goal` runs (defaulting to `code` with `worktree` isolation unless `--in-place` or `--profile audit`), `ClaimResp.pre_task_slice.predecessor_proofs`, and `save.report` materialization strictly confined under `.acdd/reports/*.{md,json}`.

### Changed

- **Embedded Profile Consolidation (`base`, `code`, `audit`)**:
  - Consolidated embedded profiles to 3 (`base`, `code`, `audit`), replacing `default`, `in_place`, and `workspace` and removing `specs.<col>.isolation`.
- **Unified DTO Schemas, Format Compression & State Layout**:
  - Derived `schemars::JsonSchema` (`#[schemars(transparent)]`) on canonical wire IDs/cursors (`crates/acdd-core/src/ids.rs`) and `acdd-state` enums/inputs, replacing manual `Mcp*` clone structs, `From` converters in `src/mcp/tools/inputs.rs`, and `src/mcp/metadata.rs` with schema-derived MCP field validation.
  - Compressed `src/format/` (`lifecycle.rs`, `coordination.rs`, `discovery.rs`, `traversal.rs`, `mod.rs`), delegating `ClaimResp` Markdown rendering to `acdd_state::context::render_claim_prompt_markdown` and removing dead 0.5 formatters.
  - Gated `acdd_state::harness`, `store::memory`, and `InMemorySqliteStore` behind `#[cfg(any(test, feature = "test-support"))]`, split `usecases/{commands,submit}/` into directory modules, consolidated `acdd-index` module layout, relocated exhaustive `acdd-state` integration suites to `crates/acdd-state/tests/state_integration.rs`, and eliminated workspace-wide `duplicates` and confirmed `dead_tails`/`phantom_tested` dead code.
  - Synchronized all workspace crates (`contextunity-acdd`, `acdd-core`, `acdd-index`, `acdd-state`) and documentation to `0.6.2`, and replaced legacy benchmark scripts/artifacts with a modernized `v0.6.2` `commerce-release-update` benchmark suite (`benchmarks/`).

### Removed

- **Legacy Profile, ErrorCode & VCS Surface Simplification**:
  - Removed runtime-red `proof: contract` / `expected_failure` / `contract_author` gates and regex command `output:` / `compile_errors:` stdout scrapers (`exit_code == 0` + `code.seam` + `max_net_loc` govern verification).
  - Removed `git_commit_notes`, `CommitMode` (`git.commit_mode`), `git.integrate`, and public `rollback` / `integrate` CLI commands and `acdd_state` actions (`acdd run` lands task branches internally via `rebase-ff` and terminal checks verify `TreeOid` parity via `verify_tasks_integrated`).
  - Removed `GIT_HOOK_REJECTED` and `GIT_HOOK_MUTATED_TREE` from `ErrorCode` (`30 -> 28` canonical variants), `src/mcp/metadata.rs`, and legacy `#![allow(dead_code, unused_imports)]` suppressions, `core`/`engine` module shims, and temporary deleted-symbol allowlists across `crates/` and `tests/commitment_integrity`.

## [0.6.1] - 2026-11-18

This maintenance and architectural hardening release enforces single-owner boundaries (`INV-SINGLE-OWNER`) across validation, glob matching, `WhenExpr` parsing, configuration loading, and process inspection, eliminates tautological verification paths, hardens SQLite read-only and schema `v2` handling, and unifies CI and release automation.

### Changed

- **Single-Owner Governance & Engine Unification (`INV-SINGLE-OWNER`)**:
  - Unified workspace, profile, spec, and cross-spec/cross-workspace dependency validation in `crates/acdd-state` (`validate` + `usecases::deps`), replacing the duplicate validator in `src/cli/validate.rs` with a thin adapter (`< 150` lines).
  - Consolidated brace-aware `{a,b}` glob expansion, path admission, and pattern/scope overlap matching into `acdd_core::path_policy` (`expand_glob_braces`, `glob_matches`, `patterns_overlap`, `scopes_overlap`, `WorkspacePathPolicy::from_workspace_config`), removing duplicate engines from `acdd-state/profile` and `acdd-state/machine/schedule`.
  - Consolidated `WhenExpr` parsing/evaluation in `acdd_state::profile::when` (reused by `machine/graph.rs`) and cross-platform `read_process_start_time` in `acdd_core::sys`.
  - Enforced strict `acdd.yaml` parsing (`#[serde(deny_unknown_fields)]`), explicit legacy `0.5.x` key rejection (`CONFIG_MIGRATION_REQUIRED`), linked-workspace path/name checks (`load_adapter_strict` / `load_adapter_report`), and pruned 3 dead `Migration*` variants from `ErrorCode` (`33 -> 30`).
- **Domain-Pure Embedded Profiles & Profile Inheritance**:
  - Removed hardcoded Rust/Unix exit codes (`101`, `134`) and `.acdd/frameworks` assumptions from embedded `default.yaml` (`expected_failure: {}` accepts runtime failures `1..=125` with `0` compile errors).
  - Fixed `inherit.rs` `tools: null`, `tools` + `tools_add`, `use_template` overrides, `profiles_dirs`, and top-level `scope_args_by_pattern` propagation.
- **Unified CI & Release Pipeline**:
  - Consolidated CI testing and multi-platform release packaging/publishing into [`.github/workflows/ci.yml`](.github/workflows/ci.yml).
- **Workspace Version `0.6.1`**:
  - Bumped all workspace crates (`contextunity-acdd`, `acdd-core`, `acdd-index`, `acdd-state`) and installation documentation to `0.6.1`.

### Fixed

- **Live `submit` Freshness, `code.seam`, `docs.links`, and Runner Expansion**:
  - Added `DomainProvider::observe_freshness` and `path_policy` propagation on `DomainCheckReq`/`DomainContextReq` so `acdd submit` verifies live freshness digests rather than trusting cached trace metadata, alongside container-scoped tree hashing and `{file_stem}` / `{path}` expansion in `runner/command.rs`.
  - Fixed `code.seam` to filter deleted files, support pure-deletion/test/type-only modules, honor `WorkspacePathPolicy`, and remove the 5-symbol truncation cap; fixed `docs.links` to pass when `admitted_scope` contains zero Markdown files.
  - Enforced physical `modules.code` / `modules.docs` disablement: index builds omit disabled sources (producing an empty index when `code: false` and `docs: false`), while `query` / `ast` / `docs` CLI commands and disabled MCP tools/read modes fail closed with `FEATURE_DISABLED`; fixed `extract_pre_task_slice` to pick the latest accepted blueprint revision, preserved `context:` lens YAML order without budget overflow, and prevented `sync_archive_to_primary` from committing directly on `main`.
- **`PF-002` `ast_grep_search` Local-Variable Recall**:
  - Restricted `nodes.name` SQL prefiltering in `ast_grep_search` / `acdd ast grep` strictly to top-level indexed declaration AST kinds so local bindings and assignments (`x = $EXPR`) keep 100% recall.
- **SQLite `READ_ONLY`-First, Schema `v2` (`runs.spec_id`), and Integration/Test Fixtures**:
  - Updated `StateReader` to open `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` first (with WAL `-shm` recovery without unintended writes), bumped state schema to `v2` adding `runs.spec_id`, hardened `git_ops/integrate.rs` blob-OID verification and crash-recovery cleanup manifests, added `INV-SINGLE-OWNER` architectural fences in `tests/commitment_integrity/state_boundaries.rs`, and updated stale `tasks_db` fixtures in `tests/mcp/**` to `state_db`.

## [0.6.0] - 2026-11-17

This release replaces the legacy task engine with a unified, domain-agnostic profile, workflow graph, and SQLite state engine (`crates/acdd-core` + `crates/acdd-state`), introduces pluggable `DomainProvider` extensions, flattens the CLI and MCP surface around `.acdd/state.sqlite`, consolidates ADRs to `0001..0012`, promotes `ROADMAP.md`, and documents manual `0.5 -> 0.6` upgrading in [migration-0.6.md](docs/runbooks/migration-0.6.md).

### Added

- **Leaf SPI & Unified State Engine (`crates/acdd-core`, `crates/acdd-state`)**:
  - Consolidated pure validated IDs (`ids`), error codes (`errors`), normalized `WorkspacePathPolicy` predicates (`path_policy`), `SchemaDef` (`schema`), and `DomainProvider` SPI (`ports`) into the strict leaf crate `acdd-core`.
  - Introduced `acdd-state` with a 9-table `.acdd/state.sqlite` store, CAS precondition/versioning transitions, unified `git:` isolation & `save:` strategies, 6-tier budget context compiler, wave scheduler/runner, and deterministic failpoint recovery.
  - Wired host `src/state_bridge/` (`CodeDomainProvider` + `DocsDomainProvider`) with live worktree AST checks, `index_generation`/`live_files_digest` cache keying, and read-only SQLite query enforcement, plus the [domain extensions guide](docs/architecture/domain-extensions.md) for thin `crates/acdd-domain-*` shim-crates.
- **Flat CLI & 19+Plugin MCP Surface**:
  - Unified lifecycle CLI commands (`init`, `start`, `draft`, `status`, `list`, `show`, `next`, `claim`, `verify`, `submit`, `extend-scope`, `reopen`, `sync`, `subtask`, `blackboard`, `snapshot`, `rollback`, `integrate`, `clear`, `run`, `tool`, `validate`).
  - Exposed 19 built-in MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`, `acdd_guide`, `acdd_read`, plus code-map, AST, and doc tools) alongside dynamic `<domain>_<name>` plugin tools gated by `acdd.yaml` `modules:`.
- **Consolidated Architecture, Performance (`PF-002`, `AR-001`) & Roadmap (`0001..0012`)**:
  - Implemented and archived `PF-002` (`docs/milestones/archive/PF-002-indexing-delta-and-query-acceleration.md`): single-pass positioned `payloads.bin` spooling, expanded `CompactTag` + `ACDD-LOCAL-FACTS` binary string-table pooling, parallel `ExtractionCache` delta with batch Zstd hydration and `TEMP TABLE` frontier lookups, and 100%-recall literal-anchor AST prefiltering (`PreparedAstMatcher`).
  - Implemented and archived `AR-001` (`docs/milestones/archive/AR-001-borrowed-data-and-composable-execution.md`): borrowed surface dispatch and single `Adapter` reuse, in-place MCP payload compaction, moved cold-extraction candidates, borrowed runner role context, workspace-wide deduplication onto `acdd-core` (`format`, `fs`, `lock`, `git`, `telemetry`, `config`), de-monomorphized CLI (`&mut dyn Write`) and state (`&dyn StateBackend` `exec_tx_dyn`) dispatch, deterministic linker/directory ordering, and `code_map_inspect` `symbol`/`file_path` aliases plus single-language directory auto-inference on `ast_grep_search`.
  - Hardened macOS/APFS SQLite safety (`PRAGMA journal_mode = DELETE` on `state.bak.sqlite`, non-creating read-only WAL fallback, canonicalized `NOFOLLOW` paths).
  - Consolidated legacy ADRs into 12 accepted decisions (`docs/adr/0001..0012`) and promoted `ROADMAP.md` to the repository root.

### Changed

- **Workspace Version `0.6.0` & 3-Library-Crate DAG**: Bumped all workspace crates (`contextunity-acdd`, `acdd-core`, `acdd-index`, `acdd-state`) to `0.6.0`.

### Removed

- **Legacy Task Engine & Surface (`crates/acdd-tasks`)**: Decommissioned `crates/acdd-tasks`, `src/engine/tasks.rs`, legacy CLI groups (`milestone`, `plan`, `workflow`, `task`, `checkpoint`), and legacy MCP tools (`task_*`, `session_checkpoint`, `forge_guide`).

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
- **Release Notes Automation**: Added `CHANGELOG.md` and automated release-notes extraction in `.github/workflows/release.yml` for `v*` tag releases.

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

[Unreleased]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.6.2...v0.7.0
[0.6.2]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.5.3...v0.6.0
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
