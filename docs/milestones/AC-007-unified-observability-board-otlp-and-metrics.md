---
id: AC-007-unified-observability-board-otlp-and-metrics
title: "CLI MCP split and unified local observability server: acdd mcp, read-only acdd serve [--webui] [--telemetry], Live Gate Board UI, Architecture Map, Specs/Roadmap/Diff Viewer, OTLP/HTTP trace export, and Prometheus metrics"
doc_type: contract
status: planned
depends_on:
  - AC-005-unified-swarm-gates-and-peer-board-coordination
  - AR-007-host-sqlite-engine-isolation-and-workspace-facade
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/telemetry.rs
  - crates/acdd-index/src/db/mod.rs
  - crates/acdd-index/src/db/map.rs
  - crates/acdd-state/src/git_ops/
  - crates/acdd-state/src/observability/
  - src/observability/
  - src/cli/args.rs
  - src/cli/serve.rs
  - src/cli/mcp.rs
  - src/cli/mod.rs
  - src/cli/reload.rs
  - src/telemetry.rs
  - install.sh
  - tests/acdd/
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - .acdd/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-007-unified-observability-board-otlp-and-metrics.md
invariants:
  - 'INV-ZERO-FRAMEWORK-DEPS: Implement HTTP serving, SSE streaming, Prometheus Text 0.0.4 exposition, OTLP/HTTP JSON export, and the offline single-file HTML5 Canvas/SVG Web UI (Live Gate Board + Architecture Map + Specs/Roadmap/Docs/Diff) using standard library / existing workspace crates (tokio, serde_json, petgraph in acdd-index) with zero new web, JS/NPM, metrics, or gRPC dependencies.'
  - 'INV-STDIO-MCP-SEPARATION: Explicit acdd mcp owns session-scoped stdio MCP serving (updated across install.sh and adapter templates with safe transition handling for legacy bare acdd / reload invocations), cleanly separated from local HTTP serving under acdd serve.'
  - 'INV-READONLY-OBSERVABILITY: acdd serve (--webui, --telemetry, and default combined mode) opens .acdd/state.sqlite and .acdd/code-map.sqlite strictly with SQLITE_OPEN_READ_ONLY and reads .acdd/run/<run_id>/traces.jsonl non-destructively, never blocking state writers, mutating lifecycle state, or exposing inbound webhooks or write routes.'
  - 'INV-PROMETHEUS-BOUNDED-CARDINALITY: Prometheus labels admit only bounded low-cardinality dimensions (workspace, spec, workflow, gate, agent, kind, tool, status, direction, reason); high-cardinality values (run_id, task_ref, worker_id, session_id, commit_sha, file paths) are forbidden in /metrics labels.'
  - 'INV-SECURITY-LOOPBACK-AND-REDACTION: Bind acdd serve to 127.0.0.1 by default unless --bind is explicitly supplied, keep AC-007 strictly single-workspace (multi-project hub deferred to AC-009), enforce WorkspacePathPolicy / sensitive-path exclusion and open_confined checks on /api/map and /api/docs (admitting only configured docs: and specs: Markdown files), allow read-only /api/diff strictly when bound to loopback 127.0.0.1 with Host/Origin loopback checks and scoped exclusively to the target task admitted scope (refusing non-loopback --bind without AC-009 auth, oversized/binary patches, and arbitrary workspace files), and never expose un-scoped raw source files, environment secrets, .acdd/*.sqlite, or sensitive-matched paths in /metrics, /api/*, or OTLP span payloads.'
  - 'INV-CORE-LEAF: Keep pure wire DTOs and text/JSON serializers in acdd-core free of filesystem, SQLite, and network I/O; extend existing crates/acdd-state/src/observability/ for snapshot/Prometheus aggregation and crates/acdd-state/src/git_ops/ for task-scoped diffing (INV-GIT-SINGLE-OWNER), host read-only code-map graph analytics in crates/acdd-index/src/db/map.rs (preserving INV-STATE-NO-INDEX), and compose HTTP/SSE/OTLP transport in src/observability/.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-007 ships in the same milestone with updated ADR-0003 and ADR-0006, architecture/reference/runbook docs (including docs/reference/mcp-setup.md), executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, skills/acdd/references/adapters.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: GIVEN a workspace with .acdd/state.sqlite, .acdd/code-map.sqlite, and traces.jsonl, WHEN observability snapshot aggregation and GET /metrics run, THEN databases open strictly with SQLITE_OPEN_READ_ONLY (zero writes or missing-DB creation), Prometheus Text 0.0.4 parses cleanly, and any high-cardinality label dimension is rejected/excluded by bounded-cardinality policy.'
  - 'AC-2: GIVEN appended spans in .acdd/run/<run_id>/traces.jsonl (including GenAI token usage attributes gen_ai.usage.input_tokens, gen_ai.usage.output_tokens, optional gen_ai.usage.cache_read_tokens, and acdd.session.action/acdd.session.reuse_reason) and an OTLP/HTTP collector endpoint, WHEN the incremental OTLP shipper runs, THEN it forwards W3C SpanRecord batches from persisted byte-offset cursors preserving numeric GenAI token/session-reuse attributes while redacting sensitive fields and applying bounded backoff; when the collector is offline, it drops/retries gracefully without blocking CLI/serve.'
  - 'AC-3: GIVEN acdd mcp and acdd serve [--webui] [--telemetry], WHEN acdd mcp runs it serves stdio MCP (with install.sh and SIGHUP reload updated cleanly); WHEN acdd serve runs on 127.0.0.1 (default launching both --webui and --telemetry), THEN any POST/PUT/DELETE mutation route fails closed with 405, --webui serves GET /, GET /api/*, and GET /events SSE rendering the full Run execution DAG (linear | parallel | swarm status highlights + click-on-gate Node Inspector showing live worker claims, node-filtered blackboard/@mention messages, verify logs, and span waterfalls), and --telemetry serves headless GET /metrics + OTLP relay.'
  - 'AC-4: GIVEN the completed AC-007 tree, WHEN verified, THEN ADR-0003, ADR-0006, docs/architecture/, docs/reference/{workflows,tasks,cli,mcp-tools,mcp-setup,configuration}.md, docs/runbooks/, .acdd/workflows/*.yaml, skills/acdd/references/acdd-workflow-examples/*.yaml, skills/acdd/references/adapters.md, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md pass acdd docs check and acdd validate with zero errors.'
  - 'AC-5: GIVEN an indexed workspace served by acdd serve --webui, WHEN GET /api/map?view=communities|files|symbols is queried, THEN it returns read-only PageRank centrality, deterministic AST complexity, fan-in/fan-out coupling, Kosaraju cycle flags, and dead_tails counts filtered through WorkspacePathPolicy (zero raw source snippets or sensitive paths), supports a Planned Specs Blast-Radius overlay (combining indexed spec symbol citations from code-map.sqlite + owners/scope globs + inbound/outbound impact callers/callees, filterable by roadmap specs vs proposals), and GET /events streams Live Agent Lens pulses highlighting active task scopes and live agent tool targets in the offline HTML5 Canvas/SVG map.'
  - 'AC-6: GIVEN a single workspace served by acdd serve --webui, WHEN GET /api/roadmap and GET /api/docs / GET /api/docs/content?path=... are queried, THEN /api/roadmap returns StateQuery::Roadmap waves/overlaps/task statuses and /api/docs serves strictly acdd.yaml-admitted docs/specs/ADRs/memories Markdown via open_confined (rejecting raw code, sensitive paths, and ../ escapes with 403/404); WHEN GET /api/diff?run=<id>&task=<ref> is queried on loopback 127.0.0.1 with loopback Host/Origin, THEN it returns bounded unified diff hunks strictly confined to that task scope (refusing non-loopback --bind without AC-009 auth with 403) wired directly into the Node Inspector and Architecture Map Changed overlay.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
  - agent-instructions
