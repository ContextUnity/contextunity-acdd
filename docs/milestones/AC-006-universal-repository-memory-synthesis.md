---
id: AC-006-universal-repository-memory-synthesis
title: Universal specs in acdd.yaml, named items, explicit spec/path lenses, multi-spec runs, roadmap customization, and repository memory
doc_type: contract
status: planned
depends_on:
  - AR-006-acdd-spec-crate-and-workflow-cutover
guidance:
  hitl: pro
owners:
  - acdd.yaml
  - .gitignore
  - crates/acdd-core/src/
  - crates/acdd-spec/src/
  - crates/acdd-index/src/docs.rs
  - crates/acdd-state/src/
  - src/state_bridge/
  - src/format/
  - src/cli/
  - src/mcp/
  - tests/acdd/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/adr/0014-declarative-spec-schemas-and-items.md
  - docs/reference/
  - docs/architecture/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/AC-006-universal-repository-memory-synthesis.md
invariants:
  - 'INV-SINGLE-OWNER: SchemaDef validation, confined ref resolution for specs_dirs:/workflows_dirs:, spec parsing, named item extraction, spec_deltas verification, 4-tier context relevance ranking, and roadmap filtering each live in one canonical module with zero duplicate validators or parallel registries.'
  - 'INV-CORE-LEAF: Pure spec-schema, roadmap-display, and shared confined ref-resolver types in crates/acdd-core stay free of SQLite, Git, HTTP, or direct filesystem traversal.'
  - 'INV-SPEC-CRATE-BOUNDARY: Spec parsing, SchemaDef field/item validation, spec_deltas checking, and static roadmap filtering live in crates/acdd-spec (depending only on acdd-core); acdd-state owns live Run attachment and gate execution.'
  - 'INV-STATE-NO-INDEX: crates/acdd-state never imports crates/acdd-index; specs.<name> and paths.<name> lenses read code-map doc edges strictly through host DocsDomainProvider in src/state_bridge/docs_domain.rs.'
  - 'INV-SPEC-UNIVERSAL: Every specification document is a Spec whose shape, path, numbering, default workflow:, and roadmap: true|false membership live strictly in acdd.yaml specs: (inline, base: contract|memory, or external .acdd/specs/*.yaml ref:); workflow files contain zero specs: or roadmap: keys.'
  - 'INV-EXPLICIT-CONTEXT-RANKING: Gates reference specs directly as specs.<name> and non-spec doc globs as paths.<name> (zero hidden specs->paths bridging, replacing docs.instructions and docs.adrs); Tier 6 context relevance ranks deterministically (1. direct ID/title citation -> 2. scope/owners overlap -> 3. code-map doc_edges -> 4. global ** fallback) bounded by max_bytes.'
  - 'INV-MULTI-SPEC-SINGLE-WORKFLOW: One Run always executes one workflow and accepts 0..N attached input specs (acdd start [SPEC ...]) surfaced in ClaimResp.contract.attached_specs[] while validating 0..M output specs via docs.specs (with optional spec/required parameters).'
  - 'INV-MEMORY-IS-A-SPEC: Repository memory is the built-in memory Spec (items.rules) written by a single-role memory_synthesis gate (scope: [specs.memories], checks: [docs.specs]) and delivered via context: [specs.memories] with zero memory-specific parser, status axis, or MCP tool.'
  - 'INV-SCOPE-INTEGRITY: Cargo.toml, member Cargo.toml files, build.rs, and engine runtime files (.acdd/*.sqlite*) remain off-limits.'
