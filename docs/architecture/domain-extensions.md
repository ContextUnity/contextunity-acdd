---
title: "Domain Extensions — DomainProvider Shim-Crates"
doc_type: architecture
---

# Domain Extensions — `DomainProvider` Shim-Crates

ContextUnity ACDD separates generic lifecycle orchestration (`crates/acdd-state`) from domain-specific verification, context retrieval, and automation via the [`DomainProvider`](../../crates/acdd-core/src/ports.rs) trait in `acdd-core`. New verification domains connect as thin **shim-crates** (`crates/acdd-domain-<name>`) wrapping existing Rust ecosystem libraries without touching `acdd-state` or `acdd-index`.

## 1. Thin Shim-Crate Architecture

A domain extension crate sits alongside `acdd-state` and `acdd-index` in the workspace DAG:

- **Allowed dependencies**: `acdd-core` (for `ports`, `errors`, `ids`, `schema`) plus the wrapped Rust ecosystem library (for example an OpenAPI validator, SQL parser/linter, or JSON Schema engine), `serde_json`, and `anyhow`.
- **Forbidden dependencies**: Never depend on `acdd-state`, `acdd-index`, `rusqlite`, `tree-sitter`, `rmcp`, or `clap`.
- **Single host integration site**: Instantiate and register the provider inside [`build_domain_providers`](../../src/state_bridge/mod.rs), controlled by `modules.domains.<name>` in `acdd.yaml`.

```text
crates/acdd-domain-<name>  ──► crates/acdd-core (DomainProvider, DomainCapabilities, SchemaDef)
        │
        ▼ (registered once)
src/state_bridge/mod.rs (build_domain_providers -> CompositeDomainProvider)
        │
        ├──► crates/acdd-state (calls checks:, context:, call: via CompositeDomainProvider)
        └──► src/mcp & src/cli (exposes <domain>_<name> plugin tools & `acdd tool`)
```

## 2. Implementing `DomainProvider`

Every domain implements [`DomainProvider`](../../crates/acdd-core/src/ports.rs) (`Send + Sync`):

1. **`domain_id(&self) -> &str`**: Return a unique lowercase alphanumeric identifier matching `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$` (for example `"openapi"` or `"sql_lint"`).
2. **`capabilities(&self) -> DomainCapabilities`**: Declare every supported capability upfront so `CompositeDomainProvider` and `acdd validate` validate profile references fail-closed:
   - `actions: Vec<ActionCapability>` (`{ name, input_schema, mutating, supports_result_lookup }`)
   - `checks: Vec<CheckCapability>` (`{ name, input_schema }`)
   - `context_lenses: BTreeSet<String>` (unqualified lens names exposed as `<domain>.<lens>`)
   - `tools: Vec<DomainToolDef>` (`{ name, description, input_schema, read_only }`, exposed globally as `<domain>_<name>`)
3. **`run_check(&self, check: &str, params: &Value, req: &DomainCheckReq) -> Result<DomainCheckOutcome>`**:
   - Inspect live files under `req.worktree` confined to `req.admitted_scope` and `req.changed_paths` (and optional `req.test_target`).
   - Return strictly `DomainCheckOutcome::Passed { summary, execution_digest_salt }` or `DomainCheckOutcome::Failed { summary, diagnostics }`. There is no `NotApplicable` fallback. Fold live file or index digests into `execution_digest_salt` when check freshness must bind to external inputs.
4. **`provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value>`**:
   - Materialize a JSON payload bounded by `req.max_bytes` for Tier 6 `ClaimResp.context_lenses` injection (using `req.worktree`, `req.admitted_scope`, and optional `req.target`).
5. **`execute_action(&self, action: &str, params: &Value, idempotency_key: &str, req: &DomainActionReq) -> Result<DomainActionResult>`** (plus optional **`lookup_action_result`**):
   - Execute `call: <domain>.<action>` in-process and return `{ summary, output }`. Mutating actions with `supports_result_lookup: true` override `lookup_action_result` to recover uncertain outcomes after a crash.
6. **`call_tool(&self, tool: &str, args: &Value, req: &DomainToolReq) -> Result<DomainToolOutput>`**:
   - Serve optional `<domain>_<name>` plugin tools, returning `DomainToolOutput::Markdown(String)` or `DomainToolOutput::Json(Value)`.

### Minimal Skeleton (`crates/acdd-domain-schema/src/lib.rs`)

