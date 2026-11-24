---
id: AR-004-agnostic-linker-lint-hygiene-and-modularity-overhaul
title: "Language-Agnostic Linker Overhaul, Comprehensive Lint Rules, Cycle Breaking & Modularity Ratchet"
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
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
required_skills:
  - acdd
  - contract-planning
  - code-quality
  - test-suite-rules
  - agent-instructions
repositories: []
linked_to: []
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
target: "Freeze ComplexityLimits on ArchitectureConfig, language-agnostic CallbackParam/CallableSignature and CompactGraph/CompactEdge leaf contracts across callers, single-pass AST statement fingerprinting + cognitive/nesting metrics in src/ast/, and canonical seam test registrations"
contract_revision: 2
depends_on: []
tags:
  - default
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
invariants:
  - 'INV-AR004-LEAF-MODELS-SEMANTIC: crates/acdd-core/src/lint.rs exposes ComplexityLimits on ArchitectureConfig (preserving ArchRuleKind::ALL.len() == 8), and crates/acdd-index/src/{models,semantic} expose typed CallbackParam, CallableSignature, structural complexity NodeDetailKey slots, and disambiguated CompactGraph/CompactEdge DTOs with zero upward imports from languages or linker.'
  - 'INV-AR004-TREE-SITTER-CONFINED-FINGERPRINTS: Single-pass AST statement fingerprinting and cognitive_complexity / max_nesting_depth computation live in crates/acdd-index/src/ast/ without leaking tree_sitter into src/db/, scoring flat exhaustive match/switch tables as +1 rather than per-arm line penalties.'
acceptance_criteria:
  - 'AC-1: ArchitectureConfig.complexity (max_cognitive_complexity, max_nesting_depth, max_fanout, optional invariant binding), ValueExpr::CallbackParam, typed callable parameter metadata on TypedFacts, typed NodeDetailKey slots (including cognitive_complexity, max_nesting_depth, and fact_limit_exceeded), and CompactGraph / CompactEdge renames compile cleanly across all admitted workspace callers.'
  - 'AC-2: crates/acdd-index/src/ast/ extracts language-agnostic AST statement sequences (skipping pure delimiter/comment trivia and collapsing multi-line formatting within a statement) alongside per-callable AST cognitive complexity and control-flow nesting depth.'
  - 'AC-3: Pre-registered seam test modules compile cleanly in canonical test binaries with zero test-harness leaks.'
status: planned
subtasks:
  - subtask_ref: leaf-complexity-policy-value-expr-and-compact-graph
    title: "Add ComplexityLimits to ArchitectureConfig in crates/acdd-core/src/lint.rs, ValueExpr::CallbackParam and typed callable signatures in semantic/ and typed_facts/, structural complexity/fact-budget NodeDetailKey fields, and rename compact_graph::{Graph,Edge} to CompactGraph/CompactEdge across all callers"
  - subtask_ref: ast-statement-fingerprints-and-cognitive-metrics
    title: "Implement language-agnostic Tree-sitter AST statement normalizer plus AST cognitive_complexity (flat match = +1, nested control flow = +1 + depth) and max_nesting_depth extraction in crates/acdd-index/src/ast/"
  - subtask_ref: pre-register-seam-tests
    title: "Pre-register seam test modules in canonical test binaries and verify compilation with cargo clippy and focused tests"
```

### task: declarative-returns-and-agnostic-value-flow

```yaml
task_ref: declarative-returns-and-agnostic-value-flow
target: "Wire compound generic member_returns/call_returns in .toml manifests, evaluate ValueExpr::CallbackParam, unified destructuring, lookup_builtin_member, and two-tier polymorphic trait/interface contract edges across shared linker phases, and migrate Tier-1 Python/TS/Web profiles onto SyntaxGrammarSpec"
contract_revision: 1
depends_on:
  - upfront-contracts-ast-fingerprints-and-binary-guard
tags:
  - default
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
invariants:
  - 'INV-AR004-DECLARATIVE-MANIFEST-RETURNS: Chained external/stdlib method and function return types (including Result/Option/Tuple/Iterator/Promise/QuerySet wrappers) resolve from .toml manifests without hardcoded language method tables.'
  - 'INV-AR004-POLYMORPHIC-CONTRACT-HONESTY: Calls on &dyn Trait, Protocol/ABC, and TS/Tier-2 interfaces link deterministically to the trait/interface method declaration (plus bounded PossibleCalls when closed) while never fabricating a single concrete struct Calls edge.'
