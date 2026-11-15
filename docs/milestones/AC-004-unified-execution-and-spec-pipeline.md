---
id: AC-004-unified-execution-and-spec-pipeline
title: Unified Task/Plan Execution, Workflow Inheritance, ADR-0021 Alignment, and Specify-Design Pipeline
doc_type: contract
status: active
depends_on: []
guidance:
  hitl: pro
owners:
- crates/acdd-tasks/src/engine/
- crates/acdd-tasks/src/tasks/
- crates/acdd-tasks/src/store/
- crates/acdd-tasks/src/milestones/
- src/cli/plan.rs
- src/cli/guide.rs
- docs/adr/0015-acdd-lifecycle-snapshots-and-proactive-defect-resolution.md
- docs/adr/0020-hierarchical-acdd-gates-and-milestone-orchestration.md
- docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md
- docs/architecture/
- README.md
- skills/acdd/
invariants:
- 'INV-ROLE-COMMAND-REJECTION-PARITY: An agent role paired with command pre-validation can return a valid structured rejection proof even when the observed command exited zero, failing closed only on false-positive pass when observed_pass is false, and preserving reuse_on_reject sessions across failed role retries.'
- 'INV-CANDIDATE-DRIFT-WARNING: candidate_fingerprint and git snapshots share the same .acdd/ runtime-path exclusions so runtime churn never triggers drift; non-runtime worktree drift during review/immutable gates emits a structured candidate_drift_warning with changed paths for LLM/reviewer inspection and enforces mandatory candidate rebinding, renewed verification when source/test paths drifted, and renewed independent_reviewer sign-off before issuing any terminal deliver/handoff receipt (reserving hard TASK_CANDIDATE_MISMATCH fail-closed strictly for corrupted/mismatched explicit commit receipts or unrecoverable HEAD divergence).'
- 'INV-COMMAND-FAILURE-FINDINGS-PROPAGATION: Failing autonomous command gates across waves, workflows, and plans emit structured failure diagnostics into findings and keep RoleObservations.findings synchronized so on_fail repair roles and reject_routing never receive null or stale diagnostics.'
- 'INV-ADR021-UNIFIED-TASK-MODEL: Under amended ADR 0021 (Variant A), Plan and Milestone share the single SQLite TasksStore and task_runner execution contour (with plan steps executing in-place via snapshots, full subtask lifecycle, blackboard, and resume, while Milestone adds worktree isolation, blueprint synthesis, red contract tests, and handoff commits); Workflow stays strictly stateless with respect to tasks and subtasks.'
- 'INV-WORKFLOW-PROFILE-INHERITANCE: Standalone and builtin workflows inherit roles (runner, models, prompt_template), commands, policies, and limits from the active repository profile without importing a tasks container, support append-only save: log audit trails without task rows, and keep local docs/workflows overrides higher priority.'
- 'INV-SPECIFY-DESIGN-TRACEABILITY: Builtin author-spec and amended blueprint schemas capture Goal, Non-goals, ambiguities, numbered AC-* criteria, C1/C2 boundary impact, ADR requirements, and SAD updates prior to code delivery without weakening core gate proofs.'
required_skills:
- acdd
- agent-instructions
related_plans: []
started_at: 2026-11-15T21:33:56+00:00
---

# AC-004: Unified Task/Plan Execution, Workflow Inheritance, ADR-0021 Alignment, and Specify-Design Pipeline

## Purpose & Architectural Direction

`AC-004` reconciles three coupled structural issues uncovered during live ACDD execution:

