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
  - 'AC-1: Every item listed in Mandatory Removals & Retirement Ledger (unused dependencies petgraph/zstd/hashbrown in root Cargo.toml, dual ClassicFieldsFirstDto/ClassicBindingsFirstDto key-order shims, 14 single-caller context-bag structs, trivial pass-through wrappers, unit-test-only PipelineTestHooks::delta_failure* constructors gated under #[cfg(test)], CompositeDomainProvider::new callers migrated to with_module_flags and unused constructors/getters deleted including StateEngine::{config, backend, query} in Wave 2b, 3 #[allow(private_interfaces)] escapes, dead ComplexityLimits threshold struct fields and ComplexityLimitsWire with strict deny_unknown_fields and doc/test updates, EPIPE stdout panics in docs_check.rs + cli/mod.rs test contract, and 98 mechanical doc stubs) is deleted or #[cfg(test)]-gated with 0 regressions.'
  - 'AC-2: Node.kind (closed 36-variant Copy NodeKind including Object plus canonical HtmlRequest/HtmlContext + typed request_role metadata preserving ADR 0011 zero-Rust overrides), Node.language (LanguageId), CompactEdge ((closed 34-variant EdgeKind including TemplateImports, EdgeConfidence) with fail-closed TryFrom<&PublicEdge>), ReferenceKind (closed 22-variant Copy enum covering live template/SQL reference kinds), and TraversalOptions/GraphQueryOptions (ImpactDirection, TraversalMode) are strongly typed; all 290 production raw node.details["..."] accesses across ast/, models/, typed_facts/, interner.rs, languages/, linker/, and db/ (plus 3 test-only sites in db/writer/cache/tests.rs) are migrated onto typed NodeSemanticEntry / NodeDetailFlags accessors; and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX is promoted to severity: error with 0 warnings.'
  - 'AC-3: LanguageLinker owns receiver member lookup, wrapper/deref projection, polymorphic dispatch, and lexical candidate filtering; semantic_context::Context and ImportContext contain 0 language-specific fields; value_flow/evaluate.rs, dispatch.rs, and candidates.rs contain 0 __rust_* string tags or direct linker::{rust,python,typescript,javascript,html} branches; the 3 duplicate Rust path/type resolvers are unified; and Tier-2 OO profiles (Go, Java, Kotlin, C#, C++, PHP, Ruby) participate in shared member/inheritance indexing.'
  - 'AC-4: acdd-state/src/usecases/{verify,commands/{claim,start,task_ops}}.rs replace Prepared* / GateCallCtx bags and Option<Result<...>> fields with idiomatic ControlFlow and &LoadedRunGraph borrows in Wave 1 (with StateEngine::{config, backend, query} removal cleanly deferred to Wave 2b); acdd-core unifies StateError into AcddError without io::Error tunneling or double YAML parsing in config/loader.rs; WorktreeSpanContext moves out of src/state_bridge to break upward host_telemetry coupling; db/{reader,symbols,traversal} and src/format/ operate on typed DTOs; and src/mcp/server.rs is decomposed into cohesive single-responsibility modules.'
  - 'AC-5: Wave 2c (submit-verify-env-isolation-and-scope-fixes) integrates all child-command env isolation, submit/verify index_generation preservation, and scope-resolution fixes from goal-20261124141330-293224-1 (commit 4bb239694) plus its follow-ups (directory-style scope matching in shell_template.rs, remaining test binary env isolation in tests/{acdd,core,languages,mcp}/ via isolate_acdd_env/run_cli_args, Phase 3 StaleClaimRevision pre-trace check, and multi-check index_generation state_integration proof) into AR-005 and retires the orphan hotfix runs.'
  - 'AC-6: Wave 2d (dead-tails-duplicates-and-call-graph-closure) fixes Rust call-graph / extractor and dead_tails closure gaps (including function-value combinator arguments such as map/and_then/map_rows, serde/schemars string callable attributes, languages/sql self-shadowed tokens call resolution, and Context::with_flows / NamedBindings propagation) and eliminates remaining genuine dead_tails, phantom_tested helpers, and block/similar duplicates across acdd-core, acdd-index, acdd-state, and src/ with 0 regressions.'
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

1. **Partial Typing & Raw JSON Debt in `acdd-index`**: Despite 1-byte `Copy` enums (`NodeKind`, `LanguageId`, `EdgeKind`, `EdgeConfidence`) existing in `models::tags`, `Node` still allocates `kind: String`, `language: String`, and `details: serde_json::Value`; `CompactEdge` carries 16-byte `CompactTag` fields; `db/{reader,symbols,traversal}` and `src/format/` build and mutate untyped `serde_json::Value` maps (`794` occurrences workspace-wide); and `290` `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` warnings remain across `linker/` (`153`), `languages/` (`78`), `ast/` (`39`), `models/` (`10`), `typed_facts/` (`4`), `db/` (`4`), and `interner.rs` (`2`) (plus 3 test-only sites in `db/writer/cache/tests.rs`).
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
- **Delete** unused stored threshold fields `max_cognitive_complexity`, `max_nesting_depth`, and `max_fanout` from `ComplexityLimits` in `crates/acdd-core/src/lint.rs` and remove `ComplexityLimitsWire` so `#[serde(default, deny_unknown_fields)]` rejects deprecated threshold keys strictly (updating `docs/reference/configuration.md:113` and `tests/core/{arch_lint,ast_contracts}.rs`).

