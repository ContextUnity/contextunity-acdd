---
id: AC-005-unified-swarm-gates-and-peer-board-coordination
title: "Unified swarm gates (mode: linear | parallel | swarm), GateDef.with coalescing, extended when:, and peer blackboard coordination"
doc_type: contract
status: planned
depends_on:
  - AR-002-modularize-db-reader-and-traversal
  - AR-003-unified-lint-engine-and-documentation-profile
  - AC-006-universal-repository-memory-synthesis
guidance:
  hitl: pro
owners:
  - .acdd/workflows/
  - crates/acdd-core/src/
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/state_bridge/
  - src/format/
  - src/cli/
  - src/mcp/
  - tests/acdd/
  - tests/commitment_integrity/state_boundaries.rs
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md
invariants:
  - 'INV-UNIFIED-GATE-MODE: Every GateDef executes under one unified GateMode enum (linear [default], parallel, or swarm) with zero special-cased TasksBlock (- tasks:); task tracks become ordinary named GateDef nodes (e.g. id: implement, mode: swarm, with: spec.tasks | blueprint.tasks) addressed by uniform hierarchical cursors (poll[telegram], implement[core-cleanup].build).'
  - 'INV-MULTI-SOURCE-WITH: Existing GateDef.with (adding zero new from: field) resolves a single reference, pipe-separated fallback chain (spec.tasks | blueprint.tasks), YAML list ([spec.tasks, blueprint.tasks]), { count: N }, or { items, args } for leaf call: gates, evaluating left-to-right across spec.<field>, input.<field>, and <gate_id>.<field> to the first non-empty collection bounded by max_concurrency.'
  - 'INV-EXTENDED-WHEN: Native GateDef.when lifts the root-gate ban in schema.rs/validate.rs and extends workflow/when.rs into a pure boolean predicate (!, &, +, |, parentheses) over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, and diff.*, keeping | strictly boolean OR inside when: while | in with: is left-to-right non-empty collection fallback.'
  - 'INV-LOCKFREE-NON-MUTATING-SWARM: repo.lock acquisition and Git TreeOid snapshot checks apply strictly when save != none; save: none parallel branches and dynamic swarm shards execute concurrently under optimistic SQLite BEGIN IMMEDIATE CAS (claim_revision + node_version).'
  - 'INV-DISJOINT-MUTATING-PARALLELISM: Concurrent save != none branches under mode: parallel or mode: swarm enforce disjoint admitted scope globs (failing closed with TASK_SCOPE_CONFLICT) or execute in isolated per-item/per-branch worktrees integrated deterministically via git_ops/integrate.rs.'
  - 'INV-BOUNDED-SWARM-BARRIER: A mode: parallel container opens all admitted child gates[] simultaneously up to max_concurrency, waits for in-flight non-observer siblings to settle, and aggregates all branch rejection_findings into one consolidated batch before routing once to GateDef.fail; optional observer: true siblings (allowed strictly when save: none) stay non-blocking with minimal observer context, run wait/watch loops, steer on drift, and exit cleanly when all primary branches reach terminal states.'
  - 'INV-MENTION-SCOPED-PEER-WAIT: acdd_blackboard(action: "wait", since_seq, mentions_only) defaults to mentions_only: true so unrelated workers incur zero wakeups or context noise, waking immediately on matching SQLite commit/notify (@<worker_id>, @<node_cursor>, @agent:<agent>, @coordinator, @all) with thread_id and monotonic seq, and optionally accepts watch mode (watch:<n> capped cycles or until-trigger/sentinel) detecting budget_overrun, thrash_loop, scope_drift, and interval_seconds.'
  - 'INV-GROUPED-SAVE-AND-AGENT-SCHEMA: WorkflowDef v2 renames roles/role to agents/agent (adding built-in coordinator: { mode: inline } and structured AgentReusePolicy: never | on_reject | always | { policy, max_reuses?, max_context_tokens? } backed by persisted gate_nodes last_session_id/reuse_count/last_context_tokens and automatic fresh-session reset) and replaces scattered SavePolicy booleans + receipt flags with mutually exclusive save variants (none | readonly | snapshot | commit | handoff: { archive, prune, commit_scope, immutable, scope_mode?: lazy|strict|off, repack?: none|linear|by_task|last|squash, report? }), where save: handoff integrates only mutating swarm/parallel worktrees, commits admitted self.scope atomically, defaults to scope_mode: lazy (with Acdd-Integrated trailers and non-blocking pre-handoff polish), and supports fail-closed worktree-only Git repacking.'
  - 'INV-HIERARCHICAL-GROUP-COORDINATORS: Any linear, parallel, or swarm GateDef acts as a composable group (arbitrarily nested via gates[]) that may bind an optional coordinator: <agent>; each group coordinator binds automatically to thread_id = <container_cursor> and @coordinator/@<group_cursor> mentions, supports bounded hot wait(timeout_seconds) plus persistent session_id handoff, and isolates intra-group chatter from parent or sibling coordinators.'
  - 'INV-CORE-LEAF: Keep pure unified-gate and blackboard DTOs (GateMode, WithSource, NamespaceRoot, BlackboardRecipient, BlackboardWaitReq) in acdd-core free of SQLite, Git, HTTP, or filesystem I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: Every AC-005 capability ships in the same milestone with updated ADR-0003, architecture/reference/runbook docs, executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml examples, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: GIVEN any v2 WorkflowDef or embedded workflow (base, code, documentation, .acdd/workflows/default.yaml, .acdd/workflows/hotfix.yaml), WHEN parsed and validated, THEN TasksBlock (- tasks:) is removed in favor of mode: swarm GateDef nodes with uniform hierarchical cursors, roles/role are renamed to agents/agent with structured reuse ({ policy, max_reuses?, max_context_tokens? } or shorthand never|on_reject|always, rejecting legacy keys with ACDD_WORKFLOW_SCHEMA_INVALID), SavePolicy + receipt collapse into mutually exclusive save variants (including save: handoff with scope_mode: lazy default and worktree-only repack), and pre-handoff polish commits absorb cleanly without TASK_NOT_INTEGRATED false failures.'
  - 'AC-2: GIVEN a mode: swarm gate declaring GateDef.with (single ref, pipe chain spec.tasks | blueprint.tasks, YAML list, { count: N }, or { items, args }), WHEN resolved across spec.<field>, input.<field>, and <gate_id>.<field>, THEN the first non-empty collection materializes deterministic shards in shard order bounded by max_concurrency and propagates typed gate_artifacts into ClaimResp.inputs; malformed refs or invalid namespaces fail closed with ACDD_WORKFLOW_SCHEMA_INVALID.'
  - 'AC-3: GIVEN root, container, or shard gates declaring extended native GateDef.when expressions over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, and diff.*, WHEN evaluated during graph admission or shard materialization, THEN boolean operators (!, &, +, |, parentheses) admit or skip gates deterministically without root-gate bans; invalid syntax or unknown roots fail closed with ACDD_WORKFLOW_SCHEMA_INVALID.'
  - 'AC-4: GIVEN a mode: parallel or mode: swarm container with concurrent children, WHEN save: none branches execute, THEN claim/verify/submit proceed lock-free under SQLite BEGIN IMMEDIATE CAS with zero repo.lock acquisition; WHEN save != none branches overlap in admitted scope without worktree isolation, THEN admission fails closed with TASK_SCOPE_CONFLICT; WHEN non-observer siblings reject, THEN the barrier waits for in-flight siblings and consolidates all rejection_findings into one fail routing while observer: true save: none peers close cleanly on the container-closing sentinel.'
  - 'AC-5: GIVEN SQLite state schema migrated v2 -> v3 with blackboard @mentions, thread_id, and monotonic seq, WHEN a worker calls acdd_blackboard(action: "wait", since_seq, mentions_only) (default mentions_only: true), THEN unrelated traffic causes zero wakeups, matching @mentions (@<worker_id>, @<node_cursor>, @agent:<name>, @coordinator, @all) or watch triggers wake immediately with structured payloads, and nested GateDef.coordinator bindings isolate @coordinator pings to the nearest enclosing group.'
  - 'AC-6: GIVEN the completed AC-005 tree, WHEN audited and verified, THEN ADR-0003, docs/architecture/, docs/reference/{workflows,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/, .acdd/workflows/*.yaml, skills/acdd/references/acdd-workflow-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check, acdd validate, commitment_integrity, and operator sign-off.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
  - agent-instructions
