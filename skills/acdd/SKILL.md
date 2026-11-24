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
signatures, [profile reference](references/profile-reference.md) for `GateProfile`
options, and [routing example](references/agents.md.example) for repo setup.

For repository onboarding or setup, follow the
[onboarding guide](references/onboarding.md). Before authoring or refactoring any
test, load `test-suite-rules`.

## Orient and select workspace

Start with `code_map_overview()` to verify the root, components, languages, and
coverage before assuming a symbol or dependency is absent. Pass `worktree` when
targeting another checkout; omitting `worktree` uses the canonical server root
regardless of active claims.

Worktree selectors resolve explicit paths first, then exact directory name,
literal or zero-normalized numeric prefix, suffix, and substring (first nonempty
tier must be unique). Git-linked worktrees resolve `<primary_worktree>/.acdd/state.sqlite`
and shared locks automatically.

When invoking `acdd` from the repository root (`acdd` on `$PATH`), pass relative
worktree flags instead of repeating `cd /abs/path && /abs/bin/acdd`:

```sh
# Refresh, read, query, or lint an isolated worktree without changing CWD:
acdd --root .worktrees/<run_name> delta
acdd --root .worktrees/<run_name> query search "SymbolName" --limit 10
acdd --root .worktrees/<run_name> read src/lib.rs --start-line 1 --line-count 80
acdd --root .worktrees/<run_name> lint --categories all

# Target the worktree for gate operations (reserve cd for git/cargo):
acdd claim <node_cursor> --run <run> --worker <worker_id> --worktree .worktrees/<run_name>
acdd submit <node_cursor> --action pass --run <run> --worker <worker_id> --worktree .worktrees/<run_name>
```

## Discovery & read protocol (Graph-First)

Always investigate codebases and documentation using ACDD's structural graph and docs tools **first**. Do **not** use `acdd_read(mode="search")` or sequential `acdd_read(start_line=...)` slices as a replacement for `grep` or blind file browsing.

1. **Discover symbols, implementations & syntax (`code_map_search`, `ast_grep_search`, `code_map_query`)**:
   - Locate definitions by name, prefix, FTS5 tokens, or type signature with `code_map_search(pattern=..., detail="compact")` (`takes:Type`, `returns:Type`, `prefix*`; filter with `kind:`, `classification: "production" | "test" | "documentation"`, `path:` / `file_path:`). Reuse returned `signature`, `inspect_selector`, and automatic previews instead of re-reading files.
   - Match structural syntax with `ast_grep_search` (`$NAME`, `$$$SEQ` over `complete_syntax`, `declaration_without_body`, `fragment_probe`).
   - Query architectural slices, trait/class implementors, untested surfaces, or custom relations with `code_map_query(operation="slice" | "implementations" | "untested" | "unwired" | "sql")`.
2. **Trace relationships, callers & blast radius (`code_map_inspect`, `code_map_explain`, `code_map_impact`, `code_map_tests`)**:
   - Call `code_map_inspect` for one declaration's summary, up to 5 direct callers/callees, linked docs, and governing `INV-*` invariants (pass `show_source: true` only when source is missing from previews).
   - Call `code_map_explain` for full paged `incoming`/`outgoing` edges of every kind (`calls`, `imports`, `implements`, `inherits`, `mutates`, `queries_table`, ...).
   - Call `code_map_impact` (`depth: 1`, `direction: "inbound" | "outbound"`, or `changes: "staged" | "unstaged" | "head"`) before modifying or removing interfaces. `code_map_prove_removal` checks static incoming dependencies (`possible_calls` included) without substituting for runtime tests.
   - Call `code_map_tests` to traverse verified `calls` paths from `test_case` nodes (`possible_calls` and lexical suggestions remain hints, not verified coverage).
   - *(Unified aliases: `symbol` aliases `selector` and `file_path` aliases `path` across `inspect`, `explain`, `impact`, `tests`, `prove_removal`, `query`, and `acdd_read`.)*
3. **Discover & read documentation (`search_docs`, `get_doc`)**:
   - Search guides, ADRs, and architecture docs with `search_docs(query=..., include_excerpt=true)`.
   - Inspect structure with `get_doc(path_or_id=..., detail="compact")` (accepts `path`, `file_path`, `doc_id`, or `selector`), then read only the needed section via `get_doc(path_or_id=..., section="...")`. Archived `*/archive/` docs are excluded from default search and emit no graph edges.
