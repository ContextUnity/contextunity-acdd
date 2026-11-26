---
id: LS-004-rust-entrypoints-and-inline-module-exports
title: Language-agnostic local-mutation resolution fix, per-language manifest folders, declarative Rust entrypoints (rmcp/axum/actix, cross-file clap), and inline module exports
doc_type: contract
status: completed
owners:
- crates/acdd-index/build_manifests.rs
- crates/acdd-index/src/ast/visitor.rs
- crates/acdd-index/src/models/details.rs
- crates/acdd-index/src/languages/rust/
- crates/acdd-index/src/languages/manifests/
- crates/acdd-index/src/linker/
- crates/acdd-index/src/db/lint/unused_public_api.rs
- crates/acdd-index/src/db/reader/
- src/format/
- tests/languages/
- tests/linker/
- tests/query_context/
- tests/mcp/context/entrypoints_and_delta.rs
- ROADMAP.md
- CHANGELOG.md
- docs/milestones/LS-004-rust-entrypoints-and-inline-module-exports.md
invariants:
- 'INV-LOCAL-MUTATES-NOT-UNRESOLVED-CALL: Local variable and parameter EdgeKind::Mutates references in crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs are handled distinctly from callable Calls/Handles across ALL languages so mutating a local binding never emits false unresolved shadowed-callee diagnostics (preserving true shadowed callable diagnostics and real cross-symbol Mutates edges).'
- 'INV-MANIFESTS-BY-LANGUAGE: Built-in framework manifests under crates/acdd-index/src/languages/manifests/ are organized into per-language subdirectories (rust/, python/, typescript/, web/) discovered deterministically by build_manifests.rs with zero hardcoded framework package names inside language parsers.'
- 'INV-PRODUCER-OWNED-ENTRYPOINTS: Declarative Rust framework packs (rmcp.toml, axum.toml, actix.toml) and cross-file clap command_dispatch project through canonical extractor/linker facts into persisted nodes.details.entrypoint (mcp_tool, http_route, cli_command) via db/writer/entrypoints.rs with zero downstream string heuristics.'
- 'INV-SHADOW-SAFE-FRAMEWORK-PROJECTION: Package-gated activation and shadow checks prevent non-rmcp/non-axum/non-actix/non-clap shadows from emitting entrypoint projections, and incremental delta re-indexing clears stale categories when attributes or imports are removed.'
- 'INV-INLINE-MOD-EXPORTS-SAFE: Scoped inline mod_item bodies publish visibility-aware EXPORTS facts cleanly without regressing existing db/lint/unused_public_api.rs liveness lint behavior.'
acceptance_criteria:
- 'AC-01: GIVEN multi-language sources (Rust, Python, TypeScript) mutating local variables or parameters alongside true shadowed callable calls and real static/field mutations, WHEN linked by crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs, THEN local-binding EdgeKind::Mutates no longer emit false unresolved shadowed-callee coverage rows, true shadowed calls still report unresolved, real cross-symbol Mutates edges remain intact, and code_map_overview reports production vs test resolution health cleanly.'
- 'AC-02: GIVEN built-in manifests reorganized into crates/acdd-index/src/languages/manifests/{rust,python,typescript,web}/, WHEN build_manifests.rs compiles and tests/languages/ runs, THEN all 27+ packs load deterministically in sorted order with zero activation regressions.'
- 'AC-03: GIVEN declarative Rust manifests (manifests/rust/{rmcp,axum,actix}.toml) backed by generic attribute/route syntax operations, WHEN Rust files importing rmcp/axum/actix are indexed (cold and delta), THEN db/writer/entrypoints.rs persists nodes.details.entrypoint = "mcp_tool" | "http_route" while ignoring shadowed attributes or inactive packages.'
- 'AC-04: GIVEN a multi-file Rust clap CLI (struct Cli #[derive(Parser)] in args.rs, Cli::parse() dispatch in mod.rs), WHEN indexed (cold and delta), THEN cross-file linker resolution emits exact cli_command_registration -> handles edges projecting nodes.details.entrypoint = "cli_command", and removing #[derive(Parser)] or shadowing clap clears the projection on delta.'
- 'AC-05: GIVEN Rust files containing public and private inline mod blocks with named and wildcard pub use re-exports, WHEN indexed, THEN scoped ModuleDeclaration nodes publish accurate EXPORTS facts reachable from crate roots while existing unused_public_api lint suites pass with zero regressions.'
guidance:
  hitl: pro
