---
title: "Source Code Architecture — Router and Operational Guards"
doc_type: guide
---

# Source Code Architecture — Router and Operational Guards

Read [root instructions](../AGENTS.md), [ADR 0001](../docs/adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0003](../docs/adr/0003-unified-profile-graph-and-state-machine.md), [modularity](../docs/architecture/modularity.md), [performance](../docs/architecture/performance.md), and [cross-platform](../docs/architecture/cross-platform.md) for canonical invariants (`INV-*`) and live `acdd-rule` boundaries.

## Layer Topology & Crate Routers

```text
  src/cli (clap) ─────────┐
  src/mcp (20 static      ├──► src/state_bridge (CodeDomainProvider, DocsDomainProvider)
       + plugin tools)    │        │                          │
  src/format              │        ▼                          ▼
                          └──► crates/acdd-state          crates/acdd-index
                                   │                          │
                                   └──────────┬───────────────┘
                                              ▼
                                      crates/acdd-core
```

- [`crates/acdd-core/AGENTS.md`](../crates/acdd-core/AGENTS.md): Leaf contracts (`config`, `errors`, `path_policy`, `lint`, `ports`, `fs`, `git`, `lock`, `sys`, `format`, `telemetry`).
- [`crates/acdd-index/AGENTS.md`](../crates/acdd-index/AGENTS.md): AST extraction, language profiles/manifests, linker, docs, Merkle commitments, and `db/` (`lint`, `reader`, `symbols`, `traversal`, `writer`).
- [`crates/acdd-state/AGENTS.md`](../crates/acdd-state/AGENTS.md): `GateProfile` (`v2`), `SpecDocument`, pure `machine/`, `validate`, `usecases/`, `store/`, `git_ops/`, `runner/`, `context/`, and `harness`.
- `src/state_bridge`: Host `CodeDomainProvider` (`code.seam`, `code.lint`, `code.signature_scope`, `code.symbols`, `code.tests`, `code.external_callers`) and `DocsDomainProvider`. Never import `src/state_bridge` from `src/engine/` or `crates/*`.
- `src/engine`, `src/format`, `src/mcp`, `src/cli`: Host indexing pipeline/reader, shared Markdown/JSON formatting, `20` static + plugin MCP server, and `33`-command CLI (`acdd lint` added).

## Operational Rules

- **Canonical single owners (`INV-SINGLE-OWNER`, `INV-GIT-SINGLE-OWNER`)**: Search `acdd_core`, `acdd_index`, `acdd_state`, and `src/` before authoring helpers; route all Git subprocesses through `acdd_core::git` and all validation through `acdd_state::{validate, usecases::deps}`.
- **Module layout & `build.rs`**: Use directory modules (`<name>/mod.rs` + sibling submodules) when responsibilities diverge; keep `#[path = "..."]` strictly for `crates/acdd-index/build.rs` generated registrations (`OUT_DIR/language_profiles.rs`).
- **De-monomorphization & determinism**: Pass `&dyn StateBackend`, `&mut dyn std::io::Write`, `&Adapter`, and `&Path`/`&str` to non-generic inner functions; sort directory entries and map keys (`BTreeMap`/`BTreeSet`) before ID assignment or commitment hashing.
- **Zero shims, clones, or mirror DTOs (`INV-SURFACE-COMPATIBILITY`, `INV-NO-DEAD-CODE-SHIMS`)**: Ban `#![allow(dead_code, unused_imports)]`, `src/core/*`/`src/db/*` proxy shims, unused or test-only `pub`/`pub use` compat shims across internal workspace crates, `Mcp*` clone structs, duplicate `ClaimResp` Markdown renderers, `>= 5`-line clones, `phantom_tested`, `dead_tails`, `dangling_calls`, and `unused_public_api` violations while keeping active CLI/MCP wire, DTO, and `ErrorCode` contracts intact.
- **Complexity escalation**: Escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before adding a new subsystem or exceeding `~80-100` net production LOC for a defect fix.

## Verification

Run scoped task proof + strict Clippy during iteration ([TESTS.md](../TESTS.md)), and verify `acdd lint`:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --test commitment_integrity
cargo run -q -- lint
```
