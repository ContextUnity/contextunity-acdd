---
id: MEM-004
title: MCP, CLI, Response Budgets, and Dual-Surface Parity Memory
spec: memories
status: active
owners:
  - crates/acdd-core/src/response.rs
  - crates/acdd-core/src/format.rs
  - src/mcp/**
  - src/cli/**
  - src/format/**
  - crates/acdd-state/src/context/**
---

# MCP, CLI, Response Budgets, and Dual-Surface Parity

### rule: knapsack-scaling-and-hirschberg-tiebreak
```yaml
scope:
  - crates/acdd-core/src/response.rs
  - crates/acdd-state/src/context/**
  - src/state_bridge/**
symbols:
  - pack_knapsack_indices
  - pack_json_lens
statement: Route ClaimResp and lens budgeting through shared knapsack helpers with exact 1-byte overhead, scaled DP above 65,536 cells, original-rank Hirschberg tie-breaks, and explicit tier1_cap vs response_hard_cap.
status: active
evidence:
  - UX-003-knapsack-context-rrf-search-and-sql-callers
  - crates/acdd-core/src/response.rs
```

### rule: compact-claim-specs-lens-payload-budget
```yaml
scope:
  - src/state_bridge/docs_domain/**
  - crates/acdd-state/src/context/**
  - docs/memories/**
symbols:
  - render_specs_lens
statement: Tier 6 specs.* lenses share the 10 KB compact ClaimResp cap; emit only fields consumed by render_specs_lens (omit redundant fields maps/duplicate keys) and keep rule statements <= 200 chars so multiple rules fit.
status: active
evidence:
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
  - crates/acdd-state/src/context/markdown.rs
```

### rule: canonical-worktree-paths-and-cli-markdown-parity
```yaml
scope:
  - src/cli/**
  - src/mcp/**
  - src/engine/guide.rs
  - src/format/**
statement: Canonicalize --root and --worktree independently against ctx_shell CWD drift, test default Markdown alongside --format json across all CLI flags (--detail, --mcp), and reject non-normalized overview path args fail-closed.
status: active
evidence:
  - UX-004-mcp-tool-surface-consolidation
  - UX-003-knapsack-context-rrf-search-and-sql-callers
```

### rule: query-sqlite-toobig-and-context-noise-filtering
```yaml
scope:
  - crates/acdd-index/src/db/paging.rs
  - src/state_bridge/code_domain/**
  - src/format/**
symbols:
  - QueryBudget
statement: 'Bound graph paging (db/paging.rs, QueryBudget, InterruptHandle) against SQLITE_TOOBIG, deduplicate Scoped Tests/Callers paths, and filter #[derive] (.ne/.le/.ge) and router TOC spam to stay within compact caps.'
status: active
evidence:
  - AR-002-modularize-db-reader-and-traversal
  - LS-004-rust-entrypoints-and-inline-module-exports
  - UX-004-mcp-tool-surface-consolidation
```

### rule: cross-platform-mcp-server-and-process-teardown
```yaml
scope:
  - src/mcp/server/**
  - crates/acdd-core/src/fs.rs
  - tests/mcp/**
symbols:
  - ReloadSignal
statement: When updating MCP reload handling, detail formatting, or ReloadSignal executable-identity checks, verify process-group descendant deadline teardown and cfg(target_os = "macos") behavior rather than assuming Linux parity.
status: active
evidence:
  - UX-004-mcp-tool-surface-consolidation
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
```
