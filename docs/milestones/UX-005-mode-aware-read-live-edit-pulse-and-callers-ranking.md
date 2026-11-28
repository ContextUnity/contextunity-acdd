---
id: UX-005-mode-aware-read-live-edit-pulse-and-callers-ranking
title: "Mode-aware acdd_read pre-edit shields, read-only <25ms acdd lint --live-edit & code.edit_pulse overlay, and inbound_callers search ranking"
doc_type: contract
status: planned
depends_on:
  - AC-006-universal-repository-memory-synthesis
  - UX-004-mcp-tool-surface-consolidation
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/ports.rs
  - crates/acdd-index/src/typed_facts/skeletons.rs
  - crates/acdd-index/src/db/lint/
  - crates/acdd-index/src/db/symbols/
  - crates/acdd-index/src/db/reader/
  - src/engine/reader.rs
  - src/state_bridge/code_domain/
  - src/cli/
  - src/format/
  - src/mcp/tools/
  - skills/acdd/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/reference/
  - website/
  - tests/query_context/
  - tests/mcp/
  - tests/acdd/
  - ROADMAP.md
  - CHANGELOG.md
  - docs/milestones/UX-005-mode-aware-read-live-edit-pulse-and-callers-ranking.md
invariants:
  - 'INV-READONLY-LIVE-EDIT-OVERLAY: evaluate_edit_pulse opens code-map.sqlite strictly SQLITE_OPEN_READ_ONLY with a short bounded busy-timeout (zero exclusive flock, zero WAL checkpoints, zero FileIdentity invalidation of concurrent swarm readers) and parses confined in-scope edited files in memory via ast::extract_typed, completing 1-3 file checks in <= 30ms without decoding 10MB local_facts blobs and falling back cleanly (never hanging or erroring) when code-map.sqlite is missing, locked, or pre-UX-005.'
  - 'INV-ADVISORY-HALF-EDITED-SAFE: Post-edit feedback (acdd lint --live-edit, acdd_read mode="delta", and code.edit_pulse) is strictly advisory (always exit 0, zero stdout bytes when clean) and suppresses half-edited multi-file reachability noise (dead_tails, phantom_tested, unused_public_api, dangling_calls, raw unresolved, forbid_cycles, must_implement, deep boundary_entry), evaluating only selector-filtered AstPattern, direct lexical ForbidDependency / AllowOnlyDependencies / SingleOwner imports/calls, syntax rules, #[cfg(test)]/test-excluded asymmetric clones + top-2 reuse_hints, broken doc anchors, and baseline-diffed signature_changed -> external_callers + covering_tests.'
  - 'INV-MODE-AWARE-READ-SURFACES: acdd_read tailors graph_pulse and governance hints strictly over code-map.sqlite + Adapter architecture config (respecting ARCH-INDEX-SPEC-ENTRY / AR-007 readiness with zero direct acdd-spec memory loading in reader.rs, leaving specs.memories to acdd context / StateBridge): outline surfaces zones + matching ARCH-* IDs + INV-* summaries + caller radius; snippet surfaces exact INV-* rules + concrete --test targets + trait siblings; lines surfaces protected_paths/archive/doc tags + file ARCH-* rules; search adds batched enclosing_symbol + [prod]/[test] markers; delta surfaces compact edit_pulse; code_map_query(search) computes batched inbound_callers counts and the 18-tool static MCP cap stays intact.'
  - 'INV-COMPACT-READ-AND-LENS-BUDGET: Pre-edit governance hints in graph_pulse stay <= 250 bytes (UTF-8 is_char_boundary safe) within ADR 0006 acdd_read inline caps (<= 9,000 B), MCP Server deduplicates repeated/sequential acdd_read calls on an unchanged (path, sha256) strictly within a 60-second TTL window (omitting repeated governance/suggested_calls metadata, shortening identical slice repeats, and adding a 1-line tip on >= 2 sequential slices before auto-expiring after 60s), --live-edit caps output at <= 6 deterministic priority-ordered lines (error/clone -> signature_changed -> reuse_hints -> docs), and empty code.edit_pulse lenses omit cleanly with zero tokens on ClaimResp / acdd context.'
