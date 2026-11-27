---
title: "Plans"
doc_type: guide
---

# Plans

The `docs/plans/` directory holds design documents, architectural proposals, research notes, and pre-implementation drafts.

## Purpose

- Technical designs and exploratory subsystem proposals originate here as plans (`doc_type: plan`, executed via `acdd start <path> --in-place` when single-checkout isolation is desired, or in isolated worktrees under `.acdd/workflows/default.yaml` / `code` by default).
- Once accepted and admitted into execution, commitments are tracked via `docs/milestones/`.
- Architectural invariants migrate to `docs/architecture/` and decisions to `docs/adr/`.
- Active plans can be validated with `acdd validate <path>` and started as runs with `acdd start <path>` (or `acdd run`). Inspect active and closed runs via `acdd list`.