required_skills:
- acdd
- contract-planning
- code-quality
- test-suite-rules
closure:
  status: completed
  closed_at: 2026-11-26T20:26:45.189Z
  target_base: main
  target_base_commit: 0ec6422a4e167acfdc5cb8d86101c839e8123433
  task_receipts:
    crossfile-clap-and-inline-mod-exports:
      run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
      node_cursor: root:crossfile-clap-and-inline-mod-exports.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ls004-t3-2
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 51c20611de3b184c2e105c992344f215c9cd9ac3
      committed_oid: f5293dff8ba815a360a1de3e34dce6e143c8c2f4
      accepted_at: 2026-11-26T20:02:50.387Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    organize-manifests-by-language-and-rust-packs:
      run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
      node_cursor: root:organize-manifests-by-language-and-rust-packs.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ls004-t2-5
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 5cdffc5ebf0246e342b912a143bfec46cf91eb6c
      committed_oid: d8b30cf796cfa47ce1af0d58e27ef9f40bc04bde
      accepted_at: 2026-11-26T19:05:32.882Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    universal-local-mutates-and-overview-health:
      run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
      node_cursor: root:universal-local-mutates-and-overview-health.deliver
      gate_id: deliver
      worker_id: codex-sol-reviewer-ls004-t1-2
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 8a7d31125b69abbcc2c6cb9bb76a1053965ec382
      committed_oid: 0c88580409fba6ae793d0459d6128ffc73c7feea
      accepted_at: 2026-11-26T18:10:25.633Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    decisions:
    - 'LS-004 architectural blueprint decision: (1) Wave 0 runs two strictly disjoint tasks: universal-local-mutates-and-overview-health isolates local-binding EdgeKind::Mutates from callable Calls/Handles in linker/resolution/value_flow_phase/dispatch.rs (zero language branches) and adds prod vs test resolution health splits in db/reader/search.rs (reusing db/cycles.rs::is_test_context_path, no schema bump) + src/format/discovery.rs; organize-manifests-by-language-and-rust-packs moves the 27 flat manifests into manifests/{rust,python,typescript,web}/ via recursive deterministic build_manifests.rs discovery and adds generic Rust attribute/route SyntaxOperation variants + manifests/rust/{rmcp,axum,actix}.toml evaluated in languages/rust/ into canonical ToolRegistration/Route edges consumed by db/writer/entrypoints.rs. Note: builder for organize-manifests-by-language-and-rust-packs should autonomously extend scope via acdd extend-scope to update direct manifest-path fixtures in tests/framework_manifests/ and crates/acdd-index/src/languages/dependency_registry/tests.rs (both disjoint from Wave 0 sibling). (2) Wave 1 (crossfile-clap-and-inline-mod-exports) persists declarative CommandDispatch metadata from languages/rust/ for cross-file resolution in linker/framework_relations.rs -> exact Handles edges into db/writer/entrypoints.rs, and publishes visibility-aware EXPORTS on scoped inline ModuleDeclaration nodes in ast/visitor.rs + languages/rust/mod.rs while preserving unused_public_api lint. All protected_paths (Cargo.toml, crates/*/Cargo.toml, .acdd/profile.yaml) and SQLite schema version 15 remain untouched; no new ADR required (conforms to ADR 0009, 0010, 0011).'
    - 'Requesting BUDGET_OVERRIDE approval for organize-manifests-by-language-and-rust-packs (max_net_loc: 350). Measured net LOC: +846 production (+16 build_manifests.rs, +110 manifests/syntax.rs, +21 manifests/rust/{rmcp,axum,actix}.toml, +7 rust/mod.rs, +692 rust/registrations.rs) and +152 tests (+44 activation.rs, +15 user_manifests.rs, +93 entrypoints_and_delta.rs), total +998 net LOC (27 manifest git-mv renames = 0 net LOC). Reason: implementing two generic, data-driven Rust SyntaxOperation variants (AttributeRegistration + RouteBuilderCall) with strict manifest validation, shadow-safe Rust import/local-mod/type/value/macro scope resolution, AST-level attribute argument parsing, builder chain & local binding/param provenance, chained Axum/Actix handler combinator extraction, and comprehensive activation + cold/delta/shadow seam tests cleanly without line-packing or hardcoded crate names genuinely requires ~850 prod + ~150 test LOC. Zero Clippy warnings, zero duplicates, zero dead_tails, zero lint violations. Holding acdd_submit pending coordinator/architect BUDGET_OVERRIDE approval.'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"organize-manifests-by-language-and-rust-packs","escalation_ref":538,"approved_max_net_loc":900,"approved_total_net_loc":1050,"measured_prod_net_loc":846,"measured_test_net_loc":152,"rationale":"Independent coordinator audit of manifests/syntax.rs (+110), build_manifests.rs (+16), manifests/rust/{rmcp,axum,actix}.toml (+21), rust/mod.rs (+7), and rust/registrations.rs (+692) confirmed zero duplicates, zero dead_tails, zero lint errors, clean reuse of existing Rust helpers (collect_use, for_each_preceding_attribute, unquote_rust_string, has_cfg_test, relations::reference), and strictly declarative framework operations. Original 350 LOC budget underestimated shadow-safe namespace-aware AttributeRegistration + RouteBuilderCall evaluation plus SyntaxOperation schema/validation."}'
    - 'LS-004 rearchitect seam decision resolving Finding #541 (architectural_drift) for organize-manifests-by-language-and-rust-packs: (1) Rust lexical scope & receiver provenance (languages/rust/): eliminate the flat file-root-only import table and the per-route unscoped function-body rescan in registrations.rs. Resolve imports, local modules, macros, types, and values strictly along the occurrence''s enclosing lexical scope chain — each inline mod_item starts an isolated module namespace (never implicitly inheriting parent-module imports), while nested blocks/functions inherit from their enclosing module/blocks with inner declarations/bindings shadowing outer ones and expiring on block exit. Derive RouteBuilderCall builder receiver provenance (Router/App/Scope/ServiceConfig) and local value shadowing (get/post/etc.) from canonical ordered lexical/value-flow binding lifetimes (languages/rust/value_flow/collector.rs / ValueFlowCollectorCore or single-pass block-scoped binding frames honoring parameters, ordered let declarations/shadowing, reassignments, and block-exit restoration) without rescanning the containing function on each call. (2) Two-tier rule activation (languages/rust/ + languages/context.rs): gate both AttributeRegistration and RouteBuilderCall through canonical LanguageContext::is_syntax_rule_active(rule, path, &facts.activation_signals) (ADR 0010 / ADR 0011) at extraction time (recording manifest-declared syntax activation keys into facts.activation_signals when rules.has_syntax_gated_rules() is set), ensuring deferred file_extension / syntax_attribute / syntax_attribute_prefix conditions fail closed on both Cold and IncrementalDelta builds. (3) Fail-closed root symlink check (build_manifests.rs): validate the discovery root via non-following symlink_metadata before read_dir traversal (rejecting symlinked roots and non-directories) while preserving deterministic sorting, child symlink rejection, duplicate-identity checks, and per-directory cargo:rerun-if-changed. (4) Seam proofs: extend tests/mcp/context/entrypoints_and_delta.rs (cold + delta) with nested inline-mod import/shadow cases, local builder/handler shadow cases, outer-binding restoration after inner block exit, and non-matching file_extension negative cases; extend tests/framework_manifests/activation.rs with the symlinked discovery root proof. Scope unchanged (all edits stay inside admitted + Decision #533 extended scope, disjoint from Wave 0 sibling, zero protected_paths edits) and remain within approved BUDGET_OVERRIDE #539 (<=900 prod / <=1050 total net LOC).'
    - 'LS-004 Round 2 rearchitect seam decision resolving Finding #548 (architectural_drift) for organize-manifests-by-language-and-rust-packs: (1) Rust branch scoping & explicit root paths (languages/rust/registrations.rs): confine conditional pattern bindings (if let / while let / let chains) strictly to the condition + consequence branch so else (alternative) branches execute outside that binding frame without leaking shadows; resolve explicit leading :: crate-root paths directly against expected_modules without applying local-module crate_shadowed rejection (preserving crate_shadowed for relative/imported paths). (2) Declarative builder factories, chain preservation & local handler shadows (manifests/syntax.rs, manifests/rust/{axum,actix}.toml, languages/rust/registrations.rs): stop propagating builder provenance across arbitrary unknown member calls; represent builder-returning factory functions (e.g., actix_web::web::scope) and builder-preserving fluent chain methods declaratively on SyntaxOperation::RouteBuilderCall (failing closed on undeclared extension methods), and fail closed when an unqualified route handler identifier resolves to a local value binding/closure in the call site''s lexical scope. (3) Overlapping SyntaxAttributePrefix signals (languages/rust/registrations.rs): whenever an observed Rust attribute path matches any configured exact attribute or prefix key in LanguageContext::syntax_activation_keys(), record the observed attribute spelling itself in FileActivationSignals.syntax_attributes (never short-circuiting to only the first matching prefix key) so canonical ActivationCondition::matches_file independently evaluates all exact and overlapping prefix rules. (4) Unbounded rule provenance (languages/rust/registrations.rs): replace the fixed 64-rule u64 bitmask with an unbounded rule-index set (e.g., compact sorted Vec<usize>) evaluated only over file-active rules after LanguageContext::is_syntax_rule_active so inactive file-gated rules never consume slots and arbitrary active rule counts preserve stable manifest precedence with zero silent drops. (5) Proofs & formatting: extend tests/mcp/context/entrypoints_and_delta.rs (cold + delta) with if-let/else restoration, explicit :: root path with local mod shadow, foreign extension method non-builder negative, local closure handler shadow negative, declarative actix_web::web::scope positive, and >64 inactive txt rules preceding an active rs rule; extend tests/framework_manifests/activation.rs with overlapping prefix + exact + non-matching controls; run cargo fmt before submit. (6) Updated BUDGET_OVERRIDE ceiling (superseding #539): approving up to 950 net production LOC and 1250 total net LOC (prod + tests) solely to absorb the declarative factory/chain fields, unbounded provenance set, and Round 2 cold/delta/activation regression matrix while keeping registrations.rs cohesive with zero duplicates, dead_tails, or lint errors.'
    - 'Requesting BUDGET_OVERRIDE adjustment for rustfmt-formatted Round 2 remediation of organize-manifests-by-language-and-rust-packs (Decision #549 approved <=950 prod / <=1250 total). After running rustfmt --edition 2021 per Decision #549(5) and simplifying registrations.rs (unified PendingReg emitter + bitflag locals + shared valid_rust_mod helper in syntax.rs), measured net LOC is +1172 production (+26 build_manifests.rs, +134 manifests/syntax.rs, +21 manifests/rust/{rmcp,axum,actix}.toml, +7 rust/mod.rs, +984 rust/registrations.rs) and +292 tests (+136 activation.rs, +15 user_manifests.rs, +141 entrypoints_and_delta.rs), total +1464 net LOC. The delta above 950/1250 is purely rustfmt 100-col wrapping (~+320 lines across registrations.rs, syntax.rs, and test matrices) rather than added complexity; packing lines to duck rustfmt would violate formatting and code-quality standards. Requesting ceiling adjustment to <=1200 prod / <=1500 total net LOC before calling acdd_submit.'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"organize-manifests-by-language-and-rust-packs","supersedes_budget_in":549,"resolves_blocker":550,"approved_max_net_loc":1200,"approved_total_net_loc":1500,"measured_prod_net_loc":1172,"measured_test_net_loc":292,"rationale":"Independent coordinator audit confirmed Decision #549''s rustfmt --edition 2021 formatting requirement expanded registrations.rs by ~320 wrapped lines alongside declarative factory/chain fields, branch scoping (if let/while let), overlapping prefix activation, and unbounded rule-index tracking. Verified exact diff (+1,172 prod / +292 tests = +1,464 total), 0 duplicates, 0 dead_tails, 0 lint errors/warnings across 60 rules."}'
    - 'Requesting BUDGET_OVERRIDE approval for crossfile-clap-and-inline-mod-exports (max_net_loc: 450). Measured net LOC: +776 production (+10 ast/visitor.rs, +4 models/details.rs, +392 languages/rust/cli_registration.rs, +22 languages/rust/mod.rs, +36 languages/rust/registrations.rs, +10 linker/framework_relations.rs, +10 linker/indexing.rs, +163 linker/rust/cli_dispatch.rs, +64 linker/rust/forwarding.rs, +46 linker/rust/linker_impl.rs, +1 linker/rust/mod.rs, -2 linker/rust/profile_linker.rs, +0 linker/rust/semantic_context.rs, +20 linker/semantic_context/type_resolution.rs), +282 tests (+115 tests/mcp/context/entrypoints_and_delta.rs, +115 tests/languages/profiles/forwarding/mod.rs, +52 tests/linker/method_semantics.rs), and +13 docs (+13 CHANGELOG.md, +0 ROADMAP.md), total +1071 net LOC. Reason: combining two substantial cross-cutting subsystems in Wave 1 — (1) replacing single-file thread-local clap matching with shadow-safe, zero-hardcoded-names persisted declarative CommandDispatch metadata (single-pass AST body scan + scope-frame reuse from registrations.rs) and cross-file linker composition (cli_dispatch.rs checking active rule, Parser/Subcommand metadata, inherent parse override via RustMembers, enum variants, and callable handlers), plus (2) scoped inline Rust ModuleDeclaration EXPORTS publication and module-ID-keyed RustForwarding/NamespaceIndexes/type_resolution traversal preserving unused_public_api lint — together with comprehensive 9-transition cold/delta output_root parity, inline mod forwarding + unused_public_api, and linker seam tests cleanly without line-packing genuinely requires ~776 prod + ~282 test + ~13 doc LOC. Verified: acdd verify crossfile-clap-and-inline-mod-exports.build passed (exit_code=0, code.seam + code.lint passed, execution_digest ca9fab75721134ce1b3e5c9a6606314975eff2ed3954d89d658e94333f796a04, /tmp/contextunity-test-* BEFORE=15 AFTER=15), zero Clippy warnings, zero duplicates, zero introduced dead_tails, zero lint errors, both subtasks marked completed. Holding acdd_submit pending coordinator/architect BUDGET_OVERRIDE approval.'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"crossfile-clap-and-inline-mod-exports","approved_max_net_loc":820,"approved_total_net_loc":1150,"measured_prod_net_loc":776,"measured_test_net_loc":282,"measured_docs_net_loc":13,"rationale":"Independent coordinator audit of crossfile-clap-and-inline-mod-exports (+392 cli_registration.rs, +163 linker/rust/cli_dispatch.rs, +64 forwarding.rs, +46 linker_impl.rs, +111 wiring across rust/linker/ast/models) confirmed 0 duplicates, 0 dead_tails, 0 lint errors/warnings, zero hardcoded framework names, zero schema changes, and all scoped suites passing (11/11 entrypoints_and_delta, 6/6 forwarding, method_semantics, dead_tails_unused_public_api). Original 450 LOC budget underestimated cross-file clap dispatch resolution plus inline mod relative export/re-export forwarding."}'
    - 'Requesting updated BUDGET_OVERRIDE (updating Decision #565) for crossfile-clap-and-inline-mod-exports to close Finding #566. Running `rustfmt --edition 2021 --config skip_children=true crates/acdd-index/src/languages/rust/mod.rs` (zero logic changes; all 17 touched .rs files now pass `rustfmt --edition 2021 --config skip_children=true --check`) wrapped pre-existing long lines in `crates/acdd-index/src/languages/rust/mod.rs` (+120 net formatting lines: from +36/-14 to +215/-73). Updated measured net LOC: +896 production (vs Decision #565 cap 820), +282 tests, +13 docs = +1191 total (vs Decision #565 cap 1150). Verified green after formatting: `acdd verify` claim_revision=2 passed (exit_code=0, code.seam + code.lint passed, execution_digest=ab76d8fb1b82ee5ac79ce3d6a31147aad562974efebe9f6988c3af89c5e9aa5b, /tmp/contextunity-test-* BEFORE=15 AFTER=15), both subtasks completed. Holding `acdd submit` pending updated coordinator BUDGET_OVERRIDE approval (~920 prod / ~1220 total).'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"crossfile-clap-and-inline-mod-exports","supersedes_decision":565,"approved_max_net_loc":920,"approved_total_net_loc":1220,"measured_prod_net_loc":896,"measured_total_net_loc":1191,"rationale":"Independent coordinator audit confirmed +120 net formatting lines came purely from running rustfmt --edition 2021 --config skip_children=true on pre-existing long lines in crates/acdd-index/src/languages/rust/mod.rs to close review finding #566 with zero logic changes. All 17 touched .rs files now pass rustfmt --check and acdd verify passed."}'
  metrics_summary:
    decisions_count: 11
    deferred_findings_count: 0
    failed_traces: 0
    passed_traces: 15
    task_receipts_count: 3
    tasks_cancelled: 0
    tasks_completed: 3
    tasks_total: 3
    total_claims: 28
    total_review_cycles: 3
    total_traces: 15
