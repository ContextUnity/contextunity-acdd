---
id: plan-lazy-worktree-scope-diff-duplicates-and-orientation-context
title: 'Plan: Lazy worktree scope (protected_paths barrier), repo-wide ClaimResp orientation, asymmetric diff duplicates with reuse hints, and verify Diff X-Ray'
doc_type: plan
status: completed
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
closure:
  status: completed
  closed_at: 2026-11-28T12:13:20.397Z
  target_base: main
  target_base_commit: 28645ee6090f14220ca92fc6cc0e4602432267fc
  task_receipts:
    orientation-lazy-scope-and-diff-duplicates:
      run_id: plan-lazy-worktree-s-20261128053926-3919028-1
      node_cursor: root:orientation-lazy-scope-and-diff-duplicates.deliver
      gate_id: deliver
      worker_id: coordinator
      claim_revision: 2
      contract_revision: 7
      candidate_tree: 63467bed4c65e36308614cec148187129ccf768c
      committed_oid: 6aab15efe5cf183a37df3a305de67cd60283bb9d
      accepted_at: 2026-11-28T11:13:30.241Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    decisions:
    - |-
      Rearchitect Decision (reconciling #705 architectural_drift across all 8 findings; plan updated in docs/plans/lazy-worktree-scope-diff-duplicates-and-orientation-context.md):

      1. Typed Repo-Wide Orientation Seam (AC-02, INV-REPO-WIDE-ORIENTATION-CONTEXT):
         - Canonical owner: crates/acdd-index/src/db/symbols/search.rs.
         - Extract the batched node signature/metadata lookup already used by search_results_with_navigation into a shared internal helper and expose a bounded typed hit projection (carrying qualname, path, line, end_line, and bounded indexed signature) over the existing RRF/FTS5 search_with_options ranking, with zero disk reads.
         - CodeDomainProvider::provide_symbols (src/state_bridge/code_domain/context.rs) must consume this typed projection directly (deleting untyped JSON /nodes/items parsing) via open_code_db_readonly root-index fallback, keeping citation_text (blueprint cited_symbols/canonical_owners/exported_interfaces) prioritized ahead of target within ADR 0006 compact caps.

      2. Canonical-Worktree-Distinct ScopeMode::Lazy Overlap Fence (AC-01, INV-LAZY-WORKTREE-PROTECTED-BARRIER):
         - Canonical owner: crates/acdd-state/src/machine/scope.rs (pure machine, INV-STATE-PURE-MACHINE).
         - Overlap bypass in check_task_scope_overlap (claim + extend_scope) and schedule eligibility must require distinct canonical worktrees: only bypass TASK_SCOPE_CONFLICT against another claimed unfinished task when both tasks are Worktree-isolated AND resolve to concrete, distinct canonical worktree paths (using ClaimInput.worktree on claim, canonicalized at the usecase boundary via canonicalize_macos_var).
         - Shared explicit/manual or unresolved worktrees fall back to strict scopes_overlap rejection (TASK_SCOPE_CONFLICT), and effective_scope_mode returns ScopeMode::Strict whenever another claimed unfinished task shares the same canonical worktree (fencing auto:lazy_scope even for disjoint declared scopes).
         - Keep detect_scope_violations before plan_auto_admit_lazy_scope so mixed protected + eligible edits fail closed with OUT_OF_SCOPE_CHANGES without mutating extended_scope.

      3. Durable Non-Committed Skeleton Hashes + Real Trait Mapper Exemptions (AC-03, INV-ASYMMETRIC-DIFF-DUPLICATES-AND-HINTS):
         - Keep local_facts.facts_blob bit-identical on ACDD-LOCAL-FACTS\0\x06 (4-field StatementFingerprint: hash, line, end_line, token_count) and keep ENGINE_SCHEMA_VERSION = "15".
         - Persist single-pass literal-masked skeleton hashes in a non-committed auxiliary SQLite table statement_skeletons(path TEXT PRIMARY KEY, skeleton_blob BLOB NOT NULL) outside commitments::DOMAINS (db/schema.rs, written alongside local_facts in db/writer/ingest.rs + src/engine/pipeline/staging.rs, cleaned on delete in db/writer/delta.rs). Extend external ExtractionCache (db/writer/cache.rs) to carry the optional skeleton_blob alongside unchanged \x06 encoded_facts so cold builds, 100% cache hits, and incremental deltas all hydrate statement_skeletons cleanly. duplicates.rs loads and merges statement_skeletons (gracefully tolerating absence in synthetic DBs). Builder: extend task scope via acdd extend-scope for crates/acdd-index/src/db/schema.rs, crates/acdd-index/src/db/writer/, and src/engine/pipeline/.
         - Replace spelling-based mapper exemption in duplicates.rs with canonical NodeDetailKey::IMPL_TRAIT facts projected from nodes.details (exempting <=6-stmt methods only when implementing standard conversion/formatting/comparison traits, never plain functions or inherent methods).

      4. Changed-Symbol Baseline Provenance for Diff Clone Promotion (AC-03, INV-ASYMMETRIC-DIFF-DUPLICATES-AND-HINTS):
         - Reuse existing canonical owners: DomainCheckReq.baseline_ref (default HEAD) + acdd_core::git::read_git_blobs_utf8_batch (single batched call) + acdd_index::ast::extract_typed.
         - Pass explicit diff provenance (diff_paths + baseline_ref) through run_lint_with_diff into duplicates.rs. Compare live callables in diff_paths against baseline_ref callables by stable structural identity and statement fingerprint sequence (hash, skeleton_hash, token_count) — immune to comments and line shifts. Promote >=0.80 exact/similar groups to LintSeverity::Error only when at least one member is newly introduced or structurally modified vs baseline (or in a new file). Comment-only/unrelated edits keep pre-existing clones as Warn; path-only query duplicates (diff_paths: None) stays unchanged.

      5. Commitments Contract Reconciliation (INV-TOOL-CAP-AND-HOST-CLEANLINESS):
         - Reconciled in plan spec: cross-build invariance applies to all 15 domain_commitments domains (all logical + FTS domains, local_facts \x06 payloads, and owner_id=0 domain roots) on identical inputs; build.rs ACDD_LANGUAGE_PROFILE_DIGEST / INDEX_SEMANTICS_VERSION freshness binding stays intact (INV-PERF-CACHE-KEY-BINDING) alongside deterministic same-build acdd-owner-sha256-v2 output_root sealing.

      6. Batched Pre-Limit Cochange Filtering & Single-Pass Diff X-Ray Hints (AC-04, INV-VISIBLE-VERIFY-DIFF-XRAY):
         - Canonical owner crates/acdd-index/src/db/analyze/cochange.rs: replace the N+1 per-file shared-callee query + premature LIMIT 8 with one batched json_each query excluding changed_paths and applying PathExposureFilter (disallowed_path_ids / is_allowed) BEFORE ranking and truncating to max_items.
         - Canonical owners crates/acdd-index/src/db/lint/{mod,duplicates}.rs + src/state_bridge/code_domain/lint.rs: return pre-pagination ranked diff reuse_hints directly from the single run_lint_with_diff duplicate scan using one shared diagnostic formatter (eliminating the second workspace duplicate scan and retiring unused public collect_diff_reuse_hints per INV-NO-DEAD-CODE-SHIMS).

      7. Proof Matrices & Doc/Website Sync (close-review-findings across all ingress surfaces):
         - Extend existing canonical test matrices (state_integration/usecases_e2e.rs, schedule_claim_verify.rs, tests/acdd/, tests/mcp/, tests/query_context/, tests/commitment_integrity/) to assert: shared explicit worktree TASK_SCOPE_CONFLICT + ScopeMode::Strict fence; mixed protected rejection non-mutation; delivered lazy-scope edit surviving through review/{scope_target}/budget/commit; cold-worktree ClaimResp external non-empty signature + blueprint citations on synthesize and build; persisted masked clones across cold/cache/delta; real IMPL_TRAIT exemption; baseline comment-only Warn vs introduced Error; >8 changed/disallowed siblings not suppressing eligible 9th shared-callee sibling; CLI+MCP passing VerifyResp Diff X-Ray parity; and \x06 domain_commitments invariance.
         - Sync docs/reference/workflows.md (code.symbols row) and website/pages/gates_builtin_code_intro.md.in (synthesize code.symbols + build duplicates + Diff X-Ray), verify ./website/build.sh --check and acdd -C <worktree> validate (note worktree acdd.yaml has no memories/retrospectives overlap; the 21 AC-006 archive-link failures in docs check are inherited out-of-scope baseline debt, with zero new docs check errors in admitted scope).
    - |-
      Rearchitect Decision (Round 4 recovery reconciling #709 architectural_drift across all 4 items; zero changes to committed extractor/linker output or 15 domain_commitments):

      1. Positive Standard-Trait Identity from Canonical Extractor/Linker (AC-03, INV-ASYMMETRIC-DIFF-DUPLICATES-AND-HINTS, #709 item 1):
         - Canonical owner: crates/acdd-index/src/db/lint/duplicates.rs (staying inside admitted scope, language-name-free, and keeping all 15 committed domain_commitments tables untouched).
         - Replace the negative-signal method-level implements-edge check and bare-name fallback with positive standard-trait provenance derived from the owning NodeKind::Impl relation (edges kind=contains -> owner impl node + owner.details.impl_trait) joined to the canonical linker resolution_coverage / coverage_expressions / coverage_evidence tables at (owner.path_id, owner.line) matching the stripped base trait expression before generics.
         - Fail closed (ineligible for <=6-stmt trivial mapper exemption) unless ALL preconditions hold: (a) candidate is a method owned by a trait impl with no local implements edge on either method or owning impl, and (b) the owning impl has a positive linker resolution_coverage row with status=external. Missing, resolved, unresolved, or ambiguous coverage is always ineligible.
         - Accept only two positive external-provenance branches:
           * Branch A (explicit std/core/alloc path or import): evidence carries an external import rooted strictly in std, core, or alloc (never a third-party crate); normalize and combine import target + trait expression into a qualified standard path and match against the closed canonical standard mapper trait set (fmt::Display, fmt::Debug, convert::From, convert::TryFrom, convert::Into, convert::TryInto, convert::AsRef, convert::AsMut, ops::Deref, ops::DerefMut, default::Default, clone::Clone, cmp::PartialEq, cmp::Eq, cmp::PartialOrd, cmp::Ord, hash::Hash, borrow::Borrow, borrow::BorrowMut, string::ToString, str::FromStr).
           * Branch B (standard library prelude): evidence matches the canonical prelude provenance for a trait strictly in the real Rust prelude mapper subset (From, TryFrom, Into, TryInto, AsRef, AsMut, Default, Clone, PartialEq, Eq, PartialOrd, Ord, ToString — excluding non-prelude traits like Display/Debug/Deref/Hash/Borrow/FromStr which must arrive via Branch A), AND the owning file has neither an unanalyzed/module-level macro expansion (resolution_coverage expression macro_expansion_not_analyzed or module-to-macro calls edge in edges) nor a wildcard import (dependencies kind=imports matching wildcard) that could shadow prelude names.
         - Load owning-impl provenance and per-path macro/wildcard guards in bounded queries (zero N+1 loops). Retain raw impl_trait on DuplicateCandidate solely for baseline structural identity matching in changed_candidates_vs_baseline.

      2. Typed Ranking & Metadata Seam Before Serialization (AC-02, INV-REPO-WIDE-ORIENTATION-CONTEXT, #709 item 2):
         - Canonical owner: crates/acdd-index/src/db/symbols/search.rs.
         - Introduce a crate-private typed hit/page representation decoded directly from rusqlite::Row (enforcing QueryBudget, statement.readonly(), and page bounds with zero serde_json::Value intermediates), unifying all four search query branches (type filter, exact, wildcard, and default RRF/FTS5) under one internal typed search owner that normalizes match_rank -> match_reason directly on typed hits (retiring SearchHitDto and RankedSearchHitDto).
         - Refactor indexed_node_metadata to accept typed &[&str] symbol IDs and decode (id, signature, is_test, docstring) directly from rusqlite::Row instead of paging::rows -> Vec<Value>.
         - Wire all three public entrypoints onto these typed owners:
           * search_signature_hits consumes typed ranked hits + typed metadata and projects directly into Vec<SearchSignatureHit> with zero JSON creation, parsing, or fallback coordinate defaulting.
           * search_with_options and search_results_with_navigation consume the same typed page/metadata owners and serialize once at the response boundary while preserving exact wire JSON parity for Detail::Compact and Detail::Full (including nullable total, has_more, next_offset, generation, and navigation previews).

      3. Eliminate Introduced 5-Line Row-Decoding Block Clone (INV-NO-DEAD-CODE-SHIMS, #709 item 3):
         - Canonical owner: crates/acdd-index/src/db/lint/duplicates.rs.
         - Replace the imperative while-let / sequential row.get(...) unpacking block in collect_symbol_duplicates (which collided as a 5-line skeleton clone with dead_tails.rs:762-766) with idiomatic typed rusqlite query_map projection directly into DuplicateCandidate.
         - Before submitting build, run duplicates (min_lines=5 over crates/acdd-index/src/db/lint and crates/acdd-index/src/db/symbols), dead_tails, and lint (all categories) using the compiled candidate binary (not the older release binary) to verify zero clones, zero dead_tails/phantom_tested/dangling_calls, and zero lint errors without exclusions.

      4. Required Consolidated Matrix Rows — Exposure, Macro-Local Trait, and Real CLI-vs-MCP Parity (AC-03, AC-04, #709 item 4):
         - Keep all new coverage consolidated as rows inside existing canonical matrices (zero new standalone #[test] functions per tests/AGENTS.md):
           * tests/acdd/lint_gate_and_docs_profile.rs (cochange matrix): add >max_items higher-ranked untouched siblings excluded by a real WorkspacePathPolicy (exercised both via collect_untouched_cochange_siblings with Some(&policy) and via DomainCheckReq.path_policy through code.lint), proving disallowed siblings never leak and never truncate lower-ranked eligible_ninth.rs.
           * tests/acdd/lint_gate_and_docs_profile.rs (trait identity matrix): add rows for macro-defined local From<u32> (two <=6-stmt impls with differing literals -> Failed duplicates.similar) and name-only LocalFrom<u32> control (-> Failed duplicates.similar), alongside the retained real std From (Passed), direct custom Display (Failed), inherent method (Failed), and free function (Failed) rows.
           * tests/mcp/harness_contract.rs (verify Diff X-Ray parity): replace the synthetic MCP-JSON-deserialized CLI render shortcut with a real CLI verify invocation through crate::cli::run_cli / run_cli_json against the same xray_ws workspace/run in both JSON and Markdown modes, asserting VerifyResp JSON parity (passed, execution_digest, [xray.cochange], [xray.reuse_hint]) and identical rendered Markdown across real CLI and real MCP ingress.
    - |-
      Rearchitect Decision (Round 5 recovery reconciling #711 architectural_drift across all 3 items; zero changes to committed extractor/linker output or 15 domain_commitments; existing directory scopes crates/acdd-index/src/db/lint/ and crates/acdd-index/src/db/symbols/ already admit all new submodules):

      1. Fail-Closed Qualified/Imported Standard-Trait Provenance Under Local Shadowing (AC-03, INV-ASYMMETRIC-DIFF-DUPLICATES-AND-HINTS, #711 item 1):
         - Root cause: has_positive_standard_trait_provenance checked !path_prelude_shadowed only on Branch B (prelude) after Branch A had already accepted `reference through external import std; target unverified` unconditionally. Because value_flow_phase/candidates.rs falls back to external_import("std") when no local indexed candidate resolves, a #![no_std] file defining `mod std { pub mod convert { pub trait From<T> ... } }` inside an unanalyzed macro (`macro_rules! local_std!()`), or shadowing `std`/`core`/`alloc` via wildcard imports or local module/import declarations, produces identical `status=external`, `reference through external import std; target unverified` evidence.
         - Canonical owner: crates/acdd-index/src/db/lint/duplicates/provenance.rs (staying inside admitted lint scope, language-name-free, zero changes to committed extractor/linker output or domain_commitments).
         - Uniform shadowing barrier: Move the `!path_shadowed` check to the top of `has_positive_standard_trait_provenance` so BOTH Branch A (qualified/imported `std`/`core`/`alloc` paths with unverified external-import evidence) AND Branch B (standard library prelude traits) fail closed (`false`) whenever the owning file is shadowable. Also defensively reject relative `base_trait` prefixes (`self::`, `super::`, `crate::`) before Branch A.
         - Harden `load_prelude_shadowed_paths` -> `load_shadowed_paths` (one bounded SQL query over committed tables): mark a `path_id` as shadowable whenever ANY of these holds on that path:
           (a) `resolution_coverage` joined to `coverage_expressions` has `expression = 'macro_expansion_not_analyzed'` (unanalyzed macro expansion anywhere in the file);
           (b) `edges` has `kind = 'calls'` with `src.kind IN ('module', 'module_declaration')` and `dst.kind = 'macro'` (module-scope macro invocation, including inline `mod { ... }` blocks);
           (c) `dependencies` has `kind = 'imports'` with wildcard symbol (`*`, `%::*`, `%.*`);
           (d) `nodes` has `name IN ('std', 'core', 'alloc')` with `kind IN ('module', 'module_declaration', 'import')` (direct local module/import shadowing a standard root);
           (e) `dependencies` has `kind = 'imports'` bringing a `std`/`core`/`alloc` root into scope (`symbol IN ('std', 'core', 'alloc')` or ending with `::std`/`::core`/`::alloc` or `.std`/`.core`/`.alloc`).
         - Clean non-shadowed files continue to exempt real unqualified prelude mappers (`impl From<u32>`) AND real qualified/imported standard-library mappers (`impl std::convert::From<u32>`, `impl core::convert::From<u32>`, `impl ::std::convert::From<u32>`, `use std::fmt; impl fmt::Display`, `use std::convert::From; impl From<u32>`).
         - Consolidated trait matrix rows (tests/acdd/lint_gate_and_docs_profile.rs; extend existing table loop, zero new standalone #[test] functions): add (i) qualified macro-local `#![no_std]` `std::convert::From<u32>` (two <=6-stmt impls with differing numeric literals + `macro_rules! local_std` defining `mod std { pub mod convert { pub trait From<T> ... } }`) -> Failed (`duplicates.similar`), (ii) real qualified `impl std::convert::From<u32>` control (two <=6-stmt impls with differing literals, no local shadowing) -> Passed, and (iii) direct non-macro `#![no_std]` local `mod std { ... }` `impl std::convert::From<u32>` control -> Failed (`duplicates.similar`), alongside all retained matrix rows.

      2. Cohesive Directory-Module Split for `crates/acdd-index/src/db/lint/duplicates/` (#711 item 2):
         - Delete oversized monolith `crates/acdd-index/src/db/lint/duplicates.rs` (1338 LOC > 1200 limit) and split along its 4 distinct domain phases inside already-admitted `crates/acdd-index/src/db/lint/duplicates/` (all files strictly under 600 prod lines; zero public/crate API breakage for `db/lint/mod.rs` and `db/traversal/duplicates.rs`):
           * `duplicates/mod.rs` (~260 LOC): shared DTO/report types (`FileDuplicateGroup`, `DuplicateCandidate`, `DuplicateLocation`, `DuplicateGroup`, `ReuseHint`, `LintDiffContext`, `DuplicateScanRequest`, `SymbolDuplicateReport`), re-exports preserving `db::lint` surface, `collect_file_duplicates`, `collect_duplicate_violations`, `path_matches_probe`, and graph-driven suggestion/hint formatting (`format_duplicate_suggestion`, `format_reuse_hint_message`).
           * `duplicates/provenance.rs` (~220 LOC): single canonical owner for positive standard-trait mapper provenance & shadowing guards (`load_shadowed_paths`, `load_standard_mapper_method_ids`, `has_positive_standard_trait_provenance`, `is_trivial_trait_mapper`, standard trait allowlist helpers).
           * `duplicates/similarity.rs` (~530 LOC): single canonical owner for asymmetric Probe-vs-Workspace corpus scanning, `statement_skeletons` hydration (`load_statement_fingerprints_for_paths`), LCS/Jaccard statement similarity, exact/similar clustering, block-window coalescing (`coalesce_block_window_matches`), and `collect_symbol_duplicates`.
           * `duplicates/baseline.rs` (~160 LOC): single canonical owner for baseline diff changed-callable comparison (`changed_candidates_vs_baseline`, batched Git blob extraction, canonical `(qualname, kind, impl_trait)` join + statement sequence comparison).

      3. Cohesive Directory-Module Split for `crates/acdd-index/src/db/symbols/search/` (#711 item 3):
         - Delete oversized monolith `crates/acdd-index/src/db/symbols/search.rs` (1341 LOC > 1200 limit) and split along its 4 distinct domain phases inside already-admitted `crates/acdd-index/src/db/symbols/search/` (all files strictly under 600 prod lines; zero public API breakage for `db/symbols/mod.rs` and wire responses):
           * `search/mod.rs` (~90 LOC): public entrypoints & re-exports (`SearchSignatureHit`, `search_with_options`, `search_signature_hits`, `search_results_with_navigation`), wiring `search_with_options` and `search_signature_hits` onto `query::search_typed` and `projection::*`.
           * `search/projection.rs` (~430 LOC): single canonical owner for typed hit/page models (`TypedSearchHit`, `TypedSearchFullFields`, `TypedSearchPage`, `SearchSignatureHit`, `SearchRowTrailer`), `rusqlite::Row` decoding & match-reason normalization (`decode_typed_search_hit`, `normalize_match_reason`), bounded ranked/window page execution (`collect_typed_search_rows`, `query_typed_ranked_page`, `query_typed_window_page`), shared batched `json_each` metadata hydration (`IndexedNodeMetadata`, `indexed_node_metadata`, `bounded_signature`), and compact/full wire JSON serialization (`SearchHitWire`, `to_wire_json`, `into_page_json`).
           * `search/query.rs` (~580 LOC): single canonical owner for FTS5 tokenization, LIKE escaping, scope/classification SQL predicates, graph-degree CTEs, and the four typed query strategies (`try_type_filter_search`, `try_default_exact_search`, `run_exact_search_query`, `run_wildcard_search_query`, `search_typed`).
           * `search/navigation.rs` (~300 LOC): single canonical owner for source-cached navigation enrichment, indexed-text match previews, and top-hit graph pulse (`SearchSourceCache`, `SearchNavigationItemDto`, `InspectSelectorDto`, `SourcePreviewWindowDto`, `apply_indexed_text_match_preview`, `search_graph_pulse`, `search_results_with_navigation`).

      Verification requirements before build submit (using the compiled candidate binary):
      - Strict Clippy (`cargo clippy --workspace --all-targets --all-features -- -D warnings`), `cargo test -p acdd-index --lib`, `cargo test --test acdd lint_gate_and_docs_profile`, `cargo test --test query_context`, `cargo test --test mcp`, `cargo test --test commitment_integrity`.
      - Rebuild worktree code-map with the candidate binary and confirm: zero >=5-line clones in `crates/acdd-index/src/db/lint` and `crates/acdd-index/src/db/symbols`, zero `dead_tails`/`phantom_tested`/`dangling_calls`, zero `lint` errors and zero `syntax.file_prod_lines_exceeded` warnings in both directories, and bit-identical `domain_commitments` against the release binary.
    - 'Coordinator architectural decision on #717 item 3: fixing extern_crate_declaration and ::-prefixed external paths at the canonical Rust extractor/linker owner (crates/acdd-index/src/languages/rust/mod.rs, linker/rust/semantic_context.rs, linker/resolution/value_flow_phase/dispatch.rs) instead of language-specific heuristics in db/lint/duplicates/provenance.rs intentionally updates the 3 owner_id=0 Rust domain_commitments roots (coverage_language_counts, dependencies, resolution_coverage) relative to the main baseline target/release/acdd. AC-03 and INV-TOOL-CAP-AND-HOST-CLEANLINESS remain satisfied via candidate self-consistency and commitment_integrity.'
  metrics_summary:
    decisions_count: 4
    deferred_findings_count: 0
    failed_traces: 14
    passed_traces: 11
    task_receipts_count: 1
    tasks_cancelled: 0
    tasks_completed: 1
    tasks_total: 1
    total_claims: 27
    total_review_cycles: 0
    total_traces: 29
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
target: Implement ScopeMode::Lazy (protected_paths-only barrier for worktree builds), DomainCheckOutcome::Passed.diagnostics Diff X-Ray, repo-wide RRF ClaimResp orientation with cold-worktree fallback, and asymmetric diff-aware duplicates with graph-ranked reuse_hints (deleting hardcoded repo names)
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
extended_scope:
- path: crates/acdd-index/src/db/analyze/mod.rs
  reason: Update DomainCheckOutcome::Passed callers and export collect_untouched_cochange_siblings
- path: crates/acdd-state/tests/state_integration/common.rs
  reason: Update DomainCheckOutcome::Passed callers and export collect_untouched_cochange_siblings
- path: src/state_bridge/docs_domain/mod.rs
  reason: Update DomainCheckOutcome::Passed callers and export collect_untouched_cochange_siblings
- path: src/state_bridge/spec_domain.rs
  reason: Update DomainCheckOutcome::Passed callers and export collect_untouched_cochange_siblings
- path: crates/acdd-index/src/db/writer/cache/tests.rs
  reason: Update LOCAL_FACTS_BINARY_MAGIC test assertion
- path: crates/acdd-state/tests/state_integration/**
  reason: Update state_integration assertions for ScopeMode::Lazy and embedded code.yaml duplicates
- path: crates/acdd-index/src/db/schema.rs
  reason: 'Per rearchitect #706 item 3: non-committed statement_skeletons auxiliary table, ExtractionCache skeleton_blob, staging/delta writers'
- path: crates/acdd-index/src/db/writer/**
  reason: 'Per rearchitect #706 item 3: non-committed statement_skeletons auxiliary table, ExtractionCache skeleton_blob, staging/delta writers'
- path: src/engine/pipeline/**
  reason: 'Per rearchitect #706 item 3: non-committed statement_skeletons auxiliary table, ExtractionCache skeleton_blob, staging/delta writers'
- path: crates/acdd-index/src/languages/rust/mod.rs
  reason: 'Fix #716 cleanly at canonical Rust extractor/linker owner instead of language-specific logic in db/lint'
- path: crates/acdd-index/src/linker/resolution/value_flow_phase/dispatch.rs
  reason: 'Fix #716 cleanly at canonical Rust extractor/linker owner instead of language-specific logic in db/lint'
- path: crates/acdd-index/src/linker/rust/semantic_context.rs
  reason: 'Fix #716 cleanly at canonical Rust extractor/linker owner instead of language-specific logic in db/lint'
- path: crates/acdd-index/src/linker/semantic_context/context.rs
  reason: 'Fix #716 cleanly at canonical Rust extractor/linker owner instead of language-specific logic in db/lint'
- path: crates/acdd-index/src/linker/semantic_context/preparation.rs
  reason: 'Fix #716 cleanly at canonical Rust extractor/linker owner instead of language-specific logic in db/lint'
- path: tests/linker/early_interning_and_compact_facts.rs
  reason: 'Hydrate statement_skeletons in binary roundtrip parity test per #706 item 3'
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: lazy-worktree-scope-and-passed-diagnostics
  title: Add ScopeMode (Lazy | Strict) + effective_scope_mode in machine/scope.rs, auto-admit non-protected changed_paths into extended_scope (auto:lazy_scope) for worktree builds across verify/schedule/commit/budget/command, remove req.node.scope-only override in context/tiers.rs, add optional diagnostics to DomainCheckOutcome::Passed, and wire code.symbols on blueprint_synthesis + duplicates on tasks.build in embedded code.yaml
  status: completed
  evidence: Verified in build rev13 (exit_code=0) and independent review rev10 (passed)
- subtask_ref: asymmetric-diff-duplicates-and-verify-xray
  title: Add single-pass literal-masked skeleton_hash (bumping LOCAL_FACTS_BINARY_MAGIC), introduce DuplicateScanRequest (Probe vs whole-workspace Corpus), promote new diff clones to Error, emit graph-ranked 0.45..0.80 reuse_hints, delete hardcoded crate/repo names from format_duplicate_suggestion, extract collect_cochange_pairs, and forward Diff X-Ray (untouched siblings + reuse_hints) into DomainCheckOutcome::Passed.diagnostics
  status: completed
  evidence: Verified in build rev13 (exit_code=0) and independent review rev10 (passed)
- subtask_ref: repo-wide-claim-orientation-and-sync
  title: Reuse db/symbols/search.rs for typed repo-wide RRF lookup in CodeDomainProvider::provide_symbols (with open_code_db_readonly root fallback + citation_text blueprint symbols), render Passed diagnostics in format/lifecycle.rs, add focused seam tests across tests/{acdd,query_context,mcp,commitment_integrity}/, and sync ADR 0006, docs, skills, and website
  status: completed
  evidence: Verified in build rev13 (exit_code=0) and independent review rev10 (passed)
receipt:
  run_id: plan-lazy-worktree-s-20261128053926-3919028-1
  node_cursor: root:orientation-lazy-scope-and-diff-duplicates.deliver
  gate_id: deliver
  worker_id: coordinator
  claim_revision: 2
  contract_revision: 7
  candidate_tree: 63467bed4c65e36308614cec148187129ccf768c
  committed_oid: 6aab15efe5cf183a37df3a305de67cd60283bb9d
  accepted_at: 2026-11-28T11:13:30.241Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```
