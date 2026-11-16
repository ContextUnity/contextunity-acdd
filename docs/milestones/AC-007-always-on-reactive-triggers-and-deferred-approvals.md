---
id: AC-007-always-on-reactive-triggers-and-deferred-approvals
title: "Always-On reactive lifecycle: deferred human approvals, lease TTLs, timers, event inbox, and idempotent outbox"
doc_type: contract
status: planned
depends_on:
  - AC-005-unified-observability-board-otlp-and-metrics
  - AC-006-universal-repository-memory-synthesis
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/
  - crates/acdd-state/src/
  - src/cli/
  - src/mcp/
  - tests/acdd/
  - docs/architecture/
  - docs/reference/
  - docs/milestones/AC-007-always-on-reactive-triggers-and-deferred-approvals.md
invariants:
  - 'INV-ZERO-TOKENS-AT-REST: Sleeping runs (awaiting manual approval, escalated review, or timer wake-up) persist cursor state in SQLite and exit cleanly with zero background LLM polling or token consumption.'
  - 'INV-SERIALIZED-DISPATCH-AND-DEDUP: External triggers enter one durable event_inbox table with explicit dedup_key idempotency and burst coalescing before sequential single-worker dispatch under existing SQLite BEGIN IMMEDIATE and repo.lock discipline.'
  - 'INV-SECRET-ISOLATION: Role runner subprocesses never inherit unapproved host secret variables; sensitive environment credentials are scrubbed unless explicitly admitted via role env_allowlist or external proxy.'
  - 'INV-OUTBOX-IDEMPOTENCY-AND-RESTRAINT: Outgoing notifications fire only from durable outbox rows tied to discrete state transitions (approval_needed, escalated, run_completed), deduplicated per revision and paced by digest windows.'
  - 'INV-CORE-LEAF: Keep pure always-on DTOs (InboxEvent, TimerSpec, OutboxMessage, ApprovalDecision) in acdd-core free of filesystem, SQLite, HTTP, or process I/O.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
---

# AC-007: Always-On reactive lifecycle and deferred human approvals

## Outcome and purpose

Extend the unified `crates/acdd-state` profile engine (and compose cleanly with
[AC-005](AC-005-unified-observability-board-otlp-and-metrics.md) and
[AC-006](AC-006-universal-repository-memory-synthesis.md)) from pull-driven execution
into a reactive, sleep-first autonomous lifecycle where runs spend idle waiting
time asleep with zero token consumption:

1. **Deferred Human Approvals & Secret Isolation**: Introduce `RoleDef.mode: manual`
   and park-and-sleep handling for `escalated` or protected-path approval gates,
   paired with subprocess environment secret scrubbing in `spawner.rs` and
   explicit wake-up via CLI/MCP or an opt-in interactive board flag layered onto
   AC-005's HTTP server.
2. **Claim Lease TTL & Durable Timers**: Add TTL leases (`lease_expires_at`) on
   `gate_claims` plus a durable `timers` table for recurring schedules (such as
   off-hot-path `memory-dream` consolidation from AC-006) and agent-initiated
   one-shot wake-ups (`snooze` / `defer`).
3. **Durable Event Inbox**: Queue incoming Git hooks, CI webhooks, and watch
   events in `event_inbox` (`202 Accepted` on `POST /events`) with `dedup_key`
   idempotency, `coalesce_window` burst folding, and sequential dispatch.
4. **Idempotent Outbox**: Route outward status notifications through a durable
   `outbox` table constrained to high-signal transitions with retry backoff and
   anti-spam pacing.

`AC-005` stays strictly read-only on its own deliverable boundary; interactive
approval endpoints live here as an explicit opt-in capability (`--interactive`).

## Tasks in this milestone

### task: deferred-human-gates-and-sandbox-secret-scrubbing

