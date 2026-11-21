---
id: LS-003-universal-parser-tier1-coverage-and-shell-css-json
title: "Universal declarative language engine (Tier-2 parity + default Bash/CSS/JSON) and Tier-1 resolution coverage (Rust, Python, Web/Vue/HTML)"
doc_type: contract
status: planned
depends_on:
  - PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
  - AR-002-modularize-db-reader-and-traversal
guidance:
  hitl: pro
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
required_skills:
  - acdd
  - contract-planning
  - test-suite-rules
repositories: []
linked_to: []
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
target: "Split 2,930-LOC dependency_registry.rs into modular dependency_registry/ ManifestParser plugins with typed Ecosystem and module-prefix extraction (go.mod, composer.json, .csproj, JVM, Ruby, C/C++), and upgrade FrameworkManifest activation and rules with typed ActivationCondition enums"
contract_revision: 1
depends_on: []
tags:
  - default
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
invariants:
  - 'INV-LS003-MANIFEST-DATA-DRIVEN: Dependency manifests parse through trait-isolated ManifestParser plugins returning typed ParsedManifest/Ecosystem values, and framework manifest activation uses typed ActivationCondition variants with backward-compatible TOML deserialization.'
  - 'INV-LS003-SINGLE-PASS-THROUGHPUT: Manifest parsing and framework rule compilation happen once per workspace/context build with zero per-AST-node manifest I/O or regex compilation.'
acceptance_criteria:
  - 'AC-1: crates/acdd-index/src/languages/dependency_registry.rs is decomposed into dependency_registry/{mod,parser,framework_packs,tests}.rs and dependency_registry/parsers/{cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs implementing ManifestParser and exposing module_prefix_for_path for go.mod, composer.json (PSR-4), and .csproj.'
  - 'AC-2: ActivationCondition in manifests/semantic.rs replaces single-variant Activation with Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, and Any while preserving existing { kind = "package", name = "..." } TOML wire compatibility.'
  - 'AC-3: Typed manifest enums (Ecosystem, ActivationCondition, validated language identifiers) replace ad-hoc string checks in manifests/{framework,semantic,syntax,projections,mod}.rs, ADR 0011 is reconciled, and tests/framework_manifests/ passes with zero clones or dead_tails.'
status: planned
subtasks:
  - subtask_ref: split-dependency-registry-manifest-parsers
    title: "Decompose dependency_registry.rs into dependency_registry/ with ManifestParser trait, Ecosystem enum, and per-ecosystem parsers including go.mod, composer.json, .csproj, pom.xml/build.gradle, Gemfile, and CMakeLists.txt"
  - subtask_ref: typed-activation-condition-and-manifest-enums
    title: "Upgrade manifests/ Activation into typed ActivationCondition (Package, SyntaxAttribute, SyntaxAttributePrefix, FileExtension, Any) and tighten manifest rule enums in manifests/{framework,semantic,syntax,projections,mod}.rs"
  - subtask_ref: manifest-parser-and-activation-contract-tests
    title: "Extend table-driven contract tests in tests/framework_manifests/ covering multi-ecosystem ManifestParser module prefixes and syntax-attribute activation, and reconcile ADR 0011"
```

### task: universal-declarative-engine-and-generic-linker

```yaml
task_ref: universal-declarative-engine-and-generic-linker
target: "Build shared ValueFlowCollectorCore/ScopeState and GenericScopeAnalysis, unify LanguageProfile + SyntaxGrammarSpec + LanguageLinker facade with typed enums and LanguageFileState, upgrade GENERIC_LINKER, and co-evolve crates/acdd-index/AGENTS.md and crates/acdd-index/src/languages/AGENTS.md"
contract_revision: 1
depends_on:
  - modular-manifest-parsers-and-typed-activation
