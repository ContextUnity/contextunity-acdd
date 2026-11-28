---
id: AR-006-acdd-spec-crate-and-workflow-cutover
title: "Extract crates/acdd-spec, unify workspace dependencies and AcddError, rename MCP presets to index/spec/state, and cut over profile/collection terminology to workflow/spec"
doc_type: contract
status: completed
depends_on:
  - UX-004-mcp-tool-surface-consolidation
guidance:
  hitl: pro
owners:
  - Cargo.toml
  - acdd.yaml
  - .gitignore
  - .acdd/profile.yaml
  - .acdd/profiles/
  - .acdd/workflows/
  - crates/acdd-core/
  - crates/acdd-spec/
  - crates/acdd-index/
  - crates/acdd-state/
  - src/
  - tests/
  - AGENTS.md
  - docs/AGENTS.md
  - src/AGENTS.md
  - docs/adr/0001-crate-dag-and-layer-boundaries.md
  - docs/adr/0003-unified-profile-graph-and-state-machine.md
  - docs/architecture/
  - docs/reference/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AR-006-acdd-spec-crate-and-workflow-cutover.md
invariants:
  - 'INV-SPEC-CRATE-BOUNDARY: crates/acdd-spec depends strictly on crates/acdd-core (zero acdd-index, acdd-state, SQLite, or Git dependencies); crates/acdd-state and crates/acdd-index read parsed specs and frontmatter from acdd-spec without violating INV-STATE-NO-INDEX or INV-CORE-LEAF, and ARCH-INDEX-SPEC-ENTRY confines acdd-index imports to acdd-spec frontmatter metadata entrypoints.'
  - 'INV-SPEC-RUNTIME-OVERLAY: Static spec discovery, validation, drafting, and static roadmap DAG construction live in crates/acdd-spec over a read-only SpecRuntimeOverlay trait (with NoopRuntimeOverlay), eliminating direct SqliteStateReader instantiation from roadmap and validation logic while acdd-state supplies live Run and tombstone overlays.'
  - 'INV-WORKSPACE-DEPS-SSOT: Root Cargo.toml owns [workspace.package] metadata and [workspace.dependencies] versions/features inherited across all workspace crates (acdd-core, acdd-spec, acdd-index, acdd-state, contextunity-acdd) with zero new third-party dependencies.'
  - 'INV-CANONICAL-ACDD-ERROR: AcddError is the sole structured error type across all workspace crates (legacy StateError compatibility alias removed); ErrorCode separates ACDD_WORKFLOW_SCHEMA_INVALID from ACDD_SPEC_SCHEMA_INVALID (both exit code 2).'
  - 'INV-THREE-SYSTEM-PRESETS: acdd.yaml top-level preset: (SubsystemPreset, replacing ambiguous top-level profile:) exposes the three workspace layers cleanly (index, spec, state, plus full and minimal) via typed SubsystemId::Spec with zero new MCP tools: preset: spec enables CLI spec commands (acdd validate, acdd roadmap, acdd draft) via crates/acdd-spec plus MCP docs/core tools (get_doc, search_docs, acdd_read, acdd_guide) without creating .acdd/state.sqlite, while removed top-level profile: fails closed via serde(deny_unknown_fields).'
  - 'INV-WORKFLOW-TERMINOLOGY-PARITY: User-facing YAML, CLI, MCP, Rust code, and docs use one consistent doc / spec / workflow vocabulary (.acdd/workflows/default.yaml, .acdd/workflows/*.yaml, workflows_dirs:, workflow:, --workflow, --rebind-workflow, --spec, acdd draft <spec>, SpecConfig, crates/acdd-state/src/workflow/, WorkflowDef, ACDD_WORKFLOW_SCHEMA_INVALID) with zero silent compatibility aliases.'
  - 'INV-MINIMAL-EMBEDDED-WORKFLOWS: Embedded binary workflows contain strictly the core triad (base, code, documentation); auxiliary workflows (audit, spec_planning, hotfix) ship as ready-to-copy templates under skills/acdd/references/acdd-workflow-examples/.'
  - 'INV-PROTECTED-PATHS-AUTHORIZED: This milestone explicitly authorizes workspace manifest edits (Cargo.toml, crates/acdd-core/Cargo.toml, crates/acdd-spec/Cargo.toml, crates/acdd-state/Cargo.toml, crates/acdd-index/Cargo.toml), acdd.yaml zone/boundary rules, and moving .acdd/profile.yaml + .acdd/profiles/ to .acdd/workflows/ without adding third-party dependencies.'