4. **Targeted `acdd_read` — ONLY for concrete scenarios**:
   - **Full symbol body (`selector=` / `symbol=`)**: Use `acdd_read(selector="SymbolName")` (optionally `path="..."`) ONLY after locating the symbol via `code_map_*` when its full body is needed beyond `search`/`inspect` previews; follow `source_offset` + `generation`.
   - **File outline (`mode="outline"`)**: Use `acdd_read(path=..., mode="outline")` when you need a quick declaration/heading map of one known file.
   - **Git diff (`mode="delta"`)**: Use `acdd_read(path=..., mode="delta")` to inspect uncommitted/baseline diffs on a known file.
   - **Targeted lines (`mode="lines"`)**: Use ONLY for narrow known line coordinates (e.g. a compiler/test error line or `code_map_impact` edge line, keeping `line_count <= 80` so metadata stays within compact client inline caps) — NEVER scan files sequentially from line 1.
   - **Literal text search (`mode="search"`)**: Use ONLY for literal config values, YAML/TOML keys, or string constants that symbol/AST search cannot index, and always scope with a specific `path:` rather than scanning `.` (`regex=true|false`); reserve `rg -n` strictly for gitignored or unadmitted paths.
- **Graph hygiene & unified lint (`duplicates`, `dead_tails`, `code_map_lint`)**: Before submitting `build`,
  `polish`, or any code review, run `code_map_query(operation="duplicates", level="symbol", min_lines=5, path=...)`,
  `code_map_query(operation="dead_tails", path=...)` (workspace-wide without
  `path`/`target` on integration reviews), and `code_map_lint` (`acdd lint`; defaults to
  `[arch, syntax, docs]`, or `categories: ["arch","duplicates","dead_tails","cycles","syntax","docs"]` / `--categories all`). `syntax` checks stored parser diagnostics (`syntax.parse`), `fact_budget_exceeded`, and `intra_crate_type_homonym` (using `complexity.severity`, while AST `cognitive_complexity` and `max_nesting_depth` are recorded in `node.details` telemetry); `cycles` isolates multi-node non-test/non-archive module `imports` SCCs (enforceable at `error` via `forbid_cycles`); `dead_tails` computes workspace-wide
  production liveness first, then reports `phantom_tested`, `dead_tails` (`unreachable`),
  `dangling_calls` (`crate::*`/`super::*`/`self::*`), and—when `architecture.unused_public_api: true` is configured—`dead_tails.unused_public_api`
  (`error` on unused/test-only `pub use` re-exports and `pub` items lacking non-test
  cross-module production callers). Eliminate or reject new
  clones, phantom-tested dead code, dead tails, dangling calls, unused public compatibility APIs, module import cycles, and `acdd-rule` / doc-integrity lint errors.
- **Executable `acdd-rule` blocks & `ComplexityLimits`**: Pair machine-enforceable `INV-*` invariants in
  docs with fenced `acdd-rule` YAML blocks (or `acdd.yaml` `architecture.{zones,rules,complexity}`)
  so `code_map_lint` / `code.lint` enforces boundaries and `docs.invariants` surfaces `[ENFORCED]`.
