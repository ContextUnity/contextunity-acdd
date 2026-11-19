[mode overridden: auto -> full, reason=instruction file requires complete content]

# ACDD `GateProfile` Reference (`version: 2`)

Every `GateProfile` YAML document uses strict `deny_unknown_fields` validation
and compiles via `acdd validate`. Repository defaults live in `.acdd/profile.yaml`
(or fallback to embedded `code`); custom workflows live in `.acdd/profiles/` or
`docs/workflows/`.

## 1. Inheritance & Execution Modes

- **`version: 2`** (required).
- **`inherit`**:
  - `base`: domain-pure worktree-isolated baseline (`gates: []`, `save: commit`, `contours: [paths, claims, administration]`, 7 core tools).
  - `code`: inherits `base`, adds full software lifecycle (`blueprint_synthesis` -> `tasks:` -> `verify` -> `polish` -> `integration_review` -> `handoff`), `tags: [default, docs]`, all 19 tools, `architecture`/`concurrency`/`project_isolation` contours, `code.seam` check, and automatic stripping of disabled `code.*`/`docs.*` hooks when `modules.code: false` or `modules.docs: false`.
  - `audit`: inherits `base`, sets `git.isolation: in_place`, `save: none`, roles `auditor` / `independent_reviewer`, and read-only gates `recon -> analyze -> triage` (`save.report: ".acdd/reports/{run_name}.md"`).
  - `none`: standalone graph with zero inherited settings.
  - `<relpath>[:<sha256_prefix>]`: inherit from a repo-confined YAML file (optional min-7-hex SHA-256 content pin).

Inherited maps (`roles`, `commands`, `scope_args_by_pattern`, `proofs`, `guidance`,
`contours`, `gate_templates`) merge by key (`key: null` clears an inherited
entry). Passing `--in-place` on `acdd start` overrides isolation to `in_place`,
`worktrees: manual`, top-level `save: none`, and neutralizes `save.post: commit`
to `none` while preserving `save.immutable: true`.

## 2. Top-Level Policy Keys

| Key | Type / Allowed Values | Purpose & Defaults |
|---|---|---|
| `git` | `{ isolation: worktree \| in_place, worktrees: manual \| managed }` | Defaults: `worktree`, `manual`. `managed` creates/cleans task & run worktrees. |
| `save` | `none \| commit` | Default save action for terminal root gate (default `commit` in `base`). |
| `tags` | `[<string>, ...]` or `null` | Allowed task tags (`default` is always permitted; `code` adds `docs`). |
| `protected_paths` | `[<glob>, ...]` | Paths protected from normal task edits even if matched elsewhere. |
| `default_tools` | `[<tool_name>, ...]` | Default MCP allowlist for gates omitting `tools:`. |
| `modules` | `{ profile?, enabled?, disabled?, required?, domains? }` | Subsystem preset/toggles (`domains.<id>: false \| null` patches domains). |
| `limits` | Object of 8 numeric caps | Bounds logs, guidance, diagnostics, metadata, and execution timeouts. |

**`limits:` defaults**: `max_guidance_bytes: 32768`, `max_log_bytes: 1048576`,
`max_capture_bytes: 262144`, `max_line_bytes: 65536`, `diagnostic_inline_bytes: 4096`,
`max_metadata_bytes: 2048`, `max_trace_summary_bytes: 2048`,
`max_command_timeout_seconds: 1800`.

## 3. Roles, Commands, Proofs & Reusable Catalogs

- **`roles.<id>`**:
  - `mode`: `subagent` (default) | `inline`.
  - `reuse_on_reject`: `bool` (default `false`; signals `acdd run` to resume agent session on reject).
  - `models`: `["model", "model:reasoning", { model, reasoning? }]` (advisory ordered preferences).
  - `guidance`: inline role instruction string (`null` clears).
  - `runner`: shell command template required for headless `acdd run` execution. Receives `ACDD_RUN_ID`, `ACDD_NODE_CURSOR`, `ACDD_CLAIM_REVISION`, `ACDD_WORKER_ID`, `ACDD_AGENT_SESSION_ID`, `ACDD_WORKTREE`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, `TRACEPARENT`, and rendered `ClaimResp` over `stdin`.
  - `prompt_template`: optional stdin prompt template rendered before delivery to `runner`.

