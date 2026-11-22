---
doc_type: api
title: Workspace configuration
---

# Workspace configuration

`acdd.yaml` is optional. Without it, ACDD scans the workspace root and excludes
common build and cache directories. `acdd init` writes a starter repository
configuration. Put the adapter in the workspace root; CLI flags `--config`
(`ACDD_CONFIG`) and `--state-db` (`ACDD_STATE_DB`) override defaults cleanly.

## Indexing scope

```yaml
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
profiles_dirs:
  - .acdd/profiles
  - docs/workflows
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

`roots` and `docs` are literal, relative directories or paths inside the
workspace. `specs:` declares named specification collections (`milestones` and
`plans` by default), each configuring `path` or `paths` (including confined globs),
optional explicit `profile` override, allowed `prefixes`, and numeric ID zero-padding
`digits` (`1..=16`, default `3`). `acdd roadmap` discovers specs across every
configured collection for cross-collection dependency waves and drill-down.
`specs.<name>.path` entries must be normalized relative paths that do not overlap
by path components. `AdapterFile`, `LinkedWorkspaceConfig`, and `ScannerLimits`
enforce `#[serde(deny_unknown_fields)]`, and `acdd_core::config::load_adapter`
rejects legacy `0.5.x` top-level keys (`milestones`, `plans`, `tasks_db`,
`workflows_dir`, `milestone_prefix`, `plan_prefix`, `task_repository`,
`task_project`, `acdd_profile`) fail-closed with `CONFIG_MIGRATION_REQUIRED`
(see [migration-0.6.md](../runbooks/migration-0.6.md)). Markdown and MDX in
admitted documentation locations, including milestone and plan documents, remain
searchable and available for graph navigation. Non-document files in those
directories stay outside the source index. `ignore` matches file or directory
basenames. The scanner admits compiled source extensions and Markdown/MDX within
these roots; scan roots do not define import or package roots. For
`linked_workspaces`, `load_adapter_strict` / `load_adapter_report` (used by
`acdd validate`) fail closed on empty or duplicate `name` values and missing,
non-directory, or symlink-escaped `path` targets; an entry with `enabled: false`
is excluded, and omission of `enabled` means true.

### Configuration digest isolation (`adapter.digest`)

Changing operational, presentation, or state settings does **not** invalidate
the code/docs index (`code.sqlite`). When computing `adapter.digest`, ACDD
explicitly excludes:
- `response`
- `DEBUG` / `debug`
- `modules` and `profile`
- `state_db`
- `retain_traces`
- `profiles_dirs`
- `specs` (and non-path spec metadata such as `profile`, `prefixes`, `digits`)

Only source/docs indexing inputs (`roots`, `docs`, `ignore`, `sensitive`,
`limits`, `linked_workspaces`, `owners`, `aliases`, `architecture`,
`adapter_version`) affect `adapter.digest`. Changing `state_db`, `retain_traces`,
`profiles_dirs`, `modules`, `response`, or `specs` never triggers a cold
code-map rebuild.

### Architectural zones and rules (`architecture:`)

`acdd.yaml` accepts an optional `architecture:` block (`#[serde(deny_unknown_fields)]`)
declaring named workspace-relative glob zones and static `ArchRuleSpec` rules
evaluated by `acdd lint`, `code_map_lint`, and `code.lint`:

```yaml
architecture:
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
```

Zone names referenced by `acdd-rule` blocks inside `docs/**` are validated against
`architecture.zones` during indexing and lint; omitting `architecture:` preserves
existing `adapter.digest` values.

The default basename exclusions cover `.git`, `.acdd`, `target`, `node_modules`, `.venv`, `__pycache__`, `build`, `dist`, `coverage`, `.cache`, `.next`, `.nuxt`, `.svelte-kit`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, and `.gradle`. The scanner also honors `.gitignore` and skips symbolic links. Add project-specific generated directories with `ignore`; authored source under an excluded basename can be scoped explicitly with a different directory name.

