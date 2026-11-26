---
id: AR-006-acdd-spec-crate-and-workflow-cutover
title: "Extract crates/acdd-spec, rename MCP presets to index/spec/state, and cut over profile/collection terminology to workflow/spec"
doc_type: contract
status: planned
depends_on: []
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
  - 'INV-SPEC-CRATE-BOUNDARY: crates/acdd-spec depends strictly on crates/acdd-core (zero acdd-index, acdd-state, SQLite, or Git dependencies); crates/acdd-state and crates/acdd-index may read parsed specs from acdd-spec without violating INV-STATE-NO-INDEX or INV-CORE-LEAF.'
  - 'INV-THREE-SYSTEM-PRESETS: acdd.yaml top-level preset: (SubsystemPreset, replacing ambiguous top-level profile:) exposes the three workspace layers cleanly (index, spec, state, plus full and minimal) with explicit fail-closed redirect hints on legacy top-level profile:.'
  - 'INV-WORKFLOW-TERMINOLOGY-PARITY: User-facing YAML, CLI, MCP, Rust code, and docs use one consistent doc / spec / workflow vocabulary (.acdd/workflows/default.yaml, .acdd/workflows/*.yaml, workflows_dirs:, workflow:, --workflow, --rebind-workflow, --spec, SpecConfig, crates/acdd-state/src/workflow/, WorkflowDef, ACDD_WORKFLOW_SCHEMA_INVALID) with zero silent compatibility aliases.'
  - 'INV-MINIMAL-EMBEDDED-WORKFLOWS: Embedded binary workflows contain strictly the core triad (base, code, documentation); auxiliary workflows (audit, spec_planning, hotfix) ship as ready-to-copy templates under skills/acdd/references/acdd-workflow-examples/.'
  - 'INV-PROTECTED-PATHS-AUTHORIZED: This milestone explicitly authorizes workspace manifest edits (Cargo.toml, crates/acdd-spec/Cargo.toml, crates/acdd-state/Cargo.toml, crates/acdd-index/Cargo.toml), acdd.yaml zone rules, and moving .acdd/profile.yaml + .acdd/profiles/ to .acdd/workflows/ without adding third-party dependencies.'
acceptance_criteria:
  - 'AC-01: GIVEN workspace members including crates/acdd-spec, WHEN acdd lint --categories arch and cargo test --test commitment_integrity run, THEN crates/acdd-spec compiles with dependencies limited strictly to crates/acdd-core, ARCH-SPEC-DEPS enforces zero upward imports, and existing spec parsing, validation, and static roadmap DAG tests pass unchanged through crates/acdd-spec.'
  - 'AC-02: GIVEN acdd.yaml using preset: index|spec|state|full|minimal, WHEN loaded by acdd-core and wired into MCP tool registration, THEN only the admitted subsystem tool sets activate; supplying legacy top-level profile: fails closed with a redirect hint to preset:.'
  - 'AC-03: GIVEN crates/acdd-state/src/workflow/ (WorkflowDef, ResolvedWorkflow, ACDD_WORKFLOW_SCHEMA_INVALID) and SpecConfig in acdd-core, WHEN embedded workflows are queried, THEN only base, code, and documentation are embedded while audit, spec_planning, and hotfix live under skills/acdd/references/acdd-workflow-examples/.'
  - 'AC-04: GIVEN a workspace migrated to .acdd/workflows/default.yaml and workflows_dirs: [.acdd/workflows], WHEN acdd init, acdd start --workflow, acdd sync --rebind-workflow, acdd roadmap --spec, acdd draft <spec> <slug>, acdd validate, and MCP acdd_state run, THEN all commands resolve workflows and specs cleanly; legacy .acdd/profile.yaml, .acdd/profiles/, profiles_dirs:, specs.<name>.profile:, frontmatter profile:, --profile, --rebind-profile, or --collection fail closed with explicit redirect hints.'
  - 'AC-05: GIVEN the completed AR-006 tree, WHEN acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --workspace --all-targets run, THEN docs/reference/workflows.md, skills/acdd/references/{acdd-workflow-examples/,workflow-reference.md,acdd_workflow.yaml.example}, ADR-0001, ADR-0003, AGENTS.md, crates/*/AGENTS.md, ROADMAP.md, and CHANGELOG.md pass with zero errors and zero regressions.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# AR-006: Extract `crates/acdd-spec`, rename presets to `index`/`spec`/`state`, and cut over `profile`/`collection` to `workflow`/`spec`

