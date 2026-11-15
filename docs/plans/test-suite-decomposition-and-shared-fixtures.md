---
title: "Test Suite Decomposition, Shared Fixtures, and Link-Diet Plan"
doc_type: plan
purpose: "Decompose oversized ACDD test suites, eliminate redundant workspace rebuilds via shared read-only fixtures, parameterize matrix test clones, and consolidate trivial top-level integration binaries."
depends_on: []
---

# Test Suite Decomposition, Shared Fixtures, and Link-Diet Plan

Operational test-suite optimization plan following the architecture rules in
[tests/AGENTS.md](../../tests/AGENTS.md) and historical lessons in
[archive/test-suite-diet-and-fixture-hardening.md](archive/test-suite-diet-and-fixture-hardening.md).
Instead of optimizing for artificial line-count reductions, each step targets
measured compilation/link duplication, repeated full-workspace `.build()` setup,
monolithic file coupling, or wrapper bloat while keeping production-seam proof
strength intact:

- `tests/acdd/orchestrator/waves.rs` (`4,786` lines) + `waves/harness.rs` (`367` lines)
- Query & linker heavy suites: `ranking_and_coverage.rs` (`1,584`), `symbol_context.rs` (`1,264`), `diagnostics_and_source.rs` (`1,247`), `incremental/scope.rs` (`1,145`), `linker/method_semantics.rs` (`1,110`), `incremental_freshness.rs` (`1,057`), `spawner.rs` (`728`)
- Small standalone Cargo integration binaries: `impact_context.rs` (`390`), `linked_workspaces.rs` (`371`), `cli_roots.rs` (`370`), `docs_check.rs` (`224`), `release_packaging.rs` (`111`)
- Shared helper self-test / facade overhead: `tests/common/workspace_contract_tests.rs` (`234`)

## 1. Decompose Orchestrator Wave Suite and Isolate Pure State Tests

- **Target**: Split the 4,786-line monolithic tests/acdd/orchestrator/waves.rs into domain submodules and decouple pure SQL/DAG scheduling tests from full Git/Python subprocess WaveHarness initialization.
- **Scope**: [tests/acdd/orchestrator/waves.rs, tests/acdd/orchestrator/waves/harness.rs, tests/acdd/orchestrator/mod.rs]
- **Status**: ready
- **Subtasks**: [{subtask_ref: split-waves-domain-modules, title: "Split tests/acdd/orchestrator/waves.rs (4,786 lines) into cohesive submodules by concern (scheduling, recovery, escalation, cancellation)"}, {subtask_ref: isolate-inprocess-dag-tests, title: "Move pure scheduler/DAG/blackboard state assertions onto lightweight in-memory TaskStore fixtures instead of Git/Python WaveHarness"}]

## 2. Share Read-Only Indexed Fixtures in Query and Linker Suites

- **Target**: Introduce OnceLock shared read-only Workspace and cold index snapshots in tests/query_context and tests/linker to eliminate redundant per-test .build() rebuilds on immutable fixtures.
- **Scope**: [tests/query_context/ranking_and_coverage.rs, tests/query_context/symbol_context.rs, tests/query_context/search_and_paging.rs, tests/linker/method_semantics.rs, tests/linker/value_flow.rs]
- **Status**: ready
- **Subtasks**: [{subtask_ref: share-readonly-query-fixtures, title: "Group immutable corpus assertions in ranking_and_coverage.rs (1,584 lines), symbol_context.rs (1,264 lines), and search_and_paging.rs (552 lines) under OnceLock read-only built workspaces"}, {subtask_ref: share-readonly-linker-fixtures, title: "Share compiled read-only indexed graphs for compatible multi-language resolution assertions in linker/method_semantics.rs (1,110 lines) and value_flow.rs (442 lines)"}]

## 3. Parameterize Repetitive Harness Clones into Contract Matrices

- **Target**: Collapse homogeneous variant test clones that repeatedly initialize heavy harnesses in spawner.rs, incremental/scope.rs, and diagnostics_and_source.rs into table-driven contract matrices sharing one harness outside the loop.
- **Scope**: [tests/acdd/orchestrator/spawner.rs, tests/incremental/scope.rs, tests/query_context/diagnostics_and_source.rs]
- **Depends On**: [Share Read-Only Indexed Fixtures in Query and Linker Suites]
- **Status**: ready
- **Subtasks**: [{subtask_ref: parameterize-spawner-and-scope-matrices, title: "Convert homogeneous role/env/session variants in spawner.rs (728 lines) and filter/exclusion variants in incremental/scope.rs (1,145 lines) into shared-setup table loops"}, {subtask_ref: parameterize-diagnostic-matrices, title: "Fold repeated source/span diagnostic permutations in diagnostics_and_source.rs (1,247 lines) into parameterized matrices without losing edge-case diagnosis"}]

## 4. Consolidate Small Top-Level Integration Test Binaries

- **Target**: Merge small standalone integration test binaries into existing domain suites (tests/core.rs, tests/query_context.rs, tests/mcp.rs) to shrink duplicate Rust linker targets while preserving domain discoverability.
- **Scope**: [tests/impact_context.rs, tests/linked_workspaces.rs, tests/cli_roots.rs, tests/docs_check.rs, tests/release_packaging.rs, tests/core.rs, tests/query_context.rs, tests/mcp.rs]
- **Status**: ready
- **Subtasks**: [{subtask_ref: fold-query-and-root-binaries, title: "Move tests/impact_context.rs (390 lines) and tests/linked_workspaces.rs (371 lines) under tests/query_context.rs, and fold tests/cli_roots.rs (370 lines) into tests/mcp.rs"}, {subtask_ref: fold-docs-and-packaging-checks, title: "Fold tests/docs_check.rs (224 lines) and tests/release_packaging.rs (111 lines) into tests/core.rs and update test route references in TESTS.md"}]

## 5. Remove Meta-Test Bloat and Verify Full Test Suite Parity

- **Target**: Trim redundant structural self-tests in tests/common/workspace_contract_tests.rs, inline trivial single-caller helpers in tests/common/mod.rs, and prove full suite parity via cargo test --all-targets and strict Clippy.
- **Scope**: [tests/common/workspace_contract_tests.rs, tests/common/mod.rs, tests/core.rs, tests/AGENTS.md, TESTS.md]
- **Depends On**: [Decompose Orchestrator Wave Suite and Isolate Pure State Tests, Parameterize Repetitive Harness Clones into Contract Matrices, Consolidate Small Top-Level Integration Test Binaries]
- **Status**: ready
- **Subtasks**: [{subtask_ref: prune-common-facade-and-metatest-bloat, title: "Inline trivial single-caller helpers in tests/common/mod.rs and deduplicate overlapping assertions in workspace_contract_tests.rs (234 lines)"}, {subtask_ref: prove-consolidated-test-suite-gates, title: "Run cargo clippy --all-targets --all-features -- -D warnings, cargo test --all-targets, and cargo test -p acdd-tasks --lib to confirm zero test coverage loss"}]
