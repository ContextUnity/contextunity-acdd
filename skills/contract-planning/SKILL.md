---
name: contract-planning
description: Build, audit, or amend implementation contracts, tasks, and milestone plans. Use to evaluate architectural viability, reconcile ADRs, uncover hidden risks or ambiguities, and constructively propose cleaner designs.
---

# Contract Planning and Architectural Audit

Specify the goal, non-goals, ambiguities, and numbered `AC-*` criteria before
design. Map criteria to bounded task slices and proof seams. Use before
creating, reopening, materially amending, or admitting an implementation task
or milestone plan, or whenever review shows an omitted owner, consumer, failure
mode, or proof obligation.

Before a review repair, compare unresolved findings against the frozen contract
and limit edits to those findings; do not invent uncontracted scope from a
generic summary.

## 1. Load Repository Context

1. Read root `AGENTS.md`, `architecture`/`decisions` docs, and `.agents/skills/contract-planning/ADDON.md`
   if the consuming repository defines one. An add-on may add local authority
   paths, invariants, or stricter checks; it never weakens this procedure.
2. Read the active task, parent plan/phase, prerequisites, and successors before
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

## 4. Probe Ambiguities, Risks & Recovery

1. **Eliminate ambiguity**: Replace vague wording with observable, testable
   acceptance criteria.
2. **Operational & platform bounds**: Check algorithmic complexity, I/O
   amplification, concurrency/lock ordering, byte limits, and OS path/symlink
   edge cases.
3. **Failure & recovery semantics**: Trace partial writes, interrupted runs, and
   restarts; require fail-closed diagnostics, clear error codes, and idempotent
   recovery.

## 5. Synthesize Constructive Simplifications

Never stop at listing flaws: propose a cleaner alternative reusing proven
patterns, and record explicit **negative boundaries** (traps, forbidden
shortcuts, and paths not to touch) alongside realistic diff budgets so builders
stay on the simplest viable seam.

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
