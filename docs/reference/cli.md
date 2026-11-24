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
| `--profile <REF>` | Explicit gate profile name (`base \| code \| audit \| documentation \| spec_planning`) or root-confined YAML path (`[:<sha256_prefix>]`); `ACDD_PROFILE` can set it. |
| `--format markdown|json` | Output format for command results. Defaults to `markdown`; invalid values fail before a command runs. |

- **Positional root**: `build`, `scan`, and `delta` also accept a positional workspace path (when combined with `--root`, both must resolve to the same directory).
- **Flag precedence**: CLI flags override matching `ACDD_*` environment variables once per invocation.
- **Profile precedence**: `--profile` / `ACDD_PROFILE` → spec `profile:` → `specs.<collection>.profile` → `profiles_dirs` → `.acdd/profile.yaml` → built-in `code` (plus `base`, `audit`, `documentation`, `spec_planning`).
- **State DB resolution**: `--state-db` → `state_db` in `acdd.yaml` → `<primary_worktree>/.acdd/state.sqlite` (resolved via `--git-common-dir`).
- **Runner vs. command environments**: spawned role runners receive `ACDD_CONFIG`, `ACDD_PROFILE`, `ACDD_DB`, `ACDD_STATE_DB`, and `ACDD_AGENT_SESSION_ID`; project build/test commands run in a sanitized environment with internal `ACDD_*` variables scrubbed.
- **`serve` & `delta` config retention**: `serve` keeps its resolved config/profile across reloads; `delta` reuses the persisted index adapter unless `--config` is passed explicitly.

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