The index records linked paths under their configured workspace names. Changing source scope or linked-workspace settings causes the MCP server to rebuild or update its owned index on the next database read. CLI readers automatically build a missing or schema-incompatible index, but do not refresh existing indexed files after source changes; run `build` or `delta` to update CLI results.

Dependency manifest discovery combines scanner-admitted files with supported
manifest files directly in the primary and enabled linked workspace roots.
These direct-root files participate even when the source roots are narrower.
Nested manifests outside admitted roots do not participate: with `roots: [src]`,
a root `requirements.txt` is included, while `unindexed/requirements.txt` is
excluded. Discovery preserves ignore and symlink guards and the 4 MiB manifest
read limit. Cold build, delta, and MCP freshness use the same selected paths;
MCP checks changes after its normal freshness TTL.

## Cold-build memory budget

Set a transient source-extraction budget in MiB under `limits`:

```yaml
limits:
  memory_budget_mb: 200
```

`memory_budget_mb` must be a positive integer whose MiB-to-byte conversion fits in `u64`. When omitted, `ACDD_MEMORY_BUDGET_MB` supplies the same setting; when both are omitted, the builder chooses an adaptive budget from available memory. The YAML value takes precedence over the environment. The budget controls extraction batch sizing and the SQLite cache. Changing the budget preserves extracted facts and per-domain commitments, but an explicit YAML value is part of the workspace configuration fingerprint and can change the overall output root. An environment-only override is transient and leaves the adapter policy fingerprint unchanged. A single admitted file can exceed the budget because the builder must hold that file while parsing it, but source memory remains bounded by the configured batch and file-size limit.

## Repository documentation admission

This repository lists current documentation files and directories explicitly in
`docs`: governance, navigation, roadmap, ADRs, architecture, reference,
runbooks, testing, and the milestone and plan trees configured separately in
`milestones` and `plans`. Those Markdown and MDX contracts are searchable too;
this indexing scope does not change task-discovery or authority rules.

## Source-only linked libraries

Set `ignore` inside each linked entry to exclude that library's vendor directories and build outputs. A linked entry does not inherit the target library's `acdd.yaml` settings. Keep the same roots and exclusions in the library's own adapter and its consumers.

```yaml
linked_workspaces:
  - name: shared-library
    path: ../shared-library
    roots: [src, frontend, tests]
    docs: [docs]
    ignore: [vendor, vendors, dist, build]
```

`code_map_overview` with `coverage_by_workspace` reports counts from the persisted primary and linked-workspace identities. Reopening the index retains those logical names and separates linked coverage from the primary workspace.

Ignore names are literal basenames, not glob patterns. Add a directory such as `static` only when the library builds all of its contents from sources retained elsewhere. Keep authored host assets and source-level type contracts. Excluded bundles neither enter the index nor trigger source refresh when rebuilt.

## State storage and module toggles

State operations read `acdd.yaml`; an omitted `state_db` defaults to
`.acdd/state.sqlite`, resolved against the **primary worktree root** (discovered
via `git rev-parse --path-format=absolute --git-common-dir`) or the workspace
root in non-Git workspaces. CLI `--state-db` / `ACDD_STATE_DB` overrides
`state_db`. `retain_traces` controls how many run trace directories under
`<primary_worktree>/.acdd/run/<run_id>/traces.jsonl` are retained across cleanup
(default `20`, must be `>= 1`). See [state, tasks & lifecycle](tasks.md) for
storage tables, advisory locks under `<git_common_dir>/acdd/`, candidate verification, and
recovery. Code indexing works without state storage configuration.

`modules:` configures MCP subsystem and `DomainProvider` plugin availability:

```yaml
modules:
  code: true
  docs: true
  state: true
  blackboard: true
  memory: false
  domains:
    code: true
    docs: true
```