---

# AC-007: CLI MCP split and unified local observability server: `acdd mcp`, read-only `acdd serve [--webui] [--telemetry]`

## Goal & Non-goals

**Goal.** Separate session-scoped `stdio` MCP serving (`acdd mcp`) from a strictly read-only, single-workspace local observability server (`acdd serve [--webui] [--telemetry]` bound to `127.0.0.1`) built directly on `.acdd/state.sqlite`, `.acdd/run/<run_id>/traces.jsonl`, and `.acdd/code-map.sqlite` with zero new web/JS/metrics/gRPC dependencies. Deliver one cohesive offline Web UI covering:
1. **Live Gate Board & Click-on-Gate Node Inspector** (`linear | parallel | swarm` execution DAG, live claims, node-filtered `@mention` / `blackboard` threads over SSE, `verify` logs, W3C span waterfalls),
2. **Architecture Map & Live Agent Lens** (`GET /api/map` with PageRank, AST complexity, coupling, Kosaraju cycles, `dead_tails`, and real-time agent/scope highlights),
3. **Specs, Roadmap & Confined Docs Viewer** (`GET /api/roadmap` + `GET /api/docs` with bidirectional `Map <-> Docs` navigation),
4. **Loopback-Only Task-Scoped Diff Viewer** (`GET /api/diff` wired into the Node Inspector and Map `Changed` overlay),
5. **Headless Telemetry** (bounded-cardinality Prometheus `0.0.4` `/metrics` + incremental OTLP/HTTP JSON trace shipper).