---

# AC-005: Unified swarm gates, `GateDef.with` coalescing, extended `when:`, and peer-to-peer blackboard coordination

## Goal & Non-goals

**Goal.** Make `WorkflowDef` (`version: 2`) in `crates/acdd-state` completely uniform for `v0.8.0` (building on [AC-006](archive/AC-006-universal-repository-memory-synthesis.md) universal `spec.<collection>` items and named `paths.*` sets) by replacing special-cased `- tasks:` (`TasksBlock`) machinery with one orthogonal execution model (`mode: linear | parallel | swarm`), multi-source `GateDef.with` coalescing, extended native `GateDef.when` predicates, grouped `save:` variants (`save: handoff`), `roles -> agents` naming, and direct `@mention` + zero-token `wait` coordination on `acdd_blackboard` under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md) and [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md).

**Non-goals.**
- Universal Spec schema parsing, `paths:` resolution, and `spec_deltas` (owned by [AC-006](archive/AC-006-universal-repository-memory-synthesis.md)).
- HTTP/SSE Web UI, Prometheus `/metrics`, or OTLP export (owned by [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)).
- Deferred `AgentDef.mode: manual` approvals, claim lease TTLs, SQLite `timers`, `acdd tick`, `inbox`, or `outbox` (owned by [AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)).
- Authenticated network/IPC ingress, multi-project hub, or Streamable HTTP MCP (owned by [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)).
- Adding new `Cargo.toml` dependencies or violating `INV-CORE-LEAF` / `INV-STATE-NO-INDEX`.

