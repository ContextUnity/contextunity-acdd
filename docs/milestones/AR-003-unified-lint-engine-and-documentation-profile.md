---
id: AR-003-unified-lint-engine-and-documentation-profile
title: "Language-agnostic architectural & graph linter (acdd lint / code_map_lint), executable INV-* rules, embedded documentation profile, and AGENTS.md invariant consolidation"
doc_type: contract
status: planned
depends_on:
  - AR-002-modularize-db-reader-and-traversal
guidance:
  hitl: pro
owners:
  - AGENTS.md
  - src/AGENTS.md
  - crates/acdd-core/AGENTS.md
  - crates/acdd-core/src/
  - crates/acdd-index/AGENTS.md
  - crates/acdd-index/src/docs.rs
  - crates/acdd-index/src/db/
  - crates/acdd-state/AGENTS.md
  - crates/acdd-state/src/profile/
  - crates/acdd-state/tests/
  - src/cli/
  - src/mcp/
  - src/format/
  - src/state_bridge/
  - tests/AGENTS.md
  - tests/core.rs
  - tests/core/
  - tests/mcp.rs
  - tests/mcp/
  - tests/acdd.rs
  - tests/acdd/
  - .acdd/profile.yaml
  - docs/AGENTS.md
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - skills/acdd/
  - README.md
  - INSTALL.md
  - ROADMAP.md
  - docs/milestones/AR-003-unified-lint-engine-and-documentation-profile.md
