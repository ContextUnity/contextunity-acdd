---
title: "ADR 0011: Language Linkers and Framework Manifests"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0011: Language Linkers and Framework Manifests

## Status

Accepted.

## Context

Supporting multi-language imports, third-party package classification, template languages, and web/backend frameworks without hardcoding framework API spellings inside Rust linkers requires clear language-profile seams and typed, data-driven framework manifest ingestion.

## Decision

### 1. `LanguageProfile`, `TemplateMasker`, and `LanguageLinker` Seams

- **`LanguageProfile`**: Owns grammar bindings, file extensions, comment boundaries, and AST extraction per language (`crates/acdd-index/src/languages/`).
- **`TemplateMasker`**: Masks `{% ... %}`, `{{ ... }}`, and `{# ... #}` template directives byte-for-byte with spaces (preserving newlines and offsets) before HTML/JS parsing while retaining template relations (`include`, `extends`, Vue interpolations).
- **`LanguageLinker`**: Performs language-owned import, scope, and receiver resolution (`crates/acdd-index/src/linker/`), keeping framework behavior strictly data-driven rather than adding framework-specific code branches.

### 2. Dependency Manifest Discovery & 4-Tier Reference Classification

- `DependencyRegistry` parses root and nearest ecosystem manifests across supported toolchains: Rust (`Cargo.toml`), Python (`pyproject.toml`, `requirements.txt`, `Pipfile`, `setup.cfg`), JS/TS (`package.json`), Go (`go.mod`), and Java (`build.gradle`, `pom.xml`).
- Every symbol reference resolves into one of four discrete statuses:
  - `resolved`: Target declaration exists in indexed workspace code.
  - `external`: Target belongs to a manifest-declared package or standard library (`external_origin`), preventing false unresolved noise.
  - `ambiguous`: Multiple workspace candidates match without narrowing evidence.
  - `unresolved`: Belongs to workspace code but lacks any matching declaration.

### 3. Serde-Driven `FrameworkManifest` Packs (`.acdd/frameworks/`)

- Framework receiver families, route/DI/semantic/syntax rules, activation conditions, and graph relations are expressed as typed `FrameworkManifest` data compiled into finite `RuleIR` operations.
- Standard packs embed via `include_str!`; user workspace overrides load from `.acdd/frameworks/<id>.toml`, `.yaml`, or `.yml` (parsed with strict `deny_unknown_fields`, rejecting legacy raw string lists like top-level `receivers`, `builtins`, `filters`, or `routes`).
- Default or `extend: true` merges project rules before bundled rules; `replace: true` uses only project rules (setting both `true` fails closed). Any invalid manifest fails the index build before publication.
- Rules with package activation require a matching dependency in `DependencyRegistry`. Manifest migrations preserve `(src, dst, kind)` edge triples and resolution status multisets on the reference corpus.

### 4. Modular `ManifestParser` Plugins and Two-Tier `ActivationCondition`

- `crates/acdd-index/src/languages/dependency_registry/` replaces the monolithic registry with trait-isolated `ManifestParser` plugins (`parsers/{cargo,npm,python,go,jvm,php,csharp,ruby,c_cpp}.rs`) keyed by a closed `Ecosystem` enum (`Rust`, `JavaScript`, `Python`, `Go`, `Jvm`, `Php`, `CSharp`, `Ruby`, `CCpp`) returning typed `ParsedManifest` and workspace-relative `ModulePrefix` records (`go.mod`, `composer.json` PSR-4, `.csproj` `RootNamespace`).
- `ActivationCondition` in `manifests/semantic.rs` replaces single-variant `Activation` with `Package`, `SyntaxAttribute`, `SyntaxAttributePrefix`, `FileExtension`, and `Any` (bounded nesting depth `<= 4`), preserving `{ kind = "package", name = "..." }` wire compatibility.
- Workspace-level package checks and file-level extension/syntax checks split into two tiers via `ActivationMatch` (`Active`, `Inactive`, `Deferred`): `LanguageContext` precomputes `SyntaxActivationKeys` once per scope and evaluates `is_syntax_rule_active(rule, path, signals)` against `FileActivationSignals` in $O(1)$ without per-AST-node I/O or regex compilation.

## Consequences

- Adding or overriding framework conventions (Django, HTMX, Nuxt, GridViewSpec, custom internal packs) requires zero Rust code changes in extractors or linkers.
- Invalid workspace manifests fail closed prior to SQLite publication.
- Third-party dependencies never pollute project-internal `unresolved` or `code_map_prove_removal` diagnostics.
