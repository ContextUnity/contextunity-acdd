---
title: "Language Engine Modularity and Extensibility Contracts"
doc_type: architecture
---

# Language Engine Modularity and Extensibility Contracts

## Architecture Overview

ContextUnity ACDD separates code parsing, AST fact extraction, cross-file symbol linking, and storage graph generation into modular, decoupled layers.

```text
crates/acdd-index/src/
├── ast/                    # Tree-sitter extraction and relation facts
├── languages/
│   ├── mod.rs              # Layer 2 facade: LanguageProfile, LanguageFileState, SyntaxGrammarSpec
│   ├── support/            # Shared syntax extraction & value-flow helpers
│   │   ├── mod.rs
│   │   ├── c_family.rs
│   │   └── value_flow_core.rs # ScopeFrameFlags, ScopeState, ValueFlowCollectorCore, GenericScopeAnalysis
│   ├── template.rs         # Shared byte-preserving template masker
│   ├── html/               # HTML extraction and configured request attributes
│   │   ├── mod.rs
│   │   ├── javascript.rs   # Embedded JavaScript islands
│   │   └── request_attributes.rs # Manifest-driven HTML request operation
│   ├── vue/                # Vue grammar and template extraction
│   │   ├── mod.rs
│   │   ├── setup_facts.rs
│   │   └── template.rs
│   ├── python/             # Python profile extraction
│   │   ├── mod.rs
│   │   ├── dependencies.rs # Python package dependency extraction
│   │   ├── logging_factories.rs # Finite logging.getLogger factory eligibility
│   │   └── value_flow.rs
│   ├── manifests/          # Typed framework rule data & ActivationCondition
│   ├── dependency_registry/ # Modular ManifestParser plugins, Ecosystem, & framework rules
│   │   ├── mod.rs
│   │   ├── parser.rs       # ManifestParser trait, MANIFEST_PARSERS, ModulePrefix
│   │   ├── framework_packs.rs
│   │   └── parsers/        # Per-ecosystem parsers (cargo, npm, python, go, jvm, php, csharp, ruby, c_cpp)
│   ├── registries/         # JavaScript package and TypeScript path registries
│   ├── rust/               # Rust profile extraction, value_flow.rs, macros.rs
│   ├── typescript/         # TypeScript/JavaScript profile extraction
│   ├── bash.rs, css.rs, json.rs # Default shell/style/JSON profiles on SyntaxGrammarSpec
│   ├── go.rs, java.rs, kotlin.rs, csharp.rs, php.rs, ruby.rs, c.rs, cpp.rs # Tier-2 declarative profiles
│   └── toml_manifest.rs    # TOML traversal for package dependency metadata
└── linker/                 # Shared graph construction and dispatch (`mod.rs`)
    ├── traits.rs
    ├── framework_relations.rs
    ├── resolution/
    │   ├── mod.rs          # Per-file resolver and phase order
    │   ├── imports.rs
    │   ├── framework_projections.rs
    │   ├── value_flow_phase/
    │   └── documentation.rs
    ├── semantic_context/
    │   └── components.rs
    ├── value_flow/
    ├── value_flow/initialize.rs
    ├── python/             # Python receiver, resolution, semantic context, and flow
    ├── rust/               # Rust resolution support, semantic context, and flow
    ├── javascript/         # JavaScript-family CommonJS bindings and resolution
    ├── typescript/         # TypeScript resolution and semantic context
    ├── vue/                # Vue resolution, semantic context, and value flow
    └── html/               # HTML relation, script, and template linking
```

## Extensibility Contracts

### 1. `LanguageProfile`
Encapsulates language-specific discovery and AST extraction:
- File extensions and filename patterns.
- Comment syntax and token boundary rules.
- Grammar definitions and Tree-sitter query conventions.
- Extraction of declarations (functions, methods, classes, types, interfaces).

### 2. `TemplateMasker`
Preprocesses mixed template syntax before the HTML or embedded JavaScript parser runs:
- Replaces `{% ... %}`, `{{ ... }}`, and `{# ... #}` bytes with spaces while retaining newline bytes and exact offsets.
- Reports original tag content and line to HTML extraction so `include` and `extends` remain structural references.
- Lets Vue extract interpolation references from original source spans between DOM nodes, including spans that the masked HTML parser omits as whitespace.
- Returns the borrowed source without a byte scan when no template marker is present.

