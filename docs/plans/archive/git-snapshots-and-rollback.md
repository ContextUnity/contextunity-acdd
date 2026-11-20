---
id: git-snapshots-and-rollback
title: Lightweight Git Checkpoints (snapshot save/restore/clear)
doc_type: plan
status: completed
owners:
- crates/acdd-state/**
- src/**
- docs/adr/0005-content-addressed-git-snapshots-and-integration.md
- skills/acdd/SKILL.md
invariants:
- 'INV-CONFINED-SNAPSHOTS: Lightweight snapshot refs (`refs/acdd/snapshots/<run_id>/<task_ref>/*` and standalone `refs/acdd/snapshots/_adhoc/<name>/*`) never mutate `HEAD`, branch refs, or `.git/index`, and `restore` mutates only confined files inside the target worktree.'
- 'INV-BOUNDED-LOC-SNAPSHOTS: Production state/git snapshot changes remain minimal and free of SQLite `op_intents` journals (T-03 extension <= 90 net prod LOC across `acdd-state` + `src/`).'
acceptance_criteria:
- 'AC-1: `acdd snapshot save|restore|clear [<task_ref>] [--all] [--rev N]` and `acdd_state(action: snapshot)` work across active tasks.'
- 'AC-2: Restoring `SaveAction::Snapshot` (`save.post: snapshot` on the `build` gate in `embedded/code.yaml`) automatically saves `refs/acdd/snapshots/<run_id>/<task_ref>/r<claim_revision>` on gate acceptance without moving `HEAD`, and `deliver`, `clear`, or `handoff` automatically delete all associated snapshot refs.'
- 'AC-3: Narrow ADR 0005 amendment explicitly authorizes only auto-cleaned `refs/acdd/snapshots/...` checkpoints and manual standalone `refs/acdd/snapshots/_adhoc/...` checkpoints, and `skills/acdd/SKILL.md` + `AGENTS.md` + reference docs document agent usage.'
- 'AC-4: Standalone checkpoints (`acdd snapshot save <name> [--rev N] [--worktree PATH]`, `restore`, `list`, and `--delete --older-than <DURATION> | --all | <name> [--rev N]`) work without an active `run` or existing task.'
closure:
  status: completed
  closed_at: 2026-11-20T05:34:42.191Z
  target_base: main
  target_base_commit: c9c58e8a6a10834c7862c0460bed5a85a49bdf0d
  task_receipts:
    T-03:
      run_id: git-snapshots-and-ro-20261119192005-316933-1
      node_cursor: root:T-03.deliver
      gate_id: deliver
      worker_id: coordinator-snap-t3
      claim_revision: 9
      contract_revision: 4
      candidate_tree: fc07d28c80afa386eba1685ee77ff3e435e3449d
      committed_oid: eca2460e280f91aab0c0d59d0daf107a80312208
      accepted_at: 2026-11-20T05:18:54.424Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-CONFINED-SNAPSHOTS: Standalone `_adhoc` and per-task snapshots never touch `HEAD`, `.git/index`, `.acdd/state.sqlite` (in standalone mode), or files outside target worktree policy.'
          - 'INV-SINGLE-OWNER: CLI (`--delete`, `--older-than`, `--all`, `--worktree`) and MCP share `StateCommand::Snapshot(SnapshotReq)` with zero shadow git implementations.'
          - 'INV-BOUNDED-LOC-SNAPSHOTS: Net production delta across `acdd-state` + `src/` for T-03 stays <= 90 LOC.'
          verified: []
  rollup:
    verified_invariants:
      applicable:
      - 'INV-CONFINED-SNAPSHOTS: Standalone `_adhoc` and per-task snapshots never touch `HEAD`, `.git/index`, `.acdd/state.sqlite` (in standalone mode), or files outside target worktree policy.'
      - 'INV-SINGLE-OWNER: CLI (`--delete`, `--older-than`, `--all`, `--worktree`) and MCP share `StateCommand::Snapshot(SnapshotReq)` with zero shadow git implementations.'
      - 'INV-BOUNDED-LOC-SNAPSHOTS: Net production delta across `acdd-state` + `src/` for T-03 stays <= 90 LOC.'
      verified: []
    decisions:
    - 'ARCH DECISION (T-03 standalone snapshots + age/bulk delete): Extend `StateCommand::Snapshot` (`SnapshotReq`: add `SnapshotAction::List`, `name: Option<String>`, `worktree: Option<PathBuf>`, `older_than: Option<String>`) and `Command::Snapshot` (`--delete`, `--older-than <DURATION>`, `--all`, `--worktree`, `list`) under `INV-SINGLE-OWNER` without adding SQLite tables or shadow git runners. Standalone refs live strictly under `refs/acdd/snapshots/_adhoc/<label>/r<rev>` (disjoint from per-task `refs/acdd/snapshots/<run_id>/<task_ref>/r<rev>` so `cleanup_refs` on deliver/clear/handoff never removes `_adhoc/*`). When `--run` is omitted and either no unambiguous active run/task matches or a standalone name/flags are used, route to `_adhoc` using `resolve_repo_layout` + `WorkspacePathPolicy` (`restore_task_snapshot` takes `scope: Option<&[String]>` matching `current_scope_tree`, restoring all `is_candidate_included` paths outside `.acdd/**`). `--delete` aliases `Clear`; `--older-than <Ns|Nm|Nh|Nd|Nw>` filters Git refs via `for-each-ref --format=%(refname)%09%(committerdate:unix)` and deletes under `repo.lock`, scoped to `_adhoc/` unless `--run` is explicitly passed. Target net prod budget <= 90 LOC across `acdd-state` + `src/`.'
    - |-
      REVISED ARCH DECISION (addressing Finding #115):
      1. Zero-DB-creation (`src/state_bridge/mod.rs`, `crates/acdd-state/src/usecases/mod.rs`, `task_ops.rs`): Change `handle_snapshot_dyn` to accept `&dyn StateReader` and run under `repo.lock` without SQLite writes/maintenance. Route `StateCommand::Snapshot` in `execute_state_command_with_config` (`src/state_bridge/mod.rs:165`) via a shared `execute_snapshot_readonly` helper backed by `SqliteStateReader` (reading `state.sqlite` only when present on disk, never calling `StateEngine::open` / `SQLITE_OPEN_CREATE`), while `StateEngine::execute` forwards `backend.as_reader()`. Both files are added to T-03 `scope:`.
      2. CLI positional normalization (`src/cli/mod.rs`): Drop `value_parser` on `Command::Snapshot.action: Option<String>` and validate manually; when `--delete` is passed with `target: None` and `action` holds a target name (not `save|restore|clear|delete|list`), shift `action` into `target`; wire `StateCommand::Snapshot(req) => req.worktree.as_deref()` in `CliStateCtx::command`.
      3. MCP worktree parity (`src/mcp/tools.rs`, `src/mcp/tools/inputs.rs`): Resolve `p.worktree.as_ref().and_then(WorktreeInput::as_selector)` (not `ctx.target_root`, which is `server.root`) via `resolve_target_root` / `SnapshotReq.worktree`; add `name`, `older_than`, and `delete` (`clear` alias) fields on `StateInput`.
      4. Bounds & edge cases: Unify `--older-than <Ns|Nm|Nh|Nd|Nw>` across plan/blueprint, handle empty `for-each-ref` cleanly in `restore_task_snapshot`, allow `--rev N` on `Clear` for single-revision deletion, and enforce `T-03 <= 90` net prod LOC across `acdd-state` + `src/`.
    - |-
      PF-003 Coordinator Authority Decision (Admitting tx.rs and cli/mod.rs into Run Scope):
      1. Root cause & justification:
         - crates/acdd-state/src/store/sqlite/tx.rs: Fixed a critical SQLite foreign-key constraint violation in apply_sync_plan where new_subtasks were inserted before added_nodes, unblocking acdd sync for Wave 2 & 3 tasks.
         - src/cli/mod.rs: Reconciled test cli_exit_codes_preflight_init_validate_and_flat_dispatch with the relaxed Command::Snapshot parsing introduced in 05e3dd42e, verifying legacy acdd snapshot syntax fails at execution time.
      2. Authority & Scope Admission:
         - Both paths are officially recorded in owners: of docs/milestones/PF-003-delta-seal-cold-pipeline-and-storage-acceleration.md (commit af13b8a6).
         - As run coordinator, I explicitly admit crates/acdd-state/src/store/sqlite/tx.rs and src/cli/mod.rs into the PF-003 milestone scope.
         - All 52 changed paths now sit strictly within admitted owners and scope boundaries.
  metrics_summary:
    decisions_count: 3
    deferred_findings_count: 0
    failed_traces: 44
    passed_traces: 69
    task_receipts_count: 1
    tasks_cancelled: 0
    tasks_completed: 1
    tasks_total: 1
    total_claims: 95
    total_review_cycles: 1
    total_traces: 113
---

# Plan: Lightweight Git Checkpoints (`snapshot save/restore/clear`)

> Delivered baseline (`b7fd354f7` / `c9c670886`): `T-01` (per-task `refs/acdd/snapshots/<run_id>/<task_ref>/r<rev>` checkpoints, `save.post: snapshot`, confined `restore`, auto-cleanup) and `T-02` (`acdd snapshot` CLI + `acdd_state(action: "snapshot")` MCP parity + ADR 0005 & docs). This reopened revision adds `T-03` for flexible standalone/ad-hoc checkpoints and age/bulk pruning.

### task: T-03

```yaml
task_ref: T-03
target: Add standalone ad-hoc named snapshots (`acdd snapshot save|restore|list <name> [--rev N] [--worktree PATH]`) without requiring a run/task, plus `--delete --older-than <DURATION> / --all`, within <= 90 net prod LOC.
spec: |
  Extend `StateCommand::Snapshot` (`crates/acdd-state`) and CLI/MCP surfaces (`src/`) cleanly via `INV-SINGLE-OWNER`:
  - Zero-DB-creation routing (`crates/acdd-state/src/usecases/mod.rs`, `crates/acdd-state/src/usecases/commands/task_ops.rs`, `src/state_bridge/mod.rs`):
    - Change `handle_snapshot_dyn` to take `&dyn StateReader` and run under `with_repo_lock_and_maintenance` (or direct `with_repo_lock` without `after_commit_maintenance`) so snapshot execution never mutates SQLite.
    - Route `StateCommand::Snapshot` inside `execute_state_command_with_config` (`src/state_bridge/mod.rs`) through a canonical reader-only entrypoint (`execute_snapshot_readonly` using `SqliteStateReader` when DB exists, without calling `StateEngine::open` / `SQLITE_OPEN_CREATE`), while `StateEngine::execute` forwards `backend.as_reader()` cleanly.
  - DTOs & actions (`crates/acdd-state/src/usecases/dto.rs`):
    - Add `SnapshotAction::List` (`list`), plus optional `name: Option<String>`, `worktree: Option<PathBuf>`, and `older_than: Option<String>` on `SnapshotReq` (keeping `task_ref: Option<TaskRef>`, `all: bool`, `revision: Option<u32>`, `run: Option<String>` backward-compatible).
  - Ref & tree primitives (`crates/acdd-state/src/git_ops/snapshot.rs`, `crates/acdd-state/src/git_ops/worktree.rs`):
    - Support `scope: Option<&[String]>` in `restore_task_snapshot` (`None` restores all `policy.is_candidate_included` paths in target worktree, enforcing `checked_child` / `guard_candidate_path` / byte rollback while never touching `.git/**`, `.worktrees/**`, or `.acdd/**`; handle empty `for-each-ref` cleanly without returning an empty ref error).
    - Support standalone ref prefix `refs/acdd/snapshots/_adhoc/<label>/` (validated `[A-Za-z0-9._-]`, non-empty, `<= 64` chars, no `..`, not `.` or `_adhoc`), auto-next revision when `save` omits `revision` in standalone mode, `list` via `git for-each-ref`, and age-filtered / revision-scoped deletion (`--older-than <Ns|Nm|Nh|Nd|Nw>` parsed against Git commit `%(committerdate:unix)` and `--rev N` / `--all`). Terminal run cleanup (`cleanup_refs`) never deletes `_adhoc/*` refs.
  - CLI & MCP surfaces (`crates/acdd-state/src/usecases/lifecycle.rs`, `src/cli/mod.rs`, `src/mcp/tools.rs`, `src/mcp/tools/inputs.rs`):
    - Drop strict `value_parser` on `Command::Snapshot.action: Option<String>` (validate manually so `acdd snapshot --delete <name>` shifts positional `<name>` cleanly into `target`), add `--delete`, `--older-than <DURATION>`, `--worktree <PATH>`, and wire `StateCommand::Snapshot(req) => req.worktree.as_deref()` in `CliStateCtx::command`.
    - Resolve `p.worktree.as_ref().and_then(WorktreeInput::as_selector)` (not `ctx.target_root`) in `src/mcp/tools.rs` + add `snapshot_name`/`name`, `older_than`, `delete` parity on `StateInput` (`src/mcp/tools/inputs.rs`).
    - When `--run` is specified -> strict run/task mode; when `--run` is omitted -> route to task mode only if `target` matches a task in an unambiguously resolved active run, otherwise route cleanly to `_adhoc` standalone mode WITHOUT requiring `.acdd/state.sqlite`. Standalone `--delete --older-than` / `--all` scopes strictly to `refs/acdd/snapshots/_adhoc/` unless `--run` is explicitly passed.
  - Narrowly amend ADR 0005 (`docs/adr/0005-content-addressed-git-snapshots-and-integration.md`), update `docs/reference/{cli,mcp-tools,tasks}.md` + `skills/acdd/SKILL.md`, and extend table-driven tests in `crates/acdd-state/tests/state_integration/usecases_queries_lifecycle.rs` and `tests/acdd/lifecycle_and_state.rs`.
scope:
- crates/acdd-state/src/git_ops/snapshot.rs
- crates/acdd-state/src/git_ops/worktree.rs
- crates/acdd-state/src/usecases/dto.rs
- crates/acdd-state/src/usecases/mod.rs
- crates/acdd-state/src/usecases/commands/task_ops.rs
- crates/acdd-state/src/usecases/lifecycle.rs
- crates/acdd-state/tests/state_integration/usecases_queries_lifecycle.rs
- src/cli/mod.rs
- src/mcp/tools.rs
- src/mcp/tools/inputs.rs
- src/state_bridge/mod.rs
- tests/acdd/lifecycle_and_state.rs
- docs/adr/0005-content-addressed-git-snapshots-and-integration.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- docs/reference/tasks.md
- skills/acdd/SKILL.md
extended_scope:
- path: crates/acdd-state/src/usecases/submit/post_save.rs
  reason: Admit single-line sibling call-site update for cleanup_locked_refs signature extension in worktree.rs
- path: tests/mcp/context/checkpoints_and_serialization.rs
  reason: 'Add live MCP acdd_state standalone/delete parity coverage per R3 finding #5'
- path: crates/acdd-state/tests/state_integration/git_ops.rs
  reason: Extend the existing cleanup matrix to prove OID-guarded ref deletion required by integration-review P2.
tags:
- default
- docs
invariants:
- 'INV-CONFINED-SNAPSHOTS: Standalone `_adhoc` and per-task snapshots never touch `HEAD`, `.git/index`, `.acdd/state.sqlite` (in standalone mode), or files outside target worktree policy.'
- 'INV-SINGLE-OWNER: CLI (`--delete`, `--older-than`, `--all`, `--worktree`) and MCP share `StateCommand::Snapshot(SnapshotReq)` with zero shadow git implementations.'
- 'INV-BOUNDED-LOC-SNAPSHOTS: Net production delta across `acdd-state` + `src/` for T-03 stays <= 90 LOC.'
acceptance_criteria:
- '`acdd snapshot save test_snapshop --rev 1`, `restore`, `list`, and `--worktree` succeed in a Git repo without any active run or existing `.acdd/state.sqlite`.'
- '`acdd snapshot --delete --older-than <DUR>`, `acdd snapshot --delete --all`, and `clear --rev N` prune matching refs without touching active task refs unless `--run` is passed.'
- All workspace clippy checks, snapshot CLI/state tests, `acdd docs check`, and `acdd validate` pass cleanly.
contract_revision: 1
status: completed
receipt:
  run_id: git-snapshots-and-ro-20261119192005-316933-1
  node_cursor: root:T-03.deliver
  gate_id: deliver
  worker_id: coordinator-snap-t3
  claim_revision: 9
  contract_revision: 4
  candidate_tree: fc07d28c80afa386eba1685ee77ff3e435e3449d
  committed_oid: eca2460e280f91aab0c0d59d0daf107a80312208
  accepted_at: 2026-11-20T05:18:54.424Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-CONFINED-SNAPSHOTS: Standalone `_adhoc` and per-task snapshots never touch `HEAD`, `.git/index`, `.acdd/state.sqlite` (in standalone mode), or files outside target worktree policy.'
      - 'INV-SINGLE-OWNER: CLI (`--delete`, `--older-than`, `--all`, `--worktree`) and MCP share `StateCommand::Snapshot(SnapshotReq)` with zero shadow git implementations.'
      - 'INV-BOUNDED-LOC-SNAPSHOTS: Net production delta across `acdd-state` + `src/` for T-03 stays <= 90 LOC.'
      verified: []
```
