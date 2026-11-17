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
  - docs/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-008-always-on-reactive-triggers-and-deferred-approvals.md
invariants:
  - 'INV-ZERO-TOKENS-AT-REST: Sleeping runs (awaiting RoleDef.mode: manual approval, escalated review, or timer wake-up) persist cursor state in SQLite and exit cleanly with zero background LLM polling, idle RAM, or token consumption; acdd tick runs as a bounded one-shot pulse and exits immediately.'
  - 'INV-SERIALIZED-DISPATCH-AND-DEDUP: Triggers enter one durable inbox table with explicit dedup_key idempotency and coalesce_window burst folding before dispatching runs through AC-005 concurrent CAS swarm execution (acquiring repo.lock strictly when save != none, never blocking save: none parallel channels).'
  - 'INV-SECRET-ISOLATION: Role runner subprocesses never inherit unapproved host secret variables; sensitive environment credentials (*_KEY, *_TOKEN, *_SECRET, *_PASSWORD) are scrubbed unless explicitly admitted via RoleDef.env_allowlist or external proxy.'
  - 'INV-OUTBOX-IDEMPOTENCY-AND-RESTRAINT: Outgoing notifications fire only from durable outbox rows tied to discrete state transitions (approval_needed, escalated, run_completed), deduplicated per (run_id, node_id, claim_revision, event_kind) and paced by digest windows.'
  - 'INV-CORE-LEAF: Keep pure reactive DTOs (InboxMessage, TimerSpec, OutboxMessage, ApprovalDecision, TickSummary) in acdd-core free of filesystem, SQLite, HTTP, or process I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-008 ships in the same milestone with updated ADR-0003 and ADR-0005 (zero-daemon RoleDef.mode: manual, lease TTLs, SQLite timers, one-shot acdd tick, inbox -> concurrent swarm dispatch, outbox, secret scrubbing), architecture/reference/runbook docs (including systemd.timer / launchd / cron acdd tick setup), executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: RoleDef.mode: manual and escalated gates park runs cleanly with exit code 0 at zero tokens and resume via CLI/MCP approve/reject, while spawner.rs scrubs secret env vars unless allowed by env_allowlist.'
  - 'AC-2: gate_claims.lease_expires_at auto-expires hung remote claims, renews on verify and AC-005 blackboard wait activity, and SQLite v2 -> v3 timers schedule recurring cron (e.g. AC-006 memory-dream) and one-shot snooze wake-ups.'
  - 'AC-3: One-shot acdd tick checks due timers, expired claim leases, queued inbox items, and pending outbox deliveries in milliseconds and exits cleanly with 0 idle RAM for systemd.timer, launchd, cron, or CI scheduling.'
  - 'AC-4: CLI/timer enqueue persists deduplicated/coalesced inbox triggers bound to input.<field> (with: input.events | event.*) and dispatches through AC-005 concurrent CAS swarm execution without blocking save: none channels, while durable outbox delivers deduplicated digest-paced notifications strictly on approval_needed, escalated, and run_completed transitions.'
  - 'AC-5: ADR-0003, ADR-0005, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/ (including systemd.timer / launchd acdd tick setup), docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
---

# AC-008: Zero-daemon reactive core: deferred manual approvals, lease TTLs, SQLite timers, `acdd tick`, inbox, and idempotent outbox

## Outcome and purpose

Extend the unified `crates/acdd-state` profile engine (composing cleanly with
[AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) and
[AC-006](AC-006-universal-repository-memory-synthesis.md)) from pull-driven execution
into a **zero-daemon, sleep-first reactive core** where sleeping runs consume zero
tokens and zero idle RAM:

1. **Deferred Human Approvals & Secret Isolation**: Introduce `RoleExecutionMode::Manual` (`RoleDef.mode: manual`)
   and park-and-sleep handling for `escalated` or protected-path approval gates,
   paired with subprocess environment secret scrubbing in `crates/acdd-state/src/runner/spawner.rs` (`env_allowlist`)
   and explicit wake-up via CLI/MCP (`acdd approve` / `acdd reject` / `acdd_state`).
2. **Claim Lease TTL, Durable Timers & One-Shot `acdd tick`**: Add TTL leases (`lease_expires_at`) on
   `gate_claims` plus a durable `timers` table via schema migration `v2 -> v3` in
   `crates/acdd-state/src/store/sqlite/migrations.rs` (building on AC-005's `v1 -> v2`
   peer blackboard schema) for recurring schedules (such as
   off-hot-path `memory-dream` consolidation from AC-006) and agent-initiated
   one-shot wake-ups (`snooze` / `defer`), renewing active claim leases during
   AC-005 `acdd_blackboard(action: "wait")` parks. Pair this with a fast,
   zero-daemon one-shot **`acdd tick`** CLI command for `systemd.timer`, `launchd`,
   `cron`, or CI runners that evaluates due timers, expires stale leases, dispatches
   queued `inbox` items, and flushes pending `outbox` deliveries in
   milliseconds before exiting cleanly with `0` idle RAM.
3. **Durable Inbox**: Queue CLI and timer triggers in `inbox` (`acdd event` / timer firing)
   with atomic `dedup_key` idempotency and `coalesce_window` burst folding,
   binding event payloads to AC-005's `input.<field>` namespace (`with: input.events | event.*`)
   and dispatching runs through AC-005's concurrent CAS `swarm` engine (`repo.lock` acquired strictly
   when `save != none`, never blocking `save: none` parallel channels).
