---
title: "Tests and Verification"
doc_type: guide
---

# Tests and Verification

Read this file for repository-wide test gates. Read [tests/AGENTS.md](tests/AGENTS.md)
for test placement, authoring boundaries, public-seam requirements, and benchmarking
isolation.

## Task-level proof

Run the focused test command required by the admitted task contract (e.g. `cargo test --test <target> <seam_filter>` or `cargo test -p <crate> --lib <filter>`). Record the exact command, exit code, passing count, and failing count in the active `build` and `review` evidence. This focused test proof uses the narrowest real production seam that proves the task without forcing slow, unrelated suite execution during task iteration.

For Rust task builds and reviews, run strict Clippy alongside the focused test:
`cargo clippy <scope_target> --all-features -- -D warnings` (or workspace-wide `cargo clippy --workspace --all-targets --all-features -- -D warnings`). Record its exact command and exit code with the build evidence; Clippy contributes no test count.
The repository profile (`.acdd/profile.yaml`) registers `rust-check-and-test` as `cargo clippy {scope_target} --all-features -- -D warnings && cargo test {scope_target} {test_filter}`.
Claim context (`acdd_claim` / `acdd claim`) supplies `workflow_guidance` with the contract seam and indexed covering-test targets; use the scoped command matching the task proof (with `--test <filter>`) and record its exact string. Full unfiltered suite execution (`cargo test --workspace --all-targets`) is deferred to the terminal verification gate.

## Milestone handoff gate

Before terminal `handoff`, run each command from the repository root:

1. `cargo test --test commitment_integrity`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test -p acdd-core --lib && cargo test -p acdd-state --lib` (and `cargo test --workspace --lib` covering all 3 library crates `acdd-core`, `acdd-index`, `acdd-state`, and root `--lib`)
4. `cargo test --workspace --all-targets`
5. `cargo run -- docs check && cargo run -- validate`

Also verify temporary directory cleanup across test runs: compare `/tmp/contextunity-test-*` counts immediately before and after executing tests so shared fixtures (`PersistedSnapshot`, local `Workspace` drops, `RunHarness`, child processes) leave zero leaked directories (`BEFORE == AFTER`).

The root package's `--all-targets` run without `--workspace` does not execute workspace member library unit tests (`acdd-core`, `acdd-index`, `acdd-state`). Record the workspace library result (`cargo test --workspace --lib` and/or `cargo test -p acdd-core --lib && cargo test -p acdd-state --lib`) alongside the integration suite result.

Follow the documentation-agent scan in [AGENTS.md](AGENTS.md#documentation-scan-before-handoff) before final verification. The repository profile's `check_full_repo` runs commands 1–3 (`commitment_integrity`, workspace strict Clippy, and `cargo test --workspace --lib`); run `cargo test --workspace --all-targets` and `cargo run -- docs check && cargo run -- validate` alongside it when verifying terminal delivery. Obtain independent review against the settled diff and target base; any source or documentation change after review invalidates prior verification and requires fresh verification and review.

Use `test-suite-refactor` only when the admitted change splits, replaces, deduplicates, or removes a typed test suite. It is not a general final-verification step.

## Test authoring and fixture invariants

To prevent test-suite bloat, redundant execution storms, and synthetic masking, all test suites MUST adhere to these architectural invariants:

1. **Anti-Fake-Gating & Shared Snapshot Setup**:
   - `#[ignore]` gating (`ACDD_TESTS_GIT=1`) is strictly forbidden as a substitute for optimizing redundant test setups.
   - Matrix tests verifying rejections, invalid receipts, or failure modes MUST NOT re-run multi-gate task/milestone lifecycles from scratch for each scenario row.
   - Establish the base state ONCE (e.g. delivered milestone state) and use lightweight branching (`git switch -c`), worktree branching, or in-memory SQLite snapshots to mutate only the single condition under test per scenario. All rejection scenarios must execute in seconds, not minutes.
   - Do NOT use `.filter(|scenario| git_matrix != ...)` to sweep unoptimized multi-minute loops into the ignored lane while leaving the underlying test unoptimized.

2. **Canonical Fixtures & Builders Over Inline Heredocs**:
   - Do NOT copy-paste raw, multi-line YAML frontmatter and markdown milestone documents (`let source = "---\nid: ...";`) across test functions.
   - Shared fixture variations belong in canonical template files under `tests/acdd/fixtures/` or structured fluent builders (`MilestoneFixtureBuilder`). Tests must reference templates or configure variations via typed builder methods, keeping test files maintainable and concise.

