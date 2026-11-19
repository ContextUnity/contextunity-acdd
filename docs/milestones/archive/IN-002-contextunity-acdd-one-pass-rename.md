---
id: IN-002-contextunity-acdd-one-pass-rename
title: One-pass ContextUnity ACDD identity and self-hosting cutover
doc_type: contract
status: completed
depends_on: []
guidance:
  hitl: pro
owners:
- Cargo.toml
- build.rs
- crates/
- src/
- tests/
- skills/
- install.sh
- .github/workflows/
- docs/
invariants:
- 'INV-SCOPE-INTEGRITY: Admit only the explicitly authorized Cargo manifests, identity-only root build.rs and index build-script edits, state/config rename, workflow files, and named implementation roots.'
- 'INV-ONE-PASS-IDENTITY: Expose contextunity-acdd, acdd, acdd.yaml, .acdd/profile.yaml, acdd-core, acdd-index, acdd-tasks, and ACDD_* without old-name aliases, fallback discovery, dual writes, or automatic legacy migration.'
- 'INV-CORE-LEAF: Preserve the core leaf and existing crate dependency directions during path and import changes.'
- 'INV-SELF-HOSTING-CUTOVER: Deliver all tasks with a pinned pre-cutover controller before switching live state; prove the new controller can resume accepted task evidence and hand off before making the switch.'
- 'INV-PROOF-CONTINUITY: Preserve task identities, accepted receipts, immutable candidate content, checkpoint ownership, and trace bytes across the offline operational cutover.'
- 'INV-NAVIGATION-PARITY: Rename the MCP reader to acdd_read while preserving its request/result contract, CLI read parity, and the code_map_*, task_*, and ast_grep_search names.'
- 'INV-MINIMAL-INTERVENTION: Keep path resolution single-pass and companion documentation external; add no rename registry, startup migration, or extra runtime dependency.'
required_skills:
- contextunity-forge
- contract-planning
- agent-instructions
polish_scope:
- CONTRIBUTING.md
- benchmarks/
related_plans: []
started_at: 2026-11-14T22:22:45+00:00
repository: forge-mcp
project: forge-mcp
handoff:
  completed_at: 2026-11-15T04:53:01.299767502+00:00
  duration: 6h 30m
  commit: e053733af299bb2559aa1b9396a7af777b60c24a
  target_base: 884d7b2138b0c23930c1267f3b6cfb06b7980b50
  integration_evidence:
    binding:
      version: 1
      repository: forge-mcp
      project: forge-mcp
      milestone: IN-002-contextunity-acdd-one-pass-rename
      authority_digest: 77931df0802c2a775ecbb588ed7d044f14064bd000633c601e8d2ba07d7bd081
      implementation_digest: b85a0c18362ec9775113d351eec15f9342ebc58cd4f84eaf390440abdc18e68c
      target_base_digest: d291e23944679eb661daf7449723ef682cf3640ce0de6e3010aee8cce7b92a3c
      builders:
      - codex-terra-builder
      - codex-wave0-builder
    target_base: main
    review:
      worker_id: integration-reviewer
      decision: pass
      summary: 'Independent integration review of cumulative IN-002 one-pass rename and self-hosting cutover candidate e053733af against target base main: verified embedded profile rename to acdd.profile.yaml, benchmarks/install/docs/skills/test alignment, archived completed plans, strict Clippy, and all 1350 root tests + acdd-tasks lib suite.'
      scope_findings: []
      contract_findings: []
    verification:
      command: cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets
      exit_code: 0
      tests_passed: 1350
      tests_failed: 0
      project_command: check_full_repo
  proof_shas:
  - 0120de42348ab55b801350f40ba11d7dcb6ba8d7
  - 05903d38b749b50ea1d7d090d4b84079c872d481
  - 074633708a49b61158ff0b575744c75fb817bae1
  - 160f20d297735c34a54a93cce5ca3b7b96d01a23
  - 2511c7faee17b0d73507e799c0e4ae659cdd100b
  - 2b75efa88df378ca0dbc4fe2e11c9b50a676b7b6
  - 2e890470e0142179c32fad523a589aa72e92a85f
  - 31d99be3c92d09d46c2382025344a0075bbece69
  - 38d84e5475121754adb0ebeab277d3d25925eeb7
  - 53de3cf8e4dc5763f93bb262fb4f021dba96ced3
  - 62cbf94cd1ebf02d25bc3cf73a2ef9b0e08b49eb
  - 688b8c6f760002e1d4604991f9e8279d25a41fcb
  - 68ba3078d6cec53d241ff275132e21cb551b5bb0
  - 6cb048f9d09a2cc5fdaaca27e691074111a2c479
  - 884d7b2138b0c23930c1267f3b6cfb06b7980b50
  - 95b8d5d7ae8336be3cbc8b5533eab077b091da69
  - a02a81899e87ba9ac089606feb29636440b07fd9
  - a69c7e7affd63fa719e152e2ffe93d6d43d33199
  - dce53b823386a4d13e02097dd90b7b90df81015d
  - e053733af299bb2559aa1b9396a7af777b60c24a
  - eae9325e68502911944a1c552b266c5f1d6a77c1
  - f644b5a93cee088f9f3f00eef669f5d97fa392f0
  passport:
    version: 1
    identity: forge-mcp/forge-mcp/IN-002-contextunity-acdd-one-pass-rename
    binding:
      version: 1
      repository: forge-mcp
      project: forge-mcp
      milestone: IN-002-contextunity-acdd-one-pass-rename
      authority_digest: 77931df0802c2a775ecbb588ed7d044f14064bd000633c601e8d2ba07d7bd081
      implementation_digest: b85a0c18362ec9775113d351eec15f9342ebc58cd4f84eaf390440abdc18e68c
      target_base_digest: d291e23944679eb661daf7449723ef682cf3640ce0de6e3010aee8cce7b92a3c
      builders:
      - codex-terra-builder
      - codex-wave0-builder
    commit: e053733af299bb2559aa1b9396a7af777b60c24a
    target_base: 884d7b2138b0c23930c1267f3b6cfb06b7980b50
    active_authority_digest: ca51f1324466a98e8b9fc0794cee9cfb6c7d7370e2d2859f8cfefb9f553469e7
    started_at: 2026-11-14T22:22:45+00:00
    completed_at: 2026-11-15T04:53:01.299767502+00:00
    duration_seconds: 23416
    tasks_count: 4
    tasks:
    - task_id: forge-mcp/IN-002-contextunity-acdd-one-pass-rename:activate-config-and-profile
      status: completed
      first_claimed_at: 1794708232
      completed_at: 1794716553
      execution_wall_seconds: 8321
      review_cycles: 3
      rejected_reviews: 2
    - task_id: forge-mcp/IN-002-contextunity-acdd-one-pass-rename:freeze-rename-and-cutover-contract
      status: completed
      first_claimed_at: 1794694965
      completed_at: 1794697014
      execution_wall_seconds: 2049
      review_cycles: 2
      rejected_reviews: 1
    - task_id: forge-mcp/IN-002-contextunity-acdd-one-pass-rename:rename-distribution-and-guidance
      status: completed
      first_claimed_at: 1794705496
      completed_at: 1794708224
      execution_wall_seconds: 2728
      review_cycles: 2
      rejected_reviews: 1
    - task_id: forge-mcp/IN-002-contextunity-acdd-one-pass-rename:rename-runtime-and-workspace
      status: completed
      first_claimed_at: 1794697055
      completed_at: 1794705487
      execution_wall_seconds: 8432
      review_cycles: 4
      rejected_reviews: 3
    diff:
      docs:
        files: 46
        added: 2298
        removed: 1281
      other:
        files: 113
        added: 5206
        removed: 5075
      src:
        files: 584
        added: 144419
        removed: 144096
      tests:
        files: 300
        added: 3683
        removed: 2895
    ast_symbol_deltas:
      added: 10714
      removed: 10753
      modified: 939
    test_suite_results:
      command: cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets
      exit_code: 0
      tests_passed: 1350
      tests_failed: 0
      project_command: check_full_repo
    automatic_project_verification:
      selection: check_full_repo
      command: cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets
      exit_code: 0
      stdout_digest: 75901160c8c58a6da0278bb52699ee9503f1106ef10402f82e94ca1ea2d75dc1
      reused_verification: true
      implementation_digest: b85a0c18362ec9775113d351eec15f9342ebc58cd4f84eaf390440abdc18e68c
    linter:
      status: unobserved
    topology:
      upstream: []
      downstream: []
      unresolved_upstream: []
    verified_handoff_proof: 1d41b414f883a3141a70292a748581254940fc410d05614c3ba3145a872e1b4b
  verification:
    command: cargo test --test commitment_integrity && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets
    status: passed
    tests_passed: 1350
    tests_failed: 0