4. **Idempotent Outbox**: Route outward status notifications through a durable
   `outbox` table in `acdd-state` constrained to high-signal transitions (`approval_needed`,
   `escalated`, `run_completed`) with retry backoff and anti-spam digest pacing.

All stateful network and local IPC transport (`POST /events` `--incoming`, `--interactive`
Web UI mutations, `.acdd/acdd.sock`, Streamable HTTP `acdd mcp --http`, and the `acdd up`
supervisor) are consolidated separately in [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)
behind `ADR-0013` authentication and confinement.

## Tasks in this milestone

### task: deferred-human-gates-and-sandbox-secret-scrubbing

```yaml
task_ref: deferred-human-gates-and-sandbox-secret-scrubbing
target: 'Add RoleDef.mode: manual with clean park-and-sleep run suspension, CLI/MCP approve/reject resume paths, and strict secret env scrubbing in role spawner'
subtasks:
- subtask_ref: manual-mode-and-park-on-approval
  title: 'Support RoleDef.mode: manual and park acdd run cleanly with exit code 0 when awaiting human approval or escalated resolution'
- subtask_ref: cli-mcp-approve-reject-resume
  title: Resume parked nodes strictly via CLI/MCP approve and reject operations with CAS claim_revision verification
- subtask_ref: spawner-secret-env-scrubbing
  title: Strip *_KEY, *_TOKEN, *_SECRET, *_PASSWORD environment variables by default in spawner.rs unless explicitly admitted via RoleDef.env_allowlist
contract_revision: 1
tags:
- default
depends_on: []
scope:
- crates/acdd-core/src/
- crates/acdd-state/src/profile/
- crates/acdd-state/src/machine/
- crates/acdd-state/src/usecases/
- crates/acdd-state/src/runner/spawner.rs
- src/cli/
- src/mcp/
- tests/acdd/always_on.rs
```

### task: claim-lease-ttl-timers-and-acdd-tick

```yaml
task_ref: claim-lease-ttl-timers-and-acdd-tick
target: Add lease_expires_at auto-expiry on gate_claims, durable timers (via SQLite migration v2 -> v3), and the zero-daemon one-shot acdd tick CLI pulse command
subtasks:
- subtask_ref: claim-lease-ttl-and-heartbeat
  title: Persist lease_expires_at from limits.claim_ttl_seconds, renew leases on verify and AC-005 blackboard wait activity, and expire hung remote claims cleanly
- subtask_ref: cron-and-agent-snooze-timers
  title: Persist recurring cron schedules and one-shot snooze/defer wake-ups in SQLite timers and enqueue inbox events without busy polling
- subtask_ref: oneshot-acdd-tick-pulse
  title: Implement one-shot acdd tick checking due timers, expired leases, queued inbox items, and pending outbox deliveries in milliseconds and exiting cleanly with 0 idle RAM
contract_revision: 1
tags:
- default
depends_on:
- deferred-human-gates-and-sandbox-secret-scrubbing
scope:
- crates/acdd-state/src/store/sqlite/migrations.rs
- crates/acdd-state/src/store/
- crates/acdd-state/src/profile/
- crates/acdd-state/src/timers.rs
- crates/acdd-state/src/usecases/
- src/cli/
- src/mcp/
- tests/acdd/always_on.rs
```

### task: durable-inbox-and-swarm-dispatch

```yaml
task_ref: durable-inbox-and-swarm-dispatch
target: Implement the durable inbox table, CLI/timer event enqueue, atomic dedup_key and coalesce_window burst folding, and concurrent CAS swarm dispatch in acdd-state
subtasks:
- subtask_ref: inbox-ingest-and-coalescing
  title: Enqueue local CLI/timer trigger payloads into inbox with atomic dedup_key idempotency and coalesce_window burst folding
- subtask_ref: concurrent-cas-swarm-inbox-dispatch
  title: 'Dispatch coalesced inbox items through AC-005 concurrent CAS swarm execution (input.<field>), holding repo.lock strictly when save != none without blocking save: none parallel channels'
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
```

### task: idempotent-outbox-and-notification-restraint

```yaml
task_ref: idempotent-outbox-and-notification-restraint
target: Add a durable outbox table and paced dispatcher emitting deduplicated notifications strictly on approval_needed, escalated, and run_completed transitions, and co-evolve ADR-0003/ADR-0005, docs, systemd.timer/launchd runbooks, workflow/profile examples, skills, ROADMAP.md, and CHANGELOG.md
subtasks:
- subtask_ref: state-transition-outbox-and-backoff
  title: Enqueue outbox items transactionally with state transitions, deduplicate by (run_id, node_id, claim_revision, event_kind), and deliver via acdd tick with bounded retry backoff and digest pacing
- subtask_ref: always-on-adr-docs-skills-and-examples
  title: Update ADR-0003, ADR-0005, architecture/reference pages (profiles, tasks, cli, mcp-tools, configuration), runbooks (incl. systemd.timer / launchd / cron acdd tick setup), executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate
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
- docs/reference/profiles.md
- docs/reference/tasks.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- docs/reference/configuration.md
- docs/runbooks/
- docs/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/acdd-profile-examples/
- ROADMAP.md
- CHANGELOG.md
- tests/acdd/always_on.rs
```
