---
id: AC-009-authenticated-ingress-interactive-webui-and-http-mcp
title: "Authenticated network & IPC ingress, multi-project folder registry, interactive Web UI, Streamable HTTP MCP, and acdd up supervisor"
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
  - .acdd/workflows/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md
invariants:
  - 'INV-RESHAPE-BEFORE-EXECUTION: MANDATORY PRE-KICKOFF RESHAPE — Before claiming or executing any task in AC-009, the architect MUST run a full contract-planning reshape against the live post-AR-006/AC-006/AC-005/AC-007/AC-008 codebase (re-evaluating scope split, security boundaries, multi-project hub vs single-project defaults, and workflow services: supervision) and obtain operator sign-off before implementation.'
  - 'INV-SUPERVISED-WORKFLOW-SERVICES: Workflow-declared long-lived companion processes (services: with command, ready_check, env_allowlist, and scoped .acdd/acdd.sock/HTTP API tokens, bound to gates/groups via services: [...]) stay strictly separated from AI agents:, run under the acdd up supervisor for the enclosing gate/group lifetime, and shut down cleanly via SIGTERM -> timeout -> SIGKILL when the container closes.'
  - 'INV-SECURITY-ADR-FIRST: ADR-0013 (Transport Security, Authentication, and IPC Ingress) is authored and reconciled in Wave 0 before any stateful HTTP or IPC ingress route is implemented.'
  - 'INV-EXPLICIT-OPT-IN-MUTATIONS: Default acdd serve remains strictly read-only on 127.0.0.1; stateful capabilities (acdd serve --interactive, acdd serve --incoming, acdd mcp --http, .acdd/acdd.sock) require explicit opt-in flags or workspace config and fail closed without valid authentication.'
  - 'INV-HOST-ORIGIN-AND-TOKEN-SCOPING: All HTTP mutation and MCP endpoints enforce strict Host and Origin validation (DNS-rebinding and browser CSRF protection on 127.0.0.1), request body byte caps, rate limiting, and constant-time scoped Bearer token verification (read, approve, worker:<role>, ingest with optional projects: scoping); --incoming also verifies per-source HMAC-SHA256 X-Hub-Signature-256 signatures and timestamp replay windows.'
  - 'INV-UNIX-SOCKET-UID-CONFINEMENT: Local IPC socket .acdd/acdd.sock is created strictly with 0600 permissions inside the confined .acdd directory and validates peer OS UID credentials before enqueuing triggers into AC-008 inbox.'
  - 'INV-WORKER-IDENTITY-BINDING: Streamable HTTP MCP (POST/GET /mcp and /p/{project_id}/mcp) enforces per-session Mcp-Session-Id binding against the authenticated Bearer token scope (worker:<role>) and allowed project_id scope, rejecting worker_id or cross-project impersonation on acdd_claim, acdd_submit, and acdd_blackboard.'
  - 'INV-PROJECT-REGISTRY-CONFINEMENT: A project is an explicitly registered local workspace folder containing acdd.yaml or .acdd/ in WorkspaceRegistry (~/.acdd/workspaces.yaml, --projects-file, or repeatable --project [<id>=]<dir>, defaulting to single-project cwd when omitted); HTTP and Streamable HTTP MCP endpoints resolve project_id strictly against registered keys (never arbitrary URL filesystem paths) and isolate state.sqlite, code-map.sqlite, acdd.sock, WorkspacePathPolicy, and token projects: scopes per folder.'
  - 'INV-CORE-LEAF: Keep pure transport security policy, workspace registry schema, and auth DTOs (TokenScope, WebhookSourcePolicy, IngressAuthError, ServeCapabilities, ProjectEntry) in acdd-core free of network, SQLite, or filesystem I/O.'
  - 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-009 ships in the same milestone with ADR-0013, updated ADR-0003 and ADR-0006, architecture/reference/runbook docs, executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, skills/acdd/references/adapters.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
  - 'AC-1: GIVEN ADR-0013 authored and unified HTTP security middleware mounted on stateful/MCP routes, WHEN a request arrives with forged Host/Origin, missing/wrong Bearer token or project scope, invalid HMAC-SHA256 signature, expired timestamp replay window, oversized body, or excess rate, THEN middleware fails closed with 401/403/413/429 before any state handler executes.'
  - 'AC-2: GIVEN .acdd/acdd.sock (mode 0600 inside confined .acdd) or acdd serve --incoming (POST /events or POST /p/{project_id}/events), WHEN a local peer with mismatched OS UID connects to the socket or an unauthenticated/replayed webhook hits --incoming, THEN ingress rejects fail-closed; valid triggers enqueue deduplicated payloads into the target folder AC-008 inbox.'
  - 'AC-3: GIVEN acdd serve --interactive with valid Host/Origin and approve Bearer scope, WHEN an operator submits Approve/Reject for a parked AgentDef.mode: manual / escalated gate or posts @mention blackboard steering, THEN the state transition executes atomically; WHEN --interactive is omitted, THEN mutation routes return 405 and default acdd serve stays strictly read-only.'
  - 'AC-4: GIVEN acdd mcp --http (POST/GET /mcp and /p/{project_id}/mcp), acdd up, and workflow-declared services: bound to a gate or group (services: [app_server, telegram_bridge]), WHEN a worker attempts acdd_claim/acdd_submit/acdd_blackboard with a worker_id or project_id outside its authenticated Bearer token scope, THEN the transport rejects impersonation with 403; acdd up composes webui, telemetry, socket, incoming, mcp_http, sleep_until(next_timer), and supervised services: (passing ready_check, env_allowlist scrubbing, scoped socket/HTTP access, and graceful SIGTERM teardown on container close).'
  - 'AC-5: GIVEN the completed AC-009 tree, WHEN verified, THEN ADR-0013, ADR-0003, ADR-0006, docs/architecture/, docs/reference/{workflows,tasks,cli,mcp-tools,mcp-setup,configuration}.md, docs/runbooks/, .acdd/workflows/*.yaml, skills/acdd/references/acdd-workflow-examples/*.yaml, skills/acdd/references/adapters.md, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md pass acdd docs check and acdd validate with zero errors.'
  - 'AC-6: GIVEN a multi-project WorkspaceRegistry configured via acdd workspace add|remove|list, ~/.acdd/workspaces.yaml, --projects-file, or repeatable --project [<id>=]<dir>, WHEN GET /api/projects and /p/{project_id}/api/* are queried, THEN portfolio summaries and per-project Live Gate Board, Architecture Map, Specs/Roadmap, Docs, Diff, and SSE routes serve isolated folder runtimes; any unknown project_id, non-workspace directory, or URL path-traversal attempt fails closed with 403/404.'
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
  - agent-instructions