1. **Orchestrator cycle, rejection, and candidate-drift defects**:
   - `RoleObservations::execute` (`role.rs`) and `evaluate_root_command_gate` (`evidence_compiler.rs`) crash when a `role` + `command` + `scheme`/`review` gate rejects cleanly after a `0` command exit (`None` supplied proof, `passed == observed_pass`, hardcoded `passed = true`), and `RoleObservations.sessions` drops `reuse_on_reject` session continuity on self-retry failures.
   - Autonomous command gates in `waves.rs`, `workflow.rs`, and `plan.rs` discard diagnostics (`findings: None`) and leave `RoleObservations.findings` unsynchronized, starving repair roles and `gate_graph::failure_class`.
   - `verify_root_gate_with_options` (`root_gates.rs`) aborts with an `Err` in `command_proof(...)` on non-zero root commands instead of returning a structured `passed: false` observation payload.
   - `candidate_fingerprint` (`evidence_compiler.rs`) and `git_snapshot.rs` filter `.acdd/` runtime files inconsistently, while rigid `TASK_CANDIDATE_MISMATCH` checks in `plan.rs`, `waves.rs`, `root_gates.rs`, and `submit.rs` fatally abort loops on worktree edits during `review` or `immutable` gates instead of emitting structured warnings (`candidate_drift_warning`) and enforcing closed candidate rebinding + renewed verification/review prior to `deliver`/`handoff` receipt issuance.
2. **Amended `ADR 0021` (Variant A — Unified Stateful Execution Engine + Stateless Workflows)**:
   - Rather than maintaining an incomplete shadow orchestrator in `plan.rs` with ad-hoc Markdown-only step tracking and unreachable `Subtasks:`, `Plan` and `Milestone` unify onto the single `TasksStore` + `task_runner` engine (`in_place`/`snapshot` vs `worktree`/`commit` execution mode, keeping Plan free of mandatory `blueprint_synthesis`, red `contract`, and `handoff` commitments while giving plans genuine SQLite steps/subtasks, `resume`, `blackboard`, and bidirectional Markdown progress projection).
   - `Workflow` remains strictly **taskless** (zero `tasks` table rows, zero worktree spawning), inherits `roles`/`runner`/`models`/`commands`/`policies` from `.acdd/profile.yaml` (so builtin workflows do not hardcode `codex exec`), implements optional append-only `save: log` trace output, and embeds standard authoring workflows (`author-spec`, `author-plan`, `author-milestone`, `author-workflow`).
3. **Specify / Clarify & Design Integration (`PRD/AC-*`, `SAD`, `ADR`, `C4`)**:
   - Introduces builtin `author-spec` workflow (`Goal`, `Non-goals`, `ambiguities`, `AC-*`), extends `proofs.blueprint` (`ac_mapping`, `c2_boundary_impact`, `adr_required`, `sad_updates`), optionally projects `spec:` / `acceptance_criteria:` into `context_bundle`, and aligns `docs/` + `skills/`.

---

## Tasks in this milestone

### Frozen execution boundaries

The admitted DAG is upfront synthesis → {cycle fixes, profile inheritance} →
Plan execution → traceability. Wave 1 cycle/profile scopes remain disjoint.
Integrate and verify the cycle task's `sync_command_findings` before the profile
task consumes it; retain the existing synthesized seam matrix and DAG edges.

The Plan task reuses five existing owners across later waves, strictly for
`ExecutionSubjectKind` dispatch: `engine/mod.rs` for authority validation,
`engine/gates/submit.rs` and `milestones/mod.rs` for subject-specific delivery
parsing/receipt projection, and `engine/context.rs` and
`engine/lifecycle/root_gates.rs` for context and root-profile resolution (all
under `crates/acdd-tasks/src/`). These paths add no parallel Plan registry or
broader engine refactor. Cycle work lands first; traceability context projection
lands afterward. Public interface changes remain limited to admitted symbols.

### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: Amend ADR 0021 (Variant A) plus ADR 0015 and ADR 0020 (runtime exclusion parity and Option 1 candidate-drift warning + mandatory rebind/re-verify/re-review before receipt), freeze disjoint AC-004 file scopes, standardize diagnostic codes, and pre-register compiling runtime-red seam test modules in tests/acdd/orchestrator/mod.rs.
proof_policy: seam-test-first
contract_revision: 1
depends_on: []
scope:
- docs/milestones/AC-004-unified-execution-and-spec-pipeline.md
- docs/adr/0015-acdd-lifecycle-snapshots-and-proactive-defect-resolution.md
- docs/adr/0020-hierarchical-acdd-gates-and-milestone-orchestration.md
- docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md
- tests/acdd/orchestrator/mod.rs
- tests/acdd/orchestrator/cycle_contracts.rs
- tests/acdd/orchestrator/workflow_inheritance.rs
- tests/acdd/orchestrator/unified_plan_engine.rs
- tests/acdd/orchestrator/spec_traceability.rs
symbols: []
status: completed
subtasks:
- subtask_ref: amend-adr0021-variant-a
  title: 'Rewrite docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md to unify Plan/Milestone on TasksStore while keeping Workflow stateless with workspace profile inheritance and save: log'
  status: completed
  evidence: Amended docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md for Variant A; verified via adr_reconciliation_and_contract_matrix_contract
- subtask_ref: reconcile-adr0015-0020-immutable-rebind-protocol
  title: Amend ADR 0015 and ADR 0020 immutable/candidate semantics to share .acdd/ runtime exclusions, emit structured candidate_drift_warning on non-runtime drift, and mandate rebinding + renewed verification/review before terminal deliver/handoff receipts
  status: completed
  evidence: Amended docs/adr/0015 and 0020 for Option 1 runtime exclusions, candidate_drift_warning, mandatory rebind/re-verify/re-review; AC004-R1 reconciled
- subtask_ref: preregister-orchestrator-seam-tests
  title: Register cycle_contracts, workflow_inheritance, unified_plan_engine, and spec_traceability seam modules in tests/acdd/orchestrator/mod.rs with compiling runtime-red baseline checks
  status: completed
  evidence: Registered 4 seam test modules in tests/acdd/orchestrator/mod.rs; Clippy exit 0 and 9 runtime-red seams verified
- subtask_ref: publish-cross-task-contract-matrix
  title: Record cross-task schemas, SQLite Plan mode invariants, and workflow profile merge rules on task_blackboard
  status: completed
  evidence: Published cross-task contracts on task_blackboard (#207) and froze Plan subject-dispatch scopes in AC-004 manifest
receipt:
  delivery_gate: deliver
  candidate_gate: build
  contract_revision: 1
  passed_at: 2026-11-15T22:06:44.910817126+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/acdd/orchestrator/mod.rs]
        PASS rust-check-and-test::tests/acdd/orchestrator/mod.rs: exit=0 passed=1 failed=0 (1329 ms)
  review:
    review_proof:
      decision: pass
      contours:
        paths:
          applicable: true
          evidence: Repair changes only ADR 0020 within admitted scope. Cargo.toml, build.rs, .acdd/profile.yaml, and production files remain untouched.
        claims:
          applicable: true
          evidence: 'AC004-R1 is fully resolved. Both immutable-gate clauses now follow Option 1: runtime exclusions, candidate_drift_warning, mandatory rebinding, required renewed verification and independent review, and withheld terminal receipts. No remaining contradictions found across ADR 0015/0020/0021.'
        concurrency:
          applicable: true
          evidence: Wave 1 scopes, DAG ordering, and the five frozen Plan subject-dispatch owners are unchanged.
        project_isolation:
          applicable: true
          evidence: Changes remain confined to the owning worktree. Test files are unchanged. Temporary-directory count and names digest remained identical; zero new leaks.
        administration:
          applicable: true
          evidence: Independently reviewed candidate 9ad9e0fc9948a627d2f99cb6fda43833a2d22f1c. No files edited or task_submit executed. Git diff whitespace check passed.
      notes: cargo clippy --test acdd --all-features -- -D warnings passed (exit 0). cargo test --test acdd orchestrator::adr_reconciliation_and_contract_matrix_contract passed (1 passed, 0 failed; exit 0). Successor contracts remain unchanged and intentionally runtime-red.
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-ROLE-COMMAND-REJECTION-PARITY: An agent role paired with command pre-validation can return a valid structured rejection proof even when the observed command exited zero, failing closed only on false-positive pass when observed_pass is false, and preserving reuse_on_reject sessions across failed role retries.'
    - 'INV-CANDIDATE-DRIFT-WARNING: candidate_fingerprint and git snapshots share the same .acdd/ runtime-path exclusions so runtime churn never triggers drift; non-runtime worktree drift during review/immutable gates emits a structured candidate_drift_warning with changed paths for LLM/reviewer inspection and enforces mandatory candidate rebinding, renewed verification when source/test paths drifted, and renewed independent_reviewer sign-off before issuing any terminal deliver/handoff receipt (reserving hard TASK_CANDIDATE_MISMATCH fail-closed strictly for corrupted/mismatched explicit commit receipts or unrecoverable HEAD divergence).'
    - 'INV-COMMAND-FAILURE-FINDINGS-PROPAGATION: Failing autonomous command gates across waves, workflows, and plans emit structured failure diagnostics into findings and keep RoleObservations.findings synchronized so on_fail repair roles and reject_routing never receive null or stale diagnostics.'
    - 'INV-ADR021-UNIFIED-TASK-MODEL: Under amended ADR 0021 (Variant A), Plan and Milestone share the single SQLite TasksStore and task_runner execution contour (with plan steps executing in-place via snapshots, full subtask lifecycle, blackboard, and resume, while Milestone adds worktree isolation, blueprint synthesis, red contract tests, and handoff commits); Workflow stays strictly stateless with respect to tasks and subtasks.'
    - 'INV-WORKFLOW-PROFILE-INHERITANCE: Standalone and builtin workflows inherit roles (runner, models, prompt_template), commands, policies, and limits from the active repository profile without importing a tasks container, support append-only save: log audit trails without task rows, and keep local docs/workflows overrides higher priority.'
    - 'INV-SPECIFY-DESIGN-TRACEABILITY: Builtin author-spec and amended blueprint schemas capture Goal, Non-goals, ambiguities, numbered AC-* criteria, C1/C2 boundary impact, ADR requirements, and SAD updates prior to code delivery without weakening core gate proofs.'
    architectural_notes: []
    review_summary:
      decision: pass
      contours:
        administration: accepted
        claims: accepted
        concurrency: accepted
        paths: accepted
        project_isolation: accepted
