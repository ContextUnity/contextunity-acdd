# ContextUnity ACDD — Agent Router

## Read before work

- Read [documentation instructions](docs/AGENTS.md) before documentation changes.
- Read [architecture](docs/architecture/README.md) and [decisions](docs/adr/README.md) before structural changes.
- Read [src/AGENTS.md](src/AGENTS.md) and the crate instructions ([`acdd-core`](crates/acdd-core/AGENTS.md), [`acdd-index`](crates/acdd-index/AGENTS.md), [`acdd-state`](crates/acdd-state/AGENTS.md)) before source code changes.
- Read [roadmap](ROADMAP.md) and [milestones](docs/milestones/README.md) for admitted direction and commitments.
- Read [tests/AGENTS.md](tests/AGENTS.md) for test placement and authoring boundaries.

## Routes

- Runtime behavior and setup: [README.md](README.md) and [docs](docs/).
- Architecture and modularity: [src/AGENTS.md](src/AGENTS.md), [architecture](docs/architecture/README.md), and [decisions](docs/adr/README.md).
- Interfaces: [configuration and tools](docs/reference/README.md), [task operations](docs/reference/tasks.md), [ACDD navigation](docs/reference/acdd.md), and [CLI commands](docs/reference/cli.md).
- Operations: [runbooks](docs/runbooks/README.md).
- Planning: [roadmap](ROADMAP.md) and [milestones](docs/milestones/README.md).
- Verification: [required gates](TESTS.md) and [test invariants](tests/AGENTS.md).
- ACDD tool discovery and task operations: load the shared `acdd` skill.
- Follow the [Blackboard procedure](skills/acdd/SKILL.md#blackboard) for claim context, progress reports, and gate or agent handoffs.

When a symbol's unique name is already known, read it directly with
`acdd_read(selector="SymbolName")`; add `path="..."` to restrict resolution.
Follow any source continuation to obtain the complete digest-verified body. Keep
`ast_grep_search` and `code_map_*` (preferring `detail: "compact"` on searches)
for AST patterns and graph queries; `rg -n` is allowed for substring/error/config
searches across directories, and `read_file` is allowed for reading large file
slices (`>120` lines).

Use Blackboard `notes` for non-obvious technical context, not routine gate
status, passing tests, or SHAs that add no context. Before rejecting a review,
post structured `findings` with concrete remediation, then include the message
ID in the required typed rejection findings; see the linked procedure.

Use ACDD MCP tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`) or the flat `acdd` CLI (`start`, `claim`, `verify`, `submit`, `next`, `status`, `show`, `integrate`, `run`, `tool`, `blackboard`) on the surface that provides the needed operation. Follow the active `GateProfile` (`.acdd/profile.yaml`) and the `workflow_guidance` returned by `acdd_claim` / `acdd claim` for gate sequence, proof policy, roles, models, review contours, and profile-specific behavior. Repository roles in `.acdd/profile.yaml` (inherited from `code`) use `mode: subagent` without shell `runner:` commands, so coordinator sessions MUST NOT call `acdd run` (`WORKFLOW_RUNNER_REQUIRED`); orchestrate gates explicitly via `acdd_claim` / `acdd_verify` / `acdd_submit` (or CLI equivalents) using subagents, swarm workers, or `codex exec` background tasks, completing `blueprint_synthesis.synthesize` $\to$ `blueprint_synthesis.review` before claiming tasks. Task builds and reviews run the scoped task seam test and strict Clippy without forcing the full unfiltered suite during iteration. Follow [TESTS.md](TESTS.md) for exact task and milestone verification commands and count rules.

The repository ACDD profile registers `rust-check-and-test` as strict Clippy followed by tests (`{scope_target}` + `{test_filter}`). For automated gates (`contract`, `build`), run `acdd verify <node_cursor> --test <test_path::test_fn>` (or MCP `acdd_verify`), then submit the stage via `acdd submit <node_cursor> --action pass` (or `acdd_submit`) without manual evidence assembly. Delivery infers proof from the accepted candidate snapshot. Review and schema-defined gates provide their declared typed evidence payload (`review_evidence` / `custom_evidence`). Write any temporary agent or workflow scratch files (inventories, baselines, scratch JSON/notes) under `.acdd/tmp/` rather than directly in `.acdd/` (except engine-managed files).

<!-- acdd:agents-verification:start -->
When authoring contracts or testing capabilities:
- **Table-driven contract matrices**: Use a single table-driven `#[test]` (`for case in cases`) — sharing one base workspace, cold index snapshot, or harness built once outside the loop — whenever testing homogeneous input, error, alias, spelling, hook, or boundary variants of the same contract or seam. Do NOT clone heavy `#[test]` functions or call `Workspace::new()` / `.build()` / `InstallerHarness::new` / `McpTestHarness::new` / subprocess spawns per iteration unless each row genuinely tests a distinct cold-build initial filesystem layout.
- **Dedicated domain tests**: Author a separate `#[test]` in the owning domain module ONLY for a genuinely distinct domain contract, independent workflow, or orthogonal failure seam — never to split a matrix of variations of the same rule, and never by appending unrelated requirements into an existing workflow test.
- **Shared read-only fixtures (`PersistedSnapshot`)**: NEVER store `Workspace` or `TempDir` inside a `static OnceLock` / `LazyLock` (statics skip `Drop` on process exit and leak `/tmp/contextunity-test-*`). Share read-only indexed state via `std::sync::OnceLock<PersistedSnapshot>` (`tests/common/sqlite.rs`), dropping the builder `Workspace` inside `get_or_init`, or create local `Workspace` instances when disk source files are required.
- **In-process state vs subprocess harnesses**: Prove pure DAG scheduling, gate ordering, transitions, and blackboard routing in-process (`RunHarness::in_memory()` / `StateEngine`); reserve Git worktrees and child-process runner tests (`tests/acdd/runner_and_waves.rs`) for genuine process, signal, or worktree isolation contracts (`tests/acdd` runs `< 15s`).
- **Zero new root test binaries & zero tempdir leaks**: Register new integration test modules inside the owning canonical domain binary (`tests/core.rs`, `tests/mcp.rs`, `tests/query_context.rs`, `tests/linker.rs`, `tests/languages.rs`, `tests/acdd.rs`, `tests/commitment_integrity.rs`). Never add new top-level `tests/*.rs` binaries, tautological enum/struct echo checks, brittle manifest/CI YAML substring checks, or helper meta-test bloat, and ensure tests leave zero leaked `/tmp/contextunity-test-*` directories.
- **Zero review-fix test bloat**: Do NOT spawn standalone `#[test]` functions or extra `Workspace` / `McpTestHarness` instances to close P1/P2 review findings. Fix the mechanism via generalization so the defect class disappears; add at most a row to the owning table-driven contract test ONLY if a real external contract boundary was missing (net new `#[test]` budget during review remediation is zero).
- **Runtime-red contract proof**: Contract tests must compile cleanly and produce runtime assertion failures for `contract` gate proof.
- **Autonomous scope extension**: Agents should extend scope autonomously via `acdd extend-scope` / `acdd_state(action: "extend_scope")` to include necessary files, fixtures, tests, and documentation within admitted roots to unblock implementation or verification without halting execution or requesting human permission.
<!-- acdd:agents-verification:end -->

<!-- acdd:contract-synthesis:start -->
## Architectural Blueprint Synthesis Gate (`blueprint_synthesis`)

The initial container of the workspace profile is `blueprint_synthesis` (`synthesize` (role: `architect`) $\to$ `review` (role: `independent_reviewer`)).
Load and execute the [`contract-planning`](skills/contract-planning/SKILL.md) skill (with [`ADDON.md`](skills/contract-planning/ADDON.md)) during spec authoring and blueprint synthesis:
1. Architectural Viability Audit: Critically evaluate design appropriateness and minimal complexity (Occam's razor).
2. ADR & Invariant Reconciliation: Reconcile proposals against `docs/adr/`, `docs/architecture/`, and active repository invariants before defining tasks.
3. Risk & Ambiguity Discovery: Probe for vague completion criteria, operational pitfalls, and failure semantics.
4. Constructive Improvement: Synthesize simpler, durable designs and leverage existing patterns before implementation begins.
5. Broad architectural exploration via ACDD MCP (`code_map_*`, `ast_grep_search`, `acdd_read`).
6. Minimal intervention principle (no duplicate registries or parallel tables).
7. Scope integrity (protected files `Cargo.toml`, `crates/*/Cargo.toml`, `.acdd/profile.yaml` are strictly off-limits without explicit authorization).
8. Recording the architectural decision on `acdd_blackboard` before submission.
9. Establishing per-task blueprint slices (`proof.tasks[<task_ref>]`), cross-task seam boundaries (`incoming_seams`, `outgoing_seams`), and invariant specifications materialized by the engine and injected automatically on `acdd_claim` / `acdd claim`.
<!-- acdd:contract-synthesis:end -->

## Documentation scan before handoff

Before terminal handoff, launch a documentation agent in the run worktree.
Give it the spec manifest, the final diff against the selected target base, the
accepted task evidence, and an explicit admitted documentation write scope.
Require it to load `agent-instructions`, read `docs/AGENTS.md` and affected owner
instructions, and reconcile behavior, CLI/MCP, configuration, workflows,
architecture, invariant descriptions, skills, and examples with live code/tests.
Update affected pages; explain when no update is needed. Preserve task-level
documentation co-evolution. Route source defects to their task owner and
architecture decision changes to owner approval. Post the scanned content binding,
updates, checks, and unresolved findings on `acdd_blackboard`. Submit the
documentation edits for independent review, then run the final checks in
`TESTS.md` (including `cargo run -- docs check` and `cargo run -- validate`).
Repeat the affected scan if implementation changes afterward. The coordinator
launches the agent; this step adds no ACDD gate or runtime launcher.

## Worktrees and Git permissions

Worktrees live at `.worktrees/<run_name>` for isolated runs and `.worktrees/<run_name>-<task_ref>` for concurrent tasks.

### Permitted Git operations inside active ACDD workflows

- Task commits:
  - Depending on `git.commit_mode` (`plumbing | cli | manual`), ACDD either creates the delivery commit on `acdd submit` (`save.post: commit`) and stamps `Acdd-Claim`, or verifies `HEAD^{tree}` against the candidate tree; do not create duplicate commits.
  - Snapshot immutability: Keep accepted snapshots (`save.immutable: true`) immutable. Rebind candidate drift via `acdd reopen` or `acdd rollback`, renew required verification (`acdd verify`), and obtain independent review before a terminal receipt. Delivery fails closed (`TASK_CANDIDATE_MISMATCH`) on un-rebound drift; follow the [task reference](docs/reference/tasks.md) for recovery.
- History integration: Preserve accepted task proofs while integrating branches. `acdd integrate` supports `manual | rebase-ff | ff-only | no-ff | squash` and checks content equivalence by Git `TreeOid` (`TASK_NOT_INTEGRATED` until task deltas are present in the run worktree). Before handoff, obtain independent `integration_review` and repository verification bound to the final content; follow the task and CLI references.
- Branch integration: Integrate delivered task branches into the run branch (`acdd integrate` or manual merge/rebase), delete temporary task branches, and remove task worktrees (`git worktree remove <worktree-path>`).

### Terminal Run Verification and Handoff Procedure

When every task in the run is completed or cancelled:

1. **Absorb Polish & Verify Repository Gates**:
   Commit all working-tree polish in the run worktree, run `acdd verify verify` (which executes `.acdd/profile.yaml`'s `check_full_repo`: `commitment_integrity`, workspace strict Clippy, and `cargo test --workspace --lib` across `acdd-core`, `acdd-index`, `acdd-state`, and root `--lib`), then run the remaining [TESTS.md](TESTS.md#milestone-handoff-gate) gates:
   - `cargo test --workspace --all-targets`
   - `cargo run -- docs check && cargo run -- validate`
   - Verify `/tmp/contextunity-test-*` `BEFORE == AFTER`.

2. **Submit `verify` and Independent `integration_review`**:
   Submit `acdd submit verify --action pass`, claim and complete `integration_review` with an independent reviewer (`review_evidence` across declared contours), then submit `acdd submit handoff --action pass` (which runs `save.post: commit`, `archive: true`, `prune: true`, moving the completed spec to `archive/` and pruning the active run into `run_tombstones`).

3. **Run Landing**:
   In the primary worktree (`main`), fast-forward the run branch, remove the run worktree, and delete the run branch:
   `git merge --ff-only <run-branch>`
   `git worktree remove .worktrees/<run_name>`
   `git branch -d <run-branch>`

### Operations requiring explicit user approval

- Commits on `main`: Do not create Git commits directly on `main` without explicit user approval.
- Dangerous Git operations: History rewrites (`git reset --hard`, destructive rebases, force pushes), branch deletion outside task/milestone cleanup, and repository-level state clearing require explicit user approval.
- Remote publications: `git push`, tagging, releases, and credential publication require explicit user approval.

Do not run benchmarks without explicit user approval. Read [benchmark instructions](benchmarks/AGENTS.md) before benchmarking.

## Versioning and Release Notes

- **Mandatory Changelog Update**: Whenever bumping crate versions in `Cargo.toml` / workspace member manifests or preparing a release tag, update [`CHANGELOG.md`](CHANGELOG.md) in the same change with user-visible additions, behavioral changes, fixes, and migration/breaking notes under `## [<version>] - YYYY-MM-DD` (moving relevant entries out of `[Unreleased]`).
- **Workspace Version Sync**: Keep `Cargo.toml`, `crates/*/Cargo.toml`, installation guides (`INSTALL.md`), and workspace contract tests synchronized to the same version.

## Worker Orchestration & Background Reviews

Execute `mode: subagent` gates via native subagents, swarm workers, or background `codex exec` processes (`run_command` with `WaitMsBeforeAsync: 500`, reactive wakeup without polling):

- **Builder continuity**: Reuse the same worker or `codex exec resume <SESSION_ID>` session when routing `review` rejection findings back to `build`.
- **Independent review**: Claim and submit `review` / `integration_review` with a distinct `worker_id` (enforced by `independent_from`) and evaluate all criteria defined in `.acdd/profile.yaml` and `acdd_claim` guidance.
