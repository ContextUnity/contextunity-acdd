---
doc_type: api
title: State, tasks & lifecycle
---

# State, tasks & lifecycle

A single declarative `Run` + `WorkflowDef` state engine in
[`crates/acdd-state`](../../crates/acdd-state/src/lib.rs), declarative spec
parsing/validation/roadmap in [`crates/acdd-spec`](../../crates/acdd-spec/src/lib.rs),
and leaf contracts in [`crates/acdd-core`](../../crates/acdd-core/src/lib.rs)
([ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md)).
Domain-specific code/docs hooks (`call:`, `checks:`, `context:`, and `<domain>_<name>`
plugin tools) connect through [`DomainProvider`](../../crates/acdd-core/src/ports.rs)
implementations in [`src/state_bridge/`](../../src/state_bridge/mod.rs)
([ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md)).

---

## Core Concepts: `Run`, `WorkflowDef`, and `GateNode`

Every lifecycle execution instantiates one **`Run`** bound to a resolved
**`v2` `WorkflowDef`**:

- **`Run`**: Created by `acdd start` (from a workspace-confined spec file or
  ephemeral `--goal`), storing immutable `profile_json`, `spec_digest`,
  `isolation` (`worktree` or `in_place`), optional `target_base` / `branch` /
  `worktree`, `trace_id`, and monotonic optimistic `generation` (`>= 1`).
- **`GateNode` (`gate_nodes`)**: Materializes the workflow's `gates:` hierarchy
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

For full field parsing rules, see [`crates/acdd-spec/src/spec.rs`](../../crates/acdd-spec/src/spec.rs)
and the example feature plan in [example-feature-plan.md](example-feature-plan.md).

---

## Five Flat MCP State Tools and CLI Parity