```

### task: fix-role-command-rejection-and-findings

```yaml
task_ref: fix-role-command-rejection-and-findings
target: Fix role + command + scheme/review rejection handling in role.rs and evidence_compiler.rs (including reuse_on_reject session retention), emit structured failure findings from waves.rs command/verification gates and sync RoleObservations.findings, return structured passed=false payloads from verify_root_gate_with_options, unify .acdd/ runtime exclusions between candidate_fingerprint and git_snapshot.rs while turning non-runtime review/immutable drift into structured candidate_drift_warning + mandatory candidate rebinding/re-verify/re-review prior to deliver receipt (preserving fail-closed only on corrupted/mismatched explicit commits), migrate existing wave/lifecycle/snapshot drift expectations, and prove all contracts in cycle_contracts.rs.
proof_policy: seam-test-first
contract_revision: 1
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
scope:
- crates/acdd-tasks/src/engine/mod.rs
- crates/acdd-tasks/src/engine/git_snapshot.rs
- crates/acdd-tasks/src/engine/gates/submit.rs
- crates/acdd-tasks/src/engine/orchestrator/role.rs
- crates/acdd-tasks/src/engine/evidence_compiler.rs
- crates/acdd-tasks/src/engine/orchestrator/waves.rs
- crates/acdd-tasks/src/engine/lifecycle/root_gates.rs
- tests/acdd/orchestrator/cycle_contracts.rs
- tests/acdd/orchestrator/waves/snapshots.rs
- tests/acdd/orchestrator/lifecycle.rs
- tests/acdd/tasks/lifecycle/snapshots.rs
symbols:
- fn:sync_command_findings
status: completed
subtasks:
- subtask_ref: role-command-rejection-and-session-retention
  title: Pass rejection proof and passed flag into evaluate_root_command_gate, forbid only false-positive passes when observed_pass is false, and keep reuse_on_reject sessions on failure
  status: completed
  evidence: Added decision-aware evaluate_root_command_gate_decision, RoleObservations reuse_on_reject retention, and false-positive pass rejection; verified in orchestrator::cycle_contracts (11 passed)
