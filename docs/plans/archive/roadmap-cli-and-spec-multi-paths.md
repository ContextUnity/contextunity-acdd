---
id: roadmap-cli-and-spec-multi-paths
title: acdd roadmap CLI & Multi-Path Glob Spec Collections
doc_type: plan
status: completed
spec: 'Add native multi-path and glob support for acdd.yaml specs collections (e.g. extensions/*/docs/milestones), prefixless plans defaults and draft support, whole-spec task dependencies, and a read-only `acdd roadmap [spec_id]` CLI + `acdd_state(action: roadmap)` query showing in-progress runs, topological execution waves, critical-path priority, scope-overlap parallel safety, and per-task dependency/blocker drill-down for v0.6.2.'
owners:
- acdd.yaml
- crates/acdd-core/src/config.rs
- crates/acdd-index/src/scanner/mod.rs
- crates/acdd-state/src/usecases/
- crates/acdd-state/src/validate.rs
- crates/acdd-state/tests/state_integration/
- src/cli/mod.rs
- src/cli/guide.rs
- src/format/
- src/mcp/tools/
- src/state_bridge/mod.rs
- tests/acdd/
- docs/reference/
- CHANGELOG.md
- docs/plans/roadmap-cli-and-spec-multi-paths.md
invariants:
- 'INV-SINGLE-OWNER: Spec directory glob resolution lives once in `acdd_core::config::SpecCollectionConfig`; roadmap discovery, dependency resolution, and scope overlap reuse `acdd_state::usecases::deps::{collect_workspace_specs, resolve_dep_target, parse_task_dependency}`, `list_all_runs`/`list_all_tombstones`, and `acdd_core::path_policy::scopes_overlap` with zero duplicate parsers or matchers.'
- 'INV-READ-ONLY-ROADMAP: `StateQuery::Roadmap` and `acdd roadmap` operate strictly over `dyn StateReader`, never open write handles, and never create missing `.acdd/state.sqlite` files.'
- 'INV-CONFINED-SPEC-GLOBS: Multi-path and glob expansion in `specs.<collection>` enforces workspace-root confinement (`is_normalized_relative` + `checked_child`), rejects `..`, absolute paths, and symlink escapes, and preserves deterministic sorted ordering and `acdd draft` target directory resolution.'
- 'INV-DETERMINISTIC-WAVES: Spec and task topological waves (`Wave 0..N`), transitive `unblocks` counts, `parallel_safe` / `scope_overlaps` annotations, and cycle diagnostics are deterministic (`BTreeMap`/`BTreeSet`) across all collections (`milestones`, `plans`, extension collections, `linked_workspaces`) and across `planned` specs, active `Run`s, tombstones, and `archive/`.'
acceptance_criteria:
- 'AC-1: `acdd.yaml` `specs.<collection>` accepts single `path`, multi-entry `paths`, and confined globs (such as `extensions/*/docs/milestones`), with full backward compatibility for existing `path`-only configs; `default_specs()` uses `prefixes: []` for `plans` and `acdd draft` generates slug-only `<slug>.md` when `prefixes` is empty and `--prefix` is omitted.'
- 'AC-2: Cross-spec dependencies work uniformly across all collections (plan <-> milestone <-> task), including `TaskSpec.depends_on` referencing an entire other spec (`<spec_id>` or `<ws>/<spec_id>`) as well as `<spec_id>:<task_ref>`.'
- 'AC-3: `acdd roadmap` displays active runs (`in_progress` with task progress, active nodes/workers, worktree, and `unblocks`), `Wave 0` ready specs sorted by transitive `unblocks` criticality with `owners`/`scope` overlap markers (`parallel_safe` vs `scope_overlaps`), and `Wave 1..N` blocked specs with exact unresolved blockers.'
- 'AC-4: `acdd roadmap <spec_id>` works for both unstarted `planned` specs on disk and active `Run`s, showing intra-spec task waves, cross-spec `depends_on` status, scope overlaps between parallel tasks, and reverse `blocks` (`depended_on_by`) across all specs; `acdd roadmap --tasks` expands task waves across the workspace, with `--collection`, `--ready`, `--active`, `--format markdown|json`, and `acdd_state(action: "roadmap")` MCP parity.'
- 'AC-5: Table-driven integration tests in `crates/acdd-state/tests/state_integration/` and `tests/acdd/` verify multi-path globs, confinement rejections, prefixless draft, task-to-whole-spec dependencies, cross-spec waves, critical-path ranking, scope-overlap detection, and reverse task blockers; `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets`, `acdd docs check`, and `acdd validate` pass with zero leaks.'
closure:
  status: completed
  closed_at: 2026-11-19T16:18:55.886Z
  target_base: main
  target_base_commit: 4461137853b07fe7d7376dede4becaff5cdce244
  task_receipts:
    T-01:
      run_id: roadmap-cli-and-spec-20261119040500-1689469-1
      node_cursor: root:T-01.deliver
      gate_id: deliver
      worker_id: codex-reviewer-t1-r5
      claim_revision: 1
      contract_revision: 5
      candidate_tree: c7f7f02f6c291ab220cd47ded09416f196b03b64
      committed_oid: f70c3e6e63cc018f75dd3c6de4120dfb4bfcd19f
      accepted_at: 2026-11-19T05:19:13.462Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
          - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
          verified:
          - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
          - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
        review_summary: T-01 passed 5-round independent review (gpt-6.2-sol low) across all 6 contours and verified multi-path/glob SpecCollectionConfig, prefixless draft, and whole-spec external task deps.
    T-02:
      run_id: roadmap-cli-and-spec-20261119040500-1689469-1
      node_cursor: root:T-02.deliver
      gate_id: deliver
      worker_id: cursor-t2-rev-r26
      claim_revision: 1
      contract_revision: 15
      candidate_tree: c9ea0ead6155fb38fb53547ba9f0c934cf28a05d
      committed_oid: 6e59cf341459308ada76727685661767f695da50
      accepted_at: 2026-11-19T12:16:38.825Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-READ-ONLY-ROADMAP: Roadmap query never creates state.sqlite or opens write connections.'
          - 'INV-SINGLE-OWNER: Reuse collect_workspace_specs, resolve_dep_target, parse_task_dependency, and scopes_overlap without duplicating graph/glob logic.'
          - 'INV-DETERMINISTIC-WAVES: Output ordering across waves, specs, tasks, overlaps, and blockers is strictly deterministic.'
          verified: []
        review_summary: Deliver T-02 after R26 review pass (cursor-t2-rev-r26)
    T-03:
      run_id: roadmap-cli-and-spec-20261119040500-1689469-1
      node_cursor: root:T-03.deliver
      gate_id: deliver
      worker_id: coordinator
      claim_revision: 1
      contract_revision: 2
      candidate_tree: c7c302808698b81e16978cc6bd3fa6bbc9c53078
      committed_oid: b8c7877988ba4cf58f3842654d72dce93c9c0860
      accepted_at: 2026-11-19T13:59:28.233Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-READ-ONLY-ROADMAP: CLI `acdd roadmap` works without existing `.acdd/state.sqlite` and without eager `--profile` validation when `--profile` is omitted.'
          - 'INV-SINGLE-OWNER: CLI and MCP delegate directly to StateQuery::Roadmap with shared formatting.'
          verified: []
  rollup:
    verified_invariants:
      applicable:
      - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
      - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
      - 'INV-READ-ONLY-ROADMAP: Roadmap query never creates state.sqlite or opens write connections.'
      - 'INV-SINGLE-OWNER: Reuse collect_workspace_specs, resolve_dep_target, parse_task_dependency, and scopes_overlap without duplicating graph/glob logic.'
      - 'INV-DETERMINISTIC-WAVES: Output ordering across waves, specs, tasks, overlaps, and blockers is strictly deterministic.'
      - 'INV-READ-ONLY-ROADMAP: CLI `acdd roadmap` works without existing `.acdd/state.sqlite` and without eager `--profile` validation when `--profile` is omitted.'
      - 'INV-SINGLE-OWNER: CLI and MCP delegate directly to StateQuery::Roadmap with shared formatting.'
      verified:
      - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
      - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
    decisions:
    - 'Blueprint for roadmap-cli-and-spec-multi-paths: (T-01) Extend SpecCollectionConfig in crates/acdd-core/src/config.rs with backward-compatible path/paths + confined glob resolver (patterns, resolve_dirs, matches_rel_path, default_draft_dir), default plans prefixes: [], prefixless acdd draft in run_ops.rs, and task-to-whole-spec external dependency support in crates/acdd-state/src/usecases/deps.rs; wire all existing spec callers through SpecCollectionConfig; (T-02) Add read-only StateQuery::Roadmap in crates/acdd-state/src/usecases/roadmap.rs reusing collect_workspace_specs, resolve_dep_target, parse_task_dependency, list_all_runs/tombstones, and acdd_core::path_policy::scopes_overlap to compute active runs, topological waves, critical-path unblocks ranking, scope-overlap parallel safety, and per-task drill-down/reverse blocks; (T-03) Expose acdd roadmap CLI, compact Markdown/JSON formatter (plus omit raw profile_json bloat in Markdown state rendering), acdd_state(action: roadmap) MCP parity, tests, and v0.6.2 reference docs.'
    - 'BUDGET_OVERRIDE: Approved T-02 net prod LOC budget expansion (~1,433L in roadmap.rs + deps/profile/runner support; ~1,896 net non-runner lines) beyond the initial 305/450L draft estimate because T-02 absorbs cross-worktree discovery with silent digest deduplication and divergence warnings, current_only and archived filters, critical-path unblocks counts, Tarjan SCC cycle isolation, LinkedWsCache, single-pass task drill-down with reverse blocks, GateDef.tools resolution, and sequential per-target command execution. Condition: builder must still eliminate duplicated Kahn wave traversal, the repeated enqueue block in compute_unblocks_counts, and decompose build_task_entries cleanly for Round 8.'
    - 'Coordinator decision for dependency provenance: persist unqualified same-repository spec dependencies with workspace ''.'', and explicitly qualified dependencies with the canonical selected checkout path, including primary. Owned canonical checkout paths are scoped for observations and reverse edges. No SQLite schema change.'
  metrics_summary:
    decisions_count: 3
    deferred_findings_count: 0
    failed_traces: 13
    passed_traces: 37
    task_receipts_count: 3
    tasks_cancelled: 0
    tasks_completed: 3
    tasks_total: 3
    total_claims: 79
    total_review_cycles: 3
    total_traces: 53
