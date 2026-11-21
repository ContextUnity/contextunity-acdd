---
title: "Source Code Architecture — Modularity and Invariants"
doc_type: architecture
---

# Source Code Architecture — Modularity and Invariants

This document defines the architectural layering, import boundaries, modularity invariants, and canonical helper ownership for all code in `src/`.

## 1. Architectural Layers and Import Boundaries

The workspace operates as **3 library crates (`acdd-core`, `acdd-index`, `acdd-state`)** plus **the root host crate `contextunity-acdd` (`src/`)** in a strict unidirectional hierarchy:

```text
  src/cli (clap) ─────────┐
  src/mcp (19 static      ├──► src/state_bridge (CodeDomainProvider, DocsDomainProvider,
       + plugin tools)    │        │              + external DomainProvider extension pattern)
  src/format              │        ▼                          ▼
                          └──► crates/acdd-state          crates/acdd-index
                                ├── machine/ (100% Pure)   (rusqlite code-map,
                                ├── usecases/               tree-sitter, docs)
                                ├── git_ops/, runner/         │
                                └── store/sqlite/             │
                                       │                      │
                                       └──────────┬───────────┘
                                                  ▼
                               crates/acdd-core (config, ids, errors, path_policy,
                                                 schema, ports, fs, git, sys, lock, format, telemetry)
```

Crate-local instructions:
- [`crates/acdd-core/AGENTS.md`](../crates/acdd-core/AGENTS.md)
- [`crates/acdd-index/AGENTS.md`](../crates/acdd-index/AGENTS.md)
- [`crates/acdd-state/AGENTS.md`](../crates/acdd-state/AGENTS.md)