---

# LS-004: Language-agnostic local-mutation resolution fix, per-language manifest folders, declarative Rust entrypoints (`rmcp`/`axum`/`actix`, cross-file `clap`), and inline module exports

## Goal & Non-goals

**Goal.** Fix a high-impact language-agnostic resolution accounting defect (~16K false `unresolved` `shadowed_binding` rows caused by local-variable `EdgeKind::Mutates` in `dispatch.rs`, lifting workspace Resolution Health from ~66% to ~79%+ and prod to ~85%+), organize built-in framework manifests cleanly by language, and close three concrete Rust semantic extraction gaps in `crates/acdd-index`:
1. **Language-agnostic local-binding `EdgeKind::Mutates` resolution (`dispatch.rs`) + `prod`/`tests` health breakdown (`code_map_overview`):** Stop counting local variable/parameter mutations as unresolved shadowed callees across all languages while preserving real cross-symbol `Mutates` edges and true shadowed-call diagnostics.
2. **Per-language manifest folders:** Reorganize the 27 flat `crates/acdd-index/src/languages/manifests/*.toml` files into `rust/`, `python/`, `typescript/`, `web/` with deterministic recursive discovery in `build_manifests.rs`.
3. **Declarative Rust entrypoints:** Add generic Rust attribute/route syntax operations with standard packs (`manifests/rust/{rmcp,axum,actix}.toml`) and extend declarative `command_dispatch` (`manifests/rust/clap.toml`) across split files (`args.rs` + `mod.rs`) on cold and delta builds.
4. **Inline module exports:** Publish visibility-aware `EXPORTS` facts on inline `mod foo { ... }` declarations (`pub use` named and wildcard re-exports) without regressing existing `unused_public_api` lint behavior.