### 2. Historical Compatibility Shims, Trivial Pass-Through Wrappers & Test-Only Constructors
- **Delete** the historical JSON key-order dual-DTO shim in `crates/acdd-index/src/typed_facts/node_view.rs` (`ClassicFieldsFirstDto`, `ClassicBindingsFirstDto`, `FlowState::original`, and `FlowState::classic_fields_first`) in Wave 2a (after Wave 1 completes, updating `linker/contracts.rs`, `db/writer/cache/tests.rs`, `tests/languages/html/classic_wire.rs`, and bumping `INDEX_SEMANTICS_VERSION`).
- **Delete** trivial dead-parameter / 1-line pass-through wrappers:
  - `resolve_effective_state_db` in `src/state_bridge/common.rs` (ignores `config`, forwards 1:1 to `try_resolve_effective_state_db`).
  - `ast_legacy_error` in `src/format/presentation.rs`.
  - Misplaced `error` and `schema_error` functions in `crates/acdd-core/src/config/constants.rs`, plus `std::io::Error::new(ErrorKind::InvalidData, StateError::new(...))` tunneling and the duplicate first-pass `serde_yaml::Value` parse in `crates/acdd-core/src/config/loader.rs`.
- **Delete or `#[cfg(test)]`-gate** verified test-only constructors and getters:
  - Gate the 3 unit-test-only fault constructors on `PipelineTestHooks` (`delta_failure`, `delta_failure_before_publication`, `delta_failure_during_verification`) in `src/engine/pipeline/hooks.rs` strictly under `#[cfg(test)]` (keeping `new_base` and integration-test-used constructors intact).
  - Delete `CompositeDomainProvider::new` (`crates/acdd-core/src/ports.rs`) after migrating `crates/acdd-state` test callers (`src/context/tests.rs` and `tests/state_integration/{profile,validate}.rs` in Wave 1) and remaining callers (`src/state_bridge/`, `tests/acdd/` in Wave 2b) to `CompositeDomainProvider::with_module_flags`.
  - Delete unused `SchemaDef::{integer, number, boolean}` helpers (`crates/acdd-core/src/schema.rs`), `StateEngine::{config, backend, query}` getters/methods (migrating all `.backend()`/`.config()`/`.query()` calls in `crates/acdd-state/src/usecases/tests.rs`, `crates/acdd-state/tests/state_integration/{usecases_e2e,usecases_queries_lifecycle,roadmap_query_waves}.rs`, and `tests/acdd/verification_and_git.rs` together in Wave 2b) + `Server::with_domain_providers` (`src/mcp/server.rs`).

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
- **Delete** all 3 `#[allow(private_interfaces)]` attributes in Wave 2a alongside `LanguageLinker`/`FileRelationContext` `pub(crate)` narrowing behind a crate-private dispatch seam (`crates/acdd-index/src/languages/profile.rs`, `crates/acdd-index/src/linker/python/profile_linker.rs`, keeping `LanguageProfile` `pub` so host `src/` and public profile helpers stay intact), `#[allow(unused_imports)]` (`typed_facts/binary.rs`), `#[allow(clippy::result_unit_err)]` (`typed_facts/flow_store.rs` in Wave 2a, `linker/routes.rs` in Wave 1), and eliminate single-use `#[allow(clippy::too_many_arguments)]` helpers across all crates.

### 4. Stringly-Typed Fields, Raw `node.details[...]` Accesses & Untyped JSON Pipelines
- **Delete** `String` storage for `Node.kind` and `Node.language` in `crates/acdd-index/src/models/types.rs` and 16-byte `CompactTag` `kind`/`confidence` storage on `CompactEdge` in `crates/acdd-index/src/models/compact_graph.rs` in Wave 2a (preserving zero-Rust `.acdd/frameworks/*.toml` `HtmlRequestAttributes` extensibility via canonical `NodeKind::HtmlRequest`/`HtmlContext` + typed `request_role` detail metadata per ADR 0011, and fail-closed `TryFrom<&PublicEdge> for CompactEdge`).
- **Delete** all `290` production raw `node.details["..."]` JSON indexing/mutation sites across `linker/` (`153`), `languages/` (`78`), `ast/` (`39`), `models/` (`10`), `typed_facts/` (`4`), `db/` (`4`), and `interner.rs` (`2`) (plus 3 test-only sites in `db/writer/cache/tests.rs`: Wave 1 Task 1 clears the `55` sites in `ast/`, `models/`, `typed_facts/`, and `interner.rs`; Wave 2a Task 4 clears the remaining `235` production sites across `languages/`, `linker/`, and `db/` plus the 3 cache test sites with typed `models/details.rs` accessors and ratchets the rule to `error`).
- **Delete** stringly-typed `inbound: bool`, `mode: Option<&str>`, and `direction: Option<&str>` fields from `TraversalOptions` and `GraphQueryOptions` (`crates/acdd-index/src/db/traversal/types.rs`).
- **Delete** in-place `serde_json::Map<String, Value>` response mutation in `crates/acdd-index/src/db/{reader,symbols,traversal}/**` and `src/format/presentation.rs`, plus the `ValidateResp` JSON round-trip patch in `src/cli/validate.rs:29-33`.

