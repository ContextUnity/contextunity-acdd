[mode overridden: auto -> full, reason=instruction file requires complete content]

[mode overridden: auto -> full, reason=instruction file requires complete content]

# ACDD MCP and CLI tool catalog

Use the active server registry and schemas as the final authority. This catalog is a compact navigation aid; repository-specific behavior and current schemas belong in that repository's reference docs.

## State lifecycle (5 MCP tools + flat CLI)

The server exports **20 built-in static MCP tools** (`11 code + 2 docs + 2 core + 5 state`) plus any optional `<domain>_<name>` plugin tools exported by external `DomainProvider` crates:

- `acdd_state(action, ...)` dispatches lifecycle commands (`start`, `draft`, `extend_scope`, `reopen`, `sync`, `subtask`, `snapshot`, `clear`, `run`) and read-only queries (`status`, `list`, `show`, `next`, `roadmap`, `context`, `observability`). CLI parity: flat top-level commands (`acdd start`, `acdd draft`, `acdd status`, `acdd list`, `acdd show`, `acdd next`, `acdd roadmap`, `acdd context`, `acdd extend-scope`, `acdd reopen`, `acdd sync`, `acdd subtask`, `acdd snapshot`, `acdd clear`, `acdd run`, `acdd tool <domain>_<name>`, `acdd validate`).
- `acdd_claim(node?, worktree?, worker_id?, force?, steal_remote?)` claims the active gate and returns its 6-tier budgeted context (`contract + guidance`, `unresolved_findings`, `pre_task_slice`, `dependency_receipts`, `blackboard`, `context` lenses, plus `context_reset_required` after `rearchitect`).
- `acdd_verify(node?, test?, worktree?)` observes claim-bound command/check/call verification without advancing state (`execution_digest` + `candidate_tree_before`/`after`).
- `acdd_submit(node?, action, evidence?, findings?, worktree?, allow_main?)` submits `pass` or `reject` (supporting `rejection_findings.kind`: `defect | architectural_drift`) against a fresh trace (or auto-verifies when `--test` is not required) and executes `save.post` (`none | commit | snapshot`), `archive`, and `prune`.
- `acdd_blackboard(action, run?, node?, recipient_node?, subtask_ref?, gate?, author?, topic?, status?, payload?, limit?, offset?, generation?, worktree?)` posts (`action="post"`) or reads (`action="read"`) `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred` messages.
- `ListReq` (`acdd list`) and `BlackboardReq` (`acdd blackboard read`) paginate with `limit` (`1..=100`, default `50`), `offset`, and `generation` (`runs.generation` fence).
- Results default to Markdown (`ADR-0006`); request MCP `format: "json"` or CLI `--format json` when parsing structured fields.

## Discovery and search

Built-in tools accept an optional `worktree` selector (`acdd_claim` and `acdd next` bind execution affinity to the target checkout). Without a selector, keep the canonical server root regardless of active claims. Use `worktree: "."` to select that root for queries or manual worktrees, but **omit `worktree` when calling `acdd_claim` on a `git.worktrees: managed` `TaskGate`**—passing an explicit `worktree` overrides and bypasses automatic creation of `.worktrees/<run_name>-<task_ref>`, whereas leaving `worktree` unset creates/reuses the managed task worktree and returns it in `ClaimResp.worktree`.

Otherwise, search canonicalized and deduplicated Git-registered worktrees and immediate `.worktrees/` children in this order: exact directory name; literal prefix or normalized numeric prefix; literal suffix; literal substring. The first nonempty tier must be unique. Multiple candidates fail without falling through to a lower tier; empty or unmatched selectors fail.

Git-linked worktrees resolve `<primary_worktree>/.acdd/state.sqlite` and `<git_common_dir>/acdd/{repo.lock,backup.lock,traces.lock,state.bak.sqlite}` cleanly across checkouts. MCP requests do not transmit the client's current directory; CLI global `--root` selects a checkout, and `acdd claim`/`acdd next` `--worktree` takes an existing directory path.