- subtask_ref: sync-findings-and-root-verify-failure
  title: Populate structured failure findings in waves.rs, sync RoleObservations.findings across command gates, and return structured passed=false JSON in verify_root_gate_with_options
  status: completed
  evidence: Added pub(crate) fn sync_command_findings with trace diagnostics in waves.rs and failed observation return in verify_root_gate_with_options; verified in orchestrator::cycle_contracts
- subtask_ref: relax-candidate-drift-and-unify-runtime-filter
  title: Share .acdd/ runtime exclusions between git_snapshot.rs and candidate_fingerprint, convert non-runtime review/immutable drift into candidate_drift_warning + mandatory rebind/re-verify/re-review before deliver receipt across waves/root_gates/submit, and migrate existing drift assertions in waves/snapshots.rs, lifecycle.rs, and tasks/lifecycle/snapshots.rs
  status: completed
  evidence: Unified .acdd runtime exclusions between git_snapshot.rs and candidate_fingerprint, implemented Option 1 candidate_drift_warning + durable rebind/re-verify/re-review across submit/root_gates/waves, and migrated waves/snapshots (7), lifecycle (35), tasks/lifecycle/snapshots (3)
receipt:
  delivery_gate: deliver
  candidate_gate: build
  contract_revision: 1
  passed_at: 2026-11-15T23:29:50.103470750+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/acdd/orchestrator/cycle_contracts.rs]
        PASS rust-check-and-test::tests/acdd/orchestrator/cycle_contracts.rs: exit=0 passed=1 failed=0 (7342 ms)
  review:
    review_proof:
      decision: pass
      contours:
        paths:
          applicable: true
          evidence: Diff against c941ae072 contains exactly the 11 admitted files. Cargo.toml, build.rs, .acdd/profile.yaml, workflow.rs and all other files are untouched.
        claims:
          applicable: true
          evidence: 'All five findings from #221 are resolved: execution watermarks, commandless recovery, native snapshot compatibility, scoped runtime exclusions and duplicate capture removal. No blocking regressions or weakened test invariants found.'
        concurrency:
          applicable: true
          evidence: Drift/rebind watermarks and transitions commit atomically under SQLite IMMEDIATE transactions. Root verification rejects executions at or below the applicable cutoff.
        project_isolation:
          applicable: true
          evidence: 'Scoped and unscoped snapshots share runtime filtering while retaining authority files. Temporary-directory sets remained identical: 5177 before and after, zero new leaks.'
        administration:
          applicable: true
          evidence: Reviewed claim revision 19; content matches candidate a99c29aa080b7aedc573d8c88e42f6ad006fdc6b. No files edited or task_submit executed. Diff hash and Git status remained unchanged; whitespace check passed.
      notes: 'All seven requested checks passed serially: both strict Clippy commands and test counts 91, 14, 7, 35 and 3. Total: 150 passed, zero failures, zero warnings.'
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-ROLE-COMMAND-REJECTION-PARITY: An agent role paired with command pre-validation can return a valid structured rejection proof even when the observed command exited zero, failing closed only on false-positive pass when observed_pass is false, and preserving reuse_on_reject sessions across failed role retries.'
    - 'INV-CANDIDATE-DRIFT-WARNING: candidate_fingerprint and git snapshots share the same .acdd/ runtime-path exclusions so runtime churn never triggers drift; non-runtime worktree drift during review/immutable gates emits a structured candidate_drift_warning with changed paths for LLM/reviewer inspection and enforces mandatory candidate rebinding, renewed verification when source/test paths drifted, and renewed independent_reviewer sign-off before issuing any terminal deliver/handoff receipt (reserving hard TASK_CANDIDATE_MISMATCH fail-closed strictly for corrupted/mismatched explicit commit receipts or unrecoverable HEAD divergence).'
    - 'INV-COMMAND-FAILURE-FINDINGS-PROPAGATION: Failing autonomous command gates across waves, workflows, and plans emit structured failure diagnostics into findings and keep RoleObservations.findings synchronized so on_fail repair roles and reject_routing never receive null or stale diagnostics.'
    - 'INV-ADR021-UNIFIED-TASK-MODEL: Under amended ADR 0021 (Variant A), Plan and Milestone share the single SQLite TasksStore and task_runner execution contour (with plan steps executing in-place via snapshots, full subtask lifecycle, blackboard, and resume, while Milestone adds worktree isolation, blueprint synthesis, red contract tests, and handoff commits); Workflow stays strictly stateless with respect to tasks and subtasks.'
    - 'INV-WORKFLOW-PROFILE-INHERITANCE: Standalone and builtin workflows inherit roles (runner, models, prompt_template), commands, policies, and limits from the active repository profile without importing a tasks container, support append-only save: log audit trails without task rows, and keep local docs/workflows overrides higher priority.'
    - 'INV-SPECIFY-DESIGN-TRACEABILITY: Builtin author-spec and amended blueprint schemas capture Goal, Non-goals, ambiguities, numbered AC-* criteria, C1/C2 boundary impact, ADR requirements, and SAD updates prior to code delivery without weakening core gate proofs.'
    architectural_notes: []
    review_summary:
      decision: pass
      contours:
        administration: accepted
        claims: accepted
        concurrency: accepted
        paths: accepted
        project_isolation: accepted
