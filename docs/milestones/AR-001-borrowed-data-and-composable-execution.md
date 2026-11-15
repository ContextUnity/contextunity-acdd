---
id: AR-001-borrowed-data-and-composable-execution
title: "Borrowed data paths, cross-crate deduplication, and composable Rust execution"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - src/cli/mod.rs
  - src/cli/ast.rs
  - src/mcp/tools.rs
  - src/mcp/response.rs
  - src/mcp/server.rs
  - src/engine/pipeline.rs
  - src/engine/tasks.rs
  - crates/acdd-core/src/config.rs
  - crates/acdd-core/src/fs.rs
  - crates/acdd-tasks/src/config.rs
  - crates/acdd-tasks/src/fs.rs
  - crates/acdd-tasks/src/engine/orchestrator/task_runner.rs
  - crates/acdd-index/src/db/cache.rs
  - crates/acdd-index/src/db/writer.rs
  - tests/
  - docs/reference/
  - docs/architecture/
  - docs/milestones/AR-001-borrowed-data-and-composable-execution.md
invariants:
  - 'INV-SCOPE-INTEGRITY: Exclude every Cargo.toml, Cargo.lock, build.rs, .acdd/**, adapter/profile configuration, and dependency change from implementation scopes and extensions; this goal authorizes no protected-file exception.'
  - 'INV-CORE-LEAF: Preserve existing crate dependency directions, core leaf isolation, and the frozen engine/database boundary; add no cross-layer imports or shared execution framework.'
  - 'INV-SURFACE-COMPATIBILITY: Preserve tool registration, CLI syntax, public Rust signatures, output defaults, errors, debug-log contents and success/error timing, serialized response bounds, and continuation identities.'
  - 'INV-OWNERSHIP-AT-BOUNDARIES: Borrow immutable policy and role configuration while their owners live; move consumed candidates and CLI results; compact bounded payloads in place without redundant candidate cloning; allocate owned copies only where transport, retained debug data, or durable DTO lifetimes require ownership.'
  - 'INV-ORDERED-SINGLE-DISPATCH: Execute each admitted stage once in its existing order, short-circuit on its existing failure, and preserve special recovery paths; reuse one request-scoped adapter snapshot and local typed helpers/FnOnce composition without a parallel registry or dynamic middleware chain.'
  - 'INV-INDEX-PUBLICATION: Preserve source snapshot verification, extraction cache identity, source-buffer reuse, permit release, cancellation and worker joining, bounded spooling, deterministic graph/commitment bytes, WAL checkpointing to DELETE mode, regular-file sidecar cleanup under exclusive lock, and verified atomic publication.'
  - 'INV-TASK-AUTHORITY: Preserve profile-driven gate selection, genuine worker/session independence, active claims, retained rejection sessions, command observations, immutable candidates, and receipt-backed delivery; runner process success never substitutes for an accepted submission.'
  - 'INV-CROSS-CRATE-SINGLE-SOURCE: Delegate duplicate configuration, path/memory-limit helpers, advisory file locks, catalog storage row builders, and runtime extractor initialization to single workspace owners while preserving public API compatibility and Cargo.toml boundaries.'
  - 'INV-PROOF-BOUNDARY: Prove compatibility through real production callers and deterministic sensitivity mutations; distinguish supplemental ownership evidence from end-to-end proof and make no unmeasured latency, throughput, RSS, or whole-operation zero-allocation claim.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
related_plans: []
---

# Borrowed data paths, cross-crate deduplication, and composable Rust execution

## Outcome and admission

Optimize five concrete ownership, deduplication, and publication boundaries:
MCP/CLI dispatch and bounded presentation, cold extraction and WAL/SHM publication
safety, task gate execution, cross-crate configuration and advisory file locks,
and duplicate catalog storage / extractor-registration wiring. Keep current engine
owners and public contracts. Use ordinary borrowed references, moves, delegations
to `acdd-core`, and small typed functions or closures to express existing stages.
Introduce a helper only when it removes duplicated policy, locking, row assembly,
or init boilerplate or makes an ownership boundary explicit; leave cohesive
algorithms readable in place.

`AR` owns architecture and composition. The inspected active and archive filename
inventory contains no later `AR-NNN` milestone; `AR-001` keeps its reserved slot.
This contract is planned and writes no runtime implementation during authoring
beyond pre-existing hotfix preparation; its task sequence governs completion.

This contract already targets the renamed `contextunity-acdd` paths completed in
the archived [IN-002 rename](archive/IN-002-contextunity-acdd-one-pass-rename.md).
Preserve those canonical paths without old/new-name compatibility scaffolding.

The planning add-on references ADR 0024, but the inspected ADR inventory contains
0001–0021 and no 0024. Its contents are unavailable authority. Existing ADRs and
verified code govern this work; no lifecycle redesign depends on that reference.
No repository-local lesson file is present in the inspected planning add-on paths.

## Architectural viability and negative boundaries

The existing `responding_with_presentation` closure boundary, bounded response
renderer, cold producer/consumer pipeline, `acdd-core` leaf crate, and canonical
task executor already provide the required structure. Extend those compositions
locally. A universal service trait, Tower stack, boxed future chain, plug-in API,
secondary dispatcher, or request-wide context object spanning unrelated domains
adds no admitted value.

