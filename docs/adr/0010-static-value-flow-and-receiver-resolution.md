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

## Consequences

- Exact call counts (`calls`) remain strictly separated from bounded candidate navigation (`possible_calls`), while impact and removal analyses conservatively follow both.
- Static analysis terminates deterministically without executing repository code.
