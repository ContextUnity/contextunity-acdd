---
id: MEM-001
title: Instructions, Skills, Blueprints, and Worker Orchestration Memory
spec: memories
status: active
owners:
  - AGENTS.md
  - "**/AGENTS.md"
  - skills/**
  - .acdd/workflows/**
---

# Instructions, Skills, Blueprints, and Worker Orchestration

### rule: strict-domain-separation-for-rules-and-skills
```yaml
scope:
  - AGENTS.md
  - "**/AGENTS.md"
  - skills/**
  - .acdd/workflows/**
statement: Keep generic skills free of repository/orchestrator specifics, keep embedded workflows free of host-repo module names, and confine project bindings to .acdd/workflows/default.yaml and AGENTS.md.
status: active
evidence:
  - AC-006-universal-repository-memory-synthesis
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```

### rule: seam-only-blueprints-and-realistic-budgets
```yaml
scope:
  - skills/contract-planning/SKILL.md
  - .acdd/workflows/default.yaml
  - AGENTS.md
statement: Author blueprints at architectural seams with zero pseudocode or raw SQL so review passes on Round 1; allocate ~600-1,000 prod LOC (incl. rustfmt and cfg(test) matrices) for AST/linker tasks or module splits.
status: active
evidence:
  - AR-002-modularize-db-reader-and-traversal
  - AR-003-unified-lint-engine-and-documentation-profile
  - LS-004-rust-entrypoints-and-inline-module-exports
```

### rule: cross-model-integration-review-and-no-builder-scouts
```yaml
scope:
  - AGENTS.md
  - .acdd/workflows/**
statement: Skip read-only scouts for builders, rotate to a fresh builder session on >= 2nd rejections, and pair integration_review with strong cross-model auditors to catch cross-layer regressions.
status: active
evidence:
  - LS-004-rust-entrypoints-and-inline-module-exports
  - UX-003-knapsack-context-rrf-search-and-sql-callers
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
```

### rule: swarm-worker-acdd-cli-ergonomics
```yaml
scope:
  - AGENTS.md
  - skills/acdd/SKILL.md
  - .acdd/workflows/**
statement: Swarm workers lack MCP tools — pass a pre-built binary path (target/debug/acdd -C <worktree>) instead of cargo run wrappers, or use invoke_subagent when MCP access is needed.
status: active
evidence:
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```

### rule: reviewer-awareness-of-engine-managed-and-test-seams
```yaml
scope:
  - AGENTS.md
  - skills/close-review-findings/SKILL.md
  - .acdd/workflows/**
  - crates/acdd-state/src/git_ops/**
  - tests/**
symbols:
  - current_scope_tree
  - engine_managed_paths
statement: 'Remember current_scope_tree excludes active spec_path via engine_managed_paths by design, and verify callers under tests/*.rs before demanding #[cfg(test)] on library test hooks.'
status: active
evidence:
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
```

### rule: docs-and-website-coevolution
```yaml
scope:
  - AGENTS.md
  - docs/AGENTS.md
  - website/**
  - skills/**
statement: Whenever changing user-facing CLI/MCP commands, workflows, or acdd.yaml config, update docs/reference/, skills/, and website/pages/*.md.in together and pass docs check, validate, and ./website/build.sh --check.
status: active
evidence:
  - AR-006-acdd-spec-crate-and-workflow-cutover
  - UX-004-mcp-tool-surface-consolidation
```