acceptance_criteria:
  - 'AC-01: GIVEN workspace members using [workspace.package] and [workspace.dependencies] including crates/acdd-spec, WHEN acdd lint --categories arch and cargo test --test commitment_integrity run, THEN crates/acdd-spec compiles with dependencies limited strictly to crates/acdd-core, ARCH-SPEC-DEPS and ARCH-INDEX-SPEC-ENTRY enforce zero upward or deep-roadmap imports from acdd-index, one canonical split_frontmatter serves both acdd-spec and acdd-index, and existing spec parsing, drafting, static validation, and static roadmap DAG tests pass via SpecRuntimeOverlay without direct SqliteStateReader coupling.'
  - 'AC-02: GIVEN AcddError as the sole workspace error type (StateError alias removed) with distinct WorkflowSchemaInvalid (ACDD_WORKFLOW_SCHEMA_INVALID) and SpecSchemaInvalid (ACDD_SPEC_SCHEMA_INVALID) codes, AND acdd.yaml using preset: index|spec|state|full|minimal with SubsystemId::Spec, WHEN loaded by acdd-core and wired into MCP/CLI registration, THEN preset: spec enables CLI acdd validate/roadmap/draft plus MCP docs/core tools (get_doc, search_docs, acdd_read, acdd_guide) with state physically disabled and zero .acdd/state.sqlite creation (adding zero new MCP tools), while removed top-level profile: fails closed via serde(deny_unknown_fields).'
  - 'AC-03: GIVEN crates/acdd-state/src/workflow/ (WorkflowDef, ResolvedWorkflow, ACDD_WORKFLOW_SCHEMA_INVALID) and SpecConfig in acdd-core, WHEN embedded workflows are queried, THEN only base, code, and documentation are embedded while audit, spec_planning, and hotfix live under skills/acdd/references/acdd-workflow-examples/.'
  - 'AC-04: GIVEN a workspace migrated to .acdd/workflows/default.yaml and workflows_dirs: [.acdd/workflows], WHEN acdd init, acdd start --workflow, acdd sync --rebind-workflow, acdd roadmap --spec, acdd draft <spec> <slug>, acdd validate, and MCP acdd_state run, THEN all commands resolve workflows and specs cleanly while removed legacy keys/flags fail closed cleanly via Clap and serde(deny_unknown_fields) with zero legacy redirect scaffolding.'
  - 'AC-05: GIVEN the completed AR-006 tree, WHEN acdd docs check, acdd validate, acdd lint --categories arch,docs, code_map_query(operation="dead_tails"), and cargo test --workspace --all-targets run, THEN docs/reference/workflows.md, skills/acdd/references/{acdd-workflow-examples/,workflow-reference.md,acdd_workflow.yaml.example}, ADR-0001, ADR-0003, AGENTS.md, crates/*/AGENTS.md, ROADMAP.md, and CHANGELOG.md pass with zero errors, zero dead_tails, and zero regressions.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# AR-006: Extract `crates/acdd-spec`, unify workspace dependencies and `AcddError`, rename presets to `index`/`spec`/`state`, and cut over `profile`/`collection` to `workflow`/`spec`

## Goal & Non-goals

**Goal.** Establish the clean 3-system workspace architecture (`acdd-index` / `acdd-spec` / `acdd-state` over leaf `acdd-core`), consolidate workspace `Cargo.toml` dependencies and canonical `AcddError` error codes, wire `SubsystemId::Spec` so `preset: spec` powers stateless CLI spec operations (`validate`/`roadmap`/`draft`) + MCP `docs`/`core` without creating `.acdd/state.sqlite` or adding any new MCP tool, and complete the `profile`/`collection` → `workflow`/`spec` terminology cutover before [AC-006](AC-006-universal-repository-memory-synthesis.md) adds universal `SchemaDef` specs, named items, and repository memory. Keeping this strictly a behavior-preserving structural refactor + mechanical cutover guarantees every existing test validates parity before new features land.