acceptance_criteria:
  - 'AC-01: GIVEN a workspace indexed with code-map.sqlite and versioned sidecar statement_skeletons storing compact (hash, skeleton_hash, token_count, line, end_line) tuples, WHEN evaluate_edit_pulse inspects 1-3 confined paths (covering literal-differing clones, #[cfg(test)] isolation, temporary Tree-sitter parse errors, ARCH-CORE-PURE-NO-IO import violations, baseline pub signature changes including unborn HEAD/untracked new files, broken documented symbols, and cold/missing/pre-UX-005 fallback DBs), THEN it executes strictly read-only with zero SQLite write locks, avoids decoding local_facts when versioned skeletons exist, emits deterministic <= 6-line priority-ordered advisory findings (or skips deleted/ignored/symlink-escaping/clean files silently), and omits dead_tails/dangling_calls/cycles.'
  - 'AC-02: GIVEN a source file governed by INV-* invariants, ARCH-* lint rules, protected_paths, and covering tests, WHEN acdd_read runs in outline, snippet, lines, search, and delta modes over CLI and MCP, THEN each mode emits its tailored pre-edit or post-edit governance hints within ADR 0006 UTF-8-safe byte caps using zero N+1 DB queries, code_map_query(search) includes batched inbound_callers counts per hit, and code_map_overview reports applicable directory lint zones without violating ARCH-INDEX-SPEC-ENTRY.'
  - 'AC-03: GIVEN PostToolUse hook execution of acdd lint --live-edit <paths...> (including worktree root-DB fallback, multi-file batches, and unindexed/non-code paths) and active-run acdd context with code.edit_pulse (including memory-owner overlap surfaced at the bridge tier), WHEN no findings trigger, THEN CLI --live-edit emits zero stdout bytes with exit 0 and empty code.edit_pulse omits cleanly; WHEN findings exist, THEN CLI --live-edit emits <= 6 priority-ranked advisory lines (exit 0) and code.edit_pulse renders within budget preserving the 18-tool MCP surface.'
  - 'AC-04: GIVEN completed UX-005 tests inside existing canonical matrices (tests/query_context/, tests/mcp/, tests/acdd/, zero inline FS/tempdir crate tests) plus reconciled ADR 0006, reference docs, skills/acdd/, website/, ROADMAP.md, and CHANGELOG.md, WHEN acdd lint --categories all, acdd validate, acdd docs check (touched pages), ./website/build.sh --check, and workspace test suites run, THEN all pass with zero regressions and zero new Cargo dependencies.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
---

# UX-005: Mode-aware `acdd_read` pre-edit shields, read-only `<25ms` `acdd lint --live-edit` & `code.edit_pulse` overlay, and `inbound_callers` search ranking

## Goal & Non-goals

**Goal.** Close the blind spots inside the `Read -> Edit -> Edit` inner loop without lock contention, noise on half-edited multi-file refactors, or breaking `UX-004`'s 18-tool static MCP cap:
1. **Pre-Edit Governance Shields in `acdd_read` (`mode`-aware)**: Tailor `graph_pulse` and inline annotations to the caller's actual `ReadMode` (`outline` module boundaries/`ARCH-*`/`INV-*`/`specs.memories` owners, `snippet` exact `INV-*` descriptions + runnable `--test` targets + trait siblings, `lines` `protected_paths`/archive tags + rules, `search` enclosing symbol + `[prod]`/`[test]` tag, `delta` live `edit_pulse`).
2. **Read-Only In-Memory `evaluate_edit_pulse` (`<= 25ms`, `0` SQLite locks)**: Extend the non-committed `statement_skeletons` sidecar table with compact `(hash, skeleton_hash, token_count, line, end_line)` tuples so duplicate/overlap analysis skips 10 MB `local_facts` Zstd decoding, parse modified files in RAM via `ast::extract_typed`, pre-filter `rules` by path selectors, and emit strictly high-signal, non-blocking diagnostics (`duplicates`, top-2 `reuse_hints`, `arch`/`syntax`, broken doc links, and `signature_changed -> external_callers + covering_tests`) while keeping noisy global reachability checks on `acdd verify`.
3. **Zero-Friction Delivery Surfaces (`0` new MCP tools)**: Expose `evaluate_edit_pulse` uniformly via `acdd lint --live-edit <path>` (for harness `PostToolUse` `Edit`/`Write` hooks — silent on clean, advisory `exit 0`), `acdd_read(mode="delta")`, and read-only `code.edit_pulse` in `CodeDomainProvider` (`acdd context`, 0 tokens when empty), plus `inbound_callers` counts on `code_map_query(search)` and zone/memory summaries on `code_map_overview`.

**Non-goals.**
- Running `delta_with_selection` or taking exclusive SQLite `.lock` on every file edit (`ADR 0007` reader invalidation & `350–750ms` latency remain confined to explicit `build` / `verify`).
- Adding a 19th built-in MCP tool (`acdd_code_read` / `acdd_edit`) or mutating filesystem files inside ACDD MCP tools.
- Recomputing `cochange` `git log` history or `dead_tails`/`dangling_calls`/`cycles` inside `--live-edit` (owned by `Diff X-Ray` at `acdd verify`).