**Non-goals.**
- Multi-project folder registry (`WorkspaceRegistry`), authenticated Web UI mutations (`--interactive`), external webhooks (`--incoming`), `.acdd/acdd.sock`, Streamable HTTP MCP (`acdd mcp --http`), or `acdd up` supervisor (all deferred to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)).
- Full-repository source file tree / IDE code editor or exposing un-scoped raw source code over HTTP.
- Adding web frameworks (`axum`/`actix`), React/NPM toolchains, or gRPC/OpenTelemetry crates to `Cargo.toml`.

## Agreed contracts

### 1. CLI split & server modes
- `acdd mcp` (`src/cli/mcp.rs`): session-scoped `stdio` MCP (`install.sh`, `src/cli/reload.rs`, and adapter templates updated with safe transition for legacy bare `acdd`).
- `acdd serve [--webui] [--telemetry]` (`src/cli/serve.rs`): strictly read-only loopback server (`127.0.0.1` default; neither flag launches both `--webui` and `--telemetry`; `--once` dumps snapshot JSON/Prometheus and exits).

### 2. Bounded Prometheus metrics catalog (`GET /metrics`) & GenAI token/session telemetry
- **Lifecycle & Gate Flow**: `acdd_runs_active{workspace, spec, workflow}`, `acdd_runs_completed_total{workspace, spec, workflow}`, `acdd_gate_nodes{workspace, workflow, gate, status}`, `acdd_gate_claims_total{workspace, workflow, gate, outcome}`, `acdd_claim_oldest_age_seconds{workspace, workflow, gate}`.
- **Agent Context & Session Reuse (`AC-005` bridge)**: `acdd_agent_context_tokens{workspace, workflow, gate, agent}` (latest observed context-window token count from `gate_nodes.last_context_tokens` / `traces.jsonl`), `acdd_agent_tokens_total{workspace, workflow, gate, agent, direction}` (`input` | `output`), and `acdd_agent_session_resets_total{workspace, workflow, gate, agent, reason}` (`policy_never` | `max_reuses` | `context_limit`). Runner/gate spans in `traces.jsonl` and OTLP export carry `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, optional `gen_ai.usage.cache_read_tokens`, `acdd.session.action` (`fresh` | `reuse`), and `acdd.session.reuse_reason` (preserving numeric counts under secret redaction; `acdd serve` stays strictly read-only while `acdd run` / `acdd submit` writes `last_context_tokens` into `gate_nodes` for `AC-005` `max_context_tokens` enforcement).
- **Verification & Test Suite Health**: `acdd_verify_runs_total{workspace, command, status}`, `acdd_verify_duration_seconds{workspace, command}`.
- **MCP Tooling & Code-Map Health**: `acdd_mcp_calls_total{workspace, tool, status}`, `acdd_mcp_response_bytes_total{workspace, tool}`, `acdd_mcp_duration_seconds{workspace, tool}`, `acdd_codemap_age_seconds{workspace}`, `acdd_codemap_nodes{workspace, kind}`.

### 3. Single-workspace Web UI projections (`--webui`)
- **Live Gate Board & Node Inspector**: Full `Run` graph with pulsing active `in_progress`/`ready`/`escalated` highlights; clicking any gate opens its unified Node Inspector (active/past claims with `context_tokens`, `reuse_count`, and `session_action` fresh/reuse reason, node-filtered `blackboard` messages over SSE, changed files + unified diff, `verify` logs, and span waterfall).
- **Architecture Map & Live Agent Lens (`GET /api/map`)**: Zoomable `Communities -> Files -> Symbols` HTML5 Canvas/SVG graph with `Complexity`, `PageRank`, `Dead Tails`, `Cycles`, `Changed`, `Planned Specs Blast-Radius` (indexed spec symbol citations + `owners:`/`scope:` + `impact` callers/callees, showing which planned specs touch each symbol/module), and `Live Agent Lens` overlays.
- **Specs, Roadmap, Docs & Task-Scoped Diff (`GET /api/roadmap`, `/api/docs`, `/api/diff`)**: Reuses `StateQuery::Roadmap`, serves only `acdd.yaml`-admitted Markdown via `open_confined`, and guards `/api/diff` strictly to loopback `127.0.0.1` + task `container_scope` + `WorkspacePathPolicy` (refusing non-loopback `--bind` without `AC-009` auth).

## Consumer & execution path map

| Surface | Canonical owner | Live consumers updated | Fail-closed rejection | Deferred destination |
|---|---|---|---|---|
| Read-only snapshot & Prometheus `/metrics` | `crates/acdd-state/src/observability/` (extending existing `mod.rs`, `digest.rs`, `metrics.rs`) + `crates/acdd-core/src/telemetry.rs` | `src/observability/http.rs`, `src/cli/serve.rs` | Missing DB returns empty read-only snapshot (never creates DB); high-cardinality label keys rejected | `AC-009` multi-project `{workspace="<id>"}` aggregation |
| OTLP/HTTP JSON trace relay | `src/observability/otlp.rs` + `src/telemetry.rs` | `src/cli/serve.rs` (`--telemetry`) | Offline collector backs off & drops boundedly; secrets/sensitive paths redacted before send | — |
| `acdd mcp` & read-only `acdd serve` + Node Inspector | `src/cli/{args,mcp,serve,reload,mod}.rs` + `src/observability/{mod,http}.rs` + `page.html` | `install.sh`, `docs/reference/mcp-setup.md`, `skills/acdd/references/adapters.md` | Non-GET/HEAD mutation routes fail with `405`; malformed cursors return `400` | `AC-009` `--interactive`, `--incoming`, `acdd mcp --http`, `acdd up` |
| Architecture Map & Live Agent Lens | `crates/acdd-index/src/db/map.rs` (reusing `cycles.rs` + `dead_tails.rs`) + `src/observability/http.rs` | `src/observability/page.html` | Unadmitted/sensitive paths excluded via `WorkspacePathPolicy`; zero raw source snippets | `AC-009` `/p/{project_id}/api/map` |
| Single-project Roadmap, Docs & Task Diff | `crates/acdd-state/src/usecases/roadmap.rs` + `git_ops/` + `src/observability/http.rs` | `src/observability/page.html`, `src/cli/serve.rs` | `/api/docs` rejects raw code, `.acdd/*.sqlite`, and `../` (`403`/`404`); `/api/diff` rejects non-loopback `--bind` or bad `Host`/`Origin` (`403`) and caps oversized/binary patches | `AC-009` Portfolio & multi-project routing |

## Operational bounds & failure recovery

- **Non-blocking read-only SQLite**: Every read in `acdd serve` uses `SQLITE_OPEN_READ_ONLY` + `SQLITE_OPEN_NOFOLLOW` + `confined_database_open_path` with zero write transactions so active `acdd run` / `acdd claim` writers never experience lock contention.
- **Bounded memory & payload caps**: `GET /api/map` caps returned nodes/edges per zoom tier; `GET /api/diff` uses `--no-ext-diff --no-textconv` with strict per-file and total byte/line caps; SSE `GET /events` uses bounded broadcast channels with clean slow-client lag recovery.
- **Offline loopback UI**: `src/observability/page.html` embeds 100% of its CSS/JS/Canvas/SVG with zero external CDN or NPM assets.

## Tasks in this milestone

### task: observability-snapshot-and-prometheus-exposition

```yaml
task_ref: observability-snapshot-and-prometheus-exposition
target: "Extend existing crates/acdd-state/src/observability/ and crates/acdd-core/src/telemetry.rs with read-only multi-source snapshot aggregation, bounded-cardinality Prometheus Text 0.0.4 rendering, and register tests/acdd/observability.rs"
subtasks:
- subtask_ref: readonly-multi-source-snapshot
  title: "Aggregate active runs, gate_nodes, gate_claims, gate_traces, run_tombstones, codemap metadata, and JSONL span summaries strictly via SQLITE_OPEN_READ_ONLY without creating missing DBs"
- subtask_ref: prometheus-text-renderer
  title: "Extend crates/acdd-state/src/observability/metrics.rs to render valid Prometheus 0.0.4 metrics with escaped label values, compile-time/test-proven bounded label cardinality, and register tests/acdd/observability.rs in tests/acdd/mod.rs"
contract_revision: 1
tags:
- default
depends_on: []
scope:
- crates/acdd-core/src/telemetry.rs
- crates/acdd-state/src/lib.rs
- crates/acdd-state/src/observability/
- tests/acdd/mod.rs
- tests/acdd/observability.rs
status: planned
```

- **Wave**: `0` | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-1`)**: `tests/acdd/observability.rs` proves snapshot aggregation never creates a missing SQLite file, renders valid Prometheus 0.0.4 text, and rejects high-cardinality labels (`run_id`, `task_ref`, file paths).

### task: otlp-http-json-exporter

```yaml
task_ref: otlp-http-json-exporter
target: "Implement incremental OTLP/HTTP JSON trace forwarding from traces.jsonl in src/observability/otlp.rs with persisted byte-offset cursors, sensitive redaction, and non-blocking retry backoff"
subtasks:
- subtask_ref: jsonl-tail-and-otlp-batch-payload
  title: "Convert W3C SpanRecord batches (including gen_ai.usage.input_tokens, gen_ai.usage.output_tokens, optional gen_ai.usage.cache_read_tokens, and acdd.session.action/reuse_reason attributes) into OTLP ExportTraceServiceRequest JSON with persisted byte-offset cursors and sensitive-field redaction"
- subtask_ref: bounded-http-delivery
  title: "Push batches to configured local/remote HTTP endpoints with timeout bounds and graceful drop/backoff when the collector is offline"
contract_revision: 1
tags:
- default
depends_on:
- observability-snapshot-and-prometheus-exposition
scope:
- src/observability/mod.rs
- src/observability/otlp.rs
- src/telemetry.rs
- tests/acdd/observability.rs
status: planned
```

- **Wave**: `1` | **Budget**: `~380` net LOC.
- **Seam & mutation proof (`AC-2`)**: `tests/acdd/observability.rs` proves incremental tailing resumes from persisted byte offsets without duplicate export, redacts secret fields, and degrades cleanly on collector timeout/refusal.

### task: cli-mcp-split-and-read-only-serve

```yaml
task_ref: cli-mcp-split-and-read-only-serve
target: "Introduce explicit acdd mcp stdio serving (updating install.sh and reload transition handling) and read-only single-workspace acdd serve [--webui] [--telemetry] with the Live Gate Board and click-on-gate Node Inspector"
subtasks:
- subtask_ref: stdio-mcp-command-and-installer-transition
  title: "Add explicit acdd mcp (src/cli/args.rs, src/cli/mcp.rs), update install.sh and SIGHUP reload matching, and preserve safe transition handling for legacy bare acdd stdio invocations"
- subtask_ref: loopback-serve-webui-and-telemetry-router
  title: "Serve read-only acdd serve [--webui] [--telemetry] on 127.0.0.1 (--webui GET /, /api/*, /events SSE; --telemetry GET /metrics + OTLP relay; default launches both; --once snapshot dump) rejecting non-GET/HEAD mutations with 405"
- subtask_ref: embedded-dag-and-waterfall-board
  title: "Provide a single-file embedded HTML/SVG board rendering the full linear | parallel | swarm execution DAG with active/completed/blocked/escalated status highlights, plus a click-on-gate Node Inspector showing that node's active worker claims, filtered blackboard/@mention messages over SSE, verify logs, and span waterfall timings"
contract_revision: 1
tags:
- default
depends_on:
- observability-snapshot-and-prometheus-exposition
- otlp-http-json-exporter
scope:
- src/observability/mod.rs
- src/observability/http.rs
- src/observability/page.html
- src/cli/args.rs
- src/cli/mod.rs
- src/cli/serve.rs
- src/cli/mcp.rs
- src/cli/reload.rs
- install.sh
- tests/acdd/observability.rs
status: planned
```

- **Wave**: `2` | **Budget**: `~800` net LOC.
- **Seam & mutation proof (`AC-3`)**: `tests/acdd/observability.rs` proves `acdd mcp` serves stdio handshake, `acdd serve` rejects `POST` routes with `405`, SSE streams gate/blackboard deltas, and Node Inspector endpoints filter claims and blackboard threads strictly to the selected node cursor.

### task: architecture-map-and-live-lens

```yaml
task_ref: architecture-map-and-live-lens
target: "Compute read-only PageRank centrality, deterministic AST complexity, coupling, cycle SCC, and dead-tail projections in crates/acdd-index/src/db/map.rs, expose GET /api/map and SSE live agent lens pulses in src/observability/http.rs, and render an interactive zoomable Architecture Map view in src/observability/page.html"
subtasks:
- subtask_ref: codemap-pagerank-and-complexity-aggregator
  title: "Add read-only module/community, file, and symbol map queries with power-iteration PageRank, AST branch complexity, fan-in/fan-out coupling, Kosaraju cycle flags, dead_tails counts, and WorkspacePathPolicy sensitive-path filtering without source snippets"
- subtask_ref: map-api-and-sse-live-lens
  title: "Expose GET /api/map?view=communities|files|symbols&module=...&signal=... in src/observability/http.rs composing acdd-index map analytics (including Planned Specs Blast-Radius via indexed spec doc_edges + owners/scope + impact traversal) with acdd-state active task scopes and traces.jsonl SSE lens pulses while preserving INV-STATE-NO-INDEX"
- subtask_ref: interactive-canvas-map-and-overlays
  title: "Add an offline zero-dependency HTML5 Canvas/SVG Architecture Map tab to src/observability/page.html supporting zoom drill-down, Complexity/PageRank/Dead/Cycles/Planned-Blast-Radius overlays (filterable by roadmap specs vs proposals), and live agent lens highlights with tests/acdd/observability.rs seams"
contract_revision: 1
tags:
- default
depends_on:
- cli-mcp-split-and-read-only-serve
scope:
- crates/acdd-core/src/telemetry.rs
- crates/acdd-index/src/db/mod.rs
- crates/acdd-index/src/db/map.rs
- src/observability/mod.rs
- src/observability/http.rs
- src/observability/page.html
- tests/acdd/observability.rs
status: planned
```

- **Wave**: `3` | **Budget**: `~750` net LOC.
- **Seam & mutation proof (`AC-5`)**: `tests/acdd/observability.rs` proves `/api/map` ranks hub modules above leaves via deterministic PageRank, flags synthetic cycle/dead_tail fixtures, omits sensitive paths and raw source bodies, and emits SSE lens pulses on trace appends.

### task: roadmap-and-docs-projection

```yaml
task_ref: roadmap-and-docs-projection
target: "Expose read-only GET /api/roadmap, confined GET /api/docs, and loopback-only task-scoped GET /api/diff in src/observability/http.rs (backed by StateQuery::Roadmap and crates/acdd-state/src/git_ops/), and render the Specs, Roadmap, Docs Reader, and Task Diff views in src/observability/page.html"
subtasks:
- subtask_ref: readonly-roadmap-and-admitted-docs-api
  title: "Add GET /api/roadmap (Wave 0..N DAG, scope overlaps, active/archived specs, task statuses) and strictly confined GET /api/docs + GET /api/docs/content?path=... (open_confined + WorkspacePathPolicy admitting only acdd.yaml docs and specs roots plus code-map doc citations, rejecting raw code and sensitive paths)"
- subtask_ref: task-scoped-diff-api-and-guard
  title: "Add read-only task-scoped diff helper in crates/acdd-state/src/git_ops/ and expose loopback-only GET /api/diff?run=...&task=... (Host/Origin loopback guard, fail closed on non-loopback --bind without auth, confined strictly to task container_scope + WorkspacePathPolicy between base_head and candidate/deliver commit with byte/line caps and --no-ext-diff)"
- subtask_ref: specs-roadmap-docs-and-diff-webui
  title: "Add offline Specs & Roadmap tab, Markdown Document Viewer with bidirectional Map <-> Docs navigation, and unified Task Diff drawer (wired directly into the Live Gate Board Node Inspector so clicking a gate lists its changed files and opens colored +/- hunks beside review findings, plus Changed overlay on Architecture Map) to src/observability/page.html with tests/acdd/observability.rs seams"
contract_revision: 1
tags:
- default
depends_on:
- architecture-map-and-live-lens
scope:
- crates/acdd-core/src/telemetry.rs
- crates/acdd-state/src/git_ops/
- crates/acdd-state/src/observability/
- src/observability/mod.rs
- src/observability/http.rs
- src/observability/page.html
- src/cli/serve.rs
- tests/acdd/observability.rs
status: planned
```

- **Wave**: `4` | **Budget**: `~650` net LOC.
- **Seam & mutation proof (`AC-6`)**: `tests/acdd/observability.rs` proves `/api/docs` rejects `src/main.rs`, `.env`, and `../` escapes with `403`/`404`, `/api/diff` rejects non-loopback `--bind` or bad `Host`/`Origin` with `403`, and `/api/diff` omits any modified file outside the target task's admitted scope.

### task: observability-docs-and-runbooks

```yaml
task_ref: observability-docs-and-runbooks
target: "Co-evolve ADR-0003/ADR-0006, architecture/reference/runbook docs, executable workflow/profile examples, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md for acdd mcp, acdd serve [--webui] [--telemetry], Architecture Map & Live Agent Lens, Specs/Roadmap, Docs & Task-Scoped Diff Viewer, Prometheus/Grafana scraping, and OTLP relay"
subtasks:
- subtask_ref: cli-and-config-reference
  title: "Update ADR-0003, ADR-0006, architecture/reference pages (profiles, tasks, cli, mcp-tools, mcp-setup, configuration), skills/acdd/references/adapters.md, and skills/acdd/SKILL.md for acdd mcp and acdd serve [--webui] [--telemetry] (including GET /api/map, Live Agent Lens, GET /api/roadmap, confined GET /api/docs, and loopback GET /api/diff)"
- subtask_ref: prometheus-otlp-quickstart-and-examples
  title: "Add Prometheus + Jaeger/Grafana runbook, update .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml examples, update ROADMAP.md and CHANGELOG.md, and pass acdd docs check and acdd validate"
contract_revision: 1
tags:
- docs
depends_on:
- roadmap-and-docs-projection
scope:
- docs/adr/0003-unified-profile-graph-and-state-machine.md
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/architecture/
- docs/reference/workflows.md
- docs/reference/tasks.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- docs/reference/mcp-setup.md
- docs/reference/configuration.md
- docs/runbooks/observability.md
- docs/runbooks/README.md
- .acdd/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/adapters.md
- skills/acdd/references/acdd-workflow-examples/
- ROADMAP.md
- CHANGELOG.md
status: planned
```

- **Wave**: `5` | **Budget**: `~450` net LOC (docs/runbooks).
- **Seam & mutation proof (`AC-4`)**: `acdd docs check` and `acdd validate` pass with zero errors and all reference/runbook examples parse cleanly.

## Negative boundaries

- No new web, JS/NPM, metrics, or gRPC dependencies in `Cargo.toml` (`INV-ZERO-FRAMEWORK-DEPS`).
- No state mutations, inbound webhooks, or multi-project routing in `AC-007` (deferred to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md)).
- No full-repo file-tree code browser or un-scoped raw source code endpoints; `/api/diff` is strictly loopback-only and task-scope confined.
- No high-cardinality labels in `/metrics` and no `acdd-index` imports inside `crates/acdd-state` (`INV-STATE-NO-INDEX`).

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-1`..`AC-6` map 1:1 across Waves 0–5 to `tests/acdd/observability.rs` and co-evolved ADR/docs/runbook updates.
- **Correctness**: Every endpoint enforces fail-closed bounds (`SQLITE_OPEN_READ_ONLY`, `405` on mutations, `403`/`404` on path/scope/non-loopback escapes, bounded Prometheus cardinality, bounded diff caps) with explicit mutation proofs.
- **Coherence**: Reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), and [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md); reuses existing `crates/acdd-state/src/observability/` and `crates/acdd-index/src/db/{cycles,traversal}` owners across strictly sequential zero-overlap waves.
