---
title: "Test Suite Debloat and Contract Consolidation Plan"
doc_type: plan
purpose: >-
  Eliminate accumulated review-fix regression bloat, duplicate harness setups, and meta-test
  overhead across repository test suites under strict negative #[test] and harness budgets
  while preserving 100% of observable public contract dimensions.
depends_on: []
---

# Test Suite Debloat and Contract Consolidation Plan

Operational in-place debloat plan governed by [tests/AGENTS.md](../../../tests/AGENTS.md),
`skills/test-suite-refactor/SKILL.md`, and the dedicated
[tests-debloat workflow](../../../skills/acdd/references/acdd-profile-examples/tests-debloat.workflow.yaml).

**Non-negotiable budget & invariants:**
- **Strict negative test-count delta**: Every step MUST strictly reduce the total `#[test]` count (`tests_after < tests_before`) in its admitted scope (target: $\ge 30\%$ reduction across top bloated modules).
- **Zero extra harness growth**: Never increase `Workspace::with_files` / `McpTestHarness::new` / `WaveHarness` calls; share one immutable base workspace or `OnceLock<PersistedSnapshot>` per contract matrix and test pure DAG/state rules against in-memory `TasksStore`.
- **Zero production tampering (Steps 1–6)**: Production source files outside `#[cfg(test)]` are strictly read-only in Steps 1–6 (Steps 7–10 explicitly authorize profile/runtime helpers, the completed `forge -> acdd` rename across `crates/` and `src/`, and unit test migrations).
- **Zero dimension loss**: Every deleted or merged review-fix regression must map (`old test -> retained contract matrix row -> preserved invariant`) or prove subsumption by an existing stronger seam test; zero `/tmp/contextunity-test-*` leaks.

## 1. Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails

- **Target**: Remove trivial single-caller common test helpers and redundant structural meta-tests in `tests/common/`, share `PersistedSnapshot` in `tests/linker/value_flow.rs`, and parameterize repetitive runner variant clones in `tests/acdd/orchestrator/spawner.rs` (`net #[test] delta < 0`).
- **Scope**: [tests/common/workspace_contract_tests.rs, tests/common/mod.rs, tests/linker/value_flow.rs, tests/acdd/orchestrator/spawner.rs, tests/core.rs]
- **Subtasks**: [{"subtask_ref":"inventory-common-and-spawner","title":"Inventory tests/common/workspace_contract_tests.rs, linker/value_flow.rs, and spawner.rs tests and map every retained contract dimension"},{"subtask_ref":"prune-meta-test-bloat","title":"Inline single-caller tests/common/mod.rs wrappers and prune redundant structural self-tests in workspace_contract_tests.rs"},{"subtask_ref":"share-linker-snapshot-and-parameterize-spawner","title":"Share OnceLock<PersistedSnapshot> in linker/value_flow.rs and collapse spawner.rs clones into table-driven matrices"}]
- **Status**: completed

## 2. Consolidate ACDD Orchestrator and Boundary Regression Bloat

- **Target**: Audit and collapse bug-fix regressions and cloned workspace setups across the highest-density `tests/acdd/` modules (`lifecycle.rs`, `milestones/clear.rs`, `tasks/boundaries/signature.rs`, `tasks/seams/indexed_paths.rs`) into focused contract tables and in-memory `TasksStore` checks (`>= 30%` fewer `#[test]` functions, zero temp-dir leaks).
- **Scope**: [tests/acdd/orchestrator/lifecycle.rs, tests/acdd/milestones/clear.rs, tests/acdd/tasks/boundaries/signature.rs, tests/acdd/tasks/seams/indexed_paths.rs]
- **Depends On**: [Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails]
- **Subtasks**: [{"subtask_ref":"inventory-acdd-bloat-clusters","title":"Freeze #[test] and harness baseline across target tests/acdd/ files and map REMOVE / MERGE clusters"},{"subtask_ref":"collapse-lifecycle-and-clear-bloat","title":"Collapse lifecycle.rs (35 tests) and clear.rs (20 tests) review-fix clones into pure in-memory or shared-base contract matrices"},{"subtask_ref":"parameterize-signature-and-indexed-paths","title":"Parameterize signature.rs (17 tests) and indexed_paths.rs (15 tests) without dropping unique boundary/failure assertions"}]
- **Status**: completed

