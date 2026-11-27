---
title: "Milestone Instructions & Authoring Governance"
doc_type: guide
---

# Milestone Instructions & Authoring Governance

This document defines the `SpecDocument` authoring format and lifecycle governance used by the unified `crates/acdd-spec`, `crates/acdd-state`, and `crates/acdd-core` engine under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md).

---

## 1. Milestone Document Structure & Frontmatter

```yaml
---
id: <TAG>-<NNN>-<slug>
title: "Human-readable milestone title"
doc_type: contract
status: planned
depends_on:
  - <upstream-milestone-id>
guidance:
  hitl: pro                  # Guidance slot binding: careless | curious | pro | paranoic
owners:
  - crates/acdd-core/src/
  - crates/acdd-state/src/
invariants:
  - 'INV-SLOT-NAME: Explicit, testable invariant statement.'
acceptance_criteria:
  - 'AC-1: Observable production seam verification criterion.'
required_skills:
  - acdd
  - agent-instructions
repositories: []
linked_to: []
---
```

### Frontmatter Invariants

1. **`id`**: Must match `<TAG>-<NNN>-<slug>` conforming to the Domain Taxonomy (`AC`, `LS`, `PF`, `AR`, `UX`, `IN`) or numeric legacy format.
2. **`doc_type: contract`**: Milestones are execution contracts (`doc_type: plan` is used for in-place plans under `docs/plans/`).
3. **`workflow`**: Optional workflow selector (`base | code | documentation | <path>`). When omitted, spec defaults apply (`specs.milestones.workflow` -> `code`).
4. **`status`**: One of `planned`, `active`, `completed`, or `cancelled` (cancelled manifests require a non-empty `cancellation_reason`).
5. **`guidance: { slot: tier }`**: Binds parameterized guidance slots (`hitl.pro`, etc.).
6. **`invariants:`**: Prefixed with `INV-<NAME>` in single quotes; every invariant must be verifiable by automated tests.
7. **`#[serde(deny_unknown_fields)]`**: Unknown or removed frontmatter keys fail `acdd validate`.

---

## 2. Task Decomposition & Symmetrical Scope Boundaries

Each `TaskSpec` block (`### task: <task_ref>`) strictly uses `TaskSpecYaml` fields (`task_ref`, `target`, `spec`, `scope`, `extended_scope`, `depends_on`, `tags`, `invariants`, `acceptance_criteria`, `contract_revision`, `status`, `subtasks`, `receipt`):

```yaml
### task: <task-slug>

```yaml
task_ref: <task-slug>
target: "Clear, single-sentence imperative outcome detailing exact deliverables"
contract_revision: 1
depends_on:
  - <upstream-task-slug>
tags:
  - default
scope:
  - crates/acdd-core/src/module.rs
  - crates/acdd-state/src/module.rs
  - docs/reference/tasks.md
invariants:
  - 'INV-EXAMPLE: Concise task-level invariant.'
acceptance_criteria:
  - 'AC-1: Concise verification criterion.'
status: planned
subtasks:
  - subtask_ref: concrete-subtask-1
    title: "Explicit, testable description of subtask 1"
  - subtask_ref: concrete-subtask-2
    title: "Explicit, testable description of subtask 2"
```
```

### Task Specification Rules

- **`target`**: States what is built, what invariants are proved, and what budgets/boundaries apply.
- **`depends_on:` (Topological Waves DAG)**:
  - Waves are formed strictly by topological dependencies in `depends_on: [...]`.
  - Tasks with `depends_on: []` execute in Wave 0 (Bootstrap).
  - Tasks depending only on Wave 0 can run concurrently in Wave 1 under `git.isolation: worktree` when their `scope:` sets are disjoint.
- **`scope:` (Physical Write Perimeter)**:
  - Declare every workspace-relative path or glob the worker may write (`is_agent_writable`).
  - **Disjoint Scopes**: Concurrent tasks in the same wave MUST have disjoint `scope:` declarations to avoid Git integration conflicts and `TASK_SCOPE_CONFLICT`.
  - **Co-Evolution of Documentation**: Include affected `docs/` files directly in `scope:` when modifying public interfaces, CLI/MCP surfaces, or runtime contracts.
  - Workers may expand admitted scope at runtime via `acdd extend-scope <node> --path <glob> --reason "..."` (or `acdd_state(action: "extend_scope")`), which increments `contract_revision` in `.acdd/state.sqlite` and fails closed with `TASK_SCOPE_CONFLICT` if the path overlaps another already-claimed incomplete task.
- **`tags:`**: Defaults to `["default"]`. Must match declared `tags:` in the active `WorkflowDef` (`acdd validate` fails closed on unknown tags).
- **No per-task workflow overrides**: Unknown or removed per-task fields fail `acdd validate` via `#[serde(deny_unknown_fields)]`.

---

## 3. Wave 0 Bootstrap Task Template & Contract Quality Checklist

For non-trivial multi-wave milestones, declare an initial **Wave 0 Bootstrap Task** (`Task 0`) to freeze shared types, register test modules inside canonical binaries (`tests/acdd.rs`, `tests/core.rs`, `tests/mcp.rs`, etc.), and confirm disjoint Wave 1 scopes before parallel builders begin.

