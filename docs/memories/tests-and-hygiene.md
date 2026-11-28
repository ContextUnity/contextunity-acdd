---
id: MEM-006
title: Test Suite Architecture, Harnesses, and Code Hygiene Memory
spec: memories
status: active
owners:
  - tests/**
  - crates/**/tests/**
---

# Test Suite Architecture, Harnesses, and Code Hygiene

### rule: no-inline-temp-dir-and-zero-tmp-leaks
```yaml
scope:
  - tests/**
  - crates/**/tests/**
symbols:
  - ScopedTempDir
  - RunHarness
  - PersistedSnapshot
statement: Enforce INV-SINGLE-OWNER-NO-INLINE-TEMP-DIR (no env::temp_dir() in src/ or cfg(test) lib); keep logic tests in RunHarness::in_memory() / PersistedSnapshot, confine disk tests with ScopedTempDir, and verify /tmp BEFORE == AFTER.
status: active
evidence:
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
  - tests/AGENTS.md
```

### rule: extend-existing-harnesses-and-zero-review-bloat
```yaml
scope:
  - tests/**
  - crates/**/tests/**
symbols:
  - write_memory_workspace
statement: Extend existing suite harnesses and owning zero-disk matrices (write_memory_workspace, tests/acdd/support.rs, validate.rs, deltas.rs) instead of cloning StateEngine/git setup or hand-writing SQL inserts.
status: active
evidence:
  - tests/AGENTS.md
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```

### rule: default-indexed-extensions-in-minimal-fixtures
```yaml
scope:
  - tests/core/**
  - tests/mcp/**
  - tests/incremental/**
statement: Default language profiles index json, toml, markdown, bash, and css automatically; never place package.json or extra config fixtures under roots=[src] when asserting max_files=1 source counts.
status: active
evidence:
  - LS-003-universal-parser-tier1-coverage-and-shell-css-json
```

### rule: structural-dedup-without-helper-shredding
```yaml
scope:
  - crates/acdd-core/src/**
  - crates/acdd-spec/src/**
  - crates/acdd-state/src/**
  - src/state_bridge/**
statement: Extract shared helpers only for >= 5-line clones or distinct domain phases (never parameter-bag micro-slices), guard UTF-8 slices with is_char_boundary, and clear duplicates=0 and dead_tails=0 across touched directories.
status: active
evidence:
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```