- **`read <path>`**: shares the same layered reader and budgets as MCP `acdd_read` (see [layered file reader](mcp-tools.md#layered-file-reader)).
- **Auto mode**: selects `snippet` when `--selector` is given, `search` when `--pattern` is given, and `lines` otherwise (or pass a symbol selector directly with `--mode snippet`).
- **Index requirements**: `lines`, `raw`, `search`, and `delta` run without an index; Markdown `outline` requires `docs`; code `outline` and `snippet` require the code index.
- **Symbol paging**: bound output with `--max-body-lines` and `--leading-lines`, and continue across pages using `--source-offset` + `--generation`.

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
- **`query run <op>`**: executes `overview`, `inspect`, `explain`, `impact`, `slice`, `unwired`, `sql`, `untested`, `implementations`, `duplicates`, or `dead_tails`.
- **`query analyze`**: accepts an indexed path, empty workspace target, or one read-only `SELECT`/`WITH` statement (computes cycles in CLI diagnostic mode; MCP uses `include_cycles: true`).
- **`query inspect --show-source`**: returns the digest-checked symbol body; continue with `--source-offset` and the previous page's `--generation`. Source pages break strictly on line boundaries (see [limits and freshness](../runbooks/limits-and-freshness.md)).
- **`--format markdown|json`**: global output flag valid before or after any subcommand; Markdown renders verified vs. candidate edge evidence, while JSON preserves exact structured payloads.
- **`analysis_gaps` & explain edges**: `inspect` and `explain` include paged `analysis_gaps` (reason, path, line, scope, evidence) plus `calls` (exact) and `possible_calls` (`possible`, bounded branch joins), including `trait_object_dispatch` and `macro_expansion_not_analyzed`.

## Documentation and syntax search

```sh
acdd docs search 'dependency' --doc-type architecture
acdd docs get 'docs/architecture/indexing.md' --section 'Ownership and data flow'
acdd ast grep 'print($VALUE)' --lang python --path src
```

- **`docs search` / `docs get`**: query indexed Markdown sections (`--component`, `--doc-type`, `--limit`).
- **`ast grep`**: narrows candidate files from the index, verifies file digests, and matches Tree-sitter patterns (`--limit 1..=100` matches MCP page objects; `101..=10000` returns CLI-only aggregate matches).

### Unified architectural and documentation lint (`acdd lint`)

```sh
acdd lint
acdd lint crates/acdd-state --categories arch,docs --severity warn
acdd lint --rule INV-CORE-LEAF --format json
acdd lint --categories all --limit 50 --offset 0
```

`acdd lint [TARGET]` runs the unified language-agnostic lint engine over `.acdd/code-map.sqlite` (matching MCP `code_map_lint`):
- `--categories`: comma-separated `arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`, or `all` (omitted defaults to `arch,syntax,docs`). `syntax` evaluates stored parser diagnostics (`syntax.parse`), `syntax.fact_budget_exceeded`, and `syntax.intra_crate_type_homonym` (using `architecture.complexity.severity`, while `cognitive_complexity` and `max_nesting_depth` are recorded as `node.details` telemetry); `cycles` inspects multi-node non-test/non-archive module `imports` SCCs (enforceable as errors via `forbid_cycles` rules); `duplicates` inspects AST-statement clones (with Type-2 local-identifier normalization and test exclusion by default); `dead_tails` computes workspace-wide production liveness and reports `dead_tails.phantom_tested`, `dead_tails.unreachable`, `dead_tails.dangling_call` (`error`), and—when `architecture.unused_public_api: true` is enabled—`dead_tails.unused_public_api` (`error` on unused/test-only `pub use` re-exports and internal `pub` items lacking non-test cross-module production callers).
- `--rule <ID>`: repeat or pass comma-separated rule/invariant IDs to filter evaluated diagnostics.
- `--severity <warn|error>`: minimum diagnostic severity threshold.
- `--limit`, `--offset`, `--generation`, `--detail <compact|full>`: standard generational paging matching `code_map_lint`.
- Exits non-zero when `summary.errors > 0`.

## Unified state lifecycle

Flat `Run` and `GateProfile` commands backed by `crates/acdd-state` and `src/state_bridge/` (see [state, tasks & lifecycle](tasks.md) for schemas, locks, and error codes).

### Workspace initialization and specification drafting

```sh
acdd init [--force]
acdd draft <collection> <slug> [--prefix PREFIX] [--title "Title"] [--profile PROFILE]
```

- `init [--force]`: Writes starter `acdd.yaml`, `.acdd/profile.yaml` (`inherit: code` when `code` module is enabled, otherwise `inherit: base`), and the canonical `.acdd/` protection block in `.gitignore`, verifying via `git check-ignore` that `.acdd/state.sqlite` is ignored and `.acdd/profile.yaml` stays trackable.
- `draft <collection> <slug>`: Creates the next sequentially zero-padded specification Markdown file in the configured `specs.<collection>` directory (`milestones` or `plans` by default).

### Run startup, inspection, and ready queue

```sh
acdd start [<spec>] [--goal TEXT] [--name RUN_NAME] [--profile PROFILE] [--scope PATH ...] [--target-base REF] [--worktree | --in-place] [--worktree-path PATH]
acdd status [--run RUN_ID_OR_NAME]
acdd list [--workspace NAME] [--status active|completed|cancelled] [--include-tombstones] [--limit 1..100] [--offset N] [--generation GEN]
acdd show [<node>] [--run RUN_ID_OR_NAME]
acdd context [<node>] [--run RUN_ID_OR_NAME] [--worker WORKER_ID]
acdd next [--run RUN_ID_OR_NAME] [--limit N]
acdd roadmap [spec_id] [--collection NAME] [--ready] [--active] [--tasks]
```

- `start`: Starts a new `Run` from a spec file (`<spec>`) or an ephemeral `--goal` (materializing `.acdd/run/<run_id>/spec.md`, attaching `--scope` globs as task scope or frontmatter `owners:` on taskless profiles like `audit`). Selects `worktree` (`--worktree`, `--worktree-path`, `--target-base`) or `--in-place` isolation (neutralizing `save.post: commit` to `none` while preserving `save.immutable: true`).
- **`status`**: renders active run summary and current gate status.
- **`list`**: lists runs (`--include-tombstones` includes cleared/completed/cancelled `run_tombstones` with `runs.generation` paging).
- **`show`**: renders the full run tree or one dotted node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`).
- **`next`**: lists currently ready node cursors and topological execution waves.
- **`context`**: reads `.acdd/state.sqlite` (`SQLITE_OPEN_READ_ONLY`) to refresh `ClaimResp` + blackboard without mutating claims or advancing `claim_revision` (ideal for `PostCompact` / `SessionStart(compact)` hooks).
- **`roadmap`**: displays cross-collection runs, dependency waves, critical-path order, and scope overlaps (`--tasks` / `<spec_id>` drills down; never creates `state.sqlite`).

### Gate execution: `claim`, `verify`, `submit`

```sh
acdd claim [<node>] [--run RUN] [--worker ID] [--session-fingerprint FP] [--force] [--steal-remote] [--worktree PATH]
acdd verify <node> [--run RUN] [--worker ID] [--test TARGET] [--claim-revision REV] [--worktree PATH]
acdd submit <node> --action pass|reject [--run RUN] [--worker ID] [--claim-revision REV] [--test TARGET] [--evidence JSON_OR_@PATH] [--findings JSON_OR_@PATH] [--invariant ID ...] [--summary TEXT] [--allow-main] [--worktree PATH]
```

- `claim`: Claims `<node>` (or auto-selects the next ready cursor when `<node>` is omitted) under optimistic concurrency (`claim_revision` + `node_version`), returning the 6-tier budgeted `ClaimResp` (`contract` + `guidance`, `unresolved_findings`, `pre_task_slice`, `dependency_receipts`, `blackboard`, `context_lenses`). Pass `--force` to take over an active claim (`--steal-remote` required across different host session fingerprints).
- `verify`: Executes the node's `command:`, `call:`, `checks:` (`DomainProvider`), and write-scope perimeter barrier without advancing state. `--test` passes the targeted test filter to `{test_filter}` and `code.seam` (for example on `build` gates).
- **`submit`**: submits `--action pass` or `--action reject` for `claim_revision`:
  - Auto-triggers verification on `command` gates when cached proof is absent; requires `--evidence` on review gates and structured `--findings` (`kind`: `defect | architectural_drift`) on `reject`.
  - Creates scoped plumbing commits on `receipt: true` + `save.post: commit` (blocking `main`/`master` unless `--allow-main`) and materializes `save.report` (`report_path`).

### Scope, reopening, sync, subtasks, and blackboard

- **`acdd snapshot <save|restore|list|clear>`**: manages Git checkpoints:
  - `--run RUN`: keeps per-task refs (`refs/acdd/snapshots/<run_id>/<task_ref>/r<rev>`); omitting `--run` resolves unambiguously matching active tasks, otherwise standalone `refs/acdd/snapshots/_adhoc/<label>/r<rev>`.
  - `--delete` / `clear`: supports `--rev N`, `--older-than <N><s|m|h|d|w>`, or `--all` (using `--rev`/`--older-than` without `--run` targets standalone refs; see [task snapshots](tasks.md#task-snapshots)).

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

### Run cleanup and integration

```sh
acdd clear [<run_id>] [--run RUN] [--reason TEXT] [--force]
```

- `clear`: Cancels/clears a run into `run_tombstones(terminal_status = 'cleared')` and cleans managed worktrees (`--force` overrides dirty worktree checks).
- Task branch integration: Headless `acdd run` waves land completed task branches internally via `rebase-ff` (`integrate_task_branch`); manual worktree runs integrate via standard `git merge` or `git rebase`, and terminal verification confirms content equivalence by Git `TreeOid` (`TASK_NOT_INTEGRATED`).

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

- `serve` / `reload`: Starts the stdio MCP server (20 built-in tools + active `DomainProvider` `<domain>_<name>` plugin tools) or reloads running MCP server processes via `SIGHUP`.
- `guide`: Returns capability-filtered guidance across the 11 topics (`query`, `init`, `adapter`, `validate`, `code`, `ast`, `docs`, `lifecycle`, `acdd`, `gates`, `subtasks`).
- `tool`: Invokes a `<domain>_<name>` plugin tool exposed by an active `DomainProvider` with JSON arguments validated against `DomainToolDef.input_schema`.

## Validation

```sh
acdd validate [<PATH>]
```

- **`validate [<PATH>]`**: runs the unified workspace validator (`acdd_state::validate` + `acdd_state::usecases::deps`):
  - **Config (`acdd.yaml`)**: validates schema keys (`load_adapter_report`, rejecting unknown keys via `#[serde(deny_unknown_fields)]`), `specs:` collections, and `linked_workspaces` paths.
  - **Profiles (`.acdd/profile.yaml`)**: verifies inheritance, fields, bound commands (when `modules.state` is enabled), and `DomainProvider` capabilities.
  - **Specs**: validates collection prefixes, zero-padded digits, unarchived terminal specs, task DAG cycles, cross-spec/cross-workspace `depends_on`, scope globs, and protected paths (omitting `<PATH>` checks the entire workspace).