> [!IMPORTANT]
> Invariant: `crates/acdd-core` is a strict leaf crate (forbidding `acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `zstd`, `rayon`, `hashbrown`, `clap`, `rmcp`, and `tokio`). `crates/acdd-state` never depends on `crates/acdd-index`, `tree-sitter`, `clap`, `rmcp`, or `tokio`. Inside `crates/acdd-state`, `machine/` is a 100% pure functional core (no `std::fs`, `std::process`, `std::net`, or `rusqlite`), and `rusqlite` imports are confined exclusively to `crates/acdd-state/src/store/sqlite/`. See [ADR 0001](../docs/adr/0001-crate-dag-and-layer-boundaries.md) and [ADR 0003](../docs/adr/0003-unified-profile-graph-and-state-machine.md).

> [!IMPORTANT]
> Invariant: `src/state_bridge` is imported by `src/cli` and `src/mcp`, and never by `src/engine` or any workspace crate (`crates/*`). Proxy modules (`src/core/*`, `src/db/*`) are banned; host modules import types directly from `acdd_core::*`, `acdd_index::*`, `acdd_state::*`, and `crate::format::*`.

- **`crates/acdd-core`**: Strict leaf crate owning decoded identifiers (`ids`), diagnostic codes (`errors`), normalized `WorkspacePathPolicy` predicates (`path_policy`), self-contained `SchemaDef` (`schema`), `DomainProvider` SPI (`ports`), workspace `Adapter` configuration (`config`, `subsystems`), response budgets (`response`), confined filesystem/Git/lock wrappers (`fs`, `git`, `lock`), hashing/JSON counting (`format`), cgroup memory calculation (`sys`), and telemetry wire DTOs (`telemetry`). See [crates/acdd-core/AGENTS.md](../crates/acdd-core/AGENTS.md).
- **`crates/acdd-index`**: Tree-sitter AST fact extraction, language profiles, cross-file symbol linking, filesystem scanner, documentation parser, Merkle commitments (`commitments`), compact graph/fact encoding (`models`, `typed_facts`, `interner`), and `code-map.sqlite` persistence (`db`). See [crates/acdd-index/AGENTS.md](../crates/acdd-index/AGENTS.md).
- **`crates/acdd-state`**: Domain-agnostic `GateProfile` (`version: 2`) parser/validator, `SpecDocument` parser, pure `machine/` transitions, `StateBackend` (`StateReader` / `StateTx`) persistence, `git_ops/`, `runner/`, `context`, `observability`, and `#[cfg(any(test, feature = "test-support"))]`-gated `harness::RunHarness`. See [crates/acdd-state/AGENTS.md](../crates/acdd-state/AGENTS.md).
- **`src/state_bridge`**: Host `CodeDomainProvider` (`code_domain/{mod,seam,signature_scope,context}.rs`: `code.seam`, `code.signature_scope`, `code.symbols`, `code.tests`, `code.external_callers`), `DocsDomainProvider` (`docs_domain.rs`: `docs.links`, `docs.instructions`, `docs.adrs`, `docs.invariants`), and `StateEngine` / `StateQueryEngine` initialization. External `crates/acdd-domain-*` shim-crates follow an opt-in extension pattern (none shipped in-tree; see [domain extensions](../docs/architecture/domain-extensions.md)).
- **`src/engine`**: Host indexing pipeline orchestration (`pipeline/`, including decomposed `delta.rs` frontier phases) and layered file/symbol reader (`reader.rs`). Pipeline modules separate budgeting, lifecycle hooks, cold extraction, staging, invalidation, hydration, and delta orchestration. Use `acdd_index::db::writer::PayloadStore` for bounded RAM staging/spooling and `acdd_index::db::writer::TempTableGuard` for temporary frontier tables.
- **`src/format`**: Shared Markdown/JSON presentation and workspace-confined `file://` URI attachment ([ADR 0006](../docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md)).
- **`src/mcp`**: JSON-RPC Model Context Protocol server (`19` static built-in tools plus registered `DomainProvider` plugin tools), decomposed under `tools/{mod,inputs,code_tools,state_tools,dispatch}.rs` with `StateCallContext::execute_query/execute_command` and `validate_explicit_worktree_git_ancestors`, input validation, and response byte-budget enforcement.
- **`src/cli`**: Flat `0.6.2` command-line entry points (`29` commands: `init`, `start`, `draft`, `status`, `list`, `show`, `next`, `claim`, `verify`, `submit`, `extend-scope`, `reopen`, `sync`, `subtask`, `blackboard`, `clear`, `run`, `tool`, `read`, `validate`, `serve`, `reload`, `build`, `scan`, `delta`, `query`, `docs`, `ast`, `guide`), decomposed across `{mod,args,discovery,dispatch_code,dispatch_state,ast,docs_check,guide,paths,reload,validate}.rs` with `resolve_cli_adapter_path` and `OverviewAspect`/`SubsystemId`-backed `discovery.rs`.

## 2. Balanced Modularity and Code Deduplication

> [!IMPORTANT]
> Invariant: Language extractors implement grammar and lexical facts. Framework semantics are validated manifest data consumed by finite typed operations; do not hardcode framework identities, API names, keyword conventions, or directory policy in extractors, validators, caches, or linkers. Follow the [framework boundary](../docs/architecture/modularity.md#framework-data-boundary).

> [!IMPORTANT]
> Invariant: Modules are split strictly along mixed architectural responsibilities (e.g. process execution vs lifecycle state, cgroup parsing vs directory walking, contract parsing vs Git handoff), never by arbitrary line-count thresholds. Do not enforce line limits on multi-pass pipeline functions that form a single cohesive algorithm.

> [!IMPORTANT]
> Invariant: Proven duplicates (identical logic repeated across files or passes) must be extracted into focused shared helpers. Domain algorithms representing distinct rules (e.g. language-specific AST profiles, linker passes, snapshot verification) must not be prematurely compressed.

> [!IMPORTANT]
> Invariant: **Holistic Root-Cause Analysis Over Test-Patching**: Never apply narrow language string/path heuristics where indexed AST/linker graphs exist, single-case string checks, arbitrary UTF-8 byte slices, or localized workarounds just to satisfy a failing test case or review finding. Always investigate the problem comprehensively: define the complete domain grammar, verify Unicode safety, ensure fail-closed disambiguation ([ADR 0012](../docs/adr/0012-fail-closed-selectors-and-readonly-sql-sandbox.md)), and respect filesystem confinement boundaries ([ADR 0002](../docs/adr/0002-workspace-confinement-and-path-policy.md)).

> [!IMPORTANT]
> Invariant: **Reuse-First Lookup & Complexity Escalation**: Before authoring any helper, Git/FS wrapper, glob matcher, shell template, or graph pass, search existing canonical owners across `acdd_core`, `acdd_index`, `acdd_state`, and `src/` with `code_map_search` / `ast_grep_search` and reuse or extend them (`INV-SINGLE-OWNER`). Escalate to the coordinator via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before introducing any new subsystem, parallel pipeline, or defect fix exceeding `~80-100` net production LOC.

### When to Keep Together (DO NOT Split)
- **High Domain Cohesion**: Closely related data structures, types, and their direct transformation logic belonging to the same semantic unit.
- **Tightly Coupled Pipelines**: Complex static analysis algorithms (`crates/acdd-index/src/linker/value_flow.rs`, `crates/acdd-index/src/linker/semantic_context.rs`, and `crates/acdd-index/src/linker/mod.rs`) where splitting would require exposing private internal state, inventing synthetic glue DTOs, or creating circular imports.
- **Linear Readability**: Keeping a cohesive multi-pass algorithm readable top-to-bottom in a single file is preferred over scattering logic across shallow delegating helpers.

### When to Split (DO Split)
- **Mixed Layer Responsibilities**: A module mixing protocol ingress, business logic, persistence queries, or external process execution.
- **Orthogonal Subsystems**: Code that changes for fundamentally different reasons or at different cadences (e.g., Git index manipulation inside task lifecycles, DOM globals inside TypeScript extraction profiles, or cgroup memory parsing inside directory walkers).
- **Proven Duplicates**: Extract repeated code blocks into single helpers (e.g. stub/overload candidate filters, arrow function parameter AST extraction, Git command wrappers, shared atomic file renaming).

## 3. Canonical Helper Ownership

> [!IMPORTANT]
> Invariant: `INV-SINGLE-OWNER` — Shared infrastructure, validation, policy, and expression logic has exactly one canonical owner. Do not invent local variants, duplicate validators, or duplicate raw subprocess calls in downstream host, index, or state modules:
> - Workspace/spec/profile/dep validation: ONLY `acdd_state::validate` + `acdd_state::usecases::deps` (zero duplicate CLI/script validators).
> - `acdd.yaml` loader & legacy `0.5` detection: ONLY `acdd_core::config`.
> - `ErrorCode` & `StateError`: ONLY `acdd_core::errors`.
> - `WorkspacePathPolicy` & brace-aware glob/overlap matching: ONLY `acdd_core::path_policy`.
> - `WhenExpr` parser/evaluator: ONLY `acdd_state::profile::when`.
> - Confined FS, file locking, Git, process start time, hashing, UTC timestamps: ONLY `acdd_core::{fs, lock, git, sys, format, telemetry}`.

> [!IMPORTANT]
> Invariant: Import shared utilities directly from `acdd_core::*` rather than creating proxy re-export modules in `src/`.

1. **Git Process Execution (`acdd_core::git`)**:
   - `acdd_core::git` (`crates/acdd-core/src/git.rs`) owns the low-level subprocess runner: env scrubbing (`scrub_git_env` / `STRIPPED_GIT_ENV_VARS`), working directory (`cwd`) binding, exit status checks, `primary_worktree_root`, `git_common_dir`, and stdout/raw capture (`run_git_*`, `git_command`, `git_stdout`, `git_status`). Raw `std::process::Command::new("git")` is strictly banned anywhere outside `crates/acdd-core/src/git.rs`.
   - High-level Git workflows (tree snapshots, scoped plumbing commits, internal `rebase-ff` wave landing + `verify_tasks_integrated`, and worktree lifecycle) remain domain logic in `crates/acdd-state/src/git_ops/` (`snapshot.rs`, `commit.rs`, `integrate.rs`, `worktree.rs`).
2. **Filesystem, Path Confinement & Brace-Aware Glob Matching (`acdd_core::fs` & `acdd_core::path_policy`)**:
   - `acdd_core::path_policy` owns `WorkspacePathPolicy` (`is_engine_managed`, `is_candidate_included`, `is_scope_admitted`, `is_agent_writable`, `from_workspace_config`) and the sole brace-aware glob/overlap engine (`expand_glob_braces`, `glob_matches`, `patterns_overlap`, `scopes_overlap`).
   - `acdd_core::fs` (`crates/acdd-core/src/fs.rs`) owns workspace path validation (`canonical_root`, `canonicalize_macos_var`, `checked_child`, `safe_relative`, `ensure_confined_dir_all`, `check_ancestors_no_symlinks`, `confined_database_open_path`), secure `atomic_write` (`0o600`/`0o755`), and `atomic_replace`.
3. **Advisory File Locks (`acdd_core::lock`)**:
   - `acdd_core::lock` (`crates/acdd-core/src/lock.rs`) owns blocking and bounded-retry shared/exclusive OS file locks via `std::fs::File::try_lock*` / `lock*` with `O_NOFOLLOW` regular-file opening (`0o600`). Do not reimplement `libc::flock` polling loops downstream.
4. **Configuration, Errors, Hashing & Formatting (`acdd_core::{config, errors, format}`)**:
   - `acdd_core::config` (`crates/acdd-core/src/config.rs`) is the sole owner of `Adapter`, `AdapterFile`, `LinkedWorkspace`, `ScannerLimits`, legacy `0.5` key detection (`ConfigMigrationRequired`), `load_adapter`, `load_adapter_strict`, `load_adapter_report`, `resolve_state_db_path`, and `resolve_code_db_path`.
   - `acdd_core::errors` (`crates/acdd-core/src/errors.rs`) is the sole owner of `ErrorCode` (`30` canonical variants) and `StateError`.
   - `acdd_core::format` (`crates/acdd-core/src/format.rs`) owns `stable_hash64`, `sha256_hex`, streaming `sha256_serde_json`, `canonical_json_sha256`, `JsonByteCounter` / `estimate_json_bytes`, and `utf8_floor_boundary` / `utf8_tail`.
5. **System Process/Memory and Telemetry (`acdd_core::{sys, telemetry}`)**:
   - `acdd_core::sys` (`crates/acdd-core/src/sys.rs`) owns Linux cgroup v1/v2 memory parsing, budget calculations, and cross-platform `read_process_start_time`; `acdd_core::telemetry` owns W3C trace IDs and UTC timestamp helpers (`now_utc_iso_ms`, `unix_epoch_ms`).
6. **Validation & `when:` Expressions (`acdd_state::{validate, usecases::deps, profile::when}`)**:
   - `acdd_state::validate` + `acdd_state::usecases::deps` own all workspace, spec, profile, and dependency validation; `src/cli/validate.rs` is a thin adapter.
   - `acdd_state::profile::when` (`WhenExpr`) owns all `when:` parsing and evaluation across profile validation and `machine/graph.rs`.

## 4. Module Layout and `build.rs` Conventions

> [!IMPORTANT]
> Invariant: `#[path = "..."]` attributes are permitted only when required by `crates/acdd-index/build.rs` code generation (`OUT_DIR/language_profiles.rs`) or explicit submodule layout constraints. Internal submodules must use standard Rust module hierarchy once the root path resolves to `mod.rs`.

- When decomposing an oversized module, convert it into an idiomatic directory module (`<name>/mod.rs` with sibling submodules `<name>/<submodule>.rs`).
- `crates/acdd-index/build.rs` generates language profile registrations into `OUT_DIR/language_profiles.rs` and computes `ACDD_LANGUAGE_PROFILE_DIGEST` from `crates/acdd-core/src/**` and `crates/acdd-index/src/**`. Root `build.rs` is removed; modifying language profile discovery requires updating `crates/acdd-index/build.rs`.

## 5. Production Rust & Optimization Standards

See [Performance Architecture](../docs/architecture/performance.md) for measured pipeline invariants. Apply these engineering rules across `src/` and `crates/*`:

1. **Ownership at Boundaries**: Borrow `&Adapter`, `&ResponsePolicy`, `&Path`, and `&str` across dispatch and query paths. Move consumed `BuildCandidate` values and non-debug CLI results instead of cloning. Compact bounded `serde_json::Value` payloads in place without per-step `.clone()`. Instantiate `domain_providers` lazily only for tools that use them.
2. **De-Monomorphization**: Route CLI dispatch through `&mut dyn std::io::Write`, `acdd-state` usecases through `&dyn StateBackend` (`exec_tx_dyn`), and `acdd-index` linker/writer helpers through `&Facts` / `&mut dyn std::io::Write`. Thin generic wrappers (`impl AsRef<Path>`, `impl Serialize`) must delegate immediately to non-generic inner functions (`fn inner(path: &Path)`).
3. **Determinism**: Sort `fs::read_dir` entries and map keys (`BTreeMap` or explicit `.sort()`) before assigning sequential IDs (`SymbolId`) or emitting serialized rows, edges, or Merkle commitments.
4. **Cross-Platform SQLite & FS Safety**: Resolve SQLite paths with `confined_database_open_path` before `SQLITE_OPEN_NOFOLLOW`; open read-only paths with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NOFOLLOW` first (recovering WAL without `-shm` without unintended writes) alongside `trusted_schema = OFF` and `busy_timeout`; checkpoint WAL (`TRUNCATE`), switch `PRAGMA journal_mode = DELETE;`, and unlink regular-file sidecars under exclusive lock before staged DB/backup publication; write private files via `0o600` `atomic_write`.
5. **Indexing & Subprocess Hot Paths**: Preserve zero nested Rayon `.par_iter()`, zero `Node`/string clones in `flush_node_batch`, interned manifest rules, `acdd_core::sys::trim_allocator()` at extraction/linker/batch boundaries, `OnceLock` cached host identity, ephemeral vs. durable `sync_all`, batched `git cat-file --batch-check`, progressive subprocess poll backoff (`1ms -> 2ms -> 5ms -> 10ms`), and explicit `run_with_stdin` ingress separation.

## 6. Verification Gates

Before delivering any source code change, verify the required gates from [TESTS.md](../TESTS.md):

1. **Task-Level Proof**: Run the focused test seam and scoped suite proving the admitted change alongside the architectural boundary suite (`cargo test --test commitment_integrity`, executed automatically by `.acdd/profile.yaml`'s `rust-check-and-test`).
2. **Strict Linter**:
   ```bash
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   ```
3. **Milestone Handoff Gate**: Run the workspace verification commands before handoff:
   Cargo test & handoff gate:
   ```bash
   cargo test --test commitment_integrity && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --lib && cargo test --workspace --all-targets
   ```

## 7. Anti-Patterns (Do Not Introduce)

- **No dead-code suppression or legacy module shims**: Ban `#![allow(dead_code, unused_imports)]` and legacy re-export shims (`pub(crate) mod core`, `pub(crate) mod engine`, `src/core/*`, `src/db/*`). Remove unused items rather than suppressing warnings.
- **No mirror `Mcp*` clone structs**: Do not duplicate `acdd_core` / `acdd_state` DTOs as `Mcp*` clone structs with manual `From` converters in `src/mcp/tools/inputs.rs`; derive `schemars::JsonSchema` directly on canonical DTOs.
- **No duplicate `ClaimResp`/status Markdown renderers**: Do not reimplement `ClaimResp` or status Markdown formatting in `src/format/`; delegate `ClaimResp` rendering to `acdd_state::context::render_claim_prompt_markdown` and reuse shared `src/format/` primitives.
- **No `phantom_tested`, `dead_tails`, or `>= 5`-line clones**: Eliminate `phantom_tested` dead code, `dead_tails`, `dangling_calls`, and `>= 5`-line clones (`duplicates`) before submission, and use `tempfile::TempDir` instead of copy-pasted `TempDirDrop` test structs.
