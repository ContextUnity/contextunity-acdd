---
name: contract-planning
description: Build, audit, or amend implementation contracts, tasks, and milestone plans. Use to evaluate architectural viability, reconcile ADRs, uncover hidden risks or ambiguities, and constructively propose cleaner designs.
---

# Contract Planning and Architectural Audit

Keep separation of layers crisp:
- **Spec (`WHAT / WHY`)**: goal, non-goals, invariants, behavioral `AC-*`, admitted `scope`, negative boundaries.
- **Blueprint (`ARCHITECTURAL SEAMS`)**: canonical owners, layer placement, `incoming_seams`/`outgoing_seams`, `max_net_loc`, behavioral `verification_seam`s. **Never write pseudocode, exact CLI flag strings, raw SQL, or line-by-line code recipes in blueprint prose.**
- **Build (`HOW`)**: live compiling code and tests verified by the compiler and `tasks.review`.

Before a review repair, limit edits strictly to the unresolved findings; do not invent uncontracted scope.

## 1. Load Repository Context

1. Read root `AGENTS.md`, relevant ADRs/architecture docs, and `.agents/skills/contract-planning/ADDON.md` if present.
2. Ground every spec or blueprint as a delta against durable system truth (`docs/architecture/`, `docs/reference/`, accepted ADRs) and admit affected doc updates into scope.
3. Check prerequisite and successor milestones before slicing tasks.

## 2. Simplicity & Canonical Ownership (Occam's Razor)

1. **Reuse before building**: Name the single canonical owner (`INV-SINGLE-OWNER`) for each responsibility; check adjacent crates/formatters (`crates/*` vs `src/format/`, `acdd-core/path_policy.rs`) for semantic duplicates before adding local grouping/matching/walk helpers, inspect already-populated DTO/domain fields before re-deriving data downstream, and forbid duplicate parsers, shadow registries, or speculative wrappers.
2. **Respect layer boundaries**: Keep leaf core, index, state, and host transport strictly separated with zero circular dependencies.
3. **Contain scope creep**: When a feature uncovers a missing capability in an unrelated upstream subsystem (such as a new language extractor/linker projection), split it into its own milestone or escalate an `AUTHORITY_GAP` instead of inflating the current task.

## 3. Behavioral `AC-*`, Boundaries & Budgets

1. **Observable `AC-*`**: State `Trigger / Precondition -> Action -> Outcome` (including fail-closed `ErrorCode`/rejections) without encoding internal implementation details.
2. **Bounds & safety**: Call out real operational risks (unbounded I/O, N+1 loops, concurrency conflicts, path/symlink confinement) at the contract level so builders solve them cleanly in code.
3. **Disjoint waves & negative boundaries**: Record explicit `negative_boundaries` (forbidden shortcuts, off-limits `protected_paths`), cap `max_net_loc <= ~400-500` prod LOC per task, plan directory-module splits once a production file approaches ~600-800 LOC instead of piling onto monoliths, separate unrelated `AC-*`/subsystems across tasks, and keep write `scope:` globs strictly disjoint within every concurrent DAG wave.

## 4. Map Consumers & Proportional Seams

1. Run `code_map_impact` for breaking changes and search the planning scope for live producers and consumers (`code_map_*`, `acdd_read`) before setting `scope:`, verifying live file paths vs directory modules so no active caller, profile, or doc is omitted.
2. Bind each `AC-*` to its narrowest real production seam (`test-suite-rules`) and note the key failure/mutation case it must catch. Keep logic tests in-memory and reserve tempdirs/Git/subprocesses strictly for isolation seams.

## 5. Blueprint Audit Rules (For Both Architect & Reviewer)

Audit every spec and `blueprint_synthesis` slice along three axes — **strictly at the architectural seam level**:

1. **Completeness**: Every `AC-*` maps 1:1 to bounded tasks, `verification_seam`s, and co-evolved doc scope.
2. **Correctness**: Chosen canonical owners actually own the claimed data/capability, failure modes fail closed, and operational bounds are stated cleanly (leave low-level flag syntax, internal helper names, and exact SQL to `build` + `tasks.review`).
3. **Coherence**: Keep milestone invariants aligned 1:1 with accepted ADRs, `INV-SINGLE-OWNER`, crate DAG layers, `protected_paths`, and disjoint same-wave scopes.

Reviewers MUST NOT reject a clean architectural blueprint for omitting pseudocode or low-level implementation details; verify implementation details against live code and tests during `tasks.review`.