**Non-goals.**
- Adding a new MCP tool (`acdd_spec`) or trimming MCP tools/actions (`code_map_prove_removal`, `code_map_tests`, MCP `acdd_state(action: roadmap|draft)` removal, and `code_map_analyze` extensions are owned by [UX-004](UX-004-mcp-tool-surface-consolidation.md)).
- Universal custom spec schemas, named `items:` collections, `spec_deltas`, roadmap CLI color/custom filters, `specs.<name>` / `paths.<name>` context lenses, multi-spec attachment, `SpecDomainProvider` split, or `memory` specs (all owned by [AC-006](AC-006-universal-repository-memory-synthesis.md)).
- Confining host `src/` `rusqlite` / `tree-sitter` / `pipeline` internals or introducing `WorkspaceServices` (deferred to [AR-007](../AR-007-host-sqlite-engine-isolation-and-workspace-facade.md) after `AC-005` and before `AC-007`).
- New third-party `Cargo.toml` dependencies or SQLite schema version bumps.
- Unified swarm gates (`AC-005`), observability HTTP server (`AC-007`), or reactive triggers (`AC-008`).

## Architecture & ownership map

### 1. Target crate DAG (`ADR 0001` update)

```
crates/acdd-core   (pure leaf: config, AcddError, ids, path_policy, schema primitives, SubsystemId::Spec)
   ├── crates/acdd-spec    (NEW: frontmatter, SpecDocument, pure spec types, draft, static validation, static roadmap DAG + SpecRuntimeOverlay)
   ├── crates/acdd-index   (code + docs graph; depends on acdd-core + acdd-spec frontmatter boundary only; NEVER acdd-state)
   └── crates/acdd-state   (workflow engine, SQLite store, Git ops, live SpecRuntimeOverlay; depends on acdd-core + acdd-spec; NEVER acdd-index)
        └── src/           (flat CLI including validate/roadmap/draft, MCP tools, host DomainProviders wiring index + spec + state)
```

### 2. Three-layer presets (`acdd.yaml` `preset:`)

| `preset:` value | Enabled subsystems | MCP tool surface (0 new tools) | Replaces |
|---|---|---|---|
| `full` (default) | `index` (`code` + `docs`) + `spec` + `state` (`state` + `blackboard`) | all 18 static tools (`code_map_*`, `ast_grep_search`, `get_doc`, `search_docs`, active-run `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`, `acdd_read`, `acdd_guide`) | `full` |
| `index` | `code` + `docs` | `code_map_*`, `ast_grep_search`, `get_doc`, `search_docs`, `acdd_read`, `acdd_guide` | `navigator` |
| `spec` | `spec` (CLI `acdd validate`/`roadmap`/`draft`, zero `.acdd/state.sqlite`) + `docs` | `get_doc`, `search_docs`, `acdd_read`, `acdd_guide` (4 tools; `acdd_state` disabled) | *(new)* |
| `state` | `state` + `blackboard` + `spec` | `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`, `acdd_read`, `acdd_guide` | `orchestrator` |
| `minimal` | core only | `acdd_read`, `acdd_guide` | `minimal` |

### 3. Complete `profile`/`collection` → `workflow`/`spec` & error cutover map

| Surface | Old name (removed, fails closed) | New canonical name |
|---|---|---|
| Workspace default workflow | `.acdd/profile.yaml` | `.acdd/workflows/default.yaml` |
| Custom workflows dir | `.acdd/profiles/` / `profiles_dirs:` | `.acdd/workflows/` / `workflows_dirs:` |
| Spec-to-workflow binding | `specs.<name>.profile:` / frontmatter `profile:` | `specs.<name>.workflow:` / frontmatter `workflow:` |
| Subsystem preset key | `acdd.yaml` top-level `profile:` | `acdd.yaml` top-level `preset:` |
| CLI & MCP flags | `--profile`, `--rebind-profile`, `--collection`, `draft <collection>` | `--workflow`, `--rebind-workflow`, `--spec`, `draft <spec>` |
| Rust types & modules | `SpecCollectionConfig`, `acdd-state::profile`, `GateProfile`, `StateError`, `ACDD_PROFILE_SCHEMA_INVALID` | `SpecConfig`, `acdd-state::workflow`, `WorkflowDef`, `AcddError`, `ACDD_WORKFLOW_SCHEMA_INVALID` + `ACDD_SPEC_SCHEMA_INVALID` |
| Embedded workflows | `base`, `code`, `documentation`, `audit`, `spec_planning` | Core triad only: `base`, `code`, `documentation` (`audit`, `spec_planning`, `hotfix` moved to `skills/acdd/references/acdd-workflow-examples/`) |
| Reference docs & examples | `docs/reference/profiles.md`, `acdd-profile-examples/`, `profile-reference.md`, `acdd_profile.yaml.example` | `docs/reference/workflows.md`, `acdd-workflow-examples/`, `workflow-reference.md`, `acdd_workflow.yaml.example` |

## Tasks in this milestone

### task: extract-acdd-spec-crate

