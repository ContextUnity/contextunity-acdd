---
id: plan-hotfix-ast-schema-cli-ergonomics
title: "Hotfix Plan: AST Inference/Captures, MCP Schema Compaction, Cross-Tool Dispatch Hints & CLI Parity"
doc_type: plan
status: active
owners:
  - crates/acdd-index/src/ast/search.rs
  - src/engine/ast.rs
  - src/mcp/tools/inputs.rs
  - src/mcp/tools/dispatch.rs
  - src/mcp/tools/code_tools.rs
  - src/mcp/tools/mod.rs
  - src/cli/dispatch_code.rs
  - tests/mcp/
  - docs/reference/cli.md
  - docs/reference/mcp-tools.md
  - skills/acdd/references/tool-catalog.md
invariants:
  - 'INV-HOTFIX-AR005-DISJOINT: Changes stay strictly outside active AR-005 Wave 2 targets (src/format/, src/engine/reader.rs, src/mcp/server.rs, crates/acdd-index/src/db/{reader,symbols,traversal}/, crates/acdd-core/src/response.rs).'
  - 'INV-MCP-SCHEMA-TEST-COMPAT: Schema docstring deduplication preserves required tool inputSchema properties tested by tests/mcp/test_read.rs (selector, symbol) and tests/mcp/context/query_and_policy.rs (format enum).'
---

# Hotfix Plan: Non-Overlapping MCP Schema, AST & CLI Ergonomics

This isolated `hotfix` plan executes the audit findings that have zero functional conflict with the active `AR-005` milestone run (which owns `src/format/`, `src/engine/reader.rs`, `src/mcp/server.rs`, and `crates/acdd-index/src/db/` DTO refactors, handled alongside `UX-003`).

## Parallel Execution Tracks

### Track A: `ast_grep_search` Capture Hygiene, Language Auto-Inference & Default Limit
1. **Drop empty capture entries for anonymous `$$$` wildcards** in `crates/acdd-index/src/ast/search.rs` (`structural_match`): when `name.is_empty()` (`__ACDD_META_`), return `true` without inserting `"": "<matched text>"` into `captures` (removing duplicate syntax payload dumps on every wildcard match).
2. **Exclude config/data grammars from fallback language auto-inference** in `src/engine/ast.rs` (`resolve_ast_languages`): when `detected.len() > 1` parses the pattern, drop `yaml`, `json`, and `toml` whenever at least one programming language matches cleanly (e.g., `Command::ExtendScope` no longer scans YAML files).
3. **Default MCP `ast_grep_search` limit to `15`** in `src/mcp/tools/code_tools.rs` (`ast_grep_search`) when `p.page.limit` is `None`.

### Track B: MCP JSON Schema Compaction, Cross-Tool Dispatch Hints & CLI Parity
1. **Compact repeated MCP tool schema docstrings** in `src/mcp/tools/inputs.rs` (`PageInput` and sibling input structs): replace the 398-character `worktree` paragraph (repeated across 14 tools, ~9.6 KB) and verbose `format` / `generation` / `limit` docstrings with concise canonical sentences while preserving all existing property names (`symbol`, `selector`, `path`, `file_path`, `format`) checked by existing schema contract tests (deferring DB-backed `GetDoc::include_candidates`, `CodeMapSearch::include_pulse`, and `OverviewInput::path` wiring to `UX-003`).
2. **Cross-tool argument suggestions** in `src/mcp/tools/dispatch.rs` (`rejected_arguments_error`): emit actionable cross-tool redirects when common mismatched arguments appear (e.g., `code_map_overview` + `path` -> `acdd_read(path=..., mode="outline") or code_map_analyze(target=...)`, `code_map_tests` + `symbol` -> `selector`, `acdd_blackboard` + `run_id` -> `run`, `get_doc` + `doc_id` -> `path_or_id`).
3. **CLI read/inspect parity** in `src/cli/dispatch_code.rs`: stop forcing `Detail::Full` on CLI `Command::Read` (respecting `ctx.adapter.response.detail` unless expanded) and default `QueryCommand::Inspect` `CoverageOptions::include_coverage` to `false` (matching MCP `code_map_inspect` unless explicitly opted in).
4. **Tests & docs co-evolution**: add regression assertions in `tests/mcp/tool_evolution/mcp_boundaries/ast_and_search.rs` and `tests/mcp/context/query_and_policy.rs` / `tests/mcp/test_read.rs` (verifying `ast_grep_search` omits empty `""` captures, excludes `yaml` on Rust code patterns, `rejected_arguments` returns cross-tool hints, and schema byte budget drops noticeably), and sync `docs/reference/{cli,mcp-tools}.md` + `skills/acdd/references/tool-catalog.md`.

## Verification Gate (Full Hotfix Scope)
1. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
2. `cargo test --test mcp` and `cargo test --test query_context` and `cargo test --test core` and `cargo test --test commitment_integrity`
3. `cargo run -q -- validate && cargo run -q -- docs check && cargo run -q -- lint --categories all --severity error`
4. Rebuild release binary (`~/.local/bin/acdd`), amend the `hotfix` base commit cleanly as requested.
