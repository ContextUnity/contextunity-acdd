---
id: AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
title: Language-Agnostic Linker Overhaul, Comprehensive Lint Rules, Cycle Breaking & Modularity Ratchet
doc_type: contract
status: completed
owners:
- crates/acdd-core/src/
- crates/acdd-index/src/
- crates/acdd-state/src/
- src/
- docs/
- skills/acdd/
- tests/
- acdd.yaml
- .acdd/profile.yaml
invariants:
- 'INV-AR004-ZERO-HEURISTIC-AGNOSTIC-LINKER: Every resolved or contract-linked edge across all 16 compiled language profiles derives strictly from declared types, declarative .toml member_returns/call_returns rules, AST destructuring/callback facts, or indexed trait/derive contracts — never from name-only guessing or unregistered package homonyms.'
- 'INV-AR004-STRUCTURAL-LINT-SIGNAL: Duplicate detection operates on normalized AST statement sequences confined behind crates/acdd-index/src/ast/ (preserving ARCH-TREE-SITTER-CONFINED), cycles.scc separates module import SCCs from function call recursion, and ArchRuleKind::ALL.len() == 8 remains locked.'
- 'INV-AR004-ENFORCED-LAYER-AND-HYGIENE-GATES: Every declared zone in acdd.yaml is referenced by at least one active rule, Layer 1/Layer 2 boundaries and hot-path constraints from LS-003 are enforced via executable acdd-rule blocks, intra-crate module import graphs are acyclic (forbid_cycles: error), production callables obey INV-BOUNDED-STRUCTURAL-COMPLEXITY (AST cognitive_complexity, max_nesting_depth, graph high_fanout, and zero ValueFlow fact_budget_exceeded rather than formatting-sensitive raw LOC thresholds), and check_full_repo enforces clean lint across all 6 categories.'
- 'INV-AR004-REPLACE-DONT-STACK: Rust-only stringly-typed callback projections (__rust_fn_param and details["signature"] text parsing), hardcoded TypeScript WebIDL method arms, duplicated external constructor matches! tables in linker/rust/value_flow.rs, raw 5-line text duplicate hashing, and intra-crate Graph/Edge homonyms are deleted once their canonical replacements land.'
acceptance_criteria:
- 'AC-1: Completed zero-heuristic Rust baseline extensions (macro $vis:vis visibility, Tuple/Range manifests, builtin trait impls, external projections, rusqlite::Transaction with unregistered-homonym rejection) remain intact and covered by rust_zero_heuristic_resolution_extensions_matrix while __rust_fn_param is migrated onto typed ValueExpr::CallbackParam.'
- 'AC-2: Declarative ReceiverTypeRule.member_returns and call_returns in .toml manifests support compound generic wrappers (Result<T, E>, Option<Tuple<A, B>>, Iterator<T>, Promise<T>, QuerySet<Self>), closing Playwright chains, deleting hardcoded WebIDL method arms in typescript/mod.rs, deleting duplicated external constructor matches! tables in linker/rust/value_flow.rs, and resolving compound Rust stdlib/Cargo chains (rsplit_once -> Option<(&str, &str)>, Statement::query -> Rows -> Row::get, sha2::Digest::update).'
- 'AC-3: Language-agnostic ValueExpr::CallbackParam, unified struct/tuple/enum-variant destructuring in ValueFlowCollectorCore, generalized lookup_builtin_member, two-tier polymorphic trait/interface declaration linking (Calls/References to Trait::method + bounded PossibleCalls), and Tier-1 SyntaxGrammarSpec adoption lift multi-language and Rust resolution while eliminating the 2,669-edge Rust false-shadowing bucket.'
- 'AC-4: Rust-specific deterministic gaps — #[derive(clap::Parser)] associated Self constructors (Cli::parse().run() in src/cli/mod.rs), pub(crate) use module re-export aliases, impl Trait for $ty declarative macros (enum_codec!, target_decoder!), #[cfg] impl disambiguation, and intra-crate helper calls — resolve cleanly with zero false-positive heuristic edges.'
- 'AC-5: AST-statement clone detection in crates/acdd-index/src/db/lint/duplicates.rs extracts normalized statement fingerprints via src/ast/, skips pure delimiter/comment trivia, collapses multi-line wrapped calls into single statement units, and merges overlapping adjacent windows into maximal clone spans.'
- 'AC-6: crates/acdd-index/src/db/lint/cycles.rs separates multi-node module import SCCs (kind = imports, len >= 2) from single-node and local mutually recursive function calls; dead_tails.dangling_call is promoted to LintSeverity::Error; syntax.rs enforces configurable ArchitectureConfig.complexity limits (syntax.cognitive_complexity, syntax.max_nesting_depth, syntax.high_fanout, syntax.fact_budget_exceeded, and syntax.intra_crate_type_homonym) wired into enforced_invariant_ids for INV-BOUNDED-STRUCTURAL-COMPLEXITY without penalizing flat match/lookup tables; and crates/acdd-index/src/db/lint/rules.rs emits docs.unused_arch_zone (LintSeverity::Warn) when any acdd.yaml zone has zero referencing rules across acdd.yaml and indexed acdd-rule blocks.'
- 'AC-7: Executable acdd-rule blocks and INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md, docs/architecture/indexing.md, docs/adr/0010-static-value-flow-and-receiver-resolution.md, docs/adr/0011-language-linkers-and-framework-manifests.md, and acdd.yaml enforce Layer 2 linker/extractor boundaries, ManifestParser must_implement, hot-path Regex::new / shrink_to_fit / raw node.details[key] bans, index_db and host_telemetry zone confinement, structural complexity bounds, and forbid_cycles.'
- 'AC-8: All intra-crate module import SCCs (including the 88-node acdd-index SCC and 5-node acdd-core/src/config SCC) are broken into acyclic DAGs, intra-crate Graph/Edge homonyms are disambiguated (CompactGraph/CompactEdge), all deeply nested / high-complexity production monoliths violating INV-BOUNDED-STRUCTURAL-COMPLEXITY (such as enter, value_depth, project, RustMembers::build, build_initialized, syntax::validate, evaluate_tracked, and evaluate_pass, while preserving clean flat routers/lookup tables like is_combining_mark) are decomposed into cohesive submodules, and all verified AST clones and proven dead_tails/phantom_tested items are eliminated.'
- 'AC-9: Live acdd serve / MCP processes detect executable binary replacement (build/digest mismatch against on-disk binary) and reload or fail-safe cleanly without index-rebuild ping-pong against updated CLI binaries.'
- 'AC-10: .acdd/profile.yaml check_full_repo enforces acdd lint across all 6 categories (arch, syntax, docs, duplicates, dead_tails, cycles), and cargo clippy, cargo test --workspace --all-targets, acdd docs check, acdd validate, and /tmp/contextunity-test-* leak checks pass with 0 errors.'
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
  closed_at: 2026-11-24T03:25:29.684Z
  target_base: main
  target_base_commit: f9a18c4760e38fd0d30d996f71d0e0bf9e529568
  task_receipts:
    declarative-returns-and-agnostic-value-flow:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:declarative-returns-and-agnostic-value-flow.deliver
      gate_id: deliver
      worker_id: ar04-reviewer-w1-flow-1
      claim_revision: 1
      contract_revision: 3
      candidate_tree: b35dc5cde1611e1da6f216a5c04a8a84e1d37d4a
      committed_oid: b2a480d7bb07d3ac4a7b6c1314220846c0771009
      accepted_at: 2026-11-22T19:27:21.046Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-DECLARATIVE-MANIFEST-RETURNS: Chained external/stdlib method and function return types (including Result/Option/Tuple/Iterator/Promise/QuerySet wrappers) resolve from .toml manifests without hardcoded language method tables.'
          - 'INV-AR004-POLYMORPHIC-CONTRACT-HONESTY: Calls on &dyn Trait, Protocol/ABC, and TS/Tier-2 interfaces link deterministically to the trait/interface method declaration (plus bounded PossibleCalls when closed) while never fabricating a single concrete struct Calls edge.'
          verified:
          - INV-AR004-DECLARATIVE-MANIFEST-RETURNS
          - INV-AR004-POLYMORPHIC-CONTRACT-HONESTY
        review_summary: Deliver accepted declarative-returns-and-agnostic-value-flow candidate
    lint-engine-ast-clones-cycles-and-rules:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:lint-engine-ast-clones-cycles-and-rules.deliver
      gate_id: deliver
      worker_id: ar04-reviewer-w1-lint-1
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 6a833b70707aa6d9301440976f57ed216d50c702
      committed_oid: ead35da15fdba599e51cb782e137434ec1b382c6
      accepted_at: 2026-11-22T18:03:58.679Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-LOCKED-ARCH-RULE-ENUM: ArchRuleKind::ALL.len() == 8 remains unchanged; structural complexity, nesting, fanout, fact-budget, and homonym checks evaluate in LintCategory::Syntax (DEFAULT_VERIFY) backed by ArchitectureConfig.complexity and wired into enforced_invariant_ids(conn, config_rules) via resolve_architecture(conn, None) while preserving its public signature.'
          - 'INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES: Closing-brace blocks and wrapped single-statement calls produce 0 duplicate warnings, adjacent AST-statement clone windows coalesce into maximal spans, flat lookup/router match tables incur no per-arm complexity penalty, and 1-node self-recursive functions produce 0 cycles.scc warnings.'
          verified:
          - INV-AR004-LOCKED-ARCH-RULE-ENUM
          - INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES
    lint-engine-soundness-perf-and-rule-hygiene:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:lint-engine-soundness-perf-and-rule-hygiene.deliver
      gate_id: deliver
      worker_id: ar04-t6-deliverer
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 830c899c6d9cbbb04720008bb19b0b8d8b06e2de
      committed_oid: d2ff0a9c6e5959f8e83cf21457543ed34ec345a7
      accepted_at: 2026-11-24T02:12:54.563Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-SOUND-LINT-ENGINE: ast_pattern parses each file once per language and fails closed on work_limited or timeout; edge_matches_rule checks possible_calls; allow_only_dependencies checks externals when configured; dead_tails uses zero raw .rs disk scanning; and duplicates normalization handles Type-2 local variable renames.'
          verified: []
    rust-extractor-and-rust-linker-coverage-recovery:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:rust-extractor-and-rust-linker-coverage-recovery.deliver
      gate_id: deliver
      worker_id: ar04-polish-coordinator
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 5bd461fd7ff732a38f03b35cac51a908b2cfc1df
      committed_oid: fd6e721e678eb33015750ca29a0daae5184a3606
      accepted_at: 2026-11-24T01:51:38.482Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-RUST-RECOVERY-NO-HEURISTICS: Every recovered Rust resolution edge derives from AST attribute/type extraction, explicit RustForwarding re-exports, inherent/trait definitions, or bounded dynamic dispatch (<= K=8) with zero name-only guessing.'
          verified: []
    rust-linker-precision-and-god-module-split:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:rust-linker-precision-and-god-module-split.deliver
      gate_id: deliver
      worker_id: ar04-reviewer-w2-rust-2
      claim_revision: 1
      contract_revision: 6
      candidate_tree: 96fabecb0b740534c339b08742c094ec6b8c3c54
      committed_oid: d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249
      accepted_at: 2026-11-22T21:40:55.393Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-RUST-NO-SHADOW-HACKS: __rust_fn_param and details["signature"] text parsing are deleted in favor of ValueExpr::CallbackParam, duplicated external constructor matches! tables are replaced by .toml manifest returns, and unregistered Cargo crate homonyms remain strictly unresolved.'
          - 'INV-AR004-RUST-MODULAR-BOUNDS: Every production function in languages/rust/** and linker/rust/** satisfies INV-BOUNDED-STRUCTURAL-COMPLEXITY (cognitive_complexity, max_nesting_depth, fanout, and zero fact_budget_exceeded) with zero >= 5-statement clones and zero dead_tails.'
          verified: []
    universal-value-flow-branch-joins-and-manifest-returns:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:universal-value-flow-branch-joins-and-manifest-returns.deliver
      gate_id: deliver
      worker_id: ar04-polish-coordinator
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 93ef0caebdadc1d2b37ad91dc2f946ee67fdee84
      committed_oid: bcf8fe611f48aaf99e63bd11c441bf441ae6ef77
      accepted_at: 2026-11-24T01:57:18.247Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-UNIVERSAL-VALUE-FLOW-PARITY: Declarative call_returns rules, element-wise Tuple/Option BranchJoin, monotonic loop variable typing, and external_aliases inheritance apply uniformly across all supported language profiles rather than hardcoded JS/TS/Vue or Rust branches.'
          verified: []
    upfront-contracts-ast-fingerprints-and-binary-guard:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:upfront-contracts-ast-fingerprints-and-binary-guard.deliver
      gate_id: deliver
      worker_id: ar04-reviewer-w0-2
      claim_revision: 1
      contract_revision: 5
      candidate_tree: faf163fe7ea440c7e911983777405cc4247cf076
      committed_oid: d0f20c1665303e03a743be2a6eac63975772878c
      accepted_at: 2026-11-22T17:26:00.130Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-LEAF-MODELS-SEMANTIC: crates/acdd-core/src/lint.rs exposes ComplexityLimits on ArchitectureConfig (preserving ArchRuleKind::ALL.len() == 8), and crates/acdd-index/src/{models,semantic} expose typed CallbackParam, CallableSignature, structural complexity NodeDetailKey slots, and disambiguated CompactGraph/CompactEdge DTOs with zero upward imports from languages or linker.'
          - 'INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS: Single-pass AST statement fingerprinting and cognitive_complexity / max_nesting_depth computation live in crates/acdd-index/src/ast/ without leaking tree_sitter into src/db/, scoring flat exhaustive match/switch tables as +1 rather than per-arm line penalties.'
          verified:
          - INV-AR004-LEAF-MODELS-SEMANTIC
          - INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS
        review_summary: Deliver accepted Wave 0 contracts, AST statement fingerprints/cognitive metrics, binary magic v5, and pre-registered seam test modules.
    workspace-cycle-breaking-dedup-and-gate-ratchet:
      run_id: AR-004-agnostic-link-20261122144815-1445895-1
      node_cursor: root:workspace-cycle-breaking-dedup-and-gate-ratchet.deliver
      gate_id: deliver
      worker_id: ar04-polish-coordinator
      claim_revision: 3
      contract_revision: 9
      candidate_tree: 7ccf997ff6c86245a9a6f6cbb9671924d280ac2a
      committed_oid: 1e395fae0ab8372fd2924213fa404bc00be91f1a
      accepted_at: 2026-11-24T00:43:58.507Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR004-WORKSPACE-ACYCLIC-AND-CLEAN: Workspace-wide forbid_cycles (severity: error), INV-BOUNDED-STRUCTURAL-COMPLEXITY (0 cognitive_complexity / max_nesting_depth / high_fanout / fact_budget_exceeded errors), and acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles report 0 errors and 0 unresolved hygiene regressions.'
          - 'INV-AR004-DAEMON-BINARY-COHERENCE: Long-lived acdd serve / MCP processes detect on-disk binary replacement via executable identity digest and reload/re-exec or fail-safe cleanly without index rebuild thrashing.'
          verified: []
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-LEAF-MODELS-SEMANTIC: crates/acdd-core/src/lint.rs exposes ComplexityLimits on ArchitectureConfig (preserving ArchRuleKind::ALL.len() == 8), and crates/acdd-index/src/{models,semantic} expose typed CallbackParam, CallableSignature, structural complexity NodeDetailKey slots, and disambiguated CompactGraph/CompactEdge DTOs with zero upward imports from languages or linker.'
      - 'INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS: Single-pass AST statement fingerprinting and cognitive_complexity / max_nesting_depth computation live in crates/acdd-index/src/ast/ without leaking tree_sitter into src/db/, scoring flat exhaustive match/switch tables as +1 rather than per-arm line penalties.'
      - 'INV-AR004-DECLARATIVE-MANIFEST-RETURNS: Chained external/stdlib method and function return types (including Result/Option/Tuple/Iterator/Promise/QuerySet wrappers) resolve from .toml manifests without hardcoded language method tables.'
      - 'INV-AR004-POLYMORPHIC-CONTRACT-HONESTY: Calls on &dyn Trait, Protocol/ABC, and TS/Tier-2 interfaces link deterministically to the trait/interface method declaration (plus bounded PossibleCalls when closed) while never fabricating a single concrete struct Calls edge.'
      - 'INV-AR004-RUST-NO-SHADOW-HACKS: __rust_fn_param and details["signature"] text parsing are deleted in favor of ValueExpr::CallbackParam, duplicated external constructor matches! tables are replaced by .toml manifest returns, and unregistered Cargo crate homonyms remain strictly unresolved.'
      - 'INV-AR004-RUST-MODULAR-BOUNDS: Every production function in languages/rust/** and linker/rust/** satisfies INV-BOUNDED-STRUCTURAL-COMPLEXITY (cognitive_complexity, max_nesting_depth, fanout, and zero fact_budget_exceeded) with zero >= 5-statement clones and zero dead_tails.'
      - 'INV-AR004-LOCKED-ARCH-RULE-ENUM: ArchRuleKind::ALL.len() == 8 remains unchanged; structural complexity, nesting, fanout, fact-budget, and homonym checks evaluate in LintCategory::Syntax (DEFAULT_VERIFY) backed by ArchitectureConfig.complexity and wired into enforced_invariant_ids(conn, config_rules) via resolve_architecture(conn, None) while preserving its public signature.'
      - 'INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES: Closing-brace blocks and wrapped single-statement calls produce 0 duplicate warnings, adjacent AST-statement clone windows coalesce into maximal spans, flat lookup/router match tables incur no per-arm complexity penalty, and 1-node self-recursive functions produce 0 cycles.scc warnings.'
      - 'INV-AR004-WORKSPACE-ACYCLIC-AND-CLEAN: Workspace-wide forbid_cycles (severity: error), INV-BOUNDED-STRUCTURAL-COMPLEXITY (0 cognitive_complexity / max_nesting_depth / high_fanout / fact_budget_exceeded errors), and acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles report 0 errors and 0 unresolved hygiene regressions.'
      - 'INV-AR004-DAEMON-BINARY-COHERENCE: Long-lived acdd serve / MCP processes detect on-disk binary replacement via executable identity digest and reload/re-exec or fail-safe cleanly without index rebuild thrashing.'
      - 'INV-AR004-SOUND-LINT-ENGINE: ast_pattern parses each file once per language and fails closed on work_limited or timeout; edge_matches_rule checks possible_calls; allow_only_dependencies checks externals when configured; dead_tails uses zero raw .rs disk scanning; and duplicates normalization handles Type-2 local variable renames.'
      - 'INV-AR004-RUST-RECOVERY-NO-HEURISTICS: Every recovered Rust resolution edge derives from AST attribute/type extraction, explicit RustForwarding re-exports, inherent/trait definitions, or bounded dynamic dispatch (<= K=8) with zero name-only guessing.'
      - 'INV-AR004-UNIVERSAL-VALUE-FLOW-PARITY: Declarative call_returns rules, element-wise Tuple/Option BranchJoin, monotonic loop variable typing, and external_aliases inheritance apply uniformly across all supported language profiles rather than hardcoded JS/TS/Vue or Rust branches.'
      verified:
      - INV-AR004-LEAF-MODELS-SEMANTIC
      - INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS
      - INV-AR004-DECLARATIVE-MANIFEST-RETURNS
      - INV-AR004-POLYMORPHIC-CONTRACT-HONESTY
      - INV-AR004-LOCKED-ARCH-RULE-ENUM
      - INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES
    decisions:
    - |-
      AR-004 Architectural Blueprint Decision:
      1. Leaf & Typed Contracts (Task 1, Wave 0): Freeze ComplexityLimits on ArchitectureConfig (crates/acdd-core/src/lint.rs) preserving ArchRuleKind::ALL.len()==8; add ValueExpr::CallbackParam + CallableSignature (semantic/types.rs, typed_facts/binary.rs) replacing __rust_fn_param; add structural complexity & AST statement fingerprint slots via NodeDetailKey/TypedFacts; rename compact_graph::{Graph,Edge} -> CompactGraph/CompactEdge to remove intra-crate homonyms; implement single-pass AST statement normalizer and AST cognitive_complexity/max_nesting_depth (flat match=+1, nested=+1+depth) inside crates/acdd-index/src/ast/ (preserving ARCH-TREE-SITTER-CONFINED); add UNUSED_ARCH_ZONE check in crates/acdd-state/src/validate.rs.
      2. Language-Agnostic Linker & Manifest Returns (Task 2, Wave 1): Extend ReceiverTypeRule.member_returns/call_returns for compound generic wrappers across TOML packs, deleting hardcoded WebIDL arms from typescript/mod.rs; evaluate ValueExpr::CallbackParam, unified ValueFlowCollectorCore destructuring, generalized lookup_builtin_member, and two-tier polymorphic trait/interface linking (inferred Calls/References + bounded <=8 PossibleCalls) in shared linker phases; adopt SyntaxGrammarSpec in Python/TS; split high-complexity functions in linker/value_flow/{mod,initialize}.rs and manifests/syntax.rs.
      3. Rust Precision & Monolith Split (Task 3, Wave 1): Migrate __rust_fn_param and signature text parsing to ValueExpr::CallbackParam; delete duplicated external_constructor_result matches! arms in favor of rust-std.toml/rust-cargo.toml; resolve #[derive(clap::Parser)] Self, destructuring false-shadowing vs dynamic_callback, pub(crate) use aliases, impl Trait for $ty macros, and #[cfg] disambiguation while rejecting unregistered Cargo homonyms; adopt SyntaxGrammarSpec in rust/mod.rs and decompose enter, value_depth, project, and build.
      4. Structural Lint Engine & Executable Rules (Task 4, Wave 1): Switch db/lint/duplicates.rs to AST-statement fingerprint windows from src/ast/ with maximal-span coalescing; separate multi-node module/file imports SCCs (len>=2) from call recursion in db/lint/cycles.rs; keep dead_tails.dangling_call at LintSeverity::Error; evaluate syntax.{cognitive_complexity,max_nesting_depth,high_fanout,fact_budget_exceeded,intra_crate_type_homonym} in syntax.rs wired to rules.rs::enforced_invariant_ids; author INV-BOUNDED-STRUCTURAL-COMPLEXITY and executable acdd-rule blocks across modularity.md, indexing.md, ADR 0010, ADR 0011, and acdd.yaml covering index_db and host_telemetry (0 UNUSED_ARCH_ZONE).
      5. Cycle Breaking, Dedup, Binary Guard & Profile Ratchet (Task 5, Wave 2): Break module import SCCs in acdd-core/src/config (5-node), acdd-index (88-node), acdd-state, and src/ using code_map_query(slice) and enable workspace-wide forbid_cycles: error; refactor remaining complexity outliers (evaluate_pass, persist_files_impl, collect_doc_findings, run_isolated_process_unix, etc.), consolidate AST clones (code_tools.rs, acdd-core, pipeline, state_bridge), prune/gate proven dead_tails; add executable identity drift detection/reload to src/mcp/server.rs + src/cli/reload.rs; align docs/reference/ + skills/acdd/ and ratchet .acdd/profile.yaml check_full_repo to all 6 lint categories.
    - |-
      AR-004 Revised Architectural Blueprint Decision (resolving finding #336):
      1. Sequential Wave Ordering (Task 2 -> Task 3 -> Task 5): rust-linker-precision-and-god-module-split (Task 3) now depends_on [declarative-returns-and-agnostic-value-flow] (Task 2, Wave 1), moving Task 3 to Wave 2 and Task 5 to Wave 3. Task 2 lands shared ValueExpr::CallbackParam evaluation, ValueFlowCollectorCore destructuring, compound TOML return rules (including rust-std.toml / rust-cargo.toml), and Python/TS SyntaxGrammarSpec first (verified via cargo test -p acdd-index --lib && cargo test --test languages); Task 3 then safely deletes __rust_fn_param and redundant external_constructor_result matches! arms and verifies cargo test --test linker rust::.
      2. Owner & Signature Confinement (Task 1 & Task 4): Removed UNUSED_ARCH_ZONE from crates/acdd-state/src/validate.rs (preserving INV-STATE-NO-INDEX, INV-SINGLE-OWNER, and acdd validate). Task 4 owns docs.unused_arch_zone (LintSeverity::Warn) in crates/acdd-index/src/db/lint/rules.rs alongside load_indexed_doc_rules, and preserves pub fn enforced_invariant_ids(conn: &Connection, config_rules: &[ArchRuleSpec]) -> Result<BTreeSet<String>> unchanged by consulting resolve_architecture(conn, None)? internally for ArchitectureConfig.complexity.invariant.
      3. Admitted Caller Scope (Task 1): Task 1 explicitly covers CompactGraph / CompactEdge renames across all admitted caller files (crates/acdd-index/src/{models/,linker/,db/writer/}, src/engine/pipeline/delta.rs, tests/{core,core/,incremental/,linker,linker/}), ComplexityLimits in crates/acdd-core/src/lint.rs, ValueExpr::CallbackParam + CallableSignature in semantic/ and typed_facts/, single-pass AST statement fingerprints + cognitive_complexity/max_nesting_depth in crates/acdd-index/src/ast/, and canonical seam test pre-registrations.
    - 'BUDGET_OVERRIDE escalation (no builder self-approval): clean shared production code across Subtasks 1 & 2 measures 645 net production LOC (+421/-63 across 29 tracked files + 287 lines in crates/acdd-index/src/ast/fingerprints.rs) vs max_net_loc=550 (+95 LOC), plus test additions (+69/-27 across 11 tracked test files + 256 lines in tests/core/ast_contracts.rs & tests/linker/callback_contracts.rs) and 1 doc update (docs/architecture/performance.md CompactEdge claim). Every line implements required Task 1 contracts (ComplexityLimits, ValueExpr::CallbackParam + CallableSignature, StatementFingerprint + binary magic v5 / epoch 15, CompactGraph/CompactEdge rename, single-pass AST fingerprints + cognitive complexity, and pre-registered seam tests) with 0 Clippy warnings and 0 targeted lint violations. Requesting coordinator/reviewer confirmation of BUDGET_OVERRIDE.'
    - 'BUDGET_OVERRIDE escalation (never self-approved by builder): Task declarative-returns-and-agnostic-value-flow spans 31 files (+3416/-1717 tracked + 988 untracked evaluate.rs = +2687 net LOC total; +2404 src / +283 tests/languages) vs task max_net_loc=650. Primary drivers are structural decomposition (splitting value_flow/mod.rs -> evaluate.rs + initialize.rs helpers, splitting SyntaxRule::validate into 6 helpers), compile-time ReturnTypeSpec parser/projector, shared ValueFlowCollectorCore destructuring + push_callback_param, two-tier polymorphic dispatch, manifest updates across 5 TOMLs, and 5 persisted seam tests. All 86 lib, 261 languages, 80 linker, and 18 commitment_integrity tests + strict clippy + acdd lint (0/31) pass cleanly with 0 /tmp/contextunity-test-* leaks (5480==5480). Request coordinator/reviewer BUDGET_OVERRIDE approval.'
    - 'BUDGET_OVERRIDE: Approved for declarative-returns-and-agnostic-value-flow (+2404 net production LOC vs underestimated blueprint max_net_loc=650, per builder escalation #341). Audited numstat across 25 source files confirms proportional shared-engine scope: ReturnTypeSpec compound wrapper parser/projector across 5 TOML manifests (+292 net in syntax.rs including decomposing 629-LOC SyntaxRule::validate; +696 across semantic_context/), ValueExpr::CallbackParam + ValueFlowCollectorCore destructuring + SyntaxGrammarSpec migration across Python/TS/Vue (+663 across support/python/typescript), two-tier polymorphic dispatch (+111 in linker/resolution & receivers), and splitting 520-LOC evaluate_tracked + 651-LOC build_initialized (+592 net across value_flow/{mod,evaluate,initialize}.rs). Zero introduced duplicates, 0 clippy/lint errors, 0 /tmp leaks, and 86+261+80+18 tests passing.'
    - 'BUDGET_OVERRIDE escalation request (+39 net prod LOC over max_net_loc=450): clean monolith decomposition + precision changes yield +489 net prod LOC in crates/ (-407 in linker/rust/value_flow.rs, +313 across value_flow/{mod,collector,expr,macros,types}.rs split from 3819L monolith, +244 languages/rust/linker.rs, +107 languages/rust/mod.rs, +102 languages/rust/macros.rs, +67 linker/rust/semantic_context.rs, +35 languages/rust/cli_registration.rs, +16 manifests/rust-std.toml, +12 linker/rust/resolution.rs, 0 visitor.rs, 0 manifests/rust-cargo.toml) plus +370 test LOC in tests/linker/. Builder did NOT self-approve; requesting coordinator/architect BUDGET_OVERRIDE review.'
    - 'BUDGET_OVERRIDE: Approved for rust-linker-precision-and-god-module-split (+489 net production LOC vs blueprint max_net_loc=450; +39 LOC delta, per builder escalation #345). Verified numstat confirms major shrinkage in linker/rust/value_flow.rs (-407 net) while decomposing the 3,819-LOC languages/rust/value_flow.rs monolith into 5 focused submodules (+313 net), migrating rust/mod.rs onto SyntaxGrammarSpec (+107), decomposing linker.rs/macros.rs/semantic_context.rs/resolution.rs, and landing #[derive(Parser)] Self, destructuring, module re-export aliases, impl Trait macros, and ValueExpr::CallbackParam migration with 0 syntax/duplicates violations, +296 resolved / -466 unresolved Rust edges, 83/83 linker tests, and passed acdd verify.'
    - 'BUDGET_OVERRIDE: Approved for rust-linker-precision-and-god-module-split (+562 net production LOC vs blueprint max_net_loc=450 and prior #346 +489, per builder escalation #348 resolving review #347). Verified +73 LOC remediation delta stems directly from: wiring pub(super)/pub(crate) use forwarding in resolve_imported_member/resolve_imported_targets and closing all 29 dead_tails in languages/rust (now 0 dead_tails, 0 duplicates, 0 syntax issues), removing redundant std.str/String/serde_json arms in linker/rust/value_flow.rs (-446 net total) with serde_yaml moved to rust-cargo.toml (+25), and fixing multi-line docstring extraction under RUST_SPEC/PYTHON_SPEC in visitor.rs/rust/mod.rs (fixing core::fact_persistence 74/74 pass) while raising Rust resolved edges +628 (to 37,188) and dropping unresolved -841 (to 10,941).'
    - 'RETRACTED #348: Blackboard entry #348 was posted prematurely before builder ar04-builder-w2-rust completed remediation of review finding #347 and MUST BE IGNORED. Only #346 (+489 net LOC) currently stands; any net LOC growth after #347 remediation will be escalated and audited fresh against measured numstat and verification output.'
    - 'BUDGET_OVERRIDE escalation update (+620 net prod LOC in crates/, +131 over #346''s +489): review finding #1 remediation fixed pub(in super::super)/pub(super)/pub(crate) re-exports, include!-module traversal, static RUST resolution, Option/LocalKey combinator typing, and syntax_doc docstrings across inferred.rs (+31), type_resolution.rs (+61), evaluate.rs (+25), languages/rust/linker.rs (+24 net delta), and value_flow/ (+13 net delta), reducing dead_tails in languages/rust from 29 to 0 and Rust unresolved calls by -625 (removing hardcoded external_constructor_result arms in linker/rust/value_flow.rs net -29). Builder did NOT self-approve; requesting coordinator review of updated +620 net prod LOC.'
    - 'BUDGET_OVERRIDE final escalation (+631 net prod LOC in crates/, +142 above #346''s +489): remediated review #347 findings (0 dead_tails across languages/rust & linker/rust, removed hardcoded external_constructor_result arms in favor of rust-std/rust-cargo TOML, preserved multi-line/attribute docstrings in syntax_doc, and extracted step_rust_module_member / push_explicit_constant_member helpers so touched functions in inferred.rs and type_resolution.rs pass max_nesting_depth). Builder did NOT self-approve; awaiting coordinator approval of +631 net prod LOC before acdd submit.'
    - 'BUDGET_OVERRIDE: Approved for rust-linker-precision-and-god-module-split (+631 net production LOC vs blueprint max_net_loc=450 and prior #346 +489, per builder escalation #351 after remediating finding #347). Verified numstat confirms net -436 in linker/rust/value_flow.rs (after removing redundant std.str/String/serde_json/serde_yaml match arms) and +142 remediation delta across linker.rs (+24), inferred.rs (+38), type_resolution.rs (+65), evaluate.rs (+25), visitor.rs (+4 syntax_doc fix), and rust-std.toml (+18), reducing languages/rust dead_tails 29->0, fixing core::fact_persistence, and improving Rust coverage +435 resolved / -627 unresolved with 0 duplicates/syntax violations and green acdd verify.'
    - 'Budget override escalation (no builder self-approval) for workspace-cycle-breaking-dedup-and-gate-ratchet: measured net production LOC is +1814 per budget.rs rules (+1701 across crates/{acdd-core,acdd-index,acdd-state}/src and src/ including -113 net in tests.rs; +2105 workspace total across 303 M / 5 D / 54 ?? files) vs blueprint max_net_loc=400. Growth comes from decomposing the 88-node acdd-index SCC, 5-node acdd-core config SCC, acdd-state/src/ SCCs, and AST cognitive_complexity/max_nesting_depth splits into 51 focused submodules (-21,727 tracked prod lines replaced by +23,428 submodule lines plus ExecutableIdentity binary-drift guard and CLI/MCP doc alignment). Verification: cargo clippy --workspace --all-targets --all-features -- -D warnings OK; 895/895 workspace tests pass; acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles --severity error => 0 errors (warn: 882 vs baseline 1014, duplicates 40->0, cycles 20->0, dead_tails 476->438, syntax 183->151, arch 295->293); acdd query run duplicates=0, dead_tails=0; /tmp/contextunity-test-* leak check 5480==5480; Cargo.toml and crates/*/Cargo.toml untouched. Requesting coordinator/architect BUDGET_OVERRIDE decision before acdd submit.'
    - 'BUDGET_OVERRIDE: Approved for workspace-cycle-breaking-dedup-and-gate-ratchet (+1814 net production LOC per budget.rs vs underestimated blueprint max_net_loc=400, per builder escalation #353). Verified numstat reflects decomposing the 88-node acdd-index SCC, 5-node acdd-core config SCC, acdd-state/src SCCs, and AST cognitive_complexity/max_nesting_depth outliers into 51 focused submodules (-21,727 tracked prod LOC -> +23,428 submodule LOC) plus ExecutableIdentity binary-drift guard, docs/skills alignment, and .acdd/profile.yaml ratchet, eliminating AST duplicates (40->0) and module cycles (20->0), reducing dead_tails (476->438) & syntax warnings (183->151), with 895/895 workspace tests, strict Clippy, acdd validate (9/9), 6-category acdd lint (0 errors), 0 /tmp leaks (5480==5480), and passing acdd verify.'
    - 'Review remediation ruling on #355 (ar04-reviewer-w3-1): (1) INV-SURFACE-COMPATIBILITY (#6): NOT authorizing breaking public API changes; builder MUST restore public LanguageProfile::linker / scope_analysis signature, languages::parse_file, and semantic::SymbolCatalog facade compatibility while keeping internal module graphs acyclic. (2) Docs Check (#5): docs/milestones/AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul.md:73 (pre-existing Task 3 move languages/rust/value_flow.rs) is an engine-protected spec file excluded from task extend-scope and is formally deferred to coordinator run-level polish/docs-scan prior to run_tests; builder MUST fix any Wave 3 link regression (e.g. line 71 languages/rust/linker.rs via surface preservation) AND align MCP setup incremental-reindexing docs with the daemon guard. (3) Findings #1-#4 must be resolved at canonical owners with accurate measured counts, followed by updated BUDGET_OVERRIDE escalation (#7) before submit.'
    - 'BUDGET_OVERRIDE_REQUEST: Requesting coordinator/architect approval for workspace-cycle-breaking-dedup-and-gate-ratchet.build after remediating review findings #1-#6 (#355/#356/#357). Baseline d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249 vs blueprint max_net_loc=400: (a) src + crates/*/src non-test = +2052 net LOC (+9559/-32144 tracked + 24637 untracked); (b) all admitted non-test excl .acdd/ = +2107 net LOC (+9662/-32192 tracked + 24637 untracked; +2113 incl .acdd/profile.yaml +6); (c) test LOC = +423 (+590/-327 tracked + 160 untracked). Growth reflects Wave 3 monolith decomposition across core/index/state/host plus restored profile.rs+linker traits compatibility (#6), live MCP binary-guard seam (#4), and dead-tail/syntax/cycle remediation (#1-#3). Latest checks: clippy --all-targets --all-features -D warnings PASS; cargo test --workspace --all-targets 894 passed 0 failed 0 ignored; /tmp/contextunity-test-* BEFORE=5480 AFTER=5480; acdd build (607 files, 26375 nodes, 69258 edges) & validate PASS; 6-category acdd lint --severity error: 0 errors (48 rules); syntax warn: 142 (89 complexity, 42 depth, 9 fanout, 2 ExplainOptions homonym); duplicates: 0; dead_tails: 358 unreachable / 58 phantom_tested / 0 dangling; docs check fails only on deferred Task-3 line 73. Awaiting coordinator approval before acdd submit.'
    - |-
      BUDGET_OVERRIDE: Approved for workspace-cycle-breaking-dedup-and-gate-ratchet remediation pass per escalation #358 (+2052 net prod LOC in crates/src, +2107 all-admitted non-test excl .acdd/, +2113 incl .acdd/profile.yaml vs blueprint max_net_loc=400).
      approved_budget: 2150
      Rationale: delta covers full Wave 3 SCC decomposition across core/index/state/host, public API facade/compatibility restoration (#6) in profile.rs & linker traits, fail-closed Server/delta binary-drift fencing + live StdioClient seams (#4), and dead-tail/complexity/cycle remediations (#1-#3) without Cargo manifest edits. Verified truthful metrics: clippy (0 warnings), 894/894 tests pass, 5480==5480 /tmp leaks, 6-category acdd lint --severity error: 0 errors (48 rules), syntax warn reduced 183->151->142, duplicates=0, cycles=0, dead_tails 358/58/0, acdd verify pass.
    - 'Coordinator R2 ruling on #360 (ar04-reviewer-w3-1): (1) Delete unused shim crates/acdd-index/src/languages/rust/linker.rs and unused allow-suppressed re-exports in linker/traits.rs:9-12 (INV-NO-DEAD-CODE-SHIMS). (2) Amend #357: BOTH broken links in engine-protected docs/milestones/AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul.md lines 71 (languages/rust/linker.rs) and 73 (languages/rust/value_flow.rs) reflect Task 3 moves and are formally deferred to coordinator run-level polish/docs-scan prior to run_tests; zero other docs check failures permitted. (3) Restore all 7 missing/private baseline public paths from #360 finding 4 (acdd_index::languages::linker_for, acdd_index::linker::named_binding_scope, acdd_index::linker::NodeBindingIndex, acdd_index::semantic::{CatalogStoragePayload, decode_catalog_references, ReceiverCatalog}, acdd_core::config::load_adapter_strict) preserving INV-SURFACE-COMPATIBILITY without unused-import suppressions. (4) Fix ExecutableIdentity::current startup image vs installed path split (#360 finding 3) and decompose ScopePublication::publish (138/50), collect_pattern_bindings (56/50), resolve_member_result (58/50), and genuine warn-level outliers (#360 finding 1).'
    - '{"kind":"BUDGET_OVERRIDE_REQUEST","previous_approval":359,"approved_budget":2150,"measured":{"prod_net_loc":2473,"non_test_excl_acdd_net_loc":2535,"tests_net_loc":770,"baseline":"d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249"},"reason":"Round 3 #362 remediations: decomposed phase_imports (imports.rs), decode_fts5_segment_pages (fts5_fastpath.rs), and link_compact_impl (linker/mod.rs); broke linker/languages module cycles by splitting rust/linker_impl.rs and typescript/members.rs while restoring FileRelationContext and LanguageLinker::compose_file_relations; removed ExecutableIdentity::capture public shim and added argv[0]/PATH launch-path binding + rename-to-backup/generation-lock pre-publication guard across cold.rs, delta.rs, mcp/server.rs, and docs/reference/mcp-setup.md.","verification":{"acdd_verify_passed":true,"execution_digest":"70f6f19ebc79ec0a4544878944a8322457e557f8f13f1ea2285126d44d6ee6b8","workspace_tests":"896 passed, 0 failed across 13 suites","clippy_all_features":"pass","rustc_probe":"PROBE_OK","lint_errors":0,"duplicates":0,"dangling_calls":0,"tmp_leak_before":5480,"tmp_leak_after":5480}}'
    - |-
      BUDGET_OVERRIDE: Approved for workspace-cycle-breaking-dedup-and-gate-ratchet R3 remediation per escalation #364 (prod_net_loc=+2473, non_test_excl_acdd_net_loc=+2535, +2541 incl .acdd/profile.yaml, tests_net_loc=+770 vs prior #359 approved_budget=2150).
      approved_budget: 2600
      Rationale: +386/+392 delta over #359 reflects required #362 fixes: decomposing phase_imports, decode_fts5_segment_pages, link_compact_impl, and extracted helpers (syntax warn 138->132), restoring FileRelationContext + LanguageLinker::compose_file_relations compatibility while breaking linker/languages cycles (rust/linker_impl.rs, typescript/members.rs), removing public ExecutableIdentity::capture shim, and implementing launch-path binding + cold.rs generation-lock before_publish guard + docs/reference/mcp-setup.md alignment. Verified: 896/896 tests pass, clippy --all-targets --all-features -D warnings pass, external rustc PROBE_OK, 5480==5480 /tmp leaks, 6-category acdd lint 0 errors, duplicates=0, acdd verify pass, Cargo manifests untouched.
    - '{"kind":"BUDGET_OVERRIDE_REQUEST","previous_approval":365,"approved_budget":2600,"measured":{"prod_net_loc":5450,"added":47957,"deleted":42507,"candidate_tree":"e98b99c81d5106e651cbe9161131f01022e209a5","baseline":"d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249","tests_net_loc":674,"breakdown":{"acdd_core_src":{"added":583,"deleted":243,"net":340},"acdd_index_src":{"added":37783,"deleted":34287,"net":3496},"acdd_state_src":{"added":5375,"deleted":4486,"net":889},"src":{"added":4097,"deleted":3443,"net":654},"other_admitted":{"added":119,"deleted":48,"net":71}}},"remaining_syntax_warn_audit":{"count":3,"items":["crates/acdd-index/src/db/reader/types.rs:236 ExplainOptions (baseline pub API homonym under INV-SURFACE-COMPATIBILITY)","crates/acdd-index/src/db/symbols/types.rs:31 ExplainOptions (baseline pub API homonym under INV-SURFACE-COMPATIBILITY)","src/cli/dispatch_state.rs:123 src.cli.dispatch_state.dispatch (flat CLI Command match router, fanout 67/40)"]},"reason":"Round 4 #366 remediations: decomposed all 129 genuine complexity/nesting/fanout syntax warn outliers (syntax warn 132->3) across acdd-index, acdd-state, and src; tightened same-directory launch-path binding and platform docs in crates/acdd-core/src/fs.rs, docs/reference/mcp-setup.md, and tests/mcp.rs; fixed recover_running_mutating_trace lookup_action_result ordering in crates/acdd-state/src/usecases/verify.rs; and reconciled exact canonical budget.rs:78-167 accounting (+5450 non-test net).","verification":{"acdd_verify_passed":true,"execution_digest":"1370c8967239fc4dcca89efb268d7ff4f83a4c23394528e027ea87a86390d309","workspace_tests":"896 passed, 0 failed across 13 suites","clippy_all_features":"pass (0 warnings)","rustc_probe":"API_PROBE_OK","lint_errors":0,"cycles_warn":0,"syntax_warn":3,"duplicates":0,"dangling_calls":0,"tmp_leak_before":5480,"tmp_leak_after":5480}}'
    - |-
      BUDGET_OVERRIDE: Approved for workspace-cycle-breaking-dedup-and-gate-ratchet R4 remediation per canonical escalation #368 (budget.rs:78-167 measured non-test net=+5450 across +47957/-42507: acdd-core +340, acdd-index +3496, acdd-state +889, src +654, config/docs/skills +71; excluded test-path net=+674 vs prior #365 approved_budget=2600).
      approved_budget: 5500
      Rationale: verified diff confirms growth comes from decomposing all 129 genuine production complexity/nesting/fanout warnings across 78 files (syntax warn dropped 132->3, leaving only 2 baseline ExplainOptions homonyms + 1 flat CLI match router) alongside tightened same-directory launch-path binding in fs.rs, docs/reference/mcp-setup.md sync, and verify.rs trace recovery fix — with zero AST duplicates (0 groups), zero module cycles (0 warnings/errors), zero dangling calls, strict Clippy clean, 896/896 tests passing, external rustc API_PROBE_OK, 5480==5480 /tmp leaks, and Cargo manifests + milestone spec untouched.
    - 'R6 remediation guidance (#370 + Neon audit #248): (1) Keep non-test net LOC strictly <= 5,500 (currently +5,450 under #369 budget — stay net-neutral/negative via compact shared helpers, no gratuitous test bloat). (2) Address all 3 Codex R5 blockers (#370): (a) non-Linux ExecutableIdentity fail-closed/independent verification + aligned docs/reference/mcp-setup.md + daemon seam test in tests/mcp.rs + stream-hash binary in hash_file (BufReader), harden PATH exec bit/empty entry check in resolve_bound_installed_path, and defer reload_notify/try_reexec until after in-flight cold.rs stage cleanup & JSON-RPC response flush; (b) drop repo.lock before provider.lookup_action_result in crates/acdd-state/src/usecases/verify.rs while keeping CAS/claim-revision revalidation, and clean up verify.rs nested fn + 20-arg execute_gate_call_if_configured; (c) resolve route_path once and check existing-route ID before cloning/scanning in crates/acdd-index/src/ast/route_includes.rs (syntax/arch warn count <= 288 baseline). (3) Eliminate split-brain linker dispatch between crates/acdd-index/src/linker/dispatch.rs and LanguageProfile::linker() (canonical owner per languages/AGENTS.md) with 0 file-level import SCCs (acdd lint cycles/all categories).'
    - 'USER_DIRECTIVE_UPDATE (supersedes prior INV-SURFACE-COMPATIBILITY facade ruling #357/#361): The project owner explicitly directed: (1) REMOVE all legacy 0.5.x migration code, keys, hints, docs, and tests (LEGACY_05_KEYS, CONFIG_MIGRATION_REQUIRED in loader.rs / cli/mod.rs / guide.rs / docs/runbooks/migration-0.6.md + references in docs/reference and tests — rely strictly on AdapterFile #[serde(deny_unknown_fields)] unknown-field rejection). (2) REMOVE dead/test-only compatibility public APIs across internal workspace crates (no external SDK semver compat needed for non-published workspace crates): load_adapter_strict, languages::linker_for, languages::parse_file (migrate tests to ast::parse_file), semantic::{CatalogStoragePayload, decode_catalog_references, ReceiverCatalog}, linker::named_binding_scope, AcddConfig aliases (use Adapter), unused linker::link* wrappers (keep only production callers / minimal canonical entrypoints, migrate tests cleanly), and disambiguate ExplainOptions homonyms. Update INV-SURFACE-COMPATIBILITY in docs/architecture/modularity.md and src/AGENTS.md accordingly (protect active CLI/MCP DTO & ErrorCode contracts, forbid unused internal pub/re-export shims). (3) ADD a real, zero-false-positive lint rule in crates/acdd-index/src/db/lint/ (dead_tails.unused_public_api at LintSeverity::Error, keeping ArchRuleKind::ALL.len()==8 intact) that flags unused/test-only pub use re-exports and internal workspace pub APIs lacking non-test cross-module production callers (or narrowable to pub(crate)/#[cfg(test)]), verify 0 errors workspace-wide across all 6 lint categories, and cover it with table-driven tests in tests/core/arch_lint.rs. Note: #370 and #248 fixes remain mandatory.'
    - |-
      BUDGET_OVERRIDE: Approved for workspace-cycle-breaking-dedup-and-gate-ratchet.build per escalation #374 (+7,247 scoped non-test net LOC vs d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249: tracked_add=26160 tracked_del=43447 untracked=24534; acdd-core/src +480, acdd-index/src +4700, acdd-state/src +964, src +1155, .acdd/profile.yaml +6, docs/reference +21, skills/acdd +16, extended build.rs/acdd.yaml/CHANGELOG/INSTALL/docs -95).
      approved_budget: 7500
      Rationale: growth over #369 (5,500) comes from Decision #373 (new dead_tails.unused_public_api Error lint rule + migrating whitebox unit tests into inline #[cfg(test)] blocks of production-path .rs modules that budget.rs counts as non-test paths) plus Wave 3 module splits. Cargo.toml and crates/*/Cargo.toml untouched; acdd verify passed (claim_revision 10), clippy clean, macOS SSH passed, 0 lint errors across all 6 categories (48 rules).
    - 'USER_DIRECTIVE_UPDATE (2026-11-23T21:57+02:00, supersedes complexity lint emissions in AR-004 AC-6 / INV-AR004-ENFORCED-LAYER-AND-HYGIENE-GATES & adds universal code.yaml rules): (1) Remove syntax.cognitive_complexity, syntax.max_nesting_depth, and syntax.high_fanout from acdd lint diagnostic emission (syntax.rs + arch_lint.rs) while keeping cognitive_complexity and max_nesting_depth in node.details telemetry and keeping syntax.parse, syntax.fact_budget_exceeded, and syntax.intra_crate_type_homonym enforced in lint. Align docs/architecture/modularity.md, docs/reference/*, skills/acdd/SKILL.md, crates/acdd-core/src/lint.rs, and acdd.yaml accordingly. (2) Strengthen crates/acdd-state/src/profile/embedded/code.yaml with universal rules: extend upstream producers instead of re-implementing downstream parsers/resolvers, forbid single-use micro-helper slicing, and enforce Gate 0 architectural ownership audit on review. (3) Reconcile run profile with acdd sync --rebind-profile before final run_tests so run_tests attests all 6 lint categories.'
    - 'BUDGET_OVERRIDE: 8000 net non-test LOC approved by coordinator for task workspace-cycle-breaking-dedup-and-gate-ratchet (raising Decision #375 cap from 7,500 to 8,000) to accommodate user-requested authoring of docs/milestones/AR-005-typed-graph-core-linker-traits-and-idiomatic-cleanup.md (+251L) and ROADMAP.md alignment inside T5 extended scope.'
    - |-
      AR-004 Wave 4 Architectural Blueprint Decision (preserving T1-T5 slices + Decision #380):
      Wave 4 runs 3 disjoint-scope parallel tasks depending on T5 (workspace-cycle-breaking-dedup-and-gate-ratchet):
      1. T6 lint-engine-soundness-perf-and-rule-hygiene (max_net_loc=900):
         - crates/acdd-core/src/lint.rs + acdd.yaml: keep ArchRuleKind::ALL.len()==8 and Decision #380 (no syntax.cognitive_complexity/max_nesting_depth/high_fanout diagnostic emissions; telemetry stays in node.details); add opt-in unused_public_api flag on ArchitectureConfig (enabled in acdd.yaml, off by default).
         - crates/acdd-index/src/db/lint/arch.rs: single-parse-per-file/language batching for ast_pattern with fail-closed errors/diagnostics on search_page work_limited/incomplete; allow_only_dependencies external checks without silent bypass; most-specific-zone selection in eval_forbid_cycles; public-only must_implement skipping private serde/helper structs, matching enums/cross-file impls, removing line-span external heuristics.
         - db/lint/{rules,types,docs_lint,syntax,duplicates,dead_tails,unused_public_api}.rs + ast/fingerprints.rs + db/cycles.rs: admit possible confidence for possible_calls; track test vs prod lines separately in seen_in_coverage; flag end > source_lines and multi-match stale_symbol_range and retire unreachable docs.broken_doc_link from run_lint; use DependencyRegistry package roots and ComplexityLimits.severity in syntax.intra_crate_type_homonym; add Type-2 identifier-normalized AST fingerprints + LCS/Jaccard clone detection excluding tests by default; delete raw .rs callback scanner in dead_tails.rs and handle unresolved receiver methods conservatively; filter test/archive imports in db/cycles.rs.
         - docs/architecture/performance.md + acdd.yaml: remove duplicate INV-WORKSPACE-BOUNDARIES, retire ARCH-INDEX-DB-BOUNDARY-ENTRY in favor of Rust module visibility + ARCH-INDEX-DB-ISOLATION, close qualified Command::new / Connection::open / #![allow(dead_code)] / #[expect(dead_code)] bypasses, and clean up noisy rules.
      2. T7 rust-extractor-and-rust-linker-coverage-recovery (max_net_loc=450):
         - crates/acdd-index/src/languages/rust/mod.rs: relate_nominal_item skips attribute_item and comments inside enum_variant bodies and emits References for #[serde(default, skip_serializing_if, serialize_with, deserialize_with, with)] callable paths.
         - crates/acdd-index/src/linker/rust/{semantic_context,forwarding,profile_linker,resolution}.rs: propagate visible external Cargo re-exports across importing modules via RustForwarding; traverse intermediate RustForwarding re-exports on multi-segment paths; register trait methods and generic-bounded inherent methods in RustMembers; record trait_object_call as resolved when dynamic_dispatch implementors <= MAX_CANDIDATE_BRANCH_JOINS (8).
      3. T8 universal-value-flow-branch-joins-and-manifest-returns (max_net_loc=450):
         - crates/acdd-index/src/linker/semantic_context/type_resolution.rs: remove js/ts/vue-only gate in resolve_call_result so ReceiverTypeRule.call_returns evaluates across all compiled languages.
         - crates/acdd-index/src/linker/value_flow/{types,evaluate,initialize}.rs + languages/support/value_flow_core.rs: element-wise BranchJoin over Tuple/RustTuple and Option<T>/Unknown (preserving known fields); preserve monotonic static types across mutable reassignments in loops/branches; accurate typed CallbackParam classification.
         - crates/acdd-index/src/linker/resolution/imports.rs: universal cross-module external_aliases propagation across all language profiles.
         - Scope note: framework TOML manifests live in crates/acdd-index/src/languages/manifests/ (not dependency_registry/frameworks/); use acdd extend-scope if .toml edits are needed.
    deferred_findings:
    - 'Non-blocking review notes from ar04-reviewer-w1-flow-1 (for Task 4/5 hygiene): (1) typescript/mod.rs:1444-1552 still retains Document..URL WebIDL arms already covered by web.toml (and Web.Response for out-of-scope linker/javascript/semantic_context.rs:15); (2) newly added/rewritten functions exceeding warn complexity limits: support/value_flow_core.rs::collect_pattern_bindings (cognitive 56), python/value_flow.rs::Collector::new (cognitive 73, depth 7), semantic_context/type_resolution.rs::resolve_member_result (cognitive 58) & resolve_call_result (cognitive 82, depth 8); (3) linker/value_flow/{evaluate,initialize}.rs carry 7 #[allow(clippy::too_many_arguments)] annotations and a 3-node value_flow cycle; (4) manifests/syntax.rs:496 ReturnTypeSpec::as_str is an uncalled 3L accessor (dead_tail); (5) note: default-feature target/debug/build/acdd-index/bf67cf542c906f4d had stale pre-Task-2 rmeta from shared --target-dir AR-004 CLI runs—fresh feature builds (--features lang-go and --all-features) compile and pass 264/264 & 265/265 tests.'
    - 'Deferred non-blocking performance/hygiene notes from AR-004 integration_review Round 1: (1) `src/state_bridge/code_domain/context.rs:255-350` (`provide_external_callers`) took ~5m45s during `acdd claim integration_review` on AR-004''s 542-file diff because it parses all 542 baseline files via per-file `git show` and runs per-symbol `git grep` + Tree-sitter fallbacks for deleted/renamed symbols before truncating to `CODE_EXTERNAL_CALLERS_MAX_ITEMS` (12); early-stopping once 12 items are collected or batching `git cat-file --batch` / skipping fallback grep when `admitted_scope` is empty on milestone-wide integration reviews would drop claim latency from minutes to seconds. (2) Four migrated test files (`crates/acdd-index/src/ast/mod.rs:264`, `tests/languages/ast_patterns.rs:302`, `tests/languages/config.rs:84`, `tests/languages/html/syntax.rs:292`, `tests/mcp/context/previews_and_repair.rs:502`) duplicate the same ~10-line `PreparedAstMatcher::search_page` helper closure; consolidating into `tests/common` would eliminate test boilerplate.'
    - 'T8 independent review non-blocking follow-ups: (1) crates/acdd-index/src/linker/resolution/imports.rs:371-394 pre-existing Rust-only external_aliases fallback is now subsumed by the universal external_origin_at path at lines 355-369 and can be removed in a future cleanup; (2) crates/acdd-index/src/linker/value_flow/initialize.rs:674-685 replay_initial_bindings only re-evaluates flow_dependent RHS, so a conditional rebinding with a non-flow_dependent Unknown RHS whose prior binding resolves in Pass 2 could also be updated via flow_dependent || prev_target.is_some(); (3) tests/linker/method_semantics.rs:1886 uses .ts fixtures under #![cfg(all(feature = "lang-python", feature = "lang-rust"))] without #[cfg(feature = "lang-typescript")] (matching pre-existing rust_and_python_methods_are_classified_persisted_and_searchable).'
    - 'AR-004 integration_review non-blocking follow-ups for AR-005: (1) T6 upgrade of duplicates.rs to Type-2 local-identifier-normalized AST fingerprints surfaced 44 warn-level workspace blocks (0 error-level lint violations), including a 6L direct_members visibility block in crates/acdd-index/src/linker/rust/forwarding.rs:428-433 vs :499-504 (introduced in parallel T7 before T6 merged), a 17L hydration block in src/engine/pipeline/{delta.rs:1263-1279,invalidation.rs:452-468}, and pre-existing cross-crate blocks (evaluate_verify, split_top_level_args, intern_symbol, split_cfg_args) — consolidate in AR-005. (2) dead_tails query reports 0 dangling_calls and 0 unused_public_api errors, with 19 phantom_tested + 146 unreachable warnings dominated by live Rust fn-pointer callbacks (decode_*_row in acdd-state/src/store/sqlite/reader.rs, builtin_receiver_key, extract_dyn_trait_name), live iterator-called GraphPart::into_graph, and test-only helpers already inventoried in AR-005 — improve Rust fn-pointer callback edge recall in AR-005. (3) crates/acdd-index/src/linker/resolution/imports.rs:366-371 checks per-file resolver.external_aliases before external_origin_at(..., true); for intra-file sibling inline Rust modules (mod a { use ext::X; } mod b { use super::a::X; }), guard that branch with external_binding_visible as well during AR-005 LanguageLinker trait inversion.'
  metrics_summary:
    decisions_count: 28
    deferred_findings_count: 4
    failed_traces: 5
    passed_traces: 51
    task_receipts_count: 8
    tasks_cancelled: 0
    tasks_completed: 8
    tasks_total: 8
    total_claims: 83
    total_review_cycles: 4
    total_traces: 64
---

# AR-004: Language-Agnostic Linker Overhaul, Comprehensive Lint Rules, Cycle Breaking & Modularity Ratchet

## Outcome and Purpose

Following [AR-003](../milestones/archive/AR-003-unified-lint-engine-and-documentation-profile.md) and [LS-003](../milestones/archive/LS-003-universal-parser-tier1-coverage-and-shell-css-json.md), live queries against `.acdd/code-map.sqlite` and `acdd lint --categories all --severity warn` exposed coupled ceilings in linker generality, lint signal-to-noise (`2,862` warnings), executable architectural coverage, and intra-crate modularity:

1. **Language-Specific Linker Duplication & Remaining Coverage Gaps**:
   - Higher-order closure parameter typing was prototyped for Rust via magic string projections (`__rust_fn_param:<arg>:<param>`) and raw text re-parsing of `details["signature"]`, inflating `languages/rust/value_flow.rs` (`3,819` LOC) and `linker/rust/value_flow.rs` (`3,247` LOC) without benefiting Python, TypeScript/JS, Vue, or Tier-2 languages.
   - `LS-003`'s deferred declarative `ReceiverTypeRule.member_returns` / `call_returns` capability remains unwired for compound generic wrappers (`Result<T, E>`, `Option<(&str, &str)>`, `Promise<T>`), leaving hardcoded WebIDL arms in `typescript/mod.rs` (`1453-1486`), unlinked Playwright chains, and duplicated constructor/adapter `matches!` tables in [`external_constructor_result`](../../crates/acdd-index/src/linker/rust/value_flow.rs).
   - Rust's `11,665` remaining `unresolved` references concentrate in deterministic, non-heuristic gaps: `4,072` missing lexical/re-export/Cargo-chain candidates (`pub(crate) use support as syntax`, `Statement::query -> Rows -> Row::get`, `sha2::Digest::update`), `3,593` compound return chains (`rsplit_once('.').map_or`), `2,669` false-shadowing misses caused by untyped struct/tuple/enum-variant pattern destructuring (`let Node { id, name, .. } = n`), `654` intra-crate homonym/cfg receiver misses (`models::Graph` vs `compact_graph::Graph`), `~490` polymorphic `&dyn Trait` calls lacking trait-declaration contract edges, `52` local `impl Trait for $ty` declarative macro expansions (`enum_codec!`, `target_decoder!`), and `#[derive(clap::Parser)]` `Self` returns (`Cli::parse().run()` in [`run_from_env`](../../src/cli/mod.rs)).
2. **Lint Signal Noise Hiding Real Defects (`2,862` warnings)**:
   - **`duplicates` (`2,074`)**: [`collect_symbol_duplicates`](../../crates/acdd-index/src/db/lint/duplicates.rs) hashes raw formatted 5-line text slices, matching 5 consecutive closing braces `}` across 18 functions, multi-line formatted call headers (`responding_blocking(...)`), and shifted overlapping windows (`0..5`, `1..6`, `2..7`).
   - **`dead_tails` (`624`)**: [`load_dead_tails_graph`](../../crates/acdd-index/src/db/lint/dead_tails.rs) loses reachability from `src/main.rs` right at `Cli::parse().run().await` (`Calls: 0` on [`run_from_env`](../../src/cli/mod.rs)), falsely stranding `111` `dead_tails` and `83` `phantom_tested` functions in `src/` alongside unlinked intra-crate Rust helper calls.
   - **`cycles` (`164`)**: [`collect_cycle_components`](../../crates/acdd-index/src/db/lint/cycles.rs) conflates `imports` module dependency SCCs (`2..88` module nodes) with benign 1-node AST/schema function self-recursion (`79` of the first `100` items).
3. **Unenforced `LS-003` Boundaries, Unused Zones, God-Functions & Daemon Drift**:
   - `docs/architecture/indexing.md`, [ADR 0010](../adr/0010-static-value-flow-and-receiver-resolution.md), and [ADR 0011](../adr/0011-language-linkers-and-framework-manifests.md) currently contain `0` executable `acdd-rule` blocks; `forbid_cycles` is unused; zones `index_db` and `host_telemetry` in `acdd.yaml` have `0` referencing rules; and [`SyntaxGrammarSpec`](../../crates/acdd-index/src/languages/mod.rs) has `0` Tier-1 adopters.
   - `crates/acdd-index` harbors an `88`-module `imports` SCC (`languages <-> linker <-> models <-> semantic <-> ast`), `crates/acdd-core/src/config/` has a `5`-module SCC, intra-crate type homonyms (`Graph`, `Edge`) degrade self-indexing, deeply nested state-machine callables (`enter`, `value_depth`, `project`) accumulate extreme cognitive complexity and risk hitting `flow.fact_limit_exceeded` without structural complexity lints, raw `node.details["..."]` lookups bypass typed facts, and resident `acdd serve` daemons thrash `.acdd/code-map.sqlite` when `/home/oleksii/.local/bin/acdd` is upgraded in place.

---

## Completed Baseline Retained on `main`

The following zero-heuristic Rust extensions are already implemented and verified (`876` workspace tests passing, `0` failures, Rust resolved edges `35,748 -> 35,866` / `75.4549%`):

1. **Macro `$vis:vis` visibility propagation** in [`extract_invocation_name_and_variants`](../../crates/acdd-index/src/languages/rust/macros.rs) (`MacroVisibility`, `collect_rule_metavars`).
2. **Stdlib `Tuple` and `Range` manifests & projections** across `rust-std.toml`, [`wrapper_value_member`](../../crates/acdd-index/src/linker/rust/semantic_context.rs), `project_deref_wrapper`, and `TypeTarget::RustTuple`.
3. **Local trait impls on primitives/builtins** via [`RustMembers`](../../crates/acdd-index/src/linker/rust/profile_linker.rs) (`builtin_trait_members`, `lookup_builtin`) and [`lookup_rust_builtin_member`](../../crates/acdd-index/src/linker/semantic_context/mod.rs).
4. **External method projections & `rusqlite::Transaction`** in [`evaluate_tracked`](../../crates/acdd-index/src/linker/value_flow/mod.rs) and [`external_constructor_result`](../../crates/acdd-index/src/linker/rust/value_flow.rs) (preserving immediate `Some(None)` abort for unregistered Cargo crate homonyms).
5. **Initial higher-order closure typing & contract matrix** (`rust_zero_heuristic_resolution_extensions_matrix` in `tests/linker/method_semantics.rs`, using [`parse_rust_type_text`](../../crates/acdd-index/src/languages/rust/value_flow/types.rs) — scheduled in Wave 0/Wave 1 below to be elevated from `__rust_fn_param` into the language-agnostic `ValueExpr::CallbackParam` contract).

---

## Authorized Protected-File Scope

- `.acdd/profile.yaml` is explicitly admitted in `workspace-cycle-breaking-dedup-and-gate-ratchet` solely to extend `check_full_repo` (`acdd lint`) from default `arch,syntax,docs` to all 6 verified categories (`arch,syntax,docs,duplicates,dead_tails,cycles`) once analyzer noise and codebase findings are remediated.
- `Cargo.toml` and `crates/*/Cargo.toml` remain strictly untouched (`0` new external crate dependencies).
- [`ArchRuleKind`](../../crates/acdd-core/src/lint.rs) remains strictly locked at `8` closed variants (`ArchRuleKind::ALL.len() == 8`).

## Non-Goals & Negative Boundaries

- Never introduce name-only heuristic call matching on untyped receivers, trait-object single-impl guessing, or unregistered Cargo crate constructor fallbacks.
- Never import `tree_sitter` directly inside `crates/acdd-index/src/db/**` (preserving `ARCH-TREE-SITTER-CONFINED`); AST-statement fingerprinting belongs strictly in `crates/acdd-index/src/ast/` or extraction facts.
- Never add temporary lint exclusion lists or `#[allow(...)]` suppressions to silence `duplicates`, `dead_tails`, `cycles`, or structural `syntax.{cognitive_complexity,max_nesting_depth,high_fanout,fact_budget_exceeded}` checks; evaluate structural complexity via AST control-flow nesting and graph fan-out rather than naive physical line counts so flat lookup tables and `match` routers are never artificially fragmented.

---

## Tasks in this milestone

### task: upfront-contracts-ast-fingerprints-and-binary-guard

```yaml
task_ref: upfront-contracts-ast-fingerprints-and-binary-guard
target: Freeze ComplexityLimits on ArchitectureConfig, language-agnostic CallbackParam/CallableSignature and CompactGraph/CompactEdge leaf contracts across callers, single-pass AST statement fingerprinting + cognitive/nesting metrics in src/ast/, and canonical seam test registrations
scope:
- docs/milestones/AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul.md
- crates/acdd-core/src/lint.rs
- crates/acdd-index/src/models/
- crates/acdd-index/src/semantic/
- crates/acdd-index/src/typed_facts/
- crates/acdd-index/src/ast/
- crates/acdd-index/src/linker/mod.rs
- crates/acdd-index/src/linker/traits.rs
- crates/acdd-index/src/linker/projections.rs
- crates/acdd-index/src/linker/rust/resolution.rs
- crates/acdd-index/src/linker/python/semantic_relations.rs
- crates/acdd-index/src/linker/typescript/resolution.rs
- crates/acdd-index/src/linker/javascript/resolution.rs
- crates/acdd-index/src/linker/vue/resolution.rs
- crates/acdd-index/src/db/writer/mod.rs
- crates/acdd-index/src/db/writer/ingest.rs
- crates/acdd-index/src/db/writer/delta.rs
- crates/acdd-index/src/db/writer/batch.rs
- src/engine/pipeline/delta.rs
- tests/core.rs
- tests/core/
- tests/incremental/
- tests/linker.rs
- tests/linker/
- tests/languages.rs
- tests/mcp.rs
extended_scope:
- path: crates/acdd-index/src/lib.rs
  reason: Bump LOCAL_FACTS_BINARY_MAGIC to v5 and INDEX_SEMANTICS_VERSION epoch 15 for CallableSignature and StatementFingerprint TypedFacts binary layout
- path: crates/acdd-index/src/db/writer/cache/tests.rs
  reason: Bump LOCAL_FACTS_BINARY_MAGIC to v5 and INDEX_SEMANTICS_VERSION epoch 15 for CallableSignature and StatementFingerprint TypedFacts binary layout
- path: tests/languages/profiles.rs
  reason: Bump LOCAL_FACTS_BINARY_MAGIC to v5 and INDEX_SEMANTICS_VERSION epoch 15 for CallableSignature and StatementFingerprint TypedFacts binary layout
- path: crates/acdd-index/src/linker/contracts.rs
  reason: Update exhaustive ValueExpr matches for CallbackParam variant
- path: crates/acdd-index/src/linker/value_flow/mod.rs
  reason: Update exhaustive ValueExpr matches for CallbackParam variant
- path: tests/languages/profiles/baseline.rs
  reason: Normalize additive AR-004 cognitive_complexity, max_nesting_depth, and statement_fingerprints in pre_refactor baseline comparison
- path: docs/architecture/performance.md
  reason: Update INV-PERF-COMPACT-EDGE-COVERAGE-MODELS symbol claim from Edge to CompactEdge after compact_graph rename
tags:
- default
invariants:
- 'INV-AR004-LEAF-MODELS-SEMANTIC: crates/acdd-core/src/lint.rs exposes ComplexityLimits on ArchitectureConfig (preserving ArchRuleKind::ALL.len() == 8), and crates/acdd-index/src/{models,semantic} expose typed CallbackParam, CallableSignature, structural complexity NodeDetailKey slots, and disambiguated CompactGraph/CompactEdge DTOs with zero upward imports from languages or linker.'
- 'INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS: Single-pass AST statement fingerprinting and cognitive_complexity / max_nesting_depth computation live in crates/acdd-index/src/ast/ without leaking tree_sitter into src/db/, scoring flat exhaustive match/switch tables as +1 rather than per-arm line penalties.'
acceptance_criteria:
- 'AC-1: ArchitectureConfig.complexity (max_cognitive_complexity, max_nesting_depth, max_fanout, optional invariant binding), ValueExpr::CallbackParam, typed callable parameter metadata on TypedFacts, typed NodeDetailKey slots (including cognitive_complexity, max_nesting_depth, and fact_limit_exceeded), and CompactGraph / CompactEdge renames compile cleanly across all admitted workspace callers.'
- 'AC-2: crates/acdd-index/src/ast/ extracts language-agnostic AST statement sequences (skipping pure delimiter/comment trivia and collapsing multi-line formatting within a statement) alongside per-callable AST cognitive complexity and control-flow nesting depth.'
- 'AC-3: Pre-registered seam test modules compile cleanly in canonical test binaries with zero test-harness leaks.'
contract_revision: 2
status: completed
subtasks:
- subtask_ref: leaf-complexity-policy-value-expr-and-compact-graph
  title: Add ComplexityLimits to ArchitectureConfig in crates/acdd-core/src/lint.rs, ValueExpr::CallbackParam and typed callable signatures in semantic/ and typed_facts/, structural complexity/fact-budget NodeDetailKey fields, and rename compact_graph::{Graph,Edge} to CompactGraph/CompactEdge across all callers
  status: completed
  evidence: Added ComplexityLimits on ArchitectureConfig, ValueExpr::CallbackParam, CallableSignature, complexity NodeDetailKey slots, and CompactGraph/CompactEdge renames verified in build+review
- subtask_ref: ast-statement-fingerprints-and-cognitive-metrics
  title: Implement language-agnostic Tree-sitter AST statement normalizer plus AST cognitive_complexity (flat match = +1, nested control flow = +1 + depth) and max_nesting_depth extraction in crates/acdd-index/src/ast/
  status: completed
  evidence: Implemented single-pass AST statement fingerprinting and cognitive_complexity/max_nesting_depth tracker in crates/acdd-index/src/ast/{fingerprints.rs,visitor.rs} with ScopeFrame symmetry verified
- subtask_ref: pre-register-seam-tests
  title: Pre-register seam test modules in canonical test binaries and verify compilation with cargo clippy and focused tests
  status: completed
  evidence: Registered seam tests tests/core/ast_contracts.rs, tests/linker/callback_contracts.rs, tests/languages.rs cognitive_and_statement_contracts, and tests/mcp.rs executable_identity_digest_seam_baseline; all suites passed
- subtask_ref: validate-unused-arch-zones-and-test-seams
  title: Add UNUSED_ARCH_ZONE check in crates/acdd-state/src/validate.rs and pre-register seam test modules in canonical test binaries
  status: cancelled
  evidence: 'Moved UNUSED_ARCH_ZONE to lint-engine-ast-clones-cycles-and-rules (crates/acdd-index/src/db/lint/rules.rs) per blueprint decision #337 and spec commit 5f56d7957 to preserve INV-STATE-NO-INDEX'
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:upfront-contracts-ast-fingerprints-and-binary-guard.deliver
  gate_id: deliver
  worker_id: ar04-reviewer-w0-2
  claim_revision: 1
  contract_revision: 5
  candidate_tree: faf163fe7ea440c7e911983777405cc4247cf076
  committed_oid: d0f20c1665303e03a743be2a6eac63975772878c
  accepted_at: 2026-11-22T17:26:00.130Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-LEAF-MODELS-SEMANTIC: crates/acdd-core/src/lint.rs exposes ComplexityLimits on ArchitectureConfig (preserving ArchRuleKind::ALL.len() == 8), and crates/acdd-index/src/{models,semantic} expose typed CallbackParam, CallableSignature, structural complexity NodeDetailKey slots, and disambiguated CompactGraph/CompactEdge DTOs with zero upward imports from languages or linker.'
      - 'INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS: Single-pass AST statement fingerprinting and cognitive_complexity / max_nesting_depth computation live in crates/acdd-index/src/ast/ without leaking tree_sitter into src/db/, scoring flat exhaustive match/switch tables as +1 rather than per-arm line penalties.'
      verified:
      - INV-AR004-LEAF-MODELS-SEMANTIC
      - INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS
    review_summary: Deliver accepted Wave 0 contracts, AST statement fingerprints/cognitive metrics, binary magic v5, and pre-registered seam test modules.
```

### task: declarative-returns-and-agnostic-value-flow

```yaml
task_ref: declarative-returns-and-agnostic-value-flow
target: Wire compound generic member_returns/call_returns in .toml manifests, evaluate ValueExpr::CallbackParam, unified destructuring, lookup_builtin_member, and two-tier polymorphic trait/interface contract edges across shared linker phases, and migrate Tier-1 Python/TS/Web profiles onto SyntaxGrammarSpec
scope:
- crates/acdd-index/src/languages/manifests/
- crates/acdd-index/src/languages/support/
- crates/acdd-index/src/languages/python/
- crates/acdd-index/src/languages/typescript/
- crates/acdd-index/src/languages/vue/
- crates/acdd-index/src/languages/html/
- crates/acdd-index/src/linker/value_flow/
- crates/acdd-index/src/linker/semantic_context/
- crates/acdd-index/src/linker/resolution/
- tests/languages/
extended_scope:
- path: crates/acdd-index/src/linker/typescript/resolution.rs
  reason: Route builtin_property_chain through generalized lookup_builtin_member after deleting hardcoded WebIDL arms in typescript/mod.rs
- path: crates/acdd-index/src/linker/python/receivers.rs
  reason: Expose Python class implementors lookup for two-tier polymorphic Protocol/ABC dispatch
depends_on:
- upfront-contracts-ast-fingerprints-and-binary-guard
tags:
- default
invariants:
- 'INV-AR004-DECLARATIVE-MANIFEST-RETURNS: Chained external/stdlib method and function return types (including Result/Option/Tuple/Iterator/Promise/QuerySet wrappers) resolve from .toml manifests without hardcoded language method tables.'
- 'INV-AR004-POLYMORPHIC-CONTRACT-HONESTY: Calls on &dyn Trait, Protocol/ABC, and TS/Tier-2 interfaces link deterministically to the trait/interface method declaration (plus bounded PossibleCalls when closed) while never fabricating a single concrete struct Calls edge.'
acceptance_criteria:
- 'AC-1: ReceiverTypeRule.member_returns and call_returns support compound generic wrappers; playwright.toml, web.toml, django.toml, rust-std.toml, and rust-cargo.toml resolve multi-hop chains (Playwright locator/request/expect, str::rsplit_once -> Option<(&str, &str)>, rusqlite Statement::query -> Rows -> Row::get, sha2::Digest::update), and typescript/mod.rs hardcoded WebIDL arms (1453-1486) are deleted.'
- 'AC-2: Shared evaluate_tracked resolves ValueExpr::CallbackParam, unified struct/tuple/variant destructuring in ValueFlowCollectorCore, and language-agnostic lookup_builtin_member across Rust, Python, and TypeScript/JS.'
- 'AC-3: High-complexity nested functions in linker/value_flow/{mod.rs,initialize.rs} and languages/manifests/syntax.rs (evaluate_tracked, build_initialized, validate) are refactored within INV-BOUNDED-STRUCTURAL-COMPLEXITY bounds, and python/mod.rs + typescript/mod.rs adopt SyntaxGrammarSpec.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: compound-manifest-returns-and-webidl-removal
  title: Extend manifests/syntax.rs, semantic_context, and value_flow to project nested generic return wrappers from .toml manifests; wire playwright.toml, web.toml, django.toml, rust-std.toml, and rust-cargo.toml; delete hardcoded WebIDL method arms in typescript/mod.rs
  status: completed
  evidence: Implemented ReturnTypeSpec/ReturnTypeNode compile-time parser & projector (zero regexes), updated playwright.toml/web.toml/django.toml/rust-std.toml/rust-cargo.toml, removed hardcoded WebIDL arms from typescript/mod.rs; verified via cargo test --test languages (playwright_and_webidl_compound_return_chains, django_queryset_compound_return_and_item_projection, rust_compound_manifest_returns_tuple_option_result_and_sha2).
- subtask_ref: agnostic-callback-destructuring-and-builtin-lookup
  title: Evaluate ValueExpr::CallbackParam, ValueFlowCollectorCore struct/tuple destructuring, generalized lookup_builtin_member, and two-tier Trait/Interface contract edges across shared linker phases
  status: completed
  evidence: Evaluated ValueExpr::CallbackParam & CallableSignature across Python/TS extractors and shared linker phases, unified ValueFlowCollectorCore destructuring + push_callback_param, generalized lookup_builtin_member across semantic_context & linker/typescript/resolution.rs, and implemented two-tier polymorphic Protocol/Interface dispatch (bounded K<=8 possible_calls, widened >8); verified via agnostic_destructuring_callback_and_polymorphic_protocol_dispatch and typescript_destructuring_callback_and_interface_polymorphism_matrix.
- subtask_ref: tier1-syntax-grammar-spec-and-shared-linker-split
  title: Adopt SyntaxGrammarSpec in Python/TypeScript extractors and decompose high-cognitive-complexity / deeply nested logic in linker/value_flow/{mod.rs,initialize.rs} and languages/manifests/syntax.rs
  status: completed
  evidence: Adopted PYTHON_SPEC/TS_SPEC/JS_SPEC SyntaxGrammarSpec in python/mod.rs, typescript/mod.rs, vue/mod.rs; decomposed linker/value_flow/mod.rs -> evaluate.rs + initialize.rs helpers and SyntaxRule::validate into 6 focused helpers in syntax.rs; zero lint warnings across 31 rules and 0 warnings under cargo clippy -p acdd-index --all-targets --all-features -- -D warnings.
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:declarative-returns-and-agnostic-value-flow.deliver
  gate_id: deliver
  worker_id: ar04-reviewer-w1-flow-1
  claim_revision: 1
  contract_revision: 3
  candidate_tree: b35dc5cde1611e1da6f216a5c04a8a84e1d37d4a
  committed_oid: b2a480d7bb07d3ac4a7b6c1314220846c0771009
  accepted_at: 2026-11-22T19:27:21.046Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-DECLARATIVE-MANIFEST-RETURNS: Chained external/stdlib method and function return types (including Result/Option/Tuple/Iterator/Promise/QuerySet wrappers) resolve from .toml manifests without hardcoded language method tables.'
      - 'INV-AR004-POLYMORPHIC-CONTRACT-HONESTY: Calls on &dyn Trait, Protocol/ABC, and TS/Tier-2 interfaces link deterministically to the trait/interface method declaration (plus bounded PossibleCalls when closed) while never fabricating a single concrete struct Calls edge.'
      verified:
      - INV-AR004-DECLARATIVE-MANIFEST-RETURNS
      - INV-AR004-POLYMORPHIC-CONTRACT-HONESTY
    review_summary: Deliver accepted declarative-returns-and-agnostic-value-flow candidate
```

### task: rust-linker-precision-and-god-module-split

```yaml
task_ref: rust-linker-precision-and-god-module-split
target: Migrate Rust onto ValueExpr::CallbackParam and SyntaxGrammarSpec, close remaining zero-heuristic Rust gaps (#[derive(Parser)] Self, destructuring false-shadowing, re-export aliases, impl macros, intra-crate helper calls), delete duplicated constructor matches! tables, and decompose high-complexity Rust monoliths
scope:
- crates/acdd-index/src/languages/rust/
- crates/acdd-index/src/linker/rust/
- tests/linker/
extended_scope:
- path: crates/acdd-index/src/languages/manifests
  reason: Add compound Result/Option/Iterator return types to rust-std.toml and rust-cargo.toml so duplicated external_constructor_result/declared_adapter_result match tables can be deleted
- path: crates/acdd-index/src/ast/visitor.rs
  reason: Rename languages::rust::value_flow::Collector to RustValueFlowCollector for syntax.intra_crate_type_homonym
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/inferred.rs
  reason: Consult RustForwarding / resolve_rust_member_path in resolve_from_imported so pub(super)/pub(crate) re-exports resolve
- path: crates/acdd-index/src/linker/semantic_context/type_resolution.rs
  reason: Resolve child module nodes in resolve_rust_member_path for include!-declared modules and static RUST receiver lookup
- path: crates/acdd-index/src/linker/value_flow/evaluate.rs
  reason: Support RustWrapper is_some_and/is_none_or/ok_or_else and LocalKey::with callback parameter typing
depends_on:
- declarative-returns-and-agnostic-value-flow
tags:
- default
invariants:
- 'INV-AR004-RUST-NO-SHADOW-HACKS: __rust_fn_param and details["signature"] text parsing are deleted in favor of ValueExpr::CallbackParam, duplicated external constructor matches! tables are replaced by .toml manifest returns, and unregistered Cargo crate homonyms remain strictly unresolved.'
- 'INV-AR004-RUST-MODULAR-BOUNDS: Every production function in languages/rust/** and linker/rust/** satisfies INV-BOUNDED-STRUCTURAL-COMPLEXITY (cognitive_complexity, max_nesting_depth, fanout, and zero fact_budget_exceeded) with zero >= 5-statement clones and zero dead_tails.'
acceptance_criteria:
- 'AC-1: #[derive(clap::Parser)] associated Self calls (Cli::parse().run()), struct/tuple/enum-variant pattern destructuring (let Node { id, name, .. } = n, if let Ok/Err/Some), pub(crate) use module aliases, local impl Trait for $ty declarative macros (enum_codec!, target_decoder!), and intra-crate RustMembers/MacroVisibility helper calls resolve deterministically.'
- 'AC-2: False-shadowing diagnostics ("callee is shadowed by a parameter or local binding") drop for typed/destructured locals while genuine Fn/FnMut/FnOnce parameters classify as dynamic_callback.'
- 'AC-3: High-cognitive-complexity monoliths in languages/rust/value_flow.rs (enter, value_depth), linker/rust/value_flow.rs (project), and languages/rust/linker.rs (build) are decomposed into cohesive submodules obeying INV-BOUNDED-STRUCTURAL-COMPLEXITY, and rust/mod.rs adopts SyntaxGrammarSpec.'
contract_revision: 2
status: completed
subtasks:
- subtask_ref: rust-derive-self-destructuring-and-macro-impls
  title: 'Wire #[derive(clap::Parser)] Self constructors, struct/enum pattern destructuring, false-shadowing vs dynamic_callback classification, module re-export aliases, and impl Trait for $ty declarative macros while migrating __rust_fn_param to ValueExpr::CallbackParam and deleting redundant external_constructor_result matches! arms'
  status: completed
  evidence: Implemented derive Parser Self, struct/tuple/enum destructuring, pub(super)/pub(crate)/pub(in super...) re-exports, macro impl Trait/fn items, ValueExpr::CallbackParam + CallableSignature, removed redundant external_constructor_result std.str/String/serde_json arms in favor of rust-std/rust-cargo TOML, and preserved multi-line/attribute Rust docstrings in syntax_doc.
- subtask_ref: decompose-rust-extractor-and-linker-monoliths
  title: Adopt SyntaxGrammarSpec in languages/rust/mod.rs and decompose deeply nested / high-cognitive-complexity functions in languages/rust/{value_flow,linker}.rs and linker/rust/value_flow.rs within INV-BOUNDED-STRUCTURAL-COMPLEXITY bounds
  status: completed
  evidence: Adopted RUST_SPEC, split languages/rust/value_flow.rs into value_flow/{mod,collector,expr,macros,types}.rs, decomposed monoliths, and resolved intra-crate/module-reexport paths to 0 syntax, 0 duplicates, and 0 dead_tails across languages/rust and linker/rust.
- subtask_ref: rust-precision-contract-matrices
  title: Extend table-driven contract matrices in tests/linker/ covering derive Self, destructuring, macro impls, re-export aliases, dynamic_callback classification, and homonym rejection
  status: completed
  evidence: Added table-driven contract matrices in tests/linker/callback_contracts.rs and tests/linker/method_semantics.rs; cargo test --test linker (83/83), languages, and commitment_integrity all passed.
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:rust-linker-precision-and-god-module-split.deliver
  gate_id: deliver
  worker_id: ar04-reviewer-w2-rust-2
  claim_revision: 1
  contract_revision: 6
  candidate_tree: 96fabecb0b740534c339b08742c094ec6b8c3c54
  committed_oid: d5c87373c1a94ce9ac9255dbfa6bb1dd5f8af249
  accepted_at: 2026-11-22T21:40:55.393Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-RUST-NO-SHADOW-HACKS: __rust_fn_param and details["signature"] text parsing are deleted in favor of ValueExpr::CallbackParam, duplicated external constructor matches! tables are replaced by .toml manifest returns, and unregistered Cargo crate homonyms remain strictly unresolved.'
      - 'INV-AR004-RUST-MODULAR-BOUNDS: Every production function in languages/rust/** and linker/rust/** satisfies INV-BOUNDED-STRUCTURAL-COMPLEXITY (cognitive_complexity, max_nesting_depth, fanout, and zero fact_budget_exceeded) with zero >= 5-statement clones and zero dead_tails.'
      verified: []
```

### task: lint-engine-ast-clones-cycles-and-rules

```yaml
task_ref: lint-engine-ast-clones-cycles-and-rules
target: Upgrade duplicates.rs to AST-statement clone windows, separate module import SCCs from call recursion in cycles.rs, promote dangling_call to error, add docs.unused_arch_zone in rules.rs without breaking INV-STATE-NO-INDEX or enforced_invariant_ids signature, enforce INV-BOUNDED-STRUCTURAL-COMPLEXITY in syntax.rs, and author executable LS-003/AR-004 invariants across docs and acdd.yaml
scope:
- crates/acdd-index/src/db/lint/
- acdd.yaml
- docs/architecture/
- docs/adr/
- tests/core.rs
extended_scope:
- path: tests/core/arch_lint.rs
  reason: Place structural lint engine contract matrices in canonical tests/core/arch_lint.rs module while keeping tests/core.rs as a pure submodule index
depends_on:
- upfront-contracts-ast-fingerprints-and-binary-guard
tags:
- default
invariants:
- 'INV-AR004-LOCKED-ARCH-RULE-ENUM: ArchRuleKind::ALL.len() == 8 remains unchanged; structural complexity, nesting, fanout, fact-budget, and homonym checks evaluate in LintCategory::Syntax (DEFAULT_VERIFY) backed by ArchitectureConfig.complexity and wired into enforced_invariant_ids(conn, config_rules) via resolve_architecture(conn, None) while preserving its public signature.'
- 'INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES: Closing-brace blocks and wrapped single-statement calls produce 0 duplicate warnings, adjacent AST-statement clone windows coalesce into maximal spans, flat lookup/router match tables incur no per-arm complexity penalty, and 1-node self-recursive functions produce 0 cycles.scc warnings.'
acceptance_criteria:
- 'AC-1: collect_symbol_duplicates in duplicates.rs compares K-statement AST fingerprint sequences from src/ast/ and merges overlapping adjacent matches into single maximal clone groups.'
- 'AC-2: cycles.rs emits cycles.scc warnings strictly on multi-node module/file import SCCs (kind = imports, len >= 2); dead_tails.dangling_call emits LintSeverity::Error; syntax.rs evaluates syntax.cognitive_complexity, syntax.max_nesting_depth, syntax.high_fanout, syntax.fact_budget_exceeded, and syntax.intra_crate_type_homonym bound to INV-BOUNDED-STRUCTURAL-COMPLEXITY while preserving enforced_invariant_ids signature; and rules.rs emits docs.unused_arch_zone (LintSeverity::Warn) whenever declared architecture.zones are unreferenced.'
- 'AC-3: Canonical INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md plus executable acdd-rule blocks in docs/architecture/indexing.md, ADR 0010, ADR 0011, and acdd.yaml enforce Layer 2 linker/extractor boundaries (boundary_entry + forbid_dependency), ManifestParser must_implement, hot-path Regex::new / shrink_to_fit / raw node.details[key] ast_pattern bans, index_db and host_telemetry zone rules (0 docs.unused_arch_zone warnings), and forbid_cycles on acyclic zones.'
contract_revision: 2
status: completed
subtasks:
- subtask_ref: ast-clone-windows-and-cycle-separation
  title: Switch duplicates.rs to AST-statement fingerprint windows with maximal-span coalescing, separate imports module SCCs from calls recursion in cycles.rs, and promote dead_tails.dangling_call to Error
  status: completed
  evidence: duplicates.rs uses StatementFingerprint windows from local_facts with maximal-span coalescing; cycles.rs separates imports SCCs (len>=2) from call recursion; dead_tails.dangling_call emits Error; verified via tests/core.rs::arch_lint::ast_clone_coalescing_and_import_cycle_separation_contract_matrix and query_context::search_and_paging::duplicates_paged_symbol_clone_matrix.
- subtask_ref: syntax-structural-complexity-and-homonym-lints
  title: Implement syntax.cognitive_complexity, syntax.max_nesting_depth, syntax.high_fanout, syntax.fact_budget_exceeded, syntax.intra_crate_type_homonym in syntax.rs, add docs.unused_arch_zone in rules.rs, and wire ArchitectureConfig.complexity into enforced_invariant_ids via resolve_architecture without signature changes
  status: completed
  evidence: syntax.rs evaluates syntax.{cognitive_complexity,max_nesting_depth,high_fanout,fact_budget_exceeded,intra_crate_type_homonym} bound to ArchitectureConfig.complexity; rules.rs emits docs.unused_arch_zone (Warn) and preserves enforced_invariant_ids signature via resolve_architecture(conn, None); verified via tests/core.rs::arch_lint::structural_complexity_homonym_unused_zone_and_enforced_invariant_contract_matrix.
- subtask_ref: executable-ls003-complexity-and-zone-rules
  title: Declare INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md and acdd.yaml, and author executable acdd-rule blocks across docs/architecture/indexing.md, ADR 0010, ADR 0011, and acdd.yaml, backed by contract matrices in tests/core.rs
  status: completed
  evidence: Declared INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md and acdd.yaml complexity config; added executable acdd-rule blocks across docs/architecture/indexing.md, ADR 0010, ADR 0011, and acdd.yaml covering index_db, host_telemetry, Layer 2 boundaries, ManifestParser, hot-path ast_pattern bans, and acyclic zones; verified 0 errors across lint, docs check, and validate.
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:lint-engine-ast-clones-cycles-and-rules.deliver
  gate_id: deliver
  worker_id: ar04-reviewer-w1-lint-1
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 6a833b70707aa6d9301440976f57ed216d50c702
  committed_oid: ead35da15fdba599e51cb782e137434ec1b382c6
  accepted_at: 2026-11-22T18:03:58.679Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-LOCKED-ARCH-RULE-ENUM: ArchRuleKind::ALL.len() == 8 remains unchanged; structural complexity, nesting, fanout, fact-budget, and homonym checks evaluate in LintCategory::Syntax (DEFAULT_VERIFY) backed by ArchitectureConfig.complexity and wired into enforced_invariant_ids(conn, config_rules) via resolve_architecture(conn, None) while preserving its public signature.'
      - 'INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES: Closing-brace blocks and wrapped single-statement calls produce 0 duplicate warnings, adjacent AST-statement clone windows coalesce into maximal spans, flat lookup/router match tables incur no per-arm complexity penalty, and 1-node self-recursive functions produce 0 cycles.scc warnings.'
      verified:
      - INV-AR004-LOCKED-ARCH-RULE-ENUM
      - INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES
```

### task: workspace-cycle-breaking-dedup-and-gate-ratchet

```yaml
task_ref: workspace-cycle-breaking-dedup-and-gate-ratchet
target: 'Break all remaining intra-crate module import SCCs (enabling workspace-wide forbid_cycles: error), refactor remaining INV-BOUNDED-STRUCTURAL-COMPLEXITY violations across acdd-state/acdd-index/src/, eliminate all verified AST clones and proven dead_tails, add acdd serve binary-drift reload guard, and ratchet .acdd/profile.yaml check_full_repo to enforce all 6 lint categories'
scope:
- crates/acdd-core/src/
- crates/acdd-index/src/
- crates/acdd-state/src/
- src/
- .acdd/profile.yaml
- docs/reference/
- skills/acdd/
- tests/mcp.rs
extended_scope:
- path: tests
  reason: Allow updating test imports for SymbolCatalog/LanguageProfileLinker, breaking crates/acdd-state/tests SCC, and enabling workspace-wide forbid_cycles in acdd.yaml
- path: crates/acdd-state/tests
  reason: Allow updating test imports for SymbolCatalog/LanguageProfileLinker, breaking crates/acdd-state/tests SCC, and enabling workspace-wide forbid_cycles in acdd.yaml
- path: acdd.yaml
  reason: Allow updating test imports for SymbolCatalog/LanguageProfileLinker, breaking crates/acdd-state/tests SCC, and enabling workspace-wide forbid_cycles in acdd.yaml
- path: docs/AGENTS.md
  reason: Update canonical doc symbol links moved during SCC breaking and Coverage->CompactCoverage homonym disambiguation so ARCH-DOCS-INTEGRITY-STRICT passes
- path: docs/adr
  reason: Update canonical doc symbol links moved during SCC breaking and Coverage->CompactCoverage homonym disambiguation so ARCH-DOCS-INTEGRITY-STRICT passes
- path: docs/architecture
  reason: Update canonical doc symbol links moved during SCC breaking and Coverage->CompactCoverage homonym disambiguation so ARCH-DOCS-INTEGRITY-STRICT passes
- path: docs/architecture/performance.md
  reason: Update INV-PERF-BORROWED-MAP-KEY-EXPORT-INDEXING link target to canonical LanguageLinker owner in languages/profile.rs (re-exported at linker/traits.rs) to satisfy ARCH-DOCS-INTEGRITY-STRICT alongside acyclic LanguageProfile/LanguageLinker
- path: docs/runbooks
  reason: 'Decision #373: remove docs/runbooks/migration-0.6.md, update docs/runbooks/README.md, and reconcile INV-SURFACE-COMPATIBILITY in src/AGENTS.md'
- path: src/AGENTS.md
  reason: 'Decision #373: remove docs/runbooks/migration-0.6.md, update docs/runbooks/README.md, and reconcile INV-SURFACE-COMPATIBILITY in src/AGENTS.md'
- path: CHANGELOG.md
  reason: Update broken links to deleted docs/runbooks/migration-0.6.md
- path: INSTALL.md
  reason: Update broken links to deleted docs/runbooks/migration-0.6.md
- path: crates/acdd-index/build.rs
  reason: 'Round 6 Decision #373 / #370 / #248 scope alignment: unused_public_api lint & cycle rules (acdd.yaml), build.rs profile visibility narrowing, legacy 0.5.x doc/runbook removal & ADR/architecture updates, and whitebox test caller migrations across tests/ and crates/acdd-state/tests/'
- path: docs/milestones
  reason: 'Align ADRs, modularity.md, crate AGENTS.md, acdd.yaml, and matrix tests with unused_public_api + Decision #380'
- path: crates/acdd-index/AGENTS.md
  reason: 'Align ADRs, modularity.md, crate AGENTS.md, acdd.yaml, and matrix tests with unused_public_api + Decision #380'
- path: ROADMAP.md
  reason: Author next architectural milestone AR-005 and align ROADMAP.md per docs/AGENTS.md
depends_on:
- rust-linker-precision-and-god-module-split
- lint-engine-ast-clones-cycles-and-rules
tags:
- default
invariants:
- 'INV-AR004-WORKSPACE-ACYCLIC-AND-CLEAN: Workspace-wide forbid_cycles (severity: error), INV-BOUNDED-STRUCTURAL-COMPLEXITY (0 cognitive_complexity / max_nesting_depth / high_fanout / fact_budget_exceeded errors), and acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles report 0 errors and 0 unresolved hygiene regressions.'
- 'INV-AR004-DAEMON-BINARY-COHERENCE: Long-lived acdd serve / MCP processes detect on-disk binary replacement via executable identity digest and reload/re-exec or fail-safe cleanly without index rebuild thrashing.'
acceptance_criteria:
- 'AC-1: The 88-node acdd-index module SCC, 5-node acdd-core/src/config SCC, and any src/ / acdd-state module import cycles are decoupled into acyclic DAGs with forbid_cycles: error active across all workspace zones.'
- 'AC-2: Remaining production functions violating INV-BOUNDED-STRUCTURAL-COMPLEXITY (e.g. machine/submit.rs::evaluate_pass, db/writer/ingest.rs::persist_files_impl, db/lint/docs_lint.rs::collect_doc_findings, runner/mod.rs::run_isolated_process_unix, while flat lookup/router tables stay intact) are refactored cleanly; MCP tool dispatchers in src/mcp/tools/code_tools.rs, acdd-core helpers, pipeline, and state_bridge clones are consolidated; and proven dead_tails / phantom_tested helpers are removed or gated under #[cfg(test)].'
- 'AC-3: src/mcp/server.rs and src/cli/reload.rs prevent stale-binary index ping-pong on binary upgrades; docs/reference/ and skills/acdd/ reflect all new lint rules and ComplexityLimits config; and .acdd/profile.yaml check_full_repo passes full 6-category acdd lint, cargo test --workspace --all-targets, docs check, and validate.'
contract_revision: 2
status: completed
subtasks:
- subtask_ref: break-intra-crate-module-sccs
  title: 'Use code_map_query(operation=''slice'') to break remaining module import SCCs in acdd-index, acdd-core/src/config, acdd-state, and src/, then enable workspace-wide forbid_cycles: error'
  status: completed
  evidence: 'Broke intra-crate module import SCCs across acdd-core, acdd-index, acdd-state, and src/; enabled forbid_cycles: error in acdd.yaml; acdd lint cycles=0, acdd query run cycles=0.'
- subtask_ref: consolidate-clones-complexity-outliers-and-dead-tails
  title: 'Refactor remaining INV-BOUNDED-STRUCTURAL-COMPLEXITY outliers across acdd-state/acdd-index/src/, extract shared MCP tool dispatch and acdd-core/pipeline/state_bridge helpers to eliminate verified AST clones, and prune or #[cfg(test)]-gate proven dead_tails'
  status: completed
  evidence: Eliminated AST clones (acdd lint duplicates=0, acdd query run duplicates=0), split complexity outliers, cleaned intra-crate homonyms, and removed/test-gated dead_tails (acdd query run dead_tails=0; lint errors=0 across all 6 categories).
- subtask_ref: daemon-binary-guard-docs-and-profile-ratchet
  title: Add executable identity drift detection/reload for acdd serve, align docs/reference/ and skills/acdd/ with ComplexityLimits and new syntax rules, and ratchet .acdd/profile.yaml check_full_repo to enforce all 6 lint categories
  status: completed
  evidence: Implemented ExecutableIdentity binary-drift guard in acdd-core/fs.rs + src/mcp/server.rs + tests/mcp.rs::binary_identity_guard; aligned docs/reference/ and skills/acdd/; ratcheted .acdd/profile.yaml tasks.build and run_tests to all 6 lint categories; 895 workspace tests + clippy + validate pass with 0 /tmp leaks.
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:workspace-cycle-breaking-dedup-and-gate-ratchet.deliver
  gate_id: deliver
  worker_id: ar04-polish-coordinator
  claim_revision: 3
  contract_revision: 9
  candidate_tree: 7ccf997ff6c86245a9a6f6cbb9671924d280ac2a
  committed_oid: 1e395fae0ab8372fd2924213fa404bc00be91f1a
  accepted_at: 2026-11-24T00:43:58.507Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-WORKSPACE-ACYCLIC-AND-CLEAN: Workspace-wide forbid_cycles (severity: error), INV-BOUNDED-STRUCTURAL-COMPLEXITY (0 cognitive_complexity / max_nesting_depth / high_fanout / fact_budget_exceeded errors), and acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles report 0 errors and 0 unresolved hygiene regressions.'
      - 'INV-AR004-DAEMON-BINARY-COHERENCE: Long-lived acdd serve / MCP processes detect on-disk binary replacement via executable identity digest and reload/re-exec or fail-safe cleanly without index rebuild thrashing.'
      verified: []
```

### task: lint-engine-soundness-perf-and-rule-hygiene

```yaml
task_ref: lint-engine-soundness-perf-and-rule-hygiene
target: 'Fix all linter soundness/performance pitfalls: single-parse fail-closed ast_pattern evaluation, possible_calls confidence matching, allow_only_dependencies external checks, seen_in_coverage test/prod separation, forbid_cycles zone specificity + INV-WORKSPACE-BOUNDARIES deduplication, public-only must_implement, docs.stale_symbol_range out-of-bounds checks, Type-2 duplicate normalization, dead_tails raw-scanner removal + opt-in unused_public_api, and noisy acdd.yaml rule cleanup'
scope:
- crates/acdd-core/src/lint.rs
- crates/acdd-index/src/ast/fingerprints.rs
- crates/acdd-index/src/db/cycles.rs
- crates/acdd-index/src/db/lint/
- acdd.yaml
- .acdd/profile.yaml
- docs/architecture/
- docs/adr/
- docs/reference/
- skills/acdd/
- tests/core/arch_lint.rs
- tests/core/ast_contracts.rs
extended_scope:
- path: crates/acdd-index/src/ast/search.rs
  reason: Batch ast_pattern Tree-sitter parsing once per file/language in ast::search while preserving ARCH-TREE-SITTER-CONFINED
depends_on:
- workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
- default
invariants:
- 'INV-AR004-SOUND-LINT-ENGINE: ast_pattern parses each file once per language and fails closed on work_limited or timeout; edge_matches_rule checks possible_calls; allow_only_dependencies checks externals when configured; dead_tails uses zero raw .rs disk scanning; and duplicates normalization handles Type-2 local variable renames.'
acceptance_criteria:
- 'AC-1: db/lint/arch.rs batches Tree-sitter parsing once per file/language and returns explicit diagnostics/errors when search_page hits work_limited or incomplete; db/lint/rules.rs admits possible confidence for possible_calls; allow_only_dependencies checks externals without silent bypass; seen_in_coverage tracks test vs production lines accurately; forbid_cycles selects the most specific zone; duplicate INV-WORKSPACE-BOUNDARIES is removed from docs/architecture/performance.md; ARCH-INDEX-DB-BOUNDARY-ENTRY is retired in favor of Rust module visibility + ARCH-INDEX-DB-ISOLATION; and qualified Command::new / Connection::open / #![allow(dead_code)] / #[expect(dead_code)] bypasses are closed.'
- 'AC-2: must_implement skips private serde/helper structs, matches enums/cross-file impls, and removes line-span external heuristics; docs_lint.rs flags end > source_lines and multi-match stale_symbol_range and retires unreachable docs.broken_doc_link from run_lint; syntax.intra_crate_type_homonym uses DependencyRegistry package roots and ComplexityLimits severity; ast/fingerprints.rs + db/lint/duplicates.rs perform Type-2 identifier-normalized and LCS/Jaccard clone detection excluding tests by default; db/lint/dead_tails.rs deletes its remaining raw .rs callback scanner and handles unresolved receiver methods conservatively; db/lint/unused_public_api.rs becomes opt-in via ArchitectureConfig (enabled in acdd.yaml, off by default for libraries); and db/cycles.rs filters test/archive imports.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: harden-arch-ast-pattern-confidences-and-rules
  title: Fix ast_pattern single-parse fail-closed execution, possible_calls confidence matching, allow_only externals, seen_in_coverage test masking, forbid_cycles zone specificity, must_implement noise, and clean up acdd.yaml/docs/architecture rules
  status: completed
  evidence: 'Verified in T6.review #327 (candidate 830c899c6d9c): ast_pattern single-parse, InterfaceDispatchIndex direct-edge + innermost-impl dispatch, allow_only/seen_in_coverage/forbid_cycles/must_implement fixes, and tests/core/arch_lint.rs + ast_contracts.rs'
- subtask_ref: harden-docs-syntax-duplicates-dead-tails-and-cycles
  title: 'Fix docs_lint out-of-bounds #L anchors, syntax package boundaries, Type-2 duplicate fingerprints/similarity, opt-in unused_public_api config, dead_tails raw-scanner removal, and cycles test filtering with matrix tests in tests/core/arch_lint.rs'
  status: completed
  evidence: 'Verified in T6.review #327 (candidate 830c899c6d9c): docs_lint anchor bounds, syntax boundaries, Type-2 duplicate fingerprints/LCS similarity, unused_public_api opt-in, dead_tails/cycles fixes, 48/48 acdd-core, 99/99 acdd-index, 75/75 core, 9/9 commitment_integrity'
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:lint-engine-soundness-perf-and-rule-hygiene.deliver
  gate_id: deliver
  worker_id: ar04-t6-deliverer
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 830c899c6d9cbbb04720008bb19b0b8d8b06e2de
  committed_oid: d2ff0a9c6e5959f8e83cf21457543ed34ec345a7
  accepted_at: 2026-11-24T02:12:54.563Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-SOUND-LINT-ENGINE: ast_pattern parses each file once per language and fails closed on work_limited or timeout; edge_matches_rule checks possible_calls; allow_only_dependencies checks externals when configured; dead_tails uses zero raw .rs disk scanning; and duplicates normalization handles Type-2 local variable renames.'
      verified: []
```

### task: rust-extractor-and-rust-linker-coverage-recovery

```yaml
task_ref: rust-extractor-and-rust-linker-coverage-recovery
target: 'Eliminate post-AR-004 Rust unresolved regressions (~2,450+ entries): skip enum_variant attribute_item tokens, propagate external re-exports (Syntax) via RustForwarding, align trait_object_call with bounded polymorphic resolved status and trait-node member registration, admit generic-bounded inherent methods in RustMembers, emit serde callback references, and resolve intermediate module re-exports in multi-segment paths'
scope:
- crates/acdd-index/src/languages/rust/
- crates/acdd-index/src/linker/rust/
- tests/languages/rust/
depends_on:
- workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
- default
invariants:
- 'INV-AR004-RUST-RECOVERY-NO-HEURISTICS: Every recovered Rust resolution edge derives from AST attribute/type extraction, explicit RustForwarding re-exports, inherent/trait definitions, or bounded dynamic dispatch (<= K=8) with zero name-only guessing.'
acceptance_criteria:
- 'AC-1: languages/rust/mod.rs relate_nominal_item skips attribute_item and comments inside enum_variant bodies (~400 bogus unresolved refs removed) and emits References for #[serde(default, skip_serializing_if, serialize_with, deserialize_with, with)] callable paths.'
- 'AC-2: linker/rust/semantic_context.rs propagates visible external Cargo re-exports (e.g. pub(crate) use tree_sitter::Node as Syntax) across importing modules (~1,450+ unresolved refs recovered); linker/rust/resolution.rs + profile_linker.rs register trait methods in RustMembers for ValueFlow return propagation and record trait_object_call as resolved when trait implementors <= MAX_CANDIDATE_BRANCH_JOINS (~504 unresolved refs recovered); inherent methods with generic bounds (e.g. CompactGraph) are admitted in RustMembers (~106+ unresolved refs recovered); and multi-segment paths traverse intermediate RustForwarding re-exports.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: fix-rust-enum-attr-bug-and-serde-callback-refs
  title: 'Fix enum_variant attribute_item type extraction bug and extract #[serde(...)] callback references in languages/rust/'
  status: completed
  evidence: 'Verified in T7 build #315 & Neon review #321: skipped attribute_item/comments inside enum_variant bodies and extracted #[serde(...)] callback references via AST attribute->token_tree walk.'
- subtask_ref: recover-rust-reexports-trait-objects-and-inherent-bounds
  title: Propagate external re-exports (Syntax), align trait_object_call + trait member registration with polymorphic bounds, admit generic inherent methods in RustMembers, and resolve intermediate module re-exports with regression tests in tests/languages/rust/
  status: completed
  evidence: 'Verified in T7 build #315 & Neon review #321: propagated external Cargo re-exports (Syntax), unified RustForwarding::resolve_path, registered trait methods + generic inherent methods in RustMembers, and recorded resolved for bounded trait_object_call; unresolved 14109->10931 (-3178), 32/32 tests/languages rust:: pass.'
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:rust-extractor-and-rust-linker-coverage-recovery.deliver
  gate_id: deliver
  worker_id: ar04-polish-coordinator
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 5bd461fd7ff732a38f03b35cac51a908b2cfc1df
  committed_oid: fd6e721e678eb33015750ca29a0daae5184a3606
  accepted_at: 2026-11-24T01:51:38.482Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-RUST-RECOVERY-NO-HEURISTICS: Every recovered Rust resolution edge derives from AST attribute/type extraction, explicit RustForwarding re-exports, inherent/trait definitions, or bounded dynamic dispatch (<= K=8) with zero name-only guessing.'
      verified: []
```

### task: universal-value-flow-branch-joins-and-manifest-returns

```yaml
task_ref: universal-value-flow-branch-joins-and-manifest-returns
target: 'Fix universal cross-language resolution gaps: unlock ReceiverTypeRule.call_returns across all compiled languages, perform element-wise Tuple/Option BranchJoin without collapsing known tuple elements to Unknown, preserve monotonic static types across loop/branch reassignments (let mut scope), propagate cross-module external_aliases universally, and classify typed CallbackParam calls accurately'
scope:
- crates/acdd-index/src/languages/support/value_flow_core.rs
- crates/acdd-index/src/linker/value_flow/
- crates/acdd-index/src/linker/semantic_context/
- crates/acdd-index/src/linker/resolution/
- crates/acdd-index/src/languages/dependency_registry/frameworks/
- tests/linker/method_semantics.rs
- tests/linker/callback_contracts.rs
depends_on:
- workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
- default
invariants:
- 'INV-AR004-UNIVERSAL-VALUE-FLOW-PARITY: Declarative call_returns rules, element-wise Tuple/Option BranchJoin, monotonic loop variable typing, and external_aliases inheritance apply uniformly across all supported language profiles rather than hardcoded JS/TS/Vue or Rust branches.'
acceptance_criteria:
- 'AC-1: linker/semantic_context/type_resolution.rs resolve_call_result evaluates declarative ReceiverTypeRule.call_returns across all languages (removing the js/ts/vue-only gate) and resolves common external type wrappers cleanly.'
- 'AC-2: linker/value_flow/evaluate.rs performs element-wise BranchJoin over Tuple/RustTuple and Option<T>/Unknown (preserving known fields such as match { None => (Default, None), Some => (Val, Some(x)) }); ValueFlowCollectorCore/value_flow preserve monotonic static types for reassigned mutables in loops/branches (e.g. scope = scope.rsplit_once(...)); linker/resolution/imports.rs propagates external_aliases across indexed modules universally; and regression tests pass in tests/linker/{method_semantics,callback_contracts}.rs.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: unlock-manifest-call-returns-and-universal-external-aliases
  title: Enable ReceiverTypeRule.call_returns for all languages in semantic_context and propagate cross-module external_aliases in resolution/imports.rs
  status: completed
  evidence: 'Verified in T8 build #317 & Neon review #322: enabled ReceiverTypeRule.call_returns across all languages in type_resolution.rs and propagated cross-module external_aliases in imports.rs + preparation.rs.'
- subtask_ref: elementwise-tuple-branch-join-and-monotonic-loop-typing
  title: Implement element-wise Tuple/Option BranchJoin in value_flow/evaluate.rs, monotonic mutable reassignment preservation in ValueFlowCollectorCore, and add tests in tests/linker/{method_semantics,callback_contracts}.rs
  status: completed
  evidence: 'Verified in T8 build #317 & Neon review #322: element-wise Tuple/Option/Result BranchJoin in evaluate.rs, monotonic mutable reassignment preservation in initialize.rs/lookup.rs (without static_variable_language), and passing matrix tests in tests/linker/{method_semantics,callback_contracts}.rs (89/89).'
receipt:
  run_id: AR-004-agnostic-link-20261122144815-1445895-1
  node_cursor: root:universal-value-flow-branch-joins-and-manifest-returns.deliver
  gate_id: deliver
  worker_id: ar04-polish-coordinator
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 93ef0caebdadc1d2b37ad91dc2f946ee67fdee84
  committed_oid: bcf8fe611f48aaf99e63bd11c441bf441ae6ef77
  accepted_at: 2026-11-24T01:57:18.247Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR004-UNIVERSAL-VALUE-FLOW-PARITY: Declarative call_returns rules, element-wise Tuple/Option BranchJoin, monotonic loop variable typing, and external_aliases inheritance apply uniformly across all supported language profiles rather than hardcoded JS/TS/Vue or Rust branches.'
      verified: []
```