```yaml
task_ref: extract-acdd-spec-crate
target: "Consolidate [workspace.package] and [workspace.dependencies] across Cargo.toml manifests, create crates/acdd-spec depending strictly on crates/acdd-core, unify split_frontmatter with acdd-index, move spec types/parsing/drafting/static validation/static deps/static roadmap DAG onto SpecRuntimeOverlay, wire acdd.yaml ARCH-SPEC-DEPS + ARCH-INDEX-SPEC-ENTRY rules, clean up acdd-index dead_tails, and update ADR 0001"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - Cargo.toml
  - acdd.yaml
  - crates/acdd-core/Cargo.toml
  - crates/acdd-spec/
  - crates/acdd-index/Cargo.toml
  - crates/acdd-index/src/docs.rs
  - crates/acdd-index/src/models/types.rs
  - crates/acdd-index/src/languages/dependency_registry/framework_packs.rs
  - crates/acdd-index/src/linker/typescript/semantic_context.rs
  - crates/acdd-state/Cargo.toml
  - crates/acdd-state/src/lib.rs
  - crates/acdd-state/src/types.rs
  - crates/acdd-state/src/spec.rs
  - crates/acdd-state/src/validate.rs
  - crates/acdd-state/src/usecases/deps/
  - crates/acdd-state/src/usecases/roadmap/
  - crates/acdd-state/src/usecases/lifecycle/
  - crates/acdd-state/src/usecases/commands/
  - crates/acdd-state/src/context/
  - crates/acdd-state/src/store/
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/tests/
  - src/state_bridge/
  - tests/commitment_integrity/
  - docs/adr/0001-crate-dag-and-layer-boundaries.md
status: completed
subtasks:
  - subtask_ref: workspace-deps-and-acdd-spec-member
    title: "Migrate root Cargo.toml and member Cargo.toml files to [workspace.package] and [workspace.dependencies] (zero new third-party deps) and scaffold crates/acdd-spec (Cargo.toml, src/lib.rs, AGENTS.md) depending strictly on acdd-core"
  - subtask_ref: move-spec-frontmatter-draft-validation-and-static-roadmap
    title: "Unify split_frontmatter in crates/acdd-spec (reused by crates/acdd-index/src/docs.rs), relocate pure spec types (ExtendedScopeEntry, InvariantsState, SubtaskStatus, SpecDocument, Receipt, ClosureSummary), draft/collection helpers, static dep syntax/validation, and static roadmap DAG behind SpecRuntimeOverlay (removing direct SqliteStateReader instantiation from roadmap/validate while acdd-state implements live Run/tombstone overlay)"
  - subtask_ref: enforce-spec-boundaries-and-index-hygiene
    title: "Add spec zone, ARCH-SPEC-DEPS, and ARCH-INDEX-SPEC-ENTRY boundary_entry rules in acdd.yaml, add boundary fences in tests/commitment_integrity/, remove the 3 acdd-index dead_tails, and update ADR 0001 crate DAG"
```

- **Wave**: `0` | **Budget**: `~750` net LOC (mostly mechanical moves + manifest consolidation).
- **Seam & mutation proof (`AC-01`)**: `cargo test --test commitment_integrity`, `acdd lint --categories arch`, and `code_map_query(operation="dead_tails")` prove `crates/acdd-spec` imports zero `acdd-state`/`acdd-index`/SQLite symbols, `crates/acdd-index` imports only the frontmatter entrypoint, `dead_tails == 0`, and all existing spec/roadmap tests pass unchanged.

### task: rename-preset-spec-and-workflow-core

