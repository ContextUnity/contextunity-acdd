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
- **Canonical invariants (`INV-TEST-*`)**: All durable test-seam, harness, in-memory-first, binary-budget, zero-leak (`/tmp/contextunity-test-*` `BEFORE == AFTER`), and profile-driven lifecycle invariants are declared canonically in [test harness architecture](../docs/architecture/test-harness.md) and [TESTS.md](../TESTS.md).
- **Table matrices & zero review-fix bloat**: Consolidate homogeneous variations into one table-driven `#[test]` building shared base state once; during `review -> build` remediation, fix the underlying mechanism and add at most one row to an existing matrix (zero net new `#[test]` functions).
- **In-process & zero-disk defaults**: Drive state/MCP/AST tests through `RunHarness::in_memory()`, `McpTestHarness` in-process handlers, and `ExtractorHarness` without disk tempdirs, `Workspace::new()`, raw `Command::new("git")`, or `Command::new(env!("CARGO_BIN_EXE_..."))` except in dedicated isolation/CLI smoke tests.
- **Fixtures & hygiene**: Use `tests/acdd/fixtures/` / `MilestoneFixtureBuilder` (no raw multiline spec heredocs), `OnceLock<PersistedSnapshot>` for shared read-only SQLite corpora (never static `Workspace`/`TempDir`), `tempfile::TempDir` instead of custom `TempDirDrop`, and remove clones, `phantom_tested`, `dead_tails`, and `dangling_calls` before submit.
<!-- acdd:test-invariants:end -->
- For semantic receiver evidence, cover exact resolution separately from
  possible targets through the linker and persisted query seam.

## 2. Domain modules and binary budget

Every `.rs` file directly under `tests/` is a separate Cargo test binary.
Do NOT create new top-level `tests/<name>.rs` binaries; register new test modules
under one of the seven canonical domain binaries (`acdd.rs`, `core.rs`,
`languages.rs`, `linker.rs`, `mcp.rs`,
`query_context.rs`, `commitment_integrity.rs`), or place `acdd-state`
integration suites in crate-local `crates/acdd-state/tests/state_integration.rs`
(`cargo test -p acdd-state`). Prefer coherent domain modules:

- `crates/acdd-state/tests/state_integration.rs`: `acdd-state` integration suite (profile resolution, machine transitions, git ops, runner, and usecases E2E)
- `tests/acdd.rs` (`tests/acdd/`): unified CLI/bridge `acdd-state` + `acdd-core` `RunHarness` suite (`< 15s`: fixtures matrix, profiles/inheritance, lifecycle/state, blackboard, verification/Git, runner/waves)
- `tests/core.rs` (`tests/core/`, `tests/incremental/`): storage, adapters, scanner limits, docs checks, release packaging, delta indexing and scopes
- `tests/languages.rs` (`tests/languages/`, `tests/framework_manifests/`): extraction, language semantics, packs and manifests
- `tests/linker.rs` (`tests/linker/`, `tests/receivers/`): linking and receivers
- `tests/mcp.rs` (`tests/mcp/`, `tests/mcp_context/`): MCP protocol, CLI roots, and tools
- `tests/query_context.rs` (`tests/query_context/`): query options, impact context, linked workspaces, and search
- `tests/commitment_integrity.rs`: Merkle / commitment integrity
- `tests/common/`: shared workspace, SQLite, MCP harness substrate

Subtree `AGENTS.md` files own local partitioning:

- [`tests/acdd/AGENTS.md`](acdd/AGENTS.md)
- [`tests/framework_manifests/AGENTS.md`](framework_manifests/AGENTS.md)
- [`tests/languages/html/AGENTS.md`](languages/html/AGENTS.md)
- [`tests/languages/profiles/AGENTS.md`](languages/profiles/AGENTS.md)
- [`tests/languages/python/frameworks/AGENTS.md`](languages/python/frameworks/AGENTS.md)
- [`tests/languages/rust/AGENTS.md`](languages/rust/AGENTS.md)
- [`tests/languages/typescript/dom/AGENTS.md`](languages/typescript/dom/AGENTS.md)
- [`tests/languages/typescript/frameworks/AGENTS.md`](languages/typescript/frameworks/AGENTS.md)
- [`tests/receivers/AGENTS.md`](receivers/AGENTS.md)
- [`tests/receivers/rust/AGENTS.md`](receivers/rust/AGENTS.md)
- [`tests/receivers/python/AGENTS.md`](receivers/python/AGENTS.md)

Focused commands:

```bash
cargo test --test languages manifests::<module>::
cargo test --test languages html::<module>::
cargo test --test languages typescript::frameworks::<module>::
cargo test --test linker rust::
cargo test --test linker python::
cargo test --test linker value_flow::
```

Use tables for permutations. Put shared setup in `tests/common/` or the suite
harness. Split files on real domain boundaries, not line-count theater.

## 3. Suite moves

When moving, renaming, splitting, or removing a suite file, update this file and
`TESTS.md` in the same change.

## 4. Authoring patterns

Read [test harness architecture](../docs/architecture/test-harness.md). Archived
debloat and contract-consolidation plan:
[test-suite-debloat-and-contract-consolidation.md](../docs/plans/archive/test-suite-debloat-and-contract-consolidation.md)
(building on the archived
[decomposition plan](../docs/plans/archive/test-suite-decomposition-and-shared-fixtures.md)
and
[reality-reset plan](../docs/plans/archive/test-suite-diet-and-fixture-hardening.md)).

1. **Extend a capability** — add a table row or a single harness mutation; do not
   copy a 100-line test to flip one field.
2. **New contract** — new descriptively named `#[test]` in the owning module;
   drive the real seam through the existing harness.
3. **Split by domain cohesion, not arbitrary line counts**: Decompose multi-subsystem
   monoliths (>1,500–2,000 lines mixing unrelated contracts such as
   `tests/mcp/context.rs` or
   `tests/mcp/tool_evolution/mcp_boundaries.rs`) or shared
   infrastructure (`tests/acdd/support.rs`) into focused domain submodules. Do NOT
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
