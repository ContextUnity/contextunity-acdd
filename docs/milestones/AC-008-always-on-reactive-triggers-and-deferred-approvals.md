---
id: AC-008-always-on-reactive-triggers-and-deferred-approvals
title: "Zero-daemon reactive core: deferred manual approvals, lease TTLs, SQLite timers, acdd tick, inbox, and idempotent outbox"
doc_type: contract
status: planned
depends_on:
  - AC-005-unified-swarm-gates-and-peer-board-coordination
  - AC-006-universal-repository-memory-synthesis
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/
  - crates/acdd-state/src/
  - src/state_bridge/
  - src/cli/
  - src/mcp/
  - tests/acdd/
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - .acdd/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-008-always-on-reactive-triggers-and-deferred-approvals.md
invariants:
  - 'INV-ZERO-TOKENS-AT-REST: Sleeping runs (awaiting AgentDef.mode: manual approval, escalated review, parked group coordinators/cycle workers awaiting @mention resume, parent runs paused on waiting_on_run_id child runs, or timer wake-up) persist cursor and session_id state in SQLite and exit cleanly with zero background LLM polling, idle RAM, or token consumption; acdd tick runs as a bounded one-shot pulse and exits immediately.'
  - 'INV-PAUSE-AND-CALL-CHILD-RUNS: Cross-workflow chaining uses isolated child Runs (parent_run_id + waiting_on_run_id in SQLite v3 -> v4, bounded by max_run_depth) rather than nested workflow YAML merging; both declarative gate spawn_run and ad-hoc acdd start --parent-run execute one canonical pause_and_spawn_child_run transition that pauses the parent gate at zero tokens while the child Run executes its own workflow, specs, worktree, and handoff, automatically waking the parent with the child receipt/output specs and integrating commits strictly according to GateDef.save (save: none|readonly = read-only; save: commit = integrate via git_ops/integrate.rs).'
  - 'INV-SERIALIZED-DISPATCH-AND-DEDUP: Triggers enter one durable inbox table with explicit dedup_key idempotency and coalesce_window burst folding before dispatching runs through AC-005 concurrent CAS swarm execution (acquiring repo.lock strictly when save != none, never blocking save: none parallel channels), plus waking parked GateDef.coordinator or cycle worker sessions via persisted session_id resume on matching @mentions.'
  - 'INV-SECRET-ISOLATION: Agent runner subprocesses never inherit unapproved host secret variables; sensitive environment credentials (*_KEY, *_TOKEN, *_SECRET, *_PASSWORD) are scrubbed unless explicitly admitted via AgentDef.env_allowlist or external proxy.'
  - 'INV-OUTBOX-IDEMPOTENCY-AND-RESTRAINT: Outgoing notifications fire only from durable outbox rows tied to discrete state transitions (approval_needed, escalated, run_completed), deduplicated per (run_id, node_id, claim_revision, event_kind) and paced by digest windows.'
  - 'INV-CORE-LEAF: Keep pure reactive DTOs (InboxMessage, TimerSpec, OutboxMessage, ApprovalDecision, TickSummary) in acdd-core free of filesystem, SQLite, HTTP, or process I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-008 ships in the same milestone with updated ADR-0003 and ADR-0005 (zero-daemon AgentDef.mode: manual, wake-on-mention coordinator/worker session resume, lease TTLs, SQLite timers, one-shot acdd tick, inbox -> concurrent swarm dispatch, outbox, secret scrubbing), architecture/reference/runbook docs (including systemd.timer / launchd / cron acdd tick setup), executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: GIVEN a gate bound to AgentDef.mode: manual or an escalated review gate, WHEN acdd run reaches it, THEN the run persists its cursor and exits with code 0 at zero tokens; WHEN a human invokes CLI/MCP approve or reject with matching claim_revision, THEN execution resumes or rewinds cleanly (mismatched revisions fail closed with STALE_CLAIM_REVISION), and spawner.rs strips *_KEY/*_TOKEN/*_SECRET/*_PASSWORD env vars by default unless listed in AgentDef.env_allowlist.'
  - 'AC-2: GIVEN SQLite state schema migrated v3 -> v4 (building on AC-005 v2 -> v3) with gate_claims.lease_expires_at and timers, WHEN a remote/hung claim passes lease_expires_at without verify or AC-005 wait renewal, THEN acdd tick expires the stale claim cleanly without corrupting active worktrees; recurring cron timers (such as AC-006 memory-dream) and one-shot snooze/defer wake-ups enqueue due triggers without busy polling.'
  - 'AC-3: GIVEN a workspace scheduled externally via systemd.timer, launchd, cron, or CI, WHEN one-shot acdd tick runs, THEN it evaluates due timers, expires stale leases, dispatches queued inbox items, and flushes pending outbox rows within bounded milliseconds and exits with code 0 and 0 idle RAM (zero persistent background daemon).'
  - 'AC-4: GIVEN bursty CLI or timer triggers sharing a dedup_key or falling within coalesce_window, WHEN enqueued into inbox, THEN duplicates fold atomically into one payload bound to input.<field> (with: input.events | event.*) and dispatch through AC-005 concurrent CAS swarm execution (holding repo.lock strictly when save != none), while outbox delivers deduplicated digest-paced notifications strictly on approval_needed, escalated, and run_completed transitions.'
  - 'AC-5: GIVEN a parent Run paused on a child Run via the unified pause_and_spawn_child_run transition (either declarative gate spawn_run: { workflow, specs?, goal? } with GateDef.save or ad-hoc acdd start [SPEC ...] --workflow <W> --parent-run <run_id>), WHEN the child Run executes its own workflow and completes handoff, THEN the parent Run wakes automatically with the child receipt and produced spec paths (integrating mutating commits strictly when save: commit and keeping save: none|readonly non-mutating), failed/cancelled children route to parent fail/escalation, and exceeding max_run_depth fails closed with ACDD_WORKFLOW_SCHEMA_INVALID.'
  - 'AC-6: GIVEN the completed AC-008 tree, WHEN audited and verified, THEN ADR-0003, ADR-0005, docs/architecture/, docs/reference/{workflows,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/ (including systemd.timer / launchd / cron acdd tick setup), .acdd/workflows/*.yaml, skills/acdd/references/acdd-workflow-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md pass acdd docs check and acdd validate with zero errors.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
  - agent-instructions
