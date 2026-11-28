---
id: MEM-002
title: Git Operations, Worktrees, and Cross-Platform Isolation Memory
spec: memories
status: active
owners:
  - crates/acdd-core/src/git.rs
  - crates/acdd-core/src/path_policy.rs
  - crates/acdd-spec/src/roadmap/**
  - crates/acdd-state/src/git_ops/**
  - crates/acdd-index/src/db/traversal/**
  - src/state_bridge/**
  - scripts/**
---

# Git Operations, Worktrees, and Cross-Platform Isolation

### rule: non-symlink-bidirectional-git-ownership
```yaml
scope:
  - crates/acdd-core/src/git.rs
  - crates/acdd-spec/src/roadmap/**
  - crates/acdd-state/src/git_ops/**
symbols:
  - list_owned_worktrees_info
  - checkout_matches_run
statement: Enforce symlink_metadata non-symlink checks, private <git_dir>/gitdir backlinks, and git symbolic-ref HEAD semantics; never infer worktree ownership from Path::is_dir() or canonical equality alone.
status: active
evidence:
  - AC-006-universal-repository-memory-synthesis
  - crates/acdd-core/src/git.rs
```

### rule: platform-exact-path-and-line-ending-decoding
```yaml
scope:
  - crates/acdd-core/src/git.rs
  - crates/acdd-state/src/git_ops/**
  - tests/**
statement: 'Never blindly strip trailing CRLF on Unix filenames where trailing \r is valid, and guard 0xFF/non-UTF8 filename tests with #[cfg(all(unix, not(target_os = "macos")))] for APFS compatibility.'
status: active
evidence:
  - AC-006-universal-repository-memory-synthesis
  - crates/acdd-core/src/git.rs
```

### rule: staged-renames-and-tracked-binaries-in-snapshots
```yaml
scope:
  - crates/acdd-state/src/git_ops/**
  - crates/acdd-core/src/path_policy.rs
symbols:
  - create_scoped_commit_with_expected
  - guard_candidate_path
  - list_stage_entries
statement: Read live worktree index entries (list_stage_entries) alongside HEAD diff and untracked files so git mv renames enter candidate trees, and keep binary extension/size limits opt-in so binary assets never fail claim.
status: active
evidence:
  - LS-004-rust-entrypoints-and-inline-module-exports
  - UX-004-mcp-tool-surface-consolidation
```

### rule: treeoid-baseline-and-commit-ref-resolution
```yaml
scope:
  - crates/acdd-core/src/git.rs
  - crates/acdd-index/src/db/traversal/**
  - crates/acdd-state/src/usecases/verify.rs
  - src/state_bridge/**
symbols:
  - resolve_git_tree_or_commit
  - verify_memory_spec_evidence
  - collect_changed_symbol_seeds
statement: baseline_ref_for_node returns TreeOid in in_place mode vs CommitOid in worktree mode; diff consumers must use check_req.baseline_ref (falling back to HEAD only when None) and accept tree-ish refs via resolve_git_tree_or_commit.
status: active
evidence:
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
  - crates/acdd-index/src/db/traversal/change_impact.rs
```

### rule: primary-worktree-gitignored-retro-paths
```yaml
scope:
  - .acdd/workflows/**
  - src/state_bridge/docs_domain/**
  - crates/acdd-state/src/usecases/verify.rs
symbols:
  - collect_gitignored_untracked_files
statement: Gitignored docs/memories/.retro/ lives only in primary workspace_root, not linked .worktrees/<run>/ checkouts; handoff must write to primary root, and worktree lens/evidence readers must fall back to primary gitignored specs.
status: active
evidence:
  - docs/adr/0015-retrospective-memory-and-diff-aware-context.md
  - .acdd/workflows/memory_optimize.yaml
```

### rule: macos-ssh-heredoc-and-checksum-rsync
```yaml
scope:
  - scripts/test-macos-ssh.sh
  - scripts/check-full-repo.sh
statement: Pass SSH stdin heredocs via ssh -T (never ssh -n), sync with checksum rsync -rlpcDz --delete without cross-branch mtimes, and capture parallel macOS output via anonymous pipes rather than /tmp files.
status: active
evidence:
  - UX-004-mcp-tool-surface-consolidation
  - scripts/test-macos-ssh.sh
  - scripts/check-full-repo.sh
```

### rule: parallel-wave-worktree-isolation
```yaml
scope:
  - .acdd/workflows/**
  - crates/acdd-state/src/git_ops/**
statement: Never dispatch concurrent mutating Wave tasks into one shared manual checkout or pre-create directories ahead of claim; assign distinct task worktrees to avoid OUT_OF_SCOPE_CHANGES and EXTERNAL_GIT_DRIFT.
status: active
evidence:
  - AR-003-unified-lint-engine-and-documentation-profile
  - LS-004-rust-entrypoints-and-inline-module-exports
```