Preserve all public function signatures and DTO compatibility (`symbols: []`).
Private helpers stay in their scoped files; `acdd-tasks::config` and
`acdd-tasks::fs` delegate/re-export existing public items from `acdd-core` rather
than introducing breaking signature changes. Do not relocate presentation into
the leaf crate or move task effects into pure models. Preserve existing dependency
directions: both `acdd-tasks` and `acdd-index` already depend directly on
`acdd-core`.

Exclude new dependencies (`Cargo.toml`, `Cargo.lock`), `build.rs` edits, `libc`
additions to `acdd-core` (use `std::fs::File` lock methods available on Rust
`1.100.0`), unsafe production code, lifetime erasure, leaked/static buffers,
interior-mutability workarounds, reader pools, concurrency changes, automatic
retries beyond bounded lock polling, storage schema migrations, cache/version
bumps, feature changes, language/linker algorithm changes, and workflow/profile
schema edits. Normal engine-owned task receipts/checkpoints are governed by the
existing workflow; they do not authorize editing protected infrastructure.

## Grounded symbols and current ownership

Complete source and outline reads confirm the following opportunities; line
numbers describe the inspected baseline and are not permanent identities.

| Verified declaration | Current path and contract | Admitted change |
| --- | --- | --- |
| [`call_tool`](../../src/mcp/tools.rs), lines 1195–1340 | Consumes RMCP requests; validates format/fields, selects worktree/adapter, dispatches through canonical router, enforces response policy, logs if debug enabled. | Keep ordering and router. Retain one request adapter snapshot, borrow `&Adapter` and `&ResponsePolicy` into `read_with_adapter` / finalization, and remove redundant adapter reloads. |
| [`responding_with_presentation`](../../src/mcp/tools.rs), lines 379–439 | Takes `run: impl FnOnce(&scanner::Adapter, &ResponsePolicy) -> anyhow::Result<Value>` and decorates Markdown before bounded rendering. | Reuse this existing closure boundary; keep handlers and presentation metadata typed and local. |
| [`search_paged`](../../src/cli/ast.rs), lines 103–236 & [`Server::read_with_adapter`](../../src/mcp/server.rs), lines 600–670 | `ast::search_paged` calls `scanner::load_adapter(root, None)` even when caller (`Server` or CLI) already loaded `&scanner::Adapter`. | Thread the already-loaded `&scanner::Adapter` (or private adapter-accepting overload inside scoped files) so per-query disk config reloads are eliminated while keeping public signatures intact. |
| [`run_selected`](../../src/cli/mod.rs), lines 1330–1941 | Owns CLI dispatch; bounded `task_list`/`acdd_read` presentation passes `Ok(result.clone())` and retains the original for optional debug logging. | Move the result on the debug-disabled bounded path. Preserve an original snapshot only when enabled logging requires it; preserve debug timing, write, and error behavior. |
| [`result_for_presented`](../../src/mcp/response.rs), lines 80–246 | Owns `Value`, clones `value.clone()` on initial admission and clones `trimmed_value.clone()` inside the progressive trimming loop. | Mutate/compact the owned payload in place when trying fitting/trimmed stages while retaining exact serialized bounds and continuation digest/offset fields without unconditional whole-value clones. |
| [`extract_candidate`](../../src/engine/pipeline.rs), lines 424–497 & [`extraction_worker`](../../src/engine/pipeline.rs), lines 700–795 | `extract_candidate` clones `BuildCandidate` before `into_entry`; `extraction_worker` prepares facts/storage and moves `MemoryPermit`. | Consume `BuildCandidate` after metadata/cache checks and compose worker preparation without cloning candidate strings. |
| [`atomic_build`](../../src/engine/pipeline.rs), lines 1390–1500 | Publishes staged database over target path; lingering `-wal`/`-shm` sidecars from outliving read-only connections trigger post-close sidecar error. | Checkpoint WAL (`checkpoint(&connection, true)`), switch `PRAGMA journal_mode=DELETE;`, close connection, and unlink regular-file `-wal`/`-shm`/`-journal` sidecars under `exclusive_lock` before final `ensure_no_publication_sidecars(&output, false)` check; prove with `output_sidecar_hazards_preserve_generation`. |
| [`run_task_lifecycle`](../../crates/acdd-tasks/src/engine/orchestrator/task_runner.rs), lines 32–248 | Clones selected `RoleProfile` into a tuple despite retaining owning profile; repeats setup/submit glue. | Borrow selected role and role ID until owned invocation boundary; compose local typed stage helpers without changing public DTO/signatures. |
| [`load_adapter`](../../crates/acdd-core/src/config.rs) vs [`load_adapter`](../../crates/acdd-tasks/src/config.rs) | `crates/acdd-tasks/src/config.rs` duplicates `AdapterFile`, `Adapter`, `LinkedWorkspace`, `ScannerLimits` helpers and `load_adapter` verbatim from `crates/acdd-core/src/config.rs`. | Delegate/re-export shared config types and loader/limit helpers from `acdd_core::config`, keeping only SQLite `read_policy(root, &Connection)` and `load_workspace_adapter` in `acdd-tasks`. |
| [`exclusive_lock`](../../crates/acdd-tasks/src/fs.rs), lines 56–101 vs [`generation_lock`](../../crates/acdd-index/src/db/cache.rs), lines 137–194 | Both crates independently reimplement `O_NOFOLLOW` / regular-file advisory `flock` polling loops with duplicate backoff/timeout logic. | Centralize shared bounded-retry advisory file lock helper in `crates/acdd-core/src/fs.rs` using std `File::try_lock`/`try_lock_shared` (Rust `1.100.0`, zero new deps) and route both `acdd-tasks` and `acdd-index` lock callers through it. |
| [`prepare_catalog_storage_with_facts`](../../crates/acdd-index/src/db/writer.rs), lines 118–268 & [`ensure_extractors`](../../src/engine/tasks.rs), lines 8–129 | `writer.rs` duplicates node/edge/doc encoding between `prepare_catalog_storage_with_facts` and `prepare_catalog_rows`; `src/engine/tasks.rs` repeats 10 `ensure_extractors()` wrapper functions. | Unify `prepare_catalog_storage_with_facts` to delegate to `prepare_catalog_rows` + encode; collapse repetitive `ensure_extractors()` wrappers in `src/engine/tasks.rs` with one private local helper. |

