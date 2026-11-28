---
title: "ADR 0014: Universal Specs in acdd.yaml, Named Items, and Workflows (.acdd/workflows/)"
doc_type: adr
status: accepted
date: 2026-11-27
---

# ADR 0014: Universal Specs in `acdd.yaml`, Named Items, and Workflows (`.acdd/workflows/`)

## Status

Accepted.

## Context

[ADR 0003](0003-unified-profile-graph-and-state-machine.md) made the gate graph declarative: `GateDef` composes roles, commands, `call` actions, `checks`, `when` predicates, and typed `proof` schemas without knowing what a milestone is. Three things created friction:

- **Spec shape.** `SpecFrontmatter` and `TaskSpecYaml` fix one field set (`acceptance_criteria`, `invariants`, `scope`, `tags`, `subtasks`, …) under `deny_unknown_fields`. `doc_type` is a free string defaulting to `contract`, and consumers branch on `doc_type == "contract"`. The only nested block is `### task: <ref>`.
- **Repository layout & context transparency.** The docs context lenses in [`docs_domain/mod.rs`](../../src/state_bridge/docs_domain/mod.rs) hardcode `docs/adr`, `docs/**`, `AGENTS.md`, `TESTS.md`, and `tests/**` as literals, and embedded workflows hardcode write scopes such as `docs/**`, `skills/**`, `AGENTS.md`, `README.md`. Relevance ranking for injected docs was also undocumented outside code.
- **Terminology & crate coupling.** Calling gate pipelines `profile` collided with `LanguageProfile`, `SubsystemPreset`, and `cargo --profile`, while `spec.rs` / static roadmap lived inside `acdd-state` unreachable from `acdd-index`. [AR-006](../milestones/archive/AR-006-acdd-spec-crate-and-workflow-cutover.md) extracts `crates/acdd-spec`, renames top-level `profile:` to `preset:` (`index` / `spec` / `state` / `full` / `minimal`), and cuts over `.acdd/workflows/default.yaml`, `workflows_dirs:`, `workflow:`, `--workflow`, `--rebind-workflow`, `--spec`, `SpecConfig`, `WorkflowDef`, and `ACDD_WORKFLOW_SCHEMA_INVALID` before [AC-006](../milestones/AC-006-universal-repository-memory-synthesis.md) lands universal specs on top.

## Decision

### 1. Three clear pillars: `doc`, `spec`, `workflow` (over `index` / `spec` / `state`)

- **`doc`** (`acdd.yaml` `docs:`): Documentation indexed and linked to code symbols by `acdd-index`.
- **`spec`** (`acdd.yaml` `specs:` + `.acdd/specs/*.yaml` via `specs_dirs:`, owned by `crates/acdd-spec`): Single registry of Spec definitions, paths, numbering (`prefixes`/`digits`), default `workflow:`, `roadmap: true | false`, `fields`, and `items`.
- **`workflow`** (`.acdd/workflows/default.yaml` + `.acdd/workflows/*.yaml` via `workflows_dirs:`, owned by `crates/acdd-state/src/workflow/`): Gate execution pipelines (`inherit`, `roles`/`agents`, `paths`, `commands`, `proofs`, `guidance`, `gates`). Embedded workflows stay minimal (`base`, `code`, `documentation`); `audit`, `spec_planning`, and `hotfix` live as ready-to-copy templates under `skills/acdd/references/acdd-workflow-examples/`.

### 2. Single `acdd.yaml` `specs:` registry

`acdd.yaml` `specs:` (`SpecConfig`) is the **single owner** of every Spec type in the workspace:

