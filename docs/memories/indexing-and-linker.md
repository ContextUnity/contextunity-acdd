---
id: MEM-005
title: Indexing, Linker Precision, Search, and Lint Engine Memory
spec: memories
status: active
owners:
  - crates/acdd-index/src/**
  - src/engine/**
---

# Indexing, Linker Precision, Search, and Lint Engine

### rule: typed-graph-enums-over-raw-details
```yaml
scope:
  - crates/acdd-index/src/**
  - crates/acdd-core/src/lint.rs
symbols:
  - NodeSemanticEntry
statement: Store AST/linker facts in Copy enums (NodeKind, EdgeKind, EdgeConfidence, LanguageId) and NodeSemanticEntry via struct_codecs! instead of raw node.details[...] lookups or JSON maps (ARCH-L2-NO-RAW-NODE-DETAILS-INDEX).
status: active
evidence:
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
```

### rule: agnostic-linker-sql-keys-and-fallback-discipline
```yaml
scope:
  - crates/acdd-index/src/linker/**
  - crates/acdd-index/src/languages/**
  - crates/acdd-index/src/db/lint/**
  - src/state_bridge/code_domain/signature_scope.rs
symbols:
  - LanguageLinker
  - identifier_keys
  - resolve_imported_member
  - emit_callable_arg
statement: Keep language hooks behind LanguageLinker (no language literals in universal linker/lint), canonicalize SQL keys via identifier_keys, drop single-element fallbacks in resolve_imported_member, and skip emit_callable_arg calls on &var.
status: active
evidence:
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
  - UX-003-knapsack-context-rrf-search-and-sql-callers
  - LS-004-rust-entrypoints-and-inline-module-exports
```

### rule: rrf-paging-pulse-isolation-selectors-and-fts-rowid
```yaml
scope:
  - crates/acdd-index/src/db/symbols/**
  - crates/acdd-index/src/db/reader/**
  - crates/acdd-index/src/db/writer/**
  - crates/acdd-index/src/db/commitments/**
symbols:
  - search_graph_pulse
statement: 'Skip edges queries when include_pulse=false or exact mode is set, stabilize 500-hit RRF shortlist cutoffs, keep FTS5 rowids signed i64 (never hashing u64), and enforce ., ::, and #private selector boundaries.'
status: active
evidence:
  - UX-003-knapsack-context-rrf-search-and-sql-callers
  - PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
```

### rule: value-flow-scope-unfiltered-lint-and-hunk-snapshots
```yaml
scope:
  - crates/acdd-index/src/linker/resolution/value_flow_phase/**
  - crates/acdd-index/src/db/lint/**
  - crates/acdd-index/src/db/traversal/**
symbols:
  - collect_changed_symbol_seeds
  - run_lint
statement: Preserve module lexical scope across value_flow_phase splits (--test linker + import_provenance), evaluate doc_integrity on unfiltered loaded_rules before --rule filtering, and return empty seeds on snapshot overflow without scanning sibling tracked files.
status: active
evidence:
  - AR-002-modularize-db-reader-and-traversal
  - AR-003-unified-lint-engine-and-documentation-profile
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```
