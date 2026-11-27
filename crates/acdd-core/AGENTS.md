# `acdd-core` — Crate Router

Read [root instructions](../../AGENTS.md), [source router](../../src/AGENTS.md), [ADR 0001](../../docs/adr/0001-crate-dag-and-layer-boundaries.md), and [modularity](../../docs/architecture/modularity.md) before editing this crate.

## Local Rules & Ownership

- Enforce `INV-CORE-LEAF`: keep `crates/acdd-core` a strict leaf crate with zero workspace or heavy runtime dependencies (`acdd-index`, `acdd-state`, `rusqlite`, `tree-sitter`, `chrono`, `rayon`, `zstd`, `hashbrown`, `clap`, `rmcp`, `tokio`).
- Route shared workspace primitives through their single owners here: `config/`, `errors` (sole `AcddError` + `ErrorCode`), `path_policy`, `lint`, `ports`, `ids`, `schema`, `subsystems` (`SubsystemId`, `SubsystemPreset`), `response`, `fs`, `lock`, `git` (sole permitted `Command::new("git")` owner), `sys`, `format`, and `telemetry`.
- Keep thin generic wrappers (`impl AsRef<Path>`, `impl Serialize`) delegating immediately to non-generic inner functions, derive `schemars::JsonSchema` on wire DTOs, and preserve public signatures (`INV-SURFACE-COMPATIBILITY`).
- Escalate via `acdd_blackboard` (`topic: decisions` / `--action escalate`) before adding new subsystems, expanding public `acdd-core` surfaces, or exceeding `~80-100` net production LOC for a defect fix.

## Verification

```bash
cargo clippy -p acdd-core --all-targets --all-features -- -D warnings
cargo test -p acdd-core --lib
cargo test --test commitment_integrity
```