- **Built-in bases (`contract`, `memory`)** ship embedded in `crates/acdd-spec` (`contract` is the default base when omitted, so existing `acdd.yaml` `specs.milestones` / `specs.plans` entries and existing manifests without frontmatter `spec:` parse and run unchanged).
- **Inline or external spec files (`.acdd/specs/*.yaml`)**: An entry under `acdd.yaml` `specs.<name>` declares `fields`, `items`, `base`, `workflow`, `roadmap`, `path`/`paths`, `prefixes`, `digits` inline, or loads an external file via `ref:` searched in `specs_dirs:` (default `[".acdd/specs"]`) through one shared confined `<name> | <path>[:<sha256_prefix>]` resolver in `crates/acdd-core/src/config/` (reused identically by `workflows_dirs:`).
- **Workflow resolution (`workflow:`)**: Running `acdd start [SPEC ...]` runs **one** workflow per `Run`, resolved from CLI `--workflow` > primary spec frontmatter `workflow:` > `specs.<name>.workflow` > `.acdd/workflows/default.yaml` (or embedded `code`). Pure data Specs (such as `memory` or `proposal`) omit `workflow:`.
- **Explicit `roadmap: true | false`**: Controls default inclusion in `acdd roadmap` (`true` on `contract`, `false` on `memory`, independent of `workflow:`).
- **Ephemeral `--goal` rename**: Narrative `--goal` text moves from frontmatter `spec:` to `goal:`; frontmatter `spec: <name>` selects the Spec type (defaulting to the owning `specs.<name>` matched by path, or `contract`).

```yaml
# acdd.yaml
preset: full                             # full | index | spec | state | minimal
specs_dirs:
  - .acdd/specs
workflows_dirs:
  - .acdd/workflows

specs:
  contract:                              # inherits built-in contract (items.tasks -> "### task: <ref>")
    path: docs/milestones
    prefixes: [P, R, B, S, W, F]
    digits: 3
    workflow: code
    roadmap: true
    fields:
      services: { type: array, items: { type: string } }
      priority: { type: string, enum: [high, medium, low] }

  commerce:                              # second directory reusing contract shape with its own prefix
    base: contract
    path: extensions/commerce/docs/milestones
    prefixes: [EC]
    digits: 3

  proposal:                              # loaded from .acdd/specs/proposal.yaml (or inline)
    ref: proposal
    path: docs/plans
    roadmap: false

  memory:                                # inherits built-in memory (items.rules -> "### rule: <ref>")
    path: docs/memories
    roadmap: false

roadmap:                                 # workspace CLI display settings alongside response:
  badges: [services, priority, readiness]
  group_by: wave                         # wave | <field> | spec
  color: auto                            # auto | always | never (NO_COLOR honored)
```

External spec file example (`.acdd/specs/research.yaml`):

```yaml
# .acdd/specs/research.yaml
workflow: research                       # optional default workflow
roadmap: false
fields:
  type: object
  required: [question]
  properties:
    question: { type: string }
items:
  probes:                                # collection: spec.probes -> "### probe: <ref>"
    item: probe
    fields:
      type: object
      required: [method]
      properties:
        method: { type: string }
  findings:                              # item omitted -> "### findings: <ref>"
    fields:
      type: object
      required: [evidence]
      properties:
        evidence: { type: array, items: { type: string } }
```

Specs do not form arbitrary multi-level inheritance trees: `base:` permits only a single-hop reference to an embedded base (`contract` or `memory`) or a named `ref:` file, with local overrides in `acdd.yaml`.

### 3. Fixed core, schema-defined rest

| Level | Core fields (Rust) | Everything else (`SchemaDef`) |
|---|---|---|
| Spec | `id`, `title`, `spec`, `status`, `cancellation_reason`, `depends_on`, `linked_to`, `workflow`, `closure`, `spec_deltas`, `goal` | validated by `specs.<name>.fields` |
| Item | `ref`, `depends_on`, `tags`, `status`, `receipt` | validated by `specs.<name>.items.<collection>.fields` |

### 4. Named items (`### <item>: <ref>`) and `spec.<collection>`

