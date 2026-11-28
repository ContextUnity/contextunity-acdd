---
id: plan-lazy-worktree-scope-diff-duplicates-and-orientation-context
title: "Plan: Lazy worktree scope (protected_paths barrier), repo-wide ClaimResp orientation, asymmetric diff duplicates with reuse hints, and verify Diff X-Ray"
doc_type: plan
status: active
workflow: hotfix
owners:
  - .acdd/workflows/default.yaml
  - crates/acdd-core/src/ports.rs
  - crates/acdd-index/src/ast/fingerprints.rs
  - crates/acdd-index/src/models/
  - crates/acdd-index/src/typed_facts/
  - crates/acdd-index/src/db/lint/
  - crates/acdd-index/src/db/traversal/duplicates.rs
  - crates/acdd-index/src/db/analyze/cochange.rs
  - crates/acdd-index/src/db/symbols/
  - crates/acdd-state/src/machine/
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/src/context/
  - crates/acdd-state/src/workflow/embedded/code.yaml
  - src/state_bridge/code_domain/
  - src/format/
  - tests/acdd/
  - tests/query_context/
  - tests/mcp/
  - tests/commitment_integrity/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/reference/
  - skills/acdd/
  - website/
invariants:
  - 'INV-LAZY-WORKTREE-PROTECTED-BARRIER: For IsolationMode::Worktree task builds under ScopeMode::Lazy, protected_paths and WorkspacePathPolicy::is_candidate_included are the sole hard write barriers; non-protected changed_paths auto-admit into extended_scope (auto:lazy_scope) without OUT_OF_SCOPE_CHANGES, claim invalidation, or TASK_SCOPE_CONFLICT across isolated worktrees, keeping commit.rs, {scope_target}, max_net_loc, and tasks.review context 100% consistent while IsolationMode::InPlace and narrow gates (memory_synthesis, documentation, polish) stay strictly scoped.'
  - 'INV-REPO-WIDE-ORIENTATION-CONTEXT: CodeDomainProvider::provide_symbols opens code-map.sqlite via open_code_db_readonly (root fallback on cold worktrees) and combines in-scope symbols with repo-wide RRF/FTS5 matches from crates/acdd-index/src/db/symbols/search.rs over target + citation_text (including blueprint cited_symbols/canonical_owners) within ADR 0006 knapsack budgets; blueprint_synthesis.synthesize in embedded code.yaml also receives code.symbols.'
  - 'INV-ASYMMETRIC-DIFF-DUPLICATES-AND-HINTS: duplicates in crates/acdd-index/src/db/lint/duplicates.rs evaluates Probe (git diff or path) asymmetrically against the entire workspace AST statement hash index (with type-preserving $str/$num literal masking exempting trivial Display/From/AsRef mappers), promotes newly introduced >=0.80 diff clones to LintSeverity::Error, emits graph-ranked 0.45..0.80 reuse_hints (inbound_callers + signature) in both duplicates and code.lint, and removes all hardcoded repo/crate names from format_duplicate_suggestion.'
  - 'INV-VISIBLE-VERIFY-DIFF-XRAY: DomainCheckOutcome::Passed carries optional non-blocking diagnostics forwarded into CheckResultRecord.diagnostics and VerifyResp so passing acdd verify runs surface bounded Diff X-Ray hints (top-3 untouched cochange/shared-callee siblings across other files + diff reuse_hints) directly to the agent.'
  - 'INV-TOOL-CAP-AND-HOST-CLEANLINESS: Preserves UX-004s 18-tool static MCP cap, adds zero Cargo.toml dependencies, keeps all new SQLite queries in crates/acdd-index/src/db/, and preserves bit-identical acdd-owner-sha256-v2 commitments.'
acceptance_criteria:
  - 'AC-01: IsolationMode::Worktree task builds auto-admit non-protected out-of-scope edits into extended_scope (auto:lazy_scope) across verify, {scope_target}, max_net_loc, commit.rs, and tasks.review, while protected_paths still fail closed with OUT_OF_SCOPE_CHANGES and in_place/narrow gates stay strict.'
  - 'AC-02: Fresh worktree ClaimResp on blueprint_synthesis.synthesize and tasks.build surfaces canonical external signatures via typed repo-wide RRF queries over target + citation_text within ADR 0006 compact byte caps.'
  - 'AC-03: Asymmetric Probe-vs-Workspace duplicates catches cross-crate >=0.80 diff clones as Error, emits graph-ranked 0.45..0.80 reuse_hints with real signature and inbound_callers, has zero hardcoded crate/repo names, and preserves commitment_integrity.'
  - 'AC-04: Passing acdd verify forwards DomainCheckOutcome::Passed.diagnostics (untouched cochange/shared-callee siblings + reuse_hints) into VerifyResp across CLI/MCP, embedded code.yaml enables duplicates on tasks.build, MCP tool count stays 18, and docs/validate/website checks pass.'
---

# Plan: Lazy worktree scope, repo-wide `ClaimResp` orientation, asymmetric diff `duplicates` with reuse hints, and `verify` Diff X-Ray

## Umbrella Structures, Canonical Reuse & Code Retirement

