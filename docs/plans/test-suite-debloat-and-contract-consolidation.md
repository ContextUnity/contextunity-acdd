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

Operational in-place debloat plan governed by [tests/AGENTS.md](../../tests/AGENTS.md),
[test-suite-refactor](../../skills/test-suite-refactor/SKILL.md), and the dedicated
[tests-debloat workflow](../../skills/acdd/references/acdd-profile-examples/tests-debloat.workflow.yaml).

**Non-negotiable budget & invariants:**
- **Strict negative test-count delta**: Every step MUST strictly reduce the total `#[test]` count (`tests_after < tests_before`) in its admitted scope (target: $\ge 30\%$ reduction across top bloated modules).
- **Zero extra harness growth**: Never increase `Workspace::with_files` / `McpTestHarness::new` / `WaveHarness` calls; share one immutable base workspace or `OnceLock<PersistedSnapshot>` per contract matrix and test pure DAG/state rules against in-memory `TasksStore`.
- **Zero production tampering**: Production source files outside `#[cfg(test)]` are strictly read-only.
- **Zero dimension loss**: Every deleted or merged review-fix regression must map (`old test -> retained contract matrix row -> preserved invariant`) or prove subsumption by an existing stronger seam test; zero `/tmp/contextunity-test-*` leaks.

## 1. Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails

- **Target**: Remove trivial single-caller common test helpers and redundant structural meta-tests in `tests/common/`, share `PersistedSnapshot` in `tests/linker/value_flow.rs`, and parameterize repetitive runner variant clones in `tests/acdd/orchestrator/spawner.rs` (`net #[test] delta < 0`).
- **Scope**: [tests/common/workspace_contract_tests.rs, tests/common/mod.rs, tests/linker/value_flow.rs, tests/acdd/orchestrator/spawner.rs, tests/core.rs]
- **Subtasks**: [{"subtask_ref":"inventory-common-and-spawner","title":"Inventory tests/common/workspace_contract_tests.rs, linker/value_flow.rs, and spawner.rs tests and map every retained contract dimension"},{"subtask_ref":"prune-meta-test-bloat","title":"Inline single-caller tests/common/mod.rs wrappers and prune redundant structural self-tests in workspace_contract_tests.rs"},{"subtask_ref":"share-linker-snapshot-and-parameterize-spawner","title":"Share OnceLock<PersistedSnapshot> in linker/value_flow.rs and collapse spawner.rs clones into table-driven matrices"}]
- **Status**: ready

## 2. Consolidate ACDD Orchestrator and Boundary Regression Bloat

- **Target**: Audit and collapse bug-fix regressions and cloned workspace setups across the highest-density `tests/acdd/` modules (`lifecycle.rs`, `milestones/clear.rs`, `tasks/boundaries/signature.rs`, `tasks/seams/indexed_paths.rs`) into focused contract tables and in-memory `TasksStore` checks (`>= 30%` fewer `#[test]` functions, zero temp-dir leaks).
- **Scope**: [tests/acdd/orchestrator/lifecycle.rs, tests/acdd/milestones/clear.rs, tests/acdd/tasks/boundaries/signature.rs, tests/acdd/tasks/seams/indexed_paths.rs]
- **Depends On**: [Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails]
- **Subtasks**: [{"subtask_ref":"inventory-acdd-bloat-clusters","title":"Freeze #[test] and harness baseline across target tests/acdd/ files and map REMOVE / MERGE clusters"},{"subtask_ref":"collapse-lifecycle-and-clear-bloat","title":"Collapse lifecycle.rs (35 tests) and clear.rs (20 tests) review-fix clones into pure in-memory or shared-base contract matrices"},{"subtask_ref":"parameterize-signature-and-indexed-paths","title":"Parameterize signature.rs (17 tests) and indexed_paths.rs (15 tests) without dropping unique boundary/failure assertions"}]
- **Status**: ready

## 3. Consolidate MCP Freshness and Incremental Harness Bloat

- **Target**: Deduplicate repeated `McpTestHarness::new` / `Workspace::with_files` setups and historical bug-fix tests in `tests/mcp/installer.rs`, `tests/mcp/test_read.rs`, `tests/mcp/freshness/incremental_freshness.rs`, `tests/mcp/freshness/admission.rs`, and `tests/incremental/scope.rs` (`>= 30%` fewer `#[test]` and harness builds).
- **Scope**: [tests/mcp/installer.rs, tests/mcp/test_read.rs, tests/mcp/freshness/incremental_freshness.rs, tests/mcp/freshness/admission.rs, tests/incremental/scope.rs]
- **Depends On**: [Remove Meta-Test Bloat and Finish Migrated Shared-Fixture Tails]
- **Subtasks**: [{"subtask_ref":"inventory-mcp-harness-density","title":"Inventory McpTestHarness and Workspace::with_files density across tests/mcp/ and tests/incremental/scope.rs"},{"subtask_ref":"consolidate-installer-and-read-matrices","title":"Consolidate installer.rs (21 tests) and test_read.rs (17 tests) homogeneous scenarios onto shared workspaces per matrix"},{"subtask_ref":"merge-freshness-admission-and-scope-duplicates","title":"Merge duplicate incremental_freshness.rs (14 tests / 16 workspaces), admission.rs (14 harnesses), and scope.rs (19 tests) regressions"}]
- **Status**: ready

## 4. Audit Query Linker Languages and Crate Unit Suites

- **Target**: Prune duplicate/tautological unit and integration tests across `tests/query_context/`, `tests/languages/`, and `crates/*/src/`, prove zero coverage regression via targeted invariant checks, and verify full suite parity (`clippy`, `cargo test --all-targets`, `cargo test -p acdd-tasks --lib`, zero leaked `/tmp/contextunity-test-*`).
- **Scope**: [tests/query_context/ranking_and_coverage.rs, tests/query_context/symbol_context.rs, tests/languages/sql.rs, tests/languages/typescript/grammar/declarations.rs, tests/languages/typescript/dom/platform_globals.rs, crates/acdd-index/src/, crates/acdd-tasks/src/, crates/acdd-core/src/]
- **Depends On**: [Consolidate ACDD Orchestrator and Boundary Regression Bloat, Consolidate MCP Freshness and Incremental Harness Bloat]
- **Subtasks**: [{"subtask_ref":"consolidate-query-language-and-crate-units","title":"Consolidate remaining query/language matrix clones (ranking_and_coverage.rs, symbol_context.rs, sql.rs, TypeScript suites) and crate unit duplicates"},{"subtask_ref":"verify-full-debloat-parity","title":"Verify strict Clippy (-D warnings), cargo test --all-targets, cargo test -p acdd-tasks --lib, and net negative test/harness delta"}]
- **Status**: ready
