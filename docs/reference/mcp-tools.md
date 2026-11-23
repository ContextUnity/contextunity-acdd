---
doc_type: api
title: MCP tool reference
---

# MCP tool reference

The server exposes **20 built-in static tools** over standard input and output
(11 code tools, 2 docs tools, 2 core tools, 5 state tools), plus any dynamic
`<domain>_<name>` plugin tools registered by active `DomainProvider`
implementations. Start with `code_map_overview` to identify the indexed
workspace. Search for a symbol, inspect its exact identifier, then use graph
tools at a narrow scope. Check the reported path and line in source before acting
on a structural inference.

## Description layers

A client receives three texts for this server. Keep each fact in one layer.

| Layer | Source | Write |
| --- | --- | --- |
| Server instructions | `instructions` on the MCP server handler | Cross-tool budget and the first call: 96 KiB maximum (lower configured limits apply), 2 second SQLite budget, start at `code_map_overview`, continue a page with `offset` and `generation`, prefer `detail=compact`. |
| Tool description | `description` on each tool | What that call returns, plus one limit the schema cannot express. |
| Input schema | Description on each input field | Accepted values, defaults, and pairings such as nonzero `offset` with `generation`. |

Selector grammar, SQL shape, proof objects, and examples stay in this page and in `acdd_guide`. Verify a changed sentence against the handler in `src/mcp/tools/` and the query implementation it calls. Connection and the first-call check stay in [MCP setup](mcp-setup.md).

## Efficient navigation

MCP navigation accepts unified parameter spellings alongside the canonical fields:

| Tool | Canonical field | Accepted aliases |
| --- | --- | --- |
| `code_map_search` | `pattern` | `query` |
| `code_map_inspect` | `selector`, `path` | `symbol`, `file_path` respectively |
| `acdd_read` | `path`, `selector`, `pattern`, `line_count` | `file_path`, `symbol`, `query`, `max_lines` (`lines`) respectively |
| `ast_grep_search` | `pattern` | `query` |
| `search_docs` | `query`, `component` | `pattern`, `path` respectively |
| `get_doc` | `path_or_id` | `path`, `selector` |
| `code_map_analyze` | `target` | `path`, `selector`; omission selects workspace totals |
| `code_map_query` | `selector` | `query`; `sql` selects read-only SQL mode |