---

# One-pass ContextUnity ACDD rename

## Admission status and authority

`status: planned` is the supported pre-claim lifecycle value for an uninitialized
milestone manifest; its first accepted task claim activates it.

The authoring session writes only this file under `docs/milestones/`. Under
`INV-SCOPE-INTEGRITY`, the owner explicitly authorizes identity-only changes to
root `Cargo.toml`, all three member manifests, `Cargo.lock`, root `build.rs`
(crate-path references and `FORGE_LANGUAGE_PROFILE_DIGEST` ->
`ACDD_LANGUAGE_PROFILE_DIGEST` only, preserving code generation, feature
selection, fingerprinting algorithm, and compiler behavior),
`crates/forge-index/build.rs` -> `crates/acdd-index/build.rs`,
`crates/forge-index/build_manifests.rs` ->
`crates/acdd-index/build_manifests.rs`, `forge-mcp.yaml` -> `acdd.yaml`,
tracked `.forge/**` -> `.acdd/**`, and `.github/workflows/*.yml`. These named
exceptions supersede the generic exclusion of Cargo manifests, `build.rs`, and
`.forge/**`. Do not disable the root build, duplicate its generator, or relocate
it through Cargo to evade scope boundaries.

The add-on references ADR 0024, but the inspected decision inventory contains
ADRs 0001–0021 and no ADR 0024. Treat it as unavailable authority; do not invent
its contents or use it to authorize lifecycle redesign. ADRs 0018, 0020, and
0021 plus live source govern this rename.

## Outcome and negative boundaries

| Surface | Required final identity | Owner task |
| --- | --- | --- |
| Root Cargo package and Rust library import | `contextunity-acdd`, `contextunity_acdd` | runtime |
| Sole CLI executable and help/version identity | `acdd` | runtime |
| Workspace members and dependency keys | `crates/acdd-core`, `crates/acdd-index`, `crates/acdd-tasks`; `acdd-core`, `acdd-index`, `acdd-tasks` | runtime |
| Root adapter and profile | `acdd.yaml`, `.acdd/profile.yaml` | activation |
| Generated databases, locks, traces, caches, framework overrides, and task artifacts | `.acdd/` with existing relative layout except the flattened profile | runtime and activation |
| Environment producers, consumers, diagnostics, and examples | `ACDD_*` replacing each `FORGE_*` | runtime and distribution |
| MCP reader registration and all first-party callers | `acdd_read` | runtime |
| Bundled navigation skill and its adapter example | `skills/acdd/`, `references/acdd.yaml.example` | distribution |
| Installer and release archive payload | package `contextunity-acdd`, executable `acdd`, skill `acdd` | distribution |

Keep the version at `0.5.2`; a release or version bump is outside this milestone.
Record the breaking rename in `CHANGELOG.md` under `[Unreleased]`. Preserve
feature sets, language behavior, response schemas, storage schemas, identifier
semantics, and security checks. Keep every MCP tool name other than the
explicitly requested `forge_read` change; this goal does not independently
authorize renaming `forge_guide` or `session_checkpoint`.

Change live project branding and actionable commands, not arbitrary occurrences
of the word "forge" in fixture languages, cryptographic domain separators,
external dependencies, or accepted historical proofs. Preserve archival
milestones, immutable receipts, benchmark results, and past release entries.
Classify remaining matches with a precise reason. Do not manufacture an empty
whole-repository search by rewriting evidence or history.

Do not publish packages, rename the remote repository, push, tag, release, run
benchmarks, edit user-home installations, or remove unrelated operational
state. Keep release download coordinates tied to the real publishing repository;
changing local package/asset names does not create a remote repository. A remote
repository move requires separate authority.

## Grounded architecture and producer-consumer map