acceptance_criteria:
  - 'AC-1: ReceiverTypeRule.member_returns and call_returns support compound generic wrappers; playwright.toml, web.toml, django.toml, rust-std.toml, and rust-cargo.toml resolve multi-hop chains (Playwright locator/request/expect, str::rsplit_once -> Option<(&str, &str)>, rusqlite Statement::query -> Rows -> Row::get, sha2::Digest::update), and typescript/mod.rs hardcoded WebIDL arms (1453-1486) are deleted.'
  - 'AC-2: Shared evaluate_tracked resolves ValueExpr::CallbackParam, unified struct/tuple/variant destructuring in ValueFlowCollectorCore, and language-agnostic lookup_builtin_member across Rust, Python, and TypeScript/JS.'
  - 'AC-3: High-complexity nested functions in linker/value_flow/{mod.rs,initialize.rs} and languages/manifests/syntax.rs (evaluate_tracked, build_initialized, validate) are refactored within INV-BOUNDED-STRUCTURAL-COMPLEXITY bounds, and python/mod.rs + typescript/mod.rs adopt SyntaxGrammarSpec.'
status: planned
subtasks:
  - subtask_ref: compound-manifest-returns-and-webidl-removal
    title: "Extend manifests/syntax.rs, semantic_context, and value_flow to project nested generic return wrappers from .toml manifests; wire playwright.toml, web.toml, django.toml, rust-std.toml, and rust-cargo.toml; delete hardcoded WebIDL method arms in typescript/mod.rs"
  - subtask_ref: agnostic-callback-destructuring-and-builtin-lookup
    title: "Evaluate ValueExpr::CallbackParam, ValueFlowCollectorCore struct/tuple destructuring, generalized lookup_builtin_member, and two-tier Trait/Interface contract edges across shared linker phases"
  - subtask_ref: tier1-syntax-grammar-spec-and-shared-linker-split
    title: "Adopt SyntaxGrammarSpec in Python/TypeScript extractors and decompose high-cognitive-complexity / deeply nested logic in linker/value_flow/{mod.rs,initialize.rs} and languages/manifests/syntax.rs"
```

### task: rust-linker-precision-and-god-module-split

```yaml
task_ref: rust-linker-precision-and-god-module-split
target: "Migrate Rust onto ValueExpr::CallbackParam and SyntaxGrammarSpec, close remaining zero-heuristic Rust gaps (#[derive(Parser)] Self, destructuring false-shadowing, re-export aliases, impl macros, intra-crate helper calls), delete duplicated constructor matches! tables, and decompose high-complexity Rust monoliths"
contract_revision: 2
depends_on:
  - declarative-returns-and-agnostic-value-flow
tags:
  - default
scope:
  - crates/acdd-index/src/languages/rust/
  - crates/acdd-index/src/linker/rust/
  - tests/linker/
invariants:
  - 'INV-AR004-RUST-NO-SHADOW-HACKS: __rust_fn_param and details["signature"] text parsing are deleted in favor of ValueExpr::CallbackParam, duplicated external constructor matches! tables are replaced by .toml manifest returns, and unregistered Cargo crate homonyms remain strictly unresolved.'
  - 'INV-AR004-RUST-MODULAR-BOUNDS: Every production function in languages/rust/** and linker/rust/** satisfies INV-BOUNDED-STRUCTURAL-COMPLEXITY (cognitive_complexity, max_nesting_depth, fanout, and zero fact_budget_exceeded) with zero >= 5-statement clones and zero dead_tails.'
acceptance_criteria:
  - 'AC-1: #[derive(clap::Parser)] associated Self calls (Cli::parse().run()), struct/tuple/enum-variant pattern destructuring (let Node { id, name, .. } = n, if let Ok/Err/Some), pub(crate) use module aliases, local impl Trait for $ty declarative macros (enum_codec!, target_decoder!), and intra-crate RustMembers/MacroVisibility helper calls resolve deterministically.'
  - 'AC-2: False-shadowing diagnostics ("callee is shadowed by a parameter or local binding") drop for typed/destructured locals while genuine Fn/FnMut/FnOnce parameters classify as dynamic_callback.'
  - 'AC-3: High-cognitive-complexity monoliths in languages/rust/value_flow.rs (enter, value_depth), linker/rust/value_flow.rs (project), and languages/rust/linker.rs (build) are decomposed into cohesive submodules obeying INV-BOUNDED-STRUCTURAL-COMPLEXITY, and rust/mod.rs adopts SyntaxGrammarSpec.'