### 5. Inverted Language-Specific Linker State, `__rust_*` Magic Strings & Duplicate Resolvers
- **Delete** per-language fields `commonjs`, `python`, `rust`, `rust_forwarding`, and `typescript` from `semantic_context::Context` (`crates/acdd-index/src/linker/semantic_context/context.rs`) and `rust_forwarding` / `rust_library_modules` from `ImportContext` / `ImportNormalizationContext` (`crates/acdd-index/src/languages/profile.rs`).
- **Delete** all magic `__rust_*` string tags (`__rust_tuple`, `__rust_array`, `__rust_if_join`, `__rust_generic_call`, `__rust_closure!`, `__rust_ref`, `__rust_ref_mut`, `__rust_field:*`, `__rust_identity_some`) and direct `linker::{rust,python,typescript,javascript,html}` branches from `linker/value_flow/evaluate.rs`, `linker/resolution/value_flow_phase/{dispatch,inferred,candidates}.rs`, and `linker/resolution/imports.rs`.
- **Delete** duplicate Rust path/type resolvers in `crates/acdd-index/src/linker/rust/profile_linker.rs` (`namespace`, `resolve_qualified_path`, `resolve_type`) and string-slicing `fallback_receiver_value` in `crates/acdd-index/src/linker/rust/resolution.rs`, consolidating onto one canonical `RustForwarding` + `ValueFlow` seam.

### 6. Mechanical Doc Stubs & Stdout Panic Hazards
- **Delete** all `98` mechanical template doc comments (`/// Represents the ... case.`, `/// Performs ...`, `/// Implements ... support.`) across `src/cli/args.rs`, `src/lib.rs`, `crates/acdd-index/src/scanner/walk.rs`, and workspace modules, replacing with accurate domain documentation inside each owning task scope.
- **Delete** `.unwrap_or_else(|error| panic!("failed printing to stdout: ..."))` on `EPIPE` in `src/cli/docs_check.rs` and update the matching broken-pipe panic assertion in `src/cli/mod.rs:917-927`.

---

## Tasks in this milestone

### Wave Isolation and Frozen Seams

Wave 1 has three disjoint write partitions:

| Task | Write partition |
| --- | --- |
| `ast-models-scanner-details-and-dto-shim-cleanup` | `crates/acdd-index/src/{ast,models,typed_facts,scanner}/`, `crates/acdd-index/src/interner.rs`, `tests/languages/` |
| `language-linker-trait-inversion-and-tier2-members` | `crates/acdd-index/src/{languages,linker}/`, `docs/adr/0011-language-linkers-and-framework-manifests.md`, `tests/linker/` |
| `state-usecase-control-flow-and-context-bag-removal` | `crates/acdd-state/src/`, `crates/acdd-state/tests/` |

These partitions have no shared write paths. Wave 1 tasks compile independently against the frozen `AcddError` alias and graph-tag contracts. Wave 2a depends on the first two tasks; Wave 2b depends on Wave 2a and the state task. Wave 0 does not register root `tests/{core,linker,mcp}.rs` modules or add empty `#[path]` stubs; each later task registers tests with its production seam. Verify the freeze with strict Clippy and library tests for `acdd-core` and `acdd-index`, plus `cargo test --test commitment_integrity`.

### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: "Freeze AcddError design and complete closed Copy-enum tag contracts (36-variant NodeKind including ModuleDeclaration/View/Service/ToolRegistration/EventHandlerRegistration/HtmlRequest/HtmlContext/Object + 34-variant EdgeKind covering all SQL/projection/template/doc edge kinds including TemplateImports + 22-variant ReferenceKind) in acdd-core/acdd-index, align ADR 0001/0003/0010/0011 (settling HtmlRequestAttributes canonical NodeKind + typed request_role metadata), and lock disjoint Wave 1 scopes"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md
  - docs/adr/
  - crates/acdd-core/src/errors.rs
  - crates/acdd-index/src/models/tags.rs
invariants:
  - 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2a/2b execute cross-crate cutovers.'
