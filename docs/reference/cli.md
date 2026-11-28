---
doc_type: api
title: CLI reference
---

# CLI reference

`acdd` provides a command-line interface and a stdio MCP server. Run `acdd --help` or append `--help` to any command for accepted arguments. Commands other than `serve` print readable Markdown by default. Pass global `--format json` to retain machine-readable JSON output.

## Global options

| Option | Behavior |
| --- | --- |
| `-C <PATH>`, `--root <PATH>` | Workspace or worktree root for all commands. Defaults to the current directory; `ACDD_WORKSPACE_ROOT` or fallback `ACDD_WORKTREE` sets it when `--root`/`-C` is omitted. |
| `--db <PATH>` | Code-index path, relative to the selected root. Defaults to `<root>/.acdd/code-map.sqlite`; `ACDD_DB` can set it. Does not select the state store. |
| `--state-db <PATH>` | State SQLite database path override (`ACDD_STATE_DB`). Defaults to `state_db` in `acdd.yaml` (`.acdd/state.sqlite` under the primary Git worktree). |
| `--config <PATH>` | Root-confined ACDD YAML configuration; `ACDD_CONFIG` can set it. Explicit missing files fail. Defaults to `<root>/acdd.yaml` when present. |
| `--workflow <REF>` | Explicit gate workflow name (`base \| code \| documentation`), `workflows_dirs` entry, or root-confined YAML path (`[:<sha256_prefix>]`); `ACDD_WORKFLOW` can set it. |
| `--format markdown|json` | Output format for command results. Defaults to `markdown`; invalid values fail before a command runs. |

- **Positional root**: `build`, `scan`, and `delta` also accept a positional workspace path (when combined with `-C`/`--root`, both must resolve to the same directory).
- **Flag & env precedence**: CLI flags override matching `ACDD_*` environment variables once per invocation; `-C`/`--root` overrides `ACDD_WORKSPACE_ROOT`, which overrides `ACDD_WORKTREE`. All CLI `--run` options accept `ACDD_RUN` as an environment fallback so nested `acdd` invocations inside an active worktree shell inherit the active run automatically; explicit `--run` overrides `-C`/`--worktree` and positional selectors, while explicit `-C`/`--worktree` (and positional `clear` target) overrides ambient `ACDD_RUN`.
- **Unified worktree targeting (`-C <worktree>`)**: passing `-C <worktree>` works uniformly for both code/docs commands (`build`, `delta`, `read`, `query`, `ast`, `lint`, `docs`) and state commands (`status`, `show`, `next`, `context`, `blackboard`, `claim`, `verify`, `submit`) without redundant `--run` or `--worktree` flags (resolving the primary repository via `--git-common-dir` and the active run via the target worktree).
- **Workflow precedence**: `--workflow` / `ACDD_WORKFLOW` → spec `workflow:` → `specs.<name>.workflow` → `workflows_dirs` → `.acdd/workflows/default.yaml` → built-in `code` (plus `base`, `documentation`). Removed CLI flags fail via Clap argument parsing.
- **State DB resolution**: `--state-db` → `state_db` in `acdd.yaml` → `<primary_worktree>/.acdd/state.sqlite` (resolved via `--git-common-dir`).
- **Runner vs. command environments**: spawned role runners receive `ACDD_RUN_ID`, `ACDD_NODE_CURSOR`, `ACDD_CLAIM_REVISION`, `ACDD_WORKER_ID`, `ACDD_AGENT_SESSION_ID`, `ACDD_WORKTREE`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, `ACDD_ROLE_*`, `ACDD_MODEL`, `ACDD_REASONING`, and `TRACEPARENT`; project build/test commands run in a sanitized environment with `TRACEPARENT`, internal `ACDD_*` variables (`ACDD_CONFIG`, `ACDD_DB`, `ACDD_EXTRACTION_CACHE_DIR`, `ACDD_WORKFLOW`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, `ACDD_WORKSPACE_ROOT`, `ACDD_AGENT_SESSION_ID`, `ACDD_ROLE_*`, `ACDD_MODEL`, `ACDD_REASONING`), and `GIT_*` variables scrubbed.
- **`serve` & `delta` config retention**: `serve` keeps its resolved config/workflow across reloads; `delta` reuses the persisted index adapter unless `--config` is passed explicitly.