3. **In-Process Engine Seams Over OS Process Spawning**:
   - Integration tests proving run and gate lifecycle logic must invoke the in-process Rust API directly (`StateEngine::execute`, `StateQueryEngine::query`, `RunHarness`).
   - OS-level binary execution (`std::process::Command::new(env!("CARGO_BIN_EXE_..."))`) is restricted exclusively to dedicated CLI smoke tests (1–2 per subcommand) to verify CLI argument parsing and exit codes. Never drive domain matrix permutations through subprocess spawning.

4. **In-Memory Store First & Subprocess Boundary**:
   - Pure state-machine logic, gate transitions, DAG wave ordering, dependency blocking, and blackboard routing must run against in-memory SQLite stores (`RunHarness::in_memory()`) in `< 5ms` without disk files, Git worktrees, or external subprocesses (full `cargo test --test acdd` runs in `< 15s`).
   - Reserve Git worktrees and child-process runner tests (`runner_and_waves.rs`) strictly for tests asserting subprocess lifecycle, signal/timeout cancellation, process recovery, or concurrent worktree filesystem isolation.

5. **Universal Declarative Test Harnesses (`<Domain>Harness`)**:
   - Integration tests must express domain intent, not procedural plumbing.
   - Individual test functions MUST NOT interleave manual Git subprocess scripting (`git checkout`, `git commit`, `git worktree`), raw SQLite mutations (`INSERT INTO blackboard_messages`), or 80-line JSON evidence payloads.
   - Every major cluster must provide a cohesive domain harness (`RunHarness`, `McpTestHarness`, `ExtractorHarness`).
   - All environment lifecycle mechanics, Git commits, evidence synthesis, and blackboard updates must be encapsulated within the respective harness. Keep each test's setup, action, and assertions clear.

6. **Seam-Preserving Functional Decomposition Over Monolithic Mega-Tests**:
   - Functional decomposition MUST NOT degrade production seam coverage into mockist micro-unit tests. Every test must execute genuine component integration pathways end-to-end (real SQLite, real Git commits, real Linker ASTs).
   - Split tests that chain distinct, unrelated scenarios, such as the 1,200-line extractor and 500-line lifecycle monoliths. Treat 50 lines as a prompt to review cohesion and readability, not as an acceptance limit.
   - Use separate, descriptively named `#[test]` functions for distinct capabilities and workflows. Related input variations may share a table when they exercise the same production seam.

7. **Zero-Disk In-Memory Syntax & AST Invariant**:
   - Unit tests for AST extraction, syntax parsing, language semantics, and framework manifest resolution MUST execute purely in-memory (`ExtractorHarness::for_lang(Rust).extract(code)` or in-memory SQLite connections).
   - They MUST NOT allocate physical filesystem tempdirs (`Workspace::new()`), initialize Git repositories, or write physical files to `/tmp` or `.acdd/` unless physical filesystem boundary behavior (such as file locking or symlinks) is the explicit subject of the test.

8. **In-Process MCP Routing Over Subprocess Transports**:
   - Protocol and tool tests in `tests/mcp/` and `tests/acdd/mcp.rs` MUST dispatch JSON-RPC requests directly through the in-process server handler (`server.handle_request(&req)`).
   - Spawning background child processes (`StdioClient::new()`, `mkfifo`, subprocess pipes) is strictly restricted to exactly 1–2 transport smoke tests in `tests/mcp/`. Never drive matrix permutations, tool freshness checks, or response compaction through OS child processes.

9. **Fixture Matrix & Unified `RunHarness` Invariant (`INV-ACDD-RUN-HARNESS`)**:
   - All spec and profile fixtures under `tests/acdd/fixtures/{milestones,plans,workflows}` are audited in `tests/acdd/fixtures_matrix.rs` using `RunHarness` and `validate_profile_document` ([ADR 0003](docs/adr/0003-unified-profile-graph-and-state-machine.md)).
   - Ad-hoc markdown/YAML heredoc copy-pasting, direct procedural bash-like scripting, and external CLI subprocess spawning are strictly banned for state-engine test suites.

