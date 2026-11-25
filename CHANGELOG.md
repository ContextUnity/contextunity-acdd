# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.1] - 2026-11-26

### Added

- **0-1 Knapsack Context Packing, RRF Search, SQL Table External Callers & Path Overview (`UX-003`)**:
  - Added bounded deterministic 0-1 knapsack selection (`KnapsackItem`, `pack_knapsack_indices`) in `crates/acdd-core/src/response.rs` (re-exported at `acdd_core` root), consolidating serialized JSON lens byte accounting (`pack_json_lens` / `crates/acdd-state/src/context/budget.rs`) across `state_bridge` and Tier 6 lenses (`apply_tier6_context_lenses`) instead of truncating after the first oversized item.
  - Added `OverviewInput.path` scoped overview (`code_map_overview(path=...)` / `acdd query overview --path`) backed by canonical relative-path validation in `crates/acdd-index/src/db/reader/search.rs`, plus `GetDocInput.include_candidates` (`false` default) and `CodeMapSearchInput.include_pulse` (`true` default) controls.
  - Extended `code.external_callers` (`src/state_bridge/code_domain/{context,signature_scope}.rs`) to include active and baseline-deleted `.sql` `table` nodes across `defines_table`, `queries_table`, `mutates_table`, and `references_table` edges with canonical quote-aware `acdd_index::languages::sql::identifier_keys` fallback while keeping `code.signature_scope` callable-only.
- **Parallel Verification Runner (`scripts/check-full-repo.sh`) & `polish_check`**:
  - Added `.acdd/profile.yaml` `polish_check` (fast `commitment_integrity`, workspace Clippy `-D warnings`, `docs check`, and `validate` on `polish`), `./scripts/check-full-repo.sh` (fast sequential Phase 1 pre-checks followed by concurrent Linux `cargo test --workspace --all-targets` and `./scripts/test-macos-ssh.sh` using zero temporary files), and the explicit `repo.tests` rule prohibiting manual `cargo test --workspace` during task/polish iteration.
- **AST `PERF-*` Rules & `classification` Search Filter**:
  - Added `PERF-*` `ast_pattern` lint rules eliminating `ctx.node.parent()` hot-path scans, plus `classification` filtering (`production` / `test`) in `code_map_search` / `acdd query search` and multi-language `ast_grep_search` (`languages` list).

### Changed

- **Weighted RRF Search Ranking, Selector Resolution, `acdd_read` & `<4 KiB` Markdown Compaction (`UX-003`)**:
  - Replaced hardcoded additive scoring constants and binary `graph_group` partitioning in `crates/acdd-index/src/db/symbols/search.rs` with weighted Reciprocal Rank Fusion (`k = 60`) over structural tier, FTS5 BM25, and bounded non-containment graph-degree centrality, fixed 500-hit shortlist generation-stable paging across cutoffs, pre-slice demotion of snippetless `module` rows, and strict `exact: true` / `include_pulse: false` SQLite graph isolation before `search_graph_pulse`.
  - Tightened `crates/acdd-index/src/db/reader/selectors.rs` (`Type::method` `n.kind` fix, `#`/`.` suffix boundary fallback, canonical import/export-only 1-hop resolution), decoupled `get_doc` reference candidates onto `include_candidates`, switched `acdd_read` (`src/engine/reader.rs`) search default to `context = 0` with `## <path>` grouping, `**/archive/**` exclusion, and `line_count = 100`, and updated defaults (`ResponsePolicy::page_size = 12`, `max_body_lines = 100`, `file_uris = false`) while preserving ADR-0006 JSON field parity and compacting Markdown discovery/traversal/preview envelopes (`src/format/`).
- **AST Search Capture Hygiene, MCP Schema Compaction & CLI Parity (`plan-hotfix-ast-schema-cli-ergonomics`)**:
  - Dropped anonymous empty `""` wildcard captures in `structural_match`, excluded `yaml`/`json`/`toml` grammars when code grammars parse an `ast_grep_search` pattern without explicit `language`, defaulted MCP `ast_grep_search` `limit` to `15`, compacted repeated MCP JSON schema docstrings in `src/mcp/tools/inputs.rs`, added cross-tool `rejected_arguments` hints in `src/mcp/tools/dispatch.rs`, and aligned CLI `acdd read` / `acdd query inspect` defaults with MCP (`Detail` config + `include_coverage = false`).

### Fixed