### 3. `LanguageLinker`
Decouples language-specific import and receiver resolutions from the core SQLite graph builder:
- Import resolution (e.g. Python `__init__.py` re-export chains, TypeScript `tsconfig.json` path mapping, Rust `use crate::` paths).
- Receiver inference (method binding on class instances, struct instances, or prototypes).
- Standard library and external package classification, ensuring third-party symbols are properly marked `external_origin` rather than leaking into `unresolved`.
- Framework semantics come from typed `FrameworkManifest` data consumed by finite operations. Bundled tables live under `crates/acdd-index/src/languages/manifests/` and are embedded in the binary. Workspace manifests load from `.acdd/frameworks/` and are available in their workspace. They merge project rules before bundled rules by default or with `extend: true`; `replace: true` selects the project table alone. Unknown top-level fields are rejected, so manifests cannot add opaque legacy string sections. Typed AST rules compile into grammar-specific `RuleIR` candidates; class receivers retain their borrowed linker rule lookup, and `semantic_rules` facts join through the shared linker. See [Framework manifests](../reference/framework-manifests.md) for authoring and language-extension guidance.
- `DependencyRegistry` (`crates/acdd-index/src/languages/dependency_registry/`) owns workspace dependency discovery via `ManifestParser` (`MANIFEST_PARSERS`), `ModulePrefix` extraction, and framework-manifest loading. JavaScript package metadata and TypeScript path mappings live in `languages/registries/`; Python dependency declarations are extracted in `python/dependencies.rs`. `LanguageProfile::manifest_filenames` continues to describe external package metadata inputs. `LanguageProfile::linker(&self)` binds each profile to its `LanguageLinker` (defaulting to `GENERIC_LINKER`), and `FileContext.state` stores typed `LanguageFileState`.

### 4. Three-Tier Language Architecture
Languages and extensions decompose into three explicit tiers:
- **Tier 1 (Declarative `SyntaxGrammarSpec`)**: Typed mappings (`DeclarationRole`, `ImportKind`, `CallForm`, `Visibility`) alongside `typed_bindings`, `constructors`, `member_calls`, `heritage`, and `conditionals` selectors compiled once into `CompiledSyntaxGrammar` supply structural language support. Explicit unknown selectors fail through the parser error result; they never dispatch as kind zero.
- **Tier 2 (Single-Pass AST Extractor & `ValueFlowCollectorCore`)**: `SinglePassExtractor` creates declaration identities on entry, accumulates body bindings and value flow during a shared DFS via `ValueFlowCollectorCore` / `GenericScopeAnalysis` (`ScopeState` / `ScopeFrameFlags`), and finalizes each owner on exit through a `ScopeFrame` stack. Nested owner frames retain their own summaries.
- **Tier 3 (Polyglot Semantic Linker)**: Deep language-specific member index construction, C3 inheritance resolution, and cross-layer framework relation composition.

The shared body traversal replaces recursive declaration-body binding, value-flow and final-return collectors. Occurrence-local conservative import and template-loader proofs may revisit lexical syntax. TypeScript's returned-object proof may inspect its enclosing function to establish same-object receiver metadata. These queries do not produce body summaries or replace scope-exit analysis. TypeScript module-alias preparation and DOM processing remain separate. This boundary does not claim that every parser operation visits every node only once. Tree-sitter query experiments remain opt-in research outside default indexing.

## Framework data boundary

> [!IMPORTANT]
> Invariant: Framework semantics belong to validated manifest data. Production code has no framework-specific branches keyed by framework IDs, package names, namespaces, API spellings, constructors, decorators, keywords, attributes, or directory names. The registry discovers pack identity from manifest files; generic operations apply the selectors and activation configured in typed rules.
> - INV-FRAMEWORK-DATA-BOUNDARY: Keep framework API identities, receiver families, keyword and attribute names, directory conventions, and activation rules in typed `FrameworkManifest` tables rather than hardcoded Rust parser/linker branches. [`named_kind`](../../crates/acdd-index/src/languages/manifests/mod.rs)

## Single-Owner and Modularity Invariants

