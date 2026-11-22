---
title: "Augmented Context & Durable Dispatch (ACDD)"
doc_type: guide
---

# Augmented Context & Durable Dispatch (ACDD)

**Augmented Context & Durable Dispatch (ACDD)** is ACDD's declarative lifecycle
state machine (`crates/acdd-state` + `crates/acdd-core`) for structured AI and
human software engineering. Instead of letting coding agents run open-ended
loops over an entire repository, ACDD binds Git-tracked specifications, SQLite
run state, and declarative `v2` YAML `GateProfile` graphs into atomic, verifiable
**gates** that deliver only the context needed for the active step.

---

## 1. What ACDD Is

ACDD separates *what* must be built from *how* work is governed and verified:

1. **Git-Tracked Authority (`docs/milestones/*.md`, `docs/plans/*.md`)**: Humans
   and architects keep contracts, `### task:` specifications, write perimeters
   (`scope`), dependency edges (`depends_on`), and proof policies in Markdown
   under Git (`specs:` collections in `acdd.yaml`). Milestone and task `spec`
   references and `acceptance_criteria` project only the claimed task's criteria
   into `ClaimResp.contract`.
2. **Persistent Run Queue & Proof Store (`.acdd/state.sqlite`)**: ACDD
   synchronizes specifications and profile graphs into the primary worktree's
   `.acdd/state.sqlite` (`runs`, `gate_nodes`, `task_dependencies`, `subtasks`,
   `gate_claims`, `gate_traces`, `gate_artifacts`, `blackboard`,
   `run_tombstones`). Rebuilding `.acdd/code-map.sqlite` never resets active run
   state or traces.
3. **Declarative Governance Profile (`version: 2` `GateProfile`)**: Defines Git
   isolation/integration (`git:`), persistence (`save:`), roles, commands,
   `DomainProvider` hooks (`call`, `checks`, `context`, `tools_add`/`tools_remove`),
   proof schemas, guidance slots, review contours, and ordered gates with `pass` /
   `fail` edges.

### Stage-Tailored Context Injection (`ClaimResp`)

Claiming a gate returns the budgeted `ClaimResp` bundle ([ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), [`crates/acdd-state/src/context/mod.rs`](../../crates/acdd-state/src/context/mod.rs))
ordered across 6 priority tiers inside `ResponsePolicy.max_output_bytes`:

1. **Tier 1 (`contract` + `guidance`)**: Mandatory claimed task/gate specification
   and resolved gate guidance entries (reserved inside `max_guidance_bytes`,
   never truncated; fails with `GUIDANCE_BUDGET_EXCEEDED` if oversized).
2. **Tier 2 (`unresolved_findings` + failed trace tail)**: Structured findings and
   the tail of the latest failed trace log after a reject/fail rewind.
3. **Tier 3 (`pre_task_slice`)**: Pre-task blueprint slice (`proof.tasks[<task_ref>]`)
   emitted by `blueprint_synthesis`, and `predecessor_proofs` from completed
   upstream sibling gates in the same container (e.g., `audit` `recon -> analyze -> triage`).
4. **Tier 4 (`dependency_receipts`)**: Receipts from completed dependency tasks.
5. **Tier 5 (`blackboard`)**: Up to 15 relevant blackboard `notes`, `blockers`,
   and `decisions` entries capped at 500 characters each (excludes `findings`,
   which arrive in Tier 2 `unresolved_findings`).
6. **Tier 6 (`context_lenses`)**: Dynamic `DomainProvider` lenses declared on the
   gate's `context:` list (e.g., `docs.adrs`, `docs.invariants`, `code.symbols`,
   `code.tests`, `code.external_callers`), trimmed cleanly by section if needed
   and reported in `omitted_sections`.

MCP state tools and flat CLI commands share `src/format/lifecycle.rs` Markdown
rendering (delegating `ClaimResp` prompt formatting to
[`render_claim_prompt_markdown`](../../crates/acdd-state/src/context/markdown.rs)) by
default, or structured JSON via `format: "json"` / `--format json`;
see the [task lifecycle Markdown contract](mcp-markdown-contracts.md#task-lifecycle).

Read-only **`StateQuery::Roadmap`** (`acdd roadmap`, `acdd_state(action: "roadmap")`)
aggregates specs across configured `specs:` collections with active runs, dependency
waves, critical-path ordering, scope-overlap markers, and optional per-spec task
drill-down. Formatting lives in `src/format/roadmap.rs`; the query opens
`.acdd/state.sqlite` read-only when present and does not create a missing database.

### One Unified `Run` + `GateProfile` Engine

Under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md)
and [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md),
ACDD runs all workflows through one state machine (`crates/acdd-state`), selected
by `GateProfile` `git:` and `save:` policies:

| Mode / Embedded Profile | Spec / Input | Isolation & `save` | Primary Use Case |
| --- | --- | --- | --- |
| **`base`** | Any spec or `--goal` | `git.isolation: worktree`, `save: commit` | Domain-pure baseline (`gates: []`) for custom overlays |
| **`code`** (default) | `docs/milestones/*.md` or custom spec | `git.isolation: worktree`, `save: commit` | Multi-task milestones with `blueprint_synthesis`, task worktrees, and verified `handoff` |
| **`audit`** | Any spec or `--goal` | `git.isolation: in_place`, `save: none` | Read-only `recon -> analyze -> triage` review and taint scanning with `.acdd/reports/{run_name}.md` |
| **`documentation`** | Any spec or `--goal` | `git.isolation: worktree`, `save: commit` | Compiler-free documentation and `acdd-rule` authoring gated on `docs.links` and `code.lint` (`[arch, docs]`) |
| **In-place / `--goal`** | Materialized `.acdd/run/<run_id>/spec.md` + `--scope` | Defaults to `code` (`worktree`) unless `--in-place` (`save: none`) or `--profile audit` | Taskless goal sweeps, autonomous QA, and single-branch runs |

---

## 2. How Gates Work

Everything in a `v2` `GateProfile` is a **gate** declared under `gates:`. Every
gate uses one primary proof/execution contract:

- **Role / review / delivery gate** (`role: <role_id>`): Claimed via `acdd claim`
  / `acdd_claim` and completed via `acdd submit` / `acdd_submit`. Review gates
  declare `proof: review` and non-empty `contours:`; delivery/handoff gates
  declare `receipt: true` and `save.post: commit` (or `none` in `in_place`).
- **Command gate** (`command: <command_id>`, optionally paired with a `role:`):
  Executed via `acdd verify` / `acdd_verify` (or headless `acdd run`) against the
  registered command slot and `test_filter`. Passing `command` proofs require
  exit code `0` (plus any attached `checks:` such as `code.seam`).
- **Domain action / check gate** (`call: <domain>.<action>`, `checks: [<domain>.<check>]`):
  Invokes `DomainProvider` capabilities registered in `src/state_bridge/` (such
  as `code.seam` reachability, `code.lint` architectural/doc lint, and
  `code.signature_scope` in `src/state_bridge/code_domain/`, or `docs.links` in
  `src/state_bridge/docs_domain.rs`).
  (`docs.invariants` stamps `"enforced": true` / `[ENFORCED]` on invariants backed
  by `acdd-rule` or `acdd.yaml` `architecture.rules`.)
- **Container gate** (`gates: [...]`): Groups sequential child gates under dotted
  hierarchical cursors (e.g., `blueprint_synthesis.synthesize` -> `blueprint_synthesis.review`).
- **Task container** (`- tasks: { when?, gates: [...] }`): Expands its child gate
  sequence for each matching admitted task, scheduled in topological DAG waves
  from `depends_on` / `task_dependencies`.

### Hierarchical gate cursors and `code.seam` checks

Profile containers expand into `gate_nodes` rows (`container`, `root_gate`,
`task`, `task_gate`) addressed by dotted `NodeCursor` identifiers
(`blueprint_synthesis.synthesize`, `<task_ref>.build`, `verify`, `handoff`).
Pre-task root gates run before `- tasks:` blocks unlock; when a pre-task gate
like `blueprint_synthesis.synthesize` submits `proof: blueprint` containing
`tasks.<task_ref>`, `acdd-state` injects strictly that task's slice as Tier 3
(`pre_task_slice`) when claiming that task.

Domain checks wired via `checks: [code.seam]` (implemented in
[`src/state_bridge/code_domain/mod.rs`](../../src/state_bridge/code_domain/mod.rs)) verify
indexed test reachability into admitted production scope (`calls`, `imports`,
`possible_calls`, etc.) and reject out-of-scope public signature callers before
completion.

