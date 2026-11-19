---
id: AC-005-unified-swarm-gates-and-peer-board-coordination
title: "Unified swarm gates (mode: linear | parallel | swarm), GateDef.with coalescing, extended when:, and peer blackboard coordination"
doc_type: contract
status: planned
depends_on: []
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
  - docs/milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md
invariants:
  - 'INV-UNIFIED-GATE-MODE: Every GateDef executes under one unified GateMode enum (linear [default], parallel, or swarm) with zero special-cased TasksBlock (- tasks:); task tracks become ordinary named GateDef nodes (e.g. id: implement, mode: swarm, with: spec.tasks | blueprint.tasks) addressed by uniform hierarchical cursors (poll[telegram], implement[core-cleanup].build).'
  - 'INV-MULTI-SOURCE-WITH: Existing GateDef.with (introducing zero new from: field) resolves a single reference, pipe-separated fallback chain (spec.tasks | blueprint.tasks), YAML list ([spec.tasks, blueprint.tasks]), { count: N }, or { items, args } for leaf call: gates, evaluating left-to-right across spec.<field>, input.<field>, and <gate_id>.<field> to the first non-empty collection bounded by max_concurrency.'
  - 'INV-EXTENDED-WHEN: Native GateDef.when lifts the root-gate ban in schema.rs/validate.rs and extends profile/when.rs into a pure boolean predicate (!, &, +, |, parentheses) over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, and diff.*, keeping | strictly boolean OR inside when: while | in with: is left-to-right non-empty collection fallback.'
  - 'INV-LOCKFREE-NON-MUTATING-SWARM: repo.lock acquisition and Git TreeOid snapshot checks apply strictly when save != none; save: none parallel branches and dynamic swarm shards execute concurrently under optimistic SQLite BEGIN IMMEDIATE CAS (claim_revision + node_version).'
  - 'INV-DISJOINT-MUTATING-PARALLELISM: Concurrent save != none branches under mode: parallel or mode: swarm enforce disjoint admitted scope globs (failing closed with TASK_SCOPE_CONFLICT) or execute in isolated per-item/per-branch worktrees integrated deterministically via git_ops/integrate.rs.'
  - 'INV-BOUNDED-SWARM-BARRIER: A mode: parallel container opens all admitted child gates[] simultaneously up to max_concurrency, waits for in-flight siblings to settle, and aggregates all branch rejection_findings into one consolidated batch before routing once to GateDef.fail.'
  - 'INV-MENTION-SCOPED-PEER-WAIT: acdd_blackboard(action: "wait", since_seq, mentions_only) defaults to mentions_only: true so unrelated workers incur zero wakeups or context noise, waking immediately on matching SQLite commit/notify (@<worker_id>, @<node_cursor>, @role:<role>, @all) with thread_id and monotonic seq.'
  - 'INV-CORE-LEAF: Keep pure unified-gate and blackboard DTOs (GateMode, WithSource, NamespaceRoot, BlackboardRecipient, BlackboardWaitReq) in acdd-core free of SQLite, Git, HTTP, or filesystem I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: Every AC-005 capability ships in the same milestone with updated ADR-0003, architecture/reference/runbook docs, executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml examples, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: TasksBlock (- tasks:) is removed from GateDef (profile/schema.rs, inherit.rs, validate.rs) and embedded profiles (default.yaml, code.yaml, in_place.yaml), turning task execution into an ordinary mode: swarm GateDef with uniform hierarchical cursors.'
  - 'AC-2: Existing GateDef.with resolves single refs, pipe chains, YAML lists, { count: N }, and { items, args } (for leaf swarm call: gates) left-to-right across spec.<field>, input.<field>, and <gate_id>.<field> (flattening sharded mode: swarm proofs in deterministic shard order) and propagates typed gate_artifacts into ClaimResp.inputs.'
  - 'AC-3: Extended native GateDef.when evaluates across root, container, and shard gates over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, and diff.* (e.g. when: "!spec.tasks", when: "!docs", when: "item.is_new & !item.muted").'
  - 'AC-4: mode: parallel opens heterogeneous child gates[] simultaneously and joins at a consolidated barrier, while mode: swarm schedules depends_on items concurrently in topological waves—lock-free under SQLite CAS for save: none and disjoint-scope or worktree-isolated for save != none.'
  - 'AC-5: acdd_blackboard and acdd blackboard support structured @mentions, thread_id filtering, monotonic seq, and zero-token blocking wait (mentions_only: true default) persisted via SQLite migration v1 -> v2.'
  - 'AC-6: ADR-0003, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/, docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