status: planned
subtasks:
  - subtask_ref: rust-derive-self-destructuring-and-macro-impls
    title: "Wire #[derive(clap::Parser)] Self constructors, struct/enum pattern destructuring, false-shadowing vs dynamic_callback classification, module re-export aliases, and impl Trait for $ty declarative macros while migrating __rust_fn_param to ValueExpr::CallbackParam and deleting redundant external_constructor_result matches! arms"
  - subtask_ref: decompose-rust-extractor-and-linker-monoliths
    title: "Adopt SyntaxGrammarSpec in languages/rust/mod.rs and decompose deeply nested / high-cognitive-complexity functions in languages/rust/{value_flow,linker}.rs and linker/rust/value_flow.rs within INV-BOUNDED-STRUCTURAL-COMPLEXITY bounds"
  - subtask_ref: rust-precision-contract-matrices
    title: "Extend table-driven contract matrices in tests/linker/ covering derive Self, destructuring, macro impls, re-export aliases, dynamic_callback classification, and homonym rejection"
```

### task: lint-engine-ast-clones-cycles-and-rules

```yaml
task_ref: lint-engine-ast-clones-cycles-and-rules
target: "Upgrade duplicates.rs to AST-statement clone windows, separate module import SCCs from call recursion in cycles.rs, promote dangling_call to error, add docs.unused_arch_zone in rules.rs without breaking INV-STATE-NO-INDEX or enforced_invariant_ids signature, enforce INV-BOUNDED-STRUCTURAL-COMPLEXITY in syntax.rs, and author executable LS-003/AR-004 invariants across docs and acdd.yaml"
contract_revision: 2
depends_on:
  - upfront-contracts-ast-fingerprints-and-binary-guard
tags:
  - default
scope:
  - crates/acdd-index/src/db/lint/
  - acdd.yaml
  - docs/architecture/
  - docs/adr/
  - tests/core.rs
invariants:
  - 'INV-AR004-LOCKED-ARCH-RULE-ENUM: ArchRuleKind::ALL.len() == 8 remains unchanged; structural complexity, nesting, fanout, fact-budget, and homonym checks evaluate in LintCategory::Syntax (DEFAULT_VERIFY) backed by ArchitectureConfig.complexity and wired into enforced_invariant_ids(conn, config_rules) via resolve_architecture(conn, None) while preserving its public signature.'
  - 'INV-AR004-ZERO-NOISE-CLONES-AND-CYCLES: Closing-brace blocks and wrapped single-statement calls produce 0 duplicate warnings, adjacent AST-statement clone windows coalesce into maximal spans, flat lookup/router match tables incur no per-arm complexity penalty, and 1-node self-recursive functions produce 0 cycles.scc warnings.'
acceptance_criteria:
  - 'AC-1: collect_symbol_duplicates in duplicates.rs compares K-statement AST fingerprint sequences from src/ast/ and merges overlapping adjacent matches into single maximal clone groups.'
  - 'AC-2: cycles.rs emits cycles.scc warnings strictly on multi-node module/file import SCCs (kind = imports, len >= 2); dead_tails.dangling_call emits LintSeverity::Error; syntax.rs evaluates syntax.cognitive_complexity, syntax.max_nesting_depth, syntax.high_fanout, syntax.fact_budget_exceeded, and syntax.intra_crate_type_homonym bound to INV-BOUNDED-STRUCTURAL-COMPLEXITY while preserving enforced_invariant_ids signature; and rules.rs emits docs.unused_arch_zone (LintSeverity::Warn) whenever declared architecture.zones are unreferenced.'
  - 'AC-3: Canonical INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md plus executable acdd-rule blocks in docs/architecture/indexing.md, ADR 0010, ADR 0011, and acdd.yaml enforce Layer 2 linker/extractor boundaries (boundary_entry + forbid_dependency), ManifestParser must_implement, hot-path Regex::new / shrink_to_fit / raw node.details[key] ast_pattern bans, index_db and host_telemetry zone rules (0 docs.unused_arch_zone warnings), and forbid_cycles on acyclic zones.'
