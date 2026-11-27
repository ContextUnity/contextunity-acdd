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

Maintaining separate execution engines, storage tables, and CLI/MCP surfaces for milestones, in-place plans, and root-only workflows duplicated DAG scheduling, claim locking, review routing, and recovery paths. Unifying all stateful execution around one declarative workflow graph and one pure state-transition core eliminates structural drift while supporting both Git-tracked specifications and ephemeral `--goal` runs.

## Decision

### 1. Two Primitives: `WorkflowDef` (`version: 2`) and `Run`

All lifecycle execution collapses onto two orthogonal primitives across `crates/acdd-spec` and `crates/acdd-state`:

1. **`WorkflowDef` (`version: 2`)**: Declarative YAML graph (`inherit`, `git:`, `save:`, `tags:`, `protected_paths:`, `default_tools:`, `modules:`, `limits:`, `roles:`, `commands:`, `scope_args_by_pattern:`, `proofs:`, `guidance:`, `contours:`, `gate_templates:`, `gates:` or mutually exclusive `gates_patch:`). Embedded baselines are strictly three core workflows (`base`, `code`, `documentation`), while auxiliary templates (`audit`, `spec_planning`, `hotfix`) ship under `skills/acdd/references/acdd-workflow-examples/`. `.acdd/workflows/default.yaml` automatically becomes the workspace default when present (discovered via `workflows_dirs`, default `[".acdd/workflows"]`), while `in_place` is an execution mode (`--in-place` or `git.isolation: in_place` in a workflow) that neutralizes `save.post: commit` to `none`. Strict `deny_unknown_fields` parsing rejects legacy or invalid workflow YAML (`ACDD_WORKFLOW_SCHEMA_INVALID`) separately from specification validation failures (`ACDD_SPEC_SCHEMA_INVALID`), and gate `save.report` materializes reports strictly under `.acdd/reports/*.{md,json}` on pass (`SubmitResp.report_path`).
2. **`Run`**: One active or completed execution instance bound to a `SpecDocument` (`docs/milestones/*.md`, `docs/plans/*.md`, parsed and statically validated in `crates/acdd-spec` over `SpecRuntimeOverlay`) or an ephemeral `--goal` specification (`.acdd/run/<run_id>/spec.md`) and materialized into a `gate_nodes` DAG addressed by validated [`NodeCursor`](../../crates/acdd-core/src/ids.rs) and `GateSelector` identifiers.

### 2. Node Hierarchy, `SpecDocument`, and `blueprint_synthesis`

- **Four `GateNode` kinds**: `container` (root gate group such as `blueprint_synthesis`), `root_gate` (pre-task or post-task gate such as `verify`, `polish`, `integration_review`, `handoff`), `task` (`gate_id = "__task__"`, `task_ref = <ref>`), and `task_gate` (`<task_ref>.build`, `<task_ref>.review`, `<task_ref>.deliver`).
- **Lifecycle states**: `SpecDocument` uses four statuses (`planned`, `active`, `completed`, `cancelled`; cancelled requires a non-empty `cancellation_reason`). `GateNode` uses `pending`, `ready`, `in_progress`, `completed`, `stale`, `escalated`, and `cancelled`.
- **Conditional tracks & routing**: `- tasks:` blocks and `task_gate` nodes support boolean `when:` tag expressions. Sibling routing uses explicit `pass:`, `fail:`, and `fail_drift:` edges with bounded `max_attempts` (default `3`, `>= 1`) and `independent_from` worker separation (`TASK_REVIEW_NOT_INDEPENDENT`). Rejecting with `RejectionKind::ArchitecturalDrift` routes through `fail_drift:` (such as `tasks.review -> rearchitect -> build`), flagging `ClaimResp.context_reset_required = true` and rotating `ACDD_AGENT_SESSION_ID` in `acdd run` so builders discard stale assumptions.
- **Pre-task blueprint fan-out & budgets**: Passing `blueprint_synthesis` with `proof.tasks[<task_ref>]` (including `canonical_owners`, `mechanism_rules`, and optional `max_net_loc` alongside scope, interfaces, and incoming/outgoing seams) fans out per-task architectural slices (`pre_task_slice`) delivered to each task on `claim`, along with `docs.instructions` (`AGENTS.md`, scoped `TESTS.md`) and `docs.adrs` (relevant ADRs). `tasks.build` enforces `max_net_loc` against scoped production Git delta (excluding test files) unless justified via `blackboard` (`topic: decisions`).
- **Terminal pruning**: Terminal `archive` / `prune` or `acdd clear` removes active run rows into compact `run_tombstones` while retaining committed archive manifests (`closure:` + per-task `receipt:`).

### 3. Pure Functional Core and `StateBackend` CAS Boundary

