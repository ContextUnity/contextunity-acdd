---
doc_type: api
title: CLI reference
---

# CLI reference

`acdd` provides a command-line interface and a stdio MCP server. Run `acdd --help` or append `--help` to any command for accepted arguments. Commands other than `serve` print readable Markdown by default. Pass global `--format json` to retain machine-readable JSON output.

## Global options

| Option | Behavior |
| --- | --- |
| `--root <PATH>` | Workspace for all commands. Defaults to the current directory; `ACDD_WORKSPACE_ROOT` can set it. |
| `--db <PATH>` | Code-index path, relative to the selected root. Defaults to `<root>/.acdd/code-map.sqlite`; `ACDD_DB` can set it. Does not select the state store. |
| `--state-db <PATH>` | State SQLite database path override (`ACDD_STATE_DB`). Defaults to `state_db` in `acdd.yaml` (`.acdd/state.sqlite` under the primary Git worktree). |
| `--config <PATH>` | Root-confined ACDD YAML configuration; `ACDD_CONFIG` can set it. Explicit missing files fail. Defaults to `<root>/acdd.yaml` when present. |
| `--profile <REF>` | Explicit gate profile name (`default \| code \| in_place \| workspace`) or root-confined YAML path (`[:<sha256_prefix>]`); `ACDD_PROFILE` can set it. |
| `--format markdown|json` | Output format for command results. Defaults to `markdown`; invalid values fail before a command runs. |

`build`, `scan`, and `delta` also accept a positional workspace path. Either form selects the workspace; when both are supplied, they must resolve to the same directory.

Selections resolve once per invocation. Flags override their environment variables.
Profile resolution checks `--profile` / `ACDD_PROFILE`, spec frontmatter `profile:`,
`specs.<collection>.profile` in `acdd.yaml`, `profiles_dirs`, `.acdd/profile.yaml`,
and embedded baselines (`default`, `code`, `in_place`).
State-store resolution uses `--state-db`, `state_db` in `acdd.yaml`, and
`<primary_worktree>/.acdd/state.sqlite` discovered via `git rev-parse --path-format=absolute --git-common-dir`.
Owned runner children (agent sessions claiming gates) receive selected paths and
session identity (`ACDD_CONFIG`, `ACDD_PROFILE`, `ACDD_DB`, `ACDD_STATE_DB`, `ACDD_AGENT_SESSION_ID`)
without mutating the parent environment. Command processes (project build, lint,
and test commands executed by command gates) execute in a sanitized environment
where internal ACDD orchestration variables are scrubbed, preventing cross-workspace
configuration pollution. Concurrent in-process invocations retain separate selections.

`serve` retains the selected config and profile for its lifetime, including
module visibility, task loaders, request policy, refreshes and reloads. Selected
worktrees rebase root-relative settings. `delta --config` validates the selected
adapter and uses the guarded rebuild path when policy changes; without an
explicit config, delta retains the persisted index adapter. Missing or malformed
explicit selections fail before index publication.

## Inventory and indexing

```sh
acdd scan .
acdd build .
acdd build /path/to/repository --output /path/to/index.sqlite
acdd delta . src/module.py src/other.py
```

`scan` reports admitted files without writing an index. `build` creates a full index. `delta` updates the listed changed or deleted paths in an existing index. `build` and `scan` accept `--adapter <PATH>` for a configuration file inside the workspace; reads use the selected configuration. Conflicting `--adapter` and `--config` paths fail.

`query`, `docs`, and `ast grep` open the index through `open_or_rebuild`: a missing or schema-incompatible index is built automatically. An existing index is not refreshed just because source files changed; run `build` or `delta` to update those results.

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