## Goal & Non-goals

**Goal.** Establish the clean 3-system workspace architecture (`acdd-index` / `acdd-spec` / `acdd-state` over leaf `acdd-core`) and complete the `profile`/`collection` → `workflow`/`spec` terminology cutover before [AC-006](AC-006-universal-repository-memory-synthesis.md) adds universal `SchemaDef` specs, named items, and repository memory. Keeping this strictly a behavior-preserving structural refactor + mechanical cutover guarantees every existing test validates parity before new features land.

**Non-goals.**
- Universal custom spec schemas, named `items:` collections, `spec_deltas`, roadmap CLI color/custom filters, `specs.<name>` / `paths.<name>` context lenses, multi-spec attachment, or `memory` specs (all owned by [AC-006](AC-006-universal-repository-memory-synthesis.md)).
- New third-party `Cargo.toml` dependencies or SQLite schema version bumps.
- Unified swarm gates (`AC-005`), observability HTTP server (`AC-007`), or reactive triggers (`AC-008`).

## Architecture & ownership map

### 1. Target crate DAG (`ADR 0001` update)

```
crates/acdd-core   (pure leaf: config, errors, ids, path_policy, schema primitives)
   ├── crates/acdd-spec    (NEW: spec parsing, static validation, static roadmap DAG)
   ├── crates/acdd-index   (code + docs graph; depends on acdd-core, optional acdd-spec; NEVER acdd-state)
   └── crates/acdd-state   (workflow engine, SQLite store, Git ops; depends on acdd-core + acdd-spec; NEVER acdd-index)
        └── src/           (CLI, MCP, host DomainProviders wiring index + spec + state)
```

### 2. Three-layer MCP presets (`acdd.yaml` `preset:`)

| `preset:` value | Enabled subsystems | Replaces |
|---|---|---|
| `full` (default) | `index` (`code` + `docs`) + `spec` + `state` (`state` + `blackboard`) | `full` |
| `index` | `code` + `docs` navigation & lint | `navigator` |
| `spec` | `spec` (`validate`, `roadmap`, `draft`) + `docs` | *(new)* |
| `state` | `state` + `blackboard` + `spec` | `orchestrator` |
| `minimal` | `acdd_guide` only | `minimal` |

### 3. Complete `profile`/`collection` → `workflow`/`spec` cutover map

| Surface | Old name (fail-closed with redirect hint) | New canonical name |
|---|---|---|
| Workspace default workflow | `.acdd/profile.yaml` | `.acdd/workflows/default.yaml` |
| Custom workflows dir | `.acdd/profiles/` / `profiles_dirs:` | `.acdd/workflows/` / `workflows_dirs:` |
| Spec-to-workflow binding | `specs.<name>.profile:` / frontmatter `profile:` | `specs.<name>.workflow:` / frontmatter `workflow:` |
| Subsystem preset key | `acdd.yaml` top-level `profile:` | `acdd.yaml` top-level `preset:` |
| CLI & MCP flags | `--profile`, `--rebind-profile`, `--collection`, `draft <collection>` | `--workflow`, `--rebind-workflow`, `--spec`, `draft <spec>` |
| Rust types & modules | `SpecCollectionConfig`, `acdd-state::profile`, `GateProfile`, `ACDD_PROFILE_SCHEMA_INVALID` | `SpecConfig`, `acdd-state::workflow`, `WorkflowDef`, `ACDD_WORKFLOW_SCHEMA_INVALID` |
| Embedded workflows | `base`, `code`, `documentation`, `audit`, `spec_planning` | Core triad only: `base`, `code`, `documentation` (`audit`, `spec_planning`, `hotfix` moved to `skills/acdd/references/acdd-workflow-examples/`) |
| Reference docs & examples | `docs/reference/profiles.md`, `acdd-profile-examples/`, `profile-reference.md`, `acdd_profile.yaml.example` | `docs/reference/workflows.md`, `acdd-workflow-examples/`, `workflow-reference.md`, `acdd_workflow.yaml.example` |

