---
id: engine-quality-context-and-profile-rules
title: Engine Context Fidelity, Clone Detection, and Review Rules
doc_type: plan
status: completed
owners:
- crates/acdd-state/**
- crates/acdd-index/**
- src/**
- skills/acdd/**
- docs/reference/**
- AGENTS.md
invariants:
- 'INV-SINGLE-OWNER: Reuse `ClaimContextRequest`, `docs_domain`, `duplicates_paged`, and embedded YAML profile slots without parallel pipelines.'
- 'INV-READONLY-CONTEXT: `acdd context` / `acdd_state(action: context)` reads strictly over `SQLITE_OPEN_READ_ONLY` without mutating claim state.'
acceptance_criteria:
- 'AC-1: `latest_pre_task_proof_value` preserves blueprint `tasks` slices so `pre_task_slice` and `max_net_loc` reach builders/reviewers.'
- 'AC-2: `docs.instructions` delivers bounded active `AGENTS.md` content and `build_contract_value` includes live SQLite `subtasks` status.'
- 'AC-3: `duplicates` defaults to `min_lines=5` and detects intra-function repeated blocks without adding `>150 LOC` AST rejections.'
- 'AC-4: Read-only `acdd context` + `PostCompact` hook guidance + native `embedded/{base,code}.yaml` rules (`NO EARLY-ABORT`, `DEFECT-CLASS SWEEP`, CLI fallback, anti-scope-creep, pragmatic modularity) are active.'
- 'AC-5: `acdd start` (and `sync`) reads `<spec_path>`, `.acdd/profile.yaml`, and profile refs from the caller''s owned worktree checkout (`--root` / `config.worktree` / existing confined `--worktree-path`) rather than failing when `primary_worktree_root` is checked out on another branch.'
closure:
  status: completed
  closed_at: 2026-11-19T15:21:52.938Z
  target_base: main
  target_base_commit: 4461137853b07fe7d7376dede4becaff5cdce244
  task_receipts:
    T-01:
      run_id: engine-quality-conte-20261119081324-1589999-2
      node_cursor: root:T-01.deliver
      gate_id: deliver
      worker_id: codex-eq-t1-rev-r3
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 6a14334130130b84158f8359f3c90687d307a1a8
      accepted_at: 2026-11-19T09:08:39.527Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-SINGLE-OWNER: Keep pre-task proof extraction, contract building, and docs context in their existing canonical owners.'
          verified: []
        review_summary: Delivered T-01 on main (in_place) after R3 independent review passed all 6 contours.
    T-02:
      run_id: engine-quality-conte-20261119081324-1589999-2
      node_cursor: root:T-02.deliver
      gate_id: deliver
      worker_id: cursor-eq-t2-rev-r2
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 0b78a30e2860b5e881b695c6ace066fabf61cf17
      accepted_at: 2026-11-19T12:37:43.233Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-ZERO-FALSE-MONOLITH-AST: Do not add AST function-length rejection thresholds.'
          verified: []
        review_summary: Deliver T-02 after R2 review pass (cursor-eq-t2-rev-r2)
    T-03:
      run_id: engine-quality-conte-20261119081324-1589999-2
      node_cursor: root:T-03.deliver
      gate_id: deliver
      worker_id: coordinator
      claim_revision: 1
      contract_revision: 3
      candidate_tree: 67707e1997525c1b9eb76039a911c9682ec88088
      accepted_at: 2026-11-19T14:22:52.596Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-READONLY-CONTEXT: `acdd context` never opens read-write SQLite or mutates claim revisions.'
          verified: []
    T-04:
      run_id: engine-quality-conte-20261119081324-1589999-2
      node_cursor: root:T-04.deliver
      gate_id: deliver
      worker_id: cursor-eq-t4-rev-r5
      claim_revision: 1
      contract_revision: 5
      candidate_tree: 37464dd4d3453f3da97fb17fc354adbcc4e46e30
      accepted_at: 2026-11-19T11:43:23.009Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-SINGLE-OWNER: Keep shared `state.sqlite` DB and lock resolution on `primary_worktree_root` while separating the owned source-checkout root for spec/profile lookup via canonical worktree ownership.'
          verified: []
        review_summary: Deliver T-04 after R5 review pass (cursor-eq-t4-rev-r5)
  rollup:
    verified_invariants:
      applicable:
      - 'INV-SINGLE-OWNER: Keep pre-task proof extraction, contract building, and docs context in their existing canonical owners.'
      - 'INV-ZERO-FALSE-MONOLITH-AST: Do not add AST function-length rejection thresholds.'
      - 'INV-READONLY-CONTEXT: `acdd context` never opens read-write SQLite or mutates claim revisions.'
      - 'INV-SINGLE-OWNER: Keep shared `state.sqlite` DB and lock resolution on `primary_worktree_root` while separating the owned source-checkout root for spec/profile lookup via canonical worktree ownership.'
      verified: []
    decisions:
    - 'ARCH DECISION (engine-quality-v062, in_place on main): Fix 3 context gaps in T-01 (`latest_pre_task_proof_value` requiring `tasks` object, bounded `AGENTS.md` content in `docs_domain::provide_instructions`, live `snapshot.subtasks` in `build_contract_value`), lower `duplicates_paged` `min_lines` to 5 with intra-function block clone detection in `crates/acdd-index/src/db/traversal.rs` in T-02 (no `>150 LOC` AST limit), and add read-only `StateQuery::Context` (`acdd context`) + persist finalized review/build/CLI rules into native `embedded/{base,code}.yaml` + `PostCompact` hook recipe & test-matrix clarification in T-03.'
    - 'UPDATED ARCH DECISION #30 (Blackboard #31 fixes): Added src/mcp/tools.rs to T-03 scope (exhaustive MCP dispatch for StateQuery::Context over SQLITE_OPEN_READ_ONLY); removed protected .acdd/profile.yaml from T-03 (cleanup of temporary .acdd/profile.yaml overrides deferred to polish stage which explicitly admits .acdd/profile.yaml).'
    - 'UPDATED ARCH DECISION #33 (Added T-04): Fix `acdd start` and `acdd sync` in linked worktrees (`start_holding_repo_lock` in `commands/start.rs`, `handle_sync_dyn` in `commands/run_ops.rs`, shared helper in `lifecycle.rs`): keep shared SQLite DB, repo lock, and managed worktree creation anchored on `primary` (`primary_worktree_root`), while resolving non-ephemeral `<spec_path>`, `.acdd/profile.yaml`, `profile_ref`, and branch/commit defaults from the verified owned source checkout (`req.worktree_path` when it is an existing owned worktree or `config.worktree` when distinct from `primary`), failing closed via `list_owned_worktrees` + `checked_child` confinement. Keeps T-01..T-03 intact.'
    - 'UPDATED ARCH DECISION #50 (Blackboard #52 R3 fixes): Kept Revision-2 `T-01..T-03` payloads verbatim byte-for-byte without modification. Bound `T-04` to existing canonical owner `crates/acdd-state/src/git_ops/worktree.rs::resolve_repo_layout` + `acdd_core::fs::checked_child` (verifying `candidate_layout.git_common_dir == primary_layout.git_common_dir` and `canonicalize_macos_var(candidate) == candidate_layout.target_worktree`, failing closed on foreign/unowned directories, subdirs of unrelated worktrees, or symlink escapes) cleanly on `main`.'
    - 'Blackboard #60 remediation needs the canonical owners: git_ops/worktree.rs to pass verified copy source into existing managed creation/rollback, validate.rs + usecases/deps.rs to separate checkout scanning from shared store, and usecases/mod.rs to carry explicit source config through normal Start/Sync dispatch. Extending only these owner files. No new subsystem or alternate Git/validation pipeline. Four remediations may exceed the existing +90 net blueprint cap; will record the measured per-file delta and request BUDGET_OVERRIDE if submission enforces it.'
    - |-
      BUDGET_OVERRIDE approval requested for T-04.build at 178 net production lines. Submission rejected verified claim revision 3 with GATE_VERIFICATION_FAILED (+178 vs max_net_loc 90). Blackboard #60 requires the four linked-checkout remediations. The patch reuses the existing confined path checks, managed copy and rollback, primary store bridge, and spec/dependency validators. No new subsystem. Explicit config is carried through existing dispatch; same-repository validation uses the verified source with the primary reader and lock.

      Production path delta from baseline commit 4461137853b07fe7d7376dede4becaff5cdce244:
      crates/acdd-state/src/git_ops/worktree.rs: 8 added, 4 removed, +4 net
      crates/acdd-state/src/usecases/commands/run_ops.rs: 19 added, 7 removed, +12 net
      crates/acdd-state/src/usecases/commands/start.rs: 78 added, 55 removed, +23 net
      crates/acdd-state/src/usecases/deps.rs: 10 added, 5 removed, +5 net
      crates/acdd-state/src/usecases/lifecycle.rs: 74 added, 2 removed, +72 net
      crates/acdd-state/src/usecases/mod.rs: 22 added, 5 removed, +17 net
      crates/acdd-state/src/validate.rs: 5 added, 6 removed, -1 net
      src/cli/mod.rs: 36 added, 13 removed, +23 net
      src/state_bridge/mod.rs: 33 added, 10 removed, +23 net
      Total: 285 added, 107 removed, +178 net. Initial accepted build was +88; these four remediations add +90 net.

      Requested decision: approve a maximum of 178 net lines for T-04.build. Revision 3 verification and code.seam passed. All requested suites passed (29, 24, 9, 18), strict Clippy passed, one CLI lib unit passed, and the scoped graph comparison found zero new duplicates, dead tails, phantom-tested symbols, or dangling calls.
    - 'BUDGET_OVERRIDE: Approved T-04.build budget increase from 90 to 178 net production lines per Blackboard #66 exact breakdown (+88 initial accepted build + 90 net remediation for all 4 Blackboard #60 defects: lexical normalized no-symlink check defeating trailing-dot bypass in lifecycle.rs, managed worktree copy of uncommitted linked spec/profile in start.rs + git_ops/worktree.rs, implicit primary state_db parity vs explicit --config profiles_dirs/collections in state_bridge/mod.rs + usecases/mod.rs, and verified-source dependency validation keeping primary reader/lock in run_ops.rs + deps.rs + validate.rs). Verified 80/80 tests, Clippy, acdd_verify, and zero new graph hygiene findings.'
    - |-
      BUDGET_OVERRIDE approval requested for T-04.build at +231 net production LOC, exceeding approved #69 (+178). Round 3 addresses #74 via destination pre-publication validation and rollback, common Git repository dependency routing with primary state DB, and trace DB parity. Baseline commit 4461137853b07fe7d7376dede4becaff5cdce244. Per-file breakdown:
      crates/acdd-state/src/git_ops/worktree.rs: 8 added, 4 removed, +4 net
      crates/acdd-state/src/usecases/commands/mod.rs: 4 added, 2 removed, +2 net
      crates/acdd-state/src/usecases/commands/run_ops.rs: 22 added, 9 removed, +13 net
      crates/acdd-state/src/usecases/commands/start.rs: 123 added, 83 removed, +40 net
      crates/acdd-state/src/usecases/deps.rs: 51 added, 14 removed, +37 net
      crates/acdd-state/src/usecases/lifecycle.rs: 74 added, 2 removed, +72 net
      crates/acdd-state/src/usecases/mod.rs: 22 added, 5 removed, +17 net
      crates/acdd-state/src/validate.rs: 5 added, 6 removed, -1 net
      src/cli/mod.rs: 39 added, 16 removed, +23 net
      src/state_bridge/mod.rs: 35 added, 11 removed, +24 net
      Total 383 additions, 152 deletions, +231 net. Request approval for max_net_loc 231; remaining verification in progress.
    - 'Correction to budget request #77: final commands/mod.rs is 10 added, 8 removed (+2 net), after retaining the existing dependency parser for local references. All other per-file counts in #77 are unchanged. Final total is 389 added, 158 removed, +231 net; requested BUDGET_OVERRIDE remains +231.'
    - 'BUDGET_OVERRIDE: Approved T-04.build budget update from 178 to 231 net production LOC per Blackboard #77 exact breakdown (+53 net across deps.rs +32, start.rs +17, commands/mod.rs +2, run_ops.rs +1, state_bridge/mod.rs +1) to close all 4 Blackboard #74 defects (uncommitted inherited profile lineage copy/rollback, managed --worktree --target-base destination parity & rollback, common Git repository alias DB routing via primary DB without partial publish, and resolve_effective_state_db trace/retention parity) via existing canonical owners without new subsystems or clones.'
    - 'BUDGET_OVERRIDE approval requested for T-04.build: final production delta currently +262 net, exceeding approved +231 (#80), from baseline 4461137853b07fe7d7376dede4becaff5cdce244. #82 requires canonical profile lineage collection, destination parity, checkout-relative alias routing, source archive observation, and fail-closed primary DB selection. Per-file added/removed/net: git_ops/worktree.rs 8/4/+4; profile/inherit.rs 22/0/+22; usecases/commands/mod.rs 10/8/+2; commands/run_ops.rs 24/14/+10; commands/start.rs 133/90/+43; usecases/deps.rs 73/19/+54; usecases/lifecycle.rs 74/2/+72; usecases/mod.rs 22/5/+17; validate.rs 8/9/-1; src/cli/mod.rs 39/16/+23; src/mcp/tools.rs 2/2/0; src/state_bridge/mod.rs 30/14/+16. Total 445 additions, 183 deletions, +262 net. Request max_net_loc 262.'
    - 'Correction to budget request #86: destination preflight now reads the actual managed spec before checking its bindings. Final start.rs delta is 134 added / 90 removed (+44); other per-file counts in #86 are unchanged. Final total 446 additions, 183 deletions, +263 net; requesting BUDGET_OVERRIDE for +263.'
    - 'BUDGET_OVERRIDE: Approved T-04.build budget update from 231 to 263 net production LOC per Blackboard #86 & #88 exact breakdown (unifying canonical profile lineage collector in profile/inherit.rs +22, canonical resolve_spec_repositories & source archive parity in deps.rs +17 removing ad-hoc maps in run_ops.rs -3, shrinking state_bridge/mod.rs -8 via fail-closed try_resolve_effective_state_db, and +4 start.rs destination preflight). Verified zero duplicates/dangling calls and Clippy pass.'
    - 'BUDGET_OVERRIDE approval requested for T-04.build at +276 net production LOC, exceeding approved +263 (#89), from baseline 4461137853b07fe7d7376dede4becaff5cdce244. Round 4 closes #94: destination satisfaction parity before publish (+12 start.rs), checkout linked_workspaces overlay resolved against the source (+8 lifecycle.rs), and sync --rebind-profile preserves snapshot.run.isolation plus current worktrees (-7 run_ops.rs). Per-file added/removed/net: git_ops/worktree.rs 8/4/+4; profile/inherit.rs 22/0/+22; usecases/commands/mod.rs 10/8/+2; commands/run_ops.rs 27/24/+3; commands/start.rs 146/90/+56; usecases/deps.rs 73/19/+54; usecases/lifecycle.rs 82/2/+80; usecases/mod.rs 22/5/+17; validate.rs 8/9/-1; src/cli/mod.rs 39/16/+23; src/state_bridge/mod.rs 30/14/+16. Total 467/191/+276. No new subsystem. Zero new #[test] functions. Duplicates min_lines=5 on start.rs, run_ops.rs, and lifecycle.rs are empty; dangling_calls are 0. Remaining dead_tails are pre-existing handle_draft_dyn, handle_clear_dyn, and is_managed_run_branch.'
    - 'BUDGET_OVERRIDE: Approved T-04.build budget update from 263 to 276 net production LOC per Blackboard #97 exact breakdown (+12 start.rs destination satisfaction parity check & rollback, +8 lifecycle.rs checkout_config linked_workspaces parity, -7 run_ops.rs preserving snapshot.run.isolation across sync --rebind-profile). Verified 0 duplicates (min_lines=5), 0 dangling calls, and all 239 tests + Clippy + acdd_verify pass.'
    - 'BUDGET_OVERRIDE: T-02 intra-function block clone detection adds ~110 net LOC in duplicates_paged (window hashing, overlap suppression, block group emission) plus normalize trim offset; unavoidable for spec AC-3 without weakening overlap discipline.'
    - 'BUDGET_OVERRIDE: Intra-function block clone detection in duplicates_paged requires +165 net LOC (window hashing, non-overlap grouping, block groups); blueprint max_net_loc=120 insufficient for AC-3.'
    - 'BUDGET_OVERRIDE: Approved T-02.build budget update from 120 to 134 net production LOC (140 added / 6 removed in crates/acdd-index/src/db/traversal.rs production lines, +21 inline #[cfg(test)] seam test lines = +155 total in traversal.rs) for Blackboard #100/#102 (`duplicates_paged` min_lines=5 default, `normalize_symbol_body_lines` trim offset, non-overlapping block-window hashing & grouping). Verified 0 duplicates, 49/49 query_context, strict Clippy, and acdd_verify pass.'
    - 'BUDGET_OVERRIDE: Updated approved T-02.build budget from 134 to 144 net production LOC (+144 prod / +171 added - 6 removed = +165 total in crates/acdd-index/src/db/traversal.rs including +21 #[cfg(test)] seam test lines) for Blackboard #104 R2 fixes (unblocking lines.len()==min_lines block windows and whole_symbol_duplicate_members suppression). Verified 0 duplicates, 70 lib + 49 query_context + 18 commitment_integrity + strict Clippy pass.'
    - 'BUDGET_OVERRIDE: T-03.build net production LOC +483 vs max_net_loc=220. Breakdown: StateQuery::Context engine (~120), CLI/MCP/format wiring (~45), embedded profile+docs (~80), lifecycle auto_select_context_cursor + minimal checkout_config/resolve_source_checkout restore (~180, required because accidental lifecycle revert would break uncommitted T-04 start/sync compile), integration tests (~58). No new subsystems; reuses GateContextAssembler. Request budget raise to 483 for T-03.build.'
    - 'BUDGET_OVERRIDE: Approved T-03.build net production LOC budget raise from 220 to 500 LOC (+483 actual net LOC delta) for StateQuery::Context engine (~120), CLI/MCP/format (~45), embedded profiles+docs (~80), lifecycle auto_select_context_cursor + minimal checkout_config/resolve_source_checkout restore (~180), and integration tests (~58). Reuses GateContextAssembler; zero new subsystems.'
    - |-
      DECISION: Polish remediation closing review finding #111:
      1. Profile inheritance: Deleted temporary guidance and architecture contours overrides from .acdd/profile.yaml so that embedded {base,code}.yaml rules are the single source of truth; restored scope_args_by_pattern.
      2. Test harness guidance: In embedded code.yaml (code.principles, task.code, task.review), generalized rule to 'Reuse existing in-process test harnesses when available; reserve disk tempdirs and child-process executions strictly for genuine environment or isolation contracts' without hardcoding specific repo type RunHarness::in_memory().
      3. Plans prefixes: Confirmed specs.plans.prefixes: [] in acdd.yaml to support slug-based plan IDs (engine-quality-context-and-profile-rules) using the native validate empty-prefix bypass.
      4. Candidate cleanliness: Added .playwright-mcp/ to .gitignore and moved untracked plan scratch to .acdd/tmp/.
      5. AGENTS.md scope: Removed **/AGENTS.md from polish.scope in code.yaml to prevent workspace-wide AGENTS.md injection during polish.
  metrics_summary:
    decisions_count: 22
    deferred_findings_count: 0
    failed_traces: 10
    passed_traces: 20
    task_receipts_count: 4
    tasks_cancelled: 0
    tasks_completed: 4
    tasks_total: 4
    total_claims: 47
    total_review_cycles: 6
    total_traces: 31
