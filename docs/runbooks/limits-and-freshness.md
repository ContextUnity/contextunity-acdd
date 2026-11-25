---
doc_type: guide
title: Limits and index freshness
---

# Limits and index freshness

ACDD bounds file admission, graph traversal, queries, and tool responses. A rejected request returns an error with the relevant limit; narrow the scope or page through results.

## Current limits

| Area | Limit | How to proceed |
| --- | --- | --- |
| Scan inventory | 100,000 files; 5 MiB per file; 500 MiB total | Reduce configured roots or ignore generated files. |
| MCP page size | Default 30; range 1–100 | Use `offset` or the returned continuation. |
| MCP response bytes | Configurable 1,024–98,304; default 98,304 | Request fewer rows or less detail; complete collection records are trimmed from the page tail with accurate continuation metadata. |
| Graph depth | At most 16 | Start at depth 1 and expand only where needed. |
| Deep graph frontier | More than 1,000 immediate links blocks depth above 1 | Narrow the selector or retry with depth 1. |
| Removal scope | More than 10,000 nodes is rejected | Analyze a smaller file, package, or symbol. |
| Direct test links | More than 1,000 links is rejected | Narrow the selected scope. |
| AST search | `offset + limit` at most 10,000 matches; 1,000,000 steps and 2 seconds per file; 5-second MCP request budget | Constrain the path and pattern. |
| SQL analysis | 2-second query progress budget; 8 MiB converted result budget | Page results and limit selected columns. |
| Cycle analysis | At most 1,000,000 selected nodes for a path target; 500,000 edges across the graph | Omit cycles if the graph exceeds the edge budget. |
| State pagination | Default 50; range 1–100 (`list`, `blackboard`) | Pass `--limit`, `--offset`, and `--generation` (`runs.generation`). |

These are protective ceilings, not latency promises. A query may finish earlier, return a partial page, or fail on a narrower budget. The configured response limit is independent of the index size.

## Keeping the index current

- **`worktree` selector**: supported across all 20 built-in MCP tools and `<domain>_<name>` plugin tools (omission keeps the server root; explicit selection uses that worktree's adapter and `.acdd/code-map.sqlite`, while Git worktrees share `<primary_worktree>/.acdd/state.sqlite` via `git rev-parse --path-format=absolute --git-common-dir`).
- **`--db` / `ACDD_DB`**: overrides the default code index path (`<root>/.acdd/code-map.sqlite`).
- **`--state-db` / `ACDD_STATE_DB` / `state_db`**: overrides the default state DB (`<primary_worktree>/.acdd/state.sqlite`).
- **`--root` / `ACDD_WORKSPACE_ROOT`**: selects the target repository root (defaults to `.`).
- **MCP freshness vs. CLI**: MCP validates workspace identity and `adapter.digest` per call and auto-runs `delta` or `build` (reusing warm inventory checks for up to 5 seconds), whereas CLI queries read the on-disk index until you run `build` or `delta`.
- **Digest staleness**: `acdd_read` snippet mode fails closed when on-disk source differs from the indexed digest (core text modes still read live files).
- **What triggers reindexing**: changes to `roots`, `specs.*.path`, `linked_workspaces`, `modules.code`, `modules.docs`, or language profiles change `adapter.digest` and trigger rebuilds; changes to `response.*`, `state_db`, `retain_traces`, `profiles_dirs`, or `modules.state` do not.

## Query and source safety

- **Read-only SQL (`code_map_analyze`)**: accepts one read-only `SELECT` or `WITH` statement (`include_cycles=true` computes structural cycles separately). Structural results reflect indexed syntax, not runtime behavior.
- **Inspect/explain source previews**: omitted by default; when enabled, default to `5` leading lines and configured `max_body_lines` (default `100`, valid `1..=500`).
- **`acdd_read` symbol paging**: returns the entire symbol when serialized output fits `max_output_bytes` (`1,024–98,304`); otherwise pages cleanly at complete line boundaries requiring `generation` on continuations (an explicit `max_body_lines` is normalized to `>= 1`; an oversized single line returns a budget error + recovery hint).
- **Read-only state reads**: `acdd status|list|show|next|blackboard --action read`, `acdd_state` reads, and `acdd_blackboard(action="read")` open `state.sqlite` strictly via `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only = ON` without creating missing DB files (`list` and `blackboard` bind `runs.generation`). See [MCP tools](../reference/mcp-tools.md).

## Reader layer admission

- **Text modes**: `mode="lines"` (index-free), `mode="raw"` (preserves line endings), and `mode="search"` (`pattern`, `regex`, bounded `context`).
- **48 KiB text cap**: core text reads cap responses at `48 KiB` and mark `truncated: true` when narrowed windows or `start_line` continuations are needed.
- **Layer fallback**: core text reads still succeed even if the optional code index is absent; graph hints appear only when an index is admitted.
- **`sensitive` & `output_file`**: `sensitive` glob/regex rules block protected files before reading/exporting; `output_file` writes only to plain workspace-relative paths (see [layered reader inputs](../reference/mcp-tools.md#layered-file-reader)).