invariants:
  - 'INV-AR003-CORE-LEAF-LINT-TYPES: Pure lint categories, severities, architectural rule kinds, zone selectors, and acdd.yaml architecture schemas reside in leaf crates/acdd-core reusing WorkspacePathPolicy glob matching with zero dependencies on acdd-index, acdd-state, rusqlite, tree-sitter, clap, rmcp, or tokio.'
  - 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Executable acdd-rule blocks live directly inside indexed Markdown sections alongside INV-* invariants and persist in Node.details["arch_rules"] (with schema/zone errors recorded fail-closed in errors), preserving deterministic cold/cache/delta parity under existing CommitmentDomain::Nodes and CommitmentDomain::Errors without adding shadow SQLite registries.'
  - 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ is the sole canonical owner of all indexed verification categories (arch, duplicates, dead_tails, cycles, syntax, docs), evaluating internal edges (respecting EdgeKind, EdgeConfidence, and is_test) and segment-normalized external dependencies in batched O(V + E) passes with zero N+1 SQLite, filesystem, or Git queries.'
  - 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint (CLI), code_map_lint (MCP), and CodeDomainProvider check code.lint share one execution and formatting contract, and code.lint observe_freshness binds the index generation/commitment digest plus live scoped workspace state so acdd submit rejects stale lint traces.'
  - 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: crates/acdd-state embeds documentation (embedded/documentation.yaml) alongside base, code, and audit as a compiler-free documentation and invariant governance profile gated on docs.links and code.lint (categories: [arch, docs]) with independent review.'
  - 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Architectural INV-* invariants and executable acdd-rule blocks reside exclusively in indexed canonical docs/architecture/*.md and docs/adr/*.md pages with verified symbol claims, while root and subtree AGENTS.md files act strictly as concise operational routers.'
acceptance_criteria:
  - 'AC-1: crates/acdd-core defines closed LintCategory (arch, duplicates, dead_tails, cycles, syntax, docs), LintSeverity (error, warn), ArchRuleKind (forbid_dependency, allow_only_dependencies, single_owner, forbid_cycles, boundary_entry, must_implement, doc_integrity, ast_pattern), ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy, adds optional architecture.{zones,rules} to AcddConfig, and registers code_map_lint in BUILTIN_MCP_TOOLS (20 built-in tools).'
  - 'AC-2: crates/acdd-index/src/docs.rs extracts fenced acdd-rule YAML blocks into section Node.details["arch_rules"] and records malformed rule YAML, duplicate rule_id definitions, or unknown zone references fail-closed in errors with identical cold, cached, and delta Merkle roots.'
  - 'AC-3: crates/acdd-index/src/db/lint/ unifies arch, duplicates, dead_tails, cycles, syntax, and docs checks across all indexed languages and linked_workspaces, normalizes external dependency prefixes across ::, ., /, and : separators, attaches governing INV-* and source doc path:line citations to violations, and rewires code_map_query(duplicates | dead_tails) and code_map_analyze(lint=true | include_cycles=true) as thin delegates.'
  - 'AC-4: crates/acdd-state embeds documentation.yaml (embedded_profile("documentation")) with compiler-free draft/build, docs.links + code.lint verification, independent review, and delivery/handoff gates, and adds code_map_lint + code.lint checks to embedded code.yaml and audit.yaml default toolsets.'
  - 'AC-5: Host src/ exposes acdd lint CLI and code_map_lint MCP with dual-surface Markdown/JSON parity, registers CheckCapability("lint") (code.lint) with matching observe_freshness in CodeDomainProvider, marks rule-backed invariants as enforced in DocsDomainProvider (docs.invariants), and wires code.lint before/with tests in .acdd/profile.yaml.'
  - 'AC-6: Scattered architectural invariants in AGENTS.md, src/AGENTS.md, crates/acdd-{core,index,state}/AGENTS.md, and tests/AGENTS.md are consolidated into canonical docs/architecture/*.md and docs/adr/*.md with INV-* IDs, verified symbol links, and live acdd-rule definitions enforcing ACDD crate/layer/single-owner boundaries; reference docs and skills/acdd/ document acdd lint, code_map_lint, acdd-rule, and --profile documentation; and acdd lint, acdd docs check, and acdd validate pass with zero violations across the workspace.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
repositories: []
linked_to: []
---

# AR-003: Language-Agnostic Architectural & Graph Linter, Executable `INV-*` Rules, Embedded `documentation` Profile, and `AGENTS.md` Invariant Consolidation

## Goal and Admission Boundary

Unify architectural boundary enforcement, graph hygiene (`duplicates`, `dead_tails`, `cycles`, `syntax`), and documentation-to-code integrity (`docs`) into a single, language-agnostic verification engine over `.acdd/code-map.sqlite`, expose it uniformly as `acdd lint` (CLI), `code_map_lint` (MCP), and `code.lint` (`GateProfile` check), introduce the built-in `documentation` profile (`--profile documentation`) for authoring and refactoring living documentation without code-build overhead, and consolidate scattered `**/AGENTS.md` invariants into canonical indexed `docs/architecture/` and `docs/adr/` pages backed by live `acdd-rule` definitions.

[`AR-002`](archive/AR-002-modularize-db-reader-and-traversal.md) is the direct upstream prerequisite (decomposing `db/{reader,symbols,traversal}`, `src/{cli,mcp,state_bridge}`, and unifying `models::tags` and `NodeDetailKey`); [`AC-005`](AC-005-unified-swarm-gates-and-peer-board-coordination.md) sequences after `AR-003` so `documentation.yaml` and `code.lint` land cleanly before `AC-005` migrates all embedded profiles onto unified `mode: swarm` and `agents:`.

## Architectural Reconciliation and Verified Owners

The design reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0002](../adr/0002-workspace-confinement-and-path-policy.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md), [ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), [ADR 0007](../adr/0007-sqlite-code-map-concurrency-and-staged-publication.md), [ADR 0008](../adr/0008-merkle-commitments-and-compressed-fact-storage.md), and [ADR 0009](../adr/0009-incremental-delta-and-transitive-relinking.md) with zero new external Cargo dependencies and zero new SQLite tables:

| Responsibility | Canonical Owner | Architectural Contract |
| --- | --- | --- |
| Pure rule & lint DTOs, `architecture` config, built-in tool list | `crates/acdd-core/src/{lint.rs, config/, ports.rs}` | Leaf types (`LintCategory`, `LintSeverity`, `ArchRuleKind`, `ArchRuleSpec`, `ZoneSelector`) reuse `WorkspacePathPolicy` globs; [`BUILTIN_MCP_TOOLS`](../../crates/acdd-core/src/ports.rs) adds `"code_map_lint"`. |
| Executable `acdd-rule` extraction from Markdown | [`extract_with_details`](../../crates/acdd-index/src/docs.rs) | Parses fenced `acdd-rule` YAML blocks into section `Node.details["arch_rules"]` (sealed under `CommitmentDomain::Nodes`); invalid YAML/zones emit fail-closed rows into `errors` (`CommitmentDomain::Errors`). |
| Unified language-agnostic lint execution | `crates/acdd-index/src/db/lint/` (replacing [`lint.rs`](../../crates/acdd-index/src/db/lint.rs)) | Single owner for `arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, and `docs` checks over `nodes`, `edges`, `dependencies`, `doc_sections`, and `errors` in batched $O(V + E)$ passes. |
| Embedded `documentation` profile & `code` gate checks | [`embedded_profile`](../../crates/acdd-state/src/profile/inherit.rs) + `embedded/documentation.yaml` | Adds `"documentation"` as the 4th embedded profile (`base`, `code`, `audit`, `documentation`) gated on `docs.links` and `code.lint` without compiling code. |
| `code.lint` gate verifier, `[ENFORCED]` lens, CLI & MCP parity | [`CodeDomainProvider`](../../src/state_bridge/code_domain/mod.rs), [`DocsDomainProvider`](../../src/state_bridge/docs_domain.rs), `src/{cli,mcp,format}/` | `CodeDomainProvider` implements `code.lint` + `observe_freshness`; `DocsDomainProvider` flags rule-backed invariants in `docs.invariants`; `acdd lint` and `code_map_lint` share `src/format/` rendering. |

