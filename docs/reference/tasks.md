---
doc_type: api
title: State, tasks & lifecycle
---

# State, tasks & lifecycle

A single declarative `Run` + `GateProfile` state engine in
[`crates/acdd-state`](../../crates/acdd-state/src/lib.rs) and leaf
contracts in [`crates/acdd-core`](../../crates/acdd-core/src/lib.rs)
([ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md)).
Domain-specific code/docs hooks (`call:`, `checks:`, `context:`, and `<domain>_<name>`
plugin tools) connect through [`DomainProvider`](../../crates/acdd-core/src/ports.rs)
implementations in [`src/state_bridge/`](../../src/state_bridge/mod.rs)
([ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md)).

---

## Core Concepts: `Run`, `GateProfile`, and `GateNode`

Every lifecycle execution instantiates one **`Run`** bound to a resolved
**`v2` `GateProfile`**:

- **`Run`**: Created by `acdd start` (from a workspace-confined spec file or
  ephemeral `--goal`), storing immutable `profile_json`, `spec_digest`,
  `isolation` (`worktree` or `in_place`), optional `target_base` / `branch` /
  `worktree`, `trace_id`, and monotonic optimistic `generation` (`>= 1`).
- **`GateNode` (`gate_nodes`)**: Materializes the profile's `gates:` hierarchy
  and `- tasks:` spec blocks into one tree of typed nodes addressed by dotted
  `NodeCursor` identifiers:
  - `container`: Structural group of root gates (e.g. `blueprint_synthesis`).
  - `root_gate`: Pre-task or post-task milestone/run gate (e.g. `run_tests`, `polish`, `integration_review`, `handoff`).
  - `task`: Task container (`gate_id = "__task__"`, `task_ref = <ref>`) matching the active track's `- tasks:` `when:` expression.
  - `task_gate`: Leaf gate inside a task (e.g. `<task_ref>.build`, `<task_ref>.review`, `<task_ref>.deliver`).
- **Node statuses**: `pending`, `ready`, `in_progress`, `completed`, `stale`,
  `escalated`, and `cancelled`.

### Specification format (`### task:`)

Specs under configured `specs:` collections (`docs/milestones/*.md`,
`docs/plans/*.md`) use YAML frontmatter + fenced `### task: <task_ref>` blocks:

```yaml
task_ref: example-task
target: Clear bounded objective
subtasks:
  - subtask_ref: s1
    title: Implement slice
contract_revision: 1
depends_on: []
tags: [default]
scope:
  - src/example.rs
status: planned
```

For full field parsing rules, see [`crates/acdd-state/src/spec.rs`](../../crates/acdd-state/src/spec.rs)
and the example feature plan in [example-feature-plan.md](example-feature-plan.md).

---

## Five Flat MCP State Tools and CLI Parity