> [!IMPORTANT]
> Invariant: Shared infrastructure, validation, Git execution, surface compatibility, and graph hygiene have canonical single owners across the workspace.
> - INV-SINGLE-OWNER: Shared workspace configuration (`acdd_core::config`), error taxonomy (`acdd_core::errors`), path confinement and brace-aware glob matching (`acdd_core::{fs, path_policy}`), advisory locking (`acdd_core::lock`), system/telemetry primitives (`acdd_core::{sys, format, telemetry}`), and `validate`/`WhenExpr` (`acdd_state`) have one canonical owner with zero downstream duplicates. [`glob_matches`](../../crates/acdd-core/src/path_policy.rs) [`load_adapter`](../../crates/acdd-core/src/config/loader.rs)
> - INV-GIT-SINGLE-OWNER: Raw `std::process::Command::new("git")` invocations are strictly confined to `crates/acdd-core/src/git.rs` (`acdd_core::git`); all downstream Git workflows in `acdd-state` and host `src/` route through `acdd_core::git`. [`git_command`](../../crates/acdd-core/src/git.rs)
> - INV-SURFACE-COMPATIBILITY: Preserve active CLI/MCP wire and DTO contracts and `ErrorCode` semantics without mirror `Mcp*` clone structs or duplicate `ClaimResp` Markdown renderers, while forbidding dead or test-only `pub` / `pub use` compatibility shims across internal workspace crates. [`render_claim_prompt_markdown`](../../crates/acdd-state/src/context/markdown.rs)
> - INV-NO-DEAD-CODE-SHIMS: Ban `#![allow(dead_code, unused_imports)]`, `#[allow(dead_code)]`, `#[expect(dead_code)]`, legacy re-export shims (`src/core/*`, `src/db/*`), `>= 5`-line clones, `phantom_tested` dead code, `dead_tails`, `dangling_calls`, and opt-in `dead_tails.unused_public_api` violations. [`collect_dead_tails`](../../crates/acdd-index/src/db/lint/dead_tails.rs)
> - INV-BOUNDED-STRUCTURAL-COMPLEXITY: Production callables stay within ValueFlow extraction budgets (`syntax.fact_budget_exceeded`) and maintain unique intra-crate nominal type names (`syntax.intra_crate_type_homonym`) with AST `cognitive_complexity` and `max_nesting_depth` recorded as `node.details` telemetry without fragmenting flat lookup tables or cohesive functions into single-use micro-helpers. [`collect_syntax_violations`](../../crates/acdd-index/src/db/lint/syntax.rs)
> - INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant has one canonical `INV-*` declaration in `docs/architecture/*.md` or `docs/adr/*.md` with verified symbol links (and `acdd-rule` blocks for machine-enforceable boundaries), while `**/AGENTS.md` files stay concise operational routers. [`enforced_invariant_ids`](../../crates/acdd-index/src/db/lint/rules.rs)

```acdd-rule
id: INV-SINGLE-OWNER-NO-INLINE-TEMP-DIR
invariant: INV-SINGLE-OWNER
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/acdd-index/src/**", "crates/acdd-state/src/**"]
  exclude_paths:
    - "crates/acdd-state/src/harness.rs"
  languages: ["rust"]
ast_pattern: "std::env::temp_dir()"
include_tests: true
message: "Do not use raw std::env::temp_dir() inside src/ or inline crate tests; place FS tests in canonical tests/<domain>.rs via RunHarness/Workspace/ScopedTempDir"
```

```acdd-rule
id: INV-GIT-SINGLE-OWNER
invariant: INV-GIT-SINGLE-OWNER
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  exclude_paths: ["crates/acdd-core/src/git.rs"]
  languages: ["rust"]
ast_pattern: 'Command::new("git")'
include_tests: false
```

```acdd-rule
id: INV-GIT-SINGLE-OWNER-QUALIFIED
invariant: INV-GIT-SINGLE-OWNER
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  exclude_paths: ["crates/acdd-core/src/git.rs"]
  languages: ["rust"]
ast_pattern: 'std::process::Command::new("git")'
include_tests: false
```

```acdd-rule
id: INV-NO-DEAD-CODE-SHIMS
invariant: INV-NO-DEAD-CODE-SHIMS
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  languages: ["rust"]
ast_pattern: "#[allow(dead_code)]"
include_tests: false
```

```acdd-rule
id: INV-NO-DEAD-CODE-SHIMS-INNER
invariant: INV-NO-DEAD-CODE-SHIMS
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  languages: ["rust"]
ast_pattern: "#![allow(dead_code)]"
include_tests: false
```

```acdd-rule
id: INV-NO-DEAD-CODE-SHIMS-EXPECT
invariant: INV-NO-DEAD-CODE-SHIMS
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  languages: ["rust"]
ast_pattern: "#[expect(dead_code)]"
include_tests: false
```

```acdd-rule
id: INV-NO-DEAD-CODE-SHIMS-EXPECT-INNER
invariant: INV-NO-DEAD-CODE-SHIMS
severity: error
kind: ast_pattern
from:
  paths: ["src/**", "crates/**"]
  languages: ["rust"]
ast_pattern: "#![expect(dead_code)]"
include_tests: false
```

