---
title: "Workflow Contract Parity, Orchestrator Logic Fixes, and Mock Elimination"
doc_type: plan
purpose: >-
  Fix all verified prompt-to-validator contract mismatches, task/root orchestrator
  and boundary bugs, shipped broken workflow definitions, and validator-cheating
  test mocks across crates/forge-tasks, src/mcp/tools.rs, docs/workflows, and
  tests/acdd.
depends_on: []
---

# Workflow Contract Parity, Orchestrator Logic Fixes, and Mock Elimination

This plan consolidates all verified findings from the multi-agent deep audit (orchestrator, gate graph, proof compilation, path/worktree/snapshot boundaries, shipped workflow definitions, and `tests/acdd/` mocks/coverage gaps) into an executable in-place remediation plan aligned with [`author-plan.workflow.yaml`](../workflows/author-plan.workflow.yaml).

**Root-Cause Summary (Why Failures Escaped Tests)**

- **Validator-cheating mock runners and evidence builders**: [`runner.py`](../../tests/acdd/fixtures/workflows/runner.py), [`wave-role.py`](../../tests/acdd/fixtures/workflows/wave-role.py), [`role_fixture.py`](../../tests/acdd/orchestrator/lifecycle/role_fixture.py), [`unobserved_evidence`](../../tests/acdd/support/fixtures.rs), and [`TaskHarness`](../../tests/acdd/tasks/harness.rs) hardcode valid proof envelopes (`{"none_proof": {}}`, gate-name-switched `scheme_proof`, pre-canned `review-proof.json`) instead of deriving proof shapes from production `expected_evidence_template` / `expected_response` and `gate_definition.proof`, while [`custom_profiles.rs`](../../tests/acdd/tasks/guidance/custom_profiles.rs) explicitly asserts an invalid empty `{"scheme_proof": {}}` template.
- **Duplicate unshared prompt producers**: [`context_bundle`](../../crates/forge-tasks/src/engine/context.rs) and [`execute`](../../crates/forge-tasks/src/engine/orchestrator/role.rs) construct expected-proof hints independently of [`validate_gate_proof`](../../crates/forge-tasks/src/tasks/gates.rs) and [`acdd.default.yaml`](../../crates/forge-tasks/src/tasks/acdd.default.yaml), letting incompatible templates (`{}`, string `contours`, omitted `notes`, empty `scheme_proof`) survive on default gates.
- **Divergent root vs task execution engines**: Root workflows (`workflow.rs`, `waves.rs`, `plan.rs`) and [execute](../../crates/forge-tasks/src/engine/orchestrator/role.rs) reimplement retry budgets, command observation, placeholder expansion, persistence/immutability, and barrier checks inconsistently with the task engine (`gate_graph.rs`, `evidence_compiler.rs`, `root_gates.rs`), and static authoring tests in [`workflows.rs`](../../tests/acdd/harness/workflows.rs) never execute authoring workflows end-to-end.


**Execution authority and safety**

Use [ADR 0020](../adr/0020-hierarchical-acdd-gates-and-milestone-orchestration.md) for shared gates and observed proofs, [ADR 0021](../adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md) for taskless workflows and in-place plans, [ADR 0001](../adr/0001-dual-surface-parity.md) for transport parity, [ADR 0005](../adr/0005-filesystem-root-guard-and-symlink-safety.md) for confinement, and [ADR 0018](../adr/0018-layer-dependency-boundaries-and-core-leaf-isolation.md) for owner boundaries. Reuse existing gate routing, command observation, schema validation, path helpers, and snapshot machinery. Keep pure proof projection in the existing task model/validation owner and process, store, and Git operations in the engine. Preserve single-pass path checks and existing response/trace limits; this plan adds no performance promise or benchmark.

The planning add-on names ADR 0024, but that ADR is absent from the current repository inventory. Treat it as an authority coverage gap, not permission to invent lifecycle policy. Keep lifecycle clear/archive redesign outside this plan. Preserve all audited P0/P1/P2 findings and their priorities; the source plan contains no individual severity labels, so the ledger below maps findings without assigning new severities.

Finalize this file in place. It currently contains only ready steps and no completion evidence. Execute it later with the Plan model: explicit in-place opt-in, task-container gates, snapshots, preserved HEAD, and no automatic commit, synthetic task IDs, milestone handoff, or new plan copy. Capture the initial dirty/untracked state and preserve unrelated files, including the existing untracked milestone. The runner owns status/evidence updates to this plan; role processes keep its source unchanged. Check scope before every mutation, preserve snapshot exclusions, and keep reviewed candidates immutable. Implement changes within each listed scope; admit any necessary additional owner file explicitly before editing it. Keep Cargo.toml, build.rs, and .forge/acdd/profile.yaml outside implementation scope.

The current author-plan invocation admits only docs/plans/ writes. The implementation scopes below describe future execution authority; they do not authorize source edits during synthesis. This taskless gate has no milestone/task blackboard subject; record the architectural blueprint and its boundaries here without manufacturing a task identity.