10. **Profile-Driven Universal Lifecycle & Worker Independence (`INV-TEST-PROFILE-DRIVEN-LIFECYCLE`)**:
    - Task integration and lifecycle tests must NOT hardcode private gate lists (`&["contract", "build", "review", "deliver"]`).
    - Task advancement must be driven dynamically via the task's active `GateProfile` (`harness.profile().executable_gates()`, `claim_next()`, `advance_to()`, `advance_to_completion()`).
    - The test harness automatically derives isolated synthetic sessions per simulated worker (`test-session-{worker}`) to make in-process tests immune to ambient host environment variables.
    - Dedicated integration tests must verify that an independent gate rejects the accepted predecessor's `worker_id` with `TASK_REVIEW_NOT_INDEPENDENT`, regardless of session metadata, while a distinct worker is not vetoed by missing or equal session metadata.
    - Runner tests must separately verify distinct session identities and fingerprints, process isolation, and retained session reuse on rejection.

11. **Shared Read-Only Indexed Corpus (`INV-TEST-SHARED-READONLY-INDEX`)**:
    - When two or more read-only query, search, or linker tests inspect the same immutable file corpus, share a `std::sync::OnceLock<PersistedSnapshot>` from `tests/common/sqlite.rs`, dropping the builder `Workspace` inside `get_or_init`, and open per-test read-only connections. Use local `Workspace` instances when disk source files are required; never store a `Workspace` in a static.

12. **Integration Binary Consolidation Budget (`INV-TEST-BINARY-BUDGET`)**:
    - Every `.rs` file directly under `tests/` compiles and links a separate integration binary. Register new test modules inside the owning canonical domain binary (`tests/core.rs`, `tests/mcp.rs`, `tests/query_context.rs`, `tests/linker.rs`, `tests/languages.rs`, `tests/acdd.rs`, `tests/commitment_integrity.rs`) rather than creating new standalone `tests/<name>.rs` files.

## Declarative Seam Testing via `<Domain>Harness`: Agent Authoring Guide

When writing or extending tests in this repository, agents MUST follow the **Production Seam Test** architecture driven by declarative `<Domain>Harness` instances.

> [!NOTE]
> For the complete architectural specification, boundary matrix, and Before/After code patterns, read [Test Harness and Production Seam Architecture](docs/architecture/test-harness.md).

### The Seam Test Mental Model
- **Real Production Pipelines (No Mockist Fakes)**: A Seam Test exercises the genuine production path end-to-end (live SQLite schemas, real Git tree hashes, real Engine gates, and AST Linkers). Never mock collaborating subsystems.
- **No Procedural Plumbing in Tests**: Tests express business contracts and verification intent. All repetitive plumbing (creating temp workspaces, `git commit`, `git worktree`, raw SQL `INSERT`, and 80-line JSON evidence builders) is encapsulated inside the respective `<Domain>Harness`.
- **One Clean Scenario per `#[test]`**: Each test function verifies one independent functional scenario on a clean baseline or RAM snapshot. Keep related input variations together when a table makes the contract clearer.

### Available `<Domain>Harness` Implementations
| Harness | Subsystem / Scope | Canonical Fixtures | Key Production Seams |
| :--- | :--- | :--- | :--- |
| `RunHarness` | Unified `GateProfile` (`v2`), DAG gates, claims, verification, blackboard, git ops, runner waves | `tests/acdd/fixtures/{milestones,plans,workflows}/` (`< 15s` full suite) | `StateEngine::execute`, `StateQueryEngine::query`, `validate_profile_document` |
| `ExtractorHarness` | Syntax parsing, AST extraction, fact emission (zero disk I/O) | In-memory code snippets | `languages::rust::extract`, `languages::python::extract` |
| `McpTestHarness` | MCP JSON-RPC protocol, tool routing, response compaction | Real Workspace files, SQLite, and workspace-local extraction cache | Production RMCP server over an in-process duplex stream |

Run the unified ACDD state-engine integration suite with
`cargo test --test acdd` (`< 15s` total across `fixtures_matrix`, `profiles_and_inheritance`, `lifecycle_and_state`, `blackboard`, `verification_and_git`, and `runner_and_waves`).

---

### The Three Canonical Authoring Scenarios

#### Scenario 1: Extending an Existing Capability (Input Variations & Edge Cases)
When verifying a new edge case, diagnostic error code, or parameter variation on an existing seam:
- **Do NOT** copy-paste an entire 100-line test function.
- If the test is table-driven, add **one row** to the existing case table:
  ```rust
  for (scenario, expected_code) in [
      ("proof_unobserved", ErrorCode::TaskProofUnobserved),
      ("not_integrated", ErrorCode::TaskNotIntegrated),
  ] {
      // parameterized assertion against RunHarness
  }
  ```
- If testing a mutation or rejection condition, mutate one argument or state condition on a shared `RunHarness`:
  ```rust
  #[test]
  fn verify_rejects_unclaimed_node_cursor() {
      let harness = RunHarness::in_memory().unwrap();
      let err = harness.verify("t1.build", None).unwrap_err();
      assert_eq!(err.code, ErrorCode::StaleClaimRevision);
  }
  ```