## Governing invariants and producer-consumer contract

`search_docs` grounds layer isolation in
[ADR 0018](../adr/0018-layer-dependency-boundaries-and-core-leaf-isolation.md#1-core-leaf-isolation),
response limits in [ADR 0011](../adr/0011-bounded-mcp-response-budgets.md#decision),
publication in [indexing](../architecture/indexing.md#documentation-references-and-verification),
and stage ownership in [performance invariants](../architecture/performance.md#model-and-agent-invariants-immutable-guidelines).
Preserve [dual-surface parity](../adr/0001-dual-surface-parity.md),
[staged publication](../adr/0003-two-phase-staged-publication.md),
[lean serialization](../adr/0012-zero-regression-serialization-and-cold-build-latency.md),
[task snapshots](../adr/0015-acdd-lifecycle-snapshots-and-proactive-defect-resolution.md),
and [workflow/task separation](../adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md).
No accepted ADR changes in this milestone.

| Boundary / owner task | Input → output and consumers | State, configuration, failure, and proof |
| --- | --- | --- |
| surfaces | RMCP request or parsed CLI command → existing engine operation → in-place bounded presentation/result | Existing worktree/config/profile selection, module enablement, defaults, debug flag and byte limits. Reuse one loaded `&Adapter` across read/AST search. Errors short-circuit at the existing stage. CLI and MCP contract tests compare parsed JSON and Markdown semantics, errors, paging and byte limits. |
| indexing | Scanner-owned candidate + shared adapter/cache + reusable source buffer → checked `FileEntry` + facts → storage/catalog → spool, WAL checkpoint + cleanup, and published index | Existing metadata/digest checks and cache-key fields; one owner moves `MemoryPermit` until ingestion/drop. `atomic_build` checkpoints WAL to `DELETE` mode and unlinks regular-file `-wal`/`-shm` sidecars under `exclusive_lock` (rejecting symlinks/non-regular files). Cold/delta and `database_guard` read back the published database. |
| task-execution | Resolved profile + persisted task/gate → role or mechanical execution → accepted gate state/events/receipt | Profile, SQL gates, claims, checkpoints and receipts remain authoritative. Keep independent sessions, rejection reuse, timeout and no-progress failures. Existing lifecycle/restart tests inspect persisted state and owned event DTOs. |
| cross-crate-dedup | `acdd-core::{config, fs}` shared owners → `acdd-tasks::{config, fs}` + `acdd-index::db::cache` callers | Public crate re-exports/signatures stay identical; no `Cargo.toml` edit (`std::fs::File::try_lock` in `acdd-core` without `libc`). Preserve `O_NOFOLLOW`/regular-file validation, stale-lock unlink semantics, and root/adapter rejection errors. |
| storage-and-port-unification | `writer::prepare_catalog_rows` → `prepare_catalog_storage_with_facts`; `src/engine/tasks::with_extractors` → single registered extractor init port | Output bytes from `prepare_catalog_storage_with_facts` match bitwise before/after unification; extractor registration executes once across every task entry point. |

Complete paths to preserve:

1. MCP request → `call_tool` format/field checks → worktree/module selection →
   single adapter load → canonical router → handler `read_with_adapter` /
   `ast::search_paged` → `responding_with_presentation` → in-place bounded
   `result_for_presented` → caller. CLI root/settings selection → `run_selected`
   → same engine owner → current presentation/renderer → writer; retain guide-init
   and source-only-read recovery.
2. Cold build → scanned candidate → byte permit/work queue → `extraction_worker`
   → checked extraction/cache → unified `writer` catalog/storage preparation →
   consumer spool → seal/verify → WAL checkpoint (`TRUNCATE` + `DELETE` mode) +
   regular-file sidecar unlink → atomic publish → locked query readback.
3. CLI task run or wave executor → `run_task_lifecycle` → `engine/tasks`
   extractor registration → canonical claim → complete context → role runner or
   mechanical submit → accepted SQL transition → event/receipt readback. Do not
   synthesize acceptance from observer events or process exit status.

All paths retain their current workspace confinement, validation, limits, cleanup,
and rejection semantics. Add no tenancy/configuration dimension, alternate backend,
network dependency, retry loop, or compensation mechanism. Planned consumers are
the same callers under IN-002's rename; any API evolution belongs to a separate
owner-approved contract.

## Waves, proof policy, and harness allocation

| Wave | Tasks | Dependency |
| --- | --- | --- |
| 0 | `freeze-ownership-and-seams` | none |
| 1 | `compose-surface-dispatch`, `move-cold-extraction-inputs`, `borrow-task-execution-context`, `deduplicate-cross-crate-config-and-locks`, `unify-catalog-storage-and-extractor-port` | each depends only on Wave 0 |

Wave 0 owns shared test registrations and creates minimal valid module stubs.
It registers `tests/mcp/borrowed_dispatch.rs` in `tests/mcp.rs`,
`tests/core/borrowed_pipeline.rs`, `tests/core/cross_crate_deduplication.rs`,
and `tests/core/writer_storage_unification.rs` in `tests/core.rs`, and
`tests/acdd/orchestrator/lifecycle/borrowed_execution.rs` as a child module in
`tests/acdd/orchestrator/lifecycle.rs`. The child inherits the existing Linux
runner lane and can reuse its parent's private `LifecycleHarness` without
exporting a second harness. `tests/core/database_guard.rs` is already registered
in `tests/core.rs`. Preserve established suite-root path conventions.
Downstream tasks never edit these shared indexes or another task's seam file.
Overlaps between bootstrap and later scopes are sequential, never concurrent.

Use `direct-proof`: this is a behavior-preserving refactor and the canonical
[task proof contract](../reference/tasks.md#proof-taxonomy-bundle-generation)
permits an observed exit 0 for that policy. Do not manufacture a red test by
asserting a new public behavior or a private helper's existence. Establish the
compatibility baseline before Build, then prove sensitivity with the mutations
below. Any additional contract that selects expected-red must compile and fail
at runtime with exit 101, never at compilation.

Use the existing `McpTestHarness`, in-process CLI runner, `Workspace`, lifecycle
harness, canonical fixtures, and commitment readback. Do not add a new Cargo
test binary or global allocator. Pointer/lifetime checks and AST ownership review
are supplemental evidence, not substitutes for production-seam compatibility.
No whole-operation allocation ceiling is admitted. Record removed deep copies,
duplication removed, test sensitivity and any limitations in each task's
evidence. Benchmark lanes and host-dependent timing thresholds are out of scope.

## Tasks in this milestone

### task: freeze-ownership-and-seams

```yaml
task_ref: freeze-ownership-and-seams
target: "Freeze the ownership/deduplication/WAL-safety matrix and disjoint waves, pre-register compiling domain seam stubs, and prove the unchanged manifest parser before parallel refactoring without altering production behavior or protected infrastructure"
proof_policy: direct-proof
contract_revision: 2
depends_on: []
scope:
  - docs/milestones/AR-001-borrowed-data-and-composable-execution.md
  - tests/mcp.rs
  - tests/core.rs
  - tests/acdd/orchestrator/lifecycle.rs
  - tests/mcp/borrowed_dispatch.rs
  - tests/core/borrowed_pipeline.rs
  - tests/core/cross_crate_deduplication.rs
  - tests/core/writer_storage_unification.rs
  - tests/acdd/orchestrator/lifecycle/borrowed_execution.rs
symbols: []
status: ready
subtasks:
  - subtask_ref: freeze-stage-and-ownership-matrix
    title: "Confirm stage order, lifetime boundaries, existing errors, protected exclusions, and exact seam allocation in this manifest"
  - subtask_ref: preregister-domain-seams
    title: "Create compiling empty seam modules and register them only in existing suite indexes"
  - subtask_ref: prove-bootstrap-closure
    title: "Validate the real manifest and dependency graph, compile registered stubs, and record the producer-consumer matrix for downstream tasks"
```

Blueprint slice:

- Implementation steps: re-inspect cited owners and the active profile; validate
  exact scopes/DAG/public-symbol closure; publish this matrix; register the five
  new stubs (`borrowed_dispatch`, `borrowed_pipeline`, `cross_crate_deduplication`,
  `writer_storage_unification`, `borrowed_execution`); run parser proof and strict
  Clippy. Keep existing errors and DTOs; shared error taxonomy changes and new
  shared core definitions are unnecessary.
- Symbol signatures: none added or modified. The parser seam is existing
  `snapshot_parses_frontmatter_identity_and_task_blocks` in
  `crates/acdd-tasks/src/milestones/contract.rs`.
- Test seam: `cargo test -p acdd-tasks --lib snapshot_parses_frontmatter_identity_and_task_blocks`;
  `acdd validate docs/milestones/AR-001-borrowed-data-and-composable-execution.md --format json`.
  Strict Clippy proves every registered stub compiles. Parser sensitivity: a
  disposable in-memory manifest fixture with a local dependency cycle must fail
  parsing; never corrupt the committed authority to demonstrate this.
- Incoming seams: current manifest parser, profile and suite indexes.
- Outgoing seams: compiling registered files and the frozen matrix consumed by
  all Wave 1 tasks; stubs carry no fake passing assertions or ignored tests.
- Negative boundaries: no production edit, ADR change, fixture framework,
  generated definitions, new error enum, or post-bootstrap index ownership.

### task: compose-surface-dispatch

```yaml
task_ref: compose-surface-dispatch
target: "Compose existing MCP/CLI dispatch and finalization with borrowed request policy, single loaded Adapter reuse, moved CLI results, and in-place bounded response compaction, proving unchanged validation order, output/debug contracts and bounded continuations"
proof_policy: direct-proof
contract_revision: 2
depends_on:
  - freeze-ownership-and-seams
scope:
  - src/mcp/tools.rs
  - src/mcp/response.rs
  - src/mcp/server.rs
  - src/cli/mod.rs
  - src/cli/ast.rs
  - tests/mcp/borrowed_dispatch.rs
  - docs/reference/mcp-tools.md
  - docs/reference/cli.md
  - docs/reference/mcp-markdown-contracts.md
symbols: []
status: planned
subtasks:
  - subtask_ref: retain-request-policy-owner
    title: "Retain one selected request adapter, thread &Adapter through read/AST handlers, and borrow final response policy while preserving rejection precedence"
  - subtask_ref: consume-bounded-cli-and-response-payloads
    title: "Move bounded CLI result payloads when debug is disabled and compact owned payloads in place inside result_for_presented without unconditional initial cloning"
  - subtask_ref: prove-surface-composition
    title: "Exercise CLI/MCP success, short-circuit errors, single-load adapter reuse, debug output, paging and response limits and align affected presentation references"
```

Blueprint slice:

- Implementation steps: keep `call_tool` as protocol boundary and the canonical
  router as sole dispatch owner; extract only cohesive private stage helpers;
  retain existing `FnOnce` handler composition; borrow policy and `&scanner::Adapter`
  from the retained request adapter into `read_with_adapter` and `ast::search_paged`
  so queries do not re-read config on disk; move CLI `Value` at its last use when
  debug is disabled; and compact `Value` in place inside `result_for_presented`
  when fitting or trimming rather than cloning upfront when unnecessary. Preserve
  debug log payload and timing, guide-init recovery, source-only reads, validation
  exit and broken-pipe success.
- Symbol signatures: all public APIs keep their existing signatures and module
  exports; private/internal adapter-passing helpers are confined to the scoped files.
- Test seam: proposed `tests/mcp/borrowed_dispatch.rs::surface_dispatch_preserves_results_and_rejections`,
  run with `cargo test --test mcp borrowed_dispatch`; also run existing
  `cargo test --test mcp cli_contract`, `cargo test --test mcp test_read`, and
  `cargo test --test mcp tool_evolution` for bounds and paging.
  Keep distinct debug and continuation workflows in separate named tests.
- Sensitivity: temporarily bypass final byte enforcement, select the primary
  instead of requested worktree, and reorder format versus unsupported-field
  rejection; the corresponding scenario must fail for each mutation. Assert
  continuation generation/offset and retained source digest, Unicode boundaries,
  JSON/Markdown outcomes, single-load adapter consistency, and debug payload
  readback. Ownership source review must show removal of unconditional CLI/response
  clones and redundant per-query `scanner::load_adapter` calls; remaining `Arc`,
  debug, or continuation snapshot copies need explicit reasons.
- Incoming seams: parsed CLI/RMCP input, selected adapter, immutable invocation
  settings, canonical handler result, current presentation/response contracts.
- Outgoing seams: unchanged engine calls and bounded renderer invocation; writer
  output and MCP response envelope. No cross-task production interface is added.
- Negative boundaries: do not change `Server` admission/generation semantics,
  tool schemas, command syntax, continuation contract fields, or logging
  infrastructure. Reconcile reference pages with live behavior; do not describe
  the refactor as new syntax.

### task: move-cold-extraction-inputs

```yaml
task_ref: move-cold-extraction-inputs
target: "Move scanner-owned candidates into checked extraction results, compose worker preparation without redundant candidate strings, and guarantee deterministic SQLite WAL checkpointing plus regular-file sidecar cleanup during atomic publication"
proof_policy: direct-proof
contract_revision: 2
depends_on:
  - freeze-ownership-and-seams
scope:
  - src/engine/pipeline.rs
  - tests/core/borrowed_pipeline.rs
  - tests/core/database_guard.rs
  - docs/architecture/indexing.md
  - docs/architecture/performance.md
symbols: []
status: planned
subtasks:
  - subtask_ref: consume-checked-candidate
    title: "Let private extraction consume BuildCandidate after metadata and cache-key checks, moving path/language/digest into the existing FileEntry"
  - subtask_ref: preserve-worker-stage-ownership-and-wal-cleanup
    title: "Compose worker preparation with explicit MemoryPermit ownership and ensure atomic_build checkpoints WAL to DELETE mode and cleans regular-file sidecars under exclusive_lock"
  - subtask_ref: prove-index-equivalence-and-cleanup
    title: "Verify warm/cold extraction, source rejection, failed-consumer cleanup, read-only WAL outliving-reader sidecar recovery, cold/delta parity, and update architecture descriptions"
```

Blueprint slice:

- Implementation steps: destructure `PipelineWork` without cloning its candidate;
  preserve pre/post-read metadata and size checks and all cache-key fields;
  compute cache inputs before consuming candidate fields; reuse existing
  `BuildCandidate::into_entry` unchanged; compose fact/storage preparation using
  local functions/`Result::and_then` where useful. In `atomic_build`, verify
  pre-existing destination sidecars (`ensure_no_publication_sidecars(&output, true)`),
  run `checkpoint(&connection, true)`, execute `PRAGMA journal_mode=DELETE;`, drop
  the connection, remove regular-file `-wal`/`-shm`/`-journal` sidecars under
  `exclusive_lock`, and re-verify `ensure_no_publication_sidecars(&output, false)` so
  outliving read-only connections on a replaced WAL database cannot strand
  sidecars. Preserve symlink/FIFO rejection on all paths.
- Symbol signatures: private `extract_candidate` changes from borrowed to owned
  `BuildCandidate`; its `extraction_worker` caller changes together. Public build,
  delta, scanner and cache signatures remain unchanged.
- Test seam: proposed `tests/core/borrowed_pipeline.rs::cold_pipeline_preserves_cache_and_published_facts`,
  run with `cargo test --test core borrowed_pipeline`; separate
  `cold_pipeline_failure_preserves_published_snapshot` covers real consumer
  failure and cleanup; `tests/core/database_guard.rs::output_sidecar_hazards_preserve_generation`
  covers outliving WAL read-only connection recovery and symlink/FIFO rejection.
  Also run `cargo test --test incremental` and `cargo test --test commitment_integrity`.
- Sensitivity: bypass candidate metadata rejection for a stale-candidate
  scenario; bypass consumer cancellation for the failure scenario; omit the
  `PRAGMA journal_mode=DELETE;` / regular-file sidecar unlink when a read-only
  connection outlives writer close; mutate an output fact for cold/delta commitment
  comparison. Tests must detect the respective rejection, sidecar recovery,
  cleanup, or published-root mismatch. Use existing hooks and fixtures, not sleeps.
- Incoming seams: scanner candidate, permit, shared registry/cache, source-buffer
  lifetime, destination database lock. Outgoing seams: existing `PipelineItem`,
  compressed payloads/catalog, consumer/spool, clean DELETE-mode published index.
- Negative boundaries: no scanner, DB writer, cache-format, linker or publication
  algorithm edit outside `src/engine/pipeline.rs`; no new per-file full-facts
  copy or nested parallel iterator; no symlink unlinking on sidecar cleanup.

### task: borrow-task-execution-context

```yaml
task_ref: borrow-task-execution-context
target: "Borrow selected role configuration through task gate preparation/execution and use local ordered stage composition while preserving canonical claims, rejection reuse, observed command proof, process failure handling and durable receipt acceptance"
proof_policy: direct-proof
contract_revision: 2
depends_on:
  - freeze-ownership-and-seams
scope:
  - crates/acdd-tasks/src/engine/orchestrator/task_runner.rs
  - tests/acdd/orchestrator/lifecycle/borrowed_execution.rs
  - docs/reference/acdd.md
symbols: []
status: planned
subtasks:
  - subtask_ref: borrow-selected-role
    title: "Retain the owning profile and borrow its selected role/role ID instead of cloning a full role into the runner tuple"
  - subtask_ref: compose-gate-execution
    title: "Separate preparation and execution with private typed functions where they remove repeated logic while retaining exact claim/context/submit ordering"
  - subtask_ref: prove-authoritative-completion
    title: "Exercise profile-driven mechanical/role execution, retained rejection sessions, restart and failure rejection and document unchanged execution authority"
```

Blueprint slice:

- Implementation steps: keep the profile alive through each iteration; borrow
  its selected role until constructing the existing owned invocation; retain
  required ownership in `Claim`, `RunnerInvocationContext`, and events. Keep
  `RunnerConfig::from_role`, context reconstruction, prompt validation, spawner,
  monitor and zero-argument submit as canonical collaborators. Use local typed
  helpers/closures only where the resulting order stays explicit; do not force
  mechanical and role branches through a new generic runtime.
- Symbol signatures: `run_task_lifecycle` and all exported DTO signatures stay
  unchanged. Only private helpers in this source file may be introduced.
- Test seam: proposed `tests/acdd/orchestrator/lifecycle/borrowed_execution.rs::task_dispatch_preserves_profile_authority`,
  run with `cargo test --test acdd acdd::orchestrator::lifecycle::borrowed_execution`;
  also run `cargo test --test acdd acdd::orchestrator::lifecycle` and
  `cargo test -p acdd-tasks --lib`. Reuse the canonical lifecycle fixtures and
  existing harness; add no second factory. Keep genuine process tests only for
  the runner-process boundary and preserve their platform constraints.
- Sensitivity: treat a zero-exit runner with no gate submission as success,
  replace the resumed builder session with a fresh session, and replay an
  accepted gate after restart. Separate scenarios must detect each mutation
  through persisted gate/claim/receipt and event readback. Cover mechanical
  gates without role spawning, active foreign claim refusal, timeout/failure,
  attempts exhaustion and incomplete-context rejection through the existing
  scoped lifecycle suite. Source/lifetime review proves the selected full role
  clone is gone; owned event and process records remain valid after local owners
  drop. No wall-clock or total allocation ceiling is inferred.
- Incoming seams: registry/worktree selection, persisted gate and profile,
  invocation options, genuine session and complete context. Outgoing seams:
  unchanged claim/submit APIs, role process, ordered observer events and durable
  accepted task state consumed by wave dispatch and CLI task run.
- Negative boundaries: no SQL/schema/store transaction edits, profile or stage
  semantics change, runner command/template change, new retry/session policy,
  prompt truncation, or synthetic proof/receipt. Keep store drop/reopen boundaries
  around subprocess execution and canonical submission unchanged.

### task: deduplicate-cross-crate-config-and-locks

```yaml
task_ref: deduplicate-cross-crate-config-and-locks
target: "Eliminate duplicated adapter/config models across acdd-core and acdd-tasks and centralize bounded-retry advisory file locking in acdd-core::fs without modifying Cargo.toml or public signatures"
proof_policy: direct-proof
contract_revision: 1
depends_on:
  - freeze-ownership-and-seams
scope:
  - crates/acdd-core/src/config.rs
  - crates/acdd-core/src/fs.rs
  - crates/acdd-tasks/src/config.rs
  - crates/acdd-tasks/src/fs.rs
  - crates/acdd-index/src/db/cache.rs
  - docs/architecture/modularity.md
  - tests/core/cross_crate_deduplication.rs
symbols: []
status: planned
subtasks:
  - subtask_ref: share-adapter-config-owner
    title: "Delegate/re-export AdapterFile, Adapter, LinkedWorkspace, memory-budget helpers, and load_adapter in crates/acdd-tasks/src/config.rs to acdd_core::config"
  - subtask_ref: centralize-advisory-file-lock
    title: "Implement bounded-retry shared/exclusive advisory file locking in crates/acdd-core/src/fs.rs using std File::try_lock methods and reuse it in acdd-tasks::fs and acdd-index::db::cache"
  - subtask_ref: prove-config-and-lock-parity
    title: "Verify config parsing/limit parity, O_NOFOLLOW/regular-file lock safety, lock contention timeouts, and document updated cross-crate reuse"
```

Blueprint slice:

- Implementation steps: audit `crates/acdd-tasks/src/config.rs` against
  `crates/acdd-core/src/config.rs`; replace duplicated structs/helpers
  (`AdapterFile`, `Adapter`, `LinkedWorkspaceConfig`, `LinkedWorkspace`,
  `load_adapter`, `resolve_memory_budget_bytes`, `memory_budget_mb_from_sources`,
  `bytes_from_mebibytes`, `check_root_scope`, `policy`, `persisted_adapter_path`,
  `workspace_path`, `local_template_target`) with delegation/re-exports from
  `acdd_core::config` (if `acdd-tasks` exposes fewer fields on `AdapterFile` or
  adds `load_workspace_adapter` / `read_policy`, wrap or preserve exact field/API
  compatibility without breaking `acdd-tasks` tests). In `crates/acdd-core/src/fs.rs`,
  add shared advisory lock helper(s) using `std::fs::File::try_lock` /
  `try_lock_shared` (available natively on Rust `1.100.0`) with existing
  `O_NOFOLLOW`/regular-file validation and backoff/timeout behavior, then delegate
  `crates/acdd-tasks/src/fs.rs::exclusive_lock` and
  `crates/acdd-index/src/db/cache.rs::generation_lock` (plus `shared_lock` /
  `exclusive_lock`) to it.
- Symbol signatures: preserve every existing public signature and field/re-export
  contract in `acdd_core`, `acdd_tasks`, and `acdd_index`; add only strictly
  additive public/crate lock helper(s) in `acdd_core::fs` if needed without
  breaking any caller. No `Cargo.toml` edit or `libc` addition in `acdd-core`.
- Test seam: proposed `tests/core/cross_crate_deduplication.rs::shared_config_and_locks_preserve_semantics`,
  run with `cargo test --test core cross_crate_deduplication`; also run
  `cargo test -p acdd-core --lib`, `cargo test -p acdd-tasks --lib`, and
  `cargo test --test core database_guard`.
- Sensitivity: invert exclusive vs shared lock conflict handling or omit symlink
  rejection on a lock file path; the deduplicated lock seam test must fail cleanly.
  Verify `acdd_tasks::config::load_adapter` and `acdd_core::config::load_adapter`
  produce identical limits, ignored names, and digests on identical fixtures.
- Incoming seams: existing `acdd-core`, `acdd-tasks`, and `acdd-index` crate
  dependency edges (`Cargo.toml` already lists `acdd-core` in both dependent crates).
- Outgoing seams: unchanged public APIs used by `src/cli/`, `src/mcp/`,
  `src/engine/`, and task lifecycle store.
- Negative boundaries: no `Cargo.toml` or `libc` dependency edits; no breaking
  field visibility removals in `acdd_tasks::config`; no removal of symlink or
  non-regular-file rejection on lock files; no unbounded blocking lock waits.

### task: unify-catalog-storage-and-extractor-port

```yaml
task_ref: unify-catalog-storage-and-extractor-port
target: "Eliminate duplicated catalog-row assembly between prepare_catalog_storage_with_facts and prepare_catalog_rows in db/writer.rs and collapse repetitive ensure_extractors() boilerplate in src/engine/tasks.rs"
proof_policy: direct-proof
contract_revision: 1
depends_on:
  - freeze-ownership-and-seams
scope:
  - crates/acdd-index/src/db/writer.rs
  - src/engine/tasks.rs
  - docs/architecture/README.md
  - tests/core/writer_storage_unification.rs
symbols: []
status: planned
subtasks:
  - subtask_ref: unify-catalog-storage-preparation
    title: "Refactor prepare_catalog_storage_with_facts to delegate row construction to prepare_catalog_rows plus encoding without altering encoded bytes"
  - subtask_ref: unify-extractor-port-wrapper
    title: "Replace repetitive ensure_extractors() entry wrappers in src/engine/tasks.rs with a single typed helper while preserving every public signature"
  - subtask_ref: prove-catalog-and-extractor-parity
    title: "Verify identical prepared storage bytes, populate_owned ingestion, extractor one-time registration across task ports, and update architecture docs"
```

Blueprint slice:

- Implementation steps: in `crates/acdd-index/src/db/writer.rs`, make
  `prepare_catalog_storage_with_facts` construct `PreparedCatalogStorage` via
  `prepare_catalog_rows` and serialize that single canonical representation rather
  than duplicating node/edge/doc encoding loops. In `src/engine/tasks.rs`, keep
  `INIT_EXTRACTOR: Once` and `ensure_extractors()`, and route the 10 exported task
  wrappers (`claim`, `submit`, `submit_zero_arg`, `verify_task`,
  `verify_task_with_options`, `submit_zero_arg_with_timeout`, `manage`, `list`,
  `reopen`, `reset`) through one private generic/closure helper (`with_extractors`)
  so extractor registration cannot accidentally drift on a subset of task APIs.
- Symbol signatures: all public functions in `crates/acdd-index/src/db/writer.rs`
  and `src/engine/tasks.rs` keep exact names, parameters, and return types.
- Test seam: proposed `tests/core/writer_storage_unification.rs::unified_catalog_storage_and_extractor_port_match_behavior`,
  run with `cargo test --test core writer_storage_unification`; also run
  `cargo test --test core` and `cargo test --test incremental`.
- Sensitivity: skip encoding doc sections or facts in `prepare_catalog_rows`, or
  omit `ensure_extractors()` on one task port; seam tests must catch the encoded
  payload divergence or missing extractor registration.
- Incoming seams: `FileEntry`, `TypedFacts`, `engine::tasks` public calls.
- Outgoing seams: `PreparedCatalogStorage` / binary payload consumed by
  `populate_owned`; task store actions consumed by CLI/MCP routers.
- Negative boundaries: no change to serialized storage format versions, Zstd
  compression levels, `Once` initialization guarantees, or database schema.

## Verification and delivery

Before each task Build, load `code-quality` and `test-contract-seam`, the relevant
owner instructions, and returned workflow guidance. For automated contract/build
gates use the claim-bound `task verify` command with the named seam, followed by
the normal stage submission. Run the scoped compatibility tests and strict
`cargo clippy --all-targets --all-features -- -D warnings`; record actual command
and test counts. Run new scenarios separately and parameterize only input
variations of the same seam. Do not duplicate or reorganize unrelated suites.

Acceptance requires all five concrete ownership/deduplication improvements plus
verified WAL/SHM sidecar publication cleanup, passing production compatibility and
sensitivity evidence, disjoint Wave 1 scopes, no unadmitted public API or
protected-file changes, and documentation matching the final code. An optimization
that cannot preserve its boundary is returned to the architect with an explicit
authority gap; do not silently broaden scope or substitute a speculative change.

At milestone handoff the coordinator launches the required documentation agent
with the final manifest, accepted evidence and diff against the explicit target
base. Its admitted documentation scope is this manifest and the reference/architecture
pages named in the implementation tasks. Submit resulting edits for independent
review; repeat affected scans after later implementation changes. Follow
[TESTS.md](../../TESTS.md#milestone-handoff-gate): commitment integrity, strict
Clippy, `cargo test --all-targets`, and the separate `cargo test -p acdd-tasks --lib`.
Preserve trace-bound counts and report the member-library count separately. Bind
independent integration review and engine verification to the final content and
selected target base before archival. This authoring gate does not claim or
submit tasks, initialize implementation, commit, publish, run benchmarks, or
perform milestone handoff.