Aliases are explicit optional properties in the published `tools/list` schemas;
the canonical field is not required when an alias supplies the value. Handlers
normalize these fields before querying. Supply one spelling per value; a nonempty
canonical field together with an alias is a duplicate-field error. `code_map_query` resolves its separate
optional fields as described under [SQL](#sql-queries).

Recommended navigation sequence:

- **`code_map_overview`**: confirm workspace root, generation token, and indexed language/doc counts.
- **`code_map_search`**: find candidate symbols (reuse search previews directly when sufficient).
- **`code_map_inspect` / `code_map_explain`**: inspect one declaration's summary (`inspect`) or direct incoming/outgoing edges (`explain`).
- **`acdd_read`**: read complete digest-verified symbol bodies (`max_body_lines` optional) instead of slicing files manually.
- **`search_docs` / `get_doc`**: triage sections with `search_docs(include_excerpt=true)`, view outlines with `get_doc(detail="compact")`, and read one section with `get_doc(section="...")`.
- **`code_map_tests` & `code_map_prove_removal`**: trace verified `calls` paths from `test_case` nodes and assess static incoming dependencies before deleting symbols.
- **`code_map_lint` & `code_map_analyze`**: run unified architectural/hygiene rules (`code_map_lint`), syntax checks (`lint=true`), or cycle detection (`include_cycles=true`).
- **`worktree` selector**: pass optional `worktree` on any tool call when targeting a secondary Git worktree (see [worktree selection](#worktree-selection)).

## Output format

Every tool schema accepts an optional `format` value: `markdown` or `json`.
Omit it for readable Markdown; `format: "json"` returns the semantic value as
JSON text for clients that decode the result. The same selection is available on
navigation, docs, guide, state, and plugin tools. Invalid values are rejected
before the operation runs. Formatting does not change query scope, pagination,
or state evidence.

Markdown blackboard reads preserve topic, gate, status (`open | acknowledged | resolved | carried_over`),
and newest-first ordering. Blockers use a caution callout that retains their
identity and available metadata.

State tools use the dedicated lifecycle layout: summaries retain dependencies,
run/gate statuses, budgeted `ClaimResp` tiers (`contract`, `guidance`, `diff`,
`context`, `blackboard`, `traces`), and verification observations. See the
[task lifecycle Markdown contract](mcp-markdown-contracts.md#task-lifecycle).

Claim/context blackboard bundles include untargeted and current-gate messages.
Active `decisions`, `findings`, and `blockers` also remain eligible across gates,
retaining their original gate and subject to the 6-tier `ClaimResp` byte budget.
Explicit gate `tools` / `tools_add` / `tools_remove` declare role tool
allowlists layered over `default_tools` in the active v2 `GateProfile`.

## Tools

### Layered file reader

`acdd_read` and CLI `acdd read` share one layered file and symbol reader:

- **Modes (`mode`)**:
  - `auto` (default): selects `snippet` when `selector` is set, `search` when `pattern` is set, and `lines` otherwise.
  - `lines`: 1-based numbered source lines (`start_line`, `line_count` `1..=10,000`, default `200`; works without an index).
  - `raw`: exact text preserving original line endings (works without an index).
  - `search`: substring or `regex: true` search starting at `start_line` with `context` lines (`0..=100`, default `2`).
  - `delta`: unified Git diff against `baseline`, the active task's candidate baseline, or `HEAD`.
  - `outline`: Markdown heading tree (`docs` module) or AST declaration list (`code` module); `paths: [...]` batches multiple files.
  - `snippet`: complete digest-verified symbol body (`selector` or alias `symbol`, optional `path` to disambiguate), paged cleanly via `source_offset` + `generation`.
- **Argument aliases & helpers**:
  - `end_line`: inclusive end line computed from `start_line`.
  - `lines`: range string (`"100-200"`, `"100..200"`) or integer count.
  - `max_lines` / `file_path` / `query` / `symbol`: aliases for `line_count`, `path`, `pattern`, and `selector` (passing both canonical and alias non-null values is rejected).
  - **Workspace-prefix stripping**: absolute paths inside the workspace automatically normalize to relative paths.
  - **Enclosing symbol footer**: reading raw lines inside an indexed function/struct appends a `💡 Enclosing symbol` hint with its exact `selector`.
- **Safety & bounds**:
  - **Text cap**: `lines`, `raw`, and `search` cap output at `48 KiB` (`truncated: true`); lines above `1 MiB` error cleanly.
  - **Binary & secret guards**: NUL bytes in the first `8 KiB` return `BINARY_FILE`; symlinks, path traversal, and `sensitive` patterns are blocked on both reads and `output_file` exports (see [sensitive paths](configuration.md#sensitive-reader-paths)).
  - **Active gate hints**: when a task gate is claimed in the selected worktree, responses include `write_scope: ALLOWED|OUT_OF_SCOPE` and copy-ready graph/verify/blackboard hints.

### Built-in MCP tools & CLI parity

Every static capability runs over identical index and state engines on both surfaces (`<domain>_<name>` plugin tools map to `acdd tool <domain>_<name>`):

| MCP Tool | CLI Equivalent | Purpose |
| --- | --- | --- |
| `code_map_overview` | `acdd query overview` | Summarize workspace counts, languages, docs hubs, entrypoints, topology, and per-workspace coverage. |
| `code_map_search` | `acdd query search` | Search symbols by exact name, prefix (`parse*`), FTS5 BM25 query, or type filter (`takes:`, `returns:`). |
| `code_map_inspect` | `acdd query inspect` | Inspect one symbol's signature, docstring, caller/callee summary, governing invariants, and linked docs. |
| `acdd_read` | `acdd read` | Read line slices, search matches, Git deltas, outlines, or digest-verified symbol bodies (plus `output_file` export). |
| `code_map_explain` | `acdd query explain` | List direct `incoming`, `outgoing`, or `both` relationship edges and analysis gaps for a symbol. |
| `code_map_impact` | `acdd query impact` | Trace multi-hop blast radius (`inbound` / `outbound`) for a symbol or `--changes staged\|unstaged\|head`. |
| `code_map_tests` | `acdd query tests` | Trace exact `calls` paths between `test_case` roots and production targets (`inbound` / `outbound`). |
| `code_map_prove_removal` | `acdd query remove` | Assess static incoming dependencies and target-scoped diagnostics before deleting a symbol. |
| `code_map_query` | `acdd query run` / `sql` | Execute named graph operations (`slice`, `unwired`, `untested`, `implementations`, `duplicates`, `dead_tails`, `sql`). |
| `code_map_lint` | `acdd lint` | Evaluate architectural (`acdd-rule` / `acdd.yaml`), `docs`, `syntax`, `duplicates`, `dead_tails`, and `cycles` rules. |
| `code_map_analyze` | `acdd query analyze` | Aggregate workspace/file resolver gaps, `untested_public_surface`, `doc_diagnostics`, stored syntax lint, and cycles. |
| `ast_grep_search` | `acdd ast grep` | Match structural Tree-sitter patterns (`$NAME`, `$$$SEQ`) narrowed by indexed file candidates. |
| `search_docs` | `acdd docs search` | Search indexed Markdown/MDX heading sections with BM25 ranking and optional `include_excerpt`. |
| `get_doc` | `acdd docs get` | Read a document outline, specific heading section, citations, and verified/candidate symbol references. |
| `acdd_guide` | `acdd guide` | Return capability-filtered usage guidance across 11 built-in topics (`query`, `code`, `ast`, `docs`, `acdd`, ...). |
| `acdd_state` | `acdd start\|draft\|status\|...` | Drive run lifecycle actions (`start`, `draft`, `status`, `list`, `show`, `next`, `roadmap`, `context`, `snapshot`, `sync`, `clear`, `run`). |
| `acdd_claim` | `acdd claim` | Atomically claim a ready or active gate node and assemble its 6-tier `ClaimResp` context envelope. |
| `acdd_verify` | `acdd verify` | Run gate shell `commands:`, `call:`, `checks:` (`code.seam`, `code.lint`), and write-scope barrier without advancing state. |
| `acdd_submit` | `acdd submit` | Submit `pass` or `reject` (`review_evidence`, structured `findings`, commits, reports, archive/prune). |
| `acdd_blackboard` | `acdd blackboard` | Read or post scoped coordination messages (`draft`, `notes`, `findings`, `blockers`, `decisions`, `deferred`). |

In addition to the 20 built-in static tools above, active `DomainProvider` plugins
can register domain-scoped tools as `<domain>_<name>` (dispatched dynamically by
`src/mcp/tools/dispatch.rs` when `modules.domains.<domain>` is enabled). See
[state, tasks & lifecycle](tasks.md) for action schemas, receipts, and recovery,
and the `acdd` skill for agent execution workflows.

## Built-in guide

`acdd_guide` and CLI `acdd guide <topic>` use the same producer, resolved
adapter, and response formatter. Omitted or blank MCP topics select `query`. The
`query` response retains its navigation fields and adds a capability-filtered
`topics` array across the 11 topics:

| Topic | Guidance | Required capability |
| --- | --- | --- |
| `query` | Existing navigation and topic index | Always available |
| `init`, `adapter`, `validate` | Configuration creation, syntax, and workspace validation | Always available |
| `code`, `ast` | Verified navigation, structural search, and language capabilities | `code` |
| `docs` | Search, outlines, source reads, frontmatter, and invariants | `docs` |
| `lifecycle` | Claims, verification, submission, role runners, rejection recovery, and commits | `state` |
| `acdd` | Unified `Run` / `GateProfile` lifecycle (`worktree`, `in_place`) | `state` |
| `gates` | Conditions, profile routes, persistence, and `DomainProvider` hooks | `state` |
| `subtasks` | Subtask status, mandatory evidence, and delivery checks | `state` |

Known disabled topics return `{}` on both surfaces. Disabling `blackboard`
removes blackboard fields from `acdd` / `lifecycle` guidance. Unknown topics
return the `unknown guide topic` diagnostic. `init` bypasses adapter loading and
requires `force: true` (CLI `--force`) to replace existing files.

The guidance topics return structured Markdown or JSON (`format: "json"`).
See [headless execution](cli.md#headless-run-execution) and
[CLI server and guide usage](cli.md#server-guide-and-plugin-tools).

## Worktree selection

Every built-in and plugin MCP tool accepts an optional `worktree` selector:

- **Omitted (default)**: targets the MCP server's root workspace (active claims never silently switch roots).
- **`"."` or path with `/` (`\`)**: selects the server root or an explicit directory path (`WORKTREE_NOT_FOUND` if missing).
- **Short name / substring matching** (searches Git-registered worktrees and `.worktrees/*` in tier order):
  1. Exact directory name.
  2. Literal prefix or zero-normalized numeric prefix (`"23"` matches `"023"`).
  3. Literal suffix.
  4. Literal substring (e.g. `worktree: "sql-schema"`).
- **Disambiguation**: the first non-empty tier must match a single worktree (`WORKTREE_AMBIGUOUS` on ties).
- **Database isolation**: each worktree uses its own `acdd.yaml` and `.acdd/code-map.sqlite`, while linked Git worktrees share `<primary_worktree>/.acdd/state.sqlite`.
- **CLI equivalent**: CLI query commands use `--root <PATH>`, while state commands (`claim`, `verify`, `submit`) accept `--worktree <PATH>`.

## Selectors and graph queries

- **Selector syntax**:
  - **Known symbol / file**: pass a unique symbol name directly (or an identifier returned by `code_map_search`), or pass a file path to select that module/file.
  - **Qualified selectors**: `path:name`, `path::name`, `path:known_kind:name` (for registered node kinds), or `path:line` / `path#Lline` (narrowest symbol covering that line).
  - **Prefixes & ambiguity**: relative paths accept `./`, `file:`, or `file://`. Add the path when a bare name matches both a module and a symbol. Markdown belongs in `get_doc` / `search_docs`.

- **`code_map_query` operations & aliases**:
  - **Supported operations**: `overview`, `inspect`, `explain`, `impact`, `slice`, `unwired`, `untested`, `implementations`, `duplicates`, `dead_tails`, and `sql` (aliases: `doctor` -> `overview`; `search` / `discover` / `find` -> symbol search).
  - **`impact` & direction**: `code_map_query(operation="impact")` defaults to depth `2`, while `code_map_impact` defaults to depth `1`. Direction defaults to `inbound` (`outbound` follows outgoing dependencies).
  - **`unwired`**: lists functions/methods with no static caller, excluding classified entrypoints (`summary.excluded_entrypoints_count` reports excluded entries; not a dead-code proof).
  - **`dead_tails`**: evaluates workspace-wide production liveness first and returns scoped `phantom_tested` (tests-only), `dead_tails` (zero prod/test reachability), and `dangling_calls` (unresolved `crate::`/`super::`/`self::` refs).
  - **`inspect` & `explain`**: return the same compact symbol summary and citation layout as the dedicated tools (`include_coverage=true` adds resolution coverage).
  - **SQL mode (`operation="sql"` / `code_map_analyze`)**: pass one read-only `SELECT` or `WITH` statement in `selector` or `target`. Leading comments/whitespace and one trailing semicolon are allowed; additional statements or writes fail closed. SQL rows preserve requested columns verbatim; structural rows strip internal IDs. Use `limit`, `offset`, and `generation` to page results (see [CLI reference](cli.md)).

- **Traversal & scope caps**: depth is capped at `16`; deep traversals whose first frontier exceeds `1,000` links fail (retry with depth `1` or a narrower selector), and removal proofs reject scopes above `10,000` nodes.

- **`code_map_search`**:
  - **`pattern` / `query`**: `query` is an alias for `pattern`. Use `pattern: "*"` with a non-empty `path` or `kind` to enumerate symbols in that scope (`path: ""` searches the whole workspace; bare/repeated `*` is rejected).
  - **Type & decorator filters**: searches indexed parameter types, return types, and decorators via `takes:GateProfile`, `returns:Result`, or `@pytest.fixture` (wildcards rejected in type filters; `exact: true` matches names unless a type filter is given).

- **Impact, slice, tests & removal details**:
  - **`code_map_impact` & `slice`**: mark the seed node with `is_seed=true` and annotate multi-hop nodes with `via` (shortest path predecessor + edge kind). A directory selector in `slice` lists internal nodes (`graph_depth_applied=0`) plus a `boundary_contract`; unique directory suffixes are accepted.
  - **`code_map_tests`**: traces exact `calls` edges from `test_case` roots (`call_path` IDs). Helpers/fixtures stay intermediate, `possible_calls` / imports do not count as coverage, and bounded lexical suggestions appear only when no graph path exists. Exceeded traversal budgets fail instead of returning partial coverage.
  - **`code_map_analyze` (`untested_public_surface`)**: pages declared-public production symbols lacking exact `calls` paths from `test_case` nodes (visibility follows language syntax, not transitive crate/package re-exports).
  - **`code_map_prove_removal`**: `assessment` returns a candidate-scoped verdict and blocking dependencies without mixing in unrelated workspace diagnostics.

- **`code_map_explain`**:
  - **Direction**: `incoming`, `outgoing`, or `both` (aliases `inbound` / `outbound`).
  - **Payload**: returns signature, docstring, receiver lexical container, distinct-edge/occurrence counts, and up to 5 callers/callees per direction (`include_coverage=true` adds coverage; `show_doc=false` omits linked docs).

- **`analysis_gaps` & edge types**:
  - **`analysis_gaps`**: paged list on `inspect` / `explain` reporting `trait_object_dispatch` (Rust runtime trait dispatch), `macro_expansion_not_analyzed` (indexed macro call preserving navigation to macro declaration), path, line, and resolver evidence.
  - **`calls` vs. `possible_calls`**: `calls` marks exact calls; `possible_calls` (`confidence: "possible"`) marks bounded branch joins (`<= 8` declarations; overflow widens coverage). Inbound impact and removal analysis treat `possible_calls` as incoming dependencies.

Markdown traversal output preserves that distinction: explain marks only exact
`calls` edges as proven and labels `possible_calls` as candidates; deeper impact
paths retain their supplied `via` evidence without invented call-site locations.
Graph-backed test paths render as ordered, linked case-to-helper-to-target
breadcrumbs. Lexical suggestions remain explicitly unverified and have no graph
breadcrumb. JSON continues to return the structured semantic result.

- **Python stub & package imports**: `from package import child` resolves unique child modules or explicit `__init__.py` relative re-exports, preferring `.py` over `.pyi` (falling back to `.pyi` only if runtime omits the symbol).

- **`code_map_analyze` summary & diagnostics**:
  - **Grouping**: aggregates unresolved references by resolver cause, groups parse diagnostics by language/file, and pages `untested_public_surface` (exact `test_case` `calls` only; external imports count toward `external_imports`).
  - **Target rules**: omit `target` for workspace scope; unindexed non-empty targets fail with `PATH_NOT_INDEXED`. Exact indexed file targets emit per-location `snippet` (when digest matches) and resolver `reason`.

- **Declared-public rules by language**:
  - **Python**: module-level names without a leading `_` (excluding nested definitions; ignores `__all__`).
  - **Rust**: declarations with unrestricted `pub` (including inside private modules).
  - **TypeScript / JavaScript**: explicit named/default exports and public/default-public members of exported classes (excluding `private`/`protected`/`#private`, type-only declarations).

- **`code_map_overview` & `explain` / `inspect` details**:
  - **Database aspects**: `counts`, `components`, `languages`, `coverage_by_workspace`, `cycles`, `entrypoints`, `entrypoints_with_tests`, `topology`, `compiled_profiles`, and `metadata`.
  - **Discovery aspects**: `discovery`, `manifest_components`, `documentation_hubs`, `runs`, and `milestones`.
  - **Default selection**: omitting `aspects` includes all database aspects + `discovery` except opt-in `cycles`, `topology`, `entrypoints`, and `entrypoints_with_tests` (`entrypoints_with_tests` includes tests; repeat the aspect when paging). Unsupported aspects fail listing valid choices.
  - **`coverage_by_workspace`**: separates persisted primary vs. linked-workspace file/symbol counts across reopens.
  - **Edge rows**: `code_map_explain` returns public endpoint IDs + relative direction (`detail="full"` adds stored evidence); `code_map_inspect` returns `{ "total": 0 }` for empty linked docs.

- **Query operations (`untested`, `implementations`, `duplicates`, `dead_tails`)**:
  - **`untested`**: pages production functions/methods with no inbound `test_case` `calls` edge inside `target`/`selector` (static signal, not runtime coverage; rejects conflicting `target`+`selector`).
  - **`implementations`**: recursively pages subclasses and trait implementors of `selector` (excluding the base type).
  - **`duplicates`**: finds exact body-SHA-256 or near line-similar clones at `level="symbol"` (default) or `level="file"`, bounded by `min_lines` (`5` default in MCP; `10` in CLI unless set), with actionable refactoring `suggestion` entries.
  - **`dead_tails`**: evaluates workspace-wide liveness first and returns scoped `phantom_tested`, `dead_tails`, and `dangling_calls` alongside `summary` counts.

- **Directory boundary slices (`code_map_query(operation="slice", selector="src/db")`)**:
  - Lists externally referenced internal symbols and groups incoming/outgoing edges by external module directory (up to 5 representative edges per module).
  - Pages internal edges and boundary collections together using `offset`, `limit`, and `generation`.
- **Module topology (`code_map_overview(aspects=["topology"])`)**:
  - Returns paged `topology.modules` (grouped by first 2 path components, or 3 under `src/engine`) and `topology.module_edges` sorted by edge count.
  - Computes module **cohesion** (`internal / (internal + inbound + outbound)`) and classifies each module into `adapter_or_entrypoint`, `foundation`, `data_access`, `orchestrator`, or `domain`.
- **Impact edge details (`code_map_impact`)**:
  - Depth `1` returns exact endpoint IDs, edge kind, path, line, direction, confidence, and occurrence count; depth `> 1` records each node's shortest `via` predecessor.

- **Git change view (`code_map_impact`)**:
  - `changes`: pass `"staged"`, `"unstaged"`, or `"head"` instead of a symbol selector.
  - **Response**: returns changed files plus paged `changed_symbols`, impacted `nodes`, related `tests`, and explicit `unmapped_changes` (`complete: false` if any range cannot be mapped because the Git postimage diverges from the index).
  - **`generation`**: binds both the graph and captured Git change snapshot, invalidating older tokens on any working-tree or staged change.

## Result size and source

- **`acdd_state(action: "snapshot")`**: manages checkpoints via `snapshot_action: "save" | "restore" | "clear" | "list"` (`delete: true` aliases `"clear"`), optional `task_ref`, `name`, `all`, `rev`, `run`, `older_than`, and `worktree` (see [task snapshots](tasks.md#task-snapshots)).
- **Preview & line bounds**:
  - `code_map_inspect` / `code_map_explain`: clamp `leading_lines` (`0..=20`, default `5`) and `max_body_lines` (`1..=500`, default from `response.source_context.max_body_lines` or `200`) with an `8 KiB` source sub-budget.
  - `acdd_read`: returns the full selected symbol when it fits `max_output_bytes` (`1,024–98,304`), honoring explicit `max_body_lines >= 1` and paging strictly on complete line boundaries (oversized single lines return a budget error + local-file hint).
- **Pagination & generations**:
  - Code-map tools use `limit` (`1..=100`, default `30`), `offset`, `next_offset`, and code-index `generation` (`source_offset > 0` always requires the previous page's `generation`).
  - Completed zero-result code-map pages return `total` and `items` cleanly without pagination fields; trimming removes tail records cleanly and advances `next_offset`.
  - Task-list pagination (`acdd_state`) reports `total`, `limit`, `offset`, `returned`, `has_more`, and a separate `runs.generation` bound to task/subtask state.
- **Search previews (`code_map_search`)**:
  - Automatically attaches previews when total hits `<= 3` (or whenever `include_preview: true`, up to 3 hits / 20 lines / 12 KiB combined) after verifying the indexed SHA-256 digest.
  - Every hit returns `signature` plus a copy-ready `inspect_selector` object for `code_map_inspect`.

## AST pattern capabilities

- **`ast_grep_search.language`**: accepts any compiled profile ID (`python`, `rust`, `typescript`, etc.) or infers it automatically when `path` is a recognized file or a single-language indexed directory (multi-language dirs require an explicit language).

Call `acdd_guide` with `topic:"ast"` for the profile matrix and syntax examples. Profiles declare these independent capabilities:

| Capability | Behavior |
| --- | --- |
| `complete_syntax` | Parses a standalone pattern accepted at the grammar root; every profile supports this. |
| `declaration_without_body` | Matches declarations after removing profile-declared function, type, class, interface, or implementation bodies. |
| `fragment_probe` | Wraps root-rejected expressions, calls, macros, or assignments in a profile-provided probe container and unwraps the target syntax node. |
| `attribute` | Matches declared decorators or annotations layered over declarations. |

- **Capability matrix**: Rust supports all 4; Python and TypeScript/JavaScript support `complete_syntax`, `declaration_without_body`, and `attribute`; HTML and Vue support `complete_syntax`.
- **Metavariables**: `$NAME` captures one node; `$$$SEQ` captures zero or more nodes (e.g. `class Widget`, `def target($$$)`, `fn target`).
- **Errors**: invalid syntax or unsupported fragments return diagnostic code, language, line/column range, repair hint, and indented examples without raw JSON dumps.

## SQL queries

- **`code_map_query` operations**: `overview`, `inspect`, `explain`, `impact`, `slice`, `unwired`, `sql`, `untested`, `implementations`, `duplicates`, and `dead_tails`.
- **SQL dispatch**: select SQL explicitly (`sql="SELECT ..."` or `operation="sql", selector="SELECT ..."`), or pass a `SELECT`/`WITH` query in `operation` / `query` (resolves `sql` -> `query` -> `selector` -> `operation`). Explicitly named non-SQL operations resolve from `target` -> `selector` -> `query` (`target` applies to `untested`, `duplicates`, `dead_tails`).
- **Read-only guard**: allows one `SELECT` or `WITH` statement under SQLite query & pagination budgets; use `code_map_analyze(include_cycles=true)` for structural cycle detection.

### SQL columns and joins

- **SQLite tables (`code-map.sqlite`)**:
  - `nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id)`
  - `path_dictionary(path_id, path)`
  - `edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count)`
  - `files(path, status, digest, size, language, is_test, generated, path_hash)`
  - `doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant)`
  - Paths and endpoints use integer IDs/hashes (`nodes.path`, `edges.src`, and `edges.dst` do not exist).

```sql
SELECT n.name, p.path, n.line
FROM nodes n JOIN path_dictionary p ON p.path_id = n.path_id
ORDER BY p.path, n.line, n.id;

SELECT src.name AS source, e.kind, dst.name AS target
FROM edges e
JOIN nodes src ON e.src_hash = src.node_hash
JOIN nodes dst ON e.dst_hash = dst.node_hash
ORDER BY src.id, e.kind, dst.id;
```

Missing-table or missing-column SQL errors retain the SQLite error and append a schema hint. State persistence (runs, gates, subtasks, claims, traces, artifacts, blackboard, tombstones) lives separately in `.acdd/state.sqlite`; see [state, tasks & lifecycle](tasks.md).

## Syntax diagnostics on demand

- **Trigger**: call `code_map_analyze` with `lint:true` and an indexed `target` (empty targets the whole workspace; unindexed paths fail with `PATH_NOT_INDEXED`; non-code targets like Markdown preserve compiled-profile errors).
- **Source**: surfaces stored Tree-sitter parse diagnostics (`rule ID`, `severity`, `language`, `path`, `line`, `message`, paged with `generation`) without launching external linters.

```json
{"target":"src","lint":true,"limit":30}
```

- **Scope & limits**: Vue covers embedded `<script>` blocks only (HTML template expressions/styles are skipped); cycle detection requires `code_map_analyze(include_cycles=true)`, and unified rules/hygiene require `code_map_lint`.

## Unified lint and executable `acdd-rule` blocks (`code_map_lint`)

`code_map_lint` (CLI: `acdd lint`) evaluates up to six categories (`arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`), defaulting to `["arch", "syntax", "docs"]`:

- **`acdd-rule` blocks**: Fenced ` ```acdd-rule ` YAML blocks inside non-archive Markdown docs declare machine-enforced rules (`id`, `invariant?`, `severity?`, `kind`, `from?`, `to?`, `owner_paths?`, `entrypoints?`, `zones?`, `depth?`, `edge_kinds?`, `confidences?`, `include_tests?`, `ast_pattern?`, `message?`) validated against `acdd.yaml` `architecture.zones`.
- **Eight `ArchRuleKind` primitives**: `forbid_dependency`, `allow_only_dependencies`, `single_owner`, `forbid_cycles`, `boundary_entry`, `must_implement`, `doc_integrity`, and `ast_pattern`.
- **`syntax` checks**: Evaluates AST `syntax.cognitive_complexity`, `syntax.max_nesting_depth`, and `syntax.high_fanout` against `acdd.yaml` `architecture.complexity` (`ComplexityLimits`), plus `syntax.fact_budget_exceeded` and `syntax.intra_crate_type_homonym`.
- **`duplicates`, `cycles`, and `dead_tails` checks**: `duplicates` hashes AST-statement fingerprint windows (`exact`, `block`, `similar`); `cycles` reports multi-node module/file `imports` SCCs (`cycles.scc`, distinct from call recursion and enforceable at `error` via `forbid_cycles`); `dead_tails` computes workspace-wide production liveness and reports `dead_tails.phantom_tested`, `dead_tails.unreachable`, `dead_tails.dangling_call` (`error`), and `dead_tails.unused_public_api` (`error` on unused/test-only `pub use` re-exports and internal `pub` items lacking non-test cross-module production callers).
- **`docs` checks**: Surfaces stored Markdown/frontmatter/rule errors, broken doc links, broken symbol claims on indexed code files, duplicate non-contract `INV-*` declarations, and orphan invariants lacking a verified `governs` link or `acdd-rule` (plus `docs.unused_arch_zone` warnings for unreferenced zones). Archived paths (`*/archive/*`) are excluded.

## Documentation queries

Use `search_docs`'s optional `component` (alias `path`) for a documentation component or directory
prefix filter, and `query` (alias `pattern`) for search terms. Prefer
`include_excerpt=true` for match-centered triage. Omit `get_doc.section` with
`detail="compact"` to inspect the outline, then supply an exact section title to
read that section.

Markdown search and document results render the actual sections, excerpts when
requested, and selected detail. A path link appears only after the selected
primary or linked workspace root resolves that path. Preserve each returned
collection's page metadata independently. Keep verified document citations,
referenced symbols, and invariant metadata separate from ambiguous candidates;
candidates are navigation, not verified references or governance edges.

- **Document indexing (`DocSection` & `search_docs`)**:
  - Admitted Markdown/MDX (including `AGENTS.md`, milestones, and plans) indexes with optional `doc_type`, `title`, and `status`.
  - Section titles carry 3x the BM25 weight of body/invariant text (`include_excerpt=true` adds match-centered excerpts; stop-word-only queries return empty pages).
  - `get_doc` requires a document path and exact heading title (`detail="compact"` returns a content-free outline).
- **Citations & symbol-claim priority**:
  - Relative links and wikilinks create `cites_doc` / `cited_by_doc` edges only when resolving to one indexed section.
  - Code claims check eligibility first (explicit file target or source document's workspace, excluding doc/invariant/component nodes), then select a singleton winner from the first non-empty tier: exact normalized `qualname` (`::` == `.`), exact declared `name`, then qualified segment suffix.
- **Ambiguous candidates (`referenced_symbol_candidates`)**:
  - Full detail exposes paged `source` + `target` candidate rows for navigation only (zero `documents` or `governs` edges). Compact detail omits candidate rows with a hint; `docs get` requests full detail.
- **Reverse links & doc diagnostics**:
  - `code_map_inspect` deduplicates `documented_in` sections per cited section/line (avoiding duplicate `documents` tables); `code_map_explain` keeps `documents`; `code_map_impact` lists `affected_docs`.
  - `code_map_analyze` reports `broken_doc_link`, `dead_doc_symbol` (with optional `suggested_symbol`), and `stale_doc_sections` (disjoint source line ranges).
- **Invariants (`INV-*`) & `governs` edges**:
  - Uniquely resolved symbol claims within an invariant node's line span emit `governs` / `governed_by` edges (surfaced up to 5 direct + 5 one-hop rules cleanly without truncating task contracts or guidance).
- **Archive exclusion rule**:
  - Documents under any `archive/` path are omitted from default `search_docs` and overview hubs unless `component`/`path` targets `archive` (though `get_doc` remains available).
  - Archived documents emit no `documents`, `references_doc`, `governs`, or `governed_by` edges; `ClaimResp` lenses (`docs.adrs`, `docs.invariants`, `documented_by`) exclude `archive/**`, `docs/milestones/**`, and `docs/contracts/**` (see [ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md#archive-exclusion-from-documentation-search-and-code-documentation-graph)).