**Finding preservation ledger**

| Original finding or missing coverage | Implementation owner |
| --- | --- |
| Empty none/seam/schema hints, string review contours, catalog overrides, rejected review notes | Step 1 |
| Validator-cheating Python runners, unobserved_evidence, TaskHarness, empty custom schema assertion | Steps 1–2 |
| Missing seam.integrate auto-proof and reopen proof | Step 3 |
| Mechanical failures leave claims active, stale failed runs, self-reject without accepted identity | Step 4 |
| Omitted retry limit mismatch and passing visits consume repair budgets | Step 5 |
| Fenced stdout, multiple JSON rejection, decision/proof disagreement, schema findings | Step 6 |
| Unobserved root command/contract/seam proof, false-green runtime-red, require_counts/catalog bypass | Step 7 |
| Role+command placeholders and stale artifact fields across passes | Step 8 |
| Bypassed pre-task cursors deadlock; simplified blueprint test misses real nested flow | Step 9 |
| Native milestone save.pre/save.post/immutable parity | Step 10 |
| Symlink-root workflow discovery | Step 11 |
| Configured tasks_db omitted from plan snapshot exclusions | Step 12 |
| Empty model/reasoning flags, driver doc/code disagreement, real reuse_on_reject semantics | Steps 6 and 13 |
| MCP canonical root/client worktree mismatch with CLI; verify the audited suspicion through existing request routing before prescribing a repair | Step 14 |
| Broken shipped persistence/retry/runner definitions; count-free ordered fixture | Steps 7 and 15 |
| Missing executed author-milestone/author-plan/author-workflow/code-quality seams | Step 16 |

**Shared acceptance and verification**

Each step includes its own production seam and sensitivity mutation. Add separate, descriptively named tests for distinct workflows; extend a case table only for variations of one contract. Keep assertions in tests, shared setup in existing domain harnesses with at least two callers, and production state/validation real. External role processes may substitute for an unavailable LLM; their proof envelopes and state transitions still exercise Forge. Consume production templates and schemas without compensating for invalid templates. A malformed template must fail the seam, not trigger a fallback proof builder.

Use hermetic temporary workspaces, local commands, and existing fixtures. The harness owns cleanup of files, child processes, SQLite handles, and snapshot refs. No live provider/account lane is required. Keep malformed-input cases explicit and separate from happy-path builders. Contract-red tests compile and fail at runtime with the configured signature; compiler failures and false-green commands remain invalid evidence.

Run the named focused seams with cargo test --test acdd <domain-filter> and member-local checks with cargo test -p forge-tasks --lib <filter>, plus strict cargo clippy --all-targets --all-features -- -D warnings for implementation changes, as required by [TESTS.md](../../TESTS.md). Use tests/AGENTS.md and affected owner instructions for placement. The final step runs repository verification. This synthesis pass validates plan syntax, links, scopes, dependency topology, and content preservation; it does not claim the implementation tests pass.

## 1. Unify Proof Templates and Review Catalog Parity

- **Target**: Make task and root role proof guidance project the same authoritative schema accepted by the gate validator.
- **Scope**: ["crates/forge-tasks/src/tasks/gates.rs","crates/forge-tasks/src/tasks/acdd.default.yaml","crates/forge-tasks/src/engine/context.rs","crates/forge-tasks/src/engine/orchestrator/role.rs","tests/acdd/tasks/guidance/custom_profiles.rs","docs/reference/acdd.md","docs/reference/tasks.md","docs/reference/profiles.md"]
- **Depends On**: []
- **Status**: ready

- Use one canonical proof-template projector from both [context_bundle](../../crates/forge-tasks/src/engine/context.rs) and [expected_proof_hint](../../crates/forge-tasks/src/engine/orchestrator/role.rs). Resolve profile.proofs overrides before choosing the wrapper/schema; preserve built-in, inline Scheme, Named, and ResolvedNamed semantics without a second schema registry or validator.
- Emit none_proof: {} for None/unspecified proof under the default catalog. Emit seam_test_proof with exit_code, seam_test_refs, tests_passed, and tests_failed. Emit review contours as an object keyed by configured contour with applicable/evidence fields. Project required schema members recursively with typed placeholders instead of empty scheme_proof or prose-only shape hints; materialize semantic placeholders before validation.
- Add optional serde-defaulted notes to [ReviewProof](../../crates/forge-tasks/src/tasks/gates.rs), preserving deny_unknown_fields and the existing review catalog/documentation contract. Keep strict rejection of unrelated fields.
- Replace the empty scheme_proof assertion in custom_profiles.rs with required, typed members and update affected proof guidance documentation.
- **Acceptance**: Through claim/context production seams and root role context, materialize templates for every default-profile gate and validate with [validate_gate_proof](../../crates/forge-tasks/src/tasks/gates.rs). Include configured review contours and notes, inline/named/resolved schemas, profile overrides, and absent proof. Assert parity of the two consumers. Removing a required key, replacing contours with a string, or restoring {} for none/seam must fail the relevant test. Run cargo test --test acdd tasks::guidance and the focused member template tests.