tags:
  - default
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
invariants:
  - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: GenericScopeAnalysis and Tier-1 collectors share ValueFlowCollectorCore/ScopeState (with typed ScopeFrameFlags instead of anonymous boolean tuples) and SyntaxGrammarSpec rules without per-language AST visitor duplication.'
  - 'INV-LS003-STATIC-NO-HEURISTICS: GENERIC_LINKER resolves only lexically or package-proven targets and classifies manifest/stdlib aliases as external without name-only guessing.'
  - 'INV-LS003-LAYER2-CONSOLIDATION: LanguageProfile owns fn linker(&self) -> &dyn LanguageLinker (eliminating hardcoded linker_for language switches), replaces kitchen-sink FileContext fields with typed LanguageFileState, delegates language-specific import/inheritance indexing and builtin-origin classification out of linker/indexing.rs and linker/resolution/mod.rs behind LanguageLinker/manifest hooks, and uses typed models::tags enums at linker call sites.'
acceptance_criteria:
  - 'AC-1: SyntaxGrammarSpec uses typed DeclarationRole, ImportKind, CallForm, and Visibility enums alongside declarative typed-binding, constructor, member-call, and heritage selectors compiled once per grammar.'
  - 'AC-2: crates/acdd-index/src/languages/support/value_flow_core.rs provides shared ScopeState/ScopeFrameFlags/ValueFlowCollectorCore powering GenericScopeAnalysis in ast/visitor.rs and ready for Tier-1 collector adoption.'
  - 'AC-3: LanguageProfile::linker(&self) unifies profile-to-linker dispatch (deleting hardcoded linker_for language switches), FileContext stores per-language state in typed LanguageFileState, linker/indexing.rs and linker/resolution/mod.rs delete hardcoded Rust/JS/Python import, inheritance, and builtin_member_origin switches in favor of data-parallel LanguageLinker/manifest hooks (preserving file/class-level Rayon parallelism and borrowed SymbolId/PathId/CompactTag lookups) while migrating linker call sites onto models::tags enums, and GENERIC_LINKER resolves multi-file same-package declarations, wildcard/static imports, external import aliases, and DependencyRegistry module prefixes (aligned with PF-003/PF-004 Layer 1 FactsLookup seam).'
  - 'AC-4: ADR 0010, crates/acdd-index/AGENTS.md, and crates/acdd-index/src/languages/AGENTS.md document the Layer 2 facade, ValueFlowCollectorCore, ManifestParser, and ActivationCondition canonical ownership.'
status: planned
subtasks:
  - subtask_ref: unified-profile-facade-and-typed-syntax-spec
    title: "Add LanguageProfile::linker(&self) (deleting linker_for switches), replace kitchen-sink FileContext fields with typed LanguageFileState, and extend SyntaxGrammarSpec with DeclarationRole, ImportKind, CallForm, Visibility, and typed-binding/constructor/heritage selectors"
  - subtask_ref: shared-value-flow-core-and-generic-scope-analysis
    title: "Extract shared ScopeState/ScopeFrameFlags/ValueFlowCollectorCore in languages/support/value_flow_core.rs, implement GenericScopeAnalysis, and clean up ast/visitor.rs LanguageAnalysis dispatch"
  - subtask_ref: decouple-linker-indexing-and-upgrade-generic-linker
    title: "Delete hardcoded Rust/JS/Python import/inheritance/builtin_member_origin switches from linker/indexing.rs and linker/resolution/mod.rs in favor of data-parallel LanguageLinker/manifest hooks, migrate linker call sites to models::tags enums, and upgrade GENERIC_LINKER and linker/resolution/imports.rs for multi-file packages, external aliases, wildcards, and module prefixes"
  - subtask_ref: universal-engine-tests-adr-and-agents-md
    title: "Extend table-driven tests in tests/languages/profiles.rs and tests/languages/extractors.rs, reconcile ADR 0010, and co-evolve crates/acdd-index/AGENTS.md and crates/acdd-index/src/languages/AGENTS.md"
```

### task: tier2-parity-and-bash-css-json-default

```yaml
task_ref: tier2-parity-and-bash-css-json-default
target: "Migrate Go, Java, Kotlin, C#, PHP, Ruby, C, and C++ onto the universal declarative engine, fix cross-file import/stdlib/dynamic bugs, and add Bash, CSS, and JSON language profiles enabled in default features and registered in LanguageId/CompactTag"
contract_revision: 1
depends_on:
  - universal-declarative-engine-and-generic-linker
