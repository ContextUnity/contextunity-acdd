---
id: AC-009-authenticated-ingress-interactive-webui-and-http-mcp
title: "Authenticated network & IPC ingress, interactive Web UI, Streamable HTTP MCP, and acdd up supervisor"
doc_type: contract
status: planned
depends_on:
  - AC-007-unified-observability-board-otlp-and-metrics
  - AC-008-always-on-reactive-triggers-and-deferred-approvals
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/
  - src/observability/
  - src/mcp/
  - src/cli/
  - tests/acdd/
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - docs/runbooks/
  - docs/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md
invariants:
  - 'INV-SECURITY-ADR-FIRST: ADR-0013 (Transport Security, Authentication, and IPC Ingress) is authored and reconciled in Wave 0 before any stateful HTTP or IPC ingress route is implemented.'
  - 'INV-EXPLICIT-OPT-IN-MUTATIONS: Default acdd serve remains strictly read-only on 127.0.0.1; stateful capabilities (acdd serve --interactive, acdd serve --incoming, acdd mcp --http, .acdd/acdd.sock) require explicit opt-in flags or workspace config and fail closed without valid authentication.'
  - 'INV-HOST-ORIGIN-AND-TOKEN-SCOPING: All HTTP mutation and MCP endpoints enforce strict Host and Origin validation (DNS-rebinding and browser CSRF protection on 127.0.0.1), request body byte caps, rate limiting, and constant-time scoped Bearer token verification (read, approve, worker:<role>, ingest); --incoming also verifies per-source HMAC-SHA256 X-Hub-Signature-256 signatures and timestamp replay windows.'
  - 'INV-UNIX-SOCKET-UID-CONFINEMENT: Local IPC socket .acdd/acdd.sock is created strictly with 0600 permissions inside the confined .acdd directory and validates peer OS UID credentials before enqueuing triggers into AC-008 inbox.'
  - 'INV-WORKER-IDENTITY-BINDING: Streamable HTTP MCP (POST/GET /mcp) enforces per-session Mcp-Session-Id binding against the authenticated Bearer token scope (worker:<role>), rejecting worker_id impersonation on acdd_claim, acdd_submit, and acdd_blackboard.'
  - 'INV-CORE-LEAF: Keep pure transport security policy and auth DTOs (TokenScope, WebhookSourcePolicy, IngressAuthError, ServeCapabilities) in acdd-core free of network, SQLite, or filesystem I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-009 ships in the same milestone with ADR-0013, updated ADR-0003 and ADR-0006, architecture/reference/runbook docs, executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, skills/acdd/references/adapters.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: ADR-0013 is recorded and unified security middleware enforces strict Host/Origin validation, constant-time scoped Bearer tokens (read, approve, worker:<role>, ingest), per-source HMAC-SHA256 X-Hub-Signature-256 with timestamp replay window, body byte caps, and rate limiting.'
  - 'AC-2: Confined .acdd/acdd.sock (0600, OS-UID auth) and acdd serve --incoming (POST /events, standalone headless or combined with --webui) ingest authenticated, deduplicated triggers into AC-008 inbox.'
  - 'AC-3: acdd serve --interactive exposes authenticated Web UI mutation routes for human Approve/Reject of RoleDef.mode: manual / escalated gates and @mention steering on acdd_blackboard without relaxing default read-only acdd serve.'
  - 'AC-4: acdd mcp --http exposes Streamable HTTP MCP (POST/GET /mcp) enforcing Mcp-Session-Id and token-bound worker_id identity, and acdd up composes the workspace-configured combination of webui, telemetry, socket, incoming, mcp_http, and sleep_until(next_timer) timer loop.'
  - 'AC-5: ADR-0013, ADR-0003, ADR-0006, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,mcp-setup,configuration}.md, docs/runbooks/, docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/references/adapters.md, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
  - acdd
  - contract-planning
  - test-contract-seam
  - agent-instructions
---

