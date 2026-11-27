---
doc_type: api
title: Workspace configuration
---

# Workspace configuration

`acdd.yaml` is optional — without it, ACDD scans the workspace root and excludes standard build and cache directories. Run `acdd init` at the repository root to generate a starter `acdd.yaml`, `.acdd/workflows/default.yaml`, and `.gitignore` rules.

- **`--config <PATH>` (`ACDD_CONFIG`)**: overrides the path to `acdd.yaml`.
- **`--state-db <PATH>` (`ACDD_STATE_DB`)**: overrides the path to `.acdd/state.sqlite`.
- **Strict schema validation**: unknown YAML keys fail closed during `acdd validate` and adapter loading.

## Indexing scope

```yaml
preset: full
roots:
  - src
docs:
  - docs
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3
workflows_dirs:
  - .acdd/workflows
state_db: .acdd/state.sqlite
retain_traces: 20
ignore:
  - target
  - node_modules
linked_workspaces:
  - name: shared-library
    path: ../shared-library
    enabled: true
    roots:
      - src
    docs:
      - docs
```

### Core scope keys

- **`roots`**: relative directories or file paths scanned for compiled language symbols and Markdown/MDX docs (scan roots control indexing only, not language import roots).
- **`docs`**: relative directories or Markdown/MDX files indexed into searchable documentation sections; non-document files inside `docs` stay out of the code index.
- **`ignore`**: literal file or directory basenames excluded from scanning (not glob patterns).
- **Schema strictness**: `AdapterFile`, `LinkedWorkspaceConfig`, and `ScannerLimits` enforce `#[serde(deny_unknown_fields)]`, rejecting unknown or removed keys fail-closed with `ACDD_WORKFLOW_SCHEMA_INVALID`.
- **Default exclusions**: `.git`, `.acdd`, `target`, `node_modules`, `.venv`, `__pycache__`, `build`, `dist`, `coverage`, `.cache`, `.next`, `.nuxt`, `.svelte-kit`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.gradle`, `.gitignore` rules, and symlinks.
- **Manifest discovery**: indexes package manifests inside admitted roots plus direct root manifests (e.g. `roots: [src]` still picks up root `Cargo.toml` or `package.json` up to `4 MiB`).

### Specification configurations (`specs:`)

`specs:` (`SpecConfig`) defines named folders for Git-tracked Markdown specifications (`milestones` and `plans` by default):

- **`path` / `paths`**: normalized relative directory (or confined globs) that must not overlap across configurations.
- **`prefixes`**: allowed ID prefixes for files in that configuration (e.g. `[AC]`, `[PLAN]`).
- **`digits`**: numeric ID zero-padding width (`1..=16`, default `3`, producing `AC-001-...md`).
- **`workflow`**: optional default `WorkflowDef` name or path applied when starting specs from this configuration (unknown or removed keys fail closed via `#[serde(deny_unknown_fields)]`).
- **Discovery**: `acdd draft <spec> <slug>` allocates the next free ID, and `acdd roadmap` scans all configured entries to compute cross-spec dependency waves.

### Linked workspaces (`linked_workspaces:`)

Federate sibling repositories into the same code and documentation graph:

- **`name`**: unique non-empty logical workspace identifier (`acdd validate` rejects empty or duplicate names).
- **`path`**: existing directory path (missing, non-directory, or symlink-escaped targets fail validation).
- **`enabled`**: boolean toggle (`true` by default when omitted; `false` skips the workspace).
- **`roots` / `docs` / `ignore`**: per-workspace scan scope; a linked entry does not inherit the target repo's own `acdd.yaml`.
- **Refresh behavior**: MCP reads auto-refresh when linked files change; CLI queries refresh when you run `acdd build` or `acdd delta`.

### Configuration digest isolation (`adapter.digest`)

Changing workflow, state, or output formatting settings does **not** trigger a cold rebuild of `.acdd/code-map.sqlite`:

- **Rebuilds the code index**: `roots`, `docs`, `ignore`, `sensitive`, `limits`, `linked_workspaces`, `owners`, `aliases`, `architecture`, and `adapter_version`.
- **Never rebuilds the code index**: `specs` (and its `workflow`, `prefixes`, `digits`), `workflows_dirs`, `state_db`, `retain_traces`, `modules`, `preset`, `response`, and `debug`.

### Architectural zones, rules, and complexity limits (`architecture:`)

