---
id: PLAN-000-example-feature-plan
title: Example In-Place Feature Plan
doc_type: plan
status: planned
spec: 'Demonstrate an operational in-place plan executed through the unified acdd-state Run engine using `--in-place` (candidate TreeOid verification, zero auto-commits).'
depends_on: []
owners:
  - crates/acdd-core/src/
  - crates/acdd-state/src/
invariants:
  - 'INV-EXAMPLE-IN-PLACE-NONE: In-place plan runs persist progress via scoped candidate TreeOid verification and SQLite receipts without committing directly to the active branch.'
acceptance_criteria:
  - 'AC-1: Plan validates under acdd validate with zero errors.'
---

# Example In-Place Feature Plan

This plan demonstrates an in-place `SpecDocument` executed on the unified `crates/acdd-state` engine under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md).

## Execution Characteristics

- **In-Place**: Executed via `acdd start docs/plans/example-feature-plan.md --in-place` (or with a `git.isolation: in_place` workflow).
- **Non-Committing Persistence**: In-place execution sets `git.isolation: in_place`, `git.worktrees: manual`, `save: none`, and neutralizes gate `save.post: commit` to `none` while preserving `save.immutable: true` (never auto-commits to Git).
- **Archiving**: Upon completion at the terminal `handoff` gate (`archive: true, prune: true`), the run moves the closed document to `docs/plans/archive/`.

---

## Tasks in this plan

### task: core-type-extraction

```yaml
task_ref: core-type-extraction
target: "Extract and decouple shared value structures in acdd-core."
scope:
  - crates/acdd-core/src/model.rs
depends_on: []
tags:
  - default
contract_revision: 1
status: planned
subtasks:
  - subtask_ref: define-dto-model
    title: "Define clean DTO model"
    status: pending
  - subtask_ref: wire-serde-serialization
    title: "Wire serde serialization"
    status: pending
```

### task: state-engine-integration

```yaml
task_ref: state-engine-integration
target: "Wire the extracted types into the unified acdd-state pipeline."
scope:
  - crates/acdd-state/src/usecases/
depends_on:
  - core-type-extraction
tags:
  - default
contract_revision: 1
status: planned
subtasks:
  - subtask_ref: replace-legacy-references
    title: "Replace legacy references"
    status: pending
  - subtask_ref: verify-unit-tests
    title: "Verify unit tests pass"
    status: pending
```
