---
title: "Test Suite Decomposition, Shared Fixtures, and Link-Diet Plan (Archived Partial)"
doc_type: plan
purpose: "Historical record of completed test binary consolidation, waves.rs decomposition, and initial PersistedSnapshot fixture adoption from commit 668951614."
depends_on: []
---

# Test Suite Decomposition, Shared Fixtures, and Link-Diet Plan

Archived record of completed structural test optimizations landed in `main` commit
`668951614` (*refactor(tests): consolidate binaries, share PersistedSnapshot fixtures, and align governance docs*).
Remaining uncompleted items (`tests/acdd/orchestrator/spawner.rs`, `tests/linker/value_flow.rs`,
and `tests/common/workspace_contract_tests.rs`) and full bug-fix regression debloat were migrated to
[Test Suite Debloat and Contract Consolidation Plan](test-suite-debloat-and-contract-consolidation.md).

## 1. Decompose Orchestrator Wave Suite and Isolate Pure State Tests

- **Target**: Split the 4,786-line monolithic `tests/acdd/orchestrator/waves.rs` into cohesive domain submodules by concern (scheduling, recovery, escalation, cancellation) and decouple pure SQL/DAG scheduling and blackboard state assertions onto lightweight in-memory `TasksStore` fixtures.
- **Scope**: `[tests/acdd/orchestrator/waves.rs, tests/acdd/orchestrator/waves, tests/acdd/orchestrator/mod.rs]`
- **Status**: completed

## 2. Share Read-Only Indexed Fixtures in Core Query and Linker Suites

- **Target**: Adopt `OnceLock<PersistedSnapshot>` in `tests/query_context/ranking_and_coverage.rs`, `tests/query_context/symbol_context.rs`, `tests/query_context/search_and_paging.rs`, and `tests/linker/method_semantics.rs`.
- **Scope**: `[tests/query_context/ranking_and_coverage.rs, tests/query_context/symbol_context.rs, tests/query_context/search_and_paging.rs, tests/linker/method_semantics.rs]`
- **Status**: completed

## 3. Parameterize Incremental Scope and Diagnostics Matrices

- **Target**: Consolidate repetitive filesystem/diagnostic permutations in `tests/incremental/scope.rs` and `tests/query_context/diagnostics_and_source.rs`.
- **Scope**: `[tests/incremental/scope.rs, tests/query_context/diagnostics_and_source.rs]`
- **Status**: completed

## 4. Consolidate Small Top-Level Integration Test Binaries

- **Target**: Fold small standalone integration binaries into canonical domain suites — `tests/impact_context.rs` and `tests/linked_workspaces.rs` into `tests/query_context.rs`, `tests/cli_roots.rs` into `tests/mcp.rs`, and `tests/docs_check.rs` and `tests/release_packaging.rs` into `tests/core.rs`.
- **Scope**: `[tests/core.rs, tests/query_context.rs, tests/mcp.rs, TESTS.md]`
- **Status**: completed