---

# AC-008: Zero-daemon reactive core: deferred manual approvals, lease TTLs, SQLite timers, `acdd tick`, inbox, and idempotent outbox

## Goal & Non-goals

**Goal.** Extend the unified `crates/acdd-state` engine (composing directly with [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) and [AC-006](AC-006-universal-repository-memory-synthesis.md)) from pull-driven runs into a **zero-daemon, sleep-first reactive core** where parked runs consume `0` tokens and `0` idle RAM, powered by `AgentDef.mode: manual`, subprocess secret scrubbing, claim lease TTLs, SQLite `v3 -> v4` durable `timers`/`inbox`/`outbox`, and a fast one-shot `acdd tick` pulse.

**Non-goals.**
- Network webhooks (`POST /events` `--incoming`), authenticated Web UI approvals (`--interactive`), `.acdd/acdd.sock`, Streamable HTTP `acdd mcp --http`, multi-project folder registry, or the long-running `acdd up` supervisor (all deferred to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md) behind `ADR-0013`).
- HTTP/SSE observability or Prometheus `/metrics` (owned by [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)).
- Persistent resident polling daemons or new cron/HTTP crate dependencies in `Cargo.toml`.

## Agreed contracts

1. **Deferred Human Approvals & Secret Isolation**:
   - `AgentExecutionMode::Manual` (`AgentDef.mode: manual`) and escalated gates park runs cleanly with exit code `0`, resuming solely via CLI/MCP `approve` / `reject` with `claim_revision` CAS checks (`STALE_CLAIM_REVISION` on mismatch).
   - `crates/acdd-state/src/runner/spawner.rs` strips `*_KEY`, `*_TOKEN`, `*_SECRET`, and `*_PASSWORD` host environment variables by default unless explicitly listed in `AgentDef.env_allowlist`.