---

# AC-009: Authenticated network & IPC ingress, multi-project folder registry, interactive Web UI, Streamable HTTP MCP, and `acdd up` supervisor

> [!IMPORTANT]
> **RESHAPE BEFORE EXECUTION (Mandatory Pre-Kickoff Gate):**
> Do **not** start implementing `AC-009` directly from this draft. Because `AC-009` sits after `AR-006`, `AC-006`, `AC-005`, `AC-007`, and `AC-008`, an architect MUST first reshape and re-scope this milestone against the live codebase (`contract-planning` skill: re-verify crate/module layout, split into smaller milestones if needed, finalize `ADR-0013`, multi-project `WorkspaceRegistry`, and workflow `services:` supervision contracts) and get explicit user sign-off before claiming Wave 0.

## Goal & Non-goals

**Goal.** Consolidate all stateful HTTP and local IPC transport, authentication, multi-project folder hub hosting, and unified supervision on top of [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md)'s read-only loopback server and [AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)'s zero-daemon reactive core, governed upfront by `ADR-0013` (Transport Security, Authentication, and IPC Ingress).

**Non-goals.**
- Relaxing default `acdd serve` read-only loopback guarantees when `--interactive` / `--incoming` are omitted.
- Arbitrary URL-driven filesystem browsing (`?root=/...`), cloud multi-tenant auth/OAuth SaaS layers, or external reverse-proxy/TLS termination re-implemented inside `acdd`.
- Breaking single-project `cwd` defaults when no `--project` / `--projects-file` registry is configured.