acceptance_criteria:
  - 'AC-01: GIVEN ADR 0014 admitted and ADR 0006 updated with the 4-tier context relevance policy, WHEN checked alongside the workspace, THEN both ADRs and the retrospectives .gitignore decision are recorded before implementation begins and tests/acdd/spec_schemas/ compiles cleanly.'
  - 'AC-02: GIVEN any Spec document validated against built-in contract (default when frontmatter spec: is omitted), WHEN parsed by crates/acdd-spec, THEN existing milestones, plans, tasks, and workflows run identically with zero doc_type == "contract" branches; ephemeral --goal text lives under goal: rather than spec:.'
  - 'AC-03: GIVEN acdd.yaml specs: entries using inline definitions, base: contract|memory, or external ref: files under specs_dirs: [.acdd/specs], WHEN loaded through the shared confined ref resolver, THEN specs.<name> resolves fields, items, workflow:, and roadmap: true|false deterministically while rejecting path traversal, multi-hop inheritance, or duplicate specs:/roadmap: keys inside .acdd/workflows/*.yaml with ACDD_WORKFLOW_SCHEMA_INVALID.'
  - 'AC-04: GIVEN a Spec declaring named items: collections (with optional singular item: token), WHEN validated in 3-step order (core shape -> SchemaDef fields/items -> closed DAG including cross-Spec refs RES-001#probe-a), THEN valid Specs pass and unknown fields fail closed with SPEC_FIELD_UNKNOWN (+ edit-distance hint), undeclared headings with SPEC_ITEM_UNKNOWN, and unresolved refs with ITEM_DEPENDENCY_UNKNOWN.'
  - 'AC-05: GIVEN acdd start [SPEC ...] [--workflow <name>] [--goal "..."] with 0, 1, or N registered specs, WHEN the run starts and a worker claims a gate, THEN one resolved workflow executes and all attached specs appear in ClaimResp.contract.attached_specs[] within byte budgets; WHEN a gate runs docs.specs with { spec: <name>, required: true }, THEN verify fails closed if no valid file of that spec type was created or modified in gate scope.'
  - 'AC-06: GIVEN gates referencing specs.<name> and workflow paths.<name> in scope: and context:, WHEN evaluated by DocsDomainProvider and workflow validation, THEN specs.<name> and paths.<name> expand scope globs and rank context items in strict 4-tier order (1. direct citation -> 2. scope/owners overlap -> 3. code-map edges -> 4. global ** fallback) bounded by max_bytes; legacy docs.instructions/docs.adrs and unknown names fail validation with redirect hints while empty globs emit a warning.'
  - 'AC-07: GIVEN a Spec declaring spec_deltas against another Spec id-keyed list field, WHEN spec.deltas_applied runs in acdd verify, THEN every added id present, every removed id absent, and every modified id carrying its now text passes; omitting an addition, leaving a removal, or mismatching modified text fails closed.'
  - 'AC-08: GIVEN acdd.yaml roadmap: display config (badges, group_by, color: auto|always|never) and Specs with explicit roadmap: true|false, WHEN acdd roadmap runs with --spec, --all, --tag, --field key=value, --group-by, or --color (respecting NO_COLOR), THEN only roadmap: true specs appear by default, configured badges/grouping render in CLI/MCP, and ANSI colors suppress cleanly under NO_COLOR or --color=never.'
  - 'AC-09: GIVEN a milestone run with a rejected review and a single-role memory_synthesis gate targeting scope: [specs.memories], WHEN the builder emits a spec: memory file citing the real blackboard finding ID and runs acdd verify, THEN docs.specs passes, a subsequent task receives the relevant rule via context: [specs.memories], zero edits pass cleanly, and fabricating a non-existent blackboard evidence ID fails closed.'
  - 'AC-10: GIVEN the completed AC-006 tree, WHEN tests/acdd/spec_schemas/acceptance.rs, acdd docs check, acdd validate, and acdd lint --categories arch,docs run, THEN ADR 0006, ADR 0014, docs/reference/workflows.md (including the complete specs.*/paths.*/code.* context table), docs/architecture/, skills/acdd/references/acdd-spec-examples/, ROADMAP.md, and CHANGELOG.md pass with zero errors.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# AC-006: Universal specs in `acdd.yaml`, named items, explicit spec/path lenses, multi-spec runs, roadmap customization, and repository memory

## Goal & Non-goals