status: planned
subtasks:
  - subtask_ref: ast-clone-windows-and-cycle-separation
    title: "Switch duplicates.rs to AST-statement fingerprint windows with maximal-span coalescing, separate imports module SCCs from calls recursion in cycles.rs, and promote dead_tails.dangling_call to Error"
  - subtask_ref: syntax-structural-complexity-and-homonym-lints
    title: "Implement syntax.cognitive_complexity, syntax.max_nesting_depth, syntax.high_fanout, syntax.fact_budget_exceeded, syntax.intra_crate_type_homonym in syntax.rs, add docs.unused_arch_zone in rules.rs, and wire ArchitectureConfig.complexity into enforced_invariant_ids via resolve_architecture without signature changes"
  - subtask_ref: executable-ls003-complexity-and-zone-rules
    title: "Declare INV-BOUNDED-STRUCTURAL-COMPLEXITY in docs/architecture/modularity.md and acdd.yaml, and author executable acdd-rule blocks across docs/architecture/indexing.md, ADR 0010, ADR 0011, and acdd.yaml, backed by contract matrices in tests/core.rs"
```

### task: workspace-cycle-breaking-dedup-and-gate-ratchet

```yaml
task_ref: workspace-cycle-breaking-dedup-and-gate-ratchet
target: "Break all remaining intra-crate module import SCCs (enabling workspace-wide forbid_cycles: error), refactor remaining INV-BOUNDED-STRUCTURAL-COMPLEXITY violations across acdd-state/acdd-index/src/, eliminate all verified AST clones and proven dead_tails, add acdd serve binary-drift reload guard, and ratchet .acdd/profile.yaml check_full_repo to enforce all 6 lint categories"
contract_revision: 2
depends_on:
  - rust-linker-precision-and-god-module-split
  - lint-engine-ast-clones-cycles-and-rules
tags:
  - default
scope:
  - crates/acdd-core/src/
  - crates/acdd-index/src/
  - crates/acdd-state/src/
  - src/
  - .acdd/profile.yaml
  - docs/reference/
  - skills/acdd/
  - tests/mcp.rs
invariants:
  - 'INV-AR004-WORKSPACE-ACYCLIC-AND-CLEAN: Workspace-wide forbid_cycles (severity: error), INV-BOUNDED-STRUCTURAL-COMPLEXITY (0 cognitive_complexity / max_nesting_depth / high_fanout / fact_budget_exceeded errors), and acdd lint --categories arch,syntax,docs,duplicates,dead_tails,cycles report 0 errors and 0 unresolved hygiene regressions.'
  - 'INV-AR004-DAEMON-BINARY-COHERENCE: Long-lived acdd serve / MCP processes detect on-disk binary replacement via executable identity digest and reload/re-exec or fail-safe cleanly without index rebuild thrashing.'
acceptance_criteria:
  - 'AC-1: The 88-node acdd-index module SCC, 5-node acdd-core/src/config SCC, and any src/ / acdd-state module import cycles are decoupled into acyclic DAGs with forbid_cycles: error active across all workspace zones.'
  - 'AC-2: Remaining production functions violating INV-BOUNDED-STRUCTURAL-COMPLEXITY (e.g. machine/submit.rs::evaluate_pass, db/writer/ingest.rs::persist_files_impl, db/lint/docs_lint.rs::collect_doc_findings, runner/mod.rs::run_isolated_process_unix, while flat lookup/router tables stay intact) are refactored cleanly; MCP tool dispatchers in src/mcp/tools/code_tools.rs, acdd-core helpers, pipeline, and state_bridge clones are consolidated; and proven dead_tails / phantom_tested helpers are removed or gated under #[cfg(test)].'
  - 'AC-3: src/mcp/server.rs and src/cli/reload.rs prevent stale-binary index ping-pong on binary upgrades; docs/reference/ and skills/acdd/ reflect all new lint rules and ComplexityLimits config; and .acdd/profile.yaml check_full_repo passes full 6-category acdd lint, cargo test --workspace --all-targets, docs check, and validate.'
status: planned
subtasks:
  - subtask_ref: break-intra-crate-module-sccs
    title: "Use code_map_query(operation='slice') to break remaining module import SCCs in acdd-index, acdd-core/src/config, acdd-state, and src/, then enable workspace-wide forbid_cycles: error"
  - subtask_ref: consolidate-clones-complexity-outliers-and-dead-tails
    title: "Refactor remaining INV-BOUNDED-STRUCTURAL-COMPLEXITY outliers across acdd-state/acdd-index/src/, extract shared MCP tool dispatch and acdd-core/pipeline/state_bridge helpers to eliminate verified AST clones, and prune or #[cfg(test)]-gate proven dead_tails"
  - subtask_ref: daemon-binary-guard-docs-and-profile-ratchet
    title: "Add executable identity drift detection/reload for acdd serve, align docs/reference/ and skills/acdd/ with ComplexityLimits and new syntax rules, and ratchet .acdd/profile.yaml check_full_repo to enforce all 6 lint categories"