### Executable Invariant Format (`acdd-rule`) and Rule Primitives

Markdown documentation sections under `docs/**` (and root `AGENTS.md`) declare machine-enforced rules directly alongside `INV-*` callouts using fenced `acdd-rule` YAML blocks:

````markdown
> [!IMPORTANT]
> Invariant: `crates/acdd-core` is a strict leaf crate forbidding workspace sibling crates and heavy runtime dependencies.
> INV-CORE-LEAF: [`SubsystemRegistry`](../../crates/acdd-core/src/subsystems.rs)

```acdd-rule
id: INV-CORE-LEAF
invariant: INV-CORE-LEAF
severity: error
kind: forbid_dependency
from:
  paths: ["crates/acdd-core/src/**"]
to:
  paths: ["crates/acdd-index/**", "crates/acdd-state/**", "src/**"]
  external_symbols: ["rusqlite", "tree_sitter", "tokio", "rmcp", "clap", "rayon", "zstd", "hashbrown"]
edge_kinds: ["imports", "calls", "references"]
```
````

`ArchRuleKind` is a closed enum of 8 language-agnostic primitives evaluated strictly against normalized SQLite index facts:

1. **`forbid_dependency`**: Rejects internal `edges` (`src` in `from` $\to$ `dst` in `to`, defaulting to `is_test = 0` and `confidence IN ('exact', 'inferred')`) and external `dependencies` matching segment-normalized `external_symbols` prefixes (`::`, `.`, `/`, `:` boundaries across Rust, Python, TypeScript/JavaScript, Go, Java, SQL, etc.).
2. **`allow_only_dependencies`**: Enforces strict layer whitelists (`from` may depend only on `from` itself, `to.paths` / `to.zones`, and allowed `external_symbols`).
3. **`single_owner`**: Rejects internal `edges` to target symbols/paths or external `dependencies` on restricted APIs (e.g. `std::process::Command` / subprocess execution) originating outside declared `owner_paths`.
4. **`forbid_cycles`**: Projects internal `edges` onto declared `architecture.zones` or directory depth buckets and runs Tarjan SCC in memory to reject inter-zone/module cycles.
5. **`boundary_entry`**: Rejects cross-zone `edges` into `to` targeting internal symbols outside declared public entrypoint globs (`entrypoints`, e.g. `**/mod.rs`, `**/lib.rs`, `**/index.ts`, `**/__init__.py`).
6. **`must_implement`**: Verifies that production type declarations (`class`, `struct`) matching `from` (and optional name/decorator filters) carry `implements` / `inherits` edges to required contract selectors.
7. **`doc_integrity`**: Rejects broken explicit-path `symbol_claims` in `doc_sections`/`nodes`, orphan `INV-*` declarations lacking both verified `governs` edges and `arch_rules`, and duplicate `INV-*` / `rule_id` declarations.
8. **`ast_pattern`**: Optional structural escape hatch delegating to the existing Tree-sitter `ast` matcher confined to `from.paths` when a syntactic body rule cannot be expressed as a graph edge.

