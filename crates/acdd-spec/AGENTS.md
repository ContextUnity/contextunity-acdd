# `acdd-spec` — Crate Router

Read [root instructions](../../AGENTS.md), [source router](../../src/AGENTS.md), [ADR 0001](../../docs/adr/0001-crate-dag-and-layer-boundaries.md), and [modularity](../../docs/architecture/modularity.md) before editing this crate.

## Local Rules & Ownership

- Enforce `INV-SPEC-CRATE-BOUNDARY` (`ARCH-SPEC-DEPS`, `ARCH-SPEC-EXTERNAL-BAN`): depend strictly on `acdd-core` within the workspace and strictly on workspace-inherited `serde`, `serde_json`, `serde_yaml`, and `schemars` externally. Never import `acdd-index`, `acdd-state`, `src/`, `rusqlite`, `tree-sitter`, `tokio`, `rmcp`, `clap`, `regex`, or spawn `git` subprocesses directly.
- Enforce `ARCH-INDEX-SPEC-ENTRY`: keep frontmatter metadata splitting isolated in `src/frontmatter.rs` as the sole `acdd-index` import boundary.
- Enforce `INV-SPEC-RUNTIME-OVERLAY`: keep spec discovery, parsing, drafting, static dependency validation, and static roadmap DAG construction over `&dyn SpecRuntimeOverlay` (`NoopRuntimeOverlay` for stateless callers) without SQLite dependencies.

## Verification

```bash
cargo clippy -p acdd-spec --all-targets --all-features -- -D warnings
cargo test -p acdd-spec --lib
cargo test --test commitment_integrity
```