#### Scenario 2: Authoring a New Contract Invariant (New Feature / Rule)
When a task introduces a distinct new behavioral contract or fail-closed invariant:
- Author a dedicated, descriptively named `#[test]` function in the owning domain module.
- Load a canonical fixture via the harness and express only the distinct domain preconditions, action, and assertion:
  ```rust
  #[test]
  fn spec_document_sections_preserve_the_declared_type() {
      let sections = docs::extract_with_details("docs/plans/linear.md", src, 0.0, &[]).unwrap();
      assert!(!sections.is_empty());
  }
  ```

#### Scenario 3: Connecting an Existing Seam to a New Consumer
When a new subsystem emerges (e.g. a new `DomainProvider` or `StateBackend` consumer):
1. **Extend the Harness API additively**: Once the production consumer exists,
   add its invocation to the domain harness so tests share the real wiring.
2. **Author the Integration Seam Test**: Instantiate producer state via the
   harness, execute the consumer, and assert observable state transitions across
   both components without mocking either collaborator.

---

### Non-Negotiable Test Authoring Rules (Anti-Pattern Checklist)
- ❌ **NEVER** write `let source = "---\nid: ...";` in test functions (use `tests/acdd/fixtures/` or typed builder).
- ❌ **NEVER** invoke `std::process::Command::new("git")` or `Command::new(env!("CARGO_BIN_EXE_..."))` in domain tests.
- ❌ **NEVER** execute raw SQL `conn.execute("INSERT INTO ...")` in domain test functions when a reusable store/harness primitive exists.
- ❌ **NEVER** dump static non-domain functions onto `<Domain>Harness` (e.g. `McpTestHarness::seed_git` or `McpTestHarness::edge_rows`) or alias harness types (`use McpTestHarness as Client` / `as Mcp`).
- ❌ **NEVER** inline a single-caller helper as a 100-line `(|| -> Result<_> { ... })()` IIFE block returning a one-off `*Observation` struct, or leave zero-method `*Observation` struct shells in `harness.rs`.
- ❌ **NEVER** run full-DB dumps (`persisted_state`) or $O(N^2)$ sequential cold index rebuilds inside per-row rejection/delta loops when lightweight checks or batched file updates prove the same contract.
- ❌ **NEVER** synthesize 40–80 lines of raw JSON evidence manually (use `default_evidence()` and declarative mutators), or hide setup bugs behind silent `"unavailable"` digest fallbacks in happy-path fixture helpers.
- ❌ **NEVER** combine unrelated workflows into 300-line mega-tests, author multi-hundred-line meta-test suites for test-only helpers, or write tautological enum/struct echo checks and brittle `Cargo.toml`/CI YAML substring assertions.
- ❌ **NEVER** call `Workspace::build()` repeatedly per test when `>= 2` read-only query, search, or linker tests inspect the same immutable fixture corpus (share one read-only index via `OnceLock`).
- ❌ **NEVER** spawn Git worktrees and child-process runner harnesses for pure in-memory DAG scheduling, dependency blocking, or SQLite state-transition assertions (use `RunHarness::in_memory()`).
- ❌ **NEVER** add new top-level `tests/<name>.rs` Cargo test binaries; register new modules inside the owning canonical domain binary.
- ❌ **NEVER** mechanically chop a cohesive single-domain test file (e.g. 800–1,600 lines testing one tool or language surface) or hide test assertions in `*Harness` helpers to chase an arbitrary line count. Decompose multi-subsystem monoliths (>1,500–2,000 lines mixing unrelated contracts such as `tests/mcp/context.rs` or `tests/mcp/tool_evolution/mcp_boundaries.rs`) by domain cohesion, keeping `tests/commitment_integrity/*` under `< 500` lines since its submodules are already partitioned by domain.

## Test lanes

Run the workspace repository suite with `cargo test --workspace --all-targets`.
All current registered scenarios, including Git/history and scale cases, stay
in this lane. The current Rust tree has no `#[ignore]` tests or
`ACDD_TESTS_GIT` / `ACDD_EXTENDED_TESTS` test switches, so there is no separate
ignored or environment-gated lane. Record passed, failed, and ignored counts;
the default run reports zero ignored tests.

Run the core and state-engine library unit tests with:
`cargo test -p acdd-core --lib && cargo test -p acdd-state --lib` (or run all
workspace member libraries together via `cargo test --workspace --lib`).

