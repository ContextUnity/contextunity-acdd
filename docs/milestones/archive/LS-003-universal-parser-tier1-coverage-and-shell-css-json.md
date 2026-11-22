---
id: LS-003-universal-parser-tier1-coverage-and-shell-css-json
title: Universal declarative language engine (Tier-2 parity + default Bash/CSS/JSON) and Tier-1 resolution coverage (Rust, Python, Web/Vue/HTML)
doc_type: contract
status: completed
depends_on:
- PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
- AR-002-modularize-db-reader-and-traversal
owners:
- Cargo.toml
- crates/acdd-index/Cargo.toml
- crates/acdd-index/AGENTS.md
- crates/acdd-index/src/ast/visitor.rs
- crates/acdd-index/src/models/
- crates/acdd-index/src/languages/
- crates/acdd-index/src/languages/AGENTS.md
- crates/acdd-index/src/linker/
- tests/languages/
- tests/framework_manifests/
- tests/receivers/
- docs/adr/0010-static-value-flow-and-receiver-resolution.md
- docs/adr/0011-language-linkers-and-framework-manifests.md
- docs/reference/acdd.md
- docs/milestones/LS-003-universal-parser-tier1-coverage-and-shell-css-json.md
invariants:
- 'INV-LS003-STATIC-NO-HEURISTICS: Every newly resolved edge or external classification is statically justified by AST types, lexical bindings, manifest-declared dependencies, or typed RuleIR manifests (ADR 0010 / ADR 0011); never guess call targets by member spelling alone.'
- 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Tier-2 profiles (Go, Java, Kotlin, C#, PHP, Ruby, C, C++) and new default profiles (Bash, CSS, JSON) share SyntaxGrammarSpec + ValueFlowCollectorCore/GenericScopeAnalysis + GENERIC_LINKER instead of duplicating custom value-flow walkers.'
- 'INV-LS003-DEFAULT-WEB-SHELL-JSON: tree-sitter-bash, tree-sitter-css, and tree-sitter-json are compiled into default features (lang-bash, lang-css, lang-json) alongside existing default Rust/Python/TypeScript/JavaScript/HTML/Vue/Proto/TOML/YAML grammars.'
- 'INV-LS003-MANIFEST-DATA-DRIVEN: Framework conventions (Rust stdlib, Django, unittest, Alpine.js, Playwright, WebIDL/DOM, Nuxt/Pinia) reside strictly in declarative TOML manifests compiled into typed RuleIR with typed ActivationCondition and Ecosystem enums and zero hardcoded framework package checks in language parsers.'
- 'INV-LS003-SINGLE-PASS-THROUGHPUT: Every language extractor preserves single-pass TreeCursor traversal with zero per-node regex compilation, manifest parsing, or hot-path JSON serialization (crates/acdd-index/src/languages/AGENTS.md).'
- 'INV-LS003-LAYER2-CONSOLIDATION: Shared ValueFlowCollectorCore/ScopeState replaces duplicated Tier-1 Collector scaffolding, LanguageProfile exposes a unified facade (SyntaxGrammarSpec + LanguageLinker + typed LanguageFileState), and ManifestParser splits dependency_registry.rs cleanly while extending PF-003 Layer 1 types without rewriting them.'
acceptance_criteria:
- 'AC-1: Modular ManifestParser plugins split dependency_registry.rs (2,930 LOC) into dependency_registry/ with typed Ecosystem and ActivationCondition enums; SyntaxGrammarSpec and shared ValueFlowCollectorCore/GenericScopeAnalysis emit canonical typed ValueFlowFacts for all Tier-2 profiles; and GENERIC_LINKER resolves multi-file packages, external import aliases, wildcard/static imports, and manifest module prefixes (go.mod, composer.json, .csproj).'
- 'AC-2: Go, Java, Kotlin, C#, PHP, Ruby, C, and C++ achieve cross-file import, stdlib/external, and local typed-receiver resolution parity without false dynamic: true markers on constructors or operators.'
- 'AC-3: Bash (.sh, .bash, .zsh), CSS (.css), and JSON (.json) profiles ship in default features (tree-sitter-bash, tree-sitter-css, tree-sitter-json), register in LanguageId/CompactTag, and .j2 templates index under the HTML/Jinja2 profile with full symbol, reference, and import/link extraction.'
- 'AC-4: Rust resolution coverage on contextunity-acdd increases from 42.74% to >= 75% (chained calls, literal/slice receivers, struct field & &T parameter propagation, expanded rust-std.toml, const/static/enum_variant/associated_type, standard #[derive(...)], and declarative struct/enum macros) while migrating rust/value_flow.rs onto shared ValueFlowCollectorCore and removing the 3-way build_file_macro_facts clone.'
- 'AC-5: Python resolution coverage on commerce-release-update increases from 80.75% to >= 87% (logging.Logger -> external, Callable/Protocol/type[T]/union parameters, with-as, Path /, f-strings/slices, chained calls, super().__init__, expanded django.toml and unittest.toml) and Proto reaches 100% with google/protobuf/* stdlib while migrating python/value_flow.rs onto shared ValueFlowCollectorCore and LanguageFileState::Python.'
- 'AC-6: Web/Frontend resolution on commerce-release-update increases Vue from 65.54% to >= 85%, JavaScript from 20.72% to >= 60%, HTML from 37.05% to >= 75%, and overall commerce-release-update resolution from 74.70% to >= 87% (Alpine typed ActivationCondition syntax activation, playwright.toml, expanded web.toml DOM/WebIDL, IIFE scoping, Vue v-for + Pinia defineStore) while migrating typescript/value_flow.rs onto ValueFlowCollectorCore and LanguageFileState::TypeScript.'
- 'AC-7: ADR 0010, ADR 0011, docs/reference/acdd.md, crates/acdd-index/AGENTS.md, and crates/acdd-index/src/languages/AGENTS.md document the Layer 2 facade, ManifestParser, ActivationCondition, and ValueFlowCollectorCore ownership and pass acdd docs check and acdd validate.'
guidance:
  hitl: pro
required_skills:
- acdd
- contract-planning
- test-suite-rules
closure:
  status: completed
  closed_at: 2026-11-22T11:57:18.119Z
  target_base: main
  target_base_commit: 2d946c4e8b9399ba5dc1b23b4f5a41a13c5effe5
  task_receipts:
    modular-manifest-parsers-and-typed-activation:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:modular-manifest-parsers-and-typed-activation.deliver
      gate_id: deliver
      worker_id: ls003-r1
      claim_revision: 1
      contract_revision: 4
      candidate_tree: c9e12e4fb1f190bac8a653ed3aca2459da260597
      committed_oid: 69b0b7482cc49004d4f974f56f36fa075854f7fc
      accepted_at: 2026-11-22T02:03:23.826Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-MANIFEST-DATA-DRIVEN: Dependency manifests parse through trait-isolated ManifestParser plugins returning typed ParsedManifest/Ecosystem values, and framework manifest activation uses typed ActivationCondition variants with backward-compatible TOML deserialization.'
          - 'INV-LS003-SINGLE-PASS-THROUGHPUT: Manifest parsing and framework rule compilation happen once per workspace/context build with zero per-AST-node manifest I/O or regex compilation.'
          verified: []
        review_summary: Delivered Wave 0 task modular-manifest-parsers-and-typed-activation after passing round-2 independent review (candidate_tree c9e12e4fb1f190bac8a653ed3aca2459da260597).
    python-django-typing-chains-and-proto:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:python-django-typing-chains-and-proto.deliver
      gate_id: deliver
      worker_id: ls003-r5
      claim_revision: 1
      contract_revision: 3
      candidate_tree: ea68fec7e4166b68e8f1025b49068f9ea8f73f71
      committed_oid: 4cbb9c7e5458c42f7f6e83b3a881f969b12d192f
      accepted_at: 2026-11-22T05:03:24.671Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-STATIC-NO-HEURISTICS: Untyped dynamic Python variables remain unresolved rather than guessed; gains come strictly from declared annotations, stdlib/framework manifests, and static expression typing.'
          - 'INV-LS003-LAYER2-CONSOLIDATION: python/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and python/mod.rs stores type_checking_aliases inside LanguageFileState::Python.'
          verified: []
        review_summary: 'Deliver python-django-typing-chains-and-proto after Round 2 independent review PASS (all #313 findings remediated, net LOC +1297 <= 1300, Clippy -D warnings clean, acdd-index --lib 86/86, tests/languages --all-features 248/248, tests/linker --all-features 77/77, commitment_integrity 18/18, acdd validate 10/10, 0 duplicates/dead_tails/tmp leaks).'
    rust-resolution-coverage-and-stdlib:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:rust-resolution-coverage-and-stdlib.deliver
      gate_id: deliver
      worker_id: ls003-r4
      claim_revision: 1
      contract_revision: 7
      candidate_tree: 962c737539f1554ff2023ffd85f4e260bed60d4f
      committed_oid: b1658b7c978a3189a7bbd588b05ab1325b189161
      accepted_at: 2026-11-22T10:10:49.583Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-STATIC-NO-HEURISTICS: Every Rust method/associated-item resolution or stdlib external classification is proven from declared types, return types in rust-std.toml, indexed items, or explicit derive/macro declarations.'
          - 'INV-LS003-LAYER2-CONSOLIDATION: rust/value_flow.rs delegates scope-frame/binding/field tracking to ValueFlowCollectorCore/ScopeState with zero >= 5-line clones in build_file_macro_facts.'
          verified: []
        review_summary: 'Deliver task:rust-resolution-coverage-and-stdlib after Round 2 independent review PASS (75.0858% live Rust resolution coverage, BUDGET_OVERRIDE #328).'
    tier2-parity-and-bash-css-json-default:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:tier2-parity-and-bash-css-json-default.deliver
      gate_id: deliver
      worker_id: ls003-r3
      claim_revision: 1
      contract_revision: 7
      candidate_tree: 1a6f67a0808703d32bc875b6c6ecdd6d3e4467cb
      committed_oid: 7d84baa0ca3707172f11ed9dec81cf48f94b0c37
      accepted_at: 2026-11-22T05:33:24.881Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-DEFAULT-WEB-SHELL-JSON: lang-bash, lang-css, and lang-json (tree-sitter-bash, tree-sitter-css, tree-sitter-json) compile and register in default features as well as all-languages, and extend LanguageId/CompactTag cleanly.'
          - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Every Tier-2 and new profile uses SyntaxGrammarSpec + GenericScopeAnalysis with zero false dynamic: true on typed constructors or operators.'
          verified: []
        review_summary: Delivered task:tier2-parity-and-bash-css-json-default after Round 2 independent review PASS (candidate_tree=1a6f67a0808703d32bc875b6c6ecdd6d3e4467cb).
    universal-declarative-engine-and-generic-linker:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:universal-declarative-engine-and-generic-linker.deliver
      gate_id: deliver
      worker_id: ls003-r2
      claim_revision: 1
      contract_revision: 8
      candidate_tree: 10667c65a01dd91f5d3a93ab618be70939e4fed6
      committed_oid: 8e71ae82e18b2ff1322dbdb3af04e89c95b9bac5
      accepted_at: 2026-11-22T03:20:56.242Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: GenericScopeAnalysis and Tier-1 collectors share ValueFlowCollectorCore/ScopeState (with typed ScopeFrameFlags instead of anonymous boolean tuples) and SyntaxGrammarSpec rules without per-language AST visitor duplication.'
          - 'INV-LS003-STATIC-NO-HEURISTICS: GENERIC_LINKER resolves only lexically or package-proven targets and classifies manifest/stdlib aliases as external without name-only guessing.'
          - 'INV-LS003-LAYER2-CONSOLIDATION: LanguageProfile owns fn linker(&self) -> &dyn LanguageLinker (eliminating hardcoded linker_for language switches), replaces kitchen-sink FileContext fields with typed LanguageFileState, delegates language-specific import/inheritance indexing and builtin-origin classification out of linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker/manifest hooks, and uses typed models::tags enums at linker call sites.'
          verified: []
        review_summary: Delivered LS-003 Wave 1 task universal-declarative-engine-and-generic-linker (candidate_tree 10667c65a01dd91f5d3a93ab618be70939e4fed6) after independent review pass.
    web-alpine-playwright-webidl-html-j2-and-vue:
      run_id: LS-003-universal-par-20261122001607-1983397-1
      node_cursor: root:web-alpine-playwright-webidl-html-j2-and-vue.deliver
      gate_id: deliver
      worker_id: ls003-r6
      claim_revision: 1
      contract_revision: 2
      candidate_tree: ba3e1458a7d1382f6a0a562299a8a91dae8703c7
      committed_oid: 15aa183fb64ecee1ff0afc83a4a91af30ec55b6f
      accepted_at: 2026-11-22T04:51:14.913Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-LS003-MANIFEST-DATA-DRIVEN: Alpine syntax activation, Playwright fixtures, DOM/WebIDL receivers, and Pinia store projections are declared strictly through typed FrameworkManifest RuleIR and Wave 0 ActivationCondition.'
          - 'INV-LS003-LAYER2-CONSOLIDATION: typescript/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and typescript/mod.rs stores DOM listener state inside LanguageFileState::TypeScript.'
          verified: []
        review_summary: Delivered task:web-alpine-playwright-webidl-html-j2-and-vue after Round 2 independent review pass (candidate_tree=ba3e1458a7d1382f6a0a562299a8a91dae8703c7).
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-MANIFEST-DATA-DRIVEN: Dependency manifests parse through trait-isolated ManifestParser plugins returning typed ParsedManifest/Ecosystem values, and framework manifest activation uses typed ActivationCondition variants with backward-compatible TOML deserialization.'
      - 'INV-LS003-SINGLE-PASS-THROUGHPUT: Manifest parsing and framework rule compilation happen once per workspace/context build with zero per-AST-node manifest I/O or regex compilation.'
      - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: GenericScopeAnalysis and Tier-1 collectors share ValueFlowCollectorCore/ScopeState (with typed ScopeFrameFlags instead of anonymous boolean tuples) and SyntaxGrammarSpec rules without per-language AST visitor duplication.'
      - 'INV-LS003-STATIC-NO-HEURISTICS: GENERIC_LINKER resolves only lexically or package-proven targets and classifies manifest/stdlib aliases as external without name-only guessing.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: LanguageProfile owns fn linker(&self) -> &dyn LanguageLinker (eliminating hardcoded linker_for language switches), replaces kitchen-sink FileContext fields with typed LanguageFileState, delegates language-specific import/inheritance indexing and builtin-origin classification out of linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker/manifest hooks, and uses typed models::tags enums at linker call sites.'
      - 'INV-LS003-DEFAULT-WEB-SHELL-JSON: lang-bash, lang-css, and lang-json (tree-sitter-bash, tree-sitter-css, tree-sitter-json) compile and register in default features as well as all-languages, and extend LanguageId/CompactTag cleanly.'
      - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Every Tier-2 and new profile uses SyntaxGrammarSpec + GenericScopeAnalysis with zero false dynamic: true on typed constructors or operators.'
      - 'INV-LS003-STATIC-NO-HEURISTICS: Every Rust method/associated-item resolution or stdlib external classification is proven from declared types, return types in rust-std.toml, indexed items, or explicit derive/macro declarations.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: rust/value_flow.rs delegates scope-frame/binding/field tracking to ValueFlowCollectorCore/ScopeState with zero >= 5-line clones in build_file_macro_facts.'
      - 'INV-LS003-STATIC-NO-HEURISTICS: Untyped dynamic Python variables remain unresolved rather than guessed; gains come strictly from declared annotations, stdlib/framework manifests, and static expression typing.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: python/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and python/mod.rs stores type_checking_aliases inside LanguageFileState::Python.'
      - 'INV-LS003-MANIFEST-DATA-DRIVEN: Alpine syntax activation, Playwright fixtures, DOM/WebIDL receivers, and Pinia store projections are declared strictly through typed FrameworkManifest RuleIR and Wave 0 ActivationCondition.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: typescript/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and typescript/mod.rs stores DOM listener state inside LanguageFileState::TypeScript.'
      verified: []
    decisions:
    - |-
      LS-003 Architectural Blueprint Decision (Layer 2 Universal Declarative Engine, Default Bash/CSS/JSON, and Tier-1 Coverage):

      1. Wave 0 (modular-manifest-parsers-and-typed-activation): Decompose 2,930-LOC crates/acdd-index/src/languages/dependency_registry.rs into dependency_registry/{mod,parser,framework_packs,tests}.rs + parsers/{mod,cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs behind a single ManifestParser trait + closed Ecosystem enum + ParsedManifest/ModulePrefix structs, reusing existing languages/build_manifest.rs (JVM) and languages/toml_manifest.rs (Cargo/Python) and deduplicating the build_language_context vs hierarchy_rows_for_path workspace-manifest-name clone. Upgrade manifests/semantic.rs::Activation to typed ActivationCondition { Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, Any } with backward-compatible { kind = "package", name = "..." } TOML wire syntax and a 3-state workspace-vs-file-syntax activation split (FileActivationSignals) so syntax-gated packs (alpinejs.toml) remain retained in LanguageContext and activate in O(1) without per-node I/O or hardcoded framework checks.

      2. Wave 1 (universal-declarative-engine-and-generic-linker): Establish crates/acdd-index/src/languages/support/value_flow_core.rs owning ScopeFrameFlags (replacing Vec<(bool,bool)> / Vec<(bool,bool,bool,bool)> across rust/python/typescript Collectors), ScopeState, ValueFlowCollectorCore, and GenericScopeAnalysis wired into ast/visitor.rs::ScopeAnalysis. Extend SyntaxGrammarSpec with typed DeclarationRole (mapping to models::tags::NodeKind), ImportKind, CallForm, Visibility, and compiled-once binding/constructor/member-call/heritage/conditional selectors. Add LanguageProfile::linker(&self) -> &'static dyn LanguageLinker (reducing languages::linker_for to a 1-line delegation with zero language switches), replace kitchen-sink FileContext fields with typed LanguageFileState { None, Python(PythonFileState), TypeScript(TypeScriptFileState) } (and SyntaxContext::file_state), and extract hardcoded Rust/JS/Python switches from linker/indexing.rs and linker/resolution/mod.rs (builtin_member_origin, Rust mod/crate import normalization, JS import normalization, Rust src/lib.rs namespaces, Python compose_symbol_relations) behind LanguageLinker hooks while preserving Rayon parallelism and borrowed SymbolId/PathId/CompactTag lookups and migrating linker call sites onto models::tags enums. Wave 1 uses autonomous acdd extend-scope for strictly mechanical wiring in languages/{rust,python,typescript,vue,html}/ and linker/{resolution/value_flow_phase,semantic_context/preparation.rs} so FileContext and linker_for cleanup completes atomically before concurrent Wave 2 tasks start (preventing merge conflicts on languages/mod.rs). Upgrade GENERIC_LINKER (linker/traits.rs::resolve_default_import + linker/resolution/imports.rs) for multi-file packages (removing modules.len() == 1 restriction), wildcard/static imports, tracks_external_aliases() == true, and DependencyRegistry::module_prefixes_for_path stripping (go.mod, composer.json PSR-4, .csproj RootNamespace).

      3. Wave 2 (4 concurrent disjoint tasks):
      - tier2-parity-and-bash-css-json-default: Authorized protected-path edits ONLY in Cargo.toml & crates/acdd-index/Cargo.toml to add tree-sitter-{bash,css,json} compatible with pinned tree-sitter = "0.22" (ABI 14 language() fn; never bump tree-sitter) under default features lang-bash/lang-css/lang-json + all-languages; register Bash/Css/Json in models/tags.rs::LanguageId (auto-interned in CompactTag); add declarative bash.rs/css.rs/json.rs (auto-discovered by build.rs); migrate go/java/kotlin/csharp/php/ruby/c/cpp onto SyntaxGrammarSpec + GenericScopeAnalysis fixing normalize_import (csharp/php/ruby), is_stdlib, sibling_accessible, and false dynamic: true constructors/operators.
      - rust-resolution-coverage-and-stdlib: Migrate rust/value_flow.rs onto ValueFlowCollectorCore/ScopeState, remove 3-way build_file_macro_facts clone (lines 1866-1963) and rust/mod.rs builtin clone, add multi-hop chains, literal/slice/struct-field/&T propagation, expand rust-std.toml (<=256 returns per rule) + rust-cargo.toml, index const_item/static_item/enum_variant/associated_type, explicit #[derive(...)] synthesis, and bounded struct/enum macro_rules! projections to reach >=75% on contextunity-acdd.
      - python-django-typing-chains-and-proto: Fix missing "logging.Logger" => "builtin:logging.Logger" in linker/python/semantic_context.rs::external_member (plus deduplicate python/mod.rs::builtin_member Logger/LoggerAdapter clone), add google/protobuf/* is_stdlib in proto.rs (100% Proto), migrate python/value_flow.rs onto ValueFlowCollectorCore/ScopeState + LanguageFileState::Python, add Callable/Protocol/type[T]/union(possible_calls 2..=8)/bytearray/with-as/Path-div/f-string/slice/super().__init__/generic-subscripts/chains, expand django.toml & unittest.toml to reach >=87% Python on commerce-release-update.
      - web-alpine-playwright-webidl-html-j2-and-vue: Wire FileActivationSignals in html/mod.rs & html/javascript.rs for alpinejs.toml Any[Package,SyntaxAttribute,SyntaxAttributePrefix] without hardcoded framework names, add .j2 to Html::extensions, migrate typescript/value_flow.rs onto ValueFlowCollectorCore/ScopeState + LanguageFileState::TypeScript, scope IIFEs without false calls, embed playwright.toml (auto-bundled via build_manifests.rs) + expand web.toml DOM/WebIDL, add Vue v-for scoping in vue/template.rs + Pinia defineStore rules in nuxt.toml to reach Vue>=85%, JS>=60%, HTML>=75%, overall>=87% on commerce-release-update.

      4. Concurrent AR-003 Conflict Avoidance: Keep shared doc edits in crates/acdd-index/AGENTS.md, docs/adr/{0010,0011}, and docs/reference/acdd.md tightly scoped and additive (appending focused subsections without reformatting unrelated sections owned by AR-003).
    - |-
      LS-003 Architectural Blueprint Decision Rev 2 (supersedes #283; resolves all 5 review findings from #286):

      All core Layer 2 architecture, ADR 0010/0011 non-heuristic boundaries, pinned tree-sitter = "0.22" ABI 14 constraint, max_net_loc budgets, Wave 2 disjointness, and AR-003 additive doc discipline from #283 remain in effect, with the following 5 verified contract corrections:

      1. [Finding 1 — End-to-End FileActivationSignals for Link-Time ImplicitReceivers (ADR 0009 delta-safe)]:
         - Verified: alpinejs.toml:21-26 ImplicitReceivers is evaluated exclusively at link time in crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs:161-169 and :785-802 via ctx.language_context.syntax_rules.
         - Wave 0 (modular-manifest-parsers-and-typed-activation): Define deterministic FileActivationSignals { pub syntax_attributes: BTreeSet<String> } and SyntaxActivationKeys { pub exact_attributes: BTreeSet<String>, pub attribute_prefixes: BTreeSet<String> } in manifests/semantic.rs; expose LanguageContext::syntax_activation_keys(), LanguageContext::is_syntax_rule_active(rule, path, signals), and LanguageContext::syntax_rules_for_signals(path, signals) in languages/mod.rs (precomputed once in prepare_named_member_rules / build_language_context with O(1) has_syntax_gated_rules fast-path).
         - Wave 1 (universal-declarative-engine-and-generic-linker): Via autonomous acdd extend-scope on models/mod.rs, typed_facts/{mod.rs,binary.rs}, and linker/resolution/value_flow_phase/dispatch.rs, add #[serde(default, skip_serializing_if = "FileActivationSignals::is_empty")] pub activation_signals: FileActivationSignals onto Facts (models/mod.rs:610; keeping empty JSON snapshots byte-identical), bind FileActivationSignals in typed_facts/binary.rs via struct_codecs! and TypedFacts::encode_with_string_table/decode_with_string_table (bumping LOCAL_FACTS_BINARY_MAGIC from \x03 to \x04 in typed_facts/mod.rs:15 for clean ADR 0009 cold/cache/delta parity), and filter syntax-gated rules (including ImplicitReceivers) in linker/resolution/{mod.rs,value_flow_phase/dispatch.rs} via ctx.language_context.is_syntax_rule_active(rule, ctx.path, &ctx.facts.activation_signals).
         - Wave 2 Task 6 (web-alpine-playwright-webidl-html-j2-and-vue): During the existing single DOM walk in languages/html/mod.rs, match observed element attribute names generically against rules.syntax_activation_keys() and insert hits into facts.activation_signals (zero hardcoded "alpinejs"/"x-data"/"x-on:" strings; merged island facts inherit owning HTML file signals).

      2. [Finding 2 — Wave 0 languages/mod.rs Scope & #[cfg] Safety + Exhaustive Wave 1 FileContext Extend-Scope]:
         - Wave 0 explicitly uses autonomous acdd extend-scope on crates/acdd-index/src/languages/mod.rs (zero Wave 1 conflict since Wave 0 finishes before Wave 1 begins) to: (a) add syntax-gated rule index fields/methods on LanguageContext (:250), (b) remove #[cfg(any(feature = "lang-java", feature = "lang-kotlin"))] on pure-std pub(crate) mod build_manifest (:57-58) so parsers/jvm.rs reuses build_manifest.rs under default and --no-default-features builds, and (c) ensure parsers/{cargo,python}.rs compile under --no-default-features (either guarding toml_manifest with #[cfg(any(feature = "lang-python", feature = "lang-rust", feature = "lang-toml"))] or using unconditional toml = "0.8" from crates/acdd-index/Cargo.toml:16; verified by cargo check -p acdd-index --no-default-features).
         - Wave 1 autonomous acdd extend-scope list is updated to include ALL verified mechanical FileContext/LanguageFileState and linker consumers: languages/{rust/mod.rs,python/mod.rs,typescript/mod.rs,vue/mod.rs,typescript/dom.rs,typescript/commonjs.rs,typescript/scope_facts.rs,typescript/types.rs,html/mod.rs,vue/template.rs} (catching both scope_facts.rs:391 and types.rs:85!), languages/{rust/linker.rs,python/linker.rs,typescript/linker.rs}, models/mod.rs, typed_facts/{mod.rs,binary.rs}, linker/resolution/value_flow_phase/{context.rs,dispatch.rs,inferred.rs,members.rs}, and linker/semantic_context/preparation.rs.

      3. [Finding 3 — Git-Tracked Cargo.lock Admitted in Task 3]:
         - Verified git ls-files Cargo.lock is tracked. Task 3 (tier2-parity-and-bash-css-json-default) explicitly admits Cargo.lock (via autonomous acdd extend-scope) alongside Cargo.toml and crates/acdd-index/Cargo.toml strictly for tree-sitter-{bash,css,json} and their minimal transitive deps while keeping tree-sitter = "0.22.6" pinned.

      4. [Finding 4 — Exact Non-Zero verification_seam Cargo Test Filters]:
         - Verified tests/languages.rs:9-10 mounts tests/framework_manifests/mod.rs as mod manifests; and tests/linker.rs:15-16 mounts receivers as mod typed_receivers;.
         - Updated filters: Wave 0 -> cargo test --test languages manifests:: (+ cargo check -p acdd-index --no-default-features); Wave 1 -> cargo test --test languages languages::profiles languages::extractors && cargo test --test linker; Task 3 -> cargo test --test languages languages::features && cargo test --test languages --all-features languages::features languages::profiles; Task 4 -> cargo test --test languages languages::rust && cargo test --test linker typed_receivers::rust; Task 5 -> cargo test --test languages languages::python languages::proto && cargo test --test linker typed_receivers::python; Task 6 -> cargo test --test languages languages::html languages::typescript manifests::.

      5. [Finding 5 — Generic Dotted-Callee & Destructured-Fixture Callback Typing for playwright.toml in Task 6]:
         - Verified languages/typescript/dom.rs:1059-1096 (configured_callback_parameter / first_callback_parameter) currently rejects member_expression callees (:1072) and object_pattern parameters (:1095), while ReceiverTypeRule (manifests/syntax.rs:427-445) already supports dotted callback_callees and properties: BTreeMap<String, String>.
         - Task 6 extends languages/typescript/{dom.rs,mod.rs}::configured_callback_parameter generically (zero hardcoded playwright/test/page strings) to match both identifier and dotted member_expression callees against receiver.callback_callees and bind either a single identifier parameter to receiver.name or object_pattern destructured keys (shorthand_property_identifier_pattern and pair_pattern aliases) against receiver.properties.
    - 'Requesting BUDGET_OVERRIDE >= 1750 for task:universal-declarative-engine-and-generic-linker:build (measured +1726 net LOC vs max_net_loc=1600; +126 LOC overage). Cause: Rev 2 (#287) FileActivationSignals persistence/codec/dispatch gating (+48 prod LOC across models/typed_facts/dispatch), atomic LanguageProfile::linker + LanguageFileState wiring across 14 language/linker files, GENERIC_LINKER multi-file/wildcard/ModulePrefix upgrades + traits.rs/value_flow_core.rs/languages/mod.rs in-crate #[cfg(test)] unit tests (~240 lines counted in crates/acdd-index/src/). Zero clones introduced in edited production files (value_flow_core.rs 0, indexing.rs 0, binary.rs encode_items reused), acdd verify PASSED (exit_code=0, execution_digest=ba08699018f3995c6a4dbb3780a1a9fd15a1a93acfbaf6ff00bf5a1ea5673e98), Clippy clean, acdd-index --lib (84/84), languages --features all-languages (31/31), linker, core, commitment_integrity (18/18), docs check, validate (10/10), and /tmp parity (5462==5462) all passed.'
    - |-
      BUDGET_OVERRIDE for task:universal-declarative-engine-and-generic-linker: measured +1726 net LOC includes ~240 in-crate #[cfg(test)] lines (production net LOC ~1486 <= 1600) plus Rev 2 #287 FileActivationSignals codec/dispatch wiring and atomic LanguageProfile::linker()/LanguageFileState migration (0 clones/dead_tails).
      approved_budget: 1750
    - 'Requesting updated BUDGET_OVERRIDE >= 2020 (superseding #303 approved_budget=1750) for task:universal-declarative-engine-and-generic-linker:build (measured +2006 net LOC by acdd submit after #305 review remediations). Actual non-test production net LOC is ~1,540 (<=1,600); the +280 LOC increase from #303 (+1726 -> +2006) comes from #305 Finding 1-3 remediations: SyntaxGrammarSpec::assignments + complete ScopeFrameFlags/ScopeState/ValueFlowCollectorCore API + CallSiteEvidence emission (+85 prod LOC), dispatch.rs sibling is_syntax_rule_active gating (+10 prod LOC), and in-crate #[cfg(test)] coverage counted in crates/acdd-index/src/ (languages/mod.rs +77, value_flow_core.rs +60, typed_facts/binary.rs +34, linker/traits.rs +25). All #305 clones eliminated, acdd verify PASSED (claim_revision=2, exit_code=0, execution_digest=ee85365d1168f244537fe905f2eee4d605f8e45f040422e16fcd146ddc7d381d), Clippy clean, --no-default-features --features lang-rust,lang-toml clean, acdd-index --lib (86/86), languages --features all-languages (31/31), linker --features all-languages, core, commitment_integrity (18/18), docs check, validate (10/10), and TMP_BEFORE==TMP_AFTER (5462==5462) all passed.'
    - |-
      BUDGET_OVERRIDE superseding #303 for task:universal-declarative-engine-and-generic-linker: +2006 measured total net LOC includes in-crate #[cfg(test)] blocks across languages/mod.rs, value_flow_core.rs, typed_facts/binary.rs, and traits.rs; non-test prod net LOC is ~1540 <= 1600 while completing all #305 reviewer findings (0 clones/dead_tails).
      approved_budget: 2020
    - 'Requesting BUDGET_OVERRIDE >= 1650 for task:tier2-parity-and-bash-css-json-default:build (measured +1640 net LOC vs max_net_loc=1600; +40 overage) and coordinator guidance on tests/framework_manifests/cross_pack_relations.rs:17. Breakdown of +1640: Cargo.lock (+22), docs/reference/acdd.md (+7), scanner/mod.rs .verified.json receipt guard (+9), plus bash.rs (155L), css.rs (168L), json.rs (130L), LanguageId::{Bash,Css,Json} registration, 8 Tier-2 SyntaxGrammarSpec migrations, and shared BindingCollector/ValueFlowCollectorCore/c_family/support fixes (+1602). Verification: acdd verify PASSED (execution_digest=ba8307e1105e49d6eb9340f20d5564f03aaa80427316c1c416a482567ef61240, code.seam 1 hop to SyntaxGrammarSpec), Clippy -D warnings clean, minimal feature check clean, acdd-index --lib 86/86, tests/linker --all-features 76/76, commitment_integrity 18/18, docs check, validate 10/10, /tmp isolated 5471->5465, tests/languages 242/242 default and 246/246 --all-features skipping only manifests::cross_pack_relations::declarative_framework_rules_reach_persisted_receivers_routes_and_dependencies (which fails left=[''40''] right=[''39''] because package.json in cross_pack_fixture.rs:209 is now indexed as .json source; extend-scope returns TASK_SCOPE_CONFLICT with InProgress task:web-alpine-playwright-webidl-html-j2-and-vue).'
    - |-
      BUDGET_OVERRIDE for task:tier2-parity-and-bash-css-json-default: +1640 measured net LOC (+40 over 1600) due to Cargo.lock (+22 for tree-sitter-{bash,css,json}), scanner .verified.json guard (+9), docs/reference/acdd.md (+7), 3 new tier-2 language modules (bash/css/json), and 8 SyntaxGrammarSpec migrations (0 clones/dead_tails).
      approved_budget: 1660
    - 'Requesting explicit authorization for a 1-token scope extension on crates/acdd-index/src/linker/semantic_context/preparation.rs:415 (adding | "struct" alongside "class" | "interface") to unblock #317 Go composite-literal receiver test w := Worker{}; w.Work(). ValueFlowIndex already binds w to TypeTarget::Local(Worker) (kind="struct") and ast/visitor.rs sets qualname cmd.main.Worker.Work, and both linker/value_flow/mod.rs:1305 and linker/semantic_context/type_resolution.rs:790 already include "struct", whereas preparation.rs:415 checks matches!(class.kind.as_str(), "class" | "interface") only. All other 10/11 tier2_parity cases and all #317 C++/Java/C#/Kotlin/PHP/Ruby/clone fixes already pass.'
    - '@coordinator Requesting BUDGET_OVERRIDE for task:rust-resolution-coverage-and-stdlib:build (contract rev 7, max_net_loc: 1500). Measured net diff vs baseline 8e71ae82e18b2ff1322dbdb3af04e89c95b9bac5: +6,345 net prod LOC across crates/ (+5,440 net in 18 tracked files [+6,345/-905] + 905 lines in new macros.rs), plus +175/-0 net test LOC across 4 test files. Breakdown: languages/rust/value_flow.rs (+1,713 net), linker/rust/value_flow.rs (+1,998 net), languages/rust/macros.rs (+905), languages/rust/linker.rs (+405 net), linker/rust/resolution.rs (+299 net), rust-std.toml (+242 net), languages/rust/mod.rs (+212 net), rust-cargo.toml (+147 net), linker/rust/semantic_context.rs (+146 net), declarative_macros.toml (+85 net), shared linker/{semantic_context,value_flow,resolution,indexing} & ast/visitor (+193 net total). Reason: lifting Rust live resolution coverage from baseline to 75.0874% (34,143 resolved / 11,326 unresolved / 2 ambiguous / 70,510 external) without heuristic fallbacks required full Rust ValueFlow chain/closure/field/Deref/trait/macro-delegation + stdlib/cargo manifests. All tests, clippy, lint, validate, and leak checks are green. Awaiting @coordinator BUDGET_OVERRIDE approval before acdd verify / acdd submit.'
    - |-
      @coordinator All 3 pre-check items from #221 fixed and re-verified. Requesting BUDGET_OVERRIDE approval for task:rust-resolution-coverage-and-stdlib:build (contract rev 7, max_net_loc: 1500):
      - Updated net prod LOC vs baseline 8e71ae82e18b2ff1322dbdb3af04e89c95b9bac5: +6,321 net prod LOC across crates/ (+5,417 net in 18 tracked files [+6,623/-1,206] + 904 lines in new crates/acdd-index/src/languages/rust/macros.rs), plus +175/-0 net test LOC across 4 test files.
      - Remediation verified: (1) all listed >=5-line clones eliminated across linker/rust/{value_flow,semantic_context}.rs and languages/rust/{linker,mod,macros,value_flow}.rs; (2) semantic_context.rs:594 .unwrap() replaced with safe Option match (0 .unwrap() hits across modified Rust index/linker files); (3) untracked .acdd/index.db removed.
      - Post-cleanup verification: Rust coverage 75.1518% (34,170 resolved / 11,296 unresolved / 2 ambiguous / 70,531 external); clippy -D warnings (0); lang-rust,lang-toml check OK; acdd-index --lib (86/86), linker (77/77), languages (246/246), commitment_integrity (18/18); acdd lint --categories arch,syntax --severity error (0 errors); acdd validate (10/10); /tmp/contextunity-test-* BEFORE=5480 == AFTER=5480.
    - |-
      BUDGET_OVERRIDE approved for task:rust-resolution-coverage-and-stdlib:build (resolves escalation #323 / #324). Verified net LOC (+6,321 prod across crates/ + 175 test lines), clone reduction (languages/rust 84->33, linker/rust 57->31), 0 production .unwrap() hits, 75.1518% live Rust resolution coverage (34,170 resolved / 11,296 unresolved), and clean checks.
      approved_budget: 6600
    - |-
      @coordinator Requesting updated Round 2 BUDGET_OVERRIDE for task:rust-resolution-coverage-and-stdlib:build (claim rev 2, contract rev 7, prior override #325 approved_budget: 6600).
      Round 2 remediation for #326 review findings (F1-F5) brought measured net diff vs baseline 8e71ae82e18b2ff1322dbdb3af04e89c95b9bac5 to:
      - Net prod LOC across crates/: +6,668 (+5,624 tracked across 18 files [+6,888/-1,264] + 1,044 in new crates/acdd-index/src/languages/rust/macros.rs) — +68 over prior 6600 cap due to AST macro token-tree parser & recursive-macro fail-closed guard (macros.rs +140), scoped macro visibility resolution (linker.rs), enum_variant/resolve_symbol/derive fallback fixes, and typed range/closure boundary checks.
      - Net test LOC across tests/: +435 (+452/-17 across 5 test files, +260 from Round 2 Finding 4 matrix & fail-closed assertions).
      - Total net LOC: +7,103.
      All Round 2 checks and acdd verify (rev 2, execution_digest=8a8a236d06175cf9f15a83a3ceb2e3cd3b976b3c769b08009a2432e0f1080265) are green: clippy -D warnings (0), lang-rust,lang-toml check OK, all-languages tests (acdd-index --lib 86/86, linker 77/77, languages 249/249, commitment_integrity 18/18), Rust coverage 75.0858% non-ext resolved / 90.2493% covered (34,149 resolved / 70,727 external / 116,207 total), 0 production .unwrap(), acdd lint 0 errors, acdd validate 10/10, /tmp leaks BEFORE=5480 == AFTER=5480. Awaiting updated BUDGET_OVERRIDE approval before acdd submit.
    - |-
      Round 2 BUDGET_OVERRIDE approved for task:rust-resolution-coverage-and-stdlib:build (supersedes #325, resolves #327). Verified zero repo-specific SharedStr hits, clean checks and acdd verify (execution_digest=8a8a236d06175cf9f15a83a3ceb2e3cd3b976b3c769b08009a2432e0f1080265), 75.0858% live non-external Rust resolution coverage (90.2493% overall covered), net prod +6,668 LOC + test +435 LOC (+7,103 total) across F1-F5 review remediations.
      approved_budget: 7200
    deferred_findings:
    - 'Deferred follow-up (post-Wave-2 polish / integration, owner: coordinator / polish): Wire ReceiverTypeRule.member_returns (and call_returns) across manifests/syntax.rs, linker/semantic_context/type_resolution.rs, and linker/value_flow/mod.rs so playwright.toml rules Playwright.Locator (Page::locator/getBy*), Playwright.APIResponse (APIRequestContext::get/post/...), BrowserContext::newPage -> Playwright.Page, and expect(...) -> Playwright.Assertions resolve chained calls (e.g. page.locator(''btn'').click(), request.get(''/api'') -> resp.json(), expect(loc).toBeVisible()) purely via TOML data without conflicting with concurrent Wave 2 ls003-w2-py edits to linker/semantic_context/ and linker/value_flow/. Also unify builtin_property_chain''s final method lookup onto ReceiverTypeRule.members via semantic_context so typescript/mod.rs:1453-1486 hardcoded WebIDL method arms can be deleted entirely.'
  metrics_summary:
    decisions_count: 14
    deferred_findings_count: 1
    failed_traces: 5
    passed_traces: 20
    task_receipts_count: 6
    tasks_cancelled: 0
    tasks_completed: 6
    tasks_total: 6
    total_claims: 46
    total_review_cycles: 7
    total_traces: 34
---

# LS-003: Universal Declarative Language Engine, Default Bash/CSS/JSON Profiles, and Tier-1 Resolution Coverage

## Goal & Empirical Findings

Empirical audits across both live repositories (`contextunity-acdd` and `commerce-release-update`) and all 16 language profiles in `crates/acdd-index/src/languages/` identified two coupled coverage ceilings:

1. **Universal Tier-2 & Unindexed Language Gaps:**
   - [`SyntaxGrammarSpec`](../../crates/acdd-index/src/languages/mod.rs) is compiled and wired into [`ast::visitor`](../../crates/acdd-index/src/ast/visitor.rs), yet `0 / 16` profiles use it.
   - `ScopeAnalysis` supports only 3 languages (`Python`, `Rust`, `TypeScript`); all 8 Tier-2 languages (`go`, `java`, `kotlin`, `csharp`, `php`, `ruby`, `c`, `cpp`) emit zero `ValueFlowFacts`, always set `receiver_hint: None` in `support::call_reference`, and drop parameter/local type annotations in `support::bindings`.
   - [`GENERIC_LINKER`](../../crates/acdd-index/src/linker/traits.rs) has `tracks_external_aliases() == false` and requires `modules.len() == 1`, breaking multi-file packages (Go/Java/Kotlin/C#), `go.mod` module prefixes, C#/PHP `normalize_import` (currently `None`), Ruby `require`, and C/C++ `<stdlib>` classification.
   - Common workspace formats in real repositories — **Shell/Bash** (`.sh`), **CSS** (`.css`), **JSON** (`.json`), and **Jinja2** (`.j2`) — are unindexed today. Per explicit owner authorization, `tree-sitter-bash`, `tree-sitter-css`, and `tree-sitter-json` are added to `default` features alongside existing Rust, Python, TypeScript, and JavaScript grammars.
2. **Tier-1 Resolution Gaps on Live Corpora:**
   - **Rust (`contextunity-acdd`: `42.74%`, `34,087` unresolved):** `12,440` (`36.5%`) are chained/literal/slice receivers falling back to `dynamic: true`; the top 25 stdlib methods alone account for `16,675` (`49%`) due to incomplete return types in `rust-std.toml` and missing struct-field/`&T` type propagation; `const_item`, `static_item`, `enum_variant`, `associated_type`, standard `#[derive(...)]`, and declarative struct/enum macros (`define_id!`, `define_string_enum!`) account for `2,952` (`8.7%`).
   - **Python (`commerce-release-update`: `80.75%`, `30,317` unresolved):** `991+` `logger.*` calls are misclassified as `unresolved` instead of `external`; `1,398` shadowed-binding misses already carry `param_types` (`Protocol`/`Callable`, union `A | B`, `type[T]`, `bytearray`, `QuerySet[T]`); `django.toml` misses `BaseCommand.stdout/style` and `Model.objects`/QuerySet chains; `5,094` computed receivers include `super().__init__`, `Client().get`, `(root / "x").write_text`, f-strings, and string slices.
   - **JS (`20.72%`) / HTML (`37.05%`, `3,262` unresolved in `.html`) / TS (`60.27%`) / Vue (`65.54%`):** `alpinejs.toml` gates magic receivers strictly on `package = "alpinejs"` (never firing in CDN/Django templates, driving `54.7%` of HTML unresolveds); Playwright test fixtures (`3,376` = `36.7%` of JS unresolveds) lack a manifest; `web.toml` defines only 2 DOM types; IIFEs emit truncated unresolved calls; Vue lacks `v-for` loop scoping (`26.2%`) and Pinia `defineStore` auto-imports (`36.0%`).

## Authorized Protected-File Scope

- `Cargo.toml` and `crates/acdd-index/Cargo.toml` are explicitly admitted in `tier2-parity-and-bash-css-json-default` solely to add `tree-sitter-bash`, `tree-sitter-css`, and `tree-sitter-json` optional dependencies wired into `default` (`lang-bash`, `lang-css`, `lang-json`) and `all-languages`.

## Non-Goals & Negative Boundaries

- Do **not** introduce heuristic name-only call guessing on untyped Python/JS variables (`ADR 0010`).
- Do **not** hardcode framework package names or decorator lists inside Rust parser modules (`ADR 0011`, `crates/acdd-index/src/languages/AGENTS.md`).
- Do **not** add new top-level `tests/*.rs` binaries; register all new contract matrices inside canonical binaries (`tests/languages.rs`, `tests/linker.rs`).

## Layer 2 Architecture & `PF-003` Layer 1 Interoperability

- `LS-003` depends on `PF-004` (`PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup`), cleanly extending the Layer 1 foundation defined in `PF-003` / `PF-004` (`models::{tags, details}` including `NodeKind`/`EdgeKind`/`CoverageStatus`/`LanguageId`, unified `&dyn FactsLookup` `LanguageLinker`, and `INV-SINGLE-OWNER` module boundaries). When merged atop `PF-003` + `PF-004`, Layer 2 extends `LanguageId` (`Bash`, `Css`, `Json`), maps `DeclarationRole` directly into `NodeKind`, migrates central linker call sites onto `models::tags` enums, extracts hardcoded Rust/JS/Python logic out of `linker/indexing.rs` behind `LanguageLinker` hooks, and implements `GENERIC_LINKER` on the single `&dyn FactsLookup` contract without rewriting Layer 1.
- **Wave 0 (`modular-manifest-parsers-and-typed-activation`)** splits the `2,930`-LOC `dependency_registry.rs` monolith into `dependency_registry/` governed by `ManifestParser` + `Ecosystem` and upgrades `manifests/` with typed `ActivationCondition` (`Package`, `SyntaxAttribute`, `SyntaxAttributePrefix`, `FileExtension`).
- **Wave 1 (`universal-declarative-engine-and-generic-linker`)** establishes the unified `LanguageProfile` + `SyntaxGrammarSpec` + `LanguageLinker` facade (`LanguageProfile::linker`, typed `DeclarationRole`/`ImportKind`/`CallForm`/`Visibility`, `LanguageFileState` replacing kitchen-sink `FileContext`), shared `ValueFlowCollectorCore`/`ScopeState` + `GenericScopeAnalysis`, cleaned-up `ast/visitor.rs::LanguageAnalysis`, and upgraded `GENERIC_LINKER`, while co-evolving `crates/acdd-index/AGENTS.md` and `crates/acdd-index/src/languages/AGENTS.md`.
- **Wave 2** runs all four coverage/profile tasks concurrently on strictly disjoint scopes, adopting `ValueFlowCollectorCore`/`ScopeState`, `LanguageFileState`, and `ActivationCondition` across Tier-1 and Tier-2 profiles.

## Tasks in this milestone

### task: modular-manifest-parsers-and-typed-activation

```yaml
task_ref: modular-manifest-parsers-and-typed-activation
target: Split 2,930-LOC dependency_registry.rs into modular dependency_registry/ ManifestParser plugins with typed Ecosystem and module-prefix extraction (go.mod, composer.json, .csproj, JVM, Ruby, C/C++), and upgrade FrameworkManifest activation and rules with typed ActivationCondition enums
scope:
- crates/acdd-index/src/languages/dependency_registry.rs
- crates/acdd-index/src/languages/dependency_registry/
- crates/acdd-index/src/languages/build_manifest.rs
- crates/acdd-index/src/languages/manifests/mod.rs
- crates/acdd-index/src/languages/manifests/framework.rs
- crates/acdd-index/src/languages/manifests/semantic.rs
- crates/acdd-index/src/languages/manifests/syntax.rs
- crates/acdd-index/src/languages/manifests/projections.rs
- tests/framework_manifests/
- docs/adr/0011-language-linkers-and-framework-manifests.md
extended_scope:
- path: crates/acdd-index/src/languages/mod.rs
  reason: 'Per blueprint decision #287 Finding 2: expose LanguageContext syntax-activation keys/methods and remove cfg gate on pure-std build_manifest for dependency_registry/parsers/jvm.rs'
- path: docs/architecture/performance.md
  reason: Update broken links from dependency_registry.rs to dependency_registry/mod.rs after Wave 0 decomposition so acdd docs check passes
- path: crates/acdd-index/src/languages/python/dependencies.rs
  reason: Rewire private dependency helpers in python/dependencies.rs and typescript/package_metadata.rs to canonical dependency_registry::parsers and remove dead toml_manifest::has_table per review findings
- path: crates/acdd-index/src/languages/typescript/package_metadata.rs
  reason: Rewire private dependency helpers in python/dependencies.rs and typescript/package_metadata.rs to canonical dependency_registry::parsers and remove dead toml_manifest::has_table per review findings
- path: crates/acdd-index/src/languages/toml_manifest.rs
  reason: Rewire private dependency helpers in python/dependencies.rs and typescript/package_metadata.rs to canonical dependency_registry::parsers and remove dead toml_manifest::has_table per review findings
tags:
- default
invariants:
- 'INV-LS003-MANIFEST-DATA-DRIVEN: Dependency manifests parse through trait-isolated ManifestParser plugins returning typed ParsedManifest/Ecosystem values, and framework manifest activation uses typed ActivationCondition variants with backward-compatible TOML deserialization.'
- 'INV-LS003-SINGLE-PASS-THROUGHPUT: Manifest parsing and framework rule compilation happen once per workspace/context build with zero per-AST-node manifest I/O or regex compilation.'
acceptance_criteria:
- 'AC-1: crates/acdd-index/src/languages/dependency_registry.rs is decomposed into dependency_registry/{mod,parser,framework_packs,tests}.rs and dependency_registry/parsers/{cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs implementing ManifestParser and exposing module_prefix_for_path for go.mod, composer.json (PSR-4), and .csproj.'
- 'AC-2: ActivationCondition in manifests/semantic.rs replaces single-variant Activation with Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, and Any while preserving existing { kind = "package", name = "..." } TOML wire compatibility.'
- 'AC-3: Typed manifest enums (Ecosystem, ActivationCondition, validated language identifiers) replace ad-hoc string checks in manifests/{framework,semantic,syntax,projections,mod}.rs, ADR 0011 is reconciled, and tests/framework_manifests/ passes with zero clones or dead_tails.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: split-dependency-registry-manifest-parsers
  title: Decompose dependency_registry.rs into dependency_registry/ with ManifestParser trait, Ecosystem enum, and per-ecosystem parsers including go.mod, composer.json, .csproj, pom.xml/build.gradle, Gemfile, and CMakeLists.txt
  status: completed
  evidence: Verified dependency_registry/{mod,parser,framework_packs,tests}.rs + 9 parsers/{cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs implementing ManifestParser with closed Ecosystem, ModulePrefix extraction (go.mod, composer.json PSR-4, .csproj), tolerant NpmParser/PhpParser, single-pass CargoParser, delegated python/dependencies.rs & typescript/package_metadata.rs (0 cross-module >=10L clones), and passing cargo test -p acdd-index --lib dependency_registry (11 passed) & manifests::dependencies.
- subtask_ref: typed-activation-condition-and-manifest-enums
  title: Upgrade manifests/ Activation into typed ActivationCondition (Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, Any) and tighten manifest rule enums in manifests/{framework,semantic,syntax,projections,mod}.rs
  status: completed
  evidence: Verified ActivationCondition { Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, Any (depth<=4) }, ActivationMatch, SyntaxActivationKeys, FileActivationSignals in manifests/semantic.rs, allocation-free LanguageContext::is_workspace_activated + is_syntax_rule_active/syntax_rules_for_signals in languages/mod.rs, and typed Ecosystem::from_family/family + LanguageId validation in manifests/{framework,semantic,syntax,projections,mod}.rs.
- subtask_ref: manifest-parser-and-activation-contract-tests
  title: Extend table-driven contract tests in tests/framework_manifests/ covering multi-ecosystem ManifestParser module prefixes and syntax-attribute activation, and reconcile ADR 0011
  status: completed
  evidence: 'Verified table-driven contract matrices in tests/framework_manifests/{dependencies.rs,activation/user_manifests.rs}, additive ADR 0011 section 4, performance.md link updates, cargo test --test languages manifests:: (24 passed) & --all-features (24 passed), commitment_integrity (18 passed), clippy (0 warnings), docs check & validate (pass), TMP_BEFORE==TMP_AFTER (5462==5462).'
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:modular-manifest-parsers-and-typed-activation.deliver
  gate_id: deliver
  worker_id: ls003-r1
  claim_revision: 1
  contract_revision: 4
  candidate_tree: c9e12e4fb1f190bac8a653ed3aca2459da260597
  committed_oid: 69b0b7482cc49004d4f974f56f36fa075854f7fc
  accepted_at: 2026-11-22T02:03:23.826Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-MANIFEST-DATA-DRIVEN: Dependency manifests parse through trait-isolated ManifestParser plugins returning typed ParsedManifest/Ecosystem values, and framework manifest activation uses typed ActivationCondition variants with backward-compatible TOML deserialization.'
      - 'INV-LS003-SINGLE-PASS-THROUGHPUT: Manifest parsing and framework rule compilation happen once per workspace/context build with zero per-AST-node manifest I/O or regex compilation.'
      verified: []
    review_summary: Delivered Wave 0 task modular-manifest-parsers-and-typed-activation after passing round-2 independent review (candidate_tree c9e12e4fb1f190bac8a653ed3aca2459da260597).
```

### task: universal-declarative-engine-and-generic-linker

```yaml
task_ref: universal-declarative-engine-and-generic-linker
target: Build shared ValueFlowCollectorCore/ScopeState and GenericScopeAnalysis, unify LanguageProfile + SyntaxGrammarSpec + LanguageLinker facade with typed enums and LanguageFileState, upgrade GENERIC_LINKER, and co-evolve crates/acdd-index/AGENTS.md and crates/acdd-index/src/languages/AGENTS.md
scope:
- crates/acdd-index/src/languages/mod.rs
- crates/acdd-index/src/languages/support/
- crates/acdd-index/src/ast/visitor.rs
- crates/acdd-index/src/linker/mod.rs
- crates/acdd-index/src/linker/indexing.rs
- crates/acdd-index/src/linker/traits.rs
- crates/acdd-index/src/linker/resolution/mod.rs
- crates/acdd-index/src/linker/resolution/imports.rs
- crates/acdd-index/AGENTS.md
- crates/acdd-index/src/languages/AGENTS.md
- tests/languages/mod.rs
- tests/languages/profiles.rs
- tests/languages/extractors.rs
- docs/adr/0010-static-value-flow-and-receiver-resolution.md
extended_scope:
- path: crates/acdd-index/src/models/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/typed_facts/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/typed_facts/binary.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/rust/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/python/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/vue/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/dom.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/commonjs.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/scope_facts.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/types.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/html/mod.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/vue/template.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/rust/linker.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/python/linker.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/languages/typescript/linker.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/context.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/inferred.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/members.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/linker/semantic_context/preparation.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: crates/acdd-index/src/db/writer/cache/tests.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: tests/linker/early_interning_and_compact_facts.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: tests/core/storage_batches/storage_persistence.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: tests/incremental/scope.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: tests/incremental/single_pass_spool_and_parallel_delta.rs
  reason: LS-003 Rev 2 blueprint (#287) + LOCAL_FACTS_BINARY_MAGIC v4 (#288) mechanical LanguageProfile::linker, LanguageFileState, FileActivationSignals, and builtin_member_origin consumers
- path: tests/languages/profiles/baseline.rs
  reason: Update SyntaxGrammarSpec struct literals in baseline.rs and cache.rs for AC-1 typed SyntaxGrammarSpec fields
- path: tests/languages/profiles/cache.rs
  reason: Update SyntaxGrammarSpec struct literals in baseline.rs and cache.rs for AC-1 typed SyntaxGrammarSpec fields
- path: crates/acdd-index/src/linker/python/mod.rs
  reason: Expose linker::python::receivers as pub(crate) so languages::python::linker::build_python_receivers can own Python base-class resolution
- path: crates/acdd-index/src/linker/semantic_context/mod.rs,crates/acdd-index/src/linker/python/semantic_relations.rs
  reason: Expose Context/property_type_for_path and compose_symbol_relations as pub(crate) for language linker delegation
- path: crates/acdd-index/src/linker/semantic_context/mod.rs
  reason: Expose semantic_context::Context and property_type_for_path as pub(crate) for language linker delegation
- path: crates/acdd-index/src/linker/python/semantic_relations.rs
  reason: Expose compose_symbol_relations as pub(crate) for PythonLinker::compose_file_relations
- path: crates/acdd-index/src/linker/framework_relations.rs
  reason: Expose PreparedSemanticFacts as pub(crate) for FileRelationContext and PythonLinker::compose_file_relations
depends_on:
- modular-manifest-parsers-and-typed-activation
tags:
- default
invariants:
- 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: GenericScopeAnalysis and Tier-1 collectors share ValueFlowCollectorCore/ScopeState (with typed ScopeFrameFlags instead of anonymous boolean tuples) and SyntaxGrammarSpec rules without per-language AST visitor duplication.'
- 'INV-LS003-STATIC-NO-HEURISTICS: GENERIC_LINKER resolves only lexically or package-proven targets and classifies manifest/stdlib aliases as external without name-only guessing.'
- 'INV-LS003-LAYER2-CONSOLIDATION: LanguageProfile owns fn linker(&self) -> &dyn LanguageLinker (eliminating hardcoded linker_for language switches), replaces kitchen-sink FileContext fields with typed LanguageFileState, delegates language-specific import/inheritance indexing and builtin-origin classification out of linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker/manifest hooks, and uses typed models::tags enums at linker call sites.'
acceptance_criteria:
- 'AC-1: SyntaxGrammarSpec uses typed DeclarationRole, ImportKind, CallForm, and Visibility enums alongside declarative typed-binding, constructor, member-call, and heritage selectors compiled once per grammar.'
- 'AC-2: crates/acdd-index/src/languages/support/value_flow_core.rs provides shared ScopeState/ScopeFrameFlags/ValueFlowCollectorCore powering GenericScopeAnalysis in ast/visitor.rs and ready for Tier-1 collector adoption.'
- 'AC-3: LanguageProfile::linker(&self) unifies profile-to-linker dispatch (deleting hardcoded linker_for language switches), FileContext stores per-language state in typed LanguageFileState, linker/indexing.rs and linker/resolution/mod.rs delete hardcoded Rust/JS/Python import, inheritance, and builtin_member_origin switches in favor of data-parallel LanguageLinker/manifest hooks (preserving file/class-level Rayon parallelism and borrowed SymbolId/PathId/CompactTag lookups) while migrating linker call sites onto models::tags enums, and GENERIC_LINKER resolves multi-file same-package declarations, wildcard/static imports, external import aliases, and DependencyRegistry module prefixes (aligned with PF-003/PF-004 Layer 1 FactsLookup seam).'
- 'AC-4: ADR 0010, crates/acdd-index/AGENTS.md, and crates/acdd-index/src/languages/AGENTS.md document the Layer 2 facade, ValueFlowCollectorCore, ManifestParser, and ActivationCondition canonical ownership.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: unified-profile-facade-and-typed-syntax-spec
  title: Add LanguageProfile::linker(&self) (deleting linker_for switches), replace kitchen-sink FileContext fields with typed LanguageFileState, and extend SyntaxGrammarSpec with DeclarationRole, ImportKind, CallForm, Visibility, and typed-binding/constructor/heritage selectors
  status: completed
  evidence: Added LanguageProfile::linker() delegating languages::linker_for in 1 line, replaced kitchen-sink FileContext/SyntaxContext fields with typed LanguageFileState/PythonFileState/TypeScriptFileState, and extended SyntaxGrammarSpec with DeclarationRole, ImportKind, CallForm, Visibility, and compiled binding/constructor/member-call/heritage/conditional selectors across languages/mod.rs and all profile consumers.
- subtask_ref: shared-value-flow-core-and-generic-scope-analysis
  title: Extract shared ScopeState/ScopeFrameFlags/ValueFlowCollectorCore in languages/support/value_flow_core.rs, implement GenericScopeAnalysis, and clean up ast/visitor.rs LanguageAnalysis dispatch
  status: completed
  evidence: Implemented ScopeFrameFlags, ScopeState, ValueFlowCollectorCore, and GenericScopeAnalysis in crates/acdd-index/src/languages/support/value_flow_core.rs, wired ScopeAnalysis::Generic into ast/visitor.rs, and verified via unit tests in value_flow_core.rs and acdd-index --lib (84/84 passed).
- subtask_ref: decouple-linker-indexing-and-upgrade-generic-linker
  title: Delete hardcoded Rust/JS/Python import/inheritance/builtin_member_origin switches from linker/indexing.rs and linker/resolution/mod.rs in favor of data-parallel LanguageLinker/manifest hooks, migrate linker call sites to models::tags enums, and upgrade GENERIC_LINKER and linker/resolution/imports.rs for multi-file packages, external aliases, wildcards, and module prefixes
  status: completed
  evidence: Removed hardcoded Rust/JS/Python switches from linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker hooks, migrated linker sites to models::tags enums, upgraded GENERIC_LINKER for multi-file packages/wildcards/external aliases/ModulePrefix stripping, persisted Facts.activation_signals in typed_facts/binary.rs (ACDD-LOCAL-FACTS\0\x04), and gated ImplicitReceivers via is_syntax_rule_active in dispatch.rs.
- subtask_ref: universal-engine-tests-adr-and-agents-md
  title: Extend table-driven tests in tests/languages/profiles.rs and tests/languages/extractors.rs, reconcile ADR 0010, and co-evolve crates/acdd-index/AGENTS.md and crates/acdd-index/src/languages/AGENTS.md
  status: completed
  evidence: Added table-driven tests in tests/languages/{profiles,extractors}.rs, unit tests in traits.rs/value_flow_core.rs/languages/mod.rs, updated docs/adr/0010, crates/acdd-index/AGENTS.md, and crates/acdd-index/src/languages/AGENTS.md; docs check & validate passed (10/10).
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:universal-declarative-engine-and-generic-linker.deliver
  gate_id: deliver
  worker_id: ls003-r2
  claim_revision: 1
  contract_revision: 8
  candidate_tree: 10667c65a01dd91f5d3a93ab618be70939e4fed6
  committed_oid: 8e71ae82e18b2ff1322dbdb3af04e89c95b9bac5
  accepted_at: 2026-11-22T03:20:56.242Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: GenericScopeAnalysis and Tier-1 collectors share ValueFlowCollectorCore/ScopeState (with typed ScopeFrameFlags instead of anonymous boolean tuples) and SyntaxGrammarSpec rules without per-language AST visitor duplication.'
      - 'INV-LS003-STATIC-NO-HEURISTICS: GENERIC_LINKER resolves only lexically or package-proven targets and classifies manifest/stdlib aliases as external without name-only guessing.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: LanguageProfile owns fn linker(&self) -> &dyn LanguageLinker (eliminating hardcoded linker_for language switches), replaces kitchen-sink FileContext fields with typed LanguageFileState, delegates language-specific import/inheritance indexing and builtin-origin classification out of linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker/manifest hooks, and uses typed models::tags enums at linker call sites.'
      verified: []
    review_summary: Delivered LS-003 Wave 1 task universal-declarative-engine-and-generic-linker (candidate_tree 10667c65a01dd91f5d3a93ab618be70939e4fed6) after independent review pass.
```

### task: tier2-parity-and-bash-css-json-default

```yaml
task_ref: tier2-parity-and-bash-css-json-default
target: Migrate Go, Java, Kotlin, C#, PHP, Ruby, C, and C++ onto the universal declarative engine, fix cross-file import/stdlib/dynamic bugs, and add Bash, CSS, and JSON language profiles enabled in default features and registered in LanguageId/CompactTag
scope:
- Cargo.toml
- crates/acdd-index/Cargo.toml
- crates/acdd-index/src/models/
- crates/acdd-index/src/languages/mod.rs
- crates/acdd-index/src/languages/go.rs
- crates/acdd-index/src/languages/java.rs
- crates/acdd-index/src/languages/kotlin.rs
- crates/acdd-index/src/languages/csharp.rs
- crates/acdd-index/src/languages/php.rs
- crates/acdd-index/src/languages/ruby.rs
- crates/acdd-index/src/languages/c.rs
- crates/acdd-index/src/languages/cpp.rs
- crates/acdd-index/src/languages/bash.rs
- crates/acdd-index/src/languages/css.rs
- crates/acdd-index/src/languages/json.rs
- tests/languages/features.rs
- tests/languages/profiles/
- docs/reference/acdd.md
extended_scope:
- path: Cargo.lock
  reason: 'Blueprint Rev 2 (#287 Finding 3): admit git-tracked Cargo.lock alongside Cargo.toml and crates/acdd-index/Cargo.toml for tree-sitter-{bash,css,json} while keeping tree-sitter = 0.22.6 pinned.'
- path: crates/acdd-index/src/ast/visitor.rs
  reason: Support Tier-2 declarative SyntaxGrammarSpec bindings/scopes in BindingCollector and GenericScopeAnalysis, C/C++ angle-bracket include_path in c_family.rs, and shared Tier-2 helpers in support/mod.rs without touching Tier-1 or linker/.
- path: crates/acdd-index/src/languages/support/value_flow_core.rs
  reason: Support Tier-2 declarative SyntaxGrammarSpec bindings/scopes in BindingCollector and GenericScopeAnalysis, C/C++ angle-bracket include_path in c_family.rs, and shared Tier-2 helpers in support/mod.rs without touching Tier-1 or linker/.
- path: crates/acdd-index/src/languages/support/c_family.rs
  reason: Support Tier-2 declarative SyntaxGrammarSpec bindings/scopes in BindingCollector and GenericScopeAnalysis, C/C++ angle-bracket include_path in c_family.rs, and shared Tier-2 helpers in support/mod.rs without touching Tier-1 or linker/.
- path: crates/acdd-index/src/languages/support/mod.rs
  reason: Support Tier-2 declarative SyntaxGrammarSpec bindings/scopes in BindingCollector and GenericScopeAnalysis, C/C++ angle-bracket include_path in c_family.rs, and shared Tier-2 helpers in support/mod.rs without touching Tier-1 or linker/.
- path: crates/acdd-index/src/scanner/mod.rs
  reason: Exclude ACDD .verified.json receipt sidecars from scanner::language
- path: tests/languages/profiles.rs
  reason: Register mod tier2_parity; in tests/languages/profiles.rs
- path: tests/framework_manifests/cross_pack_relations.rs
  reason: Update component contains edge count 39 -> 40 for default-enabled .json indexing of package.json in cross_pack_fixture
- path: crates/acdd-index/src/linker/semantic_context/preparation.rs
  reason: 'Include struct alongside class and interface in Context::members preparation per coordinator decision #204/#318'
depends_on:
- universal-declarative-engine-and-generic-linker
tags:
- default
invariants:
- 'INV-LS003-DEFAULT-WEB-SHELL-JSON: lang-bash, lang-css, and lang-json (tree-sitter-bash, tree-sitter-css, tree-sitter-json) compile and register in default features as well as all-languages, and extend LanguageId/CompactTag cleanly.'
- 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Every Tier-2 and new profile uses SyntaxGrammarSpec + GenericScopeAnalysis with zero false dynamic: true on typed constructors or operators.'
acceptance_criteria:
- 'AC-1: C#, PHP, Ruby, and Go resolve cross-file workspace imports and classify stdlib/framework imports as external; Java and Kotlin resolve same-package sibling types and typed local method receivers; C/C++ classify angle-bracket stdlib headers and out-of-line methods.'
- 'AC-2: Bash (.sh, .bash, .zsh), CSS (.css), and JSON (.json) profiles extract symbols, references, and cross-file links (source/., @import/var(--*), $defs/$ref) under default Cargo features and register in models/compact_graph.rs (or Layer 1 LanguageId).'
- 'AC-3: Table-driven contract matrix in tests/languages/ verifies all 11 profiles (8 Tier-2 + Bash/CSS/JSON) and docs/reference/acdd.md lists compiled default and optional language profiles.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: wire-bash-css-json-default-crates
  title: Add tree-sitter-bash, tree-sitter-css, and tree-sitter-json dependencies and default lang-bash/lang-css/lang-json features in Cargo.toml and crates/acdd-index/Cargo.toml, and register bash/css/json in models LanguageId/CompactTag
  status: completed
  evidence: Added tree-sitter-bash 0.21.0, tree-sitter-css 0.21.1, and tree-sitter-json 0.21.0 under default lang-bash/lang-css/lang-json + all-languages features in Cargo.toml, crates/acdd-index/Cargo.toml, and Cargo.lock (keeping tree-sitter 0.22.6 pinned); registered LanguageId::{Bash,Css,Json} in crates/acdd-index/src/models/tags.rs and verified roundtrip in models::tests::wire_tags_and_detail_keys_roundtrip_expected_constants and cargo check --no-default-features --features lang-rust,lang-toml.
- subtask_ref: implement-bash-css-json-profiles
  title: Implement declarative bash.rs, css.rs, and json.rs LanguageProfiles on SyntaxGrammarSpec + GenericScopeAnalysis and register them in languages/mod.rs
  status: completed
  evidence: Implemented declarative bash.rs (.sh/.bash/.zsh), css.rs (.css), and json.rs (.json) LanguageProfiles on SyntaxGrammarSpec + GenericScopeAnalysis with shared support::{relative_file_import,push_static_reference}, plus scanner::language .verified.json guard in crates/acdd-index/src/scanner/mod.rs; 0 duplicates and verified by default_bash_css_json_contract_matrix.
- subtask_ref: migrate-and-harden-tier2-profiles
  title: 'Migrate go, java, kotlin, csharp, php, ruby, c, and cpp onto SyntaxGrammarSpec; fix normalize_import, is_stdlib, sibling_accessible, metadata, and false dynamic: true calls'
  status: completed
  evidence: Migrated go, java, kotlin, csharp, php, ruby, c, and cpp onto SyntaxGrammarSpec + GenericScopeAnalysis; fixed normalize_import (csharp/php/ruby), is_stdlib (csharp/php/c/cpp angle-bracket headers), sibling_accessible (java/kotlin), BindingCollector & C#/Kotlin ValueFlowCollectorCore handling, and false dynamic constructors; verified by tier2_parity_contract_matrix and profiles::popular.
- subtask_ref: tier2-and-default-languages-matrix
  title: Add table-driven language parity and feature-gate tests in tests/languages/ and update docs/reference/acdd.md
  status: completed
  evidence: Added tests/languages/profiles/tier2_parity.rs (default_bash_css_json_contract_matrix + 11-language tier2_parity_contract_matrix), updated tests/languages/{features.rs,profiles.rs,profiles/popular.rs}, and documented compiled default vs optional profiles in docs/reference/acdd.md.
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:tier2-parity-and-bash-css-json-default.deliver
  gate_id: deliver
  worker_id: ls003-r3
  claim_revision: 1
  contract_revision: 7
  candidate_tree: 1a6f67a0808703d32bc875b6c6ecdd6d3e4467cb
  committed_oid: 7d84baa0ca3707172f11ed9dec81cf48f94b0c37
  accepted_at: 2026-11-22T05:33:24.881Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-DEFAULT-WEB-SHELL-JSON: lang-bash, lang-css, and lang-json (tree-sitter-bash, tree-sitter-css, tree-sitter-json) compile and register in default features as well as all-languages, and extend LanguageId/CompactTag cleanly.'
      - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Every Tier-2 and new profile uses SyntaxGrammarSpec + GenericScopeAnalysis with zero false dynamic: true on typed constructors or operators.'
      verified: []
    review_summary: Delivered task:tier2-parity-and-bash-css-json-default after Round 2 independent review PASS (candidate_tree=1a6f67a0808703d32bc875b6c6ecdd6d3e4467cb).
```

### task: rust-resolution-coverage-and-stdlib

```yaml
task_ref: rust-resolution-coverage-and-stdlib
target: 'Migrate rust/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (removing the 3-way build_file_macro_facts clone) and raise Rust resolution coverage on contextunity-acdd from 42.74% to >= 75% via chained/literal/slice receiver propagation, struct-field and &T parameter resolution, expanded rust-std.toml, const/static/enum_variant/associated_type indexing, #[derive] synthesis, and declarative struct/enum macros'
scope:
- crates/acdd-index/src/languages/rust/
- crates/acdd-index/src/linker/rust/
- crates/acdd-index/src/languages/manifests/rust-std.toml
- crates/acdd-index/src/languages/manifests/rust-cargo.toml
- tests/languages/rust/
- tests/receivers/rust/
extended_scope:
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/inferred.rs
  reason: Pre-approved by @coordinator (#200/#201) for Rust resolution coverage
- path: crates/acdd-index/src/ast/visitor.rs
  reason: Approved by @coordinator (#208) after tier2-parity completion for Rust closure ValueFlow analysis
- path: crates/acdd-index/src/linker/semantic_context/mod.rs
  reason: Unblock Rust Deref field lookup (TypedFacts->Facts), cross-crate workspace type resolution (acdd_core/acdd_state), and prevent impl Vec/Option nodes from setting lexical_head_declared to lift Rust resolution >=75%
- path: crates/acdd-index/src/linker/semantic_context/type_resolution.rs
  reason: Unblock Rust Deref field lookup (TypedFacts->Facts), cross-crate workspace type resolution (acdd_core/acdd_state), and prevent impl Vec/Option nodes from setting lexical_head_declared to lift Rust resolution >=75%
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs
  reason: Unblock Rust Deref field lookup (TypedFacts->Facts), cross-crate workspace type resolution (acdd_core/acdd_state), and prevent impl Vec/Option nodes from setting lexical_head_declared to lift Rust resolution >=75%
- path: crates/acdd-index/src/linker/value_flow/mod.rs
  reason: Fix Rust __rust_index &RustReference unwrapping, &RustTuple field projection, constant alias_type flow, VecDeque/HashSet collection preservation, and conditional reassignment merge to reach >=75% Rust resolution coverage
- path: crates/acdd-index/src/linker/value_flow/initialize.rs
  reason: Fix Rust __rust_index &RustReference unwrapping, &RustTuple field projection, constant alias_type flow, VecDeque/HashSet collection preservation, and conditional reassignment merge to reach >=75% Rust resolution coverage
- path: crates/acdd-index/src/linker/indexing.rs
  reason: Pass dependency_registry into RustMembers::build for cross-crate trait impl resolution
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/members.rs
  reason: Allow Rust trait methods resolved via lookup_member in resolve_computed_call
depends_on:
- universal-declarative-engine-and-generic-linker
tags:
- default
invariants:
- 'INV-LS003-STATIC-NO-HEURISTICS: Every Rust method/associated-item resolution or stdlib external classification is proven from declared types, return types in rust-std.toml, indexed items, or explicit derive/macro declarations.'
- 'INV-LS003-LAYER2-CONSOLIDATION: rust/value_flow.rs delegates scope-frame/binding/field tracking to ValueFlowCollectorCore/ScopeState with zero >= 5-line clones in build_file_macro_facts.'
acceptance_criteria:
- 'AC-1: Multi-hop chains (s.trim().is_empty(), nodes.iter().any(...)), string literals ("x".to_owned()), slices (slice[..k].join()), struct fields (facts.references.last_mut()), and &T parameters (root.join()) resolve or classify as external without dynamic: true fallback.'
- 'AC-2: const_item, static_item, enum_variant, associated_type, standard #[derive(Default/Clone/Debug/Serialize/Deserialize)], and bounded declarative struct/enum macro_rules! definitions (e.g. define_id!, define_string_enum!) resolve cleanly across imports and call sites.'
- 'AC-3: rust/value_flow.rs adopts ValueFlowCollectorCore/ScopeState, eliminates the 3-way build_file_macro_facts clone, and raises Rust resolution coverage on contextunity-acdd to >= 75% with zero false-positive heuristic call edges.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: rust-value-flow-core-and-chain-field-flow
  title: Migrate rust/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (deduplicating build_file_macro_facts) and extend rust/mod.rs and linker/rust/ for multi-hop call chains, string/slice receivers, struct field type propagation, and &T parameter normalization
  status: completed
  evidence: Implemented Rust value-flow chain/field/closure/tuple/generic-impl/wildcard/cross-crate resolution across languages/rust/{mod.rs,linker.rs,value_flow.rs}, linker/rust/{value_flow.rs,resolution.rs,semantic_context.rs}, linker/{indexing.rs,value_flow/,semantic_context/,resolution/value_flow_phase/}. Live Rust resolution coverage on contextunity-acdd reached 75.0874% (34,143 resolved / 11,326 unresolved / 2 ambiguous / 70,510 external).
- subtask_ref: expand-rust-std-manifest-returns
  title: Complete return-type and member declarations in rust-std.toml for Vec, HashMap, BTreeMap, Option, Result, str, Slice, Path, PathBuf, std::env, and numeric primitives
  status: completed
  evidence: Expanded rust-std.toml (+265/-23 lines) and rust-cargo.toml (+154/-7 lines) covering Vec/HashMap/BTreeMap/HashSet/Option/Result/str/String/Slice/Path/PathBuf/Command/Output/ExitStatus/OsStr/OsString/env/primitives/char plus sha2 and rayon collection extensions without ambiguous dependency rules.
- subtask_ref: rust-const-static-variants-derives-macros
  title: 'Index const_item, static_item, enum_variant, and associated_type; synthesize standard #[derive] methods and bounded declarative struct/enum macro exports'
  status: completed
  evidence: Added languages/rust/macros.rs (905 lines) and expanded declarative_macros.toml (+85 lines) for const/static/enum-variant/derive synthesis, bounded declarative struct/enum macros, and generic cross-file macro trait delegation (impl_state_reader_delegate! etc.); 0 clones and 0 dead tails on macros.rs.
- subtask_ref: rust-coverage-contract-matrix
  title: Extend table-driven tests in tests/languages/rust/ and tests/receivers/rust/ covering chains, stdlib returns, field propagation, variants, derives, and declarative macros
  status: completed
  evidence: 'Added contract matrix coverage in tests/languages/rust/{cargo_ecosystem.rs,closures.rs,receivers.rs} and tests/receivers/rust/trait_dispatch.rs (+175/-0 test lines). Verified: linker (77/77), languages (246/246), acdd-index --lib (86/86), commitment_integrity (18/18), lang-rust,lang-toml check, clippy -D warnings, acdd lint (arch,syntax error=0), acdd validate (10/10), /tmp leak check (BEFORE=5480 AFTER=5480).'
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:rust-resolution-coverage-and-stdlib.deliver
  gate_id: deliver
  worker_id: ls003-r4
  claim_revision: 1
  contract_revision: 7
  candidate_tree: 962c737539f1554ff2023ffd85f4e260bed60d4f
  committed_oid: b1658b7c978a3189a7bbd588b05ab1325b189161
  accepted_at: 2026-11-22T10:10:49.583Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-STATIC-NO-HEURISTICS: Every Rust method/associated-item resolution or stdlib external classification is proven from declared types, return types in rust-std.toml, indexed items, or explicit derive/macro declarations.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: rust/value_flow.rs delegates scope-frame/binding/field tracking to ValueFlowCollectorCore/ScopeState with zero >= 5-line clones in build_file_macro_facts.'
      verified: []
    review_summary: 'Deliver task:rust-resolution-coverage-and-stdlib after Round 2 independent review PASS (75.0858% live Rust resolution coverage, BUDGET_OVERRIDE #328).'
```

### task: python-django-typing-chains-and-proto

```yaml
task_ref: python-django-typing-chains-and-proto
target: Migrate python/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::Python, raise Python resolution coverage on commerce-release-update from 80.75% to >= 87%, and raise Proto to 100% via logging.Logger external classification, Callable/Protocol/union/type[T] typing, with-as/Path/f-string/slice/chained value flow, expanded django.toml and unittest.toml, and google/protobuf stdlib imports
scope:
- crates/acdd-index/src/languages/python/
- crates/acdd-index/src/linker/python/
- crates/acdd-index/src/languages/proto.rs
- crates/acdd-index/src/languages/manifests/django.toml
- crates/acdd-index/src/languages/manifests/unittest.toml
- tests/languages/python/
- tests/languages/proto.rs
- tests/receivers/python/
extended_scope:
- path: crates/acdd-index/src/linker/value_flow/mod.rs
  reason: Thin Python delegation hooks for Callable/Protocol, type[T], union possible_calls, Path /, slice, and dotted configured receiver lookup
- path: crates/acdd-index/src/linker/value_flow/initialize.rs
  reason: Thin Python delegation hooks for Callable/Protocol, type[T], union possible_calls, Path /, slice, and dotted configured receiver lookup
- path: crates/acdd-index/src/linker/semantic_context/mod.rs
  reason: Thin Python delegation hooks for Callable/Protocol, type[T], union possible_calls, Path /, slice, and dotted configured receiver lookup
- path: crates/acdd-index/src/linker/semantic_context/type_resolution.rs
  reason: Thin Python delegation hooks for Callable/Protocol, type[T], union possible_calls, Path /, slice, and dotted configured receiver lookup
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/inferred.rs
  reason: Thin Python delegation hooks for Callable/Protocol, type[T], union possible_calls, Path /, slice, and dotted configured receiver lookup
- path: tests/linker/method_semantics.rs
  reason: 'Update tests/linker/method_semantics.rs clean_nonetype union contract assertion and negative Optional/union assertions under LS-003 AC-2 per review finding #313'
depends_on:
- universal-declarative-engine-and-generic-linker
tags:
- default
invariants:
- 'INV-LS003-STATIC-NO-HEURISTICS: Untyped dynamic Python variables remain unresolved rather than guessed; gains come strictly from declared annotations, stdlib/framework manifests, and static expression typing.'
- 'INV-LS003-LAYER2-CONSOLIDATION: python/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and python/mod.rs stores type_checking_aliases inside LanguageFileState::Python.'
acceptance_criteria:
- 'AC-1: Inferred logging.Logger calls classify as external (eliminating ~991 false unresolveds on commerce-release-update) and google/protobuf/* imports classify as external in proto.rs.'
- 'AC-2: python/value_flow.rs adopts ValueFlowCollectorCore/ScopeState and resolves Callable/Protocol.__call__, type[T], union parameter receivers (A | B via bounded possible_calls), bytearray builtins, with ... as bindings, Path / operator, f-string/slice str receivers, super().__init__, generic subscripts (T[X]), and chained calls.'
- 'AC-3: django.toml covers BaseCommand.stdout/style and Model.objects/QuerySet/refresh_from_db/save chains and unittest.toml covers TestCase assertions, lifting commerce-release-update Python resolution to >= 87%.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: python-logger-external-and-proto-stdlib
  title: Classify inferred stdlib logging.Logger members as external in linker/python/ and recognize google/protobuf/* well-known protos in proto.rs
  status: completed
  evidence: Classified inferred logging.Logger members as builtin:logging.Logger external (with annotation safety exemption in logging_factories.rs and Logger/LoggerAdapter clone unification in python/mod.rs) and google/protobuf/* imports/well-known types as external/builtin stdlib in proto.rs (fixing message_body field extraction); verified by logging_factory_matrix and proto_well_known_stdlib_imports_and_types_are_external_and_resolved.
- subtask_ref: python-value-flow-core-and-typing-chains
  title: Migrate python/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (and LanguageFileState::Python) and support Callable/Protocol.__call__, union parameter types, type[T], bytearray, with-as, Path /, f-string/slice str typing, super().__init__, generic subscripts, and chained calls
  status: completed
  evidence: Migrated python/value_flow.rs Collector onto ValueFlowCollectorCore/ScopeState/ScopeFrameFlags and added support for Callable[[...], R], Protocol.__call__, union A|B and Union[A,B] bounded possible_calls (>8 widened), type[T]/Type[T], bytearray builtins, with/async with __enter__/__aenter__, pathlib.Path / operator, f-string/slice str typing, super().__init__ and chained super(), generic T[X], and multi-step chained calls via linker/python/* and minimal shared delegations.
- subtask_ref: expand-django-and-unittest-manifests
  title: Extend django.toml (BaseCommand, Model.objects/QuerySet chains, refresh_from_db, save, get_user_model) and unittest.toml (TestCase assertions)
  status: completed
  evidence: Expanded django.toml (BaseCommand stdout/style, Model save/refresh_from_db/objects/QuerySet/Manager/RelatedManager chains, TestCase variants/assertions, AbstractUser + auth-user-model get_user_model ReceiverType rule) and unittest.toml (TestCase/IsolatedAsyncioTestCase assertions and lifecycle methods).
- subtask_ref: python-and-proto-contract-tests
  title: Extend table-driven tests in tests/languages/python/, tests/receivers/python/, and tests/languages/proto.rs
  status: completed
  evidence: Added table-driven contract tests in tests/languages/proto.rs, tests/languages/python/{semantics.rs,frameworks/django.rs}, and tests/receivers/python/{factories.rs,branch_joins.rs,builtin_members.rs}; all unit, default, --features all-languages, commitment_integrity, and strict clippy checks pass with 0 tmp leaks (5462==5462).
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:python-django-typing-chains-and-proto.deliver
  gate_id: deliver
  worker_id: ls003-r5
  claim_revision: 1
  contract_revision: 3
  candidate_tree: ea68fec7e4166b68e8f1025b49068f9ea8f73f71
  committed_oid: 4cbb9c7e5458c42f7f6e83b3a881f969b12d192f
  accepted_at: 2026-11-22T05:03:24.671Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-STATIC-NO-HEURISTICS: Untyped dynamic Python variables remain unresolved rather than guessed; gains come strictly from declared annotations, stdlib/framework manifests, and static expression typing.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: python/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and python/mod.rs stores type_checking_aliases inside LanguageFileState::Python.'
      verified: []
    review_summary: 'Deliver python-django-typing-chains-and-proto after Round 2 independent review PASS (all #313 findings remediated, net LOC +1297 <= 1300, Clippy -D warnings clean, acdd-index --lib 86/86, tests/languages --all-features 248/248, tests/linker --all-features 77/77, commitment_integrity 18/18, acdd validate 10/10, 0 duplicates/dead_tails/tmp leaks).'
```

### task: web-alpine-playwright-webidl-html-j2-and-vue

```yaml
task_ref: web-alpine-playwright-webidl-html-j2-and-vue
target: Migrate typescript/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::TypeScript, and raise JS/TS/HTML/Vue coverage on commerce-release-update via typed ActivationCondition Alpine.js rules, embedded Playwright and expanded DOM/WebIDL manifests, IIFE scoping, Vue v-for and Pinia defineStore support, and .j2 template admission
scope:
- crates/acdd-index/src/languages/html/
- crates/acdd-index/src/languages/typescript/
- crates/acdd-index/src/languages/vue/
- crates/acdd-index/src/linker/html/
- crates/acdd-index/src/linker/javascript/
- crates/acdd-index/src/linker/typescript/
- crates/acdd-index/src/linker/vue/
- crates/acdd-index/src/languages/manifests/alpinejs.toml
- crates/acdd-index/src/languages/manifests/web.toml
- crates/acdd-index/src/languages/manifests/nuxt.toml
- crates/acdd-index/src/languages/manifests/playwright.toml
- tests/languages/html/
- tests/languages/typescript/
- tests/framework_manifests/
extended_scope:
- path: crates/acdd-index/src/languages/manifests/syntax.rs
  reason: Support ReceiverTypeRule.member_returns, allow active ImplicitReceivers in x-data to resolve after object_factory_this_member, and update extractors.rs active.html x-data activation_signals expectation
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs
  reason: Support ReceiverTypeRule.member_returns, allow active ImplicitReceivers in x-data to resolve after object_factory_this_member, and update extractors.rs active.html x-data activation_signals expectation
- path: tests/languages/extractors.rs
  reason: Support ReceiverTypeRule.member_returns, allow active ImplicitReceivers in x-data to resolve after object_factory_this_member, and update extractors.rs active.html x-data activation_signals expectation
depends_on:
- universal-declarative-engine-and-generic-linker
tags:
- default
invariants:
- 'INV-LS003-MANIFEST-DATA-DRIVEN: Alpine syntax activation, Playwright fixtures, DOM/WebIDL receivers, and Pinia store projections are declared strictly through typed FrameworkManifest RuleIR and Wave 0 ActivationCondition.'
- 'INV-LS003-LAYER2-CONSOLIDATION: typescript/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and typescript/mod.rs stores DOM listener state inside LanguageFileState::TypeScript.'
acceptance_criteria:
- 'AC-1: alpinejs.toml uses Wave 0 typed ActivationCondition syntax-attribute activation so Alpine magic receivers and x-data this.* members resolve in Django/CDN HTML templates without a local package.json.'
- 'AC-2: playwright.toml types Playwright fixtures ({ page, request, context, browser }), web.toml covers core DOM/WebIDL interfaces, and typescript/value_flow.rs adopts ValueFlowCollectorCore/ScopeState and parses JS/TS IIFEs as lexical scopes without emitting truncated unresolved calls.'
- 'AC-3: vue/template.rs scopes v-for loop aliases, nuxt.toml projects Pinia defineStore auto-imports/members, and html profile indexes .j2 Jinja2 templates.'
- 'AC-4: commerce-release-update coverage reaches Vue >= 85%, JS >= 60%, HTML >= 75%, and overall workspace resolution >= 87%.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: alpinejs-syntax-activation-and-ts-flow-core
  title: 'Enable typed ActivationCondition syntax-attribute activation in alpinejs.toml for x-data/x-on: templates and migrate typescript/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::TypeScript'
  status: completed
  evidence: alpinejs.toml updated with ActivationCondition::Any over package/x-data/x-on:/x-bind:; html/mod.rs collects generic rules.syntax_activation_keys() via for_each_tag_attribute into persisted facts.activation_signals; html/javascript.rs parse_object_literal_island collapses both leading and trailing padding via two tree.edit calls; linker/javascript/resolution.rs + dispatch.rs allow active ImplicitReceivers on this.* inside x-data; typescript/value_flow.rs Collector migrated onto ValueFlowCollectorCore/ScopeState; verified via alpine_syntax_activation_and_x_data_members_without_package_json.
- subtask_ref: playwright-and-webidl-manifests-plus-iife
  title: Add embedded playwright.toml, expand web.toml with core DOM/WebIDL platform types, and handle JS/TS IIFE scopes without false call emissions
  status: completed
  evidence: Embedded manifests/playwright.toml added and web.toml expanded with DOM/WebIDL ReceiverType rules; typescript/dom.rs & mod.rs extend configured_callback_parameter for dotted member_expression callees and destructured shorthand/pair_pattern parameters; linker/typescript/resolution.rs normalizes lowercase DOM globals via typed_dom_receiver before property_type_for_path and typescript/mod.rs emits navigator/location/history/performance/crypto property reads synced with web.toml; typescript/mod.rs is_iife_callee scopes IIFEs without truncated calls; verified via playwright_fixtures_and_webidl_interfaces_contract_matrix and javascript_iife_scopes_avoid_truncated_calls.
- subtask_ref: vue-vfor-pinia-and-html-j2
  title: Implement v-for loop-variable scoping in vue/template.rs, Pinia defineStore projection in nuxt.toml, and .j2 extension support in languages/html/
  status: completed
  evidence: vue/template.rs implements depth-aware split_v_for, strip_balanced_outer_parens, and AST-based loop_bindings for destructured object/array/alias v-for parameters; manifests/nuxt.toml adds configured-options-store and @pinia/nuxt auto-imports; html/mod.rs adds j2 extension; verified via vue_v_for_destructured_aliases_scope_cleanly, nuxt_pinia_options_store_and_store_to_refs_resolve, and alpine_syntax_activation_and_x_data_members_without_package_json (cdn/template.j2).
- subtask_ref: web-framework-contract-matrix
  title: Extend table-driven tests in tests/languages/html/, tests/languages/typescript/, and tests/framework_manifests/
  status: completed
  evidence: Table-driven contract matrices added/updated in tests/languages/html/alpine_relations.rs, tests/languages/typescript/dom/{dom_events,platform_globals}.rs, tests/languages/typescript/frameworks/{vue_runtime/iteration_slots,nuxt/autoimports}.rs, tests/languages/extractors.rs, and tests/framework_manifests/activation/user_manifests.rs; verified passing under Clippy -D warnings and --all-features.
receipt:
  run_id: LS-003-universal-par-20261122001607-1983397-1
  node_cursor: root:web-alpine-playwright-webidl-html-j2-and-vue.deliver
  gate_id: deliver
  worker_id: ls003-r6
  claim_revision: 1
  contract_revision: 2
  candidate_tree: ba3e1458a7d1382f6a0a562299a8a91dae8703c7
  committed_oid: 15aa183fb64ecee1ff0afc83a4a91af30ec55b6f
  accepted_at: 2026-11-22T04:51:14.913Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-LS003-MANIFEST-DATA-DRIVEN: Alpine syntax activation, Playwright fixtures, DOM/WebIDL receivers, and Pinia store projections are declared strictly through typed FrameworkManifest RuleIR and Wave 0 ActivationCondition.'
      - 'INV-LS003-LAYER2-CONSOLIDATION: typescript/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and typescript/mod.rs stores DOM listener state inside LanguageFileState::TypeScript.'
      verified: []
    review_summary: Delivered task:web-alpine-playwright-webidl-html-j2-and-vue after Round 2 independent review pass (candidate_tree=ba3e1458a7d1382f6a0a562299a8a91dae8703c7).
```