acceptance_criteria:
  - 'AC-1: AcddError type design, complete closed Copy-enum tag contracts (36-variant NodeKind adding ModuleDeclaration, View, Service, ToolRegistration, EventHandlerRegistration, HtmlRequest, HtmlContext, Object; 34-variant EdgeKind adding DefinesTable, QueriesTable, MutatesTable, ReferencesTable, UsesModel, CallsEndpoint, ReferencesRoute, RendersTemplate, InjectsDependency, Injects, Renders, ContextProvider, Includes, TemplateImports, CitesDoc, CitedByDoc, GovernedBy; 22-variant ReferenceKind adding TemplateImports, TemplateLoads, TemplateBindings, QueriesTable, MutatesTable, ReferencesTable; LanguageId; EdgeConfidence), ADR updates (docs/adr/0001-crate-dag-and-layer-boundaries.md, 0003-unified-profile-graph-and-state-machine.md, 0010-static-value-flow-and-receiver-resolution.md, and 0011-language-linkers-and-framework-manifests.md settling HtmlRequestAttributes canonical NodeKind::HtmlRequest/HtmlContext + typed request_role metadata preserving zero-Rust .acdd/frameworks/*.toml overrides), and disjoint Wave 1 write scopes are frozen with 0 scope overlaps and verified inside acdd-core/acdd-index.'
status: planned
subtasks:
  - subtask_ref: freeze-core-error-and-tag-contracts
    title: "Freeze AcddError alias/type contract in errors.rs, complete 36-variant NodeKind, 34-variant EdgeKind, and 22-variant ReferenceKind tag helpers in models/tags.rs, and update ADRs 0001/0003/0010/0011"
  - subtask_ref: verify-disjoint-wave1-scopes
    title: "Lock disjoint Wave 1 write scopes, sequential Wave 2a/2b handoff contracts, and acdd-core/acdd-index tag-contract checks without cross-task test stubs"
```

### task: ast-models-scanner-details-and-dto-shim-cleanup

```yaml
task_ref: ast-models-scanner-details-and-dto-shim-cleanup
target: "Stage additive Copy-enum and typed detail accessors on Node and CompactEdge, delete WalkContext, clear #[allow(unused_imports)] in typed_facts/binary.rs, and eliminate all 55 raw node.details[key] accesses and mechanical doc stubs in ast/, models/, typed_facts/, scanner/, and interner.rs"
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
  - 'AC-1: Additive typed NodeKind/LanguageId/EdgeKind/EdgeConfidence constructors/accessors and typed NodeDetailKey accessors in models/details.rs are staged without breaking downstream callers; #[allow(unused_imports)] in typed_facts/binary.rs and WalkContext are deleted (deferring FlowStore::read_named_uncertainties error-type cutover and #[allow(clippy::result_unit_err)] removal to Wave 2a); mechanical doc stubs in ast/models/scanner/typed_facts are replaced; and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX reports 0 warnings across ast/, models/, typed_facts/ (4 sites), and interner.rs (2 sites) totaling 55 eliminated sites.'
status: planned
subtasks:
  - subtask_ref: stage-typed-accessors-and-clean-typed-facts
    title: "Add typed Copy-enum constructors/accessors on Node and CompactEdge and remove #[allow(unused_imports)] in typed_facts/binary.rs while preserving FlowStore::read_named_uncertainties Result<(), ()> compatibility until Wave 2a"
  - subtask_ref: eliminate-ast-models-raw-details-and-walk-context
    title: "Migrate all 55 raw node.details[key] sites across ast/ (39), models/ (10), typed_facts/ (4), and interner.rs (2) onto typed semantic accessors, remove scanner WalkContext, and replace mechanical doc stubs in scope"
```

### task: language-linker-trait-inversion-and-tier2-members

```yaml
task_ref: language-linker-trait-inversion-and-tier2-members
target: "Extend LanguageLinker hooks to eliminate per-language Context fields, universal-linker __rust_* string checks and language branches, BuiltinResolutionFlags, MemberAccumulator, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member indexing while keeping semantic/types.rs unchanged and public profile/linker test seams compiling until Wave 2a"
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
  - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string checks or language-specific branches in universal linker modules, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
acceptance_criteria:
  - 'AC-1: LanguageLinker owns member lookup, wrapper projection, polymorphic dispatch, and candidate filtering via lifetime-preserving LanguageMemberIndex/LanguageImportState hooks while leaving semantic/types.rs untouched and existing public LanguageProfile/LanguageLinker test calls compiling until Wave 2a; BuiltinResolutionFlags, MemberAccumulator, universal-linker __rust_* string checks, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs across languages/ and linker/ are eliminated (deferring internal ValueExpr/TypeExpr variant replacement across semantic/types.rs, typed_facts/binary.rs, languages/rust/value_flow/, and linker/rust/value_flow.rs to Wave 2a); and Go/Java/Kotlin/C#/C++/PHP/Ruby resolve class/interface/method members via shared member indexing.'
