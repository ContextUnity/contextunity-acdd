---
id: plan-retrospective-memory-and-diff-aware-context
title: "Plan: Retrospective Memory Pipeline, Unified Context SPI, and Zero-Hardcode Specs (ADR 0015)"
doc_type: plan
status: completed
owners:
  - crates/acdd-core/src/ports.rs
  - crates/acdd-core/src/config/schema.rs
  - crates/acdd-core/src/config/refs.rs
  - crates/acdd-spec/src/discovery.rs
  - crates/acdd-spec/src/spec.rs
  - crates/acdd-spec/src/embedded/memory.yaml
  - crates/acdd-index/src/db/traversal/change_impact.rs
  - crates/acdd-state/src/workflow/schema.rs
  - crates/acdd-state/src/workflow/validate.rs
  - crates/acdd-state/src/workflow/embedded/code.yaml
  - crates/acdd-state/src/workflow/embedded/documentation.yaml
  - crates/acdd-state/src/context/tiers.rs
  - crates/acdd-state/src/context/markdown.rs
  - crates/acdd-state/src/context/tests.rs
  - crates/acdd-state/src/usecases/verify.rs
  - src/state_bridge/common.rs
  - src/state_bridge/code_domain/common.rs
  - src/state_bridge/code_domain/mod.rs
  - src/state_bridge/docs_domain/mod.rs
  - src/state_bridge/docs_domain/ranking.rs
  - acdd.yaml
  - .acdd/specs/retrospective.yaml
  - .acdd/workflows/default.yaml
  - .acdd/workflows/memory_optimize.yaml
  - skills/acdd/references/acdd-spec-examples/retrospective.yaml
  - skills/acdd/references/acdd-workflow-examples/memory_optimize.yaml
  - tests/acdd/spec_schemas/memory.rs
invariants:
  - "INV-SINGLE-OWNER: SpecConfig.ignore owns collection directory/router-doc filtering across discovery, docs_domain, and state_bridge; ContextRef + ContextCapability own all context dispatch without string hacks in ports.rs or tiers.rs."
  - "INV-CORE-LEAF: crates/acdd-core remains dependency-free of acdd-spec, acdd-index, and acdd-state."
---

# Plan: Retrospective Memory Pipeline, Unified Context SPI, and Zero-Hardcode Specs

Implements [ADR 0015](../../adr/0015-retrospective-memory-and-diff-aware-context.md) and eliminates remaining spec/context/workflow hardcodes across the workspace.

## Completed Tracks (1–4)

1. **Declarative `SpecConfig.ignore` & Disjoint Subtrees (`acdd-core`, `acdd-spec`, `src/state_bridge`)**
   - Added `ignore: Option<Vec<String>>` on `SpecConfig` plus `ignores_subpattern` so `docs/memories` and `docs/memories/.retro` coexist cleanly without false overlap errors.
2. **Agnostic Declarative `.acdd/specs/retrospective.yaml` & Optional `symbols` on `memory`**
   - Authored `.acdd/specs/retrospective.yaml` + `skills/acdd/references/acdd-spec-examples/retrospective.yaml` (`ref: retrospective`), wired `specs.retrospectives` and `docs/memories` in `acdd.yaml`, with zero `"retrospective"` strings in Rust crates.
3. **Universal & Hunk-Exact Diff-Aware `specs.*` Ranking (`acdd-index`, `src/state_bridge`)**
   - Exposed `collect_changed_symbol_seeds` over `git diff -U0` hunk ranges, upgraded `ranking.rs` to 5-tier universal file + hunk-symbol + `doc_section` code-map edge ranking, and replaced hardcoded item summary field names (`statement`/`target`/`summary`/`issue`) with schema-driven `summarize_spec_item_fields`.
4. **Workflows & `.gitignore`**
   - Wired `specs.memories` into `.acdd/workflows/default.yaml` gates, added `.acdd/workflows/memory_optimize.yaml` (+ skill example) consuming `specs.retrospectives` and `specs.memories`, and ignored `/docs/memories/.retro/`.

## Active Tracks (5–6 — Delegated to Subagent)

