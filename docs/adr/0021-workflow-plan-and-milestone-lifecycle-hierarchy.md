---
title: "ADR 0021: Workflow, Plan, and Milestone Lifecycle Hierarchy"
doc_type: adr
status: accepted
date: 2026-11-15
decision_makers:
  - Oleksii (Architecture Owner)
  - Antigravity (ACDD Coordinator)
consulted:
  - Forge MCP Core Team
informed:
  - Autonomous Agent Coordinators and Builders
---

# ADR 0021: Workflow, Plan, and Milestone Lifecycle Hierarchy

> [!NOTE]
> Alignment of legacy `plan.rs` in-place orchestration onto SQLite `TasksStore`, explicit `inherit: workspace` resource resolution, and append-only `save: log` workflow traces is tracked in [`AC-004-unified-execution-and-spec-pipeline`](../milestones/AC-004-unified-execution-and-spec-pipeline.md). See the [ACDD execution reference](../reference/acdd.md#headless-task-milestone-plan-and-workflow-execution) for current runtime boundaries.

## Context and decision: Variant A

[ADR 0020](0020-hierarchical-acdd-gates-and-milestone-orchestration.md) establishes the hierarchical gate grammar. Standalone procedures (`Workflow`), in-place multi-step execution (`Plan`), and contract-governed feature delivery (`Milestone`) share reusable agent and command configuration while differing fundamentally in state persistence and architectural ceremony.

ACDD separates two orthogonal axes:

1. **Two execution engines** across three authoring artifacts:
   - **Stateless Taskless Engine (`Workflow`)**: Executes a pure root `gates:` graph with no `tasks:` container, no SQLite task or subtask rows, and no Git worktree management.
   - **Unified Stateful Task Engine (`TasksStore` + `task_runner`)**: Executes multi-step task/subtask DAGs in SQLite with wave scheduling, claim scoping, blackboard coordination, findings propagation, and crash/resume recovery in one of two lifecycle modes: **Plan** or **Milestone**.
2. **Shared resources vs. pipeline topology (`inherit`)**: Profiles separate shared execution resources (`roles`, `commands`, `policies`, `limits`, `guidance`, `scope_args_by_pattern`, `proofs`) from the lifecycle topology (`gates:` and `save:`).

### Workflow

A Workflow is a pure root `gates:` graph (`tasks:` absent) for repeatable standalone operations: security audits and penetration checks, CI quality verification, repository polish, document authoring (`author-spec`, `author-plan`, `author-milestone`, `author-workflow`), and isolated test harnesses. Workflows execute in memory (`save: none`) or append structured gate progression and command observation records to an append-only run log (`save: log`) without mutating `.acdd/tasks.sqlite` or spawning worktrees.

### Unified Task Engine: Plan Mode vs. Milestone Mode

Plan and Milestone both run on the same SQLite `TasksStore` and `task_runner` engine rather than duplicating step loops, subtask state machines, or repair routing:

`ExecutionSubjectKind` distinguishes Plan from Milestone in the canonical store;
legacy rows default to Milestone. Plan identity is stable within the repository
namespace and normalized confined document path. Plan steps and their subtasks
use the shared claims, transitions, findings, blackboard, and resume state.
Subject dispatch selects authority parsing, context/profile resolution, and
Markdown receipt projection in the existing owners. It introduces no parallel
registry, scheduler, proof store, or persistence table.

| Dimension | Plan Mode (`docs/plans/*.md`) | Milestone Mode (`docs/milestones/*.md`) |
| :--- | :--- | :--- |
| **Workspace isolation** | Active working tree (`--in-place`) | Isolated task and milestone Git worktrees |
| **Persistence (`save`)** | `snapshot` (SQLite state + tree snapshots, zero auto-commits) | `commit` (delivery commits, bound receipts, archival) |
| **Task & subtask engine** | Full SQLite tasks, subtasks, `resume`, `blackboard`, bidirectional Markdown sync | Full SQLite tasks, subtasks, `resume`, `blackboard`, manifest sync |
| **Pre-task design gate** | None (plan manifest is already tactical) | Mandatory `blueprint_synthesis` (`synthesize` $\to$ `review`) with per-task slice fan-out |
| **Seam & contract gates** | Optional ordinary gates; no mandatory red contract or integration seam | Red `contract` proof (`expected_failure`), conditional `seam` integration |
| **Closure** | Terminal step delivery (`deliver` / snapshot) | `verify` $\to$ `polish` $\to$ `milestone_review` $\to$ `handoff` |

A canonical Milestone profile composes all three phases while keeping task execution strictly inside the `- tasks:` container:

```yaml
gates:
  - id: blueprint_synthesis
    gates:
      - id: synthesize
        role: architect
        proof: blueprint
      - id: review
        role: independent_reviewer
        independent_from: synthesize
        fail: synthesize
  - tasks:
      gates:
        - id: contract
          role: contract_author
          proof: contract
        - id: build
          role: builder
          command: rust-check-and-test
        - id: review
          role: independent_reviewer
          independent_from: build
          fail: build
        - id: seam
          when: seam
          gates:
            - id: integrate
              role: builder
              proof: seam_test
              command: rust-check-and-test
            - id: review
              role: independent_reviewer
              independent_from: integrate
              fail: integrate
        - id: deliver
          receipt: true
          need: [review]
          save: commit
  - id: verify
    command: check_full_repo
    pass: milestone_review
    fail: polish
  - id: polish
    role: architect
    pass: verify
    save:
      pre: snapshot
      post: snapshot
  - id: milestone_review
    role: independent_reviewer
    independent_from: polish
    pass: handoff
    fail: polish
    save:
      immutable: true
  - id: handoff
    receipt: true
    absorb: true
    save: commit
```

### Resource vs. gate inheritance (`inherit`)

The `inherit` field selects an explicit base target so standalone procedures can stay isolated or reuse workspace configuration cleanly without leaking task pipelines:

- `inherit: none`: Compiles a completely self-contained profile with zero external configuration lookup. Suitable for isolated pentests, clean-room CI gates, security sandboxes, and self-contained verification profiles.
- `inherit: default`: Overlays the embedded `acdd.profile.yaml` defaults (the default when `inherit` is omitted).
- `inherit: workspace`: Imports shared execution resources (`roles`, `commands`, `policies`, `limits`, `guidance`, `scope_args_by_pattern`, `proofs`) from the active workspace profile (`.acdd/profile.yaml` or `--profile` override) while ignoring the workspace profile's `gates:` and root `save:`. Builtin and local authoring workflows inherit the host repository's configured agent runners and models without inheriting Milestone `blueprint_synthesis` or `tasks:`. Self-referential `inherit: workspace` on the root workspace profile fails closed during validation.
- `inherit: <path>`: Inherits explicitly from a confined workspace-relative profile file, following the same resource merge rules and retaining local Workflow/Plan topology and save settings. Reject escaping paths, inheritance cycles, and root self-inheritance.

Merge selected base resources by key, applying local definitions after inherited
definitions: roles (including runner, models, and prompt_template), commands,
policies, limits, guidance, scope_args_by_pattern, and proofs. Resolve the active
workspace profile through its explicit `--profile` override when supplied.
Workflow and Plan overlays retain their own `gates:` and `save:`; resource
inheritance never imports a Milestone task pipeline into a Workflow.

Local overrides in `docs/workflows/` resolve ahead of builtin embedded workflows of the same name.

## Persistence and audit log

The four architectural persistence levels are `none`, `log`, `snapshot`, and `commit`:

- `none`: Ephemeral in-memory execution.
- `log`: Append-only execution trace recording gate progression, timestamps, observed commands, exit codes, output digests, durations, review decisions, and terminal status without creating task rows in `.acdd/tasks.sqlite`.
- `snapshot`: Working-tree snapshots paired with SQLite state (`Plan` default).
- `commit`: Isolated Git commits, bound verification receipts, and milestone handoff archival (`Milestone` default).

Sequential `save: log` Workflow runs append observations without overwriting
earlier runs or creating task/subtask rows. Plan `clear|archive` prunes exactly
the selected Plan's SQLite rows and dependent traces, checkpoints, and snapshot
state while preserving foreign subjects. Partial cleanup failures are reported
and remain safe to retry. Plan snapshot completion follows the mandatory drift
rebind protocol in [ADR 0015](0015-acdd-lifecycle-snapshots-and-proactive-defect-resolution.md).

## Non-goals and consequences

- **No synthetic tasks in workflows**: Stateless workflows never fabricate `tasks` rows or require `milestone init`.
- **Single stateful engine for plans and milestones**: Plan execution shares SQLite DAG waves, subtask transitions, findings propagation, and `resume` guarantees with Milestones instead of maintaining divergent markdown-only runners.
- **Explicit resource/topology boundary**: `inherit: workspace` eliminates boilerplate runner and command duplication across workflows and plans while preserving isolated `inherit: none` sandboxes for security, CI, and clean-room verification.