Inspected with `code_map_inspect` in the canonical repository, generation
`7ab39c54f296fb04e2717155dd3b1fdd10044e358e8a2e1c18332e7c7f9b2912`:

| Declaration and current signature | Responsibility and consumers |
| --- | --- |
| [`load_adapter`](../../crates/forge-core/src/config.rs), `pub fn load_adapter(root: &Path, adapter_path: Option<&Path>) -> std::io::Result<Adapter>` | Default config and excluded-state names; pipeline build/delta consume the resulting adapter. Mirror the required string changes in the existing task-owned adapter loader without introducing a new registry. |
| [`ResolvedPaths`](../../src/cli/paths.rs), current `impl ResolvedPaths` | CLI config/profile selection; `Cli::run` consumes it. Preserve root confinement and explicit-selection semantics. |
| [`load_for_workspace`](../../crates/forge-tasks/src/tasks/profile/mod.rs), `pub fn load_for_workspace(root: &Path) -> Result<Arc<GateProfile>>` | Repository profile discovery; registry loading, pinned profiles, and handoff telemetry consume it. |
| [`path`](../../crates/forge-tasks/src/engine/mod.rs), `pub fn path(&self, root: &Path) -> Result<std::path::PathBuf>` on `TaskSettings` | Resolves task DB against the primary Git worktree, not necessarily the caller worktree. Store opening, registry loading, and checkpoint fences consume it. |
| [`forge_read`](../../src/mcp/tools.rs), `fn forge_read(&self, Parameters(mut p): Parameters<Snippet>) -> CallToolResult` | MCP ingress delegates to `forge_read_tool` through `Server::read_with_adapter`. Rename the registration and corresponding reader presentation/caller names without changing the read contract. |
| [`generate_profiles`](../../build.rs), `fn generate_profiles()` | Root build-time profile generation; update crate paths (`crates/acdd-index/...`) and emitted digest key (`ACDD_LANGUAGE_PROFILE_DIGEST`). |
| [`generate_framework_manifests`](../../build.rs), `fn generate_framework_manifests() -> Vec<std::path::PathBuf>` | Root build-time manifest path; update directory path to `crates/acdd-index/src/languages/manifests`. |
| [`release_package_step_produces_installable_archive_and_checksum`](../../tests/release_packaging.rs), `fn release_package_step_produces_installable_archive_and_checksum()` | Executes the actual workflow packaging step, extracts its archive, runs the packaged executable, and verifies companion assets. Extend this production seam. |

Depth-one impact analysis of `load_for_workspace` identifies registry loading,
pinned-profile loading, and handoff command observation as consumers. Reader
impact includes first-party setup, CLI, tool, and runbook documentation. Graph
coverage is partial: the overview reports unresolved/ambiguous references; an
absent call edge does not prove a runtime consumer is absent. Inspect literals,
build inputs, shell, YAML, and test fixtures as well.