## Agreed contracts

1. **Unified Local & Network Security Layer (`ADR-0013`)**: Strict `Host`/`Origin` validation (DNS-rebinding & CSRF guard), constant-time scoped Bearer tokens (`read`, `approve`, `worker:<role>`, `ingest` with optional `projects: ["*"] | ["<id>", ...]`), per-source HMAC-SHA256 `X-Hub-Signature-256` with timestamp replay window, request body byte caps, and bounded rate limiting.
2. **Confined Local Unix Domain Socket (`.acdd/acdd.sock`, `0600`)**: Zero-port, OS-UID-authenticated local IPC ingress feeding `AC-008` `inbox` for Git hooks, watchers, and co-located scripts.
3. **Interactive Web Control (`acdd serve --interactive`)**: Opt-in authenticated routes for human `Approve / Reject` of parked `AgentDef.mode: manual` / `escalated` gates and `@mention` steering on `acdd_blackboard`.
4. **External Webhook Listener (`acdd serve --incoming`)**: Authenticated `POST /events` (and `/p/{project_id}/events`) ingress feeding `AC-008`'s deduplicated/coalesced `inbox` (standalone headless or combined with `--webui`).
5. **Streamable HTTP MCP (`acdd mcp --http`)**: Standard `POST/GET /mcp` and `/p/{project_id}/mcp` sharing warm per-project code-map indexes and state stores while binding `Mcp-Session-Id` to token-authorized `worker_id` and `project_id`.
6. **Multi-Project Folder Hub (`WorkspaceRegistry` & Portfolio Web UI)**: Each project is a validated local folder (`acdd.yaml` or `.acdd/`) registered via `acdd workspace`, `~/.acdd/workspaces.yaml`, `--projects-file`, or `--project [<id>=]<dir>` (defaulting to `cwd`). Exposes `GET /api/projects` portfolio summaries, cross-folder pending approval queues, `/p/{project_id}/*` scoped views, and isolated per-folder state/index/socket/token boundaries.
7. **Unified Supervisor Alias (`acdd up`) & Workflow `services:`**: Single entrypoint launching configured `webui`, `telemetry`, `socket`, `incoming`, `mcp_http`, an interruptible `sleep_until(next_timer)` loop across registered projects, and workflow-declared companion `services:` (such as Telegram/Slack bridges or local dev/API servers attached via `services: [...]` on a gate/group — kept strictly separate from `agents:`, verified by `ready_check`, scrubbed via `env_allowlist`, given scoped `.acdd/acdd.sock`/HTTP API access, and torn down cleanly via `SIGTERM` on container close).

## Consumer & execution path map

| Surface | Canonical owner | Live consumers updated | Fail-closed rejection | Deferred destination |
|---|---|---|---|---|
| `ADR-0013` & HTTP security middleware | `crates/acdd-core/src/{transport.rs,config/projects.rs}` + `src/observability/auth.rs` | `src/observability/http.rs`, `ServeHooks` | Bad `Host`/`Origin` (`403`), missing/wrong Bearer (`401`/`403`), bad HMAC/replay (`401`), oversize body (`413`), rate limit (`429`) | — |
| Unix socket & webhook `--incoming` | `src/observability/{unix_socket.rs,incoming.rs}` | `AC-008` `inbox` writer, `src/cli/serve.rs` | Mismatched OS UID, non-`0600` or symlink `.acdd` dir, invalid HMAC/Bearer | — |
| Interactive Web UI `--interactive` | `src/observability/interactive.rs` + `page.html` | `AC-008` `approve`/`reject`, `acdd_blackboard` | Disabled when `--interactive` omitted (`405`); missing `approve` token or project mismatch (`403`) | — |
| Multi-project `WorkspaceRegistry` & Portfolio | `crates/acdd-core/src/config/projects.rs` + `src/observability/registry.rs` | `src/observability/{http.rs,page.html}`, `src/cli/workspace.rs` | Unregistered `project_id` (`404`), symlink/non-workspace folder at registration (`ACDD_WORKFLOW_SCHEMA_INVALID`), cross-project token (`403`) | — |
| Streamable HTTP MCP & `acdd up` | `src/mcp/http_transport.rs` + `src/cli/{args,serve,mcp,up,workspace,mod}.rs` | `src/mcp/mod.rs`, `docs/`, `skills/acdd/` | `worker_id` or `project_id` impersonation (`403`); missing `Mcp-Session-Id` (`400`/`401`) | — |