The unified `acdd` integration suite lives in `tests/acdd/` (`< 15s` total).
Run all modules with `cargo test --test acdd`, or select a single module:
`cargo test --test acdd <module>::` (`fixtures_matrix`,
`profiles_and_inheritance`, `lifecycle_and_state`, `blackboard`,
`verification_and_git`, `runner_and_waves`).

## Cross-checking macOS targets from Linux

For cross-platform boundary verification, record the host locale, the original
`TMPDIR` spelling, filesystem type and observed case behavior with the test
results. Do not override locale or temporary-directory settings to hide aliases.
Native macOS execution remains pending until a macOS runner exercises APFS
case-folding and Darwin live reload; cross-compilation is compilation evidence only.

The boundary failure matrix includes the following independent seams:

| Failure | Test owner |
| --- | --- |
| Busy or incomplete WAL checkpoint, retry, stage and sidecar cleanup obstruction | `tests/core/database_guard.rs` |
| Rollback trust recovery, tampering, malformed or obstructed receipt publication | `tests/mcp/freshness/incremental_freshness.rs` and `tests/mcp/freshness/admission.rs` |
| Escaping and leaf symlinks, excluded paths, case and Unicode spelling | `tests/incremental/scope.rs` and `tests/mcp/freshness/incremental_freshness.rs` |
| Truncated HTTP, installer interruption, timeout and early child exit | `tests/mcp/installer.rs` |
| Optional debug-log obstruction | `tests/core/debug_logging.rs` |

Verify fixture teardown and preservation assertions alongside each failure case;
the existence of a test alone does not establish that it passed on a given host.
The MCP freshness modules are nested under `freshness::freshness_and_admission`;
use `cargo test --test mcp freshness::freshness_and_admission::admission::` and
`cargo test --test mcp freshness::freshness_and_admission::incremental_freshness::`
to select those cases. The shorter `freshness::admission::` and
`freshness::incremental_freshness::` filters select no tests.

Use `rustup` with `zig cc` on Linux hosts to verify `#[cfg(target_os = "macos")]`
compilation and strict Clippy across all crates and integration test targets
before pushing changes that touch platform-gated code.

1. Install the macOS target standard library and ensure `zig` (tested with
   `0.15.2`) is active in `PATH`:

   ```bash
   rustup target add x86_64-apple-darwin
   rustup target add aarch64-apple-darwin
   zig version
   ```

2. Create an executable wrapper (for example `/tmp/zig-macos-cc.sh`) that strips
   `cc`-injected `--target=*-apple-darwin` and `-mmacosx-version-min=*` flags
   before calling `zig cc` for the selected target:

   ```bash
   #!/usr/bin/env bash
   args=()
   for arg in "$@"; do
     case "$arg" in
       --target=x86_64-apple-darwin|--target=aarch64-apple-darwin|--target=arm64-apple-darwin) ;;
       -mmacosx-version-min=*) ;;
       *) args+=("$arg") ;;
     esac
   done
   exec zig cc -target "$ACDD_ZIG_TARGET" "${args[@]}"
   ```

   Use `x86_64-macos` or `aarch64-macos` without a single-component version suffix (such as
   `x86_64-macos.14`), which `zig cc` rejects with
   `InvalidOperatingSystemVersion`, and strip `--target=*-apple-darwin`, which
   `zig cc` rejects with `UnknownOperatingSystem`.

3. Mark the wrapper executable and run Clippy with `CC` and `CXX` pointing to it:

   ```bash
   chmod +x /tmp/zig-macos-cc.sh
   ACDD_ZIG_TARGET=x86_64-macos CC=/tmp/zig-macos-cc.sh CXX=/tmp/zig-macos-cc.sh \
     cargo clippy --target x86_64-apple-darwin --tests --all-features -- -D warnings
   ACDD_ZIG_TARGET=aarch64-macos CC=/tmp/zig-macos-cc.sh CXX=/tmp/zig-macos-cc.sh \
     cargo clippy --target aarch64-apple-darwin --tests --all-features -- -D warnings
   ```

This local cross-check compiles C dependencies (`tree-sitter`, `zstd-sys`,
`libsqlite3-sys`) and catches macOS-only `unused_mut`, `unused_imports`, and
`dead_code` lints in the selected package's test targets. Runtime execution of native macOS
tests (such as `kqueue`, `proc_pidinfo`, and `KERN_PROCARGS2`) remains scoped to
the `macos-15` runner in [.github/workflows/ci.yml](.github/workflows/ci.yml).
