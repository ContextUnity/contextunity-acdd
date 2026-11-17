---
title: "ADR 0003: Unified Profile Graph and State Machine"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0003: Unified Profile Graph and State Machine

## Status

Accepted.

## Context

Maintaining separate execution engines, storage tables, and CLI/MCP surfaces for milestones, in-place plans, and root-only workflows duplicated DAG scheduling, claim locking, review routing, and recovery paths. Unifying all stateful execution around one declarative profile graph and one pure state-transition core eliminates structural drift while supporting both Git-tracked specifications and ephemeral `--goal` runs.

## Decision

### 1. Two Primitives: `GateProfile` (`version: 2`) and `Run`

All lifecycle execution collapses onto two orthogonal primitives in `crates/acdd-state`:

1. **`GateProfile` (`version: 2`)**: Declarative YAML graph (`inherit`, `gates_merge: replace | patch`, `git:`, `save:`, `tags:`, `protected_paths:`, `default_tools:`, `modules:`, `limits:`, `git_commit_notes:`, `roles:`, `commands:`, `proofs:`, `guidance:`, `contours:`, `gate_templates:`, `gates:`). Strict `deny_unknown_fields` parsing rejects un-migrated `0.5` syntax (`ACDD_PROFILE_SCHEMA_INVALID`).
2. **`Run`**: One active or completed execution instance bound to a `SpecDocument` (`docs/milestones/*.md`, `docs/plans/*.md`) or an ephemeral `--goal` specification (`.acdd/run/<run_id>/spec.md`) and materialized into a `gate_nodes` DAG addressed by validated [`NodeCursor`](../../crates/acdd-core/src/ids.rs) and `GateSelector` identifiers.

### 2. Node Hierarchy, `SpecDocument`, and `blueprint_synthesis`

- **Four `GateNode` kinds**: `container` (root gate group such as `blueprint_synthesis`), `root_gate` (pre-task or post-task gate such as `verify`, `polish`, `integration_review`, `handoff`), `task` (`gate_id = "__task__"`, `task_ref = <ref>`), and `task_gate` (`<task_ref>.contract`, `<task_ref>.build`, etc.).
- **Lifecycle states**: `SpecDocument` uses four statuses (`planned`, `active`, `completed`, `cancelled`; cancelled requires a non-empty `cancellation_reason`). `GateNode` uses `pending`, `ready`, `in_progress`, `completed`, `stale`, `escalated`, and `cancelled`.
- **Conditional tracks & routing**: `- tasks:` blocks and `task_gate` nodes support boolean `when:` tag expressions. Sibling routing uses explicit `pass:` and `fail:` edges with bounded `max_attempts` (default `3`, `>= 1`) and `independent_from` worker separation (`TASK_REVIEW_NOT_INDEPENDENT`).
- **Pre-task blueprint fan-out**: Passing `blueprint_synthesis` with `proof.tasks[<task_ref>]` fans out per-task architectural slices (`pre_task_slice`) delivered to each task on `claim`.
- **Terminal pruning**: Terminal `archive` / `prune` or `acdd clear` removes active run rows into compact `run_tombstones` while retaining committed archive manifests (`closure:` + per-task `receipt:`).

### 3. Pure Functional Core and `StateBackend` CAS Boundary

- **`crates/acdd-state/src/machine/`**: Pure functional transition planner (`(RunGraphSnapshot, ResolvedGateProfile, ValidatedObservations, TransitionInput) -> TransitionPlan + StoragePreconditions`) with zero disk, process, network, or SQLite I/O.
- **`StateBackend` (`StateReader` / `StateTx`)**: Short optimistic-CAS transactions verify `node_version` and `runs.generation`, returning `ErrorCode::BackendConflict` on concurrent drift.
- **SQLite schema**: [`crates/acdd-state/src/store/sqlite/schema.rs`](../../crates/acdd-state/src/store/sqlite/schema.rs) defines `_sqlite_meta` (`schema_version`, `write_seq`, `last_backup_seq`) and 9 domain tables: `runs`, `gate_nodes`, `task_dependencies`, `subtasks`, `gate_claims`, `gate_traces`, `gate_artifacts`, `blackboard`, and `run_tombstones`.

### 4. Blackboard Hierarchy and Bounded Pagination

Blackboard messages scope to a `Run`, `NodeCursor`, or `SubtaskRef` across six topics (`draft`, `notes`, `findings`, `blockers`, `decisions`, `deferred`). Reads return summaries newest-first (default 10 items, max 50, with pagination metadata), while `inspect` retrieves a single message payload by ID. `acdd_state(action: "list")` and `blackboard` enforce `runs.generation` paging fences.

## Consequences

- Ephemeral `--goal` runs, `in_place` plans, taskless workflows, and multi-task `worktree` milestones execute through one flat CLI (`acdd start`, `claim`, `verify`, `submit`, `next`, `status`, `show`, `integrate`, `run`, `validate`) and five MCP state tools.
- Pure `machine/` transition rules execute in `< 5 ms` against in-memory backends in `RunHarness::in_memory()` without disk or Git overhead.
- Unrecognized YAML fields, stale `claim_revision`, and worker collisions fail closed with deterministic `ErrorCode` diagnostics (exit codes `2`, `3`, `4`).