tags:
  - default
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
invariants:
  - 'INV-LS003-DEFAULT-WEB-SHELL-JSON: lang-bash, lang-css, and lang-json (tree-sitter-bash, tree-sitter-css, tree-sitter-json) compile and register in default features as well as all-languages, and extend LanguageId/CompactTag cleanly.'
  - 'INV-LS003-UNIVERSAL-DECLARATIVE-LAYER: Every Tier-2 and new profile uses SyntaxGrammarSpec + GenericScopeAnalysis with zero false dynamic: true on typed constructors or operators.'
acceptance_criteria:
  - 'AC-1: C#, PHP, Ruby, and Go resolve cross-file workspace imports and classify stdlib/framework imports as external; Java and Kotlin resolve same-package sibling types and typed local method receivers; C/C++ classify angle-bracket stdlib headers and out-of-line methods.'
  - 'AC-2: Bash (.sh, .bash, .zsh), CSS (.css), and JSON (.json) profiles extract symbols, references, and cross-file links (source/., @import/var(--*), $defs/$ref) under default Cargo features and register in models/compact_graph.rs (or Layer 1 LanguageId).'
  - 'AC-3: Table-driven contract matrix in tests/languages/ verifies all 11 profiles (8 Tier-2 + Bash/CSS/JSON) and docs/reference/acdd.md lists compiled default and optional language profiles.'
status: planned
subtasks:
  - subtask_ref: wire-bash-css-json-default-crates
    title: "Add tree-sitter-bash, tree-sitter-css, and tree-sitter-json dependencies and default lang-bash/lang-css/lang-json features in Cargo.toml and crates/acdd-index/Cargo.toml, and register bash/css/json in models LanguageId/CompactTag"
  - subtask_ref: implement-bash-css-json-profiles
    title: "Implement declarative bash.rs, css.rs, and json.rs LanguageProfiles on SyntaxGrammarSpec + GenericScopeAnalysis and register them in languages/mod.rs"
  - subtask_ref: migrate-and-harden-tier2-profiles
    title: "Migrate go, java, kotlin, csharp, php, ruby, c, and cpp onto SyntaxGrammarSpec; fix normalize_import, is_stdlib, sibling_accessible, metadata, and false dynamic: true calls"
  - subtask_ref: tier2-and-default-languages-matrix
    title: "Add table-driven language parity and feature-gate tests in tests/languages/ and update docs/reference/acdd.md"
```

### task: rust-resolution-coverage-and-stdlib

```yaml
task_ref: rust-resolution-coverage-and-stdlib
target: "Migrate rust/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (removing the 3-way build_file_macro_facts clone) and raise Rust resolution coverage on contextunity-acdd from 42.74% to >= 75% via chained/literal/slice receiver propagation, struct-field and &T parameter resolution, expanded rust-std.toml, const/static/enum_variant/associated_type indexing, #[derive] synthesis, and declarative struct/enum macros"
contract_revision: 1
depends_on:
  - universal-declarative-engine-and-generic-linker
tags:
  - default
scope:
  - crates/acdd-index/src/languages/rust/
  - crates/acdd-index/src/linker/rust/
  - crates/acdd-index/src/languages/manifests/rust-std.toml
  - crates/acdd-index/src/languages/manifests/rust-cargo.toml
  - tests/languages/rust/
  - tests/receivers/rust/
invariants:
  - 'INV-LS003-STATIC-NO-HEURISTICS: Every Rust method/associated-item resolution or stdlib external classification is proven from declared types, return types in rust-std.toml, indexed items, or explicit derive/macro declarations.'
  - 'INV-LS003-LAYER2-CONSOLIDATION: rust/value_flow.rs delegates scope-frame/binding/field tracking to ValueFlowCollectorCore/ScopeState with zero >= 5-line clones in build_file_macro_facts.'