## Architecture, Edge Cases & Ownership Map

### 1. Pre-Edit vs Post-Edit Flow (`ARCH-INDEX-SPEC-ENTRY` & `AR-007` ready)

```text
  PRE-EDIT (`acdd_read` / `code_map_query(search)` / `code_map_overview`)
    SQLITE_OPEN_READ_ONLY (existing governing_invariants / edges / Adapter lint zones & rules; zero acdd-spec loading)
      ├── outline  -> zones, matching ARCH-* IDs, INV-* summaries, external caller radius
      ├── snippet  -> exact INV-* text, concrete --test targets, trait siblings
      ├── lines    -> protected_paths/archive/doc tag + matching ARCH-* rules
      ├── search   -> single-query batched enclosing_symbol + [prod]/[test] tag; query(search) includes batched inbound_callers
      └── overview -> governing lint zones for directory

  POST-EDIT (`PostToolUse` `acdd lint --live-edit <path>` | `acdd_read(mode="delta")` | `acdd context` `code.edit_pulse`)
    SQLITE_OPEN_READ_ONLY (bounded busy-timeout, primary-root fallback on cold worktrees) + RAM `ast::extract_typed(&edited_files)`
      ──► evaluate_edit_pulse (~12-25 ms, 0 write locks, batched Git baseline read, pre-filtered rules)
          ├── Selector-filtered AstPattern, direct ForbidDependency / AllowOnlyDependencies / SingleOwner imports/calls, syntax rules (including syntax errors mid-edit)
          ├── Prod/#[cfg(test)]-isolated asymmetric Probe(edited) vs Workspace duplicates (>= 0.80) + top-2 reuse_hints (0.45..0.80)
          ├── Baseline pub signature_changed -> top-5 external_callers + top-3 covering_tests (new/unborn HEAD = clean new symbols)
          └── Broken documents/governs anchor links on renamed/deleted symbols
          (Excludes dead_tails, phantom_tested, unused_public_api, unresolved, cycles, MustImplement, deep BoundaryEntry -> verify only)
```

### 2. Boundary & Edge-Case Matrix (Verified Contract Bounds)

| Edge-case category | Operational hazard | Mandatory contract guardrail |
|---|---|---|
| **Layer isolation (`AR-007` readiness)** | `src/engine/reader.rs` moving to `crates/acdd-index` cannot import `acdd-spec` memory discovery (`ARCH-INDEX-SPEC-ENTRY`); `acdd.yaml` excludes `docs/memories` from `docs:` index | Keep `reader.rs` strictly on `code-map.sqlite` + `Adapter` `.acdd/lint/` zones/rules; surface `specs.memories` hints only at the `src/state_bridge/` (`acdd context`) tier |
| **Cold / locked / stale DB** | Edit hook hangs on `SQLITE_BUSY` during parallel `acdd build`, or crashes on cold worktree / pre-UX-005 `statement_skeletons` blob | Use `SQLITE_OPEN_READ_ONLY` + short busy-timeout, fall back to primary root DB on cold worktree or in-memory AST-only when no DB exists, bump `STATEMENT_SKELETONS_MAGIC` + `ExtractionCache` cleanly with `local_facts` fallback |
| **Path & content edge inputs** | Hook passes escaped symlinks, `target/`, `.md`, deleted files, or mid-edit syntax typo (`ERROR` AST node) | Enforce `checked_child` + `WorkspacePathPolicy` + non-symlink confinement; silently skip missing/deleted/ignored/binary files (`exit 0`); emit clean parse warning without panicking |
| **`#[cfg(test)]` & test files** | Editing inline unit tests falsely triggers prod duplicate `Error`s or `signature_changed` caller alarms | Apply test-span filtering (`#[cfg(test)]` / `is_test`) identically to `duplicates.rs` & `signature_scope.rs` unless `ArchRuleSpec.include_tests` opts in |
| **Cross-file half-edited rules** | Multi-hop `forbid_cycles`, `must_implement`, and deep `boundary_entry` fail mid-refactor without linked DB edges | Restrict `evaluate_edit_pulse` strictly to rules evaluable from single-file `TypedFacts` (`AstPattern`, direct lexical `ForbidDependency` / `AllowOnlyDependencies` / `SingleOwner`, `Syntax`); keep graph-wide checks in `acdd verify` |
| **N+1 I/O, UTF-8 & output floods** | Per-hit `inbound_callers` or per-line `enclosing_symbol` queries blow 25ms; multi-file `--live-edit` floods agent context | Batch `inbound_callers` (`json_each`), `enclosing_symbol` (1 range query/file), and baseline Git blobs (`read_git_blobs_at_paths_batch`); slice strictly on `is_char_boundary`; cap `--live-edit` at `<= 6` priority-sorted lines (`exit 0`) |