---

# Plan: Engine Context Fidelity, Clone Detection, and Review Rules

### task: T-01

```yaml
task_ref: T-01
target: Fix pre_task_slice shadowing, inject bounded AGENTS.md instructions, and project live SQLite subtasks in ClaimResp.
spec: |
  Fix three context-injection gaps in `acdd-state` and `src/state_bridge`:
  1. `crates/acdd-state/src/usecases/queries.rs` (`latest_pre_task_proof_value`): require `val.get("tasks").is_some_and(|v| v.is_object())` so `blueprint_synthesis.review` never shadows `blueprint_synthesis.synthesize`'s `tasks` payload (`pre_task_slice` + `max_net_loc`).
  2. `src/state_bridge/docs_domain.rs` (`provide_instructions`): return bounded, digestible `AGENTS.md` instruction content (owning subtree `AGENTS.md` plus essential root `AGENTS.md` sections within profile limits) instead of bare paths/headings.
  3. `crates/acdd-state/src/context.rs` (`build_contract_value`): read live task subtasks from `snapshot.subtasks` in SQLite (with fallback to spec YAML if absent) so claimed workers see accurate current subtask completion state.
  - Extend existing unit/integration tests in `crates/acdd-state/` and `tests/mcp/` verifying all three behaviors.
scope:
- crates/acdd-state/src/usecases/queries.rs
- crates/acdd-state/src/context.rs
- src/state_bridge/docs_domain.rs
- crates/acdd-state/tests/state_integration/
- tests/mcp/
tags:
- default
invariants:
- 'INV-SINGLE-OWNER: Keep pre-task proof extraction, contract building, and docs context in their existing canonical owners.'
acceptance_criteria:
- A completed `blueprint_synthesis.review` no longer prevents `ClaimResp.pre_task_slice` or `enforce_blueprint_max_net_loc` from reading `tasks`.
- '`docs.instructions` includes bounded live `AGENTS.md` rules and `contract.subtasks` reflects live SQLite subtask updates.'
contract_revision: 1
status: completed
receipt:
  run_id: engine-quality-conte-20261119081324-1589999-2
  node_cursor: root:T-01.deliver
  gate_id: deliver
  worker_id: codex-eq-t1-rev-r3
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 6a14334130130b84158f8359f3c90687d307a1a8
  accepted_at: 2026-11-19T09:08:39.527Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-SINGLE-OWNER: Keep pre-task proof extraction, contract building, and docs context in their existing canonical owners.'
      verified: []
    review_summary: Delivered T-01 on main (in_place) after R3 independent review passed all 6 contours.
```

