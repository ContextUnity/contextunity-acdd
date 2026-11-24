---
id: AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
title: "Typed Graph Core, Full LanguageLinker Trait Inversion, Context-Bag Removal & Idiomatic Rust Cleanup"
doc_type: contract
status: planned
depends_on:
  - AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
guidance:
  hitl: pro
owners:
  - Cargo.toml
  - crates/acdd-core/src/
  - crates/acdd-index/src/
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/
  - docs/
  - tests/
  - acdd.yaml
invariants:
  - 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node.kind, Node.language, CompactEdge, and TraversalOptions use compact Copy enums rather than heap-allocated strings or 16-byte tag wrappers, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX enforces 0 raw node.details[key] accesses at severity: error across all crates.'
  - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: LanguageLinker and FileRelationContext are narrowed to pub(crate) behind a crate-private linker dispatch seam while keeping LanguageProfile pub (0 #[allow(private_interfaces)] escapes), ImportContext and semantic_context::Context carry zero per-language fields, and universal resolution/value-flow engines delegate strictly through LanguageLinker hooks with zero __rust_* magic strings or hardcoded language branches.'
  - 'INV-AR005-IDIOMATIC-RUST-NO-BAG-SHIMS: Single-caller local-variable/parameter-bag structs, dual key-order DTO shims, unused root Cargo.toml dependencies, std::io::Error error tunneling, and mechanical placeholder doc stubs are completely removed, reducing #[allow(clippy::too_many_arguments)] and .clone() churn across all four crates.'
  - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: acdd-core exposes a unified domain-neutral AcddError (retaining a compatibility alias only where required), and index DB readers and host formatters exchange typed response DTOs rather than mutating untyped serde_json::Map<String, Value> trees in place.'
acceptance_criteria:
  - 'AC-1: Every item listed in Mandatory Removals & Retirement Ledger (unused dependencies petgraph/zstd/hashbrown in root Cargo.toml, dual ClassicFieldsFirstDto/ClassicBindingsFirstDto key-order shims, 14 single-caller context-bag structs, trivial pass-through wrappers, unit-test-only PipelineTestHooks::delta_failure* constructors gated under #[cfg(test)], CompositeDomainProvider::new callers migrated to with_module_flags and unused constructors/getters deleted, 3 #[allow(private_interfaces)] escapes, dead ComplexityLimits threshold fields, EPIPE stdout panics, and 98 mechanical doc stubs) is deleted or #[cfg(test)]-gated with 0 regressions.'
  - 'AC-2: Node.kind (NodeKind), Node.language (LanguageId), CompactEdge ((EdgeKind, EdgeConfidence)), and TraversalOptions/GraphQueryOptions (ImpactDirection, TraversalMode) are strongly typed; all 288 raw node.details["..."] accesses across ast/, models/, interner.rs, languages/, linker/, and db/ are migrated onto typed NodeSemanticEntry / NodeDetailFlags accessors; and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX is promoted to severity: error with 0 warnings.'
  - 'AC-3: LanguageLinker owns receiver member lookup, wrapper/deref projection, polymorphic dispatch, and lexical candidate filtering; semantic_context::Context and ImportContext contain 0 language-specific fields; value_flow/evaluate.rs, dispatch.rs, and candidates.rs contain 0 __rust_* string tags or direct linker::{rust,python,typescript,javascript,html} branches; the 3 duplicate Rust path/type resolvers are unified; and Tier-2 OO profiles (Go, Java, Kotlin, C#, C++, PHP, Ruby) participate in shared member/inheritance indexing.'
  - 'AC-4: acdd-state/src/usecases/{verify,commands/{claim,start,task_ops}}.rs replace Prepared* / GateCallCtx bags and Option<Result<...>> fields with idiomatic ControlFlow and &LoadedRunGraph borrows; acdd-core unifies StateError into AcddError without io::Error tunneling or double YAML parsing in config/loader.rs; WorktreeSpanContext moves out of src/state_bridge to break upward host_telemetry coupling; db/{reader,symbols,traversal} and src/format/ operate on typed DTOs; and src/mcp/server.rs is decomposed into cohesive single-responsibility modules.'
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
repositories: []
linked_to: []
---

# AR-005: Typed Graph Core, Full `LanguageLinker` Trait Inversion, Context-Bag Removal & Idiomatic Rust Cleanup

## Outcome and Purpose

Following [AR-004](archive/AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul.md), a deep workspace-wide Rust architecture audit across all `423` production `.rs` files (`209.7k` LOC) identified structural technical debt and legacy scaffolding that limit type safety, linker extensibility, memory efficiency, and idiomatic readability:

1. **Partial Typing & Raw JSON Debt in `acdd-index`**: Despite 1-byte `Copy` enums (`NodeKind`, `LanguageId`, `EdgeKind`, `EdgeConfidence`) existing in `models::tags`, `Node` still allocates `kind: String`, `language: String`, and `details: serde_json::Value`; `CompactEdge` carries 16-byte `CompactTag` fields; `db/{reader,symbols,traversal}` and `src/format/` build and mutate untyped `serde_json::Value` maps (`794` occurrences workspace-wide); and `288` `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` warnings remain across `linker/` (`151`), `languages/` (`78`), `ast/`+`models/` (`53`), `db/` (`4`), and `interner.rs` (`2`).
2. **Incomplete `LanguageLinker` Inversion**: `pub trait LanguageLinker` leaks crate-private linker types via `#[allow(private_interfaces)]`, `ImportContext` and `semantic_context::Context` carry hardcoded `rust`/`python`/`typescript`/`commonjs` fields, universal resolution phases (`evaluate.rs`, `dispatch.rs`, `candidates.rs`) branch on `274` language-specific checks and `__rust_*` magic string tags, Rust maintains 3 separate path/type normalizers, and Tier-2 OO profiles (`Go`, `Java`, `Kotlin`, `C#`, `C++`, `PHP`, `Ruby`) lack member/inheritance index wiring.
3. **Mechanical Complexity-Slicing Artifacts**: Prior function-level complexity thresholds incentivized mechanical function shredding (`100` `#[allow(clippy::too_many_arguments)]` attributes and `14` single-caller local-variable/parameter-bag structs such as 24-field `ChunkPersistSink` and 18-field `PreparedVerifyPhase` + 19-field `GateCallCtx`), inflating `.clone()` density in `acdd-state` (`22.8/kLOC`).
4. **Unused Dependencies, DTO Shims & Host Ergonomics**: Root `Cargo.toml` carries unused `petgraph`, `zstd`, and `hashbrown` in `[dependencies]` (`0` imports in root `src/` or `tests/`), `typed_facts/node_view.rs` maintains dual key-order DTO shims, `acdd-core` tunnels `StateError` inside `std::io::Error` while parsing `acdd.yaml` twice, `src/mcp/server.rs` mixes 5 responsibilities across `1,499` lines, and `98` mechanical `/// Represents the ... case.` doc stubs leak into `acdd --help`.

---

## Authorized Protected-File Scope

- Root `Cargo.toml` is explicitly authorized strictly to **delete** unused dependencies (`petgraph`, `zstd`, `hashbrown`; `0` new external crates added).

---

## Mandatory Removals & Retirement Ledger

Every item below MUST be explicitly deleted (or strictly gated under `#[cfg(test)]` where noted) and verified absent from production builds (`INV-AR005-IDIOMATIC-RUST-NO-BAG-SHIMS`):

### 1. Dead Dependencies & Unused Config Fields
- **Delete** `petgraph = "0.6"`, `zstd = "0.13"`, and `hashbrown = "0.14"` from `[dependencies]` in root `Cargo.toml` (`0` uses in root `src/` and `tests/`).
- **Delete** unused threshold fields `max_cognitive_complexity`, `max_nesting_depth`, and `max_fanout` from `ComplexityLimits` in `crates/acdd-core/src/lint.rs` once backward-compatible config transition completes.

### 2. Historical Compatibility Shims, Trivial Pass-Through Wrappers & Test-Only Constructors
- **Delete** the historical JSON key-order dual-DTO shim in `crates/acdd-index/src/typed_facts/node_view.rs` (`ClassicFieldsFirstDto`, `ClassicBindingsFirstDto`, `FlowState::original`, and `FlowState::classic_fields_first`) in Wave 2 (after Wave 1 completes, updating `linker/contracts.rs`, `db/writer/cache/tests.rs`, `tests/languages/html/classic_wire.rs`, and bumping `INDEX_SEMANTICS_VERSION`).
- **Delete** trivial dead-parameter / 1-line pass-through wrappers:
  - `resolve_effective_state_db` in `src/state_bridge/common.rs` (ignores `config`, forwards 1:1 to `try_resolve_effective_state_db`).
  - `ast_legacy_error` in `src/format/presentation.rs`.
  - Misplaced `error` and `schema_error` functions in `crates/acdd-core/src/config/constants.rs`, plus `std::io::Error::new(ErrorKind::InvalidData, StateError::new(...))` tunneling and the duplicate first-pass `serde_yaml::Value` parse in `crates/acdd-core/src/config/loader.rs`.
- **Delete or `#[cfg(test)]`-gate** verified test-only constructors and getters:
  - Gate the 3 unit-test-only fault constructors on `PipelineTestHooks` (`delta_failure`, `delta_failure_before_publication`, `delta_failure_during_verification`) in `src/engine/pipeline/hooks.rs` strictly under `#[cfg(test)]` (keeping `new_base` and integration-test-used constructors intact).
  - Delete `CompositeDomainProvider::new` (`crates/acdd-core/src/ports.rs`) after migrating `crates/acdd-state` test callers (`src/context/tests.rs` and `tests/state_integration/{profile,validate}.rs` in Wave 1) and remaining callers (`src/state_bridge/`, `tests/acdd/` in Wave 2) to `CompositeDomainProvider::with_module_flags`.
  - Delete unused `SchemaDef::{integer, number, boolean}` helpers (`crates/acdd-core/src/schema.rs`), `StateEngine::{config, backend}` getters in Wave 1 (`crates/acdd-state/src/usecases/mod.rs`), and `StateEngine::query` (migrating `tests/acdd/verification_and_git.rs` and `crates/acdd-state/tests/` callers in Wave 2) + `Server::with_domain_providers` (`src/mcp/server.rs`).

### 3. Single-Caller Context-Bag Structs & `#[allow(...)]` Suppressions
- **Delete** all 14 synthetic single-caller local-state / parameter-bag structs created during mechanical function slicing, replacing them with idiomatic `ControlFlow`, borrows, or cohesive domain receivers:
  1. `ChunkPersistSink` (24 fields, 15 `&mut` borrows) in `crates/acdd-index/src/db/writer/ingest.rs`.
  2. `EdgeOccurrencesInput` (destructured immediately on entry) in `crates/acdd-index/src/db/writer/ingest.rs`.
  3. `PreparedVerifyPhase` (18 owned fields + `early_failure: Option<Result<VerifyResp, StateError>>`) in `crates/acdd-state/src/usecases/verify.rs`.
  4. `GateCallCtx` (19 re-borrowed fields) in `crates/acdd-state/src/usecases/verify.rs`.
  5. `PreparedClaimTarget` in `crates/acdd-state/src/usecases/commands/claim.rs`.
  6. `PreparedRunInputs` in `crates/acdd-state/src/usecases/commands/start.rs`.
  7. `PreparedStartGraph` in `crates/acdd-state/src/usecases/commands/start.rs`.
  8. `SnapshotTargetContext` in `crates/acdd-state/src/usecases/commands/task_ops.rs`.
  9. `BuiltinResolutionFlags` in `crates/acdd-index/src/linker/resolution/value_flow_phase/candidates.rs`.
  10. `MemberAccumulator` (plus nested tuple aliases `MacroTraitTables` and `TraitDeclTables`) in `crates/acdd-index/src/linker/rust/profile_linker.rs`.
  11. `WalkContext` in `crates/acdd-index/src/scanner/walk.rs`.
  12. `TextMatchPreviewParams` in `crates/acdd-index/src/db/symbols/search.rs`.
  13. `ParsedAdapterConfig` in `crates/acdd-core/src/config/loader.rs`.
  14. `SyncBuffers` (`crates/acdd-state/src/machine/sync.rs`) and `ReopenAcc` (`crates/acdd-state/src/machine/reopen.rs`).
- **Delete** all 3 `#[allow(private_interfaces)]` attributes in Wave 2 alongside `LanguageLinker`/`FileRelationContext` `pub(crate)` narrowing behind a crate-private dispatch seam (`crates/acdd-index/src/languages/profile.rs`, `crates/acdd-index/src/linker/python/profile_linker.rs`, keeping `LanguageProfile` `pub` so host `src/` and public profile helpers stay intact), `#[allow(unused_imports)]` (`typed_facts/binary.rs`), `#[allow(clippy::result_unit_err)]` (`typed_facts/flow_store.rs`, `linker/routes.rs`), and eliminate single-use `#[allow(clippy::too_many_arguments)]` helpers across all crates.

### 4. Stringly-Typed Fields, Raw `node.details[...]` Accesses & Untyped JSON Pipelines
- **Delete** `String` storage for `Node.kind` and `Node.language` in `crates/acdd-index/src/models/types.rs` and 16-byte `CompactTag` `kind`/`confidence` storage on `CompactEdge` in `crates/acdd-index/src/models/compact_graph.rs`.
- **Delete** all `288` raw `node.details["..."]` JSON indexing/mutation sites across `linker/` (`151`), `languages/` (`78`), `ast/`+`models/` (`53`), `db/` (`4`), and `interner.rs` (`2`) (Wave 1 Task 1 clears the `55` sites in `ast/`, `models/`, and `interner.rs`; Wave 2 Task 4 clears the remaining `233` sites across `languages/`, `linker/`, and `db/` with typed `models/details.rs` accessors and ratchets the rule to `error`).
- **Delete** stringly-typed `inbound: bool`, `mode: Option<&str>`, and `direction: Option<&str>` fields from `TraversalOptions` and `GraphQueryOptions` (`crates/acdd-index/src/db/traversal/types.rs`).
- **Delete** in-place `serde_json::Map<String, Value>` response mutation in `crates/acdd-index/src/db/{reader,symbols,traversal}/**` and `src/format/presentation.rs`, plus the `ValidateResp` JSON round-trip patch in `src/cli/validate.rs:29-33`.

### 5. Inverted Language-Specific Linker State, `__rust_*` Magic Strings & Duplicate Resolvers
- **Delete** per-language fields `commonjs`, `python`, `rust`, `rust_forwarding`, and `typescript` from `semantic_context::Context` (`crates/acdd-index/src/linker/semantic_context/context.rs`) and `rust_forwarding` / `rust_library_modules` from `ImportContext` / `ImportNormalizationContext` (`crates/acdd-index/src/languages/profile.rs`).
- **Delete** all magic `__rust_*` string tags (`__rust_tuple`, `__rust_array`, `__rust_if_join`, `__rust_generic_call`, `__rust_closure!`, `__rust_ref`, `__rust_ref_mut`, `__rust_field:*`, `__rust_identity_some`) and direct `linker::{rust,python,typescript,javascript,html}` branches from `linker/value_flow/evaluate.rs`, `linker/resolution/value_flow_phase/{dispatch,inferred,candidates}.rs`, and `linker/resolution/imports.rs`.
- **Delete** duplicate Rust path/type resolvers in `crates/acdd-index/src/linker/rust/profile_linker.rs` (`namespace`, `resolve_qualified_path`, `resolve_type`) and string-slicing `fallback_receiver_value` in `crates/acdd-index/src/linker/rust/resolution.rs`, consolidating onto one canonical `RustForwarding` + `ValueFlow` seam.

### 6. Mechanical Doc Stubs & Stdout Panic Hazards
- **Delete** all `98` mechanical template doc comments (`/// Represents the ... case.`, `/// Performs ...`, `/// Implements ... support.`) across `src/cli/args.rs`, `src/lib.rs`, `crates/acdd-index/src/scanner/walk.rs`, and workspace modules, replacing with accurate domain documentation inside each owning task scope.
- **Delete** `.unwrap_or_else(|error| panic!("failed printing to stdout: ..."))` on `EPIPE` in `src/cli/docs_check.rs`.

---

## Tasks in this milestone

### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: "Freeze AcddError design and Copy-enum tag contracts in acdd-core/acdd-index, document architectural decisions in docs/adr/, register canonical seam test modules, and lock disjoint Wave 1 scopes"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md
  - docs/adr/
  - crates/acdd-core/src/errors.rs
  - crates/acdd-index/src/models/tags.rs
  - tests/core.rs
  - tests/linker.rs
  - tests/mcp.rs
invariants:
  - 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2 executes cross-crate cutovers.'
acceptance_criteria:
  - 'AC-1: AcddError type design, Copy enum tag contracts (NodeKind, LanguageId, EdgeKind, EdgeConfidence), ADR updates, and seam test module registrations are frozen with 0 scope overlaps across all three parallel Wave 1 tasks.'
status: planned
subtasks:
  - subtask_ref: freeze-core-error-and-tag-contracts
    title: "Freeze AcddError alias/type contract in errors.rs, Copy enum tag helpers in models/tags.rs, and Wave 1/Wave 2 ADR specs"
  - subtask_ref: register-seam-tests-and-disjoint-scopes
    title: "Pre-register seam test modules in tests/{core,linker,mcp}.rs and verify disjoint Wave 1 write scopes"
```

### task: ast-models-scanner-details-and-dto-shim-cleanup

```yaml
task_ref: ast-models-scanner-details-and-dto-shim-cleanup
target: "Stage additive Copy-enum and typed detail accessors on Node and CompactEdge, delete WalkContext, clear #[allow(unused_imports)]/#[allow(clippy::result_unit_err)] in typed_facts, and eliminate all 55 raw node.details[key] accesses and mechanical doc stubs in ast/, models/, typed_facts/, scanner/, and interner.rs"
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
tags:
  - default
scope:
  - crates/acdd-index/src/interner.rs
  - crates/acdd-index/src/models/
  - crates/acdd-index/src/typed_facts/
  - crates/acdd-index/src/ast/
  - crates/acdd-index/src/scanner/
  - tests/languages/
invariants:
  - 'INV-AR005-AST-MODELS-CLEAN-DETAILS: ast/, models/, typed_facts/, scanner/, and interner.rs contain 0 raw node.details[key] accesses, 0 WalkContext bags, and 0 mechanical doc stubs.'
acceptance_criteria:
  - 'AC-1: Additive typed NodeKind/LanguageId/EdgeKind/EdgeConfidence constructors/accessors and typed NodeDetailKey accessors in models/details.rs are staged without breaking downstream callers; #[allow(unused_imports)] in typed_facts/binary.rs, #[allow(clippy::result_unit_err)] in typed_facts/flow_store.rs, and WalkContext are deleted; mechanical doc stubs in ast/models/scanner/typed_facts are replaced; and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX reports 0 warnings across ast/, models/, and interner.rs.'
status: planned
subtasks:
  - subtask_ref: stage-typed-accessors-and-clean-typed-facts
    title: "Add typed Copy-enum constructors/accessors on Node and CompactEdge and remove #[allow] suppressions in typed_facts"
  - subtask_ref: eliminate-ast-models-raw-details-and-walk-context
    title: "Migrate all 55 raw node.details[key] sites across ast/, models/, and interner.rs onto typed semantic accessors, remove scanner WalkContext, and replace mechanical doc stubs in scope"
```

### task: language-linker-trait-inversion-and-tier2-members

```yaml
task_ref: language-linker-trait-inversion-and-tier2-members
target: "Extend LanguageLinker hooks to eliminate per-language Context fields, __rust_* magic strings, BuiltinResolutionFlags, MemberAccumulator, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member indexing while keeping public profile/linker test seams compiling until Wave 2"
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
tags:
  - default
scope:
  - crates/acdd-index/src/languages/
  - crates/acdd-index/src/linker/
  - docs/adr/0011-language-linkers-and-framework-manifests.md
  - tests/linker/
invariants:
  - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string tags in evaluate.rs, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
acceptance_criteria:
  - 'AC-1: LanguageLinker owns member lookup, wrapper projection, polymorphic dispatch, and candidate filtering while keeping existing public LanguageProfile/LanguageLinker test calls compiling until Wave 2; BuiltinResolutionFlags, MemberAccumulator, __rust_* string tags, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs across languages/ and linker/ are eliminated; and Go/Java/Kotlin/C#/C++/PHP/Ruby resolve class/interface/method members via shared member indexing.'
status: planned
subtasks:
  - subtask_ref: invert-language-linker-trait-and-context
    title: "Remove per-language fields from ImportContext and semantic_context::Context and migrate dispatch/evaluate hooks onto LanguageLinker"
  - subtask_ref: unify-rust-resolvers-tier2-members-and-doc-stubs
    title: "Consolidate duplicate Rust path/type resolvers, delete BuiltinResolutionFlags/MemberAccumulator/__rust_* string tags and doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member/inheritance indexing"
```

### task: state-usecase-control-flow-and-context-bag-removal

```yaml
task_ref: state-usecase-control-flow-and-context-bag-removal
target: "Delete PreparedVerifyPhase, GateCallCtx, PreparedClaimTarget, PreparedRunInputs, PreparedStartGraph, SnapshotTargetContext, SyncBuffers, ReopenAcc, StateEngine::{config, backend} getters, and mechanical doc stubs across acdd-state, replacing with idiomatic ControlFlow and borrowed graph slices"
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
tags:
  - default
scope:
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
invariants:
  - 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, 0 phantom_tested config/backend getters, and 0 mechanical doc stubs.'
acceptance_criteria:
  - 'AC-1: All 8 single-caller context/parameter bags and StateEngine::{config, backend} in acdd-state are deleted (migrating internal tests cleanly, with StateEngine::query deferred to Wave 2 alongside tests/acdd/ and crates/acdd-state/tests/), CompositeDomainProvider::new callers in acdd-state tests are migrated to with_module_flags, #[allow(clippy::too_many_arguments)] count in acdd-state drops by >= 80%, mechanical doc stubs in acdd-state are replaced, and .clone() density in usecases/ and machine/ is measurably reduced with full state_integration parity.'
status: planned
subtasks:
  - subtask_ref: refactor-verify-claim-start-usecases
    title: "Replace PreparedVerifyPhase + GateCallCtx and claim/start/task_ops bags with ControlFlow and &LoadedRunGraph borrows"
  - subtask_ref: clean-machine-accumulators-and-phantom-getters
    title: "Remove SyncBuffers, ReopenAcc, StateEngine::{config, backend}, CompositeDomainProvider::new test calls, RunHarness error-code misuse, and mechanical doc stubs in acdd-state"
```

### task: typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup

```yaml
task_ref: typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup
target: "Cut over Node.kind, Node.language, and CompactEdge struct fields to Copy enums across all callers, delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims, narrow LanguageLinker/FileRelationContext to pub(crate) behind a crate-private dispatch seam (removing #[allow(private_interfaces)] while keeping LanguageProfile pub), eliminate all remaining 233 raw node.details[key] sites across languages/, linker/, and db/ and ratchet ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to error, delete StateEngine::query across all test callers, unify StateError into AcddError without io::Error tunneling, delete ChunkPersistSink/EdgeOccurrencesInput/TextMatchPreviewParams, migrate db/{reader,symbols,traversal} and src/format/ onto typed DTOs, decompose src/mcp/server.rs and align acdd.yaml/docs/architecture/, delete unused petgraph/zstd/hashbrown from root Cargo.toml, and sweep all remaining mechanical doc stubs"
contract_revision: 1
depends_on:
  - ast-models-scanner-details-and-dto-shim-cleanup
  - language-linker-trait-inversion-and-tier2-members
  - state-usecase-control-flow-and-context-bag-removal
tags:
  - default
scope:
  - Cargo.toml
  - acdd.yaml
  - crates/acdd-core/src/
  - crates/acdd-index/src/
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/
  - docs/architecture/
  - docs/reference/
  - tests/
invariants:
  - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: Node and CompactEdge store 1-byte Copy enums across all callers, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, root Cargo.toml has 0 unused dependencies, ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, db readers and formatters use typed DTOs, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
acceptance_criteria:
  - 'AC-1: Node.kind (NodeKind), Node.language (LanguageId), and CompactEdge ((EdgeKind, EdgeConfidence)) field cutovers compile cleanly across all crates and integration tests; ClassicFieldsFirstDto/ClassicBindingsFirstDto/FlowState::{original,classic_fields_first} are deleted (updating linker/contracts.rs, db/writer/cache/tests.rs, tests/languages/html/classic_wire.rs) and INDEX_SEMANTICS_VERSION bumped; LanguageLinker and FileRelationContext are narrowed to pub(crate) via a crate-private linker dispatch seam (removing all 3 #[allow(private_interfaces)] escapes and migrating tests/languages/profiles.rs:153-160 while keeping LanguageProfile pub); all remaining 233 raw node.details[key] sites across languages/, linker/, and db/ are migrated onto typed accessors and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX is promoted to severity: error with 0 warnings; StateEngine::query is deleted (migrating tests/acdd/verification_and_git.rs and crates/acdd-state/tests/state_integration/{usecases_e2e,usecases_queries_lifecycle}.rs); petgraph, zstd, and hashbrown are deleted from root Cargo.toml; AcddError replaces io::Error tunneling across all loader callers; CompositeDomainProvider::new, unused SchemaDef helpers, ParsedAdapterConfig, ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, resolve_effective_state_db, ast_legacy_error, Server::with_domain_providers, and dead ComplexityLimits fields are deleted; the 3 delta_failure* constructors on PipelineTestHooks are #[cfg(test)]-gated; src/mcp/server.rs is split cleanly with acdd.yaml (ARCH-SUBPROCESS-CONFINED, ARCH-TELEMETRY-CONFINED) and docs/architecture/cross-platform.md links aligned; docs_check EPIPE panic is fixed; and 0 template doc stubs remain in CLI --help or workspace source.'
status: planned
subtasks:
  - subtask_ref: atomic-node-compact-edge-linker-privacy-and-acdd-error-cutover
    title: "Cut over Node.kind, Node.language, and CompactEdge struct fields to Copy enums across all crates/tests, delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims, narrow LanguageLinker/FileRelationContext via crate-private dispatch (0 #[allow(private_interfaces)], keeping LanguageProfile pub), delete StateEngine::query across crates/acdd-state/tests/ and tests/acdd/, generalize AcddError and single-pass config/loader.rs across all callers, relocate WorktreeSpanContext, and delete unused petgraph/zstd/hashbrown from root Cargo.toml"
  - subtask_ref: typed-db-reader-dtos-ingest-bag-removal-and-details-ratchet
    title: "Delete ChunkPersistSink, EdgeOccurrencesInput, and TextMatchPreviewParams, migrate all remaining 233 raw node.details[key] sites across languages/, linker/, and db/ and promote ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to error, adopt ImpactDirection/TraversalMode on TraversalOptions, and replace untyped Value maps in db/{reader,symbols,traversal} and src/format/ with typed DTOs"
  - subtask_ref: mcp-server-split-cli-hardening-and-remaining-doc-stubs
    title: "Decompose src/mcp/server.rs with acdd.yaml and docs/architecture/ aligned, #[cfg(test)]-gate PipelineTestHooks::delta_failure* constructors, fix cli/docs_check EPIPE handling, and replace all remaining mechanical doc stubs in acdd-core, db/, and src/"
```
