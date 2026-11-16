# Tests — Invariants and Test Creation Boundaries

Rules for placing, creating, refactoring, and maintaining tests in
`acdd`.

## 1. Positive contract specifications

- Prove observable positive contracts, valid boundaries, and real fail-closed errors.
- Verify defect fixes through existing domain seams. Structure tests around
  permanent specifications, not transient incidents.
- Prove admitted milestone requirements. Do not invent uncontracted requirements
  to justify a new subsystem.
<!-- acdd:test-invariants:start -->
- **Table-driven contract matrices**: Consolidate homogeneous variations of the
  same capability, boundary rule, error diagnostic, alias/spelling case, or hook
  mode into one table-driven `#[test]` (`for case in cases`). Ban cloning
  heavy `#[test]` functions or calling `Workspace::new()`, `.build()`,
  `InstallerHarness::new`, `McpTestHarness::new`, or subprocess spawns inside
  per-row loop bodies unless each case genuinely tests a distinct cold-build
  initial directory layout; build shared immutable base state once outside the
  loop and copy/mutate only the case-specific delta.
- **Domain decomposition**: Author a separate `#[test]` in the owning domain
  module only for genuinely distinct domain contracts or orthogonal multi-step
  workflows — never by appending unrelated scenarios into an existing workflow
  test, and never by splitting one homogeneous matrix into sibling heavy tests.
- **Fixture boundaries**: Keep unit/extractor tests in-memory. Do not build
  heavy filesystem fixtures or `Workspace::new()` inside pure syntax/AST or
  path-helper tests, and place unit tests inside the owning crate module rather
  than integration crates.
- **Subsume duplicates & review-fix anti-bloat**: When a stronger seam covers
  the contract, fold any unique edge case into it and delete the
  weaker/duplicate test in the same diff. Do **NOT** spawn standalone `#[test]`
  functions or extra `Workspace`/`McpTestHarness` setups to close P1/P2 review
  findings (`review -> build` remediation loops): fix the underlying mechanism by
  generalization (shared validator, single SQL/engine path) so the defect class
  cannot recur, and add at most one data row to an existing contract matrix ONLY
  when a genuine public boundary was missing (zero net new `#[test]` functions
  during review remediation).
- **Runtime-red contract proof**: Contract tests compile and fail at runtime
  (exit 101). Do not use compile failures as red proof.
- **Anti-fake-gating**: Do not hide unoptimized matrices behind `#[ignore]`,
  env filters, hardcoded `thread::sleep`, or `#[test]`-as-subprocess `libtest`
  hacks. Build shared base state once; mutate one field per row.
- **No inline milestone heredocs**: Use fixtures / `MilestoneFixtureBuilder`,
  not raw multi-line milestone strings in tests.
- **In-process engine**: Prefer `milestone_engine::*` / `tasks::*`. Reserve
  `std::process::Command` for dedicated CLI smoke tests.
- **Harness discipline**: Shared setup lives in `tests/common` or a domain
  harness. A harness function needs ≥2 callers. The `#[test]` keeps the mutation
  and every assertion. Never dump static non-domain functions onto `<Domain>Harness`,
  never alias harness types (`use McpTestHarness as Client`), and never inline
  single-caller helpers as 100-line IIFE blocks returning one-off `*Observation`
  structs. See [test harness architecture](../docs/architecture/test-harness.md).
- **Bounded loop & cache discipline**: Do not run full-DB dumps (`persisted_state`)
  or $O(N^2)$ cold index rebuilds inside per-row loops when lightweight checks or
  batched deltas prove the same invariant. Clean up temporary `.acdd/cold-*` files
  and avoid 0%-hit `Weak<Workspace>` or per-iteration directory-walk caches.
- **Seam-preserving decomposition**: Do not replace production seams with
  mockist unit doubles. Ban tautological enum/struct self-equality echoes,
  brittle `Cargo.toml`/CI YAML substring assertions, and multi-hundred-line
  meta-test suites for `tests/common` helpers.
- **Shared read-only indexed fixtures (`PersistedSnapshot`) & zero leaks**:
  NEVER store `Workspace` or `TempDir` inside a `static OnceLock` / `LazyLock`
  (statics never run `Drop` at process exit and leak `/tmp/contextunity-test-*`).
  When two or more read-only tests in a query, linker, or AST suite inspect
  the same immutable SQLite corpus without mutating files, initialize one
  `OnceLock<PersistedSnapshot>` (`tests/common/sqlite.rs`), creating and
  dropping the seed `Workspace` inside `get_or_init`, and open per-test
  in-memory/snapshot connections. When tests require live disk files (e.g.
  `McpTestHarness` or CLI commands), create local `Workspace` values so `Drop`
  runs at test exit, and verify zero leaked `/tmp/contextunity-test-*`
  directories (`BEFORE == AFTER`).
- **Orchestrator & `WaveHarness` boundary**: Reserve Git worktree and Python
  subprocess harnesses (`WaveHarness`, `LifecycleHarness`) strictly for tests
  proving subprocess execution, signal/timeout recovery, or physical worktree
  isolation. Drive pure DAG scheduling, dependency readiness, gate transitions,
  and blackboard routing in-process against lightweight in-memory `TasksStore` /
  `TaskHarness` fixtures.
- **In-process MCP**: Prefer in-process handlers / `McpTestHarness`. Reserve
  `StdioClient` / `mkfifo` for dedicated transport smoke tests.
- **Zero-disk AST**: Language extraction and framework-manifest unit tests stay
  in-memory (no tempdir Git/SQLite) unless the claim requires disk.