# AC-009: Authenticated network & IPC ingress, interactive Web UI, Streamable HTTP MCP, and `acdd up` supervisor

## Outcome and purpose

Consolidate all stateful HTTP and local IPC transport, authentication, and authorization
concerns on top of [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)'s
read-only loopback server (`acdd serve [--webui] [--telemetry]`) and
[AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)'s zero-daemon
reactive core (`RoleDef.mode: manual`, SQLite `timers`, `inbox`, `outbox`, `acdd tick`),
governed upfront by a mandatory Transport & Security ADR (`ADR-0013`):

1. **Unified Local & Network Security Layer (`ADR-0013`)**:
   Enforce strict `Host` and `Origin` validation (preventing DNS-rebinding and
   cross-origin browser CSRF against `127.0.0.1`), constant-time scoped Bearer token
   verification (`read`, `approve`, `worker:<role>`, `ingest`), per-source HMAC-SHA256
   webhook signature verification (`X-Hub-Signature-256` with timestamp replay window),
   explicit request body byte caps, and bounded rate limiting across all stateful routes.
2. **Confined Local Unix Domain Socket (`.acdd/acdd.sock`, `0600`)**:
   Provide zero-port, OS-UID-authenticated local IPC ingress for Git hooks, file
   watchers, and co-located scripts pushing triggers directly into `AC-008`'s `inbox`.
3. **Interactive Web Control (`acdd serve --interactive`)**:
   Opt-in authenticated mutation routes layered onto `--webui` enabling human
   `Approve / Reject` decisions for parked `RoleDef.mode: manual` / `escalated`
   gates and `@mention` steering on `acdd_blackboard`. Default `acdd serve` remains
   strictly read-only when `--interactive` is omitted.
4. **External Webhook Listener (`acdd serve --incoming`)**:
   Authenticated `POST /events` HTTP ingress feeding `AC-008`'s deduplicated/coalesced
   `inbox` (usable standalone headless on servers or combined with `--webui`).
5. **Streamable HTTP MCP Transport (`acdd mcp --http`)**:
   Standard MCP Streamable HTTP endpoint (`POST/GET /mcp`) allowing containerized,
   VM, and remote workers—as well as dense local `AC-005` `swarm` pools—to share
   one warm code-map index and state engine process, enforcing per-session
   `Mcp-Session-Id` and token-bound `worker_id` identity (preventing worker
   impersonation on `acdd_claim` and `acdd_submit`).
6. **Unified Supervisor Alias (`acdd up`)**:
   Single entrypoint launching the workspace's configured combination of `webui`,
   `telemetry`, `socket`, `incoming`, `mcp_http`, and an interruptible `timers`
   loop (`sleep_until(next_timer)` with zero busy-polling).

## Tasks in this milestone

### task: security-adr-and-auth-middleware

```yaml
task_ref: security-adr-and-auth-middleware
target: Author upfront ADR-0013, define pure acdd-core transport/auth policy DTOs, implement unified HTTP security middleware and composable ServeHooks in src/observability/auth.rs and http.rs, and pre-register AC-009 seam test modules
subtasks:
- subtask_ref: adr-0013-transport-security
  title: Author docs/adr/0013-transport-security-auth-and-ipc-ingress.md and register it in docs/adr/README.md covering Host/Origin, scoped Bearer tokens, HMAC-SHA256 replay bounds, Unix socket UID auth, and Streamable HTTP worker identity
- subtask_ref: core-auth-policy-and-middleware
  title: Implement acdd-core auth DTOs and src/observability/auth.rs middleware (Host/Origin checks, constant-time scoped Bearer verification, HMAC-SHA256 + timestamp replay window, byte caps, rate limiting) plus composable ServeHooks in http.rs
- subtask_ref: seam-test-module-registration
  title: Register transport_security, ingress_transport, interactive_webui, and http_mcp_and_up test modules in tests/acdd/mod.rs
contract_revision: 1
tags:
- default
depends_on: []
scope:
- docs/adr/0013-transport-security-auth-and-ipc-ingress.md
- docs/adr/README.md
- crates/acdd-core/src/lib.rs
- crates/acdd-core/src/transport.rs
- src/observability/mod.rs
- src/observability/http.rs
- src/observability/auth.rs
- tests/acdd.rs
- tests/acdd/mod.rs
- tests/acdd/transport_security.rs
- tests/acdd/ingress_transport.rs
- tests/acdd/interactive_webui.rs
- tests/acdd/http_mcp_and_up.rs
```