acceptance_criteria:
  - 'AC-1: Multi-hop chains (s.trim().is_empty(), nodes.iter().any(...)), string literals ("x".to_owned()), slices (slice[..k].join()), struct fields (facts.references.last_mut()), and &T parameters (root.join()) resolve or classify as external without dynamic: true fallback.'
  - 'AC-2: const_item, static_item, enum_variant, associated_type, standard #[derive(Default/Clone/Debug/Serialize/Deserialize)], and bounded declarative struct/enum macro_rules! definitions (e.g. define_id!, define_string_enum!) resolve cleanly across imports and call sites.'
  - 'AC-3: rust/value_flow.rs adopts ValueFlowCollectorCore/ScopeState, eliminates the 3-way build_file_macro_facts clone, and raises Rust resolution coverage on contextunity-acdd to >= 75% with zero false-positive heuristic call edges.'
status: planned
subtasks:
  - subtask_ref: rust-value-flow-core-and-chain-field-flow
    title: "Migrate rust/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (deduplicating build_file_macro_facts) and extend rust/mod.rs and linker/rust/ for multi-hop call chains, string/slice receivers, struct field type propagation, and &T parameter normalization"
  - subtask_ref: expand-rust-std-manifest-returns
    title: "Complete return-type and member declarations in rust-std.toml for Vec, HashMap, BTreeMap, Option, Result, str, Slice, Path, PathBuf, std::env, and numeric primitives"
  - subtask_ref: rust-const-static-variants-derives-macros
    title: "Index const_item, static_item, enum_variant, and associated_type; synthesize standard #[derive] methods and bounded declarative struct/enum macro exports"
  - subtask_ref: rust-coverage-contract-matrix
    title: "Extend table-driven tests in tests/languages/rust/ and tests/receivers/rust/ covering chains, stdlib returns, field propagation, variants, derives, and declarative macros"
```

### task: python-django-typing-chains-and-proto

```yaml
task_ref: python-django-typing-chains-and-proto
target: "Migrate python/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::Python, raise Python resolution coverage on commerce-release-update from 80.75% to >= 87%, and raise Proto to 100% via logging.Logger external classification, Callable/Protocol/union/type[T] typing, with-as/Path/f-string/slice/chained value flow, expanded django.toml and unittest.toml, and google/protobuf stdlib imports"
contract_revision: 1
depends_on:
  - universal-declarative-engine-and-generic-linker
tags:
  - default
scope:
  - crates/acdd-index/src/languages/python/
  - crates/acdd-index/src/linker/python/
  - crates/acdd-index/src/languages/proto.rs
  - crates/acdd-index/src/languages/manifests/django.toml
  - crates/acdd-index/src/languages/manifests/unittest.toml
  - tests/languages/python/
  - tests/languages/proto.rs
  - tests/receivers/python/
invariants:
  - 'INV-LS003-STATIC-NO-HEURISTICS: Untyped dynamic Python variables remain unresolved rather than guessed; gains come strictly from declared annotations, stdlib/framework manifests, and static expression typing.'
  - 'INV-LS003-LAYER2-CONSOLIDATION: python/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and python/mod.rs stores type_checking_aliases inside LanguageFileState::Python.'
acceptance_criteria:
  - 'AC-1: Inferred logging.Logger calls classify as external (eliminating ~991 false unresolveds on commerce-release-update) and google/protobuf/* imports classify as external in proto.rs.'
  - 'AC-2: python/value_flow.rs adopts ValueFlowCollectorCore/ScopeState and resolves Callable/Protocol.__call__, type[T], union parameter receivers (A | B via bounded possible_calls), bytearray builtins, with ... as bindings, Path / operator, f-string/slice str receivers, super().__init__, generic subscripts (T[X]), and chained calls.'
  - 'AC-3: django.toml covers BaseCommand.stdout/style and Model.objects/QuerySet/refresh_from_db/save chains and unittest.toml covers TestCase assertions, lifting commerce-release-update Python resolution to >= 87%.'