### Canonical Wave 0 (Task 0) Template

```yaml
### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: "Synthesize cross-task contract matrix, establish deterministic topological waves, register shared seam test modules, and freeze disjoint Wave 1 scopes"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - docs/milestones/<TAG>-<NNN>-<slug>.md
  - docs/adr/<NNNN>-<slug>.md
  - <path/to/shared_core_definitions>
  - <path/to/canonical_test_root>
  - <path/to/task_1_seam_test>
  - <path/to/task_2_seam_test>
status: planned
subtasks:
  - subtask_ref: cross-task-data-contract-matrix
    title: "Define canonical cross-task contract matrix and shared data structures"
  - subtask_ref: deterministic-priority-waves-dag
    title: "Establish deterministic topological execution waves and explicit DAG dependencies"
  - subtask_ref: seam-test-harness-pre-registration
    title: "Pre-register seam test modules in canonical test binaries"
  - subtask_ref: disjoint-scope-boundaries-freeze
    title: "Freeze disjoint file write scopes across all milestone tasks"
```
```

### Contract Quality Checklist

1. **Zero-Collision Test Registration**: Register new seam modules in canonical domain test binaries (`tests/acdd.rs`, `tests/core.rs`, `tests/mcp.rs`, `tests/query_context.rs`, `tests/linker.rs`, `tests/languages.rs`, `tests/commitment_integrity.rs`) in Task 0; downstream wave tasks edit only their own seam modules.
2. **Disjoint File Scopes**: Concurrent tasks in the same wave must have disjoint `scope:` declarations.
3. **Closed DAG & Valid Tags**: All `depends_on` references, `tags`, and `profile` references must pass `acdd validate <manifest>` before `acdd start <manifest>`.
4. **Build Seam Proof (`proof: command`)**: Author compiling, passing scoped tests directly in `build` (`exit_code == 0`, respecting `max_net_loc` budgets; embedded `code` and opt-in workflows additionally verify `code.seam` AST reachability `<= 8` hops from the `--test` target to production symbols in `admitted_scope`, while this repository's `.acdd/workflows/default.yaml` enforces `code.lint` + `rust-check-and-test`).
5. **Replace, Don't Stack**: Whenever an AC introduces a new enum, type, or facade, explicitly name the retired symbol/wrapper to delete and the live production call sites to migrate; never specify production enum variants or predicates with zero production callers.
6. **Non-Fallback Fast-Path Proof**: Whenever an AC introduces an optimization with a silent fallback (`Ok(None) => slow_path`), require a production-seam test asserting the fast path actually executes on realistic production-generated data (e.g. `pipeline::build` with full-range hashes) rather than only synthetic fixtures.
7. **Bounded Milestone Scope**: Keep each milestone to a cohesive 2–4 task slice; do not bolt unrelated architectural refactor waves onto a mid-flight performance or feature milestone.
8. **Roadmap Alignment**: Update [the roadmap](../../ROADMAP.md) in the same change whenever adding a new milestone or refining milestone commitments, dependencies, or release targets with the user.

---

## 4. Gate Lifecycle, Rejection Routing, and HITL Tiers

Under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), all gate routing uses explicit `pass:` and `fail:` edges with bounded `max_attempts`:

```text
blueprint_synthesis.synthesize -> blueprint_synthesis.review
  -> tasks: build -> review -> deliver
  -> run_tests (or `verify` in base `code`) -> (fail -> polish -> run_tests) -> integration_review -> handoff
```

- **Rejection routing**: `acdd submit <node> --action reject --findings <json>` records structured `RejectionFindings` (`summary`, optional `blackboard_message_id`, and `items` with `{ severity, path?, message, remediation }`) and transitions to `gate.fail` (e.g. `build` or `polish`) up to `max_attempts` (escalating to `escalated` when exhausted).
- **HITL Governance Tiers (`guidance.hitl`)**:
  - `careless`: Escalate only on DAG deadlocks or environment failures.
  - `curious`: Surface interface evolution on `acdd_blackboard`; escalate breaking public APIs or schema migrations.
  - `pro` (*default*): Approve internal helpers; escalate public library APIs, CLI/MCP surfaces, and DB migrations.
  - `paranoic`: Require explicit human approval for every interface deviation.

---

## 5. Run Verification, Documentation Scan, and Handoff

Before terminal `handoff` (`archive: true, prune: true`):

1. Ensure all non-cancelled tasks are `completed` and integrated (`TASK_NOT_INTEGRATED` check passes).
2. Run the documentation scan agent against the final diff and reconcile affected `docs/` pages.
3. Run required repository gates in `TESTS.md` (via `acdd verify run_tests` / `./scripts/test-macos-ssh.sh`, covering `commitment_integrity`, strict Clippy, `cargo test --workspace --all-targets`, `acdd docs check`, `acdd validate`, `/tmp/contextunity-test-*` leak check, and remote macOS SSH verification).
4. Execute `run_tests` -> `integration_review` -> `handoff` (or `acdd run`) to archive the completed specification (`closure:` + per-task `receipt:` rollups) into `docs/milestones/archive/` and prune active run rows into `run_tombstones`.