### task: unix-socket-and-webhook-incoming

```yaml
task_ref: unix-socket-and-webhook-incoming
target: Implement confined .acdd/acdd.sock (0600, OS-UID auth) IPC ingress and authenticated --incoming POST /events webhook listener feeding AC-008 inbox
subtasks:
- subtask_ref: confined-unix-socket-ingress
  title: Create .acdd/acdd.sock with 0600 mode, verify peer OS UID credentials, and enqueue validated payloads into AC-008 inbox
- subtask_ref: authenticated-webhook-incoming
  title: Expose --incoming POST /events handler enforcing scoped ingest Bearer token or per-source HMAC-SHA256 X-Hub-Signature-256 + replay bounds before writing to inbox
contract_revision: 1
tags:
- default
depends_on:
- security-adr-and-auth-middleware
scope:
- src/observability/unix_socket.rs
- src/observability/incoming.rs
- tests/acdd/ingress_transport.rs
```

### task: interactive-webui-controls

```yaml
task_ref: interactive-webui-controls
target: Implement opt-in --interactive authenticated Web UI mutation routes for human Approve/Reject of manual/escalated gates and @mention blackboard steering
subtasks:
- subtask_ref: authenticated-approval-and-steering-routes
  title: Expose POST approve/reject and blackboard @mention routes gated on --interactive, Host/Origin checks, and approve Bearer scope without altering default read-only acdd serve
- subtask_ref: interactive-board-controls-ui
  title: Enhance src/observability/page.html with capability-gated Approve/Reject controls and @mention thread reply UI when --interactive is active
contract_revision: 1
tags:
- default
depends_on:
- security-adr-and-auth-middleware
scope:
- src/observability/interactive.rs
- src/observability/page.html
- tests/acdd/interactive_webui.rs
```

### task: streamable-http-mcp-and-acdd-up

```yaml
task_ref: streamable-http-mcp-and-acdd-up
target: Implement Streamable HTTP MCP (acdd mcp --http) with Mcp-Session-Id and token-bound worker_id identity, wire acdd up supervisor with sleep_until(next_timer), and co-evolve ADR-0003/ADR-0006, docs, runbooks, skills, examples, ROADMAP.md, and CHANGELOG.md
subtasks:
- subtask_ref: streamable-http-mcp-transport
  title: Serve POST/GET /mcp Streamable HTTP MCP transport sharing one warm index and state store with Mcp-Session-Id and worker:<role> token-bound worker_id enforcement
- subtask_ref: acdd-up-supervisor-and-serve-flags
  title: Wire --interactive and --incoming into src/cli/serve.rs, --http into src/cli/mcp.rs, and add acdd up launching configured webui, telemetry, socket, incoming, mcp_http, and sleep_until(next_timer) loop
- subtask_ref: transport-docs-skills-and-examples
  title: Update ADR-0003, ADR-0006, architecture/reference/runbook docs, executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, adapters.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate
contract_revision: 1
tags:
- docs
- default
depends_on:
- security-adr-and-auth-middleware
scope:
- src/mcp/http_transport.rs
- src/mcp/mod.rs
- src/cli/serve.rs
- src/cli/mcp.rs
- src/cli/up.rs
- src/cli/mod.rs
- tests/acdd/http_mcp_and_up.rs
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
- docs/runbooks/always-on.md
- docs/runbooks/README.md
- docs/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/
- ROADMAP.md
- CHANGELOG.md
```