## Non-Goals and Negative Boundaries

- **Protected manifests off-limits**: Do **not** modify `Cargo.toml`, `crates/*/Cargo.toml`, or `build.rs`; implement all parsing and linting with existing workspace dependencies (`serde_yaml`, `serde_json`, `pulldown-cmark`, `rusqlite`).
- **Zero shadow SQLite tables or binary-fact breakage**: Do **not** add a new `SCHEMA_DDL` table or alter `TypedFacts` binary magic when `Node.details["arch_rules"]` and `errors` already provide incremental delta persistence and Merkle commitment sealing.
- **Zero duplicate glob or cycle engines**: Reuse `acdd_core::path_policy` (`WorkspacePathPolicy`, `glob_matches`, `expand_glob_braces`) and `crates/acdd-index/src/db/cycles.rs` / `traversal` helpers rather than writing parallel matchers or graph loaders.
- **Zero `N+1` queries**: Preload candidate rules, `path_dictionary`, and filtered edge/dependency projections in batched queries; never run per-rule or per-file SQLite queries in loops.
- **Preserve backward compatibility on existing MCP/CLI query ops**: Keep `code_map_query(operation="duplicates" | "dead_tails")` and `code_map_analyze(lint=true | include_cycles=true)` working as thin delegates to `db::lint`.

## Topological Wave Architecture

- **Wave 0 (`core-lint-contracts-and-seam-bootstrap`)**: Freezes pure `acdd_core::lint` contracts, `acdd.yaml` `architecture` schema, `"code_map_lint"` in `BUILTIN_MCP_TOOLS`, and pre-registers seam test modules in canonical root test binaries (`tests/{core,mcp,acdd}.rs`).
- **Wave 1 (Parallel, strictly disjoint scopes)**:
  - `index-executable-invariants-and-unified-lint-engine` (`crates/acdd-index/**` + `tests/core/arch_lint.rs`)
  - `embedded-documentation-profile-and-profile-validation` (`crates/acdd-state/**`)
- **Wave 2 (`host-cli-mcp-lint-surface-and-domain-provider`)**: Wires `acdd lint`, `code_map_lint`, `src/format/`, `CodeDomainProvider` (`code.lint` + `observe_freshness`), `DocsDomainProvider` (`enforced` invariants), and `.acdd/profile.yaml`.
- **Wave 3 (`agents-md-invariant-consolidation-and-docs-alignment`)**: Consolidates scattered `**/AGENTS.md` invariants into `docs/architecture/` & `docs/adr/` with live `acdd-rule` blocks, updates reference docs and `skills/acdd/` for `--profile documentation` and `acdd lint`, and proves zero-violation self-hosting across the repository.

## Tasks in this milestone

### task: core-lint-contracts-and-seam-bootstrap

```yaml
task_ref: core-lint-contracts-and-seam-bootstrap
target: "Define pure leaf lint/rule schemas and ZoneSelector in crates/acdd-core, extend AcddConfig with optional architecture.{zones,rules}, register code_map_lint in BUILTIN_MCP_TOOLS, and pre-register seam test modules in canonical root test binaries"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-core/src/lib.rs
  - crates/acdd-core/src/lint.rs
  - crates/acdd-core/src/ports.rs
  - crates/acdd-core/src/config.rs
  - crates/acdd-core/src/config/
  - tests/core.rs
  - tests/core/arch_lint.rs
  - tests/mcp.rs
  - tests/mcp/lint_surface.rs
  - tests/acdd.rs
  - tests/acdd/lint_gate_and_docs_profile.rs
invariants:
  - 'INV-AR003-CORE-LEAF-LINT-TYPES: crates/acdd-core/src/lint.rs owns LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy glob helpers with zero I/O or non-leaf dependencies.'
acceptance_criteria:
  - 'AC-1: crates/acdd-core/src/lint.rs defines closed enums LintCategory, LintSeverity, and ArchRuleKind plus validated ArchRuleSpec/ZoneSelector/ArchitectureConfig types deriving Serialize, Deserialize (#[serde(deny_unknown_fields)]), and JsonSchema, with unit matrices covering valid rules, unknown fields, empty selectors, and unknown zone references.'
  - 'AC-2: AcddConfig accepts optional architecture: { zones, rules } in acdd.yaml (defaulting empty), BUILTIN_MCP_TOOLS in crates/acdd-core/src/ports.rs registers code_map_lint (20 built-in tools), and tests/{core,mcp,acdd}.rs pre-register compiling seam submodules so parallel Wave 1 tasks stay disjoint.'
status: planned
subtasks:
  - subtask_ref: define-core-lint-and-arch-rule-types
    title: "Implement LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, ZoneSelector, and ArchitectureConfig in crates/acdd-core/src/lint.rs reusing path_policy globs"
  - subtask_ref: wire-config-and-builtin-mcp-tool-list
    title: "Add optional architecture config to AcddConfig and register code_map_lint in BUILTIN_MCP_TOOLS with collision unit tests"
  - subtask_ref: preregister-canonical-seam-test-modules
    title: "Pre-register tests/core/arch_lint.rs, tests/mcp/lint_surface.rs, and tests/acdd/lint_gate_and_docs_profile.rs in root test binaries"
```