## 2. Replace Validator-Cheating Role and Evidence Fixtures

- **Target**: Make test role/evidence producers consume production guidance and expose template defects instead of hiding them.
- **Scope**: ["tests/acdd/fixtures/workflows/","tests/acdd/orchestrator/lifecycle/role_fixture.py","tests/acdd/orchestrator/lifecycle/harness.rs","tests/acdd/support/fixtures.rs","tests/acdd/tasks/harness.rs","tests/acdd/harness/workflows.rs","tests/acdd/orchestrator/lifecycle.rs","tests/acdd/orchestrator/waves/harness.rs","docs/architecture/test-harness.md"]
- **Depends On**: ["Unify Proof Templates and Review Catalog Parity"]
- **Status**: ready

- Replace gate-name/proof-wrapper switches in runner.py and wave-role.py, pre-canned review-proof.json injection in role_fixture.py, and hand-built envelopes in [unobserved_evidence](../../tests/acdd/support/fixtures.rs) and [TaskHarness](../../tests/acdd/tasks/harness.rs) with materialization of expected_response/expected_evidence_template and the resolved gate_definition.proof schema.
- Preserve explicit scenario controls for reject, malformed decision, stale binding, and timeout. Keep semantic scenario content separate from proof shape. Reject missing/invalid guidance instead of inferring a valid wrapper from the gate name or maintaining a copied built-in catalog. Share only demonstrated duplicate fixture logic, within existing fixture/harness owners.
- Preserve verify plus zero-argument submit for mechanical command/contract/seam gates; the fixture supplies no invented command observations or counts. Preserve real Forge identity, independence, store writes, binding checks, and failure routing.
- **Acceptance**: Execute existing lifecycle and wave role tests with production guidance, then corrupt the emitted template (missing required key/wrong wrapper) and assert production rejection. Rename a gate without changing its proof contract and assert the fixture still works. Run cargo test --test acdd orchestrator::lifecycle and cargo test --test acdd orchestrator::waves. Retain all existing malformed-input scenarios; document the external-process substitution boundary.

## 3. Compile Observed Task Seam Proofs

- **Target**: Allow seam.integrate to pass verify and zero-argument submit using durable per-test command observations.
- **Scope**: ["crates/forge-tasks/src/engine/evidence_compiler.rs","crates/forge-tasks/src/tasks/gates.rs","crates/forge-tasks/src/milestones/command_gate.rs","tests/acdd/runner/command_gate_proofs.rs","tests/acdd/runner/output_contracts.rs","docs/reference/tasks.md","docs/reference/profiles.md"]
- **Depends On**: ["Replace Validator-Cheating Role and Evidence Fixtures"]
- **Status**: ready

- Implement the SeamTest branch in [compile_evidence_for_task](../../crates/forge-tasks/src/engine/evidence_compiler.rs) using existing claim/candidate-bound traces and persisted command facts. Extend [validate_observed_seam_test](../../crates/forge-tasks/src/tasks/gates.rs), which currently checks reference membership, to verify supplied references and observed counts together; preserve registered command identity, timeout, freshness, claim revision, and candidate checks.
- Build seam_test_proof from actually observed per-test results. Reject aggregate-only output, stale or foreign refs, missing counts, failed tests, and a supplied proof inconsistent with the observation. Keep the current store representation unless the existing observation seam demonstrably lacks a required fact; admit any necessary storage extension before implementation.
- **Acceptance**: Add a positive seam.integrate verify/submit pass in command_gate_proofs.rs, reopen the store and prove accepted transition/evidence durability. Add input variations for missing/foreign per-test refs and forged counts using real command observation. Restoring the unconditional auto-proof-unavailable branch or accepting aggregate-only traces must fail the tests. Run cargo test --test acdd runner::command_gate_proofs and runner::output_contracts.

## 4. Route Mechanical Task Failures and Recover Rejected Sessions

- **Target**: Route failed task verification through configured fail transitions and recover first-attempt self-rejection without inventing an accepted identity.
- **Scope**: ["crates/forge-tasks/src/engine/evidence_compiler.rs","crates/forge-tasks/src/engine/orchestrator/task_runner.rs","crates/forge-tasks/src/engine/workspaces.rs","tests/acdd/orchestrator/lifecycle.rs","tests/acdd/orchestrator/lifecycle/harness.rs","tests/acdd/tasks/lifecycle.rs","docs/reference/tasks.md"]
- **Depends On**: ["Compile Observed Task Seam Proofs"]
- **Status**: ready