```

### task: lint-engine-soundness-perf-and-rule-hygiene

```yaml
task_ref: lint-engine-soundness-perf-and-rule-hygiene
target: "Fix all linter soundness/performance pitfalls: single-parse fail-closed ast_pattern evaluation, possible_calls confidence matching, allow_only_dependencies external checks, seen_in_coverage test/prod separation, forbid_cycles zone specificity + INV-WORKSPACE-BOUNDARIES deduplication, public-only must_implement, docs.stale_symbol_range out-of-bounds checks, Type-2 duplicate normalization, dead_tails raw-scanner removal + opt-in unused_public_api, and noisy acdd.yaml rule cleanup"
contract_revision: 1
depends_on:
  - workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
  - default
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
invariants:
  - 'INV-AR004-SOUND-LINT-ENGINE: ast_pattern parses each file once per language and fails closed on work_limited or timeout; edge_matches_rule checks possible_calls; allow_only_dependencies checks externals when configured; dead_tails uses zero raw .rs disk scanning; and duplicates normalization handles Type-2 local variable renames.'
acceptance_criteria:
  - 'AC-1: db/lint/arch.rs batches Tree-sitter parsing once per file/language and returns explicit diagnostics/errors when search_page hits work_limited or incomplete; db/lint/rules.rs admits possible confidence for possible_calls; allow_only_dependencies checks externals without silent bypass; seen_in_coverage tracks test vs production lines accurately; forbid_cycles selects the most specific zone; duplicate INV-WORKSPACE-BOUNDARIES is removed from docs/architecture/performance.md; ARCH-INDEX-DB-BOUNDARY-ENTRY is retired in favor of Rust module visibility + ARCH-INDEX-DB-ISOLATION; and qualified Command::new / Connection::open / #![allow(dead_code)] / #[expect(dead_code)] bypasses are closed.'
  - 'AC-2: must_implement skips private serde/helper structs, matches enums/cross-file impls, and removes line-span external heuristics; docs_lint.rs flags end > source_lines and multi-match stale_symbol_range and retires unreachable docs.broken_doc_link from run_lint; syntax.intra_crate_type_homonym uses DependencyRegistry package roots and ComplexityLimits severity; ast/fingerprints.rs + db/lint/duplicates.rs perform Type-2 identifier-normalized and LCS/Jaccard clone detection excluding tests by default; db/lint/dead_tails.rs deletes its remaining raw .rs callback scanner and handles unresolved receiver methods conservatively; db/lint/unused_public_api.rs becomes opt-in via ArchitectureConfig (enabled in acdd.yaml, off by default for libraries); and db/cycles.rs filters test/archive imports.'
status: planned
subtasks:
  - subtask_ref: harden-arch-ast-pattern-confidences-and-rules
    title: "Fix ast_pattern single-parse fail-closed execution, possible_calls confidence matching, allow_only externals, seen_in_coverage test masking, forbid_cycles zone specificity, must_implement noise, and clean up acdd.yaml/docs/architecture rules"
  - subtask_ref: harden-docs-syntax-duplicates-dead-tails-and-cycles
    title: "Fix docs_lint out-of-bounds #L anchors, syntax package boundaries, Type-2 duplicate fingerprints/similarity, opt-in unused_public_api config, dead_tails raw-scanner removal, and cycles test filtering with matrix tests in tests/core/arch_lint.rs"
```

### task: rust-extractor-and-rust-linker-coverage-recovery

```yaml
task_ref: rust-extractor-and-rust-linker-coverage-recovery
target: "Eliminate post-AR-004 Rust unresolved regressions (~2,450+ entries): skip enum_variant attribute_item tokens, propagate external re-exports (Syntax) via RustForwarding, align trait_object_call with bounded polymorphic resolved status and trait-node member registration, admit generic-bounded inherent methods in RustMembers, emit serde callback references, and resolve intermediate module re-exports in multi-segment paths"
contract_revision: 1
depends_on:
  - workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
  - default
scope:
  - crates/acdd-index/src/languages/rust/
  - crates/acdd-index/src/linker/rust/
  - tests/languages/rust/
