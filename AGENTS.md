# ContextUnity ACDD — Agent Router

## Instruction Authority (SSOT)

- **Root [`AGENTS.md`](AGENTS.md)**: workspace routing, tool/script boundaries, worker self-claim, Git/worktree permissions, `BUDGET_OVERRIDE` authority, and handoff workflow.
- **Domain/Owner `AGENTS.md` + [`TESTS.md`](TESTS.md) + [`docs/AGENTS.md`](docs/AGENTS.md)**: detailed architecture, `INV-SINGLE-OWNER`, test harness/placement rules, and documentation governance (loaded by scope via `paths.instructions`).
- **Workflow `ClaimResp.guidance` (`.acdd/workflows/default.yaml` <- `code` <- `base`)**: concise gate- and role-specific operational checklists injected on `acdd_claim` / `acdd claim`.
- **Skills (`skills/*/SKILL.md`) & `acdd_guide`**: step-by-step procedures, evidence schemas, and SQLite/AST usage examples on demand.

## Tool Routing (ACDD-First — Mandatory)

Start every code, symbol, architecture, and Markdown documentation lookup with ACDD (registered MCP tools first; when MCP is absent—e.g., `swarm` workers—or a worktree changes `acdd.yaml`/`.acdd/lint/`/workflow schemas ahead of `~/.local/bin/acdd`, use the `acdd` CLI directly, never wrapped inside `lean-ctx:ctx_shell`). **Do NOT use `cat`, `sed`, `head`, `tail`, `sqlite3`, or `lean-ctx` (`ctx_read`) on indexed code or docs, and do NOT default to `read_file`/`rg`/`grep` for symbol or doc discovery.** Permitted targeted exceptions:
- **`rg -n`**: (1) gitignored/unadmitted paths (`.acdd/run/**`, `.acdd/tmp/**`, `/tmp/**`, `website/**`, `scripts/**`), (2) exhaustive literal/comment/leftover sweeps across the whole repo after renames or removals, and (3) fallback when `acdd` is unavailable or fails.
- **`read_file`**: unadmitted/binary/unindexed files, or narrow edit-prep fallback when `acdd_read` is unavailable.

| Intent | MCP (preferred when registered) | Exact CLI (`acdd` — omit `-C` when CWD is already the worktree) |
|---|---|---|
| Docs / ADRs / `AGENTS.md` / specs | `search_docs` -> `get_doc(path_or_id, detail="compact"\|"full", section=...)` | `acdd docs search "<query>"` -> `acdd docs get <path> [--detail compact\|full] [--section "<heading>"]` |
| Orient & symbol discovery | `code_map_overview` -> `code_map_search(detail="compact")` -> `ast_grep_search` | `acdd query overview` -> `acdd query search "<pat>" [--path <dir>] [--kind <k>]` -> `acdd ast grep '<pat>' [--path <dir>]` |
| Relations, callers, tests, removal | `code_map_inspect`, `code_map_explain`, `code_map_impact`, `code_map_analyze`, `code_map_query` | `acdd query inspect\|explain\|impact\|tests\|remove <sel> [--path <p>]`, `acdd query analyze [<target>] [--mode ...]`, `acdd query sql "<SQL>"` |
| Symbol body, outline, diff, literal search | `acdd_read(selector="Sym")` / `mode="outline"\|"delta"\|"search"` (`detail="full"` up to `48 KiB`) | `acdd read --selector <Sym> [<path>\|--path <p>]`, `acdd read <path> --mode outline\|delta\|search --pattern "<pat>" [--detail full]` |
| Hygiene & unified lint | `code_map_lint`, `code_map_query(operation="duplicates"\|"dead_tails", path=...)` | `acdd lint [<target>] [--categories all]`, `acdd query run duplicates --level symbol [--min-lines 5] --path <p>`, `acdd query run dead_tails --path <p>` |
| Built-in tool / workflow help | `acdd_guide(topic=...)` | `acdd guide [code\|query\|docs\|ast\|lifecycle\|acdd]` |
| Active run, gates & blackboard | `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard` | `acdd start\|next\|status\|show\|context\|claim\|verify\|submit`, `acdd extend-scope <node> --path <glob> --reason "<why>"`, `acdd blackboard --action read\|post --topic <t> --author <id> --payload "<msg>" [--recipient-node <cur>]` |