---

# `acdd roadmap` CLI & Multi-Path Glob Spec Collections

Target release: **`v0.6.2`**.

## Motivation

When migrating multi-package repositories (such as ContextUnity monorepos with `docs/milestones`, `extensions/*/docs/milestones`, and `docs/plans`) onto ACDD specs, operators need:

1. **Multi-path / glob spec collections & prefixless plans**: `acdd.yaml` `specs.<collection>` should accept `paths: [docs/milestones, extensions/*/docs/milestones]` (as well as backward-compatible `path:`), while `plans` defaults to `prefixes: []` so plans use clean slug IDs without artificial `PLAN-001` prefixes.
2. **Universal cross-spec & task-to-spec dependencies**: Any spec or task across any collection (`plans`, `milestones`, extension collections, `linked_workspaces`) can depend on another spec or task—including a single task depending on an entire external spec without blocking the rest of its own spec.
3. **Workspace-wide `acdd roadmap` CLI**: A read-only command combining all unarchived specs on disk with live `.acdd/state.sqlite` runs, tombstones, and archives to show what is in progress, what to start first (critical-path `unblocks`), what can run safely in parallel (`owners`/`scope` overlap detection), and dependent task waves (`acdd roadmap <spec_id>` / `--tasks`).

## Reuse Map & Estimated Net Production Footprint (~590–620 LOC)

