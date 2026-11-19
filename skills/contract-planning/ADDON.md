# Contract planning — ContextUnity ACDD add-on

This add-on supplies repository-specific planning authority, invariants, and inspection
tools for ContextUnity ACDD.

---

## 1. Architectural Authority and Reading List

Before authoring, auditing, or amending any milestone contract or task in this repository:

1. **Architecture Overview:** Read [`docs/architecture/README.md`](../../docs/architecture/README.md) and [`docs/architecture/performance.md`](../../docs/architecture/performance.md).
2. **Architectural Decisions:** Consult the relevant ADRs in [`docs/adr/`](../../docs/adr/):
   - `ADR 0001`: Leaf isolation and 3-library-crate dependency boundaries (`acdd-core`, `acdd-index`, `acdd-state`).
   - `ADR 0003` & `ADR 0004`: Unified `GateProfile` state engine (`crates/acdd-state`) and `DomainProvider` SPI (`crates/acdd-core/src/ports.rs`).
   - `ADR 0005` & `ADR 0006`: Content-bound `TreeOid` candidate verification and 6-tier `ClaimResp` context bundles.
3. **Subsystem Routing:** Read [`src/AGENTS.md`](../../src/AGENTS.md) and [`docs/AGENTS.md`](../../docs/AGENTS.md) for module decomposition rules.

---

## 2. Mandatory Architectural Invariants

Every contract must be evaluated against these repository invariants:

1. **`INV-CORE-LEAF` (Subsystem Purity):**
   - `crates/acdd-core` is a strict leaf crate. It must not depend on `crates/acdd-index`, `crates/acdd-state`, CLI, MCP, or engine subsystems, and `crates/acdd-state` must never depend on `crates/acdd-index`.
   - Core path helpers (`acdd_core::path_policy`, `acdd_core::fs`) must remain fast, standalone primitives without circular dependencies.
2. **`INV-MINIMAL-INTERVENTION` (Binary Leanliness):**
   - Keep the compiled Rust binary focused on indexing, AST search, code maps, and domain-agnostic state orchestration.
   - Avoid embedding static markdown documents, templates, or onboarding wizards into the compiled binary via `build.rs` codegen or `include_bytes!`. Distribute companion skills and scripts as clean, external assets.
3. **`INV-ZERO-PERFORMANCE-REGRESSION` (I/O Efficiency):**
   - Path resolution (`checked_child`, `canonical_root`) must remain single-pass checks. Never perform multi-entry directory scans (`read_dir` loops) on path components.
4. **`INV-SCOPE-INTEGRITY` (Protected Infrastructure):**
   - Core configuration files (`Cargo.toml`, `crates/*/Cargo.toml`, `crates/acdd-index/build.rs`, `.acdd/profile.yaml`) are protected infrastructure. Contracts must not modify them without explicit milestone-level authorization.

---

## 3. Code Discovery and Inspection Seams

Use ACDD MCP tools rather than unstructured grep/find during contract planning:

- **Symbol Inspection:** `code_map_inspect(selector: "...")` for full caller/callee trees and covering tests.
- **Impact Analysis:** `code_map_impact(selector: "...", depth: 1)` to trace consumer boundaries before assigning task write scopes.
- **Structural Syntax Search:** `ast_grep_search(pattern: "...", language: "...")` for pattern matching.
- **Targeted Source Reading:** `acdd_read(path: "...", selector: "...")` for digest-verified source context.

---

## 4. Integration with ACDD `blueprint_synthesis` Gate and Authoring Workflows

When executing the root `blueprint_synthesis` container (`synthesize` $\to$ `review`) in the active ACDD profile or running standalone authoring workflows (`author-milestone.workflow.yaml`, `author-plan.workflow.yaml`, `author-workflow.workflow.yaml`):

1. Apply the **Architectural Viability Audit** from `SKILL.md` to ensure the milestone, plan, or workflow design is minimal, clean, and robust.
2. Post the synthesized architectural blueprint, data flow, and invariants to `acdd_blackboard` (topic: `decisions`) before gate submission.
3. Submit the structured `proof` payload (`proof: blueprint` from `proofs:` or inline `proof.scheme`) including per-task `tasks[<task_ref>]` entries (`layer`, `summary`, `exported_interfaces`, `negative_boundaries`, `canonical_owners`, `mechanism_rules`, `max_net_loc`, `verification_seam`, `incoming_seams`, `outgoing_seams`), which the engine injects per task into `context_bundle` on `acdd_claim` / `acdd claim`.
