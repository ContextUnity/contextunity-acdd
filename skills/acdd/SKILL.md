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
and shared locks automatically. CLI `--worktree` takes an explicit directory path.

## Discovery & read protocol

- **Known symbol**: Call `acdd_read(selector="SymbolName")` directly (optionally add
  `path="..."`); follow returned `source_offset` + `generation` until complete
  instead of searching + slicing lines manually. `symbol` aliases `selector` (do
  not supply both non-null).
- **Line/text/diff reads**: Use `acdd_read(path=..., mode="lines|raw|search|delta|outline")`
  (`end_line`, `lines="100-200"`, `file_path`, and `query` aliases supported).
  Use `mode="search"` (`regex=false|true`) for workspace text/config searches;
  reserve shell `rg -n` for gitignored or unadmitted paths.
- **Discovery & call graph**: Use `code_map_search(pattern=..., detail="compact")`
  (`takes:Type`, `returns:Type`, `prefix*`), `ast_grep_search` (`$NAME`, `$$$SEQ`
  over `complete_syntax`, `declaration_without_body`, `fragment_probe`),
  `code_map_inspect` / `code_map_explain`, and depth-1 `code_map_impact` before
  changing or removing interfaces. `code_map_prove_removal` checks static
  candidates (`possible_calls`) but is not a runtime-safety proof.
- **Tests**: `code_map_tests` traverses verified `calls` paths from tests;
  `possible_calls` and lexical suggestions are navigation hints, not verified
  coverage.
- **Documentation**: Use `search_docs(query=..., include_excerpt=true)`, then
  `get_doc(path=..., detail="compact")` for outlines and `get_doc(path=..., section="...")`
  for full sections. Archived files under `*/archive/` are excluded from default
  search and emit no `governs` / `documents` graph edges.
- **Graph hygiene (`duplicates` & `dead_tails`)**: Before submitting `build`,
  `polish`, or any code review, run `code_map_query(operation="duplicates", level="symbol", min_lines=5, path=...)`
  and `code_map_query(operation="dead_tails", path=...)` (workspace-wide without
  `path`/`target` on integration reviews). `dead_tails` computes workspace-wide
  production liveness first, then reports `phantom_tested`, `dead_tails`, and
  `dangling_calls` (`crate::*`/`super::*`/`self::*`). Eliminate or reject new
  clones, phantom-tested dead code, dead tails, and dangling calls.
- **SQL & bounds**: Supply one alias per parameter (`sql` or `query` for read-only
  `SELECT`/`WITH` joins over `path_dictionary` and `node_hash`; see
  [tool catalog](references/tool-catalog.md#sql-columns-and-joins-code-mapsqlite)).
  Queries have a 2s budget, 96 KiB cap, and default page size 30 (max 100); continue
  with the returned `next_offset` / `next_source_offset` and `generation`.

## ACDD profiles and task operations

Embedded profiles (`base`, `code`, `audit`) or repository `.acdd/profile.yaml`
(`version: 2`, validated via `acdd validate`) govern gates, proof schemas,
contours, isolation (`--in-place` overrides to `git.isolation: in_place`, `save: none`,
preserving `save.immutable: true`), and `save.report` (written strictly under
`.acdd/reports/*.{md,json}` on pass). Do not assume a hardcoded gate sequence:

1. Inspect cross-worktree specs, dependency waves, critical paths, and scope-overlap markers with `acdd_state(action="roadmap")` (`acdd roadmap [spec_id] [--collection NAME] [--ready] [--active] [--tasks]`), then start a run with `acdd start <spec_path>` or `acdd start --goal "..."`
   (defaults to `code`; select `inherit: audit` for read-only SAST/PR/hygiene audits,
   or `inherit: base` for non-code workflows).
2. Inspect ready cursors (`acdd_state(action="next")` / `acdd next`) and claim
   (`acdd_claim` / `acdd claim` with `node`, `worker_id`, `worktree`).
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
