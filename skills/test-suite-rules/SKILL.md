---
name: test-suite-rules
description: Author, organize, deduplicate, and refactor tests through real production seams, table-driven contract matrices, lowest-cost isolation, and leak-free harnesses. Load before adding, modifying, reviewing, or refactoring any test.
---

# Test Suite Rules

Prove admitted observable contracts through the narrowest real production seam
without duplicating lower-level matrices, weakening assertions, or leaking
harnesses and temporary resources.

Read the repository's root `AGENTS.md`, `TESTS.md`, and nearest `tests/**/AGENTS.md`
before authoring or refactoring tests. Optional repository add-ons under
`.agents/skills/test-suite-rules/ADDON.md` may supply local commands, lanes, and
stricter budgets without weakening these rules.

## 1. Iterative Focus vs. Completion Gate

- **During iteration**: Run only the narrowest real seam test for the active task
  alongside strict static checks/linters, retaining real exit codes and counts.
- **At final handoff**: Run the full-workspace/all-targets verification suite once
  on the settled diff before completion.
- **No synthetic masking**: Never bypass unoptimized or slow suites via ignore
  flags, environment-gated skips, or wall-clock `sleep` padding; fix setup cost
  or synchronize with deterministic readiness signals and explicit deadlines.

## 2. Production Seams & Evidence Classes

Enter through a supported caller or composition root and traverse real parsers,
stores, and collaborators. Place protocol-faithful fakes strictly behind
unavailable external boundaries, never inside in-memory unit wrappers masking
broken wiring. Assert observable outputs, persisted state, revisions, cleanup,
and rejected-operation **non-mutation**.

| Evidence Class | What It Proves | Role |
|---|---|---|
| `contract-seam` | Observable behavior through a real production caller/interface | Primary contract proof |
| `integration-boundary` | Serialization, persistence transactions, process/transport, or adapters | Primary boundary proof |
| `local-projection` | Pure mapping, dispatch, or local invariant math | Supplemental only |
| `architecture-guard` | Static boundary, schema, or structural ownership constraint | Supplemental only |
| `brittle-structural` | Private internals, call counts, setter echoes, or config substrings | Refactor / remove candidate |

For every material behavior scenario, name and confirm at least one plausible
contract-breaking mutation that the test rejects.

## 3. Table-Driven Matrices vs. Domain Boundaries

- **Table-driven matrices**: Consolidate homogeneous variations (inputs, error
  codes, boundary limits, spelling aliases, mode flags) into one parameterized /
  table-driven test case.
- **One shared base state**: Build shared immutable workspaces, schemas, or cold
  snapshots **once outside the loop** (ordering read-only rows before mutating
  rows) so per-row checks mutate only the single tested delta without $O(N^2)$
  rebuilds, subprocess storms, or full-store dumps per iteration.
- **Distinct domain tests**: Author a dedicated test function only for a genuinely
  distinct domain contract or orthogonal workflow—never by cloning heavy setups,
  pasting sequential multi-setup scenarios into one mega-test, or splitting one
  homogeneous rule into sibling files.
- **Target consolidation**: Keep tests inside the repository's canonical domain
  test suites rather than spawning new standalone test binaries.

## 4. Lowest Honest Isolation & Speed

- **In-memory first**: Drive pure parsers, AST rules, DAG scheduling, gate
  transitions, and validation logic in-memory / in-process.
- **Reserve heavy isolation**: Use child processes, network servers, and isolated
  VCS worktrees strictly when testing process lifecycle, signals, timeouts, IPC
  transport, or physical directory isolation—using minimal 1–2 stage fixtures when
  full multi-stage lifecycles add no signal.
- **Cancel pending async work**: Explicitly abort background sleep/timer tasks on
  timeout tests so workers never hang teardown waiting for a long sleep deadline.

## 5. Harness Discipline & Resource Hygiene

- **Reuse existing harnesses first**: Inspect shared test support before adding
  any helper. Only extract a new helper when `>= 2` callers share the setup on
  one coherent domain ownership seam.
- **Helpers build state; tests assert**: Keep scenario-specific mutations and all
  contract assertions visible inside the test function. Ban single-caller wrapper
  functions, one-off observation structs returned from huge blocks, unrelated
  static utilities attached to stateful harnesses, harness type aliases, and
  silent fallbacks on setup failure.
- **Zero temporary leaks (`BEFORE == AFTER`)**: Never store RAII temp dirs or
  workspace handles in process-global statics (statics skip cleanup destructors
  on exit). Share read-only snapshots built from temporary directories that drop
  immediately after initialization, keep local scoped temp dirs for disk-mutating
  tests, restore read-only directory modes safely without following symlinks, and
  verify zero leaked temp directories across test runs.

## 6. Zero Review-Fix Bloat & Refactoring Order

- **Review remediation**: Fix the root mechanism in production code by
  generalization (shared validator, canonical backend path) so the defect class
  disappears. Maintain zero standalone bug-fix tests during review closure; add at
  most one data row to an existing contract matrix when an external contract
  boundary was genuinely missing.
- **Suite refactoring order**: Execute cleanly in sequence:
  `Inventory unique baseline` $\to$ `Audit reachability (KEEP defensive | REWRITE | REMOVE | UNCERTAIN)` $\to$ `Replace & Delete in the same diff` $\to$ `Decompose monoliths along natural domain seams` $\to$ `Verify baseline count <= before`.
  - Keep defensive corruption, race, security, or crash tests only when a real
    path can produce the state and no stronger seam already covers it.
  - Delete duplicate mocks, private-field tests, tautological self-equality checks,
    brittle manifest/CI substring checks, and tests guarding abandoned
    intermediate designs unless an active compatibility/security contract requires
    their absence.
  - Record an explicit `old test -> retained seam` mapping when replacing weaker
    tests; block closure if any reachability classification stays `UNCERTAIN` or
    production code outside an authorized scope was altered incidentally.