**Goal.** Build on [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md) (`crates/acdd-spec`, `preset:`, `.acdd/workflows/`) to implement [ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md): universal schema-validated Specs in `acdd.yaml` `specs:` (inline, `base: contract|memory`, or `.acdd/specs/*.yaml` `ref:`), named `items:` collections, `0..N` attached input specs per run (`acdd start [SPEC ...]`) and `0..M` validated output specs (`docs.specs`), explicit `specs.<name>` and `paths.<name>` context/scope lenses with documented 4-tier deterministic relevance ranking, `spec_deltas` verified by `spec.deltas_applied`, customizable ANSI CLI/MCP `acdd roadmap`, and repository memory as the built-in `memory` Spec.

**Non-goals.**
- Crate extraction (`crates/acdd-spec`) and `profile`→`workflow` / `collection`→`spec` cutover (already completed in [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md)).
- Generalized `mode: swarm` / `with: spec.<collection>` execution over arbitrary item collections or extended `when:` field predicates (owned by [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md)).
- Pause-and-call child runs (`parent_run_id` / `waiting_on_run_id`) across workflows (owned by [AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)).
- Vector stores, embeddings, fuzzy retrieval, model clients, or new `Cargo.toml` dependencies.

## Target architecture & decisions

### 1. Single `acdd.yaml` `specs:` registry & `roadmap:` display config
- `acdd.yaml` `specs:` (`SpecConfig`) owns `path`/`paths`, `prefixes`, `digits`, `base: contract|memory`, external `ref:` (resolved in `specs_dirs: [.acdd/specs]` via one shared confined `<name> | <path>[:<sha256_prefix>]` helper in `crates/acdd-core/src/config/`), `workflow:`, `roadmap: true|false`, `fields`, and `items`.
- `acdd.yaml` `roadmap:` (alongside `response:`) owns CLI presentation (`badges`, `group_by: wave|<field>|spec`, `color: auto|always|never` with `NO_COLOR`).
- Workflow files (`.acdd/workflows/*.yaml`) reject `specs:` and `roadmap:` keys fail-closed (`ACDD_WORKFLOW_SCHEMA_INVALID`).

### 2. Explicit `specs.<name>` & `paths.<name>` + 4-tier deterministic relevance ranking
- `scope: [specs.<name>]` and `context: [specs.<name>]` reference `acdd.yaml` `specs:` directly (zero hidden `specs → paths` magic).
- `paths:` inside workflows defines strictly non-spec doc globs (`paths.docs`, `paths.adrs`, `paths.instructions`), replacing hardcoded `docs.instructions` and `docs.adrs`.
- Tier 6 `ClaimResp.context_lenses` ranks candidate items/documents deterministically (documented in `ADR 0006`, `ADR 0014`, and `docs/reference/workflows.md`) and trims by `max_bytes`:
  1. Direct ID/title citation in task `target` or owning Spec body (highest priority).
  2. Explicit `scope`/`owners` overlap (`scopes_overlap` against task `admitted_scope`).
  3. Indexed `code-map.sqlite` edges (`documents`, `references_doc`, `cites_doc`, `governs`, `governed_by`) to symbols/paths in `admitted_scope`.
  4. Global fallback rules (`scope: ["**"]` / unlinked repo-wide docs — dropped first under budget pressure).

### 3. `0..N` attached input specs & `0..M` validated output specs
- `acdd start [SPEC ...] [--workflow <name>] [--goal "..."]` binds `0..N` registered Specs to one `Run` executing one resolved workflow; attached specs persist on the run and render in `ClaimResp.contract.attached_specs[]` within byte budgets.
- `checks: [{ check: docs.specs, with: { spec: <name>, required: true } }]` enforces that a gate creates/updates at least one valid document of that Spec type in its admitted scope; plain `checks: [docs.specs]` keeps zero-edit tolerance.

### 4. Repository memory as built-in `memory` Spec
- Built-in `memory` base (`roadmap: false`, `items.rules` → `### rule: <ref>` with `scope`, `statement`, `evidence`, `status`) lives in `crates/acdd-spec`.
- A single-role `memory_synthesis` gate template (`scope: [specs.memories]`, `checks: [docs.specs]`) verifies that every cited `evidence` blackboard ID exists in the run's SQLite blackboard; subsequent tasks consume relevant rules via `context: [specs.memories]`.

