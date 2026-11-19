---
id: AC-007-unified-observability-board-otlp-and-metrics
title: "CLI MCP split and unified local observability server: acdd mcp, read-only acdd serve [--webui] [--telemetry], Live Gate Board UI, OTLP/HTTP trace export, and Prometheus metrics"
doc_type: contract
status: planned
depends_on:
  - AC-005-unified-swarm-gates-and-peer-board-coordination
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/telemetry.rs
  - crates/acdd-state/src/observability/
  - src/observability/
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
  - docs/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-007-unified-observability-board-otlp-and-metrics.md
invariants:
  - 'INV-ZERO-FRAMEWORK-DEPS: Implement HTTP serving, SSE streaming, Prometheus Text 0.0.4 exposition, and OTLP/HTTP JSON export using standard library / existing workspace crates (tokio, serde_json) with zero new web, metrics, or gRPC dependencies in Cargo.toml.'
  - 'INV-STDIO-MCP-SEPARATION: Explicit acdd mcp owns session-scoped stdio MCP serving (updated across install.sh and adapter templates with safe transition handling for legacy bare acdd / reload invocations), cleanly separated from local HTTP serving under acdd serve.'
  - 'INV-READONLY-OBSERVABILITY: acdd serve (--webui, --telemetry, and default combined mode) opens .acdd/state.sqlite and .acdd/code-map.sqlite strictly with SQLITE_OPEN_READ_ONLY and reads .acdd/run/<run_id>/traces.jsonl non-destructively, never blocking state writers, mutating lifecycle state, or exposing inbound webhooks or write routes.'
  - 'INV-PROMETHEUS-BOUNDED-CARDINALITY: Prometheus labels admit only bounded low-cardinality dimensions (workspace, collection, profile, gate, kind, tool, status); high-cardinality values (run_id, task_ref, commit_sha, file paths) are forbidden in /metrics labels.'
  - 'INV-SECURITY-LOOPBACK-AND-REDACTION: Bind acdd serve to 127.0.0.1 by default unless --bind is explicitly supplied, and never expose raw source snippets, environment secrets, or sensitive-matched paths in /metrics, /api/*, or OTLP span payloads.'
  - 'INV-CORE-LEAF: Keep pure wire DTOs and text/JSON serializers in acdd-core free of filesystem, SQLite, and network I/O; keep pure snapshot/Prometheus aggregation in crates/acdd-state/src/observability/ and host HTTP/SSE/OTLP transport in src/observability/.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-007 ships in the same milestone with updated ADR-0003 and ADR-0006 (explicit acdd mcp stdio command, read-only loopback acdd serve [--webui] [--telemetry] Live Gate Board UI, SSE, bounded Prometheus, OTLP relay), architecture/reference/runbook docs (including docs/reference/mcp-setup.md), executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, skills/acdd/references/adapters.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: Read-only multi-source snapshot aggregator reads .acdd/state.sqlite, .acdd/code-map.sqlite, and traces.jsonl under SQLITE_OPEN_READ_ONLY and renders valid Prometheus 0.0.4 /metrics with bounded low-cardinality labels.'
  - 'AC-2: Incremental OTLP/HTTP JSON trace shipper tails traces.jsonl with persisted byte-offset cursors, sensitive redaction, and bounded retry backoff.'
  - 'AC-3: Explicit acdd mcp serves session-scoped stdio MCP (with install.sh and adapter templates updated and safe transition handling for existing configurations), while acdd serve [--webui] [--telemetry] binds 127.0.0.1 strictly read-only: --webui serves GET /, GET /api/*, and GET /events SSE rendering linear | parallel | swarm hierarchies and @mention threads; --telemetry serves headless GET /metrics and background OTLP/HTTP JSON relay; and default acdd serve (neither flag) launches both together with zero inbound state mutations.'
  - 'AC-4: ADR-0003, ADR-0006, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,mcp-setup,configuration}.md, docs/runbooks/, docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/references/adapters.md, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
  - agent-instructions
---

# AC-007: CLI MCP split and unified local observability server: `acdd mcp`, read-only `acdd serve [--webui] [--telemetry]`, Live Gate Board UI, OTLP/HTTP trace export, and Prometheus metrics

## Outcome and purpose

Cleanly separate session-scoped `stdio` MCP serving from local HTTP observability while splitting interactive Web UI inspection from headless monitoring on top of the unified `crates/acdd-state` engine (`.acdd/state.sqlite`, `.acdd/run/<run_id>/traces.jsonl`, and `.acdd/code-map.sqlite`):

