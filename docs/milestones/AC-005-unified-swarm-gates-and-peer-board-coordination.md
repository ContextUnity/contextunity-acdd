---
id: AC-005-unified-swarm-gates-and-peer-board-coordination
title: "Unified swarm gates (mode: linear | parallel | swarm), GateDef.with coalescing, extended when:, and peer blackboard coordination"
doc_type: contract
status: planned
depends_on:
  - AR-002-modularize-db-reader-and-traversal
  - AR-003-unified-lint-engine-and-documentation-profile
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
  - .acdd/profiles/
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
  - 'INV-BOUNDED-SWARM-BARRIER: A mode: parallel container opens all admitted child gates[] simultaneously up to max_concurrency, waits for in-flight non-observer siblings to settle, and aggregates all branch rejection_findings into one consolidated batch before routing once to GateDef.fail; optional observer: true siblings (allowed strictly when save: none) stay non-blocking with minimal observer context (guidance + active sibling scopes/targets), run acdd_blackboard wait/watch loops with optional cycle limits or until a trigger/container-closing sentinel, steer on drift, and exit cleanly when all primary branches reach terminal states.'
  - 'INV-MENTION-SCOPED-PEER-WAIT: acdd_blackboard(action: "wait", since_seq, mentions_only) defaults to mentions_only: true so unrelated workers incur zero wakeups or context noise, waking immediately on matching SQLite commit/notify (@<worker_id>, @<node_cursor>, @agent:<agent>, @all) with thread_id and monotonic seq, and optionally accepts watch mode (watch:<n> capped cycles or until-trigger/sentinel) that wakes early on universal triggers (budget_overrun, thrash_loop, scope_drift, interval_seconds).'
  - 'INV-GROUPED-SAVE-AND-AGENT-SCHEMA: GateProfile v2 renames roles/role to agents/agent (with built-in coordinator: { mode: inline }) and replaces scattered SavePolicy booleans + receipt flags with mutually exclusive save variants (none | readonly | snapshot | commit | handoff: { archive, prune, commit_scope, immutable, scope_mode?: lazy|strict|off, repack?: none|linear|by_task|last|squash, report? }), where save: handoff integrates only mutating swarm/parallel worktree branches, atomically commits admitted self.scope alongside archive, defaults to scope_mode: lazy (non-blocking ancestry/no-revert checks across extend_scope overlaps and accumulated pre-handoff polish absorbed at handoff with Acdd-Integrated trailers vs strict blob equality or off), and supports fail-closed worktree-only Git repacking (by_task splits final tree T_final across task scopes + 1 handoff tail commit; last folds archive, self.scope, and accumulated polish into the last delivered task commit; squash collapses base..T_final into 1 commit).'
  - 'INV-HIERARCHICAL-GROUP-COORDINATORS: Any linear, parallel, or swarm GateDef acts as a composable group (arbitrarily nested via gates[]) that may bind an optional coordinator: <agent>; each group coordinator binds automatically to thread_id = <container_cursor> and @coordinator/@<group_cursor> mentions, supports bounded hot wait(timeout_seconds) plus persistent session_id handoff, and isolates intra-group chatter from parent or sibling coordinators.'
  - 'INV-CORE-LEAF: Keep pure unified-gate and blackboard DTOs (GateMode, WithSource, NamespaceRoot, BlackboardRecipient, BlackboardWaitReq) in acdd-core free of SQLite, Git, HTTP, or filesystem I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: Every AC-005 capability ships in the same milestone with updated ADR-0003, architecture/reference/runbook docs, executable .acdd/profiles/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml examples, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: TasksBlock (- tasks:) is removed from GateDef (profile/schema.rs, inherit.rs, validate.rs) and embedded profiles (base.yaml, code.yaml, audit.yaml), turning task execution into an ordinary mode: swarm GateDef with uniform hierarchical cursors; roles/role are renamed to agents/agent (adding built-in coordinator inline agent) and SavePolicy + receipt collapse into mutually exclusive save variants including save: handoff { archive, prune, commit_scope, immutable, scope_mode?, repack?, report? } with simplified non-blocking pre-handoff polish under default scope_mode: lazy.'
  - 'AC-2: Existing GateDef.with resolves single refs, pipe chains, YAML lists, { count: N }, and { items, args } (for leaf swarm call: gates) left-to-right across spec.<field>, input.<field>, and <gate_id>.<field> (flattening sharded mode: swarm proofs in deterministic shard order) and propagates typed gate_artifacts into ClaimResp.inputs.'
  - 'AC-3: Extended native GateDef.when evaluates across root, container, and shard gates over tags, spec.*, input.*, <gate_id>.*, item.*, event.*, and diff.* (e.g. when: "!spec.tasks", when: "!docs", when: "item.is_new & !item.muted").'
  - 'AC-4: mode: parallel opens heterogeneous child gates[] simultaneously and joins non-observer branches at a consolidated barrier (while optional observer: true save: none peers run with lightweight guidance + active sibling scopes, watch sibling health via acdd_blackboard wait/watch, and exit cleanly on container-closing sentinel), while mode: swarm schedules depends_on items concurrently in topological waves—lock-free under SQLite CAS for save: none and disjoint-scope or worktree-isolated for save != none.'
  - 'AC-5: acdd_blackboard and acdd blackboard support structured @mentions (@<worker_id>, @<node_cursor>, @agent:<name>, @coordinator, @all), thread_id scoping, monotonic seq, and zero-token blocking wait/watch (since_seq, mentions_only: true default, timeout_seconds?, watch? with optional cycle count N or continuous until-trigger mode detecting budget_overrun, thrash_loop, scope_drift, and interval_seconds heartbeats without a daemon) persisted via SQLite migration v1 -> v2, alongside recursive GateDef.coordinator group supervision scoped by hierarchical cursor.'
  - 'AC-6: ADR-0003, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/, .acdd/profiles/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
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