### CLI Efficiency & Execution Rules

- **No repeated `cd /abs/path` or 200-char `-C` prefixes**: When your shell CWD is already the target worktree, omit `-C` completely. From another directory, pass relative `-C .worktrees/<name>` or set `export ACDD_WORKSPACE_ROOT=.worktrees/<name> ACDD_RUN=<run>` once at the top of a batched command.
- **Batch read-only CLI queries**: Combine independent `acdd query ...` / `acdd read ...` / `acdd docs get ...` lookups into a single shell command instead of issuing 10–30 sequential calls.
- **Worktree commands (`acdd exec`)**: Run `git`/`cargo` inside an active worktree via `acdd exec [--run <id|name>] [--worker <worker_id>] -- <program> [args...]` (OMIT `-C` on `acdd exec`).
- **Pre-built binary & schema skew**: Prefer `acdd` on `$PATH` (or pre-built `./target/release/acdd` / `./target/debug/acdd` when the worktree modifies `acdd.yaml`, `.acdd/lint/*.yaml`, or workflow schemas ahead of `~/.local/bin/acdd`); never invoke `cargo run -- ...` for routine queries when a binary exists. After landing schema changes onto `main`, rebuild and reinstall `~/.local/bin/acdd` and run `acdd reload`.
- **Shell & script discipline**: Never run inline Python (`python3 -c` / heredocs) or ad-hoc `.py` edit/verification scripts. Run heavy commands (`cargo build`, `cargo test`, `cargo clippy`, `acdd verify run_tests`, `./scripts/test-macos-ssh.sh`, benchmarks) ONLY in `run_command(WaitMsBeforeAsync: 500)`; reserve `ctx_shell` strictly for lightweight non-`acdd` shell commands.

## Read before work

- Read [documentation instructions](docs/AGENTS.md) before documentation changes.
- Read [architecture](docs/architecture/README.md) and [decisions](docs/adr/README.md) before structural changes.
- Read [src/AGENTS.md](src/AGENTS.md) and the crate instructions ([`acdd-core`](crates/acdd-core/AGENTS.md), [`acdd-spec`](crates/acdd-spec/AGENTS.md), [`acdd-index`](crates/acdd-index/AGENTS.md), [`acdd-state`](crates/acdd-state/AGENTS.md)) before source code changes.
- Read [roadmap](ROADMAP.md) and [milestones](docs/milestones/README.md) for admitted direction and commitments.
- Read [tests/AGENTS.md](tests/AGENTS.md) for test placement and authoring boundaries.

## Routes