- **Plan/workflow fixtures**: Keep Markdown/profile extract fixtures thin until
  AC-003 admits execution. Do not grow execution harnesses ahead of the engine.
<!-- acdd:test-invariants:end -->
- For semantic receiver evidence, cover exact resolution separately from
  possible targets through the linker and persisted query seam.

## 2. Domain modules and binary budget

Every `.rs` file directly under `tests/` is a separate Cargo test binary.
Do NOT create new top-level `tests/<name>.rs` binaries; register new test modules
under the owning canonical domain binary (`acdd.rs`, `core.rs`, `incremental.rs`,
`languages.rs`, `framework_manifests.rs`, `linker.rs`, `mcp.rs`,
`query_context.rs`, `commitment_integrity.rs`). Prefer coherent domain modules:

- `tests/acdd.rs` (`tests/acdd/`): tasks, milestones, blackboard, runners
- `tests/core.rs` (`tests/core/`): storage, adapters, scanner limits, docs checks, release packaging
- `tests/incremental.rs` (`tests/incremental/`): delta indexing and scopes
- `tests/languages.rs` (`tests/languages/`): extraction and language semantics
- `tests/framework_manifests.rs` (`tests/framework_manifests/`): packs and manifests
- `tests/linker.rs` (`tests/linker/`, `tests/receivers/`): linking and receivers
- `tests/mcp.rs` (`tests/mcp/`, `tests/mcp_context/`): MCP protocol, CLI roots, and tools
- `tests/query_context.rs` (`tests/query_context/`): query options, impact context, linked workspaces, and search
- `tests/commitment_integrity.rs`: Merkle / commitment integrity
- `tests/common/`: shared workspace, SQLite, MCP harness substrate

Subtree `AGENTS.md` files own local partitioning:

- [`tests/acdd/AGENTS.md`](acdd/AGENTS.md)
- [`tests/acdd/milestones/AGENTS.md`](acdd/milestones/AGENTS.md)
- [`tests/acdd/tasks/AGENTS.md`](acdd/tasks/AGENTS.md)
- [`tests/acdd/tasks/workspaces/AGENTS.md`](acdd/tasks/workspaces/AGENTS.md)
- [`tests/framework_manifests/AGENTS.md`](framework_manifests/AGENTS.md)
- [`tests/languages/html/AGENTS.md`](languages/html/AGENTS.md)
- [`tests/languages/typescript/frameworks/AGENTS.md`](languages/typescript/frameworks/AGENTS.md)
- [`tests/receivers/AGENTS.md`](receivers/AGENTS.md)
- [`tests/receivers/rust/AGENTS.md`](receivers/rust/AGENTS.md)
- [`tests/receivers/python/AGENTS.md`](receivers/python/AGENTS.md)

Focused commands:

```bash
cargo test --test framework_manifests manifests::<module>::
cargo test --test languages html::<module>::
cargo test --test languages typescript::frameworks::<module>::
cargo test --test linker typed_receivers::rust::<module>::
cargo test --test linker typed_receivers::python::<module>::
cargo test --test linker value_flow::
```

Use tables for permutations. Put shared setup in `tests/common/` or the suite
harness. Split files on real domain boundaries, not line-count theater.

## 3. Suite moves

When moving, renaming, splitting, or removing a suite file, update this file and
`TESTS.md` in the same change.

## 4. Authoring patterns

Read [test harness architecture](../docs/architecture/test-harness.md). Active
debloat and contract-consolidation plan:
[test-suite-debloat-and-contract-consolidation.md](../docs/plans/test-suite-debloat-and-contract-consolidation.md)
(building on the archived
[decomposition plan](../docs/plans/archive/test-suite-decomposition-and-shared-fixtures.md)
and
[reality-reset plan](../docs/plans/archive/test-suite-diet-and-fixture-hardening.md)).

1. **Extend a capability** — add a table row or a single harness mutation; do not
   copy a 100-line test to flip one field.
2. **New contract** — new descriptively named `#[test]` in the owning module;
   drive the real seam through the existing harness.
3. **Split by domain cohesion, not arbitrary line counts**: Decompose multi-subsystem
   monoliths (>1,500–2,000 lines mixing unrelated contracts such as `tests/acdd/mcp.rs`,
   `tests/mcp/context.rs`, `tests/mcp/tool_evolution/mcp_boundaries.rs`, or
   `tests/acdd/orchestrator/waves.rs`) or shared
   infrastructure (`tests/acdd/support/*`) into focused domain submodules. Do NOT
   mechanically chop a cohesive single-domain test file (e.g., 800–1,600 lines testing
   one tool or language surface) or hide test assertions in harness helpers just to
   hit an arbitrary line count. Keep `tests/commitment_integrity/*` under `< 500`
   lines since its submodules are already organized by domain.
4. **New consumer** — extend the existing harness once the production consumer
   exists; prove the real connection. Do not mock both sides.
5. **Return a one-caller helper cleanly** — compose shared primitives (`Workspace`,
   `sqlite::query_rows`, domain harness methods) directly in the test and delete
   any one-off `*Observation` struct shell.

Treat ~50 lines per `#[test]` as a review prompt, not a reason to hide assertions
inside a 1-caller wrapper. Prefer deletion of duplicates over extraction into
god-harnesses.

## 5. Benchmarking isolation

Place latency benchmarks and stress probes under `benchmarks/`, managed by
`benchmarks/run_benchmarks.py`.