status: planned
subtasks:
  - subtask_ref: invert-language-linker-trait-and-context
    title: "Remove per-language fields from ImportContext and semantic_context::Context and migrate dispatch/evaluate/type_resolution hooks onto LanguageLinker"
  - subtask_ref: unify-rust-resolvers-tier2-members-and-doc-stubs
    title: "Consolidate duplicate Rust path/type resolvers, delete BuiltinResolutionFlags/MemberAccumulator, remove universal-linker __rust_* string branches into linker/rust/ hooks, clear doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member/inheritance indexing"
```

### task: state-usecase-control-flow-and-context-bag-removal

```yaml
task_ref: state-usecase-control-flow-and-context-bag-removal
target: "Delete PreparedVerifyPhase, GateCallCtx, PreparedClaimTarget, PreparedRunInputs, PreparedStartGraph, SnapshotTargetContext, SyncBuffers, ReopenAcc, and mechanical doc stubs across acdd-state, replacing with idiomatic ControlFlow and borrowed graph slices while keeping StateEngine::{config, backend, query} intact until Wave 2b"
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
tags:
  - default
scope:
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
invariants:
  - 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, and 0 mechanical doc stubs.'
acceptance_criteria:
  - 'AC-1: All 8 single-caller context/parameter bags in acdd-state are deleted (with StateEngine::{config, backend, query} kept intact for unified Wave 2b removal alongside all 38 state_integration, 8 usecases/tests.rs, and tests/acdd callers), CompositeDomainProvider::new callers in acdd-state tests are migrated to with_module_flags, #[allow(clippy::too_many_arguments)] count in acdd-state drops by >= 80%, mechanical doc stubs in acdd-state are replaced, RunHarness error-code misuse is fixed, and .clone() density in usecases/ and machine/ is measurably reduced with full cargo test -p acdd-state --test state_integration parity.'
status: planned
subtasks:
  - subtask_ref: refactor-verify-claim-start-usecases
    title: "Replace PreparedVerifyPhase + GateCallCtx and claim/start/task_ops bags with ControlFlow and &LoadedRunGraph borrows"
  - subtask_ref: clean-machine-accumulators-and-doc-stubs
    title: "Remove SyncBuffers and ReopenAcc, migrate acdd-state-internal CompositeDomainProvider::new test calls, fix RunHarness error-code misuse, preserve StateEngine::{config, backend, query} until Wave 2b, and replace mechanical doc stubs in acdd-state"
```

### task: typed-graph-linker-and-raw-details-cutover

```yaml
task_ref: typed-graph-linker-and-raw-details-cutover
target: "Execute Wave 2a typed-graph and linker cutover: flip Node.kind (NodeKind with canonical HtmlRequest/HtmlContext + typed request_role metadata), Node.language (LanguageId), and CompactEdge (with fail-closed TryFrom<&PublicEdge> + CompactGraph::try_push_public_edge) across workspace callers and tests; upgrade FlowStore::read_named_uncertainties error and internal __rust_* ValueExpr/TypeExpr representations alongside INDEX_SEMANTICS_VERSION bump; delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims; narrow LanguageLinker/FileRelationContext to pub(crate) while keeping LanguageProfile pub; clear all remaining 235 production raw node.details[key] sites (+ 3 cache test sites) and promote ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to error; and clear semantic/types.rs doc stubs"
contract_revision: 1
depends_on:
  - ast-models-scanner-details-and-dto-shim-cleanup
  - language-linker-trait-inversion-and-tier2-members
tags:
  - default
scope:
  - crates/acdd-index/src/
  - src/state_bridge/code_domain/
  - src/engine/pipeline/
  - src/engine/reader.rs
  - docs/architecture/indexing.md
  - docs/reference/languages.md
  - tests/core/
  - tests/languages/
  - tests/linker/
  - tests/framework_manifests/
  - tests/receivers/