For efficient navigation, confirm workspace and coverage with overview, reuse sufficient search previews, inspect one declaration, and explain only when relation edges are needed. Use `acdd_read(selector)` for complete symbol bodies when previews are insufficient. Triage docs with `search_docs(query="...", include_excerpt=true)` and inspect `get_doc(path_or_id, detail="compact")` outlines before reading selected sections with full content. Exact `code_map_tests` call paths are verified test relationships; possible calls and lexical suggestions are not. Removal analysis is indexed static evidence, not runtime-safety proof. Lint reads stored syntax diagnostics; cycles are opt-in.

- `code_map_overview(aspects?, worktree?, limit?, offset?, detail?, generation?)` returns workspace-wide root, components, counts, language coverage, profiles, and persisted primary/linked `coverage_by_workspace` (has no `path` parameter; use `code_map_search` or `code_map_analyze` for directory scope). Database aspects are `counts`, `components`, `languages`, `coverage_by_workspace`, `cycles`, `entrypoints`, `entrypoints_with_tests`, `topology`, `compiled_profiles`, and `metadata`; adapter facets are `discovery`, `manifest_components`, `documentation_hubs`, and `runs` (with `'milestones'` alias). Omitted/empty aspects include all database aspects except opt-in `cycles`, plus adapter facets.
- `code_map_search(pattern, kind?, classification?, path?, exact?, include_docs?, include_preview?, group_by_file?, worktree?, detail?, limit?, offset?, generation?)` returns eligible exact name/qualname matches alone when present by default, otherwise ranked FTS/prefix results. `query` aliases `pattern`; `file_path` aliases `path`; `classification` filters to `production`, `test`, or `documentation`; `exact=true` requires an indexed exact name/qualname; `pattern="*"` needs a `path`, `kind`, or `classification` scope.
- `ast_grep_search(pattern, language?, path?, worktree?, detail?, limit?, offset?, generation?)` uses Tree-sitter `$NAME` and `$$$ARGS` captures (`query` aliases `pattern`; `file_path` aliases `path`; default MCP `limit` is `15`). Omit `language` to infer it from `path` or search compatible indexed languages in scope (excluding `yaml`/`json`/`toml` when a programming language parses cleanly); supply `language` to force a single language. Anonymous `$$$` wildcards omit empty `""` entries from `captures`. CLI: `acdd ast grep [--lang rust] [--path file.rs] 'PATTERN'`.
- `search_docs(query, doc_type?, component?, include_excerpt?, worktree?, limit?)` searches indexed docs, guides, and ADRs (`pattern` aliases `query`; `path`/`file_path` alias `component`). Use `search_docs(query="...", include_excerpt=true)` for match-centered triage; use `get_doc(path_or_id, detail="compact")` (`path`, `file_path`, `doc_id`, `selector` aliases accepted) for an outline, then `get_doc(path_or_id, section="...")` for full section content.

## Inspection and hierarchy

- `code_map_inspect(selector, path?, worktree?, include_coverage?, show_doc?, show_source?, leading_lines?, max_body_lines?, source_offset?, generation?)` (`symbol` aliases `selector`; `file_path` aliases `path`) returns symbol summary, receiver metadata, exact callers/callees, verified `documented_in`, compact governing invariants, paged `analysis_gaps`, and candidate dispatch targets (`possible_calls`), with `include_coverage` off by default on both MCP and CLI (`acdd query inspect`).
- `acdd_read(path?, selector?, mode?, start_line?, line_count?, pattern?, regex?, context?, baseline?, output_file?, worktree?)` (`file_path` aliases `path`; `symbol` aliases `selector`) reads text without an index in core modes (`lines`, `raw`, `search`, `delta`) and respects `response.detail` (`compact` by default) on both MCP and CLI (`acdd read`). Markdown `outline` requires `docs`; code `outline` and symbol `snippet` require `code`. Active claims add `in_write_scope` (`ALLOWED` | `OUT_OF_SCOPE`) and hints. CLI parity: `acdd read <path> --mode <mode>`.
- `acdd_read(selector, ...)` returns the complete digest-verified symbol when its serialized response fits the configured byte limit; otherwise it pages at complete source-line boundaries using explicit `generation` tokens.
- `code_map_explain(selector, path?, direction?, worktree?, include_coverage?, show_doc?, show_source?, leading_lines?, max_body_lines?, source_offset?, generation?)` (`symbol` aliases `selector`; `file_path` aliases `path`) returns endpoint IDs, query-relative direction, ownership, invariants, and candidate dispatch.
- `code_map_impact(selector?, changes?, path?, direction?, depth?, worktree?, detail?, limit?, offset?, generation?)` (`symbol` aliases `selector`; `file_path` aliases `path`) traces inbound/outbound dependencies or Git `changes` (`staged`, `unstaged`, `head`).
- `code_map_tests(selector, path?, direction?, worktree?, detail?, limit?, offset?, generation?)` (`symbol` aliases `selector`; `file_path` aliases `path`) follows exact call paths from `test_case` roots or from tests into production.

