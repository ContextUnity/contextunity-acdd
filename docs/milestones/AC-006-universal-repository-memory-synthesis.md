---
id: AC-006-universal-repository-memory-synthesis
title: Universal spec schemas, named path sets, and repository memory as a spec-driven gate
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
- crates/acdd-core/src/ports.rs
- crates/acdd-state/src/spec.rs
- crates/acdd-state/src/profile/
- crates/acdd-state/src/validate.rs
- crates/acdd-state/src/usecases/
- crates/acdd-state/src/context/
- src/state_bridge/
- src/engine/
- src/format/
- docs/adr/
- docs/architecture/
- docs/reference/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
invariants:
- 'INV-SPEC-UNIVERSAL: Every specification document is a Spec whose shape comes from a profile-declared schema named by frontmatter `spec:`; the Rust core owns only the fields the state machine and DAG need.'
- 'INV-SPEC-CONTRACT-COMPAT: `spec:` defaults to the embedded `contract` schema, which reproduces the current milestone and `### task:` shape so existing manifests parse and run unchanged.'
- 'INV-SPEC-SINGLE-RESOLVER: Schema references resolve through the existing profile reference resolver (`<name> | <path>[:<sha256_prefix>]`, `profiles_dirs`, confinement); no second lookup path.'
- 'INV-SPEC-NO-PROMPTS-IN-SPECS: Specs carry data only; agent instructions stay in profile guidance and gates.'
- 'INV-PATHS-NO-LAYOUT-LITERALS: Repository layout (docs, ADR, instruction, memory locations, and gate write scopes) comes from the profile `paths:` section; Rust context code and embedded gate scopes contain no repository layout literals.'
- 'INV-PATHS-EXPLICIT-REFERENCE: Path sets are referenced only as `paths.<name>`; an unknown name fails validation.'
- 'INV-MEMORY-IS-A-SPEC: Repository memory is Specs of the embedded `memory` schema, written by an ordinary single-role gate and delivered through `paths.memories`; no memory-specific parser, registry, lens, CLI, or MCP tool exists.'
- 'INV-MINIMAL-INTERVENTION: Reuse SchemaDef, profile inherit merging, the profile reference resolver, docs extraction, code-map doc edges, ClaimResp budgets, and blackboard evidence; add no dependency, vector store, model client, or parallel registry.'
- 'INV-SCOPE-INTEGRITY: Cargo.toml, member Cargo.toml files, build.rs, and .acdd/** stay outside every task write scope.'
acceptance_criteria:
- 'AC-01: A Spec without `spec:` parses as `contract`; every manifest under docs/milestones/ and docs/plans/ validates and starts exactly as before.'
- 'AC-02: Frontmatter `spec:` selects a schema from the merged profile `specs:` map; schema fields at Spec and item level are validated by SchemaDef with unknown, mistyped, and missing fields rejected (`SPEC_FIELD_UNKNOWN` with edit-distance hint).'
- 'AC-03: `specs:` merges key-wise through `inherit`; an entry may be an inline schema or a reference resolved by the profile reference resolver; an unresolvable reference fails closed.'
- 'AC-04: A schema declares item collections (`items: {<collection>: {item?: <item>, fields}}`; `item` defaults to `<collection>`; embedded contract: `tasks: {item: task}`); `### <item>: <ref>` blocks parse into the collection with core `ref`, `depends_on`, `tags`, `status`, `receipt`; unknown item headings fail (`SPEC_ITEM_UNKNOWN`).'
- 'AC-05: Parsed items are exposed as `spec.<collection>` (for example `spec.tasks`) in `SpecDocument` and `ClaimResp.contract`; the existing `- tasks:` block executes `contract` tasks unchanged; no new gate block is added.'
- 'AC-06: `depends_on` resolves across Specs of any schema, including item references `<spec_id>#<ref>`; `acdd validate` and `acdd roadmap` report one closed DAG.'
- 'AC-07: The ephemeral `--goal` narrative is stored in `goal:`; `spec:` is no longer a narrative field.'
- 'AC-08: `spec_deltas` entries name `target`, `field`, and `added` / `modified` / `removed` ids; the check `spec.deltas_applied` passes only when every added id exists in the target field, every removed id is absent, and every modified id carries its `now` text.'
- 'AC-09: The profile `paths:` section merges key-wise through `inherit`; embedded `base` declares `docs`, `adrs`, `instructions`, and `memories` reproducing current behavior; `paths.<name>` expands to globs in `scope:` and delivers related files in `context:` (code-map doc edges to the task scope, instruction files on the ancestor path, files named in the target; whole set when the set has no links), ordered by path and cut by the ClaimResp byte budget.'
- 'AC-10: `docs.instructions` and `docs.adrs` are replaced by `paths.instructions` and `paths.adrs` in embedded profiles and examples; the old names fail validation with a redirect hint; `docs.invariants` reads from `paths.docs`; an unknown `paths.<name>` fails validation and an empty set warns.'
- 'AC-11: The embedded `memory` schema (`topic` field, `### rule:` items with `evidence`) and the single-role `memory_synthesis` gate template let any profile write rules into `paths.memories` from rejections and decisions already in ClaimResp; `acdd.yaml` admits `docs/memories` in `docs:`; ACDD validates the edited Specs (schema fields; every `evidence` entry names a blackboard message of the current run); zero changes is a clean pass; a later gate listing `paths.memories` receives the memory Specs that link to its task files.'
- 'AC-12: Code branches on `doc_type == "contract"` and repository layout literals in docs context code and embedded gate scopes are removed; ADR 0014 is accepted; docs/reference (including a context table listing every `paths.*` and `code.*` entry with its source, selection rule, and budget), docs/architecture, skills/acdd, ROADMAP.md, and CHANGELOG.md pass `acdd docs check` and `acdd validate`.'
required_skills:
- acdd
- contract-planning
- agent-instructions
- test-suite-rules
---