## Agreed contracts

### 1. Unified 5-milestone system architecture (`AC-006` → `AC-005` → `AC-007` + `AC-008` → `AC-009`)

1. **[AC-006](archive/AC-006-universal-repository-memory-synthesis.md)** (`v0.8.0`): Universal `specs:` schemas (`spec.<collection>`), named `paths.*` sets, `spec_deltas`, and `memory` Specs.
2. **`AC-005`** (`v0.8.0`, this milestone): Uniform `mode: linear | parallel | swarm` gates, `GateDef.with` coalescing, extended `when:`, `agents:` + grouped `save: handoff`, lock-free `save: none` CAS, and `@mention` + zero-token `wait`.
3. **[AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)**: `acdd mcp` split and single-workspace read-only `acdd serve` (Live Gate Board, Architecture Map, Specs/Roadmap/Docs, task-scoped Diff, Prometheus, OTLP).
4. **[AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)**: Zero-daemon reactive core (`AgentDef.mode: manual`, lease TTLs, `timers`, `acdd tick`, `inbox`, `outbox`, secret scrubbing).
5. **[AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)**: `ADR-0013` security layer, multi-project folder registry, `.acdd/acdd.sock`, `--interactive`, `--incoming`, `acdd mcp --http`, and `acdd up`.

### 2. Core engine contracts in `AC-005`

- **Unified `GateMode` & hierarchical cursors**: `linear` (default sequential `gates[]`), `parallel` (simultaneous heterogeneous `gates[]` up to `max_concurrency` joining at one consolidated rejection barrier), and `swarm` (dynamic shards materialized from `with:` with topological `depends_on` wave scheduling). Cursors follow uniform `<container>[<item_id>].<gate>` form (`poll[telegram]`, `implement[core-cleanup].build`).
- **Multi-source `GateDef.with` & extended `GateDef.when`**:
  - `with:` evaluates left-to-right across `spec.<field>`, `input.<field>`, `<gate_id>.<field>`, `{ count: N }`, or `{ items, args }` to the first non-empty collection and binds typed payloads into `ClaimResp.inputs`.
  - `when:` lifts the root-gate ban and evaluates pure boolean expressions (`!`, `&`, `+`, `|`, parentheses) over `tags`, `spec.*`, `input.*`, `<gate_id>.*`, `item.*`, `event.*`, and `diff.*`.