## Verification and utility

- `code_map_prove_removal(selector, path?, worktree?)` (`symbol` aliases `selector`; `file_path` aliases `path`) considers static references and blocks removal for candidate callers (`possible_calls`).
- `code_map_lint(target?, categories?, rules?, severity?, worktree?, detail?, limit?, offset?, generation?, format?)` (`path`/`file_path` alias `target`) runs unified architectural (`acdd-rule` / `acdd.yaml` `architecture`, including `forbid_cycles` and `ast_pattern`), graph hygiene (`duplicates`, `dead_tails`, `cycles`), `syntax` (`syntax.parse`, `fact_budget_exceeded`, `intra_crate_type_homonym`; AST `cognitive_complexity` and `max_nesting_depth` remain in `node.details`), and `docs` lint rules (default categories `[arch, syntax, docs]`). CLI: `acdd lint [TARGET] [--categories ...] [--rule ...] [--severity ...]`.
- `code_map_analyze(target?, worktree?, lint?, include_cycles?)`: empty target means workspace diagnostics; a path (`path`, `file_path`, `selector` aliases) means scoped diagnostics with digest-verified snippets/reasons where known.
- `code_map_query(operation, selector?, target?, path?, level?, min_lines?, worktree?, include_coverage?, depth?, limit?, offset?, generation?)` (`symbol` aliases `selector`; `file_path` aliases `path`) returns graph slices, unwired nodes, implementations, untested symbols, code clones/duplicates (`operation="duplicates"` using AST-statement windows), dead-code/phantom-test analysis (`operation="dead_tails"`, returning workspace-liveness-checked `phantom_tested`, `dead_tails`, and `dangling_calls` scoped by `path` or `target`), or read-only SQL (`operation="sql"`).
  - Mandatory quality checks: builders and independent reviewers run `code_map_query(operation="duplicates", path=<scope>)` and `code_map_query(operation="dead_tails", path=<scope>)` (omitting `path`/`target` for workspace-wide `integration_review`) and eliminate or reject new clones, `phantom_tested` dead code, `dead_tails`, `dangling_calls`, and module import cycles.
- `acdd_guide(topic?, force?, worktree?, format?)` defaults to `query`; its `topics` index advertises enabled `lifecycle`, `acdd`, `gates`, `subtasks`, `code`, `docs`, `ast`, `adapter`, `init`, and `validate` topics. CLI: `acdd guide TOPIC`. Known disabled topics return `{}`; blackboard instructions disappear when `blackboard` (or `state`) is disabled. Follow the active profile and claim `guidance`.
- Selector forms include canonical node IDs; paths (`src/scanner.rs`, stripping `file://`, `file:`, `./`); path+symbol (`src/scanner.rs:scan`); and path+line (`src/scanner.rs:42`, `src/scanner.rs#L42`). `.md` selectors route to `get_doc` or `search_docs`.

## Documentation and invariant graph

Admitted Markdown/MDX, including spec collections configured in `specs:`, is indexed with section locations and frontmatter status (`default_doc_type` classifies files under `specs.*.path` as `contract`). Confined relative links/wikilinks create `cites_doc`/`cited_by_doc` only when one section resolves. Documentary code claims are eligible in the source document's workspace unless an explicit normalized file path confines them elsewhere.

`INV-*` declarations become invariant nodes; `governs` requires a uniquely resolved code claim within the invariant node's line span. Archived specs under `*/archive/` are excluded from default documentation search and hubs; `get_doc` remains available.

### SQL columns and joins (`code-map.sqlite`)

The code-map SQL schema is `nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id)`, `path_dictionary(path_id, path)`, `edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count)`, `files(path, status, digest, size, language, is_test, generated, path_hash)`, and `doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant)`. Paths and endpoints use integer IDs/hashes.

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