# AC-006: Universal spec schemas, named path sets, and repository memory

## Outcome

ACDD stops hardcoding the shape of a specification and the layout of a
repository. A profile declares spec schemas and named path sets the same way it
already declares roles, proofs, and gates. Repository memory is the first
consumer: memory is a `memory` Spec written by an ordinary gate and delivered
through an ordinary path set.

The design is recorded in
[ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md); Task 0 accepts it.

## Agreed mechanisms

### Spec and schema

- Every document is a **Spec**. Frontmatter `spec: <schema>` names its schema;
  the default is `contract` (today's milestone).
- A schema declares Spec-level `fields`, item collections (`items:`), and an
  optional `workflow` (`acdd start` requires one unless `--profile` is passed).
- Fields are validated by the existing `SchemaDef` engine that already
  validates proof evidence.

### Fixed core

| Level | Core (Rust) | Schema-defined |
|---|---|---|
| Spec | `id`, `title`, `spec`, `status`, `cancellation_reason`, `depends_on`, `linked_to`, `profile`, `closure`, `spec_deltas`, `goal` | everything else |
| Item | `ref`, `depends_on`, `tags`, `status`, `receipt` | everything else |

### Item collections

```yaml
items:
  tasks:          # collection: spec.tasks
    item: task    # optional; defaults to collection name ("### tasks: <ref>")
    fields: {...}
```

Parsed items are exposed as `spec.<collection>`. `- tasks:` keeps
executing `contract` tasks; running other items through gates arrives with
AC-005 (`mode: swarm` + `with: spec.<collection>`).

### Where schemas live

One mechanism: the `specs:` map of the resolved profile. Embedded profiles
carry `contract` and `memory`; workspace profiles add or patch entries through
`inherit`; an entry may reference a schema file with the same grammar and
resolver as `inherit` and frontmatter `profile:`. Schemas do not extend each
other.

### Validation order

`acdd validate <spec>`: core shape → schema fields → closed cross-Spec DAG.

### Spec deltas

`spec_deltas` in frontmatter declares an edit of an id-keyed list in another
Spec. An agent applies it in an ordinary gate; `spec.deltas_applied` verifies
the result.

### Named path sets

```yaml
paths:                                   # embedded base defaults
  docs:         ["docs/**", "AGENTS.md", "README.md"]
  adrs:         ["docs/adr/**"]
  instructions: ["**/AGENTS.md", "TESTS.md"]
  memories:     ["docs/memories/**"]

gates:
  - id: build
    context: [paths.instructions, paths.adrs, paths.memories, code.symbols]
  - id: polish
    scope: [paths.docs]
```

- Declared in `paths:`, referenced as `paths.<name>`; same form as `code.symbols`.
- `acdd.yaml` still owns what is indexed and discovered (`docs:`, `specs:` directories); profile `paths:` and `specs:` own gate scopes, context sets, and Spec schemas. Directories used by `context: [paths.<name>]` for link matching (such as `docs/memories`) belong in `acdd.yaml` `docs:`.
- In `scope:` a reference expands to globs.
- In `context:` the claim delivers related files: linked to the task scope by
  code-map doc edges, instruction files on the ancestor path, or named in the
  target. A set with no links at all is delivered whole. Ordered by path, cut
  by the ClaimResp byte budget.
- Projects with another layout override entries, for example
  `paths: {adrs: ["architecture/decisions/**"]}`.

### Memory

1. `memory_synthesis` is an embedded gate template at the end of a workflow,
   executed by one role.
2. The role reads rejections and decisions from the ordinary `ClaimResp`
   (`unresolved_findings`, `blackboard`).
3. The role edits `memory` Specs under `paths.memories`: one Spec per topic,
   one `### rule: <ref>` item per rule with `evidence` (blackboard message ids)
   and prose that links the code it applies to.
4. ACDD validates the edited Specs like any Spec. Zero changes is a clean pass.
5. Later gates listing `paths.memories` in `context:` receive the memory Specs
   whose links point at their task files.

Batch consolidation is an ordinary `--goal` run under an example
`memory-dream` profile reading `run_tombstones` summaries and archived specs.

> [!IMPORTANT]
> `docs/memories/` already holds free-form run retrospectives without
> `spec: memory`. Task 0 records the owner decision to move them out of
> `paths.memories` or convert them before the memory gate ships.

## Tasks in this milestone

### task: bootstrap-adr-and-seams

```yaml
task_ref: bootstrap-adr-and-seams
target: Accept ADR 0014, record the decision on existing docs/memories retrospectives, register the spec_schemas seam modules once, and freeze disjoint downstream scopes
subtasks:
- subtask_ref: accept-adr
  title: Mark ADR 0014 accepted and list it in the ADR index
- subtask_ref: memories-decision
  title: Record the owner decision for existing docs/memories retrospectives on the blackboard
- subtask_ref: seam-registration
  title: Register tests/acdd/spec_schemas/ with one module per downstream task and shared fixtures
contract_revision: 1
depends_on: []
scope:
- docs/adr/0014-declarative-spec-schemas-and-items.md
- docs/adr/README.md
- tests/acdd/mod.rs
- tests/acdd/spec_schemas/
- tests/acdd/fixtures/spec_schemas/
status: planned
```

### task: spec-core-and-contract-schema

```yaml
task_ref: spec-core-and-contract-schema
target: Split SpecDocument into the fixed core plus schema-validated fields, embed the contract schema, move the goal narrative to `goal:`, and replace doc_type == "contract" branches
subtasks:
- subtask_ref: core-split
  title: Core Spec and item structs with fields as validated values; TaskSpecYaml becomes the embedded contract task schema
- subtask_ref: goal-rename
  title: Store ephemeral --goal narrative in `goal:`
- subtask_ref: contract-branches
  title: Replace doc_type == "contract" checks with schema properties
contract_revision: 1
depends_on:
- bootstrap-adr-and-seams
scope:
- crates/acdd-state/src/spec.rs
- crates/acdd-state/src/usecases/
- src/state_bridge/docs_domain.rs
- src/engine/pipeline/
- src/format/roadmap.rs
- tests/acdd/spec_schemas/core.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/core.rs` — every live manifest in the repository
parses and validates identically (AC-01, AC-07).

### task: profile-specs-section

```yaml
task_ref: profile-specs-section
target: Add the `specs:` profile section with key-wise inherit merging and schema references through the existing profile reference resolver
subtasks:
- subtask_ref: specs-section
  title: Parse and merge `specs:` like roles and proofs
- subtask_ref: schema-references
  title: Resolve string entries via the profile reference resolver; fail closed when unresolved
contract_revision: 1
depends_on:
- bootstrap-adr-and-seams
scope:
- crates/acdd-state/src/profile/
- tests/acdd/spec_schemas/profile.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/profile.rs` (AC-02, AC-03).

### task: named-path-sets

```yaml
task_ref: named-path-sets
target: Add the `paths:` profile section, resolve `paths.<name>` in gate scope and context, replace docs.instructions and docs.adrs, and remove layout literals from docs context code and embedded gate scopes
subtasks:
- subtask_ref: paths-section
  title: Parse and merge `paths:`; embedded base defaults reproduce current behavior
- subtask_ref: scope-expansion
  title: Expand `paths.<name>` in gate `scope:`; embedded polish and documentation scopes use path sets
- subtask_ref: context-delivery
  title: Deliver related files of a path set in `context:` using existing doc-edge, ancestor, and target matching and ClaimResp budgets
- subtask_ref: lens-migration
  title: Replace docs.instructions and docs.adrs with redirect-hint validation errors; docs.invariants reads paths.docs
contract_revision: 1
depends_on:
- spec-core-and-contract-schema
- profile-specs-section
scope:
- crates/acdd-state/src/profile/
- crates/acdd-state/src/context/
- src/state_bridge/
- src/engine/guide.rs
- skills/acdd/references/acdd-profile-examples/
- tests/acdd/spec_schemas/paths.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/paths.rs` — the repository's own runs receive the
same instructions and ADRs as before; an overridden `paths.adrs` in a fixture
workspace delivers ADRs from the new location (AC-09, AC-10).

### task: named-items-and-validation

```yaml
task_ref: named-items-and-validation
target: Parse schema-declared item headings into spec collections and validate core, fields, and the cross-Spec DAG
subtasks:
- subtask_ref: item-parsing
  title: "Parse `### <item>: <ref>` blocks per schema"
- subtask_ref: item-collections
  title: "Expose parsed items as `spec.<collection>`; keep `- tasks:` for contract tasks"
- subtask_ref: validation-order
  title: Three-step validation with SPEC_FIELD_UNKNOWN and SPEC_ITEM_UNKNOWN
contract_revision: 1
depends_on:
- named-path-sets
scope:
- crates/acdd-state/src/spec.rs
- crates/acdd-state/src/profile/
- crates/acdd-state/src/validate.rs
- crates/acdd-state/src/usecases/
- tests/acdd/spec_schemas/items.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/items.rs` (AC-04, AC-05, AC-06).

### task: spec-deltas

```yaml
task_ref: spec-deltas
target: Add frontmatter `spec_deltas` and the deterministic `spec.deltas_applied` check
subtasks:
- subtask_ref: deltas-field
  title: Core `spec_deltas` field with target, field, added, modified, removed
- subtask_ref: deltas-check
  title: Verify added, modified, and removed ids in the target field
contract_revision: 1
depends_on:
- named-items-and-validation
scope:
- crates/acdd-state/src/spec.rs
- crates/acdd-state/src/profile/validate.rs
- crates/acdd-state/src/usecases/verify.rs
- src/state_bridge/
- tests/acdd/spec_schemas/deltas.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/deltas.rs` (AC-08).

### task: memory-schema-and-gate

```yaml
task_ref: memory-schema-and-gate
target: Embed the memory schema, the single-role memory_synthesis gate template, admit docs/memories in acdd.yaml, and ship the memory-dream example profile
subtasks:
- subtask_ref: memory-schema
  title: Embedded memory schema with topic and rule items carrying evidence
- subtask_ref: synthesis-gate
  title: memory_synthesis template reading ClaimResp findings; evidence ids validated against the run blackboard
- subtask_ref: acdd-yaml-memories
  title: Admit docs/memories in acdd.yaml docs so acdd-index builds code-map links for paths.memories
- subtask_ref: dream-profile
  title: memory-dream example profile over run tombstones and archived specs
contract_revision: 1
depends_on:
- spec-deltas
scope:
- acdd.yaml
- crates/acdd-core/src/ports.rs
- crates/acdd-state/src/profile/embedded/
- crates/acdd-state/src/usecases/verify.rs
- src/state_bridge/
- skills/acdd/references/acdd-profile-examples/
- tests/acdd/spec_schemas/memory.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/memory.rs` — a rejected review yields a memory
rule that the next gate touching the linked files receives through
`paths.memories`; zero findings is a clean pass (AC-11).

### task: docs-and-acceptance

```yaml
task_ref: docs-and-acceptance
target: Reconcile reference, architecture, skills, roadmap, and changelog with the delivered mechanisms, add the context table, and run the cross-surface acceptance seam
subtasks:
- subtask_ref: context-table
  title: Table in docs/reference/profiles.md listing every paths.* and code.* context entry with source, selection rule, and budget
- subtask_ref: docs
  title: Update profiles, tasks, cli references and architecture pages
- subtask_ref: acceptance
  title: End-to-end seam across contract, custom schema, overridden paths, and memory Specs
contract_revision: 1
depends_on:
- memory-schema-and-gate
scope:
- docs/reference/
- docs/architecture/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
- tests/acdd/spec_schemas/acceptance.rs
status: planned
```

Seam: `tests/acdd/spec_schemas/acceptance.rs` (AC-12).

## Negative boundaries

- No memory-specific parser, registry, status axis, evidence collector,
  reviewer gate, context lens, CLI command, or MCP tool.
- No schema-to-schema inheritance; no prompts inside Specs.
- No aliases for `docs.instructions` / `docs.adrs`; the old names fail with a
  redirect hint.
- No new spec schemas beyond `contract` and `memory` are embedded; projects
  declare their own.
- No vector store, embeddings, fuzzy retrieval, or model client.
- No change to proof authority, independent review, or Git delivery rules.