`code_map_query(operation: "duplicates")` pages duplicate symbol bodies and
repeated normalized code blocks in the indexed graph. At `level: "symbol"`
(default), `duplicates_paged` and MCP `code_map_query(operation: "duplicates")`
default `min_lines` to **5** when the argument is omitted; the CLI
`acdd query run duplicates --min-lines` flag still defaults to **10** unless set
explicitly. **`exact`** whole-symbol groups hash the blank-trimmed raw symbol
body (leading/trailing blank lines removed, indentation preserved). **`block`**
windows hash indent-normalized non-empty lines for each `>= min_lines` slice.
Near clones cluster by name and line similarity. **`block`** matches cover
repeated normalized windows across symbols/files or at non-overlapping positions
within one function (overlapping shifted windows are not double-counted). Block
groups that only repeat an already-reported whole-symbol **`exact`** or
**`similar`** clone are suppressed. See [mcp-tools.md](mcp-tools.md) and
[cli.md](cli.md) for pagination and scope options.

### Independence, Routing, Conditions, and Persistence

- **Independent Worker Enforcement (`independent_from`)**: Claim and submission
  verify that `worker_id` differs from the accepted worker of each named
  predecessor gate (`TASK_REVIEW_NOT_INDEPENDENT`).
- **Forward & Rejection Routing (`pass`, `fail`, `fail_drift`)**: Passing advances sequentially
  or to `pass:`. Rejecting (`--action reject`) or failing routes along `fail:` (on `defect`)
  or `fail_drift:` (on `architectural_drift`, e.g., `rearchitect` -> `build` triggering builder
  context reset) with mandatory structured `RejectionFindings`. Roles with `reuse_on_reject: true`
  instruct the runner/coordinator to return findings to the prior worker on `defect` rejections.
- **Conditional Task Tracks (`when`) & Retry Budgets (`max_attempts`)**: `- tasks:`
  and `task_gate` nodes support boolean `when:` tag expressions evaluated
  against each task's effective `tags` (`tags:` in profile + task YAML). Explicit
  `max_attempts` (default `3`, `>= 1`) caps failures/cycles before escalation.
