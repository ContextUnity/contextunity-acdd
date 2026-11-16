---
id: AC-005-unified-observability-board-otlp-and-metrics
title: "Unified local observability server: Live Gate Board UI, OTLP/HTTP trace export, and Prometheus metrics"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/telemetry.rs
  - crates/acdd-state/src/observability/
  - src/cli/inspect.rs
  - src/telemetry.rs
  - tests/acdd/
  - docs/architecture/
  - docs/reference/
  - docs/milestones/AC-005-unified-observability-board-otlp-and-metrics.md
invariants:
  - 'INV-ZERO-FRAMEWORK-DEPS: Implement HTTP serving, SSE streaming, Prometheus Text 0.0.4 exposition, and OTLP/HTTP JSON export using standard library / existing workspace crates (tokio, serde_json) with zero new web, metrics, or gRPC dependencies in Cargo.toml.'
  - 'INV-READONLY-OBSERVABILITY: All board, API, and /metrics queries open .acdd/state.sqlite and .acdd/code-map.sqlite strictly with SQLITE_OPEN_READ_ONLY and read .acdd/run/*/traces.jsonl non-destructively, never blocking state writers or mutating lifecycle state.'
  - 'INV-PROMETHEUS-BOUNDED-CARDINALITY: Prometheus labels admit only bounded low-cardinality dimensions (workspace, collection, profile, gate, kind, tool, status); high-cardinality values (run_id, task_ref, commit_sha, file paths) are forbidden in /metrics labels.'
  - 'INV-SECURITY-LOOPBACK-AND-REDACTION: Bind to 127.0.0.1 by default unless --bind is explicitly supplied, and never expose raw source snippets, environment secrets, or sensitive-matched paths in /metrics, /api/*, or OTLP span payloads.'
  - 'INV-CORE-LEAF: Keep pure wire DTOs and text/JSON serializers in acdd-core free of filesystem, SQLite, and network I/O.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
---

# Unified local observability server: Live Gate Board UI, OTLP/HTTP trace export, and Prometheus metrics

## Outcome and purpose

Provide one lightweight, zero-dependency observability subsystem (`acdd inspect`)
on top of the unified `crates/acdd-state` engine (`.acdd/state.sqlite`,
`.acdd/run/<run_name>/traces.jsonl`, and `.acdd/code-map.sqlite`). Instead of
splitting live UI inspection, distributed trace export, and operational metrics
into separate daemons, a single local loopback server exposes:

1. **Live Gate & Task Board (`GET /`, `GET /api/*`, `GET /events` SSE)**:
   Real-time visualization of active and historical `Run` graphs, wave DAG
   progress, active worker claims, `blackboard` threads, `verify` logs, and W3C
   span waterfalls read directly from `state.sqlite` and `traces.jsonl`.
2. **Prometheus Metrics Exposition (`GET /metrics`)**:
   Stateless derive-on-scrape metrics in standard Prometheus Text `0.0.4` format
   synthesized directly from persistent SQLite tables (`runs`, `gate_nodes`,
   `gate_claims`, `gate_traces`, `run_tombstones`), `code-map.sqlite` metadata,
   and `traces.jsonl` counters — capturing 100% of short-lived CLI commands
   without requiring a Prometheus Pushgateway.
3. **OTLP/HTTP JSON Trace Relay (`POST /v1/traces` outbound & local ingest)**:
   Incremental background shipper forwarding newly appended spans from
   `.acdd/run/<run_name>/traces.jsonl` (and `.acdd/debug-traces.jsonl`) to any
   configured OTLP/HTTP JSON collector (`Jaeger`, `Grafana Tempo`, `Arize Phoenix`,
   `OpenTelemetry Collector`) with durable cursor offsets and bounded backoff.

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
target: "Build the read-only state/trace/codemap snapshot aggregator and deterministic Prometheus Text 0.0.4 /metrics renderer with strict label-cardinality enforcement"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on: []
scope:
  - crates/acdd-core/src/telemetry.rs
  - crates/acdd-state/src/lib.rs
  - crates/acdd-state/src/observability/mod.rs
  - crates/acdd-state/src/observability/snapshot.rs
  - crates/acdd-state/src/observability/prometheus.rs
  - tests/acdd/observability.rs
subtasks:
  - subtask_ref: readonly-multi-source-snapshot
    title: "Aggregate active runs, gate_nodes, gate_claims, gate_traces, run_tombstones, codemap freshness, and JSONL span summaries via SQLITE_OPEN_READ_ONLY"
  - subtask_ref: prometheus-text-renderer
    title: "Render valid Prometheus 0.0.4 metrics with escaped label values and compile-time/test-proven bounded label cardinality"
```

### task: otlp-http-json-exporter

```yaml
task_ref: otlp-http-json-exporter
target: "Implement incremental OTLP/HTTP JSON trace forwarding from traces.jsonl with cursor tracking, redaction, and non-blocking retry backoff"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on:
  - observability-snapshot-and-prometheus-exposition
scope:
  - crates/acdd-state/src/observability/otlp.rs
  - src/telemetry.rs
  - tests/acdd/observability.rs
subtasks:
  - subtask_ref: jsonl-tail-and-otlp-batch-payload
    title: "Convert W3C SpanRecord batches into OTLP ExportTraceServiceRequest JSON with persisted byte-offset cursors"
  - subtask_ref: bounded-http-delivery
    title: "Push batches to configured local/remote HTTP endpoints with timeout bounds and graceful drop/backoff when collector is offline"
```

### task: unified-http-server-and-live-board-ui

```yaml
task_ref: unified-http-server-and-live-board-ui
target: "Serve the embedded Live Gate Board UI, JSON/SSE state and waterfall endpoints, and /metrics on one loopback HTTP server via acdd inspect"
proof_policy: seam-test-first
contract_revision: 1
tags: [default]
depends_on:
  - observability-snapshot-and-prometheus-exposition
  - otlp-http-json-exporter
scope:
  - crates/acdd-state/src/observability/http.rs
  - crates/acdd-state/src/observability/page.html
  - src/cli/mod.rs
  - src/cli/inspect.rs
  - tests/acdd/observability.rs
subtasks:
  - subtask_ref: loopback-http-and-sse-router
    title: "Serve GET /, /api/runs, /api/runs/:id, /api/traces/:id, /events (SSE), and /metrics on 127.0.0.1 with --once JSON dump mode"
  - subtask_ref: embedded-dag-and-waterfall-board
    title: "Provide a single-file embedded HTML/SVG board displaying Run waves, gate statuses, blackboard feed, and span waterfall timings"
```

### task: observability-docs-and-runbooks

```yaml
task_ref: observability-docs-and-runbooks
target: "Document acdd inspect, Prometheus/Grafana scraping, and OTLP collector configuration across reference docs and runbooks"
proof_policy: seam-test-first
contract_revision: 1
tags: [docs]
depends_on:
  - unified-http-server-and-live-board-ui
scope:
  - docs/reference/cli.md
  - docs/reference/configuration.md
  - docs/runbooks/observability.md
  - docs/runbooks/README.md
subtasks:
  - subtask_ref: cli-and-config-reference
    title: "Document acdd inspect flags, observability config block, and Prometheus metric definitions"
  - subtask_ref: prometheus-otlp-quickstart
    title: "Add minimal Prometheus scrape + Jaeger/Grafana local setup runbook and pass acdd docs check"
```
