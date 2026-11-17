---
name: acdd
description: Load for repository onboarding to ACDD, code discovery, symbol lookup, dependency tracing, impact analysis, architectural verification, or ACDD task operations. Routes setup, structural search, and task coordination through ACDD MCP.
---

# ContextUnity ACDD

Use ACDD's indexed code graph, AST search, documentation graph, and task tools.
Prefer registered MCP tools; use the CLI for operations it owns. Fall back to
local search only when ACDD is unavailable. The active registry and schemas are
the authority; see the [tool catalog](references/tool-catalog.md) for signatures
and [routing examples](references/agents.md.example) for repository integration.

For a request to onboard or configure a repository for ACDD, follow the
[repository onboarding guide](references/onboarding.md). It adapts the companion
examples to the repository's actual tooling, instructions, and delivery goals.

## Orient and select the workspace

Start with `code_map_overview()` and confirm the root, components, languages, and
coverage before claiming a symbol or dependency is absent. Pass `worktree` to
select another checkout; omission keeps the canonical server root regardless of
client cwd or active task claims.

Worktree selectors resolve explicit paths first, then exact directory name,
literal or zero-normalized numeric prefix, suffix, and substring. The first
nonempty tier must be unique. Selected worktrees use their own adapter and index;
Git-linked worktrees share the resolved state store (`<primary_worktree>/.acdd/state.sqlite`).
CLI navigation uses `--root`; CLI `acdd claim` uses an existing `--worktree` path, not MCP name aliases.

Use [adapter defaults](references/adapters.md) and the
[adapter example](references/acdd.yaml.example) for roots, linked workspaces,
documentation paths, and response policy. Include both `src` and `crates` in
multi-crate workspace roots.

## Unified parameter conventions

| Intent | Parameters and examples |
| --- | --- |
| Find symbols | `code_map_search(pattern="Router", path="src")`; `query` aliases `pattern`. Use `takes:Type`, `returns:Type`, decorators, or `prefix*` to narrow discovery. |
| Resolve / inspect a symbol | `code_map_inspect(selector="Router", path="src")`; `symbol` aliases `selector`, and `file_path` aliases `path`. Selectors accept an exact ID or name, `path:name`, `path:known_kind:name`, `path:line`, or `path#Lline`. |
| Match syntax | `ast_grep_search(pattern="fn $NAME($$$ARGS) {}", language="rust", path="crates")`; `query` aliases `pattern`, and `file_path` aliases `path`. Omit `language` for recognized file extensions or single-language indexed directories; supply `language` when `path` is omitted or spans multiple compiled languages. CLI requires `--lang`. |
| Find documentation | `search_docs(query="routing", path="docs", include_excerpt=true)`; `pattern` aliases `query`, and `path` aliases `component`. |
| Read documentation | `get_doc(path="docs/reference/tasks.md", detail="compact")`; `path` and `selector` alias `path_or_id`. Omit `section` for an outline; specify an exact section title to read its content. |
| Diagnose | `code_map_analyze(path="src")`; `path` and `selector` alias `target`. Omit the target for workspace totals. `lint=true` reads stored syntax diagnostics; `include_cycles=true` opts into cycles. |
| Query SQL | `code_map_query(sql="SELECT count(*) FROM nodes")` implies `operation="sql"`. The explicit form is `code_map_query(operation="sql", selector="SELECT ...")`; `query` also accepts SQL. |
| Query graph | `code_map_query(operation="implementations", selector="LanguageLinker")` or `code_map_query(operation="untested", target="src")`; `query` aliases the selector. |
| Find duplicates | `code_map_query(operation="duplicates", level="symbol", min_lines=10, target="crates")`; `level` accepts `"symbol"` (functions/methods body clones) or `"file"` (identical files by digest). `min_lines` defaults to 10. `target` or `path` restricts search scope. |