## Operational bounds & failure recovery

- **Zero symlink/path traversal in `WorkspaceRegistry`**: Folder roots are canonicalized at registration/startup, verified to contain `acdd.yaml` or `.acdd/`, and assigned unique validated `project_id` slugs; HTTP/MCP handlers never pass raw path strings into filesystem APIs.
- **Bounded Prometheus cardinality across projects**: `{workspace="<project_id>"}` remains bounded by the static `WorkspaceRegistry` size (unknown URL `project_id` values never create metric label series).
- **Clean shutdown & socket hygiene**: `acdd up` and `.acdd/acdd.sock` remove stale sockets strictly after verifying non-active ownership inside confined `.acdd/` and exit cleanly on `SIGINT`/`SIGTERM`.

## Tasks in this milestone

### task: security-adr-and-auth-middleware

```yaml
task_ref: security-adr-and-auth-middleware
target: "Author upfront ADR-0013, define pure acdd-core transport/auth policy and WorkspaceRegistry DTOs, implement unified HTTP security middleware and composable ServeHooks in src/observability/auth.rs and http.rs, and pre-register AC-009 seam test modules"
subtasks:
- subtask_ref: adr-0013-transport-security
  title: "Author docs/adr/0013-transport-security-auth-and-ipc-ingress.md and register it in docs/adr/README.md covering Host/Origin, scoped Bearer tokens, project scoping, HMAC-SHA256 replay bounds, Unix socket UID auth, and Streamable HTTP worker identity"
- subtask_ref: core-auth-policy-and-middleware
  title: "Implement acdd-core auth and WorkspaceRegistry DTOs (transport.rs, config/projects.rs) and src/observability/auth.rs middleware (Host/Origin checks, constant-time scoped Bearer verification, HMAC-SHA256 + timestamp replay window, byte caps, rate limiting) plus composable ServeHooks in http.rs"
- subtask_ref: seam-test-module-registration
  title: "Register transport_security, ingress_transport, interactive_webui, multi_project, and http_mcp_and_up test modules in tests/acdd/mod.rs"
contract_revision: 1
tags:
- default
depends_on: []
scope:
- docs/adr/0013-transport-security-auth-and-ipc-ingress.md
- docs/adr/README.md
- crates/acdd-core/src/lib.rs
- crates/acdd-core/src/transport.rs
- crates/acdd-core/src/config/mod.rs
- crates/acdd-core/src/config/projects.rs
- src/observability/mod.rs
- src/observability/http.rs
- src/observability/auth.rs
- tests/acdd.rs
- tests/acdd/mod.rs
- tests/acdd/transport_security.rs
- tests/acdd/ingress_transport.rs
- tests/acdd/interactive_webui.rs
- tests/acdd/multi_project.rs
- tests/acdd/http_mcp_and_up.rs
status: planned
```

- **Wave**: `0` | **Budget**: `~650` net LOC.
- **Seam & mutation proof (`AC-1`)**: `tests/acdd/transport_security.rs` proves DNS-rebinding `Host`/`Origin` spoofing returns `403`, wrong Bearer or out-of-scope project returns `401`/`403`, replayed HMAC timestamps fail closed, and oversized payloads return `413`.

### task: unix-socket-and-webhook-incoming