Each key under `items:` declares a collection exposed as `spec.<collection>` (e.g., `spec.tasks`, `spec.rules`, `spec.probes`). Its optional `item:` sets the singular Markdown heading token (`### <item>: <ref>`, defaulting to the collection key). Built-in `contract` declares `tasks: { item: task, fields: ... }`, preserving every existing `### task:` block, `task_ref`, and `- tasks:` gate execution unchanged. Generalized execution of other collections (`with: spec.<collection>`) belongs to [AC-005](../milestones/AC-005-unified-swarm-gates-and-peer-board-coordination.md).

### 5. `0..N` attached input specs & `0..M` validated output specs per run

- **Input specs (`0..N`)**: `acdd start [SPEC ...] [--workflow <name>] [--goal "..."]` attaches zero, one, or multiple registered Specs to a single `Run` executing **one** workflow. The first spec (when supplied) acts as primary (determining default `workflow:` if `--workflow` is omitted). All attached specs are persisted on the `Run` (`attached_specs`) and injected into `ClaimResp.contract.attached_specs[]` as compact budgeted descriptors (`spec_id`, `spec_type`, `title`, `path`, key `fields`, and `items` summaries; full text readable via `acdd_read`). No separate `inputs:` DSL is required on workflows or milestones.
- **Output specs (`0..M`)**: Any gate writing specs declares `scope: [specs.<name>]` (or multiple `specs.*` entries) and validates them through existing `checks: [docs.specs]`. When a gate MUST produce at least one valid document of a specific spec type, it passes `check: docs.specs` with `with: { spec: <name>, required: true }` (or `specs: [...]`); omitting `required: true` permits zero edits while still validating any created/modified spec files.

### 6. Three-step validation order

`acdd validate <spec>` fails closed in this order using `acdd.yaml` `specs:` (independent of which workflow is active):

1. **Core shape**: required core fields, `ref` uniqueness, `depends_on` syntax (`ITEM_DEPENDENCY_UNKNOWN`).
2. **Schema fields & items**: unknown, mistyped, or missing fields at Spec and item level via `SchemaDef` (`SPEC_FIELD_UNKNOWN`, with edit-distance hint); block headings whose `<item>` no collection declares (`SPEC_ITEM_UNKNOWN`).
3. **Closed DAG**: no cycles; every reference resolves, including cross-Spec item references `RES-001#probe-a`.

### 7. `spec_deltas`: one Spec declares changes to another

A spec delta declares an edit of an id-keyed list inside another Spec:

```yaml
spec_deltas:
  - target: API-WORKER
    field: endpoints
    added: [CancelSession]
    modified: { Run: { previously: "blocking", now: "streaming" } }
    removed: [LegacyRun]
```

An agent applies the deltas in an ordinary gate under workflow guidance. The built-in check `spec.deltas_applied` verifies deterministically that every `added` id exists in `target.field`, every `removed` id is absent, and every `modified` id carries its `now` text.

### 8. Explicit `specs.<name>` and `paths.<name>` in `scope:` / `context:` + 4-tier relevance ranking

No hidden magic bridges `specs:` to `paths:`:

- **`specs.<name>`** directly references a Spec registered in `acdd.yaml` `specs:`:
  - In `scope: [specs.memories]` — authorizes writing strictly inside that Spec's configured paths.
  - In `context: [specs.memories]` — delivers relevant items/rules from that Spec into Tier 6 `ClaimResp.context_lenses`, ranked by the 4-tier deterministic relevance policy below.
- **`paths.<name>`** is declared in workflows (embedded `base` provides defaults; workspace workflows override via `inherit`) strictly for **non-spec** documentation globs:
  ```yaml
  paths:
    docs:         ["docs/**", "AGENTS.md", "README.md"]
    adrs:         ["docs/adr/**"]
    instructions: ["**/AGENTS.md", "TESTS.md"]
  ```
  `docs.instructions` and `docs.adrs` are replaced by `paths.instructions` and `paths.adrs` (legacy names fail validation with a redirect hint); `docs.invariants` reads `paths.docs`; unknown `specs.<name>` or `paths.<name>` fails validation (`ACDD_WORKFLOW_SCHEMA_INVALID`), and an empty matched set emits a warning.