### task: index-executable-invariants-and-unified-lint-engine

```yaml
task_ref: index-executable-invariants-and-unified-lint-engine
target: "Extract fenced acdd-rule blocks in crates/acdd-index/src/docs.rs with fail-closed diagnostics and implement the unified language-agnostic crates/acdd-index/src/db/lint/ engine covering arch, duplicates, dead_tails, cycles, syntax, and docs"
contract_revision: 1
depends_on:
  - core-lint-contracts-and-seam-bootstrap
tags:
  - default
scope:
  - crates/acdd-index/src/docs.rs
  - crates/acdd-index/src/db/mod.rs
  - crates/acdd-index/src/db/lint.rs
  - crates/acdd-index/src/db/lint/
  - crates/acdd-index/src/db/reader.rs
  - crates/acdd-index/src/db/reader/
  - crates/acdd-index/src/db/traversal.rs
  - crates/acdd-index/src/db/traversal/
  - tests/core/arch_lint.rs
invariants:
  - 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Fenced acdd-rule blocks persist in Node.details["arch_rules"] and malformed blocks emit deterministic errors rows with identical cold, cached, and delta Merkle commitments.'
  - 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ executes all 6 lint categories in batched passes without N+1 queries and serves existing duplicates, dead_tails, syntax, and cycles callers as the sole canonical owner.'
acceptance_criteria:
  - 'AC-1: docs.rs parses fenced acdd-rule YAML blocks inside Markdown sections, validates them via ArchRuleSpec, stores valid specs on Node.details["arch_rules"], and emits fail-closed errors diagnostics on invalid YAML, duplicate rule IDs, or undefined zones while preserving cold/cache/delta Merkle parity.'
  - 'AC-2: crates/acdd-index/src/db/lint/ implements run_lint(conn, request) across arch (forbid_dependency, allow_only_dependencies, single_owner, forbid_cycles, boundary_entry, must_implement, doc_integrity, ast_pattern), duplicates, dead_tails, cycles, syntax, and docs with path/scope filtering, segment-normalized external dependency matching (::, ., /, :), EdgeConfidence/is_test filtering, and attached INV-* + doc path:line citations.'
  - 'AC-3: Existing traversal (duplicates, dead_tails) and reader/analyze (syntax_paged, include_cycles) entrypoints delegate to db::lint with zero duplicated logic, proved by multi-language (Rust, Python, TypeScript) seam matrices in tests/core/arch_lint.rs.'
status: planned
subtasks:
  - subtask_ref: extract-and-validate-fenced-acdd-rules-in-docs
    title: "Parse fenced acdd-rule blocks in crates/acdd-index/src/docs.rs into Node.details[\"arch_rules\"] and record schema errors fail-closed"
  - subtask_ref: build-unified-language-agnostic-db-lint-engine
    title: "Implement crates/acdd-index/src/db/lint/ covering all 8 ArchRuleKind primitives, external symbol normalization, doc_integrity, and hygiene categories"
  - subtask_ref: rewire-existing-hygiene-and-analyze-delegates
    title: "Delegate traversal duplicates/dead_tails and analyze syntax/cycles to db::lint and verify multi-language + delta parity in tests/core/arch_lint.rs"
```