Declare workspace-relative glob zones, static `ArchRuleSpec` boundary rules, and `ComplexityLimits` telemetry/invariant rules (`syntax.fact_budget_exceeded`, `syntax.intra_crate_type_homonym`) evaluated by `acdd lint`, `code_map_lint`, and `code.lint` (`#[serde(deny_unknown_fields)]`):

```yaml
architecture:
  unused_public_api: true
  complexity:
    severity: warn
    invariant: INV-BOUNDED-STRUCTURAL-COMPLEXITY
  zones:
    core: ["crates/acdd-core/src/**"]
    state: ["crates/acdd-state/src/**"]
  rules:
    - id: INV-CORE-LEAF
      invariant: INV-CORE-LEAF
      severity: error
      kind: forbid_dependency
      from: { zones: ["core"] }
      to: { zones: ["state"], external_symbols: ["rusqlite"] }
    - id: ARCH-ACYCLIC-CORE
      severity: error
      kind: forbid_cycles
      from: { zones: ["core"] }
      depth: 2
```

- **`architecture.unused_public_api`**: defaults to `false` (library-friendly) and enables `dead_tails.unused_public_api` when set to `true` (enabled in this repository's `acdd.yaml`).
- **`architecture.zones`**: named path-glob sets reusable across `acdd.yaml` rules and fenced `acdd-rule` blocks inside `docs/**` (zone names referenced in docs are validated during indexing and lint).
- **`architecture.rules`**: list of lint rules (`allow_only_dependencies`, `forbid_dependency`, `single_owner`, `boundary_entry`, `ast_pattern`, `forbid_cycles`, `must_implement`, `doc_integrity`).
- **`architecture.complexity`**: `ComplexityLimits` (`severity: warn`, optional `invariant: null`). AST `cognitive_complexity` and `max_nesting_depth` are recorded in `node.details` telemetry, while `syntax.fact_budget_exceeded` and `syntax.intra_crate_type_homonym` use `complexity.severity` and bind to `invariant` so `enforced_invariant_ids` marks it `[ENFORCED]`.

## Cold-build memory budget

Bound transient memory usage during source extraction under `limits`:

```yaml
limits:
  memory_budget_mb: 200
```

- **`limits.memory_budget_mb`**: positive integer in MiB controlling extraction batch size and SQLite cache without changing indexed graph facts.
- **`ACDD_MEMORY_BUDGET_MB`**: environment fallback used when YAML omits `memory_budget_mb` (does not alter `adapter.digest`).
- **Default**: when both are omitted, the builder selects an adaptive budget from available system RAM.

## Snapshot and commit binary/size limits

Optional candidate-file guards under `limits` (disabled when omitted):

```yaml
limits:
  max_snapshot_file_bytes: 16777216
  allowed_binary_extensions: [mo, png, jpg]
```

- **`limits.max_snapshot_file_bytes`**: positive byte ceiling (`>= 1`); snapshot or commit candidates above this size fail closed with `EXTERNAL_GIT_DRIFT`.
- **`limits.allowed_binary_extensions`**: case-insensitive list of allowed binary extensions (pass `[]` to reject all files containing NUL bytes in their first `8 KiB`).
- **Always-on safety guards**: Git submodule gitlinks (`160000`), nested `.git` directories, and symlinks are always rejected regardless of `limits`.

## Source-only linked libraries

Keep generated bundles out of linked-workspace indexes by listing literal folder basenames under each entry's `ignore`:

```yaml
linked_workspaces:
  - name: shared-library
    path: ../shared-library
    roots: [src, frontend, tests]
    docs: [docs]
    ignore: [vendor, vendors, dist, build]
```

- **`coverage_by_workspace`**: `code_map_overview` reports file and symbol counts separately for the primary workspace and each linked workspace.
- **`ignore` scope**: exclude directories like `dist` or `static` only when all of their contents are generated build artifacts rather than authored source contracts.

## State storage and module toggles

- **`state_db`**: SQLite path for runs and gates (defaults to `<primary_worktree>/.acdd/state.sqlite`; see [state, tasks & lifecycle](tasks.md)).
- **`retain_traces`**: number of run trace folders kept under `<primary_worktree>/.acdd/run/<run_id>/` (`>= 1`, default `20`).
- **Standalone index**: code and docs indexing work normally even if state execution is never used.

Toggle MCP and CLI subsystems and `DomainProvider` plugins via top-level `preset:` (or `modules:` flags/`preset: full | index | spec | state | minimal`):

```yaml
preset: full
modules:
  code: true
  docs: true
  spec: true
  state: true
  blackboard: true
  memory: false
  domains:
    code: true
    docs: true
```

| Preset (`preset:`) | Enabled Subsystems | Active CLI / MCP Surface |
| --- | --- | --- |
| `full` *(default)* | `code`, `docs`, `spec`, `state`, `blackboard` | All CLI commands and all 18 static MCP tools |
| `index` | `code`, `docs` | Code/docs CLI + 13 MCP tools (`code_map_*`, `ast_grep_search`, `get_doc`, `search_docs`, `acdd_read`, `acdd_guide`) |
| `spec` | `spec`, `docs` | Stateless CLI `acdd validate` / `roadmap` / `draft` + 4 MCP tools (`get_doc`, `search_docs`, `acdd_read`, `acdd_guide`) with zero `.acdd/state.sqlite` creation |
| `state` | `state`, `blackboard`, `spec` | State/spec CLI + 7 MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`, `acdd_read`, `acdd_guide`) |
| `minimal` | *(none)* | Core `acdd_read` (index-free text modes) and `acdd_guide` only |

- **`code: false`**: disables source indexing and rejects code-graph queries with `FEATURE_DISABLED`.
- **`docs: false`**: skips Markdown indexing and strips `get_doc`, `search_docs`, and `docs.*` hooks from built-in workflows.
- **`spec: false`**: disables `acdd validate`, `acdd roadmap`, and `acdd draft` (`FEATURE_DISABLED`).
- **`state: false`**: disables run/gate commands and physically disables `blackboard`.
- **Required-subsystem validation (`FEATURE_REQUIRED_BY_PRESET` vs. `DOMAIN_CAPABILITY_UNAVAILABLE`)**: `SubsystemRegistry::validate()` ([`subsystems.rs`](../../crates/acdd-core/src/subsystems.rs)) rejects contradictory enable/disable entries and embeds `FEATURE_REQUIRED_BY_PRESET: ACDD subsystem '<name>' is required but disabled` in the `ACDD_WORKFLOW_SCHEMA_INVALID` diagnostic message whenever a single `modules:` registry's `required` list demands a subsystem disabled by that same `preset:` / `modules:` block (either in `acdd.yaml` or workflow-locally). Separately, when a workflow's `modules.required` demands a subsystem disabled by the **workspace** `acdd.yaml` `preset:` / `modules:`, [`validate_workflow`](../../crates/acdd-state/src/workflow/validate.rs) emits structured diagnostic code `DOMAIN_CAPABILITY_UNAVAILABLE` (resolved workflows that satisfy all checks emit `WORKFLOW_VALID`).
- **Core tools**: `acdd_read` and `acdd_guide` remain active in every preset, including `minimal`.
- **`domains.<id>`**: enables or disables registered `DomainProvider` plugins (`true | false`).
- **Database collision guard**: code-map builds refuse to overwrite `state_db`, and state startup rejects code-map databases.

## MCP response policy

```yaml
response:
  detail: compact
  page_size: 12
  file_uris: false
  max_output_bytes: 98304
  source_context:
    enabled_by_default: false
    leading_lines: 5
    max_body_lines: 100
```

- **`detail`**: `compact` (default) or `full`.
- **`page_size`**: items per page (`1..=100`, default `12`).
- **`file_uris`**: emit workspace-confined `file:///` links in compact Markdown (`false` by default; `detail: full` or per-call `file_uris: true` opts in).
- **`max_output_bytes`**: serialized response cap (`1,024..=98,304`, default `98,304`); oversized pages trim whole tail items cleanly and advance `next_offset`.
- **`source_context.enabled_by_default`**: includes or omits source snippets on `code_map_inspect` / `code_map_explain` (`false` by default).
- **`source_context.leading_lines`**: context lines shown above a symbol (`0..=20`, default `5`).
- **`source_context.max_body_lines`**: preview line limit for `inspect` / `explain` (`1..=500`, default `100`).
- **`acdd_read` symbol bodies**: returns the full symbol when it fits `max_output_bytes`, otherwise pages cleanly on line boundaries.

## Declarative ACDD workflow and role configuration

Workflow gates are configured via **`WorkflowDef` (`version: 2`)** YAML files (see [ACDD Workflows Reference](workflows.md)).

### Loading precedence

1. **CLI / env override**: `--workflow <name|path[:sha256_prefix]>` or `ACDD_WORKFLOW`.
2. **Spec frontmatter**: `workflow: <name|path[:sha256_prefix]>` inside the Markdown spec.
3. **Spec default**: `specs.<name>.workflow` in `acdd.yaml`.
4. **Custom workflow folders**: named `<workflow>.yaml` files under `workflows_dirs` (default `[".acdd/workflows"]`).
5. **Workspace default**: `.acdd/workflows/default.yaml` at the repository root (falls back to built-in `code` when absent).
6. **Built-in workflows**: `base`, `code`, and `documentation` (plus copyable `audit`, `spec_planning`, and `hotfix` templates in `skills/acdd/references/acdd-workflow-examples/`; linked workspaces' workflows are ignored).

### Gate capabilities at a glance

- **`id` & `role`**: stable gate identifier (`[A-Za-z0-9_-]`) and assigned worker role (`architect`, `builder`, `independent_reviewer`, ...).
- **Execution & checks**: shell `command:`, plugin action `call: <domain>.<action>` (`with:`), and automated `checks: [<domain>.<check>]`.
- **Context & tools**: dynamic `context: [<domain>.<lens>]` slices and per-gate `tools` / `tools_add` / `tools_remove` allowlists.
- **Proofs**: built-in `none`, `command`, `call`, or `review` (`contours:`), plus custom JSON Schemas under `proofs:`.
- **Persistence (`save:`)**: `snapshot` checkpoints on leaf task gates (see [task snapshots](tasks.md#task-snapshots)), atomic `commit` receipts on delivery, `report` Markdown outputs, and terminal `archive` / `prune`.
- **Routing & guards**: `pass` / `fail` / `fail_drift` edges, `need` dependencies, `independent_from` worker separation, and `when` predicates.
- **SHA-256 pinning**: `<path>:<sha256_prefix>` (`>= 7` hex chars) locks parent/custom workflows and fails closed (`ACDD_WORKFLOW_SCHEMA_INVALID`) on unauthorized edits. See [ACDD Workflows Reference](workflows.md) for full schema details.

### Role and model configuration

Configure execution mode, model preferences, and optional CLI runners per role under `roles:`:

- **`mode`**: `subagent` (default) or `inline`.
- **`reuse_on_reject`**: boolean (`false` by default; `true` keeps the same builder session on review rework).
- **`models`**: ordered preference list (`'model:reasoning'`, `'model'`, or `{ model, reasoning? }`).
- **`guidance`**: coordinator dispatch note surfaced in `acdd next` / `acdd_state(action: "next")`.
- **`runner` / `prompt_template`**: optional shell command and prompt template used by headless `acdd run` (exports `ACDD_ROLE_MODE`, `ACDD_ROLE_MODELS`, `ACDD_MODEL`, and `ACDD_REASONING`).

```yaml
# .acdd/workflows/default.yaml
version: 2
inherit: code

roles:
  builder:
    models:
      - gemini-4-hydrogen-high
  independent_reviewer:
    mode: subagent
    reuse_on_reject: false
    models:
      - gemini-4-neon-xhigh
      - gemini-4-neon-high
    guidance: "Spawn an independent fresh reviewer on a high-reasoning model."
```

### Repository `.gitignore` and protected paths

Keep committed assets (`.acdd/workflows/**`, `.acdd/specs/**`, `.acdd/frameworks/**`) tracked in Git while ignoring runtime databases and logs (`acdd init` writes this block automatically):

```gitignore
/.worktrees/
!/.acdd/
!/.acdd/workflows/
!/.acdd/workflows/**
!/.acdd/specs/
!/.acdd/specs/**
!/.acdd/frameworks/
!/.acdd/frameworks/**
.acdd/*.sqlite*
.acdd/*.lock
.acdd/*.log
.acdd/*.json
.acdd/*.jsonl
.acdd/reports/
.acdd/run/
.acdd/tmp/
```

See [MCP tools](mcp-tools.md) for response paging and [limits and freshness](../runbooks/limits-and-freshness.md) for query ceilings.

## Sensitive reader paths

Block secret files from `acdd_read` / `acdd read` (both inputs and `output_file` exports) across primary and linked workspaces using gitignore-style patterns:

```yaml
sensitive:
  - '.env*'
  - 'secrets/**'
```
