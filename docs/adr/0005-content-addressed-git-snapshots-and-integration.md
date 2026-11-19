---
title: "ADR 0005: Content-Addressed Git Candidate Verification, Commits, and Integration"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0005: Content-Addressed Git Candidate Verification, Commits, and Integration

## Status

Accepted.

## Context

Coupling gate verification to branch commit history (`-p HEAD` or `git merge-base --is-ancestor`) breaks when sibling worktrees modify unrelated files, when `rebase-ff` or `squash` integration rewrites commit SHAs, or when intermediate build gates run before review. Meanwhile, interrupted Git mutations must recover deterministically without a fragile SQLite intent journal or hidden snapshot refs.

## Decision

### 1. Scoped Candidate `TreeOid` Verification and Explicit Commit Persistence

- **Deterministic scoped `TreeOid` (no snapshot refs)**: Candidate state is verified directly against deterministic Git tree OIDs (`WorkspacePathPolicy::is_candidate_included`) without touching the working branch index, creating `HEAD` commits prematurely, or storing hidden `refs/acdd/snapshots/*` refs. Explicit `acdd snapshot`, `save: snapshot`, and `save.pre` do not exist; `save` / `save.post` support `none | commit` only.
- **Content equivalence via `TreeOid`**: Gate proofs, trace freshness (`candidate_tree_before == candidate_tree_after`), `save.immutable: true`, `need:` predecessor bindings, and `TASK_NOT_INTEGRATED` completeness checks compare exact scoped Git tree OIDs (`TreeOid`) rather than branch commit ancestry. Rebase, squash, cherry-pick, and fast-forward merges preserve accepted proofs whenever scoped tree content matches.

### 2. Declarative `git:` Policy, Atomic Plumbing Commits, and `rebase-ff` Wave Landing

Profiles configure Git execution declaratively under `git:` ([`GitPolicy`](../../crates/acdd-state/src/profile/schema.rs)):

- **`isolation`**: `worktree` | `in_place`
- **`worktrees`**: `manual` | `managed`
- **Atomic plumbing commits (`save.post: commit`)**: Task and handoff delivery creates scoped commits via Git plumbing (`write-tree` + `commit-tree` + CAS `update-ref`), stamping `Acdd-Claim` without invoking shell hooks or maintaining shadow VCS toggles (`commit_mode` and `git_commit_notes` do not exist).
- **Task branch integration (`rebase-ff`)**: Headless `acdd run` waves land completed task branches into the run branch internally via `rebase-ff` ([`integrate_task_branch`](../../crates/acdd-state/src/git_ops/integrate.rs)), stamping `Acdd-Integrated`. Manual worktree runs integrate task branches with standard `git merge` / `git rebase`, and terminal verification confirms content equivalence by Git `TreeOid` ([`verify_tasks_integrated`](../../crates/acdd-state/src/git_ops/integrate.rs) / `TASK_NOT_INTEGRATED`).
  - Protected branches (`main`, `master`) fail closed with `MAIN_BRANCH_PROTECTED` unless `--allow-main` is explicitly supplied.
  - `submit` never performs cross-worktree merges; integration conflicts abort cleanly without leaving `MERGE_HEAD` or `REBASE_HEAD` on disk (`GIT_INTEGRATION_CONFLICT`).

### 3. Immutability, Scope Conflicts, and Review Contours

- **Candidate drift & rebinding**: Non-runtime drift on `save.immutable: true` gates fails closed with `TASK_CANDIDATE_MISMATCH`. Reopening (`acdd reopen`), confined Git restore, re-verifying, and obtaining fresh independent review binds the new candidate cleanly.
- **Hierarchical scope conflict detection**: `acdd extend-scope` checks exact, parent, and child path overlap against other claimed incomplete tasks (`TASK_SCOPE_CONFLICT`) and blocks `.acdd/**` and `protected_paths` (`OUT_OF_SCOPE_CHANGES`).
- **Six review contours**: `paths`, `claims`, `architecture`, `concurrency`, `project_isolation`, and `administration` govern independent reviews (`proof: review`).

### 4. Stateless Git Recovery Without `op_intents`

No separate SQLite `op_intents` intent journal exists. Under `<git_common_dir>/acdd/repo.lock`:

- `commit` stamps `Acdd-Claim: <run_id>/<node_id>/<claim_rev>`, enabling interrupted `submit` retries to adopt an already-committed matching tree idempotently or fail closed with `EXTERNAL_GIT_DRIFT`.
- [`integrate_task_branch`](../../crates/acdd-state/src/git_ops/integrate.rs) aborts leftover `MERGE_HEAD` / `rebase-merge` / `rebase-apply` markers before executing and stamps `Acdd-Integrated: <run_id>/<task_ref>/<accepted_tree>`.
- Terminal `archive` (`is_ephemeral == 0`) tracks `run_tombstones.cleanup_pending = 1` so interrupted post-commit cleanup completes idempotently on the next run command.

## Consequences

- Rebase, squash, and fast-forward workflows integrate without invalidating accepted task proofs.
- Interrupted commits, integrations, and terminal cleanups recover cleanly from native Git state and `run_tombstones` without dual-write intent-table drift.
- Scoped `TreeOid` calculations remain immune to runtime `.acdd/*.sqlite*` files and sibling branch commits.