**Non-goals.**
- Speculative `dyn Trait` implementation fan-out (honest `trait_object_dispatch` analysis gaps remain preserved).
- Adding new MCP tools or modifying `code_map_analyze` presentation (already handled by [UX-004](../UX-004-mcp-tool-surface-consolidation.md)).
- Adding framework packs for Tier-2 languages (Go/Java/C#/Kotlin/PHP/Ruby).

## Tasks in this milestone

### task: universal-local-mutates-and-overview-health

```yaml
task_ref: universal-local-mutates-and-overview-health
target: Fix language-agnostic local-binding EdgeKind::Mutates handling in linker/resolution/value_flow_phase/dispatch.rs so local mutations do not emit false unresolved shadowed-callee rows across any language, and expose prod vs test resolution health breakdown in code_map_overview
scope:
- crates/acdd-index/src/linker/resolution/
- crates/acdd-index/src/db/reader/
- src/format/
- tests/linker/
- tests/query_context/
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: separate-local-mutates-from-shadowed-calls
  title: Handle local variable/parameter EdgeKind::Mutates cleanly in dispatch.rs across all languages while preserving real static/field Mutates edges and true shadowed Calls/Handles unresolved diagnostics
  status: completed
  evidence: Independent review pass, claim revision 2, tree 8a7d31125b69abbcc2c6cb9bb76a1053965ec382. cargo test --test linker local_binding_mutates_stay_distinct_from_shadowed_calls_across_languages exited 0 (1 passed). Rust, Python and TypeScript prove local Mutates does not create false shadowed-callee rows, true shadowed calls still report unresolved, and real field Mutates edges remain.
- subtask_ref: prod-vs-tests-resolution-health-overview
  title: Add prod vs tests resolution health breakdown in code_map_overview and verify multi-language linker/overview parity in tests/linker/ and tests/query_context/
  status: completed
  evidence: 'Independent review pass, claim revision 2, tree 8a7d31125b69abbcc2c6cb9bb76a1053965ec382. cargo test --test query_context overview_ exited 0 (2 passed). Candidate CLI JSON confirms exact Prod/Tests count and denominator sums and schema version 15; the zero-denominator seam reports unavailable. Live Markdown verifies #540 closure with workspace totals and diagnostic reasons outside the partition list.'
receipt:
  run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
  node_cursor: root:universal-local-mutates-and-overview-health.deliver
  gate_id: deliver
  worker_id: codex-sol-reviewer-ls004-t1-2
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 8a7d31125b69abbcc2c6cb9bb76a1053965ec382
  committed_oid: 0c88580409fba6ae793d0459d6128ffc73c7feea
  accepted_at: 2026-11-26T18:10:25.633Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `0` (concurrent with `organize-manifests-by-language-and-rust-packs`, strictly disjoint scopes) | **Budget**: `~260` net LOC.
- **Seam & mutation proof (`AC-01`)**: `tests/linker/` and `tests/query_context/` prove local `Mutates` across Rust/Python/TypeScript no longer inflate `unresolved` `shadowed_binding`, true shadowed callable calls still report `unresolved`, real cross-symbol `Mutates` edges remain, and `code_map_overview` distinguishes prod vs test health.

### task: organize-manifests-by-language-and-rust-packs

```yaml
task_ref: organize-manifests-by-language-and-rust-packs
target: Reorganize built-in framework manifests into per-language subdirectories (rust/, python/, typescript/, web/) with deterministic build_manifests.rs discovery, add generic Rust attribute/route syntax operations, and ship manifests/rust/{rmcp,axum,actix}.toml
scope:
- crates/acdd-index/build_manifests.rs
- crates/acdd-index/src/languages/manifests/
- crates/acdd-index/src/languages/rust/
- tests/languages/
- tests/mcp/context/entrypoints_and_delta.rs
extended_scope:
- path: tests/framework_manifests
  reason: 'Update direct manifest-path fixtures and recursive inventory check for per-language manifest subfolders (per Decision #533)'
- path: crates/acdd-index/src/languages/dependency_registry/tests.rs
  reason: 'Update direct manifest-path fixtures and recursive inventory check for per-language manifest subfolders (per Decision #533)'
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: manifests-language-subfolders
  title: Move flat manifests/*.toml into manifests/{rust,python,typescript,web}/ and update build_manifests.rs for deterministic recursive discovery with full manifest-activation test parity
  status: completed
  evidence: '27 manifests moved into manifests/{rust,python,typescript,web}/; recursive symlink-safe sorted discovery verified via manifests::activation:: and acdd verify (execution_digest=105accdc41aa68b94a0320fdd41571e88b6e05145ef195de1bf3df4ba6d5f669).'
- subtask_ref: declarative-rust-rmcp-axum-actix-packs
  title: Wire generic declarative Rust attribute/route registration operations and add package-gated, shadow-safe manifests/rust/{rmcp,axum,actix}.toml packs verified on cold and delta builds
  status: completed
  evidence: 'AttributeRegistration + RouteBuilderCall with builder_constructors/builder_factories/chain_methods, lexical scoping, two-tier activation, unbounded rule provenance, and manifests/rust/{rmcp,axum,actix}.toml verified via manifests::activation:: (4/4), entrypoints_and_delta:: (10/10, incl. Router::foreign() negative + Router::default() positive), and acdd verify (execution_digest=52e1a36fd58cab0e76ca0d87e34a825b986f0238283142fb6a957034d6a0b6f8). BUDGET_OVERRIDE #551.'
receipt:
  run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
  node_cursor: root:organize-manifests-by-language-and-rust-packs.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ls004-t2-5
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 5cdffc5ebf0246e342b912a143bfec46cf91eb6c
  committed_oid: d8b30cf796cfa47ce1af0d58e27ef9f40bc04bde
  accepted_at: 2026-11-26T19:05:32.882Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `0` (concurrent with `universal-local-mutates-and-overview-health`, strictly disjoint scopes) | **Budget**: `~350` net LOC.
- **Seam & mutation proof (`AC-02`, `AC-03`)**: `tests/languages/` and `tests/mcp/context/entrypoints_and_delta.rs` prove deterministic loading across language subfolders, `rmcp`/`axum`/`actix` `mcp_tool` & `http_route` persistence, package activation gating, and negative shadow rejection.

### task: crossfile-clap-and-inline-mod-exports

```yaml
task_ref: crossfile-clap-and-inline-mod-exports
target: Extend declarative clap command_dispatch across split Rust files via linker framework relations and publish visibility-aware EXPORTS facts on scoped inline Rust ModuleDeclaration nodes while preserving unused_public_api lint
scope:
- crates/acdd-index/src/ast/visitor.rs
- crates/acdd-index/src/models/details.rs
- crates/acdd-index/src/languages/rust/
- crates/acdd-index/src/linker/
- crates/acdd-index/src/db/lint/unused_public_api.rs
- tests/languages/
- tests/linker/
- tests/mcp/context/entrypoints_and_delta.rs
- ROADMAP.md
- CHANGELOG.md
depends_on:
- universal-local-mutates-and-overview-health
- organize-manifests-by-language-and-rust-packs
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: crossfile-clap-linker-and-delta-proofs
  title: Persist clap Parser metadata + cross-file cli_command_registration candidates, resolve exact handles edges in linker framework relations, and verify cold + incremental delta addition/removal/shadow cases
  status: completed
  evidence: Replaced single-file thread-local clap handler matching with persisted declarative CommandDispatch metadata (NodeDetailKey::{CLI_COMMAND_PARSER, CLI_COMMAND_SUBCOMMAND, CLI_COMMAND_DISPATCH}) in languages/rust/cli_registration.rs and cross-file linker resolution in linker/rust/cli_dispatch.rs + linker/framework_relations.rs emitting exact cli_command_registration -> handles edges into db/writer/entrypoints.rs. Verified via tests/mcp/context/entrypoints_and_delta.rs::rust_crossfile_clap_dispatch_cold_and_delta_transitions (9 cold/delta add/remove/shadow/override/Cargo.toml transitions with Cold==Delta output_root parity), preserved all 4 existing clap negative tests, and verified split-file args::Cli::parse() in tests/linker/method_semantics.rs::rust_inline_mod_relative_reexports_and_split_clap_handles_resolve_exactly.
- subtask_ref: scoped-inline-mod-exports-and-lint-preservation
  title: Publish inline mod_item pub use exports on scoped ModuleDeclaration nodes in ast/visitor.rs and languages/rust/, proving named/wildcard re-exports and unchanged unused_public_api lint results
  status: completed
  evidence: Published visibility-aware named and wildcard pub use EXPORTS facts on scoped inline Rust ModuleDeclaration nodes (NodeDetailKey::INLINE_MODULE) in ast/visitor.rs::ScopePublication::publish and languages/rust/mod.rs::module_exports_with_bindings, and extended RustForwarding (keyed by module.id.as_str()), NamespaceIndexes, RustLinker::module_namespaces/normalize_declared_child_module, and semantic_context to forward inline mod re-exports from crate roots while keeping db/lint/unused_public_api.rs regression-free. Verified via tests/languages/profiles/forwarding/mod.rs::inline_rust_mod_exports_forward_from_crate_root_without_unused_public_api_regressions, tests/linker/method_semantics.rs::rust_inline_mod_relative_reexports_and_split_clap_handles_resolve_exactly, and tests/core/arch_lint.rs::dead_tails_unused_public_api_enforces_cross_module_non_test_usage_matrix.
receipt:
  run_id: LS-004-rust-entrypoi-20261126165954-1739070-3
  node_cursor: root:crossfile-clap-and-inline-mod-exports.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ls004-t3-2
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 51c20611de3b184c2e105c992344f215c9cd9ac3
  committed_oid: f5293dff8ba815a360a1de3e34dce6e143c8c2f4
  accepted_at: 2026-11-26T20:02:50.387Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `1` | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-04`, `AC-05`)**: `tests/languages/`, `tests/linker/`, and `tests/mcp/context/entrypoints_and_delta.rs` prove split-file `clap` cold/delta entrypoints, inline `mod` named/wildcard `pub use` publication, and unchanged `unused_public_api` lint results.

## Negative boundaries

- No new third-party dependencies in `Cargo.toml` and no SQLite schema version changes.
- No hardcoded framework package names inside Rust parser code (`INV-MANIFESTS-BY-LANGUAGE`).
- No suppression of true shadowed callable `Calls`/`Handles` unresolved diagnostics or deletion of real cross-symbol `Mutates` edges.
- No downstream string heuristics inside `db/analyze/` or `src/`.
- No regressions in existing `unused_public_api` lint findings or `entrypoints_and_delta` negative shadow tests.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Wave 0 (parallel disjoint: language-agnostic local `Mutates` + `overview` health vs. manifest subfolders + declarative Rust packs) and Wave 1 (cross-file `clap` + inline `mod` exports).
- **Correctness**: Preserves real `Mutates` edges and shadowed callable diagnostics, enforces deterministic recursive manifest discovery, package-gated shadow safety, cold + delta consistency, and non-regression of `unused_public_api`.
- **Coherence**: Fully independent of [UX-004](../UX-004-mcp-tool-surface-consolidation.md) (`depends_on: []`), lifting multi-language resolution accuracy and automatically enriching `code_map_analyze(mode="entrypoints")`, `code_map_overview`, and `AC-007` `GET /api/map`.