## 3. Consolidate MCP Freshness and Incremental Harness Bloat

- **Target**: Deduplicate repeated `McpTestHarness::new` / `Workspace::with_files` setups and historical bug-fix tests in `tests/mcp/installer.rs`, `tests/mcp/test_read.rs`, `tests/mcp/freshness/incremental_freshness.rs`, `tests/mcp/freshness/admission.rs`, and `tests/incremental/scope.rs` (`>= 30%` fewer `#[test]` and harness builds).
- **Scope**: [tests/mcp/installer.rs, tests/mcp/test_read.rs, tests/mcp/freshness/incremental_freshness.rs, tests/mcp/freshness/admission.rs, tests/incremental/scope.rs]
- **Depends On**: [Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails]
- **Subtasks**: [{"subtask_ref":"inventory-mcp-harness-density","title":"Inventory McpTestHarness and Workspace::with_files density across tests/mcp/ and tests/incremental/scope.rs"},{"subtask_ref":"consolidate-installer-and-read-matrices","title":"Consolidate installer.rs (21 tests) and test_read.rs (17 tests) homogeneous scenarios onto shared workspaces per matrix"},{"subtask_ref":"merge-freshness-admission-and-scope-duplicates","title":"Merge duplicate incremental_freshness.rs (14 tests / 16 workspaces), admission.rs (14 harnesses), and scope.rs (19 tests) regressions"}]
- **Status**: completed

## 4. Audit Query Linker Languages and Crate Unit Suites

- **Target**: Prune duplicate/tautological unit and integration tests across `tests/query_context/`, `tests/languages/`, `crates/*/src/`, and remove fragile repo-manifest/ADR substring meta-tests (`tests/acdd/orchestrator/mod.rs::adr_reconciliation_and_contract_matrix_contract`), proving zero production-contract loss via `OnceLock<PersistedSnapshot>` sharing and clean parity.
- **Scope**: [tests/query_context/ranking_and_coverage.rs, tests/query_context/symbol_context.rs, tests/languages/sql.rs, tests/languages/typescript/grammar/declarations.rs, tests/languages/typescript/dom/platform_globals.rs, tests/acdd/orchestrator/mod.rs, crates/acdd-index/src/, crates/acdd-tasks/src/, crates/acdd-core/src/]
- **Depends On**: [Consolidate ACDD Orchestrator and Boundary Regression Bloat, Consolidate MCP Freshness and Incremental Harness Bloat]
- **Subtasks**: [{"subtask_ref":"consolidate-query-language-and-crate-units","title":"Consolidate query/language matrix clones, crate unit duplicates, and remove adr_reconciliation_and_contract_matrix_contract"},{"subtask_ref":"verify-full-debloat-parity","title":"Verify strict Clippy (-D warnings), cargo test --all-targets, cargo test -p acdd-tasks --lib, and net negative test/harness delta"}]
- **Status**: completed

## 5. Collapse Remaining Medium Bloat Clusters and Subprocess Loops

- **Target**: Consolidate the remaining 8–15 test clusters (`tests/core/`, `tests/acdd/orchestrator/waves/`, `tests/acdd/orchestrator/cycle_contracts.rs`, `tests/acdd/orchestrator/workflow_inheritance.rs`, `tests/mcp/context/guide.rs`, `tests/receivers/`, `tests/framework_manifests/`) and move pure state/routing/recovery rows off `git`/`python3` subprocesses onto in-memory `TasksStore` checks while keeping thin integration seam coverage (`<= 1-2` harnesses per file).
- **Scope**: [tests/core/scanner_guard_limits.rs, tests/core/database_guard.rs, tests/acdd/orchestrator/waves/configuration.rs, tests/acdd/orchestrator/waves/command_proofs.rs, tests/acdd/orchestrator/waves/recovery.rs, tests/acdd/orchestrator/cycle_contracts.rs, tests/acdd/orchestrator/workflow_inheritance.rs, tests/mcp/context/guide.rs, tests/receivers/, tests/framework_manifests/]
- **Depends On**: [Consolidate ACDD Orchestrator and Boundary Regression Bloat, Consolidate MCP Freshness and Incremental Harness Bloat]
- **Subtasks**: [{"subtask_ref":"collapse-core-and-guide-matrices","title":"Collapse database_guard.rs (12t/12h), scanner_guard_limits.rs (13t/10h), and mcp/context/guide.rs (12t/6h) into 1-2 shared matrices each"},{"subtask_ref":"debloat-waves-receivers-and-manifests","title":"Fold waves/*, cycle_contracts.rs, workflow_inheritance.rs pure state cases onto TasksStore and consolidate tests/receivers (62t) and tests/framework_manifests (40t)"}]
- **Status**: completed