- **Agents & grouped `save:` variants**:
  - `roles:` / `role:` rename to `agents:` / `agent:` (adding built-in `coordinator: { mode: inline }`).
  - Mutually exclusive `save:` variants (`none | readonly | snapshot | commit | handoff: { archive, prune, commit_scope, immutable, scope_mode?: lazy|strict|off, repack?: none|linear|by_task|last|squash, report? }`). `save: handoff` integrates only mutating worktrees, stamps `Acdd-Integrated` trailers, absorbs pre-handoff `polish` cleanly under `scope_mode: lazy`, and supports fail-closed worktree-only repacking.
- **Lock-free non-mutating CAS vs. mutating isolation**: `save: none` executes concurrently under SQLite `BEGIN IMMEDIATE` CAS with zero `repo.lock` or Git tree snapshot; `save != none` enforces disjoint `scope` globs (`TASK_SCOPE_CONFLICT`) or isolated worktrees merged via `git_ops/integrate.rs`.
- **Recursive `GateDef.coordinator`, `observer: true`, and SQLite `v2 -> v3` blackboard `wait`**:
  - Any container may declare `coordinator: <agent>` bound to `thread_id = <container_cursor>`; `@coordinator` resolves `O(depth)` to the nearest enclosing coordinator.
  - `observer: true` (strictly `save: none`) runs non-blocking with lightweight `ClaimResp` and exits cleanly on the container-closing sentinel.
  - SQLite `v2 -> v3` migration (building on live `apply_v2` in `migrations.rs`) adds blackboard `@mentions`, `thread_id`, and monotonic `seq` backing zero-token `acdd_blackboard(action: "wait", since_seq, mentions_only: true)` and optional `watch` triggers.

### 3. Concise workflow YAML examples (`.acdd/workflows/*.yaml`)

```yaml
# Example 1: Task-driven code milestone (with: spec.tasks | blueprint.tasks + save: handoff)
version: 2
agents:
  coordinator: { mode: inline }
  architect:   { mode: subagent, reuse: { policy: on_reject, max_reuses: 1, max_context_tokens: 100000 } }
  builder:     { mode: subagent, reuse: { policy: on_reject, max_reuses: 1, max_context_tokens: 100000 } }
  reviewer:    { mode: subagent, reuse: never }
gates:
  - id: blueprint
    agent: architect
    when: "!spec.tasks"
    pass: implement
  - id: implement
    mode: swarm
    when: "!docs"
    with: spec.tasks | blueprint.tasks
    max_concurrency: 4
    save: none
    gates:
      - { id: build,   agent: builder,  proof: command, save: snapshot, pass: review }
      - { id: review,  agent: reviewer, proof: review,  save: readonly, independent_from: [build], pass: deliver, fail: build }
      - { id: deliver, agent: builder,  save: commit }
    pass: verify
  - id: verify
    agent: builder
    save: readonly
    pass: handoff
  - id: handoff
    agent: coordinator
    need: [verify]
    scope: [specs.memories]
    save:
      handoff: { archive: true, prune: true, commit_scope: true, scope_mode: lazy, repack: last }
```

```yaml
# Example 2: 10-channel reactive monitor (with: input.channels -> with: poll.items)
version: 2
agents:
  watcher:   { mode: subagent }
  triager:   { mode: subagent }
  publisher: { mode: subagent }
gates:
  - id: poll
    agent: watcher
    mode: swarm
    max_concurrency: 10
    with: input.channels
    save: none
    pass: triage
  - id: triage
    agent: triager
    mode: swarm
    with: poll.items
    when: "item.is_new & !item.muted"
    context: [specs.memories]
    save: none
    pass: review
  - id: review
    mode: parallel
    gates:
      - { id: dedup_check,  agent: triager, save: none }
      - { id: policy_check, agent: triager, save: none }
    pass: publish
  - id: publish
    agent: publisher
    save:
      handoff: { archive: false, prune: true, commit_scope: true }
```

## Pre-kickoff Simplification Proposal (`AUTHORITY_GAP` — Pending Operator Review)

> [!NOTE]
> Recorded architectural decision candidate to confirm or trim with the operator before claiming `swarm-contract-and-schema-freeze`. Preserves core `AC-005` foundations (`- tasks:` removal, hierarchical cursors, lock-free `save: none` CAS, `save: handoff`, `roles -> agents`, extended `when:`, and SQLite `v2 -> v3` blackboard `@mentions` / `thread_id` / `seq`).

