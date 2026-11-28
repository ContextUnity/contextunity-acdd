---
name: close-review-findings
description: Diagnose and fix any defect, and classify, remediate, or evidence-close findings from reviews, audits, or repeated review rounds. Use for every defect fix and when analyzing findings, verifying remediation completeness, or preventing recurring defects.
---

# Close Review Findings

A finding ID going quiet is not closure. Closure restores one canonical-owner
invariant across every equivalent mutation, caller, backend, and persistence
seam without leaving redundant abstractions, unverified paths, or bug-fix test
bloat.

Read the nearest repository `AGENTS.md`, load `review-authority` before
classifying or editing findings, and load `.agents/skills/close-review-findings/ADDON.md`
if present (add-ons may require extra evidence, never weaken this procedure).
Load `test-suite-rules` before modifying tests.

## 1. Establish Authority & Scope

Higher authority governs; review comments never override higher layers:

1. Explicit user decision in the active session
2. Frozen Contract (admitted task acceptance, scope, and forbidden effects)
3. Named owning or deferred plans holding a separate cutover boundary
4. Review findings and proposed fixes

Do not treat reviewer wording, claimed causation, or proposed patches as proof.
Preserve raw/unparsed outputs as open evidence, require terminal status before
final reporting, and never rename/delete a contracted surface without explicit
contract/user authority.

## 2. Attribute Origins & Map the Owner Path

Compare the reviewed snapshot, pre-fix state, and current code to assign one
origin per finding (`introduced-regression`, `pre-existing-missed`,
`incomplete-remediation`, `duplicate`, `invalid`, or `plan-deferred`).

For each non-duplicate finding, trace the full owner path before editing:

`reachable scenario -> canonical owner -> typed internal model -> persisted/runtime propagation -> consumers -> fail-closed rejection -> surviving mutation checks -> live call sites`

Stop and escalate for an explicit design decision if the governing contract does
not fix ownership, wire/persisted schemas, concurrency, or compatibility
semantics. Do not stop at local symptom patches when a second boundary or
consumer stays unprotected.

## 3. Verify Reachability & Graph Hygiene Structurally

1. Search the canonical owner and enumerate all syntactic variants (constructors,
   methods, adapters, composition roots, entrypoints) with validated
   structural/AST queries rather than ad-hoc string assumptions.
2. Check blast radius (`impact`) across callers before editing, and treat stale
   or empty index coverage as a missing-evidence gap until refreshed.
3. After edits, verify zero newly introduced clones (`duplicates`), unreachable
   `phantom_tested` helpers, `dead_tails`, or `dangling_calls`.

## 4. Cluster by Owner & Invariant

Group confirmed findings by **canonical owner + post-fix invariant**, fixing one
coherent cluster at a time rather than queueing N disconnected patches.

Check adjacent boundary dimensions for every touched owner:
- alternate callers, adapters, compiled vs source execution;
- valid, missing, malformed, legacy, and exact-limit inputs (enforcing byte
  bounds before serialization/canonicalization, not only at ingress);
- separate permissive ingress DTOs from normalized internal types consumed by
  application ports;
- transactions, crash/restart recovery, cleanup failure preservation (never
  discarding primary errors or converting partial failure into success);
- filesystem confinement, symlinks, cross-device/shared-VCS paths, and TOCTOU;
- isolation, cache, and visibility seams: sweep **all** equivalent aggregation
  points, fallbacks, cache keys (including active config, schema, or scope
  discriminators), and derived views behind one canonical owner instead of
  patching only the reported consumer;
- concurrency, revision/CAS fencing, and authorization changes;
- released vs uncommitted formats (squash unshipped dev schemas instead of adding
  legacy shims).

Reject reviewer suggestions that fight higher authority, weaken fail-closed
checks, mask failures, or invent speculative layers.

## 5. Generalize Mechanism Without Test Bloat

Eliminate the defect class in production code (consolidate duplicate branches into
one shared validator or delegate to canonical backend semantics). **Do not spawn
standalone bug-fix tests or extra heavy harnesses per finding**; keep
remediation focused on mechanism generalization, existing seam suites, and
mutation verification. Add at most one data row to an existing parameterized
contract matrix only if a genuine external boundary was unrepresented (failing
before the fix, green after).

Use real state boundaries matching each claim (real DB transactions, concurrent
processes for races, real process kills for restarts, interleaved mutations for
TOCTOU, malformed durable bytes for fail-closed readers) rather than in-process
mocks.

## 6. Close Verification & Prevent Review-Loop Clinics

Before submitting a fix or scheduling repeat review:
1. Verify every finding has an evidence-backed origin and disposition
   (`confirmed-fixed`, `dismissed-with-evidence`, `duplicate-of`,
   `plan-deferred-with-guard`, or `unresolved`).
2. Re-run structural residual scans, `duplicates`/`dead_tails`, diff impact, one
   deliberate mutation check per invariant, a **live end-to-end smoke check**
   through the real entrypoint against live runtime state or a stateful fixture
   reproducing the reported path, and all repository verification commands on a
   clean, committed state (`dirty`/partial runs keep closure open).
3. **Hard stop on recurring clinics**: On repeated (`>= 2`) multi-boundary
   rejections, or if a follow-up round mostly uncovers `incomplete-remediation`
   on the same owners or lacks index coverage or mutation proof, stop patching
   incrementally—escalate for an architectural or capacity decision and report
   the open clusters and missing seam evidence cleanly.
