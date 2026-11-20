---
id: PF-005-roadmap-and-dep-resolution-hotpath
title: "Eliminate O(E*A) Git subprocess proliferation and redundant archive scans in acdd roadmap and dependency resolution"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/git.rs
  - crates/acdd-state/src/git_ops/snapshot.rs
  - crates/acdd-state/src/usecases/deps.rs
  - crates/acdd-state/src/usecases/roadmap.rs
invariants:
  - 'INV-PF005-BATCHED-GIT-READS: Multi-blob archive and tree inspections batch Git object reads via a single `git cat-file --batch` invocation per snapshot instead of spawning `cat-file -t` + `cat-file blob` per file, while preserving symlink, non-blob, UTF-8, and workspace-confinement rejection.'
  - 'INV-PF005-SINGLE-PASS-ROADMAP-DEPS: `handle_roadmap` and `validate_spec_dependencies` preload SQLite runs/tombstones and discovered archive specs once per invocation (`O(A + E)`) instead of invoking `visit_archived_specs`, `list_all_runs`, or `list_all_tombstones` inside per-edge or per-worktree dependency loops.'
  - 'INV-PF005-NO-REPLAY-ARCHIVES: `discover_specs_across_worktrees` does not re-read or re-parse unchanged archive files across worktrees, and `build_dependency_graph` skips resolving archived specs'' own `depends_on` edges unless `archived` display or an archived `spec_id` target is explicitly requested.'
acceptance_criteria:
  - 'AC-1: `crates/acdd-core/src/git.rs` provides a batched UTF-8 blob reader (`git cat-file --batch`) used by `crates/acdd-state/src/usecases/deps.rs`, and `list_git_tree_entries` avoids redundant `git rev-parse --verify` spawns when given a resolved tree OID.'
  - 'AC-2: `handle_roadmap` and `validate_spec_dependencies` preload runs, tombstones, and parsed archive summaries once per command and resolve `DepTargetResolution` against the in-memory index with zero per-edge `visit_archived_specs` or sibling-worktree rescan loops.'
  - 'AC-3: Release `./target/release/acdd roadmap` latency on the repository (3 worktrees, 60 archives) drops from ~471 ms (`280 ms` sys) to <= 80 ms with identical roadmap wave/dependency/divergence output (`crates/acdd-state/tests/state_integration/roadmap_query_waves.rs` and `tests/acdd/roadmap_cli.rs` pass).'
required_skills:
  - acdd
  - code-quality
  - test-suite-rules
repositories: []
linked_to: []
---

# PF-005: Roadmap & Dependency Resolution Hot-Path Consolidation

## Architectural Context & Measured Bottlenecks

Profiling `./target/release/acdd roadmap` vs `./target/release/acdd status` on `main` (3 Git worktrees, 60 archived Markdown specs totaling ~3.0 MiB, 53 archived specs declaring 171 `depends_on:` entries, and 7 `run_tombstones` in `.acdd/state.sqlite`) showed:

- `acdd status`: **6 ms** (`0.001s` user, `0.005s` sys)
- `acdd roadmap`: **471 ms** (`0.181s` user, **`0.280s` sys**)
- `acdd roadmap --tasks`: **422 ms** (`0.174s` user, **`0.254s` sys**)

Nearly 60% of wall time is kernel `sys` overhead from Git subprocess proliferation plus repeated archive parsing:

1. `crates/acdd-core/src/git.rs::read_git_blob_utf8_spec` spawns **two** `git` processes per object (`git cat-file -t` followed by `git cat-file blob`), even when called inside `visit_archived_specs` where `list_git_tree_entries` already filtered to `blob` entries.
2. `crates/acdd-state/src/git_ops/snapshot.rs::list_git_tree_entries` runs `git rev-parse --verify` again on an OID already produced by `resolve_git_tree_oid`, and `list_owned_worktrees` spawns `rev-parse --show-toplevel` + `rev-parse --git-common-dir` per worktree.
3. `discover_specs_across_worktrees` calls `collect_archived_specs` once per worktree (reading and parsing all 60 archives from `HEAD` via Git blobs on primary, then re-reading and re-parsing all 60 archives from disk for every sibling worktree).
4. `build_dependency_graph` resolves `depends_on` for every entry in `archived_by_id` even when `--archived` is omitted, and `resolve_dep_target` only receives `active_docs` (not preloaded `archived_by_id`, `runs`, or `tombstones`). Every archived/missing dependency edge falls back to `visit_archived_specs(ws_path, collections, false, ...)` (and repeats across every sibling worktree on `NotFound`), producing an `O(E * A * W)` Git/SQLite storm.

---

### task: batched-git-blob-and-worktree-readers