### task: T-02

```yaml
task_ref: T-02
target: Lower duplicates min_lines default to 5 and detect intra-function repeated blocks in acdd-index.
spec: |
  Upgrade duplicate detection in `crates/acdd-index/src/db/traversal.rs`:
  - Lower `duplicates_paged` default `min_lines` from `10` to `5`.
  - Detect repeated normalized blocks (`>= min_lines`) occurring multiple times within the SAME function/method body as well as across different functions/files (without adding any `>= 150 LOC` function-length AST check).
  - Add table-driven tests in `tests/query_context/` verifying 5-line cross-function clones, intra-function repeated blocks, and clean non-clones (<5 lines).
scope:
- crates/acdd-index/src/db/traversal.rs
- tests/query_context/
- docs/reference/acdd.md
tags:
- default
invariants:
- 'INV-ZERO-FALSE-MONOLITH-AST: Do not add AST function-length rejection thresholds.'
acceptance_criteria:
- '`code_map_query(operation: duplicates)` defaults `min_lines` to 5 and reports both cross-function and intra-function clones >= 5 lines.'
- Table-driven test matrix in `tests/query_context/` passes cleanly.
contract_revision: 1
status: completed
receipt:
  run_id: engine-quality-conte-20261119081324-1589999-2
  node_cursor: root:T-02.deliver
  gate_id: deliver
  worker_id: cursor-eq-t2-rev-r2
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 0b78a30e2860b5e881b695c6ace066fabf61cf17
  accepted_at: 2026-11-19T12:37:43.233Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-ZERO-FALSE-MONOLITH-AST: Do not add AST function-length rejection thresholds.'
      verified: []
    review_summary: Deliver T-02 after R2 review pass (cursor-eq-t2-rev-r2)
```