All core primitives already exist under `INV-SINGLE-OWNER` ([ADR 0003](../../adr/0003-unified-profile-graph-and-state-machine.md)):

| Component | Reused Canonical Owners | Est. Net Prod LOC |
|---|---|---:|
| Multi-path & glob `SpecCollectionConfig`, prefixless `plans`/`draft`, task $\to$ whole-spec deps (`T-01`) | `acdd_core::config::SpecCollectionConfig`, `acdd_core::fs::{is_normalized_relative, checked_child}`, `acdd_state::usecases::deps::{collect_workspace_specs, resolve_dep_target, parse_task_dependency}` | ~155 |
| `StateQuery::Roadmap` engine (`T-02`) | `collect_workspace_specs`, `resolve_dep_target`, `parse_task_dependency`, `list_all_runs`, `list_all_tombstones`, `acdd_core::path_policy::scopes_overlap` | ~305 |
| `acdd roadmap` CLI, formatter, MCP parity, docs (`T-03`) | `src/cli/mod.rs`, `src/state_bridge/mod.rs`, `src/format/`, `src/mcp/tools/` | ~165 |
| **Total production code** (+ ~180 LOC table-driven tests) | Zero new parsers, zero new glob matchers, zero new root test binaries | **~625** |

---

## Tasks in this plan

