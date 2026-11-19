---
title: "Milestones"
doc_type: guide
---

# Milestones

Milestone commitment files in this directory (`0XX-*.md`, `TAG-NNN-*.md`) define the repository's execution queue and lifecycle commitments.

## Ownership and Truth

- **Single Source of Truth:** Each milestone file (`docs/milestones/0XX-*.md`, `docs/milestones/TAG-NNN-*.md`) owns its `SpecDocument` status (`planned`, `active`, `completed`, or `cancelled`), invariants, task breakdown, and acceptance outcomes. A cancelled specification records a non-empty `cancellation_reason`.
- **Queue Order:** Numeric or domain-tagged filename prefixes (`AC-005-*.md`, `AR-001-*.md`, `PF-002-*.md`, etc.) identify commitments and dependencies. Completed milestones (`AR-001`, `PF-002`, `PF-001`, etc.) reside in [archive/](archive/README.md).
- **Archive:** Completed and cancelled milestones are retained in [archive/](archive/README.md) with their `closure:` receipts and rollups. Terminal closure prunes operational run state from `.acdd/state.sqlite` into `run_tombstones`.

Use `acdd list` (or `acdd status`) to inspect active runs and closed tombstones, `acdd draft milestones <slug>` to scaffold the next numbered milestone, and `acdd validate` to check schemas and DAG dependencies. Continue an active run or start the next planned specification with `acdd start <spec_path>` when its `depends_on` commitments are satisfied.

Milestone contracts are searchable documentation when admitted by configured roots (`specs.milestones`). Search visibility does not admit a specification into execution or replace `SpecDocument` + `.acdd/state.sqlite` authority. Use `acdd` state commands and MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`) to inspect and advance operational state. See [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md) and [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md) for lifecycle, visibility, and coordination invariants.

## Milestone Taxonomy and Domain Tagging

Milestones use a 2-letter domain prefix and a 3-digit sequence number (`TAG-NNN-slug.md`), providing up to 1000 milestone slots (`001`–`999`) per domain:

| Domain Prefix | Domain Name | Scope & Responsibilities | Range |
| :--- | :--- | :--- | :--- |
| **`AC`** | **ACDD & State Engine** | Unified `GateProfile`/`Run` engine (`acdd-state`), SPI (`acdd-core`), observability, memory, triggers | `AC-001` .. `AC-999` |
| **`LS`** | **Language Semantics** | Tree-sitter parsers, AST extraction, declarative framework packs, linker | `LS-001` .. `LS-999` |
| **`PF`** | **Performance & Storage** | Cold-build throughput, SQLite compaction, latency budgets, index memory | `PF-001` .. `PF-999` |
| **`AR`** | **Architecture & Modularity** | Engine architecture, traits (`LanguageLinker`), crate decoupling | `AR-001` .. `AR-999` |
| **`UX`** | **Agent UX & Ergonomics** | MCP response compaction, token efficiency, Markdown-first dual-surface | `UX-001` .. `UX-999` |
| **`IN`** | **Installer & Onboarding** | Multi-flavor distribution, `install.sh`, packaging, repo onboarding | `IN-001` .. `IN-999` |

### Legacy Phase 0 Series (`010`–`056`)

Archived milestones with 3-digit pure numbers (`010`–`056`) represent the foundational Phase 0 implementation. They map to the domain taxonomy as follows:
- `010`–`019` $\to$ Phase 0 predecessors of **`AC`** (`ACDD & Lifecycle`).
- `020`–`023` $\to$ Phase 0 predecessors of **`LS`** (`Language Semantics`).
- `030`–`031` $\to$ Phase 0 predecessors of **`PF`** (`Performance`).
- `040`–`048` $\to$ Phase 0 predecessors of **`AR`** (`Architecture`).
- `050`–`056` $\to$ Phase 0 predecessors of **`UX`** (`Agent Ergonomics & MCP`).

Active commitments follow the `TAG-NNN` standard; archived milestone files retain their historical commitments and receipts in [archive/](archive/README.md).

## Milestone Authoring and Contract Governance

Every milestone document in `docs/milestones/` is a strict `SpecDocument` execution contract (`#[serde(deny_unknown_fields)]`). Follow [`docs/milestones/AGENTS.md`](AGENTS.md) for the active `0.6.0` format and Wave 0 bootstrap template.