```yaml
task_ref: rename-preset-spec-and-workflow-core
target: "Standardize workspace errors strictly on AcddError (removing StateError alias) with distinct WorkflowSchemaInvalid and SpecSchemaInvalid codes, add SubsystemId::Spec and rename acdd.yaml top-level profile to preset (index/spec/state/full/minimal), rename SpecCollectionConfig to SpecConfig and crates/acdd-state/src/profile to workflow (WorkflowDef), and trim embedded workflows to base, code, and documentation"
contract_revision: 1
depends_on:
  - extract-acdd-spec-crate
tags:
  - default
scope:
  - crates/acdd-core/src/
  - crates/acdd-spec/src/
  - crates/acdd-index/src/docs.rs
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/
  - skills/acdd/references/acdd-workflow-examples/
  - skills/acdd/references/acdd-profile-examples/
  - tests/commitment_integrity/
status: completed
subtasks:
  - subtask_ref: canonical-acdd-error-and-error-code-split
    title: "Remove StateError compatibility alias across workspace crates in favor of AcddError and split ErrorCode::ProfileSchemaInvalid into WorkflowSchemaInvalid (ACDD_WORKFLOW_SCHEMA_INVALID) and SpecSchemaInvalid (ACDD_SPEC_SCHEMA_INVALID)"
  - subtask_ref: preset-subsystem-spec-and-spec-config-rename
    title: "Add SubsystemId::Spec (gating CLI spec commands without requiring state), rename SubsystemPreset YAML key profile -> preset (index | spec | state | full | minimal), SpecCollectionConfig -> SpecConfig, profiles_dirs -> workflows_dirs, and specs.<name>.profile -> workflow with clean fail-closed parsing"
  - subtask_ref: state-workflow-module-and-core-embedded-triad
    title: "Rename crates/acdd-state/src/profile/ -> workflow/ and GateProfile -> WorkflowDef / ResolvedWorkflow; keep only base, code, and documentation embedded while moving audit, spec_planning, and hotfix templates to skills/acdd/references/acdd-workflow-examples/"
```

- **Wave**: `1` | **Budget**: `~800` net LOC (mechanical renames across core/spec/state/host).
- **Seam & mutation proof (`AC-02`, `AC-03`)**: Unit and integration tests prove `StateError` no longer exists, spec vs workflow schema failures emit `ACDD_SPEC_SCHEMA_INVALID` vs `ACDD_WORKFLOW_SCHEMA_INVALID`, `preset: index|spec|state|full|minimal` gates the right subsystems (with `preset: spec` enabling CLI spec + MCP docs/core and zero `.acdd/state.sqlite`), removed top-level `profile:` fails closed via `serde(deny_unknown_fields)`, and embedded lookup resolves strictly `base`, `code`, `documentation`.

### task: cli-mcp-and-workspace-workflow-cutover

```yaml
task_ref: cli-mcp-and-workspace-workflow-cutover
target: "Route CLI validate/roadmap/draft cleanly under SubsystemId::Spec without state.sqlite, cut over CLI and MCP flags (--workflow, --rebind-workflow, --spec, acdd draft <spec>), update acdd init/guide to emit .acdd/workflows/default.yaml, migrate workspace .acdd/profile.yaml and .acdd/profiles/ to .acdd/workflows/, and update test suites"
contract_revision: 1
depends_on:
  - rename-preset-spec-and-workflow-core
tags:
  - default
scope:
  - acdd.yaml
  - .gitignore
  - .acdd/profile.yaml
  - .acdd/profiles/
  - .acdd/workflows/
  - src/cli/
  - src/mcp/
  - src/state_bridge/
  - src/format/
  - src/engine/
  - tests/
status: completed
subtasks:
  - subtask_ref: cli-and-mcp-flag-cutover
    title: "Route CLI acdd validate, acdd roadmap --spec, and acdd draft <spec> <slug> via crates/acdd-spec under SubsystemId::Spec without creating .acdd/state.sqlite, and rename CLI/MCP --profile -> --workflow, --rebind-profile -> --rebind-workflow, --collection -> --spec, draft <collection> -> draft <spec> with clean Clap/serde rejection on legacy flags/keys"
  - subtask_ref: init-guide-and-repo-workflows-migration
    title: "Update acdd init/guide and .gitignore for .acdd/workflows/default.yaml and .acdd/specs/, move this repo .acdd/profile.yaml -> .acdd/workflows/default.yaml and .acdd/profiles/hotfix.yaml -> .acdd/workflows/hotfix.yaml, update acdd.yaml workflows_dirs, and migrate tests/acdd/ + tests/mcp/"
```

- **Wave**: `2` | **Budget**: `~550` net LOC.
- **Seam & mutation proof (`AC-04`)**: `tests/acdd/` and `tests/mcp/` prove `preset: spec` exposes only `get_doc`/`search_docs`/`acdd_read`/`acdd_guide` in MCP while CLI `acdd validate`, `acdd roadmap --spec`, and `acdd draft <spec>` pass with zero `.acdd/state.sqlite` creation; `acdd init`, `acdd start --workflow`, `acdd sync --rebind-workflow`, and MCP `acdd_state` pass on `.acdd/workflows/default.yaml` (with `code.seam` removed from `tasks.build.checks` per post-merge decision); removed legacy keys/flags fail closed cleanly without redirect scaffolding.

### task: docs-references-and-agents-reconciliation