Or using preset form (`profile: full | navigator | orchestrator | minimal` with
optional `enabled`, `disabled`, `required`, and `domains` maps). Module flags
are physically enforced across indexing, CLI, MCP, and profile resolution:
disabling `code` skips source indexing and rejects code queries/CLI commands
with `FEATURE_DISABLED`; disabling `docs` skips documentation indexing/checks
and strips `get_doc`, `search_docs`, and `docs.*` hooks from embedded `code`
profiles; disabling `state` automatically disables `blackboard`. Core tools
`acdd_read` and `acdd_guide` remain available even under `minimal`. External
`DomainProvider` plugins are toggled via `modules.domains.<domain_id>: true | false`.

Code-map output guards reject unreadable existing database targets and refuse
to overwrite `.acdd/state.sqlite` or the configured `state_db` path. Opening the
state store rejects code-map database filenames and schemas. Run initialization
and state/backup locking use bounded advisory locks under `<git_common_dir>/acdd/`.

Subproject identity uses spec `project` frontmatter, then the longest matching
`owners` prefix, then path inference, then the workspace default (`aliases` maps
legacy owner names once). Set `specs` collections above to govern discovery,
default profile selection, and `acdd draft` ID generation.

## MCP response policy

```yaml
response:
  detail: compact
  page_size: 30
  max_output_bytes: 98304
  source_context:
    enabled_by_default: false
    leading_lines: 5
    max_body_lines: 35
```

`page_size` accepts 1–100; `max_output_bytes` accepts 1,024–98,304 and defaults to 98,304. Smaller configured byte limits remain authoritative; this repository's `acdd.yaml` explicitly selects 65,536 bytes. The budget counts serialized UTF-8 content, response metadata, and JSON or Markdown escaping/framing. Overflowing paged collections retain whole records and update their pagination cursor to the number actually returned; a single record or required metadata that cannot fit returns an actionable bounded error. `code_map_inspect` and `code_map_explain` source previews default to 5 leading lines and 200 body lines. They accept 0–20 leading lines and 1–500 body lines; configured `source_context` values use those same bounds. This repository overrides the preview body limit to 35 in `acdd.yaml`. `acdd_read` uses the configured leading-line default but returns the complete selected symbol when its serialized response fits `max_output_bytes`; an explicit `max_body_lines` is normalized to at least one and limits the returned body. Oversized snippets page at complete-line boundaries; a single line that cannot fit returns a budget error and a local-file recovery hint. `detail` is `compact` or `full`. These settings control presentation and do not change extracted facts or require an index rebuild. Unknown response fields and out-of-range values are rejected.

The root `acdd.yaml` and the file selected with CLI `--adapter` are configuration rather than indexed sources. Nested files with that name and linked-workspace adapters remain ordinary YAML sources when admitted by their configured roots. A linked workspace's own adapter does not control the parent index.

## Declarative ACDD profile and role configuration

The ACDD unified state engine (`crates/acdd-state`) governs runs declaratively
using **v2 `GateProfile` YAML** files (`version: 2`).

### Loading precedence
1. Explicit CLI flag `--profile <name|path[:sha256_prefix]>` / `ACDD_PROFILE`
   or `acdd start --profile ...`
2. Spec frontmatter `profile: <name|path[:sha256_prefix]>` (optional `sha256` pin)
3. Collection override `specs.<collection>.profile` in `acdd.yaml` (if set)
4. Custom profile files searched under `profiles_dirs` (`.acdd/profiles/`,
   `docs/workflows/`)
5. Repository default `.acdd/profile.yaml` when present in the workspace root,
   automatically falling back to embedded `"code"` when absent (domain hooks
   unavailable under `modules.code: false` / `modules.docs: false` are stripped
   automatically)
6. Embedded built-in profiles (`base`, `code`, `audit`, `documentation`)

Profiles in linked workspaces are strictly **ignored**; only the active
workspace root defines the ACDD profile.

Any present or referenced profile with invalid YAML syntax, legacy `0.5.x` keys,
or unrecognized fields fails closed with `ACDD_PROFILE_SCHEMA_INVALID`
diagnostics. Existing `0.5.x` configuration and `.acdd/profile.yaml` files are
upgraded manually per the [0.6.0 migration guide](../runbooks/migration-0.6.md).