- In [run_task_lifecycle](../../crates/forge-tasks/src/engine/orchestrator/task_runner.rs), turn a failed observed mechanical gate into a typed reject through the existing router instead of propagating submit_zero_arg compilation failure with an open claim. Preserve infrastructure errors as explicit errors; record failed observations before routing.
- Reuse only valid fresh passing command evidence. Ensure a cached failed execution cannot prevent a recoverable rerun after rejection/new claim; keep candidate and revision binding intact.
- Align [rejected_workflow_context](../../crates/forge-tasks/src/engine/workspaces.rs) and [handle_gate_rejection](../../crates/forge-tasks/src/engine/orchestrator/task_runner.rs): a self-targeted rejection with no previously passed target has no resumable accepted worker, returns Ok(None), retains findings, and obtains a fresh runner session. Preserve fail-closed behavior for a purported accepted worker whose stored session is missing or inconsistent.
- **Acceptance**: Separate tests prove command/contract failure -> configured repair -> new passing observation -> completion, including process restart/store reopen; first-attempt self-reject recovery covers build, contract, polish, and seam.integrate where configured. A real accepted predecessor resumes its identity. Leaving the claim active, reusing failed traces, or requiring a prior pass for every rejection must break these tests. Run cargo test --test acdd orchestrator::lifecycle and tasks::lifecycle.

## 5. Align Root Retry Budgets with Gate Routing

- **Target**: Give standalone, plan, and milestone root execution consistent failure-based retry budgets.
- **Scope**: ["crates/forge-tasks/src/engine/gate_graph.rs","crates/forge-tasks/src/engine/orchestrator/workflow.rs","crates/forge-tasks/src/engine/orchestrator/waves.rs","tests/acdd/fixtures/workflows/","tests/acdd/harness/workflows.rs","tests/acdd/orchestrator/waves.rs","docs/reference/acdd.md","docs/reference/profiles.md"]
- **Depends On**: ["Replace Validator-Cheating Role and Evidence Fixtures"]
- **Status**: ready

- Reconcile [resolve_fail_target](../../crates/forge-tasks/src/engine/gate_graph.rs) with [run_gate_graph](../../crates/forge-tasks/src/engine/orchestrator/workflow.rs). Preserve the existing router meaning that omitted max_attempts is unbounded; remove the root-only implicit limit of one. Explicit max_attempts bounds failures, while passing visits do not consume the failure budget. Keep route selection centralized.
- Keep execution visit numbers available for diagnostics separately from failure counters. Reconstruct equivalent counters on durable milestone replay in waves.rs. Keep timeout/cancellation effective when a user declares an unbounded repair loop.
- **Acceptance**: Execute omitted-limit reject/repair/pass, explicit finite exhaustion, and verify-pass -> later review-reject -> polish -> verify repair cases. Assert exact visits, failures, terminal errors, and no skipped repair. A finite fixture terminates even for the unbounded configuration. Restoring unwrap_or(1) or charging a passing visit must fail the tests. Run focused workflow retry tests and cargo test --test acdd orchestrator::waves.

## 6. Validate Role Decisions and Gate Driver Semantics

- **Target**: Decode one structured role decision and keep its proof and routing decision consistent.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/role.rs","crates/forge-tasks/src/tasks/profile/mod.rs","tests/acdd/fixtures/workflows/","tests/acdd/harness/workflows.rs","tests/acdd/orchestrator/lifecycle.rs","docs/reference/acdd.md","docs/reference/profiles.md"]
- **Depends On**: ["Align Root Retry Budgets with Gate Routing"]
- **Status**: ready

- Decode either a bare JSON object or one optional complete outer Markdown code fence in [execute](../../crates/forge-tasks/src/engine/orchestrator/role.rs). Preserve the stdout bound, session/gate identity checks, strict fields, and process completion rules. Reject trailing prose, extra objects, incomplete fences, and ambiguous framing.
- Require scheme_proof.decision to match the outer decision when present. On rejection, accept schema-declared structured findings from scheme_proof when top-level findings is omitted, including authoring review step_findings; normalize to the existing router finding representation. Reject absent/empty findings and conflicting duplicated findings rather than choosing silently.
- Preserve role execution plus registered command verification for role+command gates. Align the [driver](../../crates/forge-tasks/src/tasks/profile/mod.rs) comment and published contract with role precedence, consistent with existing task/root callers.
- **Acceptance**: Execute bare/fenced pass and structured reject/resynthesis, mismatch, identity, missing findings, and multiple-object cases through the role runner. Exercise role-only, command-only, role+command, and container selection as one driver case table. Restoring raw-only parsing or allowing contradictory decisions must fail. Run focused workflow role tests and cargo test --test acdd orchestrator::lifecycle.

## 7. Enforce Observed Proofs Across Root Execution

- **Target**: Make workflow, milestone, and plan gates validate registered command facts instead of treating exit zero as sufficient proof.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/role.rs","crates/forge-tasks/src/engine/orchestrator/workflow.rs","crates/forge-tasks/src/engine/orchestrator/waves.rs","crates/forge-tasks/src/engine/orchestrator/plan.rs","crates/forge-tasks/src/engine/evidence_compiler.rs","crates/forge-tasks/src/milestones/command_gate.rs","crates/forge-tasks/src/tasks/gates.rs","tests/acdd/runner/command_gate_proofs.rs","tests/acdd/runner/output_contracts.rs","tests/acdd/harness/workflows.rs","tests/acdd/harness/plans.rs","tests/acdd/orchestrator/waves.rs","tests/acdd/fixtures/workflows/ordered.yaml","docs/reference/acdd.md","docs/reference/profiles.md"]
- **Depends On**: ["Compile Observed Task Seam Proofs","Validate Role Decisions and Gate Driver Semantics"]
- **Status**: ready