- **Interactive Mechanical Verify Retry & Post-Delivery Trailer Integration (`fix-verify-integrated`, `fix-run-tests-retry`)**:
  - Updated `crates/acdd-state/src/git_ops/integrate.rs::verify_tasks_integrated` Rule (a) to recognize `Acdd-Claim: <run_id>/task:<task_ref>:deliver/<rev>` trailers (`rev: u32 > 0`) when `Acdd-Integrated:` is absent (preserving the anti-revert check) so manual `rebase-ff`, rebases onto `main`, and post-delivery polish pass cleanly.
  - Updated `crates/acdd-state/src/usecases/verify.rs::handle_verify_with_failure_return` so interactive CLI/MCP `acdd verify` failures record failed traces and keep mechanical gates (`run_tests`) `InProgress` at the same `claim_revision` for legal retry without bouncing to `polish`, while preserving `acdd run` (`execute_gate_node`) automatic `fail:` routing and explicit `submit --action reject`.

### Added (Earlier Unreleased Milestones)

- **Language-Agnostic Linker & Structural Lint Extensions (`AR-004`)**:
  - Added `ComplexityLimits` (`architecture.complexity`), opt-in `architecture.unused_public_api` (default `false`; enabled in `acdd.yaml`), and `ArchitectureConfig` validation in `crates/acdd-core/src/lint.rs`, plus `syntax.fact_budget_exceeded`, `syntax.intra_crate_type_homonym`, and `docs.unused_arch_zone` diagnostics in `crates/acdd-index/src/db/lint/`.
  - Added single-pass Tree-sitter AST statement fingerprinting (`StatementFingerprint`, Type-2 local-identifier normalization) and `cognitive_complexity` / `max_nesting_depth` telemetry in `crates/acdd-index/src/ast/fingerprints.rs` (bumping `LOCAL_FACTS_BINARY_MAGIC` to `ACDD-LOCAL-FACTS\0\x05`).
  - Added `ValueExpr::CallbackParam`, `CallableSignature`, `ReturnTypeSpec` compound generic wrappers on `ReceiverTypeRule.{member_returns,call_returns}`, and SQLite/linker-backed `dead_tails.unused_public_api` analysis in `crates/acdd-index/src/db/lint/unused_public_api.rs`.
  - Added executable `ExecutableIdentity` binary-drift detection and live Unix re-exec reload (`BINARY_DRIFT_RELOAD` / `BINARY_DRIFT_UNAVAILABLE`) for `acdd serve` (`crates/acdd-core/src/fs.rs`, `src/mcp/server.rs`, `src/cli/reload.rs`), plus `INV-SINGLE-OWNER-NO-INLINE-TEMP-DIR` and Layer 2/hot-path `acdd-rule` blocks across `docs/architecture/{modularity,indexing}.md`, `docs/adr/{0010,0011}.md`, and `acdd.yaml`.