## Tasks in this milestone

### task: extract-acdd-spec-crate

```yaml
task_ref: extract-acdd-spec-crate
target: "Create crates/acdd-spec depending strictly on crates/acdd-core, move spec parsing, static spec validation, and static roadmap DAG out of crates/acdd-state, wire acdd.yaml ARCH-SPEC-DEPS zone rules, and update ADR 0001"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - Cargo.toml
  - acdd.yaml
  - crates/acdd-spec/
  - crates/acdd-state/Cargo.toml
  - crates/acdd-state/src/lib.rs
  - crates/acdd-state/src/spec.rs
  - crates/acdd-state/src/validate.rs
  - crates/acdd-state/src/usecases/roadmap/
  - crates/acdd-state/src/context/
  - crates/acdd-state/src/store/
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/tests/
  - src/state_bridge/
  - tests/commitment_integrity/
  - docs/adr/0001-crate-dag-and-layer-boundaries.md
status: planned
subtasks:
  - subtask_ref: scaffold-acdd-spec-member
    title: "Add crates/acdd-spec workspace member (Cargo.toml, src/lib.rs, AGENTS.md) depending only on acdd-core plus existing workspace serde/serde_yaml/anyhow crates"
  - subtask_ref: move-spec-validation-and-static-roadmap
    title: "Move SpecDocument parsing, static spec validation helpers, and static roadmap DAG construction into crates/acdd-spec; keep live SQLite run-status overlay in crates/acdd-state"
  - subtask_ref: enforce-spec-crate-zone-and-adr0001
    title: "Add spec zone and ARCH-SPEC-DEPS rule in acdd.yaml, add boundary fence in tests/commitment_integrity/, and update ADR 0001 crate DAG"
```

- **Wave**: `0` | **Budget**: `~550` net LOC (mostly mechanical moves).
- **Seam & mutation proof (`AC-01`)**: `cargo test --test commitment_integrity` + `acdd lint --categories arch` prove `crates/acdd-spec` imports zero `acdd-state`/`acdd-index` symbols and all existing spec/roadmap tests pass unchanged.

### task: rename-preset-spec-and-workflow-core

```yaml
task_ref: rename-preset-spec-and-workflow-core
target: "Rename acdd.yaml top-level profile to preset (index/spec/state/full/minimal), rename SpecCollectionConfig to SpecConfig and crates/acdd-state/src/profile to workflow (WorkflowDef, ACDD_WORKFLOW_SCHEMA_INVALID), and trim embedded workflows to base, code, and documentation"
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
  - skills/acdd/references/acdd-workflow-examples/
  - skills/acdd/references/acdd-profile-examples/
  - tests/commitment_integrity/
status: planned
subtasks:
  - subtask_ref: preset-and-spec-config-rename
    title: "Rename SubsystemPreset YAML key profile -> preset (index | spec | state | full | minimal), SpecCollectionConfig -> SpecConfig, profiles_dirs -> workflows_dirs, specs.<name>.profile -> workflow, and ErrorCode to ACDD_WORKFLOW_SCHEMA_INVALID with fail-closed redirect hints"
  - subtask_ref: state-workflow-module-and-core-embedded-triad
    title: "Rename crates/acdd-state/src/profile/ -> workflow/ and GateProfile -> WorkflowDef / ResolvedWorkflow; keep only base, code, and documentation embedded while moving audit, spec_planning, and hotfix templates to skills/acdd/references/acdd-workflow-examples/"
```

- **Wave**: `1` | **Budget**: `~650` net LOC (mechanical renames across core/spec/state).
- **Seam & mutation proof (`AC-02`, `AC-03`)**: Unit and integration tests prove `preset: index|spec|state|full|minimal` gates the right subsystems, legacy top-level `profile:` fails closed with redirect hint to `preset:`, and embedded lookup resolves strictly `base`, `code`, `documentation`.

