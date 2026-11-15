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

- **Target**: Split the 4,786-line monolithic tests/acdd/orchestrator/waves.rs into cohesive domain submodules by concern (scheduling, recovery, escalation, cancellation) and decouple pure SQL/DAG scheduling and blackboard state assertions onto lightweight in-memory TaskStore fixtures instead of full Git/Python subprocess WaveHarness initialization.
- **Scope**: [tests/acdd/orchestrator/waves.rs, tests/acdd/orchestrator/waves, tests/acdd/orchestrator/mod.rs]
- **Status**: completed
- **Evidence**: {"profile_digest":"f4d5e2a0b61f1355a5966e552641e40a463521438ddd2aab3f9cf1d22e4ca615","outcomes":[{"gate":"build","attempt":1,"passed":true,"evidence":{"session_id":"forge-runner-d06892f7d632a9fd8263a85a3b8ab3973c24a16e8016f237eebf50dac09e424a","proof":{"none_proof":{}},"integration_review":null,"binding":null}},{"gate":"review","attempt":1,"passed":true,"evidence":{"session_id":"forge-runner-2283138623000aff4da8b41dc1e6a7681c5d209757b5cbdb9586258084187d52","proof":{"review_proof":{"decision":"pass","contours":{"paths":{"applicable":true,"evidence":"Step changes remain within waves.rs and waves/. Authorized pre-plan changes were excluded from scope findings. No files were edited or staged during review."},"claims":{"applicable":true,"evidence":"Compared all 104 original tests, case tables, assertions, attributes, and moved helpers against HEAD. No dropped cases or weakened assertions found. Added one in-memory DAG/dependency test. Domain decomposition is cohesive; validation-only matrices use RootGateHarness without full WaveHarness initialization. Remaining root API fixtures preserve filesystem registry and snapshot seams."},"concurrency":{"applicable":true,"evidence":"cargo test --test acdd acdd::orchestrator::waves:: passed under parallel execution: 105 passed, 0 failed, 0 ignored. Mutable stores and snapshots are confined to individual fixtures. No qualifying shared immutable indexed corpus requires OnceLock."},"project_isolation":{"applicable":true,"evidence":"Reviewed workspace ownership, marker cleanup, store replacement, and FixtureSnapshot capture/restore containment. No host repository mutation or cross-test fixture sharing found; repository HEAD and working-tree status remained unchanged."},"administration":{"applicable":true,"evidence":"Reviewed TESTS.md, tests/AGENTS.md, tests/acdd/AGENTS.md, and test-harness architecture. Existing documentation links and canonical acdd test binary remain valid. Test collection lists 105 wave tests. Strict Clippy exited 0; permitted pre-existing manifest notices were non-blocking. git diff --check was clean. No unused helpers or dead shims found."}}}},"integration_review":null,"binding":null}},{"gate":"deliver","attempt":1,"passed":true,"evidence":{"save":"snapshot"}}]}

## 2. Share Read-Only Indexed Fixtures in Query and Linker Suites

- **Target**: Share read-only cold indexes via `OnceLock<PersistedSnapshot>` from `tests/common/sqlite.rs`, dropping the builder `Workspace` inside `get_or_init`, for compatible immutable corpus assertions in ranking_and_coverage.rs (1,584 lines), symbol_context.rs (1,264 lines), search_and_paging.rs (552 lines), linker/method_semantics.rs (1,110 lines), and linker/value_flow.rs (442 lines). Use local `Workspace` instances when disk source files are required.
- **Scope**: [tests/query_context/ranking_and_coverage.rs, tests/query_context/symbol_context.rs, tests/query_context/search_and_paging.rs, tests/linker/method_semantics.rs, tests/linker/value_flow.rs]
- **Status**: ready

## 3. Parameterize Repetitive Harness Clones into Contract Matrices

- **Target**: Collapse homogeneous variant test clones (role/env/session variants in spawner.rs 728 lines, filter/exclusion variants in incremental/scope.rs 1,145 lines, source/span diagnostic permutations in diagnostics_and_source.rs 1,247 lines) into parameterized table-driven contract matrices sharing one harness setup outside each loop without losing edge-case diagnosis.
- **Scope**: [tests/acdd/orchestrator/spawner.rs, tests/incremental/scope.rs, tests/query_context/diagnostics_and_source.rs]
- **Depends On**: [Share Read-Only Indexed Fixtures in Query and Linker Suites]
- **Status**: ready

## 4. Consolidate Small Top-Level Integration Test Binaries

- **Target**: Fold small standalone integration binaries into existing domain suites—move tests/impact_context.rs (390 lines) and tests/linked_workspaces.rs (371 lines) under tests/query_context.rs, tests/cli_roots.rs (370 lines) into tests/mcp.rs, tests/docs_check.rs (224 lines) and tests/release_packaging.rs (111 lines) into tests/core.rs—and update test route references in TESTS.md while preserving domain discoverability.
- **Scope**: [tests/impact_context.rs, tests/linked_workspaces.rs, tests/cli_roots.rs, tests/docs_check.rs, tests/release_packaging.rs, tests/core.rs, tests/query_context.rs, tests/mcp.rs, TESTS.md]
- **Status**: ready

## 5. Remove Meta-Test Bloat and Verify Full Test Suite Parity

- **Target**: Inline trivial single-caller helpers in tests/common/mod.rs, deduplicate overlapping assertions and trim redundant structural self-tests in tests/common/workspace_contract_tests.rs (234 lines), and prove full suite parity via cargo clippy --all-targets --all-features -- -D warnings, cargo test --all-targets, and cargo test -p acdd-tasks --lib.
- **Scope**: [tests/common/workspace_contract_tests.rs, tests/common/mod.rs, tests/core.rs, tests/AGENTS.md, TESTS.md]
- **Depends On**: [Decompose Orchestrator Wave Suite and Isolate Pure State Tests, Parameterize Repetitive Harness Clones into Contract Matrices, Consolidate Small Top-Level Integration Test Binaries]
- **Status**: ready
