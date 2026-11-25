---
title: "ADR 0014: Declarative Spec Schemas, Items, and Named Path Sets"
doc_type: adr
status: proposed
---

# ADR 0014: Declarative Spec Schemas, Items, and Named Path Sets

> [!NOTE]
> Status: **Proposed**. Not yet admitted; the index in [README](README.md) lists accepted decisions only.

## Context

[ADR 0003](0003-unified-profile-graph-and-state-machine.md) made the gate graph declarative: `GateDef` composes roles, commands, `call` actions, `checks`, `when` predicates, and typed `proof` schemas without knowing what a milestone is. Two things stayed hardcoded:

- **Spec shape.** `SpecFrontmatter` and `TaskSpecYaml` in [`spec.rs`](../../crates/acdd-state/src/spec.rs) fix one field set (`acceptance_criteria`, `invariants`, `scope`, `tags`, `subtasks`, …) under `deny_unknown_fields`. `doc_type` is a free string defaulting to `contract`, and consumers branch on `doc_type == "contract"`. The only nested block is `### task: <ref>`.
- **Repository layout.** The docs context lenses in [`docs_domain.rs`](../../src/state_bridge/docs_domain.rs) read `docs/adr`, `docs/**`, `AGENTS.md`, `TESTS.md`, and `tests/**` as literals, and embedded profiles hardcode write scopes such as the `polish` gate's `docs/**`, `skills/**`, `AGENTS.md`, `README.md`. A project with another layout silently receives empty context.

## Decision

### 1. Every document is a Spec; `spec:` names its schema

```yaml
---
id: MEM-STATE-MACHINE
title: State machine pitfalls
spec: memory
---
```

`spec:` defaults to `contract`, the built-in schema equal to today's milestone shape, so existing manifests parse unchanged. The narrative frontmatter field `spec:` used by ephemeral `--goal` specs moves to `goal:`.

### 2. Schemas are a profile section

A schema declares frontmatter `fields`, item collections, and an optional `workflow` a Spec of that schema runs. Pure data Specs (such as `memory`) omit `workflow`; `acdd start <spec>` requires a resolved `workflow` (or `--profile`). Each key under `items:` names a collection; its optional `item:` names one element (defaulting to the collection key), used in the block heading `### <item>: <ref>`. The embedded `contract` schema declares `tasks: {item: task}`, matching the existing `### task:` blocks, `task_ref`, and `- tasks:`.

```yaml
specs:
  memory:
    fields:                       # SchemaDef (same JSON-Schema subset as proofs)
      type: object
      required: [topic]
      properties:
        topic: {type: string}
    items:
      rules:                      # collection: spec.rules
        item: rule                # one element: "### rule: <ref>"
        fields:
          type: object
          required: [evidence]
          properties:
            evidence: {type: array, items: {type: string}}
  research:
    fields:
      type: object
      required: [question]
      properties:
        question: {type: string}
    items:
      probes:                     # collection: spec.probes
        item: probe               # one element: "### probe: <ref>"
        fields:
          type: object
          required: [method]
          properties:
            method: {type: string}
      findings:                   # item omitted -> "### findings: <ref>"
        fields:
          type: object
          required: [evidence]
          properties:
            evidence: {type: array, items: {type: string}}
    workflow: research            # gate graph started by `acdd start <spec>`
```

### 3. Fixed core, schema-defined rest

| Level | Core fields (Rust) | Everything else |
|---|---|---|
| Spec | `id`, `title`, `spec`, `status`, `cancellation_reason`, `depends_on`, `linked_to`, `profile`, `closure`, `spec_deltas`, `goal` | validated by `specs.<name>.fields` |
| Item | `ref`, `depends_on`, `tags`, `status`, `receipt` | validated by `specs.<name>.items.<collection>.fields` |

`ClaimResp.contract` carries the Spec and item fields as today; `when:` keeps evaluating item tags (`WhenExpr`), while field expressions belong to [AC-005](../milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md).

### 4. Items are spec collections

Parsed items are exposed as `spec.<collection>` (for example `spec.tasks`, `spec.rules`). The existing `- tasks:` block keeps executing `contract` tasks unchanged. Executing items of any other schema through gates belongs to [AC-005](../milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md), whose `mode: swarm` + `with:` consumes `spec.<collection>` and replaces `- tasks:`; this ADR adds no gate block.

### 5. Validation

`acdd validate <spec>` fails closed in this order:

1. **Core shape**: required core fields, `ref` uniqueness, `depends_on` syntax (`ITEM_DEPENDENCY_UNKNOWN`).
2. **Schema fields**: unknown, mistyped, or missing fields at Spec and item level (`SPEC_FIELD_UNKNOWN`, with the existing edit-distance hint); block headings whose `<item>` no collection declares (`SPEC_ITEM_UNKNOWN`).
3. **Closed DAG**: no cycles; every reference resolves, including cross-Spec item references `RES-001#probe-a`.

### 6. `spec_deltas`: one Spec declares changes to another