1. **First-class `swarm:` gate executor alongside `command:` and `agent:`**: Keep `with:` strictly for `call:`/`check:` parameters and express concurrent/sharded gates via `swarm: { over: ..., gates: [...], max_concurrency: N }` while plain `gates: [...]` stays linear.
2. **Operational `acdd swarm` vs. semantic `acdd blackboard`**: Separate operational `acdd swarm claim | pulse | wait` (with `< 5 ms` read-only `pulse`, mandatory `--timeout`, and optional `DomainProvider::swarm_health` hooks) from semantic `acdd blackboard` knowledge messages (`decisions`, `findings`, `blockers`, `notes`).
3. **Lazy `swarm.supervisor` instead of `observer: true`**: Replace `GateDef.observer: true` with `swarm.supervisor: { agent, wake_on, interval_seconds?, max_interventions? }` that consumes `0` tokens when shards progress cleanly and closes automatically on `settled`.

## Consumer & execution path map

| Surface | Canonical owner | Live consumers updated | Fail-closed rejection | Deferred destination |
|---|---|---|---|---|
| `GateMode`, `agents:`, grouped `save: handoff`, SQLite `v2 -> v3` | `crates/acdd-core/src/` + `crates/acdd-state/src/workflow/` + `store/sqlite/migrations.rs` | All 3 embedded workflows (`base`, `code`, `documentation`), `.acdd/workflows/default.yaml`, `.acdd/workflows/hotfix.yaml`, `tests/commitment_integrity/state_boundaries.rs`, `crates/acdd-state/tests/` | Legacy `- tasks:` or `roles:`/`role:` (`ACDD_WORKFLOW_SCHEMA_INVALID`); non-worktree `repack` (`ExternalGitDrift` / `ACDD_WORKFLOW_SCHEMA_INVALID`) | `AC-007` Web UI DAG renderer; `AC-008` `v3 -> v4` migration |
| Parallel barrier, lock-free CAS & mutating worktree isolation | `crates/acdd-state/src/machine/` + `runner/waves.rs` + `git_ops/` | `usecases/commands/claim.rs`, `usecases/submit/`, `usecases/run.rs` | Overlapping `save != none` scope without worktree (`TASK_SCOPE_CONFLICT`); `observer: true` with `save != none` (`ACDD_WORKFLOW_SCHEMA_INVALID`); stale CAS (`BACKEND_CONFLICT`) | `AC-008` `inbox` dispatch into concurrent CAS swarm |
| `GateDef.with` coalescing & extended `when:` | `crates/acdd-state/src/workflow/{when,validate}.rs` + `usecases/` + `context/` | `ClaimResp.inputs` rendering in `context/markdown.rs` and `src/format/` | Invalid predicate syntax or unknown namespace (`ACDD_WORKFLOW_SCHEMA_INVALID`) | — |
| Peer `@mention` routing, `thread_id`, `wait`/`watch`, `GateDef.coordinator` | `crates/acdd-state/src/store/` + `src/cli/` + `src/mcp/` | `src/cli/dispatch_state.rs`, `src/mcp/tools/state_tools.rs`, `docs/`, `skills/acdd/` | Invalid recipient/cursor or non-existent thread (`ACDD_WORKFLOW_SCHEMA_INVALID`) | `AC-008` cold `session_id` wake-on-mention via `inbox`; `AC-007` SSE thread streaming |

## Operational bounds & failure recovery

- **SQLite migration continuity**: Live `crates/acdd-state/src/store/sqlite/migrations.rs` is already at `v2` (`apply_v2`); `AC-005` MUST add idempotent `apply_v3` (`v2 -> v3`) without rewriting `v1`/`v2` steps.
- **Zero busy-polling & bounded `wait`**: Blocking `wait` wakes on SQLite commit/notify or bounded polling with mandatory timeout ceiling; `save: none` shards never acquire `.git/acdd/repo.lock`.
- **Fail-closed Git `repack` & `handoff`**: `repack` executes strictly inside isolated worktrees (refusing dirty/main primary checkouts without `--allow-main`) and preserves tree equivalence (`T_final`) or aborts cleanly without history corruption. When `save: handoff` archives a spec (`archive_spec_with_git`), it MUST atomically rewrite outgoing relative links inside the archived spec (`foo.md` → `../foo.md`, `../...` → `../../...`) and incoming milestone links in `ROADMAP.md` / active `docs/milestones/*.md` in the same handoff commit, and fail closed if post-archive `docs check` detects any broken links.