### task: T-03

```yaml
task_ref: T-03
target: Add read-only acdd context CLI/MCP, persist tightened rules in embedded base/code YAML, and document PostCompact hook + test-matrix clarification.
spec: |
  Wire read-only gate context refresh and persist finalized workflow/review governance:
  - Add read-only `ContextReq` + `StateQuery::Context` (`queries.rs` + `dto.rs`) returning budgeted `ClaimResp` for the specified (or currently active) node over `SQLITE_OPEN_READ_ONLY` with zero claim mutation or revision bump.
  - Expose `acdd context [<node>] [--worker <id>] [--run <name>]` in `src/cli/mod.rs` and `acdd_state(action: "context")` in `src/mcp/tools/inputs.rs` + `src/state_bridge/mod.rs`.
  - Update `crates/acdd-state/src/profile/embedded/base.yaml` (`core.execution`: `acdd` CLI fallback when MCP unavailable; `core.review`: `NO EARLY-ABORT REVIEW` & `DEFECT-CLASS SWEEP`) and `crates/acdd-state/src/profile/embedded/code.yaml` (`code.principles`: anti-architecture-invention + cohesive pipeline modularity + reviewer verification of `BUDGET_OVERRIDE` reasons; `task.code` & `task.review`: pragmatic `>150 LOC` evaluation without mechanical line-count rejection, `DEFECT-CLASS SWEEP`, isolated regression tests without fake mega-tests). (Temporary overrides in `.acdd/profile.yaml` are removed afterward in `polish`.)
  - Document `acdd context` + `PostCompact`/`SessionStart(compact)` hook recipe in `skills/acdd/SKILL.md` and `docs/reference/{cli,mcp-tools}.md`; clarify `AGENTS.md` `net new #[test] == 0` rule so additions use true `for case in cases` tables or shared local helpers rather than imperative mega-test chains.