### task: cli-mcp-and-workspace-workflow-cutover

```yaml
task_ref: cli-mcp-and-workspace-workflow-cutover
target: "Cut over CLI and MCP flags (--workflow, --rebind-workflow, --spec, acdd draft <spec>), update acdd init/guide to emit .acdd/workflows/default.yaml, migrate workspace .acdd/profile.yaml and .acdd/profiles/ to .acdd/workflows/, and update test suites"
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
status: planned
subtasks:
  - subtask_ref: cli-and-mcp-flag-cutover
    title: "Rename CLI --profile -> --workflow, --rebind-profile -> --rebind-workflow, --collection -> --spec, draft <collection> -> draft <spec>, and matching MCP acdd_state parameters with fail-closed redirect hints on legacy flags/paths"
  - subtask_ref: init-guide-and-repo-workflows-migration
    title: "Update acdd init/guide and .gitignore for .acdd/workflows/default.yaml and .acdd/specs/, move this repo .acdd/profile.yaml -> .acdd/workflows/default.yaml and .acdd/profiles/hotfix.yaml -> .acdd/workflows/hotfix.yaml, update acdd.yaml workflows_dirs, and migrate tests/acdd/ + tests/mcp/"
```

- **Wave**: `2` | **Budget**: `~550` net LOC.
- **Seam & mutation proof (`AC-04`)**: `tests/acdd/` and `tests/mcp/` prove `acdd init`, `acdd start --workflow`, `acdd sync --rebind-workflow`, `acdd roadmap --spec`, `acdd draft <spec>`, and `acdd validate` pass on `.acdd/workflows/default.yaml`, while legacy `.acdd/profile.yaml`, `profiles_dirs:`, `--profile`, `--collection`, and `--rebind-profile` fail closed with redirect hints.

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
status: planned
subtasks:
  - subtask_ref: rename-workflow-docs-and-examples
    title: "Rename docs/reference/profiles.md -> workflows.md, acdd_profile.yaml.example -> acdd_workflow.yaml.example, skills/acdd/references/profile-reference.md -> workflow-reference.md, and update onboarding.md, acdd.yaml.example, gitignore.example, and SKILL.md"
  - subtask_ref: reconcile-adr-agents-and-verify
    title: "Reconcile ADR 0003, AGENTS.md routers, docs/architecture/, ROADMAP.md, and CHANGELOG.md; pass acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --workspace --all-targets"
```

- **Wave**: `3` | **Budget**: `~450` net LOC (docs/renames).
- **Seam & mutation proof (`AC-05`)**: `acdd docs check`, `acdd validate`, `acdd lint --categories arch,docs`, and `cargo test --workspace --all-targets` pass with zero errors and zero stale `.acdd/profile.yaml` references.

## Negative boundaries

- No new third-party dependencies in any `Cargo.toml`.
- No new functional spec schema features (`SchemaDef` custom fields, named `items:`, `spec_deltas`, `memory` specs) inside `AR-006` — keep `AR-006` strictly structural refactor + terminology cutover.
- No silent compatibility shims accepting legacy `.acdd/profile.yaml`, `profiles_dirs:`, `profile:`, `--profile`, `--rebind-profile`, or `--collection` without fail-closed redirect diagnostics.
- No `acdd-state` imports inside `crates/acdd-spec` or `crates/acdd-index` (`INV-SPEC-CRATE-BOUNDARY`, `INV-STATE-NO-INDEX`).

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Waves 0–3 covering crate extraction (`crates/acdd-spec`), `preset:` rename, `workflow`/`spec` cutover across code/CLI/MCP/repo, and full docs/routers reconciliation.
- **Correctness**: Every `AC-*` specifies exact fail-closed behavior and workspace verification seams (`commitment_integrity`, `acdd lint --categories arch,docs`, `acdd docs check`, `acdd validate`, `cargo test --workspace --all-targets`).
- **Coherence**: Reconciles [ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md), and [ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md); unblocks [AC-006](AC-006-universal-repository-memory-synthesis.md) cleanly on Wave 1 with zero concurrent scope overlap.
