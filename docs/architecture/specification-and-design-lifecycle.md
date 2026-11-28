---
title: "Specification, Design, and Verification Lifecycle"
doc_type: architecture
---

# Specification, Design, and Verification Lifecycle

ContextUnity ACDD separates problem definition (**Specify / Clarify**), architectural framing (**Design**), and automated proof (**Execution & Verification**), sizing ceremony to the scope of change (**S / M / L**) while keeping low-level component and code diagrams grounded in the live indexed AST graph.

> [!NOTE]
> Runtime boundaries are documented in the [architecture index](README.md), [ACDD execution reference](../reference/acdd.md), and [ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md). Universal `acdd.yaml` `specs:` collections (built-in `contract` and `memory` schemas plus external `.acdd/specs/*.yaml` schemas), `0..N` attached specs per run, `spec_deltas` (`spec.deltas_applied`), external `.acdd/lint/*.yaml` rules, the `base`, `code`, and `documentation` embedded workflows (plus copyable `audit`, `spec_planning`, and `hotfix` templates in `skills/acdd/references/acdd-workflow-examples/`), blueprint properties `ac_mapping` and `c2_boundary_impact`, optional `spec` and `acceptance_criteria` projection, `crates/acdd-spec` parsing/validation, and unified `crates/acdd-state` `Run` execution support this lifecycle.

## Specify / Clarify before implementation

Ambiguous requirements cause rework faster than syntax bugs. Before choosing code edits or task boundaries, specification artifacts establish three explicit contracts:

1. **Goal**: The concrete outcome or capability being introduced or changed.
2. **Non-goals**: Adjacent behaviors, integrations, or optimizations explicitly excluded from scope.
3. **Ambiguities and Acceptance Criteria (`AC-*`)**: Edge cases, failure semantics, security boundaries, and testable numbered assertions (`AC-1`, `AC-2`, ...) against which reviewers and seam tests verify completion.

Product requirements, domain specifications, and repository memories (declared under `acdd.yaml` `specs:` — such as `docs/specs/*.md`, `docs/milestones/*.md`, `docs/plans/*.md`, or `docs/memories/*.md` — and optionally attached `0..N` per run or mutated via `spec_deltas`) remain concise behavioral authorities rather than implementation walkthroughs.

## Design artifacts: SAD, ADRs, and live C4

ACDD treats architectural documentation as a multi-resolution hierarchy where humans author high-level boundaries and the indexer maintains structural truth:

| Layer | Authoring Owner | Purpose in ACDD |
| :--- | :--- | :--- |
| **PRD / Spec (`AC-*`)** | Product / Architect | Defines `Goal`, `Non-goals`, edge cases, and testable acceptance criteria. |
| **C4 Level 1 & 2 + SAD** | Architect (`docs/architecture/`) | Captures external actors, system context (`C1`), container/crate boundaries (`C2`), data flows, and `INV-*` invariants. |
| **ADRs** | Architecture Owner (`docs/adr/`) | Records durable present-tense architectural trade-offs and superseded boundaries ([ADR index](../adr/README.md)). |
| **C4 Level 3 & 4** | Source index (`acdd-index`) | Derived from AST parsing and symbol linking (`code_map_overview`, `code_map_inspect`, `code_map_impact`, `code_map_query`); refresh the index after source changes. |

Static Markdown documentation stops at **C4 Level 1–2** and invariant definitions (`INV-*`). Detailed call graphs, dependency edges, and symbol signatures come directly from the SQLite code map (`governs` and `cites_doc` edges linking documentation claims to uniquely resolved declarations).

## S / M / L right-sizing cadence

Every initiative maps to the lightest ACDD `WorkflowDef` (`git.isolation` and `save:` policy) defined in [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md) that preserves verification integrity:

```mermaid
flowchart LR
  Spec["Specify / Clarify<br/>(Goal, Non-goals, AC-*)"] --> Triage{"Scope & Risk<br/>(S / M / L)"}
  Triage -->|"S: Ephemeral / standalone check"| WF["Ephemeral or Root-Only Run<br/>(--goal / workflow, save: none)"]
  Triage -->|"M: Multi-step in-place refactor/feature"| PL["In-Place Plan Run<br/>(--in-place, save: none)"]
  Triage -->|"L: Architectural / multi-agent subsystem"| MS["Worktree Milestone Run<br/>(blueprint_synthesis + worktrees + commit/archive)"]
  WF --> Verify["Live Verification<br/>(Commands + Independent Review)"]
  PL --> Verify
  MS --> Graph["Design & Seam Gate<br/>(SAD / C1-C2 / ADR + build seam)"]
  Graph --> Verify
  Verify --> C34["Live C3/C4 Graph & Traceability<br/>(acdd-index + governs/cites_doc)"]
```

- **Small (`S`) -> Ephemeral / Root-Only Run**: Single-purpose runs (`acdd start --goal ...` with `--workflow` referencing custom or copied `audit` / `spec_planning` workflows in `.acdd/workflows/`) using `inherit: audit`, `inherit: base`, or `inherit: none` and `save: none` (and explicit `save.report` paths under `.acdd/reports/*.{md,json}` when emitting reports).
- **Medium (`M`) -> In-Place Plan Run (`--in-place` or `git.isolation: in_place` workflow)**: Coherent multi-step changes on the active working tree (`docs/plans/*.md`) with clear `AC-*` targets, ordered tasks and subtasks, `save: none` (zero auto-commits), and independent review.
- **Large (`L`) -> Worktree Milestone Run (`.acdd/workflows/default.yaml` / `code`)**: Architectural or concurrent multi-task changes (`docs/milestones/*.md`) requiring pre-task `blueprint_synthesis` (`synthesize` -> `review`, mapping `AC-*` criteria, `C2` boundary impact, `ADR` requirements, and `SAD` updates into per-task slices), isolated Git worktrees, scoped build verification (`code.lint` + `rust-check-and-test` in this repository's `.acdd/workflows/default.yaml`, or `code.seam` reachability in embedded `code` / opt-in overrides), and bound `handoff` archival.

## Governance invariants

> [!IMPORTANT]
> Invariant: Live source indexes (`acdd-index`) remain the sole authority for C4 Level 3–4 component and code dependency graphs; static architecture documents declare C1–C2 boundaries and `INV-*` rules without duplicating mutable symbol catalogs. [`extract_with_details_with_zones`](../../crates/acdd-index/src/docs.rs)

> [!IMPORTANT]
> Invariant: Pre-task `blueprint_synthesis` gates persist per-task architectural slices (`proof.tasks[<task_ref>]`) in `gate_artifacts` / `ClaimResp.pre_task_slice` before task claims unlock, isolating each task's context to its own contract slice. [`build_claim_context`](../../crates/acdd-state/src/context/mod.rs)
