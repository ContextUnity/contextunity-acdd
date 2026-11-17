# ACDD Test Suite — Router and Module Invariants (0.6.0)

This suite proves the `acdd-state` + `acdd-core` integration contracts using
`acdd_state::harness::RunHarness` (`< 15s` wall-clock budget, table-driven
matrices, zero `thread::sleep`, zero `/tmp/contextunity-test-*` leaks).

## Modules

- [fixtures_matrix.rs](fixtures_matrix.rs): Full audit of all 47 fixtures under `fixtures/{milestones,plans,workflows}` split into valid `0.6.0` fixtures vs negative contract fixtures (`ErrorCode`).
- [profiles_and_inheritance.rs](profiles_and_inheritance.rs): Embedded profiles, `inherit` + SHA-256 pinning, `gates_merge: patch`, `when:` expressions, `{test_filter}` quoting, `DomainProvider` capabilities, and `modules:` fail-closed validation.
- [lifecycle_and_state.rs](lifecycle_and_state.rs): `RunHarness` in-memory & disk `StateEngine` / `StateQueryEngine` lifecycle (`Start`, `Draft`, `Claim`, `Subtask`, `ExtendScope`, `Reopen`, `Sync`, `Clear`, read-only zero-DB-creation, `modules.state: false`).
- [blackboard.rs](blackboard.rs): Blackboard topics, recipient routing, pagination, `modules.blackboard: false`, and deferred rollup.
- [verification_and_git.rs](verification_and_git.rs): `Verify`, `Submit`, `proof: contract/command/review`, `git.commit_mode`, `git.integrate`, `Snapshot`/`Rollback`, `TASK_NOT_INTEGRATED`, `MainBranchProtected`, and `archive` / `run_tombstones`.
- [runner_and_waves.rs](runner_and_waves.rs): `StateCommand::Run` workflow execution, retries, fail-closed halt, timeout process-group kill, and role runner env/stdin/session continuity.
- [support.rs](support.rs): Shared test helpers (`make_engine_config`, Git repo seeding, `MockDomainProvider`).