## Tasks in this milestone

### task: swarm-contract-and-schema-freeze

```yaml
task_ref: swarm-contract-and-schema-freeze
target: "Freeze GateMode, WithSource, NamespaceRoot, and BlackboardRecipient/wait DTOs in acdd-core, replace TasksBlock (- tasks:) with mode/max_concurrency and agents:/grouped save: handoff across WorkflowDef v2 and all embedded/repo profiles, add SQLite v2->v3 migration, and pre-register tests/acdd/swarm.rs"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - docs/milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md
  - .acdd/workflows/default.yaml
  - .acdd/workflows/hotfix.yaml
  - crates/acdd-core/src/
  - crates/acdd-state/src/workflow/schema.rs
  - crates/acdd-state/src/workflow/inherit.rs
  - crates/acdd-state/src/workflow/validate.rs
  - crates/acdd-state/src/workflow/mod.rs
  - crates/acdd-state/src/workflow/embedded/
  - crates/acdd-state/src/store/sqlite/migrations.rs
  - crates/acdd-state/src/store/sqlite/schema.rs
  - crates/acdd-state/tests/
  - tests/commitment_integrity/state_boundaries.rs
  - tests/acdd/mod.rs
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: core-swarm-and-blackboard-dtos
    title: "Add pure GateMode (linear|parallel|swarm), AgentReusePolicy (never|on_reject|always|{ policy, max_reuses, max_context_tokens }), WithSource, NamespaceRoot, BlackboardRecipient, and BlackboardWaitReq DTOs in acdd-core"
  - subtask_ref: replace-tasks-block-and-sqlite-v3
    title: "Remove TasksBlock (- tasks:) from GateDef and embedded profiles in favor of mode/with/when/max_concurrency, add optional GateDef.coordinator, rename roles/role -> agents/agent (adding coordinator inline agent and structured reuse policy), group SavePolicy/receipt into mutually exclusive save variants including save: handoff, lift root-gate when: restriction, and add SQLite v2->v3 migration for blackboard @mentions/thread_id/seq plus gate_nodes last_session_id/reuse_count/last_context_tokens"
  - subtask_ref: align-acdd-profile-and-handoff-scope
    title: "Update .acdd/workflows/default.yaml, .acdd/workflows/hotfix.yaml, embedded workflows, and state_boundaries.rs to agents:/agent: + grouped save: handoff; simplify verify_tasks_integrated / archive_spec_with_git via scope_mode (lazy | strict | off) + Acdd-Integrated trailers + non-blocking pre-handoff polish + automatic post-archive relative link rewriting (archived spec + ROADMAP.md + active docs/milestones/*.md) with post-archive docs check invariant + fail-closed worktree repack modes (none | linear | by_task | last | squash)"
  - subtask_ref: swarm-seam-registration
    title: "Register tests/acdd/swarm.rs in tests/acdd/mod.rs with compiling schema and migration seam matrices"
```

- **Wave**: `0` | **Budget**: `~850` net LOC.
- **Seam & mutation proof (`AC-1`)**: `tests/acdd/swarm.rs` + `tests/commitment_integrity` prove all 3 embedded workflows and `.acdd/workflows/default.yaml` parse under `agents:` + grouped `save: handoff`, SQLite upgrades cleanly `v2 -> v3`, and supplying legacy `- tasks:` or `roles:` fails closed with `ACDD_WORKFLOW_SCHEMA_INVALID`.

### task: parallel-barrier-and-lockfree-cas