## Tasks in this milestone

### task: bootstrap-adr-and-seams

```yaml
task_ref: bootstrap-adr-and-seams
target: "Admit ADR 0014, document the 4-tier context relevance policy in ADR 0006, record the docs/memories/ .gitignore decision, and register tests/acdd/spec_schemas/ harness modules"
contract_revision: 1
depends_on: []
tags:
  - docs
  - default
scope:
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/adr/0014-declarative-spec-schemas-and-items.md
  - docs/adr/README.md
  - tests/acdd/mod.rs
  - tests/acdd/spec_schemas/
status: planned
subtasks:
  - subtask_ref: admit-adr0014-and-update-adr0006
    title: "Move ADR 0014 to accepted in docs/adr/README.md, document the 4-tier deterministic relevance ranking in ADR 0006, and record docs/memories/ tracking decision"
  - subtask_ref: register-spec-schema-seams
    title: "Register tests/acdd/spec_schemas/ submodules in tests/acdd/mod.rs compiling cleanly against existing crates/acdd-spec"
```

- **Wave**: `0` | **Budget**: `~220` net LOC.
- **Seam & mutation proof (`AC-01`)**: `acdd docs check` and `cargo test --test acdd spec_schemas` pass cleanly.

### task: spec-core-and-contract-schema

```yaml
task_ref: spec-core-and-contract-schema
target: "Split SpecDocument into fixed core fields plus SchemaDef-validated fields, embed built-in contract and memory bases in crates/acdd-spec, move ephemeral --goal narrative from frontmatter spec: to goal:, and remove doc_type == contract branches"
contract_revision: 1
depends_on:
  - bootstrap-adr-and-seams
tags:
  - default
scope:
  - crates/acdd-core/src/schema.rs
  - crates/acdd-spec/src/
  - crates/acdd-state/src/usecases/lifecycle/
  - tests/acdd/spec_schemas/core.rs
status: planned
subtasks:
  - subtask_ref: fixed-core-and-embedded-bases
    title: "Split SpecDocument into core fields + SchemaDef-validated extra fields in crates/acdd-spec, embed built-in contract and memory bases, and preserve default contract compatibility when spec: is omitted"
  - subtask_ref: goal-frontmatter-and-doctype-cleanup
    title: "Move ephemeral --goal narrative text to frontmatter goal: so spec: selects Spec type cleanly, and eliminate remaining doc_type == contract branches"
```

- **Wave**: `1` | **Budget**: `~480` net LOC.
- **Seam & mutation proof (`AC-02`)**: `tests/acdd/spec_schemas/core.rs` proves existing milestones without `spec:` parse under built-in `contract`, `--goal` writes `goal:` rather than `spec:`, and unknown core/schema fields fail closed.

### task: acdd-yaml-specs-and-refs

```yaml
task_ref: acdd-yaml-specs-and-refs
target: "Extend SpecConfig in acdd.yaml with base, ref, roadmap, fields, and items, add specs_dirs and shared confined ref resolution in crates/acdd-core/src/config/, parse acdd.yaml roadmap display config, and reject specs/roadmap keys inside workflows"
contract_revision: 1
depends_on:
  - bootstrap-adr-and-seams
tags:
  - default
scope:
  - crates/acdd-core/src/config/
  - crates/acdd-index/src/docs.rs
  - crates/acdd-state/src/workflow/schema.rs
  - crates/acdd-state/src/workflow/validate.rs
  - tests/acdd/spec_schemas/config.rs
status: planned
subtasks:
  - subtask_ref: shared-ref-resolver-and-specs-config
    title: "Add shared confined <name> | <path>[:<sha256_prefix>] resolver in crates/acdd-core/src/config/ for both specs_dirs: [.acdd/specs] and workflows_dirs:, extend SpecConfig with base/ref/roadmap/fields/items (single-hop only), and add RoadmapDisplayConfig"
  - subtask_ref: reject-duplicate-workflow-keys
    title: "Ensure .acdd/workflows/*.yaml rejects duplicate specs: or roadmap: keys fail-closed with ACDD_WORKFLOW_SCHEMA_INVALID and acdd-index reads spec_paths cleanly without acdd-state coupling"
```

