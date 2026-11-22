---
id: AR-002-modularize-db-reader-and-traversal
title: Workspace-wide modularization, constant & wire-tag unification, typed query/domain contracts, clone elimination, and test-placement hygiene
doc_type: contract
status: completed
depends_on:
- PF-004-fts-fastpath-codec-safety-and-pipeline-cleanup
owners:
- crates/acdd-core/AGENTS.md
- crates/acdd-core/src/
- crates/acdd-index/AGENTS.md
- crates/acdd-index/src/
- crates/acdd-state/AGENTS.md
- crates/acdd-state/src/
- crates/acdd-state/tests/
- src/AGENTS.md
- src/cli/
- src/engine/
- src/mcp/
- src/state_bridge/
- docs/architecture/indexing.md
- docs/milestones/AR-002-modularize-db-reader-and-traversal.md
- tests/acdd/
- tests/core/
- tests/linker/
- tests/mcp/
- tests/query_context/
invariants:
- 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: Workspace layout/file constants, SubsystemId, SQLite MetadataKey, MAX_QUERY_RESULT_BYTES, expanded NodeDetailKey, and models::tags::{EdgeKind, NodeKind, ReferenceKind, CoverageStatus, EdgeConfidence, LanguageId} act as the single canonical constant owners across acdd-core, acdd-index, acdd-state, and src/ instead of scattered raw string or numeric literals.'
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: Multi-thousand-line god-files across all four crates (acdd-core::config, acdd-index::{db/reader, db/symbols, db/traversal, semantic, linker/resolution/value_flow_phase}, acdd-state::{context, usecases/roadmap, usecases/deps, usecases/lifecycle}, and src/{cli, mcp/tools, state_bridge/code_domain, engine/pipeline/delta}) are decomposed into focused single-responsibility submodules preserving all public re-exports and wire contracts.'
- 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: Boundary-parsed enums (SubsystemId, OverviewAspect, ExplainDirection, ImpactDirection, TraversalMode, GraphQueryOp, SharedOwnerKind) replace raw string dispatch, and typed parameter/context structs replace all #[allow(clippy::too_many_arguments)] suppressions in acdd-index::{ast, db/writer, languages/manifests/semantic}.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: Duplicate structs, SQL predicates, and logic blocks (CompositeDomainProvider capability lookup, config canonical root collection, SharedRow/shared_owners hashing, governing_invariants, strip_selector_uri_prefix, ExplainOptions, db/symbols/search.rs SQL filters, split_python_pipe_types, required_configuration_facts, machine active-node guards, tx.rs admit_claim UPDATE, CLI --adapter/--config validation, MCP state_bridge execution, and engine reader/delta frontier clones) are consolidated behind single canonical helpers with phantom-tested non-_with_path db wrappers retired.'
- 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: paging::query avoids redundant double CTE execution where safe, directory_boundary avoids ORDER BY json_object(...), rows_with_mode counts bytes without double serialization, dead_tails_paged/duplicates_paged materialize JSON Value items only for the requested page slice, and inline integration-style test suites in acdd-state production modules (profile/mod.rs, validate.rs, context.rs, store/sqlite/mod.rs) are relocated to crates/acdd-state/tests/state_integration/.'
acceptance_criteria:
- 'AC-1: crates/acdd-core defines canonical workspace/path constants and a typed SubsystemId enum in subsystems.rs/config, deduplicates CompositeDomainProvider action/check lookup in ports.rs, and decomposes config.rs (2,061 LOC) into config/{mod,schema,loader,linked,paths}.rs with a single confined canonical-root collector.'
- 'AC-2: crates/acdd-index expands NodeDetailKey (signature, doc, visibility, params, returns, async, export, static, category), defines MetadataKey + read_metadata helpers and shared MAX_QUERY_RESULT_BYTES, unifies SharedOwnerKind + hash_shared_owner_leaves across db/commitments/domain_leaves.rs and db/writer/seal.rs, and replaces #[allow(clippy::too_many_arguments)] in db/writer/{mod,ingest}.rs with typed parameter structs.'
- 'AC-3: crates/acdd-index/src/db/{reader.rs (2,380 LOC), symbols.rs (1,531 LOC), traversal.rs (3,521 LOC)} are decomposed into db/{reader,symbols,traversal}/* submodules using OverviewAspect, ExplainDirection, ImpactDirection, TraversalMode, GraphQueryOp, MetadataKey, NodeDetailKey, and models::tags; governing_invariants, ExplainOptions, strip_selector_uri_prefix, and db/symbols/search.rs SQL filter builders each have one canonical owner; phantom-tested non-_with_path wrappers (reader::{inspect_paged, explain_with_options}, symbols::{inspect_with_options, explain_with_options, tests_paged}, traversal::{traverse_with_options, removal_paged, query_with_options}) are retired; and paging::query, rows_with_mode, directory_boundary, dead_tails_paged, and duplicates_paged eliminate redundant double execution, per-line String clones, and unpaged Value materialization.'
- 'AC-4: crates/acdd-index/src/semantic.rs (2,568 LOC) is split into semantic/{mod,catalog,types,relations,builtins}.rs; phase_value_flow (1,974 LOC in linker/resolution/value_flow_phase/mod.rs) is decomposed into focused phase helpers; split_python_pipe_types and required_configuration_facts clones between languages/ and linker/ are unified; and all 8 #[allow(clippy::too_many_arguments)] attributes in ast/{mod,search}.rs and languages/manifests/semantic.rs are replaced by typed context structs.'
- 'AC-5: crates/acdd-state extracts require_active_claimable_node across machine/{claim,submit,scope,verify}.rs, collapses store/sqlite/tx.rs::admit_claim into a single parameterized UPDATE, splits context.rs (2,260 LOC) and usecases/{roadmap.rs (2,593 LOC), deps.rs (2,325 LOC), lifecycle.rs (2,128 LOC)} into focused submodules adopting acdd-core constants/SubsystemId, deduplicates sample_snapshot/build_sample_graph helpers, and relocates inline integration test suites from profile/mod.rs (1,485 LOC tests), validate.rs, context.rs, and store/sqlite/mod.rs into crates/acdd-state/tests/state_integration/.'
- 'AC-6: Root host crate src/ decomposes src/cli/mod.rs (3,242 LOC; run_selected = 1,144 LOC), src/mcp/tools.rs (2,331 LOC), and src/state_bridge/code_domain.rs (2,467 LOC) into focused submodules, extracts shared CLI adapter-conflict validation, MCP ctx.execute_query/execute_command and worktree git-ancestor validation helpers, adopts OverviewAspect and SubsystemId in cli::discovery/state_bridge, deduplicates src/engine/{reader.rs, pipeline/delta.rs}, updates crate AGENTS.md and docs/architecture/indexing.md, and passes full workspace clippy, tests, docs check, validate, duplicates, and dead_tails gates.'
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
  closed_at: 2026-11-21T23:40:56.491Z
  target_base: main
  target_base_commit: c1470e513edaee0ff8aee2c7596ce2f06f499b43
  task_receipts:
    core-constants-ports-and-config-modularization:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:core-constants-ports-and-config-modularization.deliver
      gate_id: deliver
      worker_id: reviewer-t1
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 191fe2bfc36273beb280100b7b02373257865bda
      committed_oid: 5e3deb760535e8ae77a13900ad5cc04e6ccf4094
      accepted_at: 2026-11-21T20:27:49.898Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: acdd-core owns canonical workspace path constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, DEFAULT_STATE_DB_REL, DEFAULT_CODE_DB_REL, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and typed SubsystemId (Code, Docs, State, Blackboard, Memory) with alias resolution.'
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-core/src/config.rs (2,061 LOC) is split into config/{mod,schema,loader,linked,paths}.rs preserving all public acdd_core::config exports.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CompositeDomainProvider resolves action and check capabilities through single canonical helpers without duplicate lookup loops, and load_adapter_inner uses one confined canonical-root collector for roots and docs.'
          verified:
          - INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS
          - INV-AR002-MODULAR-MONOLITH-DECOMPOSITION
          - INV-AR002-SINGLE-OWNER-DEDUPLICATION
        review_summary: Delivered core-constants-ports-and-config-modularization after independent review pass.
    index-wire-tags-detail-keys-and-metadata-constants:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:index-wire-tags-detail-keys-and-metadata-constants.deliver
      gate_id: deliver
      worker_id: reviewer-t2
      claim_revision: 1
      contract_revision: 2
      candidate_tree: e5b46dcd45fedfef52c1fb75298b877f8235f59f
      committed_oid: 75fbc2bf7bad3ed31dfb7647080b699010d7acb3
      accepted_at: 2026-11-21T20:32:44.918Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: NodeDetailKey covers all persisted node detail JSON keys, and MetadataKey + read_metadata + MAX_QUERY_RESULT_BYTES eliminate hardcoded metadata key queries and 8 MiB literals across db/commitments, db/cycles, and db/writer.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: SharedOwnerKind and hash_shared_owner_leaves in db/commitments own shared_owners leaf projection and hashing across both cold/delta writer sealing (db/writer/seal.rs) and commitment verification (db/commitments/domain_leaves.rs).'
          - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: db/writer/{mod,ingest}.rs pass typed parameter structs instead of suppressing #[allow(clippy::too_many_arguments)].'
          verified: []
    modularize-db-reader-and-unify-query-projections:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-db-reader-and-unify-query-projections.deliver
      gate_id: deliver
      worker_id: reviewer-t3
      claim_revision: 1
      contract_revision: 2
      candidate_tree: be2ca3b810f23b5ca4c374ba56cdcd19ad33d253
      committed_oid: d02164ad1fe2ddf297de6acafd1c5249db9ecf86
      accepted_at: 2026-11-21T21:11:47.793Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/reader/ submodules isolate open/confinement, SQL budget execution, search, inspect/explain, docs, diagnostics, and selectors behind db::reader re-exports with zero clones or dead_tails.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: governing_invariants and governing_invariants_for_scope share one canonical query/projection helper, and selector URI normalization lives in one helper.'
          - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: OverviewAspect and ExplainDirection parse once at the db/reader boundary, and MetadataKey/NodeDetailKey/EdgeKind/NodeKind replace raw metadata and detail strings in db/reader and db/paging.'
          - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: paging::query avoids redundant double execution of CTEs via COUNT(*) OVER() or single-pass counting, and rows_with_mode avoids redundant row re-serialization for byte accounting.'
          verified: []
    modularize-db-symbols-and-deduplicate-search-options:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-db-symbols-and-deduplicate-search-options.deliver
      gate_id: deliver
      worker_id: reviewer-t6
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 7b0440fefa6af323a72d9a144c8c1be327f60ccd
      committed_oid: 75c092f852e85ca7fbf51151c7133cd3abe3bc04
      accepted_at: 2026-11-21T21:57:12.760Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/symbols/ submodules isolate confined source caching, line-FTS/centered previews, search navigation and SQL building, inspect/explain composition, and test discovery cleanly behind db::symbols re-exports with zero >= 5-line clones.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: ExplainOptions has one canonical definition/composition over ExplainDirection without manual cross-module reconstruction, and db/symbols/search.rs owns one shared SQL kind/path/doc filter builder and FTS tokenizer.'
          verified: []
    modularize-db-traversal-and-bound-analysis-queries:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-db-traversal-and-bound-analysis-queries.deliver
      gate_id: deliver
      worker_id: reviewer-t8
      claim_revision: 1
      contract_revision: 2
      candidate_tree: e6dc9d21f7408c48b3f9f931cfbf41e2b1ec27b6
      committed_oid: 9cc2781b55fb7d90728ed0f5da76ab4db07383a7
      accepted_at: 2026-11-21T22:34:53.538Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/traversal/ submodules isolate impact/BFS, directory slice, Git change_impact, duplicates, and dead_tails engines cleanly behind db::traversal re-exports.'
          - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: ImpactDirection, TraversalMode, and GraphQueryOp parse once at the db/traversal boundary and drive all internal BFS/SQL edge-kind and operation dispatch without raw string matching.'
          - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: dead_tails_paged and duplicates_paged materialize JSON Value payloads only for the requested page slice, duplicates avoids per-line String cloning, and directory_boundary sorts without ORDER BY json_object(...).'
          verified: []
    modularize-host-cli-mcp-state-bridge-and-engine:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-host-cli-mcp-state-bridge-and-engine.deliver
      gate_id: deliver
      worker_id: reviewer-t9
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 05f5ed48aa83877f50435274ce70dbec4c4b7b99
      committed_oid: 3ac1fc977e224f866cb4388cc3060cdc37874320
      accepted_at: 2026-11-21T22:32:13.134Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs, and src/engine/pipeline/delta.rs::compute_delta_frontiers are decomposed into focused single-responsibility submodules and helpers.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CLI --adapter vs --config conflict resolution, MCP state_bridge query/command execution and worktree git-ancestor validation, engine/reader.rs acdd_read_tool blocks, and engine/pipeline/delta.rs frontier expansion blocks each have one canonical helper.'
          - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: cli::discovery uses db::reader::OverviewAspect and acdd_core::subsystems::SubsystemId, and state_bridge uses SubsystemId and models::tags::{EdgeKind, NodeKind} instead of raw string literals.'
          verified: []
    modularize-semantic-linker-and-ast-contexts:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-semantic-linker-and-ast-contexts.deliver
      gate_id: deliver
      worker_id: reviewer-t4
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 20b8ef3c0c6def731b3185dd20279e31ebba9c45
      committed_oid: c2b9cda18df47d7182170ffe013e4f38323ea548
      accepted_at: 2026-11-21T21:20:37.721Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-index/src/semantic.rs is split into semantic/{mod,catalog,types,relations,builtins}.rs and phase_value_flow (1,974 LOC) in linker/resolution/value_flow_phase/mod.rs is decomposed into focused phase helpers.'
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: split_python_pipe_types and required_configuration_facts each have a single canonical definition shared between languages/ and linker/.'
          - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: All 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs are replaced by typed context structs, and NodeDetailKey/models::tags constants replace raw node/reference/language strings.'
          verified: []
        review_summary: Delivered modularize-semantic-linker-and-ast-contexts from reviewed candidate tree 20b8ef3c.
    modularize-state-usecases-roadmap-deps-and-lifecycle:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:modularize-state-usecases-roadmap-deps-and-lifecycle.deliver
      gate_id: deliver
      worker_id: reviewer-t7
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 0fae11920aeec16f513d79c9875bce0c1ca80a37
      committed_oid: 2cc5fa8a9d87ab485bc5e1682572ad9895b40daf
      accepted_at: 2026-11-21T21:48:13.205Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: usecases/{roadmap,deps,lifecycle}.rs (7,046 LOC combined) are decomposed into focused single-responsibility submodules under usecases/{roadmap,deps,lifecycle}/ preserving all pub and pub(crate) exports.'
          - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: usecases/ reuses canonical acdd-core workspace constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and SubsystemId instead of raw string literals.'
          verified: []
    state-machine-sqlite-dedup-and-test-hygiene:
      run_id: AR-002-modularize-db-20261121191319-1314429-1
      node_cursor: root:state-machine-sqlite-dedup-and-test-hygiene.deliver
      gate_id: deliver
      worker_id: reviewer-t5-2
      claim_revision: 1
      contract_revision: 2
      candidate_tree: af509e0be846a586b07f959a56d2f254f46de1a0
      committed_oid: c53e8327d08ca11d2c7ee8011711ea3c32bdb969
      accepted_at: 2026-11-21T21:16:22.368Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: machine/{claim,submit,scope,verify}.rs validate active run and claimable gate nodes through one require_active_claimable_node helper, tx.rs::admit_claim executes a single parameterized UPDATE statement, and build_sample_graph test fixtures share one canonical helper.'
          - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-state/src/context.rs (2,260 LOC) is decomposed into context/{mod,budget,tiers,markdown}.rs with named pub const tier/budget limits.'
          - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: Inline integration test suites in profile/mod.rs (1,485 LOC), validate.rs (641 LOC), context.rs (518 LOC), and store/sqlite/mod.rs (473 LOC) live under crates/acdd-state/tests/state_integration/ per crates/acdd-state/AGENTS.md.'
          verified:
          - INV-AR002-SINGLE-OWNER-DEDUPLICATION
          - INV-AR002-MODULAR-MONOLITH-DECOMPOSITION
          - INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE
        review_summary: Delivered state-machine-sqlite-dedup-and-test-hygiene after round-2 independent review pass (candidate_tree af509e0be846a586b07f959a56d2f254f46de1a0).
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: acdd-core owns canonical workspace path constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, DEFAULT_STATE_DB_REL, DEFAULT_CODE_DB_REL, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and typed SubsystemId (Code, Docs, State, Blackboard, Memory) with alias resolution.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-core/src/config.rs (2,061 LOC) is split into config/{mod,schema,loader,linked,paths}.rs preserving all public acdd_core::config exports.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CompositeDomainProvider resolves action and check capabilities through single canonical helpers without duplicate lookup loops, and load_adapter_inner uses one confined canonical-root collector for roots and docs.'
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: NodeDetailKey covers all persisted node detail JSON keys, and MetadataKey + read_metadata + MAX_QUERY_RESULT_BYTES eliminate hardcoded metadata key queries and 8 MiB literals across db/commitments, db/cycles, and db/writer.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: SharedOwnerKind and hash_shared_owner_leaves in db/commitments own shared_owners leaf projection and hashing across both cold/delta writer sealing (db/writer/seal.rs) and commitment verification (db/commitments/domain_leaves.rs).'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: db/writer/{mod,ingest}.rs pass typed parameter structs instead of suppressing #[allow(clippy::too_many_arguments)].'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/reader/ submodules isolate open/confinement, SQL budget execution, search, inspect/explain, docs, diagnostics, and selectors behind db::reader re-exports with zero clones or dead_tails.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: governing_invariants and governing_invariants_for_scope share one canonical query/projection helper, and selector URI normalization lives in one helper.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: OverviewAspect and ExplainDirection parse once at the db/reader boundary, and MetadataKey/NodeDetailKey/EdgeKind/NodeKind replace raw metadata and detail strings in db/reader and db/paging.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: paging::query avoids redundant double execution of CTEs via COUNT(*) OVER() or single-pass counting, and rows_with_mode avoids redundant row re-serialization for byte accounting.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-index/src/semantic.rs is split into semantic/{mod,catalog,types,relations,builtins}.rs and phase_value_flow (1,974 LOC) in linker/resolution/value_flow_phase/mod.rs is decomposed into focused phase helpers.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: split_python_pipe_types and required_configuration_facts each have a single canonical definition shared between languages/ and linker/.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: All 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs are replaced by typed context structs, and NodeDetailKey/models::tags constants replace raw node/reference/language strings.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: machine/{claim,submit,scope,verify}.rs validate active run and claimable gate nodes through one require_active_claimable_node helper, tx.rs::admit_claim executes a single parameterized UPDATE statement, and build_sample_graph test fixtures share one canonical helper.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-state/src/context.rs (2,260 LOC) is decomposed into context/{mod,budget,tiers,markdown}.rs with named pub const tier/budget limits.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: Inline integration test suites in profile/mod.rs (1,485 LOC), validate.rs (641 LOC), context.rs (518 LOC), and store/sqlite/mod.rs (473 LOC) live under crates/acdd-state/tests/state_integration/ per crates/acdd-state/AGENTS.md.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/symbols/ submodules isolate confined source caching, line-FTS/centered previews, search navigation and SQL building, inspect/explain composition, and test discovery cleanly behind db::symbols re-exports with zero >= 5-line clones.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: ExplainOptions has one canonical definition/composition over ExplainDirection without manual cross-module reconstruction, and db/symbols/search.rs owns one shared SQL kind/path/doc filter builder and FTS tokenizer.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: usecases/{roadmap,deps,lifecycle}.rs (7,046 LOC combined) are decomposed into focused single-responsibility submodules under usecases/{roadmap,deps,lifecycle}/ preserving all pub and pub(crate) exports.'
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: usecases/ reuses canonical acdd-core workspace constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and SubsystemId instead of raw string literals.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/traversal/ submodules isolate impact/BFS, directory slice, Git change_impact, duplicates, and dead_tails engines cleanly behind db::traversal re-exports.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: ImpactDirection, TraversalMode, and GraphQueryOp parse once at the db/traversal boundary and drive all internal BFS/SQL edge-kind and operation dispatch without raw string matching.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: dead_tails_paged and duplicates_paged materialize JSON Value payloads only for the requested page slice, duplicates avoids per-line String cloning, and directory_boundary sorts without ORDER BY json_object(...).'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs, and src/engine/pipeline/delta.rs::compute_delta_frontiers are decomposed into focused single-responsibility submodules and helpers.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CLI --adapter vs --config conflict resolution, MCP state_bridge query/command execution and worktree git-ancestor validation, engine/reader.rs acdd_read_tool blocks, and engine/pipeline/delta.rs frontier expansion blocks each have one canonical helper.'
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: cli::discovery uses db::reader::OverviewAspect and acdd_core::subsystems::SubsystemId, and state_bridge uses SubsystemId and models::tags::{EdgeKind, NodeKind} instead of raw string literals.'
      verified:
      - INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS
      - INV-AR002-MODULAR-MONOLITH-DECOMPOSITION
      - INV-AR002-SINGLE-OWNER-DEDUPLICATION
      - INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE
    decisions:
    - |-
      AR-002 Architectural Blueprint Decision (9 tasks across 4 disjoint waves, zero ADR changes, zero Cargo.toml/protected_paths edits):

      1. C2 Boundaries & Invariants Preserved:
         - INV-CORE-LEAF (ADR 0001): acdd-core remains strict leaf; acdd-state depends only on acdd-core; acdd-state::machine remains 100% pure (zero fs/process/net/rusqlite); rusqlite in acdd-state stays confined to store/sqlite/; src/state_bridge stays isolated from src/engine and crates/*.
         - Wire/Schema/Digest Preservation: all 19 MCP tool schemas, CLI flags/outputs, ErrorCodes, SQLite DDL, Merkle acdd-owner-sha256-v2 digests, 8 MiB row budgets, 2s QueryBudget, reader::select, and symbols::snippet_paged remain unchanged.
         - Directory Module Conversion Rule (src/AGENTS.md §4): delete monolithic <mod>.rs when creating <mod>/mod.rs and remove legacy #[path = "..."] attributes (db/reader/selectors.rs, db/symbols/{search,test_discovery}.rs).

      2. Critical Cross-Wave Seams & Grounded Engineering Safeguards:
         - T1 (acdd-core): Define SubsystemId (Code/Docs/State/Blackboard/Memory + codemap/acdd aliases) and workspace constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, DEFAULT_STATE_DB_REL=DEFAULT_STATE_DB, DEFAULT_CODE_DB_REL=DEFAULT_CODE_DB, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY); extract resolve_action_capability/resolve_check_capability in ports.rs (covering execute_action, run_check, observe_freshness, validate_*, action_capability, lookup_action_result); split config.rs into config/{mod,schema,loader,linked,paths}.rs with one confined canonical-root collector.
         - T2 (acdd-index models/commitments/writer): Expand NodeDetailKey (add SIGNATURE, DOC, VISIBILITY, PARAMS, RETURNS, ASYNC, EXPORT, STATIC, CATEGORY + DEFAULT_EXPORT, RETURN_TYPE, RULE) and EdgeKind (add Governs, Documents, ReferencesDoc, Overrides, Extends); place MetadataKey + read_metadata/read_metadata_optional/write_metadata + MAX_QUERY_RESULT_BYTES in db/commitments.rs (avoiding Wave 0/1 touch on db/mod.rs); unify SharedOwnerKind + hash_shared_owner_leaves preserving lexicographic sort on kind.as_str() ("default_export" < "reference" < "symbol") for bit-exact Merkle digests; replace 4 too_many_arguments suppressions in db/writer/{mod,ingest}.rs via typed param structs. T2 extends scope autonomously to the 3 external populate_owned callers (src/engine/pipeline/cold.rs, tests/framework_manifests/support/virtual_workspace.rs, tests/incremental/single_pass_spool_and_parallel_delta.rs; zero Wave 0 overlap).
         - T3 (db/reader & paging): Split reader.rs into reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs; introduce OverviewAspect and ExplainDirection (parsed at boundary, passed into paging::populate_edge_relations); unify governing_invariants* and strip_selector_uri_prefix; optimize paging::query with single-pass COUNT(*) OVER() + past-end fallback ONLY when items.is_empty() && offset > 0; replace double serialization in rows_with_mode with incremental exact byte accounting verified against serde_json::to_vec. T3 retires reader::{inspect_paged, explain_with_options} and extends scope to 4 non-overlapping test callers (tests/query_context/{search_and_paging,linked_workspaces}.rs, tests/languages/{profiles/popular,html/syntax}.rs); Wave 1 peer T4 migrates tests/linker/overload_disambiguation.rs:224 to existing inspect_paged_with_path to maintain 100% Wave 1 disjointness.
         - T4 (semantic/linker/ast): Split semantic.rs into semantic/{mod,catalog,types,relations,builtins}.rs and phase_value_flow into <=350-LOC phase submodules; unify split_python_pipe_types (in languages/python/value_flow.rs) and required_configuration_facts helper (preserving linker -> languages -> semantic DAG); remove all 8 too_many_arguments in ast/{mod,search}.rs and languages/manifests/semantic.rs, autonomously extending scope to the 7 internal languages/ caller files (zero Wave 1 conflict); migrate tests/linker/overload_disambiguation.rs:224 off reader::inspect_paged.
         - T5 (acdd-state machine/sqlite/context/tests): Extract require_active_claimable_node (and shared require_active_node so machine/scope.rs preserves Task container targeting); collapse tx.rs::admit_claim into single UPDATE with (?12 IS NULL OR node_version = ?12); split context.rs into context/{mod,budget,tiers,markdown}.rs; relocate inline integration suites from profile/mod.rs (<80 LOC), validate.rs, context.rs, store/sqlite/mod.rs into tests/state_integration/; unify build_sample_graph in harness.rs.
         - T6 (db/symbols): Split symbols.rs into symbols/{mod,source,preview,inspect,search,test_discovery}.rs; unify ExplainOptions by composing reader::ExplainOptions (backed by ExplainDirection) inside symbols::ExplainOptions with zero field duplication; deduplicate symbols/search.rs SQL filter builders & FTS tokenizer; retire symbols::{inspect_with_options, explain_with_options, tests_paged}, autonomously extending scope to tests/query_context/{symbol_context,diagnostics_and_source}.rs, tests/mcp/tool_evolution/cli_and_relations.rs, docs/architecture/performance.md:88, src/cli/mod.rs:2719, src/mcp/tools.rs:1086 (zero Wave 2 conflict with T7).
         - T7 (acdd-state usecases): Split usecases/{roadmap,deps,lifecycle}.rs into focused directory submodules; adopt acdd-core constants & SubsystemId; unify sample_snapshot test fixture across archive_notes.rs, queries.rs, and submit/post_save.rs; update crates/acdd-state/AGENTS.md.
         - T8 (db/traversal): Split traversal.rs into traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs; parse ImpactDirection/TraversalMode/GraphQueryOp at boundary while preserving exact call signatures used by Wave 3 peer T9 (src/cli/, src/mcp/); bound dead_tails_paged & duplicates_paged JSON Value materialization strictly to [offset..offset+limit]; drop per-line Vec<String> cloning in duplicates via borrowed &str slices & shared streaming slice hasher; replace ORDER BY json_object(...) in directory_boundary with native column ordering; retire traverse_with_options/removal_paged/query_with_options (autonomously extending scope to tests/incremental/directory_slice.rs, tests/mcp/context/entrypoints_and_delta.rs, tests/languages/{features,sql,python/frameworks/import_provenance}.rs; zero Wave 3 conflict with T9); update crates/acdd-index/AGENTS.md and docs/architecture/indexing.md.
         - T9 (host src/): Split src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs into focused submodules; extract resolve_cli_adapter_path, MCP ctx.execute_query/execute_command, validate_explicit_worktree_git_ancestors; wire OverviewAspect/SubsystemId/EdgeKind/NodeKind; deduplicate src/engine/{reader.rs, pipeline/delta.rs} and decompose compute_delta_frontiers; update src/AGENTS.md.
    - |-
      AR-002 Architectural Blueprint Decision (Revision 2 — supersedes #258 and closes all 4 findings from #259):

      1. C2 Boundaries & Invariants Preserved (unchanged from #258):
         - INV-CORE-LEAF (ADR 0001), pure acdd-state::machine (zero fs/process/net/rusqlite), rusqlite confined to acdd-state::store::sqlite, src/state_bridge isolated from src/engine & crates/*, zero Cargo.toml/protected_paths edits, adr_required: false.
         - All 19 MCP tool schemas, CLI flags/outputs, ErrorCodes, SQLite DDL, Merkle acdd-owner-sha256-v2 digests, 8 MiB row budgets, 2s QueryBudget, reader::select, and symbols::snippet_paged preserved unchanged.
         - Directory Module Conversion Rule (src/AGENTS.md §4): delete monolithic <mod>.rs when creating <mod>/mod.rs and remove ALL legacy #[path = "..."] attributes (db/reader/selectors.rs, db/symbols/{search,test_discovery}.rs, and src/mcp/tools.rs:37 #[path = "tools/inputs.rs"] while preserving existing src/mcp/tools/inputs.rs).

      2. Explicit Remediations Closing Blackboard #259 Findings 1–4:
         - [Finding 1 — T3/T6 populate_edge_relations two-step typing]: In Wave 1 T3, keep paging::populate_edge_relations accepting direction: Option<&str> and parse ONCE via ExplainDirection::parse_optional(direction)? INSIDE populate_edge_relations (eliminating raw string dispatch in paging.rs while leaving live caller db/symbols.rs:1004 untouched & compiling). In Wave 2 T6, extend scope to crates/acdd-index/src/db/paging.rs (zero Wave 2 overlap with T7) and tighten BOTH reader::ExplainOptions.direction AND paging::populate_edge_relations to take ExplainDirection directly alongside consolidating symbols::ExplainOptions over reader::ExplainOptions.
         - [Finding 2 — Complete caller sweeps & T7 fixture guard]:
           * T2 extends scope to crates/acdd-index/src/db/writer/batch.rs (#[cfg(test)] persist_graph caller at line 473) PLUS the 3 external populate_owned callers (src/engine/pipeline/cold.rs, tests/framework_manifests/support/virtual_workspace.rs, tests/incremental/single_pass_spool_and_parallel_delta.rs) — zero Wave 0 overlap with T1.
           * T3 extends scope to tests/query_context/ranking_and_coverage.rs (4 reader::inspect_paged callers at lines 1289,1304,1313,1387) PLUS tests/query_context/{search_and_paging,linked_workspaces}.rs and tests/languages/{profiles/popular,html/syntax}.rs — zero Wave 1 overlap with T4/T5.
           * T7 MUST keep canonical #[cfg(test)] pub(crate) fn sample_snapshot inside crates/acdd-state/src/usecases/archive_notes.rs (reused by usecases/queries.rs) so out-of-scope caller crates/acdd-state/src/usecases/submit/post_save.rs:607 continues compiling cleanly without scope creep.
         - [Finding 3 — Explicit Wave 1 integration barrier on tests/linker/overload_disambiguation.rs:224]:
           * Remove the DAG-free T4->T3 incoming_seam claim. T4 migrates tests/linker/overload_disambiguation.rs:224 off reader::inspect_paged to already-existing reader::inspect_paged_with_path inside T4 scope.
           * Wave 1 Integration Barrier (coordinator enforcement): T4 MUST land on the run branch BEFORE T3 integrates (or Wave 1 must fully drain both T3 and T4 before Wave 2 T6 runs --workspace --all-targets).
           * T3 verification stays scoped (-p acdd-index --lib --tests, --test query_context, --test languages --no-run, --test commitment_integrity — never --workspace) while isolated from T4. T6 verification explicitly documents the Wave 1 drain precondition (runs --workspace --all-targets after T4 lands, or scoped -p acdd-index --lib --bin acdd --test query_context --test mcp --test commitment_integrity until T4 has landed).
         - [Finding 4 — Disjoint Markdown link co-evolution for cargo run -q -- docs check across 7 active docs (15 relative links + 1 YAML owners entry)]:
           * Wave 1 T3 (deletes db/reader.rs): extend scope to docs/architecture/cross-platform.md (lines 41, 47 -> db/reader/open.rs).
           * Wave 1 T4 (deletes semantic.rs): extend scope to docs/architecture/indexing.md (line 40 -> semantic/types.rs) and docs/architecture/performance.md (line 7 owners -> semantic/, lines 184 [x3], 185, 238, 329 -> semantic/{types,catalog,mod}.rs).
           * Wave 1 T5 (deletes context.rs): extend scope to docs/reference/tasks.md (132 -> context/mod.rs), docs/architecture/specification-and-design-lifecycle.md (63 -> context/mod.rs), docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md (22 -> context/markdown.rs, 34 -> context/mod.rs; hyperlink co-evolution only, adr_required remains false), docs/reference/mcp-markdown-contracts.md (86 -> context/markdown.rs), and docs/reference/acdd.md (41 -> context/mod.rs, 63 -> context/markdown.rs).
           * Wave 2 T6 (retires tests_paged): extend scope to docs/architecture/performance.md (line 88 -> tests_paged_with_path).
           * Wave 3 T8: updates owned crates/acdd-index/AGENTS.md and docs/architecture/indexing.md.
           * Wave 3 T9 (deletes src/state_bridge/code_domain.rs and src/mcp/tools.rs): extend scope to docs/reference/acdd.md (line 123 -> src/state_bridge/code_domain/mod.rs), and remove #[path = "tools/inputs.rs"] in src/mcp/tools.rs while preserving existing src/mcp/tools/inputs.rs.
           * All doc extensions are 100% disjoint within every wave (Wave 1: T3=cross-platform.md, T4=indexing.md+performance.md, T5=tasks+spec-lifecycle+adr/0006+mcp-markdown+acdd.md; Wave 2: T6=performance.md; Wave 3: T8=indexing.md, T9=acdd.md).

      3. Core Task Blueprint Contracts Retained from #258:
         - T1 (acdd-core): SubsystemId, workspace constants (preserving DEFAULT_STATE_DB/DEFAULT_CODE_DB alongside *_REL), ports.rs resolve_action_capability/resolve_check_capability, config/{mod,schema,loader,linked,paths}.rs with single confined canonical-root collector.
         - T2 (acdd-index models/commitments/writer): NodeDetailKey & EdgeKind expansion, MetadataKey + read_metadata/read_metadata_optional/write_metadata + MAX_QUERY_RESULT_BYTES in db/commitments.rs, SharedOwnerKind + hash_shared_owner_leaves preserving lexicographic kind.as_str() sort, 4 db/writer param structs.
         - T3 (db/reader & paging): reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs, OverviewAspect & ExplainDirection, unified governing_invariants* & strip_selector_uri_prefix, single-pass COUNT(*) OVER() with past-end fallback ONLY on items.is_empty() && offset > 0, incremental rows_with_mode byte accounting verified against serde_json::to_vec.
         - T4 (semantic/linker/ast): semantic/{mod,catalog,types,relations,builtins}.rs, phase_value_flow <=350-LOC submodules, unified split_python_pipe_types & required_configuration_facts, 8 too_many_arguments replaced (extend-scope to 7 internal languages/ caller files), overload_disambiguation.rs:224 migration.
         - T5 (acdd-state machine/sqlite/context/tests): require_active_node + require_active_claimable_node (preserving machine/scope.rs Task container targeting), single UPDATE admit_claim with (?12 IS NULL OR node_version = ?12), context/{mod,budget,tiers,markdown}.rs, inline test relocation to tests/state_integration/ (profile/mod.rs < 80 LOC), unified build_sample_graph in harness.rs.
         - T6 (db/symbols): symbols/{mod,source,preview,inspect,search,test_discovery}.rs, unified ExplainOptions composing reader::ExplainOptions (ExplainDirection), deduplicated search.rs SQL filters & FTS tokenizer, retire symbols::{inspect_with_options, explain_with_options, tests_paged}.
         - T7 (acdd-state usecases): usecases/{roadmap,deps,lifecycle}/ splits, acdd-core constants & SubsystemId, unified sample_snapshot in archive_notes.rs, crates/acdd-state/AGENTS.md update.
         - T8 (db/traversal): traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs, boundary-parsed ImpactDirection/TraversalMode/GraphQueryOp preserving T9-compatible call signatures, lazy [offset..offset+limit] Value materialization in dead_tails_paged/duplicates_paged, borrowed &str slices + shared streaming slice hasher in duplicates, native column ORDER BY in directory_boundary, retire traverse_with_options/removal_paged/query_with_options (promote traverse_with_path_options to pub).
         - T9 (host src/): src/cli/{mod,args,discovery,dispatch_code,dispatch_state}.rs, src/mcp/tools/{mod,inputs,code_tools,state_tools,dispatch}.rs, src/state_bridge/code_domain/{mod,seam,signature_scope,context}.rs, resolve_cli_adapter_path, ctx.execute_query/execute_command, validate_explicit_worktree_git_ancestors, src/engine/{reader,pipeline/delta} dedup & compute_delta_frontiers split, src/AGENTS.md update.
    - |-
      AR-002 Architectural Blueprint Decision (Revision 3 — supersedes #260 and closes #261; exhaustive T1..T9 caller sweep complete):

      1. Explicit Closure of Blackboard #261 (Wave 2 T6 -> db/traversal.rs:3431-3439 & db/symbols/mod.rs pub(crate) re-exports):
         - T6 (modularize-db-symbols-and-deduplicate-search-options) extends scope autonomously to crates/acdd-index/src/db/traversal.rs STRICTLY for lines 3431-3439 (updating symbols::explain_paged_response to pass &reader::ExplainOptions { direction: ExplainDirection::Both, show_doc: true, page: options }; zero Wave 2 overlap with T7, matching T6 existing extensions to src/cli/mod.rs:2719 and src/mcp/tools.rs:1086). T6 negative_boundaries #1 is narrowed to permit only that single call-site update while keeping all other db/traversal.rs logic and modularization in Wave 3 T8.
         - T6 exported_interfaces and outgoing_seams explicitly require db/symbols/mod.rs to preserve pub(crate) use source::{VerifiedSourceCache, verified_source, read_confined_source_bytes}; and pub(crate) use inspect::{inspect_paged_response, explain_paged_response}; so db/reader/diagnostics.rs:2025,2172 and db/traversal.rs:448,1935,2943 keep compiling untouched.

      2. Exhaustive T1..T9 Cross-Module Caller Audit Confirmed Complete:
         - T3 (db/reader/mod.rs): explicitly preserves pub(crate)/pub(super) re-exports (QueryBudget, RowMode, rows_with_mode, select_detail, select_detail_with_path) consumed by db/{paging,cycles,symbols,traversal}.
         - T4 (semantic/linker/ast): preserves pub(crate) fn languages::typescript::linker::required_configuration_facts signature called by crates/acdd-index/src/linker/mod.rs:205 while sharing the inner matching/config-path helper with linker/semantic_context/components.rs.
         - All 4 remediations from #260 (closing #259) remain intact: T3/T6 populate_edge_relations two-step typing, T2 batch.rs:473 + 3 populate_owned callers, T3 ranking_and_coverage.rs + 4 test callers, T7 archive_notes::tests::sample_snapshot canonical path, Wave 1 integration barrier (T4 lands before T3 integrates / before T6 runs --workspace), and all 15 live active Markdown links + YAML owners across 7 docs + src/mcp/tools.rs #[path = "tools/inputs.rs"] removal.
    - 'BUDGET_OVERRIDE: approved up to +380 net LOC for core-constants-ports-and-config-modularization (measured +365: production net LOC is only +168 across workspace constants, SubsystemId, ports.rs dedup (-7), and config/ modularization; remaining +197 is inline #[cfg(test)] verification_seam coverage in src/*.rs; clippy/tests/commitment_integrity/duplicates/dead_tails are green).'
    - 'BUDGET_OVERRIDE: approved up to +1000 net LOC for modularize-semantic-linker-and-ast-contexts (measured +968: decomposing the 1,974-LOC phase_value_flow monolith + 15 closures into 8 typed submodules with PhaseContext/RefQuery/ReferenceState and max fn <=309 LOC accounts for ~+798 LOC of explicit struct/method/import/docstring headers, plus ~+190 LOC for typed context/input structs replacing all 8 #[allow(clippy::too_many_arguments)] suppressions and updating callers, offset by -80 LOC from deduplicating split_python_pipe_types and required_configuration_facts; clippy, acdd-index --lib 74/74, tests/linker 76/76, commitment_integrity 18/18, docs check, workspace check, and duplicate/dead_tail checks are green).'
    - 'BUDGET_OVERRIDE: approved up to +520 net LOC for modularize-host-cli-mcp-state-bridge-and-engine (measured +499 across splitting 4 host monoliths — src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs, and src/engine/{reader.rs,pipeline/delta.rs} totaling ~9,000 LOC — into focused submodules with explicit module headers, re-exports, and docstrings; acdd verify, clippy, lib/core/acdd/commitment_integrity/docs check are all green).'
    deferred_findings:
    - 'Non-blocking follow-up notes from modularize-semantic-linker-and-ast-contexts.review: (1) docs/architecture/indexing.md:124 still contains an unlinked prose backtick `crates/acdd-index/src/semantic.rs` (docs check passes since line 37 & 40 links were updated; update during pre-handoff documentation scan to `crates/acdd-index/src/semantic/types.rs`); (2) a few untouched language/kind string literals remain in ast/mod.rs (lines 208-221, 296, 417-437, 494) and untouched value_flow_phase/{members,relation_edges}.rs.'
    - 'Non-blocking follow-up notes from modularize-state-usecases-roadmap-deps-and-lifecycle.review: (1) crates/acdd-state/AGENTS.md:20 still has one unlinked prose backtick `context.rs` ("size 6-tier `context.rs` payloads") that can be updated to `context/` during the pre-handoff documentation scan (docs check passes cleanly); (2) crates/acdd-state/src/usecases/tests.rs:44,121,124 (out-of-scope test helpers) still contain literal `.acdd`/`.worktrees` path fixtures.'
    - 'Non-blocking follow-up notes from modularize-db-symbols-and-deduplicate-search-options.review: (1) PRE-EXISTING RUN-BRANCH REGRESSION FROM T3 (889c88018, NOT introduced by T6; reproduced identically on run/AR-002-modularize-db-reader-and-traversa): `cargo test --test mcp` fails 2/159 tests (`context::query_and_policy::mcp_budget_and_query_errors_explain_how_to_retry` and `tool_evolution::mcp_limits::mcp_enforces_page_row_and_time_budgets`) with `Error code 18: String or BLOB exceeds size limit` instead of the friendly `8 MiB row budget` / `select fewer and smaller SQL columns` error because T3 `crates/acdd-index/src/db/paging.rs:89` (`SELECT COUNT(*) OVER() AS __acdd_total, _q.*`) causes SQLite to materialize the combined 12 MiB two-column row into a window B-tree record exceeding `SQLITE_LIMIT_LENGTH = 8 MiB` before Rust `rows_with_mode` runs — fix in polish/T8 before `run_tests`; (2) `docs/reference/mcp-markdown-contracts.md:336` still has an unlinked prose backtick `crates/acdd-index/src/db/symbols.rs` alongside `crates/acdd-index/src/db/symbols/` (`docs check` passes cleanly; update during pre-handoff documentation scan); (3) `crates/acdd-index/src/db/symbols/test_discovery.rs:335` still uses raw `unresolved`/`ambiguous` literals where `CoverageStatus::{Unresolved, Ambiguous}` constants exist.'
    - 'Non-blocking follow-up notes from modularize-host-cli-mcp-state-bridge-and-engine.review: (1) Unlinked prose backtick references to deleted src/mcp/tools.rs remain in docs/AGENTS.md:30, docs/reference/mcp-tools.md:26,189, docs/reference/mcp-markdown-contracts.md:335, and docs/milestones/AC-006-universal-repository-memory-synthesis.md:120,627 (docs check passes cleanly since these are prose backticks, not links; update during pre-handoff documentation scan); (2) Pre-existing T3 db/paging.rs:89 SQLITE_TOOBIG regression (#272) still fails the same 2/159 tests in cargo test --test mcp (157/159 pass on T9 worktree; tracked for T8/polish); (3) Minor optional polish: src/engine/{reader.rs,pipeline/delta.rs} retain pre-existing adapter.modules.is_enabled("code"|"docs"|"state"|"codemap") calls that could use SubsystemId, pre-existing 5-line looks_like_sql remains in both src/cli/dispatch_code.rs:25 and src/mcp/tools/code_tools.rs:19, and src/engine/pipeline/delta.rs:511 carries a superfluous #[allow(clippy::too_many_arguments)] on a 7-arg helper.'
    - 'Non-blocking polish note from modularize-db-traversal-and-bound-analysis-queries.review: crates/acdd-index/src/db/paging.rs:78 defines const QUERY_RESULT_TOO_LARGE with the same 1-line error string as out-of-scope crates/acdd-index/src/db/reader/sql.rs:7; unify behind one shared pub(crate) constant (e.g. in db/commitments.rs or db/reader/sql.rs) during polish.'
    review_summary: AR-002 completed across all 9 tasks (4 waves), verified on Linux (852 workspace + 18 commitment_integrity) and macOS SSH (832 tests, 0 leaks), passed independent integration_review across all 6 contours, synthesized memory in docs/memories/AR-002-modularize-db-reader-and-traversal.md, and completed pre-handoff doc scan (#281).
  metrics_summary:
    decisions_count: 6
    deferred_findings_count: 5
    failed_traces: 1
    passed_traces: 13
    task_receipts_count: 9
    tasks_cancelled: 0
    tasks_completed: 9
    tasks_total: 9
    total_claims: 38
    total_review_cycles: 3
    total_traces: 15
---

# AR-002: Workspace-Wide Modularization, Constant & Wire-Tag Unification, Typed Contracts, Clone Elimination, and Test Hygiene

## Goal & Repository-Wide Audit Findings

Before executing feature milestones [`LS-003`](LS-003-universal-parser-tier1-coverage-and-shell-css-json.md) and [`AC-005`](../AC-005-unified-swarm-gates-and-peer-board-coordination.md), a full workspace audit across `crates/acdd-core`, `crates/acdd-index`, `crates/acdd-state`, and root `src/` identified six cross-cutting optimization pillars that are consolidated into this single, parallel-wave foundation milestone:

1. **Unfinished Constant & Wire-Tag Unification (`INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS`):**
   - [`models/tags.rs`](../../crates/acdd-index/src/models/tags.rs) (`EdgeKind`, `EdgeConfidence`, `CoverageStatus`, `NodeKind`, `ReferenceKind`, `LanguageId`) has only **1 caller** (`models/compact_graph.rs:26-33`), and [`models/details.rs`](../../crates/acdd-index/src/models/details.rs) (`NodeDetailKey`) has **0 external callers** outside `details.rs` while missing common detail keys (`"signature"`, `"doc"`, `"visibility"`, `"params"`, `"returns"`, `"async"`, `"export"`, `"static"`).
   - SQLite `metadata` keys (`"output_root"`, `"workspace_root"`, `"adapter"`, `"adapter_linked_workspaces"`, `"schema_version"`, `"indexer_engine"`, `"index_semantics_version"`, `"commitment_algorithm"`, `"inventory_snapshot"`) are hardcoded across **18+ queries** in `db/{reader,paging,symbols,traversal,commitments,writer/delta}`.
   - `8 * 1024 * 1024` (`MAX_QUERY_RESULT_BYTES`) is redefined separately in `db/reader.rs`, `db/cycles.rs`, and `db/traversal.rs`.
   - Workspace layout literals (`"acdd.yaml"`, `".acdd"`, `".worktrees"`, `"state.sqlite"`, `"code.sqlite"`) and subsystem names (`"code"`, `"docs"`, `"state"`, `"blackboard"`, `"memory"`) are hardcoded as raw strings across `acdd-core`, `acdd-state`, `src/cli/`, `src/mcp/`, and `src/state_bridge/`.

2. **Multi-Domain God-Files & Giant Functions (`INV-AR002-MODULAR-MONOLITH-DECOMPOSITION`):**
   - `crates/acdd-index/src/db/{traversal.rs (3,521 LOC), reader.rs (2,380 LOC), symbols.rs (1,531 LOC)}` (`7,432` LOC total).
   - `crates/acdd-index/src/semantic.rs` (`2,568` LOC, `52` types, `387` symbols) and `crates/acdd-index/src/linker/resolution/value_flow_phase/mod.rs` (`2,039` LOC containing one **`1,974`-LOC function `phase_value_flow`**).
   - `crates/acdd-core/src/config.rs` (`2,061` LOC).
   - `crates/acdd-state/src/{usecases/roadmap.rs (2,593 LOC), usecases/deps.rs (2,325 LOC), context.rs (2,260 LOC), usecases/lifecycle.rs (2,128 LOC)}`.
   - `src/{cli/mod.rs (3,242 LOC; run_selected = 1,144 LOC), state_bridge/code_domain.rs (2,467 LOC), mcp/tools.rs (2,331 LOC), engine/reader.rs (1,804 LOC), engine/pipeline/delta.rs (1,559 LOC; compute_delta_frontiers = 748 LOC)}`.

3. **Stringly-Typed Dispatch & `#[allow(clippy::too_many_arguments)]` Debt (`INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS`):**
   - `db/` propagates raw `Option<&str>` / `&str` for overview aspects, explain/impact directions, traversal modes, and graph operations instead of boundary-parsed enums (`OverviewAspect`, `ExplainDirection`, `ImpactDirection`, `TraversalMode`, `GraphQueryOp`), and `cli::discovery::includes_discovery` re-hardcodes aspect strings.
   - `12` production functions across `db/writer/{mod,ingest}.rs` (4), `ast/{mod,search}.rs` (2), and `languages/manifests/semantic.rs` (6) suppress `#[allow(clippy::too_many_arguments)]` instead of passing typed parameter/context structs.

4. **Concrete Code Clones, Duplicate Structs & Phantom-Tested Wrappers (`INV-AR002-SINGLE-OWNER-DEDUPLICATION`):**
   - `crates/acdd-core/src/ports.rs:521-654`: 3-way duplicated action lookup in `validate_action_ref`, `action_capability`, and `lookup_action_result` plus redundant double lookup in `execute_action` / `run_check`.
   - `crates/acdd-core/src/config.rs:1134-1160`: duplicated canonical path containment loop for `roots` and `docs`.
   - `crates/acdd-index/src/db/commitments/domain_leaves.rs:333-405` vs `db/writer/seal.rs:546-607`: verbatim 65-line `SharedRow` struct, `kind_id` (`0|1|2`) mapping, sorting, and `shared_owners` leaf hashing loop.
   - `crates/acdd-index/src/db/reader.rs:1761-1876`: 9 duplicate blocks (`45` lines) between `governing_invariants` and `governing_invariants_for_scope`, plus duplicated selector URI prefix stripping (`file://`, `file:`, `./`) across `reader` and `traversal`.
   - `crates/acdd-index/src/db/reader.rs:165` vs `db/symbols.rs:54`: duplicate `ExplainOptions` structs and 5x repeated SQL `kind`/`path`/`include_docs` filter predicates + FTS tokenization in `db/symbols/search.rs:88-202`.
   - `crates/acdd-index/src/languages/python/value_flow.rs:406-433` vs `linker/python/value_flow.rs:187-214`: identical 28-line `split_python_pipe_types`, and `languages/typescript/linker.rs:702-715` vs `linker/semantic_context/components.rs:181-194`: identical 14-line `required_configuration_facts`.
   - `crates/acdd-state/src/machine/{claim,submit,scope,verify}.rs`: 4x repeated active-run + `to_node_id()` + `find_node` + `RootGate | TaskGate` guard, and `store/sqlite/tx.rs:787-833`: duplicated 12-parameter `UPDATE gate_nodes` execution.
   - `src/cli/mod.rs:2433-2484`: duplicated `--adapter` vs `--config` conflict validation; `src/mcp/tools.rs:591-614,760-799`: 4x repeated 7-argument `state_bridge::execute_state_*` calls and unformatted 600-char single-line check at `tools.rs:1644`; `src/engine/reader.rs:407-418,585-596` and `src/engine/pipeline/delta.rs:864-877,989-1002`: duplicated 12–14 line blocks.
   - Phantom-tested non-`_with_path` wrappers (`reader::{inspect_paged, explain_with_options}`, `symbols::{inspect_with_options, explain_with_options, tests_paged}`, `traversal::{traverse_with_options, removal_paged, query_with_options}`) have zero production callers.

5. **Query-Time Allocations & Inline Test Placement (`INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE`):**
   - `paging::query` executes complex CTEs twice; `rows_with_mode` serializes each row twice for byte counting; `directory_boundary` sorts via `ORDER BY json_object(...)`; `dead_tails_paged` and `duplicates_paged` build full-repo `Vec<Value>` lists before slicing to `offset..offset+limit` (and `duplicates_paged` clones per-line `Vec<String>`).
   - `crates/acdd-state/src/profile/mod.rs` contains `1,485` LOC of inline tests out of `1,540` total lines (plus `641` LOC in `validate.rs`, `518` LOC in `context.rs`, `473` LOC in `store/sqlite/mod.rs`), violating `crates/acdd-state/AGENTS.md`, and duplicates `build_sample_graph` / `sample_snapshot` across `harness.rs`, `machine/graph.rs`, and `usecases/{archive_notes,queries}.rs`.

## Topological Wave Architecture (Maximum Disjoint Parallelism)

```text
Wave 0 (2 parallel tasks):
  ├── [T1] core-constants-ports-and-config-modularization      (crates/acdd-core/*)
  └── [T2] index-wire-tags-detail-keys-and-metadata-constants  (crates/acdd-index/{models,commitments,db/commitments,db/writer,db/cycles})

Wave 1 (3 parallel tasks):
  ├── [T3] modularize-db-reader-and-unify-query-projections    (crates/acdd-index/src/db/{mod,paging,reader*})
  ├── [T4] modularize-semantic-linker-and-ast-contexts         (crates/acdd-index/src/{semantic*,ast*,languages*,linker*})
  └── [T5] state-machine-sqlite-dedup-and-test-hygiene         (crates/acdd-state/src/{machine,store,profile,validate,context,harness}, crates/acdd-state/tests/*)

Wave 2 (2 parallel tasks):
  ├── [T6] modularize-db-symbols-and-deduplicate-search-options (crates/acdd-index/src/db/symbols*)
  └── [T7] modularize-state-usecases-roadmap-deps-and-lifecycle (crates/acdd-state/src/usecases/*)

Wave 3 (2 parallel tasks):
  ├── [T8] modularize-db-traversal-and-bound-analysis-queries   (crates/acdd-index/src/db/traversal*, docs/architecture/indexing.md)
  └── [T9] modularize-host-cli-mcp-state-bridge-and-engine      (src/{cli,mcp,state_bridge,engine}/*)
```

Every concurrent task pair within Wave 0, Wave 1, Wave 2, and Wave 3 has **100% disjoint `scope:` declarations**.

## Non-Goals & Negative Boundaries

- Preserve all existing CLI, MCP, and JSON wire schemas, error codes (`ErrorCode`), SQLite table schemas, Merkle commitment digests (`acdd-owner-sha256-v2`), `8 MiB` query result bounds, and `2s` query budgets.
- Keep `reader::select` and `symbols::snippet_paged` intact for existing language/linker test harnesses.

## Tasks in this milestone

### task: core-constants-ports-and-config-modularization

```yaml
task_ref: core-constants-ports-and-config-modularization
target: Define canonical workspace layout constants and typed SubsystemId in acdd-core, deduplicate CompositeDomainProvider capability lookups in ports.rs, and decompose config.rs into focused config/ submodules
scope:
- crates/acdd-core/AGENTS.md
- crates/acdd-core/src/lib.rs
- crates/acdd-core/src/subsystems.rs
- crates/acdd-core/src/ports.rs
- crates/acdd-core/src/config.rs
- crates/acdd-core/src/config/
tags:
- default
invariants:
- 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: acdd-core owns canonical workspace path constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, DEFAULT_STATE_DB_REL, DEFAULT_CODE_DB_REL, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and typed SubsystemId (Code, Docs, State, Blackboard, Memory) with alias resolution.'
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-core/src/config.rs (2,061 LOC) is split into config/{mod,schema,loader,linked,paths}.rs preserving all public acdd_core::config exports.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CompositeDomainProvider resolves action and check capabilities through single canonical helpers without duplicate lookup loops, and load_adapter_inner uses one confined canonical-root collector for roots and docs.'
acceptance_criteria:
- 'AC-1: crates/acdd-core/src/subsystems.rs defines SubsystemId (Code, Docs, State, Blackboard, Memory) with FromStr/as_str/alias handling and canonical workspace filename/directory constants exported from acdd_core.'
- 'AC-2: crates/acdd-core/src/ports.rs consolidates validate_action_ref, action_capability, lookup_action_result, execute_action, validate_check_ref, and run_check onto resolve_action_capability and resolve_check_capability with zero duplicate lookup blocks in code_map_query(duplicates, target="crates/acdd-core").'
- 'AC-3: crates/acdd-core/src/config.rs is decomposed into config/{mod,schema,loader,linked,paths}.rs, load_adapter_inner deduplicates its roots/docs canonicalization loop, and cargo test -p acdd-core passes cleanly.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: define-core-workspace-constants-and-subsystem-id
  title: Add canonical workspace layout constants and typed SubsystemId enum with alias resolution in crates/acdd-core
  status: completed
  evidence: Added SubsystemId enum with aliases/FromStr/Display/is_reserved_default_off in crates/acdd-core/src/subsystems.rs and exported canonical workspace constants from config and crate root; verified by subsystem_id_aliases_parse_and_reject_matrix and workspace_constants_and_db_path_resolution_matrix.
- subtask_ref: deduplicate-composite-domain-provider-lookups
  title: Extract resolve_action_capability and resolve_check_capability in crates/acdd-core/src/ports.rs and eliminate duplicate lookup/validation calls
  status: completed
  evidence: Extracted resolve_provider_capability, resolve_action_capability, and resolve_check_capability in crates/acdd-core/src/ports.rs; verified 0 duplicate blocks via code_map_query(duplicates) and resolve_capability_parity_across_validation_inspection_and_dispatch.
- subtask_ref: split-core-config-and-dedup-canonical-roots
  title: Decompose crates/acdd-core/src/config.rs into config/{mod,schema,loader,linked,paths}.rs and unify confined canonical root collection
  status: completed
  evidence: Decomposed config.rs into config/{mod,schema,loader,linked,paths}.rs, deleted config.rs, extracted collect_confined_canonical_roots in config/linked.rs, updated crates/acdd-core/AGENTS.md, and passed cargo clippy, cargo test -p acdd-core --lib (39/39), and commitment_integrity (18/18).
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:core-constants-ports-and-config-modularization.deliver
  gate_id: deliver
  worker_id: reviewer-t1
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 191fe2bfc36273beb280100b7b02373257865bda
  committed_oid: 5e3deb760535e8ae77a13900ad5cc04e6ccf4094
  accepted_at: 2026-11-21T20:27:49.898Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: acdd-core owns canonical workspace path constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, DEFAULT_STATE_DB_REL, DEFAULT_CODE_DB_REL, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and typed SubsystemId (Code, Docs, State, Blackboard, Memory) with alias resolution.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-core/src/config.rs (2,061 LOC) is split into config/{mod,schema,loader,linked,paths}.rs preserving all public acdd_core::config exports.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CompositeDomainProvider resolves action and check capabilities through single canonical helpers without duplicate lookup loops, and load_adapter_inner uses one confined canonical-root collector for roots and docs.'
      verified:
      - INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS
      - INV-AR002-MODULAR-MONOLITH-DECOMPOSITION
      - INV-AR002-SINGLE-OWNER-DEDUPLICATION
    review_summary: Delivered core-constants-ports-and-config-modularization after independent review pass.
```

### task: index-wire-tags-detail-keys-and-metadata-constants

```yaml
task_ref: index-wire-tags-detail-keys-and-metadata-constants
target: Expand NodeDetailKey and models::tags helpers, introduce canonical SQLite MetadataKey and MAX_QUERY_RESULT_BYTES constants, unify SharedOwnerKind commitment leaf hashing across domain_leaves.rs and seal.rs, and replace db/writer too_many_arguments suppressions with typed parameter structs
scope:
- crates/acdd-index/src/models/mod.rs
- crates/acdd-index/src/models/details.rs
- crates/acdd-index/src/models/tags.rs
- crates/acdd-index/src/commitments.rs
- crates/acdd-index/src/db/cycles.rs
- crates/acdd-index/src/db/commitments.rs
- crates/acdd-index/src/db/commitments/
- crates/acdd-index/src/db/writer/mod.rs
- crates/acdd-index/src/db/writer/ingest.rs
- crates/acdd-index/src/db/writer/seal.rs
- crates/acdd-index/src/db/writer/delta.rs
extended_scope:
- path: crates/acdd-index/src/db/writer/batch.rs
  reason: Update external callers of refactored populate_owned and persist_graph typed parameter structs per blueprint
- path: src/engine/pipeline/cold.rs
  reason: Update external callers of refactored populate_owned and persist_graph typed parameter structs per blueprint
- path: tests/framework_manifests/support/virtual_workspace.rs
  reason: Update external callers of refactored populate_owned and persist_graph typed parameter structs per blueprint
- path: tests/incremental/single_pass_spool_and_parallel_delta.rs
  reason: Update external callers of refactored populate_owned and persist_graph typed parameter structs per blueprint
tags:
- default
invariants:
- 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: NodeDetailKey covers all persisted node detail JSON keys, and MetadataKey + read_metadata + MAX_QUERY_RESULT_BYTES eliminate hardcoded metadata key queries and 8 MiB literals across db/commitments, db/cycles, and db/writer.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: SharedOwnerKind and hash_shared_owner_leaves in db/commitments own shared_owners leaf projection and hashing across both cold/delta writer sealing (db/writer/seal.rs) and commitment verification (db/commitments/domain_leaves.rs).'
- 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: db/writer/{mod,ingest}.rs pass typed parameter structs instead of suppressing #[allow(clippy::too_many_arguments)].'
acceptance_criteria:
- 'AC-1: NodeDetailKey in crates/acdd-index/src/models/details.rs adds SIGNATURE, DOC, VISIBILITY, PARAMS, RETURNS, ASYNC, EXPORT, STATIC, and CATEGORY constants; models/tags.rs exposes const wire helpers; and MetadataKey + read_metadata(conn, key) + MAX_QUERY_RESULT_BYTES replace raw metadata SQL strings and 8 * 1024 * 1024 literals in db/commitments*, db/cycles.rs, and db/writer/delta.rs.'
- 'AC-2: SharedOwnerKind (Symbol = 0, Reference = 1, DefaultExport = 2) and hash_shared_owner_leaves eliminate the duplicated SharedRow struct and hashing loop between db/commitments/domain_leaves.rs and db/writer/seal.rs while preserving exact Merkle leaf digests.'
- 'AC-3: All 4 #[allow(clippy::too_many_arguments)] suppressions in db/writer/{mod.rs,ingest.rs} (populate_owned, persist_files_impl, persist_graph, persist_graph_prepared) are replaced by typed parameter structs.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: expand-node-detail-keys-and-metadata-constants
  title: Expand NodeDetailKey, add MetadataKey + read_metadata helpers and shared MAX_QUERY_RESULT_BYTES, and wire into db/commitments, db/cycles, and db/writer/delta
  status: completed
  evidence: Expanded NodeDetailKey and wire_tags! (ALL/WIRE_NAMES/parse + 5 EdgeKind variants), added MetadataKey + read_metadata/read_metadata_optional/write_metadata + MAX_SQLITE_VALUE_BYTES/MAX_QUERY_RESULT_BYTES in db/commitments.rs, and wired into db/commitments*, db/cycles.rs, db/writer/{mod,delta}.rs.
- subtask_ref: unify-shared-owner-commitment-leaf-hashing
  title: Extract SharedOwnerKind, SharedOwnerRow, and hash_shared_owner_leaves in db/commitments and reuse across domain_leaves.rs and db/writer/seal.rs
  status: completed
  evidence: Extracted SharedOwnerKind, SharedOwnerRow, and hash_shared_owner_leaves (lexicographic kind.as_str() sort) in db/commitments/domain_leaves.rs and reused across domain_leaves.rs, db/writer/seal.rs, and db/writer/ingest.rs.
- subtask_ref: replace-db-writer-too-many-arguments-with-param-structs
  title: 'Introduce typed PersistFilesParams/PersistGraphParams structs in db/writer/{mod,ingest}.rs and remove #[allow(clippy::too_many_arguments)]'
  status: completed
  evidence: 'Replaced all 4 #[allow(clippy::too_many_arguments)] suppressions in db/writer/{mod,ingest}.rs via PopulateOwnedParams, PersistFilesParams, and PersistGraphParams and updated all callers via extended scope.'
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:index-wire-tags-detail-keys-and-metadata-constants.deliver
  gate_id: deliver
  worker_id: reviewer-t2
  claim_revision: 1
  contract_revision: 2
  candidate_tree: e5b46dcd45fedfef52c1fb75298b877f8235f59f
  committed_oid: 75fbc2bf7bad3ed31dfb7647080b699010d7acb3
  accepted_at: 2026-11-21T20:32:44.918Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: NodeDetailKey covers all persisted node detail JSON keys, and MetadataKey + read_metadata + MAX_QUERY_RESULT_BYTES eliminate hardcoded metadata key queries and 8 MiB literals across db/commitments, db/cycles, and db/writer.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: SharedOwnerKind and hash_shared_owner_leaves in db/commitments own shared_owners leaf projection and hashing across both cold/delta writer sealing (db/writer/seal.rs) and commitment verification (db/commitments/domain_leaves.rs).'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: db/writer/{mod,ingest}.rs pass typed parameter structs instead of suppressing #[allow(clippy::too_many_arguments)].'
      verified: []
```

### task: modularize-db-reader-and-unify-query-projections

```yaml
task_ref: modularize-db-reader-and-unify-query-projections
target: Decompose db/reader.rs into focused db/reader/ submodules, introduce OverviewAspect and ExplainDirection enums, wire MetadataKey/NodeDetailKey/tags into reader and paging, unify governing_invariants and selector normalization, optimize paging::query and rows_with_mode, and retire non-_with_path reader wrappers
scope:
- crates/acdd-index/src/db/mod.rs
- crates/acdd-index/src/db/paging.rs
- crates/acdd-index/src/db/reader.rs
- crates/acdd-index/src/db/reader/
- tests/query_context/symbol_context.rs
- tests/query_context/diagnostics_and_source.rs
extended_scope:
- path: tests/query_context/ranking_and_coverage.rs
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
- path: tests/query_context/search_and_paging.rs
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
- path: tests/query_context/linked_workspaces.rs
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
- path: tests/languages/profiles/popular.rs
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
- path: tests/languages/html/syntax.rs
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
- path: docs/architecture/cross-platform.md
  reason: 'Retire reader::inspect_paged across non-overlapping test callers and co-evolve docs/architecture/cross-platform.md relative links from db/reader.rs to db/reader/open.rs per blueprint #260/#262'
depends_on:
- index-wire-tags-detail-keys-and-metadata-constants
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/reader/ submodules isolate open/confinement, SQL budget execution, search, inspect/explain, docs, diagnostics, and selectors behind db::reader re-exports with zero clones or dead_tails.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: governing_invariants and governing_invariants_for_scope share one canonical query/projection helper, and selector URI normalization lives in one helper.'
- 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: OverviewAspect and ExplainDirection parse once at the db/reader boundary, and MetadataKey/NodeDetailKey/EdgeKind/NodeKind replace raw metadata and detail strings in db/reader and db/paging.'
- 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: paging::query avoids redundant double execution of CTEs via COUNT(*) OVER() or single-pass counting, and rows_with_mode avoids redundant row re-serialization for byte accounting.'
acceptance_criteria:
- 'AC-1: db/reader.rs is split into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs with all public signatures re-exported cleanly from db::reader.'
- 'AC-2: OverviewAspect replaces duplicated VALID_OVERVIEW_ASPECTS string lists and raw aspect string matching, ExplainDirection replaces raw direction strings in db/reader and db/paging::attach_relations, MetadataKey/read_metadata replaces all raw metadata queries in reader and paging, governing_invariants and governing_invariants_for_scope share one unified helper (0 duplicate blocks), and reader/selectors share one strip_selector_uri_prefix helper.'
- 'AC-3: paging::query eliminates double query execution on heavy CTEs while preserving exact total/has_more/generation semantics, rows_with_mode eliminates double serde_json::to_writer overhead, and reader::inspect_paged / reader::explain_with_options are retired in favor of *_with_path* (with tests/query_context/{symbol_context,diagnostics_and_source}.rs migrated).'
contract_revision: 3
status: completed
subtasks:
- subtask_ref: split-db-reader-submodules
  title: Decompose db/reader.rs into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs preserving db::reader exports
  status: completed
  evidence: 'Deleted crates/acdd-index/src/db/reader.rs (2,380 LOC), removed #[path = "reader/selectors.rs"], decomposed into db/reader/{mod,open,sql,search,inspect,docs,diagnostics,selectors}.rs with all public and pub(crate) re-exports preserved cleanly (except retired inspect_paged/explain_with_options); co-evolved docs/architecture/cross-platform.md:41,47 to db/reader/open.rs (docs check passed).'
- subtask_ref: type-overview-and-direction-enums-and-unify-projections
  title: Introduce OverviewAspect and ExplainDirection enums, wire MetadataKey/NodeDetailKey/tags, deduplicate governing_invariants and selector URI normalization, and migrate tests off reader::inspect_paged/explain_with_options
  status: completed
  evidence: Introduced OverviewAspect (15 variants, ALL/as_str/is_discovery/FromStr) and ExplainDirection (Both/Incoming/Outgoing, parse_optional/FromStr) in db/reader/mod.rs; wired MetadataKey/read_metadata*, NodeDetailKey/NodeDetailFlags, EdgeKind/NodeKind/CoverageStatus, and MAX_QUERY_RESULT_BYTES across db/reader/* and db/paging.rs; unified governing_invariants* on governing_invariants_for_cte and strip_selector_uri_prefix in db/reader/selectors.rs; retired reader::{inspect_paged,explain_with_options} and migrated all 6 non-overlapping test files (tests/query_context/{symbol_context,ranking_and_coverage,search_and_paging,linked_workspaces}.rs, tests/languages/{html/syntax,profiles/popular}.rs).
- subtask_ref: optimize-paging-count-and-rows-byte-accounting
  title: Optimize paging::query double-execution and rows_with_mode byte counting while preserving exact 8 MiB / 2s budgets
  status: completed
  evidence: Updated paging::query_with_mode to single-pass SELECT COUNT(*) OVER() AS __acdd_total, _q.* with index-0 stripping in rows_with_window_total and SELECT count(*) fallback only on items.is_empty() && options.offset > 0; replaced redundant whole-row serde_json::to_writer in rows_with_mode with exact incremental byte accounting (i64_decimal_len/exact_json_str_bytes/estimate_json_bytes) verified against serde_json::to_vec and 8 MiB / 2s limits in db::reader::tests (77/77 acdd-index --lib, 49/49 query_context, 18/18 commitment_integrity passed).
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-db-reader-and-unify-query-projections.deliver
  gate_id: deliver
  worker_id: reviewer-t3
  claim_revision: 1
  contract_revision: 2
  candidate_tree: be2ca3b810f23b5ca4c374ba56cdcd19ad33d253
  committed_oid: d02164ad1fe2ddf297de6acafd1c5249db9ecf86
  accepted_at: 2026-11-21T21:11:47.793Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/reader/ submodules isolate open/confinement, SQL budget execution, search, inspect/explain, docs, diagnostics, and selectors behind db::reader re-exports with zero clones or dead_tails.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: governing_invariants and governing_invariants_for_scope share one canonical query/projection helper, and selector URI normalization lives in one helper.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: OverviewAspect and ExplainDirection parse once at the db/reader boundary, and MetadataKey/NodeDetailKey/EdgeKind/NodeKind replace raw metadata and detail strings in db/reader and db/paging.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: paging::query avoids redundant double execution of CTEs via COUNT(*) OVER() or single-pass counting, and rows_with_mode avoids redundant row re-serialization for byte accounting.'
      verified: []
```

### task: modularize-semantic-linker-and-ast-contexts

```yaml
task_ref: modularize-semantic-linker-and-ast-contexts
target: Decompose semantic.rs (2,568 LOC) and phase_value_flow (1,974 LOC) into focused submodules, unify split_python_pipe_types and required_configuration_facts across languages/ and linker/, replace ast/ and manifests/semantic.rs too_many_arguments suppressions with typed context structs, and wire models::tags and NodeDetailKey constants
scope:
- crates/acdd-index/src/semantic.rs
- crates/acdd-index/src/semantic/
- crates/acdd-index/src/ast/mod.rs
- crates/acdd-index/src/ast/search.rs
- crates/acdd-index/src/ast/visitor.rs
- crates/acdd-index/src/languages/manifests/semantic.rs
- crates/acdd-index/src/languages/python/mod.rs
- crates/acdd-index/src/languages/python/value_flow.rs
- crates/acdd-index/src/languages/typescript/linker.rs
- crates/acdd-index/src/linker/python/value_flow.rs
- crates/acdd-index/src/linker/semantic_context/components.rs
- crates/acdd-index/src/linker/resolution/value_flow_phase/
- tests/linker/
extended_scope:
- path: crates/acdd-index/src/languages/mod.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/vue/mod.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/html/mod.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/html/javascript.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/python/projections.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/python/syntax_operations.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: crates/acdd-index/src/languages/typescript/projections.rs
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: docs/architecture/indexing.md
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
- path: docs/architecture/performance.md
  reason: Update extract_tree_with_language_context and manifests::semantic callers to typed context structs, and update semantic.rs links in indexing.md and performance.md
depends_on:
- index-wire-tags-detail-keys-and-metadata-constants
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-index/src/semantic.rs is split into semantic/{mod,catalog,types,relations,builtins}.rs and phase_value_flow (1,974 LOC) in linker/resolution/value_flow_phase/mod.rs is decomposed into focused phase helpers.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: split_python_pipe_types and required_configuration_facts each have a single canonical definition shared between languages/ and linker/.'
- 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: All 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs are replaced by typed context structs, and NodeDetailKey/models::tags constants replace raw node/reference/language strings.'
acceptance_criteria:
- 'AC-1: crates/acdd-index/src/semantic.rs (2,568 LOC) is decomposed into semantic/{mod,catalog,types,relations,builtins}.rs preserving all public acdd_index::semantic re-exports, and phase_value_flow (1,974 LOC) in linker/resolution/value_flow_phase/ is split into focused phase submodules with no function exceeding 350 LOC.'
- 'AC-2: The 28-line split_python_pipe_types clone (languages/python/value_flow.rs vs linker/python/value_flow.rs) and 14-line required_configuration_facts clone (languages/typescript/linker.rs vs linker/semantic_context/components.rs) are consolidated into single canonical helpers.'
- 'AC-3: All #[allow(clippy::too_many_arguments)] attributes in ast/mod.rs, ast/search.rs, and languages/manifests/semantic.rs (8 total) are removed in favor of typed context structs, and NodeDetailKey / models::tags constants are wired across semantic/, ast/, and value_flow_phase/.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: split-semantic-rs-and-phase-value-flow
  title: Decompose crates/acdd-index/src/semantic.rs into semantic/{mod,catalog,types,relations,builtins}.rs and split phase_value_flow (1,974 LOC) into focused phase submodules
  status: completed
  evidence: Decomposed semantic.rs (2,568 LOC) into semantic/{mod,catalog,types,relations,builtins}.rs preserving all acdd_index::semantic::* exports, updated docs/architecture/{indexing,performance}.md links, and split phase_value_flow into value_flow_phase/{mod,context,parameters,dispatch,inferred,candidates}.rs with max function LOC <= 350.
- subtask_ref: deduplicate-python-pipe-types-and-ts-config-facts
  title: Unify split_python_pipe_types and required_configuration_facts across languages/ and linker/
  status: completed
  evidence: Consolidated split_python_pipe_types in languages/python/value_flow.rs (reused by linker/python/value_flow.rs) and unified component-environment config/provider helpers in languages/manifests/semantic.rs (reused by languages/typescript/linker.rs and linker/semantic_context/components.rs) while preserving required_configuration_facts signature.
- subtask_ref: replace-ast-and-manifest-too-many-arguments-and-wire-tags
  title: 'Replace 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs with typed context structs and wire NodeDetailKey/models::tags'
  status: completed
  evidence: 'Removed all 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs via typed context structs, wired NodeDetailKey/models::tags across semantic/, ast/, and value_flow_phase/, and migrated tests/linker/overload_disambiguation.rs:224 to reader::inspect_paged_with_path.'
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-semantic-linker-and-ast-contexts.deliver
  gate_id: deliver
  worker_id: reviewer-t4
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 20b8ef3c0c6def731b3185dd20279e31ebba9c45
  committed_oid: c2b9cda18df47d7182170ffe013e4f38323ea548
  accepted_at: 2026-11-21T21:20:37.721Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-index/src/semantic.rs is split into semantic/{mod,catalog,types,relations,builtins}.rs and phase_value_flow (1,974 LOC) in linker/resolution/value_flow_phase/mod.rs is decomposed into focused phase helpers.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: split_python_pipe_types and required_configuration_facts each have a single canonical definition shared between languages/ and linker/.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: All 8 #[allow(clippy::too_many_arguments)] suppressions in ast/{mod,search}.rs and languages/manifests/semantic.rs are replaced by typed context structs, and NodeDetailKey/models::tags constants replace raw node/reference/language strings.'
      verified: []
    review_summary: Delivered modularize-semantic-linker-and-ast-contexts from reviewed candidate tree 20b8ef3c.
```

### task: state-machine-sqlite-dedup-and-test-hygiene

```yaml
task_ref: state-machine-sqlite-dedup-and-test-hygiene
target: Extract shared require_active_claimable_node guard in acdd-state::machine, deduplicate store/sqlite/tx.rs::admit_claim UPDATE, decompose context.rs into context/ submodules, and relocate inline integration test suites from profile/mod.rs, validate.rs, context.rs, and store/sqlite/mod.rs into crates/acdd-state/tests/state_integration/
scope:
- crates/acdd-state/src/machine/
- crates/acdd-state/src/store/sqlite/mod.rs
- crates/acdd-state/src/store/sqlite/tx.rs
- crates/acdd-state/src/profile/mod.rs
- crates/acdd-state/src/validate.rs
- crates/acdd-state/src/context.rs
- crates/acdd-state/src/context/
- crates/acdd-state/src/harness.rs
- crates/acdd-state/tests/
extended_scope:
- path: docs/reference/tasks.md
  reason: 'Co-evolve active Markdown hyperlinks from deleted context.rs to context/mod.rs and context/markdown.rs per AR-002 blueprint #260/#262'
- path: docs/architecture/specification-and-design-lifecycle.md
  reason: 'Co-evolve active Markdown hyperlinks from deleted context.rs to context/mod.rs and context/markdown.rs per AR-002 blueprint #260/#262'
- path: docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
  reason: 'Co-evolve active Markdown hyperlinks from deleted context.rs to context/mod.rs and context/markdown.rs per AR-002 blueprint #260/#262'
- path: docs/reference/mcp-markdown-contracts.md
  reason: 'Co-evolve active Markdown hyperlinks from deleted context.rs to context/mod.rs and context/markdown.rs per AR-002 blueprint #260/#262'
- path: docs/reference/acdd.md
  reason: 'Co-evolve active Markdown hyperlinks from deleted context.rs to context/mod.rs and context/markdown.rs per AR-002 blueprint #260/#262'
depends_on:
- core-constants-ports-and-config-modularization
tags:
- default
invariants:
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: machine/{claim,submit,scope,verify}.rs validate active run and claimable gate nodes through one require_active_claimable_node helper, tx.rs::admit_claim executes a single parameterized UPDATE statement, and build_sample_graph test fixtures share one canonical helper.'
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-state/src/context.rs (2,260 LOC) is decomposed into context/{mod,budget,tiers,markdown}.rs with named pub const tier/budget limits.'
- 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: Inline integration test suites in profile/mod.rs (1,485 LOC), validate.rs (641 LOC), context.rs (518 LOC), and store/sqlite/mod.rs (473 LOC) live under crates/acdd-state/tests/state_integration/ per crates/acdd-state/AGENTS.md.'
acceptance_criteria:
- 'AC-1: require_active_claimable_node in crates/acdd-state/src/machine/mod.rs replaces the duplicated active-run + node_cursor + find_node + RootGate|TaskGate guard across machine/{claim,submit,scope,verify}.rs, and store/sqlite/tx.rs::admit_claim collapses its two 18-line execute branches into one SQL query with (?12 IS NULL OR node_version = ?12).'
- 'AC-2: crates/acdd-state/src/context.rs is split into context/{mod,budget,tiers,markdown}.rs preserving all public re-exports and exposing canonical tier budget constants.'
- 'AC-3: Inline integration tests from profile/mod.rs, validate.rs, context.rs, and store/sqlite/mod.rs are moved into crates/acdd-state/tests/state_integration/ (reducing profile/mod.rs from 1,540 LOC to < 80 LOC), duplicate build_sample_graph helpers are unified in harness.rs, and cargo test -p acdd-state passes with identical test coverage.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: unify-machine-node-guard-and-sqlite-admit-claim-update
  title: Extract require_active_claimable_node in machine/mod.rs and collapse store/sqlite/tx.rs::admit_claim into one parameterized UPDATE
  status: completed
  evidence: Extracted require_active_node and require_active_claimable_node in crates/acdd-state/src/machine/mod.rs and replaced duplicated guards across machine/{claim,submit,scope,verify,reopen}.rs; collapsed store/sqlite/tx.rs::admit_claim UPDATE gate_nodes into one parameterized SQL query with (?12 IS NULL OR node_version = ?12). Verified with cargo test -p acdd-state (70/70) and cargo clippy.
- subtask_ref: split-state-context-submodules
  title: Decompose crates/acdd-state/src/context.rs into context/{mod,budget,tiers,markdown}.rs with canonical budget constants
  status: completed
  evidence: Deleted crates/acdd-state/src/context.rs and split into context/{mod,budget,tiers,markdown}.rs with named pub const limits, ACDD_DIR and SubsystemId::Blackboard adoption, latest_container_findings deduplication, and updated Markdown links across 5 extended-scope doc files. Verified with cargo test -p acdd-state (70/70) and cargo run -q -- docs check.
- subtask_ref: relocate-inline-state-tests-and-dedup-fixtures
  title: Relocate inline integration test suites from profile/mod.rs, validate.rs, context.rs, and store/sqlite/mod.rs to tests/state_integration/ and deduplicate build_sample_graph
  status: completed
  evidence: Moved inline integration tests from profile/mod.rs (now 30 LOC), validate.rs, context.rs, and store/sqlite/mod.rs into tests/state_integration/{profile,validate,context,sqlite}.rs wired in state_integration.rs; unified build_sample_graph and canonical_profile into RunHarness in harness.rs. Verified 20 lib + 50 state_integration = 70/70 passed, 18/18 commitment_integrity passed.
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:state-machine-sqlite-dedup-and-test-hygiene.deliver
  gate_id: deliver
  worker_id: reviewer-t5-2
  claim_revision: 1
  contract_revision: 2
  candidate_tree: af509e0be846a586b07f959a56d2f254f46de1a0
  committed_oid: c53e8327d08ca11d2c7ee8011711ea3c32bdb969
  accepted_at: 2026-11-21T21:16:22.368Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: machine/{claim,submit,scope,verify}.rs validate active run and claimable gate nodes through one require_active_claimable_node helper, tx.rs::admit_claim executes a single parameterized UPDATE statement, and build_sample_graph test fixtures share one canonical helper.'
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: crates/acdd-state/src/context.rs (2,260 LOC) is decomposed into context/{mod,budget,tiers,markdown}.rs with named pub const tier/budget limits.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: Inline integration test suites in profile/mod.rs (1,485 LOC), validate.rs (641 LOC), context.rs (518 LOC), and store/sqlite/mod.rs (473 LOC) live under crates/acdd-state/tests/state_integration/ per crates/acdd-state/AGENTS.md.'
      verified:
      - INV-AR002-SINGLE-OWNER-DEDUPLICATION
      - INV-AR002-MODULAR-MONOLITH-DECOMPOSITION
      - INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE
    review_summary: Delivered state-machine-sqlite-dedup-and-test-hygiene after round-2 independent review pass (candidate_tree af509e0be846a586b07f959a56d2f254f46de1a0).
```

### task: modularize-db-symbols-and-deduplicate-search-options

```yaml
task_ref: modularize-db-symbols-and-deduplicate-search-options
target: Decompose db/symbols.rs into focused db/symbols/ submodules, consolidate duplicate ExplainOptions structs onto ExplainDirection, deduplicate db/symbols/search.rs SQL predicates and FTS tokenization, wire MetadataKey/NodeDetailKey/tags, and retire phantom-tested non-_with_path symbol wrappers
scope:
- crates/acdd-index/src/db/reader/
- crates/acdd-index/src/db/symbols.rs
- crates/acdd-index/src/db/symbols/
- tests/query_context/ranking_and_coverage.rs
- tests/query_context/search_and_paging.rs
extended_scope:
- path: crates/acdd-index/src/db/paging.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: crates/acdd-index/src/db/traversal.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: tests/query_context/symbol_context.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: tests/query_context/diagnostics_and_source.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: tests/mcp/tool_evolution/cli_and_relations.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: docs/architecture/performance.md
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: src/cli/mod.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
- path: src/mcp/tools.rs
  reason: 'Extend scope per blueprint #260/#262 for ExplainDirection tightening (paging.rs, traversal.rs:3431-3439, cli/mod.rs:2719, mcp/tools.rs:1086) and retiring phantom-tested symbol wrappers across test and doc callers'
depends_on:
- modularize-db-reader-and-unify-query-projections
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/symbols/ submodules isolate confined source caching, line-FTS/centered previews, search navigation and SQL building, inspect/explain composition, and test discovery cleanly behind db::symbols re-exports with zero >= 5-line clones.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: ExplainOptions has one canonical definition/composition over ExplainDirection without manual cross-module reconstruction, and db/symbols/search.rs owns one shared SQL kind/path/doc filter builder and FTS tokenizer.'
acceptance_criteria:
- 'AC-1: db/symbols.rs (1,531 LOC) is split into db/symbols/{mod,source,preview,inspect,search,test_discovery}.rs preserving public db::symbols re-exports, wiring MetadataKey/NodeDetailKey/tags, and keeping VerifiedSourceCache/verified_source isolated in db/symbols/source.rs.'
- 'AC-2: Duplicate reader::ExplainOptions vs symbols::ExplainOptions structs are consolidated onto one canonical ExplainDirection-backed contract without duplicating fields or reconstructing reader::ExplainOptions inside db/symbols.'
- 'AC-3: db/symbols/search.rs extracts shared kind/path/doc SQL filter builders and FTS query tokenization (eliminating all >= 5-line duplicate blocks in db/symbols/search.rs), and phantom-tested symbols::{inspect_with_options, explain_with_options, tests_paged} wrappers are retired onto *_with_path* (with tests/query_context/{ranking_and_coverage,search_and_paging}.rs migrated).'
contract_revision: 2
status: completed
subtasks:
- subtask_ref: split-db-symbols-submodules
  title: Decompose db/symbols.rs into db/symbols/{mod,source,preview,inspect,search,test_discovery}.rs preserving db::symbols exports and wiring MetadataKey/NodeDetailKey/tags
  status: completed
  evidence: Deleted db/symbols.rs and split into db/symbols/{mod,source,preview,inspect,search,test_discovery}.rs preserving pub and pub(crate) re-exports and wiring MetadataKey, NodeDetailKey, and models::tags
- subtask_ref: consolidate-explain-options-and-dedup-search-sql
  title: Unify ExplainOptions over ExplainDirection and extract shared SQL filter/FTS builders in db/symbols/search.rs
  status: completed
  evidence: Consolidated reader::ExplainOptions and symbols::ExplainOptions over ExplainDirection, tightened paging::populate_edge_relations, updated db/traversal.rs:3431-3439, src/cli/mod.rs:2719, and src/mcp/tools.rs:1086, and deduplicated search.rs SQL filters, FTS tokenization, and exact query builders
- subtask_ref: retire-phantom-tested-symbol-wrappers
  title: Retire symbols::{inspect_with_options, explain_with_options, tests_paged} onto *_with_path* and migrate tests/query_context/{ranking_and_coverage,search_and_paging}.rs
  status: completed
  evidence: Retired symbols::{inspect_with_options, explain_with_options, tests_paged} onto *_with_path* and migrated tests/query_context/{ranking_and_coverage,search_and_paging,symbol_context,diagnostics_and_source}.rs, tests/mcp/tool_evolution/cli_and_relations.rs, and docs/architecture/performance.md:88
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-db-symbols-and-deduplicate-search-options.deliver
  gate_id: deliver
  worker_id: reviewer-t6
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 7b0440fefa6af323a72d9a144c8c1be327f60ccd
  committed_oid: 75c092f852e85ca7fbf51151c7133cd3abe3bc04
  accepted_at: 2026-11-21T21:57:12.760Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/symbols/ submodules isolate confined source caching, line-FTS/centered previews, search navigation and SQL building, inspect/explain composition, and test discovery cleanly behind db::symbols re-exports with zero >= 5-line clones.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: ExplainOptions has one canonical definition/composition over ExplainDirection without manual cross-module reconstruction, and db/symbols/search.rs owns one shared SQL kind/path/doc filter builder and FTS tokenizer.'
      verified: []
```

### task: modularize-state-usecases-roadmap-deps-and-lifecycle

```yaml
task_ref: modularize-state-usecases-roadmap-deps-and-lifecycle
target: Decompose acdd-state usecases/{roadmap.rs (2,593 LOC), deps.rs (2,325 LOC), lifecycle.rs (2,128 LOC)} into focused submodules, wire acdd-core workspace constants and SubsystemId across usecases/, and deduplicate sample_snapshot test fixtures
scope:
- crates/acdd-state/AGENTS.md
- crates/acdd-state/src/usecases/mod.rs
- crates/acdd-state/src/usecases/roadmap.rs
- crates/acdd-state/src/usecases/roadmap/
- crates/acdd-state/src/usecases/deps.rs
- crates/acdd-state/src/usecases/deps/
- crates/acdd-state/src/usecases/lifecycle.rs
- crates/acdd-state/src/usecases/lifecycle/
- crates/acdd-state/src/usecases/queries.rs
- crates/acdd-state/src/usecases/archive_notes.rs
extended_scope:
- path: crates/acdd-state/src/usecases/commands
  reason: Replace raw workspace literals (SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY, ACDD_DIR, ACDD_CONFIG_FILE, WORKTREES_DIR) and SubsystemId checks across usecases/ per INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS and AC-2
- path: crates/acdd-state/src/usecases/submit
  reason: Replace raw workspace literals (SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY, ACDD_DIR, ACDD_CONFIG_FILE, WORKTREES_DIR) and SubsystemId checks across usecases/ per INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS and AC-2
depends_on:
- state-machine-sqlite-dedup-and-test-hygiene
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: usecases/{roadmap,deps,lifecycle}.rs (7,046 LOC combined) are decomposed into focused single-responsibility submodules under usecases/{roadmap,deps,lifecycle}/ preserving all pub and pub(crate) exports.'
- 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: usecases/ reuses canonical acdd-core workspace constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and SubsystemId instead of raw string literals.'
acceptance_criteria:
- 'AC-1: crates/acdd-state/src/usecases/roadmap.rs (2,593 LOC) is split into usecases/roadmap/{mod,discovery,graph,waves,render}.rs; usecases/deps.rs (2,325 LOC) is split into usecases/deps/{mod,syntax,snapshot,validate,external}.rs; and usecases/lifecycle.rs (2,128 LOC) is split into usecases/lifecycle/{mod,workspace,specs,runs,io}.rs.'
- 'AC-2: Raw workspace path/filename literals and subsystem string checks across usecases/ are replaced by canonical acdd-core constants and SubsystemId, and duplicate sample_snapshot test helpers in usecases/{archive_notes,queries}.rs are unified.'
- 'AC-3: crates/acdd-state/AGENTS.md documents the modular usecases/{roadmap,deps,lifecycle}/ and context/ layout, and cargo test -p acdd-state passes cleanly.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: split-usecases-roadmap-deps-and-lifecycle
  title: Decompose usecases/{roadmap,deps,lifecycle}.rs into focused submodules under usecases/{roadmap,deps,lifecycle}/
  status: completed
  evidence: Decomposed usecases/{roadmap,deps,lifecycle}.rs into directory submodules under usecases/{roadmap,deps,lifecycle}/ preserving all pub/pub(crate)/pub(super) exports; clippy and cargo test -p acdd-state (20 lib + 50 integration) pass.
- subtask_ref: wire-core-constants-and-dedup-usecase-test-snapshots
  title: Adopt acdd-core workspace constants and SubsystemId across usecases/, unify sample_snapshot test fixtures, and update crates/acdd-state/AGENTS.md
  status: completed
  evidence: Replaced raw workspace literals and subsystem strings across usecases/ with acdd-core constants and SubsystemId, unified queries.rs test fixture onto archive_notes::tests::sample_snapshot, and updated crates/acdd-state/AGENTS.md.
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-state-usecases-roadmap-deps-and-lifecycle.deliver
  gate_id: deliver
  worker_id: reviewer-t7
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 0fae11920aeec16f513d79c9875bce0c1ca80a37
  committed_oid: 2cc5fa8a9d87ab485bc5e1682572ad9895b40daf
  accepted_at: 2026-11-21T21:48:13.205Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: usecases/{roadmap,deps,lifecycle}.rs (7,046 LOC combined) are decomposed into focused single-responsibility submodules under usecases/{roadmap,deps,lifecycle}/ preserving all pub and pub(crate) exports.'
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: usecases/ reuses canonical acdd-core workspace constants (ACDD_CONFIG_FILE, ACDD_DIR, WORKTREES_DIR, SNAPSHOT_REF_PREFIX, CLAIM_TRAILER_KEY) and SubsystemId instead of raw string literals.'
      verified: []
```

### task: modularize-db-traversal-and-bound-analysis-queries

```yaml
task_ref: modularize-db-traversal-and-bound-analysis-queries
target: Decompose db/traversal.rs into focused db/traversal/ submodules, introduce ImpactDirection/TraversalMode/GraphQueryOp enums, wire MetadataKey/NodeDetailKey/tags, bound dead_tails_paged and duplicates_paged page materialization, optimize directory_boundary, retire non-_with_path wrappers, and co-evolve indexing docs
scope:
- crates/acdd-index/src/db/traversal.rs
- crates/acdd-index/src/db/traversal/
- crates/acdd-index/AGENTS.md
- docs/architecture/indexing.md
- tests/query_context/
- tests/mcp/tool_evolution/
extended_scope:
- path: crates/acdd-index/src/db/paging.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
- path: tests/incremental/directory_slice.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
- path: tests/mcp/context/entrypoints_and_delta.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
- path: tests/languages/features.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
- path: tests/languages/sql.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
- path: tests/languages/python/frameworks/import_provenance.rs
  reason: 'Extend scope per AR-002 blueprint #260/#262 and #272: migrate 5 non-overlapping test callers off retired traversal wrappers (and update directory_slice ordering expectation for native column sort) plus fix SQLITE_TOOBIG in db/paging.rs for tests/mcp budget tests'
depends_on:
- modularize-db-symbols-and-deduplicate-search-options
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/traversal/ submodules isolate impact/BFS, directory slice, Git change_impact, duplicates, and dead_tails engines cleanly behind db::traversal re-exports.'
- 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: ImpactDirection, TraversalMode, and GraphQueryOp parse once at the db/traversal boundary and drive all internal BFS/SQL edge-kind and operation dispatch without raw string matching.'
- 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: dead_tails_paged and duplicates_paged materialize JSON Value payloads only for the requested page slice, duplicates avoids per-line String cloning, and directory_boundary sorts without ORDER BY json_object(...).'
acceptance_criteria:
- 'AC-1: db/traversal.rs is split into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs reusing reader::strip_selector_uri_prefix, models::details::{NodeDetailFlags, NodeDetailKey}, MetadataKey, models::tags, MAX_QUERY_RESULT_BYTES, and typed ImpactDirection/TraversalMode/GraphQueryOp enums parsed at the boundary.'
- 'AC-2: dead_tails_paged and duplicates_paged sort lightweight metadata structs and construct serde_json::Value objects only for [offset..offset+limit] items; duplicates shares one streaming slice-hash helper and avoids per-line Vec<String> cloning; directory_boundary replaces ORDER BY json_object(...) with native column sorting.'
- 'AC-3: traverse_with_options, removal_paged, and query_with_options (plus remaining tests_paged callers in tests/mcp/tool_evolution/) are collapsed onto *_with_path*, crates/acdd-index/AGENTS.md and docs/architecture/indexing.md are updated, and cargo test --test query_context --test mcp + code_map_query(duplicates, dead_tails) pass.'
contract_revision: 3
status: completed
subtasks:
- subtask_ref: split-db-traversal-submodules-type-enums-and-retire-wrappers
  title: Decompose db/traversal.rs into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs, introduce ImpactDirection/TraversalMode/GraphQueryOp enums, wire MetadataKey/NodeDetailKey/tags, and collapse *_with_options wrappers
  status: completed
  evidence: Decomposed db/traversal.rs into db/traversal/{mod,impact,slice,change_impact,duplicates,dead_tails}.rs, added ImpactDirection/TraversalMode/GraphQueryOp boundary enums, wired strip_selector_uri_prefix/NodeDetailFlags/NodeDetailKey/MetadataKey/tags/MAX_QUERY_RESULT_BYTES, promoted traverse_with_path_options to pub, retired traverse_with_options/removal_paged/query_with_options, and migrated all test callers.
- subtask_ref: bound-dead-tails-duplicates-and-directory-boundary
  title: Materialize JSON items only for paged slices in dead_tails_paged and duplicates_paged, share streaming slice hashing and drop per-line String clones in duplicates, and remove ORDER BY json_object(...) in directory_boundary
  status: completed
  evidence: 'Bounded dead_tails_paged and duplicates_paged JSON Value materialization strictly to [offset..offset+limit] via page_bounds, eliminated per-line Vec<String> cloning in duplicates via borrowed &str slices and shared hash_line_slice helper, replaced ORDER BY json_object(...) in directory_boundary with native column ordering, and fixed #272 SQLITE_TOOBIG window fallback in db/paging.rs.'
- subtask_ref: update-db-query-docs-and-verify-parity
  title: Update crates/acdd-index/AGENTS.md and docs/architecture/indexing.md and verify query_context, mcp, duplicates, and dead_tails
  status: completed
  evidence: Updated crates/acdd-index/AGENTS.md and docs/architecture/indexing.md; verified clippy -D warnings, acdd-index --lib (79/79), query_context (49/49), mcp (159/159), core directory_slice (3/3), languages features/sql/import_provenance seam, commitment_integrity (18/18), docs check, and 0 duplicates/dead_tails in db/traversal.
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-db-traversal-and-bound-analysis-queries.deliver
  gate_id: deliver
  worker_id: reviewer-t8
  claim_revision: 1
  contract_revision: 2
  candidate_tree: e6dc9d21f7408c48b3f9f931cfbf41e2b1ec27b6
  committed_oid: 9cc2781b55fb7d90728ed0f5da76ab4db07383a7
  accepted_at: 2026-11-21T22:34:53.538Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: db/traversal/ submodules isolate impact/BFS, directory slice, Git change_impact, duplicates, and dead_tails engines cleanly behind db::traversal re-exports.'
      - 'INV-AR002-TYPED-CONTRACTS-AND-CONTEXTS: ImpactDirection, TraversalMode, and GraphQueryOp parse once at the db/traversal boundary and drive all internal BFS/SQL edge-kind and operation dispatch without raw string matching.'
      - 'INV-AR002-BOUNDED-QUERY-AND-TEST-HYGIENE: dead_tails_paged and duplicates_paged materialize JSON Value payloads only for the requested page slice, duplicates avoids per-line String cloning, and directory_boundary sorts without ORDER BY json_object(...).'
      verified: []
```

### task: modularize-host-cli-mcp-state-bridge-and-engine

```yaml
task_ref: modularize-host-cli-mcp-state-bridge-and-engine
target: Decompose host monoliths src/cli/mod.rs (3,242 LOC), src/mcp/tools.rs (2,331 LOC), and src/state_bridge/code_domain.rs (2,467 LOC) into focused submodules, wire OverviewAspect/SubsystemId/EdgeKind/NodeKind constants, extract shared CLI/MCP/engine helpers, and deduplicate src/engine/{reader,pipeline/delta}.rs
scope:
- src/AGENTS.md
- src/cli/
- src/mcp/
- src/state_bridge/
- src/engine/reader.rs
- src/engine/pipeline/delta.rs
- tests/core/
- tests/acdd/
extended_scope:
- path: docs/reference/acdd.md
  reason: 'Update relative link from src/state_bridge/code_domain.rs to src/state_bridge/code_domain/mod.rs per AR-002 blueprint #260/#262'
depends_on:
- modularize-db-symbols-and-deduplicate-search-options
- modularize-state-usecases-roadmap-deps-and-lifecycle
- modularize-semantic-linker-and-ast-contexts
tags:
- default
invariants:
- 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs, and src/engine/pipeline/delta.rs::compute_delta_frontiers are decomposed into focused single-responsibility submodules and helpers.'
- 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CLI --adapter vs --config conflict resolution, MCP state_bridge query/command execution and worktree git-ancestor validation, engine/reader.rs acdd_read_tool blocks, and engine/pipeline/delta.rs frontier expansion blocks each have one canonical helper.'
- 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: cli::discovery uses db::reader::OverviewAspect and acdd_core::subsystems::SubsystemId, and state_bridge uses SubsystemId and models::tags::{EdgeKind, NodeKind} instead of raw string literals.'
acceptance_criteria:
- 'AC-1: src/cli/mod.rs (3,242 LOC; run_selected = 1,144 LOC) is decomposed into src/cli/{mod,args,discovery,dispatch_code,dispatch_state}.rs, deduplicates --adapter vs --config conflict validation across Build/Scan/Delta into resolve_cli_adapter_path, and replaces hardcoded aspect and subsystem strings in cli::discovery with OverviewAspect and SubsystemId.'
- 'AC-2: src/mcp/tools.rs (2,331 LOC) is decomposed into src/mcp/tools/{mod,inputs,code_tools,state_tools,dispatch}.rs, replaces the 4x repeated 7-argument state_bridge::execute_state_* blocks with ctx.execute_query/execute_command methods, and extracts the single-line worktree .git ancestor check (line 1644) into validate_explicit_worktree_git_ancestors.'
- 'AC-3: src/state_bridge/code_domain.rs (2,467 LOC) is split into src/state_bridge/code_domain/{mod,seam,signature_scope,context}.rs using EdgeKind/NodeKind/SubsystemId constants and deduplicating its 4x test assertion helper blocks; src/engine/reader.rs and src/engine/pipeline/delta.rs eliminate their >= 12-line duplicate blocks and decompose compute_delta_frontiers (748 LOC); and src/AGENTS.md is updated.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: split-cli-and-wire-overview-aspect-and-subsystem-id
  title: Decompose src/cli/mod.rs into src/cli/{mod,args,discovery,dispatch_code,dispatch_state}.rs, extract resolve_cli_adapter_path, and wire OverviewAspect/SubsystemId into cli::discovery
  status: completed
  evidence: Decomposed src/cli/mod.rs into src/cli/{mod,args,discovery,dispatch_code,dispatch_state}.rs, extracted resolve_cli_adapter_path across Build/Scan/Delta, and wired OverviewAspect/SubsystemId in cli::discovery and CLI dispatch.
- subtask_ref: split-mcp-tools-and-extract-bridge-helpers
  title: Decompose src/mcp/tools.rs into src/mcp/tools/{mod,inputs,code_tools,state_tools,dispatch}.rs, add ctx.execute_query/execute_command methods, and extract validate_explicit_worktree_git_ancestors
  status: completed
  evidence: 'Deleted src/mcp/tools.rs and decomposed into src/mcp/tools/{mod,inputs,code_tools,state_tools,dispatch}.rs removing legacy #[path], added StateCallContext::execute_query/execute_command across all 7 state_bridge calls, and extracted validate_explicit_worktree_git_ancestors.'
- subtask_ref: split-state-bridge-code-domain-and-dedup-engine-reader-delta
  title: Decompose src/state_bridge/code_domain.rs into submodules, deduplicate src/engine/{reader.rs,pipeline/delta.rs} and decompose compute_delta_frontiers, wire EdgeKind/NodeKind/SubsystemId, and update src/AGENTS.md
  status: completed
  evidence: Deleted src/state_bridge/code_domain.rs and decomposed into src/state_bridge/code_domain/{mod,seam,signature_scope,context}.rs wiring EdgeKind/NodeKind/SubsystemId and deduplicating git diff and test assertion helpers; deduplicated src/engine/reader.rs and decomposed/deduplicated src/engine/pipeline/delta.rs::compute_delta_frontiers; updated src/AGENTS.md and docs/reference/acdd.md:123.
receipt:
  run_id: AR-002-modularize-db-20261121191319-1314429-1
  node_cursor: root:modularize-host-cli-mcp-state-bridge-and-engine.deliver
  gate_id: deliver
  worker_id: reviewer-t9
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 05f5ed48aa83877f50435274ce70dbec4c4b7b99
  committed_oid: 3ac1fc977e224f866cb4388cc3060cdc37874320
  accepted_at: 2026-11-21T22:32:13.134Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR002-MODULAR-MONOLITH-DECOMPOSITION: src/cli/mod.rs, src/mcp/tools.rs, src/state_bridge/code_domain.rs, and src/engine/pipeline/delta.rs::compute_delta_frontiers are decomposed into focused single-responsibility submodules and helpers.'
      - 'INV-AR002-SINGLE-OWNER-DEDUPLICATION: CLI --adapter vs --config conflict resolution, MCP state_bridge query/command execution and worktree git-ancestor validation, engine/reader.rs acdd_read_tool blocks, and engine/pipeline/delta.rs frontier expansion blocks each have one canonical helper.'
      - 'INV-AR002-UNIFIED-CONSTANTS-AND-WIRE-TAGS: cli::discovery uses db::reader::OverviewAspect and acdd_core::subsystems::SubsystemId, and state_bridge uses SubsystemId and models::tags::{EdgeKind, NodeKind} instead of raw string literals.'
      verified: []
```