2. **Claim Lease TTL, Durable Timers (`v3 -> v4`), & One-Shot `acdd tick`**:
   - `gate_claims.lease_expires_at` derives from `limits.claim_ttl_seconds`, renews on `verify` and `AC-005` `wait` activity, and auto-expires hung remote claims cleanly.
   - Durable `timers` table (added in SQLite migration `v3 -> v4` on top of `AC-005`'s `v2 -> v3`) schedules recurring cron expressions (such as `AC-006` `memory-dream`) and one-shot `snooze` / `defer` wake-ups.
   - `acdd tick` runs one bounded pulse (due timers → expired leases → queued inbox → pending outbox) and exits immediately (`0` idle RAM).
3. **Durable Inbox & Concurrent CAS Swarm Dispatch**:
   - CLI (`acdd event`) and timer triggers enqueue into `inbox` with atomic `dedup_key` idempotency and `coalesce_window` burst folding, binding payloads to `input.<field>` (`with: input.events | event.*`) and waking parked `GateDef.coordinator` or worker sessions via persisted `session_id` resume on matching `@mentions`.
   - `repo.lock` is acquired strictly when `save != none`, never blocking `save: none` parallel channels.
4. **Idempotent Outbox**:
   - Transactional `outbox` rows emit strictly on `approval_needed`, `escalated`, and `run_completed` transitions, deduplicated per `(run_id, node_id, claim_revision, event_kind)` with bounded retry backoff and anti-spam digest pacing.
5. **Pause-and-Call Child Runs (`parent_run_id` + `waiting_on_run_id`)**:
   - Cross-workflow invocation never merges nested workflow YAML graphs: both declarative `GateDef.spawn_run: { workflow, specs?, goal? }` and ad-hoc `acdd start [SPEC ...] --workflow <W> --parent-run <run_A>` execute **one canonical `pause_and_spawn_child_run` transition** that parks the active parent gate (`waiting_on_run_id`, 0 tokens) while a first-class child `Run` executes its own workflow, specs, worktree, and `handoff`.
   - Commit integration is governed strictly by existing `GateDef.save` (`save: none | readonly` = read-only, zero Git integration; `save: commit` = integrate child commit back via `git_ops/integrate.rs`). Bounded by `max_run_depth` (default `4`, fail-closed); child completion wakes the parent automatically via `inbox`/`acdd tick` with the child's terminal receipt and produced spec paths.

## Consumer & execution path map

| Surface | Canonical owner | Live consumers updated | Fail-closed rejection | Deferred destination |
|---|---|---|---|---|
| `AgentDef.mode: manual` & `spawner.rs` scrubbing | `crates/acdd-state/src/workflow/` + `machine/` + `runner/spawner.rs` | `usecases/`, `src/cli/`, `src/mcp/` (`approve`/`reject`), `tests/acdd/always_on.rs` | Stale approval (`STALE_CLAIM_REVISION`); non-manual target (`ACDD_WORKFLOW_SCHEMA_INVALID`); scrubbed secret absent from child env | `AC-009` `--interactive` Web UI `Approve/Reject` buttons |
| Lease TTL, SQLite `v3 -> v4` `timers`, `acdd tick` | `crates/acdd-state/src/timers.rs` + `store/sqlite/migrations.rs` | `store/`, `usecases/`, `src/cli/` (`acdd tick`), `src/mcp/` | Invalid cron expression (`ACDD_WORKFLOW_SCHEMA_INVALID`); concurrent lease race (`BACKEND_CONFLICT`) | `AC-009` `acdd up` interruptible `sleep_until(next_timer)` loop |
| Durable `inbox` & CAS swarm dispatch | `crates/acdd-state/src/inbox.rs` + `store/` + `usecases/` | `src/state_bridge/`, `src/cli/` (`acdd event`) | Duplicate `dedup_key` folded idempotently; invalid payload rejected (`ACDD_WORKFLOW_SCHEMA_INVALID`) | `AC-009` `.acdd/acdd.sock` & `--incoming POST /events` writers |
| Idempotent `outbox` | `crates/acdd-state/src/outbox.rs` + `store/` | `src/cli/` (`acdd tick`), `docs/runbooks/` | Unsupported transition kind rejected; failed delivery backs off boundedly without duplicating rows | — |

## Operational bounds & failure recovery

- **SQLite migration continuity**: Live `migrations.rs` is at `v2`, `AC-005` adds `v2 -> v3`, so `AC-008` adds idempotent `v3 -> v4` (`lease_expires_at`, `timers`, `inbox`, `outbox`).
- **Crash-safe idempotency**: Re-running `acdd tick` after a mid-tick crash never double-fires a `dedup_key` inbox item or duplicates an outbox `(run_id, node_id, claim_revision, event_kind)` notification.
- **Strict secret boundary**: Secret scrubbing runs unconditionally in `spawner.rs` before `Command::spawn`, regardless of profile inheritance; allowlisted names must be exact env keys (no wildcard `*`).

## Tasks in this milestone

### task: deferred-human-gates-and-sandbox-secret-scrubbing

```yaml
task_ref: deferred-human-gates-and-sandbox-secret-scrubbing
target: "Add AgentDef.mode: manual with clean park-and-sleep run suspension, CLI/MCP approve/reject resume paths, strict secret env scrubbing in spawner.rs, and register tests/acdd/always_on.rs"
subtasks:
- subtask_ref: manual-mode-and-park-on-approval
  title: "Support AgentDef.mode: manual and park acdd run cleanly with exit code 0 when awaiting human approval or escalated resolution"
- subtask_ref: cli-mcp-approve-reject-resume
  title: "Resume parked nodes strictly via CLI/MCP approve and reject operations with CAS claim_revision verification"
- subtask_ref: spawner-secret-env-scrubbing
  title: "Strip *_KEY, *_TOKEN, *_SECRET, *_PASSWORD environment variables by default in spawner.rs unless explicitly admitted via AgentDef.env_allowlist, and register tests/acdd/always_on.rs in tests/acdd/mod.rs"
contract_revision: 1
tags:
- default
depends_on: []
scope:
- crates/acdd-core/src/
- crates/acdd-state/src/workflow/
- crates/acdd-state/src/machine/
- crates/acdd-state/src/usecases/
- crates/acdd-state/src/runner/spawner.rs
- src/cli/
- src/mcp/
- tests/acdd/mod.rs
- tests/acdd/always_on.rs
status: planned
```

- **Wave**: `0` | **Budget**: `~550` net LOC.
- **Seam & mutation proof (`AC-1`)**: `tests/acdd/always_on.rs` proves a `mode: manual` gate parks `acdd run` with exit `0`, stale `claim_revision` approval fails with `STALE_CLAIM_REVISION`, and child processes never receive un-allowlisted `*_SECRET`/`*_TOKEN` env vars.

### task: claim-lease-ttl-timers-and-acdd-tick

```yaml
task_ref: claim-lease-ttl-timers-and-acdd-tick
target: "Add lease_expires_at auto-expiry on gate_claims, durable timers via SQLite migration v3 -> v4, and the zero-daemon one-shot acdd tick CLI pulse command"
subtasks:
- subtask_ref: claim-lease-ttl-and-heartbeat
  title: "Persist lease_expires_at from limits.claim_ttl_seconds, renew leases on verify and AC-005 wait activity, and expire hung remote claims cleanly"
- subtask_ref: cron-and-agent-snooze-timers
  title: "Persist recurring cron schedules and one-shot snooze/defer wake-ups in SQLite timers (migration v3 -> v4) and enqueue inbox events without busy polling"
- subtask_ref: oneshot-acdd-tick-pulse
  title: "Implement one-shot acdd tick checking due timers, expired leases, queued inbox items, and pending outbox deliveries in milliseconds and exiting cleanly with 0 idle RAM"
contract_revision: 1
tags:
- default
depends_on:
- deferred-human-gates-and-sandbox-secret-scrubbing
scope:
- crates/acdd-state/src/store/sqlite/migrations.rs
- crates/acdd-state/src/store/
- crates/acdd-state/src/workflow/
- crates/acdd-state/src/timers.rs
- crates/acdd-state/src/usecases/
- src/cli/
- src/mcp/
- tests/acdd/always_on.rs
status: planned
```

- **Wave**: `1` | **Budget**: `~600` net LOC.
- **Seam & mutation proof (`AC-2`, `AC-3`)**: `tests/acdd/always_on.rs` proves SQLite upgrades `v3 -> v4`, an expired `lease_expires_at` claim is released by `acdd tick` while a renewed claim stays locked, and malformed cron syntax fails closed with `ACDD_WORKFLOW_SCHEMA_INVALID`.

### task: durable-inbox-and-swarm-dispatch

```yaml
task_ref: durable-inbox-and-swarm-dispatch
target: "Implement the durable inbox table, CLI/timer event enqueue, atomic dedup_key and coalesce_window burst folding, and concurrent CAS swarm dispatch in acdd-state"
subtasks:
- subtask_ref: inbox-ingest-and-coalescing
  title: "Enqueue local CLI/timer trigger payloads into inbox with atomic dedup_key idempotency and coalesce_window burst folding"
- subtask_ref: concurrent-cas-swarm-inbox-dispatch
  title: "Dispatch coalesced inbox items through AC-005 concurrent CAS swarm execution (input.<field>) and wake parked GateDef.coordinator or worker sessions via persisted session_id resume on matching @mentions, holding repo.lock strictly when save != none"
- subtask_ref: pause-and-call-child-runs
  title: "Add parent_run_id and waiting_on_run_id (SQLite v3 -> v4), max_run_depth guard, and one unified pause_and_spawn_child_run transition serving both declarative GateDef.spawn_run (governed by GateDef.save) and ad-hoc acdd start --parent-run with automatic wake-up and receipt/spec handback"
contract_revision: 1
tags:
- default
depends_on:
- claim-lease-ttl-timers-and-acdd-tick
scope:
- crates/acdd-state/src/inbox.rs
- crates/acdd-state/src/store/
- crates/acdd-state/src/usecases/
- src/state_bridge/
- src/cli/
- tests/acdd/always_on.rs
status: planned
```

- **Wave**: `2` | **Budget**: `~480` net LOC.
- **Seam & mutation proof (`AC-4`)**: `tests/acdd/always_on.rs` proves duplicate `dedup_key` triggers fold into one inbox dispatch, `save: none` parallel inbox channels succeed even when `.git/acdd/repo.lock` is held, and matching `@mention` resumes the persisted `session_id`.

### task: idempotent-outbox-and-notification-restraint

```yaml
task_ref: idempotent-outbox-and-notification-restraint
target: "Add a durable outbox table and paced dispatcher emitting deduplicated notifications strictly on approval_needed, escalated, and run_completed transitions, and co-evolve ADR-0003/ADR-0005, docs, systemd.timer/launchd runbooks, workflow/profile examples, skills, ROADMAP.md, and CHANGELOG.md"
subtasks:
- subtask_ref: state-transition-outbox-and-backoff
  title: "Enqueue outbox items transactionally with state transitions, deduplicate by (run_id, node_id, claim_revision, event_kind), and deliver via acdd tick with bounded retry backoff and digest pacing"
- subtask_ref: always-on-adr-docs-skills-and-examples
  title: "Update ADR-0003, ADR-0005, architecture/reference pages (profiles, tasks, cli, mcp-tools, configuration), runbooks (incl. systemd.timer / launchd / cron acdd tick setup), executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate"
contract_revision: 1
tags:
- docs
- default
depends_on:
- durable-inbox-and-swarm-dispatch
scope:
- crates/acdd-state/src/outbox.rs
- crates/acdd-state/src/store/
- src/cli/
- docs/adr/0003-unified-profile-graph-and-state-machine.md
- docs/adr/0005-content-addressed-git-snapshots-and-integration.md
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
- tests/acdd/always_on.rs
status: planned
```

- **Wave**: `3` | **Budget**: `~520` net LOC (incl. docs/runbooks).
- **Seam & mutation proof (`AC-4`, `AC-5`)**: `tests/acdd/always_on.rs` proves outbox emits once per `(run_id, node_id, claim_revision, event_kind)` across retried `acdd tick` pulses and rejects non-admitted event kinds; `acdd docs check` and `acdd validate` pass cleanly.

## Negative boundaries

- No resident daemon process, HTTP listener, webhook endpoint, or Unix socket in `AC-008` (all transport belongs to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)).
- No wildcard `env_allowlist: ["*"]` bypassing `spawner.rs` secret scrubbing.
- No LLM polling loops while parked (`INV-ZERO-TOKENS-AT-REST`).
- No `acdd-index` imports inside `crates/acdd-state` (`INV-STATE-NO-INDEX`), no I/O in `crates/acdd-core` (`INV-CORE-LEAF`), and no new `Cargo.toml` dependencies.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-1`..`AC-5` map 1:1 across Waves 0–3 to `tests/acdd/always_on.rs` and co-evolved ADR/runbook/example deliverables.
- **Correctness**: Every `AC-*` verifies trigger, idempotent/CAS outcome, fail-closed rejection (`STALE_CLAIM_REVISION`, `ACDD_WORKFLOW_SCHEMA_INVALID`, `BACKEND_CONFLICT`), and seam mutation proof; SQLite migration is sequenced cleanly at `v3 -> v4`.
- **Coherence**: Reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0005](../adr/0005-content-addressed-git-snapshots-and-integration.md), [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md), and [AC-006](AC-006-universal-repository-memory-synthesis.md), using unified `AgentDef` naming throughout and strictly sequential zero-overlap waves.
