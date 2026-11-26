---
title: "ADR 0012: Fail-Closed Selectors and Read-Only SQL Sandbox"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0012: Fail-Closed Selectors and Read-Only SQL Sandbox

## Status

Accepted.

## Context

In large multi-package workspaces, bare symbol names (`parse`, `validate`, `Config`) match multiple declarations, structural AST searches over thousands of files degrade if every file is parsed blindly with Tree-sitter, and untrusted agent-supplied SQL queries risk mutating `code-map.sqlite` or stalling the server.

## Decision

### 1. Four-Tier Fail-Closed Selector Disambiguation

Symbol and documentation selectors resolve across four descending priority tiers without heuristic guessing:

1. **Tier 1**: Exact canonical node ID (e.g., `def:src/auth.py:parse:14`).
2. **Tier 2**: Qualified name (e.g., `auth::Parser.parse`).
3. **Tier 3**: Path-scoped coordinates (`src/auth.py:parse`, `src/auth.py::parse`, `src/auth.py:14`, `src/auth.py#L14`).
4. **Tier 4**: Bare symbol name (`parse`).

If multiple distinct symbols match in the winning tier, resolution halts with `AMBIGUOUS_SELECTOR` and enumerates candidate IDs and paths. Documentation code claims follow the same first-nonempty-tier uniqueness rule (confined to the document's workspace unless an explicit normalized file path scopes the claim); multi-candidate matches may assist navigation but never fabricate verified `documents` or `governs` edges.

### 2. Prepared AST Patterns & 100%-Recall Literal-Anchor Prefilter (`PF-002`)

Structural `ast_grep_search` / `acdd ast grep` queries validate patterns against the target `LanguageProfile` grammar (`PatternSearchDiagnostic` on unsupported syntax), compile the matcher once per query, and prefilter candidate files safely ([`crates/acdd-index/src/ast/search.rs`](../../crates/acdd-index/src/ast/search.rs), [`src/engine/ast.rs`](../../src/engine/ast.rs), archived [PF-002](../milestones/archive/PF-002-indexing-delta-and-query-acceleration.md)):

- **Declaration index & source literal-anchor prefilter**: `pattern_symbol_name` narrows indexed declaration queries via `nodes` (with full-language fallback when `candidate_files` is empty for local bindings), while [`PreparedAstMatcher`](../../crates/acdd-index/src/ast/search.rs) reuses one parsed pattern and Tree-sitter `Parser` per query and extracts restrictive non-metavariable literal identifier anchors (ignoring `$VAR` / `$$$`, placeholders, string/comment literals, keywords, and synthetic wrappers) so `search_paged` skips `parser.parse` on files lacking those literal anchors (bypassing the source-text shortcut when `\\u` escapes could spell an identifier differently).
- **100% Recall Guarantee (`INV-AST-PREFILTER-RECALL`)**: Whenever a pattern lacks restrictive literal anchors—or when declaration candidate lookup falls back for local constructs—execution runs the full-language ordered Tree-sitter scan, preserving exact result ordering, `SEARCH_MATCH_HORIZON` (`10_000`), continuation offsets, and `generation` parity.

### 3. Read-Only SQL Execution Sandbox (`code_map_query` / `code_map_analyze`)

Agent-supplied diagnostic SQL queries execute inside a strict read-only sandbox:

- Connections enforce `PRAGMA query_only = ON;` inside read-only transactions.
- AST/statement validation admits strictly a single `SELECT` or `WITH` statement, rejecting mutation/DDL keywords (`INSERT`, `UPDATE`, `DELETE`, `DROP`, `ALTER`, `CREATE`, `ATTACH`, `DETACH`, `VACUUM`, `REINDEX`) and semicolons separating stacked statements.
- A two-second SQLite progress-handler budget interrupts runaway queries, and result pages cap at 100 rows (default 12).

## Consequences

- Impact, removal, and read operations never silently target the wrong homonymous symbol.
- Structural AST queries skip non-matching files safely with zero false negatives.
- Agent SQL introspection cannot mutate or lock `code-map.sqlite`.