```yaml
task_ref: docs-references-and-agents-reconciliation
target: "Rename profile reference docs/examples to workflow equivalents and reconcile ADR 0003, AGENTS.md, crate routers, architecture/reference pages, skills/acdd/, ROADMAP.md, and CHANGELOG.md"
contract_revision: 1
depends_on:
  - cli-mcp-and-workspace-workflow-cutover
tags:
  - docs
scope:
  - AGENTS.md
  - docs/AGENTS.md
  - src/AGENTS.md
  - crates/acdd-core/AGENTS.md
  - crates/acdd-index/AGENTS.md
  - crates/acdd-state/AGENTS.md
  - crates/acdd-spec/AGENTS.md
  - docs/adr/0003-unified-profile-graph-and-state-machine.md
  - docs/architecture/
  - docs/reference/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
status: completed
subtasks:
  - subtask_ref: rename-workflow-docs-and-examples
    title: "Rename docs/reference/profiles.md -> workflows.md, acdd_profile.yaml.example -> acdd_workflow.yaml.example, skills/acdd/references/profile-reference.md -> workflow-reference.md, update configuration.md for preset: index/spec/state, and update onboarding.md, acdd.yaml.example, gitignore.example, and SKILL.md"
  - subtask_ref: reconcile-adr-agents-and-verify
    title: "Reconcile ADR 0003, AGENTS.md routers, docs/architecture/, ROADMAP.md, and CHANGELOG.md; pass acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --workspace --all-targets"
```

- **Wave**: `3` | **Budget**: `~450` net LOC (docs/renames).
- **Seam & mutation proof (`AC-05`)**: `acdd docs check`, `acdd validate`, `acdd lint --categories arch,docs`, and `cargo test --workspace --all-targets` pass with zero errors and zero stale `.acdd/profile.yaml` or `StateError` references.

## Negative boundaries

- No new third-party dependencies in any `Cargo.toml`.
- No new MCP tools (`acdd_spec` is explicitly excluded; `validate`/`roadmap`/`draft` remain flat CLI commands).
- No MCP tool/action pruning or `code_map_analyze` extensions inside `AR-006` (owned by [UX-004](UX-004-mcp-tool-surface-consolidation.md)).
- No new functional spec schema features (`SchemaDef` custom fields, named `items:`, `spec_deltas`, `memory` specs) or `SpecDomainProvider` split inside `AR-006` — keep `AR-006` strictly structural refactor + terminology cutover.
- No host `src/engine/` or `src/state_bridge/` SQLite/pipeline migration inside `AR-006` (owned by [AR-007](../AR-007-host-sqlite-engine-isolation-and-workspace-facade.md)).
- No legacy compatibility shims or redirect scaffolding for removed `.acdd/profile.yaml`, `profiles_dirs:`, `profile:`, `--profile`, `--rebind-profile`, `--collection`, or `StateError` — removed surfaces fail closed cleanly.
- No `acdd-state` imports inside `crates/acdd-spec` or `crates/acdd-index` (`INV-SPEC-CRATE-BOUNDARY`, `INV-STATE-NO-INDEX`), and no direct `SqliteStateReader` construction inside `crates/acdd-spec`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Waves 0–3 covering workspace dependency SSOT, full `crates/acdd-spec` extraction (`split_frontmatter`, pure types, `draft`, static `deps`, `SpecRuntimeOverlay`), canonical `AcddError` + `ACDD_SPEC_SCHEMA_INVALID`, `SubsystemId::Spec` (`preset: spec` CLI + MCP `docs`/`core` with zero new MCP tools), `workflow`/`spec` cutover across code/CLI/MCP/repo, `dead_tails` hygiene, and full docs/routers reconciliation.
- **Correctness**: Every `AC-*` specifies exact fail-closed behavior and workspace verification seams (`commitment_integrity`, `acdd lint --categories arch,docs`, `code_map_query(operation="dead_tails")`, `acdd docs check`, `acdd validate`, `cargo test --workspace --all-targets`).
- **Coherence**: Reconciles [ADR 0001](../../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../../adr/0003-unified-profile-graph-and-state-machine.md), and [ADR 0014](../../adr/0014-declarative-spec-schemas-and-items.md); unblocks [AC-006](AC-006-universal-repository-memory-synthesis.md) cleanly on Wave 1 while keeping MCP surface consolidation and diagnostics in [UX-004](UX-004-mcp-tool-surface-consolidation.md) and host SQLite/pipeline confinement in [AR-007](../AR-007-host-sqlite-engine-isolation-and-workspace-facade.md).