State operations map symmetrically across the flat `0.6.2` CLI and five state
MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`,
`acdd_blackboard`):

| Operation | MCP tool / action | CLI command | Purpose |
| --- | --- | --- | --- |
| Status / List / Show / Next | `acdd_state(action: "status \| list \| show \| next \| observability")` | `acdd status`, `list`, `show`, `next` | Read-only run/node inspection, ready waves, and telemetry |
| Start / Draft | `acdd_state(action: "start \| draft")` | `acdd start`, `acdd draft` | Initialize a new `Run` or draft numbered spec |
| Scope / Reopen / Sync / Subtask | `acdd_state(action: "extend_scope \| reopen \| sync \| subtask")` | `acdd extend-scope`, `reopen`, `sync`, `subtask` | Manage scope extensions, cascades, spec sync, and subtasks |
| Clear / Run | `acdd_state(action: "clear \| run")` | `acdd clear`, `acdd run` | Clear tombstones and execute DAG waves (with internal `rebase-ff` landing) |
| Claim | `acdd_claim` | `acdd claim [<node>]` | Atomically claim a ready/active node and return `ClaimResp` |
| Verify | `acdd_verify` | `acdd verify <node>` | Execute `command:`, `call:`, `checks:`, and scope barrier without advancing |
| Submit | `acdd_submit` | `acdd submit <node> --action pass\|reject` | Advance on `pass` or rewind along `fail:` with `RejectionFindings` |
| Snapshot | `acdd_state(action: "snapshot", snapshot_action: "save \| restore \| clear \| list")` | `acdd snapshot save\|restore\|list\|clear` | Save, restore, list, or delete task and standalone Git checkpoints |
| Blackboard | `acdd_blackboard(action: "read \| post")` | `acdd blackboard --action read\|post` | Scoped coordination messages (`draft`, `notes`, `findings`, `blockers`, `decisions`, `deferred`) |

All 19 built-in MCP tools (plus `<domain>_<name>` plugin tools) accept an
optional `worktree` selector. See [CLI reference](cli.md) and
[MCP tools](mcp-tools.md) for exact flags and schemas.

---

## Gates and evidence

Every gate transition is mediated by `claim -> (verify) -> submit` inside
`crates/acdd-state/src/machine/`:

- **Claim (`claim`)**: Acquires or resumes an active claim in `gate_claims`,
  bumping `claim_revision` and `node_version` atomically, recording `claim_tree`
  from the scoped Git tree (`WorkspacePathPolicy::is_candidate_included`), and
  returning `ClaimResp` (including `context_reset_required: true` when re-entering
  after `rearchitect`). When `git.worktrees: managed` (`acdd start --worktree` or
  configured in the profile), claiming a `TaskGate` without an explicit
  `worktree` override automatically creates `.worktrees/<run_name>-<task_ref>`
  and branch `task/<run_name>-<task_ref>` (reused by downstream gates of that
  task) and returns it in `ClaimResp.worktree`; under default `git.worktrees: manual`
  or when an explicit `worktree` override is supplied, `claim` creates no new
  worktree and binds to the active/selected checkout. Concurrent foreign claims
  require `--force` (and `--steal-remote` if `session_fingerprint` differs across
  hosts).
- **Verify (`verify`)**:
  1. Validates working-tree changes against `WorkspacePathPolicy::is_agent_writable`.
  2. Executes configured `command:` (applying `test_filter` / `{test_filter}`
     when `--test` is supplied) or `call: <domain>.<action>` (`with:`), saving a
     `gate_traces` row and streaming output to `.acdd/run/<run_id>/traces.jsonl`.
  3. Runs configured `checks: [<domain>.<check>]` (e.g., `code.seam` reachability
     and `code.signature_scope`).
  4. Validates `ProofKind`: `command` requires exit `0`.
- **Submit (`submit`)**:
  - Verifies `claim_revision` parity (`STALE_CLAIM_REVISION`) and
    `independent_from` worker separation (`TASK_REVIEW_NOT_INDEPENDENT`).
  - Checks `save.immutable` parity against `claim_tree` (`TASK_CANDIDATE_MISMATCH`)
    and `need:` predecessor accepted tree parity, plus `blueprint` `max_net_loc`
    production LOC budgets on `tasks.build` unless justified on `blackboard`
    (`topic: decisions`).
  - On `--action pass`: validates proof (auto-verifying `command` if
    no fresh trace exists, validating `review_evidence` against `contours:`, or
    validating `custom_evidence` against `proofs.<name>` / inline `{ scheme }`),
    checks that all task `subtasks` are `completed` or `cancelled` with
    non-empty evidence when `receipt: true`, executes `save.post` (`none`,
    `commit`, or `snapshot`), materializes any declared `save.report` (returning `report_path` in `SubmitResp`), writes `gate_artifacts`, and transitions along `pass:`.
  - On `--action reject`: requires structured `RejectionFindings` (`findings` +
    `summary` + optional `kind`: `defect | architectural_drift`), routes to `fail:`
    on `defect` or `fail_drift:` on `architectural_drift`, increments `attempt` /
    `cycle_count`, or marks the node `escalated` when `max_attempts` is reached.

### 6-Tier Claim Context Budget (`ClaimResp`)

[`GateContextAssembler`](../../crates/acdd-state/src/context.rs) (exposed via
`build_claim_context`) builds `ClaimResp` identically for `claim`, `show`, and
`run` within `ResponsePolicy.max_output_bytes` across six ordered tiers:

1. **Tier 1 (`contract` + `guidance`)**: Mandatory claimed contract (omitting
   empty `invariants` and default `tools` matching `profile.default_tools`,
   sending the full spec body on `blueprint_synthesis` or taskless specs and
   `spec_path` on post-task root gates) plus resolved gate guidance inside
   `limits.max_guidance_bytes` (never truncated; fails closed with
   `GUIDANCE_BUDGET_EXCEEDED`).
2. **Tier 2 (`unresolved_findings` + failed trace tail)**: Container-sibling
   (`parent_id == node.parent_id`) rework findings and latest failed trace
   excerpt, cleared automatically once the rejecting node passes or the latest
   container trace passes/runs.
3. **Tier 3 (`pre_task_slice`)**: Per-task slice (`proof.tasks[<task_ref>]`)
   with global negative boundaries and cited invariants, or the global
   blueprint slice on post-task root gates, plus `predecessor_proofs` from
   completed same-container sibling gates emitting `ArtifactKind::Proof`
   (e.g., `recon` $\to$ `analyze` $\to$ `triage` in `audit`) (skipped alongside Tiers 4–5 on pure
   receipt/commit gates).
4. **Tier 4 (`dependency_receipts`)**: Accepted receipts of completed dependency
   tasks, deduplicated to the latest `claim_revision` per dependency with compact
   coordinates (`node_cursor`, `candidate_tree`, `committed_oid?`, `accepted_at`).
5. **Tier 5 (`blackboard`)**: Up to 15 open messages (`Open` only, excluding
   `Findings` and `Deferred`; 500 characters per payload), prioritizing
   `Decisions` and `Blockers` before filling remaining slots with newest `Notes`.
6. **Tier 6 (`context_lenses`)**: `DomainProvider` lenses declared in `gate.context`
   (`docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`)
   given proportional per-lens budgets and skipping empty `items: []` lenses cleanly
   without mutating `omitted_sections` (`omitted_sections` records only actual budget omissions).

---

## Task snapshots

Use `acdd snapshot save|restore|list|clear [<target>] [--all] [--rev N] [--run RUN] [--delete] [--older-than DUR] [--worktree PATH]` or `acdd_state(action: "snapshot", snapshot_action: "save|restore|clear|list", task_ref: ..., name: ..., all: ..., rev: ..., run: ..., delete: ..., older_than: ..., worktree: ...)` to manage checkpoints. Save captures admitted files (task scope when a run task matches; otherwise the worktree candidates outside `.acdd/**`). Restore applies a selected revision inside that scope. Clear deletes one revision (`--rev`), refs older than `<N><s|m|h|d|w>`, a named prefix, or `--all`. Omitting `--run` resolves to task mode if the target matches a task in an unambiguously resolved active run (cleaned up on `deliver`, run `clear`, or `handoff`), except `clear` with `--rev` or `--older-than`, which requires `--run` to target task refs; otherwise it uses standalone refs at `refs/acdd/snapshots/_adhoc/<label>/r<rev>`, which `deliver`, run `clear`, and `handoff` do not remove. Profiles declare `save.post: snapshot` only on leaf task gates.

---

## Scope and candidate tree management

[`WorkspacePathPolicy`](../../crates/acdd-core/src/path_policy.rs) defines four
strictly separated path predicates so candidate tree computation, human spec
authority, and agent writes never conflate:

1. **`is_engine_managed(rel_path)`**: Matches active `Run` `spec_path` /
   `archive_path` files. Human spec edits are reconciled via `acdd sync`.
2. **`is_candidate_included(rel_path)`**: Computes reproducible Git tree OIDs
   (`claim_tree`, `accepted_tree`). Excludes `.git/**`, `.worktrees/**`,
   `is_engine_managed`, configured `state_db` / `code_db` (+ `-wal`, `-shm`,
   `.backup` sidecars), and `.acdd/**` **except** the tracked configuration
   whitelist (`.acdd/profile.yaml`, `.acdd/profiles/**`, `.acdd/frameworks/**`).
   Tracked assets like `Cargo.lock` and `tests/**/*.jsonl` remain included.
3. **`is_scope_admitted(rel_path, scope)`**: Brace-aware (`{a,b}`) component-wise
   glob/directory match owned solely by `acdd_core::path_policy` against the
   node's initial `scope` + approved `extended_scope`.
4. **`is_agent_writable(rel_path, admitted_scope, protected_paths)`**: Enforces
   `is_candidate_included(rel_path)` AND `!is_scope_admitted(rel_path, protected_paths)`
   AND `is_scope_admitted(rel_path, admitted_scope)`. Out-of-scope edits fail
   closed with `OUT_OF_SCOPE_CHANGES`.

### Scope extension and worktree integration

- **`extend-scope`**: Extends `admitted_scope` for a claimed node with a
  non-empty `--reason`, rejecting protected paths (`.acdd/**`, `protected_paths`)
  with `OUT_OF_SCOPE_CHANGES` and overlapping active incomplete tasks with
  `TASK_SCOPE_CONFLICT`.
- **Worktree integration**: Headless `acdd run` waves land completed task
  branches into the run worktree internally via `rebase-ff`
  (`integrate_task_branch`), protecting `main`/`master` unless `--allow-main`
  is supplied (`MAIN_BRANCH_PROTECTED`) and aborting cleanly on conflicts
  (`GIT_INTEGRATION_CONFLICT`). Manual worktree runs integrate via standard
  `git merge` / `git rebase`, and terminal verification checks content parity
  by Git `TreeOid` (`verify_tasks_integrated` / `TASK_NOT_INTEGRATED`).

### Linked repository tasks

In `0.6.2`, `linked_workspaces` in `acdd.yaml` applies solely to multi-repository
code and documentation indexing (`acdd-index`), except read-only cross-workspace
dependency resolution (`<ws>/<run>:<task>`) in `acdd_state::usecases::deps`
which reads the linked workspace's own `acdd.yaml` (`specs` and `state_db`)
strictly read-only. State runs (`runs`, `gate_nodes`) and write scopes stay
confined to the primary workspace repository and its linked Git worktrees;
cross-repository state writes are not admitted.

---

## SQLite storage, locks, and backup recovery

[`crates/acdd-state/src/store/`](../../crates/acdd-state/src/store/mod.rs)
provides two `StateBackend` implementations (`SqliteStateBackend` for durable runs
and `MemoryStateBackend` for in-memory `RunHarness` tests) over `StateReader` and
`StateTx` optimistic CAS mutations (`node_version` and `runs.generation`).
Read-only queries (`open_read_only_if_exists`) strictly open
`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` first (handling WAL databases
without `-shm` without unintended writes) and never create missing DB files.

### Primary worktree layout and advisory locks

Regardless of which linked Git worktree invokes `acdd`, ACDD resolves the
primary repository root and common Git dir via
`git rev-parse --path-format=absolute --git-common-dir`:

- **State DB**: `<primary_worktree>/.acdd/state.sqlite` (overridable via `state_db` / `--state-db`)
- **Run trace dir**: `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl` (pruned via `retain_traces`, default `20`)
- **Report artifacts (`save.report`)**: `<primary_worktree>/.acdd/reports/...` (when `save.report` targets `.acdd/`, gitignored by default) or `<target_worktree>/<relpath>` (participates in scoped commits when `save.post: commit`)
- **Advisory locks & backup** (inside `<git_common_dir>/acdd/`):
  - `repo.lock`: Serializes Git branch/worktree/ref mutations and run initialization
  - `backup.lock`: Serializes snapshot backup creation and recovery promotion
  - `traces.lock`: Serializes run trace retention pruning
  - `state.bak.sqlite`: Verified SQLite online-backup snapshot maintained atomically alongside `_sqlite_meta.write_seq` / `last_backup_seq`

### Schema (`v2`: 9 domain tables + `_sqlite_meta`)

[`crates/acdd-state/src/store/sqlite/schema.rs`](../../crates/acdd-state/src/store/sqlite/schema.rs)
defines `_sqlite_meta` (`schema_version = 2`, `write_seq`, `last_backup_seq`) plus
9 pure domain tables:

| Table | Purpose |
| --- | --- |
| `runs` | Run identity, `spec_id`, resolved `profile_json`, `spec_digest`, `isolation`, `target_base`, `status`, `generation` |
| `gate_nodes` | Hierarchical nodes (`container`, `root_gate`, `task`, `task_gate`), status, `node_version`, `claim_revision`, `contract_revision`, trees, scopes |
| `task_dependencies` | Intra-run and cross-run task dependency DAG edges and satisfaction flags |
| `subtasks` | Subtask status (`pending \| in_progress \| completed \| cancelled`) with mandatory terminal `evidence` CHECK constraint |
| `gate_claims` | Active and historical claim revisions (`active \| passed \| rejected \| superseded \| reopened \| cancelled`) |
| `gate_traces` | Observed command/action/check verification runs (`running`, `passed`, `failed`, `timed_out`, `check_failed`, `scope_violation`, `aborted`; schema CHECK also accepts legacy `expected_failure` and `compile_error` rows) |
| `gate_artifacts` | Structured `proof`, `findings`, `receipt`, and accepted candidate tree JSON + commit OIDs |
| `blackboard` | Scoped coordination messages (`draft \| notes \| findings \| blockers \| decisions \| deferred`) with resolution status |
| `run_tombstones` | Compact closed-run summaries, `metrics_json`, and cleanup manifests retained across `archive` / `prune` / `clear` |

When `.acdd/state.sqlite` is missing or damaged, `SqliteStateBackend` restores from
`<git_common_dir>/acdd/state.bak.sqlite` under `backup.lock`, reconciles
committed archive manifests and tombstones, and verifies SQLite `quick_check`.

---

## Canonical Diagnostic Error Codes (`ErrorCode`)

[`crates/acdd-core/src/errors.rs`](../../crates/acdd-core/src/errors.rs) defines
28 machine-readable `ErrorCode` variants mapped to stable CLI exit codes:

- **Exit `2` (Usage / Config / Schema)**:
  `CONFIG_MIGRATION_REQUIRED`, `ACDD_PROFILE_SCHEMA_INVALID`, `COMMAND_UNBOUND`,
  `COMMAND_TEST_FILTER_UNSUPPORTED`, `COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED`,
  `UNKNOWN_GUIDANCE_SLOT`, `GUIDANCE_BUDGET_EXCEEDED`, `RUN_SELECTOR_AMBIGUOUS`,
  `FEATURE_DISABLED`, `WORKFLOW_RUNNER_REQUIRED`.
- **Exit `3` (State / Gate / Scope Violation)**:
  `TASK_ALREADY_CLAIMED`, `STALE_CLAIM_REVISION`, `TASK_REVIEW_NOT_INDEPENDENT`,
  `OUT_OF_SCOPE_CHANGES`, `TASK_SCOPE_CONFLICT`, `TASK_PROOF_UNOBSERVED`,
  `TASK_CANDIDATE_MISMATCH`, `TASK_NOT_INTEGRATED`, `PRE_TASK_ARTIFACT_STALE`,
  `DEPENDENT_TASK_COMPLETED`, `DOMAIN_CAPABILITY_UNAVAILABLE`,
  `MAIN_BRANCH_PROTECTED`.
- **Exit `4` (Execution / Git / Backend / Uncertainty)**:
  `GATE_VERIFICATION_FAILED`, `RUNNER_NO_SUBMISSION`,
  `GIT_INTEGRATION_CONFLICT`, `EXTERNAL_GIT_DRIFT`,
  `MUTATING_ACTION_UNCERTAIN_OUTCOME`, `BACKEND_CONFLICT`.