```

### task: builtin-profile-presets-and-workflow-inheritance

```yaml
task_ref: builtin-profile-presets-and-workflow-inheritance
target: "Replace boolean inherit_defaults with explicit inherit targets (none | default | workspace | <path>) so standalone/builtin workflows and plan overlays can explicitly write inherit: workspace to pull workspace resources (roles, runner, models, prompt_template, commands, policies, limits, guidance) without importing milestone tasks/gates (rejecting self-referential workspace inheritance on the root workspace profile); embed builtin plan preset and author-* workflows (author-spec, author-plan, author-milestone, author-workflow, code-quality-and-polish), propagate command findings and save: log traces in workflow.rs, extend proofs.blueprint, and prove contracts in workflow_inheritance.rs."
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
scope:
  - crates/acdd-tasks/src/tasks/profile/mod.rs
  - crates/acdd-tasks/src/tasks/profile/load.rs
  - crates/acdd-tasks/src/tasks/profile/normalize.rs
  - crates/acdd-tasks/src/tasks/profile/validate.rs
  - crates/acdd-tasks/src/tasks/acdd.profile.yaml
  - crates/acdd-tasks/src/engine/orchestrator/workflow.rs
  - docs/workflows/README.md
  - docs/workflows/author-milestone.workflow.yaml
  - docs/workflows/author-plan.workflow.yaml
  - docs/workflows/author-workflow.workflow.yaml
  - tests/acdd/orchestrator/workflow_inheritance.rs
symbols:
  - enum:InheritTarget
  - fn:resolve_effective_workflow_profile
status: ready
subtasks:
  - subtask_ref: explicit-inherit-target-and-workspace-resources
    title: "Implement InheritTarget (none | default | workspace | <path>) inheriting workspace resources without milestone tasks/gates, rejecting cyclic or root-workspace self-inheritance, and honoring --profile overrides"
  - subtask_ref: builtin-workflows-save-log-and-findings
    title: "Embed author-spec/plan/milestone/workflow/polish builtins, fix workflow.rs failure findings + save: log audit output, and extend proofs.blueprint schema"
```

### task: unify-plan-execution-on-task-store

```yaml
task_ref: unify-plan-execution-on-task-store
target: "Unify Plan execution (plan.rs) onto TasksStore and task_runner semantics under amended ADR 0021 (in-place/snapshot mode with SQLite step and subtask tracking, resume, findings, placeholder expansion, immutable predecessor guard, and nested ### heading preservation in complete_source); update `plan clear|archive` in src/cli/plan.rs to prune both snapshot/trace state and SQLite plan task/subtask rows while keeping CLI parity and migrating obsolete ADR-021 v1 fixtures to real plan subtask lifecycle tests in unified_plan_engine.rs."
proof_policy: seam-test-first
contract_revision: 2
depends_on:
  - fix-role-command-rejection-and-findings
  - builtin-profile-presets-and-workflow-inheritance