## 6. Consolidate Multi-File Language Suites and Cross-Layer Unit Duplicates

- **Target**: Eliminate small-file fragmentation across `tests/languages/` (373 tests across scattered files such as `python/semantics.rs`, `html/browser_templates/assets.rs`, `typescript/modules/barrels.rs`, `typescript/dom/factories.rs`) into domain matrices backed by shared `OnceLock<PersistedSnapshot>` fixtures (`>= 40%` reduction), and eliminate cross-layer duplication between `crates/acdd-index/src/` (`memory_budget_tests.rs`, `scanner/mod.rs`) and integration tests.
- **Scope**: [tests/languages/, crates/acdd-index/src/db/writer/memory_budget_tests.rs, crates/acdd-index/src/scanner/mod.rs]
- **Depends On**: [Audit Query Linker Languages and Crate Unit Suites, Collapse Remaining Medium Bloat Clusters and Subprocess Loops]
- **Subtasks**: [{"subtask_ref":"consolidate-language-domains-into-snapshots","title":"Consolidate scattered tests/languages/* modules into per-domain OnceLock<PersistedSnapshot> contract matrices"},{"subtask_ref":"deduplicate-index-unit-vs-integration","title":"Deduplicate memory_budget_tests.rs (13t) and scanner/mod.rs (14t) against core/index integration boundaries"}]
- **Status**: completed

## 7. Optimize Test Execution Speed and Binary Timeouts

- **Target**: Cut full verification wall time (`check_full_repo`, currently ~5.5 min) by consolidating redundant root integration binaries in `tests/*.rs` (currently 9 separate link targets), trimming artificial `sleep`/fixed timeouts in process tests (`tests/acdd/orchestrator/spawner.rs`, `lifecycle.rs`), and tuning test debuginfo (`[profile.test] debug = "line-tables-only"`) plus parallel cross-binary execution (`cargo nextest` / `check_full_repo` command where approved).
- **Scope**: [tests/core.rs, tests/acdd.rs, tests/mcp.rs, tests/languages.rs, tests/query_context.rs, tests/incremental.rs, tests/linker.rs, tests/framework_manifests.rs, tests/acdd/orchestrator/spawner.rs, tests/acdd/orchestrator/lifecycle.rs]
- **Depends On**: [Consolidate Multi-File Language Suites and Cross-Layer Unit Duplicates]
- **Subtasks**: [{"subtask_ref":"trim-subprocess-timeouts-and-binary-links","title":"Lower unnecessarily long fixed sleeps/timeouts in readiness-synchronized process tests and consolidate small tests/*.rs binaries"},{"subtask_ref":"tune-test-profile-and-nextest-runner","title":"Benchmark and apply [profile.test] line-tables-only debuginfo and cross-binary parallel test execution (with explicit approval for Cargo.toml / .acdd/profile.yaml)"}]
- **Status**: completed

## 8. Eradicate Legacy Forge Identities and Git Refs