`search_docs` locates the core-leaf invariant in
`docs/adr/0018-layer-dependency-boundaries-and-core-leaf-isolation.md`
and workspace boundaries in
[performance invariants](../../architecture/performance.md#model-and-agent-invariants-immutable-guidelines).
The explicit protected-infrastructure exception comes from the user goal and
`skills/contract-planning/ADDON.md`, not a fabricated
search hit. `docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md`
keeps this taskless authoring workflow separate from future milestone task rows.

Production path to prove:

`ACDD_WORKSPACE_ROOT / --root` → `Cli::run` → root-confined config/profile
selection → task settings → primary-worktree `.acdd/tasks.sqlite` → accepted
task receipts and root verification → independently reviewed milestone handoff
→ archived manifest and completion evidence. MCP selection follows the same
adapter/state ownership; `acdd_read` uses the existing reader boundary.

Preserve the `acdd-core` leaf and the current `index → core`, `tasks → core`,
root-adapter → members dependency directions. Do not move task I/O into core,
alter schema versions, or add an index/tasks cycle. Keep path helpers
single-pass and existing symlink rejection intact.

## Wave DAG and scope ownership

| Wave | Task | Depends on | Exclusive concern |
| --- | --- | --- | --- |
| 0 | `freeze-rename-and-cutover-contract` | none | Inventory, scoped proof registration, controller preparation contract |
| 1 | `rename-runtime-and-workspace` | wave 0 | Atomic Cargo/crate/`build.rs`/source/test rename |
| 2 | `rename-distribution-and-guidance` | wave 1 | Installer, skills, CI/release, packaging proof, remaining live docs |
| 3 | `activate-config-and-profile` | wave 2 | Tracked adapter/profile move and final offline-controller rehearsal |

Use singleton waves deliberately. Crate moves, Rust imports, binary test macros,
and defaults are coupled compile-time changes; splitting them into concurrent
tasks creates artificial broken intermediate builds. All same-wave scopes are
disjoint by construction. Scope overlap across waves is ordered. Renames admit
both source and destination paths. Directory scopes below own identity-only
changes within those subtrees; they do not authorize semantic refactoring.

## Tasks in this milestone

### task: freeze-rename-and-cutover-contract

```yaml
task_ref: freeze-rename-and-cutover-contract
target: Freeze the rename inventory, evidence-preserving cutover protocol, and registered production seams before any runtime identity changes
proof_policy: seam-test-first
contract_revision: 3
depends_on: []
scope:
- docs/milestones/IN-002-contextunity-acdd-one-pass-rename.md
- docs/runbooks/in002-rename-inventory.md
- docs/runbooks/in002-cutover-runbook.md
- tests/acdd/milestones/discovery/initialization.rs
symbols: []
status: completed
subtasks:
- subtask_ref: identity-and-authority-matrix
  title: Inventory every identity producer/consumer and classify historical exceptions within authorized scope
  status: completed
  evidence: Enumerated all cargo/crate/bin/env/path/MCP/skill/installer/doc targets and preserved exclusions in docs/runbooks/in002-rename-inventory.md
- subtask_ref: compile-clean-seam-registration
  title: Register the rename cutover module and compile-clean domain test skeletons without changing production behavior
  status: completed
  evidence: Verified companion runbook discovery boundary via existing tests/acdd/milestones/discovery/initialization.rs seam without adding standalone fallback modules
- subtask_ref: controller-and-state-ownership
  title: Specify pinned-controller paths, primary-worktree DB ownership, explicit config/profile selection, quiescence, and recovery boundaries
  status: completed
  evidence: Specified pinned-controller, primary-worktree DB ownership, writer quiescence, backup, ownership-aware entry transfer, and recovery boundaries in docs/runbooks/in002-cutover-runbook.md
receipt:
  delivery_gate: deliver
  candidate_gate: build
  commit: 688b8c6f760002e1d4604991f9e8279d25a41fcb
  contract_revision: 3
  passed_at: 2026-11-14T22:56:54.212452211+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/acdd/milestones/discovery/initialization.rs]
        PASS rust-check-and-test::tests/acdd/milestones/discovery/initialization.rs: exit=0 passed=1 failed=0 (5869 ms)
  review:
    review_proof:
      contours:
        administration:
          applicable: true
          evidence: 'Protected configuration, schemas, receipts, and delivery mechanisms remain unchanged. Decision #173 is preserved without standalone cutover modules or fallback matrices.'
        claims:
          applicable: true
          evidence: W0-01 closed by pinned binary, sha256sum/version checks, explicit root/config/profile invocation, primary-store binding, and renamed crate/bin mappings; W0-02 closed by Milestone::parse DAG/scope assertions plus real discovery/init and misplaced-companion checks via MilestoneHarness.
        concurrency:
          applicable: true
          evidence: Sequential waves preserve ordered overlapping scopes; writer quiescence, checkpoint resolution, SQLite closure, backup, occupied-destination rejection, and interruption recovery remain required.
        paths:
          applicable: true
          evidence: All four admitted files match candidate snapshot 68ba3078d6cec53d241ff275132e21cb551b5bb0; runtime, Cargo, build scripts, config, profile, installer, skills, and workflows remain untouched.
        project_isolation:
          applicable: true
          evidence: Operational overrides use root-confined paths in .forge-mcp and retain primary-worktree task-store ownership; test uses isolated MilestoneHarness workspace.
      decision: pass
      notes: 'Round 2 independent gpt-6.1-sol (xhigh) review passed (codex session 01a1dc3b-1bf0-7e41-9d59-260a8bd88859), closing W0-01 and W0-02 from #174.'
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-SCOPE-INTEGRITY: Admit only the explicitly authorized Cargo manifests, identity-only root build.rs and index build-script edits, state/config rename, workflow files, and named implementation roots.'
    - 'INV-ONE-PASS-IDENTITY: Expose contextunity-acdd, acdd, acdd.yaml, .acdd/profile.yaml, acdd-core, acdd-index, acdd-tasks, and ACDD_* without old-name aliases, fallback discovery, dual writes, or automatic legacy migration.'
    - 'INV-CORE-LEAF: Preserve the core leaf and existing crate dependency directions during path and import changes.'
    - 'INV-SELF-HOSTING-CUTOVER: Deliver all tasks with a pinned pre-cutover controller before switching live state; prove the new controller can resume accepted task evidence and hand off before making the switch.'
    - 'INV-PROOF-CONTINUITY: Preserve task identities, accepted receipts, immutable candidate content, checkpoint ownership, and trace bytes across the offline operational cutover.'
    - 'INV-NAVIGATION-PARITY: Rename the MCP reader to acdd_read while preserving its request/result contract, CLI read parity, and the code_map_*, task_*, and ast_grep_search names.'
    - 'INV-MINIMAL-INTERVENTION: Keep path resolution single-pass and companion documentation external; add no rename registry, startup migration, or extra runtime dependency.'
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

Implementation steps: enumerate source,
config, build, shell, workflow, test, skill, and documentation occurrences. Pin a
copy of the installed pre-rename executable outside `target/` and the renamed
state tree, with its checksum and exact invocation. Use existing explicit
`--config` and `--profile` selection for a temporary execution configuration
whose task DB points at the current authoritative DB. Its command definitions
must target the actual post-rename test/crate names when those waves execute.
Keep this execution configuration operational and untracked, not a shipped
fallback. Do not edit the installed executable or ambient MCP configuration.

Test seam: use existing `milestones::discovery::initialization` in the `acdd`
test binary to verify companion runbook/inventory discovery stability; do not
add standalone cutover test modules or fallback-shim test matrices. Task 0's own contract seam proves its
inventory and dependency/scope rules through the existing manifest parser.
Downstream tests add runtime assertions at their own contract gate.

Incoming seams: current manifest parser, profile loader, explicit selection,
Git worktree ownership, and existing domain harnesses. Outgoing seams: frozen
path inventory, exact controller invocations, test module registration, and the
offline transfer checklist. Negative boundaries: no live state move, no
compatibility config shipped with the product, no manual SQL mutation, no task
or receipt identity renumbering, and no new migration command.

### task: rename-runtime-and-workspace

```yaml
task_ref: rename-runtime-and-workspace
target: Atomically rename Cargo identities, workspace directories, Rust imports, runtime defaults, environment keys, and the MCP reader while proving unchanged architecture and read contracts
proof_policy: seam-test-first
contract_revision: 1
depends_on:
- freeze-rename-and-cutover-contract
scope:
- Cargo.toml
- Cargo.lock
- build.rs
- crates/forge-core/Cargo.toml
- crates/forge-core/src/
- crates/acdd-core/Cargo.toml
- crates/acdd-core/src/
- crates/forge-index/Cargo.toml
- crates/forge-index/build.rs
- crates/forge-index/build_manifests.rs
- crates/forge-index/src/
- crates/acdd-index/Cargo.toml
- crates/acdd-index/build.rs
- crates/acdd-index/build_manifests.rs
- crates/acdd-index/src/
- crates/forge-tasks/Cargo.toml
- crates/forge-tasks/src/
- crates/acdd-tasks/Cargo.toml
- crates/acdd-tasks/src/
- src/
- tests/
- docs/reference/
- TESTS.md
symbols:
- cmd:acdd
- route:acdd_read
status: completed
subtasks:
- subtask_ref: manifests-crates-and-caller-imports
  title: Rename package/member/dependency identities and all Rust import/test binary references in a single compiling change
  status: completed
  evidence: Cargo.toml, Cargo.lock, crates/acdd-{core,index,tasks}, src/, and tests/ renamed; verified workspace clippy + macOS zig clippy + --lib (26+61+89+24) + --test core (54) + --test mcp (188) pass.
- subtask_ref: runtime-config-state-environment
  title: Replace default config, state, profile, cache, exclusion, and FORGE_* producer/consumer identities without fallback lookup
  status: completed
  evidence: Replaced runtime defaults (acdd.yaml, .acdd/profile.yaml, .acdd/, ACDD_*) with no fallback lookup and cleaned leftover non-contract runtime forge strings; verified via --lib, --test core, --test mcp, --test acdd.
- subtask_ref: reader-registration-and-production-seams
  title: Register only acdd_read for the reader and prove CLI/MCP parity, old-name absence, and unchanged navigation/task names
  status: completed
  evidence: Registered acdd_read only (rejecting forge_read) with CLI/MCP parity and preserved navigation/task names; verified via --test mcp (188 passed) including test_read::acdd_read_symbol_alias_is_discoverable_in_mcp_schema.
receipt:
  delivery_gate: deliver
  candidate_gate: build
  commit: eae9325e68502911944a1c552b266c5f1d6a77c1
  contract_revision: 1
  passed_at: 2026-11-15T01:18:07.802260134+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/mcp/test_read.rs]
        PASS rust-check-and-test::tests/mcp/test_read.rs: exit=0 passed=1 failed=0 (28732 ms)
  review:
    review_proof:
      contours:
        administration:
          applicable: true
          evidence: acdd-core remains independent of acdd-index and acdd-tasks. Schema/hash constants, snapshot namespaces, runtime identity, and preserved MCP names remain unchanged by this remediation.
        claims:
          applicable: true
          evidence: Cache generation and classification share workspace-based resolution. The production regression exercises relative and absolute cache overrides, rejects policy.json, and permits claiming after its removal. R1-R5 all resolved.
        concurrency:
          applicable: true
          evidence: Transaction, revision fencing, rollback, and candidate-binding mechanics remain unchanged. Production scope checks receive explicit workspace or worktree roots.
        paths:
          applicable: true
          evidence: All six remediation files are within admitted scope and match candidate 05903d38b749b50ea1d7d090d4b84079c872d481. Protected Wave 2/3 assets remain unchanged.
        project_isolation:
          applicable: true
          evidence: Configured cache paths are resolved against the owning workspace. Only validated cache artifact shapes receive runtime exemptions; configuration protection remains intact.
      decision: pass
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-SCOPE-INTEGRITY: Admit only the explicitly authorized Cargo manifests, identity-only root build.rs and index build-script edits, state/config rename, workflow files, and named implementation roots.'
    - 'INV-ONE-PASS-IDENTITY: Expose contextunity-acdd, acdd, acdd.yaml, .acdd/profile.yaml, acdd-core, acdd-index, acdd-tasks, and ACDD_* without old-name aliases, fallback discovery, dual writes, or automatic legacy migration.'
    - 'INV-CORE-LEAF: Preserve the core leaf and existing crate dependency directions during path and import changes.'
    - 'INV-SELF-HOSTING-CUTOVER: Deliver all tasks with a pinned pre-cutover controller before switching live state; prove the new controller can resume accepted task evidence and hand off before making the switch.'
    - 'INV-PROOF-CONTINUITY: Preserve task identities, accepted receipts, immutable candidate content, checkpoint ownership, and trace bytes across the offline operational cutover.'
    - 'INV-NAVIGATION-PARITY: Rename the MCP reader to acdd_read while preserving its request/result contract, CLI read parity, and the code_map_*, task_*, and ast_grep_search names.'
    - 'INV-MINIMAL-INTERVENTION: Keep path resolution single-pass and companion documentation external; add no rename registry, startup migration, or extra runtime dependency.'
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