- **Wave**: `1` | **Budget**: `~440` net LOC.
- **Seam & mutation proof (`AC-03`)**: `tests/acdd/spec_schemas/config.rs` proves inline, `base: contract|memory`, and `.acdd/specs/*.yaml` `ref:` entries load deterministically, while path traversal, multi-hop `base:`, or `specs:`/`roadmap:` inside `.acdd/workflows/*.yaml` fail closed.

### task: named-items-and-attached-specs

```yaml
task_ref: named-items-and-attached-specs
target: "Implement named items: collections (### <item>: <ref>), 3-step validation (SPEC_FIELD_UNKNOWN, SPEC_ITEM_UNKNOWN, ITEM_DEPENDENCY_UNKNOWN), multi-spec acdd start [SPEC ...] with ClaimResp.contract.attached_specs[], and docs.specs required-spec check parameters"
contract_revision: 1
depends_on:
  - spec-core-and-contract-schema
  - acdd-yaml-specs-and-refs
tags:
  - default
scope:
  - crates/acdd-spec/src/
  - crates/acdd-state/src/context/
  - crates/acdd-state/src/store/
  - crates/acdd-state/src/usecases/
  - src/state_bridge/docs_domain.rs
  - src/cli/
  - src/mcp/
  - tests/acdd/spec_schemas/items.rs
status: planned
subtasks:
  - subtask_ref: named-items-and-three-step-validation
    title: "Parse ### <item>: <ref> blocks across declared items: collections, enforce 3-step validation (core -> SchemaDef -> closed cross-Spec DAG RES-001#probe-a), and emit SPEC_FIELD_UNKNOWN (+ edit-distance hint), SPEC_ITEM_UNKNOWN, and ITEM_DEPENDENCY_UNKNOWN"
  - subtask_ref: multi-spec-start-and-docs-specs-required
    title: "Support acdd start [SPEC ...] (0..N specs) persisted as attached_specs and rendered in ClaimResp.contract.attached_specs[] within byte budgets, plus docs.specs with: { spec/specs, required } verification"
```

- **Wave**: `2` | **Budget**: `~620` net LOC.
- **Seam & mutation proof (`AC-04`, `AC-05`)**: `tests/acdd/spec_schemas/items.rs` proves custom `### probe:` / `### rule:` items validate with cross-Spec DAG refs, `acdd start AUD-001 AC-006` surfaces both specs in `ClaimResp.contract.attached_specs[]`, and `docs.specs` with `required: true` fails closed when the target spec file is missing.

### task: spec-and-path-context-lenses

```yaml
task_ref: spec-and-path-context-lenses
target: "Wire explicit specs.<name> and workflow paths.<name> into scope: and context: with 4-tier deterministic relevance ranking (direct citation -> scope/owners overlap -> code-map edges -> global **), replacing hardcoded docs.instructions and docs.adrs"
contract_revision: 1
depends_on:
  - named-items-and-attached-specs
tags:
  - default
scope:
  - crates/acdd-state/src/workflow/
  - src/state_bridge/docs_domain.rs
  - tests/acdd/spec_schemas/paths.rs
status: planned
subtasks:
  - subtask_ref: workflow-paths-and-specs-scope-expansion
    title: "Add paths: to WorkflowDef (with defaults in embedded base), resolve specs.<name> and paths.<name> in gate scope: and context:, reject unknown names and legacy docs.instructions/docs.adrs with redirect hints, and warn on empty glob matches"
  - subtask_ref: four-tier-relevance-ranking
    title: "Implement deterministic 4-tier relevance ranking in DocsDomainProvider (1. direct ID/title citation -> 2. scope/owners overlap -> 3. code-map doc edges -> 4. global ** fallback) bounded by max_bytes for both specs.<name> and paths.<name>"
```

