---
name: acdd
description: Load for repository onboarding to ACDD, code discovery, symbol lookup, dependency tracing, impact analysis, graph hygiene, or ACDD task operations. Routes navigation, structural search, and task coordination through ACDD MCP.
---

# ContextUnity ACDD

Use `acdd snapshot save|restore|list|clear [<target>] [--all] [--rev N] [--run RUN] [--delete] [--older-than DUR] [--worktree PATH]` or `acdd_state(action: "snapshot", snapshot_action: "save|restore|clear|list", task_ref: ..., name: ..., delete: ..., older_than: ..., worktree: ...)` for task and standalone (`_adhoc/<label>`) checkpoints. See [task snapshots](../../docs/reference/tasks.md#task-snapshots).

Use ACDD's indexed code graph, AST search, documentation graph, and task tools.
Prefer registered MCP tools; use the CLI (`acdd ...`) for offline operations it
owns, and fall back to local search only when ACDD is unavailable. Active tool
schemas are the authority; see [tool catalog](references/tool-catalog.md) for
signatures, [workflow reference](references/workflow-reference.md) for `WorkflowDef`
options, and [routing example](references/agents.md.example) for repo setup.

For repository onboarding or setup, follow the
[onboarding guide](references/onboarding.md). Before authoring or refactoring any
test, load `test-suite-rules`.

## Orient and select workspace

Start with `code_map_overview()` (optionally scoped with `path="..."`) to verify the root, components, languages, and
coverage before assuming a symbol or dependency is absent. Pass `worktree` when
targeting another checkout; omitting `worktree` uses the canonical server root
regardless of active claims.

Worktree selectors resolve explicit paths first, then exact directory name,
literal or zero-normalized numeric prefix, suffix, and substring (first nonempty
tier must be unique). Git-linked worktrees resolve `<primary_worktree>/.acdd/state.sqlite`
and shared locks automatically.

When invoking `acdd` from the CLI (`acdd` on `$PATH`, or a pre-built binary when testing uncommitted schema changes):
- **Avoid `-C` / `cd` repetition**: Omit `-C` when your shell CWD is already the target worktree. From another directory, pass relative `-C .worktrees/<name>` (short alias for `--root`) or set `export ACDD_WORKSPACE_ROOT=<worktree> ACDD_RUN=<run>` once per compound command (`ACDD_WORKSPACE_ROOT` -> `ACDD_WORKTREE`; `ACDD_RUN` is inherited by all `--run` options).
- **Batch read-only queries**: Combine independent `acdd query`/`acdd read`/`acdd docs` calls in one shell command instead of issuing many single-command round trips. Do not wrap `acdd` in shell-compression proxies with low command-size caps or allowlist restrictions.

### CLI-Only / Swarm Quick Reference (exact flags)

```sh
# 1. Orient, search & inspect code (no rg/grep/cat/read_file on indexed code)
acdd query overview
acdd query search "SymbolName" [--path src/] [--kind function] [--classification production] [--limit 15]
acdd query inspect "src/mod.rs:SymbolName" [--show-source]
acdd query explain "src/mod.rs:SymbolName"
acdd query impact "src/mod.rs:SymbolName" --depth 1    # or: acdd query impact --changes unstaged
acdd query tests "src/mod.rs:SymbolName"
acdd query remove "src/mod.rs:SymbolName"
acdd query analyze [src/] --mode hotspots|boundary|diff_impact|cochange [--base main]
acdd ast grep 'fn $NAME($$$ARGS) {}' [--lang rust] [--path src/]

# 2. Layered reads & indexed Markdown docs
acdd read --selector SymbolName [src/mod.rs | --path src/mod.rs] [--detail full]
acdd read src/mod.rs --mode outline|delta|search [--pattern "literal"] [--start-line 1 --line-count 80] [--detail full]
acdd docs search "query terms" [--doc-type architecture]
acdd docs get docs/guide.md [--detail compact | --section "Heading Title"]
acdd docs check

# 3. Hygiene, lint & index refresh
acdd delta src/mod.rs                                  # or: acdd build
acdd lint [src/] --categories all
acdd query run duplicates --level symbol --min-lines 5 --path src/
acdd query run dead_tails --path src/

# 4. State lifecycle, blackboard, scope & worktree exec (omit -C on `acdd exec`)
acdd status && acdd next
acdd claim <node_cursor> --worker <worker_id> [--detail full]
acdd context [<node_cursor>] [--worker <worker_id>] [--detail compact|full]
acdd extend-scope <node_cursor> --path "src/extra/**" --reason "justification"
acdd blackboard --action read [--topic findings|decisions]
acdd blackboard --action post --topic findings --author <id> --node <cur> [--recipient-node <cur>] --payload "<msg>"
acdd verify <node_cursor> [--test <filter>]
acdd submit <node_cursor> --action pass|reject --worker <id> [--evidence @.acdd/tmp/proof.json] [--findings @.acdd/tmp/findings.json]
acdd exec [--run <id|name>] [--worker <worker_id>] -- <program> [args...]
```

## Discovery & read protocol (ACDD-First)

Start every codebase and documentation lookup with ACDD's structural graph and docs tools (MCP when registered, otherwise the `acdd` CLI commands above). Do not use `cat`, `sed`, `head`, `tail`, or raw `sqlite3` on indexed code or docs, and do not default to `read_file`/`rg`/`grep` for symbol or doc discovery. Permitted exceptions: use `rg -n` for gitignored/unadmitted paths (`.acdd/run/**`, `.acdd/tmp/**`, `/tmp/**`, build artifacts), exhaustive literal/comment leftover sweeps after renames or removals, or when ACDD is unavailable; use `read_file` for unadmitted/unindexed files or narrow edit-prep fallback when `acdd_read` is unavailable.

1. **Discover symbols, implementations & syntax (`code_map_search`, `ast_grep_search`, `code_map_query`, `code_map_analyze`)**:
   - Locate definitions by name, prefix, FTS5 tokens, or type signature with `code_map_search(pattern=..., detail="compact")` (`takes:Type`, `returns:Type`, `prefix*`; filter with `kind:`, `classification: "production" | "test" | "documentation"`, `path:` / `file_path:`). Reuse returned `signature`, `inspect_selector`, and automatic previews instead of re-reading files.
   - Match structural syntax with `ast_grep_search` (`$NAME`, `$$$SEQ` over `complete_syntax`, `declaration_without_body`, `fragment_probe`).
   - Query covering tests, removal safety, architectural slices, trait/class implementors, untested surfaces, or custom relations with `code_map_query(operation="tests" | "prove_removal" | "slice" | "implementations" | "untested" | "unwired" | "sql")`.
   - Run read-only architecture and change diagnostics with `code_map_analyze(mode="summary" | "hotspots" | "boundary" | "path" | "entrypoints" | "diff_impact" | "cochange", target=..., to=..., base=...)`.
2. **Trace relationships, callers & blast radius (`code_map_inspect`, `code_map_explain`, `code_map_impact`)**:
   - Call `code_map_inspect` for one declaration's summary, up to 5 direct callers/callees, linked docs, and governing `INV-*` invariants (pass `show_source: true` only when source is missing from previews).
   - Call `code_map_explain` for full paged `incoming`/`outgoing` edges of every kind (`calls`, `imports`, `implements`, `inherits`, `mutates`, `queries_table`, ...).
   - Call `code_map_impact` (`depth: 1`, `direction: "inbound" | "outbound"`, or `changes: "staged" | "unstaged" | "head"`) and `ast_grep_search` across `archive`/`sync`/`restore`/examples/sibling docs before modifying or removing interfaces. `code_map_query(operation="prove_removal")` checks static incoming dependencies (`possible_calls` included) without substituting for runtime tests.
   - Check adjacent modules/formatters for semantic duplicates before adding local grouping/matching/walk helpers, inspect already-populated DTO/domain fields before re-deriving data downstream, split production files into directory modules once approaching ~600-800 LOC instead of piling onto monoliths, and guard byte-index slicing with UTF-8 `is_char_boundary` plus empty-scope fail-closed checks.
   - Call `code_map_query(operation="tests")` to traverse verified `calls` paths from `test_case` nodes (`possible_calls` and lexical suggestions remain hints, not verified coverage).
   - *(Unified aliases: `symbol` aliases `selector` and `file_path` aliases `path` across `inspect`, `explain`, `impact`, `query`, and `acdd_read`.)*
3. **Discover & read documentation (`search_docs`, `get_doc`)**:
   - Search guides, ADRs, and architecture docs with `search_docs(query=..., include_excerpt=true)`.
   - Inspect structure with `get_doc(path_or_id=..., detail="compact")` (accepts `path`, `file_path`, `doc_id`, or `selector`), then read only the needed section via `get_doc(path_or_id=..., section="...")` (pass `include_candidates=true` only when ambiguous symbol candidates are needed). Archived `*/archive/` docs are excluded from default search and emit no graph edges.
4. **Targeted `acdd_read` (`<= 9,000` bytes default; `detail="full"` up to `48 KiB`)**:
   - **Full symbol body (`selector=` / `symbol=`)**: Use `acdd_read(selector="SymbolName")` (optionally `path="..."`) after locating a symbol when its full body is needed beyond `search`/`inspect` previews; follow `source_offset` + `generation`.
   - **File outline (`mode="outline"`)**: Use `acdd_read(path=..., mode="outline")` for a declaration/heading map of a known file (`paths: [...]` batches multiple files with `next_path_index` / `remaining_count` continuation).
   - **Git diff (`mode="delta"`)**: Use `acdd_read(path=..., mode="delta")` to inspect uncommitted/baseline diffs on a known file.
   - **Targeted lines (`mode="lines"`)**: Use for narrow known line coordinates (`<= 9,000` bytes default), and pass `detail="full"` or `output_file` (up to `48 KiB`) when a larger slice is needed rather than sequential line paging. Read Markdown docs (`docs/**`, ADRs, `AGENTS.md`, specs) via `get_doc` / `search_docs`, and use `selector`, `outline`, or `mode="search"` before line ranges.
   - **Literal text search (`mode="search"`)**: Use for literal config values, YAML/TOML keys, or string constants scoped with `path:` (`regex=true|false`); use `rg -n` for gitignored or unadmitted paths.
   - **Tool help (`acdd_guide`)**: Call `acdd_guide` / `acdd guide` for built-in schema, tool, and workflow help.
- **Graph hygiene & unified lint (`duplicates`, `dead_tails`, `code_map_lint`)**: Before submitting `build`,
  `polish`, or any code review, run `code_map_query(operation="duplicates", level="symbol", min_lines=5, path=...)` (asymmetric Probe-vs-Workspace scan promoting newly introduced `>= 0.80` diff clones to `Error` and surfacing graph-ranked `0.45..0.80` `reuse_hints` plus Diff X-Ray untouched siblings on passing `verify`),
  `code_map_query(operation="dead_tails", path=...)` (workspace-wide without
  `path`/`target` on integration reviews), and `code_map_lint` (`acdd lint`; defaults to
  `[arch, syntax, docs]`, or `categories: ["arch","duplicates","dead_tails","cycles","syntax","docs"]` / `--categories all`). `syntax` checks stored parser diagnostics (`syntax.parse`), `fact_budget_exceeded`, and `intra_crate_type_homonym` (using `complexity.severity`, while AST `cognitive_complexity` and `max_nesting_depth` are recorded in `node.details` telemetry); `cycles` isolates multi-node non-test/non-archive module `imports` SCCs (enforceable at `error` via `forbid_cycles`); `dead_tails` computes workspace-wide
  production liveness first, then reports `phantom_tested`, `dead_tails` (`unreachable`),
  `dangling_calls` (`crate::*`/`super::*`/`self::*`), and—when `.acdd/lint/*.yaml` `unused_public_api: true` is configured—`dead_tails.unused_public_api`
  (`error` on unused/test-only `pub use` re-exports and `pub` items lacking non-test
  cross-module production callers). Eliminate or reject new
  clones, phantom-tested dead code, dead tails, dangling calls, unused public compatibility APIs, module import cycles, and `acdd-rule` / doc-integrity lint errors.
- **Executable `acdd-rule` blocks & `ComplexityLimits`**: Pair machine-enforceable `INV-*` invariants in
  docs with fenced `acdd-rule` YAML blocks (or `.acdd/lint/*.yaml` `zones`/`rules`/`complexity`)
  so `code_map_lint` / `code.lint` enforces boundaries and `docs.invariants` surfaces `[ENFORCED]`.
- **SQL & bounds**: Supply one alias per parameter (`sql` or `query` for read-only
  `SELECT`/`WITH` joins over `path_dictionary` and `node_hash`; see
  [tool catalog](references/tool-catalog.md#sql-columns-and-joins-code-mapsqlite)).
  Queries have a 2s budget, 96 KiB cap, and default page size 12 (max 100); continue
  with the returned `next_offset` / `next_source_offset` and `generation` (`>= 8`-char prefix).

## ACDD workflows and task operations

Embedded workflows (`base`, `code`, `documentation`) or repository `.acdd/workflows/default.yaml`
and `.acdd/workflows/<name>.yaml` overlays (`version: 2`, validated via `acdd validate`, plus ready-to-copy `audit`, `spec_planning`, and `hotfix` templates in `references/acdd-workflow-examples/` and spec schema templates in `references/acdd-spec-examples/`) govern gates,
proof schemas, contours, isolation (`--in-place` overrides to `git.isolation: in_place`, `save: none`,
preserving `save.immutable: true`), and `save.report` (written strictly under
`.acdd/reports/*.{md,json}` on pass). Do not assume a hardcoded gate sequence:

| Workflow (`--workflow` / `inherit`) | Purpose & Gate Flow | Typical Launch Command |
|---|---|---|
| `code` (default) | Full software lifecycle (`blueprint_synthesis` -> `tasks` -> `run_tests` -> `polish` -> `integration_review` -> `handoff`) | `acdd start [<spec_path> ...]` or `acdd start --goal "..."` |
| `base` | Minimal worktree-isolated baseline (`gates: []`, `save: commit`) for custom non-code overlays | `acdd start --goal "..." --workflow base` |
| `documentation` | Compiler-free living docs & `acdd-rule` invariant lifecycle gated on `docs.links` + `code.lint` (`[arch, docs]`) | `acdd start [<spec_path> ...] --workflow documentation` or `acdd start --goal "..." --workflow documentation` |
| `audit` (template) | Read-only `in_place` (`save: none`) review/SAST/hygiene (`recon -> analyze -> triage`) writing `.acdd/reports/{run_name}.md` | `acdd start --goal "..." --workflow audit` (or path to template) |
| `spec_planning` (template) | Read-only `in_place` synthesis & review (`synthesize -> review`) aware of `acdd.yaml` `specs.*` configurations | `acdd start --goal "..." --workflow spec_planning` (or path to template) |
| Custom (`.acdd/workflows/<name>.yaml`, e.g. `hotfix`) | Repo-local workflow overlays (`gates_patch:` / `inherit:`), e.g. `hotfix` (`goal.build -> goal.review -> goal.deliver -> run_tests -> handoff`, skipping `blueprint_synthesis` & `integration_review`) | `acdd start --goal "..." --workflow hotfix --scope <path>...` |

1. Inspect cross-worktree specs, dependency waves, critical paths, and scope-overlap markers via CLI `acdd roadmap [spec_id] [--spec NAME] [--all] [--tag TAG] [--field KEY=VALUE] [--group-by AXIS] [--color auto|always|never] [--ready] [--active] [--tasks]` (MCP `acdd_state` is active-run-only and redirects `roadmap`/`draft` to CLI), then start a run with one of the workflow commands above (adding `--in-place`, `--scope <glob>`, or `--worktree` as needed).
2. Inspect ready cursors (`acdd_state(action="next")` / `acdd next`). For `mode: subagent` gates, the coordinator dispatches `(run, node_cursor, worker_id)` and the **worker itself** calls `acdd_claim` / `acdd claim` first before editing files (default `<= 10,000` bytes compact soft cap with `omitted_sections`; pass `detail="full"` for up to `32 KiB`; omit `worktree` under `git.worktrees: managed` so `claim` creates/reuses `.worktrees/<run_name>-<task_ref>`, because passing an explicit `worktree` overrides auto-creation; `session_fingerprint` and full `ClaimResp` bind to the real worker process, which then indexes/syncs `ClaimResp.worktree` via `acdd -C <ClaimResp.worktree> build` or MCP `worktree` before reading/querying code and works strictly inside `ClaimResp.worktree`); the coordinator calls `acdd_claim` directly only for `mode: inline` gates or dead-worker recovery.
3. Follow `ClaimResp` (`contract`, `guidance`, `unresolved_findings`, `pre_task_slice`
   including `predecessor_proofs`, `blackboard`, and `context_lenses`). Stay within
   admitted write scope (`in_write_scope: ALLOWED`); extend autonomously via
   `acdd_state(action="extend_scope", ...)` (`acdd extend-scope`) inside module
   roots when needed. Keep scratch files in `.acdd/tmp/`. When updating
   documentation or running a pre-handoff documentation scan in a repository
   that maintains a `website/` site (`website/pages/`, `website/templates/`,
   `website/sync-docs.py`), reconcile affected `website/` sources in the same
   scope and verify `./website/build.sh --check` (`sync-docs.py && zola build && zola check`).
4. Verify via `acdd_verify(node=..., test_target=...)` (`acdd verify <cursor> [--test <filter>]`)
   before submitting command/check gates so `execution_digest` and candidate trees bind.
5. Submit via `acdd_submit` / `acdd submit` (`pass` or `reject`). For large JSON proofs (`> 8 KiB`, e.g. `blueprint`), write `.acdd/tmp/<name>.json` and submit via `acdd submit <cursor> --action pass --evidence @.acdd/tmp/<name>.json`, keeping `tasks` keys as plain unquoted `task_ref` IDs (no extra inner quotes or `{}`). Note `acdd run`
   requires shell `runner:` commands on roles (`WORKFLOW_RUNNER_REQUIRED` when roles
   use `mode: subagent` without `runner:`); subagent orchestration coordinates
   explicit `claim` / `verify` / `submit` steps. On `TASK_CANDIDATE_MISMATCH`,
   restore via Git or `acdd reopen` and renew verification/review.



## Context refresh after compaction

When a host session compacts context (Claude `PostCompact` or Codex `SessionStart(compact)` hooks), reload gate authority without re-claiming:

```bash
acdd context [<node>] [--run <name>] [--worker <id>] [--detail compact|full]
```

MCP equivalent:

```json
{"action": "context", "node": "T-03.build", "worker_id": "builder", "detail": "compact", "format": "json"}
```

This call is strictly read-only (`SQLITE_OPEN_READ_ONLY`) and must not create `.acdd/state.sqlite` or mutate `claim_revision`.

## Blackboard

Topics are `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred`
(`action: "read" | "post"`). Use `notes` only for non-obvious technical context—do
not duplicate routine passing checks or gate status. Record accepted architectural
decisions (`decisions`), blocking constraints (`blockers`), and non-blocking
follow-ups (`deferred` with an owner) before delivery; blackboard supplements
authority and never replaces proof.

Before rejecting a review, post structured `findings` naming the affected code,
defect, and concrete remediation, then reference that returned message ID in
`rejection_findings.blackboard_message_id` on `acdd_submit(action="reject")` (with
`kind: "defect"` for `fail:` or `"architectural_drift"` for `fail_drift:` context reset).
After claim, `ClaimResp.unresolved_findings` carries open findings while
`ClaimResp.blackboard` carries active context (`decisions`, `blockers`, untargeted/current-gate
messages); use `acdd_blackboard(action="read", ...)` (`limit` default `50`, max `100`)
if a message payload was truncated.