Implementation steps: add an explicit `[[bin]]` named `acdd` using `src/main.rs`;
rename the package/library and member dependency keys with no Cargo `package =`
old-name aliases. Move the member trees once (including
`crates/forge-index/build.rs` and `crates/forge-index/build_manifests.rs` to
`crates/acdd-index/`), update root `build.rs` crate paths and
`FORGE_LANGUAGE_PROFILE_DIGEST` -> `ACDD_LANGUAGE_PROFILE_DIGEST` while
preserving codegen/feature selection, update imports/re-exports and
`CARGO_BIN_EXE_acdd` users, then update every current
`FORGE_*` producer and consumer, including compile-time emitters, tests,
inherited child-process environments, tracing/debug switches, caches, session
identity, and memory-budget validation. Keep Cargo feature/dependency versions
unchanged. Update `.acdd` exclusions and protected-profile path checks together;
flatten only `.forge/acdd/profile.yaml` to `.acdd/profile.yaml`.

Preserve general explicit `--config`, `--profile`, and `--db` overrides; remove
automatic discovery of old identity names. Preserve missing-config defaults and
existing error taxonomy rather than inventing a mandatory-config failure.
Old environment keys do not influence behavior. An old-only config/profile tree
does not become an alternate discovery source. Register `acdd_read` without a
`forge_read` alias; update reader-specific dispatch/presentation hints and
first-party callers. Avoid renaming unrelated public Rust APIs.

Public admission is the package/crate namespace move, `cmd:acdd`, and
`route:acdd_read`. Existing function signatures and schema payloads stay fixed;
private reader helper renames stay inside file scope. If validation requires a
different symbol kind for the tool route, amend the contract explicitly rather
than inventing a Rust declaration named `acdd_read` before its implementation.

Test seams: existing `core` adapter/config tests, `cli_roots`, `mcp` reader and
tool-list tests, and `acdd` lifecycle/profile tests. Add separate descriptive
tests for root/config defaults, primary-worktree task-store ownership, new-only
environment selection, reader registration, and profile protection. Compile
contract tests with current imports, then use runtime assertions for red proof;
do not make unresolved new crate imports the expected failure. Sensitivity:
restore one old default/env lookup/reader alias at a time and demonstrate the
corresponding assertion fails. Use dedicated CLI smoke tests only for parser,
help/version, and executable identity; use in-process seams for domain matrices.

Incoming seams: frozen inventory and controller plan. Outgoing seams: a built
`target/debug/acdd`, renamed crate API imports, new-only runtime defaults, and
stable lifecycle schemas. Keep the live controller on the pinned old executable
and explicit execution config; a newly built `acdd` must not open the live store
before the activation barrier. Negative boundaries: no live config/profile move,
no schema migration, no old-name shim, no semantic linker/hash redesign.

### task: rename-distribution-and-guidance