scope:
- crates/acdd-state/src/usecases/dto.rs
- crates/acdd-state/src/usecases/queries.rs
- crates/acdd-state/src/context.rs
- crates/acdd-state/src/profile/embedded/base.yaml
- crates/acdd-state/src/profile/embedded/code.yaml
- crates/acdd-state/tests/state_integration/
- src/cli/mod.rs
- src/cli/guide.rs
- src/mcp/tools.rs
- src/mcp/tools/inputs.rs
- src/state_bridge/mod.rs
- tests/acdd/
- tests/mcp/
- skills/acdd/SKILL.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- AGENTS.md
extended_scope:
- path: crates/acdd-state/src/usecases/lifecycle.rs
  reason: Lifecycle checkout helpers required for workspace compile; format renders Context query markdown parity with Claim
- path: src/format/lifecycle.rs
  reason: Lifecycle checkout helpers required for workspace compile; format renders Context query markdown parity with Claim
- path: src/cli/docs_check.rs
  reason: Ignore .acdd directory in docs check
depends_on:
- T-01
tags:
- default
- docs
invariants:
- 'INV-READONLY-CONTEXT: `acdd context` never opens read-write SQLite or mutates claim revisions.'
acceptance_criteria:
- '`acdd context` and `acdd_state(action: context)` return current gate contract, guidance, findings, and bounded `AGENTS.md` over `SQLITE_OPEN_READ_ONLY`.'
- Native `embedded/{base,code}.yaml` carry all updated review/build/CLI rules and pass profile contract tests (`acdd validate`, `acdd docs check`).
contract_revision: 1
status: completed
receipt:
  run_id: engine-quality-conte-20261119081324-1589999-2
  node_cursor: root:T-03.deliver
  gate_id: deliver
  worker_id: coordinator
  claim_revision: 1
  contract_revision: 3
  candidate_tree: 67707e1997525c1b9eb76039a911c9682ec88088
  accepted_at: 2026-11-19T14:22:52.596Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-READONLY-CONTEXT: `acdd context` never opens read-write SQLite or mutates claim revisions.'
      verified: []