- Task & standalone checkpoints: Use `acdd snapshot save|restore|list|clear [<target>] [--all] [--rev N] [--run RUN] [--delete] [--older-than DUR] [--worktree PATH]` or `acdd_state(action: "snapshot")`; see [task snapshots](docs/reference/tasks.md#task-snapshots).

- Runtime behavior and setup: [README.md](README.md) and [docs](docs/).
- Architecture and modularity: [src/AGENTS.md](src/AGENTS.md), [architecture](docs/architecture/README.md), and [decisions](docs/adr/README.md).
- Interfaces: [configuration and tools](docs/reference/README.md), [task operations](docs/reference/tasks.md), [ACDD navigation](docs/reference/acdd.md), and [CLI commands](docs/reference/cli.md).
- Operations: [runbooks](docs/runbooks/README.md).
- Planning & live DAG waves: Use `acdd roadmap [spec_id] [--spec NAME] [--ready] [--active] [--tasks]` alongside [roadmap](ROADMAP.md) and [milestones](docs/milestones/README.md). Update [roadmap](ROADMAP.md) in the same change whenever authoring new milestones or refining milestone scope, sequencing, or release lines with the user.
- Verification: [required gates](TESTS.md) and [test invariants](tests/AGENTS.md).
- Canonical ownership & architecture rules: Enforce `INV-CORE-LEAF`, `INV-STATE-NO-INDEX`, `INV-STATE-BRIDGE-BOUNDARY` ([ADR 0001](docs/adr/0001-crate-dag-and-layer-boundaries.md)), `INV-STATE-PURE-MACHINE`, `INV-STATE-SQLITE-CONFINED` ([ADR 0003](docs/adr/0003-unified-profile-graph-and-state-machine.md)), `INV-SINGLE-OWNER`, `INV-GIT-SINGLE-OWNER`, and `INV-NO-DEAD-CODE-SHIMS` ([modularity](docs/architecture/modularity.md#single-owner-and-modularity-invariants)); search existing owners across adjacent crates/formatters (`crates/*` vs `src/format/`, `acdd-core/path_policy.rs`) via `code_map_search` / `ast_grep_search` before authoring local grouping/matching/walk helpers, inspect already-populated DTO/domain fields before re-deriving data downstream, split production files into directory modules once approaching `~600-800` LOC instead of piling onto monoliths, and escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before adding any new subsystem or exceeding `~80-100` net production LOC for a defect fix.
- Mandatory clone & dead-tail hygiene: Before submitting `build` or passing `review` / `integration_review`, run `code_map_query(operation="duplicates", path=<scope>)` and `code_map_query(operation="dead_tails", path=<scope>)` (workspace-wide without `path`/`target` for `integration_review`; CLI equivalents `acdd query run duplicates` / `acdd query run dead_tails`), plus `acdd lint` (`code_map_lint`). Builders and independent reviewers must eliminate or reject introduced `>= 5`-line clones, `phantom_tested` dead code, `dead_tails`, `dangling_calls`, and lint errors without temporary exclusion lists.
- ACDD tool discovery and task operations: load the shared `acdd` skill.
- Follow the [Blackboard procedure](skills/acdd/SKILL.md#blackboard) for claim context, progress reports, and gate or agent handoffs.

Use Blackboard `notes` for non-obvious technical context, not routine gate
status, passing tests, or SHAs that add no context. Before rejecting a review,
post structured `findings` with concrete remediation, then include the message
ID in the required typed rejection findings; see the linked procedure.

Follow the active `WorkflowDef` (`.acdd/workflows/default.yaml`) and `ClaimResp` (`contract` and `guidance`) for gate sequence, proof policy, roles, models, and contours (`acdd_state` is active-run-only; use CLI `acdd roadmap` / `acdd draft` for spec planning and drafting). Repository roles in `.acdd/workflows/default.yaml` (`code`) use `mode: subagent` without shell `runner:` commands, so coordinators MUST NOT call `acdd run` (`WORKFLOW_RUNNER_REQUIRED`); orchestrate gates explicitly via `claim`/`verify`/`submit`, completing `blueprint_synthesis.synthesize` -> `blueprint_synthesis.review` before claiming tasks. Task builds and reviews run scoped seam tests and strict Clippy without forcing the full suite; follow [TESTS.md](TESTS.md) for verification commands and counts.

The repository ACDD workflow registers `rust-check-and-test` as strict Clippy, scoped tests (`{scope_target}` + `{test_filter}`), and `cargo test --test commitment_integrity`. For automated gates (`build`), run `acdd verify <node_cursor> --test <test_path::test_fn>` (or MCP `acdd_verify`), then submit the stage via `acdd submit <node_cursor> --action pass` (or `acdd_submit`) without manual evidence assembly. Delivery infers proof from the accepted candidate tree. Review and schema-defined gates provide their declared typed evidence payload (`review_evidence` / `custom_evidence`). Write any temporary agent or workflow scratch files (inventories, baselines, scratch JSON/notes) under `.acdd/tmp/` rather than directly in `.acdd/` (except engine-managed files).

- **Hotfix workflow (`.acdd/workflows/hotfix.yaml`)**: For localized bugfixes that do not need a milestone spec, `blueprint_synthesis`, or `integration_review`, start an ephemeral run via `acdd start --goal "<summary>" --workflow hotfix [--scope <path> ...]` (add `--worktree` when isolated branch execution is needed) and run `goal.build` -> `goal.review` -> `goal.deliver` -> `run_tests` -> `handoff` (routing to `polish` on `run_tests` or `handoff` failure).

<!-- acdd:agents-verification:start -->
When authoring contracts or testing capabilities:
- Follow [`tests/AGENTS.md`](tests/AGENTS.md) and [`TESTS.md`](TESTS.md) for test placement inside existing canonical binaries (`tests/core.rs`, `tests/mcp.rs`, `tests/query_context.rs`, `tests/linker.rs`, `tests/languages.rs`, `tests/acdd.rs`, `tests/commitment_integrity.rs`, `crates/acdd-state/tests/state_integration.rs`), `RunHarness::in_memory()` / `PersistedSnapshot` reuse, table-driven contract matrices, zero review-fix test bloat, and zero `/tmp/contextunity-test-*` leaks (`BEFORE == AFTER`).
- **Build seam proof**: Author compiling, passing scoped tests directly in `build` (`exit_code == 0` and `max_net_loc` budgets; `.acdd/workflows/default.yaml` configures `code.lint` + `rust-check-and-test`, while `code.seam` AST reachability remains active in the embedded `code` workflow and opt-in overrides) without separate runtime-red `contract` steps.
- **Autonomous scope extension**: Extend scope autonomously via `acdd extend-scope` / `acdd_state(action: "extend_scope")` within admitted roots to unblock implementation or verification without halting for human permission.
<!-- acdd:agents-verification:end -->

<!-- acdd:contract-synthesis:start -->
## Architectural Blueprint Synthesis Gate (`blueprint_synthesis`)

`blueprint_synthesis` (`synthesize` (role: `architect`) $\to$ `review` (role: `independent_reviewer`)) runs before tasks. Load [`contract-planning`](skills/contract-planning/SKILL.md) and:
1. Keep blueprints strictly at the **architectural seam level** (canonical owners, layer boundaries, invariants, `incoming_seams`/`outgoing_seams`, `negative_boundaries`, `max_net_loc <= ~400-500` prod LOC, behavioral `verification_seam`s) — zero pseudocode, exact CLI flag recipes, or raw SQL in prose. Run `code_map_impact` for breaking changes before setting `scope:`, separate unrelated `AC-*`/subsystems across tasks, verify live file paths vs directory modules, keep milestone invariants aligned 1:1 with ADRs, and escalate/split unrelated upstream gaps rather than inflating task scope.
2. Keep `protected_paths` (`Cargo.toml`, `crates/*/Cargo.toml`, `.acdd/workflows/**`) off-limits without explicit authorization.
3. Post a concise architectural decision on `acdd_blackboard` (`topic: decisions`) and materialize per-task slices (`proof.tasks[<task_ref>]`) before submitting. Independent blueprint reviewers verify architecture, owners, scopes, and seams — never demand pseudocode or low-level code recipes during `blueprint_synthesis.review`.
<!-- acdd:contract-synthesis:end -->

## Documentation scan before handoff

Before terminal handoff, launch a documentation agent in the run worktree.
Give it the spec manifest, the final diff against the selected target base, the
accepted task evidence, and an explicit admitted documentation write scope
(including `website/` whenever user-facing docs, CLI/MCP, workflows, or
configuration change). Require it to load `agent-instructions`, read
`docs/AGENTS.md` and affected owner instructions, and reconcile behavior,
CLI/MCP, configuration, workflows, architecture, invariant descriptions, skills,
examples, and `website/` (`website/pages/`, `website/templates/`,
`website/sync-docs.py`) with live code/tests. Run `rg -n` for removed/renamed
config keys, lens names, and workflow identifiers across `README.md`,
`AGENTS.md`, `docs/AGENTS.md`, `docs/adr/`, `docs/milestones/AGENTS.md`, and
`website/pages/*.md.in` (`sync-docs.py` does not rewrite hand-authored `.md.in`
prose outside sync markers). Update affected pages; explain
when no update is needed. Preserve task-level documentation co-evolution. Route
source defects to their task owner and architecture decision changes to owner
approval. Post the scanned content binding, updates, checks, and unresolved
findings on `acdd_blackboard`. Submit the documentation edits for independent
review, then run the final checks in `TESTS.md` (including
`cargo run -- docs check`, `cargo run -- validate`, and
`./website/build.sh --check` covering `sync-docs.py && zola build && zola check`).
Repeat the affected scan if implementation changes afterward. The coordinator
launches the agent; this step adds no ACDD gate or runtime launcher.

## Worktrees and Git permissions

Worktrees live at `.worktrees/<run_name>` for isolated runs and `.worktrees/<run_name>-<task_ref>` for concurrent tasks.

### Permitted Git operations inside active ACDD workflows

- Task commits:
  - With `save.post: commit`, ACDD creates the atomic plumbing delivery commit on `acdd submit` and stamps `Acdd-Claim`; do not create duplicate commits.
  - Candidate immutability: Keep accepted candidate trees (`save.immutable: true`) immutable. Rebind candidate drift via `acdd reopen` or confined Git restore, renew required verification (`acdd verify`), and obtain independent review before a terminal receipt. Delivery fails closed (`TASK_CANDIDATE_MISMATCH`) on un-rebound drift; follow the [task reference](docs/reference/tasks.md) for recovery.
- History integration: Keep run branch history strictly linear (one `deliver` commit per task, optional `polish` commit, and one `handoff` commit). `acdd run` lands completed task branches via `rebase-ff` (`integrate_task_branch`); manual worktree runs MUST also integrate via linear `rebase-ff` (`git rebase <run-branch>` in the task worktree followed by `git merge --ff-only <task-branch>` in the run worktree) — never create non-FF `Merge branch 'task/...'` commits or ad-hoc manual commits on the run branch outside `deliver` / `polish` / `handoff`. Terminal verification confirms content equivalence by Git `TreeOid` (`verify_tasks_integrated` / `TASK_NOT_INTEGRATED`). Before handoff, obtain independent `integration_review` and repository verification bound to the final content; follow the task and CLI references.
- Branch integration: Integrate delivered task branches into the run branch, delete temporary task branches, and remove task worktrees (`git worktree remove <worktree-path>`).

### Terminal Run Verification and Handoff Procedure

When every task in the run is completed or cancelled:

1. **Absorb Polish & Verify Repository Gates (`run_tests`)**:
   Commit all working-tree polish in the run worktree and run `acdd verify run_tests` (which executes `.acdd/workflows/default.yaml`'s `check_full_repo` via `./scripts/check-full-repo.sh`: sequential `commitment_integrity`, workspace strict Clippy, `docs check`, and `validate`, followed by parallel `cargo test --workspace --all-targets` on Linux and `./scripts/test-macos-ssh.sh` on the configured macOS SSH host; `polish` runs fast `polish_check` without duplicating full Linux + macOS test suites). Verify `/tmp/contextunity-test-*` `BEFORE == AFTER` locally (remote macOS leak checks run inside `scripts/test-macos-ssh.sh`). Never assemble manual `ssh`/`rsync` commands for macOS verification; always invoke `acdd verify run_tests` or `./scripts/test-macos-ssh.sh`.

2. **Submit `run_tests` and Independent `integration_review`**:
   Submit `acdd submit run_tests --action pass`, claim and complete `integration_review` with an independent reviewer (`review_evidence` across declared contours), then submit `acdd submit handoff --action pass` (which runs `save.post: commit`, `archive: true`, `prune: true`, moving the completed spec to `archive/` and pruning the active run into `run_tombstones`).

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

- **Worker self-claim, navigation & thin coordinator prompt (mandatory for `mode: subagent`)**: The parent/coordinator session MUST NOT call `acdd_claim` / `acdd claim` on a subagent's behalf, MUST NOT pre-author code recipes or `review_evidence` JSON payloads, and MUST launch workers with only `(run, node_cursor, worker_id, worktree)` (plus the pre-built binary path only if a worktree changed schema ahead of `$PATH` `acdd`). Instruct every worker to:
  1. Call `acdd_claim` / `acdd claim` first to obtain `ClaimResp.worktree` (omit `--worktree` on `managed` task gates).
  2. Index/sync that worktree once (`acdd -C <wt> build` or MCP `worktree`), and read `paths.instructions`, `ClaimResp.contract`, `ClaimResp.guidance`, `pre_task_slice`, `unresolved_findings`, and `blackboard`.
  3. Navigate and read indexed code/docs first via ACDD MCP or `acdd` CLI (`acdd query ...`, `acdd read --selector ...`, `acdd docs get|search`, `acdd ast grep`, `acdd lint`), batching independent read-only CLI calls — never using `cat`/`sed`/`sqlite3` on indexed files or wrapping `acdd` in `ctx_shell` (reserve `rg -n` strictly for unadmitted paths, exhaustive rename/removal leftover sweeps, or ACDD fallback; reserve `read_file` for unadmitted files or narrow edit-prep fallback).
  4. Omit `-C` when already in `ClaimResp.worktree` (or set `ACDD_WORKSPACE_ROOT`/`ACDD_RUN` once), and run `git`/`cargo` via `acdd exec [--worker <id>] [--run <run>] -- <cmd>` (OMIT `-C` on `acdd exec`).
- **Round-1 builder discipline**: Before editing, run workspace-wide `code_map_impact` / `ast_grep_search` across `archive`/`sync`/`restore`/examples/sibling docs so no caller or doc is missed; split modules cleanly past ~600-800 non-test LOC along domain seams; enforce UTF-8 `is_char_boundary` on byte-index string slicing and fail-closed empty-scope checks; and eliminate duplicate walkers/parsers by reusing canonical owners.
- **Builder continuity, context hygiene & `BUDGET_OVERRIDE` authority**: Reuse the same builder worker or `codex exec resume <SESSION_ID>` on a first lightweight `review` rejection when its session context is still compact (`< ~100K` tokens / small-to-medium diff). When a builder's context is already heavy (`>= ~100K` tokens or large multi-file tasks) or on a repeated (`>= 2nd`) rejection, spawn a **fresh** builder session instead (and escalate or upgrade the worker model on repeated `>= 2` multi-boundary rejections) — the worktree `git diff` plus `ClaimResp.unresolved_findings` and `acdd_blackboard` preserve full state without carrying stale context or confirmation bias. On any rejection, load `close-review-findings`, trace the full owner path across all ingress surfaces (`CLI`, `MCP`, internal handlers), and run a live `CLI`/`MCP` smoke check against `--state-db` (or a stateful fixture) before submitting `polish`. A `builder` MUST NEVER self-approve `BUDGET_OVERRIDE`; escalate to coordinator/architect (`acdd_blackboard` `decisions` or `--action escalate`) when clean shared logic genuinely exceeds `max_net_loc`.
- **Independent review**: Instruct an independent reviewer (distinct `worker_id`, enforced by `independent_from`) to self-claim and submit `review` / `integration_review`, check every spec `AC` 1:1, verify `BUDGET_OVERRIDE` authority vs measured net LOC, run `code_map_query` (`duplicates` & `dead_tails`; workspace-wide on `integration_review`), verify non-fallback fast-path proofs, and route repeated multi-surface defects to `architectural_drift` (`rearchitect`).