### task: T-01

```yaml
task_ref: T-01
target: Add confined multi-path/glob support to SpecCollectionConfig, prefixless plans/draft behavior, and task-to-whole-spec external dependencies.
spec: |
  1. Extend `SpecCollectionConfig` in `crates/acdd-core/src/config.rs`:
     - Keep `path: Option<String>` backward-compatible (defaulting `plans` in `default_specs()` to `prefixes: []`) and add `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub paths: Vec<String>`.
     - Require at least one configured pattern across `path` / `paths`; normalize each pattern, reject `..`, absolute paths, empty entries, and invalid glob syntax.
     - Add canonical `patterns(&self) -> Vec<&str>`, `resolve_dirs(&self, root: &Path) -> std::io::Result<Vec<PathBuf>>` (deterministic sorted expansion supporting `*` / `**` segments strictly confined to `root` via `checked_child` without following symlinks outside `root`), `matches_rel_path(&self, rel: &str) -> bool`, and `default_draft_dir(&self) -> Option<&str>` (first literal non-glob path).
     - Update all existing callers to use these canonical methods: `crates/acdd-state/src/usecases/deps.rs`, `crates/acdd-state/src/usecases/lifecycle.rs`, `crates/acdd-state/src/usecases/commands/run_ops.rs`, `crates/acdd-state/src/usecases/commands/start.rs`, `crates/acdd-state/src/validate.rs`, and `crates/acdd-index/src/scanner/mod.rs`.
  2. Prefixless `acdd draft`:
     - In `crates/acdd-state/src/usecases/commands/run_ops.rs` (`handle_draft_dyn`), when `col.prefixes.is_empty()` and `req.prefix.is_none()`, generate `spec_id = slug` and `spec_rel = "<dir>/<slug>.md"` (rejecting collision if the file already exists) instead of synthesizing `SPEC-001-<slug>`.
     - Keep `acdd.yaml` `specs.plans.prefixes: []`.
  3. Task-to-whole-spec external dependency support:
     - In `crates/acdd-state/src/usecases/deps.rs` (and `crates/acdd-state/src/usecases/commands/start.rs` / `sync.rs` / `machine` if needed), allow a `TaskSpec.depends_on` entry to target an entire external spec (`<spec_id>` when not matching a local `task_ref`, or `<ws>/<spec_id>` without `:<task_ref>`), resolving satisfaction when that target spec is completed (completed tombstone or committed completed archive) and participating in cross-spec cycle detection.
scope:
- acdd.yaml
- crates/acdd-core/src/config.rs
- crates/acdd-index/src/scanner/mod.rs
- crates/acdd-state/src/usecases/deps.rs
- crates/acdd-state/src/usecases/lifecycle.rs
- crates/acdd-state/src/usecases/commands/run_ops.rs
- crates/acdd-state/src/usecases/commands/start.rs
- crates/acdd-state/src/validate.rs
- crates/acdd-state/tests/state_integration/
extended_scope:
- path: crates/acdd-core/src/ids.rs
  reason: Support TaskRef::whole_spec() sentinel and pass local_task_refs to parse_task_dependency for task-to-whole-spec dependencies
- path: crates/acdd-state/src/store/sqlite/reader.rs
  reason: Support TaskRef::whole_spec() sentinel and pass local_task_refs to parse_task_dependency for task-to-whole-spec dependencies
- path: crates/acdd-state/src/usecases/commands/mod.rs
  reason: Support TaskRef::whole_spec() sentinel and pass local_task_refs to parse_task_dependency for task-to-whole-spec dependencies
- path: crates/acdd-state/src/usecases/mod.rs
  reason: Wire SpecCollectionConfig::patterns() in StateEngineConfig/build_path_policy
- path: crates/acdd-state/src/machine/schedule.rs
  reason: Allow compute_waves to break cleanly when remaining tasks are blocked on unsatisfied external dependencies
- path: crates/acdd-core/src/fs.rs
  reason: Add write_new_confined for collision-safe atomic draft creation and recursive list_git_tree_files supporting empty/root prefix for glob archive lookup
- path: crates/acdd-state/src/git_ops/snapshot.rs
  reason: Add write_new_confined for collision-safe atomic draft creation and recursive list_git_tree_files supporting empty/root prefix for glob archive lookup
tags:
- default
invariants:
- 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
- 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
acceptance_criteria:
- Existing single-path `acdd.yaml` configs deserialize and behave identically.
- '`paths: [docs/milestones, extensions/*/docs/milestones]` discovers all matching directories deterministically across `validate`, `start`, `draft`, dependency resolution, and index scanning.'
- '`acdd draft plans my-slug` with `prefixes: []` creates `docs/plans/my-slug.md` with `id: my-slug`.'
- 'A task in a milestone or plan can declare `depends_on: [<other_spec_id>]` or `[<ws>/<other_spec_id>]` in addition to `<other_spec_id>:<task_ref>`, and `validate` + runtime satisfaction enforce it cleanly.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: config-multi-paths
  title: Add path/paths schema, validation, default_specs plans prefixes:[], and confined resolve_dirs/matches_rel_path in acdd-core
  status: completed
  evidence: Extended SpecCollectionConfig with path/paths, default_specs plans prefixes:[], validation, and confined resolve_dirs/matches_rel_path/default_draft_dir in crates/acdd-core/src/config.rs
- subtask_ref: wire-callers-and-draft
  title: Wire canonical SpecCollectionConfig methods across acdd-state and acdd-index and support prefixless acdd draft
  status: completed
  evidence: Wired SpecCollectionConfig canonical methods across acdd-index scanner, acdd-state deps/lifecycle/run_ops/usecases, updated acdd.yaml plans.prefixes:[], and added prefixless draft
- subtask_ref: task-whole-spec-deps
  title: Support task-to-whole-spec dependencies in deps.rs and add table-driven contract tests
  status: completed
  evidence: Added TaskRef::whole_spec() and task-to-whole-spec dependency parsing, validation, cycle detection, and satisfaction in deps.rs/reader.rs/schedule.rs
receipt:
  run_id: roadmap-cli-and-spec-20261119040500-1689469-1
  node_cursor: root:T-01.deliver
  gate_id: deliver
  worker_id: codex-reviewer-t1-r5
  claim_revision: 1
  contract_revision: 5
  candidate_tree: c7f7f02f6c291ab220cd47ded09416f196b03b64
  committed_oid: f70c3e6e63cc018f75dd3c6de4120dfb4bfcd19f
  accepted_at: 2026-11-19T05:19:13.462Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
      - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
      verified:
      - 'INV-SINGLE-OWNER: SpecCollectionConfig in acdd-core is the single owner of spec collection path normalization, glob expansion, and confinement.'
      - 'INV-CONFINED-SPEC-GLOBS: Glob expansion never escapes workspace root or follows out-of-root symlinks.'
    review_summary: T-01 passed 5-round independent review (gpt-6.2-sol low) across all 6 contours and verified multi-path/glob SpecCollectionConfig, prefixless draft, and whole-spec external task deps.
```

### task: T-02

```yaml
task_ref: T-02
target: Implement read-only StateQuery::Roadmap in acdd-state computing active runs, spec topological waves, critical-path priority, scope overlaps, and task drill-down.
spec: |
  Add read-only `RoadmapReq` / `RoadmapResp` DTOs (`JsonSchema` + `Serialize` + `Deserialize`) in `crates/acdd-state/src/usecases/dto.rs` and handler `crates/acdd-state/src/usecases/roadmap.rs` wired into `StateQuery::Roadmap` in `queries.rs`:
  - Inputs: `spec_id: Option<String>`, `collection: Option<String>`, `ready_only: bool`, `active_only: bool`, `include_tasks: bool`.
  - Discovery & overlay:
    - Scan all workspace specs across every configured collection via `collect_workspace_specs` (build the full cross-collection DAG first so cross-collection blockers stay accurate, then apply `collection` display filter if supplied).
    - Overlay active runs from `list_all_runs(reader, Some(RunStatus::Active))` (worktree, `completed_tasks`/`total_tasks`, active gate cursors + claiming worker IDs, per-task live status).
    - Evaluate spec-level and cross-spec task-level `depends_on` (both `<spec_id>` and `<spec_id>:<task_ref>`, local or `linked_workspaces`) via `resolve_dep_target` / `parse_task_dependency` against planned specs, active runs, tombstones (`cleared` never satisfies), and committed archives.
  - Scheduling & priority:
    - Partition specs into `in_progress` (active `Run`), `waves` (`Wave 0` = ready planned specs with 0 unsatisfied spec-level dependencies; `Wave 1..N` = Kahn topological layers over unresolved spec dependencies), plus `cycles` diagnostics if any cycle exists.
    - Compute transitive `unblocks_specs` and `unblocks_tasks` fan-out for every spec and sort each wave descending by `(unblocks_specs, unblocks_tasks, spec_id)`.
    - For specs within the same active/ready wave, compute pairwise scope overlap via `acdd_core::path_policy::scopes_overlap` over effective write globs (union of task `scope:` globs, falling back to `doc.owners`), populating `parallel_safe: bool` and `scope_overlaps: Vec<String>`.
  - Task drill-down (`spec_id: Some(..)` or `include_tasks: true`):
    - Compute intra-spec task waves (`Wave 0..M`), per-task `status` (`planned | ready | in_progress | completed | cancelled`), `depends_on` with resolved satisfaction status (local, cross-spec task, and whole-spec deps), reverse `blocks` (`depended_on_by` both intra-spec and cross-spec), and intra-wave task `scope_overlaps`.
scope:
- crates/acdd-state/src/usecases/dto.rs
- crates/acdd-state/src/usecases/roadmap.rs
- crates/acdd-state/src/usecases/queries.rs
- crates/acdd-state/src/usecases/mod.rs
- crates/acdd-state/tests/state_integration/
extended_scope:
- path: crates/acdd-core/src/config.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-core/src/git.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/context.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/git_ops/mod.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/git_ops/snapshot.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/machine/schedule.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/profile/inherit.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/profile/mod.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/profile/schema.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/profile/shell_template.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/profile/validate.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/runner/command.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/runner/tests.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/types.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/commands/claim.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/commands/mod.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/commands/run_ops.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/commands/start.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/deps.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/run.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/submit/mod.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/usecases/verify.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: crates/acdd-state/src/validate.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
- path: tests/acdd/profiles_and_inheritance.rs
  reason: 'Restore prior R1-R24 approved T-02 extensions cleared during reopen/reset (Blackboard #101 remediation)'
depends_on:
- T-01
tags:
- default
invariants:
- 'INV-READ-ONLY-ROADMAP: Roadmap query never creates state.sqlite or opens write connections.'
- 'INV-SINGLE-OWNER: Reuse collect_workspace_specs, resolve_dep_target, parse_task_dependency, and scopes_overlap without duplicating graph/glob logic.'
- 'INV-DETERMINISTIC-WAVES: Output ordering across waves, specs, tasks, overlaps, and blockers is strictly deterministic.'
acceptance_criteria:
- Works cleanly when `.acdd/state.sqlite` does not yet exist (all specs reported from disk).
- Correctly ranks Wave 0 ready specs by transitive unblocks criticality and flags overlapping owners/scopes.
- Accurately resolves cross-collection dependencies (plan <-> milestone <-> task) and reverse `blocks` for both planned and active specs.
contract_revision: 1
status: completed
subtasks:
- subtask_ref: roadmap-dtos
  title: Define RoadmapReq, RoadmapResp, RoadmapWave, RoadmapSpecEntry, and RoadmapTaskEntry in dto.rs
  status: completed
  evidence: Added RoadmapReq (with include_archived, current_only), RoadmapResp (with archived_count, warnings, cycles), RoadmapWave, RoadmapSpecEntry, RoadmapTaskEntry, RoadmapDepEntry, RoadmapActiveGateEntry, RoadmapWorktreeCopy in dto.rs
- subtask_ref: roadmap-engine
  title: Implement read-only DAG, waves, critical-path unblocks, scope-overlap detection, and task drill-down in roadmap.rs
  status: completed
  evidence: Implemented read-only StateQuery::Roadmap in roadmap.rs with cross-worktree discovery, silent identical-digest dedup, divergence warnings, Kahn wave layering, unblocks criticality, scopes_overlap detection, and task drill-down; plus profile.resolved.yaml deduplication and per-target {scope_target} execution
- subtask_ref: roadmap-state-tests
  title: Add table-driven state_integration matrix tests for waves, overlays, overlaps, cross-collection deps, and missing state.sqlite
  status: completed
  evidence: Added seam_roadmap_query_waves_overlaps_overlay_and_missing_db_matrix and updated profile/runner tests; all 24 state_integration tests passed
receipt:
  run_id: roadmap-cli-and-spec-20261119040500-1689469-1
  node_cursor: root:T-02.deliver
  gate_id: deliver
  worker_id: cursor-t2-rev-r26
  claim_revision: 1
  contract_revision: 15
  candidate_tree: c9ea0ead6155fb38fb53547ba9f0c934cf28a05d
  committed_oid: 6e59cf341459308ada76727685661767f695da50
  accepted_at: 2026-11-19T12:16:38.825Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-READ-ONLY-ROADMAP: Roadmap query never creates state.sqlite or opens write connections.'
      - 'INV-SINGLE-OWNER: Reuse collect_workspace_specs, resolve_dep_target, parse_task_dependency, and scopes_overlap without duplicating graph/glob logic.'
      - 'INV-DETERMINISTIC-WAVES: Output ordering across waves, specs, tasks, overlaps, and blockers is strictly deterministic.'
      verified: []
    review_summary: Deliver T-02 after R26 review pass (cursor-t2-rev-r26)
```

### task: T-03

```yaml
task_ref: T-03
target: 'Expose `acdd roadmap [spec_id]` CLI, compact Markdown/JSON formatter, `acdd_state(action: roadmap)` MCP parity, integration tests, and v0.6.2 reference docs.'
spec: |
  Wire the user-facing CLI and MCP surfaces on top of `StateQuery::Roadmap`:
  - CLI in `src/cli/mod.rs`:
    `acdd roadmap [spec_id] [--collection <name>] [--ready] [--active] [--tasks]`
    supporting global `--format markdown|json` and `--root` / `--config` / `--state-db`.
  - Markdown formatter in `src/format/`:
    - Overview mode: compact header summary, `In Progress` section (progress, active nodes, worktree, `unblocks`), `Wave 0 — Ready Now` (numbered by critical path with `parallel-safe` or `⚠ scope overlap: ...`), `Wave 1..N — Blocked` (`waiting on: ...`), and optional inline task waves when `--tasks` is passed.
    - Detail mode (`acdd roadmap <spec_id>`): spec metadata, active run status if started, intra-spec task waves with per-task status, `depends_on` resolution states, `scope_overlaps`, and reverse `blocks`.
  - MCP parity in `src/mcp/tools/` (`acdd_state` `action: "roadmap"`) + `src/state_bridge/mod.rs` + `src/cli/guide.rs`.
  - Docs: update `docs/reference/cli.md`, `docs/reference/configuration.md`, `docs/reference/acdd.md`, `docs/reference/mcp-tools.md`, and `CHANGELOG.md` under `[0.6.2]`.
  - Integration tests in `tests/acdd/` verifying `acdd roadmap` CLI Markdown and JSON output over multi-collection workspaces.
scope:
- src/cli/mod.rs
- src/cli/guide.rs
- src/format/
- src/mcp/tools/
- src/state_bridge/mod.rs
- tests/acdd/
- docs/reference/cli.md
- docs/reference/configuration.md
- docs/reference/acdd.md
- docs/reference/mcp-tools.md
- CHANGELOG.md
extended_scope:
- path: src/mcp/tools.rs
  reason: MCP acdd_state roadmap action lives in canonical src/mcp/tools.rs per T-03 spec
depends_on:
- T-02
tags:
- default
invariants:
- 'INV-READ-ONLY-ROADMAP: CLI `acdd roadmap` works without existing `.acdd/state.sqlite` and without eager `--profile` validation when `--profile` is omitted.'
- 'INV-SINGLE-OWNER: CLI and MCP delegate directly to StateQuery::Roadmap with shared formatting.'
acceptance_criteria:
- '`acdd roadmap` and `acdd roadmap <spec_id>` produce clean Markdown and valid JSON outputs matching RoadmapResp.'
- '`acdd roadmap` on this repository renders `AC-005` in Wave 0, `AC-006`/`AC-007` in Wave 1 with overlap warning, `AC-008` in Wave 2, and `AC-009` in Wave 3.'
- Workspace strict clippy, full test suite, `acdd docs check`, and `acdd validate` pass cleanly.
contract_revision: 1
status: completed
subtasks:
- subtask_ref: cli-and-mcp-wiring
  title: Add `acdd roadmap` CLI subcommand, state_bridge dispatch, and `acdd_state` roadmap action
  status: completed
  evidence: CLI and MCP acdd roadmap wired to StateQuery::Roadmap
- subtask_ref: markdown-formatter
  title: Implement concise Markdown rendering for overview waves and single-spec task drill-down in src/format
  status: completed
  evidence: render_roadmap_markdown overview and detail modes implemented and verified
- subtask_ref: cli-tests-and-docs
  title: Add table-driven CLI tests in tests/acdd and update reference docs + CHANGELOG.md
  status: completed
  evidence: roadmap CLI contract tests in tests/acdd and reference docs updated
receipt:
  run_id: roadmap-cli-and-spec-20261119040500-1689469-1
  node_cursor: root:T-03.deliver
  gate_id: deliver
  worker_id: coordinator
  claim_revision: 1
  contract_revision: 2
  candidate_tree: c7c302808698b81e16978cc6bd3fa6bbc9c53078
  committed_oid: b8c7877988ba4cf58f3842654d72dce93c9c0860
  accepted_at: 2026-11-19T13:59:28.233Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-READ-ONLY-ROADMAP: CLI `acdd roadmap` works without existing `.acdd/state.sqlite` and without eager `--profile` validation when `--profile` is omitted.'
      - 'INV-SINGLE-OWNER: CLI and MCP delegate directly to StateQuery::Roadmap with shared formatting.'
      verified: []
```