1. **Explicit `stdio` MCP Command (`acdd mcp`)**:
   Move session-scoped `stdio` MCP serving into explicit `acdd mcp` (`src/cli/mcp.rs`), updating `install.sh` and MCP adapter templates (`docs/reference/mcp-setup.md`, `skills/acdd/references/adapters.md`) while keeping safe transition handling for bare `acdd` invocations and `acdd reload` `SIGHUP` process discovery.
2. **Unified Read-Only Loopback Server (`acdd serve [--webui] [--telemetry]`)**:
   Replace legacy `acdd serve` stdio aliasing with a strictly read-only local HTTP server (`src/cli/serve.rs`) bound to `127.0.0.1` by default (zero inbound state mutations, zero open webhooks):
   - **`--webui` (Live Gate & Task Board: `GET /`, `GET /api/*`, `GET /events` SSE)**:
     Real-time visualization of active and historical `Run` graphs—natively rendering [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) `mode: linear | parallel | swarm` node hierarchies (`poll[telegram]`, `implement[core-cleanup].build`), active worker claims, live `@mention` / `thread_id` peer `blackboard` threads streamed via `GET /events` SSE, `verify` logs, and W3C span waterfalls read directly from `state.sqlite` and `traces.jsonl`.
   - **`--telemetry` (Headless Monitoring & Trace Relay: `GET /metrics` + OTLP/HTTP JSON)**:
     Exposes stateless derive-on-scrape `GET /metrics` in Prometheus Text `0.0.4` format synthesized by pure `crates/acdd-state/src/observability/` from persistent SQLite tables (`runs`, `gate_nodes`, `gate_claims`, `gate_traces`, `run_tombstones`), `code-map.sqlite` metadata, and MCP spans aggregated from `traces.jsonl` (zero writes to `state.sqlite`)—capturing 100% of short-lived CLI commands without requiring a Prometheus Pushgateway—and runs the background incremental OTLP/HTTP JSON shipper in `src/observability/otlp.rs` forwarding newly appended spans from `.acdd/run/<run_id>/traces.jsonl` (and `.acdd/debug-traces.jsonl`) to configured collectors (`Jaeger`, `Grafana Tempo`, `Arize Phoenix`, `OpenTelemetry Collector`) with durable cursor offsets and bounded backoff.
   - **Default (`acdd serve` with neither `--webui` nor `--telemetry` specified)**:
     Launches **both `--webui` and `--telemetry`** together in strictly read-only mode. Opt-in authenticated write/ingress capabilities (`acdd serve --interactive`, `acdd serve --incoming`, `acdd mcp --http`) are deferred to [AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md).

## Prometheus metrics catalog (bounded cardinality)

- **Lifecycle & Gate Flow**:
  - `acdd_runs_active{workspace, collection, profile}` (`gauge`) — active runs by status.
  - `acdd_runs_completed_total{workspace, collection, profile}` (`counter`) — derived from `run_tombstones`.
  - `acdd_gate_nodes{workspace, profile, gate, status}` (`gauge`) — current node distribution (`ready`, `in_progress`, `stale`, `escalated`, `completed`).
  - `acdd_gate_claims_total{workspace, profile, gate, outcome}` (`counter`) — pass / reject / superseded churn per gate.
  - `acdd_claim_oldest_age_seconds{workspace, profile, gate}` (`gauge`) — age of the longest currently running `in_progress` claim (zombie/stall detection).
- **Verification & Test Suite Health**:
  - `acdd_verify_runs_total{workspace, command, status}` (`counter`) — mechanical verification executions (`completed`, `failed`, `timed_out`).
  - `acdd_verify_duration_seconds{workspace, command}` (`summary`/`histogram`) — command runtime distribution from `gate_traces`.
- **MCP Tooling & Code-Map Health**:
  - `acdd_mcp_calls_total{workspace, tool, status}` (`counter`) — tool invocation volume aggregated from `traces.jsonl`.
  - `acdd_mcp_response_bytes_total{workspace, tool}` (`counter`) — cumulative context budget consumed per MCP tool.
  - `acdd_mcp_duration_seconds{workspace, tool}` (`histogram`) — tool execution latency.
  - `acdd_codemap_age_seconds{workspace}` & `acdd_codemap_nodes{workspace, kind}` (`gauge`) — index freshness and graph scale.

## Tasks in this milestone

### task: observability-snapshot-and-prometheus-exposition