## Tasks in this milestone

### task: readonly-edit-pulse-and-compact-skeletons

```yaml
task_ref: readonly-edit-pulse-and-compact-skeletons
target: "Extend sidecar statement_skeletons with compact statement tuples to bypass 10MB local_facts corpus decode, implement read-only evaluate_edit_pulse in crates/acdd-index/src/db/lint/, add selector-filtered rule/memory lookup and inbound_callers counts in db/reader/ + db/symbols/, and pre-register Wave 1 seam tests"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-index/src/typed_facts/skeletons.rs
  - crates/acdd-index/src/db/lint/
  - crates/acdd-index/src/db/reader/
  - crates/acdd-index/src/db/symbols/
  - tests/query_context.rs
  - tests/query_context/live_edit_pulse.rs
  - tests/mcp.rs
  - tests/mcp/mode_aware_read.rs
  - tests/acdd.rs
  - tests/acdd/live_edit_cli_and_context.rs
status: planned
subtasks:
  - subtask_ref: compact-skeletons-and-evaluate-edit-pulse
    title: "Bump STATEMENT_SKELETONS_MAGIC (+ ExtractionCache version) to store compact (hash, skeleton_hash, token_count, line, end_line) tuples with pre-UX-005 local_facts fallback, and implement strictly read-only evaluate_edit_pulse (confined paths, batched Git baseline with unborn HEAD support, #[cfg(test)]-safe clones/reuse_hints, syntax/parse-error handling, single-file selector rules, signature_changed callers/tests, broken doc links, <= 6 priority lines)"
  - subtask_ref: governance-hints-and-inbound-callers-queries
    title: "Expose compact file/symbol governance summaries (ARCH-* rule IDs, INV-* text via governing_invariants_for_scope, directory zones, zero acdd-spec coupling) plus batched enclosing_symbol range lookup in db/reader/, batched json_each inbound_callers counts in db/symbols/search.rs, and pre-register Wave 1 test modules"
```

- **Wave**: `0` | **Budget**: `~220` net prod LOC.
- **Seam proof (`AC-01`)**: `tests/query_context/live_edit_pulse.rs` proves `evaluate_edit_pulse` runs strictly read-only with zero write locks, skips full-corpus `local_facts` decode, flags clones/rule violations/broken doc links/`external_callers` + `covering_tests`, and suppresses half-edited reachability noise.

### task: mode-aware-reader-and-search-callers

```yaml
task_ref: mode-aware-reader-and-search-callers
target: "Upgrade src/engine/reader.rs (acdd_read) to emit ReadMode-tailored pre-edit and post-edit signals within ADR 0006 byte caps, attach zone/memory summaries to code_map_overview and inbound_callers to code_map_query(search) formatting"
contract_revision: 1
depends_on:
  - readonly-edit-pulse-and-compact-skeletons
tags:
  - default
scope:
  - src/engine/reader.rs
  - src/engine/mod.rs
  - src/format/core.rs
  - src/format/discovery.rs
  - tests/mcp/mode_aware_read.rs
status: planned
subtasks:
  - subtask_ref: readmode-tailored-graph-pulse
    title: "Specialize graph_pulse and compact governance annotations across ReadMode::{Outline, Snippet, Lines, Search, Delta} in src/engine/reader.rs (invoking evaluate_edit_pulse on delta, <= 250 B pre-edit shield on outline/snippet/lines, enclosing symbol + prod/test marker on search) and add a lightweight 60s-TTL session dedup cache on MCP Server to skip repeated governance metadata, collapse identical repeats within 60s, and hint once on >= 2 sequential slices"
  - subtask_ref: overview-zones-and-search-callers-formatting
    title: "Surface applicable lint zones in overview responses and batched inbound_callers counts in code_map_query(search) across CLI and MCP formatters, verified in tests/mcp/mode_aware_read.rs"
```

- **Wave**: `1` (concurrent with `live-edit-cli-hook-and-context-lens`, strictly disjoint scopes) | **Budget**: `~150` net prod LOC.
- **Seam proof (`AC-02`)**: `tests/mcp/mode_aware_read.rs` proves all 5 `ReadMode` payloads, `search` `inbound_callers`, and `overview` governance hints match across CLI and MCP within `ADR 0006` byte limits.

### task: live-edit-cli-hook-and-context-lens