---

# AC-005: Unified swarm gates, `GateDef.with` coalescing, extended `when:`, and peer-to-peer blackboard coordination

## Outcome and purpose

Make `GateProfile` (`version: 2`) in `crates/acdd-state` completely uniform by replacing the special-cased `- tasks:` (`TasksBlock`) machinery with one orthogonal execution model (`mode: linear | parallel | swarm`), multi-source coalescing via existing `GateDef.with` (adding zero new `from:` field), and extended native `GateDef.when` predicates under [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md) and [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md). As the immediate next milestone (`v0.7.0`), `AC-005` turns both software engineering task tracks (`with: spec.tasks | blueprint.tasks`) and domain-agnostic multi-agent pipelines (`with: input.channels` -> `with: poll.items`) into ordinary named `GateDef` nodes with uniform hierarchical cursors (`poll[telegram]`, `implement[core-cleanup].build`) and direct `@mention` + zero-token `wait` coordination on `acdd_blackboard`.

## Unified 5-milestone system architecture (`AC-005` + `AC-006` + `AC-007` + `AC-008` + `AC-009`)

Together, the five `AC` milestones compose into one general-purpose, local-first, durable multi-agent and automation runtime:

1. **`AC-005` (Immediate Foundation — Unified `mode: linear | parallel | swarm` & Peer Board)**: Replaces `- tasks:` with `GateMode` (`linear | parallel | swarm`), binds swarm collections through existing `GateDef.with` over `spec.* | input.* | <gate_id>.* | { count: N }` and extends native `GateDef.when` across root/container/shard gates, enables lock-free `save: none` SQLite CAS vs. disjoint/worktree `save != none` parallelism, and adds `@mention` + zero-token `acdd_blackboard(action: "wait")` coordination.
2. **[AC-006](AC-006-universal-repository-memory-synthesis.md) (Universal Memory — Cross-Run Learning)**: Git-tracked `docs/memories/*.md` projected through `doc_search` BM25 deterministically inject relevant operational rules into every `linear`, `parallel`, and `swarm` node's `ClaimResp` via `DomainProvider::context_lens`, while off-hot-path `--dream` passes distill completed runs into durable rules.
3. **[AC-007](AC-007-unified-observability-board-otlp-and-metrics.md) (CLI MCP Split & Read-Only Observability)**: Introduces explicit `acdd mcp` for session-scoped `stdio` MCP (`install.sh` + adapter updates) and replaces `acdd serve` stdio aliasing with strictly read-only loopback `acdd serve [--webui] [--telemetry]` (`GET /`, `GET /api/*`, `GET /events` SSE rendering `AC-005` `linear | parallel | swarm` node hierarchies and live `@mention` peer `blackboard` threads, bounded-cardinality `/metrics`, and OTLP/HTTP JSON trace export).
4. **[AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md) (Zero-Daemon Reactive Core)**: Adds `RoleDef.mode: manual` park-and-sleep approvals (resumed via CLI/MCP), `spawner.rs` `env_allowlist` secret scrubbing, claim `lease_expires_at` TTLs (renewed while parked in `AC-005` `wait`), durable SQLite `timers` (`v2 -> v3` migration), one-shot `acdd tick`, deduplicated `inbox` (`dedup_key` + `coalesce_window` dispatching through `AC-005`'s concurrent CAS `swarm` engine), and paced `outbox` notifications.
5. **[AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md) (Authenticated Network/IPC Ingress, Interactive Web UI & Streamable HTTP MCP)**: Adds upfront `ADR-0013` security middleware (`Host`/`Origin`, scoped Bearer tokens, HMAC-SHA256 webhooks, body caps, rate limiting), confined `.acdd/acdd.sock` (`0600`), `acdd serve --interactive`, `acdd serve --incoming` (`POST /events`), `acdd mcp --http` (`POST/GET /mcp` with `Mcp-Session-Id` & token-bound `worker_id`), and `acdd up`.

## Core mechanisms

### 1. Single gate execution model (`mode: linear | parallel | swarm`) & removal of `- tasks:`

Replace `TasksBlock` (`tasks: Option<TasksBlock>` / `RawTasksBlock` / `WorkingTasksBlock` in `crates/acdd-state/src/profile/{schema,inherit,validate}.rs` and embedded profiles `default.yaml`, `code.yaml`, `in_place.yaml`) with one unified `GateMode` enum on `GateDef` plus optional `max_concurrency: Option<u32>`:

1. **`mode: linear`** *(default)*: Executes child `gates[]` sequentially (`A -> B -> C`), or runs a leaf gate once.
2. **`mode: parallel`**: Opens all admitted child `gates[]` simultaneously (`A || B || C` heterogeneous branches up to `max_concurrency`) and joins at a container barrier. If any branch fails or rejects, the barrier waits for in-flight siblings to settle, aggregates all branch `rejection_findings` into one consolidated batch, and routes once to `GateDef.fail` (respecting `max_attempts`).
3. **`mode: swarm`**: Instantiates this gate (leaf or child `gates[]` pipeline) dynamically per item resolved from `with:` (`G[item_1] || ... || G[item_N]`), scheduling ready items concurrently in topological waves (up to `max_concurrency`) when items carry `depends_on`.

Removing `- tasks:` turns the task lifecycle into an ordinary named `GateDef` (for example, `id: implement`, `mode: swarm`, `with: spec.tasks | blueprint.tasks`). Every gate and shard is addressed uniformly by hierarchical cursors (`poll[telegram]`, `implement[core-cleanup].build`), and profile inheritance (`gates_merge: patch`) addresses gates uniformly by `id` without special-cased `WorkingTasksBlock` patch rules.

### 2. Lock-free `save: none` CAS vs. disjoint-scope / worktree isolation (`save != none`)

- **Lock-free non-Git workflows (`save: none`)**: Acquire `repo.lock` and capture/verify Git `TreeOid` state **strictly** when `save != none` (`save.post: commit`). Parallel branches and swarm shards with `save: none` run concurrently with zero Git lock contention using millisecond SQLite `BEGIN IMMEDIATE` CAS (`claim_revision` + `node_version`).
- **Disjoint or worktree-isolated mutating branches (`save != none`)**: Concurrent `save != none` branches enforce disjoint admitted `scope` globs (failing closed with `TASK_SCOPE_CONFLICT` on overlap) or run in per-item/per-branch Git worktrees integrated deterministically via `crates/acdd-state/src/git_ops/integrate.rs`.

### 3. Multi-source coalescing `GateDef.with` & extended native `GateDef.when`

- **Reusing existing `GateDef.with` (no `from:` field)**: `GateDef.with` (`Option<Value>`) already exists on `GateDef` (`crates/acdd-state/src/profile/schema.rs`). For `mode: swarm` gates, `with:` accepts a single reference (`with: input.channels`), a pipe-separated fallback chain (`with: spec.tasks | blueprint.tasks`), a YAML list (`with: [spec.tasks, blueprint.tasks]`), `{ count: N }` (materializing `0..N` homogeneous items), or `{ items: <source>, args: <object> }` when a leaf `mode: swarm` gate also uses `call:` (passing each shard its item payload or `{ item, ...args }`). Multi-source expressions evaluate left-to-right to the **first non-empty collection** across three deterministic root namespaces:
  1. **`spec.<field>`**: Bound `SpecDocument` parsed from the run's Markdown file (`spec.tasks` from `### task:` blocks, `spec.invariants`, `spec.owners`; evaluates to empty `[]`/`null` if the `.md` spec omits `### task:` blocks or when a headless workflow run starts without a spec file).
  2. **`input.<field>`**: Runtime JSON payload passed at `acdd start --input` (or `AC-008`/`AC-009` timer/event/webhook trigger), e.g. `input.channels`.
  3. **`<gate_id>.<field>`**: Schema-validated JSON `proof` stored in `gate_artifacts` by completed upstream gate `<gate_id>` (automatically flattening/collecting across shards in deterministic shard order when `<gate_id>` executed in `mode: swarm`, e.g. `poll.items` or `blueprint.tasks`).
- **Extended native `GateDef.when`**: Lift the root-gate ban on `when:` in `crates/acdd-state/src/profile/{schema,validate}.rs` and extend `crates/acdd-state/src/profile/when.rs` from bare tags into a pure boolean predicate (`!`, `&`, `+`, `|`, parentheses) over `tags`, `spec.*`, `input.*`, `<gate_id>.*`, `item.*`, `event.*`, and `diff.*` (e.g., `when: "!spec.tasks"` on root `blueprint`, `when: "!docs"` on `implement`, `when: "item.is_new & !item.muted"` on 10-channel `triage`). Keep `|` strictly boolean OR inside `when:`, while `|` inside `with:` is left-to-right non-empty collection fallback.
- **Typed edge propagation**: Validated `proof` payloads persisted in `gate_artifacts` flow deterministically along `pass:` / `need:` edges into downstream `ClaimResp.inputs` (bounded inside the 6-tier context envelope) and `when:` predicates, while each `mode: swarm` instance receives its bound item (`item.*`, `shard.id`, `shard.index`).

### 4. Direct peer-to-peer coordination over `acdd_blackboard` (`@mention` + zero-token `wait`)

While structured pipeline inputs and outputs flow through `gate_artifacts`, concurrent workers in `mode: parallel` or `mode: swarm` runs need direct lateral coordination (resolving interface questions, flagging cross-channel duplicates, or escalating to a peer or coordinator):

- **Structured addressing & threads**: Extend `blackboard` (`crates/acdd-core` DTOs and SQLite migration `v1 -> v2` in `crates/acdd-state/src/store/sqlite/migrations.rs`) with `recipient` / `@mentions` (`@<worker_id>`, `@<node_cursor>`, `@role:<role>`, `@all`), `thread_id`, and monotonic `seq`.
- **Zero-token blocking `wait`**: Add `acdd_blackboard(action: "wait", since_seq, mentions_only)` (and CLI `acdd blackboard wait`). The call parks without consuming LLM tokens and wakes immediately on SQLite commit/notify when a matching entry arrives (`mentions_only` defaults to `true`, so unrelated peers incur zero wakeups or context noise).

## Concise `profile.yaml` examples

### Example 1: Task-driven code milestone (`with: spec.tasks | blueprint.tasks` + extended `when:`, zero `- tasks:`)

```yaml
version: 2
roles:
  architect: { mode: subagent }
  builder:   { mode: subagent }
  reviewer:  { mode: subagent }
gates:
  - id: blueprint
    role: architect
    when: "!spec.tasks"                   # run blueprint only if spec omits tasks
    pass: implement
  - id: implement
    mode: swarm
    when: "!docs"                         # skip code swarm on docs-only tracks
    with: spec.tasks | blueprint.tasks    # cursors: implement[core-cleanup].build
    max_concurrency: 4
    save: none
    gates:
      - { id: contract, role: builder,  proof: contract, pass: build }
      - { id: build,    role: builder,  proof: build,    pass: review }
      - { id: review,   role: reviewer, proof: review,   independent_from: [build], pass: deliver, fail: build }
      - { id: deliver,  role: builder,  save: commit }
    pass: verify
  - id: verify
    role: builder
```

### Example 2: 10-channel reactive monitor (`with: input.channels` -> `with: poll.items` + `when: "item.is_new & !item.muted"`)

```yaml
version: 2
roles:
  watcher:   { mode: subagent }
  triager:   { mode: subagent }
  publisher: { mode: subagent }
gates:
  - id: poll
    role: watcher
    mode: swarm
    max_concurrency: 10
    with: input.channels                  # cursors: poll[telegram], poll[github], ...
    save: none                            # lock-free SQLite CAS, zero repo.lock
    pass: triage
  - id: triage
    role: triager
    mode: swarm
    with: poll.items                      # flattens sharded poll proofs in shard order
    when: "item.is_new & !item.muted"     # skip muted/seen items per shard
    context: [docs.memory]                # AC-006 operational rules per shard
    save: none
    pass: review
  - id: review
    mode: parallel                        # heterogeneous branches join at barrier
    gates:
      - { id: dedup_check, role: triager, save: none }
      - { id: policy_check, role: triager, save: none }
    pass: publish
  - id: publish
    role: publisher
    save: commit
```

## Tasks in this milestone

### task: swarm-contract-and-schema-freeze

```yaml
task_ref: swarm-contract-and-schema-freeze
target: "Freeze GateMode, WithSource, NamespaceRoot, and BlackboardRecipient/wait DTOs in acdd-core, replace TasksBlock (- tasks:) with mode/max_concurrency and reuse GateDef.with/when in GateProfile v2 and embedded profiles, add SQLite v1->v2 migration, and pre-register tests/acdd/swarm.rs"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - docs/milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md
  - crates/acdd-core/src/
  - crates/acdd-state/src/profile/schema.rs
  - crates/acdd-state/src/profile/inherit.rs
  - crates/acdd-state/src/profile/validate.rs
  - crates/acdd-state/src/profile/mod.rs
  - crates/acdd-state/src/profile/embedded/default.yaml
  - crates/acdd-state/src/profile/embedded/code.yaml
  - crates/acdd-state/src/profile/embedded/in_place.yaml
  - crates/acdd-state/src/store/sqlite/migrations.rs
  - crates/acdd-state/src/store/sqlite/schema.rs
  - tests/acdd/mod.rs
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: core-swarm-and-blackboard-dtos
    title: "Add pure GateMode (linear|parallel|swarm), WithSource, NamespaceRoot, BlackboardRecipient, and BlackboardWaitReq DTOs in acdd-core"
  - subtask_ref: replace-tasks-block-and-sqlite-v2
    title: "Remove TasksBlock (- tasks:) from GateDef and embedded profiles in favor of mode/with/when/max_concurrency, lift root-gate when: restriction, plus SQLite v1->v2 blackboard @mentions/thread_id/seq migration"
  - subtask_ref: swarm-seam-registration
    title: "Register tests/acdd/swarm.rs in tests/acdd/mod.rs with runtime-red contract matrices"
```

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
    title: "Expose all admitted child gates up to max_concurrency in acdd_state(next) under mode: parallel and join at container barrier with consolidated rejection_findings"
  - subtask_ref: lockfree-save-none-and-disjoint-git
    title: "Gate repo.lock and Git TreeOid snapshots strictly on save != none while running save: none branches lock-free under SQLite CAS"
  - subtask_ref: mutating-parallel-worktree-integration
    title: "Enforce disjoint scope globs or isolate concurrent save != none branches into worktrees merged via git_ops/integrate.rs"
```

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
  - crates/acdd-state/src/profile/when.rs
  - crates/acdd-state/src/profile/validate.rs
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/src/context.rs
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: multi-source-with-coalescing-resolver
    title: "Evaluate single refs, pipe chains, YAML lists, { count: N }, and { items, args } in GateDef.with left-to-right across spec.*, input.*, and flattened <gate_id>.* shard proofs"
  - subtask_ref: extended-when-and-dynamic-swarm-waves
    title: "Extend native when: evaluator over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, diff.* across root/container/shard gates and schedule mode: swarm topological depends_on waves with ClaimResp.inputs propagation"
```

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
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: mention-routing-and-blocking-wait
    title: "Implement @mention recipient indexing, thread_id queries, and zero-token acdd_blackboard wait(since_seq, mentions_only) waking on SQLite commit/notify"
  - subtask_ref: adr-docs-skills-and-examples-coevolution
    title: "Update ADR-0003, architecture/reference/runbook pages, executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate"
```