```yaml
task_ref: observability-snapshot-and-prometheus-exposition
target: Build the read-only state/trace/codemap snapshot aggregator and deterministic Prometheus Text 0.0.4 /metrics renderer with strict label-cardinality enforcement
subtasks:
- subtask_ref: readonly-multi-source-snapshot
  title: Aggregate active runs, gate_nodes, gate_claims, gate_traces, run_tombstones, codemap freshness, and JSONL span summaries via SQLITE_OPEN_READ_ONLY
- subtask_ref: prometheus-text-renderer
  title: Render valid Prometheus 0.0.4 metrics with escaped label values and compile-time/test-proven bounded label cardinality
contract_revision: 1
tags:
- default
depends_on: []
scope:
- crates/acdd-core/src/telemetry.rs
- crates/acdd-state/src/lib.rs
- crates/acdd-state/src/observability/mod.rs
- crates/acdd-state/src/observability/metrics.rs
- crates/acdd-state/src/observability/digest.rs
- crates/acdd-state/src/observability/tests.rs
- tests/acdd/observability.rs
```

### task: otlp-http-json-exporter

```yaml
task_ref: otlp-http-json-exporter
target: Implement incremental OTLP/HTTP JSON trace forwarding from traces.jsonl in src/observability/otlp.rs with cursor tracking, redaction, and non-blocking retry backoff
subtasks:
- subtask_ref: jsonl-tail-and-otlp-batch-payload
  title: Convert W3C SpanRecord batches into OTLP ExportTraceServiceRequest JSON with persisted byte-offset cursors
- subtask_ref: bounded-http-delivery
  title: Push batches to configured local/remote HTTP endpoints with timeout bounds and graceful drop/backoff when collector is offline
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
```

### task: cli-mcp-split-and-read-only-serve

```yaml
task_ref: cli-mcp-split-and-read-only-serve
target: Introduce explicit acdd mcp stdio serving (updating install.sh and reload transition handling) and read-only acdd serve [--webui] [--telemetry] in src/cli/serve.rs and src/observability/http.rs
subtasks:
- subtask_ref: stdio-mcp-command-and-installer-transition
  title: Add explicit acdd mcp (src/cli/mcp.rs), update install.sh and SIGHUP reload matching, and preserve safe transition handling for legacy bare acdd stdio invocations
- subtask_ref: loopback-serve-webui-and-telemetry-router
  title: Serve read-only acdd serve [--webui] [--telemetry] on 127.0.0.1 (--webui GET /, /api/*, /events SSE; --telemetry GET /metrics + OTLP relay; default launches both; --once snapshot dump) with zero mutation routes
- subtask_ref: embedded-dag-and-waterfall-board
  title: Provide a single-file embedded HTML/SVG board rendering linear | parallel | swarm node hierarchies, active claims, live @mention peer blackboard threads over SSE, and span waterfall timings
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
- src/cli/mod.rs
- src/cli/serve.rs
- src/cli/mcp.rs
- src/cli/reload.rs
- install.sh
- tests/acdd/observability.rs
```

### task: observability-docs-and-runbooks

```yaml
task_ref: observability-docs-and-runbooks
target: Co-evolve ADR-0003/ADR-0006, architecture/reference/runbook docs, executable workflow/profile examples, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md for acdd mcp, acdd serve [--webui] [--telemetry], Prometheus/Grafana scraping, and OTLP relay
subtasks:
- subtask_ref: cli-and-config-reference
  title: Update ADR-0003, ADR-0006, architecture/reference pages (profiles, tasks, cli, mcp-tools, mcp-setup, configuration), skills/acdd/references/adapters.md, and skills/acdd/SKILL.md for acdd mcp and acdd serve [--webui] [--telemetry]
- subtask_ref: prometheus-otlp-quickstart-and-examples
  title: Add Prometheus + Jaeger/Grafana runbook, update docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml examples, update ROADMAP.md and CHANGELOG.md, and pass acdd docs check and acdd validate
contract_revision: 1
tags:
- docs
depends_on:
- cli-mcp-split-and-read-only-serve
scope:
- docs/adr/0003-unified-profile-graph-and-state-machine.md
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/architecture/
- docs/reference/profiles.md
- docs/reference/tasks.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- docs/reference/mcp-setup.md
- docs/reference/configuration.md
- docs/runbooks/observability.md
- docs/runbooks/README.md
- docs/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/adapters.md
- skills/acdd/references/acdd-profile-examples/
- ROADMAP.md
- CHANGELOG.md
```