```rust
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ports::{
    CheckCapability, DomainActionReq, DomainActionResult, DomainCapabilities,
    DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
    DomainToolDef, DomainToolOutput, DomainToolReq,
};
use acdd_core::schema::SchemaDef;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub struct SchemaDomainProvider;

impl DomainProvider for SchemaDomainProvider {
    fn domain_id(&self) -> &str {
        "schema"
    }

    fn capabilities(&self) -> DomainCapabilities {
        DomainCapabilities {
            actions: vec![],
            checks: vec![CheckCapability {
                name: "lint".to_owned(),
                input_schema: SchemaDef::object(),
            }],
            context_lenses: BTreeSet::from(["contracts".to_owned()]),
            tools: vec![DomainToolDef {
                name: "diff".to_owned(),
                description: "Compare JSON Schema files against target base".to_owned(),
                input_schema: SchemaDef::object(),
                read_only: true,
            }],
        }
    }

    fn execute_action(
        &self,
        action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> Result<DomainActionResult> {
        Err(StateError::new(
            ErrorCode::DomainCapabilityUnavailable,
            format!("unsupported action 'schema.{action}'"),
        )
        .into())
    }

    fn run_check(
        &self,
        check: &str,
        _params: &Value,
        req: &DomainCheckReq,
    ) -> Result<DomainCheckOutcome> {
        match check {
            "lint" => Ok(DomainCheckOutcome::Passed {
                summary: format!("schema.lint verified {} changed path(s)", req.changed_paths.len()),
                execution_digest_salt: None,
            }),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unsupported check 'schema.{check}'"),
            )
            .into()),
        }
    }

    fn provide_context(&self, lens: &str, req: &DomainContextReq) -> Result<Value> {
        match lens {
            "contracts" => Ok(json!({ "scope": req.admitted_scope })),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unsupported context lens 'schema.{lens}'"),
            )
            .into()),
        }
    }

    fn call_tool(
        &self,
        tool: &str,
        _args: &Value,
        _req: &DomainToolReq,
    ) -> Result<DomainToolOutput> {
        match tool {
            "diff" => Ok(DomainToolOutput::Markdown("No schema breaking changes.".to_owned())),
            _ => Err(StateError::new(
                ErrorCode::DomainCapabilityUnavailable,
                format!("unsupported tool 'schema_{tool}'"),
            )
            .into()),
        }
    }
}
```

## 3. Connecting Capabilities in `.acdd/profile.yaml` (`version: 2`)

Reference qualified `<domain>.<capability>` identifiers inside `.acdd/profile.yaml` (see [ACDD Profiles Reference](../reference/profiles.md)):

```yaml
version: 2
inherit: code
gates_merge: patch

gates:
  - id: tasks.contract
    context:
      - docs.adrs
      - schema.contracts

  - id: tasks.build
    checks:
      - code.seam
      - schema.lint
    tools_add:
      - schema_diff

  - id: schema_publish
    proof: call
    call: schema.publish
    with:
      strict: true
```

- **`checks:`**: Executed automatically during `acdd verify` / `acdd_verify` and `acdd submit` / `acdd_submit`. Any `execution_digest_salt` folds into the claim-bound `execution_digest`, bound to the active `TreeOid`.
- **`context:`**: Materialized into Tier 6 `ClaimResp.context_lenses` in YAML declaration order during `acdd claim` / `acdd_claim`, trimmed cleanly if the `ResponsePolicy.max_output_bytes` budget is tight.
- **`call:` + `with:`**: Runs an in-process action gate (`proof: call`), validating `with:` against `ActionCapability.input_schema` and recording `DomainActionResult` in `gate_artifacts`.
- **`tools_add:` / `default_tools:`**: Admits `<domain>_<name>` plugin tools on specific gates. `CompositeDomainProvider` rejects any `<domain>_<name>` colliding with the 19 built-in tools in [`BUILTIN_MCP_TOOLS`](../../crates/acdd-core/src/ports.rs) fail-closed (`ProfileSchemaInvalid`).

## 4. Wiring, Module Toggles, and Fail-Closed Validation

1. **Single-site registration (`src/state_bridge/mod.rs`)**:
   Add `Arc::new(SchemaDomainProvider)` to the provider vector in [`build_domain_providers`](../../src/state_bridge/mod.rs), passing `config.modules.domains` (merged with `code` and `docs` flags) to `CompositeDomainProvider::with_module_flags`.
2. **Workspace toggle (`acdd.yaml`)**:
   External domain providers default to disabled (`false`) in [`SubsystemRegistry::is_domain_enabled`](../../crates/acdd-core/src/subsystems.rs) unless explicitly enabled in `acdd.yaml` (or passed via `with_module_flags`):
   ```yaml
   modules:
     code: true
     docs: true
     state: true
     blackboard: true
     domains:
       schema: true
   ```
   When `modules.domains.schema: false` (or omitted when disabled), any profile gate referencing `schema.*` or `schema_diff` fails closed with `DOMAIN_CAPABILITY_UNAVAILABLE`.
3. **Fail-closed verification (`acdd validate`)**:
   Run `acdd validate` ([`src/cli/validate.rs`](../../src/cli/validate.rs)). Validation constructs `CompositeDomainProvider` via `build_domain_providers`, parses `.acdd/profile.yaml`, and checks every `call:`, `checks:`, `context:`, and `tools` / `tools_add` entry against enabled domain capabilities and `SchemaDef` schemas before any run can start.
