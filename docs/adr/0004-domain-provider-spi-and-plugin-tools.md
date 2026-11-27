---
title: "ADR 0004: DomainProvider SPI and Plugin Tools"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0004: `DomainProvider` SPI and Plugin Tools

## Status

Accepted.

## Context

Hardcoding AST seam checks, documentation link checks, or language-specific heuristics directly inside `crates/acdd-state` couples the state machine to `crates/acdd-index` and forces engine edits whenever a new verification domain (OpenAPI diffs, SQL migration lints, policy checks) is added. The state engine needs a narrow, fail-closed Service Provider Interface (SPI) in `acdd-core` that supports gate checks, context injection, in-process actions, and optional MCP/CLI plugin tools.

## Decision

### 1. `DomainProvider` Trait (`crates/acdd-core/src/ports.rs`)

[`DomainProvider`](../../crates/acdd-core/src/ports.rs) (`Send + Sync`) defines the canonical domain boundary:

- `domain_id(&self) -> &str`
- `capabilities(&self) -> DomainCapabilities` (`actions: Vec<ActionCapability>`, `checks: Vec<CheckCapability>`, `context_lenses: BTreeSet<String>`, `tools: Vec<DomainToolDef>`)
- `execute_action(&self, action: &str, params: &Value, idempotency_key: &str, req: &DomainActionReq) -> Result<DomainActionResult>` (with optional `lookup_action_result` for crash recovery when `supports_result_lookup: true`)
- `run_check(&self, check: &str, params: &Value, req: &DomainCheckReq) -> Result<DomainCheckOutcome>`
- `provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value>`
- `call_tool(&self, tool: &str, args: &Value, req: &DomainToolReq) -> Result<DomainToolOutput>` (`Markdown(String)` | `Json(Value)`)

### 2. Fail-Closed Capability Dispatch (`CompositeDomainProvider`)

- [`CompositeDomainProvider`](../../crates/acdd-core/src/ports.rs) registers all providers at startup (honoring `modules.domains.<id>` flags via `with_module_flags`).
- Registration fails closed (`WorkflowSchemaInvalid`) on invalid component names, duplicate `domain_id`, duplicate `<domain>_<name>` plugin tool names, invalid `SchemaDef` capability schemas, or any collision with the **18 built-in static MCP tools** (`BUILTIN_MCP_TOOLS`) or reserved retired names (`RETIRED_MCP_TOOLS`).
- **`run_check` has zero `NotApplicable` fallback**: `DomainCheckOutcome` admits strictly `Passed { summary, execution_digest_salt }` or `Failed { summary, diagnostics }`. Optional `execution_digest_salt` (such as code-map generation + live file digests) binds check freshness into `execution_digest`.
- Referencing an unregistered or disabled `call:`, `checks:`, `context:`, or `<domain>_<name>` tool fails closed with `ErrorCode::DomainCapabilityUnavailable`.

### 3. Host Providers and Thin Shim-Crate Extensions

- **Host providers ([`src/state_bridge/mod.rs`](../../src/state_bridge/mod.rs))**: `build_domain_providers` registers `CodeDomainProvider` (`domain_id = "code"`: `code.seam`, `code.lint`, `code.signature_scope`, `code.symbols`, `code.tests`, `code.external_callers`) and `DocsDomainProvider` (`domain_id = "docs"`: `docs.links`, `docs.instructions`, `docs.adrs`, `docs.invariants`), both with `tools: []`.
  - Replacing legacy default-blocking signature-scope heuristics, `code.external_callers` surfaces modified-signature callers non-blockingly via `ClaimResp.context_lenses`, while `code.signature_scope` remains available as an opt-in `checks:` rule.
- **External thin shim-crates (`crates/acdd-domain-<name>`)**: New verification domains live in thin shim-crates depending solely on `acdd-core` plus an existing Rust ecosystem library (never `acdd-state` or `acdd-index`), wired once in `src/state_bridge/mod.rs` and gated by `modules.domains.<name>` in `acdd.yaml`. See the practical [domain extensions guide](../architecture/domain-extensions.md).

## Consequences

- `acdd-state` remains 100% domain-agnostic with zero dependency on `acdd-index` or external domain libraries.
- The 18 static MCP tools retain compile-time `#[tool]` schemas while `<domain>_<name>` plugin tools scale cleanly through `acdd tool <domain>_<name>` and MCP `list_tools` / `call_tool`.
- Every workflow reference to `call:`, `checks:`, `context:`, and plugin tools is verified fail-closed by `acdd validate`.