```acdd-rule
id: ARCH-LANGUAGE-PROFILE-MUST-IMPLEMENT
severity: error
kind: must_implement
from:
  paths: ["crates/acdd-index/src/languages/*.rs"]
  exclude_paths:
    - "crates/acdd-index/src/languages/mod.rs"
    - "crates/acdd-index/src/languages/build_manifest.rs"
    - "crates/acdd-index/src/languages/context.rs"
    - "crates/acdd-index/src/languages/profile.rs"
    - "crates/acdd-index/src/languages/routes.rs"
    - "crates/acdd-index/src/languages/semantic.rs"
    - "crates/acdd-index/src/languages/template.rs"
    - "crates/acdd-index/src/languages/toml_manifest.rs"
    - "crates/acdd-index/src/languages/types.rs"
to:
  paths: ["crates/acdd-index/src/languages/profile.rs"]
  symbols: ["LanguageProfile"]
```

Keep framework API identities, receiver families, keyword and attribute names,
directory conventions, activation conditions, and graph-relation mappings in
manifests. The generated bundle inventory binds filename-derived pack identities
to included data files; it does not implement pack behavior. Keep Vue syntax in its language profile and
place application-framework conventions in manifest rules.

Validate operation shapes, identifiers, target types, normal paths, and supported
language combinations. Read the particular attribute, path prefix, and callable
from the rule. Implement a missing generic operation in its admitted prerequisite
task with its own extraction, linking, persistence, and proof boundary. A pack
task changes data, domain tests, and affected documentation; extending its file
scope does not admit an engine extension.

Prove manifest causality through the real scanner, extractor, linker, and
persisted graph. Removing or replacing a rule removes its framework facts and
relations while ordinary language calls and imports remain. Renaming configured
API, attribute, keyword, and directory spellings together with the fixture
preserves equivalent results without production changes. Prove missing,
ambiguous, shadowed, and dynamic cases without guessed exact edges. Verify the
same facts and commitments after cold and incremental indexing, including rule
and activation changes. A source-spelling scan supplements these proofs.

Ordinary grammar read candidates remain available to the common resolver.
Framework member admission additionally proves association with an effective
configured factory result; it does not suppress unresolved ordinary reads.
Write-only assignment targets are not read candidates. Language-owned import,
enum, class and same-object resolution is preserved in embedded Vue grammar.

## Language resolution ownership

`semantic_context::LanguageMemberLookup` gives the shared resolver borrowed, read-only access to prebuilt Python, Rust, and TypeScript member indexes. Each language index retains its own lookup rules; the shared resolution input does not carry one field per language, and reference linking does not rebuild indexes or perform per-reference database reads.

Rust namespace indexing also prepares one bounded forwarding index from existing `use` references and public export facts. Import resolution, package exports, and Rust member lookup share that index. Private parent bindings remain descendant-only, while public named bindings follow the existing export contract; conflicting candidates, cycles, and exhausted alias walks do not produce guessed exact targets. Root crate imports use the root manifest's normalized package or explicit library name only when the conventional `src/lib.rs` is indexed in that same manifest scope.

`framework_relations::compose` joins configured route, HTTP request-attribute, and HTML template facts after ordinary reference resolution. It consumes indexed facts, owner sets, flow state, the dependency registry, language profiles, semantic context, and route nodes, then returns graph edges and coverage. The linker merges those results and applies its existing deterministic graph ordering.

## Bounded semantic resolution ownership

Python value flow currently joins direct identifier assignments from every explicit `if`/`elif`/`else` path. A missing `else` includes the incoming value; an unproven incoming value remains unknown. The shared reducer deduplicates by stable declaration identity, orders candidates deterministically, and widens the whole result when more than eight distinct local declarations are found. Other language-specific join syntax remains the responsibility of its language profile.

Exact receiver resolution and possible-target evidence remain separate outcomes. Candidate members use `possible_calls` with possible confidence and never become exact `calls` edges. Unknown alternatives remain incomplete; overflow past K=8 emits no truncated candidate prefix. These edges are persisted for navigation and included in inbound impact and removal assessments, while exact call counts continue to use only `calls`. The type-expression parser depth guard (eight), alias depth (default 64), reducer iteration limits, and candidate cardinality are independent bounds. These rules add no runtime evaluation and do not activate disabled language profiles.

## Documentation Invariants

- **Rust Code**: Standard Rustdoc (`///` with Markdown, `# Arguments`, `# Returns`, `# Errors`, `# Panics`). Every public trait, struct, enum variant, and method must have self-contained documentation explaining ownership, invariants, and performance complexity.
- **Python Code**: Google-style docstrings (PEP 257) with strict section typing (`Args:`, `Returns:`, `Raises:`).
