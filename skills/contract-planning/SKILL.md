---
name: contract-planning
description: Build, audit, or amend implementation contracts, tasks, and milestone plans. Use to evaluate architectural viability, reconcile ADRs, uncover hidden risks or ambiguities, and constructively propose cleaner designs.
---

# Contract Planning and Architectural Audit

Specify the goal, non-goals, ambiguities, and numbered testable `AC-*`
behavioral scenarios before design. Separate **WHAT / WHY** (spec contract,
invariants, observable behavior, scope, negative boundaries) from **HOW**
(architecture/ADRs and `blueprint_synthesis` `pre_task_slice`), map criteria to
bounded task slices and proof seams, and audit against durable repository truth.
Use before creating, reopening, materially amending, or admitting an
implementation task or milestone plan, or whenever review shows an omitted owner,
consumer, failure mode, or proof obligation.

Before a review repair, compare unresolved findings against the frozen contract
and limit edits to those findings; do not invent uncontracted scope from a
generic summary.

## 1. Load Repository Context

1. Read root `AGENTS.md`, `architecture`/`decisions` docs, and `.agents/skills/contract-planning/ADDON.md`
   if the consuming repository defines one. An add-on may add local authority
   paths, invariants, or stricter checks; it never weakens this procedure.
2. Separate durable system truth (`docs/architecture/`, `docs/reference/`,
   accepted ADRs) from active change specs (`docs/milestones/`, `docs/plans/`);
   ground every spec or blueprint edit as a delta against that truth and admit
   affected doc/ADR updates into scope.
3. Read the active task, parent plan/phase, prerequisites, and successors before
   drafting or amending any slice.

## 2. Audit Viability & Simplicity (Occam's Razor)

1. **Minimize moving parts**: Verify the design uses existing capabilities,
   canonical owners, declarative config, or existing dependencies rather than
   bespoke wrappers, shadow registries, parallel lookup paths, or speculative
   abstractions.
2. **Enforce tier placement**: Place responsibilities in their authoritative
   layer (leaf core vs domain/index vs state orchestration vs adapter/UI) with
   zero circular coupling or abstraction leaks.

## 3. Reconcile ADRs & Invariants

1. Reconcile against existing ADRs, architecture boundaries, and active `INV-*`
   invariants rather than silently reversing past trade-offs.
2. When an unresolved architectural conflict or trade-off remains, record an
   explicit `AUTHORITY_GAP` decision candidate rather than burying assumptions in
   implementation tasks.

## 4. Probe Ambiguities, Risks & Testable Scenarios

1. **Behavioral `AC-*` criteria (`Trigger / Precondition -> Action -> Outcome`)**:
   Replace vague capability wishes with observable, falsifiable behavioral
   scenarios naming inputs/conditions, action, and expected result or fail-closed
   `ErrorCode`/rejection. Keep internal implementation choices (`HOW`) out of spec
   task blocks unless they are external compatibility or boundary contracts.
2. **Operational & platform bounds**: Check algorithmic complexity, I/O
   amplification, concurrency/lock ordering, byte limits, and OS path/symlink
   edge cases.
3. **Failure & recovery semantics**: Trace partial writes, interrupted runs, and
   restarts; require fail-closed diagnostics, clear error codes, and idempotent
   recovery.

## 5. Negative Boundaries, Wave Disjointness & Simplifications

Never stop at listing flaws: propose a cleaner alternative reusing proven
patterns, record explicit **`negative_boundaries`** (forbidden shortcuts,
duplicate parsers/registries, uncontracted dependencies, off-limits
`protected_paths`), set realistic `max_net_loc` budgets, and enforce strictly
disjoint write `scope:` globs within every concurrent DAG wave.

## 6. Map Consumers & Freeze Execution Paths

Search the planning scope for touched types, interfaces, config keys, callers,
and consumers. Read every live producer and consumer before selecting an owner,
naming current callers separately from planned/deferred consumers.

Trace each affected surface end-to-end before slicing tasks:

```text
trigger -> production caller -> typed contract -> canonical owner -> storage/runtime propagation -> consumer -> observable result or rejection
```

Record owner, inputs/outputs, authority, durable state, config/defaults,
validation/bounds, concurrency/retry/cleanup rules, fail-closed rejections,
forbidden effects, proof seam, and deferred destination (changed, proven
unchanged, deferred, or N/A).

## 7. Specify Proportional Verification Before Build

Bind every `AC-*` claim to its narrowest real production seam (`test-suite-rules`)
and name a deliberate contract-breaking mutation it rejects. Keep hermetic and
live integration lanes distinct.

Whenever a contract promises latency, throughput, size, or query-count bounds,
specify the exact production operation, warmup policy, fixture cardinality,
measurement interval, sample method, target bound, and regression mutation.
Microbenchmarks on isolated helpers do not prove caller-path budgets.

## 8. Three-Axis Spec & Blueprint Audit (Completeness, Correctness, Coherence)

Audit every new or amended spec and `blueprint_synthesis` slice along three axes:

1. **Completeness**: Every goal and `AC-*` maps 1:1 to bounded tasks/subtasks,
   `verification_seam`s, and co-evolved documentation/ADR/roadmap scope without
   unmapped requirements.
2. **Correctness**: `AC-*` scenarios and seams verify happy paths, boundary
   conditions, concurrency/confinement guards, and fail-closed rejections rather
   than permissive fallbacks.
3. **Coherence**: Changes reconcile cleanly with accepted ADRs, active `INV-*`
   invariants, `INV-SINGLE-OWNER` canonical owners, crate DAG layers, and
   disjoint same-wave write scopes.