- **Unified Architectural & Graph Linter (`acdd lint`, `code_map_lint`, `code.lint`)**:
  - Added pure leaf lint types (`LintCategory`, `LintSeverity`, `ArchRuleKind`, `ArchRuleSpec`, `ZoneSelector`, `ArchitectureConfig`), segment-normalized external symbol prefix matching, and optional `acdd.yaml` `architecture.{zones,rules}` in `crates/acdd-core/src/lint.rs`, registering `code_map_lint` as the 20th built-in static MCP tool (`BUILTIN_MCP_TOOLS`).
  - Added fenced ` ```acdd-rule ` YAML extraction in `crates/acdd-index/src/docs.rs` persisted on section `Node.details["arch_rules"]` (failing closed into `errors` under existing Merkle commitments while skipping fenced code blocks during `INV-*` scanning).
  - Unified `arch` (all 8 `ArchRuleKind` primitives), `duplicates`, `dead_tails`, `cycles`, `syntax`, and `docs` verification behind `crates/acdd-index/src/db/lint/`, delegating `db::traversal`, `db::cycles`, and `db::reader::diagnostics` callers without duplicate logic.
  - Exposed dual-surface `acdd lint` CLI, `code_map_lint` MCP tool, shared Markdown/JSON formatting in `src/format/lint.rs`, freshness-bound `CodeDomainProvider` check `code.lint`, and `[ENFORCED]` invariant badges in `DocsDomainProvider` + `ClaimResp`.
- **Embedded `documentation` GateProfile (`--profile documentation`)**:
  - Added `crates/acdd-state/src/profile/embedded/documentation.yaml` as the 4th embedded profile (`base`, `code`, `audit`, `documentation`) for compiler-free living documentation and `acdd-rule` governance gated on `docs.links` and `code.lint` (`[arch, docs]`), and wired `code_map_lint` + `code.lint` into `code.yaml`, `audit.yaml`, and `.acdd/profile.yaml`.
- **Default Bash, CSS, JSON Profiles & `.j2` Template Support (`LS-003`)**:
  - Enabled `lang-bash` (`tree-sitter-bash`: `.sh`, `.bash`, `.zsh`), `lang-css` (`tree-sitter-css`: `.css`), and `lang-json` (`tree-sitter-json`: `.json`) in `default` and `all-languages` features, registered `Bash`, `Css`, and `Json` in `LanguageId`, and added `.j2` template indexing to the HTML profile.
- **Modular `ManifestParser` Plugins, Two-Tier `ActivationCondition`, & Expanded Framework Packs (`LS-003`)**:
  - Split `dependency_registry.rs` into `crates/acdd-index/src/languages/dependency_registry/` (`mod.rs`, `parser.rs`, `framework_packs.rs`, `parsers/{cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs`, `tests.rs`) with closed `Ecosystem` enums, `MANIFEST_PARSERS`, `ModulePrefix` extraction (`go.mod`, `composer.json` PSR-4, `*.csproj` `RootNamespace`), and `read_confined_manifest_string`.
  - Extended `ActivationCondition` in `manifests/semantic.rs` with `Package`, `SyntaxAttribute`, `SyntaxAttributePrefix`, `FileExtension`, and `Any` (bounded nesting depth `<= 4`), `SyntaxActivationKeys`, and persisted `FileActivationSignals` (`ACDD-LOCAL-FACTS\0\x04`) for syntax-activated rules (`alpinejs.toml`).
  - Added bundled `playwright.toml` and `web.toml` (DOM/WebIDL platform types), expanded `rust-std.toml`, `rust-cargo.toml`, `django.toml`, `unittest.toml`, and `nuxt.toml` (Pinia `defineStore` auto-imports), added `rust/declarative_macros.toml` + `rust/macros.rs` (`#[derive(...)]` and declarative struct/enum macros), and added `python/logging_factories.rs` and Proto `google/protobuf/*` stdlib recognition.

### Changed

- **Typed Graph Core, `LanguageLinker` Trait Inversion, Submit/Verify Isolation & Hygiene Closure (`AR-005`)**:
  - Migrated `Node.kind`, `Node.language`, and `CompactEdge` (`TryFrom<&PublicEdge>`) onto closed 1-byte `Copy` enums (`36`-variant `NodeKind`, `34`-variant `EdgeKind`, `22`-variant `ReferenceKind`, `LanguageId`, `EdgeConfidence`), migrated all `290` production raw `node.details[key]` accesses onto typed `NodeSemanticEntry` / `NodeDetailFlags` accessors (`ARCH-L2-NO-RAW-NODE-DETAILS-INDEX: error`), and replaced `ClassicFieldsFirstDto` / `ClassicBindingsFirstDto` shims and untyped `serde_json::Map` DB reader / formatter pipelines with typed DTOs and `ImpactDirection` / `TraversalMode` enums.
  - Completed `LanguageLinker` trait inversion behind crate-private `ProfileLinker::linker()` (removing all `#[allow(private_interfaces)]` escapes, per-language fields on `ImportContext` / `semantic_context::Context`, `__rust_*` string tags, and duplicate Rust path resolvers while wiring Tier-2 OO member indexing), unified `StateError` onto `AcddError` with single-pass `config/loader.rs`, decomposed `src/mcp/server.rs`, relocated `WorktreeSpanContext` to `acdd_core::telemetry`, removed `ComplexityLimitsWire` and dead threshold fields under strict `deny_unknown_fields`, fixed `docs_check` `EPIPE` panics, and removed unused root `Cargo.toml` dependencies (`petgraph`, `zstd`, `hashbrown`), `14` single-caller context-bag structs, and `98` mechanical doc stubs.
  - Hardened gate execution and test isolation: removed auto-verify fallback from `submit` (`ensure_fresh_trace` fails closed with `TASK_PROOF_UNOBSERVED`), moved `observe_freshness` in `verify::run_gate_checks` after all checks so post-mutation `index_generation` binds into `execution_digest`, scrubbed `ACDD_DB` / `ACDD_EXTRACTION_CACHE_DIR` and `ACDD_*`/`GIT_*` in child processes and test harnesses, and added directory-style matching in `shell_template::scope_entry_matches_path`.
  - Closed Rust call-graph / `dead_tails` gaps (combinator callbacks, `serde`/`schemars` string callable attributes, shadowed local calls such as `sql::tokens`, and `Context::with_flows` / `NamedBindings` chains), deleted unused `SyntaxActivationKeys::matches_attribute`, `DependencyRegistry::standard_manifests_digest`, and `PipelineTestHooks::overlap`, and eliminated remaining `>= 5`-line workspace duplicates.
- **Agnostic Value Flow, Rust Linker Precision & Workspace Cycle Breaking (`AR-004`)**:
  - Evaluated `ValueExpr::CallbackParam`, unified `ValueFlowCollectorCore` struct/tuple/variant destructuring, generalized `lookup_builtin_member`, element-wise `Tuple`/`RustTuple`/`Option`/`Result` `BranchJoin`, monotonic mutable reassignment typing, universal `ReceiverTypeRule.call_returns` and `external_aliases` propagation, and Tier-1 `SyntaxGrammarSpec` adoption (`python`, `typescript`, `javascript`, `vue`, `rust`), removing `__rust_fn_param` and hardcoded TypeScript WebIDL arms.
  - Resolved deterministic Rust gaps (`#[derive(clap::Parser)]` `Self`, `#[serde(...)]` callback references, `enum_variant` attribute skipping, `RustForwarding` external re-export aliases, `impl Trait for $ty` macros, generic-bounded inherent methods, and `<= 8` bounded `trait_object_dispatch` `resolved` coverage).
  - Switched `duplicates` to Type-2 AST-statement fingerprint windows with maximal-span coalescing and cross-name LCS/Jaccard similarity (`>= 0.80`, excluding tests by default), isolated `cycles.scc` to multi-node non-test/non-archive `imports` SCCs (`len >= 2`), promoted `dead_tails.dangling_call` to `error`, and hardened `ast_pattern` single-parse fail-closed evaluation and `docs_lint` out-of-bounds `#L` checks.
  - Broke all intra-crate module import SCCs across `acdd-core`, `acdd-index`, `acdd-state`, and `src/`, disambiguated `CompactGraph`/`CompactEdge`, decomposed high-complexity modules, eliminated verified clones and `dead_tails`, strengthened embedded `code` profile architectural-reuse and inline-test guards, and ratcheted `.acdd/profile.yaml` `check_full_repo` onto all 6 `acdd lint` categories.
- **Universal Declarative Language Engine & Tier-1/Tier-2 Resolution (`LS-003`)**:
  - Unified Layer 2 dispatch behind `LanguageProfile::linker()`, typed `LanguageFileState`, and `SyntaxGrammarSpec` (`DeclarationRole`, `ImportKind`, `CallForm`, `Visibility`) compiled into `CompiledSyntaxGrammar`.
  - Extracted shared `ScopeFrameFlags`, `ScopeState`, `ValueFlowCollectorCore`, and `GenericScopeAnalysis` into `crates/acdd-index/src/languages/support/value_flow_core.rs`, migrating Rust, Python, TypeScript/JavaScript, and all Tier-2 profiles (`go`, `java`, `kotlin`, `csharp`, `php`, `ruby`, `c`, `cpp`) onto the shared core and upgrading `GENERIC_LINKER` for multi-file packages, external import aliases, wildcard/static imports, and `ModulePrefix` normalization.
  - Expanded static resolution coverage across Rust (chained/literal/slice receivers, struct-field & `&T` propagation, `const`/`static`/`enum_variant`/`associated_type`), Python (`Callable`/`Protocol`, `type[T]`, union parameters, `with ... as`, `Path /`, f-strings/slices, `super().__init__`), and Web/Frontend (IIFE scopes, Vue `v-for` loop variables, DOM/WebIDL, Alpine syntax activation).
- **Canonical Invariant Consolidation (`**/AGENTS.md` $\to$ `docs/adr/` & `docs/architecture/`)**:
  - Consolidated architectural `INV-*` definitions and live self-hosting `acdd-rule` blocks (`INV-CORE-LEAF`, `INV-STATE-NO-INDEX`, `INV-STATE-BRIDGE-BOUNDARY`, `INV-STATE-PURE-MACHINE`, `INV-STATE-SQLITE-CONFINED`, `INV-GIT-SINGLE-OWNER`) into `docs/adr/{0001,0003}.md` and `docs/architecture/{modularity,test-harness}.md`, leaving root and subtree `AGENTS.md` files as concise routers.

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

This release replaces the legacy task engine with a unified, domain-agnostic profile, workflow graph, and SQLite state engine (`crates/acdd-core` + `crates/acdd-state`), introduces pluggable `DomainProvider` extensions, flattens the CLI and MCP surface around `.acdd/state.sqlite`, consolidates ADRs to `0001..0012`, promotes `ROADMAP.md`, and documents manual `0.5 -> 0.6` upgrading.

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

[Unreleased]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.7.1...HEAD
[0.7.1]: https://github.com/ContextUnity/contextunity-acdd/compare/v0.7.0...v0.7.1
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