- **Wave**: `3` | **Budget**: `~460` net LOC.
- **Seam & mutation proof (`AC-06`)**: `tests/acdd/spec_schemas/paths.rs` proves direct ID citations outrank `scope` overlaps, which outrank `code-map` edges, which outrank `**` globals under a tight byte budget; legacy `docs.instructions` / `docs.adrs` fail closed with redirect hints.

### task: spec-deltas

```yaml
task_ref: spec-deltas
target: "Add spec_deltas core parsing in crates/acdd-spec and built-in spec.deltas_applied verification check confirming added, modified, and removed id-keyed entries in target Specs"
contract_revision: 1
depends_on:
  - spec-and-path-context-lenses
tags:
  - default
scope:
  - crates/acdd-spec/src/deltas.rs
  - crates/acdd-spec/src/lib.rs
  - src/state_bridge/docs_domain.rs
  - tests/acdd/spec_schemas/deltas.rs
status: planned
subtasks:
  - subtask_ref: deltas-parser-and-check
    title: "Parse spec_deltas (target, field, added, modified, removed) and register spec.deltas_applied verifying target list membership and modified now text deterministically"
```

- **Wave**: `4` | **Budget**: `~260` net LOC.
- **Seam & mutation proof (`AC-07`)**: `tests/acdd/spec_schemas/deltas.rs` proves `spec.deltas_applied` passes when all target edits match and fails closed if any `added` entry is missing, `removed` entry remains, or `modified.now` diverges.

### task: roadmap-display-and-schema-filters

```yaml
task_ref: roadmap-display-and-schema-filters
target: "Wire acdd.yaml roadmap: display config (badges, group_by, color with NO_COLOR/--color), explicit roadmap: true|false default filtering, and --spec/--all/--tag/--field/--group-by filters across CLI and MCP"
contract_revision: 1
depends_on:
  - spec-and-path-context-lenses
tags:
  - default
scope:
  - crates/acdd-spec/src/roadmap.rs
  - src/format/roadmap.rs
  - src/cli/dispatch_state.rs
  - src/cli/args.rs
  - src/mcp/tools/state_tools.rs
  - tests/acdd/spec_schemas/roadmap.rs
status: planned
subtasks:
  - subtask_ref: roadmap-filtering-and-badges
    title: "Filter acdd roadmap by specs.<name>.roadmap: true by default, add --spec/--all/--tag/--field key=value/--group-by across CLI and MCP, and project configured badges"
  - subtask_ref: ansi-color-and-no-color
    title: "Add ANSI color rendering in src/format/roadmap.rs honoring acdd.yaml roadmap.color, --color=auto|always|never, TTY detection, and NO_COLOR"
```

- **Wave**: `4` | **Budget**: `~380` net LOC.
- **Seam & mutation proof (`AC-08`)**: `tests/acdd/spec_schemas/roadmap.rs` proves `roadmap: false` specs hide by default unless `--spec` or `--all` is passed, `--field priority=high` and `--group-by` render configured badges, and `NO_COLOR=1` / `--color=never` emit zero ANSI escape sequences.

### task: memory-schema-and-gate

```yaml
task_ref: memory-schema-and-gate
target: "Wire built-in memory Spec (docs/memories/), single-role memory_synthesis gate template with blackboard evidence verification, specs.memories delivery, and reusable spec examples (audit_brief, audit_report, proposal)"
contract_revision: 1
depends_on:
  - spec-deltas
tags:
  - default
scope:
  - acdd.yaml
  - .gitignore
  - crates/acdd-state/src/workflow/embedded/
  - crates/acdd-state/src/usecases/verify.rs
  - skills/acdd/references/acdd-spec-examples/
  - tests/acdd/spec_schemas/memory.rs
status: planned
subtasks:
  - subtask_ref: memory-synthesis-gate-and-evidence-check
    title: "Add single-role memory_synthesis gate template (scope: [specs.memories], checks: [docs.specs]), verify every cited evidence blackboard ID exists in run SQLite blackboard, and update acdd.yaml/.gitignore per Wave 0 decision"
  - subtask_ref: reusable-spec-examples
    title: "Add ready-to-copy spec templates (audit_brief.yaml, audit_report.yaml, proposal.yaml) under skills/acdd/references/acdd-spec-examples/ paired with acdd-workflow-examples/"
```