### task: embedded-documentation-profile-and-profile-validation

```yaml
task_ref: embedded-documentation-profile-and-profile-validation
target: "Author the 4th embedded GateProfile documentation (embedded/documentation.yaml) for compiler-free documentation and invariant authoring/refactoring and wire code_map_lint and code.lint into embedded code and audit profiles"
contract_revision: 1
depends_on:
  - core-lint-contracts-and-seam-bootstrap
tags:
  - default
scope:
  - crates/acdd-state/src/profile/inherit.rs
  - crates/acdd-state/src/profile/validate.rs
  - crates/acdd-state/src/profile/embedded/documentation.yaml
  - crates/acdd-state/src/profile/embedded/code.yaml
  - crates/acdd-state/src/profile/embedded/audit.yaml
  - crates/acdd-state/tests/state_integration/
invariants:
  - 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: embedded_profile("documentation") resolves a self-contained v2 profile inheriting base, enforcing docs.links and code.lint (categories: [arch, docs]) with independent review and zero compiler command requirement.'
acceptance_criteria:
  - 'AC-1: crates/acdd-state/src/profile/embedded/documentation.yaml defines blueprint_synthesis, tasks (build -> review -> deliver), verify_docs, polish, integration_review, and handoff gates using checks: [docs.links, code.lint] (categories: [arch, docs]), rich documentation/invariant guidance, and context lenses [docs.instructions, docs.adrs, docs.invariants, code.symbols].'
  - 'AC-2: embedded_profile in crates/acdd-state/src/profile/inherit.rs resolves "documentation" alongside "base", "code", and "audit"; embedded code.yaml and audit.yaml include code_map_lint in default_tools; and code.yaml wires code.lint into tasks.build checks and pre-test verification.'
  - 'AC-3: In-memory RunHarness and profile validation matrices in crates/acdd-state/tests/state_integration/ verify --profile documentation resolution, --in-place and --goal execution, fail-closed capability checks, and review/polish routing in < 5 ms per scenario.'
status: planned
subtasks:
  - subtask_ref: author-embedded-documentation-profile-yaml
    title: "Create crates/acdd-state/src/profile/embedded/documentation.yaml and register embedded_profile(\"documentation\") in inherit.rs"
  - subtask_ref: wire-code-lint-into-embedded-code-and-audit
    title: "Add code_map_lint to embedded default_tools and wire code.lint check into embedded code.yaml gates"
  - subtask_ref: state-integration-documentation-profile-matrix
    title: "Add RunHarness::in_memory() lifecycle and validation seam tests for the embedded documentation profile"
```

### task: host-cli-mcp-lint-surface-and-domain-provider

```yaml
task_ref: host-cli-mcp-lint-surface-and-domain-provider
target: "Expose acdd lint CLI and code_map_lint MCP with dual-surface parity, implement CodeDomainProvider check code.lint with live observe_freshness, surface enforced invariant badges in DocsDomainProvider, and wire .acdd/profile.yaml"
contract_revision: 1
depends_on:
  - index-executable-invariants-and-unified-lint-engine
  - embedded-documentation-profile-and-profile-validation
tags:
  - default
scope:
  - src/cli/
  - src/mcp/
  - src/format/
  - src/state_bridge/
  - .acdd/profile.yaml
  - tests/mcp/lint_surface.rs
  - tests/acdd/lint_gate_and_docs_profile.rs
invariants:
  - 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint, code_map_lint, and CodeDomainProvider::run_check("lint") share one request/response pipeline, exit non-zero / fail gate on error-severity violations, and bind observe_freshness to index commitments and live workspace paths.'
acceptance_criteria:
  - 'AC-1: acdd lint [TARGET] (--categories, --rule, --severity, --format markdown|json) and MCP code_map_lint execute db::lint::run_lint with workspace-confined file:// URIs, compact/full response budgeting, and non-zero CLI exit code when error-severity violations exist.'
  - 'AC-2: CodeDomainProvider registers CheckCapability("lint") accepting optional categories/severity/rules params, scopes task-level checks to req.admitted_scope / req.changed_paths (running workspace-wide on root gates), and implements observe_freshness so mutating source or doc files after acdd verify fails acdd submit closed.'
  - 'AC-3: DocsDomainProvider::provide_invariants marks invariants backed by indexed arch_rules with enforced: true ([ENFORCED] in ClaimResp prompt rendering), .acdd/profile.yaml wires code.lint, and tests/mcp/lint_surface.rs + tests/acdd/lint_gate_and_docs_profile.rs prove end-to-end CLI/MCP parity, gate blocking before tests/review, freshness drift rejection, and --profile documentation runs.'
status: planned
subtasks:
  - subtask_ref: implement-code-domain-lint-check-and-enforced-lens
    title: "Implement CodeDomainProvider code.lint + observe_freshness and DocsDomainProvider enforced invariant tagging in src/state_bridge/"
  - subtask_ref: expose-cli-acdd-lint-and-mcp-code-map-lint
    title: "Add acdd lint CLI subcommand, code_map_lint MCP handler, and shared Markdown/JSON formatter in src/{cli,mcp,format}/"
  - subtask_ref: verify-mcp-and-gate-freshness-seams
    title: "Prove CLI/MCP parity, tasks.build and root gate blocking, freshness drift rejection, and --profile documentation execution in tests/{mcp,acdd}/"
```