- **SQL & bounds**: Supply one alias per parameter (`sql` or `query` for read-only
  `SELECT`/`WITH` joins over `path_dictionary` and `node_hash`; see
  [tool catalog](references/tool-catalog.md#sql-columns-and-joins-code-mapsqlite)).
  Queries have a 2s budget, 96 KiB cap, and default page size 30 (max 100); continue
  with the returned `next_offset` / `next_source_offset` and `generation`.

## ACDD profiles and task operations

Embedded profiles (`base`, `code`, `audit`, `documentation`, `spec_planning`) or repository `.acdd/profile.yaml`
and `.acdd/profiles/<name>.yaml` overlays (`version: 2`, validated via `acdd validate`) govern gates,
proof schemas, contours, isolation (`--in-place` overrides to `git.isolation: in_place`, `save: none`,
preserving `save.immutable: true`), and `save.report` (written strictly under
`.acdd/reports/*.{md,json}` on pass). Do not assume a hardcoded gate sequence:

| Profile (`--profile` / `inherit`) | Purpose & Gate Flow | Typical Launch Command |
|---|---|---|
| `code` (default) | Full software lifecycle (`blueprint_synthesis` -> `tasks` -> `run_tests` -> `polish` -> `integration_review` -> `handoff`) | `acdd start <spec_path>` or `acdd start --goal "..."` |
| `base` | Minimal worktree-isolated baseline (`gates: []`, `save: commit`) for custom non-code overlays | `acdd start --goal "..." --profile base` |
| `audit` | Read-only `in_place` (`save: none`) review/SAST/hygiene (`recon -> analyze -> triage`) writing `.acdd/reports/{run_name}.md` | `acdd start --goal "..." --profile audit` |
| `documentation` | Compiler-free living docs & `acdd-rule` invariant lifecycle gated on `docs.links` + `code.lint` (`[arch, docs]`) | `acdd start <spec_path> --profile documentation` or `acdd start --goal "..." --profile documentation` |
| `spec_planning` | Read-only `in_place` synthesis & review (`synthesize -> review`) aware of `acdd.yaml` `specs.*` collections | `acdd start --goal "..." --profile spec_planning` |
| Custom (`.acdd/profiles/<name>.yaml`, e.g. `hotfix`) | Repo-local workflow overlays (`gates_patch:` / `inherit:`), e.g. `hotfix` (`goal.build -> goal.review -> goal.deliver -> run_tests -> handoff`, skipping `blueprint_synthesis` & `integration_review`) | `acdd start --goal "..." --profile hotfix --scope <path>...` |

1. Inspect cross-worktree specs, dependency waves, critical paths, and scope-overlap markers with `acdd_state(action="roadmap")` (`acdd roadmap [spec_id] [--collection NAME] [--ready] [--active] [--tasks]`), then start a run with one of the profile commands above (adding `--in-place`, `--scope <glob>`, or `--worktree` as needed).
2. Inspect ready cursors (`acdd_state(action="next")` / `acdd next`). For `mode: subagent` gates, the coordinator dispatches `(run, node_cursor, worker_id)` and the **worker itself** calls `acdd_claim` / `acdd claim` first before editing files (omitting `worktree` under `git.worktrees: managed` so `claim` creates/reuses `.worktrees/<run_name>-<task_ref>`, because passing an explicit `worktree` overrides auto-creation; `session_fingerprint` and full `ClaimResp` bind to the real worker process, which then works strictly inside `ClaimResp.worktree`); the coordinator calls `acdd_claim` directly only for `mode: inline` gates or dead-worker recovery.
3. Follow `ClaimResp` (`contract`, `guidance`, `unresolved_findings`, `pre_task_slice`
   including `predecessor_proofs`, `blackboard`, and `context_lenses`). Stay within
   admitted write scope (`in_write_scope: ALLOWED`); extend autonomously via
   `acdd_state(action="extend_scope", ...)` (`acdd extend-scope`) inside module
   roots when needed. Keep scratch files in `.acdd/tmp/`.
4. Verify via `acdd_verify(node=..., test_target=...)` (`acdd verify <cursor> [--test <filter>]`)
   before submitting command/check gates so `execution_digest` and candidate trees bind.
5. Submit via `acdd_submit` / `acdd submit` (`pass` or `reject`). Note `acdd run`
   requires shell `runner:` commands on roles (`WORKFLOW_RUNNER_REQUIRED` when roles
   use `mode: subagent` without `runner:`); subagent orchestration coordinates
   explicit `claim` / `verify` / `submit` steps. On `TASK_CANDIDATE_MISMATCH`,
   restore via Git or `acdd reopen` and renew verification/review.



## Context refresh after compaction

When a host session compacts context (Claude `PostCompact` or Codex `SessionStart(compact)` hooks), reload gate authority without re-claiming:

```bash
acdd context [<node>] [--run <name>] [--worker <id>]
```

MCP equivalent:

```json
{"action": "context", "node": "T-03.build", "worker_id": "builder", "format": "json"}
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