| Category | Symbol / Seam | Action |
|---|---|---|
| **New umbrella enum** | `ScopeMode` (`Lazy \| Strict`) + `effective_scope_mode(...)` in `crates/acdd-state/src/machine/scope.rs` | Single pure resolver shared by `verify.rs`, `schedule.rs`, `commit.rs`, and `budget.rs` (reusable by `AC-005`). |
| **New umbrella request** | `DuplicateScanRequest<'a>` in `crates/acdd-index/src/db/lint/duplicates.rs` | Unifies `db/lint/mod.rs` and `db/traversal/duplicates.rs`; extends `SymbolDuplicateReport` with `reuse_hints` (`inbound_callers`, `signature`). |
| **Reuse existing owner** | `crates/acdd-index/src/db/symbols/search.rs` | Typed repo-wide RRF/FTS5 canonical-owner helper (zero duplicate RRF query). |
| **Reuse existing owner** | `crates/acdd-index/src/db/analyze/cochange.rs` | Extract shared `collect_cochange_pairs(...)` helper used by `analyze_cochange` and `Diff X-Ray`. |
| **Reuse existing owner** | `DomainContextReq.citation_text` + `tiers.rs::build_citation_text` | Append blueprint `pre_task_slice` `cited_symbols`/`canonical_owners` into existing `citation_text`. |
| **Reuse existing owner** | `open_code_db_readonly` (`src/state_bridge/common.rs`) | Used in `provide_symbols` (matching `provide_tests`) for cold-worktree root-index fallback. |
| **Retire / delete** | Hardcoded `"crates/acdd-core"`, `"crates/acdd-index/build_manifests.rs"`, `"contextunity-acdd"` in `duplicates.rs::format_duplicate_suggestion` | Replace with graph-driven owner suggestion (`inbound_callers` + `pub`). |
| **Retire / delete** | `req.node.scope`-only override in `crates/acdd-state/src/context/tiers.rs` | Delete special-case branch so `code.symbols` / `code.tests` observe `admitted_scope` (`scope ∪ extended_scope`) for `tasks.review`. |
| **Retire / delete** | Single-prefix `WHERE p.path >= ?2 AND p.path < ?3` corpus filter in `duplicates.rs::collect_symbol_duplicates` | Replace with asymmetric Probe (`diff`/`path`) vs full-workspace Corpus over `stmt_index`. |

## Tasks in this plan

### task: orientation-lazy-scope-and-diff-duplicates

```yaml
task_ref: orientation-lazy-scope-and-diff-duplicates
target: "Implement ScopeMode::Lazy (protected_paths-only barrier for worktree builds), DomainCheckOutcome::Passed.diagnostics Diff X-Ray, repo-wide RRF ClaimResp orientation with cold-worktree fallback, and asymmetric diff-aware duplicates with graph-ranked reuse_hints (deleting hardcoded repo names)"
contract_revision: 1
depends_on: []
tags:
  - default
scope:
  - .acdd/workflows/default.yaml
  - crates/acdd-core/src/ports.rs
  - crates/acdd-index/src/ast/fingerprints.rs
  - crates/acdd-index/src/models/
  - crates/acdd-index/src/typed_facts/
  - crates/acdd-index/src/db/lint/
  - crates/acdd-index/src/db/traversal/duplicates.rs
  - crates/acdd-index/src/db/analyze/cochange.rs
  - crates/acdd-index/src/db/symbols/
  - crates/acdd-state/src/machine/
  - crates/acdd-state/src/usecases/
  - crates/acdd-state/src/context/
  - crates/acdd-state/src/workflow/embedded/code.yaml
  - src/state_bridge/code_domain/
  - src/format/
  - tests/acdd.rs
  - tests/acdd/
  - tests/query_context.rs
  - tests/query_context/
  - tests/mcp.rs
  - tests/mcp/
  - tests/commitment_integrity/
  - docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  - docs/reference/
  - skills/acdd/
  - website/
status: planned
subtasks:
  - subtask_ref: lazy-worktree-scope-and-passed-diagnostics
    title: "Add ScopeMode (Lazy | Strict) + effective_scope_mode in machine/scope.rs, auto-admit non-protected changed_paths into extended_scope (auto:lazy_scope) for worktree builds across verify/schedule/commit/budget/command, remove req.node.scope-only override in context/tiers.rs, add optional diagnostics to DomainCheckOutcome::Passed, and wire code.symbols on blueprint_synthesis + duplicates on tasks.build in embedded code.yaml"
  - subtask_ref: asymmetric-diff-duplicates-and-verify-xray
    title: "Add single-pass literal-masked skeleton_hash (bumping LOCAL_FACTS_BINARY_MAGIC), introduce DuplicateScanRequest (Probe vs whole-workspace Corpus), promote new diff clones to Error, emit graph-ranked 0.45..0.80 reuse_hints, delete hardcoded crate/repo names from format_duplicate_suggestion, extract collect_cochange_pairs, and forward Diff X-Ray (untouched siblings + reuse_hints) into DomainCheckOutcome::Passed.diagnostics"
  - subtask_ref: repo-wide-claim-orientation-and-sync
    title: "Reuse db/symbols/search.rs for typed repo-wide RRF lookup in CodeDomainProvider::provide_symbols (with open_code_db_readonly root fallback + citation_text blueprint symbols), render Passed diagnostics in format/lifecycle.rs, add focused seam tests across tests/{acdd,query_context,mcp,commitment_integrity}/, and sync ADR 0006, docs, skills, and website"
```
