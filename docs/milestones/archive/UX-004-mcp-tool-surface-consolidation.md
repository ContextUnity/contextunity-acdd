---
id: UX-004-mcp-tool-surface-consolidation
title: Lean MCP surface (20 -> 18 tools, active-run-only acdd_state) and read-only code_map_analyze diagnostics (diff_impact, boundary, path, entrypoints, PageRank hotspots, cochange)
doc_type: contract
status: completed
owners:
- crates/acdd-core/src/git.rs
- crates/acdd-core/src/ports.rs
- crates/acdd-core/src/subsystems.rs
- crates/acdd-index/src/db/
- crates/acdd-state/src/context/
- crates/acdd-state/src/git_ops/commit.rs
- crates/acdd-state/src/profile/embedded/
- crates/acdd-state/tests/
- .acdd/profile.yaml
- .agents/
- src/engine/guide.rs
- src/engine/reader.rs
- src/state_bridge/mod.rs
- src/state_bridge/code_domain/
- src/format/
- src/cli/
- src/mcp/tools/
- src/mcp/server/
- benchmarks/profiles/
- tests/query_context/
- tests/mcp/
- tests/acdd/
- AGENTS.md
- src/AGENTS.md
- README.md
- INSTALL.md
- docs/adr/
- docs/architecture/
- docs/reference/
- docs/runbooks/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
- docs/milestones/UX-004-mcp-tool-surface-consolidation.md
invariants:
- 'INV-LEAN-STATIC-MCP-SURFACE: Static MCP tool count drops cleanly from 20 to 18 (9 code, 2 docs, 2 core, 5 active-run state) across Server::tool_router() and crates/acdd-core/src/ports.rs::BUILTIN_MCP_TOOLS by folding code_map_prove_removal and code_map_tests into code_map_query(operation="prove_removal" | "tests") with zero new fields on code_map_query and zero new MCP tools.'
- 'INV-ACTIVE-RUN-MCP-STATE: MCP acdd_state focuses strictly on active-run lifecycle operations; out-of-workflow spec planning/authoring actions (roadmap, draft) are removed from McpStateAction and StateInput with fail-closed redirect hints to CLI acdd roadmap and acdd draft.'
- 'INV-COMPACT-ANALYZE-EXTENSION: All six new analysis modes (diff_impact, boundary, path, entrypoints, hotspots, cochange) live inside existing code_map_analyze adding strictly 3 compact optional schema properties (mode, to, base), preserving default mode="summary", include_cycles, lint, and read-only SQL compatibility.'
- 'INV-READONLY-OVER-INDEX: UX-004 analyzers stay a pure read-only layer over already-indexed code-map.sqlite facts (nodes, edges, details.entrypoint, doc_sections, is_test) without modifying language extractors, linkers, or lint pipelines (Rust rmcp/clap/inline-mod extraction belongs to LS-004).'
- 'INV-SINGLE-OWNER-DIFF-IMPACT: code_map_analyze(mode="diff_impact") delegates strictly to the canonical crates/acdd-index/src/db/traversal/change_impact.rs engine, computing signature_touched strictly from indexed declaration start-line overlap (nodes.line, zero downstream syntax scanning or cat-file blob reparsing), verified call-graph uncovered_symbols, and governed INV-* affected_docs.'
- 'INV-DETERMINISTIC-PAGERANK: PageRank centrality in hotspots computes deterministically over sorted node_hash graphs loaded from code-map.sqlite (damping 0.85, bounded iterations and graph limits within the 2-second query budget) with zero SQLite schema changes and zero new third-party dependencies, reusable by AC-007 GET /api/map.'
- 'INV-GIT-SINGLE-OWNER: Bounded Git history for cochange (--name-status -z --find-renames, zero --find-copies-harder) and diff_impact invoke Git exclusively through crates/acdd-core/src/git.rs and exclude sensitive/internal/DB paths via WorkspacePathPolicy and shared sensitive-path matching.'
- 'INV-INLINE-CLAIM-AND-READ-BUDGET: Default acdd_claim (<= 9,500 bytes inline, optional detail="full" for 32 KiB) and default acdd_read (<= 9,000 bytes inline with exact next_start_line/source_offset continuation) fit cleanly inside ~10 KiB MCP inline buffers while retaining full task framing (Contract, Guidance, Unresolved Findings, Pre-Task Slice, mandatory doc paths, Invariants, non-derived Scoped Symbols, Scoped Tests, External Callers).'
- 'INV-SCOPED-COMMIT-STAGED-RENAMES: create_scoped_commit_with_expected includes real worktree index entries (list_stage_entries(&req.worktree, None)) alongside base_head and untracked entries so git mv / staged additions never drop during scoped delivery commits.'
acceptance_criteria:
- 'AC-01: GIVEN an indexed workspace, WHEN code_map_analyze(mode="hotspots" | "boundary" | "path" | "entrypoints") runs, THEN hotspots returns deterministic PageRank centrality, fan-in/fan-out, LOC/complexity proxy, and call-based test-gap rankings within the 2-second query budget; boundary reports inbound external callers, grouped outbound dependencies, and pub symbols with zero external callers; path returns shortest/top-K typed edge chains (failing closed when to is omitted); and entrypoints traces reverse paths to stored nodes.details.entrypoint roots plus uncalled public root symbols.'
- 'AC-02: GIVEN a Git workspace with uncommitted changes or commit history, WHEN code_map_analyze(mode="diff_impact" | "cochange") runs, THEN diff_impact delegates through canonical change_impact.rs and returns changed symbols, signature_touched flags (strictly by indexed nodes.line declaration-line overlap, zero downstream syntax scanning or cat-file reparsing), governed INV-* affected_docs, reaching test_case selectors, and verified uncovered_symbols (excluding lexical fallback); cochange returns bounded Git co-change pairs lacking direct static edges while excluding WorkspacePathPolicy/sensitive paths and binding continuations to history+policy fingerprint; non-Git roots fail closed cleanly.'
- 'AC-03: GIVEN the updated MCP server under full preset, WHEN tools/list is queried, THEN exactly 18 built-in static tools are exposed matching BUILTIN_MCP_TOOLS (code_map_prove_removal and code_map_tests removed; code_map_analyze preserved with only mode/to/base added; zero new fields on code_map_query; net tools/list schema bytes shrink), plugin registration reserves retired tool names, embedded profiles and reader/guide hints omit removed tool names, and code_map_query(operation="prove_removal" | "tests") matches former dedicated tool output 1:1.'
- 'AC-04: GIVEN direct MCP calls to removed tools (code_map_prove_removal, code_map_tests) or removed acdd_state actions (roadmap, draft), WHEN executed, THEN every call fails closed with an explicit redirect hint (code_map_query(operation=...) or CLI acdd roadmap / acdd draft), while CLI acdd query remove, acdd query tests, acdd query analyze --mode, acdd roadmap, and acdd draft remain fully functional.'
- 'AC-05: GIVEN the completed UX-004 tree, WHEN tests/query_context/, tests/mcp/, tests/acdd/, acdd docs check, acdd validate, and acdd lint --categories arch,docs run, THEN active docs, ADRs, routers, skills/acdd/, benchmarks/profiles/, ROADMAP.md, and CHANGELOG.md reflect the 18 static MCP tools, active-run-only acdd_state, code_map_analyze modes, inline acdd_claim (<= 9,500 bytes) and acdd_read (<= 9,000 bytes) behavior, and pragmatic ACDD-first navigation guidance (removing the hard .agents/read_file_guard.py / .agents/hooks.json block) with zero errors.'
- 'AC-06: GIVEN an active gate claim, a large workspace file read, or a task with git-mv staged renames, WHEN acdd_claim runs with default compact detail (or detail="full"), acdd_read reads a large line window, or create_scoped_commit_with_expected commits staged renames, THEN default acdd_claim stays <= 9,500 bytes inline (filtering generated/derive .ne/.le/.ge noise from Scoped Symbols, stripping headings TOC spam from Repository Instructions, deduplicating paths in Scoped Tests, capping External Callers to top-5 + overflow count, bounding Blackboard to latest 6 x 220 chars while keeping Unresolved Findings intact, and removing .acdd/profile.yaml duplicate code.tools override without weakening code.principles/core.*/task.*/repo.tests), acdd_read defaults to <= 9,000 bytes inline with exact next_start_line continuation, and staged git-mv renames commit identically to unstaged moves.'
guidance:
  hitl: pro