```yaml
task_ref: parallel-barrier-and-lockfree-cas
target: "Implement GateDef.mode: linear | parallel simultaneous child admission up to max_concurrency, consolidated barrier rejection, lock-free save: none SQLite CAS, and disjoint-scope or worktree isolation for save != none"
contract_revision: 1
depends_on:
  - swarm-contract-and-schema-freeze
tags:
  - default
scope:
  - crates/acdd-state/src/machine/
  - crates/acdd-state/src/runner/waves.rs
  - crates/acdd-state/src/git_ops/
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: parallel-claim-and-barrier-join
    title: "Expose all admitted child gates up to max_concurrency in acdd_state(next) under mode: parallel, validate observer: true strictly with save: none + lightweight ClaimResp, support atomic worker self-claim of explicit or next unclaimed ready shards under SQLite CAS, enforce AgentReusePolicy (parsing optional runner JSON usage.input_tokens -> last_context_tokens, resetting session_id when reuse_count >= max_reuses or last_context_tokens > max_context_tokens, surfacing session_action fresh|reuse + reason in dispatch/ClaimResp, and falling back to max_reuses when token telemetry is absent), and join non-observer branches at container barrier with consolidated rejection_findings"
  - subtask_ref: lockfree-save-none-and-disjoint-git
    title: "Gate repo.lock and Git TreeOid snapshots strictly on save != none while running save: none branches lock-free under SQLite CAS"
  - subtask_ref: mutating-parallel-worktree-integration
    title: "Enforce disjoint scope globs (failing closed with TASK_SCOPE_CONFLICT) or isolate concurrent save != none branches into worktrees merged via git_ops/integrate.rs"
```

- **Wave**: `1` | **Budget**: `~750` net LOC.
- **Seam & mutation proof (`AC-4`)**: `tests/acdd/swarm.rs` proves concurrent `save: none` claims succeed while holding an external `.git/acdd/repo.lock`, overlapping `save != none` branches reject with `TASK_SCOPE_CONFLICT`, and two rejecting parallel branches merge into one consolidated `rejection_findings` batch.

### task: swarm-with-coalescing-and-extended-when

```yaml
task_ref: swarm-with-coalescing-and-extended-when
target: "Resolve multi-source coalescing GateDef.with across spec.<field>, input.<field>, <gate_id>.<field>, { count: N }, and { items, args } into dynamic mode: swarm shards with uniform hierarchical cursors, topological wave scheduling, extended boolean when: predicates, and ClaimResp.inputs propagation"
contract_revision: 1
depends_on:
  - parallel-barrier-and-lockfree-cas
tags:
  - default
scope:
  - crates/acdd-state/src/workflow/when.rs
  - crates/acdd-state/src/workflow/validate.rs
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/src/context/
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: multi-source-with-coalescing-resolver
    title: "Evaluate single refs, pipe chains, YAML lists, { count: N }, and { items, args } in GateDef.with left-to-right across spec.*, input.*, and flattened <gate_id>.* shard proofs"
  - subtask_ref: extended-when-and-dynamic-swarm-waves
    title: "Extend native when: evaluator over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, diff.* across root/container/shard gates and schedule mode: swarm topological depends_on waves with ClaimResp.inputs propagation"
```

- **Wave**: `2` | **Budget**: `~650` net LOC.
- **Seam & mutation proof (`AC-2`, `AC-3`)**: `tests/acdd/swarm.rs` proves `with: spec.tasks | blueprint.tasks` falls back deterministically when `spec.tasks` is empty, `when: "item.is_new & !item.muted"` filters shards, and an unknown `when:` namespace fails validation with `ACDD_WORKFLOW_SCHEMA_INVALID`.

### task: peer-blackboard-mentions-and-reactive-wait

```yaml
task_ref: peer-blackboard-mentions-and-reactive-wait
target: "Implement @mention recipient routing, thread_id filtering, monotonic seq, zero-token acdd_blackboard wait (mentions_only: true default), and co-evolve ADR-0003, architecture/reference/runbook docs, executable workflow/profile examples, skills, ROADMAP.md, and CHANGELOG.md"
contract_revision: 1
depends_on:
  - swarm-with-coalescing-and-extended-when
tags:
  - docs
  - default
scope:
  - crates/acdd-state/src/store/
  - src/cli/
  - src/mcp/
  - docs/adr/0003-unified-profile-graph-and-state-machine.md
  - docs/architecture/
  - docs/reference/workflows.md
  - docs/reference/tasks.md
  - docs/reference/cli.md
  - docs/reference/mcp-tools.md
  - docs/reference/configuration.md
  - docs/runbooks/
  - .acdd/workflows/
  - skills/acdd/SKILL.md
  - skills/acdd/references/acdd-workflow-examples/
  - ROADMAP.md
  - CHANGELOG.md
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: mention-routing-and-blocking-wait
    title: "Implement @mention recipient indexing (including relative @coordinator -> nearest enclosing GateDef.coordinator and @<container_cursor> scoping), thread_id queries, container completion sentinel for coordinators/observers, and zero-token wait/watch(since_seq, mentions_only, timeout_seconds?, watch?) waking on SQLite commit/notify or universal triggers"
  - subtask_ref: adr-docs-skills-and-examples-coevolution
    title: "Update ADR-0003, architecture/reference/runbook pages, executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate"
```