- Reuse the existing observation/compiler/validator pipeline at role+command gates and pure-command branches of [run_standalone_workflow](../../crates/forge-tasks/src/engine/orchestrator/workflow.rs), [run_milestone_waves](../../crates/forge-tasks/src/engine/orchestrator/waves.rs), and [run_plan_workflow](../../crates/forge-tasks/src/engine/orchestrator/plan.rs). Factor only common observed-proof policy; preserve the three subjects' distinct persistence.
- Validate configured catalog schemas, command identity, timeout, output.require_counts, observed exit/counts, and supplied proof agreement. Contract success requires compiled runtime-red failure with the configured signature and zero-exit only under an explicit permitted policy; a compiler error or accidental green command is invalid. Seam success requires matching per-test references/counts. Keep rejection paths typed and routable.
- Define count-free shell commands explicitly with require_counts: false in ordered.yaml; keep required-count commands strict. Preserve executable-proof restrictions where a driver cannot provide the required observed evidence.
- **Acceptance**: Separate domain tests cover workflow/root role runtime-red acceptance and false-green/compiler-error rejection, seam ref mismatch, pure-command missing counts, count-free opt-out, and catalog-invalid payloads across workflow/plan/native milestone paths. Change observed counts or disable the shared verifier and assert failure. Run cargo test --test acdd runner plus affected workflow/plan and orchestrator::waves seams.

## 8. Refresh Artifact Tokens and Expand Role Commands

- **Target**: Bind downstream command placeholders to the latest accepted artifact proof without stale fields.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/workflow.rs","crates/forge-tasks/src/engine/orchestrator/role.rs","tests/acdd/harness/workflows.rs","tests/acdd/fixtures/workflows/","docs/reference/acdd.md"]
- **Depends On**: ["Enforce Observed Proofs Across Root Execution"]
- **Status**: ready

- Replace accumulated artifact fields on a producer's accepted re-pass; retain independently owned prerequisite fields while replacing that producer's prior contribution. Keep a failed/unaccepted producer's fields unavailable to downstream validation. Define token ownership using current gate observations, not a parallel artifact registry.
- Apply [expand_workflow_placeholders](../../crates/forge-tasks/src/engine/orchestrator/workflow.rs) to role+command gates as well as pure-command gates. Preserve plan_slug/plan_name aliasing, unresolved-placeholder rejection, path-character restrictions, and safe process argument handling.
- **Acceptance**: Execute synthesize -> review reject -> resynthesize with one removed and one changed field -> preflight. Assert the removed key cannot resolve, the changed path is the only validated artifact, and unrelated prerequisite fields remain. Include role+command substitution and unsafe values. Restoring insert-only accumulation or skipping expansion must fail. Run focused workflow placeholder/rejection tests and member placeholder tests.

## 9. Make Pre-Task Barriers Follow the Accepted Route

- **Target**: Unlock task claim and proof fan-out after the required reachable pre-task route passes.
- **Scope**: ["crates/forge-tasks/src/engine/gate_graph.rs","crates/forge-tasks/src/engine/lifecycle/root_gates.rs","crates/forge-tasks/src/engine/lifecycle/proof_fanout.rs","crates/forge-tasks/src/engine/orchestrator/waves.rs","tests/acdd/orchestrator/waves.rs","tests/acdd/orchestrator/waves/harness.rs","tests/acdd/fixtures/workflows/wave-role.py","docs/reference/acdd.md","docs/reference/tasks.md"]
- **Depends On**: ["Enforce Observed Proofs Across Root Execution"]
- **Status**: ready

- Reconcile [first_unpassed_root_cursor](../../crates/forge-tasks/src/engine/gate_graph.rs), [ensure_pre_task_root_gates_passed](../../crates/forge-tasks/src/engine/lifecycle/root_gates.rs), [ready_pre_task_root_cursors](../../crates/forge-tasks/src/engine/lifecycle/proof_fanout.rs), and native wave dispatch with accepted on_success routing. Deliberately bypassed cursors do not become pending requirements; applicable reachable gates and required predecessor proofs still block claims until accepted.
- Replace the simplified preflight fixture in [pre_task_blueprint_synthesis_container_barrier_and_per_task_proof_injection](../../tests/acdd/orchestrator/waves.rs) with the real nested blueprint_synthesis.synthesize -> review container, rejection/resynthesis, independent reviewer, and per-task proof injection. Preserve task-map validation and missing/foreign slice errors.
- **Acceptance**: Separate tests prove the real nested container and a deliberate bypass route, including store reopen/replay and conditional gates. Assert no task claim before required acceptance, no fan-out from rejected proof, and only the matching task slice reaches context. Requiring every flattened cursor or treating a missing reachable gate as skipped must fail. Run cargo test --test acdd orchestrator::waves and focused claim/root-gate tests.