A spec delta is a declared edit of an id-keyed list inside another Spec: which Spec, which list field, and which entries are added, rewritten, or removed.

```yaml
spec_deltas:
  - target: API-WORKER
    field: endpoints
    added: [CancelSession]
    modified: {Run: {previously: "blocking", now: "streaming"}}
    removed: [LegacyRun]
```

An agent applies the deltas in an ordinary gate under profile guidance. The built-in check `spec.deltas_applied` verifies deterministically that every `added` id exists in `target.field`, every `removed` id is absent, and every `modified` id carries the `now` text.

### 7. Where schemas come from

A Spec runs under one resolved profile (embedded `code`, `.acdd/profile.yaml`, or frontmatter `profile:`); its `spec:` value is looked up in that profile's merged `specs:` map.

- **Embedded profiles** carry built-in schemas (`contract` in `base`, `memory`) and the gate graphs that consume them.
- **Workspace profiles** add or patch entries; `specs:` merges by key through the existing `inherit` chain, like `roles` and `proofs`.
- **Schema files** hold one schema each and attach by reference with the same grammar and resolver as `inherit` and frontmatter `profile:` (`<name> | <path>[:<sha256_prefix>]`, looked up in `profiles_dirs` from `acdd.yaml` with confinement checks):

  ```yaml
  inherit: code
  specs:
    research: research              # resolves <profiles_dir>/research.yaml
    api: .acdd/schemas/api.yaml:3f9a
  ```

Schemas do not extend each other.

### 8. Named path sets: `paths:`

Repository layout is a profile section. Embedded `base` declares the defaults that reproduce current behavior; projects override entries through `inherit`:

```yaml
paths:
  docs:         ["docs/**", "AGENTS.md", "README.md"]
  adrs:         ["docs/adr/**"]
  instructions: ["**/AGENTS.md", "TESTS.md"]
  memories:     ["docs/memories/**"]
```

A path set is referenced as `paths.<name>`, the same `<section>.<name>` form as built-in `code.symbols`:

```yaml
gates:
  - id: build
    context: [paths.instructions, paths.adrs, paths.memories, code.symbols]
  - id: polish
    scope: [paths.docs]
```

- **In `scope:`** the reference expands to its globs.
- **In `context:`** the claim delivers files of the set that relate to the claimed task: files linked to the task's scope through code-map edges (`documents`, `references_doc`, `cites_doc`, `governs`, `governed_by`), instruction files on the ancestor path of the task's scope, and files named in the task target. When no file in the set has any code-map link, the whole set is delivered. Results are ordered by path and cut by the existing `ClaimResp` byte budget.
- Built-in lenses that extract content rather than files (`docs.invariants`) read from `paths.docs`.
- An unknown `paths.<name>` fails `acdd validate`; a set that matches no file is reported as a warning.

`docs.instructions` and `docs.adrs` are replaced by `paths.instructions` and `paths.adrs`; validation reports the old names with a redirect hint.

### 9. Discovery (`acdd.yaml`) vs profile policy

`acdd.yaml` and the gate profile split file discovery from validation and execution semantics:

| Owner | Section | Responsibility |
|---|---|---|
| `acdd.yaml` | `docs:` | Which documentation roots `acdd-index` scans and indexes (creating code-map doc edges) |
| `acdd.yaml` | `specs:` | Where spec collections live on disk (`path`, `prefixes`, `digits`) for discovery and `acdd roadmap` |
| Profile | `specs:` | Which schema validates a Spec's frontmatter, items, and workflow (`spec: <schema>`) |
| Profile | `paths:` | Named glob sets used by gates (`scope: [paths.<name>]`, `context: [paths.<name>]`) |

When a profile's `paths.<name>` is used in `context:` to deliver link-matched files (such as `paths.adrs` or `paths.memories`), its directory must be admitted in `acdd.yaml` (`docs:` or `specs:`) so `acdd-index` extracts its code-map links. Repository memories (`docs/memories`) therefore enter `acdd.yaml` `docs:` when a project enables memory Specs; `acdd validate` checks every Spec found under `acdd.yaml` `specs:` or matching `paths.memories` against its declared schema.

## Consequences

- New Spec shapes and workflows need profile YAML only.
- Projects with another layout override `paths:` instead of receiving empty context.
- `doc_type == "contract"` branches become schema properties (for example "has items with `scope`").
- `SpecDocument` splits into a core struct plus `fields: Value` per Spec and item; `TaskSpecYaml` becomes the embedded `contract.task` schema.
- `roadmap` builds one DAG across schemas through core `depends_on`.
- Migration: narrative `spec:` becomes `goal:` in `--goal` materialization; profiles rename `docs.instructions` / `docs.adrs`; no milestone manifest changes.
- Repository memory ([AC-006](../milestones/AC-006-universal-repository-memory-synthesis.md)) is the `memory` schema:
  - Writing: the single-role `memory_synthesis` gate at the end of a run edits `memory` Specs under `paths.memories`; ACDD validates them like any Spec.
  - Reading: a gate listing `paths.memories` in `context:` receives memory Specs that link to its task's files.