- **Wave**: `3` | **Budget**: `~750` net LOC (incl. docs/examples).
- **Seam & mutation proof (`AC-5`, `AC-6`)**: `tests/acdd/swarm.rs` proves a post mentioning `@worker-b` never wakes `@worker-a` (`mentions_only: true`), `@coordinator` resolves strictly to the nearest enclosing group coordinator, and `acdd docs check` + `acdd validate` pass cleanly.

### task: human-guided-gate-proof-and-logic-audit

```yaml
task_ref: human-guided-gate-proof-and-logic-audit
target: "Conduct a human-in-the-loop walkthrough verifying every gate interaction, proof handoff, save/repack transition, context tier, and state-machine logic branch end to end with the operator before closing AC-005"
contract_revision: 1
depends_on:
  - peer-blackboard-mentions-and-reactive-wait
tags:
  - default
scope:
  - crates/acdd-state/src/
  - src/state_bridge/
  - src/format/
  - .acdd/workflows/default.yaml
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: gate-and-proof-handoff-walkthrough
    title: "Walk through every gate transition, proof artifact propagation (predecessor_proofs, blueprints, reviews, receipts), and budget/independence guard interactively with the human operator"
  - subtask_ref: profile-logic-and-edge-case-signoff
    title: "Verify every profile logic element (mode, with, when, save/handoff/repack, @mentions, wait, and ClaimResp context tiers) against live rendered outputs and record operator sign-off"
  - subtask_ref: terminal-test-polish-graph-audit
    title: "Audit all post-build-delivery gates (run_tests, polish, integration_review, handoff), failure/retry transitions, command orchestration, and escalation rules end to end with the operator"
```

- **Wave**: `4` | **Budget**: `~250` net LOC (remediation polish only).
- **Seam & mutation proof (`AC-1`..`AC-6`)**: Operator-signed walkthrough covering live rendered `ClaimResp`, post-build-delivery transitions (`run_tests`, `polish`, `integration_review`, `handoff`), and `acdd validate` + `tests/acdd/swarm.rs`.

## Negative boundaries

- No second `from:` field on `GateDef` (reuse `with:` or confirmed `swarm.over:` from Pre-kickoff decision).
- No `- tasks:` (`TasksBlock`) compatibility shim after cutover and no silent fallback accepting legacy `roles:`/`role:` without explicit rejection.
- No `repo.lock` acquisition or Git `TreeOid` snapshot on `save: none` shards.
- No `observer: true` gate permitted with `save != none`.
- No `acdd-index` imports inside `crates/acdd-state` (`INV-STATE-NO-INDEX`), no I/O in `crates/acdd-core` (`INV-CORE-LEAF`), and no new `Cargo.toml` dependencies.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-1`..`AC-6` map 1:1 across Waves 0–4 (`tests/acdd/swarm.rs`, `tests/commitment_integrity/state_boundaries.rs`, docs/examples, and operator sign-off).
- **Correctness**: Every `AC-*` names trigger, expected behavior, fail-closed error (`ACDD_WORKFLOW_SCHEMA_INVALID`, `TASK_SCOPE_CONFLICT`, `BACKEND_CONFLICT`), and seam mutation proof; SQLite migration aligns to live `v2 -> v3`.
- **Coherence**: Reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md), and [AC-006](archive/AC-006-universal-repository-memory-synthesis.md); preserves the Pre-kickoff `AUTHORITY_GAP` decision candidate explicitly and keeps sequential waves free of concurrent scope collisions.