- **Save & Candidate Immutability (`save`)**: Claim admission records `claim_tree`
  from the scoped candidate tree (`TreeOid`); `save.immutable: true` rejects
  candidate drift at submit (`TASK_CANDIDATE_MISMATCH`); `save.post: commit`
  creates a scoped Git plumbing commit (`Acdd-Claim`) without
  polluting untracked engine paths excluded by `WorkspacePathPolicy`; `save.post: snapshot`
  on leaf task gates writes confined checkpoint refs under `refs/acdd/snapshots/...`
  (see [task snapshots](tasks.md#task-snapshots)).

---

## 3. Embedded Profiles & Repository Overlays

Profiles follow the loading precedence documented in [ACDD profiles](profiles.md)
and [Workspace configuration](configuration.md#declarative-acdd-profile-and-role-configuration).
Repositories typically rely on the automatic workspace default (`.acdd/profile.yaml`
when present, else built-in `code`) and customize `.acdd/profile.yaml` with `version: 2`,
`inherit: code` (or `base` / `audit`), `gates_patch:`, and project
command bindings:

```yaml
version: 2
inherit: code
gates_patch: []

commands:
  test: "cargo test --lib {test_filter}"
  build_check: "cargo clippy --all-targets -- -D warnings && cargo test --lib"
  verify_all: "cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets"
```

---

## Built-in Default Profile Walkthrough (`base.yaml`, `code.yaml`, `audit.yaml`, & `documentation.yaml`)

[`crates/acdd-state/src/profile/embedded/base.yaml`](../../crates/acdd-state/src/profile/embedded/base.yaml)
defines the domain-pure baseline (`gates: []`, 7 core `default_tools`,
universal `core.execution`, `core.review`, and `hitl.pro` guidance, and three
core contours `paths`, `claims`, and `administration`). Embedded
[`code.yaml`](../../crates/acdd-state/src/profile/embedded/code.yaml) (the
built-in code-delivery fallback when `.acdd/profile.yaml` is absent) inherits `base`, adds the three-phase
milestone lifecycle (`commands: build_check` and `verify_all` are marked
`unbound: true`), sets `tags: [default, docs]` (`docs`-tagged tasks skip
`code.seam`), binds the 20 `default_tools`, attaches `code.seam` and `code.lint` checks and
`docs.*` / `code.*` context lenses, adds `code.principles` and `code.tools`, and
automatically strips disabled `code.*` / `docs.*` tools, checks, and lenses when
`modules.code: false` or `modules.docs: false`. Embedded
[`audit.yaml`](../../crates/acdd-state/src/profile/embedded/audit.yaml) inherits `base`
with `git.isolation: in_place` and `save: none`, defining a read-only `recon -> analyze -> triage`
review and security/taint scanning pipeline that propagates upstream stage proofs via
`predecessor_proofs` and writes Markdown reports via `save.report: ".acdd/reports/{run_name}.md"`.
Embedded [`documentation.yaml`](../../crates/acdd-state/src/profile/embedded/documentation.yaml)
inherits `base` with `modules: { required: [docs, code] }`, providing a
compiler-free lifecycle (`blueprint_synthesis` -> tasks `build`/`review`/`deliver`
-> `verify_docs` -> `polish` -> `integration_review` -> `handoff`) gated by
`docs.links` and `code.lint` (`[arch, docs]`).

```text
Phase 1: Pre-Task Root Container (`blueprint_synthesis`)
  └─ synthesize (role: architect, proof: blueprint) ──► review (role: independent_reviewer, fail -> synthesize)
          │
          ▼  (fans out per-task blueprint slices; unlocks `- tasks:` wave)
Phase 2: Per-Task DAG Container (`- tasks:`, executed in topological waves)
  └─ build (role: builder, command: build_check, proof: command, save.post: snapshot)
        └─► review (role: independent_reviewer, proof: review, 6 contours, immutable, fail -> build, fail_drift -> rearchitect)
              └─► deliver (role: independent_reviewer, need [build, review], receipt: true, save.post: commit)
        (rearchitect: role: architect, pass -> build with builder context reset)
          │
          ▼  (all non-cancelled tasks completed)
Phase 3: Post-Task Closure
  └─ run_tests (command: verify_all, proof: command, max_attempts: 3)
       ├─ fail ──► polish (role: builder, command: verify_all, explicit polish scope, pass -> run_tests)
       └─ pass ──► integration_review (role: independent_reviewer, independent_from [polish, tasks.build], need [run_tests], immutable, fail -> polish)
                     └─ pass ──► handoff (role: independent_reviewer, need [run_tests, integration_review], receipt: true, save { immutable, commit, archive, prune })
```

### Roles, Contours, and Guidance Slots in `base.yaml` & `code.yaml`

- **Roles** (`code.yaml`): `architect` (`subagent`, `reuse_on_reject: true`),
  `builder` (`subagent`, `reuse_on_reject: true`), and `independent_reviewer`
  (`subagent`, `reuse_on_reject: false`).
- **Contours**: `paths`, `claims`, and `administration` (in `base.yaml`);
  `architecture` (single-owner reuse, minimal complexity, strict layer
  boundaries, and zero shadow subsystems), `concurrency`, and `project_isolation`
  (in `code.yaml`).
- **Guidance slots**:
  - Universal (`base.yaml`): `core.execution` (admitted scope, proportional
    verification, and `REBUTTAL OVER BLIND REWRITE`), `core.review` (`BURDEN OF
    PROOF` and `acdd_blackboard` deferred routing), and `hitl.pro`.
  - Code & milestone (`code.yaml`): `code.principles`, `code.tools`,
    `milestone.synthesis`, `milestone.blueprint_review`, `milestone.polish`,
    `milestone.review`, `milestone.handoff`, `task.code`, `task.review`, and
    `task.rearchitect` (each gate composes four clean layers: task/stage slot,
    `core.*`, `code.principles` + `code.tools`, and `hitl.pro`).

---

## 4. Headless Execution (`acdd run`)

`acdd run` (`--jobs N`, `--allow-main`, `--dry-run`) drives the active `Run`'s
DAG waves using `crates/acdd-state/src/runner/`:
- Mechanical `command:` / `call:` gates run directly via `verify` + `submit`.
- Role-assigned gates require an explicit `runner:` template on `roles.<role>`
  (otherwise failing closed with `WORKFLOW_RUNNER_REQUIRED`).
- Each spawned runner receives a fresh session fingerprint, isolated process
  group, and rendered `ClaimResp` on stdin; exit status `0` without an accepted
  submission fails closed with `RUNNER_NO_SUBMISSION`.
- See [CLI reference](cli.md#headless-run-execution) and
  [State, tasks & lifecycle](tasks.md) for wave execution and recovery details.

---

## 5. Documentation and instructions co-evolution

A task that changes observable behavior, public interfaces, workflows, configuration, or operational assumptions updates matching documentation and agent instructions during its `build` stage (and includes those paths in its initial `scope`). Follow the [shared skill's task guidance](../../skills/acdd/SKILL.md#acdd-profiles-and-task-operations).