> [!IMPORTANT]
> Invariant: The state machine reducer stays 100% pure and SQLite imports stay confined to `crates/acdd-state/src/store/sqlite/`.
> - INV-STATE-PURE-MACHINE: `crates/acdd-state/src/machine/` is a pure functional transition planner with zero `std::fs`, `std::process`, `std::net`, or `rusqlite` calls. [`evaluate_transition`](../../crates/acdd-state/src/machine/mod.rs)
> - INV-STATE-SQLITE-CONFINED: Direct `rusqlite` usage in `crates/acdd-state/src/` is confined exclusively to `crates/acdd-state/src/store/sqlite/`, while `usecases/` routes persistence through `&dyn StateBackend` (`exec_tx_dyn`). [`open_read_only_if_exists`](../../crates/acdd-state/src/store/sqlite/mod.rs)
> - INV-STATE-SINGLE-VALIDATOR: All workspace, spec, workflow, and dependency validation routes through `acdd_spec::{validate, deps}`, `acdd_state::validate`, and `acdd_state::usecases::deps`, and all `when:` parsing/evaluation routes through `WhenExpr`. [`validate_workspace`](../../crates/acdd-state/src/validate.rs) [`normalize_optional_when`](../../crates/acdd-state/src/workflow/when.rs)

```acdd-rule
id: INV-STATE-PURE-MACHINE
invariant: INV-STATE-PURE-MACHINE
severity: error
kind: forbid_dependency
from:
  zones: [state_machine]
to:
  paths:
    - "crates/acdd-state/src/store/**"
    - "crates/acdd-state/src/git_ops/**"
    - "crates/acdd-state/src/runner/**"
    - "crates/acdd-state/src/usecases/**"
    - "crates/acdd-core/src/fs.rs"
    - "crates/acdd-core/src/git.rs"
    - "crates/acdd-core/src/lock.rs"
    - "crates/acdd-core/src/sys.rs"
  external_symbols: ["std::fs", "std::process", "std::net", "std::env", "std::time::SystemTime", "std::time::Instant", "rusqlite"]
include_tests: false
```

```acdd-rule
id: INV-STATE-SQLITE-CONFINED
invariant: INV-STATE-SQLITE-CONFINED
severity: error
kind: single_owner
from:
  zones: [state]
to:
  external_symbols: ["rusqlite"]
owner_paths: ["crates/acdd-state/src/store/sqlite/**"]
include_tests: false
```

```acdd-rule
id: INV-STATE-SQLITE-LEAF
invariant: INV-STATE-SQLITE-CONFINED
severity: error
kind: forbid_dependency
from:
  zones: [state_sqlite]
to:
  paths:
    - "crates/acdd-state/src/git_ops/**"
    - "crates/acdd-state/src/usecases/**"
    - "crates/acdd-state/src/runner/**"
    - "crates/acdd-core/src/git.rs"
include_tests: false
```

- **`StateBackend` (`StateReader` / `StateTx`)**: Short optimistic-CAS transactions verify `node_version` and `runs.generation`, returning `ErrorCode::BackendConflict` on concurrent drift.
- **Canonical errors**: `acdd-core::errors::AcddError` is the sole structured error type across all workspace crates (`StateError` compatibility alias removed), carrying `ErrorCode`, a message, and an optional hint while keeping stable wire names and exit codes.
- **SQLite schema**: [`crates/acdd-state/src/store/sqlite/schema.rs`](../../crates/acdd-state/src/store/sqlite/schema.rs) defines `_sqlite_meta` (`schema_version`, `write_seq`, `last_backup_seq`) and 9 domain tables: `runs`, `gate_nodes`, `task_dependencies`, `subtasks`, `gate_claims`, `gate_traces`, `gate_artifacts`, `blackboard`, and `run_tombstones`.

Use `ControlFlow` for early exits and borrow loaded graph slices through use-case phases. Keep one-caller preparation and accumulator structs out of the state API.

### 4. Blackboard Hierarchy and Bounded Pagination

Blackboard messages scope to a `Run`, `NodeCursor`, or `SubtaskRef` across six topics (`draft`, `notes`, `findings`, `blockers`, `decisions`, `deferred`). Reads return summaries newest-first (default 50 items, max 100, with pagination metadata). `acdd_state(action: "list")` and `blackboard` enforce `runs.generation` paging fences.

## Consequences

- Ephemeral `--goal` runs, `in_place` plans, taskless workflows, and multi-task `worktree` milestones execute through one flat CLI (`acdd start`, `claim`, `verify`, `submit`, `next`, `status`, `show`, `run`, `validate`, `roadmap`, `draft`) and five MCP state tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`).
- Pure `machine/` transition rules execute in `< 5 ms` against in-memory backends in `RunHarness::in_memory()` without disk or Git overhead.
- Unrecognized YAML fields, stale `claim_revision`, and worker collisions fail closed with deterministic `ErrorCode` diagnostics (exit codes `2`, `3`, `4`).