## 10. Enforce Native Milestone Snapshot and Immutability Policy

- **Target**: Apply declared root save.pre/save.post and immutable boundaries during native wave execution.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/waves.rs","crates/forge-tasks/src/engine/lifecycle/root_gates.rs","crates/forge-tasks/src/engine/git_snapshot.rs","tests/acdd/orchestrator/waves.rs","tests/acdd/orchestrator/waves/harness.rs","docs/reference/acdd.md"]
- **Depends On**: ["Make Pre-Task Barriers Follow the Accepted Route"]
- **Status**: ready

- Reuse explicit root lifecycle persistence/verification in native waves instead of adding separate snapshot policy. Honor pre/post snapshots at their declared boundaries and check the accepted candidate before and after immutable gates.
- Bind cached/replayed decisions and snapshots to the same authority/profile/candidate inputs. Candidate mutation invalidates downstream accepted suffixes; preserve accepted task receipts and existing handoff binding semantics.
- **Acceptance**: Execute native root pre/post snapshot capture and a separate immutable-review mutation rejection, then reopen and verify replay cannot hide changed content. Assert snapshot refs/candidates and unchanged HEAD where snapshots are requested. Removing the immutable check or a declared snapshot must fail. Run cargo test --test acdd orchestrator::waves and focused member root persistence tests.

## 11. Canonicalize Workflow Discovery Roots

- **Target**: Keep workflow list, resolution, and execution confined and functional when the repository root is reached through a symlink.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/workflow.rs","tests/acdd/harness/workflows.rs","tests/acdd/fixtures/workflows/","docs/reference/cli.md"]
- **Depends On**: ["Refresh Artifact Tokens and Expand Role Commands"]
- **Status**: ready

- In [list_workflows](../../crates/forge-tasks/src/engine/orchestrator/workflow.rs), canonicalize the root once and use the same root for starts_with and strip_prefix. Reuse existing root/path guards; keep external symlinks and loops rejected without scanning path components repeatedly.
- **Acceptance**: Through existing workflow harness/CLI smoke boundaries, list, resolve, and run a workflow under a symlink alias of a temporary repository (including the macOS /var versus /private/var shape). Separate rejection cases prove an escaping workflow link remains invalid. Returning to the mixed canonical/lexical prefix check must fail. Run focused workflow discovery/execution tests on supported symlink platforms.

## 12. Exclude the Configured Task Database from Plan Snapshots

- **Target**: Use the resolved tasks_db path for all plan snapshots while preserving in-place candidate safety.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/plan.rs","crates/forge-tasks/src/engine/git_snapshot.rs","tests/acdd/harness/plans.rs","tests/acdd/fixtures/plans/","docs/reference/acdd.md"]
- **Depends On**: ["Enforce Observed Proofs Across Root Execution"]
- **Status**: ready

- In [run_plan_workflow](../../crates/forge-tasks/src/engine/orchestrator/plan.rs) and [gate_snapshot](../../crates/forge-tasks/src/engine/orchestrator/plan.rs), replace hardcoded .forge/tasks.sqlite with the task-store path resolved through database_path(root). Preserve existing root/config confinement and exclusion handling for database sidecars.
- Distinguish an allowed non-default path inside the repository from a path external to it. Exercise allowed external storage only where current configuration authority permits it; otherwise assert its existing rejection. This plan does not relax database confinement.
- **Acceptance**: Use a custom configured database outside .forge/tasks.sqlite, create actual DB/WAL activity, execute pre/post and completed-step snapshots, and inspect their trees. Assert database/sidecars are absent, admitted implementation and plan status/evidence are present, HEAD is unchanged, source drift is rejected, and unrelated initial files survive. Restoring either hardcoded exclusion must fail. Run focused PlanHarness snapshot tests and member snapshot tests.

## 13. Validate Runner Slots and Rejection Resumption

- **Target**: Reject missing required runner values and make configured CLI reuse_on_reject preserve the actual supported session.
- **Scope**: ["crates/forge-tasks/src/engine/orchestrator/types.rs","crates/forge-tasks/src/engine/orchestrator/spawner.rs","crates/forge-tasks/src/engine/orchestrator/role.rs","crates/forge-tasks/src/engine/orchestrator/task_runner.rs","crates/forge-tasks/src/tasks/profile/validate.rs","tests/acdd/orchestrator/types.rs","tests/acdd/orchestrator/spawner.rs","tests/acdd/orchestrator/runner_fixture.py","tests/acdd/orchestrator/lifecycle.rs","docs/reference/acdd.md","docs/reference/profiles.md"]
- **Depends On**: ["Route Mechanical Task Failures and Recover Rejected Sessions","Validate Role Decisions and Gate Driver Semantics"]
- **Status**: ready