```

### task: T-04

```yaml
task_ref: T-04
target: Fix acdd start / sync in linked worktrees when spec or .acdd/profile.yaml lives in --root / config.worktree / --worktree-path instead of primary_worktree_root.
spec: |
  Fix linked-worktree checkout resolution during run start & sync while keeping shared `.acdd/state.sqlite` DB & repo lock anchored on `primary`:
  - `crates/acdd-state/src/usecases/commands/start.rs` (`start_holding_repo_lock`, `plan_start_worktree`): keep SQLite DB / lock / managed-worktree creation anchored at `primary`, while reading non-ephemeral `<spec_path>`, `.acdd/profile.yaml`, `profile_ref`, and `current_branch_name` / `current_head_commit` defaults from the resolved owned checkout root (`req.worktree_path` if an existing owned worktree or `config.worktree` when distinct from `primary`, failing closed via Git worktree ownership & `checked_child` confinement — never falling back silently to wrong checkout or escaping the repository).
  - `crates/acdd-state/src/usecases/commands/run_ops.rs` (`handle_sync_dyn`): check the run's active worktree checkout (for non-ephemeral runs with `snapshot.run.worktree`) before `primary` when syncing `<spec_path>` and `.acdd/profile.yaml`.
  - Inspect `src/state_bridge/mod.rs` (`state_engine_config`) and `src/cli/mod.rs` (`Commands::Start`) to confirm `--root`, cwd in `.worktrees/<name>`, and `--worktree-path <wt>` resolve `AcddConfig` (`acdd.yaml`) + `config.worktree` consistently without breaking shared `state.sqlite` DB routing.
  - Add table-driven matrix tests in `crates/acdd-state/tests/state_integration/` and `tests/acdd/` proving `acdd start` + `sync` succeed when `primary` is on an unrelated branch lacking `<spec_path>` and `.acdd/profile.yaml` (tested both via `--root <worktree>`/cwd and via `--worktree-path <existing_worktree>`), while keeping foreign-checkout confinement rejection.