invariants:
  - 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, HtmlRequestAttributes maps onto NodeKind::HtmlRequest/HtmlContext plus typed request_role metadata while preserving zero-Rust .acdd/frameworks/*.toml overrides, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide.'
acceptance_criteria:
  - 'AC-1: Node.kind (36-variant NodeKind including Object), Node.language (LanguageId), and CompactEdge ((34-variant EdgeKind including TemplateImports, EdgeConfidence)) field cutovers compile cleanly across --workspace --all-targets using fail-closed TryFrom<&PublicEdge> for CompactEdge and CompactGraph::try_push_public_edge (updating docs/architecture/indexing.md:146, src/engine/reader.rs:478, tests/receivers/, tests/linker/public_output.rs:5-38 for closed tags + UTF-8 identifiers/evidence + negative TryFrom rejection, cache/tests.rs:123 synthetic scope kind, and linker/contracts.rs:671 LanguageId::Workspace); HtmlRequestAttributes emits NodeKind::HtmlRequest/HtmlContext with typed request_node/context_node role metadata preserving zero-Rust .acdd/frameworks/*.toml overrides (updating docs/reference/languages.md:116-118, tests/languages/html/{syntax,htmx_relations}.rs, and linker/html/relations.rs); FlowStore::read_named_uncertainties returns InvalidNamedUncertaintyCarrier (removing #[allow(clippy::result_unit_err)] in flow_store.rs and updating linker/mod.rs); remaining __rust_* ValueExpr/TypeExpr representations across semantic/types.rs, typed_facts/binary.rs, languages/rust/value_flow/, linker/rust/value_flow.rs, and db/lint/dead_tails.rs are typed and semantic/types.rs doc stubs are replaced; ClassicFieldsFirstDto/ClassicBindingsFirstDto/FlowState::{original,classic_fields_first} are deleted (updating linker/contracts.rs, db/writer/cache/tests.rs, tests/languages/html/classic_wire.rs) and INDEX_SEMANTICS_VERSION bumped; LanguageLinker and FileRelationContext are narrowed to pub(crate) via crate-private dispatch (removing all 3 #[allow(private_interfaces)] escapes and migrating tests/languages/profiles.rs:153-160 while keeping LanguageProfile pub); and all remaining 235 production raw node.details[key] sites across languages/ (78), linker/ (153), and db/ (4) plus 3 #[cfg(test)] sites in db/writer/cache/tests.rs are migrated with ARCH-L2-NO-RAW-NODE-DETAILS-INDEX promoted to severity: error and 0 warnings.'
status: planned
subtasks:
  - subtask_ref: atomic-node-compact-edge-html-and-linker-privacy-cutover
    title: "Flip Node.kind (including HtmlRequest/HtmlContext + typed request_role metadata), Node.language, and CompactEdge (with fail-closed TryFrom<&PublicEdge> + indexing.md/languages.md/public_output.rs/html test updates), upgrade FlowStore::read_named_uncertainties and __rust_* ValueExpr/TypeExpr variants + semantic/types.rs doc stubs, delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims + bump INDEX_SEMANTICS_VERSION, and narrow LanguageLinker/FileRelationContext to pub(crate)"
  - subtask_ref: eliminate-remaining-raw-details-and-ratchet-arch-rule
    title: "Migrate all remaining 235 production raw node.details[key] sites across languages/ (78), linker/ (153), and db/ (4) plus 3 #[cfg(test)] sites in db/writer/cache/tests.rs onto typed accessors and promote ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to severity: error"
```

### task: acdd-error-db-dtos-mcp-and-cli-cleanup

```yaml
task_ref: acdd-error-db-dtos-mcp-and-cli-cleanup
target: "Execute Wave 2b error, DTO, state-surface, MCP, config, CLI, and Cargo cleanup: unify StateError into AcddError + single-pass config/loader.rs without io::Error tunneling; delete StateEngine::{config, backend, query} across all test callers; delete ChunkPersistSink/EdgeOccurrencesInput/TextMatchPreviewParams and migrate db/{reader,symbols,traversal} and src/format/ onto typed DTOs with ImpactDirection/TraversalMode; remove dead ComplexityLimits threshold fields and ComplexityLimitsWire alongside tests/core/{arch_lint,ast_contracts}.rs and docs/reference/configuration.md; decompose src/mcp/server.rs aligning acdd.yaml/docs/architecture/cross-platform.md; relocate WorktreeSpanContext; fix docs_check EPIPE + src/cli/mod.rs:917-927 test contract; delete unused root Cargo.toml deps; and clear all remaining mechanical doc stubs"
contract_revision: 1
depends_on:
  - typed-graph-linker-and-raw-details-cutover
  - state-usecase-control-flow-and-context-bag-removal
tags:
  - default
scope:
  - Cargo.toml
  - acdd.yaml
  - crates/acdd-core/src/
  - crates/acdd-index/src/db/
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/
  - docs/architecture/
  - docs/reference/
  - tests/
invariants:
  - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: root Cargo.toml has 0 unused dependencies, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, ComplexityLimits drops dead threshold fields and rejects deprecated keys strictly via deny_unknown_fields, db readers and formatters use typed DTOs, WorktreeSpanContext lives in acdd_core::telemetry, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
acceptance_criteria:
  - 'AC-1: StateEngine::{config, backend, query} are deleted (migrating all 37 .backend() + 1 .config() + .query() calls in crates/acdd-state/tests/state_integration/{usecases_e2e,usecases_queries_lifecycle,roadmap_query_waves}.rs, 8 .backend() calls in crates/acdd-state/src/usecases/tests.rs, and .query() in tests/acdd/verification_and_git.rs); petgraph, zstd, and hashbrown are deleted from root Cargo.toml; AcddError replaces io::Error tunneling across config/loader.rs (single-pass Result<StrictAdapterReport, AcddError>) and all crate callers; ComplexityLimits deletes dead max_cognitive_complexity/max_nesting_depth/max_fanout struct fields and ComplexityLimitsWire (rejecting deprecated and unknown keys strictly via deny_unknown_fields) and updates docs/reference/configuration.md:113 + tests/core/{arch_lint,ast_contracts}.rs; CompositeDomainProvider::new, unused SchemaDef helpers, ParsedAdapterConfig, ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, resolve_effective_state_db, ast_legacy_error, Server::with_domain_providers, and ValidateResp JSON patch are deleted; WorktreeSpanContext moves to acdd_core::telemetry; TraversalOptions/GraphQueryOptions adopt ImpactDirection/TraversalMode and db/{reader,symbols,traversal} + src/format/ use typed DTOs; PipelineTestHooks::delta_failure* constructors are #[cfg(test)]-gated; src/mcp/server.rs is split cleanly with acdd.yaml (ARCH-SUBPROCESS-CONFINED, ARCH-TELEMETRY-CONFINED) and docs/architecture/cross-platform.md links aligned; src/cli/docs_check.rs EPIPE handling returns Ok(()) cleanly on BrokenPipe with src/cli/mod.rs:917-927 contract updated; and 0 template doc stubs remain in CLI --help or workspace source.'
status: planned
subtasks:
  - subtask_ref: acdd-error-state-getters-and-complexity-limits-cleanup
    title: "Generalize AcddError + single-pass config/loader.rs across all callers, delete StateEngine::{config, backend, query} across crates/acdd-state/tests/ and tests/acdd/, delete CompositeDomainProvider::new/SchemaDef/ParsedAdapterConfig/dead ComplexityLimits fields (with strict deny_unknown_fields + tests/core + configuration.md updates), relocate WorktreeSpanContext, and delete unused petgraph/zstd/hashbrown from root Cargo.toml"
  - subtask_ref: typed-db-reader-dtos-and-ingest-bag-removal
    title: "Delete ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, resolve_effective_state_db, and ast_legacy_error; adopt ImpactDirection/TraversalMode on TraversalOptions; and replace untyped Value maps in db/{reader,symbols,traversal} and src/format/ with typed DTOs"
  - subtask_ref: mcp-server-split-cli-epipe-and-remaining-doc-stubs
    title: "Decompose src/mcp/server.rs with acdd.yaml and docs/architecture/cross-platform.md aligned, #[cfg(test)]-gate PipelineTestHooks::delta_failure* constructors, fix cli/docs_check.rs EPIPE + src/cli/mod.rs:917-927 test contract, remove ValidateResp JSON patch, and replace all remaining mechanical doc stubs in acdd-core, db/, and src/"
```

### task: submit-verify-env-isolation-and-scope-fixes

```yaml
task_ref: submit-verify-env-isolation-and-scope-fixes
target: "Wave 2c: Integrate and verify all child-command env isolation, submit/verify post-mutation index_generation preservation, and scope-arg fixes from goal-20261124141330-293224-1 (4bb239694) on top of Wave 2b, close review follow-ups (#436: directory-style scope_args matching in shell_template.rs, remaining Command::new(CARGO_BIN_EXE_acdd) env isolation outside tests/common, and multi-check index_gen mutation state_integration test), and retire the orphan hotfix runs"
contract_revision: 1
depends_on:
  - acdd-error-db-dtos-mcp-and-cli-cleanup
tags:
  - default
scope:
  - .acdd/profile.yaml
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - tests/acdd/
  - tests/common/
  - tests/core/
  - tests/languages/
  - tests/mcp/
  - docs/reference/
invariants:
  - 'INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION: Gate child processes and test CLI harnesses scrub ambient ACDD_*/GIT_* variables deterministically, auto-verify during submit propagates refreshed index_generation observations, and directory-style scopes resolve target test filters without falling back or leaking ambient state.'