Replace `TasksBlock` (`tasks: Option<TasksBlock>` / `RawTasksBlock` / `WorkingTasksBlock` in `crates/acdd-state/src/profile/{schema,inherit,validate}.rs` and embedded profiles `base.yaml`, `code.yaml`, `audit.yaml`) with one unified `GateMode` enum on `GateDef` plus optional `max_concurrency: Option<u32>`:

1. **`mode: linear`** *(default)*: Executes child `gates[]` sequentially (`A -> B -> C`), or runs a leaf gate once.
2. **`mode: parallel`**: Opens all admitted child `gates[]` simultaneously (`A || B || C` heterogeneous branches up to `max_concurrency`) and joins at a container barrier. If any branch fails or rejects, the barrier waits for in-flight siblings to settle, aggregates all branch `rejection_findings` into one consolidated batch, and routes once to `GateDef.fail` (respecting `max_attempts`).
3. **`mode: swarm`**: Instantiates this gate (leaf or child `gates[]` pipeline) dynamically per item resolved from `with:` (`G[item_1] || ... || G[item_N]`), scheduling ready items concurrently in topological waves (up to `max_concurrency`) when items carry `depends_on`.

Removing `- tasks:` turns the task lifecycle into an ordinary named `GateDef` (for example, `id: implement`, `mode: swarm`, `with: spec.tasks | blueprint.tasks`). Every gate and shard is addressed uniformly by hierarchical cursors (`poll[telegram]`, `implement[core-cleanup].build`), and profile inheritance (`gates_patch:`) addresses gates uniformly by `id` without special-cased `WorkingTasksBlock` patch rules.

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

- **Structured addressing & threads**: Extend `blackboard` (`crates/acdd-core` DTOs and SQLite migration `v1 -> v2` in `crates/acdd-state/src/store/sqlite/migrations.rs`) with `recipient` / `@mentions` (`@<worker_id>`, `@<node_cursor>`, `@agent:<agent>`, `@all`), `thread_id`, and monotonic `seq`.
- **Zero-token blocking `wait` & daemonless `watch` watchdog**: Add `acdd_blackboard(action: "wait", since_seq, mentions_only, timeout_seconds?, watch?)` (and CLI `acdd blackboard wait` / `acdd blackboard watch [<n>]`). Plain `wait` parks without consuming LLM tokens and wakes immediately on SQLite commit/notify when a matching entry arrives (`mentions_only` defaults to `true`, so unrelated peers incur zero wakeups or context noise). When `watch` is enabled (optionally bounded to `watch:<n>` cycles, or running across cycles until a trigger or `container_closing` sentinel when `<n>` is omitted), the same blocking `wait` evaluates lightweight domain-agnostic delta checks with zero background daemon and wakes early only on concrete triggers—including **`budget_overrun`** (active sibling delta exceeds configured `max_net_loc` or node budget without approved override), **`thrash_loop`** (repeated `>= 2` rejections or consecutive stagnant retries on a cursor), **`scope_drift`** (unadmitted file/scope writes), **`interval_seconds`** heartbeat ticks (attaching a concise progress + diff delta for early mechanism steering), or `@mention` / `container_closing` sentinels.