status: planned
subtasks:
  - subtask_ref: python-logger-external-and-proto-stdlib
    title: "Classify inferred stdlib logging.Logger members as external in linker/python/ and recognize google/protobuf/* well-known protos in proto.rs"
  - subtask_ref: python-value-flow-core-and-typing-chains
    title: "Migrate python/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState (and LanguageFileState::Python) and support Callable/Protocol.__call__, union parameter types, type[T], bytearray, with-as, Path /, f-string/slice str typing, super().__init__, generic subscripts, and chained calls"
  - subtask_ref: expand-django-and-unittest-manifests
    title: "Extend django.toml (BaseCommand, Model.objects/QuerySet chains, refresh_from_db, save, get_user_model) and unittest.toml (TestCase assertions)"
  - subtask_ref: python-and-proto-contract-tests
    title: "Extend table-driven tests in tests/languages/python/, tests/receivers/python/, and tests/languages/proto.rs"
```

### task: web-alpine-playwright-webidl-html-j2-and-vue

```yaml
task_ref: web-alpine-playwright-webidl-html-j2-and-vue
target: "Migrate typescript/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::TypeScript, and raise JS/TS/HTML/Vue coverage on commerce-release-update via typed ActivationCondition Alpine.js rules, embedded Playwright and expanded DOM/WebIDL manifests, IIFE scoping, Vue v-for and Pinia defineStore support, and .j2 template admission"
contract_revision: 1
depends_on:
  - universal-declarative-engine-and-generic-linker
tags:
  - default
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
invariants:
  - 'INV-LS003-MANIFEST-DATA-DRIVEN: Alpine syntax activation, Playwright fixtures, DOM/WebIDL receivers, and Pinia store projections are declared strictly through typed FrameworkManifest RuleIR and Wave 0 ActivationCondition.'
  - 'INV-LS003-LAYER2-CONSOLIDATION: typescript/value_flow.rs delegates scope/binding/conditional-stack state to ValueFlowCollectorCore/ScopeState and typescript/mod.rs stores DOM listener state inside LanguageFileState::TypeScript.'
acceptance_criteria:
  - 'AC-1: alpinejs.toml uses Wave 0 typed ActivationCondition syntax-attribute activation so Alpine magic receivers and x-data this.* members resolve in Django/CDN HTML templates without a local package.json.'
  - 'AC-2: playwright.toml types Playwright fixtures ({ page, request, context, browser }), web.toml covers core DOM/WebIDL interfaces, and typescript/value_flow.rs adopts ValueFlowCollectorCore/ScopeState and parses JS/TS IIFEs as lexical scopes without emitting truncated unresolved calls.'
  - 'AC-3: vue/template.rs scopes v-for loop aliases, nuxt.toml projects Pinia defineStore auto-imports/members, and html profile indexes .j2 Jinja2 templates.'
  - 'AC-4: commerce-release-update coverage reaches Vue >= 85%, JS >= 60%, HTML >= 75%, and overall workspace resolution >= 87%.'
status: planned
subtasks:
  - subtask_ref: alpinejs-syntax-activation-and-ts-flow-core
    title: "Enable typed ActivationCondition syntax-attribute activation in alpinejs.toml for x-data/x-on: templates and migrate typescript/value_flow.rs onto shared ValueFlowCollectorCore/ScopeState and LanguageFileState::TypeScript"
  - subtask_ref: playwright-and-webidl-manifests-plus-iife
    title: "Add embedded playwright.toml, expand web.toml with core DOM/WebIDL platform types, and handle JS/TS IIFE scopes without false call emissions"
  - subtask_ref: vue-vfor-pinia-and-html-j2
    title: "Implement v-for loop-variable scoping in vue/template.rs, Pinia defineStore projection in nuxt.toml, and .j2 extension support in languages/html/"
  - subtask_ref: web-framework-contract-matrix
    title: "Extend table-driven tests in tests/languages/html/, tests/languages/typescript/, and tests/framework_manifests/"
```