`read` uses the same layered reader as MCP `acdd_read`. The positional path is
required; `--selector` selects a symbol within that file. Alternatively, pass a
symbol selector as the positional value with `--mode snippet`. Auto mode selects
snippet for `--selector`, search for `--pattern`, and lines otherwise.
Core text modes run without an index. Markdown outline requires docs; code
outline and snippet require codemap. Use `--max-body-lines` and `--leading-lines`
to bound symbol output, then `--source-offset` with the previous `--generation`
for continuation. See the [reader contract](mcp-tools.md#layered-file-reader) for
text bounds, exports, sensitive paths, task badges, and conditional hints.

## Queries

```sh
acdd query overview
acdd query overview --aspects entrypoints
acdd query overview --aspects entrypoints_with_tests
acdd query overview --aspects topology
acdd query search 'parse*' --kind function --limit 30
acdd query search 'takes:GateProfile'
acdd query search 'returns:Result'
acdd query inspect 'src/module.py:parse' --show-source
acdd query explain 'src/module.py:parse'
acdd query impact 'src/module.py:parse' --depth 1
acdd query impact --changes staged --depth 1
acdd query tests 'src/module.py:parse' --direction inbound
acdd query remove 'src/module.py:parse'
acdd query analyze 'SELECT name FROM nodes ORDER BY name'
acdd query run slice 'src/module.py:parse' --depth 1 --limit 30
acdd query run slice src/db --limit 30
acdd query run untested src/core --limit 30
acdd query run implementations LanguageLinker --limit 30
acdd query run duplicates --level symbol --min-lines 10 --limit 30
acdd query run duplicates src --level file --limit 30
acdd --format json query overview
```

`query overview --aspects` accepts comma-separated aspect names. `entrypoints` excludes tests; `entrypoints_with_tests` includes them in the same `entrypoints` result. Repeat the chosen aspect and use `--limit`, `--offset`, and the previous page's `--generation` to continue a bounded aspect page.

`topology` is opt-in and returns paged module statistics and dependency edges;
see the [MCP reference](mcp-tools.md#selectors-and-graph-queries) for grouping
and role rules. Directory slices include bounded inbound and outbound boundary
summaries. CLI and MCP read the same index and return the same graph data.

`query impact` defaults to one edge and accepts exactly one symbol selector or `--changes staged|unstaged|head`; `--path` can restrict a change view to a workspace-relative path. Use `--limit` from 1 to 100 (default 100) for page size and `--offset` to request a later page; a nonzero offset requires the preceding page's `--generation`. A change view returns changed symbols, impacted nodes and tests, plus explicit unmapped changes. It maps ranges only when the selected Git postimage matches the indexed file, and sets `complete` to false when some changes cannot be mapped. Its continuation generation binds the graph and captured Git change snapshot. Depth-one results include the selected edge on each non-seed node; full detail includes stored edge evidence. `query run impact` keeps its depth 2 default; both accept `--depth` to override it. `query tests` also accepts `--direction outbound` for dependencies of a test. `query run` accepts `overview`, `inspect`, `explain`, `impact`, `slice`, `unwired`, `sql`, `untested`, `implementations`, and `duplicates`. `query analyze` accepts an indexed path, an empty target for the workspace, or one read-only `SELECT`/`WITH` statement. Its diagnostic form computes cycles; the MCP form requires `include_cycles: true`.

The CLI `query inspect --show-source` returns the indexed symbol's source range after checking the file digest. Use `--source-offset` with `--generation` from the previous source page to continue. CLI and MCP source continuations always require that explicit token; missing or stale tokens return an error and a restart hint. Source pages stop at complete-line boundaries; an individually oversized line returns a budget error rather than partial code. See [limits and freshness](../runbooks/limits-and-freshness.md).

`--format` is global and can be placed before or after a subcommand. Markdown
uses the shared operation-aware renderer and keeps traversal evidence visible:
explain distinguishes exact calls from candidate relationships, deeper impact
paths retain their available predecessor evidence, and graph-backed test results
show ordered, linked test-to-target paths with only producer-supplied roles.
Lexical test suggestions remain explicitly unverified. JSON mode preserves the
returned semantic value for clients that parse stdout.

`query inspect` and `query explain` include paged `analysis_gaps` for the selected source span. Each gap names its reason, path, line, scope symbol, and resolver evidence. Explain edge rows retain both public endpoint IDs, kind, path, line, and direction relative to the query; full detail also includes stored evidence. Rust trait-object calls report `trait_object_dispatch`; indexed macro invocations report `macro_expansion_not_analyzed` while retaining their macro-declaration navigation edge. Exact call edges use `calls`; bounded branch-join navigation uses `possible_calls` with confidence `possible`. Inbound impact follows possible calls, and removal analysis treats candidate callers as incoming dependencies.

## Documentation and syntax search

```sh
acdd docs search 'dependency' --doc-type architecture
acdd docs get 'docs/architecture/indexing.md' --section 'Ownership and data flow'
acdd ast grep 'print($VALUE)' --lang python --path src
```

`docs` reads indexed documents. `ast grep` reads admitted file paths from the index, narrows them with FTS terms, and parses matching source after checking its indexed digest. Both can trigger the missing or incompatible-index rebuild described above; `ast grep` also requires a compiled language profile. `docs search` accepts `--component` and `--limit`; `ast grep` accepts `--limit`.

For `ast grep`, a `--limit` from 1 through 100 (default 100) returns the same
JSON `matches` page object as MCP AST search, including applicable continuation
and total-status fields. CLI limits 101..10000 retain the aggregate `matches`
array, `limit`, and `truncated` result. That larger CLI-only range is outside
shared page parity.

## Unified state lifecycle (`0.6.1` flat commands)

`0.6` replaces nested `task` / `plan` / `workflow` / `milestone` / `checkpoint`
commands with one flat `Run` / `GateProfile` lifecycle CLI backed by
`crates/acdd-state` and `src/state_bridge/`. See [state, tasks & lifecycle](tasks.md)
for schemas, state machines, locks, candidate verification, and error codes.

### Workspace initialization and specification drafting

```sh
acdd init [--force]
acdd draft <collection> <slug> [--prefix PREFIX] [--title "Title"] [--profile PROFILE]
```

- `init [--force]`: Writes starter `acdd.yaml`, `.acdd/profile.yaml` (`inherit: code` when `code` module is enabled, otherwise `inherit: default`), and the canonical `.acdd/` protection block in `.gitignore`, verifying via `git check-ignore` that `.acdd/state.sqlite` is ignored and `.acdd/profile.yaml` stays trackable.
- `draft <collection> <slug>`: Creates the next sequentially zero-padded specification Markdown file in the configured `specs.<collection>` directory (`milestones` or `plans` by default).

### Run startup, inspection, and ready queue

```sh
acdd start [<spec>] [--goal TEXT] [--name RUN_NAME] [--profile PROFILE] [--scope PATH ...] [--target-base REF] [--worktree | --in-place] [--worktree-path PATH]
acdd status [--run RUN_ID_OR_NAME]
acdd list [--workspace NAME] [--status active|completed|cancelled] [--include-tombstones] [--limit 1..100] [--offset N] [--generation GEN]
acdd show [<node>] [--run RUN_ID_OR_NAME]
acdd next [--run RUN_ID_OR_NAME] [--limit N]
```

- `start`: Starts a new `Run` from a spec file (`<spec>`) or an ephemeral `--goal` (materializing `.acdd/run/<run_id>/spec.md` with explicit repeatable `--scope` globs). Selects `worktree` (`--worktree`, `--worktree-path`, `--target-base`) or `--in-place` isolation according to the resolved `GateProfile`.
- `status`, `list`, `show`, `next`: Read-only queries (`StateQuery`). `list` supports `--include-tombstones` for cleared/completed/cancelled `run_tombstones` and stable `runs.generation` paging. `show` renders the full run tree or a specific node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`). `next` lists currently schedulable node cursors and topological execution waves.

### Gate execution: `claim`, `verify`, `submit`

```sh
acdd claim [<node>] [--run RUN] [--worker ID] [--session-fingerprint FP] [--force] [--steal-remote] [--worktree PATH]
acdd verify <node> [--run RUN] [--worker ID] [--test TARGET] [--claim-revision REV] [--worktree PATH]
acdd submit <node> --action pass|reject [--run RUN] [--worker ID] [--claim-revision REV] [--test TARGET] [--evidence JSON_OR_@PATH] [--findings JSON_OR_@PATH] [--invariant ID ...] [--summary TEXT] [--allow-main] [--worktree PATH]
```

- `claim`: Claims `<node>` (or auto-selects the next ready cursor when `<node>` is omitted) under optimistic concurrency (`claim_revision` + `node_version`), returning the 6-tier budgeted `ClaimResp` (`contract`, `guidance`, `diff`, `context`, `blackboard`, `traces`). Pass `--force` to take over an active claim (`--steal-remote` required across different host session fingerprints).
- `verify`: Executes the node's `command:`, `call:`, `checks:` (`DomainProvider`), and write-scope perimeter barrier without advancing state. `--test` passes the targeted test filter to `{test_filter}` and `code.seam` (required on `proof: contract`).
- `submit`: Submits `--action pass` or `--action reject` for `claim_revision`. On `command`/`contract` gates, omitting cached proof automatically triggers verification. Review gates require `--evidence` (JSON or `@path`); `reject` requires structured `--findings` (JSON or `@path`, with optional `kind`: `defect | architectural_drift`). Passing `receipt: true` with `save.post: commit` creates the scoped delivery commit (refusing `main`/`master` unless `--allow-main`).

### Scope, reopening, sync, subtasks, and blackboard

```sh
acdd extend-scope <node> --path PATH [--path PATH ...] --reason TEXT [--run RUN] [--worker ID]
acdd reopen <node> [--run RUN] [--cascade] [--reason TEXT]
acdd sync [--run RUN] [--rebind-profile]
acdd subtask <task_ref> <subtask_ref> --status pending|in_progress|completed|cancelled [--evidence TEXT] [--run RUN]
acdd blackboard [--action read|post] [--run RUN] [--node CURSOR] [--recipient-node CURSOR] [--subtask-ref REF] [--gate GATE] [--author WORKER] [--topic draft|notes|findings|blockers|decisions|deferred] [--payload TEXT] [--status open|acknowledged|resolved|carried_over] [--limit N] [--offset N] [--generation GEN]
```

- `extend-scope`: Expands `admitted_scope` after conflict checks against other claimed incomplete tasks (blocking `.acdd/**` and `protected_paths` with `OUT_OF_SCOPE_CHANGES`).
- `reopen`: Reopens a gate or task container; `--cascade` marks downstream `in_progress`/`completed` dependent tasks stale.
- `sync`: Reconciles spec edits into an active run; `--rebind-profile` rebinds non-completed gates against the latest profile.
- `subtask`: Transitions subtask state; terminal `completed` or `cancelled` requires non-empty `--evidence`.
- `blackboard`: Reads or posts scoped collaboration messages (`open | acknowledged | resolved | carried_over`) across topics `draft | notes | findings | blockers | decisions | deferred`.

### Rollback, integration, and cleanup

```sh
acdd rollback <target> [--run RUN] [--node CURSOR] [--worktree PATH]
acdd integrate [<task_ref>] [--run RUN] [--strategy manual|rebase-ff|ff-only|no-ff|squash] [--allow-main]
acdd clear [<run_id>] [--run RUN] [--reason TEXT] [--force]
```

- `rollback`: Confined-restores working-tree files from `<target>` (commit ref, commit OID, or tree OID), restricted to `--node`'s `admitted_scope` when specified.
- `integrate`: Integrates one completed task branch (or all unintegrated completed tasks) into the run branch using the selected strategy (refusing `main`/`master` unless `--allow-main`).
- `clear`: Cancels/clears a run into `run_tombstones(terminal_status = 'cleared')` and cleans managed worktrees (`--force` overrides dirty worktree checks).

### Headless run execution

```sh
acdd run [--run RUN] [--jobs N] [--allow-main] [--dry-run]
```

`acdd run` drives the active `Run` across topological DAG waves using the
resolved `GateProfile`:
- Executes mechanical `command:` / `call:` gates directly via `verify` + `submit`.
- Spawns configured `roles.<role>.runner` processes for role-assigned gates (clamped to `--jobs 1` when `git.isolation: in_place`), streaming child output and collecting accepted submissions.
- `--dry-run` computes schedulable waves without mutating storage or spawning runners.
- Role-assigned gates without a configured `runner:` command fail closed with `WORKFLOW_RUNNER_REQUIRED` (manual/subagent orchestration uses `claim` -> `verify` -> `submit` directly).

## Server, guide, and plugin tools

```sh
acdd --root /path/to/repository serve
acdd reload
acdd guide init [--force]
acdd guide query
acdd guide lifecycle --format json
acdd guide acdd
acdd guide gates
acdd guide subtasks
acdd guide code
acdd guide docs
acdd tool <domain>_<name> [--args JSON] [--run RUN] [--node CURSOR] [--worktree PATH]
```

- `serve` / `reload`: Starts the stdio MCP server (19 built-in tools + active `DomainProvider` `<domain>_<name>` plugin tools) or reloads running MCP server processes via `SIGHUP`.
- `guide`: Returns capability-filtered guidance across the 11 topics (`query`, `init`, `adapter`, `validate`, `code`, `ast`, `docs`, `lifecycle`, `acdd`, `gates`, `subtasks`).
- `tool`: Invokes a `<domain>_<name>` plugin tool exposed by an active `DomainProvider` with JSON arguments validated against `DomainToolDef.input_schema`.

## Validation

```sh
acdd validate [<PATH>]
```

- `validate [<PATH>]`: Routes through the single canonical validator (`acdd_state::validate` + `acdd_state::usecases::deps`) to validate `acdd.yaml` (`load_adapter_report`, rejecting legacy `0.5.x` keys with `CONFIG_MIGRATION_REQUIRED`), active `.acdd/profile.yaml` (requiring bound commands when `modules.state` is enabled), `DomainProvider` capabilities, and spec files fail-closed (collection prefixes, zero-padded digits, unarchived terminal specs, task DAG cycles, cross-spec/cross-workspace `depends_on`, scope globs, and protected paths). When `<PATH>` is omitted, validates the full workspace configuration, active profile, and configured `specs:` collections. For manual upgrading from `0.5.x`, see the [0.6.0 migration guide](../runbooks/migration-0.6.md).