### 1. Frontmatter Format

```yaml
---
id: AC-008-example-milestone
title: "Example milestone title"
doc_type: contract
profile: workspace
status: planned
depends_on: []
owners:
  - crates/acdd-core/src/
  - crates/acdd-state/src/
invariants:
  - 'INV-SLOT-NAME: Explicit invariant statement'
acceptance_criteria:
  - 'AC-1: Observable production verification criterion'
guidance:
  hitl: pro
required_skills:
  - acdd
---
```

Frontmatter must strictly declare only fields supported by `SpecFrontmatter` (`id`, `title`, `doc_type`, `profile`, `status`, `cancellation_reason`, `spec`, `depends_on`, `owners`, `invariants`, `acceptance_criteria`, `guidance`, `required_skills`, `repositories`, `linked_to`, `closure`). Unknown fields fail `acdd validate`.

### 2. Task Blocks and Scope Boundaries

Every YAML task block (`### task: <task_ref>`) under `## Tasks in this milestone` must define physical file boundaries (`scope`), dependencies (`depends_on`), and subtasks conforming to `TaskSpecYaml`:

```yaml
### task: sqlite-state-migrations

```yaml
task_ref: sqlite-state-migrations
target: "Implement SQLite schema migrations in acdd-state store"
contract_revision: 1
tags:
  - default
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
scope:
  - crates/acdd-state/src/store/sqlite/migrations.rs
status: planned
subtasks:
  - subtask_ref: schema-migration-table
    title: "Implement SQLite schema migration and CAS precondition tests"
```
```

- **`depends_on:` (Topological Waves)**: Waves are strictly formed by the dependency DAG (`depends_on: [...]`). Tasks with empty `depends_on: []` execute in Wave 0; tasks depending solely on Wave 0 run concurrently in Wave 1 under `git.isolation: worktree`.
- **`scope:` and `extended_scope:` (Files)**: `scope` is the admitted baseline. Workers add paths via `acdd extend-scope <node> --path <glob> --reason "..."` (or `acdd_state(action: "extend_scope")`), which increments `contract_revision` in `.acdd/state.sqlite` until terminal archival writes `extended_scope` into the archived spec.
- **`tags:` and `when:`**: Task blocks default to `tags: [default]`; `when:` expressions in `GateProfile` (`version: 2`) select conditional gates based on task tags.

### 3. Review & Verification Governance

- **Gate progression**: The resolved `GateProfile` (`version: 2`) defines root gates, `- tasks:` gates, `pass:`/`fail:`/`fail_drift:` edges, `need:` candidate tree parity checks, and `DomainProvider` capabilities (`call:`, `checks:`, `context:`).
- **Reuse-first & complexity escalation**: During synthesis, build, and polish, audit canonical owners (`INV-SINGLE-OWNER`), reuse existing shared abstractions, and record justification or escalate via `acdd_blackboard` (`topic: decisions` or `blockers`) before introducing any new subsystem, shadow resolver, narrow string heuristic, or disproportionate defect-fix complexity growth.
- **Review and escalation**: Independent reviewers (`independent_from`) evaluate every configured contour (`paths`, `claims`, `architecture`, `concurrency`, `project_isolation`, `administration`) using typed `ReviewEvidence` (`--evidence`) or reject with structured `RejectionFindings` (`--findings`). Builders refute ADR- or contract-contradicting findings via `acdd_blackboard` (`topic: decisions`) citing `file:line` proof (`REBUTTAL OVER BLIND REWRITE`), while reviewers reject only on concrete contract/invariant violations or reproducible defects (`BURDEN OF PROOF`).