acceptance_criteria:
  - 'AC-1: All 12 files from fix-submit-verify-env-and-scope (4bb239694) are integrated onto AR-005 without regressing Wave 2b (acdd-state/tests + tests/common + tests/acdd + tests/mcp/harness_contract green), shell_template::select_scope_args matches directory-style scope prefixes alongside ** globs, remaining direct CARGO_BIN_EXE_acdd callers in tests/mcp/, tests/acdd/, tests/core/, and tests/languages/ isolate ACDD_*/GIT_* env cleanly, multi-check verify->submit preserves mutated index_generation in state_integration, and no stale orphan hotfix run worktree blocks AR-005 handoff.'
status: planned
subtasks:
  - subtask_ref: integrate-2c-env-isolation-and-verify-generation
    title: "Cherry-pick/reconcile goal-20261124141330-293224-1 (4bb239694) onto AR-005 cleanly across shell_template.rs, command.rs, submit/, verify.rs, and test harnesses"
  - subtask_ref: close-2c-directory-scopes-and-remaining-harness-callers
    title: "Support directory-style scopes in shell_template::select_scope_args (plus acdd-state test mapping in .acdd/profile.yaml), isolate remaining direct CARGO_BIN_EXE_acdd callers, and add multi-check index_generation preservation seam test"