scope:
- crates/acdd-state/src/usecases/commands/start.rs
- crates/acdd-state/src/usecases/commands/run_ops.rs
- crates/acdd-state/src/usecases/lifecycle.rs
- src/state_bridge/mod.rs
- src/cli/mod.rs
- crates/acdd-state/tests/state_integration/
- tests/acdd/
extended_scope:
- path: crates/acdd-state/src/git_ops/worktree.rs
  reason: 'Blackboard #60: pass verified source through canonical managed worktree copy and dependency validators, and carry explicit source configuration through the existing engine dispatch'
- path: crates/acdd-state/src/usecases/deps.rs
  reason: 'Blackboard #60: pass verified source through canonical managed worktree copy and dependency validators, and carry explicit source configuration through the existing engine dispatch'
- path: crates/acdd-state/src/validate.rs
  reason: 'Blackboard #60: pass verified source through canonical managed worktree copy and dependency validators, and carry explicit source configuration through the existing engine dispatch'
- path: crates/acdd-state/src/usecases/mod.rs
  reason: 'Blackboard #60: pass verified source through canonical managed worktree copy and dependency validators, and carry explicit source configuration through the existing engine dispatch'
- path: crates/acdd-state/src/usecases/commands/mod.rs
  reason: Blackboard 74 requires passing the primary database to the shared dependency materializer used by start and sync