### 5. Unified `agents:` naming & grouped `save: handoff` policy (`SavePolicy` cleanup)

- **Rename `roles` / `role` -> `agents` / `agent`**: Rename `roles:`/`role:` to `agents:`/`agent:` across `GateProfile` (`AgentDef`, `AgentPatch`, `AgentExecutionMode`, embedded `base.yaml`/`code.yaml`/`audit.yaml`, `.acdd/profile.yaml`, `context.rs`, and `@agent:<name>` blackboard routing), adding built-in `coordinator: { mode: inline }` to `base.yaml`/`code.yaml` so coordinator-owned gates work out of the box without per-repo patches.
- **Grouped mutually exclusive `save:` variants (`none | readonly | snapshot | commit | handoff`)**: Collapse scattered `SavePolicy` booleans (`post`, `immutable`, `archive`, `prune`, `report`) and `GateDef.receipt` into explicit variants:
  - `none` (lock-free SQLite CAS, zero Git lock/tree checks) and `readonly` (asserts clean/immutable tree without committing).
  - `snapshot` (checkpoint ref, no branch commit) and `commit` (scoped branch commit).
  - `handoff: { archive, prune, commit_scope, immutable, scope_mode?, repack?, report? }`: replaces `receipt: true + save: { post: commit, archive: true, prune: true }`, integrates only upstream mutating `mode: swarm`/`parallel` worktrees (never `save: none` shards, replacing `TasksBlock`-bound `is_post_task_root_node`), and when `commit_scope: true` stages `self.scope` (e.g. `docs/memories/**`) in the same terminal archive commit while excluding `self.scope` from upstream `need:` tree drift checks. Optional `scope_mode` (`lazy` [default] | `strict` | `off`) replaces rigid per-file `TASK_NOT_INTEGRATED` claim blocks: `lazy` allows overlapping `extend_scope` updates across parallel tasks and free manual/accumulated pre-handoff `polish` commits on top of delivered task trees (requiring only ancestor containment / non-revert until `handoff` absorbs `polish` and records `Acdd-Integrated` trailers), `strict` enforces exact per-file `blob == accepted_tree` equality for audit profiles, and `off` skips integration drift checks altogether. Optional `repack` (`none` [default] | `linear` | `by_task` | `last` | `squash`, permitted strictly under `git.isolation: worktree` after verifying `target_base_commit` is an ancestor of `HEAD`) deterministically rebuilds run history from `target_base_commit` to the verified final `TreeOid` `T_final` via Git plumbing (`commit-tree`): `linear` drops merge bubbles, `by_task` emits one commit per completed task sliced by its `admitted_scope` + one final `handoff` commit containing everything outside task scopes (`spec`/archive, `polish`, cross-cutting docs, `self.scope`), `last` folds milestone archival, `self.scope`, and accumulated `polish` directly into the last delivered task commit without emitting a separate tail commit (falling back to 1 commit when no prior task commit exists), and `squash` folds the whole run into 1 clean commit (ideal for plans/hotfixes), while preserving all SQLite receipts/tombstones.

### 6. Recursive group composition & `GateDef.coordinator:` (Hybrid sleep/wake supervision)