5. **Unify `context:` and `lenses` via `ContextRef` + `ContextCapability` (+ optional `with:`)**
   - **Single `ContextRef` enum (`crates/acdd-core/src/ports.rs`)**: Parse every `context:` entry in one canonical place into `ContextRef::Paths(String)`, `ContextRef::Specs(String)`, or `ContextRef::Domain { domain: String, name: String }`.
   - **Symmetric `ContextEntry` in `WorkflowDef` (`crates/acdd-state/src/workflow/`)**: Support both compact string shorthand (`- specs.memories`, `- code.symbols`) and parameterized object form (`- context: <ref>`, `with: { ... }`) symmetric with `CheckEntry` (`checks:`).
   - **Typed `ContextCapability` in `DomainCapabilities` (`crates/acdd-core/src/ports.rs`)**:
     - Replace `context_lenses: BTreeSet<String>` (or keep serde compat alias if needed) with `Vec<ContextCapability>` declaring `{ name, scope_mode: ContextScopeMode (Admitted | NodeOnly), default_paths_key: Option<String>, input_schema: SchemaDef }` plus `supports_collection_context: bool` on `DomainCapabilities`.
     - `CodeDomainProvider` declares `scope_mode: NodeOnly` on `symbols` and `tests`, and `Admitted` on `external_callers`.
     - `DocsDomainProvider` declares `supports_collection_context: true`, `default_paths_key: Some("docs")` on `invariants`, and `Admitted` on `workspace_config`.
   - **De-magic `ports.rs` and `tiers.rs`**:
     - Remove `if matches!(domain, "paths" | "specs") -> active_provider("docs")` string hack in `CompositeDomainProvider` (`ports.rs`); route `ContextRef::Paths` / `ContextRef::Specs` explicitly to the enabled provider declaring `supports_collection_context: true`, and validate `with:` params against `ContextCapability.input_schema`.
     - Remove hardcoded `"code.symbols" | "code.tests"` and `"docs.invariants"` / `"docs"` checks in `crates/acdd-state/src/context/tiers.rs`, reading `scope_mode` and `default_paths_key` directly from `ContextCapability`.
     - Update mock providers in `crates/acdd-state/src/context/tests.rs` and `crates/acdd-state/tests/state_integration/common.rs`.

6. **Eliminate All 6 Remaining Spec/Workflow Hardcodes**
   - **`verify_memory_spec_evidence` (`crates/acdd-state/src/usecases/verify.rs`)**:
     - Replace hardcoded `rel.starts_with("archive/") || rel.contains("/archive/")` with owning `SpecConfig::is_ignored_rel_path`.
     - Compare changed `memory` rules against `HEAD` (`read_git_blob_utf8`): enforce active-run `blackboard` lookup ONLY for newly added/changed `#<digits>` / `blackboard#<digits>` citations (so appending a new rule to an existing `docs/memories/<domain>.md` never fails on prior-run `#12` entries), and accept valid workspace Spec / Spec-item IDs (e.g. `RETRO-001`, `AC-006`, `RETRO-001#problem-ref` resolved across `effective_spec_collections` + `archive/`) as non-blackboard evidence while keeping fabricated `#999999` / `#+1` / `+1` failing closed with `MEMORY_EVIDENCE_UNKNOWN`.
   - **Strip project leaks from embedded `code.yaml` and `documentation.yaml`**:
     - Remove `./website/build.sh --check` and `docs/{adr,architecture,reference}/` from embedded `documentation.yaml` guidance.
     - Replace hardcoded `"website/**"`, `"skills/**"`, `"ROADMAP.md"`, `"TESTS.md"`, `"INSTALL.md"`, `"docs/**"`, `"AGENTS.md"` in embedded `code.yaml` and `documentation.yaml` `polish.scope` with universal `[".acdd/workflows/**", ".acdd/specs/**", ".acdd/lint/**", "paths.docs", "paths.adrs", "paths.instructions", "CHANGELOG.md", ".gitignore"]`; move repo-specific `"website/**"`, `"skills/**"`, `"ROADMAP.md"`, `"INSTALL.md"` to `.acdd/workflows/default.yaml` `polish` `gates_patch`.
   - **Base-selector fallback on `specs.<base>` (`memory` / `contract`)**:
     - Allow `specs.memory` (in `scope:` and `context:`) to match collections whose `effective_builtin_base == Some("memory")` when no collection is literally keyed `"memory"` (symmetric with `docs.specs`), and update embedded `gate_templates.memory_synthesis` (`code.yaml`, `documentation.yaml`) from repo-specific `specs.memories` to base-agnostic `specs.memory`.
   - **Single-owner router-doc ignore in `SpecConfig`**:
     - Consolidate the 3x duplicated `matches!(name, "README.md" | "AGENTS.md" | "CLAUDE.md")` across `crates/acdd-spec/src/discovery.rs` and `src/state_bridge/common.rs` into `DEFAULT_SPEC_IGNORE` (`["archive", ".*", "README.md", "AGENTS.md", "CLAUDE.md"]`) / `SpecConfig::is_ignored_entry_name` so `SpecConfig.ignore` is the single owner for both subdirectories and router files.
   - **Schema-driven empty `items` in `SpecDocument` (`crates/acdd-spec/src/spec.rs`)**:
     - Pre-populate `SpecDocument.items` keys with empty `Vec::new()` for every collection declared in `SpecSchemaDef.items` during `parse_spec_with_collections`, and delete the hardcoded `if field == "rules" && self.spec.as_deref() == Some("memory")` branch in `spec.rs`.

## Verification

- `(! rg -n "retrospective" crates/ src/)` and zero `./website/` in `crates/acdd-state/src/workflow/embedded/`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --package acdd-core --lib && cargo test --package acdd-spec --lib && cargo test --package acdd-state --lib && cargo test --package acdd-state --test state_integration && cargo test --lib && cargo test --test acdd && cargo test --test commitment_integrity`
- `cargo run -q -- build && cargo run -q -- validate && cargo run -q -- lint`
- `duplicates` and `dead_tails` clean across all modified paths; `/tmp/contextunity-test-*` `BEFORE == AFTER`.