- **Target**: Rename all remaining active `forge` identities (`forge_guide` -> `acdd_guide`, default repository/project `"forge-mcp"` -> `"acdd"`, `refs/forge/{state,snapshots,root-snapshots,evidence}/` -> `refs/acdd/...`, `"forge-snapshot:"`, `"checkpoint@forge.invalid"`, `"Forge checkpoint"`, `"forge-provisional-v1"`, `"forge-owner-sha256-v2"`, `FORGE-LOCAL-FACTS`, `FORGE-EXTRACTION-FACTS`, `__FORGE_*` / `__forge_*`, `forgetakes`/`forgereturns`, and `forge-*`/`forge_*` temp/log prefixes) across `crates/`, `src/`, `tests/`, `README.md`, `skills/`, and active `docs/`, plus remove stale live `.git/refs/forge/` and legacy checkpoint refs.
- **Scope**: [crates/acdd-core/src/, crates/acdd-index/src/, crates/acdd-tasks/src/, src/, tests/, README.md, skills/acdd/, docs/reference/, docs/runbooks/in002-rename-inventory.md, docs/workflows/test-plan-lifecycle.workflow.yaml]
- **Depends On**: [Optimize Test Execution Speed and Binary Timeouts]
- **Subtasks**: [{"subtask_ref":"rename-mcp-core-and-index-tokens","title":"Rename forge_guide -> acdd_guide, commitment algorithm, binary fact headers, AST meta-var tokens, and index/config forge identifiers across acdd-core, acdd-index, src/, README.md, skills/, and MCP/core/incremental tests"},{"subtask_ref":"rename-git-refs-and-default-project","title":"Rename refs/forge/* -> refs/acdd/*, forge-snapshot:/checkpoint@forge.invalid/forge-provisional-v1 markers, and default repository/project 'forge-mcp' -> 'acdd' across acdd-tasks, src/cli/plan.rs, active docs, and tests/acdd"},{"subtask_ref":"clean-live-git-refs-and-db-checkpoints","title":"Delete stale refs/forge/* refs in .git, align local acdd state, and verify zero unintended forge tokens remain in live code/configs/tests/active docs"}]
- **Status**: completed

## 9. Accelerate Snapshot Recovery Role Matrices Linker and Languages

- **Target**: Move pure SQLite/checkpoint descriptor permutations out of `snapshot_recovery/database.rs` into fast `acdd-tasks` unit tests, replace repeated Python+CLI role spawns in `lifecycle`/`waves` matrices with one cached precompiled Rust test-role helper binary, consolidate ~12 cold `Workspace::new().build()` calls in `tests/linker/method_semantics.rs` onto shared `OnceLock<PersistedSnapshot>` fixtures, and optimize slow `tests/languages/` suites while keeping every contract seam intact.
- **Scope**: [crates/acdd-tasks/src/store/recovery/, tests/acdd/snapshot.rs, tests/acdd/tasks/workspaces/snapshot_recovery/, tests/acdd/orchestrator/lifecycle/, tests/acdd/orchestrator/lifecycle.rs, tests/acdd/orchestrator/waves/, tests/linker/, tests/languages/]
- **Depends On**: [Eradicate Legacy Forge Identities and Git Refs]
- **Subtasks**: [{"subtask_ref":"move-pure-checkpoint-permutations-in-memory","title":"Move pure checkpoint corruption permutations out of snapshot_recovery/database.rs into crates/acdd-tasks/src/store/recovery/ while preserving disk crash/lock/GC fault injection seams"},{"subtask_ref":"compiled-role-helper-for-orchestrator-matrices","title":"Compile one cached Rust test-role helper binary for non-CLI lifecycle/waves matrices while preserving canonical Python/acdd external runner seam tests"},{"subtask_ref":"share-linker-and-language-snapshots","title":"Consolidate redundant Workspace::new().build() fixtures in tests/linker/method_semantics.rs and slow tests/languages/ modules onto shared OnceLock<PersistedSnapshot> or in-memory extractors"}]
- **Status**: completed

## 10. Reconcile Rules Independent Review and Cross-Platform Verify

- **Target**: Reconcile `AGENTS.md`, `src/AGENTS.md`, `tests/AGENTS.md`, `.acdd/profile.yaml`, and active docs with final symbols, pass independent `sol:high` reviews on every change, and pass full clippy + all-targets + zero-leak verification on Linux and `ssh -n maxos`.
- **Scope**: [AGENTS.md, src/AGENTS.md, tests/AGENTS.md, .acdd/profile.yaml, docs/]
- **Depends On**: [Accelerate Snapshot Recovery Role Matrices Linker and Languages]
- **Subtasks**: [{"subtask_ref":"reconcile-rules-and-docs","title":"Reconcile AGENTS docs, local profile, and reference docs with final live symbols and run acdd validate"},{"subtask_ref":"verify-linux-and-maxos-via-ssh","title":"Verify Linux uncontended timings + ISO_TMP_LEAKS=0 and run full clippy + test --workspace --all-targets + leak check on maxos over ssh -n"}]
- **Status**: completed