```

### task: dead-tails-duplicates-and-call-graph-closure

```yaml
task_ref: dead-tails-duplicates-and-call-graph-closure
target: "Wave 2d: Fix Rust call-graph / extractor & dead_tails closure gaps (function-value combinator args, serde/schemars callable string attributes, sql::tokens self-shadowed calls, Context::with_flows / NamedBindings propagation) and eliminate remaining genuine dead_tails, phantom_tested helpers, and block/similar duplicates workspace-wide"
contract_revision: 1
depends_on:
  - submit-verify-env-isolation-and-scope-fixes
tags:
  - default
scope:
  - crates/acdd-core/src/
  - crates/acdd-index/src/
  - crates/acdd-state/src/
  - crates/acdd-state/tests/
  - src/
  - tests/
  - docs/
invariants:
  - 'INV-AR005-HYGIENE-CLOSURE: Rust call-graph extraction and dead_tails reachability track function-value callbacks, attribute-named Serde/Schemars hooks, and shadowed-local callable calls without false positives, while all remaining genuine dead_tails, phantom_tested helpers, and >=5L code duplicates are collapsed or removed cleanly.'
acceptance_criteria:
  - 'AC-1: Rust extractor/linker and db/lint/dead_tails properly attribute calls from function-value arguments (.map/.and_then/map_rows), #[serde(default=...)]/#[schemars(schema_with=...)] attributes, shadowed local calls such as languages/sql/mod.rs::tokens, and Context::with_flows/NamedBindings chains with dedicated tests in tests/languages.rs and tests/core.rs.'
  - 'AC-2: Remaining genuine dead_tails, phantom_tested items (such as dead/test-only helpers in acdd-core, acdd-index, acdd-state, and src/), and block/similar duplicates across acdd-index, acdd-state, and src/ (including admit_reclaim_in_progress, evaluate_verify, hydrate_and_link_delta, present_value_bounded, trim_tasks_page_tail, compute_merkle_root, coverage_dictionary) are deleted, #[cfg(test)]-gated, or collapsed into shared helpers with 0 lint errors and dramatically reduced duplicates/dead_tails warnings.'
status: planned
subtasks:
  - subtask_ref: fix-rust-call-graph-and-dead-tails-closure-gaps
    title: "Trace and fix Rust extractor/linker/dead_tails gaps for function-value combinator callbacks, serde/schemars string attributes, shadowed-local calls (sql::tokens), and Context::with_flows/NamedBindings chains with seam tests"
  - subtask_ref: sweep-genuine-dead-tails-phantom-tested-and-duplicates
    title: "Remove or #[cfg(test)]-gate remaining genuine dead_tails/phantom_tested items and collapse remaining pre-existing duplicates across acdd-core, acdd-index, acdd-state, and src/"
```

---

## Final Polish & Amendment Notes

- **Strict `ComplexityLimits` schema (`crates/acdd-core/src/lint.rs`)**: Removed `ComplexityLimitsWire` so `ComplexityLimits` derives `Deserialize` directly with `#[serde(default, deny_unknown_fields)]`, rejecting deprecated threshold keys (`max_cognitive_complexity`, `max_nesting_depth`, `max_fanout`) strictly alongside updates to `docs/reference/configuration.md` and `tests/core/ast_contracts.rs`.
- **Typed reader-eviction guard (`crates/acdd-index/src/db/cache.rs`, `src/engine/pipeline/delta.rs`)**: Replaced `Box<dyn std::any::Any>` in `DeltaAdmission::idle_readers_guard` with typed `cache::IdleReadersGuard`.
- **Workspace architecture lint rules (`docs/architecture/modularity.md`)**: Added `ARCH-NO-ANY-DOWNCAST` (`std::any`) and `ARCH-NO-PRIVATE-INTERFACES-ESCAPE*` (`#[allow(private_interfaces)]`, `#![allow(private_interfaces)]`, `#[expect(private_interfaces)]`) rules at `severity: error`.
- **Profile test routing & worker startup (`.acdd/profile.yaml`)**: Added `by_pattern` `rust-check-and-test` entries for `crates/acdd-index/src/languages/**` and `crates/acdd-index/src/linker/**` running both `acdd-index --lib` checks/tests and `--test languages --test linker {test_filter}` (leaving `scope_args_by_pattern` cleanly intact), and documented the 2-step worker startup (`acdd_claim` / `acdd claim` first for `ClaimResp.worktree`, then `acdd --root <ClaimResp.worktree> build` or MCP `worktree` before reading/querying code).
- **Pre-handoff status**: `status` remains `planned` until terminal `handoff` archives the milestone with `closure:`.