```yaml
task_ref: unix-socket-and-webhook-incoming
target: "Implement confined .acdd/acdd.sock (0600, OS-UID auth) IPC ingress and authenticated --incoming POST /events webhook listener feeding AC-008 inbox"
subtasks:
- subtask_ref: confined-unix-socket-ingress
  title: "Create .acdd/acdd.sock with 0600 mode, verify peer OS UID credentials, and enqueue validated payloads into AC-008 inbox"
- subtask_ref: authenticated-webhook-incoming
  title: "Expose --incoming POST /events and /p/{project_id}/events handlers enforcing scoped ingest Bearer token or per-source HMAC-SHA256 X-Hub-Signature-256 + replay bounds before writing to the target folder inbox"
contract_revision: 1
tags:
- default
depends_on:
- security-adr-and-auth-middleware
scope:
- src/observability/unix_socket.rs
- src/observability/incoming.rs
- tests/acdd/ingress_transport.rs
status: planned
```

- **Wave**: `1` (disjoint with `interactive-webui-controls`) | **Budget**: `~480` net LOC.
- **Seam & mutation proof (`AC-2`)**: `tests/acdd/ingress_transport.rs` proves `.acdd/acdd.sock` enforces `0600` permissions + peer UID check, valid webhooks enqueue deduplicated inbox rows, and tampered HMAC signatures fail with `401` leaving `inbox` untouched.

### task: interactive-webui-controls

```yaml
task_ref: interactive-webui-controls
target: "Implement opt-in --interactive authenticated Web UI mutation routes for human Approve/Reject of AgentDef.mode: manual / escalated gates and @mention blackboard steering"
subtasks:
- subtask_ref: authenticated-approval-and-steering-routes
  title: "Expose POST approve/reject and blackboard @mention routes gated on --interactive, Host/Origin checks, and approve Bearer scope without altering default read-only acdd serve"
- subtask_ref: interactive-board-controls-ui
  title: "Enhance src/observability/page.html with capability-gated Approve/Reject controls and @mention thread reply UI when --interactive is active"
contract_revision: 1
tags:
- default
depends_on:
- security-adr-and-auth-middleware
scope:
- src/observability/interactive.rs
- src/observability/page.html
- tests/acdd/interactive_webui.rs
status: planned
```

- **Wave**: `1` (disjoint with `unix-socket-and-webhook-incoming`) | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-3`)**: `tests/acdd/interactive_webui.rs` proves default `acdd serve` rejects approval `POST` with `405`, `--interactive` accepts valid `approve`-scoped tokens, and `read`-only tokens fail with `403`.

### task: multi-project-workspace-registry-and-portfolio

```yaml
task_ref: multi-project-workspace-registry-and-portfolio
target: "Implement validated multi-folder WorkspaceRegistry loading (~/.acdd/workspaces.yaml, --projects-file, repeatable --project [<id>=]<dir>), isolated per-project runtime routing (/api/projects, /p/{project_id}/*, per-project SSE & token scope checks), and Portfolio + Project Switcher Web UI"
subtasks:
- subtask_ref: validated-folder-registry-and-isolation
  title: "Validate workspace roots (acdd.yaml or .acdd/, symlink-canonicalized, unique project_id slugs), load WorkspaceRegistry with single-project cwd fallback, and enforce project-scoped Bearer/HMAC isolation across isolated state/code-map/policy handles"
- subtask_ref: portfolio-api-and-switcher-ui
  title: "Expose GET /api/projects portfolio summary (active runs, pending manual/escalated approvals, ready Wave 0 specs, map health), mount /p/{project_id}/api/* + multiplexed /events SSE in src/observability/http.rs, and add Portfolio + Project Switcher UI in src/observability/page.html"