```yaml
task_ref: deferred-human-gates-and-sandbox-secret-scrubbing
target: "Add RoleDef.mode: manual with clean park-and-sleep run suspension, explicit approve/reject resume paths (CLI/MCP and opt-in interactive board action), and strict secret env scrubbing in role spawner"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on: []
scope:
  - crates/acdd-core/src/
  - crates/acdd-state/src/profile/
  - crates/acdd-state/src/engine/
  - crates/acdd-state/src/spawner.rs
  - crates/acdd-state/src/observability/http.rs
  - src/cli/
  - src/mcp/
  - tests/acdd/always_on.rs
subtasks:
  - subtask_ref: manual-mode-and-park-on-approval
    title: "Support RoleDef.mode: manual and park acdd run cleanly with exit code 0 when awaiting human approval or escalated resolution"
  - subtask_ref: approve-reject-resume-and-opt-in-board-action
    title: "Resume parked nodes from CLI/MCP approve/reject and expose an opt-in --interactive board approval route without relaxing AC-005 default read-only mode"
  - subtask_ref: spawner-secret-env-scrubbing
    title: "Strip *_KEY, *_TOKEN, *_SECRET, *_PASSWORD environment variables by default in spawner.rs unless explicitly admitted via RoleDef.env_allowlist"
```

### task: claim-lease-ttl-and-durable-timers

```yaml
task_ref: claim-lease-ttl-and-durable-timers
target: "Add lease_expires_at auto-expiry on gate_claims and a durable timers scheduler for recurring cron profiles (e.g. AC-006 memory-dream) and agent snooze/defer follow-ups"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on:
  - deferred-human-gates-and-sandbox-secret-scrubbing
scope:
  - crates/acdd-state/src/store/
  - crates/acdd-state/src/profile/
  - crates/acdd-state/src/timers.rs
  - src/cli/
  - src/mcp/
  - tests/acdd/always_on.rs
subtasks:
  - subtask_ref: claim-lease-ttl-and-heartbeat
    title: "Persist lease_expires_at from limits.claim_ttl_seconds, renew leases on verify/blackboard activity, and expire hung remote claims cleanly"
  - subtask_ref: cron-and-agent-snooze-timers
    title: "Persist recurring cron schedules and one-shot snooze/defer wake-ups in SQLite timers and enqueue inbox events without busy polling"
```

### task: durable-event-inbox-and-webhook-coalescing

```yaml
task_ref: durable-event-inbox-and-webhook-coalescing
target: "Implement the durable event_inbox table, loopback POST /events webhook ingest, dedup_key and coalesce_window folding, and serialized single-worker dispatch"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on:
  - claim-lease-ttl-and-durable-timers
scope:
  - crates/acdd-state/src/inbox.rs
  - crates/acdd-state/src/store/
  - crates/acdd-state/src/observability/http.rs
  - src/cli/
  - tests/acdd/always_on.rs
subtasks:
  - subtask_ref: event-inbox-ingest-and-coalescing
    title: "Accept external webhook/hook payloads via 202 Accepted POST /events, persist in event_inbox with dedup_key, and coalesce rapid bursts"
  - subtask_ref: sequential-inbox-worker-dispatch
    title: "Dispatch queued inbox items sequentially to wake/reopen matching active Runs or launch mapped profile runs without overlapping workers"
```

### task: idempotent-outbox-and-notification-restraint

```yaml
task_ref: idempotent-outbox-and-notification-restraint
target: "Add a durable outbox table and paced dispatcher emitting deduplicated notifications strictly on approval_needed, escalated, and run_completed transitions"
proof_policy: seam-test-first
contract_revision: 1
tags: [docs, default]
depends_on:
  - durable-event-inbox-and-webhook-coalescing
scope:
  - crates/acdd-state/src/outbox.rs
  - crates/acdd-state/src/store/
  - src/cli/
  - docs/architecture/
  - docs/reference/
  - tests/acdd/always_on.rs
subtasks:
  - subtask_ref: state-transition-outbox-and-backoff
    title: "Enqueue outbox items transactionally with state transitions, deduplicate by (run_id, node_id, claim_revision, event_kind), and deliver with bounded retry backoff and digest pacing"
  - subtask_ref: always-on-docs-and-runbook
    title: "Document deferred approvals, lease TTLs, timers, event_inbox, and outbox configuration and operational safety guarantees"
```