- **Recursive groups without a separate schema type**: Because `GateDef` already nests `gates[]` recursively under `mode: linear | parallel | swarm`, every container IS a composable group (groups inside groups, e.g. `delivery` -> `backend_group` (`api_swarm` + `db_migration`) `||` `frontend_group`).
- **Per-group `coordinator: <agent>` & automatic tree-scoped routing**: Any container `GateDef` may declare `coordinator: <agent>` (persisting a container `coordinator_session_id` alongside `parent_id`). Each group coordinator is bound automatically to `thread_id = <container_cursor>` and `@<container_cursor>` mentions; child `@coordinator` pings resolve `O(depth)` up `parent_id` to the nearest enclosing coordinator without waking sibling or parent coordinators unless explicitly escalated.
- **Hybrid hot `wait` + cold `session_id` `resume` model**: During active bursts, a group coordinator (or peer shard) may park inside `acdd_blackboard(action: "wait", thread_id, mentions_only: true, timeout_seconds)` at 0 tokens; on timeout/exit, `session_id` remains persisted so `AC-008` `inbox` wake-on-mention (or `acdd run` re-entry) resumes the exact coordinator or cycle worker at 0 idle RAM. When all primary (non-`observer`) children of the container reach terminal states, the engine emits a container-closing sentinel on `<container_cursor>` (and to any parked `observer: true` peer cursors) so coordinators and standing observers wrap up cleanly.
- **Non-blocking observers (`GateDef.observer: true`)**: Any leaf child inside a `mode: parallel` group may set `observer: true` (validated strictly against `save: none` in `profile/validate.rs`) to run as a zero-token standing advisor with lightweight `ClaimResp` context (the gate's `guidance` + active sibling scopes/targets without heavy builder/reviewer gate boilerplate). On startup the observer enters `acdd_blackboard` `wait` / `watch` (with optional `watch:<n>` cycle cap, or running continuously until a trigger or sentinel when `<n>` is omitted), waking immediately on `@mention` questions, `budget_overrun`, `thrash_loop`, `scope_drift`, or `interval_seconds` checks to inspect sibling progress and steer before wasted builds/reviews, and closing cleanly without holding the `pass:` barrier once every non-observer sibling settles.
- **Worker self-claim & atomic unclaimed-shard admission**: Every spawned `mode: subagent` worker calls `acdd_claim` / `acdd claim` itself (binding `session_fingerprint`, `ClaimResp.worktree`, `ClaimResp.inputs`, and lease TTL to the real worker process rather than the parent orchestrator). Support both explicit shard cursors (`implement[t1].build`) and container-scoped pool self-claim (`acdd claim` against a `swarm` container/gate cursor selecting the next unclaimed ready shard atomically under SQLite `BEGIN IMMEDIATE` CAS up to `max_concurrency`), requiring workers to execute all edits and `verify`/`submit` strictly inside `ClaimResp.worktree`.

## Concise `profile.yaml` examples

### Example 1: Task-driven code milestone (`with: spec.tasks | blueprint.tasks` + `save: handoff`, zero `- tasks:`)

```yaml
version: 2
agents:
  coordinator: { mode: inline }
  architect:   { mode: subagent }
  builder:     { mode: subagent }
  reviewer:    { mode: subagent }
gates:
  - id: blueprint
    agent: architect
    when: "!spec.tasks"                   # run blueprint only if spec omits tasks
    pass: implement
  - id: implement
    mode: swarm
    when: "!docs"                         # skip code swarm on docs-only tracks
    with: spec.tasks | blueprint.tasks    # cursors: implement[core-cleanup].build
    max_concurrency: 4
    save: none
    gates:
      - { id: build,    agent: builder,  proof: command, save: snapshot, pass: review }
      - { id: review,   agent: reviewer, proof: review,  save: readonly, independent_from: [build], pass: deliver, fail: build }
      - { id: deliver,  agent: builder,  save: commit }
    pass: verify
  - id: verify
    agent: builder
    save: readonly
    pass: handoff
  - id: handoff
    agent: coordinator
    need: [verify]
    scope: ["docs/memories/**"]
    save:
      handoff: { archive: true, prune: true, commit_scope: true, scope_mode: lazy, repack: last }
```

### Example 2: 10-channel reactive monitor (`with: input.channels` -> `with: poll.items` + `when: "item.is_new & !item.muted"`)

```yaml
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
    with: input.channels                  # cursors: poll[telegram], poll[github], ...
    save: none                            # lock-free SQLite CAS, zero repo.lock
    pass: triage
  - id: triage
    agent: triager
    mode: swarm
    with: poll.items                      # flattens sharded poll proofs in shard order
    when: "item.is_new & !item.muted"     # skip muted/seen items per shard
    context: [docs.memory]                # AC-006 operational rules per shard
    save: none
    pass: review
  - id: review
    mode: parallel                        # heterogeneous branches join at barrier
    gates:
      - { id: dedup_check, agent: triager, save: none }
      - { id: policy_check, agent: triager, save: none }
    pass: publish
  - id: publish
    agent: publisher
    save:
      handoff: { archive: false, prune: true, commit_scope: true }
```

## Pre-kickoff Simplification Proposal (Pending Operator Review)

> [!NOTE]
> Recorded architectural refinement (100% harness-agnostic and workflow-agnostic) to review and approve or trim before claiming `swarm-contract-and-schema-freeze`. Keeps core `AC-005` foundations (`- tasks:` removal, hierarchical cursors, lock-free `save: none` CAS, `save: handoff`, `roles -> agents`, extended `when:`, and SQLite `v1 -> v2` blackboard `@mentions` / `thread_id` / `seq`) intact.

1. **First-class `swarm:` gate executor alongside `command:` and `agent:`**:
   - **`command:` / `call:`** (when `agent:` is omitted): autonomous deterministic gate (`0` LLM tokens, `exit_code == 0` -> `pass:`, failure -> `fail:`); keeps `with:` strictly for `call:`/`check:` parameters instead of overloading `with:` for shard collections (`{ items, args }`).
   - **`agent: <name>`**: single cognitive gate (`claim -> [verify] -> submit`).
   - **`swarm: { ... }`**: first-class concurrent/sharded gate — dynamic `swarm.over:` (`spec.tasks | blueprint.tasks`, `input.channels`, `poll.items`, `{ count: N }`) with dependency-wave scheduling up to `max_concurrency`, or static parallel branches `swarm.gates: [...]` joining at one consolidated rejection barrier. Plain `gates: [...]` without `swarm:` stays linear.
2. **Domain separation: `acdd swarm` vs `acdd blackboard` (Pros & Cons)**:
   - *Putting everything (`wait`/`watch`/heartbeats) on `acdd blackboard`*: **Pros** — fewer top-level commands and single surface for `@mention -> wait`. **Cons** — pollutes the semantic `blackboard` knowledge log (`decisions`, `findings`, `blockers`, `notes` injected into `ClaimResp` and run archives) with noisy operational ticks, sentinels, and idle polling churn.
   - *Separating operational swarm control under **`acdd swarm ...`** while keeping `acdd blackboard` strictly semantic*:
     - **`acdd swarm claim <cursor> --worker <id>`**: atomic CAS next-ready shard claim (`ClaimResp` or `{ status: "drained" }`).
     - **`acdd swarm pulse [<cursor>] [--since-gen <g>] [--since-msg <id>]`**: instant (`< 5 ms`), read-only, non-blocking trigger check (`mention`, `blocker`, `thrash_loop`, `stalled_claim`, `wave_ready`, `settled`, plus optional `DomainProvider::swarm_health` hooks for domain-specific rules like `budget_overrun` / `scope_drift` under `INV-STATE-NO-INDEX`).
     - **`acdd swarm wait [<cursor>] [--since-gen <g>] [--since-msg <id>] [--interval <sec>] [--timeout <sec>]`**: built over `pulse` + SQLite change wait with mandatory upper `--timeout`, returning structured trigger events (`mention | anomaly | interval | settled | timeout`) consumable identically by stateless CLI/cron runners, `acdd run`, scripts, or persistent MCP sessions.
3. **Lazy `swarm.supervisor` instead of `observer: true` or `AgentDef.mode: advisor`**:
   - **`swarm.supervisor: { agent, wake_on, interval_seconds?, max_interventions? }`** replaces both `GateDef.observer: true` (no fake barrier `GateNode`, no special trimmed `ClaimResp`) and global `AgentMode::Advisor` (supervision is a property of a `swarm:` container, not a permanent agent identity).
   - Binds relative `@supervisor` / `@coordinator` / `@<swarm_cursor>` to `thread_id = <swarm_cursor>` (`O(depth)` nesting), stays **lazy by default** (`0` supervisor invocations and `0` LLM tokens when shards progress cleanly without triggers or `@mentions`), and closes automatically on `settled`.

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
  - .acdd/profile.yaml
  - crates/acdd-core/src/
  - crates/acdd-state/src/profile/schema.rs
  - crates/acdd-state/src/profile/inherit.rs
  - crates/acdd-state/src/profile/validate.rs
  - crates/acdd-state/src/profile/mod.rs
  - crates/acdd-state/src/profile/embedded/base.yaml
  - crates/acdd-state/src/profile/embedded/code.yaml
  - crates/acdd-state/src/profile/embedded/audit.yaml
  - crates/acdd-state/src/store/sqlite/migrations.rs
  - crates/acdd-state/src/store/sqlite/schema.rs
  - tests/acdd/mod.rs
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: core-swarm-and-blackboard-dtos
    title: "Add pure GateMode (linear|parallel|swarm), WithSource, NamespaceRoot, BlackboardRecipient, and BlackboardWaitReq DTOs in acdd-core"
  - subtask_ref: replace-tasks-block-and-sqlite-v2
    title: "Remove TasksBlock (- tasks:) from GateDef and embedded profiles in favor of mode/with/when/max_concurrency, add optional GateDef.coordinator for recursive group supervision, rename roles/role -> agents/agent (adding coordinator inline agent), group SavePolicy/receipt into mutually exclusive save variants including save: handoff { archive, prune, commit_scope, immutable, scope_mode?, repack?, report? }, lift root-gate when: restriction, plus SQLite v1->v2 blackboard @mentions/thread_id/seq migration"
  - subtask_ref: align-acdd-profile-and-handoff-scope
    title: "Update .acdd/profile.yaml to agents:/agent: + grouped save: handoff, simplify verify_tasks_integrated / archive_spec_with_git via scope_mode (lazy [default] | strict | off) + always-stamped Acdd-Integrated trailers + unblocked pre-handoff polish, integrate only mutating swarm/parallel worktrees, commit self.scope atomically, and implement fail-closed worktree repack modes (none | linear | by_task | last | squash)"
  - subtask_ref: swarm-seam-registration
    title: "Register tests/acdd/swarm.rs in tests/acdd/mod.rs with compiling seam test matrices"
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
    title: "Expose all admitted child gates up to max_concurrency in acdd_state(next) under mode: parallel, validate observer: true strictly with save: none + lightweight ClaimResp envelope (guidance + active sibling scopes), support atomic worker self-claim of explicit or next unclaimed ready swarm shards under SQLite CAS, and join non-observer branches at container barrier with consolidated rejection_findings"
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
  - .acdd/profiles/
  - skills/acdd/SKILL.md
  - skills/acdd/references/acdd-profile-examples/
  - ROADMAP.md
  - CHANGELOG.md
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: mention-routing-and-blocking-wait
    title: "Implement @mention recipient indexing (including relative @coordinator -> nearest enclosing GateDef.coordinator and @<container_cursor> scoping), thread_id queries, container completion sentinel for GateDef.coordinator and observer: true peers, and zero-token acdd_blackboard wait/watch(since_seq, mentions_only, timeout_seconds?, watch?) waking on SQLite commit/notify or universal triggers (budget_overrun, thrash_loop, scope_drift, interval_seconds) with optional watch:<n> cycle limit or run-until-trigger mode"
  - subtask_ref: adr-docs-skills-and-examples-coevolution
    title: "Update ADR-0003, architecture/reference/runbook pages, executable .acdd/profiles/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate"
```

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
  - .acdd/profile.yaml
  - tests/acdd/swarm.rs
status: planned
subtasks:
  - subtask_ref: gate-and-proof-handoff-walkthrough
    title: "Walk through every gate transition, proof artifact propagation (predecessor_proofs, blueprints, reviews, receipts), and budget/independence guard interactively with the human operator"
  - subtask_ref: profile-logic-and-edge-case-signoff
    title: "Verify every profile logic element (mode, with, when, save/handoff/repack, @mentions, wait, and ClaimResp context tiers) against live rendered outputs and record operator sign-off"
```