```yaml
task_ref: batched-git-blob-and-worktree-readers
target: "Add a single-subprocess `git cat-file --batch` UTF-8 blob reader in `crates/acdd-core/src/git.rs` and eliminate duplicate `rev-parse` verification in `list_git_tree_entries` and `list_owned_worktrees`"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - crates/acdd-core/src/git.rs
  - crates/acdd-state/src/git_ops/snapshot.rs
invariants:
  - 'INV-PF005-BATCHED-GIT-READS: Multi-blob Git reads execute via a single `git cat-file --batch` invocation preserving OID validation, non-blob/missing handling, strict UTF-8 decoding, and repo-relative confinement.'
acceptance_criteria:
  - 'AC-1: `crates/acdd-core/src/git.rs` exposes a batched blob reader using `git cat-file --batch` (via `run_git_*` with stdin) handling missing objects, non-blob types, exact byte-length framing, and invalid UTF-8 errors.'
  - 'AC-2: `list_owned_worktrees` avoids redundant `git rev-parse` child spawns for identical worktree paths, and `crates/acdd-state/src/git_ops/snapshot.rs` avoids re-running `rev-parse --verify` when `list_git_tree_entries` receives a validated 40/64-hex tree OID (or batches multi-prefix `ls-tree` calls cleanly).'
  - 'AC-3: Unit/seam tests in `crates/acdd-core/src/git.rs` and `crates/acdd-state` prove parity with `read_git_blob_utf8_by_oid` for valid, missing, non-blob, and malformed OID inputs.'
status: planned
subtasks:
  - subtask_ref: implement-cat-file-batch-reader
    title: "Implement `read_git_blobs_utf8_batch` in `crates/acdd-core/src/git.rs` using `git cat-file --batch` with strict framing and UTF-8 checks"
  - subtask_ref: trim-redundant-rev-parse-and-worktree-spawns
    title: "Deduplicate `rev-parse` verification in `list_git_tree_entries` / `resolve_git_tree_oid` and minimize per-worktree `git` spawns in `list_owned_worktrees`"
```

---

### task: roadmap-and-deps-single-pass-cache

```yaml
task_ref: roadmap-and-deps-single-pass-cache
target: "Eliminate O(E*A*W) repeated archive scans, worktree re-parses, and per-edge SQLite queries in `crates/acdd-state/src/usecases/{roadmap.rs,deps.rs}`"
contract_revision: 1
depends_on:
  - batched-git-blob-and-worktree-readers
tags:
  - default
scope:
  - crates/acdd-state/src/usecases/deps.rs
  - crates/acdd-state/src/usecases/roadmap.rs
  - crates/acdd-state/tests/state_integration/roadmap_query_waves.rs
  - tests/acdd/roadmap_cli.rs
invariants:
  - 'INV-PF005-SINGLE-PASS-ROADMAP-DEPS: Preloaded `StateReader` runs/tombstones and discovered archive maps serve all dependency lookups in `O(1)`/`O(log N)` without per-edge SQLite or Git tree rescans.'
  - 'INV-PF005-NO-REPLAY-ARCHIVES: Sibling worktrees do not re-parse unchanged archive files, and archived spec `depends_on` edges are skipped when archived entries are not requested.'
acceptance_criteria:
  - 'AC-1: `visit_archived_specs` collects matching tree OIDs across collections first and loads all blobs in one `read_git_blobs_utf8_batch` call instead of calling `read_git_blob_utf8_by_oid` inside the entry loop.'
  - 'AC-2: `discover_specs_across_worktrees` deduplicates identical archive files across worktrees before `parse_spec` (checking HEAD tree parity / content digest or relative archive set against primary) while still emitting divergence/warning diagnostics when an archive copy genuinely differs.'
  - 'AC-3: `resolve_dep_target` accepts an optional preloaded snapshot (`runs`, `tombstones`, and `archived_specs` keyed by `spec_id`), reused by both `build_dependency_graph` in `roadmap.rs` and `validate_spec_dependencies` in `deps.rs` with zero per-edge `visit_archived_specs` or sibling rescan fallbacks.'
  - 'AC-4: `build_dependency_graph` omits `discovered.archived_by_id` dependency resolution unless `req.archived` or `req.spec_id` targets an archived spec; `cargo test -p acdd-state` and `cargo test --test acdd roadmap_cli` pass and `./target/release/acdd roadmap` executes in <= 80 ms on this repository.'
status: planned
subtasks:
  - subtask_ref: preload-state-and-batch-visit-archived
    title: "Update `visit_archived_specs` to batch Git blob reads and introduce preloaded runs/tombstones/archives lookup context for `resolve_dep_target` and `validate_spec_dependencies`"
  - subtask_ref: dedup-worktree-archives-and-skip-unrequested-archive-edges
    title: "Skip redundant archive `parse_spec` across unchanged worktrees and omit archived `depends_on` resolution in `roadmap.rs` when archived specs are not requested"
  - subtask_ref: verify-roadmap-parity-and-budget
    title: "Extend table-driven parity tests in `roadmap_query_waves.rs` / `roadmap_cli.rs` covering worktree archive divergence and verify release timing <= 80 ms"
```
