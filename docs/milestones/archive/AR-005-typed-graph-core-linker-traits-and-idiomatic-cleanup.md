---
id: AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup
title: Typed Graph Core, Full LanguageLinker Trait Inversion, Context-Bag Removal & Idiomatic Rust Cleanup
doc_type: contract
status: completed
depends_on:
- AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
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
guidance:
  hitl: pro
required_skills:
- acdd
- contract-planning
- code-quality
- test-suite-rules
- agent-instructions
closure:
  status: completed
  closed_at: 2026-11-25T07:14:39.188Z
  target_base: main
  target_base_commit: bd281cc8d7c1cec06c9d5d82ecdfd20ab208748f
  task_receipts:
    acdd-error-db-dtos-mcp-and-cli-cleanup:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:acdd-error-db-dtos-mcp-and-cli-cleanup.deliver
      gate_id: deliver
      worker_id: ar05-w2b-reviewer-pro
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 2c19dfeacb39e5640473492df11d99505e11533e
      committed_oid: 50bf90ef9e5a458114e64192548088697579536b
      accepted_at: 2026-11-25T00:04:59.875Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: root Cargo.toml has 0 unused dependencies, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, ComplexityLimits preserves backward-compatible YAML deserialization with 0 dead struct fields, db readers and formatters use typed DTOs, WorktreeSpanContext lives in acdd_core::telemetry, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
          verified:
          - INV-AR005-TYPED-READER-AND-ERROR-SURFACES
        review_summary: 'Delivered Wave 2b (task:acdd-error-db-dtos-mcp-and-cli-cleanup) after Round 2 review approval (#476 remediated, #459/#472/#475 verified, all 3 subtasks completed, candidate_tree=2c19dfeacb39e5640473492df11d99505e11533e).'
    ast-models-scanner-details-and-dto-shim-cleanup:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:ast-models-scanner-details-and-dto-shim-cleanup.deliver
      gate_id: deliver
      worker_id: ar05-w1-ast-reviewer-r3-pro
      claim_revision: 1
      contract_revision: 2
      candidate_tree: fd954addfeb047f33bde9724a92c8dd6c1f9a449
      committed_oid: e0acee2937b7c1efb4f9dd90cc81b85e3792d7e9
      accepted_at: 2026-11-24T09:15:11.447Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-AST-MODELS-CLEAN-DETAILS: ast/, models/, typed_facts/, scanner/, and interner.rs contain 0 raw node.details[key] accesses, 0 WalkContext bags, and 0 mechanical doc stubs.'
          verified:
          - INV-AR005-AST-MODELS-CLEAN-DETAILS
        review_summary: Deliver Wave 1 Task 1 (ast-models-scanner-details-and-dto-shim-cleanup) with both subtasks completed and Round 3 independent review passed.
    dead-tails-duplicates-and-call-graph-closure:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:dead-tails-duplicates-and-call-graph-closure.deliver
      gate_id: deliver
      worker_id: ar05-coord
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 92e5079336880a8af6eddf5d958cc7ba8657445c
      committed_oid: 03576033f38fbed60ee4b79de1de1415bc06ef0c
      accepted_at: 2026-11-25T06:38:54.725Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-HYGIENE-CLOSURE: Rust call-graph extraction and dead_tails reachability track function-value callbacks, attribute-named Serde/Schemars hooks, and shadowed-local callable calls without false positives, while all remaining genuine dead_tails, phantom_tested helpers, and >=5L code duplicates are collapsed or removed cleanly.'
          verified: []
    language-linker-trait-inversion-and-tier2-members:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:language-linker-trait-inversion-and-tier2-members.deliver
      gate_id: deliver
      worker_id: ar05-w1-linker-clean-reviewer-pro
      claim_revision: 2
      contract_revision: 1
      candidate_tree: 9d4c827981c0759af4b28520fd510a50febd0cc4
      committed_oid: fe853ed8c312a29e835f78fdbbdd563defbd6eec
      accepted_at: 2026-11-24T16:19:43.270Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string checks or language-specific branches in universal linker modules, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
          verified:
          - INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY
        review_summary: Deliver reopened language-linker-trait-inversion-and-tier2-members candidate_tree 9d4c827981c0759af4b28520fd510a50febd0cc4 after independent review pass.
    state-usecase-control-flow-and-context-bag-removal:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:state-usecase-control-flow-and-context-bag-removal.deliver
      gate_id: deliver
      worker_id: ar05-w1-state-reviewer-r2-pro
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 3eda8b6b61ecc1ca1787becb20f0cf3a453085d4
      committed_oid: 19bd10a835a4fe91be09b5c92a538676e3750308
      accepted_at: 2026-11-24T11:07:58.366Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, and 0 mechanical doc stubs.'
          verified:
          - INV-AR005-STATE-ZERO-BAG-SHIMS
          - INV-STATE-NO-INDEX
          - INV-STATE-PURE-MACHINE
          - INV-STATE-SQLITE-CONFINED
          - INV-SINGLE-OWNER
          - INV-NO-DEAD-CODE-SHIMS
        review_summary: Deliver state-usecase-control-flow-and-context-bag-removal after Round 2 review pass and subtask reconciliation
    submit-verify-env-isolation-and-scope-fixes:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:submit-verify-env-isolation-and-scope-fixes.deliver
      gate_id: deliver
      worker_id: ar05-w2c-reviewer-grok
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 1df3b49acbef29847cdb2194712039922af7d6a3
      committed_oid: 252a7189abee800396fec0c342ad905b96246c20
      accepted_at: 2026-11-25T02:45:16.144Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION: Gate child processes and test CLI harnesses scrub ambient ACDD_*/GIT_* variables deterministically, auto-verify during submit propagates refreshed index_generation observations, and directory-style scopes resolve target test filters without falling back or leaking ambient state.'
          verified:
          - INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION
        review_summary: 'Deliver Wave 2c after re-review closed finding #484. Env isolation, Phase 3 claim fencing, directory scope matching, and the multi-check index_generation seam are on candidate 1df3b49.'
    typed-graph-linker-and-raw-details-cutover:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:typed-graph-linker-and-raw-details-cutover.deliver
      gate_id: deliver
      worker_id: ar05-w2a-reviewer-r2-pro
      claim_revision: 1
      contract_revision: 3
      candidate_tree: 73c7d4209c26c0e203d554b1da92d3fe10704c4e
      committed_oid: 608a1db16e3b0a5aee1cfb6e38195412b2269269
      accepted_at: 2026-11-24T22:25:31.594Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, HtmlRequestAttributes maps onto NodeKind::HtmlRequest/HtmlContext plus typed request_role metadata while preserving zero-Rust .acdd/frameworks/*.toml overrides, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide.'
          verified:
          - INV-AR005-TYPED-GRAPH-AND-DETAILS
        review_summary: Deliver Wave 2a typed-graph and linker cutover after Round 2 review pass
    upfront-contract-synthesis-and-dag-prioritization:
      run_id: AR-005-typed-graph-c-20261124040532-2665544-1
      node_cursor: root:upfront-contract-synthesis-and-dag-prioritization.deliver
      gate_id: deliver
      worker_id: ar05-w0-reviewer-pro
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 5fbe9508f604aa7f525b9fb4f0d7154a0a52b1ea
      committed_oid: 48ff9cfb8090daa6daa901f1a33e7cc7b3b99721
      accepted_at: 2026-11-24T07:04:08.642Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2a/2b execute cross-crate cutovers.'
          verified:
          - INV-AR005-DISJOINT-WAVES
        review_summary: Delivered Wave 0 contract freeze (errors.rs, models/tags.rs, ADRs 0001/0003/0010/0011, and disjoint Wave 1 / sequential Wave 2a/2b spec contract).
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2a/2b execute cross-crate cutovers.'
      - 'INV-AR005-AST-MODELS-CLEAN-DETAILS: ast/, models/, typed_facts/, scanner/, and interner.rs contain 0 raw node.details[key] accesses, 0 WalkContext bags, and 0 mechanical doc stubs.'
      - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string checks or language-specific branches in universal linker modules, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
      - 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, and 0 mechanical doc stubs.'
      - 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, HtmlRequestAttributes maps onto NodeKind::HtmlRequest/HtmlContext plus typed request_role metadata while preserving zero-Rust .acdd/frameworks/*.toml overrides, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide.'
      - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: root Cargo.toml has 0 unused dependencies, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, ComplexityLimits preserves backward-compatible YAML deserialization with 0 dead struct fields, db readers and formatters use typed DTOs, WorktreeSpanContext lives in acdd_core::telemetry, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
      - 'INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION: Gate child processes and test CLI harnesses scrub ambient ACDD_*/GIT_* variables deterministically, auto-verify during submit propagates refreshed index_generation observations, and directory-style scopes resolve target test filters without falling back or leaking ambient state.'
      - 'INV-AR005-HYGIENE-CLOSURE: Rust call-graph extraction and dead_tails reachability track function-value callbacks, attribute-named Serde/Schemars hooks, and shadowed-local callable calls without false positives, while all remaining genuine dead_tails, phantom_tested helpers, and >=5L code duplicates are collapsed or removed cleanly.'
      - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, root Cargo.toml has 0 unused dependencies, ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, db readers and formatters use typed DTOs, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
      verified:
      - INV-AR005-DISJOINT-WAVES
      - INV-AR005-AST-MODELS-CLEAN-DETAILS
      - INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY
      - INV-AR005-STATE-ZERO-BAG-SHIMS
      - INV-STATE-NO-INDEX
      - INV-STATE-PURE-MACHINE
      - INV-STATE-SQLITE-CONFINED
      - INV-SINGLE-OWNER
      - INV-NO-DEAD-CODE-SHIMS
      - INV-AR005-TYPED-GRAPH-AND-DETAILS
      - INV-AR005-TYPED-READER-AND-ERROR-SURFACES
      - INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION
    decisions:
    - |-
      AR-005 Architectural Blueprint Synthesis Decision:

      1. Wave 0 Contract Freeze (upfront-contract-synthesis-and-dag-prioritization):
         - Unify error core around `AcddError` in `crates/acdd-core/src/errors.rs` while retaining `pub type StateError = AcddError;` alias through Wave 1 so `acdd-state`/`acdd-index`/`src/` compile independently; preserve `ErrorCode` (`as_str`, `exit_code`, wire JSON) and `AcddError::from_anyhow` without `io::Error` tunneling in Wave 2.
         - Complete 1-byte `Copy` enum variants in `crates/acdd-index/src/models/tags.rs` (`NodeKind` including `ModuleDeclaration`, `LanguageId`, `EdgeKind`, `EdgeConfidence`, `CoverageStatus`, `ReferenceKind`) and add non-breaking equality/wire helpers without altering `Node`/`CompactEdge` field types in Wave 0.
         - Update `docs/adr/{0001,0003,0010,0011}` to authorize `AcddError`, `Copy`-enum graph tags, `pub(crate) LanguageLinker`/`FileRelationContext` (keeping `LanguageProfile` `pub`), Tier-2 OO shared member indexing, and bag-free `ControlFlow` idioms. Pre-register seam modules in `tests/{core,linker,mcp}.rs`.

      2. Wave 1 Strictly Disjoint Parallel Slices (0 shared write paths):
         - Task 1 (`ast-models-scanner-details-and-dto-shim-cleanup`): Add additive `NodeKind`/`LanguageId`/`EdgeKind`/`EdgeConfidence` accessors/constructors on `Node` and `CompactEdge` (`models/{types,compact_graph}.rs`) and typed `NodeDetailKey` / `NodeDetailFlags` / `NodeSemanticEntry` accessors (`models/{details,semantic}.rs`), keeping `Node.{kind,language}: String`, `CompactEdge` `CompactTag`, and `ClassicFieldsFirstDto`/`ClassicBindingsFirstDto` intact until Wave 2; clear all 55 raw `node.details[...]` sites across `ast/`, `models/`, `interner.rs`; delete `WalkContext` (`scanner/walk.rs`); replace `#[allow(unused_imports)]` (`typed_facts/binary.rs`) and `#[allow(clippy::result_unit_err)]` (`typed_facts/flow_store.rs` via `InvalidNamedUncertaintyCarrier`); clear mechanical doc stubs in scope.
         - Task 2 (`language-linker-trait-inversion-and-tier2-members`): Extend `LanguageLinker` (remaining `pub` in Wave 1 so `tests/languages/profiles.rs:153-160` compiles) with lifetime-preserving `LanguageMemberIndex<'a>` / `LanguageImportState<'a>` hooks (`build_semantic_state`, `lookup_member`, `lookup_property`, `deref_target`, `polymorphic_implementors`, `filter_candidates`, `admit_implicit_receiver`, `resolve_computed_call`), eliminating `rust`, `rust_forwarding`, `rust_library_modules`, `python`, `typescript`, `commonjs` fields from `semantic_context::Context`, `ImportContext`, and `ImportNormalizationContext` without `'static`/`Any` hacks; replace all `__rust_*` magic strings and direct `linker::{rust,python,typescript,javascript,html}` branches in `evaluate.rs`, `dispatch.rs`, `candidates.rs`, `imports.rs`; unify the 3 Rust path/type resolvers (`profile_linker.rs`, `forwarding.rs`, `resolution.rs`) onto one canonical `RustForwarding` + `ValueFlow` seam; delete `fallback_receiver_value`, `BuiltinResolutionFlags`, `MemberAccumulator` (`MacroTraitTables`, `TraitDeclTables`), and `#[allow(clippy::result_unit_err)]` (`linker/routes.rs`); wire Tier-2 OO profiles (`Go`, `Java`, `Kotlin`, `C#`, `C++`, `PHP`, `Ruby`) into shared `populate_class_members` + inheritance indexing (`inherits`/`bases`/`implements` + `receiver_type`/`is_method`/`is_static`/`is_stub`); clear doc stubs in `languages/` & `linker/`, update `docs/adr/0011-...`.
         - Task 3 (`state-usecase-control-flow-and-context-bag-removal`): Delete all 8 single-caller bags (`PreparedVerifyPhase`, `GateCallCtx`, `PreparedClaimTarget`, `PreparedRunInputs`, `PreparedStartGraph`, `SnapshotTargetContext`, `SyncBuffers`, `ReopenAcc`) and `StateEngine::{config, backend}` (`usecases/mod.rs`); replace `Option<Result<...>>` with `ControlFlow` and `&LoadedRunGraph` borrows; cut `#[allow(clippy::too_many_arguments)]` by >= 80% and reduce `.clone()` density; migrate `CompositeDomainProvider::new` test callers in `crates/acdd-state/` to `with_module_flags` (deferring `StateEngine::query` removal to Wave 2); fix `RunHarness` error-code misuse (`harness.rs`); replace doc stubs in `acdd-state`.

      3. Wave 2 Atomic Cutover & Host Cleanup (`typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup`):
         - Cut over `Node.kind: NodeKind`, `Node.language: LanguageId`, `CompactEdge.{kind: EdgeKind, confidence: EdgeConfidence}`, and `TraversalOptions`/`GraphQueryOptions` (`ImpactDirection`, `TraversalMode`) across all crates/tests; delete `ClassicFieldsFirstDto`/`ClassicBindingsFirstDto`/`FlowState::{original,classic_fields_first}` and bump `INDEX_SEMANTICS_VERSION`.
         - Narrow `LanguageLinker`/`FileRelationContext` to `pub(crate)` via crate-private dispatch seam (`linker/traits.rs`, `languages/profile.rs`), keeping `LanguageProfile` `pub`, deleting all 3 `#[allow(private_interfaces)]` and migrating `tests/languages/profiles.rs:153-160` to `LanguageProfile::allows_multi_file_modules()`.
         - Eliminate remaining 233 raw `node.details[...]` accesses across `languages/`, `linker/`, `db/` and promote `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` to `severity: error`.
         - Delete `StateEngine::query` (migrating `tests/acdd/` + `crates/acdd-state/tests/`), `CompositeDomainProvider::new`, `SchemaDef::{integer,number,boolean}`, dead `ComplexityLimits` fields, `ParsedAdapterConfig`, `ChunkPersistSink`, `EdgeOccurrencesInput`, `TextMatchPreviewParams`, `resolve_effective_state_db`, `ast_legacy_error`, `Server::with_domain_providers`, and unused `petgraph`/`zstd`/`hashbrown` in root `Cargo.toml`; `#[cfg(test)]`-gate `PipelineTestHooks::delta_failure*`.
         - Generalize `AcddError` + single-pass `config/loader.rs` (`Result<StrictAdapterReport, AcddError>`) without `io::Error` tunneling; relocate `WorktreeSpanContext` to `acdd_core::telemetry` and forbid `host_telemetry -> host_state_bridge`; replace `Map<String, Value>` mutation in `db/{reader,symbols,traversal}` and `src/format/` with typed DTOs; remove `ValidateResp` patch (`src/cli/validate.rs:29-33`); decompose `src/mcp/server.rs` into cache/admission, worktree-resolution, and server-session modules aligning `acdd.yaml` + `docs/architecture/cross-platform.md`; fix `src/cli/docs_check.rs` EPIPE panic; sweep remaining mechanical doc stubs.
    - |-
      AR-005 Revised Architectural Blueprint Decision (supersedes #397, resolves all 5 blocking findings + non-blocking notes in #398):

      0. Explicit Resolution of Findings #398:
         - Finding 1 (Wave 1 Task 1 vs Task 2 `FlowStore::read_named_uncertainties` signature compatibility): Defer `FlowStore::read_named_uncertainties` return-error cutover (`()` -> `InvalidNamedUncertaintyCarrier`) and `#[allow(clippy::result_unit_err)]` removal (`crates/acdd-index/src/typed_facts/flow_store.rs:111-118`) plus `crates/acdd-index/src/linker/mod.rs:425-442` (`Err(()) =>`) update to Wave 2 (`typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup`). Wave 1 Task 1 removes `#[allow(unused_imports)]` in `typed_facts/binary.rs` and preserves `Result<_, ()>` compatibility.
         - Finding 2 (`crates/acdd-index/src/semantic/` & `ValueExpr`/`TypeExpr` boundary between Task 1 and Task 2): Keep `crates/acdd-index/src/semantic/` untouched in Wave 1 so `typed_facts/binary.rs` (`enum_codec!` over `TypeExpr`, `StaticArgument`, `ValueExpr`) does not collide with Task 1. In Wave 1 Task 2, remove `__rust_*` string checks and direct language branches from universal linker modules (`evaluate.rs`, `dispatch.rs`, `inferred.rs`, `candidates.rs`, `imports.rs`, `type_resolution.rs`) behind lifetime-preserving `LanguageLinker` / `LanguageMemberIndex<'a>` / `LanguageImportState<'a>` hooks (`linker/rust/value_flow.rs`), leaving `semantic/types.rs`, `typed_facts/binary.rs`, `languages/rust/value_flow/{expr,collector}.rs`, and `db/lint/dead_tails.rs` untouched until Wave 2. In Wave 2 Task 4, atomically replace the internal `__rust_*` `ValueExpr`/`TypeExpr` representations across `semantic/types.rs`, `typed_facts/binary.rs`, `languages/rust/value_flow/`, `linker/rust/value_flow.rs`, and `db/lint/dead_tails.rs` alongside `INDEX_SEMANTICS_VERSION` bump.
         - Finding 3 (Live `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` count & `typed_facts/` raw sites): Reconcile milestone spec and blueprint to the verified live workspace count of `290` production warnings (+ `3` `#[cfg(test)]` sites in `db/writer/cache/tests.rs`). Wave 1 Task 1 eliminates all `55` production sites across `ast/` (`39`), `models/` (`10`), `typed_facts/` (`4` in `flow_store.rs` and `catalog.rs`), and `interner.rs` (`2`). Wave 2 Task 4 eliminates the remaining `235` production sites across `languages/` (`78`), `linker/` (`153`), and `db/` (`4`) plus the `3` test sites in `db/writer/cache/tests.rs` and ratchets `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` to `severity: error`.
         - Finding 4 (Wave 0 scope cleanup): Remove `tests/core.rs`, `tests/linker.rs`, and `tests/mcp.rs` pre-registration from Wave 0 (`upfront-contract-synthesis-and-dag-prioritization`) so Wave 0 stays strictly inside `docs/milestones/...`, `docs/adr/`, `crates/acdd-core/src/errors.rs`, and `crates/acdd-index/src/models/tags.rs` (`cargo clippy/test -p acdd-core -p acdd-index` + `commitment_integrity`) with zero premature `#[path]` stubs.
         - Finding 5 + non-blocking notes: Wave 2 (`typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup`) uses fail-closed `TryFrom<&PublicEdge> for CompactEdge` and `CompactGraph::try_push_public_edge` (replacing infallible `From<&PublicEdge>`) for `CompactEdge.{kind: EdgeKind, confidence: EdgeConfidence}` while preserving `PublicEdge` `String` fields and `CompactEdge.{src,dst,path,evidence}` `CompactTag`/`Arc<str>` UTF-8 storage; updates `docs/architecture/indexing.md` (`146`) and `tests/linker/public_output.rs` (`5-38`) to verify closed `EdgeKind`/`EdgeConfidence` tags, UTF-8 identifiers/evidence, and negative `TryFrom` rejection; sweeps `crates/acdd-index/src/semantic/types.rs` mechanical doc stubs in Wave 2 Task 4; and requires explicit coordinator/architect `BUDGET_OVERRIDE` escalation rather than builder self-approval if clean extraction exceeds `max_net_loc` (`450` in Task 2, `500` in Task 4).

      1. Wave 0 Contract Freeze (`upfront-contract-synthesis-and-dag-prioritization`):
         - Freeze `AcddError` unification design in `crates/acdd-core/src/errors.rs` (`pub type StateError = AcddError;` bridge during Wave 1; `ErrorCode`, `exit_code`, and wire JSON preserved; `io::Error` tunneling replaced in Wave 2).
         - Complete 1-byte `Copy` enum variants in `crates/acdd-index/src/models/tags.rs` (`NodeKind` including `ModuleDeclaration`, `LanguageId`, `EdgeKind`, `EdgeConfidence`, `CoverageStatus`, `ReferenceKind`) and add non-breaking equality/parse helpers without changing `Node`/`CompactEdge` field types yet.
         - Update `docs/adr/{0001,0003,0010,0011}` to authorize `AcddError`, closed `Copy`-enum `CompactEdge`/`Node` tags with fail-closed `TryFrom<&PublicEdge>`, `pub(crate) LanguageLinker`/`FileRelationContext` (keeping `LanguageProfile` `pub`), Tier-2 OO shared member indexing, and bag-free `ControlFlow` idioms. Lock strictly disjoint Wave 1 scopes (0 overlapping files).

      2. Wave 1 Strictly Disjoint Parallel Slices:
         - Task 1 (`ast-models-scanner-details-and-dto-shim-cleanup`): Stage additive `NodeKind`/`LanguageId`/`EdgeKind`/`EdgeConfidence` accessors/constructors on `Node` and `CompactEdge` (`models/{types,compact_graph}.rs`) and typed `NodeDetailKey` / `NodeDetailFlags` / `NodeSemanticEntry` accessors (`models/{details,semantic}.rs`), preserving `Node.{kind,language}: String`, `CompactEdge` `CompactTag`, `ClassicFieldsFirstDto`/`ClassicBindingsFirstDto`, and `FlowStore::read_named_uncertainties` `Result<_, ()>` signature until Wave 2; clear all `55` raw `node.details[...]` sites across `ast/` (`39`), `models/` (`10`), `typed_facts/` (`4`), and `interner.rs` (`2`); delete `WalkContext` (`scanner/walk.rs`); delete `#[allow(unused_imports)]` (`typed_facts/binary.rs`); clear mechanical doc stubs in scope.
         - Task 2 (`language-linker-trait-inversion-and-tier2-members`): Extend `LanguageLinker` (kept `pub` in Wave 1 so `tests/languages/profiles.rs` compiles) with lifetime-preserving `LanguageMemberIndex<'a>` / `LanguageImportState<'a>` hooks (`build_semantic_state`, `lookup_member`, `lookup_property`, `deref_target`, `polymorphic_implementors`, `filter_candidates`, `admit_implicit_receiver`, `resolve_computed_call`), removing `rust`, `rust_forwarding`, `rust_library_modules`, `python`, `typescript`, `commonjs` fields from `semantic_context::Context`, `ImportContext`, and `ImportNormalizationContext`; eliminate `__rust_*` string checks and direct `linker::{rust,python,typescript,javascript,html}` branches from universal linker modules (`evaluate.rs`, `dispatch.rs`, `inferred.rs`, `candidates.rs`, `imports.rs`, `type_resolution.rs`) via `LanguageLinker` hooks while keeping `semantic/types.rs` untouched; consolidate duplicate Rust path/type resolvers (`profile_linker.rs`, `forwarding.rs`, `resolution.rs`) into one canonical `RustForwarding` + `ValueFlow` seam; delete `fallback_receiver_value`, `BuiltinResolutionFlags`, `MemberAccumulator` (`MacroTraitTables`, `TraitDeclTables`), and `#[allow(clippy::result_unit_err)]` (`linker/routes.rs`); wire Tier-2 OO profiles (`Go`, `Java`, `Kotlin`, `C#`, `C++`, `PHP`, `Ruby`) into shared `populate_class_members` + inheritance indexing (`inherits`/`bases`/`implements` + `receiver_type`/`is_method`/`is_static`/`is_stub`); clear doc stubs in `languages/` & `linker/`, update `docs/adr/0011-...`.
         - Task 3 (`state-usecase-control-flow-and-context-bag-removal`): Delete all 8 single-caller bags (`PreparedVerifyPhase`, `GateCallCtx`, `PreparedClaimTarget`, `PreparedRunInputs`, `PreparedStartGraph`, `SnapshotTargetContext`, `SyncBuffers`, `ReopenAcc`) and `StateEngine::{config, backend}` (`usecases/mod.rs`); replace `Option<Result<...>>` with `ControlFlow` and `&LoadedRunGraph` borrows; cut `#[allow(clippy::too_many_arguments)]` by >= 80% and reduce `.clone()` density; migrate `CompositeDomainProvider::new` test callers in `crates/acdd-state/` to `with_module_flags` (deferring `StateEngine::query` removal to Wave 2); fix `RunHarness` error-code misuse (`harness.rs`); replace doc stubs in `acdd-state`.

      3. Wave 2 Atomic Cutover & Host Cleanup (`typed-graph-cutover-db-dtos-acdd-error-and-host-cleanup`):
         - Cut over `Node.kind: NodeKind`, `Node.language: LanguageId`, `CompactEdge.{kind: EdgeKind, confidence: EdgeConfidence}` with fail-closed `TryFrom<&PublicEdge> for CompactEdge` + `CompactGraph::try_push_public_edge` (updating `docs/architecture/indexing.md:146` and `tests/linker/public_output.rs:5-38`), and `TraversalOptions`/`GraphQueryOptions` (`ImpactDirection`, `TraversalMode`) across all crates/tests; upgrade `FlowStore::read_named_uncertainties` to `InvalidNamedUncertaintyCarrier` (removing `#[allow(clippy::result_unit_err)]` in `flow_store.rs` and updating `linker/mod.rs`); replace internal `__rust_*` `ValueExpr`/`TypeExpr` wire strings across `semantic/types.rs`, `typed_facts/binary.rs`, `languages/rust/value_flow/`, `linker/rust/value_flow.rs`, and `db/lint/dead_tails.rs`; delete `ClassicFieldsFirstDto`/`ClassicBindingsFirstDto`/`FlowState::{original,classic_fields_first}` and bump `INDEX_SEMANTICS_VERSION`.
         - Narrow `LanguageLinker`/`FileRelationContext` to `pub(crate)` via crate-private dispatch seam (`linker/traits.rs`, `languages/profile.rs`), keeping `LanguageProfile` `pub`, removing all 3 `#[allow(private_interfaces)]` and migrating `tests/languages/profiles.rs:153-160`.
         - Eliminate all remaining `235` production raw `node.details[...]` sites across `languages/` (`78`), `linker/` (`153`), `db/` (`4`) plus `3` cache test sites in `db/writer/cache/tests.rs` and promote `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` to `severity: error`.
         - Delete `StateEngine::query`, `CompositeDomainProvider::new`, `SchemaDef::{integer,number,boolean}`, dead `ComplexityLimits` fields, `ParsedAdapterConfig`, `ChunkPersistSink`, `EdgeOccurrencesInput`, `TextMatchPreviewParams`, `resolve_effective_state_db`, `ast_legacy_error`, `Server::with_domain_providers`, and unused `petgraph`/`zstd`/`hashbrown` in root `Cargo.toml`; `#[cfg(test)]`-gate `PipelineTestHooks::delta_failure*`.
         - Generalize `AcddError` + single-pass `config/loader.rs` (`Result<StrictAdapterReport, AcddError>`) without `io::Error` tunneling; relocate `WorktreeSpanContext` to `acdd_core::telemetry` and forbid `host_telemetry -> host_state_bridge`; replace `Map<String, Value>` mutation in `db/{reader,symbols,traversal}` and `src/format/` with typed DTOs; remove `ValidateResp` patch (`src/cli/validate.rs:29-33`); decompose `src/mcp/server.rs` aligning `acdd.yaml` + `docs/architecture/cross-platform.md`; fix `src/cli/docs_check.rs` EPIPE panic; sweep all remaining mechanical doc stubs including `semantic/types.rs`.
    - |-
      AR-005 Revised Architectural Blueprint Decision (supersedes #399 and #397; resolves all 4 blocking findings in #400):

      0. Explicit Resolution of Findings #400 (Round 2):
         - Finding 1 (Exhaustive Closed `NodeKind`/`EdgeKind` Inventory & `ADR 0011` `HtmlRequestAttributes` Reconciliation):
           * Audited all production `Node.kind`, `Edge.kind`, `SemanticRelation`, `ReferenceKind`, `DeclarationRole`, and `db/{reader,symbols,lint}` edge/node emissions & queries across `crates/acdd-index/src/` and `tests/`:
             - `NodeKind` (`crates/acdd-index/src/models/tags.rs:62-91`, currently 28 variants) expands to **35** bounded 1-byte `Copy` variants by adding **7** live canonical kinds in Wave 0: `ModuleDeclaration => "module_declaration"` (`languages/rust/mod.rs:36`), `View => "view"` (`languages/sql/mod.rs:791-792`), `Service => "service"` (`languages/proto.rs:20,75`), `ToolRegistration => "tool_registration"` and `EventHandlerRegistration => "event_handler_registration"` (`languages/python/decorated_registration.rs:322-324,349`), plus canonical `HtmlRequest => "html_request"` and `HtmlContext => "html_context"` (`languages/html/request_attributes.rs:142,169`).
             - `EdgeKind` (`crates/acdd-index/src/models/tags.rs:28-46`, currently 17 variants) expands to **33** closed variants by adding **16** live canonical edge kinds in Wave 0: `DefinesTable => "defines_table"`, `QueriesTable => "queries_table"`, `MutatesTable => "mutates_table"`, `ReferencesTable => "references_table"` (`linker/framework_relations.rs:104-112,148`), `UsesModel => "uses_model"` (`linker/projections.rs:113`, `semantic/relations.rs:59`), `CallsEndpoint => "calls_endpoint"` (`linker/projections.rs:169`, `linker/semantic_facts.rs:226`), `ReferencesRoute => "references_route"` (`linker/semantic_facts.rs:233`), `RendersTemplate => "renders_template"`, `InjectsDependency => "injects_dependency"` (`semantic/relations.rs:57-58`, `linker/semantic_facts.rs:85`), `Injects => "injects"` (`linker/resolution/value_flow_phase/mod.rs:31` + `candidates.rs:504`, `tests/languages/html/manifest_relations/configured/di_origins.rs:128`), `Renders => "renders"`, `ContextProvider => "context_provider"` (`linker/html/relations.rs:64,245`), `Includes => "includes"` (`linker/resolution/framework_projections.rs:102`, `ReferenceKind::Includes`), `CitesDoc => "cites_doc"`, `CitedByDoc => "cited_by_doc"`, `GovernedBy => "governed_by"` (`linker/resolution/documentation.rs:41-42,95`, `db/reader/docs.rs:493-494`). (Verified all 4 `EdgeConfidence` values `"exact"`, `"inferred"`, `"possible"`, `"heuristic"` already exist in `tags.rs:48-53`; `"file"`/`"record"` only appear in legacy DB SQL filters; synthetic test fixture `"scope"` in `db/writer/cache/tests.rs:123` updates to `NodeKind::TemplateScope` in Wave 2a.)
             - `ADR 0011` Reconciliation (`HtmlRequestAttributes` in `docs/adr/0011-language-linkers-and-framework-manifests.md` + `languages/manifests/syntax.rs:198-210` + `htmx.toml:4`): `.acdd/frameworks/*.{toml,yaml,yml}` continues declaring arbitrary `is_ident_like` `request_node`, `context_node`, and `namespace` strings with zero Rust code changes (`ADR 0011` invariant preserved!). Instead of allocating open `Node.kind: String` variants, `extract_request_attributes` emits canonical 1-byte `NodeKind::HtmlRequest` / `NodeKind::HtmlContext` and records the manifest-configured role tag (`request_node`/`context_node`, e.g. `"htmx_request"`, `"proof_request"`, `"htmx_context"`) + `namespace` in a typed `NodeDetailKey` slot (`REQUEST_NODE_ROLE`), verified by `linker/html/relations.rs::is_request_with_rules`. `ADR 0011` is amended in Wave 0, and `docs/reference/languages.md:116-118` + `tests/languages/html/{syntax,htmx_relations}.rs` update in Wave 2a alongside `Node.kind: NodeKind`.
         - Finding 2 (`StateEngine::{config, backend}` vs `crates/acdd-state/tests/state_integration/`):
           * Deferred `StateEngine::{config, backend}` removal cleanly from Wave 1 Task 3 to **Wave 2b** (`acdd-error-db-dtos-mcp-and-cli-cleanup`) alongside `StateEngine::query`. Wave 1 Task 3 focuses strictly on deleting the 8 single-caller parameter/accumulator bags (`PreparedVerifyPhase`, `GateCallCtx`, `PreparedClaimTarget`, `PreparedRunInputs`, `PreparedStartGraph`, `SnapshotTargetContext`, `SyncBuffers`, `ReopenAcc`) via `ControlFlow` + `&LoadedRunGraph`, migrating `acdd-state`-internal `CompositeDomainProvider::new` callers, fixing `RunHarness`, and clearing `acdd-state` doc stubs so `cargo test -p acdd-state --test state_integration` passes cleanly in Wave 1 without breaking the 37 `.backend()` + 1 `.config()` `state_integration` sites or 8 `usecases/tests.rs` sites. Wave 2b removes `StateEngine::{config, backend, query}` together across all `state_integration/*.rs`, `usecases/tests.rs`, and `tests/acdd/` callers.
         - Finding 3 (`ComplexityLimits` Backward Compatibility & `src/cli/mod.rs:917-927` EPIPE Test Scope):
           * In Wave 2b, delete the unused stored threshold fields `max_cognitive_complexity`, `max_nesting_depth`, and `max_fanout` from `ComplexityLimits` (`crates/acdd-core/src/lint.rs:299-342`), while implementing custom `Deserialize` handling that parses and ignores optional legacy `max_cognitive_complexity`/`max_nesting_depth`/`max_fanout` keys when `>= 1` (preserving non-breaking `acdd.yaml` backward compatibility while still rejecting `0` or genuinely unknown keys like `unknown_limit: 1`). Explicitly update `docs/reference/configuration.md:113`, `tests/core/arch_lint.rs:907-913`, and `tests/core/ast_contracts.rs:11-57` in Wave 2b.
           * In Wave 2b, fix `src/cli/docs_check.rs:18,27,32` to return `Ok(())` cleanly on `io::ErrorKind::BrokenPipe` (propagating non-`BrokenPipe` `Err`) and explicitly update the test contract in `src/cli/mod.rs:917-927` (`cli_writer_handles_broken_pipe_and_docs_check_write_panics`) to assert `Ok(())` on `BrokenPipe` and `Err` on non-`BrokenPipe` write failure instead of expecting panic.
         - Finding 4 (Real ADR Path & Sequential Wave 2a / Wave 2b Split):
           * Corrected nonexistent `docs/adr/0010-compact-typed-graph-storage.md` to actual `docs/adr/0010-static-value-flow-and-receiver-resolution.md` in Wave 0 `canonical_owners` and `sad_updates` (alongside `0001-crate-dag-and-layer-boundaries.md`, `0003-unified-profile-graph-and-state-machine.md`, and `0011-language-linkers-and-framework-manifests.md`).
           * Split monolithic Wave 2 into two bounded sequential tasks with realistic `max_net_loc: 450` budgets (plus mandatory `BUDGET_OVERRIDE` escalation if exceeded):
             1. **Wave 2a (`typed-graph-linker-and-raw-details-cutover`)** [depends on Wave 1 Tasks 1 & 2]: flips `Node.kind: NodeKind` (35-variant including `HtmlRequest`/`HtmlContext` + typed `request_role`), `Node.language: LanguageId`, and `CompactEdge.{kind: EdgeKind, confidence: EdgeConfidence}` with fail-closed `TryFrom<&PublicEdge>` + `CompactGraph::try_push_public_edge` across workspace callers/tests (`docs/architecture/indexing.md:146`, `docs/reference/languages.md:116-118`, `tests/linker/public_output.rs:5-38`, `tests/languages/html/{syntax,htmx_relations}.rs`, `cache/tests.rs:123`, `src/state_bridge/code_domain/` mechanical field updates); upgrades `FlowStore::read_named_uncertainties` + `__rust_*` `ValueExpr`/`TypeExpr` + deletes `ClassicFieldsFirstDto`/`ClassicBindingsFirstDto` + bumps `INDEX_SEMANTICS_VERSION`; narrows `LanguageLinker`/`FileRelationContext` to `pub(crate)` (keeping `LanguageProfile` `pub`, 0 `#[allow(private_interfaces)]`, updating `tests/languages/profiles.rs:153-160`); eliminates remaining `235` production raw `node.details[key]` sites (`languages/` 78, `linker/` 153, `db/` 4) + `3` cache test sites; promotes `ARCH-L2-NO-RAW-NODE-DETAILS-INDEX` to `severity: error`; and clears `semantic/types.rs` doc stubs.
             2. **Wave 2b (`acdd-error-db-dtos-mcp-and-cli-cleanup`)** [depends on Wave 2a + Wave 1 Task 3]: unifies `AcddError` + single-pass `config/loader.rs` (`Result<StrictAdapterReport, AcddError>`, no `io::Error` tunneling); deletes `StateEngine::{config, backend, query}` across all `37` `.backend()` + `1` `.config()` + `.query()` sites in `state_integration/*.rs`, `8` in `usecases/tests.rs`, and `tests/acdd/verification_and_git.rs`; strips dead `ComplexityLimits` struct fields with backward-compatible deprecated-key deserialization + `tests/core/{arch_lint,ast_contracts}.rs` + `docs/reference/configuration.md:113`; deletes `ChunkPersistSink`, `EdgeOccurrencesInput`, `TextMatchPreviewParams`, `CompositeDomainProvider::new`, `SchemaDef::{integer,number,boolean}`, `ParsedAdapterConfig`, `resolve_effective_state_db`, `ast_legacy_error`, `Server::with_domain_providers`, `ValidateResp` JSON patch, and unused `petgraph`/`zstd`/`hashbrown` in root `Cargo.toml`; relocates `WorktreeSpanContext` to `acdd_core::telemetry`; migrates `TraversalOptions` (`ImpactDirection`/`TraversalMode`) and `db/{reader,symbols,traversal}/` + `src/format/` onto typed DTOs; decomposes `src/mcp/server.rs` aligning `acdd.yaml` + `docs/architecture/cross-platform.md`; `#[cfg(test)]`-gates `PipelineTestHooks::delta_failure*`; fixes `src/cli/docs_check.rs` + `src/cli/mod.rs:917-927` EPIPE contract; and clears all remaining workspace doc stubs.
    - |-
      AR-005 Revised Architectural Blueprint Decision (supersedes #401, #399, and #397; resolves all 3 blocking findings and non-blocking seam typo in #402):

      0. Explicit Resolution of Round 3 Findings #402 (verified exhaustively via AST grep across crates/acdd-index/src/, src/, and tests/):
         - Finding 1 (Missing NodeKind::Object): Verified live emission of kind = "object" in languages/typescript/mod.rs:917, 979, 1012, ast/visitor.rs:702-705, 852, 859, and linker matching in linker/named_bindings.rs:344 and linker/semantic_context/context.rs:989. Expanded closed 1-byte Copy NodeKind in Wave 0 (crates/acdd-index/src/models/tags.rs) from 28 -> 36 variants by adding Object => "object" alongside ModuleDeclaration, View, Service, ToolRegistration, EventHandlerRegistration, HtmlRequest, and HtmlContext. Exhaustively audited all 26 Node literals in acdd-index, 8 in src/, and every symbol_kind/symbol/symbol_with_source/symbol_with_context/DeclarationRole arm across languages/ — 0 remaining unmapped production NodeKind or LanguageId values (Wave 2a updates synthetic test fixtures db/writer/cache/tests.rs:123 kind="scope" -> NodeKind::TemplateScope and linker/contracts.rs:671 language="unknown" -> LanguageId::Workspace).
         - Finding 2 (Missing EdgeKind::TemplateImports + ReferenceKind Audit): Verified live emission of "template_imports" in languages/html/mod.rs:591-592, 595 and direct edge emission in linker/resolution/framework_projections.rs:69, 98-106. Expanded closed 1-byte Copy EdgeKind in Wave 0 from 17 -> 34 variants by adding TemplateImports => "template_imports" alongside DefinesTable, QueriesTable, MutatesTable, ReferencesTable, UsesModel, CallsEndpoint, ReferencesRoute, RendersTemplate, InjectsDependency, Injects, Renders, ContextProvider, Includes, CitesDoc, CitedByDoc, and GovernedBy. Exhaustively audited all 31 push_edge calls and 18 Edge literals in acdd-index + src/ — 0 remaining unmapped live edge kinds (db/writer/entrypoints.rs:84-96 "routes_to"|"dispatches"|"listens_to" have 0 emitters in code/tests; in Wave 2a cold_ranks matches EdgeKind::Handles directly while SQL recompute_effective_categories retains DB text column queries). Audited ReferenceKind (models/tags.rs:93-110) against all Reference emissions and expanded it from 16 -> 22 variants in Wave 0 by adding TemplateImports, TemplateLoads, TemplateBindings, QueriesTable, MutatesTable, and ReferencesTable so CompactTag::known (interner.rs:365) covers all live template and SQL reference kinds.
         - Finding 3 (Wave 2a Scope Omitted src/engine/reader.rs & tests/receivers/): Verified Node.kind ==/&str comparisons in src/engine/reader.rs:478 (plus line 485 raw n.details["signature"]) and tests/receivers/{python/branch_joins.rs:47,154,216, python/collections.rs:68, rust/field_chains.rs:30,35,79}. Audited all remaining src/ and tests/{acdd,common,commitment_integrity,incremental,mcp,query_context} directories — 0 other out-of-scope Node.kind, Node.language, CompactEdge, CompactGraph, LanguageLinker, or FileRelationContext references exist. Added src/engine/reader.rs and tests/receivers/ to task typed-graph-linker-and-raw-details-cutover scope in docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md, ran acdd sync (updated 30 nodes) and acdd validate (0 errors, 0 warnings).
         - Non-blocking seam cleanup: Removed stray source_task field and note prefix from proof.tasks["typed-graph-linker-and-raw-details-cutover"].outgoing_seams[0].

      1. Preserved Accepted Architecture from #401:
         - Wave 0 freezes AcddError (+ pub type StateError = AcddError bridge), 36-variant NodeKind, 34-variant EdgeKind, 22-variant ReferenceKind, LanguageId, EdgeConfidence, and real ADRs 0001/0003/0010-static-value-flow-and-receiver-resolution.md/0011-language-linkers-and-framework-manifests.md (reconciling HtmlRequestAttributes onto canonical NodeKind::HtmlRequest/HtmlContext + typed NodeDetailKey::REQUEST_NODE_ROLE metadata preserving zero-Rust .acdd/frameworks/*.toml overrides).
         - Wave 1 executes 3 strictly disjoint parallel tasks (ast/models/typed_facts/scanner/interner.rs; languages/linker/trait-inversion + Tier-2 OO; acdd-state 8-bag ControlFlow cleanup with StateEngine::{config,backend,query} cleanly deferred to Wave 2b).
         - Wave 2a executes typed-graph/linker/raw-details cutover (now scoping src/engine/reader.rs and tests/receivers/, fail-closed TryFrom<&PublicEdge> for CompactEdge, pub(crate) LanguageLinker/FileRelationContext keeping LanguageProfile pub, remaining 235+3 raw node.details migrations, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX: error ratchet).
         - Wave 2b executes AcddError + single-pass config/loader.rs, StateEngine::{config,backend,query} deletion across all tests, backward-compatible ComplexityLimits field cleanup, typed DB reader/formatter DTOs + ImpactDirection/TraversalMode, WorktreeSpanContext relocation, src/mcp/server.rs split, cli/docs_check.rs EPIPE + src/cli/mod.rs:917-927 contract update, unused root Cargo.toml dep deletion, and remaining doc stubs.
    - 'BUDGET_OVERRIDE escalation request for language-linker-trait-inversion-and-tier2-members.build. Watchdog #408 confirms the initial LanguageMemberIndexes facade did not invert LanguageLinker. The accepted #404 contract requires lifetime-preserving LanguageLinker hooks and opaque state across member lookup, imports, dispatch, and candidate filtering, eliminating per-language Context/ImportContext fields and universal __rust_* branches. Completing that cutover across the admitted linker/languages/tests/ADR scope, plus consolidating the three Rust resolvers and Tier-2 tests, is not a clean localized extraction and is expected to exceed max_net_loc=450. Please authorize a measured override (proposed max_net_loc=900) or provide a narrowed seam boundary. I am continuing within current budget while this is reviewed; I will not self-approve the override.'
    - 'Coordinator ruling on #409 (`language-linker-trait-inversion-and-tier2-members.build`): `BUDGET_OVERRIDE` is conditionally authorized up to <= 650 net non-test LOC ONLY if genuinely required by real `LanguageLinker` trait inversion + opaque state + 3-way Rust resolver consolidation + clean zero-clone/zero-dead-tail hygiene (target staying within <= 450 net prod LOC where deletions offset additions; current net diff is +33 LOC). Keep `semantic/types.rs` untouched for Wave 2a.'
    - 'BUDGET_OVERRIDE: Approving +593 net non-test LOC (<= 650 ceiling pre-authorized in #410) for task `language-linker-trait-inversion-and-tier2-members`: full `LanguageMemberIndex` / `LanguageImportState` inversion, `CallEvalContext` parameter grouping (removing 8 new `#[allow(private_interfaces)]` and 2 `#[allow(clippy::too_many_arguments)]`), restored `fallback_receiver_value` hint-free Rust receiver flow, Cargo package-isolation fix, JS/Vue member registration, and doc-stub cleanup, with 0 warnings across Clippy/lint and 262+91+100+9 passing tests.'
    - 'Budget escalation for reopened build: measured `git diff bef5bd729 --numstat -- crates/acdd-index/src/languages crates/acdd-index/src/linker docs/adr/0011-language-linkers-and-framework-manifests.md` at 1,977 additions, 1,353 deletions, +624 net production lines, leaving 26 lines under the conditional +650 ceiling in #410; #427 approved +593 for the previous candidate. Reopened scope still requires independent trait dispatch inversions in dispatch.rs (property reference, trait-object call, builtins, HTML templates, optional chaining and explicit module prefixes) and replacement of concrete Python/Rust fields and construction in indexing.rs/mod.rs, plus other universal files. These are materially broader than the already approved candidate; no implementation has been attempted, so the eventual net overage is not yet measured or proven. Request architect/coordinator budget ruling or a revised task slice before implementing this reopened work; no override is self-approved.'
    - '{"approved_max_net_loc": 950, "baseline_commit": "48ff9cfb8090daa6daa901f1a33e7cc7b3b99721", "budget_override": true, "reason": "Coordinator approves reopened T2 completion budget up to +950 cumulative net production LOC (above +624 prior accepted candidate) to complete INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY and ADR 0011 across universal linker modules (dispatch, members, candidates, evaluate, initialize, lookup, semantic_context, preparation, imports, indexing, mod.rs), extract forwarding.rs clone, and enable cross-file Tier-2 OO inheritance without copy-paste boilerplate.", "responds_to": 439}'
    - 'Architectural decision requested for recurring incomplete-remediation cluster: accepted #404 requires lifetime-preserving opaque LanguageLinker/LanguageImportState/LanguageMemberIndex inversion across the universal linker, but repeated reviews (#428, #430, #437) and current inspection still expose cross-cutting concrete state construction (mod.rs:1049-1124; indexing.rs:179-248) and many direct per-language dispatch calls. RustMembers::build requires concrete RustForwarding during construction; Python receiver construction consumes shared namespace/export indexes; TypeScript member construction occurs after semantic context exists. A cohesive ownership/lifetime design is needed to replace these without Any/downcasts, feature-flag leaks, duplicated construction or premature context borrowing. #440 authorized +950 cumulative LOC but does not specify this construction seam. Current partial scoped production diff is +767 net vs bef5bd729; no submission. Please supply an explicit architecture decision or route to rearchitect. This is not a request to expand budget or narrow acceptance.'
    - '{"architectural_decision": "Resolution for #446: (1) crates/acdd-index/src/linker/mod.rs and crates/acdd-index/src/linker/indexing.rs serve as the stack-lifetime composition root of the linker and MAY construct concrete language states (RustForwarding, RustMembers, PythonReceivers, ResolvedCommonJsBindings, TypeScriptMembers, HTML registries) in dependency order before injecting them strictly as &dyn LanguageImportState / &dyn LanguageMemberIndex into Context and ResolutionInput (no Any/downcasts). Record this composition-root invariant in docs/adr/0011-language-linkers-and-framework-manifests.md. (2) Any LanguageLinker implementation for HTML (HtmlLinker) MUST override allows_multi_file_modules() -> true to preserve tests/languages/profiles.rs matrix compatibility until Wave 2a. (3) Universal phase/evaluation modules (resolution/, value_flow/, semantic_context/) MUST still eliminate direct language-name branching and direct per-language module calls via cohesive LanguageLinker hooks within the #440 +950 net prod LOC ceiling.", "responds_to": 446}'
    - '{"architectural_decision": "Refinement to #449 for finite T2 closure: Per AR-005 AC-1 deferral of ValueExpr/TypeExpr internal variants to Wave 2a, TypeTarget::Rust*-coupled branches in semantic_context/{mod,type_resolution}.rs and value_flow/evaluate.rs remain until Wave 2a (zero throwaway intermediate traits). In this final T2 build step, finish ONLY the remaining universal phase dispatch leaks in resolution/value_flow_phase/candidates.rs (builtin_origin_and_evidence, external_annotation_type) and resolution/value_flow_phase/dispatch.rs (property_reference, trait_object_call, html::templates::resolve_reference, optional chaining, explicit module prefix) via LanguageLinker hooks (staying within #440 +950 cumulative net prod LOC vs 48ff9cfb8), then complete all Cargo checks, graph hygiene queries, acdd verify, and acdd submit --action pass.", "responds_to": 450}'
    - 'Decision requested: removing the obsolete LanguageLinker::framework_manifest hook exposes existing public DependencyRegistry::framework_manifest_for_path as the sole newly phantom-tested method (baseline 19, candidate 20). It has externally observable local-override and dependency-activation semantics distinct from the active ungated manifest pipeline, and no remaining production use with matching semantics was found. Please decide whether this public API may be deleted with its tests, or whether its established external API status warrants an explicit hygiene exception. No BUDGET_OVERRIDE is requested; net production LOC remains within 450.'
    - '{"architectural_decision": "Resolution for #464: Delete dead public method `DependencyRegistry::framework_manifest_for_path` (`crates/acdd-index/src/languages/dependency_registry/framework_packs.rs:704`) rather than retaining a phantom_tested hygiene exception. Verified that even at baseline HEAD its sole production reference was the dead uncalled default trait method `LanguageLinker::framework_manifest` (`profile.rs:632`), while live production manifest resolution runs through `effective_framework_manifest` + `declares_for_path`. Extend task scope via `acdd extend-scope` to admit `tests/languages.rs` (within `Owners: tests/`), migrate `dependency_registry/tests.rs` onto `effective_framework_manifest` / `declares_for_path` (removing tests that solely exercise the deleted dead method), and replace `framework_manifest_for_path` calls in `tests/framework_manifests/{activation.rs, activation/user_manifests.rs, cross_pack_relations.rs}` and `tests/languages.rs` with real indexing/manifest seam assertions (`indexed.conn` / `declares_for_path` / `extract_typed_with_registry`). Confirm `phantom_tested <= 19` (0 introduced), rebuild index, rerun affected tests/clippy/acdd verify, and `acdd submit ... --action pass`.", "responds_to": 464}'
    - |-
      ESCALATION — requesting independent `BUDGET_OVERRIDE` review for `task:acdd-error-db-dtos-mcp-and-cli-cleanup:build`:
      - Measured by `acdd submit`: `+673` net production LOC vs `max_net_loc=450` (`+223` over budget).
      - Root cause of net LOC delta: mandatory Retirement Ledger §4 cutover replacing all untyped `serde_json::Map<String, Value>` / `.as_object_mut()` response mutations across `src/format/presentation.rs` (`+383` net, 14 sites -> typed `CompactRootDto`, `NavigationPageDto`, `NavigationItemDto`, `FreshnessDto`, `SourcePreviewDto`, `SourceCandidateDto`, `TasksPageDto`, `ItemsPageDto`, `SearchNodesEnvelopeDto`), `src/format/core.rs` (`+46` net), `crates/acdd-index/src/db/symbols/` (`+91` net), `crates/acdd-index/src/db/reader/` (`+44` net), and `crates/acdd-index/src/db/traversal/` (`+112` net) with explicit `#[derive(Deserialize, Serialize)]` typed DTO structs, plus splitting `src/mcp/server.rs` (`-1499`) into 5 acyclic modules under `src/mcp/server/` (`mod.rs`, `types.rs`, `admission.rs`, `worktree.rs`, `session.rs`, `+41` net module headers/docstrings) and backward-compatible `ComplexityLimitsWire` custom `Deserialize` (`+30` net).
      - Simplification / cleanliness verified: `0` clones introduced (`duplicates`: 42/444 <= baseline 42/465), `dead_tails`: 139/8/0 <= baseline 140/19/0, `acdd lint --categories all`: 0 errors / 189 warnings (baseline 201), `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean, `commitment_integrity` 9/9, full `cargo test --workspace --all-targets` 923/923 passed, `/tmp/contextunity-test-*` 5488==5488, `acdd verify` passed (`execution_digest=4906f5b0e6a8cf6aba9531f119c5eb252e60896af5ebdfd8d87463950bc6af6`).
      Requesting independent `BUDGET_OVERRIDE: <reason>` decision on `acdd_blackboard` (`topic: decisions`) from coordinator/architect before re-running `acdd submit --action pass`.
    - '{"approved_max_net_loc": 720, "baseline_commit": "608a1db16e3b0a5aee1cfb6e38195412b2269269", "budget_override": true, "reason": "BUDGET_OVERRIDE: Coordinator verifies +673 net production LOC (above 450) is driven strictly by mandatory AC-4/AC-1 deliverables: typed serde DTO structs replacing untyped serde_json::Map/.as_object_mut() across src/format/{presentation,core}.rs (+429) and crates/acdd-index/src/db/{reader,symbols,traversal} (+247), clean 5-file acyclic split of src/mcp/server.rs (+48 module headers), ComplexityLimitsWire custom Deserialize (+30), and #459 polish scope/extend_scope support (+7), while net deletions in crates/acdd-core/state and src/format/traversal.rs (render_duplicates collapsed) reduced duplicates (465->444 lines), dead_tails (140->139), phantom_tested (19->8), and warnings (201->189) with 0 lint errors and 923/923 passing tests.", "responds_to": 474}'
    - |-
      AR-005 Architectural Blueprint Extension for Waves 2c & 2d (preserves #404 W0..W2b architecture and closes #436):

      1. Wave 2c (`submit-verify-env-isolation-and-scope-fixes`, depends_on: `acdd-error-db-dtos-mcp-and-cli-cleanup`):
         - Reconcile all 12 files from sibling commit `4bb239694` (`run/fix-submit-verify-env-and-scope`, `goal-20261124141330-293224-1`) cleanly onto Wave 2b's refactored `crates/acdd-state/src/usecases/{verify.rs, submit/mod.rs, submit/freshness.rs}`, `crates/acdd-state/src/runner/command.rs`, `crates/acdd-state/src/profile/shell_template.rs`, `crates/acdd-state/tests/state_integration/{runner,usecases_e2e}.rs`, `tests/acdd/lint_gate_and_docs_profile.rs`, `tests/common/{mod,cli,mcp_client}.rs`, and `tests/mcp/harness_contract.rs`. Because Wave 1/2b deleted `PreparedVerifyPhase`/`GateCallCtx` in `verify.rs` and modified `submit/` and `usecases_e2e.rs`, reconcile `4bb239694` semantically instead of blindly overwriting Wave 2b changes.
         - Close all 4 review follow-ups from `#436`:
           1) Multi-check `verify -> submit` test row in `crates/acdd-state/tests/state_integration/usecases_e2e.rs` proving post-mutation `index_generation` preservation across two checks (`code.seam` + mutating second check bumping `index_gen`).
           2) Isolate remaining direct `Command::new(CARGO_BIN_EXE_acdd)` callers outside `tests/common` (`tests/mcp/cli_roots.rs::cli_with_explicit_roots`, `tests/acdd/lifecycle_and_state.rs`, and remaining unisolated invocations in `tests/core/{docs_check,debug_logging,storage_batches/storage_persistence}.rs`, `tests/languages/rust/workspace_factories.rs`, `tests/mcp/{installer,transport}.rs`) through `isolate_acdd_env` / `run_cli_args` while preserving explicit `.env("ACDD_*", ...)` test settings.
           3) In `crates/acdd-state/src/usecases/submit/freshness.rs::inspect_submit_preflight`, validate `node.status == NodeStatus::InProgress`, `claim_revision` match, and `worker_id` ownership BEFORE `ensure_fresh_trace`, so unclaimed or stale-revision `needs_trace` gates return `StaleClaimRevision` rather than `TaskProofUnobserved`.
           4) Add `"crates/acdd-state/tests/**/*.rs": "--package acdd-state --test state_integration"` to `.acdd/profile.yaml` (`scope_args_by_pattern`), and extend `crates/acdd-state/src/profile/shell_template.rs::select_scope_args` to match non-trailing-slash directory prefixes and `/**` directory scope patterns alongside trailing `/` prefixes for placeholder-free patterns.
         - Retire orphan active hotfix runs (`goal-20261124141330-293224-1` and `goal-20261124093934-3551704-1`) and remove any leftover `.worktrees/fix-submit-verify-env-and-scope` worktree cleanly once integrated.

      2. Wave 2d (`dead-tails-duplicates-and-call-graph-closure`, depends_on: `submit-verify-env-isolation-and-scope-fixes`):
         - Live `W_RUN` baseline measured via `acdd --root ... query run dead_tails|duplicates` & `acdd lint --categories all`: `0` errors, `189` warnings (`131` `dead_tails`, `17` `phantom_tested`, `0` `dangling_calls`, `41` `duplicates` at `>=5L` / `1` group at `>=10L` `admit_reclaim_in_progress`).
         - Fix 4 verified upstream Rust call-graph / extractor / `dead_tails` closure gaps (preserving `INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY` with zero language-name branches in universal `linker/resolution/`):
           1) `crates/acdd-index/src/languages/rust/mod.rs` (`extract_serde_callable_references` -> generalize for `#[schemars(schema_with = "...")]` alongside `serde` callable keys; extract bare `identifier` / `scoped_identifier` function-value arguments passed to calls such as `map_rows`, `.map()`, `.and_then()`, `.filter_map()`), plus `crates/acdd-index/src/db/lint/dead_tails.rs::wire_value_expr_callbacks` traversal of `ValueExpr::Call` / `ValueExpr::RustProject` callbacks.
           2) `crates/acdd-index/src/linker/rust/value_flow.rs` (`constructor_local_result` / `return_type_tracked`): propagate `-> Self` return types on associated constructors beyond `new/default/create/current/none` (e.g. `Context::build`) and chained builder methods (`.with_member_state(...).with_flows(...)`) so `linker/mod.rs:1115-1117` resolves `Context::with_flows` and `NamedBindings::build_lookup`.
           3) `crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs::collect_lexical_scope_state`: make local binding shadowing source-position-aware (via `FlowState` / `LanguageLinker` hook) so self-shadowing initializers (`let tokens = tokens(source)` in `languages/sql/mod.rs:469,930`) do not shadow their own RHS call.
           4) Add dedicated seam tests in `tests/languages/` (`tests/languages.rs`) and `tests/core/` (`tests/core.rs`) per `INV-TEST-BINARY-BUDGET`.
         - Re-measure `dead_tails`, `phantom_tested`, and `duplicates --min-lines 5` after Subtask 1 lands, then delete genuinely dead production helpers, `#[cfg(test)]`-gate test-only hooks (preserving `code_map_impact`/`tests`/`prove_removal` safety), and collapse remaining `>=5L` block/similar clones (`admit_reclaim_in_progress`, `evaluate_verify`, `hydrate_and_link_delta`, `present_value_bounded`, `trim_tasks_page_tail`, `compute_merkle_root`, `coverage_dictionary`, `split_top_level_args`, `changed_nodes_page`, `project_graph_domains`, `make_reset_patch`, `evaluate_reject`).
    - |-
      AR-005 Architectural Blueprint Extension Addendum (supersedes #481 and resolves all 3 findings in #482):

      0. Explicit Resolution of #482 Findings:
         - Finding 1 (Unquoted `proof.tasks` keys): Re-submitting `root:blueprint_synthesis.synthesize` via confined JSON (`@path`) with bare unquoted `task_ref` keys for all 8 tasks (`upfront-contract-synthesis-and-dag-prioritization`, `ast-models-scanner-details-and-dto-shim-cleanup`, `language-linker-trait-inversion-and-tier2-members`, `state-usecase-control-flow-and-context-bag-removal`, `typed-graph-linker-and-raw-details-cutover`, `acdd-error-db-dtos-mcp-and-cli-cleanup`, `submit-verify-env-isolation-and-scope-fixes`, `dead-tails-duplicates-and-call-graph-closure`) so `extract_pre_task_slice` (`crates/acdd-state/src/context/slices.rs:116-117`) and `enforce_blueprint_max_net_loc` (`crates/acdd-state/src/usecases/submit/budget.rs:34-39`) match every `task_ref` cleanly.
         - Finding 2 (Wave 2c `tests/core/` + `tests/languages/` alignment): Spec `docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md:317-326` and `gate_nodes.scope_json` now admit `tests/core/` and `tests/languages/`. Wave 2c `canonical_owners`, `mechanism_rules`, and `verification_seam` align 1:1 to isolate all remaining direct `Command::new(CARGO_BIN_EXE_acdd)` callers in `tests/mcp/cli_roots.rs::cli_with_explicit_roots`, `tests/acdd/lifecycle_and_state.rs`, `tests/core/{docs_check,debug_logging,storage_batches/storage_persistence}.rs`, `tests/languages/rust/workspace_factories.rs`, and `tests/mcp/{installer,transport}.rs` through `isolate_acdd_env` / `run_cli_args` while preserving explicit `.env("ACDD_*", ...)` overrides.
         - Finding 3 (Phase 3 submit preflight ordering for `#436` item 3): Phase 1 `crates/acdd-state/src/usecases/submit/freshness.rs::inspect_submit_preflight` intentionally returns `Ok(None)` when `node.status != NodeStatus::InProgress` or `claim_revision` mismatches. Therefore `#436` item (3) MUST be fixed in Phase 3 (`crates/acdd-state/src/usecases/submit/mod.rs::submit_holding_repo_lock` -> `prepare_submit_observations` -> before `ensure_fresh_trace`), validating `node.status == NodeStatus::InProgress`, `claim_revision` match, and `worker_id` ownership (or evaluating `evaluate_submit` claim preconditions) prior to calling `ensure_fresh_trace`, so unclaimed or stale-revision `needs_trace` gates return `StaleClaimRevision` (or `TaskAlreadyClaimed`) rather than `TaskProofUnobserved`.

      1. Preserved Decisions from #481 & #404:
         - All W0..W2b task slices and architecture from #404 remain unchanged.
         - Wave 2c (`submit-verify-env-isolation-and-scope-fixes`): reconcile all 12 files from commit `4bb239694` cleanly onto Wave 2b's refactored `verify.rs`, `submit/`, and `usecases_e2e.rs`, close all 4 `#436` follow-ups (including Phase 3 claim/revision guard before `ensure_fresh_trace`, multi-check `index_generation` preservation row in `usecases_e2e.rs`, `.acdd/profile.yaml` `crates/acdd-state/tests/**/*.rs` mapping + `shell_template.rs::select_scope_args` trailing/non-trailing/`/**` directory scope matching, and remaining `CARGO_BIN_EXE_acdd` isolation across `tests/{mcp,acdd,core,languages}/`), and retire orphan active hotfix runs (`goal-20261124141330-293224-1`, `goal-20261124093934-3551704-1`).
         - Wave 2d (`dead-tails-duplicates-and-call-graph-closure`): fix all 4 Rust call-graph / extractor / `dead_tails` closure gaps (`#[schemars(schema_with = "...")]` + function-value arguments in `languages/rust/mod.rs` + `dead_tails.rs::wire_value_expr_callbacks`, `Self`/builder-chain return propagation in `linker/rust/value_flow.rs` for `Context::build().with_member_state().with_flows()`, and source-position-aware local shadowing in `dispatch.rs::collect_lexical_scope_state` for `sql::tokens`) with seam tests in `tests/languages.rs` and `tests/core.rs`, then sweep remaining genuine `dead_tails`, `phantom_tested`, and `>=5L` duplicates workspace-wide.
    deferred_findings:
    - 'Wave 2a follow-up (deferred per #451 finite T2 closure): (1) Migrate remaining non-TypeTarget::Rust* direct language calls/branches in universal linker modules — inferred.rs (JS/TS lookup_property, Python __init__/__call__/module calls, Rust associated/reexport/constant calls, Vue component/slot calls), candidates.rs (macro_reference_candidate, admits_static_receiver_candidate, macro_analysis_gap, has_protocol_or_abc_base, impl_source, html_scripts), dispatch.rs (implicit_receiver_member, dynamic_receiver_member, resolve_early_language_specials, resolve_vue_template_typed_member, plus cosmetic indentation in dispatch.rs:44-76), initialize.rs:375 (collection_return_element), and HTML-gated fields on ReferencePhase in linker/traits.rs. (2) Keep Context::member / lookup_local_member_mode free of language == "python" exceptions once Python member lookup uses LanguageMemberIndex uniformly.'
    - 'Operator directive for Wave 2b (`crates/acdd-state/src/`): (1) Add `ROADMAP.md` and `TESTS.md` to `root:polish` default `scope` in `crates/acdd-state/src/profile/embedded/code.yaml`. (2) Support `extend_scope` for `root:polish` (`NodeKind::RootGate` `root:polish`, non-terminal) using the same `scope_extensions` persistence/verification mechanics in `crates/acdd-state/src/machine/scope.rs`, `machine/verify.rs` (`container_scope` / `effective_scope`), and `tests` (`crates/acdd-state/tests/state_integration/`), while keeping `protected_paths` and other `RootGate`s fail-closed. Also fix pre-existing broken doc links (`ROADMAP.md:26`, `docs/milestones/AR-005-...md:44`, `docs/milestones/archive/AR-004-...md:291,302`) if not already resolved by polish so `docs check` exits 0.'
    - 'Non-blocking: tests/mcp/transport.rs:159 and tests/mcp/installer.rs Command::new(&binary) reload/read/version spawns still inherit ambient ACDD_*/GIT_*. They are copied install binaries, not Command::new(env!(CARGO_BIN_EXE_acdd)). The named CARGO_BIN_EXE_acdd serve path is already scrubbed inside StdioClient::with_pending_initialization. Optional follow-up: isolate those copied-binary CLI spawns while preserving explicit HOME / ACDD_* overrides.'
  metrics_summary:
    decisions_count: 18
    deferred_findings_count: 3
    failed_traces: 6
    passed_traces: 48
    task_receipts_count: 8
    tasks_cancelled: 1
    tasks_completed: 8
    tasks_total: 9
    total_claims: 70
    total_review_cycles: 7
    total_traces: 65
---

# AR-005: Typed Graph Core, Full `LanguageLinker` Trait Inversion, Context-Bag Removal & Idiomatic Rust Cleanup

## Outcome and Purpose

Following [AR-004](AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul.md), a deep workspace-wide Rust architecture audit across all `423` production `.rs` files (`209.7k` LOC) identified structural technical debt and legacy scaffolding that limit type safety, linker extensibility, memory efficiency, and idiomatic readability:

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
target: Freeze AcddError design and complete closed Copy-enum tag contracts (36-variant NodeKind including ModuleDeclaration/View/Service/ToolRegistration/EventHandlerRegistration/HtmlRequest/HtmlContext/Object + 34-variant EdgeKind covering all SQL/projection/template/doc edge kinds including TemplateImports + 22-variant ReferenceKind) in acdd-core/acdd-index, align ADR 0001/0003/0010/0011 (settling HtmlRequestAttributes canonical NodeKind + typed request_role metadata), and lock disjoint Wave 1 scopes
scope:
- docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md
- docs/adr/
- crates/acdd-core/src/errors.rs
- crates/acdd-index/src/models/tags.rs
tags:
- default
invariants:
- 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2a/2b execute cross-crate cutovers.'
acceptance_criteria:
- 'AC-1: AcddError type design, complete closed Copy-enum tag contracts (36-variant NodeKind adding ModuleDeclaration, View, Service, ToolRegistration, EventHandlerRegistration, HtmlRequest, HtmlContext, Object; 34-variant EdgeKind adding DefinesTable, QueriesTable, MutatesTable, ReferencesTable, UsesModel, CallsEndpoint, ReferencesRoute, RendersTemplate, InjectsDependency, Injects, Renders, ContextProvider, Includes, TemplateImports, CitesDoc, CitedByDoc, GovernedBy; 22-variant ReferenceKind adding TemplateImports, TemplateLoads, TemplateBindings, QueriesTable, MutatesTable, ReferencesTable; LanguageId; EdgeConfidence), ADR updates (docs/adr/0001-crate-dag-and-layer-boundaries.md, 0003-unified-profile-graph-and-state-machine.md, 0010-static-value-flow-and-receiver-resolution.md, and 0011-language-linkers-and-framework-manifests.md settling HtmlRequestAttributes canonical NodeKind::HtmlRequest/HtmlContext + typed request_role metadata preserving zero-Rust .acdd/frameworks/*.toml overrides), and disjoint Wave 1 write scopes are frozen with 0 scope overlaps and verified inside acdd-core/acdd-index.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: freeze-core-error-and-tag-contracts
  title: Freeze AcddError alias/type contract in errors.rs, complete 36-variant NodeKind, 34-variant EdgeKind, and 22-variant ReferenceKind tag helpers in models/tags.rs, and update ADRs 0001/0003/0010/0011
  status: completed
  evidence: 'Verified frozen AcddError + StateError alias in crates/acdd-core/src/errors.rs, 36-variant NodeKind / 34-variant EdgeKind / 22-variant ReferenceKind #[repr(u8)] Copy tags in crates/acdd-index/src/models/tags.rs, and aligned ADRs 0001/0003/0010/0011 (clippy + acdd-core/acdd-index lib tests + commitment_integrity green).'
- subtask_ref: register-seam-tests-and-disjoint-scopes
  title: Pre-register seam test modules in tests/{core,linker,mcp}.rs and verify disjoint Wave 1 write scopes
  status: cancelled
  evidence: 'Cancelled stale pre-sync subtask row: premature tests/{core,linker,mcp}.rs #[path] registration was explicitly dropped in Round 1 Finding #398 and superseded by verify-disjoint-wave1-scopes in docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md:146-150 (noted in Round 3 Finding #402).'
- subtask_ref: verify-disjoint-wave1-scopes
  title: Lock disjoint Wave 1 write scopes, sequential Wave 2a/2b handoff contracts, and acdd-core/acdd-index tag-contract checks without cross-task test stubs
  status: completed
  evidence: Verified strictly disjoint Wave 1 write partitions across ast-models-scanner-details-and-dto-shim-cleanup, language-linker-trait-inversion-and-tier2-members, and state-usecase-control-flow-and-context-bag-removal plus sequential Wave 2a -> Wave 2b handoff dependencies in spec and state.sqlite without cross-task test stubs.
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:upfront-contract-synthesis-and-dag-prioritization.deliver
  gate_id: deliver
  worker_id: ar05-w0-reviewer-pro
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 5fbe9508f604aa7f525b9fb4f0d7154a0a52b1ea
  committed_oid: 48ff9cfb8090daa6daa901f1a33e7cc7b3b99721
  accepted_at: 2026-11-24T07:04:08.642Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-DISJOINT-WAVES: All three Wave 1 tasks share zero overlapping write paths and compile independently against frozen core error/tag contracts before Wave 2a/2b execute cross-crate cutovers.'
      verified:
      - INV-AR005-DISJOINT-WAVES
    review_summary: Delivered Wave 0 contract freeze (errors.rs, models/tags.rs, ADRs 0001/0003/0010/0011, and disjoint Wave 1 / sequential Wave 2a/2b spec contract).
```

### task: ast-models-scanner-details-and-dto-shim-cleanup

```yaml
task_ref: ast-models-scanner-details-and-dto-shim-cleanup
target: 'Stage additive Copy-enum and typed detail accessors on Node and CompactEdge, delete WalkContext, clear #[allow(unused_imports)] in typed_facts/binary.rs, and eliminate all 55 raw node.details[key] accesses and mechanical doc stubs in ast/, models/, typed_facts/, scanner/, and interner.rs'
scope:
- crates/acdd-index/src/interner.rs
- crates/acdd-index/src/models/
- crates/acdd-index/src/typed_facts/
- crates/acdd-index/src/ast/
- crates/acdd-index/src/scanner/
- tests/languages/
extended_scope:
- path: tests/languages/ast_patterns.rs
  reason: 'Watchdog #407 requires a focused integration test that exercises the new CompactEdge typed tag helpers; this file is in tests/languages, but the current code map delta inventory does not admit it.'
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
tags:
- default
invariants:
- 'INV-AR005-AST-MODELS-CLEAN-DETAILS: ast/, models/, typed_facts/, scanner/, and interner.rs contain 0 raw node.details[key] accesses, 0 WalkContext bags, and 0 mechanical doc stubs.'
acceptance_criteria:
- 'AC-1: Additive typed NodeKind/LanguageId/EdgeKind/EdgeConfidence constructors/accessors and typed NodeDetailKey accessors in models/details.rs are staged without breaking downstream callers; #[allow(unused_imports)] in typed_facts/binary.rs and WalkContext are deleted (deferring FlowStore::read_named_uncertainties error-type cutover and #[allow(clippy::result_unit_err)] removal to Wave 2a); mechanical doc stubs in ast/models/scanner/typed_facts are replaced; and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX reports 0 warnings across ast/, models/, typed_facts/ (4 sites), and interner.rs (2 sites) totaling 55 eliminated sites.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: stage-typed-accessors-and-clean-typed-facts
  title: 'Add typed Copy-enum constructors/accessors on Node and CompactEdge and remove #[allow(unused_imports)] in typed_facts/binary.rs while preserving FlowStore::read_named_uncertainties Result<(), ()> compatibility until Wave 2a'
  status: completed
  evidence: 'Added additive Copy-enum accessors on Node (kind_tag/with_kind_tag/language_tag/with_language_tag) and CompactEdge (kind_tag/confidence_tag) in models/types.rs & models/compact_graph.rs, removed #[allow(unused_imports)] in typed_facts/binary.rs, preserved FlowStore::read_named_uncertainties Result<_, ()> + #[allow(clippy::result_unit_err)] for Wave 2a; verified via cargo clippy -p acdd-index --all-targets --all-features -- -D warnings, cargo test -p acdd-index --lib (101 passed), cargo test --test languages (258 passed), and commitment_integrity (9 passed).'
- subtask_ref: eliminate-ast-models-raw-details-and-walk-context
  title: Migrate all 55 raw node.details[key] sites across ast/ (39), models/ (10), typed_facts/ (4), and interner.rs (2) onto typed semantic accessors, remove scanner WalkContext, and replace mechanical doc stubs in scope
  status: completed
  evidence: Migrated all 55 raw node.details[key] sites across ast/ (39), models/ (10), typed_facts/ (4), and interner.rs (2) onto NodeDetailKey/NodeDetail*/NodeSemanticEntry accessors, replaced WalkContext/RootWalkOptions with CandidateWalker in scanner/walk.rs, extracted ast/mod.rs with_language_str (#416 remediation), cleared all mechanical doc stubs in scope, and verified 0 ARCH-L2-NO-RAW-NODE-DETAILS-INDEX warnings, 0 lint errors/warnings in scope, 0 introduced >=5L clones, and 0 new dead_tails.
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:ast-models-scanner-details-and-dto-shim-cleanup.deliver
  gate_id: deliver
  worker_id: ar05-w1-ast-reviewer-r3-pro
  claim_revision: 1
  contract_revision: 2
  candidate_tree: fd954addfeb047f33bde9724a92c8dd6c1f9a449
  committed_oid: e0acee2937b7c1efb4f9dd90cc81b85e3792d7e9
  accepted_at: 2026-11-24T09:15:11.447Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-AST-MODELS-CLEAN-DETAILS: ast/, models/, typed_facts/, scanner/, and interner.rs contain 0 raw node.details[key] accesses, 0 WalkContext bags, and 0 mechanical doc stubs.'
      verified:
      - INV-AR005-AST-MODELS-CLEAN-DETAILS
    review_summary: Deliver Wave 1 Task 1 (ast-models-scanner-details-and-dto-shim-cleanup) with both subtasks completed and Round 3 independent review passed.
```

### task: language-linker-trait-inversion-and-tier2-members

```yaml
task_ref: language-linker-trait-inversion-and-tier2-members
target: 'Extend LanguageLinker hooks to eliminate per-language Context fields, universal-linker __rust_* string checks and language branches, BuiltinResolutionFlags, MemberAccumulator, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member indexing while keeping semantic/types.rs unchanged and public profile/linker test seams compiling until Wave 2a'
scope:
- crates/acdd-index/src/languages/
- crates/acdd-index/src/linker/
- docs/adr/0011-language-linkers-and-framework-manifests.md
- tests/linker/
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
tags:
- default
invariants:
- 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string checks or language-specific branches in universal linker modules, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
acceptance_criteria:
- 'AC-1: LanguageLinker owns member lookup, wrapper projection, polymorphic dispatch, and candidate filtering via lifetime-preserving LanguageMemberIndex/LanguageImportState hooks while leaving semantic/types.rs untouched and existing public LanguageProfile/LanguageLinker test calls compiling until Wave 2a; BuiltinResolutionFlags, MemberAccumulator, universal-linker __rust_* string checks, duplicate Rust path resolvers, #[allow(clippy::result_unit_err)] in linker/routes.rs, and mechanical doc stubs across languages/ and linker/ are eliminated (deferring internal ValueExpr/TypeExpr variant replacement across semantic/types.rs, typed_facts/binary.rs, languages/rust/value_flow/, and linker/rust/value_flow.rs to Wave 2a); and Go/Java/Kotlin/C#/C++/PHP/Ruby resolve class/interface/method members via shared member indexing.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: invert-language-linker-trait-and-context
  title: Remove per-language fields from ImportContext and semantic_context::Context and migrate dispatch/evaluate/type_resolution hooks onto LanguageLinker
  status: completed
  evidence: 'Reopened audit review passed (candidate_tree 9d4c827981c0759af4b28520fd510a50febd0cc4): LanguageLinker hooks inverted across candidates.rs and dispatch.rs, HtmlLinker preserves multi-file matrix compatibility, ADR 0011 records stack-lifetime composition root (#449/#451), clippy/tests/lint green.'
- subtask_ref: unify-rust-resolvers-tier2-members-and-doc-stubs
  title: Consolidate duplicate Rust path/type resolvers, delete BuiltinResolutionFlags/MemberAccumulator, remove universal-linker __rust_* string branches into linker/rust/ hooks, clear doc stubs in languages/ and linker/, and wire Tier-2 OO profiles into shared member/inheritance indexing
  status: completed
  evidence: 'Reopened audit review passed (candidate_tree 9d4c827981c0759af4b28520fd510a50febd0cc4): forwarding.rs 6L clone extracted (extend_visible_members), cross-file Tier-2 inheritance enabled in preparation.rs + tested in tests/linker/receivers/typed.rs (linker 92/92, languages 262/262, acdd-index 101/101, commitment_integrity 9/9).'
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:language-linker-trait-inversion-and-tier2-members.deliver
  gate_id: deliver
  worker_id: ar05-w1-linker-clean-reviewer-pro
  claim_revision: 2
  contract_revision: 1
  candidate_tree: 9d4c827981c0759af4b28520fd510a50febd0cc4
  committed_oid: fe853ed8c312a29e835f78fdbbdd563defbd6eec
  accepted_at: 2026-11-24T16:19:43.270Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY: Zero per-language fields on ImportContext or semantic_context::Context, zero __rust_* string checks or language-specific branches in universal linker modules, one canonical Rust path resolver, and 0 mechanical doc stubs in languages/ or linker/.'
      verified:
      - INV-AR005-PURE-LANGUAGE-LINKER-BOUNDARY
    review_summary: Deliver reopened language-linker-trait-inversion-and-tier2-members candidate_tree 9d4c827981c0759af4b28520fd510a50febd0cc4 after independent review pass.
```

### task: state-usecase-control-flow-and-context-bag-removal

```yaml
task_ref: state-usecase-control-flow-and-context-bag-removal
target: Delete PreparedVerifyPhase, GateCallCtx, PreparedClaimTarget, PreparedRunInputs, PreparedStartGraph, SnapshotTargetContext, SyncBuffers, ReopenAcc, and mechanical doc stubs across acdd-state, replacing with idiomatic ControlFlow and borrowed graph slices while keeping StateEngine::{config, backend, query} intact until Wave 2b
scope:
- crates/acdd-state/src/
- crates/acdd-state/tests/
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
tags:
- default
invariants:
- 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, and 0 mechanical doc stubs.'
acceptance_criteria:
- 'AC-1: All 8 single-caller context/parameter bags in acdd-state are deleted (with StateEngine::{config, backend, query} kept intact for unified Wave 2b removal alongside all 38 state_integration, 8 usecases/tests.rs, and tests/acdd callers), CompositeDomainProvider::new callers in acdd-state tests are migrated to with_module_flags, #[allow(clippy::too_many_arguments)] count in acdd-state drops by >= 80%, mechanical doc stubs in acdd-state are replaced, RunHarness error-code misuse is fixed, and .clone() density in usecases/ and machine/ is measurably reduced with full cargo test -p acdd-state --test state_integration parity.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: refactor-verify-claim-start-usecases
  title: Replace PreparedVerifyPhase + GateCallCtx and claim/start/task_ops bags with ControlFlow and &LoadedRunGraph borrows
  status: completed
  evidence: 'Verified in Round 2 review: PreparedVerifyPhase, GateCallCtx, PreparedClaimTarget, PreparedRunInputs, PreparedStartGraph, and SnapshotTargetContext deleted and replaced with std::ops::ControlFlow, helper returns, and &LoadedRunGraph/&GateDef/&ResolvedGateProfile borrows; _pre_lock unconditionally dropped; clones reduced 805->764.'
- subtask_ref: clean-machine-accumulators-and-doc-stubs
  title: Remove SyncBuffers and ReopenAcc, migrate acdd-state-internal CompositeDomainProvider::new test calls, fix RunHarness error-code misuse, preserve StateEngine::{config, backend, query} until Wave 2b, and replace mechanical doc stubs in acdd-state
  status: completed
  evidence: 'Verified in Round 2 review: SyncBuffers and ReopenAcc deleted; all 4 CompositeDomainProvider::new test calls in crates/acdd-state/ migrated to with_module_flags; RunHarness::on_disk error codes aligned to ExternalGitDrift; StateEngine::{config, backend, query} preserved for Wave 2b per #405; 0 mechanical doc stubs remaining.'
- subtask_ref: clean-machine-accumulators-and-phantom-getters
  title: Remove SyncBuffers, ReopenAcc, StateEngine::{config, backend}, CompositeDomainProvider::new test calls, RunHarness error-code misuse, and mechanical doc stubs in acdd-state
  status: completed
  evidence: 'Verified in Round 2 review: SyncBuffers and ReopenAcc deleted; StateEngine::{config, backend, query} preserved for Wave 2b parity per pre_task_slice/#405; CompositeDomainProvider::new calls migrated to with_module_flags (0 remaining); RunHarness error codes fixed; #[allow(clippy::too_many_arguments)] in src/ reduced 43->8 (-81.4%); 0 mechanical doc stubs.'
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:state-usecase-control-flow-and-context-bag-removal.deliver
  gate_id: deliver
  worker_id: ar05-w1-state-reviewer-r2-pro
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 3eda8b6b61ecc1ca1787becb20f0cf3a453085d4
  committed_oid: 19bd10a835a4fe91be09b5c92a538676e3750308
  accepted_at: 2026-11-24T11:07:58.366Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-STATE-ZERO-BAG-SHIMS: acdd-state contains 0 Option<Result<...>> control-flow fields, 0 single-caller parameter-bag structs, and 0 mechanical doc stubs.'
      verified:
      - INV-AR005-STATE-ZERO-BAG-SHIMS
      - INV-STATE-NO-INDEX
      - INV-STATE-PURE-MACHINE
      - INV-STATE-SQLITE-CONFINED
      - INV-SINGLE-OWNER
      - INV-NO-DEAD-CODE-SHIMS
    review_summary: Deliver state-usecase-control-flow-and-context-bag-removal after Round 2 review pass and subtask reconciliation
```

### task: typed-graph-linker-and-raw-details-cutover

```yaml
task_ref: typed-graph-linker-and-raw-details-cutover
target: 'Execute Wave 2a typed-graph and linker cutover: flip Node.kind (NodeKind with canonical HtmlRequest/HtmlContext + typed request_role metadata), Node.language (LanguageId), and CompactEdge (with fail-closed TryFrom<&PublicEdge> + CompactGraph::try_push_public_edge) across workspace callers and tests; upgrade FlowStore::read_named_uncertainties error and internal __rust_* ValueExpr/TypeExpr representations alongside INDEX_SEMANTICS_VERSION bump; delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims; narrow LanguageLinker/FileRelationContext to pub(crate) while keeping LanguageProfile pub; clear all remaining 235 production raw node.details[key] sites (+ 3 cache test sites) and promote ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to error; and clear semantic/types.rs doc stubs'
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
extended_scope:
- path: tests/incremental
  reason: Update existing codec-version assertions after the required FlowState binary format change.
- path: tests/languages.rs
  reason: 'Replace dead DependencyRegistry::framework_manifest_for_path smoke check per decision #466'
depends_on:
- ast-models-scanner-details-and-dto-shim-cleanup
- language-linker-trait-inversion-and-tier2-members
tags:
- default
invariants:
- 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, HtmlRequestAttributes maps onto NodeKind::HtmlRequest/HtmlContext plus typed request_role metadata while preserving zero-Rust .acdd/frameworks/*.toml overrides, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide.'
acceptance_criteria:
- 'AC-1: Node.kind (36-variant NodeKind including Object), Node.language (LanguageId), and CompactEdge ((34-variant EdgeKind including TemplateImports, EdgeConfidence)) field cutovers compile cleanly across --workspace --all-targets using fail-closed TryFrom<&PublicEdge> for CompactEdge and CompactGraph::try_push_public_edge (updating docs/architecture/indexing.md:146, src/engine/reader.rs:478, tests/receivers/, tests/linker/public_output.rs:5-38 for closed tags + UTF-8 identifiers/evidence + negative TryFrom rejection, cache/tests.rs:123 synthetic scope kind, and linker/contracts.rs:671 LanguageId::Workspace); HtmlRequestAttributes emits NodeKind::HtmlRequest/HtmlContext with typed request_node/context_node role metadata preserving zero-Rust .acdd/frameworks/*.toml overrides (updating docs/reference/languages.md:116-118, tests/languages/html/{syntax,htmx_relations}.rs, and linker/html/relations.rs); FlowStore::read_named_uncertainties returns InvalidNamedUncertaintyCarrier (removing #[allow(clippy::result_unit_err)] in flow_store.rs and updating linker/mod.rs); remaining __rust_* ValueExpr/TypeExpr representations across semantic/types.rs, typed_facts/binary.rs, languages/rust/value_flow/, linker/rust/value_flow.rs, and db/lint/dead_tails.rs are typed and semantic/types.rs doc stubs are replaced; ClassicFieldsFirstDto/ClassicBindingsFirstDto/FlowState::{original,classic_fields_first} are deleted (updating linker/contracts.rs, db/writer/cache/tests.rs, tests/languages/html/classic_wire.rs) and INDEX_SEMANTICS_VERSION bumped; LanguageLinker and FileRelationContext are narrowed to pub(crate) via crate-private dispatch (removing all 3 #[allow(private_interfaces)] escapes and migrating tests/languages/profiles.rs:153-160 while keeping LanguageProfile pub); and all remaining 235 production raw node.details[key] sites across languages/ (78), linker/ (153), and db/ (4) plus 3 #[cfg(test)] sites in db/writer/cache/tests.rs are migrated with ARCH-L2-NO-RAW-NODE-DETAILS-INDEX promoted to severity: error and 0 warnings.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: atomic-node-compact-edge-html-and-linker-privacy-cutover
  title: Flip Node.kind (including HtmlRequest/HtmlContext + typed request_role metadata), Node.language, and CompactEdge (with fail-closed TryFrom<&PublicEdge> + indexing.md/languages.md/public_output.rs/html test updates), upgrade FlowStore::read_named_uncertainties and __rust_* ValueExpr/TypeExpr variants + semantic/types.rs doc stubs, delete ClassicFieldsFirstDto/ClassicBindingsFirstDto shims + bump INDEX_SEMANTICS_VERSION, and narrow LanguageLinker/FileRelationContext to pub(crate)
  status: completed
  evidence: 'Independent review passed (`candidate_tree: 73c7d4209c26c0e203d554b1da92d3fe10704c4e`): NodeKind/LanguageId/CompactEdge fail-closed TryFrom<&PublicEdge>, HtmlRequest/HtmlContext + REQUEST_NODE_ROLE, FlowStore::read_named_uncertainties, ClassicFieldsFirstDto removal + INDEX_SEMANTICS_VERSION 16, pub(crate) LanguageLinker/FileRelationContext (0 allow(private_interfaces)), universal linker #468/#455 isolation + broken module cycle (0 cycles), +56 net prod LOC (<=450), cargo test --workspace --all-targets 914/914, clippy exit 0.'
- subtask_ref: eliminate-remaining-raw-details-and-ratchet-arch-rule
  title: 'Migrate all remaining 235 production raw node.details[key] sites across languages/ (78), linker/ (153), and db/ (4) plus 3 #[cfg(test)] sites in db/writer/cache/tests.rs onto typed accessors and promote ARCH-L2-NO-RAW-NODE-DETAILS-INDEX to severity: error'
  status: completed
  evidence: 'Independent review passed (`candidate_tree: 73c7d4209c26c0e203d554b1da92d3fe10704c4e`): all 235 prod + 3 cache test raw node.details[...] sites migrated to typed accessors, ARCH-L2-NO-RAW-NODE-DETAILS-INDEX promoted to severity: error in docs/architecture/indexing.md:214 with 0 warnings/errors across workspace lint (52 rules, 0 errors), baseline hygiene parity (duplicates 42<=42, dead_tails 140<=144, phantom_tested 19<=19, dangling_calls 0).'
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:typed-graph-linker-and-raw-details-cutover.deliver
  gate_id: deliver
  worker_id: ar05-w2a-reviewer-r2-pro
  claim_revision: 1
  contract_revision: 3
  candidate_tree: 73c7d4209c26c0e203d554b1da92d3fe10704c4e
  committed_oid: 608a1db16e3b0a5aee1cfb6e38195412b2269269
  accepted_at: 2026-11-24T22:25:31.594Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-TYPED-GRAPH-AND-DETAILS: Node and CompactEdge store 1-byte Copy enums across all callers with fail-closed TryFrom<&PublicEdge> tag conversion, HtmlRequestAttributes maps onto NodeKind::HtmlRequest/HtmlContext plus typed request_role metadata while preserving zero-Rust .acdd/frameworks/*.toml overrides, LanguageLinker and FileRelationContext have 0 #[allow(private_interfaces)] escapes while LanguageProfile stays pub, and ARCH-L2-NO-RAW-NODE-DETAILS-INDEX passes at severity: error with 0 warnings workspace-wide.'
      verified:
      - INV-AR005-TYPED-GRAPH-AND-DETAILS
    review_summary: Deliver Wave 2a typed-graph and linker cutover after Round 2 review pass
```

### task: acdd-error-db-dtos-mcp-and-cli-cleanup

```yaml
task_ref: acdd-error-db-dtos-mcp-and-cli-cleanup
target: 'Execute Wave 2b error, DTO, state-surface, MCP, config, CLI, and Cargo cleanup: unify StateError into AcddError + single-pass config/loader.rs without io::Error tunneling; delete StateEngine::{config, backend, query} across all test callers; delete ChunkPersistSink/EdgeOccurrencesInput/TextMatchPreviewParams and migrate db/{reader,symbols,traversal} and src/format/ onto typed DTOs with ImpactDirection/TraversalMode; remove dead ComplexityLimits threshold fields and ComplexityLimitsWire alongside tests/core/{arch_lint,ast_contracts}.rs and docs/reference/configuration.md; decompose src/mcp/server.rs aligning acdd.yaml/docs/architecture/cross-platform.md; relocate WorktreeSpanContext; fix docs_check EPIPE + src/cli/mod.rs:917-927 test contract; delete unused root Cargo.toml deps; and clear all remaining mechanical doc stubs'
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
extended_scope:
- path: Cargo.lock
  reason: Removing petgraph, zstd, and hashbrown from root Cargo.toml updated Cargo.lock
depends_on:
- typed-graph-linker-and-raw-details-cutover
- state-usecase-control-flow-and-context-bag-removal
tags:
- default
invariants:
- 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: root Cargo.toml has 0 unused dependencies, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, ComplexityLimits drops dead threshold fields and rejects deprecated keys strictly via deny_unknown_fields, db readers and formatters use typed DTOs, WorktreeSpanContext lives in acdd_core::telemetry, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
acceptance_criteria:
- 'AC-1: StateEngine::{config, backend, query} are deleted (migrating all 37 .backend() + 1 .config() + .query() calls in crates/acdd-state/tests/state_integration/{usecases_e2e,usecases_queries_lifecycle,roadmap_query_waves}.rs, 8 .backend() calls in crates/acdd-state/src/usecases/tests.rs, and .query() in tests/acdd/verification_and_git.rs); petgraph, zstd, and hashbrown are deleted from root Cargo.toml; AcddError replaces io::Error tunneling across config/loader.rs (single-pass Result<StrictAdapterReport, AcddError>) and all crate callers; ComplexityLimits deletes dead max_cognitive_complexity/max_nesting_depth/max_fanout struct fields and ComplexityLimitsWire (rejecting deprecated and unknown keys strictly via deny_unknown_fields) and updates docs/reference/configuration.md:113 + tests/core/{arch_lint,ast_contracts}.rs; CompositeDomainProvider::new, unused SchemaDef helpers, ParsedAdapterConfig, ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, resolve_effective_state_db, ast_legacy_error, Server::with_domain_providers, and ValidateResp JSON patch are deleted; WorktreeSpanContext moves to acdd_core::telemetry; TraversalOptions/GraphQueryOptions adopt ImpactDirection/TraversalMode and db/{reader,symbols,traversal} + src/format/ use typed DTOs; PipelineTestHooks::delta_failure* constructors are #[cfg(test)]-gated; src/mcp/server.rs is split cleanly with acdd.yaml (ARCH-SUBPROCESS-CONFINED, ARCH-TELEMETRY-CONFINED) and docs/architecture/cross-platform.md links aligned; src/cli/docs_check.rs EPIPE handling returns Ok(()) cleanly on BrokenPipe with src/cli/mod.rs:917-927 contract updated; and 0 template doc stubs remain in CLI --help or workspace source.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: acdd-error-state-getters-and-complexity-limits-cleanup
  title: Generalize AcddError + single-pass config/loader.rs across all callers, delete StateEngine::{config, backend, query} across crates/acdd-state/tests/ and tests/acdd/, delete CompositeDomainProvider::new/SchemaDef/ParsedAdapterConfig/dead ComplexityLimits fields (with backward-compatible YAML deserialization + tests/core + configuration.md updates), relocate WorktreeSpanContext, and delete unused petgraph/zstd/hashbrown from root Cargo.toml
  status: completed
  evidence: 'Round 2 independent review passed (candidate_tree: 2c19dfeacb39e5640473492df11d99505e11533e): AcddError unified across acdd-core/state/host, single-pass config/loader.rs, StateEngine::{config,backend,query} deleted, CompositeDomainProvider::new/SchemaDef/ParsedAdapterConfig/resolve_effective_state_db removed, ComplexityLimits dead fields removed with ComplexityLimitsWire Deserialize, WorktreeSpanContext moved to acdd_core::telemetry + ARCH-TELEMETRY-CONFINED, petgraph/zstd/hashbrown removed from Cargo.toml, #459 polish scope/extend_scope support added & verified across 923/923 tests and clippy exit 0.'
- subtask_ref: typed-db-reader-dtos-and-ingest-bag-removal
  title: Delete ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, resolve_effective_state_db, and ast_legacy_error; adopt ImpactDirection/TraversalMode on TraversalOptions; and replace untyped Value maps in db/{reader,symbols,traversal} and src/format/ with typed DTOs
  status: completed
  evidence: 'Round 2 independent review passed (candidate_tree: 2c19dfeacb39e5640473492df11d99505e11533e): ChunkPersistSink, EdgeOccurrencesInput, TextMatchPreviewParams, ast_legacy_error deleted; ImpactDirection/TraversalMode typed on TraversalOptions/GraphQueryOptions; untyped Value maps in db/{reader,symbols,traversal} & src/format/{core,presentation} replaced with typed DTOs (0 serde_json::Map/.as_object_mut() remaining); render_duplicates/render_dead_tails collapsed and #476 clone eliminated via shorten_hash_dto (duplicates 41/438 <= 42/465, phantom_tested 8 <= 19).'
- subtask_ref: mcp-server-split-cli-epipe-and-remaining-doc-stubs
  title: 'Decompose src/mcp/server.rs with acdd.yaml and docs/architecture/cross-platform.md aligned, #[cfg(test)]-gate PipelineTestHooks::delta_failure* constructors, fix cli/docs_check.rs EPIPE + src/cli/mod.rs:917-927 test contract, remove ValidateResp JSON patch, and replace all remaining mechanical doc stubs in acdd-core, db/, and src/'
  status: completed
  evidence: 'Round 2 independent review passed (candidate_tree: 2c19dfeacb39e5640473492df11d99505e11533e): src/mcp/server.rs split into acyclic src/mcp/server/{mod,types,admission,worktree,session}.rs with acdd.yaml & docs aligned, PipelineTestHooks::delta_failure* #[cfg(test)]-gated, cli/docs_check.rs & cli/mod.rs EPIPE fixed, ValidateResp JSON patch removed, 0 remaining mechanical doc stubs in acdd-core/db/src, mcp 160/160 + query_context 48/48 + commitment_integrity 9/9 + validate 10/10 passed.'
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:acdd-error-db-dtos-mcp-and-cli-cleanup.deliver
  gate_id: deliver
  worker_id: ar05-w2b-reviewer-pro
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 2c19dfeacb39e5640473492df11d99505e11533e
  committed_oid: 50bf90ef9e5a458114e64192548088697579536b
  accepted_at: 2026-11-25T00:04:59.875Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-TYPED-READER-AND-ERROR-SURFACES: root Cargo.toml has 0 unused dependencies, config/loader.rs parses YAML once into Result<StrictAdapterReport, AcddError>, ComplexityLimits preserves backward-compatible YAML deserialization with 0 dead struct fields, db readers and formatters use typed DTOs, WorktreeSpanContext lives in acdd_core::telemetry, and 0 mechanical placeholder doc stubs remain anywhere in the workspace.'
      verified:
      - INV-AR005-TYPED-READER-AND-ERROR-SURFACES
    review_summary: 'Delivered Wave 2b (task:acdd-error-db-dtos-mcp-and-cli-cleanup) after Round 2 review approval (#476 remediated, #459/#472/#475 verified, all 3 subtasks completed, candidate_tree=2c19dfeacb39e5640473492df11d99505e11533e).'
```

### task: submit-verify-env-isolation-and-scope-fixes

```yaml
task_ref: submit-verify-env-isolation-and-scope-fixes
target: 'Wave 2c: Integrate and verify all child-command env isolation, submit/verify post-mutation index_generation preservation, and scope-arg fixes from goal-20261124141330-293224-1 (4bb239694) on top of Wave 2b, close review follow-ups (#436: directory-style scope_args matching in shell_template.rs, remaining Command::new(CARGO_BIN_EXE_acdd) env isolation outside tests/common, and multi-check index_gen mutation state_integration test), and retire the orphan hotfix runs'
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
depends_on:
- acdd-error-db-dtos-mcp-and-cli-cleanup
tags:
- default
invariants:
- 'INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION: Gate child processes and test CLI harnesses scrub ambient ACDD_*/GIT_* variables deterministically, verify propagates post-check index_generation observations while submit validates live observe_freshness fail-closed (TASK_PROOF_UNOBSERVED) without auto-running verify, and directory-style scopes resolve target test filters without falling back or leaking ambient state.'
acceptance_criteria:
- 'AC-1: All 12 files from fix-submit-verify-env-and-scope (4bb239694) are integrated onto AR-005 without regressing Wave 2b (acdd-state/tests + tests/common + tests/acdd + tests/mcp/harness_contract green), shell_template::select_scope_args matches directory-style scope prefixes alongside ** globs, remaining direct CARGO_BIN_EXE_acdd callers in tests/mcp/, tests/acdd/, tests/core/, and tests/languages/ isolate ACDD_*/GIT_* env cleanly, verify propagates post-mutation index_generation while submit enforces fresh traces fail-closed (TASK_PROOF_UNOBSERVED) in state_integration, and no stale orphan hotfix run worktree blocks AR-005 handoff.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: integrate-2c-env-isolation-and-verify-generation
  title: Cherry-pick/reconcile goal-20261124141330-293224-1 (4bb239694) onto AR-005 cleanly across shell_template.rs, command.rs, submit/, verify.rs, and test harnesses
  status: completed
  evidence: Reconciled 4bb239694 cleanly across runner/command.rs, shell_template.rs, usecases/{verify,submit/{mod,freshness}}.rs, tests/{common,acdd,mcp}, and crates/acdd-state/tests/state_integration; Phase 3 StaleClaimRevision guard placed before ensure_fresh_trace; verified all unit & integration tests.
- subtask_ref: close-2c-directory-scopes-and-remaining-harness-callers
  title: Support directory-style scopes in shell_template::select_scope_args (plus acdd-state test mapping in .acdd/profile.yaml), isolate remaining direct CARGO_BIN_EXE_acdd callers, and add multi-check index_generation preservation seam test
  status: completed
  evidence: Added crates/acdd-state/tests/**/*.rs mapping in .acdd/profile.yaml, directory/trailing-slash/glob scope matching in shell_template.rs, isolate_acdd_env across remaining CARGO_BIN_EXE_acdd callers, and multi-check index_generation + unclaimed StaleClaimRevision rows in usecases_e2e.rs.
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:submit-verify-env-isolation-and-scope-fixes.deliver
  gate_id: deliver
  worker_id: ar05-w2c-reviewer-grok
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 1df3b49acbef29847cdb2194712039922af7d6a3
  committed_oid: 252a7189abee800396fec0c342ad905b96246c20
  accepted_at: 2026-11-25T02:45:16.144Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION: Gate child processes and test CLI harnesses scrub ambient ACDD_*/GIT_* variables deterministically, auto-verify during submit propagates refreshed index_generation observations, and directory-style scopes resolve target test filters without falling back or leaking ambient state.'
      verified:
      - INV-AR005-SUBMIT-VERIFY-ENV-ISOLATION
    review_summary: 'Deliver Wave 2c after re-review closed finding #484. Env isolation, Phase 3 claim fencing, directory scope matching, and the multi-check index_generation seam are on candidate 1df3b49.'
```

### task: dead-tails-duplicates-and-call-graph-closure

```yaml
task_ref: dead-tails-duplicates-and-call-graph-closure
target: 'Wave 2d: Fix Rust call-graph / extractor & dead_tails closure gaps (function-value combinator args, serde/schemars callable string attributes, sql::tokens self-shadowed calls, Context::with_flows / NamedBindings propagation) and eliminate remaining genuine dead_tails, phantom_tested helpers, and block/similar duplicates workspace-wide'
scope:
- crates/acdd-core/src/
- crates/acdd-index/src/
- crates/acdd-state/src/
- crates/acdd-state/tests/
- src/
- tests/
- docs/
depends_on:
- submit-verify-env-isolation-and-scope-fixes
tags:
- default
invariants:
- 'INV-AR005-HYGIENE-CLOSURE: Rust call-graph extraction and dead_tails reachability track function-value callbacks, attribute-named Serde/Schemars hooks, and shadowed-local callable calls without false positives, while all remaining genuine dead_tails, phantom_tested helpers, and >=5L code duplicates are collapsed or removed cleanly.'
acceptance_criteria:
- 'AC-1: Rust extractor/linker and db/lint/dead_tails properly attribute calls from function-value arguments (.map/.and_then/map_rows), #[serde(default=...)]/#[schemars(schema_with=...)] attributes, shadowed local calls such as languages/sql/mod.rs::tokens, and Context::with_flows/NamedBindings chains with dedicated tests in tests/languages.rs and tests/core.rs.'
- 'AC-2: Remaining genuine dead_tails, phantom_tested items (such as dead/test-only helpers in acdd-core, acdd-index, acdd-state, and src/), and block/similar duplicates across acdd-index, acdd-state, and src/ (including admit_reclaim_in_progress, evaluate_verify, hydrate_and_link_delta, present_value_bounded, trim_tasks_page_tail, compute_merkle_root, coverage_dictionary) are deleted, #[cfg(test)]-gated, or collapsed into shared helpers with 0 lint errors and dramatically reduced duplicates/dead_tails warnings.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: fix-rust-call-graph-and-dead-tails-closure-gaps
  title: Trace and fix Rust extractor/linker/dead_tails gaps for function-value combinator callbacks, serde/schemars string attributes, shadowed-local calls (sql::tokens), and Context::with_flows/NamedBindings chains with seam tests
  status: completed
  evidence: Fixed Rust call graph & dead_tails false-positive closure gaps (combinator fn-value calls, schemars/serde schema_with/default attrs, macro token trees, ValueExpr::Call/Project callbacks, -> Self builder chains, source-pos lexical shadowing, private mod vs restricted pub(crate) use forwarding + step_rust_module_member); verified with clippy (acdd-index -D warnings), core arch_lint 8/8, languages 258/258, linker 89/89, and cold target/debug/acdd build.
- subtask_ref: sweep-genuine-dead-tails-phantom-tested-and-duplicates
  title: 'Remove or #[cfg(test)]-gate remaining genuine dead_tails/phantom_tested items and collapse remaining pre-existing duplicates across acdd-core, acdd-index, acdd-state, and src/'
  status: completed
  evidence: 'Remediated #491: restored AR-005 milestone spec from HEAD; collapsed all 5 >=5L duplicates (compute_merkle_root via update_sorted_pairs, python non_comment_named_children<''c,''a>, domain_leaves row_i64_quad, acdd-state reader col* helpers + query_opt_decoded) to duplicates --min-lines 5 = 0; removed SyntaxActivationKeys::matches_attribute and DependencyRegistry::standard_manifests_digest. Net prod LOC +204 (<=450). ./target/debug/acdd lint --categories all: 0 errors / 23 warnings (arch 0, duplicates 0, cycles 0, dead_tails 23 [13 integration-test phantom_tested + live/cfg stubs]). Clippy workspace -D warnings exit 0, 9 test suites 777/777 passed, TMP_BEFORE=5488 == TMP_AFTER=5488.'
receipt:
  run_id: AR-005-typed-graph-c-20261124040532-2665544-1
  node_cursor: root:dead-tails-duplicates-and-call-graph-closure.deliver
  gate_id: deliver
  worker_id: ar05-coord
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 92e5079336880a8af6eddf5d958cc7ba8657445c
  committed_oid: 03576033f38fbed60ee4b79de1de1415bc06ef0c
  accepted_at: 2026-11-25T06:38:54.725Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR005-HYGIENE-CLOSURE: Rust call-graph extraction and dead_tails reachability track function-value callbacks, attribute-named Serde/Schemars hooks, and shadowed-local callable calls without false positives, while all remaining genuine dead_tails, phantom_tested helpers, and >=5L code duplicates are collapsed or removed cleanly.'
      verified: []
```

---

## Final Polish & Amendment Notes

- **Strict `ComplexityLimits` schema (`crates/acdd-core/src/lint.rs`)**: Removed `ComplexityLimitsWire` so `ComplexityLimits` derives `Deserialize` directly with `#[serde(default, deny_unknown_fields)]`, rejecting deprecated threshold keys (`max_cognitive_complexity`, `max_nesting_depth`, `max_fanout`) strictly alongside updates to `docs/reference/configuration.md` and `tests/core/ast_contracts.rs`.
- **Typed reader-eviction guard (`crates/acdd-index/src/db/cache.rs`, `src/engine/pipeline/delta.rs`)**: Replaced `Box<dyn std::any::Any>` in `DeltaAdmission::idle_readers_guard` with typed `cache::IdleReadersGuard`.
- **Workspace architecture lint rules (`docs/architecture/modularity.md`)**: Added `ARCH-NO-ANY-DOWNCAST` (`std::any`) and `ARCH-NO-PRIVATE-INTERFACES-ESCAPE*` (`#[allow(private_interfaces)]`, `#![allow(private_interfaces)]`, `#[expect(private_interfaces)]`) rules at `severity: error`.
- **Profile test routing & worker startup (`.acdd/profile.yaml`)**: Added `by_pattern` `rust-check-and-test` entries for `crates/acdd-index/src/languages/**` and `crates/acdd-index/src/linker/**` running both `acdd-index --lib` checks/tests and `--test languages --test linker {test_filter}` (leaving `scope_args_by_pattern` cleanly intact, plus `crates/acdd-state/tests/**/*.rs` in Wave 2c), and documented the 2-step worker startup (`acdd_claim` / `acdd claim` first for `ClaimResp.worktree`, then `acdd --root <ClaimResp.worktree> build` or MCP `worktree` before reading/querying code).
- **Wave 2c submit/verify separation (`crates/acdd-state/src/usecases/{submit/,verify.rs}`)**: Removed auto-verify fallback from `submit` (`ensure_fresh_trace` now fails closed with `ErrorCode::TaskProofUnobserved` when no fresh passing trace exists), moved `observe_freshness` in `verify::run_gate_checks` to run after all checks complete so post-mutation `index_generation` binds into `execution_digest`, and scrubbed `ACDD_DB` and `ACDD_EXTRACTION_CACHE_DIR` alongside `ACDD_*`/`GIT_*` in `sanitize_command_process`.
- **Wave 2d hygiene & dead-method removals**: Deleted unused `SyntaxActivationKeys::matches_attribute` (`languages/manifests/activation.rs`), `DependencyRegistry::standard_manifests_digest` (`languages/dependency_registry/framework_packs.rs`), and `PipelineTestHooks::overlap` (`src/engine/pipeline/hooks.rs`), consolidated `split_top_level_args` onto `split_top_level_delimited` and `acdd-state` SQLite row preambles onto `col*` helpers, and closed `duplicates --min-lines 5` to `0` groups.
- **Pre-handoff status**: `status` remains `planned` until terminal `handoff` archives the milestone with `closure:`.
