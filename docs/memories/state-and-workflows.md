---
id: MEM-003
title: State Engine, Workflow SPI, and Spec Discovery Memory
spec: memories
status: active
owners:
  - crates/acdd-core/src/config/**
  - crates/acdd-core/src/ports.rs
  - crates/acdd-spec/src/**
  - crates/acdd-state/src/**
  - src/state_bridge/**
---

# State Engine, Workflow SPI, and Spec Discovery

### rule: polish-subsystem-sweep-and-live-smoke
```yaml
scope:
  - skills/close-review-findings/SKILL.md
  - crates/acdd-state/src/**
  - src/state_bridge/**
statement: When fixing isolation, cache, visibility, or ranking defects, sweep all equivalent aggregation, fallback, cache-key (with config/schema discriminators), and derived views behind one owner and run a live CLI/MCP smoke check.
status: active
evidence:
  - AC-006-universal-repository-memory-synthesis
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```

### rule: zero-legacy-shims-and-strict-schemas
```yaml
scope:
  - crates/acdd-core/src/**
  - crates/acdd-spec/src/**
  - crates/acdd-state/src/workflow/**
statement: Remove deprecated keys, flags, and fallback wrappers cleanly via serde(deny_unknown_fields), Clap, and SchemaDef instead of compatibility shims or hardcoding spec, field, or repo-path literals in engine crates.
status: active
evidence:
  - AR-006-acdd-spec-crate-and-workflow-cutover
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
```

### rule: verify-before-submit-readonly-freshness-and-env-scrub
```yaml
scope:
  - crates/acdd-state/src/usecases/verify.rs
  - crates/acdd-state/src/usecases/submit/**
  - crates/acdd-state/src/runner/**
  - src/state_bridge/**
symbols:
  - observe_freshness
statement: Enforce explicit verify before submit (TASK_PROOF_UNOBSERVED), keep observe_freshness strictly read-only without phantom index_generation bumps or broken-worktree fallbacks, and scrub ACDD_*/GIT_*/TRACEPARENT in child spawns.
status: active
evidence:
  - AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
  - AR-003-unified-lint-engine-and-documentation-profile
```

### rule: walkdir-depth-zero-dotdir-collection-roots
```yaml
scope:
  - src/state_bridge/**
  - crates/acdd-spec/src/discovery.rs
  - crates/acdd-core/src/config/**
symbols:
  - compute_specs_digest
  - collect_workspace_specs
statement: Guard directory-ignore predicates (SpecConfig.is_ignored_dir_name / .*) with WalkDir depth > 0 while keeping symlink rejection at depth 0 so dot-dir collections (.retro) scan cleanly while .processed/ stays skipped.
status: active
evidence:
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
  - src/state_bridge/common.rs
```

### rule: cascade-reopen-and-worktree-validate-fallback
```yaml
scope:
  - crates/acdd-state/src/machine/schedule.rs
  - crates/acdd-state/src/validate.rs
  - crates/acdd-state/src/usecases/verify.rs
symbols:
  - is_task_claimed_unfinished
  - handle_verify_with_failure_return
statement: Skip NodeStatus::Stale in is_task_claimed_unfinished to avoid --cascade deadlocks, let validate fall back to worktree state_db on WorkflowSchemaInvalid, and keep interactive acdd verify retries separate from acdd run transitions.
status: active
evidence:
  - AR-006-acdd-spec-crate-and-workflow-cutover
  - UX-003-knapsack-context-rrf-search-and-sql-callers
```

### rule: worktree-binary-isolation-and-verify-safety
```yaml
scope:
  - crates/acdd-state/src/usecases/**
  - .acdd/workflows/default.yaml
  - scripts/check-full-repo.sh
statement: Run commands via worktree binary when schemas lead $PATH acdd, never run <worktree>/target/debug/acdd verify run_tests while check_full_repo runs cargo run in the same target/, and clean .acdd/index.db* before submit.
status: active
evidence:
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
  - LS-003-universal-parser-tier1-coverage-and-shell-css-json
```
