---
title: "ADR 0010: Static Value Flow and Receiver Resolution"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0010: Static Value Flow and Receiver Resolution

## Status

Accepted.

## Context

Method-call linking (for example resolving `user.save()` to `User.save`) requires receiver type inference, yet dynamic code execution introduces severe security and latency hazards while unbounded static analysis risks cyclic alias explosions. Furthermore, unconditional dropping of all conditional branches loses valid finite navigation evidence (`if/else` or `match` arms), whereas guessing a single target causes false-positive call edges.

## Decision

### 1. Purely Static Bounded Data-Flow Analysis

- Receiver inference operates solely over lexical AST facts (assignment positions, type annotations, constructors, and straight-line factory summaries) with zero runtime code evaluation or sandboxing.
- Separate independent bounds prevent runaway recursion: `MAX_VALUE_FLOW_TYPE_DEPTH = 8` for type-expression parsing, `SemanticLimits::alias_depth` (default `64`), and bounded reducer iteration limits. Alias cycles and unproven external side effects terminate fail-closed.

### 2. Bounded Local Branch Joins (`MAX_CANDIDATE_BRANCH_JOINS = 8`)

In [`crates/acdd-index/src/linker/value_flow/mod.rs`](../../crates/acdd-index/src/linker/value_flow/mod.rs):

- **Exact (`calls`) vs. Possible (`possible_calls`)**:
  - When every enumerated local exit path proves the same single declaration identity, emit an exact `calls` edge (`resolved`).
  - When a complete local branch join enumerates `2..=8` (`2..=MAX_CANDIDATE_BRANCH_JOINS`) distinct known targets (including implicit fall-through/incoming values when syntax permits), retain them deterministically as `possible_calls` navigation evidence with `possible` confidence, stored in the compressed value-flow store rather than `nodes.details`.
  - Any unknown/opaque alternative stays explicit (never dropped to manufacture an exact or exhaustive set), and joins exceeding `8` distinct targets widen to `Unknown/Incomplete` with exhaustion reporting instead of truncating a prefix.
  - Later unconditional proven writes replace earlier joins in source order; unknown writes invalidate prior exact proofs.

### 3. Branch-Join Contract Matrix (`v1`)

| Source Domain | Exact (`calls`) | Possible (`possible_calls`, `2..=8`) | Unknown / Incomplete | Forbidden False Positive |
| --- | --- | --- | --- | --- |
| **Python** | Annotations, constructors, aliases proving 1 target across all paths | Complete `if`/`elif`/`else` joins (omitted `else` includes incoming value) | Unknown factories, alias cycles, >8 targets, external reassignments | Selecting first/last method or dropping unassigned path |
| **Rust** | Nominal annotations & constructors proving 1 target | Complete `if`/`else` or finite `match` arms with nominal targets | Macros, `cfg`-dependent paths, trait objects, >8 targets | Picking one trait impl when multiple dispatch paths exist |
| **TypeScript / JS** | Annotations, constructors, aliases proving 1 target | Complete conditional joins with known local declarations | Dynamic writes, `any`, external values, >8 targets | Resolving by member spelling alone |
| **Mixed Frameworks** | Explicit composition relations under owning rules | Cross-language join only with explicit composition evidence | Runtime transforms, generated code, dynamic template names | Inferring cross-language calls from shared names alone |

### 4. Declarative Extraction, Activation Gating, and Canonical Ownership

- **Layer 2 Facade & Declarative Syntax Selectors**: `crates/acdd-index/src/languages/mod.rs` serves as the Layer 2 language facade. Profiles opt into declarative extraction via `SyntaxGrammarSpec` with typed `DeclarationRole`, `ImportKind`, `CallForm`, and `Visibility` mappings alongside `typed_bindings`, `constructors`, `member_calls`, `heritage`, and `conditionals` selectors compiled once per grammar (`CompiledSyntaxGrammar`).
- **Shared Value-Flow Collector Core**: `crates/acdd-index/src/languages/support/value_flow_core.rs` owns `ScopeFrameFlags`, `ScopeState`, `ValueFlowCollectorCore`, and `GenericScopeAnalysis`, driving `ast/visitor.rs` single-pass cursor traversal without per-node regexes, allocations, or duplicate AST walks.
- **Canonical Manifest & Activation Ownership**: `languages::dependency_registry::parser::{ManifestParser, MANIFEST_PARSERS, ModulePrefix}` owns ecosystem manifest discovery and module-prefix parsing; `languages::manifests::semantic::{ActivationCondition, ActivationMatch, SyntaxActivationKeys, FileActivationSignals}` owns two-tier workspace vs. per-file syntax activation. Observed `FileActivationSignals` persist on `Facts.activation_signals` (`ACDD-LOCAL-FACTS\0\x04` in `typed_facts/binary.rs`) and gate `LanguageContext::is_syntax_rule_active` / `LanguageContext::syntax_rules_for_signals` and `ImplicitReceivers` evaluation at link time.
- **Data-Driven Linker Dispatch**: `LanguageProfile::linker()` exposes per-profile `LanguageLinker` implementations (eliminating hardcoded language switches from `linker/indexing.rs` and `linker/resolution/mod.rs`), while `FileContext` stores per-language extractor state in typed `LanguageFileState` and `GENERIC_LINKER` handles multi-file same-package declarations, wildcard/static imports, external import aliases, and `ModulePrefix` normalization.

## Consequences

- Exact call counts (`calls`) remain strictly separated from bounded candidate navigation (`possible_calls`), while impact and removal analyses conservatively follow both.
- Static analysis terminates deterministically without executing repository code.

## Executable Boundary Rules

```acdd-rule
- id: ARCH-L2-SUPPORT-BOUNDARY-ENTRY
  severity: error
  kind: boundary_entry
  to:
    paths: ["crates/acdd-index/src/languages/support/**"]
  entrypoints:
    - "crates/acdd-index/src/languages/support/mod.rs"
    - "crates/acdd-index/src/languages/support/value_flow_core.rs"
    - "crates/acdd-index/src/languages/support/c_family.rs"
  message: "Callers must enter languages/support through its public facade modules"

- id: ARCH-L2-SUPPORT-NO-DB-OR-LINKER
  severity: error
  kind: forbid_dependency
  from:
    paths: ["crates/acdd-index/src/languages/support/**"]
  to:
    zones: [index_db]
    paths: ["crates/acdd-index/src/linker/**"]
  message: "Shared extractor support must not depend on db or linker layers"
```