- **Deterministic 4-tier relevance ranking** (co-documented in [ADR 0006](0006-budgeted-gate-context-and-dual-surface-parity.md) and `docs/reference/workflows.md`, sliced top-down by `max_bytes` via `budget_scored_items`):
  1. **Direct ID/title citation** in the current task `target` or owning Spec body (highest priority).
  2. **Explicit `scope` / `owners` overlap** (`scopes_overlap` between item `scope` / spec `owners` and task `admitted_scope`).
  3. **Indexed code-map graph edges** (`documents`, `references_doc`, `cites_doc`, `governs`, `governed_by` in `code-map.sqlite` linking the document to any path/symbol in `admitted_scope`).
  4. **Global fallback rules** (`scope: ["**"]` or unlinked repo-wide documents — lowest priority, dropped first when budget is tight).

### 9. Clean separation of `acdd.yaml` vs. `.acdd/workflows/` (zero duplicate keys)

| File / Dir | Section | Single Responsibility |
|---|---|---|
| `acdd.yaml` | `preset:` | Active MCP/CLI subsystem preset (`full`, `index`, `spec`, `state`, `minimal`) |
| `acdd.yaml` | `docs:` | Directories `acdd-index` scans and links to code symbols (`code-map.sqlite` doc edges, including planned specs for blast-radius inspection) |
| `acdd.yaml` | `specs:` (+ `specs_dirs:`) | Single owner of Spec definitions, paths, numbering (`prefixes`/`digits`), default `workflow:`, `roadmap: true \| false`, `fields`, and `items` |
| `acdd.yaml` | `roadmap:` | Workspace CLI roadmap presentation (`badges`, `group_by`, `color`) alongside `response:` |
| `acdd.yaml` | `workflows_dirs:` | Search directories for custom workflows (default `[".acdd/workflows"]`) |
| `.acdd/workflows/` | `default.yaml`, `*.yaml` | Execution pipelines (`inherit`, `paths`, `agents`/`roles`, `commands`, `proofs`, `guidance`, `gates`; zero `specs:` or `roadmap:` keys) |

### 10. Repository Memory Tracking and Retrospective Path Alignment

- **Tracked memory specs vs. gitignored retrospectives**: `docs/memories/` is tracked in Git for durable `spec: memory` rule specs (`specs.memories`), while transient milestone telemetry retrospectives live under gitignored `docs/memories/retrospectives/<spec.name>.md` (`.gitignore` narrowed from `/docs/memories/` to `/docs/memories/retrospectives/` in Wave 5, with `.acdd/workflows/default.yaml` `coordinator` / `repo.memory_synthesis` guidance reconciled under `polish` or explicit authorization prior to `handoff` so `handoff` never creates an untracked file directly under tracked `docs/memories/`).

## Consequences

- Clean, self-explanatory vocabulary (`doc`, `spec`, `workflow`) over three workspace layers (`index`, `spec`, `state`) and symmetric `.acdd/{specs,workflows,frameworks}/` layout.
- Zero duplicate `specs:` or `roadmap:` keys across `acdd.yaml` and workflow files, and zero hidden `specs → paths` magic.
- `acdd validate`, `acdd roadmap`, `acdd draft`, and `acdd-index` read one authoritative spec registry (`crates/acdd-spec` + `acdd.yaml` `specs:`) regardless of which workflow is selected.
- Any workflow can accept `0..N` attached input specs (`acdd start [SPEC ...]`) and validate `0..M` output specs (`docs.specs`), while multi-workflow chaining uses pause-and-call child runs (`parent_run_id` / `waiting_on_run_id` in [AC-008](../milestones/AC-008-always-on-reactive-triggers-and-deferred-approvals.md)) rather than nested workflow YAML merging.
- Repository memory ([AC-006](../milestones/AC-006-universal-repository-memory-synthesis.md)) is the built-in `memory` Spec (`items.rules`) written by the single-role `memory_synthesis` gate template (`scope: [specs.memories]`) and delivered via `context: [specs.memories]`.