- path: crates/acdd-state/src/profile/inherit.rs
  reason: Reuse canonical profile lineage discovery and guard MCP trace retention for malformed owner config.
- path: src/mcp/tools.rs
  reason: Reuse canonical profile lineage discovery and guard MCP trace retention for malformed owner config.
- path: tests/mcp/context/sibling_worktrees.rs
  reason: Cover malformed primary configuration through the existing linked-worktree MCP matrix.
tags:
- default
invariants:
- 'INV-SINGLE-OWNER: Keep shared `state.sqlite` DB and lock resolution on `primary_worktree_root` while separating the owned source-checkout root for spec/profile lookup via canonical worktree ownership.'
acceptance_criteria:
- '`acdd start <spec> --name ... --worktree-path <wt>` succeeds from inside `<wt>`, with `--root <wt>`, or from `primary` when `<spec>` and `.acdd/profile.yaml` exist only in `<wt>`.'
- Foreign unowned paths passed as `--worktree-path` or escaping symlinks remain rejected fail-closed.
contract_revision: 1
status: completed
receipt:
  run_id: engine-quality-conte-20261119081324-1589999-2
  node_cursor: root:T-04.deliver
  gate_id: deliver
  worker_id: cursor-eq-t4-rev-r5
  claim_revision: 1
  contract_revision: 5
  candidate_tree: 37464dd4d3453f3da97fb17fc354adbcc4e46e30
  accepted_at: 2026-11-19T11:43:23.009Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-SINGLE-OWNER: Keep shared `state.sqlite` DB and lock resolution on `primary_worktree_root` while separating the owned source-checkout root for spec/profile lookup via canonical worktree ownership.'
      verified: []
    review_summary: Deliver T-04 after R5 review pass (cursor-eq-t4-rev-r5)
```