```yaml
task_ref: live-edit-cli-hook-and-context-lens
target: "Add acdd lint --live-edit <path> silent-on-clean advisory CLI fast-path for PostToolUse Edit/Write hooks and expose code.edit_pulse in CodeDomainProvider (acdd context) while keeping static MCP tools at 18"
contract_revision: 1
depends_on:
  - readonly-edit-pulse-and-compact-skeletons
tags:
  - default
scope:
  - src/cli/
  - src/format/lint.rs
  - src/state_bridge/code_domain/
  - tests/acdd/live_edit_cli_and_context.rs
status: planned
subtasks:
  - subtask_ref: cli-live-edit-fastpath
    title: "Wire acdd lint --live-edit <path>... delegating to evaluate_edit_pulse, emitting zero stdout bytes with exit 0 when clean and <= 6 concise advisory lines (exit 0) when triggered"
  - subtask_ref: code-edit-pulse-context-lens
    title: "Register read-only code.edit_pulse in CodeDomainProvider (omitted cleanly with zero tokens when clean) and prove PostToolUse CLI + acdd context parity in tests/acdd/live_edit_cli_and_context.rs"
```

- **Wave**: `1` (concurrent with `mode-aware-reader-and-search-callers`, strictly disjoint scopes) | **Budget**: `~110` net prod LOC.
- **Seam proof (`AC-03`)**: `tests/acdd/live_edit_cli_and_context.rs` proves `--live-edit` silent clean vs bounded triggered stdout (`exit 0`), `code.edit_pulse` zero-token empty omission, and 18-tool MCP invariance.

### task: docs-skills-and-website-reconciliation

```yaml
task_ref: docs-skills-and-website-reconciliation
target: "Reconcile ADR 0006, reference/architecture docs, skills/acdd/ hook guidance, website/, ROADMAP.md, and CHANGELOG.md with mode-aware acdd_read, acdd lint --live-edit, code.edit_pulse, and inbound_callers search ranking"
contract_revision: 1
depends_on:
  - mode-aware-reader-and-search-callers
  - live-edit-cli-hook-and-context-lens
tags:
  - default
  - docs
scope:
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/reference/
  - skills/acdd/
  - website/
  - ROADMAP.md
  - CHANGELOG.md
status: planned
subtasks:
  - subtask_ref: docs-and-skills-sync
    title: "Update ADR 0006, docs/reference/{cli,mcp-tools,workflows}.md, skills/acdd/ (including optional PostToolUse --live-edit hook snippet), website/, ROADMAP.md, and CHANGELOG.md"
  - subtask_ref: verification-checks
    title: "Verify acdd validate, acdd docs check, acdd lint --categories all, and ./website/build.sh --check pass"
```

- **Wave**: `2` | **Budget**: `~120` doc lines.
- **Seam proof (`AC-04`)**: `acdd validate`, `acdd lint --categories all`, and `./website/build.sh --check` pass cleanly.

## Negative boundaries

- No SQLite writes, exclusive `.lock` acquisition, or WAL checkpoints inside `acdd_read`, `--live-edit`, or `code.edit_pulse` (`ADR 0007`).
- No new built-in static MCP tools (`BUILTIN_MCP_TOOLS` remains `18`).
- No multi-hop `dead_tails`, `phantom_tested`, `unused_public_api`, `dangling_calls`, `forbid_cycles`, `must_implement`, or deep `boundary_entry` checks in `--live-edit` or `mode="delta"`.
- No direct `acdd-spec` memory discovery imports inside `src/engine/reader.rs` or `crates/acdd-index/` (preserving `ARCH-INDEX-SPEC-ENTRY` and `AR-007` readiness).
- No FS/`temp_dir` `#[cfg(test)]` tests added inside `crates/**/src/**` (place isolation tests strictly in canonical `tests/*.rs` matrices per `tests/AGENTS.md`).
- No third-party `Cargo.toml` dependencies.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-04` trace cleanly across Waves 0–2 covering compact sidecar `statement_skeletons`, read-only `evaluate_edit_pulse`, mode-aware `acdd_read`, `inbound_callers` search ranking, `--live-edit`, `code.edit_pulse`, and docs/website sync.
- **Correctness**: Derived from empirical measurements on the live 123 MB `code-map.sqlite` (`<= 25ms` read-only overlay vs `350-750ms` WAL lock `delta` and `100ms` full `local_facts` decode) and immune to multi-file half-edited refactor noise.
- **Coherence**: Stacks cleanly on top of the non-committed `statement_skeletons` table and `DuplicateScanRequest` introduced in the active hotfix, keeps concurrent Wave 1 tasks strictly disjoint, and preserves `UX-004` and `ADR 0006`.