```yaml
task_ref: rename-distribution-and-guidance
target: Ship the acdd executable and skill through the real installer/release paths and reconcile all live instructions with the renamed runtime
proof_policy: seam-test-first
contract_revision: 2
depends_on:
- rename-runtime-and-workspace
scope:
- install.sh
- .github/workflows/ci.yml
- .github/workflows/release.yml
- tests/release_packaging.rs
- tests/mcp/installer.rs
- tests/mcp/skill_sync.rs
- skills/
- README.md
- INSTALL.md
- TESTS.md
- AGENTS.md
- CHANGELOG.md
- docs/
- tests/framework_manifests/activation/user_manifests.rs
- tests/acdd/tasks/guidance/profile_defaults.rs
- tests/acdd/harness/workflows.rs
- tests/AGENTS.md
- tests/acdd/gates_ast/profile_parser.rs
- crates/acdd-index/src/languages/dependency_registry.rs
symbols: []
status: completed
subtasks:
- subtask_ref: installer-and-archive-contract
  title: Update installer flags, executable paths, checksums, CI commands, and release payload naming through their real tested seams
  status: completed
  evidence: Updated install.sh, .github/workflows/release.yml, tests/release_packaging.rs, and tests/mcp/installer.rs to ship contextunity-acdd / acdd / skills/acdd cleanly without fallback aliases; verified bash -n and --test release_packaging + --test mcp pass.
- subtask_ref: skill-directory-and-consumers
  title: Move the navigation skill to skills/acdd, rename its adapter example, and update bundled references and required-skills consumers
  status: completed
  evidence: Moved skills/contextunity-forge/ to skills/acdd/ with references/acdd.yaml.example, updated bundled SKILL/references/ADDON, and updated skill_sync, user_manifests, profile_defaults, profile_parser, workflows, and crates/acdd-index/src/languages/dependency_registry.rs include_str! paths; verified clippy + --lib (26+61+91+24) + 303 integration tests + 436 acdd tests pass.
- subtask_ref: live-docs-and-guidance
  title: Reconcile current architecture, setup, workflow commands, invariants, examples, and breaking-change notes without rewriting accepted history
  status: completed
  evidence: Reconciled README.md, INSTALL.md, TESTS.md, AGENTS.md, tests/AGENTS.md, CHANGELOG.md ([Unreleased]), and live docs/** (workflows, architecture, reference, runbooks, active plans) without touching docs/milestones/archive/** or prior releases.
receipt:
  delivery_gate: deliver
  candidate_gate: build
  commit: 160f20d297735c34a54a93cce5ca3b7b96d01a23
  contract_revision: 2
  passed_at: 2026-11-15T02:03:44.234647158+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/release_packaging.rs]
        PASS rust-check-and-test::tests/release_packaging.rs: exit=0 passed=1 failed=0 (16298 ms)
  review:
    review_proof:
      contours:
        administration:
          applicable: true
          evidence: AuthoritySnapshot::capture freezes milestone_required_skills; validate_owner rejects mismatches. Persisted snapshots retain the original list for completed Waves 0 and 1. F1 is withdrawn; F3 follows explicit owner authority.
        claims:
          applicable: true
          evidence: F2 executable and package commands are corrected. Positive packaging assertions remain intact. Accepted build evidence binds candidate 2511c7faee17b0d73507e799c0e4ae659cdd100b to the passing packaging seam (plus verified clippy, lib 202, mcp 188, release_packaging 4, docs_check 9, acdd 436).
        concurrency:
          applicable: true
          evidence: Round 2 changes only five plan documents. Wave ordering, claims, revision fencing, recovery checks, and runtime concurrency behavior remain unchanged.
        paths:
          applicable: true
          evidence: All 192 candidate files match disk. Edits stay within admitted Wave 2 scope; protected files, historical evidence, remote repository URLs, and version 0.5.2 remain unchanged.
        project_isolation:
          applicable: true
          evidence: 'Verification records were inspected read-only. Latest command proof reports scope_violations: []; installer isolation remains unchanged. No home installation, live cutover, publication, or task transition was performed.'
      decision: pass
      notes: Round 2 independent gpt-6.1-sol (xhigh) review passed (session 01a1dce0-a99e-7933-93f2-5d4b820544cd); F1 withdrawn per AuthoritySnapshot invariant, F2 fixed in live plans, F3 closed per explicit owner directive.
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-SCOPE-INTEGRITY: Admit only the explicitly authorized Cargo manifests, identity-only root build.rs and index build-script edits, state/config rename, workflow files, and named implementation roots.'
    - 'INV-ONE-PASS-IDENTITY: Expose contextunity-acdd, acdd, acdd.yaml, .acdd/profile.yaml, acdd-core, acdd-index, acdd-tasks, and ACDD_* without old-name aliases, fallback discovery, dual writes, or automatic legacy migration.'
    - 'INV-CORE-LEAF: Preserve the core leaf and existing crate dependency directions during path and import changes.'
    - 'INV-SELF-HOSTING-CUTOVER: Deliver all tasks with a pinned pre-cutover controller before switching live state; prove the new controller can resume accepted task evidence and hand off before making the switch.'
    - 'INV-PROOF-CONTINUITY: Preserve task identities, accepted receipts, immutable candidate content, checkpoint ownership, and trace bytes across the offline operational cutover.'
    - 'INV-NAVIGATION-PARITY: Rename the MCP reader to acdd_read while preserving its request/result contract, CLI read parity, and the code_map_*, task_*, and ast_grep_search names.'
    - 'INV-MINIMAL-INTERVENTION: Keep path resolution single-pass and companion documentation external; add no rename registry, startup migration, or extra runtime dependency.'
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

Implementation steps: move `skills/contextunity-forge` to `skills/acdd`; update
its frontmatter name, examples, tool catalog, agent example, adapter example,
profile examples, all other bundled skills/add-ons that route to it, and skill
installer expectations. Update this milestone's `required_skills` to `acdd` in
the same change so later validation sees an available skill. Package archives as
`contextunity-acdd-<target>.tar.gz` containing `acdd` and the real companion
assets. Update installer download/compile/install/uninstall paths and every
project-owned `FORGE_*` switch; do not rename third-party environment variables.

Extend the existing packaging seam to execute the actual CI package step with
the new binary macro, verify `SHA256SUMS`, extract and run `acdd --version`,
verify the new skill payload, and assert absence of the old executable/skill.
Keep the four existing platform targets and nightly/release behavior. Run
installer proofs against a temporary home and local HTTP/asset fixture; no live
install, network publication, or touching the user's skill directories.

Test seams: `cargo test --test release_packaging`, focused installer and skill
sync tests in `mcp`, shell syntax checks, and existing instruction/layout checks.
Sensitivity: restore one old archive filename, executable path, or skill path;
the production packaging/install assertion must fail. Reuse the existing seam,
not a new test binary or copied workflow string.

Incoming seams: compiled new executable, renamed runtime contract. Outgoing
seams: installable archive, new skill and complete current docs. Negative
boundaries: preserve archived milestone and evidence bytes; do not bump versions,
change remote ownership, weaken verification counts, or publish assets.

### task: activate-config-and-profile

```yaml
task_ref: activate-config-and-profile
target: Move the tracked adapter/profile and prove an offline self-hosting state cutover followed by new-only milestone verification and handoff
proof_policy: seam-test-first
contract_revision: 4
depends_on:
- rename-distribution-and-guidance
scope:
- forge-mcp.yaml
- acdd.yaml
- .forge/acdd/profile.yaml
- .acdd/profile.yaml
- .gitignore
- tests/acdd/milestones/handoff.rs
- docs/milestones/IN-002-contextunity-acdd-one-pass-rename.md
- docs/runbooks/in002-cutover-runbook.md
- docs/runbooks/
- crates/acdd-tasks/src/tasks/profile/tests.rs
- tests/acdd/tasks/seams/tiered_context.rs
- tests/acdd/runner/command_gate_proofs.rs
- tests/acdd/tasks/guidance/profile_defaults.rs
- tests/acdd/milestones/harness.rs
- tests/acdd/milestones/harness/store.rs
- tests/acdd/milestones/harness
symbols: []
status: completed
subtasks:
- subtask_ref: tracked-control-plane-move
  title: Rename the adapter/profile, update package/bin command targeting and ignore rules, and remove tracked old config/profile paths
  status: completed
  evidence: Moved forge-mcp.yaml -> acdd.yaml and .forge/acdd/profile.yaml -> .acdd/profile.yaml; updated .gitignore and test/profile fixtures