scope:
  - crates/acdd-tasks/src/engine/mod.rs
  - crates/acdd-tasks/src/engine/gates/submit.rs
  - crates/acdd-tasks/src/milestones/mod.rs
  - crates/acdd-tasks/src/engine/context.rs
  - crates/acdd-tasks/src/engine/lifecycle/root_gates.rs
  - crates/acdd-tasks/src/engine/orchestrator/plan.rs
  - crates/acdd-tasks/src/engine/orchestrator/task_runner.rs
  - crates/acdd-tasks/src/store/mod.rs
  - crates/acdd-tasks/src/store/lifecycle.rs
  - crates/acdd-tasks/src/store/subtasks.rs
  - crates/acdd-tasks/src/milestones/synthesis.rs
  - src/cli/plan.rs
  - tests/acdd/orchestrator/waves/plans.rs
  - tests/acdd/orchestrator/unified_plan_engine.rs
symbols:
  - enum:ExecutionSubjectKind
status: ready
subtasks:
  - subtask_ref: sqlite-backed-plan-steps-and-subtasks
    title: "Sync Plan steps/subtasks into TasksStore under in-place snapshot mode with resume, subtask transitions, bidirectional Markdown status/evidence projection, and SQLite cleanup in src/cli/plan.rs clear|archive"
  - subtask_ref: plan-execution-invariants-and-tests
    title: "Emit candidate_drift_warning with mandatory rebind/re-verify/re-review before terminal snapshot delivery on immutable plan drift, fix immutable predecessor none-guard, command placeholder expansion, and nested heading preservation while migrating obsolete pre-completed subtask fixtures to real in-place subtask lifecycle tests"
```

### task: spec-traceability-docs-and-skills-reconciliation

```yaml
task_ref: spec-traceability-docs-and-skills-reconciliation
target: "Add optional spec and acceptance_criteria manifest/task fields with scoped AC-* projection into context_bundle, reconcile CLI guide (src/cli/guide.rs) and docs (AGENTS.md, architecture, milestones, reference/acdd.md, reference/cli.md) and skills (acdd, contract-planning) with C1/C2 + live C3/C4 graph, Option 1 immutable-rebind semantics, and S/M/L Specify-Design cadence, and verify all repository test gates."
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - unify-plan-execution-on-task-store
scope:
  - crates/acdd-tasks/src/tasks/mod.rs
  - crates/acdd-tasks/src/engine/context.rs
  - src/cli/guide.rs
  - AGENTS.md
  - README.md
  - docs/AGENTS.md
  - docs/architecture/README.md
  - docs/architecture/specification-and-design-lifecycle.md
  - docs/milestones/AGENTS.md
  - docs/reference/acdd.md
  - docs/reference/cli.md
  - docs/reference/tasks.md
  - skills/AGENTS.md
  - skills/acdd/SKILL.md
  - skills/acdd/references/acdd-profile-examples/README.md
  - skills/acdd/references/acdd-profile-examples/docs-refactor-and-reconcile.workflow.yaml
  - skills/acdd/references/acdd-profile-examples/repo-polish.workflow.yaml
  - skills/acdd/references/acdd-profile-examples/in-place-refactor.plan.yaml
  - skills/contract-planning/SKILL.md
  - tests/acdd/orchestrator/spec_traceability.rs
symbols: []
status: ready
subtasks:
  - subtask_ref: optional-spec-ac-context-projection
    title: "Support optional spec and acceptance_criteria fields in Milestone/TaskSpec with scoped AC-* injection into context_bundle"
  - subtask_ref: docs-skills-and-suite-verification
    title: "Reconcile documentation and skills with ADR 0021 Variant A and Specify/Design paradigms and pass all TESTS.md checks"
```