Use one spelling for each aliased parameter in a request. The schemas advertise
aliases as optional properties; handlers reject a nonempty canonical value combined
with an alias. For SQL, an explicit `sql` field takes
precedence over `query`, then `selector`. A `SELECT`/`WITH` statement in `operation`,
or in `query` with an omitted/blank operation, also selects SQL mode. SQL accepts
one read-only `SELECT`/`WITH` statement using integer `path_dictionary` and `node_hash` joins; see the [SQL column schema](references/tool-catalog.md#sql-columns-and-joins-code-mapsqlite) before writing custom queries. Missing-table or missing-column SQL errors retain the SQLite error and append a schema hint.

## Read symbols and trace relationships

Reuse sufficient search previews. Use `code_map_inspect` for a declaration,
signature, docstring, and compact call summary; use `code_map_explain` for paged
incoming/outgoing relationships. Request `include_coverage=true` when resolution
evidence matters.

**Known symbol reads**: When the unique name is already known, use
`acdd_read(selector="SymbolName")` directly; add the owner path with
`acdd_read(path="...", selector="SymbolName")` to restrict resolution. For
example, prefer `acdd_read(selector="render_role_prompt")` over first calling
`code_map_search(pattern="render_role_prompt")` and then reading a line range:
the direct snippet checks the indexed digest and returns the complete symbol body
within the response budget. If it is paged, continue with its `source_offset`
and `generation` until complete. Search when the name is unknown or resolution
needs disambiguation; inspect the returned node to confirm the intended owner.
MCP also accepts `acdd_read(symbol="SymbolName")` as an alias. Two non-null
`symbol` and `selector` values conflict even if one is empty; `null` is absent.
Path restriction and paging are unchanged.

For arbitrary line bounds, `acdd_read` accepts `start_line` and `line_count`, plus ergonomic variations: `end_line` computes the inclusive `line_count` from `start_line` (default 1) and rejects zero or an end before the start, `lines="100-200"` parses range bounds into `start_line` and `line_count`, `lines=50` or `max_lines=50` aliases `line_count`, `file_path` aliases `path`, and `query` aliases `pattern`. Workspace-root absolute paths are automatically stripped to relative workspace paths. When reading lines enclosed inside an indexed function, struct, or method, `acdd_read` emits an enclosing symbol hint in the output footer with the copy-ready `selector` call. Use `mode="search"` with `pattern` for literal/regex search, `mode="delta"` for Git changes, and `mode="outline"` for Markdown or code declarations.
CLI parity is `acdd read notes.txt --mode lines`. Snippets default to the complete symbol within the
serialized byte budget; `max_body_lines` sets an explicit smaller bound. Inspect
and explain retain configured preview defaults. Core text reads work without codemap. Respect `sensitive` path blocking and
confined `output_file` exports. Follow the conditional `Hints` footer only when
the enabled graph or active task confirms the suggested operation. Use `ctx_read`
for a single source line too large for the MCP budget.

Run depth-1 `code_map_impact` before interface edits or removals. Git change views
accept exactly one of `selector` or `changes="staged"|"unstaged"|"head"`; check
`complete` and `unmapped_changes` before treating the view as exhaustive.
`code_map_prove_removal` supplies indexed removal evidence, not runtime proof.

`code_map_tests` follows exact `calls` paths from classified test cases. Helpers
may occur inside a path but are not coverage roots. `possible_calls`, lexical
suggestions, and an absent indexed test call do not prove runtime coverage.

### Code clone and duplicate detection

Use `code_map_query(operation="duplicates", level="symbol", min_lines=10, target="crates")` to identify duplicated code across the workspace:
- `level="symbol"`: Finds function and method clones. Exact matches use SHA-256 hashes of literal symbol source slices; near-clones use normalized line similarity clustering among symbols with matching names.
- `level="file"`: Finds byte-identical duplicate files by SHA-256 content digest.
- `min_lines`: Filters candidates by minimum lines of code (default: 10).
- `target` or `path`: Restricts detection scope to a specific directory or file.
- Results provide grouped locations, line counts, duplicate counts, and actionable architectural `suggestion` strings indicating how and where to centralize or deduplicate code.
- CLI equivalent: `acdd query run duplicates --level symbol --min-lines 10`.

## Documentation and graph provenance

Triage with `search_docs(query="...", include_excerpt=true)`, inspect an outline
with `get_doc(path="...", detail="compact")`, then read a specific
`section="..."`. Full content is the default for `get_doc`.

Confined Markdown links/wikilinks create `cites_doc`/`cited_by_doc` only when one
indexed section resolves. Code claims use the document's workspace unless an
explicit normalized source-file path confines the target. Eligible declarations
resolve by the first nonempty tier: normalized qualname, exact name, then
qualified-name segment suffix. Only one target in that tier verifies a claim.

Full document results expose ambiguous `referenced_symbol_candidates`;
compact results omit them and hint at full detail. Candidates and suggestions
are navigation only. Verified references appear in `documented_in`; impact
returns `affected_docs`, including retained source-file claims after removal.
An `INV-*` governance edge requires a unique code claim within the invariant
node's line span. An explicit normalized source-file link confines resolution;
an unscoped code claim uses the document's workspace and the same uniqueness rule.
Archived documents under any `archive/` directory are excluded from default
documentation search and hubs. Search them with an explicit archive
`component`/`path`, or read them with `get_doc`. They emit no `documents`,
`references_doc`, `governs`, or `governed_by` edges. Claim context bundles
intersect indexed non-archive documentation across the repository via `governs`
and `documents` edges while excluding `archive/**` and configured spec-collection
or contract documents already materialized in `context_bundle.contract` and
`context_bundle.governing_invariants`. Bounded governing-rule projections
supplement task authority and proof.

Follow the [documentation layout](references/documentation-layout.md); adapt the
[docs README](references/docs-readme.md.example) and
[docs rules](references/docs-agents.md.example) when setting up a repository.
Read [framework pack guidance](references/framework-packs.md) before changing
declarative extraction rules; keep API identities in rule data and preserve
canonical typed facts and exact-target evidence.

## Response bounds and continuation

SQLite queries have a 2-second budget. Responses cap at 96 KiB unless configured
lower; serialized metadata and Markdown/JSON escaping count toward that limit.
Pages default to 30 items and allow 1–100. Prefer compact detail for discovery and
full detail for one selected entity. MCP defaults to Markdown; use `format="json"`
or CLI `--format json` for structured consumption.

Continue collection pages with `next_offset` and the returned `generation`.
Source continuation uses `next_source_offset` as `source_offset` with that page's
explicit generation. Missing or stale tokens require a restart; no generation is
inferred. Source pages contain complete lines and verify the on-disk digest.
Task-list generations bind task state separately from code-index generations.
On computation limits, narrow scope/depth; on byte limits, reduce detail or page
size. Inspect a truncated blackboard message by its returned message ID.

## Test harness ownership

When present, read the target repository's `docs/architecture/test-harness.md`,
section "Rule 3: The Two-Caller Rule".
Shared disk setup lives in `tests/common`. Shared ACDD session, evidence, and
task completion live in `tests/acdd/support.rs`. A domain harness function needs
at least two callers. Load `test-suite-refactor` when moving setup out of a
`#[test]`.

## ACDD profiles and task operations

> [!NOTE]
> **Upgrading from `0.5.x`:** If a repository still uses `0.5.x` keys in `acdd.yaml` (`milestones`, `plans`, `tasks_db`, `task_repository`, `task_project`, `acdd_profile`), upgrade `acdd.yaml`, `.gitignore`, `.acdd/profile.yaml` (`version: 2`), and open specs per the `0.6.0` migration runbook and verify fail-closed with `acdd validate`.

Workflow sequence, roles/models, proof gates, review contours, commands, `git:` isolation/integration policy, and persistence belong to the active embedded or repository `GateProfile` (`version: 2`). Read `workflow_guidance` returned by `acdd_claim` (`acdd claim`) and submit the declared proof payload; do not infer a fixed gate sequence from this skill. Repository `AGENTS.md` owns verification commands and Git permissions. Keep temporary run artifacts under `.acdd/tmp/` and outside source authority.

Use `inherit: none` with `save: none` for taskless workflows, `inherit: in_place` (or `--in-place`) for single-checkout plans, and `inherit: default` or `inherit: code` for isolated worktree specs with blueprint synthesis and handoff gates (`acdd start <spec_path>` or `acdd start --goal "..."`). Specify numbered `AC-*` criteria before design and map each task to its applicable criteria. Keep C1/C2 boundaries in architecture documentation and inspect live C3/C4 relationships in the code map. Rebind candidate drift (`acdd reopen` / `acdd rollback`) and renew verification and independent review before a terminal receipt.

To customize a repository or task workflow, copy the [canonical ACDD profile example](references/acdd_profile.yaml.example) into `.acdd/profile.yaml` (or a `profiles_dirs` path) and validate it with `acdd validate` before starting a run. Start with `version: 2`:
- With `inherit: default`, `inherit: code`, or `inherit: in_place`, repository profiles overlay the embedded profile by merging `roles`, `commands`, `proofs`, `guidance`, `contours`, `gate_templates`, `git_commit_notes`, and `scope_args_by_pattern` by key, and patching `gates:` via `gates_merge: patch` (matching gate `id` or `- tasks:` blocks, with optional `after:`/`before:` insertion). Use `inherit: none` for standalone workflows.
- Gates declare explicit `proof:` (`none` when omitted, `contract` requiring `command` + `expected_failure` + `{test_filter}`, `command`, `call`, `review` requiring `role` + `contours`, or `<catalog>` / `{ scheme }`), optional `checks:` (`code.seam`, `code.signature_scope`, `docs.links`), and dynamic `context:` lenses (`code.symbols`, `code.tests`, `code.external_callers`, `docs.adrs`, `docs.invariants`).
- When a pre-task root gate (`blueprint_synthesis`) passes with `proof.tasks[<task_ref>]`, the engine records each task's slice and injects only that task's slice into `context_bundle` upon `acdd_claim`.
- Use the [ACDD domain profile examples catalog](references/acdd-profile-examples/README.md) (including scenarios such as [`pentest-security-audit.workflow.yaml`](references/acdd-profile-examples/pentest-security-audit.workflow.yaml), [`ci-verification-gate.workflow.yaml`](references/acdd-profile-examples/ci-verification-gate.workflow.yaml), [`repo-polish.workflow.yaml`](references/acdd-profile-examples/repo-polish.workflow.yaml), [`docs-refactor-and-reconcile.workflow.yaml`](references/acdd-profile-examples/docs-refactor-and-reconcile.workflow.yaml), [`in-place-refactor.plan.yaml`](references/acdd-profile-examples/in-place-refactor.plan.yaml), and [`subsystem-feature.milestone.yaml`](references/acdd-profile-examples/subsystem-feature.milestone.yaml)) as starting points; `acdd validate` is the schema authority.

Standalone role runners return a structured decision and matching proof; a role's registered command is observed by ACDD before acceptance. Bound repair loops with explicit `max_attempts`, `pass:`, and `fail:` targets, and use `independent_from` for reviewer separation (`worker_id` inequality enforced at both `acdd_claim` and `acdd_submit`). Custom profiles may configure `roles.<name>.runner` and `roles.<name>.prompt_template` (receiving `ACDD_RUN_ID`, `ACDD_NODE_CURSOR`, `ACDD_CLAIM_REVISION`, `ACDD_WORKER_ID`, `ACDD_AGENT_SESSION_ID`, `ACDD_WORKTREE`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, `TRACEPARENT`, and rendered `ClaimResp` over stdin).

Explicit gate `tools`, `default_tools`, `tools_add`, and `tools_remove` validate fail-closed against the 19 built-in static tools (filtered by `modules:`) plus active `<domain>_<name>` plugin tools from registered `DomainProvider` crates. Keep optional navigation advice out of hard `tools:` requirements when a workflow must run with `modules.code: false` or `modules.docs: false`.

Use `acdd_guide` (or `acdd guide <topic>`) for live tool/gate/profile topic references. Claim the next ready node (`acdd_state(action="next")` / `acdd_claim` or `acdd next` / `acdd claim`), stay within admitted scope, and extend scope autonomously via `acdd_state(action="extend_scope", ...)` (`acdd extend-scope`) before expanding edits into unadmitted files within module roots. Run iterative checks with `acdd_verify(node="...", test_target="...")` or `acdd verify <cursor> [--test <file.rs::test_fn>]`, then submit via `acdd_submit` or `acdd submit`. Typed contract and command proofs require a fresh claim-bound trace (`execution_digest` + `candidate_tree_before == candidate_tree_after`). Under `worktree` isolation, integrate completed tasks with `acdd_state(action="integrate")` (`acdd integrate`) before post-task root gates. For schemas, crash recovery, delivery, and handoff, consult the [task reference](https://github.com/ContextUnity/contextunity-acdd/blob/main/docs/reference/tasks.md).

## Blackboard

Use run-level messages (`node` omitted) for cross-task state and `node` / `subtask_ref` targeting for local decisions. Topics are `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred`. Post material architectural decisions, findings, blockers, and scope justifications via `acdd_blackboard(action="post", ...)` or `acdd blackboard --action post`. Use `notes` only for non-obvious technical context that the next worker needs; do not duplicate routine gate status, successful tests, or SHAs unless they add context.

Before rejecting a review, post structured `findings` with the affected code, the observed issue, and a concrete remediation. Then submit the rejection with the Blackboard message ID referenced in `rejection_findings.blackboard_message_id`. MCP uses `acdd_submit(..., action="reject", rejection_findings=<JSON>)`; CLI uses `acdd submit <cursor> --action reject --findings '<JSON>'` (plus `--evidence '<JSON_OBJECT>'` when the gate requires review/custom evidence). Respect the active profile's evidence and routing rules.

Read applicable messages after claim (`acdd_blackboard(action="read", ...)` or `acdd blackboard --action read`) and inspect IDs for full payloads. A `blueprint_synthesis` architect records `topic="decisions"`; build drafts may target `gate="build"`. Active `decisions`, `findings`, and `blockers` remain eligible across gates in `context_bundle.blackboard`, preserving the original gate within response bounds. Other topics follow untargeted/current-gate selection. Record accepted decisions and deferred work with an owner/destination before delivery; blackboard content supplements authority and never replaces proof.