### task: agents-md-invariant-consolidation-and-docs-alignment

```yaml
task_ref: agents-md-invariant-consolidation-and-docs-alignment
target: "Extract scattered architectural invariants from root and subtree AGENTS.md files into canonical docs/architecture/ and docs/adr/ pages with live acdd-rule blocks, align reference docs and skills/acdd for acdd lint and --profile documentation, and prove zero-violation self-hosting"
contract_revision: 1
depends_on:
  - host-cli-mcp-lint-surface-and-domain-provider
tags:
  - default
  - docs
scope:
  - AGENTS.md
  - src/AGENTS.md
  - crates/acdd-core/AGENTS.md
  - crates/acdd-index/AGENTS.md
  - crates/acdd-state/AGENTS.md
  - tests/AGENTS.md
  - docs/AGENTS.md
  - docs/adr/
  - docs/architecture/
  - docs/reference/
  - skills/acdd/
  - README.md
  - INSTALL.md
invariants:
  - 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant across contextunity-acdd has one canonical INV-* declaration in docs/architecture/*.md or docs/adr/*.md with verified symbol claims (and acdd-rule blocks for structural boundaries), while **/AGENTS.md files remain concise operational routers.'
acceptance_criteria:
  - 'AC-1: Architectural invariants scattered across AGENTS.md, src/AGENTS.md, crates/acdd-{core,index,state}/AGENTS.md, and tests/AGENTS.md are consolidated into canonical docs/architecture/*.md and docs/adr/*.md with unique INV-* IDs and verified symbol links, and **/AGENTS.md files retain only concise routing, commands, and local operational guards.'
  - 'AC-2: Live acdd-rule blocks in docs/adr/ and docs/architecture/ machine-enforce ACDD own architecture (INV-CORE-LEAF, ADR-0001 3-crate DAG, acdd-state machine/ pure-core isolation, store/sqlite/ rusqlite confinement, acdd_core::git single-owner subprocess rule, and src/state_bridge import boundary), verified by acdd lint returning 0 errors across the live repository.'
  - 'AC-3: docs/reference/{cli,mcp-tools,profiles,acdd,configuration}.md + README.md + INSTALL.md + skills/acdd/{SKILL.md,references/*} document acdd lint, code_map_lint, acdd-rule, and the built-in --profile documentation profile, and acdd docs check + acdd validate pass cleanly.'
status: planned
subtasks:
  - subtask_ref: consolidate-scattered-agents-invariants-and-author-acdd-rules
    title: "Migrate invariants from **/AGENTS.md into docs/architecture/ and docs/adr/ with INV-* IDs, verified symbol links, and live self-hosting acdd-rule blocks"
  - subtask_ref: update-references-and-acdd-skill-for-lint-and-docs-profile
    title: "Update README.md, INSTALL.md, docs/reference/*, and skills/acdd/* to document acdd lint, code_map_lint, acdd-rule, and --profile documentation"
  - subtask_ref: verify-self-hosting-lint-docs-check-and-validate
    title: "Rebuild workspace index and verify acdd lint, acdd docs check, and acdd validate pass with zero violations"
```