State operations map symmetrically across the flat CLI and five state MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`):

| Operation | MCP tool / action | CLI command | Purpose |
| --- | --- | --- | --- |
| Status / List / Show / Next | `acdd_state(action: "status \| list \| show \| next \| observability")` | `acdd status`, `list`, `show`, `next` | Read-only run/node inspection, ready waves, and telemetry |
| Start / Draft | `acdd_state(action: "start")` | `acdd start`, `acdd draft` | Initialize a new `Run` (MCP/CLI) or draft numbered spec (CLI `acdd draft`) |
| Roadmap | *(CLI only; `acdd_state` redirects)* | `acdd roadmap` | Inspect cross-spec runs, dependency waves, and critical path |
| Scope / Reopen / Sync / Subtask | `acdd_state(action: "extend_scope \| reopen \| sync \| subtask")` | `acdd extend-scope`, `reopen`, `sync`, `subtask` | Manage scope extensions, cascades, spec sync, and subtasks |
| Clear / Run | `acdd_state(action: "clear \| run")` | `acdd clear`, `acdd run` | Clear tombstones and execute DAG waves (with internal `rebase-ff` landing) |
| Claim | `acdd_claim` | `acdd claim [<node>] [--detail compact\|full]` | Atomically claim a ready/active node and return `ClaimResp` (`<= 10,000` B compact soft cap, `32 KiB` full) |
| Verify | `acdd_verify` | `acdd verify <node>` | Execute `command:`, `call:`, `checks:`, and scope barrier without advancing |
| Submit | `acdd_submit` | `acdd submit <node> --action pass\|reject` | Advance on `pass` or rewind along `fail:` with `RejectionFindings` |
| Snapshot | `acdd_state(action: "snapshot", snapshot_action: "save \| restore \| clear \| list")` | `acdd snapshot save\|restore\|list\|clear` | Save, restore, list, or delete task and standalone Git checkpoints |
| Blackboard | `acdd_blackboard(action: "read \| post")` | `acdd blackboard --action read\|post` | Scoped coordination messages (`draft`, `notes`, `findings`, `blockers`, `decisions`, `deferred`) |

- **Optional `worktree` selector**: supported across all 18 built-in MCP tools and `<domain>_<name>` plugin tools (see [CLI reference](cli.md) and [MCP tools](mcp-tools.md)). Out-of-workflow `roadmap` and `draft` actions are excluded from MCP `acdd_state` and fail closed with redirect hints to CLI `acdd roadmap` and `acdd draft`.
- **Enforced invariants (`[ENFORCED]`)**: in Tier 6 `ClaimResp.context_lenses`, `docs.invariants` marks invariants backed by `acdd.yaml` `architecture.rules` or `acdd-rule` blocks with `[ENFORCED]`.
- **Scoped vs. workspace lint checks**: `code.lint` runs scoped to the task's files on task gates and workspace-wide on root gates, binding `index_generation` and `live_files_digest` into `execution_digest`.

---

## Gates and evidence

Every gate transition is mediated by `claim -> verify -> submit` (with `verify`
required before `submit` whenever the gate declares `command:`, `call:`,
`checks:`, or `proof: command|call`) inside `crates/acdd-state/src/machine/`:

- **Claim (`claim`)**:
  - Atomically locks or resumes the gate in `gate_claims`, bumping `claim_revision` and `node_version` and recording `claim_tree`.
  - Returns the 6-tier `ClaimResp` envelope (setting `context_reset_required: true` when re-entering after `rearchitect`).
  - Under `git.worktrees: managed` (`acdd start --worktree`), claiming a task gate creates `.worktrees/<run_name>-<task_ref>` on branch `task/<run_name>-<task_ref>`; under `manual` or an explicit `worktree`, it binds to the selected checkout.
  - Taking over another worker's active claim requires `--force` (plus `--steal-remote` across different host fingerprints).
- **Verify (`verify`)**:
  1. Validates working-tree edits against `WorkspacePathPolicy::is_agent_writable`.
  2. Runs configured `command:` (expanding `{test_filter}` when `--test` is passed) or `call: <domain>.<action>`, streaming output to `.acdd/run/<run_id>/traces.jsonl`.
  3. Runs configured `checks: [<domain>.<check>]` (such as `code.seam` and `code.lint`), then observes post-check freshness (`observe_freshness`) after all checks pass so post-mutation `index_generation` and `execution_digest_salt` values bind into `execution_digest`.
  4. Checks `ProofKind` (`command` requires exit status `0`).
  5. On a mechanical gate (`role` omitted) with a `fail:` route, interactive
     `acdd verify` and `acdd_verify` record the failed trace and return the error
     with the same claim still active. Retry verification under that claim or
     use `acdd submit --action reject` to take the failure route. `acdd run`
     takes the failure route automatically.
- **Submit (`submit`)**:
  - **Concurrency & independence**: verifies active claim ownership (`TASK_ALREADY_CLAIMED`), `claim_revision` (`STALE_CLAIM_REVISION`), and `independent_from` worker separation (`TASK_REVIEW_NOT_INDEPENDENT`).
  - **Trace & tree/LOC guards**: requires a fresh passing `verify` trace and non-mutating `observe_freshness` parity without auto-running `verify` (`TASK_PROOF_UNOBSERVED` when missing or stale), plus `save.immutable` checks against `claim_tree` (`TASK_CANDIDATE_MISMATCH`), `need:` predecessor parity, and `max_net_loc` budgets on `tasks.build`.
  - **On `--action pass`**: validates proofs (`command`, `review_evidence`, or JSON Schema `custom_evidence`), confirms all task `subtasks` have terminal evidence when `receipt: true`, runs `save.post` (`none`, `commit`, or `snapshot`), writes `save.report`, and advances along `pass:`.
  - **On `--action reject`**: requires structured `RejectionFindings` (`defect` routes to `fail:`; `architectural_drift` routes to `fail_drift:`), or marks the node `escalated` at `max_attempts`.

### 6-Tier Claim Context Budget (`ClaimResp`)

[`GateContextAssembler`](../../crates/acdd-state/src/context/mod.rs) (exposed via
`build_claim_context`) builds `ClaimResp` identically for `claim`, `show`, and
`run` with `optional_target_cap = response_max_bytes.min(response_hard_cap)`
(default `10,000`-byte compact soft ceiling; `detail: "full"` expands to `32 KiB`)
and `response_hard_cap` (`ResponsePolicy.max_output_bytes`, bounded by `32 KiB`
in full mode, capping the entire `ClaimResp` across Tiers 1–6 and
`omitted_sections`) across six ordered tiers:

1. **Tier 1 (`contract` + `guidance`)**: Mandatory claimed contract (omitting
   empty `invariants` and default `tools` matching `workflow.default_tools`,
   sending the full spec body on `blueprint_synthesis` or taskless specs and
   `spec_path` on post-task root gates) plus resolved gate guidance (never
   truncated; settled at its exact `total_bytes` fixpoint and failing closed with
   `GUIDANCE_BUDGET_EXCEEDED` only when Tier 1 exceeds
   `tier1_cap = limits.max_guidance_bytes.min(response_hard_cap)`, or when
   mandatory Tier 1 plus required omission notices still exceeds
   `response_hard_cap` after reclaiming optional items). When compact Tier 1
   fits within `tier1_cap` but exceeds `optional_target_cap` (`10,000` bytes),
   `ClaimResp` succeeds with zero optional budget and records omitted Tiers 2–6
   in `omitted_sections` within `response_hard_cap` (reclaiming lower-priority
   optional items when needed so omission notices are never silently dropped).
   When `limits.max_guidance_bytes < optional_target_cap`, `tier1_cap` bounds
   only Tier 1 while optional Tiers 2–6 may fill up to
   `optional_target_cap <= response_hard_cap`.
2. **Tier 2 (`unresolved_findings` + failed trace tail)**: Container-sibling
   (`parent_id == node.parent_id`) rework findings and latest failed trace
   excerpt, cleared automatically once the rejecting node passes or the latest
   container trace passes/runs; drops trace tail first and omits
   `unresolved_findings` into `omitted_sections` when compact budget is exceeded.
3. **Tier 3 (`pre_task_slice`)**: Per-task slice (`proof.tasks[<task_ref>]`)
   with global negative boundaries and cited invariants, or the global
   blueprint slice on post-task root gates, plus `predecessor_proofs` from
   completed same-container sibling gates emitting `ArtifactKind::Proof`
   (e.g., `recon` $\to$ `analyze` $\to$ `triage` in `audit`) (skipped alongside Tiers 4–5 on pure
   receipt/commit gates).
4. **Tier 4 (`dependency_receipts`)**: Accepted receipts of completed dependency
   tasks, deduplicated to the latest `claim_revision` per dependency with compact
   coordinates (`node_cursor`, `candidate_tree`, `committed_oid?`, `accepted_at`).
5. **Tier 5 (`blackboard`)**: Up to 6 open messages (`Open` only, excluding
   `Findings` and `Deferred`; 220 characters per payload), prioritizing
   `Decisions` and `Blockers` before filling remaining slots with newest `Notes`;
   records omitted counts in `omitted_sections` whenever `shown < relevant_total`.
6. **Tier 6 (`context_lenses`)**: `DomainProvider` lenses declared in `gate.context`
   (`docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`)
   receive proportional per-lens budgets. Producers compact lens payloads upfront
   (`docs.instructions` omits heading TOC spam, `code.symbols` filters generated/derive
   methods, `code.tests` deduplicates paths capped at top 8, and `code.external_callers`
   caps at top 5 plus overflow count). Providers and Tier 6 fallback share compact JSON
   accounting for complete and truncated envelopes, including array separators. Bounded 0-1
   knapsack selection preserves original candidate order across equal-score subsets. Empty
   lenses are skipped, and budget-exhausted lenses are recorded in `omitted_sections`.
   `code.external_callers` surfaces non-test external consumers for both public callables and
   scoped `.sql` `table` nodes (active or baseline-deleted) across `defines_table`,
   `queries_table`, `mutates_table`, and `references_table` incoming edges. Baseline lookup
   for unindexed targets uses the same quote-aware SQL identifier keys as graph linking.
   `code.signature_scope` checks callable signatures only.

---

## Task snapshots

Manage Git checkpoints via `acdd snapshot <save|restore|list|clear> ...` or `acdd_state(action: "snapshot", snapshot_action: "save|restore|clear|list", ...)`:

- **`save`**: captures admitted files (task scope when matched to a run task; otherwise all non-`.acdd/**` worktree candidates).
- **`restore`**: reapplies a selected snapshot revision inside that scope.
- **`list`**: lists saved snapshot revisions for a task or standalone label.
- **`clear`**: deletes one revision (`--rev`), refs older than `<N><s|m|h|d|w>` (`--older-than`), a named prefix, or `--all` (alias `--delete`).
- **Task vs. standalone refs**: omitting `--run` resolves to task mode if the target matches an unambiguously resolved active run (cleaned up automatically on `deliver`, run `clear`, or `handoff`). When clearing with `--rev` or `--older-than` without `--run`, it targets standalone refs at `refs/acdd/snapshots/_adhoc/<label>/r<rev>`, which task lifecycle events never delete.
- **Workflow hook**: `save.post: snapshot` is valid only on leaf task gates.

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
   whitelist (`.acdd/workflows/**`, `.acdd/specs/**`, `.acdd/frameworks/**`).
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

- **Indexing & read-only dependencies**: `linked_workspaces` in `acdd.yaml` federates code/docs indexing and supports read-only cross-workspace task dependencies (`<ws>/<run>:<task>`).
- **Write confinement**: active `runs`, `gate_nodes`, and agent write scopes remain strictly confined to the primary workspace repository and its Git worktrees.

---

## SQLite storage, locks, and backup recovery

[`crates/acdd-state/src/store/`](../../crates/acdd-state/src/store/mod.rs) provides two `StateBackend` implementations (`SqliteStateBackend` for durable runs and `MemoryStateBackend` for in-memory tests) using optimistic CAS counters (`node_version` and `runs.generation`). Read-only queries open SQLite with `SQLITE_OPEN_READ_ONLY` and never create missing database files.

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
| `gate_traces` | Observed command, action, and check verification runs (`running`, `passed`, `failed`, `timed_out`, `check_failed`, `scope_violation`, `aborted`) |
| `gate_artifacts` | Structured `proof`, `findings`, `receipt`, and accepted candidate tree JSON + commit OIDs |
| `blackboard` | Scoped coordination messages (`draft \| notes \| findings \| blockers \| decisions \| deferred`) with resolution status |
| `run_tombstones` | Compact closed-run summaries, `metrics_json`, and cleanup manifests retained across `archive` / `prune` / `clear` |

When `.acdd/state.sqlite` is missing or damaged, `SqliteStateBackend` restores from
`<git_common_dir>/acdd/state.bak.sqlite` under `backup.lock`, reconciles
committed archive manifests and tombstones, and verifies SQLite `quick_check`.

---

## Canonical Diagnostic Error Codes (`ErrorCode`)

[`crates/acdd-core/src/errors.rs`](../../crates/acdd-core/src/errors.rs) maps `AcddError` (the sole structured error type across all workspace crates) and its `ErrorCode` variants to stable CLI exit codes:

| Exit Code | Category | Diagnostic Codes |
| --- | --- | --- |
| **`2`** | Usage, Config & Schema | `CONFIG_MIGRATION_REQUIRED`, `ACDD_WORKFLOW_SCHEMA_INVALID`, `ACDD_SPEC_SCHEMA_INVALID`, `COMMAND_UNBOUND`, `COMMAND_TEST_FILTER_UNSUPPORTED`, `COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED`, `UNKNOWN_GUIDANCE_SLOT`, `GUIDANCE_BUDGET_EXCEEDED`, `RUN_SELECTOR_AMBIGUOUS`, `FEATURE_DISABLED`, `WORKFLOW_RUNNER_REQUIRED` |
| **`3`** | State, Gate & Scope | `TASK_ALREADY_CLAIMED`, `STALE_CLAIM_REVISION`, `TASK_REVIEW_NOT_INDEPENDENT`, `OUT_OF_SCOPE_CHANGES`, `TASK_SCOPE_CONFLICT`, `TASK_PROOF_UNOBSERVED`, `TASK_CANDIDATE_MISMATCH`, `TASK_NOT_INTEGRATED`, `PRE_TASK_ARTIFACT_STALE`, `DEPENDENT_TASK_COMPLETED`, `DOMAIN_CAPABILITY_UNAVAILABLE`, `MAIN_BRANCH_PROTECTED` |
| **`4`** | Execution, Git & Storage | `GATE_VERIFICATION_FAILED`, `RUNNER_NO_SUBMISSION`, `GIT_INTEGRATION_CONFLICT`, `EXTERNAL_GIT_DRIFT`, `MUTATING_ACTION_UNCERTAIN_OUTCOME`, `BACKEND_CONFLICT` |

Additional validation diagnostics include `WORKFLOW_VALID` (pass diagnostic from `acdd validate` for resolved `WorkflowDef` documents) and registry-local `FEATURE_REQUIRED_BY_PRESET` text inside `ACDD_WORKFLOW_SCHEMA_INVALID` `message` from `SubsystemRegistry::validate()` ([`subsystems.rs`](../../crates/acdd-core/src/subsystems.rs)), while cross-layer workspace-preset vs. workflow `modules.required` mismatches emit `DOMAIN_CAPABILITY_UNAVAILABLE` ([`validate.rs`](../../crates/acdd-state/src/workflow/validate.rs)). See [configuration](configuration.md#state-storage-and-module-toggles), [CLI reference](cli.md), and [MCP tools](mcp-tools.md) for the exact `StartReq` / `StateInput`, `SyncReq` / `SyncResp`, `DraftReq` / `DraftResp`, `ValidateReq`, and `RoadmapReq` / `RoadmapSpecEntry` field contracts.