- Validate model/reasoning when referenced by a runner template before spawning. Keep model-free runners valid when those slots are absent. [render_runner](../../crates/forge-tasks/src/engine/orchestrator/types.rs) must not produce dangling -m "" or -c model_reasoning_effort="" arguments. Preserve one-pass substitution and safe argument transport.
- Trace configured CLI process launch, Forge session fingerprint, actual provider resume capability, and rejection reuse. Reuse the supported spawner/session mechanism and enforce its requirements; document precisely whether a runner resumes conversation state or only receives a stable Forge identity. Reject configurations promising unsupported conversation resumption rather than claiming identity reuse proves it. Preserve independent reviewer separation and fresh sessions for targets without an accepted predecessor.
- **Acceptance**: Test missing referenced slots, absent unused slots, quoted arguments, fresh and resumed invocation, reject -> same accepted builder session, and independent reviewer session. Use a deterministic runner that records actual invocation/session continuity; add a CLI shape smoke test where necessary without contacting an LLM. Empty arguments or a silently fresh resume must fail. Run cargo test --test acdd orchestrator::types, orchestrator::spawner, and focused lifecycle recovery tests.

## 14. Verify MCP and CLI Worktree Claim Parity

- **Target**: Prove canonical claim identity and task-store effects through existing MCP routing and CLI claim boundaries; repair only a demonstrated failing parity case.
- **Scope**: ["src/mcp/tools.rs","src/mcp/server.rs","src/mcp/tasks.rs","src/cli/task.rs","crates/forge-tasks/src/tasks/mod.rs","crates/forge-tasks/src/engine/workspaces.rs","tests/acdd/mcp/worktrees_and_isolation.rs","tests/mcp/context/worktree_claims.rs","tests/acdd/tasks/workspaces/","docs/reference/mcp-tools.md","docs/reference/tasks.md","docs/reference/cli.md"]
- **Depends On**: ["Route Mechanical Task Failures and Recover Rejected Sessions"]
- **Status**: ready

- Trace the production path first: [call_tool](../../src/mcp/tools.rs) resolves the requested selector through [resolve_target_root_and_db](../../src/mcp/server.rs), constructs a request-local server with the selected canonical root, then dispatches [task_claim](../../src/mcp/tools.rs) or [task_manage](../../src/mcp/tools.rs). Their assignment of self.root therefore forwards the selected root. Verify this path through the MCP dispatcher; a direct handler invocation bypasses selector resolution and cannot establish a routing defect.
- Preserve MCP name aliases and explicit-directory acceptance in [resolve_explicit_worktree](../../src/mcp/server.rs). CLI passes its worktree argument to [claim_worktree](../../crates/forge-tasks/src/tasks/mod.rs), which canonicalizes an existing directory without requiring Git registration. Compare equivalent explicit paths across transports; test MCP aliases separately. Preserve each surface's current omitted/empty handling (the MCP dispatcher rejects an explicitly empty selector), existing confinement rules, session fingerprint, shared task-store routing, selected adapter/profile, declared scope checks, and parked-worktree affinity.
- Establish the boundary matrix and record any failing production case before choosing a source edit. If the matrix passes, close the audited suspicion with evidence and retain the coverage; source changes require a reproducible mismatch and stay with its existing owner. Keep adapters thin and reuse current path/lifecycle helpers. Align affected tool/schema descriptions and MCP/CLI documentation with observed behavior in the same step.
- **Acceptance**: Use the existing in-process MCP dispatcher and task harnesses for separate claim and next tests. Cover canonical root, symlink alias, registered sibling, existing unregistered explicit directory, omitted/empty values, nonexistent/non-directory paths, MCP alias ambiguity, and currently forbidden scope/path escapes. Assert equal canonical claim identity and task-store effects for equivalent CLI/MCP inputs, the existing transport-specific errors, and no claim mutation on invalid input. Retain [parking_preserves_and_resumes_the_original_worktree](../../tests/acdd/mcp/worktrees_and_isolation.rs) as positive coverage; it already asserts preserved canonical identity on resume. Demonstrate sensitivity by deliberately routing a sibling-worktree request to the original server root or persisting a different canonical claim root and require the new assertions to fail. Run cargo test --test acdd mcp::worktrees_and_isolation, focused tasks::workspaces tests, and cargo test --test mcp context::worktree_claims.

## 15. Repair Shipped Workflow Definitions and Examples

- **Target**: Make shipped polish/lifecycle workflows runnable under current standalone persistence, retry, runner, and proof rules.
- **Scope**: ["docs/workflows/code-quality-and-polish.workflow.yaml","docs/workflows/test-workflow-lifecycle.workflow.yaml","skills/contextunity-forge/SKILL.md","skills/contextunity-forge/references/acdd-profile-examples/","skills/contextunity-forge/references/acdd_profile.yaml.example","docs/reference/acdd.md","docs/reference/cli.md","docs/reference/profiles.md","tests/acdd/harness/workflows.rs"]
- **Depends On**: ["Enforce Native Milestone Snapshot and Immutability Policy","Canonicalize Workflow Discovery Roots","Exclude the Configured Task Database from Plan Snapshots","Validate Runner Slots and Rejection Resumption","Verify MCP and CLI Worktree Claim Parity"]
- **Status**: ready

