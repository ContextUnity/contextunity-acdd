# ACDD Test Suite — Router and Module Invariants (0.6.2)

This suite proves the CLI/bridge `acdd-state` + `acdd-core` integration contracts
using `acdd_state::harness::RunHarness` (`< 15s` wall-clock budget, table-driven
matrices, zero `thread::sleep`, zero `/tmp/contextunity-test-*` leaks). Exhaustive
`acdd-state` engine/machine/git_ops/runner integration tests live in
`crates/acdd-state/tests/state_integration.rs` (`cargo test -p acdd-state`).

## Modules

- [fixtures_matrix.rs](fixtures_matrix.rs): Full audit of all fixtures under `fixtures/{milestones,plans,workflows}` split into valid `0.6.2` fixtures vs negative contract fixtures (`ErrorCode`).
- [profiles_and_inheritance.rs](profiles_and_inheritance.rs): Embedded profiles (`base`, `code`, `audit`), `inherit` + SHA-256 pinning, `gates:` vs `gates_patch:`, `when:` expressions, `{test_filter}` quoting, `DomainProvider` capabilities, and `modules:` fail-closed validation.
- [lifecycle_and_state.rs](lifecycle_and_state.rs): `RunHarness` in-memory & disk `StateEngine` / `StateQueryEngine` lifecycle (`Start`, `Draft`, `Claim`, `Subtask`, `ExtendScope`, `Reopen`, `Sync`, `Clear`, read-only zero-DB-creation, `modules.state: false`).
- [blackboard.rs](blackboard.rs): Blackboard topics, recipient routing, pagination, `modules.blackboard: false`, and deferred rollup.
- [cli_worktree_exec.rs](cli_worktree_exec.rs): `-C` / `--root` worktree parity across code/docs and state commands, `ACDD_WORKTREE` / `ACDD_RUN` env inheritance, and `acdd exec` deterministic resolution, exit-code propagation, zero-DB creation, and fail-closed diagnostics.
- [verification_and_git.rs](verification_and_git.rs): `Verify`, `Submit`, `proof: command/review`, atomic plumbing `save.post: commit`, `max_net_loc` budgets, `ArchitecturalDrift`/`rearchitect` session rotation, `TASK_NOT_INTEGRATED`, `MainBranchProtected`, and `archive` / `run_tombstones`.
- [runner_and_waves.rs](runner_and_waves.rs): `StateCommand::Run` workflow execution, retries, fail-closed halt, timeout process-group kill, and role runner env/stdin/session continuity.
- [support.rs](support.rs): Shared test helpers (`make_engine_config`, Git repo seeding, `MockDomainProvider`).