- **`commands.<id>`** (four YAML forms; `null` removes an inherited slot):
  - `{ unbound: true, description? }`: abstract slot (must be overridden before use).
  - `"<shell command>"`: plain command string.
  - `{ command, description?, timeout_seconds?, test_filter?, scope_args_by_pattern? }`: detailed command (`test_filter: true | false | "-- {test}"` expands `{test_filter}` from `--test <filter>`; `{scope_target}` expands from `scope_args_by_pattern` matching task scope globs).
  - `{ by_pattern: { "<glob>": "<command>", ... }, description?, timeout_seconds?, test_filter?, scope_args_by_pattern? }` (or YAML list of `{ pattern, command, ... }`): selects the most specific scope-matching command.

- **`proofs.<id>`**: `{ description?, schema: <JSON Schema> }` reusable schema catalog referenced by `proof: <id>`.
- **`guidance.<id>`**: plain string or `{ description, criteria: [...] }` reusable slot referenced on gates (inline overrides use `{ inline: "..." }`).
- **`contours.<id>`**: `{ description, criteria: [...] }` required when referenced in a review gate's `contours:`.
- **`gate_templates.<id>`**: `{ gates: [...], command?, guidance?, max_attempts? }` expanded into any gate setting `use: <id>`.

## 4. Gates (`gates:` vs. `gates_patch:`)

`gates:` replaces inherited gates; `gates_patch:` patches inherited gates by
matching `id`, cursor (`"tasks.build"`, `"blueprint_synthesis.review"`), or the
`- tasks:` block (mutually exclusive with `gates:`, forbidden with `inherit: none`).

Inside `gates_patch`:
- `- <gate_id_or_cursor>: null` deletes an inherited gate.
- `after: <sibling_id>` or `before: <sibling_id>` inserts a new gate relative to an existing sibling.
- `PatchField` properties accept explicit `null` to clear an inherited value (`role: null`, `command: null`).

### Gate & `- tasks:` Fields

- `id`: unique gate name (omitted on an anonymous `- tasks:` track).
- `gates: [...]`: nested child gates (marks this gate a container).
- `tasks: { when?, gates: [...] }`: instantiates per-task gates for non-cancelled matching tasks.
- `when`: boolean expression over task tags (`tag`, `!tag`, `&`/`+`, `|`, parentheses; allowed on `- tasks:` and task gates, forbidden on root gates).
- `use`: expand named template from `gate_templates`.
- `role`, `command`, `call` (`"<domain>.<action>"`), `with` (JSON arguments for `call`).
- `checks`: list of `"<domain>.<check>"` or `{ check, with? }` (e.g. `code.seam`, `code.signature_scope`, `docs.links`).
- `context`: list of domain context lenses (e.g. `docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`).
- `proof`: `none` (default) | `command` | `call` | `review` (requires `role` + non-empty `contours`) | `<catalog_id>` | `{ scheme: <JSON Schema> }`.
- `tools` / `tools_add` / `tools_remove`: replace or adjust `default_tools` (validated fail-closed against static + registered `DomainProvider` plugin tools).
- `guidance`, `contours`, `scope` (`[<glob>, ...]` explicit gate scope e.g. `polish`).
- `save`: `{ post?: none | commit | snapshot, immutable?: bool, archive?: bool, prune?: bool, report?: ".acdd/reports/..." }` (scalar `none | commit | snapshot` shorthand supported on gates; profile-root `save:` accepts only `none | commit`. `save.post: snapshot` is valid only on leaf task gates, writes `refs/acdd/snapshots/<run_id>/<task_ref>/r<claim_revision>` without moving `HEAD`, and refs auto-clear on deliver, run `clear`, or `handoff`. `save.report` writes `.md` or `.json` strictly under `.acdd/reports/` using `{run_id}`, `{run_name}`, `{gate_id}`, `{spec_id}`).
- `max_attempts` (default `3`), `receipt: true` (closes container & materializes `ArtifactKind::Proof`), `independent_from: [<cursor>]`, `need: [<cursor>]` (enforces predecessor accepted tree match), `pass`, `fail` (`defect` routing), `fail_drift` (`architectural_drift` routing triggering builder context reset).
- Passing a container predecessor gate with `proof` propagates structured outputs into downstream `ClaimResp.pre_task_slice` (`tasks[<task_ref>]` and `predecessor_proofs`).

```yaml
# Minimal repository overlay example (inherit: code + gates_patch + command bindings)
version: 2
inherit: code
tags: [default, docs]

commands:
  build_check: "pytest -q {scope_target} {test_filter}"
  verify_all: "pytest -q"

scope_args_by_pattern:
  "src/**/*.py": "tests/"

gates_patch:
  - id: tasks.build
    max_attempts: 4
```