required_skills:
- acdd
- contract-planning
- code-quality
- test-suite-rules
- agent-instructions
closure:
  status: completed
  closed_at: 2026-11-26T23:17:33.519Z
  target_base: main
  target_base_commit: 50977c6c3b50f04ba5b04c0b741b4e0a2e5e46af
  task_receipts:
    compact-claim-and-read-inline-budget:
      run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
      node_cursor: root:compact-claim-and-read-inline-budget.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ux004-t4-3
      claim_revision: 1
      contract_revision: 5
      candidate_tree: e97707a1aa9e109fe9c59467a1d13c5774fdac52
      committed_oid: d0c9ecd8e7e709b31564ab367ebbef7e83c833d7
      accepted_at: 2026-11-26T22:07:31.975Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    consolidate-mcp-surface-and-wire-analyze:
      run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
      node_cursor: root:consolidate-mcp-surface-and-wire-analyze.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ux004-t3-2
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 90a23b2d2a0d6f5a240a4517a08d60e48848f4d9
      committed_oid: c8178ea65970ae11b3a2a5f1c2829e83201220ef
      accepted_at: 2026-11-26T21:06:45.249Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    git-diff-impact-and-cochange-analyzers:
      run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
      node_cursor: root:git-diff-impact-and-cochange-analyzers.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ux004-t2-6
      claim_revision: 1
      contract_revision: 5
      candidate_tree: 64a5e84301775d85c1daee9d28fc52b5b688508c
      committed_oid: f32192bd54feb955f6c5beb85405b9269d583a62
      accepted_at: 2026-11-26T20:08:50.491Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Deliver independently reviewed candidate.
    index-analyze-graph-and-pagerank-engine:
      run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
      node_cursor: root:index-analyze-graph-and-pagerank-engine.deliver
      gate_id: deliver
      worker_id: codex-sol-reviewer-ux004-t1-4
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 41e7fd57bdebea9f331a11fce789b2fb80a3dc75
      committed_oid: bfda6747a9ab33e7a4554f5739151a95554fb70b
      accepted_at: 2026-11-26T18:29:48.796Z
      rollup:
        verified_invariants:
          applicable: []
          verified:
          - INV-READONLY-OVER-INDEX
          - INV-DETERMINISTIC-PAGERANK
          - INV-SINGLE-OWNER
          - ARCH-INDEX-DEPS
        review_summary: 'Deliver candidate accepted by independent review rev4. Blackboard #546 closed; scoped matrix passed in 0.40s with no temp-directory leaks, and both subtasks have terminal evidence.'
    reconcile-mcp-tool-catalog-docs-and-skills:
      run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
      node_cursor: root:reconcile-mcp-tool-catalog-docs-and-skills.deliver
      gate_id: deliver
      worker_id: codex-terra-reviewer-ux004-t5-2
      claim_revision: 1
      contract_revision: 4
      candidate_tree: a1c1c5490a641048de48a542edf5ebbe0e709cf7
      committed_oid: 57452463da6194ba4dad2a45c06fa606d8f9fe61
      accepted_at: 2026-11-26T22:38:43.214Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
  rollup:
    verified_invariants:
      applicable: []
      verified:
      - INV-READONLY-OVER-INDEX
      - INV-DETERMINISTIC-PAGERANK
      - INV-SINGLE-OWNER
      - ARCH-INDEX-DEPS
    decisions:
    - |-
      UX-004 Architectural Blueprint Decision (v2 — read-only index layer, 20 -> 18 static MCP tools):

      1. Layering & Invariants: Preserves ADR-0001 crate DAG (INV-CORE-LEAF, INV-STATE-NO-INDEX, INV-STATE-BRIDGE-BOUNDARY) with zero new Cargo dependencies and zero SQLite schema version changes. Enforces INV-READONLY-OVER-INDEX: no edits to language extractors (languages/), ast/visitor.rs, linker/, or db/lint/unused_public_api.rs (owned by LS-004).
      2. Read-only Graph & PageRank Engine (Wave 0 — index-analyze-graph-and-pagerank-engine): Adds pure read-only analyzers under crates/acdd-index/src/db/analyze/ (AnalyzeMode enum + hotspots, boundary, path, entrypoints) backed by stored code-map.sqlite facts and a single canonical verified calls-edge test-coverage helper in crates/acdd-index/src/db/symbols/test_discovery.rs (excluding lexical fallback). Deterministic PageRank (sorted node_hash, damping 0.85, bounded work/2s budget) is structured for reuse by AC-007 GET /api/map.
      3. Canonical Diff Impact & Bounded Co-change (Wave 1 — git-diff-impact-and-cochange-analyzers): Extends canonical crates/acdd-index/src/db/traversal/change_impact.rs (INV-SINGLE-OWNER-DIFF-IMPACT) with declaration-line-overlap signature_touched (zero cat-file blob reparsing), verified calls-edge uncovered_symbols, governed INV-* affected_docs, and optional base ref. Adds bounded Git history collection exclusively in crates/acdd-core/src/git.rs (INV-GIT-SINGLE-OWNER), a shared WorkspacePathPolicy + sensitive-path filter in crates/acdd-index/src/db/source.rs (reusing existing ignore crate in acdd-index), cochange analyzer with history+policy continuation fingerprinting, and compact Markdown presentation in src/format/.
      4. MCP Surface Consolidation & Analyze Wiring (Wave 2 — consolidate-mcp-surface-and-wire-analyze): Folds code_map_prove_removal and code_map_tests into code_map_query via GraphQueryOp::{Tests, ProveRemoval} (zero new fields on Query, 1:1 Markdown/JSON parity via src/format/traversal.rs). Drops static MCP tools 20 -> 18 in sync across Server::tool_router() and crates/acdd-core/src/ports.rs::BUILTIN_MCP_TOOLS while keeping retired tool names reserved against plugin registration. Removes out-of-workflow Roadmap/Draft actions and their dead fields from McpStateAction/StateInput, adds fail-closed redirect hints in src/mcp/tools/dispatch.rs, wires strictly 3 optional fields (mode, to, base) on Analyze and CLI QueryCommand::Analyze with WorkspacePathPolicy bridged via src/state_bridge/mod.rs, and syncs embedded profiles, reader/guide hints, and benchmark profiles.
      5. Active Documentation & Skills Reconciliation (Wave 3 — reconcile-mcp-tool-catalog-docs-and-skills): Updates active routers, ADRs, architecture/reference/runbook docs, skills/acdd/, ROADMAP.md, and CHANGELOG.md and verifies via acdd docs check, acdd validate, acdd lint, and scoped test suites.
    - |-
      UX-004 Architectural Blueprint Decision (v2.1 — resolving blueprint_synthesis.review finding #529):

      Supersedes decision #528 with two targeted architectural seam resolutions while keeping Waves 0–3 strictly seam-level and preserving INV-READONLY-OVER-INDEX (zero extractor/linker/lint edits):

      1. Canonical `signature_touched` header-span seam (AC-02, Wave 1):
         - `nodes.end_line` records the full syntax node body end and MUST NOT be treated as the declaration header end.
         - `crates/acdd-index/src/db/traversal/change_impact.rs` already loads and digest-verifies the changed file's postimage bytes (or reads verified indexed source via `db/source.rs::VerifiedSourceCache` for base-ref comparisons) with zero `git cat-file` blob reparsing and zero extractor changes.
         - Within `nodes.line..=nodes.end_line`, `change_impact.rs` bounds each symbol's declaration header span from indexed `nodes.line` to the declaration header end in the already-loaded file lines so declaration-header hunks (single- or multi-line) set `signature_touched: true` while body-only hunks strictly after the header end set `signature_touched: false`.
         - Wave 1 `tests/query_context/` verification seam explicitly tests both negative (body-only edit -> `signature_touched: false`) and positive (declaration-header edit -> `signature_touched: true`) cases.

      2. Single-owner sensitive-path matcher delivered atomically in Wave 1 (INV-SINGLE-OWNER):
         - Admitted `src/engine/reader.rs` into Wave 1 (`git-diff-impact-and-cochange-analyzers`, `contract_revision: 2` via `acdd_state(action="extend_scope")`) alongside `crates/acdd-index/src/db/source.rs`.
         - Wave 1 introduces the canonical sensitive-path + `WorkspacePathPolicy` filter in `crates/acdd-index/src/db/source.rs` (preserving existing worktree and volume case-insensitivity semantics) AND migrates `src/engine/reader.rs` to consume it in the exact same task — zero temporary duplicate sensitive matchers between Wave 1 and Wave 2.
         - Wave 2 (`consolidate-mcp-surface-and-wire-analyze`, sequential after Wave 1) touches `src/engine/reader.rs` solely to update the active-gate hint from `code_map_tests` to `code_map_query(operation="tests")`.

      All other Wave 0–3 seams from decision #528 (read-only `db/analyze/` PageRank/boundary/path/entrypoints, shared verified calls-edge test-coverage helper, bounded `acdd-core::git` cochange, `20 -> 18` static MCP tools synced with `ports.rs` + retired-name reservation, active-run-only `acdd_state` + fail-closed redirects, `Analyze` +3 fields with `state_bridge` policy wiring, and Wave 3 docs/skills reconciliation) remain unchanged.
    - |-
      UX-004 Architectural Blueprint Decision (v2.2 — resolving blueprint_synthesis.review finding #531):

      Supersedes decision #530 solely on the Wave 1 `signature_touched` seam to match the reconciled and synced spec (`docs/milestones/UX-004-mcp-tool-surface-consolidation.md`, `INV-SINGLE-OWNER-DIFF-IMPACT` and `AC-02`):

      - Canonical `signature_touched` seam (`AC-02`, Wave 1 — `crates/acdd-index/src/db/traversal/change_impact.rs`):
        - `signature_touched` is computed strictly from indexed declaration start-line overlap (`nodes.line` falling within a changed Git hunk range) — 100% pure read-only over already-indexed `code-map.sqlite` facts (`INV-READONLY-OVER-INDEX`).
        - Zero downstream source syntax scanning, zero heuristic header parsing over postimage/source lines, zero `git cat-file` blob reparsing, and zero AST extractor edits (`ast/visitor.rs` / `languages/` remain untouched for `LS-004`).
        - Existing verified source admission and hunk mapping stay owned by `change_impact.rs`.
        - Wave 1 `tests/query_context/` verification matrix explicitly proves both cases: body-only edits on lines strictly greater than `nodes.line` (`> nodes.line`) yield `signature_touched: false`, while edits overlapping indexed `nodes.line` yield `signature_touched: true`.

      All other Wave 0–3 seams from decision #530 (including atomic Wave 1 single-owner sensitive-path matcher migration into `src/engine/reader.rs` admitted at `contract_revision: 2`, pure read-only `db/analyze/` PageRank/boundary/path/entrypoints, shared verified calls-edge test-coverage helper, bounded `acdd-core::git` cochange, `20 -> 18` static MCP tools + retired-name reservation, active-run-only `acdd_state` + fail-closed redirects, `Analyze` +3 fields with `state_bridge` policy wiring, and Wave 3 docs/skills reconciliation) remain unchanged.
    - '{"kind":"budget_override","decision":"BUDGET_OVERRIDE","approved_max_net_loc":1250,"measured_net_loc":1219,"original_max_net_loc":500,"blocker_id":534,"rationale":"Audited crates/acdd-index/src/db/analyze/{mod,common,hotspots,boundary,entrypoints,path}.rs and db/symbols/test_discovery.rs: all four pure read-only graph analyzers (deterministic PageRank hotspots reusable by AC-007, boundary, bounded top-K path, reverse entrypoints) plus batched frontier loaders and shared verified calls-edge coverage are cohesive, clone-free (0 duplicates, 0 dead_tails, 0 lint violations), strictly within admitted scope (INV-READONLY-OVER-INDEX), and verified green via acdd verify."}'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"index-analyze-graph-and-pagerank-engine","supersedes_decision":535,"resolves_blocker":544,"approved_max_net_loc":1400,"measured_prod_net_loc":1383,"rationale":"Independent coordinator audit of rev3 remediation for #542 in crates/acdd-index/src/db/paging.rs (InterruptHandle watchdog + connection-keyed ACTIVE_BUDGETS preserving caller SQLite progress_handler) and db/analyze/{mod,common,hotspots,path,entrypoints}.rs (+142 net prod LOC over rev2''s +1,241) confirmed zero duplicates, zero dead_tails, zero lint errors across 60 rules, and airtight closure of all 5 review probes."}'
    - '{"kind":"escalation","task":"git-diff-impact-and-cochange-analyzers","worker":"builder-ux004-t2","measured_prod_net_loc":1143,"original_max_net_loc":450,"requested_budget_override":1200,"issues":[{"id":"acyclicity_vs_unused_public_api","summary":"ARCH-ACYCLIC-WORKSPACE-MODULES forbids the 3-cycle db/reader/mod.rs -> db/analyze/mod.rs -> db/traversal/change_impact.rs -> db/reader/mod.rs. Breaking db/reader/mod.rs -> db/analyze yields 0 arch/syntax/docs lint violations across 60 rules, but code.lint (dead_tails.unused_public_api) then fails on db/analyze/mod.rs (AnalyzeRequest, AnalyzeContext, analyze_with_mode, analyze_with_context — 4 errors) because the only production callers above db/analyze (src/cli/dispatch_code.rs and src/mcp/tools/code_tools.rs) sit in Wave 2 Negative Boundaries.","recommended_resolution":"Approve admitting a 1-line delegation in src/cli/dispatch_code.rs (QueryCommand::Analyze calling analyze::analyze_with_mode with AnalyzeMode::Summary, zero new CLI flags or MCP schema edits) and folding AnalyzeContext into AnalyzeRequest so both ARCH-ACYCLIC-WORKSPACE-MODULES and dead_tails.unused_public_api pass cleanly with zero shims."},{"id":"budget_override","summary":"Clean, clone-free implementation across 5 production subsystems (git.rs +158, db/source.rs + reader.rs migration +73 net, change_impact.rs +287, cochange.rs + analyze/mod.rs + db/reader/mod.rs +272 net, src/format/ 6-mode Markdown renderers +353) totals +1,143 net prod LOC vs original 450 budget. Verified green: query_context analyze matrices (2/2), reader sensitive_policy (1/1), format tests (3/3), mcp graph_and_impact (1/1), commitment_integrity (9/9), strict workspace clippy (0 warnings), 0 duplicates, 0 dead_tails/dangling_calls, 0 /tmp leaks (BEFORE=15 AFTER=15), 0 arch/syntax/docs lint errors across 60 rules."}]}'
    - '{"decision_type":"BUDGET_OVERRIDE_AND_SEAM_RULING","task":"git-diff-impact-and-cochange-analyzers","resolves_escalation":555,"approved_max_net_loc":1200,"measured_prod_net_loc":1143,"cycle_resolution":"Keep src/cli/ and src/mcp/ strictly untouched in Wave 1 (preserving Wave 1 Negative Boundaries). Break the db/reader/mod.rs -> db/analyze/mod.rs -> db/traversal/change_impact.rs -> db/reader/mod.rs cycle inside crates/acdd-index/src/db/ by replacing reader::rows in change_impact.rs with paging::rows (already imported) and referencing strip_selector_uri_prefix without importing db::reader::mod (via db::reader::selectors or moving that 5-line pure helper to db::paging/db::source re-exported by selectors), folding AnalyzeContext into AnalyzeRequest, and keeping db::reader::analyze_paged delegating to analyze::analyze_with_mode.","rationale":"Independent coordinator audit of git.rs (+158), db/source.rs + atomic reader.rs migration (+73), change_impact.rs (+287), cochange.rs/analyze (+272), and src/format/ 6-mode Markdown renderers (+353) confirmed 0 duplicates, 0 dead_tails, strict Clippy green, and 9/9 commitment_integrity passing. Original 450 LOC budget underestimated five cohesive subsystems plus all 6 Markdown formatters."}'
    - '{"kind":"budget_override_request","task":"git-diff-impact-and-cochange-analyzers","worker":"builder-ux004-t2","supersedes_decision":556,"resolves_finding":558,"measured_prod_net_loc":1318,"previous_approved_max_net_loc":1200,"requested_max_net_loc":1350,"delta_over_rev1":176,"delta_breakdown":{"crates/acdd-core/src/git.rs":"+73 (ancestor-only repo rejection via git_toplevel_exact, pinned GitLogSnapshot base_oid, streaming bounded run_git_stdout_bounded with kill/reap on overflow)","crates/acdd-index/src/db/source.rs":"+22 (unconditional internal .acdd/.acdd/** exclusion + disallowed_path_ids)","crates/acdd-index/src/db/traversal/change_impact.rs":"+66 (canonical PathExposureFilter + disallowed_path_ids across all graph-derived nodes/tests/affected_docs with exact post-filter pagination totals)","crates/acdd-index/src/db/analyze/cochange.rs":"+9 (pin, fingerprint, and fence resolved base_oid)","src/format/analyze.rs":"+6 (rustfmt)"},"verification":{"execution_digest":"1156935f05a87ae23e24932588da48f0e6651afe0e81b1bd33965834838bddd0","exit_code":0,"code_seam":"passed","code_lint":"0 errors, 0 warnings across 60 rules","duplicates":0,"dead_tails":0,"test_runtime_s":0.43,"tmp_leaks":"BEFORE=15 AFTER=15"}}'
    - '{"decision_type":"BUDGET_OVERRIDE","task":"git-diff-impact-and-cochange-analyzers","supersedes_decision":556,"approved_max_net_loc":1350,"measured_prod_net_loc":1318,"rationale":"Independent coordinator audit of rev2 remediation for all 4 findings in #558 (streaming 8 KiB bounded git stdout with subprocess kill/reap in git.rs +73, exact toplevel isolation, .acdd/** and graph-level disallowed_path_ids filtering with exact post-filter pagination in source.rs/change_impact.rs +88, and pinned GitLogSnapshot base_oid continuation fencing in cochange.rs +15) confirmed 0 duplicates, 0 dead_tails, 0 lint errors/warnings across 60 rules, 0.43s matrix runtime, and zero src/cli or src/mcp edits."}'
    - '{"decision_type":"USER_APPROVED_SCOPE_AND_PROTECTED_PATH","task":"compact-claim-and-read-inline-budget","user_approved_protected_paths":[".acdd/profile.yaml"],"summary":"User explicitly approved adding Wave 3 task compact-claim-and-read-inline-budget (AC-06) and Wave 4 .agents/read_file_guard.py removal to UX-004 via acdd sync.","seams":{"claim_budget":"Lower CLAIM_DEFAULT_MAX_BYTES to 9_500 bytes (adding optional detail=''full'' for 32 KiB on acdd_claim), filter generated/derive methods from Scoped Symbols, strip headings TOC spam from Repository Instructions (keep file paths), deduplicate paths in Scoped Tests (top 8), cap External Callers to top-5 + overflow count, bound Blackboard to latest 6 x 220 chars while keeping Unresolved Findings full, and drop .acdd/profile.yaml duplicate code.tools override (keep short universal code.tools in code.yaml; do NOT weaken code.principles/core.*/task.*/repo.tests).","read_inline_cap":"Default <= 9,000-byte inline cap on acdd_read line windows in src/engine/reader.rs with exact next_start_line continuation.","git_mv_commit_fix":"Include real worktree index entries (list_stage_entries(&req.worktree, None)) in crates/acdd-state/src/git_ops/commit.rs::create_scoped_commit_with_expected so git-mv staged renames never drop during scoped delivery commits."}}'
    - |-
      UX-004 Architectural Blueprint Decision (v2.3 — rev 4 adding Wave 3 AC-06 and Wave 4 .agents/ guard removal per Coordinator Decision #572):

      Supersedes decision #532 by preserving Waves 0–2 slices intact (Waves 0–1 already Completed) and adding Wave 3 (`compact-claim-and-read-inline-budget`, `AC-06`) plus updating Wave 4 (`reconcile-mcp-tool-catalog-docs-and-skills`, `AC-05`):

      1. Wave 3 (`compact-claim-and-read-inline-budget`, `AC-06`, user-authorized protected path `.acdd/profile.yaml` via Coordinator Decision #572):
         - **ClaimResp inline budget & `detail` seam (`INV-INLINE-CLAIM-AND-READ-BUDGET`)**: Lower default `CLAIM_DEFAULT_MAX_BYTES` in `crates/acdd-state/src/context/budget.rs` to `9_500` bytes while retaining a `32 KiB` ceiling when optional `detail="full"` is passed via `ClaimInput` (`src/mcp/tools/`) -> `ClaimReq` (`crates/acdd-state/src/usecases/dto.rs`) -> `SnapshotContextInput` (`crates/acdd-state/src/context/` & `usecases/{commands/claim,queries}.rs`). Keep Tier 2 `unresolved_findings` intact while bounding Tier 5 blackboard to latest `6 x 220` chars (`MAX_BLACKBOARD_ITEMS = 6`, `MAX_BLACKBOARD_CHARS = 220`).
         - **Canonical lens compaction (`INV-SINGLE-OWNER`)**: Strip headings TOC spam at the canonical producer in `src/state_bridge/docs_domain.rs::provide_instructions` (retaining mandatory instruction file paths), filter generated/derive methods (e.g. `.ne`/`.le`/`.ge`) from `code.symbols`, deduplicate paths in `code.tests` (capped at top 8), and cap `code.external_callers` to top-5 plus overflow count in `src/state_bridge/{common.rs,code_domain/context.rs}` and `crates/acdd-state/src/context/markdown.rs`. Remove the duplicate `code.tools` override in user-approved `.acdd/profile.yaml` (preserving the concise universal `code.tools` in `crates/acdd-state/src/profile/embedded/code.yaml` and keeping `code.principles`/`core.*`/`task.*`/`repo.tests` intact).
         - **`acdd_read` inline cap seam**: Enforce default `<= 9,000`-byte inline cap on line windows in `src/engine/reader.rs` with exact `next_start_line` continuation metadata surfaced cleanly by `src/format/presentation.rs::render_acdd_read`, preserving `output_file` exports and snippet `source_offset` continuations.
         - **Scoped commit staged-rename fix (`INV-SCOPED-COMMIT-STAGED-RENAMES`)**: Include real worktree index entries (`list_stage_entries(&req.worktree, None)`) alongside `base_head` and untracked paths in `crates/acdd-state/src/git_ops/commit.rs::create_scoped_commit_with_expected` so `git mv` / staged additions never drop or fail with `TASK_CANDIDATE_MISMATCH`.
         - Extended Wave 3 admitted scope (`contract_revision: 3`) with the exact canonical owners (`crates/acdd-state/src/usecases/{dto,commands/claim,queries,run,tests}.rs`, `src/state_bridge/{docs_domain,common}.rs`, `src/cli/dispatch_state.rs`, `src/format/presentation.rs`) to avoid shadow layers or build breaks.

      2. Wave 4 (`reconcile-mcp-tool-catalog-docs-and-skills`, `AC-05`):
         - Depends on `compact-claim-and-read-inline-budget`. Removes the hard `.agents/read_file_guard.py` / `.agents/hooks.json` `read_file` block, updates active routers/docs/ADRs/skills/ROADMAP/CHANGELOG with pragmatic ACDD-first navigation guidance plus the `18`-tool MCP surface, active-run-only `acdd_state`, `code_map_analyze` modes, and `<= 9 KiB` inline `acdd_claim`/`acdd_read` behavior, and passes all doc/lint/integration checks.
    - |-
      UX-004 Architectural Blueprint Decision (v2.4 — rev 5 closing blueprint_synthesis.review finding #578):

      Supersedes decision #577 solely to align all inline budget wording with the synced run contract (`AC-01..AC-06` verified present in `ClaimResp.contract` and `docs/milestones/UX-004-mcp-tool-surface-consolidation.md`):
      - Exact inline budget figures across Wave 3 (`AC-06`), Wave 4 (`AC-05`), and all documentation/ADR updates: `acdd_claim` default `<= 9,500 bytes` (opt-in `detail="full"` up to `32 KiB`) and `acdd_read` default `<= 9,000 bytes` with exact `next_start_line` continuation (replacing approximate `<= 9 KiB` wording everywhere).
      - All architectural seams, canonical owners, Wave 0–4 slices, Wave 3 scope extensions (`contract_revision: 3`), and user-authorized `.acdd/profile.yaml` `code.tools` deduplication (Coordinator Decision #572) remain identical to decision #577.
  metrics_summary:
    decisions_count: 12
    deferred_findings_count: 0
    failed_traces: 5
    passed_traces: 21
    task_receipts_count: 5
    tasks_cancelled: 0
    tasks_completed: 5
    tasks_total: 5
    total_claims: 57
    total_review_cycles: 6
    total_traces: 30
---

# UX-004: Lean MCP surface (20 → 18 tools, active-run-only `acdd_state`) and read-only `code_map_analyze` diagnostics

## Goal & Non-goals

**Goal.** Shrink static MCP schema overhead and give active coding, review, and planning agents single-call structural diagnostics strictly as a **pure read-only layer over already-indexed facts**:
1. **Lean MCP surface (20 → 18 static tools, zero new tools):** Fold `code_map_prove_removal` and `code_map_tests` into `code_map_query(operation="prove_removal" | "tests")` reusing existing fields (`0` new schema properties on `code_map_query`), and remove out-of-workflow `roadmap` and `draft` actions from MCP `acdd_state` (keeping `acdd_state` strictly for active runs while CLI `acdd roadmap` / `acdd draft` stay first-class).
2. **Read-only `code_map_analyze` modes:** Extend existing `code_map_analyze` with 3 compact optional fields (`mode`, `to`, `base`) delivering deterministic PageRank `hotspots` (reusable by [AC-007](../AC-007-unified-observability-board-otlp-and-metrics.md) `GET /api/map`), `boundary`, `path`, `entrypoints`, canonical `diff_impact` (reusing `change_impact.rs` with header-line `signature_touched` and zero `cat-file` blob reparsing), and bounded Git `cochange`.

**Non-goals.**
- Modifying Rust/language AST extractors, cross-file Clap/rmcp linkers, or inline `mod` export publication (owned by [LS-004](LS-004-rust-entrypoints-and-inline-module-exports.md)).
- Adding OID-pinned `git cat-file` blob streaming or quadratic `--find-copies-harder` scans.
- Adding any `acdd_spec` MCP tool or new third-party dependencies in `Cargo.toml`.
- Changing SQLite `code-map.sqlite` schema version or altering CLI `acdd query remove` / `acdd query tests` / `acdd roadmap` / `acdd draft`.

## Architecture & ownership map

### 1. MCP surface consolidation (`20 -> 18` static tools)

| Removed MCP surface | Replacement | Consumers updated | CLI parity |
|---|---|---|---|
| `code_map_prove_removal` | `code_map_query(operation="prove_removal", selector=..., path=...)` | `crates/acdd-core/src/ports.rs`, `src/mcp/tools/`, `crates/acdd-index/src/db/traversal/`, `crates/acdd-state/src/profile/embedded/`, `src/engine/guide.rs`, `benchmarks/profiles/`, `tests/mcp/` | `acdd query remove` (unchanged) |
| `code_map_tests` | `code_map_query(operation="tests", selector=..., path=...)` | Same + `src/engine/reader.rs` active-gate hint + `tests/query_context/ranking_and_coverage.rs` | `acdd query tests` (unchanged) |
| `acdd_state(action: roadmap \| draft)` | CLI `acdd roadmap` / `acdd draft` (fail-closed redirect hint) | `src/mcp/tools/{inputs,state_tools,dispatch}.rs`, `crates/acdd-state/src/profile/embedded/spec_planning.yaml`, `tests/mcp/`, docs | `acdd roadmap`, `acdd draft` (unchanged) |
| `code_map_analyze` *(extended)* | `+3` optional fields (`mode`, `to`, `base`), default `summary` unchanged | `src/mcp/tools/{inputs,code_tools}.rs`, `src/cli/{args,dispatch_code}.rs`, `src/format/` | `acdd query analyze --mode` |

### 2. Read-only `code_map_analyze` diagnostic modes

| `mode` | Canonical owner | Existing data reused (zero extractor/schema changes) |
|---|---|---|
| `hotspots` | `crates/acdd-index/src/db/analyze/hotspots.rs` | `nodes`, `edges`, `calls` test coverage (`db/symbols/test_discovery.rs`), deterministic PageRank (`damping = 0.85`) |
| `boundary` | `crates/acdd-index/src/db/analyze/boundary.rs` | `nodes`, `edges`, `path_dictionary`, `dependencies` (`is_public` with 0 external callers) |
| `path` | `crates/acdd-index/src/db/analyze/path.rs` | `nodes`, `edges`, `edge_occurrences` |
| `entrypoints` | `crates/acdd-index/src/db/analyze/entrypoints.rs` | Reverse `edges`, stored `nodes.details['entrypoint']` (auto-enriched later by `LS-004`), uncalled public root symbols |
| `diff_impact` | `crates/acdd-index/src/db/traversal/change_impact.rs` | Existing Git hunk ranges + indexed `nodes.line` (`signature_touched` strictly by declaration start-line overlap) + `doc_sections` (`INV-*`) + verified `calls` `test_case` reachability (`uncovered_symbols`) |
| `cochange` | `crates/acdd-index/src/db/analyze/cochange.rs` + `crates/acdd-core/src/git.rs` | Bounded `git log --no-merges --name-status -z --find-renames` + `edges` difference + `WorkspacePathPolicy` + shared sensitive-path matcher |

## Tasks in this milestone

### task: index-analyze-graph-and-pagerank-engine

```yaml
task_ref: index-analyze-graph-and-pagerank-engine
target: Implement pure read-only graph analyzers (deterministic PageRank hotspots, boundary, path, and entrypoints) under crates/acdd-index/src/db/analyze/ plus shared verified calls test-coverage helper in db/symbols/test_discovery.rs within the 2-second query budget, with contract tests in tests/query_context/
scope:
- crates/acdd-index/src/db/analyze/
- crates/acdd-index/src/db/mod.rs
- crates/acdd-index/src/db/paging.rs
- crates/acdd-index/src/db/reader/
- crates/acdd-index/src/db/symbols/
- tests/query_context/
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: pagerank-hotspots-and-boundary
  title: Add shared verified calls-edge test-coverage helper in db/symbols/test_discovery.rs, implement deterministic PageRank (sorted node_hash, damping 0.85, bounded work/clock checks) hotspots and pure graph boundary analyzer (inbound callers, grouped outbound deps, pub symbols with 0 external callers)
  status: completed
  evidence: 'Remediated #537 and #542: shared verified calls-edge coverage helper in db/symbols/test_discovery.rs; connection-keyed nested QueryBudget using rusqlite InterruptHandle watchdog in db/paging.rs (preserving caller SQLite progress_handler + enforcing post-SQL/empty-graph deadlines in pagerank_centrality, compute_pagerank, load_nodes, fetch_frontier_edges, analyze_with_mode); single-load deterministic PageRank hotspots with eligible-only budgets; pure graph boundary with canonical calls callers and distinct external vs unresolved deps. Verified via `acdd verify` (rev3 digest 7eb9e81c773d0b081d025222216dfbd1736b55da092ff7ccc2a83768f3799987, exit_code=0, code.seam=pass, code.lint=0 errors/0 warnings, duplicates=0, dead_tails=0, TMP_LEAKS BEFORE=15 AFTER=15) and tests/query_context/diagnostics_and_source.rs::analyze_graph_and_pagerank_modes_contract_matrix.'
- subtask_ref: shortest-path-and-entrypoints
  title: Implement bounded shortest/top-K path tracer (failing closed when destination is missing) and reverse entrypoints analyzer over stored nodes.details.entrypoint plus uncalled public roots, with tests in tests/query_context/
  status: completed
  evidence: 'Remediated #537, #542, and #546: for k==1, path.rs records deterministic first-seen incoming predecessor edges during forward BFS, stops immediately when to_hash is reached (before deeper unrelated expansion), and reconstructs the shortest chain in O(d) (proven on S->T, S->B->20,000 leaves); k>1 retains reverse-distance-pruned ascending-remaining-distance top-K across max_depth=12; entrypoints.rs fails closed on >32-hop frontier exhaustion and enforces NodeSummary.is_prod on selected depth-0 roots. Dense 150x150 and 20k-leaf fixtures batched via set-oriented SQLite CTEs (focused matrix runs in 0.41s < 1s). Verified via `acdd verify` (rev5 digest c35508e4fb26c15d327d8b352b586536c6a6152333e50e64cf3c2b7a5eea4bae, exit_code=0, code.seam=pass, code.lint=0 errors/0 warnings, duplicates=0, dead_tails=0, TMP_LEAKS BEFORE=15 AFTER=15, +1,391 <= 1,400 net LOC).'
receipt:
  run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
  node_cursor: root:index-analyze-graph-and-pagerank-engine.deliver
  gate_id: deliver
  worker_id: codex-sol-reviewer-ux004-t1-4
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 41e7fd57bdebea9f331a11fce789b2fb80a3dc75
  committed_oid: bfda6747a9ab33e7a4554f5739151a95554fb70b
  accepted_at: 2026-11-26T18:29:48.796Z
  rollup:
    verified_invariants:
      applicable: []
      verified:
      - INV-READONLY-OVER-INDEX
      - INV-DETERMINISTIC-PAGERANK
      - INV-SINGLE-OWNER
      - ARCH-INDEX-DEPS
    review_summary: 'Deliver candidate accepted by independent review rev4. Blackboard #546 closed; scoped matrix passed in 0.40s with no temp-directory leaks, and both subtasks have terminal evidence.'
```

- **Wave**: `0` | **Budget**: `~500` net LOC.
- **Seam & mutation proof (`AC-01`)**: `tests/query_context/` proves deterministic PageRank ordering and budget bounds, exact `boundary` inbound/outbound/internal-only `pub` counts, `path` shortest edge chain resolution + missing-`to` rejection, and `entrypoints` root tracing over stored entrypoint categories.

### task: git-diff-impact-and-cochange-analyzers

```yaml
task_ref: git-diff-impact-and-cochange-analyzers
target: Extend canonical change_impact.rs with signature_touched (strictly indexed nodes.line overlap, zero downstream syntax scanning or cat-file reparsing), verified uncovered_symbols, governed INV-* affected_docs, and optional base ref, implement bounded cochange in crates/acdd-index/src/db/analyze/cochange.rs via crates/acdd-core/src/git.rs + WorkspacePathPolicy, and add compact Markdown formatters in src/format/
scope:
- crates/acdd-core/src/git.rs
- crates/acdd-index/src/db/source.rs
- crates/acdd-index/src/db/traversal/change_impact.rs
- crates/acdd-index/src/db/traversal/mod.rs
- crates/acdd-index/src/db/analyze/
- src/format/
- tests/query_context/
extended_scope:
- path: src/engine/reader.rs
  reason: 'Admit src/engine/reader.rs into Wave 1 (git-diff-impact-and-cochange-analyzers) per blueprint review finding #529 so the canonical sensitive-path matcher in crates/acdd-index/src/db/source.rs replaces the host-private matcher in the same delivery with zero temporary duplication (INV-SINGLE-OWNER).'
- path: crates/acdd-index/src/db/mod.rs
  reason: Expose pub mod source in crates/acdd-index/src/db/mod.rs (within admitted crates/acdd-index/src/db/ owner root) so src/engine/reader.rs consumes the single canonical sensitive-path matcher in crates/acdd-index/src/db/source.rs with zero shims (INV-SINGLE-OWNER).
- path: crates/acdd-index/src/db/reader/mod.rs
  reason: Break ARCH-ACYCLIC-WORKSPACE-MODULES cycle (db/reader/mod.rs -> db/analyze -> db/traversal -> db/reader) within admitted crates/acdd-index/src/db/ owner root by having reader::analyze_paged call sibling diagnostics::analyze_summary_paged directly.
- path: crates/acdd-index/src/db/traversal/types.rs
  reason: 'Per coordinator Decision #556, break ARCH-ACYCLIC-WORKSPACE-MODULES inside crates/acdd-index/src/db/ (admitted owner root) by switching traversal/types.rs from reader facade to reader::selectors::strip_selector_uri_prefix and moving predecessor_endpoints/reverse_predecessor_union from traversal/mod.rs to traversal/types.rs so change_impact.rs has zero import edge to reader/mod.rs or traversal/mod.rs.'
depends_on:
- index-analyze-graph-and-pagerank-engine
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: canonical-diff-impact-enrichment-and-cochange
  title: Add bounded git log --name-status -z --find-renames helper in crates/acdd-core/src/git.rs and shared sensitive/policy path filter in db/source.rs; extend change_impact.rs with signature_touched, verified uncovered_symbols (excluding lexical fallback), governed INV-* affected_docs, and base ref; implement cochange with history+policy continuation fingerprint
  status: completed
  evidence: 'Rev7 closure of #570: updated cochange.rs loop to collect and apply rename_to_current mappings (via commit_renames) for every collected commit including oversized >30-file commits before skipping pair/file-count accumulation for is_large commits. Verified green: acdd verify rev7 execution_digest a47eeaa5841bc213920f229900939b01e6f7b747aac88297c42d71a7b6e9ea88 (code.lint 0 errors/0 warnings across 60 rules, 0 duplicates, 0 dead_tails, net prod LOC +1,320 <= Decision #560 1,350).'
- subtask_ref: analyze-markdown-formatters-and-tests
  title: Add compact Markdown presentation for all 6 code_map_analyze modes in src/format/ and verify diff_impact, cochange, sensitive/DB-path exclusion, continuation invalidation, and non-Git fail-closed behavior in tests/query_context/
  status: completed
  evidence: 'Rev7 closure of #570: added a >30-file mass-rename commit (renaming pkg/unlinked_b.py -> pkg/unlinked_b_mass.py and pkg/caller.py -> pkg/caller_mass.py alongside 30 bulk files) inside existing analyze_diff_impact_cochange_and_markdown_contract_matrix proving commits_skipped_large >= 1, historical unlinked pair resolves to (''pkg/unlinked_a.py'', ''pkg/unlinked_b_mass.py'') with zero stale pre-rename path, and statically linked (''pkg/caller_mass.py'', ''pkg/service.py'') remains subtracted (0.69s steady-state runtime < 1s, BEFORE=15 AFTER=15 /tmp leak check, acdd verify rev7 execution_digest a47eeaa5841bc213920f229900939b01e6f7b747aac88297c42d71a7b6e9ea88).'
receipt:
  run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
  node_cursor: root:git-diff-impact-and-cochange-analyzers.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ux004-t2-6
  claim_revision: 1
  contract_revision: 5
  candidate_tree: 64a5e84301775d85c1daee9d28fc52b5b688508c
  committed_oid: f32192bd54feb955f6c5beb85405b9269d583a62
  accepted_at: 2026-11-26T20:08:50.491Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Deliver independently reviewed candidate.
```

- **Wave**: `1` | **Budget**: `~450` net LOC.
- **Seam & mutation proof (`AC-02`)**: `tests/query_context/` proves `diff_impact` delegates through `change_impact.rs` and reports changed symbols, `signature_touched`, governed `INV-*` docs, reaching `test_case` nodes, and verified `uncovered_symbols` (excluding lexical fallback); `cochange` surfaces unlinked co-changed files while excluding sensitive/policy paths and failing closed outside Git repos.

### task: consolidate-mcp-surface-and-wire-analyze

```yaml
task_ref: consolidate-mcp-surface-and-wire-analyze
target: Fold code_map_prove_removal and code_map_tests into code_map_query (20 -> 18 static MCP tools synced with ports.rs), remove out-of-workflow roadmap and draft actions from MCP acdd_state with fail-closed CLI redirect hints, update embedded profiles, reader/guide hints, and benchmark profiles, wire code_map_analyze mode/to/base across MCP and CLI, and update test suites
scope:
- crates/acdd-core/src/ports.rs
- crates/acdd-core/src/subsystems.rs
- crates/acdd-index/src/db/traversal/types.rs
- crates/acdd-index/src/db/traversal/mod.rs
- crates/acdd-state/src/profile/embedded/
- src/engine/guide.rs
- src/engine/reader.rs
- src/state_bridge/mod.rs
- src/format/
- src/cli/
- src/mcp/tools/
- src/mcp/server/
- benchmarks/profiles/
- tests/mcp/
- tests/query_context/
- tests/acdd/
depends_on:
- git-diff-impact-and-cochange-analyzers
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: fold-tools-and-trim-mcp-state-actions
  title: Add GraphQueryOp::{Tests, ProveRemoval}, route operation='prove_removal' and operation='tests' through code_map_query (zero new schema fields), sync BUILTIN_MCP_TOOLS (20 -> 18) and retired-name plugin reservation in ports.rs, unregister code_map_prove_removal and code_map_tests, update embedded profiles + src/engine/{guide,reader}.rs + benchmarks/profiles/, remove roadmap/draft from McpStateAction/StateInput, and fail closed with redirect hints
  status: completed
  evidence: Added GraphQueryOp::{Tests, ProveRemoval} in crates/acdd-index/src/db/traversal/{types,mod}.rs and TraversalKind::Tests dispatch in src/format/traversal.rs (0 new fields on Query); synced BUILTIN_MCP_TOOLS (20 -> 18) and RETIRED_MCP_TOOLS plugin collision reservation in crates/acdd-core/src/ports.rs; unregistered code_map_tests and code_map_prove_removal from Server::tool_router(); updated embedded profiles (audit/code/spec_planning), src/engine/{guide,reader}.rs, and benchmarks/profiles/; trimmed Roadmap/Draft and 8 dead fields from McpStateAction/StateInput with fail-closed CLI redirect hints in src/mcp/tools/dispatch.rs. Verified via ports, traversal_seam_tests, mcp::tools, tests/mcp, tests/query_context, and tests/acdd.
- subtask_ref: wire-analyze-modes-and-verify-mcp-schema
  title: Add optional mode/to/base to Analyze and CLI acdd query analyze (passing resolved WorkspacePathPolicy via src/state_bridge) preserving default summary/include_cycles/lint/SQL compatibility, verify 18 static MCP tools and net tools/list schema byte reduction in tests/mcp/, and confirm CLI parity
  status: completed
  evidence: Added strictly 3 optional fields (mode, to, base) to Analyze in src/mcp/tools/inputs.rs and CLI QueryCommand::Analyze in src/cli/{args,dispatch_code,mod}.rs unified behind canonical state_bridge::dispatch_analyze (resolving WorkspacePathPolicy read-only for diff_impact/cochange and preserving default summary/include_cycles/lint/SQL compatibility). Verified 18 static MCP tools matching BUILTIN_MCP_TOOLS, net tools/list schema byte shrinkage (58,429 -> 53,767 bytes), 1:1 code_map_query tests/prove_removal and CLI analyze/remove/tests/roadmap/draft parity, and 0 /tmp/contextunity-test-* leaks (BEFORE=15 AFTER=15) in ux004_mcp_surface_consolidation_and_analyze_cli_parity_contract.
receipt:
  run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
  node_cursor: root:consolidate-mcp-surface-and-wire-analyze.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ux004-t3-2
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 90a23b2d2a0d6f5a240a4517a08d60e48848f4d9
  committed_oid: c8178ea65970ae11b3a2a5f1c2829e83201220ef
  accepted_at: 2026-11-26T21:06:45.249Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `2` | **Budget**: `~450` net LOC (deletions + dispatch wiring).
- **Seam & mutation proof (`AC-03`, `AC-04`)**: `tests/mcp/`, `tests/query_context/`, and `tests/acdd/` prove `tools/list` returns 18 built-in tools matching `BUILTIN_MCP_TOOLS` with net smaller schema bytes, `code_map_query` serves `prove_removal` and `tests` with full parity, `acdd_state` exposes active-run actions only, removed tool/action calls fail closed with redirect hints, and CLI commands work cleanly.

### task: compact-claim-and-read-inline-budget

```yaml
task_ref: compact-claim-and-read-inline-budget
target: Keep default acdd_claim (<= 9,500 bytes, optional detail='full' for 32 KiB) and acdd_read (<= 9,000 bytes with next_start_line continuation) cleanly inline without losing task framing or engineering guardrails, remove duplicate code.tools override in user-approved .acdd/profile.yaml, and include real worktree index entries in create_scoped_commit_with_expected for git-mv staged renames
scope:
- crates/acdd-state/src/context/
- crates/acdd-state/src/git_ops/commit.rs
- crates/acdd-state/src/profile/embedded/code.yaml
- crates/acdd-state/tests/
- .acdd/profile.yaml
- src/state_bridge/code_domain/
- src/engine/reader.rs
- src/format/lifecycle.rs
- src/mcp/tools/
- tests/mcp/
- tests/acdd/
- tests/query_context/
- docs/milestones/UX-004-mcp-tool-surface-consolidation.md
extended_scope:
- path: crates/acdd-state/src/usecases/dto.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: crates/acdd-state/src/usecases/commands/claim.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: crates/acdd-state/src/usecases/run.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: crates/acdd-state/src/usecases/tests.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: src/state_bridge/docs_domain.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: src/state_bridge/common.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: src/cli/dispatch_state.rs
  reason: 'Admit canonical producers and ClaimReq callers for AC-06: ClaimReq.detail threading (usecases/{dto,commands/claim,run,tests}.rs, src/cli/dispatch_state.rs) and canonical docs.instructions / lens cap owners (src/state_bridge/{docs_domain,common}.rs) so headings TOC stripping and detail=''full'' budget routing happen at their single canonical owners without shadow layers or compilation breaks.'
- path: crates/acdd-state/src/usecases/queries.rs
  reason: Admit remaining SnapshotContextInput caller (crates/acdd-state/src/usecases/queries.rs) and acdd_read Markdown metadata renderer (src/format/presentation.rs) for AC-06 next_start_line presentation and ClaimReq/ContextReq detail routing.
- path: src/format/presentation.rs
  reason: Admit remaining SnapshotContextInput caller (crates/acdd-state/src/usecases/queries.rs) and acdd_read Markdown metadata renderer (src/format/presentation.rs) for AC-06 next_start_line presentation and ClaimReq/ContextReq detail routing.
- path: src/state_bridge/mod.rs
  reason: Update two existing ClaimReq test literals in src/state_bridge/mod.rs after adding ClaimReq.detail for AC-06
- path: src/cli/args.rs
  reason: 'Expose --detail compact|full parity on CLI acdd claim and acdd read per review finding #584'
- path: src/cli/dispatch_code.rs
  reason: 'Expose --detail compact|full parity on CLI acdd claim and acdd read per review finding #584'
depends_on:
- consolidate-mcp-surface-and-wire-analyze
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: compact-claim-lenses-and-profile-dedup
  title: Lower default ClaimResp budget to <= 9,500 bytes (with optional detail='full' for 32 KiB), filter generated/derive methods from Scoped Symbols, strip headings TOC spam from Repository Instructions, deduplicate paths in Scoped Tests and cap External Callers to top-5 + overflow count, bound Blackboard to latest 6 x 220 chars while keeping Unresolved Findings full, and drop .acdd/profile.yaml duplicate code.tools override (preserving code.principles/core.*/task.*/repo.tests)
  status: completed
  evidence: 'Candidate e97707a1: compact_claim_budget_detail_and_lens_compaction_matrix and context tests passed; authorized profile diff reviewed; build and independent review accepted.'
- subtask_ref: read-inline-cap-and-git-mv-commit-fix
  title: Enforce default <= 9,000-byte inline cap with exact next_start_line continuation in acdd_read (src/engine/reader.rs), include real worktree index entries (list_stage_entries(&req.worktree, None)) in crates/acdd-state/src/git_ops/commit.rs::create_scoped_commit_with_expected for git-mv staged renames, and prove all contracts in tests
  status: completed
  evidence: 'Candidate e97707a1: MCP test_read matrix passed including inline budgets, continuation, 700 paths and CLI/MCP exports; scoped_commit_plumbing_and_archive_matrix passed; build and independent review accepted.'
receipt:
  run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
  node_cursor: root:compact-claim-and-read-inline-budget.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ux004-t4-3
  claim_revision: 1
  contract_revision: 5
  candidate_tree: e97707a1aa9e109fe9c59467a1d13c5774fdac52
  committed_oid: d0c9ecd8e7e709b31564ab367ebbef7e83c833d7
  accepted_at: 2026-11-26T22:07:31.975Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `3` | **Budget**: `~320` net LOC (user-approved `protected_path` edit to `.acdd/profile.yaml` for `code.tools` dedup only).
- **Seam & mutation proof (`AC-06`)**: `crates/acdd-state/src/context/tests.rs`, `tests/acdd/`, `tests/mcp/`, and `tests/query_context/` prove default `acdd_claim` stays `<= 9,500` bytes with clean non-derived symbols/tests/callers and `detail="full"` returns up to `32 KiB`, `acdd_read` caps default line reads at `<= 9,000` bytes with exact `next_start_line` continuation, and `create_scoped_commit_with_expected` commits `git mv`-staged renames without `TASK_CANDIDATE_MISMATCH`.

### task: reconcile-mcp-tool-catalog-docs-and-skills

```yaml
task_ref: reconcile-mcp-tool-catalog-docs-and-skills
target: Update active AGENTS.md/src/AGENTS.md, ADRs, README/INSTALL, docs/architecture/, docs/reference/, docs/runbooks/, skills/acdd/, ROADMAP.md, and CHANGELOG.md for the 18 static MCP tools, active-run-only acdd_state, code_map_analyze modes, and inline acdd_claim (<= 9,500 bytes) / acdd_read (<= 9,000 bytes) budgets; remove hard .agents/read_file_guard.py / .agents/hooks.json read_file block and adopt pragmatic ACDD-first navigation guidance in AGENTS.md
scope:
- .agents/
- AGENTS.md
- src/AGENTS.md
- README.md
- INSTALL.md
- docs/adr/
- docs/architecture/
- docs/reference/
- docs/runbooks/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
extended_scope:
- path: crates/acdd-state/tests/state_integration/profile.rs
  reason: Update stale 20-tool assertions in state_integration/profile.rs and default inline-cap detail=full in tests/mcp/installer.rs after Waves 2-3 consolidation
- path: tests/mcp/installer.rs
  reason: Update stale 20-tool assertions in state_integration/profile.rs and default inline-cap detail=full in tests/mcp/installer.rs after Waves 2-3 consolidation
- path: website/**
  reason: 'Remediate Terra review finding #592: reconcile website sources/SVGs, canonical lifecycle guide src/engine/guide.rs:254, and active plan doc to 18 static MCP tools and active-run-only acdd_state.'
- path: src/engine/guide.rs
  reason: 'Remediate Terra review finding #592: reconcile website sources/SVGs, canonical lifecycle guide src/engine/guide.rs:254, and active plan doc to 18 static MCP tools and active-run-only acdd_state.'
- path: docs/plans/ast-inference-schema-compaction-and-cli-parity.md
  reason: 'Remediate Terra review finding #592: reconcile website sources/SVGs, canonical lifecycle guide src/engine/guide.rs:254, and active plan doc to 18 static MCP tools and active-run-only acdd_state.'
- path: tests/acdd/**
  reason: 'Remediate Terra review finding #592: reconcile website sources/SVGs, canonical lifecycle guide src/engine/guide.rs:254, and active plan doc to 18 static MCP tools and active-run-only acdd_state.'
- path: tests/mcp/context/guide.rs
  reason: Add lifecycle guide assertion verifying retired acdd_state(roadmap|draft) guidance is absent
depends_on:
- compact-claim-and-read-inline-budget
tags:
- docs
contract_revision: 1
status: completed
subtasks:
- subtask_ref: update-mcp-reference-and-skills
  title: Update active tool counts (20 -> 18), active-run-only acdd_state guidance, code_map_query prove_removal/tests examples, code_map_analyze modes, inline acdd_claim (<= 9,500 bytes) / acdd_read (<= 9,000 bytes) docs, and pragmatic ACDD-first navigation guidance (removing .agents/read_file_guard.py and .agents/hooks.json hard read_file block) across active docs, ADRs, routers, skills/acdd/, ROADMAP.md, and CHANGELOG.md
  status: completed
  evidence: Removed .agents/read_file_guard.py and .agents/hooks.json; updated AGENTS.md, src/AGENTS.md, README.md, INSTALL.md, ROADMAP.md, CHANGELOG.md, docs/adr/{0001,0004,0006,0011}, docs/architecture/{concurrency,domain-extensions,indexing,performance}, docs/reference/{README,mcp-setup,mcp-tools,cli,tasks,acdd,profiles}, docs/runbooks/limits-and-freshness.md, skills/acdd/{SKILL.md,references/tool-catalog.md,references/profile-reference.md}, crates/acdd-state/tests/state_integration/profile.rs, and tests/mcp/installer.rs.
- subtask_ref: verify-docs-lint-and-suites
  title: Verify acdd docs check, acdd validate, acdd lint --categories arch,docs, and cargo test --test mcp --test query_context --test acdd pass with zero errors
  status: completed
  evidence: Verified acdd docs check, acdd validate, acdd lint --categories arch,docs, git diff --check, cargo clippy --workspace --all-targets --all-features -- -D warnings, state_integration (49/49), mcp (165/165), query_context (56/56), acdd (16/16), commitment_integrity (9/9), and /tmp/contextunity-test-* BEFORE==AFTER.
receipt:
  run_id: UX-004-mcp-tool-surf-20261126161604-1739070-2
  node_cursor: root:reconcile-mcp-tool-catalog-docs-and-skills.deliver
  gate_id: deliver
  worker_id: codex-terra-reviewer-ux004-t5-2
  claim_revision: 1
  contract_revision: 4
  candidate_tree: a1c1c5490a641048de48a542edf5ebbe0e709cf7
  committed_oid: 57452463da6194ba4dad2a45c06fa606d8f9fe61
  accepted_at: 2026-11-26T22:38:43.214Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `4` | **Budget**: `~240` net LOC (docs/skills + `.agents/` guard removal).
- **Seam & mutation proof (`AC-05`)**: `acdd docs check`, `acdd validate`, `acdd lint --categories arch,docs`, and `cargo test --test mcp --test query_context --test acdd` pass with zero stale active `20 static` / `code_map_prove_removal` / `code_map_tests` / `acdd_state(action: roadmap|draft)` instructions and zero `.agents/read_file_guard.py` hard-deny blocks.

## Negative boundaries

- No new static MCP tools (`acdd_spec` excluded; net count drops `20 -> 18`) and no new third-party dependencies in `Cargo.toml`.
- No edits to language extractors (`languages/`), `ast/visitor.rs`, `linker/`, or `db/lint/unused_public_api.rs` inside `UX-004` (owned by [LS-004](LS-004-rust-entrypoints-and-inline-module-exports.md)).
- No `git cat-file` blob reparsing or `--find-copies-harder` scans inside `UX-004`.
- No new schema properties on `code_map_query` (`Query` input struct) and no SQLite schema version bump.
- No raw `Command::new("git")` outside `crates/acdd-core/src/git.rs` (`INV-GIT-SINGLE-OWNER`).
- No exposure of sensitive or `WorkspacePathPolicy`-excluded paths in `diff_impact` or `cochange`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: `AC-01`..`AC-05` map 1:1 across Waves 0–3 covering read-only graph/PageRank analyzers, canonical `change_impact.rs` + bounded Git `cochange`, `20 -> 18` MCP tool/action consolidation synced with `ports.rs`, and full active doc/router reconciliation.
- **Correctness**: Enforces deterministic PageRank within 2-second budgets, `INV-READONLY-OVER-INDEX`, `INV-SINGLE-OWNER-DIFF-IMPACT`, `INV-GIT-SINGLE-OWNER`, `WorkspacePathPolicy` filtering, and fail-closed redirect hints.
- **Coherence**: Ready immediately in Wave 0 (`depends_on: []`), cleanly separated from Rust extractor/linker upgrades in [LS-004](LS-004-rust-entrypoints-and-inline-module-exports.md), ahead of [AR-006](../AR-006-acdd-spec-crate-and-workflow-cutover.md), and sharing deterministic PageRank `hotspots` with [AC-007](../AC-007-unified-observability-board-otlp-and-metrics.md).