## Inventory and indexing

```sh
acdd scan .
acdd build .
acdd build /path/to/repository --output /path/to/index.sqlite
acdd delta . src/module.py src/other.py
```

- **`scan`**: reports admitted files without writing an index.
- **`build`**: creates a full SQLite code & docs index.
- **`delta`**: updates the listed changed or deleted paths in an existing index.
- **`--adapter <PATH>`**: selects a workspace configuration file for `scan`/`build` (fails if conflicting with `--config`).
- **`open_or_rebuild`**: `query`, `docs`, and `ast grep` build a missing or schema-incompatible index automatically, though existing indexed files still require `build` or `delta` after source edits.

## File and symbol reads

```sh
acdd read notes.txt --start-line 20 --line-count 40
acdd read notes.txt --mode search --pattern 'timeout' --context 2
acdd read notes.txt --mode search --pattern 'error|warning' --regex
acdd read docs/reference/tasks.md --mode outline
acdd read src/engine/reader.rs --selector acdd_read_tool
acdd read notes.txt --mode delta --baseline HEAD
acdd read notes.txt --mode raw --output-file exports/read.txt
```

- **`read <path>`**: shares the same layered reader, response detail policy (`response.detail`, `compact` by default), and budgets as MCP `acdd_read` (`lines`/`search` default `line_count` `100`; default `<= 9,000` bytes inline with `next_start_line` continuation; pass `--detail full` or `--output-file` for the `48 KiB` slice ceiling; `search` defaults to `--context 0`, groups directory matches under `## <path>`, and excludes `**/archive/**` unless targeting `archive`; see [layered file reader](mcp-tools.md#layered-file-reader)).
- **Auto mode**: selects `snippet` when `--selector` is given, `search` when `--pattern` is given, and `lines` otherwise (or pass a symbol selector directly with `--mode snippet`).
- **Index requirements**: `lines`, `raw`, `search`, and `delta` run without an index; Markdown `outline` requires `docs`; code `outline` and `snippet` require the code index.
- **Symbol paging**: bound output with `--max-body-lines` and `--leading-lines`, and continue across pages using `--source-offset` + `--generation`.

## Queries

```sh
acdd query overview
acdd query overview --aspects entrypoints
acdd query overview --aspects entrypoints_with_tests
acdd query overview --aspects topology
acdd query search 'parse*' --kind function --classification production --limit 30
acdd query search 'takes:WorkflowDef'
acdd query search 'returns:Result'
acdd query inspect 'src/module.py:parse' --show-source
acdd query explain 'src/module.py:parse'
acdd query impact 'src/module.py:parse' --depth 1
acdd query impact --changes staged --depth 1
acdd query tests 'src/module.py:parse' --direction inbound
acdd query remove 'src/module.py:parse'
acdd query analyze 'SELECT name FROM nodes ORDER BY name'
acdd query analyze --mode hotspots src/
acdd query analyze --mode diff_impact --base main
acdd query run slice 'src/module.py:parse' --depth 1 --limit 30
acdd query run slice src/db --limit 30
acdd query run untested src/core --limit 30
acdd query run implementations LanguageLinker --limit 30
acdd query run duplicates --level symbol --min-lines 10 --limit 30
acdd query run duplicates src --level file --limit 30
acdd query run dead_tails --path crates/acdd-state --limit 30
acdd --format json query overview
```

- **`query overview --aspects`**: accepts comma-separated aspects:
  - `entrypoints`: production entrypoints (excludes tests).
  - `entrypoints_with_tests`: entrypoints plus covering tests.
  - `topology`: opt-in paged module metrics and dependency edges; directory slices include bounded boundary summaries (see [MCP reference](mcp-tools.md#selectors-and-graph-queries)).
- **`query impact`**: defaults to depth 1 and accepts either one symbol selector or `--changes staged|unstaged|head` (plus optional `--path`, `--limit 1..100`, `--offset`, and `--generation`):
  - Change views return `changed_symbols`, impacted `nodes` and `tests`, and explicit `unmapped_changes` (`complete: false` when Git postimages diverge from the index).
  - `query run impact` keeps depth `2` as its default; both forms accept `--depth`.
- **`query tests`**: supports `--direction inbound` (covering tests) and `--direction outbound` (production targets exercised by a test).
- **`query search`** orders module indexed-text matches behind concrete matches before selecting the page. It includes the top-hit graph pulse for navigation. See the [MCP search reference](mcp-tools.md#selectors-and-graph-queries) for ranking and MCP request options.
- **`query run <op>`**: executes `overview`, `inspect`, `explain`, `impact`, `tests`, `prove_removal`, `slice`, `unwired`, `sql`, `untested`, `implementations`, `duplicates`, or `dead_tails`.
- **`query analyze`**: accepts an indexed path, empty workspace target, or one read-only `SELECT`/`WITH` statement (`--mode summary` default, computing cycles in CLI diagnostic mode; MCP uses `include_cycles: true`), or `--mode hotspots|boundary|path|entrypoints|diff_impact|cochange` (with `--to` for `path` and optional `--base` for `diff_impact`/`cochange`).
- **`query inspect --show-source`** returns the digest-checked symbol body (omitting resolution `coverage` by default, matching MCP `code_map_inspect`). Continue with `--source-offset` and the previous page's `--generation`. Source pages break on line boundaries, and terminal JSON previews retain null continuation fields. See [limits and freshness](../runbooks/limits-and-freshness.md).
- **`--format markdown|json`**: global output flag valid before or after any subcommand; Markdown renders verified vs. candidate edge evidence, while JSON preserves exact structured payloads.
- **`analysis_gaps` & explain edges**: `inspect` and `explain` include paged `analysis_gaps` (reason, path, line, scope, evidence) plus `calls` (exact) and `possible_calls` (`possible`, bounded branch joins), including `trait_object_dispatch` and `macro_expansion_not_analyzed`.

## Documentation and syntax search

```sh
acdd docs search 'dependency' --doc-type architecture
acdd docs get 'docs/architecture/indexing.md' --section 'Ownership and data flow'
acdd ast grep 'print($VALUE)' --lang python --path src
```

- **`docs search` / `docs get`**: query indexed Markdown sections (`--component`, `--doc-type`, `--limit`); pass `--include-candidates` on `acdd docs get` to opt into ambiguous documentary symbol reference candidates (omitted by default).
- **`ast grep`**: narrows candidate files from the index, verifies file digests, and matches Tree-sitter patterns (infers compatible language(s) from `--path` or pattern grammar when `--lang` is omitted, dropping `yaml`/`json`/`toml` whenever at least one programming language parses cleanly; omits empty capture entries for anonymous `$$$` wildcards; `--limit 1..=100` matches MCP page objects while defaulting to `100` on CLI vs `min(15, response.page_size)` (`12` by default) on MCP `ast_grep_search`, and `101..=10000` returns CLI-only aggregate matches).

### Unified architectural and documentation lint (`acdd lint`)

```sh
acdd lint
acdd lint crates/acdd-state --categories arch,docs --severity warn
acdd lint --rule INV-CORE-LEAF --format json
acdd lint --categories all --limit 50 --offset 0
```

`acdd lint [TARGET]` runs the unified language-agnostic lint engine over `.acdd/code-map.sqlite` (matching MCP `code_map_lint`):
- `--categories`: comma-separated `arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`, or `all` (omitted defaults to `arch,syntax,docs`). `syntax` evaluates stored parser diagnostics (`syntax.parse`), `syntax.fact_budget_exceeded`, and `syntax.intra_crate_type_homonym` (using `.acdd/lint/*.yaml` `complexity.severity`, while `cognitive_complexity` and `max_nesting_depth` are recorded as `node.details` telemetry); `cycles` inspects multi-node non-test/non-archive module `imports` SCCs (enforceable as errors via `forbid_cycles` rules); `duplicates` inspects AST-statement clones (with Type-2 local-identifier normalization and test exclusion by default); `dead_tails` computes workspace-wide production liveness and reports `dead_tails.phantom_tested`, `dead_tails.unreachable`, `dead_tails.dangling_call` (`error`), and—when `.acdd/lint/*.yaml` `unused_public_api: true` is enabled—`dead_tails.unused_public_api` (`error` on unused/test-only `pub use` re-exports and internal `pub` items lacking non-test cross-module production callers).
- `--rule <ID>`: repeat or pass comma-separated rule/invariant IDs to filter evaluated diagnostics.
- `--severity <warn|error>`: minimum diagnostic severity threshold.
- `--limit`, `--offset`, `--generation`, `--detail <compact|full>`: standard generational paging matching `code_map_lint`.
- Exits non-zero when `summary.errors > 0`.

## Unified state lifecycle

Flat `Run` and `WorkflowDef` commands backed by `crates/acdd-state`, `crates/acdd-spec` (`draft`, `roadmap`, `validate`), and `src/state_bridge/` (see [state, tasks & lifecycle](tasks.md) for schemas, locks, and error codes).

### Workspace initialization and specification drafting

```sh
acdd init [--force]
acdd draft <spec> <slug> [--prefix PREFIX] [--title "Title"]
```

- `init [--force]`: Writes starter `acdd.yaml` (`preset: full`, `workflows_dirs: [.acdd/workflows]`), `.acdd/workflows/default.yaml` (`inherit: code` when `code` module is enabled, otherwise `inherit: base`), and the canonical `.acdd/` protection block in `.gitignore` (keeping `.acdd/workflows/**`, `.acdd/specs/**`, `.acdd/lint/**`, and `.acdd/frameworks/**` trackable), verifying via `git check-ignore` that `.acdd/state.sqlite` is ignored and `.acdd/workflows/default.yaml` stays trackable.
- `draft <spec> <slug>`: Creates the next sequentially zero-padded specification Markdown file in the configured `specs.<spec>` directory (`milestones` or `plans` by default) via `crates/acdd-spec` under `SubsystemId::Spec` without creating `.acdd/state.sqlite`. Populates `DraftReq` (`spec`, `slug`, `prefix?`, `title?`, `workflow?`) and returns `DraftResp` (`spec_id`, `spec_path`, `spec`) under `--format json`.

### Run startup, inspection, and ready queue

```sh
acdd start [<spec> ...] [--goal TEXT] [--name RUN_NAME] [--workflow WORKFLOW] [--scope PATH ...] [--target-base REF] [--worktree | --in-place] [--worktree-path PATH]
acdd status [--run RUN_ID_OR_NAME]
acdd list [--workspace NAME] [--status active|completed|cancelled] [--include-tombstones] [--limit 1..100] [--offset N] [--generation GEN]
acdd show [<node>] [--run RUN_ID_OR_NAME] [--artifact proof|findings|receipt|all] [--include-profile]
acdd context [<node>] [--run RUN_ID_OR_NAME] [--worker WORKER_ID] [--detail compact|full]
acdd next [--run RUN_ID_OR_NAME] [--limit N]
acdd roadmap [spec_id] [--spec NAME] [--all] [--tag TAG] [--field KEY=VALUE] [--group-by AXIS] [--color auto|always|never] [--ready] [--active] [--tasks]
```

- `start`: Starts a new `Run` from `0..N` spec files (`[<spec> ...]`, where the first is the primary spec and any additional specs are persisted in `runs.attached_specs_json`) or an ephemeral `--goal` (materializing `.acdd/run/<run_id>/spec.md`, attaching `--scope` globs as task scope or frontmatter `owners:` on taskless workflows like `audit`). Selects `worktree` (`--worktree`, `--worktree-path`, `--target-base`) or `--in-place` isolation (neutralizing `save.post: commit` to `none` while preserving `save.immutable: true`). Maps to `StartReq` (`spec_path?`, `spec_paths`, `goal?`, `run_name?`, `workflow?`, `scope`, `target_base?`, `worktree`, `in_place`, `worktree_path?`, `#[serde(deny_unknown_fields)]`).
- **`status`**: renders active run summary and current gate status.
- **`list`**: lists runs (`--include-tombstones` includes cleared/completed/cancelled `run_tombstones` with `runs.generation` paging).
- **`show`**: renders the full run tree or one dotted node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`). Lists persisted `gate_artifacts` metadata (`proof`, `findings`, `receipt`) by default; pass `--artifact proof|findings|receipt|all` for full parsed JSON payloads and `--include-profile` to retain `run.profile_json` on node-level output.
- **`next`**: lists currently ready node cursors and topological execution waves.
- **`context`**: reads `.acdd/state.sqlite` (`SQLITE_OPEN_READ_ONLY`) to refresh `ClaimResp` + blackboard without mutating claims or advancing `claim_revision` (ideal for `PostCompact` / `SessionStart(compact)` hooks); pass `--detail compact|full` (`compact` defaults to `<= 10,000` bytes; `full` allows up to `32 KiB`).
- **`roadmap`**: displays cross-spec runs, dependency waves, critical-path order, and scope overlaps via `crates/acdd-spec` + `SpecRuntimeOverlay` under `SubsystemId::Spec` (by default showing collections with `roadmap: true`; pass `--all` to include `roadmap: false` collections such as `memories`, `--spec <CONFIG_KEY>` to filter by configured `specs:` key, `--tag <TAG>` and `--field KEY=VALUE` to filter by schema tags/fields, `--group-by <AXIS>` to group by `wave`, `spec`, or a schema field, `--color auto|always|never` respecting `NO_COLOR`, and `--tasks` / positional `[SPEC_ID]` to drill down; never creates `state.sqlite`). Maps to `RoadmapReq` (`spec_id?`, `spec?`, `all`, `tags`, `fields`, `group_by?`, `badges`, `ready_only`, `active_only`, `include_tasks`, `archived`, `current_only`) and emits `RoadmapResp` (`group_by?`, `groups`, `waves`, ...) with [`RoadmapSpecEntry`](../../crates/acdd-spec/src/types.rs) items (`spec_id`, `title`, `spec`, `path`, `doc_type`, `status`, `badges`, `run_id?`, `worktree?`, `source_worktree?`, `divergence`, `completed_tasks`, `total_tasks`, `active_gates`, `unblocks_specs`, `unblocks_tasks`, `parallel_safe`, `scope_overlaps`, `depends_on`, `blocks`, `tasks`) under `--format json`.

### Gate execution: `claim`, `verify`, `submit`

```sh
acdd claim [<node>] [--run RUN] [--worker ID] [--session-fingerprint FP] [--force] [--steal-remote] [--worktree PATH] [--detail compact|full]
acdd verify <node> [--run RUN] [--worker ID] [--test TARGET] [--claim-revision REV] [--worktree PATH]
acdd submit <node> --action pass|reject [--run RUN] [--worker ID] [--claim-revision REV] [--test TARGET] [--evidence JSON_OR_@PATH] [--findings JSON_OR_@PATH] [--invariant ID ...] [--summary TEXT] [--allow-main] [--worktree PATH]
```

- `claim`: Claims `<node>` (or auto-selects the next ready cursor when `<node>` is omitted) under optimistic concurrency (`claim_revision` + `node_version`), returning the 6-tier budgeted `ClaimResp` (`<= 10,000` bytes compact soft cap with `omitted_sections` on overflow, or up to `32 KiB` with `--detail full`). Pass `--force` to take over an active claim (`--steal-remote` required across different host session fingerprints).
- `verify`: Executes the node's `command:`, `call:`, `checks:` (`DomainProvider`), and write-scope perimeter barrier without advancing state. `--test` passes the targeted test filter to `{test_filter}` and `code.seam` (for example on `build` gates).
- **`submit`**: submits `--action pass` or `--action reject` for `claim_revision`:
  - Requires a fresh passing `verify` trace bound to the active `claim_revision` and working tree on gates with `command:`, `call:`, `checks:`, or `proof: command|call` (failing closed with `TASK_PROOF_UNOBSERVED` when absent or stale; `--test` checks trace digest parity without auto-running `verify`), `--evidence` on review/schema gates, and structured `--findings` (`kind`: `defect | architectural_drift`) on `reject`.
  - Creates scoped plumbing commits on `receipt: true` + `save.post: commit` (blocking `main`/`master` unless `--allow-main`) and materializes `save.report` (`report_path`).

### Scope, reopening, sync, subtasks, and blackboard

- **`acdd snapshot <save|restore|list|clear>`**: manages Git checkpoints:
  - `--run RUN`: keeps per-task refs (`refs/acdd/snapshots/<run_id>/<task_ref>/r<rev>`); omitting `--run` resolves unambiguously matching active tasks, otherwise standalone `refs/acdd/snapshots/_adhoc/<label>/r<rev>`.
  - `--delete` / `clear`: supports `--rev N`, `--older-than <N><s|m|h|d|w>`, or `--all` (using `--rev`/`--older-than` without `--run` targets standalone refs; see [task snapshots](tasks.md#task-snapshots)).

```sh
acdd extend-scope <node> --path PATH [--path PATH ...] --reason TEXT [--run RUN] [--worker ID]
acdd reopen <node> [--run RUN] [--cascade] [--reason TEXT]
acdd sync [--run RUN] [--rebind-workflow]
acdd subtask <task_ref> <subtask_ref> --status pending|in_progress|completed|cancelled [--evidence TEXT] [--run RUN]
acdd blackboard [--action read|post] [--run RUN] [--node CURSOR] [--recipient-node CURSOR] [--subtask-ref REF] [--gate GATE] [--author WORKER] [--topic draft|notes|findings|blockers|decisions|deferred] [--payload TEXT] [--status open|acknowledged|resolved|carried_over] [--limit N] [--offset N] [--generation GEN]
```

- `extend-scope`: Expands `admitted_scope` after conflict checks against other claimed incomplete tasks (blocking `.acdd/**` and `protected_paths` with `OUT_OF_SCOPE_CHANGES`).
- `reopen`: Reopens a gate or task container; `--cascade` marks downstream `in_progress`/`completed` dependent tasks stale.
- `sync`: Reconciles spec edits into an active run (`SyncReq`: `run?`, `rebind_workflow`); `--rebind-workflow` rebinds non-completed gates against the latest workflow, returning `SyncResp` (`run_id`, `spec_digest`, `rebound_workflow`, `added_nodes`, `updated_nodes`).
- `subtask`: Transitions subtask state; terminal `completed` or `cancelled` requires non-empty `--evidence`.
- `blackboard`: Reads or posts scoped collaboration messages (`open | acknowledged | resolved | carried_over`) across topics `draft | notes | findings | blockers | decisions | deferred`.

### Run cleanup and integration

```sh
acdd clear [<run_id>] [--run RUN] [--reason TEXT] [--force] [--purge]
```

- `clear`: Cancels/clears a run into `run_tombstones(terminal_status = 'cleared')` and cleans managed worktrees (`--force` overrides dirty worktree checks); `--purge` deletes a single matching already-cleared (`terminal_status = 'cleared'`) tombstone row after manual completion/archival while refusing active runs and `completed`/`cancelled` tombstones.
- Task branch integration: Headless `acdd run` waves land completed task branches internally via `rebase-ff` (`integrate_task_branch`); manual worktree runs integrate via standard `git merge` or `git rebase`, and terminal verification confirms content equivalence by Git `TreeOid` (`TASK_NOT_INTEGRATED`).

### Headless run execution

```sh
acdd run [--run RUN] [--jobs N] [--allow-main] [--dry-run]
```

`acdd run` drives the active `Run` across topological DAG waves using the
resolved `WorkflowDef`:
- Executes mechanical `command:` / `call:` gates directly via `verify` + `submit`.
- Spawns configured `roles.<role>.runner` processes for role-assigned gates (clamped to `--jobs 1` when `git.isolation: in_place`), streaming child output and collecting accepted submissions.
- `--dry-run` computes schedulable waves without mutating storage or spawning runners.
- Role-assigned gates without a configured `runner:` command fail closed with `WORKFLOW_RUNNER_REQUIRED` (manual/subagent orchestration uses `claim` -> `verify` -> `submit` directly).

### Worktree command transit (`exec`)

```sh
acdd exec [--run <RUN_ID_OR_NAME>] [--worker <WORKER_ID>] -- <PROGRAM> [ARGS...]
acdd -C <WORKTREE> exec -- <PROGRAM> [ARGS...]
```

- Resolves the target checkout directory and optional active `run_id` strictly read-only (respecting `SubsystemId::State` and never creating `.acdd/state.sqlite` when absent) in deterministic priority order:
  1. Explicit `-C`/`--root` (or current working directory already inside a linked Git worktree) uses that checkout directory and resolves its bound active run if present.
  2. Explicit `--worker <WORKER_ID>` queries active `in_progress` gate claims across active runs and selects the claimed node/run worktree (failing closed on `0` or `>1` active claims).
  3. Explicit `--run <RUN_ID_OR_NAME>` (or `ACDD_RUN`) resolves the active run and selects `run.worktree` (or the primary root for `in_place`).
  4. Otherwise queries active runs and selects the single active run's worktree (failing closed on `0` runs or `>1` active runs listing candidates).
- Executes `<PROGRAM> [ARGS...]` with `current_dir` set to the resolved worktree, setting `ACDD_WORKSPACE_ROOT`, `ACDD_WORKTREE`, and `ACDD_RUN` (when bound to a run; removed when unbound), inheriting stdio, and exiting with the child's exact exit code (`128 + signal` on Unix signals).

## Server, guide, and plugin tools

```sh
acdd --root /path/to/repository serve
acdd reload
acdd guide init [--force]
acdd guide [topic] [--mcp]
acdd guide query
acdd guide lifecycle --format json
acdd guide acdd
acdd guide gates
acdd guide subtasks
acdd guide code
acdd guide code --mcp
acdd guide docs
acdd tool <domain>_<name> [--args JSON] [--run RUN] [--node CURSOR] [--worktree PATH]
```

- `serve` / `reload`: Starts the stdio MCP server (18 built-in tools + active `DomainProvider` `<domain>_<name>` plugin tools) or reloads running MCP server processes via `SIGHUP`.
- `guide`: Returns capability-filtered CLI-focused guidance across the 11 topics (`query`, `init`, `adapter`, `validate`, `code`, `ast`, `docs`, `lifecycle`, `acdd`, `gates`, `subtasks`), or pass `--mcp` for MCP-tool guidance matching `acdd_guide`.
- `tool`: Invokes a `<domain>_<name>` plugin tool exposed by an active `DomainProvider` with JSON arguments validated against `DomainToolDef.input_schema`.

## Validation

```sh
acdd validate [<PATH>]
```

- **`validate [<PATH>]`**: runs the unified validator (`ValidateReq`: `path?`, `explicit_config?`, `explicit_workflow?`, invoked through `acdd_state::validate` whenever `SubsystemId::Spec` is enabled; `modules.state` controls whether live `StateRuntimeOverlay` run/tombstone checks and bound workflow `commands:` checks run; never creates `.acdd/state.sqlite`):
  - **Config (`acdd.yaml`)**: validates schema keys (`load_adapter_report`, rejecting unknown or removed keys such as inline `architecture:` via `#[serde(deny_unknown_fields)]`), `specs:` entries, `specs_dirs:` (`.acdd/specs/*.yaml`), `lint_dirs:` (`.acdd/lint/*.yaml`), and `linked_workspaces` paths.
  - **Workflows (`.acdd/workflows/default.yaml`)**: verifies inheritance, fields, bound commands (when `modules.state` is enabled), `DomainProvider` capabilities, and `modules.required` (`WORKFLOW_VALID` on pass; `ACDD_WORKFLOW_SCHEMA_INVALID` with `FEATURE_REQUIRED_BY_PRESET` in `message` for registry-local requirement mismatches, or `DOMAIN_CAPABILITY_UNAVAILABLE` when a workflow requires a subsystem disabled by workspace `preset:` / `modules:`).
  - **Specs**: validates prefixes, zero-padded digits, `SchemaDef` frontmatter fields and named `items`, unarchived terminal specs, task/item DAG cycles, cross-spec/cross-workspace `depends_on`, scope globs, and protected paths (`ACDD_SPEC_SCHEMA_INVALID` with structured `diag_code` `SPEC_FIELD_UNKNOWN`, `SPEC_ITEM_UNKNOWN`, or `ITEM_DEPENDENCY_UNKNOWN` on error; omitting `<PATH>` checks the entire workspace).