### Gate IDs, proofs, policies, and DomainProvider hooks

Each gate declares a stable `id` (`[A-Za-z0-9_-]`), optional `role`, `command`,
`call: <domain>.<action>` (+ `with:`), `checks: [<domain>.<check>]`,
`context: [<domain>.<lens>]`, `tools` / `tools_add` / `tools_remove`, `proof`,
`save`, `pass` / `fail` routing edges, `independent_from`, `need`, and optional
`when` predicates. Built-in proof kinds are `none`, `command`, `call`, and
`review` (requiring `role` and `contours`); named catalog schemas (`proofs:`)
and inline `{ scheme: ... }` JSON Schemas bind typed `--evidence` payloads.
Delivery/handoff gates use `receipt: true` with `save.post: commit` (and
`archive: true` / `prune: true` for run finalization). Leaf task gates may declare
`save.post: snapshot` for auto-checkpointed scoped trees (see [task snapshots](tasks.md#task-snapshots)).
See [ACDD Profiles Reference](profiles.md) for complete schema documentation.

### Profile pinning and tampering protection

Custom profiles pinned at planning time use `<path>:<sha256_prefix>` (at least 7
lowercase hex characters). During `acdd start` and `acdd sync`, ACDD records the
normalized profile snapshot and hash in SQLite (`runs`); claims and submissions
verify the persisted profile identity and fail closed on unauthorized tamper or
un-migrated `0.5.x` keys.

### Role and model configuration

Agent execution and model guidance are configured per **role** (`roles:`):
- `mode`: `subagent` (default) or `inline`
- `reuse_on_reject`: explicit boolean (`false` by default; `true` in embedded roles that retain builder sessions on reject)
- `models`: ordered list of `'model:reasoning'`, `'model'`, or `{ model, reasoning? }`
- `guidance`: coordinator-facing role dispatch guidance shown in `acdd next` / `acdd_state(action: "next")`
- `runner` / `prompt_template`: optional command and prompt templates for automated `acdd run`

```yaml
# .acdd/profile.yaml
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

During `acdd start`, ACDD projects the compact gate dispatch plan (`role`,
`mode`, `reuse_on_reject`, recommended `models`, `command`/`call`) without
per-gate guidance; during `acdd next` / `acdd_state(action: "next")`, it
projects coordinator `guidance` alongside each currently ready gate right before
dispatch. During `acdd claim` / `acdd_claim` and `acdd show` /
`acdd_state(action: "show")`, ACDD projects bound `ClaimResp` tiers (`contract`, `guidance`, `diff`, `context`, `blackboard`, `traces`) for the
assigned executor without spending `Tier 1` worker budget on model-selection lists.
Headless `acdd run` also exports `ACDD_ROLE_MODE`, `ACDD_ROLE_MODELS`, and primary
`ACDD_MODEL` / `ACDD_REASONING` environment variables to configured `runner:` processes.

### Repository `.gitignore` and protected paths

Because `.acdd/profile.yaml`, `.acdd/profiles/**`, and `.acdd/frameworks/**` are
committed repository assets, do **not** ignore the whole `.acdd/` directory. Keep
ACDD runtime state ignored while unignoring configuration assets:

```gitignore
/.worktrees/
!/.acdd/
!/.acdd/profile.yaml
!/.acdd/profiles/
!/.acdd/profiles/**
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

See [MCP tools](mcp-tools.md) for response paging and [limits and freshness](../runbooks/limits-and-freshness.md) for computation budgets.
## Sensitive reader paths

Configure `sensitive` in `acdd.yaml` with workspace-relative gitignore-style
patterns. The reader checks the requested path and the resolved workspace-relative
path before reading or exporting content, including configured linked workspaces.
Patterns apply to output destinations as well as inputs.

```yaml
sensitive:
  - '.env*'
  - 'secrets/**'
```