- Declare save: none for standalone examples; remove the unsupported claim that save: log emits .forge/runs records. Supply executable runner definitions, proof declarations matching returned decisions, independence, and explicit max_attempts on repair loops. Preserve intended verify -> polish -> review behavior.
- Align affected companion skills/profile examples with final schema, observed-proof policy, runner semantics, and persistence. Update only examples whose described behavior changes; validate all changed examples through the production profile parser. Keep authoring workflow schemas and three-gate semantics intact.
- **Acceptance**: Read both shipped workflows through [read_workflow](../../crates/forge-tasks/src/engine/orchestrator/workflow.rs), assert usable drivers/runners and persistence, and validate affected skill/reference profiles. Restoring unsupported persistence or removing a required runner must fail. Run focused WorkflowHarness shipped-profile tests and affected member profile tests.

## 16. Execute Authoring Workflows and Close the Coverage Ledger

- **Target**: Prove the finalized authoring and polish workflows execute their declared contracts and complete repository verification.
- **Scope**: ["tests/acdd/harness/workflows.rs","tests/acdd/harness/plans.rs","tests/acdd/orchestrator/waves.rs","tests/acdd/fixtures/workflows/","docs/reference/acdd.md","docs/reference/cli.md","docs/architecture/test-harness.md","skills/contextunity-forge/SKILL.md","skills/contextunity-forge/references/acdd-profile-examples/"]
- **Depends On**: ["Repair Shipped Workflow Definitions and Examples"]
- **Status**: ready

- Add separate execution tests for author-milestone, author-plan, author-workflow, and code-quality-and-polish using the shipped profiles as the source. Substitute only external LLM runner behavior and expensive repository command bodies with declared hermetic test equivalents; keep profile schemas, routing, placeholder expansion, artifact parsing, proof validation, identity/independence, and persistence real. Record substitutions explicitly.
- Each authoring seam creates or updates its expected artifact, rejects and repairs it at least once, and runs the corresponding real parser/validation command at preflight. The author-plan seam finalizes an existing file, verifies numbered steps/scopes/DAG with plan show, and proves no duplicate. Keep generated fixture content minimal and illustrative narrative examples Ukrainian. Prove taskless workflows create no task rows/claims or synthetic run logs. The polish seam executes a failing verification, repair, independent review, and completion.
- Reconcile every ledger entry with a passing positive seam and a demonstrated sensitivity mutation. Preserve distinct domain coverage when removing superseded tests; keep full workflow scenarios outside unrelated tests. Align runtime documentation and affected companion examples with observed behavior; list any authority gap explicitly rather than marking it implemented.
- **Acceptance**: Run the focused new workflow tests and cargo test -p forge-tasks --lib. Run the TESTS.md compound gate: cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets. Record passed/failed/ignored counts and the content binding used for review. Validate changed frontmatter, relative links and profile examples, then run git diff --check. Independent execution review checks the finding ledger, scoped diff, snapshot/HEAD safety, and remaining gaps; no commit or remote publication is implied by plan completion.

## 17. Consolidate Test Matrices, Subsume Heavy Clones, and Optimize Suite Speed

- **Target**: Audit all tests added or modified in this branch (Steps 1–16) and adjacent `tests/acdd/` / crate suites, collapse homogeneous variations into fast table-driven `for case in cases` tests, delete weaker/duplicate heavy `#[test]` clones, and eliminate redundant git/SQLite/process setup bottlenecks before final closure.
- **Scope**: ["AGENTS.md","tests/AGENTS.md","tests/acdd/orchestrator/waves.rs","tests/acdd/orchestrator/lifecycle.rs","tests/acdd/harness/","tests/acdd/fixtures/","crates/forge-tasks/"]
- **Depends On**: ["Execute Authoring Workflows and Close the Coverage Ledger"]
- **Status**: ready

- Enforce the updated `AGENTS.md` / `tests/AGENTS.md` rules retroactively across Steps 1–16 (as well as during Steps 10–16): never keep separate heavy `#[test]` functions (each spinning up `WaveHarness`, `TempDir`, `git init`, Python/CLI child processes) for homogeneous case variations such as `{workflow, plan, milestone}`, catalog error inputs, or `role+command` failure modes when a single table-driven `for case in cases` test sharing base fixture state covers every row cleanly with explicit per-case assertions.
- Subsume weaker overlapping tests into their stronger domain seams (e.g., overlapping bypass fixtures in `waves.rs`), replace slow subprocess/sleep loops with in-process engine seams and deterministic status checks where process transport is not under test, and ensure zero duplicate fixtures or redundant `git init` / cold rebuild work inside row loops.
- **Acceptance**: Preserve every contract assertion and sensitivity guarantee while reducing heavy `#[test]` duplication and total `cargo test --test acdd` runtime cleanly. Re-run `cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets`, `cargo test --workspace --all-targets`, and `git diff --check`, and confirm independent review (`DECISION: PASS`).