contract_revision: 1
tags:
- default
depends_on:
- security-adr-and-auth-middleware
- interactive-webui-controls
scope:
- src/observability/registry.rs
- src/observability/http.rs
- src/observability/page.html
- tests/acdd/multi_project.rs
status: planned
```

- **Wave**: `2` | **Budget**: `~620` net LOC.
- **Seam & mutation proof (`AC-6`)**: `tests/acdd/multi_project.rs` proves two registered folders serve isolated runs/maps/roadmaps under `/p/{id}/api/*`, a token scoped to project A fails with `403` against project B, and `/p/..%2Fetc/api/docs` fails closed with `403`/`404`.

### task: streamable-http-mcp-and-acdd-up

```yaml
task_ref: streamable-http-mcp-and-acdd-up
target: "Implement Streamable HTTP MCP (acdd mcp --http) with Mcp-Session-Id, token-bound worker_id, and project_id isolation, wire acdd workspace + multi-project acdd serve/up supervisor with sleep_until(next_timer), and co-evolve ADR-0003/ADR-0006, docs, runbooks, skills, examples, ROADMAP.md, and CHANGELOG.md"
subtasks:
- subtask_ref: streamable-http-mcp-transport
  title: "Serve POST/GET /mcp and /p/{project_id}/mcp Streamable HTTP MCP transport sharing warm per-project indexes and state stores with Mcp-Session-Id and worker:<role> + project-scoped identity enforcement"
- subtask_ref: acdd-up-supervisor-and-serve-flags
  title: "Add acdd workspace add|remove|list CLI management (src/cli/args.rs, src/cli/workspace.rs), wire --interactive/--incoming/--project/--projects-file into src/cli/serve.rs, --http into src/cli/mcp.rs, and add acdd up launching configured webui, telemetry, socket, incoming, mcp_http, and multi-project sleep_until(next_timer) loop"
- subtask_ref: transport-docs-skills-and-examples
  title: "Update ADR-0003, ADR-0006, architecture/reference/runbook docs, executable .acdd/workflows/*.yaml and skills/acdd/references/acdd-workflow-examples/*.yaml workflows, skills/acdd/SKILL.md, adapters.md, ROADMAP.md, and CHANGELOG.md; pass acdd docs check and acdd validate"
contract_revision: 1
tags:
- docs
- default
depends_on:
- unix-socket-and-webhook-incoming
- multi-project-workspace-registry-and-portfolio
scope:
- src/mcp/http_transport.rs
- src/mcp/mod.rs
- src/cli/args.rs
- src/cli/serve.rs
- src/cli/mcp.rs
- src/cli/up.rs
- src/cli/workspace.rs
- src/cli/mod.rs
- tests/acdd/http_mcp_and_up.rs
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
- docs/runbooks/always-on.md
- docs/runbooks/README.md
- .acdd/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/
- ROADMAP.md
- CHANGELOG.md
status: planned
```

- **Wave**: `3` | **Budget**: `~850` net LOC (incl. docs/runbooks).
- **Seam & mutation proof (`AC-4`, `AC-5`)**: `tests/acdd/http_mcp_and_up.rs` proves a worker claiming with a spoofed `worker_id` or wrong `project_id` over `POST /mcp` fails with `403`, `acdd up` fires a scheduled timer via `sleep_until` without busy-polling, and `acdd docs check` + `acdd validate` pass cleanly.

## Negative boundaries

- No unauthenticated stateful HTTP/IPC routes and no weakening of default read-only `acdd serve`.
- No arbitrary URL path parameters opening unregistered filesystem directories.
- No cross-project leakage of `state.sqlite`, `code-map.sqlite`, `WorkspacePathPolicy`, HMAC secrets, or Bearer tokens.
- No busy-polling timer loops in `acdd up`, no `acdd-index` imports inside `crates/acdd-state`, and no I/O inside `crates/acdd-core`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-1`..`AC-6` map 1:1 across Waves 0–3 to 5 dedicated `tests/acdd/` seam suites plus upfront `ADR-0013` and co-evolved docs/runbooks.
- **Correctness**: Every security boundary (`Host`/`Origin`, Bearer scope, project scope, HMAC replay window, `0600` socket + OS UID, `Mcp-Session-Id` identity, registry confinement) specifies exact fail-closed HTTP/IPC rejections and mutation tests.
- **Coherence**: Reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), [AC-007](AC-007-unified-observability-board-otlp-and-metrics.md), and [AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md); uses unified `AgentDef` naming and keeps Wave 1 concurrent tasks (`unix-socket-and-webhook-incoming` vs. `interactive-webui-controls`) strictly disjoint.