- subtask_ref: offline-continuity-rehearsal
  title: Exercise delivered-task evidence, a quiescent state transfer, restart, root verification, recovery rejection, and final handoff in an isolated repository
  status: completed
  evidence: cargo test --workspace --all-features --test acdd milestones::handoff (7 passed, 0 failed)
- subtask_ref: post-delivery-switch-checklist
  title: Record the exact post-delivery cutover and new-controller handoff invocations without mutating a reviewed candidate
  status: completed
  evidence: Updated docs/runbooks/in002-cutover-runbook.md with runnable post-delivery offline switch and rollback checklist
extended_scope:
- tests/acdd/milestones/harness.rs
- tests/acdd/milestones/harness/store.rs
- tests/acdd/milestones/harness
receipt:
  delivery_gate: deliver
  candidate_gate: build
  commit: f644b5a93cee088f9f3f00eef669f5d97fa392f0
  contract_revision: 4
  passed_at: 2026-11-15T04:22:33.645709424+00:00
  evidence:
    command_proof:
      command: rust-check-and-test
      exit_code: 0
      tests_passed: 1
      tests_failed: 0
      log: |
        [rust-check-and-test::tests/acdd/milestones/handoff.rs]
        PASS rust-check-and-test::tests/acdd/milestones/handoff.rs: exit=0 passed=1 failed=0 (114121 ms)
  review:
    review_proof:
      decision: pass
      contours:
        paths:
          applicable: true
          evidence: Verified scope matches initial+extended paths (acdd.yaml, .acdd/profile.yaml, .gitignore, handoff.rs, harness.rs, store.rs, in002-cutover-runbook.md, profile/tests.rs, tiered_context.rs, command_gate_proofs.rs, profile_defaults.rs).
        claims:
          applicable: true
          evidence: control_plane_activation_matches_acdd_paths proves acdd.yaml and .acdd/profile.yaml activation on production paths; cutover.rs removed cleanly.
        concurrency:
          applicable: true
          evidence: Offline cutover rehearsal uses atomic generation/checkpoint ownership with clean candidate_snapshot a69c7e7.
        project_isolation:
          applicable: true
          evidence: All path checks and profile selectors confine to workspace root.
        administration:
          applicable: true
          evidence: Protected Cargo.toml/build.rs untouched in task scope; runbook updated cleanly.
      extended_scope_review:
        approved_paths:
        - tests/acdd/milestones/harness.rs
        - tests/acdd/milestones/harness/store.rs
        - tests/acdd/milestones/harness
        reason_verified: true
      notes: Independent review passed across all 5 contours and extended scope.
  decision: pass
  rollup:
    verified_invariants:
    - 'INV-SCOPE-INTEGRITY: Admit only the explicitly authorized Cargo manifests, identity-only root build.rs and index build-script edits, state/config rename, workflow files, and named implementation roots.'
    - 'INV-ONE-PASS-IDENTITY: Expose contextunity-acdd, acdd, acdd.yaml, .acdd/profile.yaml, acdd-core, acdd-index, acdd-tasks, and ACDD_* without old-name aliases, fallback discovery, dual writes, or automatic legacy migration.'
    - 'INV-CORE-LEAF: Preserve the core leaf and existing crate dependency directions during path and import changes.'
    - 'INV-SELF-HOSTING-CUTOVER: Deliver all tasks with a pinned pre-cutover controller before switching live state; prove the new controller can resume accepted task evidence and hand off before making the switch.'
    - 'INV-PROOF-CONTINUITY: Preserve task identities, accepted receipts, immutable candidate content, checkpoint ownership, and trace bytes across the offline operational cutover.'
    - 'INV-NAVIGATION-PARITY: Rename the MCP reader to acdd_read while preserving its request/result contract, CLI read parity, and the code_map_*, task_*, and ast_grep_search names.'
    - 'INV-MINIMAL-INTERVENTION: Keep path resolution single-pass and companion documentation external; add no rename registry, startup migration, or extra runtime dependency.'
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

