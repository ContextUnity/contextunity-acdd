# `acdd-index` — Crate Router

Read [root instructions](../../AGENTS.md), [source router](../../src/AGENTS.md), [indexing architecture](../../docs/architecture/indexing.md), [performance](../../docs/architecture/performance.md), [cross-platform](../../docs/architecture/cross-platform.md), [ADR 0007](../../docs/adr/0007-sqlite-code-map-concurrency-and-staged-publication.md), and [ADR 0008](../../docs/adr/0008-merkle-commitments-and-compressed-fact-storage.md) before editing. Read [language extractors](src/languages/AGENTS.md) before modifying `src/languages/`.

## Local Rules & Ownership

- Depend only on `acdd-core` inside this workspace (never `acdd-state` or `src/`); reuse `acdd_core::{config, errors, format, fs, git, lint, lock, path_policy, sys, telemetry}` (`INV-SINGLE-OWNER`, `INV-GIT-SINGLE-OWNER`).
- Owns `ast/`, `languages/`, `linker/`, `scanner/`, `docs.rs`, `models/`, `semantic/`, `typed_facts/`, `interner.rs`, `commitments.rs`, and `db/{reader,symbols,traversal,lint,cache,commitments,cycles,paging,schema,writer}`.
- Layer 2 facade & shared value-flow owners: `languages::LanguageProfile`, crate-private `languages::profile::ProfileLinker::linker()` & `languages::LanguageFileState`, `languages::support::value_flow_core::{ScopeFrameFlags, ScopeState, ValueFlowCollectorCore, GenericScopeAnalysis}`, `languages::dependency_registry::parser::{ManifestParser, MANIFEST_PARSERS}`, and `languages::manifests::semantic::{ActivationCondition, FileActivationSignals}` (`ACDD-LOCAL-FACTS\0\x05`).
- Preserve `INV-PERF-DETERMINISTIC-MERKLE-SEALING`, zero `N+1` SQLite/disk/Git loops, `verified_source` disk reads (ban raw `std::fs::read_to_string`), `SQLITE_OPEN_NOFOLLOW` + `confined_database_open_path`, `PayloadStore`/`TempTableGuard`, `acdd_core::format::sha256_serde_json`, non-fallback fast-path seam proofs, and `--release` hot-path timing checks.
- Run `code_map_query(operation="duplicates", path="crates/acdd-index")` and `code_map_query(operation="dead_tails", path="crates/acdd-index")` before submission; escalate new subsystems or `~80-100`+ net LOC defect fixes via `acdd_blackboard`.

## Verification

```bash
cargo clippy -p acdd-index --all-targets --all-features -- -D warnings
cargo test -p acdd-index --lib
```