invariants:
  - 'INV-AR004-RUST-RECOVERY-NO-HEURISTICS: Every recovered Rust resolution edge derives from AST attribute/type extraction, explicit RustForwarding re-exports, inherent/trait definitions, or bounded dynamic dispatch (<= K=8) with zero name-only guessing.'
acceptance_criteria:
  - 'AC-1: languages/rust/mod.rs relate_nominal_item skips attribute_item and comments inside enum_variant bodies (~400 bogus unresolved refs removed) and emits References for #[serde(default, skip_serializing_if, serialize_with, deserialize_with, with)] callable paths.'
  - 'AC-2: linker/rust/semantic_context.rs propagates visible external Cargo re-exports (e.g. pub(crate) use tree_sitter::Node as Syntax) across importing modules (~1,450+ unresolved refs recovered); linker/rust/resolution.rs + profile_linker.rs register trait methods in RustMembers for ValueFlow return propagation and record trait_object_call as resolved when trait implementors <= MAX_CANDIDATE_BRANCH_JOINS (~504 unresolved refs recovered); inherent methods with generic bounds (e.g. CompactGraph) are admitted in RustMembers (~106+ unresolved refs recovered); and multi-segment paths traverse intermediate RustForwarding re-exports.'
status: planned
subtasks:
  - subtask_ref: fix-rust-enum-attr-bug-and-serde-callback-refs
    title: "Fix enum_variant attribute_item type extraction bug and extract #[serde(...)] callback references in languages/rust/"
  - subtask_ref: recover-rust-reexports-trait-objects-and-inherent-bounds
    title: "Propagate external re-exports (Syntax), align trait_object_call + trait member registration with polymorphic bounds, admit generic inherent methods in RustMembers, and resolve intermediate module re-exports with regression tests in tests/languages/rust/"
```

### task: universal-value-flow-branch-joins-and-manifest-returns

```yaml
task_ref: universal-value-flow-branch-joins-and-manifest-returns
target: "Fix universal cross-language resolution gaps: unlock ReceiverTypeRule.call_returns across all compiled languages, perform element-wise Tuple/Option BranchJoin without collapsing known tuple elements to Unknown, preserve monotonic static types across loop/branch reassignments (let mut scope), propagate cross-module external_aliases universally, and classify typed CallbackParam calls accurately"
contract_revision: 1
depends_on:
  - workspace-cycle-breaking-dedup-and-gate-ratchet
tags:
  - default
scope:
  - crates/acdd-index/src/languages/support/value_flow_core.rs
  - crates/acdd-index/src/linker/value_flow/
  - crates/acdd-index/src/linker/semantic_context/
  - crates/acdd-index/src/linker/resolution/
  - crates/acdd-index/src/languages/dependency_registry/frameworks/
  - tests/linker/method_semantics.rs
  - tests/linker/callback_contracts.rs
invariants:
  - 'INV-AR004-UNIVERSAL-VALUE-FLOW-PARITY: Declarative call_returns rules, element-wise Tuple/Option BranchJoin, monotonic loop variable typing, and external_aliases inheritance apply uniformly across all supported language profiles rather than hardcoded JS/TS/Vue or Rust branches.'
acceptance_criteria:
  - 'AC-1: linker/semantic_context/type_resolution.rs resolve_call_result evaluates declarative ReceiverTypeRule.call_returns across all languages (removing the js/ts/vue-only gate) and resolves common external type wrappers cleanly.'
  - 'AC-2: linker/value_flow/evaluate.rs performs element-wise BranchJoin over Tuple/RustTuple and Option<T>/Unknown (preserving known fields such as match { None => (Default, None), Some => (Val, Some(x)) }); ValueFlowCollectorCore/value_flow preserve monotonic static types for reassigned mutables in loops/branches (e.g. scope = scope.rsplit_once(...)); linker/resolution/imports.rs propagates external_aliases across indexed modules universally; and regression tests pass in tests/linker/{method_semantics,callback_contracts}.rs.'
status: planned
subtasks:
  - subtask_ref: unlock-manifest-call-returns-and-universal-external-aliases
    title: "Enable ReceiverTypeRule.call_returns for all languages in semantic_context and propagate cross-module external_aliases in resolution/imports.rs"
  - subtask_ref: elementwise-tuple-branch-join-and-monotonic-loop-typing
    title: "Implement element-wise Tuple/Option BranchJoin in value_flow/evaluate.rs, monotonic mutable reassignment preservation in ValueFlowCollectorCore, and add tests in tests/linker/{method_semantics,callback_contracts}.rs"
```