Implementation steps: rename the root YAML and profile, update `acdd_profile`
to `.acdd/profile.yaml`, set new state exclusions, change profile package/bin
scope arguments to `acdd-*`/`acdd`, and update `.gitignore` to track the profile
while excluding generated state. Preserve command IDs, gate IDs, proof policy,
roles, output/count extraction, and the full-repository verification command.
An operational execution config retains the old live task DB selection only
until all four tasks are delivered; it is never installed or committed.

Test seam: `cargo test --test acdd milestones::handoff`. The fixture
must use accepted task delivery evidence, not fabricated rows, and exercise the
exact pinned-controller/new-controller sequence below. Preserve the existing
task repository/project identifiers during transfer; changing a directory or
binary is not permission to rewrite task IDs. Prove the final new controller
sees the same accepted tasks, receipts, blackboard, root cursor, checkpoint
ownership, and historical trace content, then records fresh final verification
and archives the milestone. Prove rejection before writes for an occupied
destination or unresolved checkpoint, and prove interruption recovery without
creating a second task store. Use the existing lifecycle harness; keep
subprocesses limited to the dedicated binary-switch smoke seam.

Sensitivity: point the final config at an empty task store or move the state
while a writer remains open; continuity assertions must fail. Restore the old
profile/bin command mapping and prove verification fails. No timing benchmark
or wall-clock promise is admitted.

Incoming seams: runtime/packaging deliveries and explicit controller selection.
Outgoing seams: the sole tracked `acdd.yaml` and `.acdd/profile.yaml`, reviewed
offline-cutover instructions, and successful isolated new-controller handoff.
Negative boundaries: do not execute the live move from a task gate, run the new
controller against an empty live store, bypass a pending checkpoint, rewrite
accepted proof bytes, or add automatic old-path migration to the application.

## Offline self-hosting switch and handoff

The coordinator owns this operational sequence **after** activation task delivery.
The rename changes product defaults once; the temporary pinned execution host
is an orchestration tool, not a second supported product identity.

1. Before wave 1, retain the pinned executable outside build outputs and the
   state tree. Record explicit root, config, profile, and authoritative task DB
   paths. Remember that linked task worktrees resolve the shared task DB under
   the primary Git worktree; moving a milestone-local `.forge` is insufficient.
2. Build and test the renamed executable in task worktrees and isolated fixtures.
   Complete all task reviews and deliveries through the pinned controller with
   explicit operational config/profile. Keep reviewed candidate files immutable.
   Do not delete an input still needed by the pinned host.
3. Require the isolated continuity rehearsal to pass before delivery of the final
   task. Inventory all open writer processes, DBs/WAL/SHM sidecars, advisory
   locks, trace directories, keeper/checkpoint data, and Git-owned checkpoint
   refs. Freeze new claims; close the controller and MCP/daemon writers across
   all affected worktrees. Complete existing checkpoints through the supported
   recovery command before moving anything.
4. Record counts and identities from the accepted task store. Checkpoint/close
   SQLite through supported APIs before transfer. Preserve a read-only backup
   outside the product state tree. Fail closed if the destination contains
   unrelated state; do not merge two databases, copy a live SQLite main file
   without its transaction state, or recursively delete a destination.
5. Move the operational state home once to `.acdd` at the actual owning root and
   each required worktree-local artifact home. The tracked profile already
   occupies `.acdd/profile.yaml`; install operational entries without replacing
   that file. Keep task IDs and accepted evidence immutable. Rebuild disposable
   code indexes/caches with `acdd` instead of trusting old path/config identities.
   Git refs remain subject to their existing ownership checks; do not rename
   refs or hash domains merely because they contain the old word.
6. Start only the new executable with `acdd.yaml` and `.acdd/profile.yaml`.
   Compare authoritative task/receipt/checkpoint identities with the recorded
   pre-switch inventory before admitting a write. Read historical proof bytes
   without translating or forging their contents. If the rehearsal finds an
   indispensable old path reference that the new controller cannot consume,
   stop and return it to the architect; a compatibility resolver is forbidden.
7. Verify delivery continuity, remove only the temporary operational controller
   files after their consumers exit, and confirm the new process creates no
   `.forge` tree. Restart MCP with the new executable and check its tool list.
   If transfer fails before first new write, restore the offline backup using
   the recorded ownership map. After a new write, stop and use supported
   recovery; do not switch a stale backup into service or run both controllers.
8. Run the required documentation scan in the milestone worktree with an admitted
   docs scope, then independent review of its edits. Use `acdd milestone
   handoff-context IN-002-contextunity-acdd-one-pass-rename --target-base main`.
   Bind independent integration review to the exact final content and base.
9. Run `acdd milestone verify IN-002-contextunity-acdd-one-pass-rename
   --target-base main`. Its registered `check_full_repo` must still execute
   commitment integrity, strict all-target/all-feature Clippy, and root
   `cargo test --all-targets`. Run `cargo test -p acdd-tasks --lib` separately.
   Follow [TESTS.md](../../../TESTS.md) for trace-bound and deduplicated counts.
10. Write the exact binding and observed command/count evidence to an ignored
    `.acdd/integration-evidence.json`, then run `acdd milestone handoff
    IN-002-contextunity-acdd-one-pass-rename --evidence-file
    .acdd/integration-evidence.json`. Archive/commit on the milestone branch and
    fast-forward land using the repository's handoff procedure. No commit on
    `main` or remote publication is authorized by this authoring request.

## Acceptance and proof policy

Require compile-clean runtime-red contract proof, focused production seams plus
strict Clippy at each build/review, and the complete handoff checks above. Do
not use compiler failures from half-renamed crates as contract proof. The
pre-task milestone blueprint must provide each task's `implementation_steps`,
inspected `symbol_signatures`, `test_seam`, `incoming_seams`,
`outgoing_seams`, and `negative_boundaries` from this document; the standalone
authoring response uses its supplied smaller proof schema.

Final inventory verifies Cargo metadata has one root package and three renamed
members, only the `acdd` CLI, only the new config/profile and skill locations,
new-only environment behavior, the reader rename, installable release payloads,
and successful final handoff through the renamed controller. Preserve all
declared unchanged contracts. Document justified historical, external, or
non-renamed-tool matches individually; accept no executable old-name fallback.

Authoring validation is structural only. Independent review and `milestone init
--in-place` validate the manifest without asserting that an execution
rehearsal has already run.