- **Wave**: `5` | **Budget**: `~340` net LOC.
- **Seam & mutation proof (`AC-09`)**: `tests/acdd/spec_schemas/memory.rs` proves a rejected review produces a linked `spec: memory` rule delivered to a later task via `context: [specs.memories]`, zero edits pass cleanly, and a fabricated blackboard `evidence` ID fails closed.

### task: docs-and-acceptance

```yaml
task_ref: docs-and-acceptance
target: "Reconcile docs/reference/workflows.md (with complete specs.*/paths.*/code.* context and 4-tier relevance table), docs/architecture/, skills/acdd/, ROADMAP.md, and CHANGELOG.md, and execute end-to-end acceptance seam"
contract_revision: 1
depends_on:
  - memory-schema-and-gate
  - roadmap-display-and-schema-filters
tags:
  - docs
scope:
  - docs/reference/
  - docs/architecture/
  - skills/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - tests/acdd/spec_schemas/acceptance.rs
status: planned
subtasks:
  - subtask_ref: context-table-and-reference-docs
    title: "Document complete specs.*, paths.*, and code.* context table + 4-tier relevance ranking in docs/reference/workflows.md, reconcile configuration/cli/tasks/acdd references, skills/acdd/, ROADMAP.md, and CHANGELOG.md"
  - subtask_ref: end-to-end-acceptance
    title: "Execute tests/acdd/spec_schemas/acceptance.rs across contract, external .acdd/specs/*.yaml ref, multi-spec start, specs.*/paths.* ranking, roadmap filters, spec_deltas, and memory Specs; pass acdd docs check, acdd validate, and acdd lint"
```

- **Wave**: `6` | **Budget**: `~400` net LOC.
- **Seam & mutation proof (`AC-10`)**: `tests/acdd/spec_schemas/acceptance.rs` + `acdd docs check` + `acdd validate` + `acdd lint --categories arch,docs` pass with zero errors.

## Negative boundaries

- No duplicate `specs:` or `roadmap:` keys inside `.acdd/workflows/*.yaml`, and no hidden `specs → paths` magic bridging.
- No separate `inputs:` workflow/milestone DSL and no nested `GateDef.workflow:` YAML merging (multi-workflow chaining uses pause-and-call child runs in [AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md)).
- No memory-specific parser, registry, status axis, evidence collector, reviewer gate, context lens, CLI command, or MCP tool.
- No second schema validator or file resolver outside `SchemaDef` and the shared `acdd-core` confined reference resolver.
- No multi-level spec inheritance trees, no vector store/embeddings, and no `Cargo.toml` modifications in `AC-006`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: Every criterion (`AC-01`..`AC-10`) maps 1:1 to a bounded task across Waves 0–6, dedicated seams in `tests/acdd/spec_schemas/`, and co-evolved ADRs/docs/examples.
- **Correctness**: Every `AC-*` names trigger, expected behavior, fail-closed rejection (`SPEC_FIELD_UNKNOWN`, `SPEC_ITEM_UNKNOWN`, `ITEM_DEPENDENCY_UNKNOWN`, `ACDD_WORKFLOW_SCHEMA_INVALID`), and seam mutation proof.
- **Coherence**: Builds cleanly on [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md), reconciles [ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md) and [ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md), preserves `INV-SINGLE-OWNER`, `INV-CORE-LEAF`, `INV-SPEC-CRATE-BOUNDARY`, and `INV-STATE-NO-INDEX`, and keeps all concurrent same-wave `scope:` globs strictly disjoint (Wave 1: `spec-core-and-contract-schema` vs. `acdd-yaml-specs-and-refs`; Wave 4: `spec-deltas` vs. `roadmap-display-and-schema-filters`).
