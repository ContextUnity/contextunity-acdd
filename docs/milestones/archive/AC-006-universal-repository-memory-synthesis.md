---
id: AC-006-universal-repository-memory-synthesis
title: Universal specs in acdd.yaml, named items, explicit spec/path lenses, multi-spec runs, roadmap customization, and repository memory
doc_type: contract
status: completed
depends_on:
- AR-006-acdd-spec-crate-and-workflow-cutover
guidance:
  hitl: pro
owners:
- acdd.yaml
- .acdd/lint/
- .gitignore
- crates/acdd-core/src/
- crates/acdd-spec/src/
- crates/acdd-index/src/docs.rs
- crates/acdd-state/src/
- src/state_bridge/
- src/format/
- src/cli/
- src/mcp/
- tests/acdd/
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/adr/0014-declarative-spec-schemas-and-items.md
- docs/reference/
- docs/architecture/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
- docs/milestones/AC-006-universal-repository-memory-synthesis.md
invariants:
- 'INV-SINGLE-OWNER: SchemaDef validation, confined ref resolution for specs_dirs:/workflows_dirs:/lint_dirs:, spec parsing, named item extraction, spec_deltas verification, 4-tier context relevance ranking, and roadmap filtering each live in one canonical module with zero duplicate validators or parallel registries.'
- 'INV-CORE-LEAF: Pure spec-schema, roadmap-display, and shared confined ref-resolver types in crates/acdd-core stay free of SQLite, Git, HTTP, or direct filesystem traversal.'
- 'INV-SPEC-CRATE-BOUNDARY: Spec parsing, SchemaDef field/item validation, spec_deltas checking, and static roadmap filtering live in crates/acdd-spec (depending only on acdd-core); acdd-state owns live Run attachment and gate execution.'
- 'INV-STATE-NO-INDEX: crates/acdd-state never imports crates/acdd-index; specs.<name> and paths.<name> lenses read code-map doc edges strictly through host DocsDomainProvider in src/state_bridge/docs_domain/.'
- 'INV-SPEC-UNIVERSAL: Every specification document is a Spec whose shape, path, numbering, default workflow:, and roadmap: true|false membership live strictly in acdd.yaml specs: (inline, base: contract|memory, or external .acdd/specs/*.yaml ref:); workflow files contain zero specs: or roadmap: keys.'
- 'INV-EXPLICIT-CONTEXT-RANKING: Gates reference specs directly as specs.<name> and non-spec doc globs as paths.<name> (zero hidden specs->paths bridging, replacing docs.instructions and docs.adrs); Tier 6 context relevance ranks deterministically (1. direct ID/title citation -> 2. scope/owners overlap -> 3. code-map doc_edges -> 4. global ** fallback) bounded by max_bytes.'
- 'INV-MULTI-SPEC-SINGLE-WORKFLOW: One Run always executes one workflow and accepts 0..N attached input specs (acdd start [SPEC ...]) surfaced in ClaimResp.contract.attached_specs[] while validating 0..M output specs via docs.specs (with optional spec/required parameters).'
- 'INV-MEMORY-IS-A-SPEC: Repository memory is the built-in memory Spec (items.rules) written by a single-role memory_synthesis gate (scope: [specs.memories], checks: [docs.specs]) and delivered via context: [specs.memories] with zero memory-specific parser, status axis, or MCP tool.'
- 'INV-SCOPE-INTEGRITY: Cargo.toml, member Cargo.toml files, build.rs, and engine runtime files (.acdd/*.sqlite*) remain off-limits.'
acceptance_criteria:
- 'AC-01: GIVEN ADR 0014 admitted and ADR 0006 updated with the 4-tier context relevance policy, WHEN checked alongside the workspace, THEN both ADRs and the retrospectives .gitignore decision are recorded before implementation begins and tests/acdd/spec_schemas/ compiles cleanly.'
- 'AC-02: GIVEN any Spec document validated against built-in contract (default when frontmatter spec: is omitted), WHEN parsed by crates/acdd-spec, THEN existing milestones, plans, tasks, and workflows run identically with zero doc_type == "contract" branches; ephemeral --goal text lives under goal: rather than spec:.'
- 'AC-03: GIVEN acdd.yaml specs: entries using inline definitions, base: contract|memory, or external ref: files under specs_dirs: [.acdd/specs] and architecture/lint definitions under lint_dirs: [.acdd/lint], WHEN loaded through the shared confined ref resolver, THEN specs.<name> resolves fields, items, workflow:, and roadmap: true|false deterministically, .acdd/lint/*.yaml merges zones/rules/complexity/unused_public_api deterministically (failing closed on duplicate zone names, rule IDs, or scalar settings), inline architecture: in acdd.yaml is rejected by strict schema validation (zero legacy compatibility shim), and path traversal, multi-hop inheritance, or duplicate specs:/roadmap: keys inside .acdd/workflows/*.yaml fail closed.'
- 'AC-04: GIVEN a Spec declaring named items: collections (with optional singular item: token), WHEN validated in 3-step order (core shape -> SchemaDef fields/items -> closed DAG including cross-Spec refs RES-001#probe-a), THEN valid Specs pass and unknown fields fail closed with SPEC_FIELD_UNKNOWN (+ edit-distance hint), undeclared headings with SPEC_ITEM_UNKNOWN, and unresolved refs with ITEM_DEPENDENCY_UNKNOWN.'
- 'AC-05: GIVEN acdd start [SPEC ...] [--workflow <name>] [--goal "..."] with 0, 1, or N registered specs, WHEN the run starts and a worker claims a gate, THEN one resolved workflow executes and all attached specs appear in ClaimResp.contract.attached_specs[] within byte budgets; WHEN a gate runs docs.specs with { spec: <name>, required: true }, THEN verify fails closed if no valid file of that spec type was created or modified in gate scope.'
- 'AC-06: GIVEN gates referencing specs.<name> and workflow paths.<name> in scope: and context:, WHEN evaluated by DocsDomainProvider and workflow validation, THEN specs.<name> and paths.<name> expand scope globs and rank context items in strict 4-tier order (1. direct citation -> 2. scope/owners overlap -> 3. code-map edges -> 4. global ** fallback) bounded by max_bytes; legacy docs.instructions/docs.adrs and unknown names fail validation with redirect hints while empty globs emit a warning.'
- 'AC-07: GIVEN a Spec declaring spec_deltas against another Spec id-keyed list field, WHEN spec.deltas_applied runs in acdd verify, THEN every added id present, every removed id absent, and every modified id carrying its now text passes; omitting an addition, leaving a removal, or mismatching modified text fails closed.'
- 'AC-08: GIVEN acdd.yaml roadmap: display config (badges, group_by, color: auto|always|never) and Specs with explicit roadmap: true|false, WHEN acdd roadmap runs with --spec, --all, --tag, --field key=value, --group-by, or --color (respecting NO_COLOR), THEN only roadmap: true specs appear by default, configured badges/grouping render in CLI (`acdd_state` MCP remains active-run-only), and ANSI colors suppress cleanly under NO_COLOR or --color=never.'
- 'AC-09: GIVEN a milestone run with a rejected review and a single-role memory_synthesis gate targeting scope: [specs.memories], WHEN the builder emits a spec: memory file citing the real blackboard finding ID and runs acdd verify, THEN docs.specs passes, a subsequent task receives the relevant rule via context: [specs.memories], zero edits pass cleanly, and fabricating a non-existent blackboard evidence ID fails closed.'
- 'AC-10: GIVEN the completed AC-006 tree, WHEN tests/acdd/spec_schemas/acceptance.rs, acdd docs check, acdd validate, and acdd lint --categories arch,docs run, THEN ADR 0006, ADR 0014, docs/reference/workflows.md (including the complete specs.*/paths.*/code.* context table), docs/architecture/, skills/acdd/references/acdd-spec-examples/, ROADMAP.md, and CHANGELOG.md pass with zero errors.'
required_skills:
- acdd
- contract-planning
- code-quality
- test-suite-rules
- agent-instructions
closure:
  status: completed
  closed_at: 2026-11-28T01:57:17.669Z
  target_base: main
  target_base_commit: 278c69f92ac255f7b4f7a7e5b8d7d91ed188ce88
  task_receipts:
    acdd-yaml-specs-and-refs:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:acdd-yaml-specs-and-refs.deliver
      gate_id: deliver
      worker_id: reviewer-w1-config
      claim_revision: 1
      contract_revision: 3
      candidate_tree: 7eb53e7efca8c45364dad061d0d19908fce87f8b
      committed_oid: 2eea0d09cc61b69670e79203ce49a5c4a06dfe6f
      accepted_at: 2026-11-27T14:46:28.081Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Delivered acdd-yaml-specs-and-refs after independent review pass across all 6 contours (+939 production net LOC within approved_budget=1000).
    bootstrap-adr-and-seams:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:bootstrap-adr-and-seams.deliver
      gate_id: deliver
      worker_id: deliver-ac006-w0
      claim_revision: 1
      contract_revision: 1
      candidate_tree: aa6a9b7018d01f7c17e974bb0af936004e4ecda6
      committed_oid: 980268917cf3f12d7f72373cd3fae329b93419f8
      accepted_at: 2026-11-27T12:02:06.003Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
    docs-and-acceptance:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:docs-and-acceptance.deliver
      gate_id: deliver
      worker_id: reviewer-w6-docs-r3
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 55540c8f0508ec805fa8695ee48eb85f9ce1a892
      committed_oid: 019e010fae71d77735f8731d3238015e20f529f6
      accepted_at: 2026-11-27T20:29:12.648Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Delivered docs-and-acceptance (AC-10) after R3 independent review pass.
    memory-schema-and-gate:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:memory-schema-and-gate.deliver
      gate_id: deliver
      worker_id: reviewer-w5-memory-r2
      claim_revision: 1
      contract_revision: 1
      candidate_tree: 9c5aac063790e67217b18518cf672930d16936a8
      committed_oid: f78a231c35427dcc7835ded112583d376b433920
      accepted_at: 2026-11-27T19:50:29.485Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Deliver memory-schema-and-gate (AC-09) after Round 2 independent review PASS
    named-items-and-attached-specs:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:named-items-and-attached-specs.deliver
      gate_id: deliver
      worker_id: reviewer-w2-items-r3
      claim_revision: 1
      contract_revision: 6
      candidate_tree: 1f45ed1442e91d755b3389b844dc9cf4f064cf7c
      committed_oid: 500bb1d2c0d54a6918a7ad47c4b1eaa48b48a702
      accepted_at: 2026-11-27T16:36:38.712Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Delivered named-items-and-attached-specs after R3 independent review pass across all 6 contours.
    roadmap-display-and-schema-filters:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:roadmap-display-and-schema-filters.deliver
      gate_id: deliver
      worker_id: reviewer-w4-roadmap-r3
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 71cfac3ff0bfefb6b57dbec63536f887ae87f649
      committed_oid: 3ab97f0df99c9c24ee19d6a89073671c428c05b6
      accepted_at: 2026-11-27T19:13:09.072Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: Delivered roadmap-display-and-schema-filters after Round 3 independent review PASS
    spec-and-path-context-lenses:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:spec-and-path-context-lenses.deliver
      gate_id: deliver
      worker_id: reviewer-w3-paths-r3
      claim_revision: 1
      contract_revision: 5
      candidate_tree: 17552d4c5348d9956cfdccec4814d13730c1dfd6
      committed_oid: c324184802fc1fa145ea90592e0335b0d2718ad2
      accepted_at: 2026-11-27T18:11:52.353Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: 'Delivered spec-and-path-context-lenses (AC-06) after Round 3 independent review PASS closing all #663 findings.'
    spec-core-and-contract-schema:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:spec-core-and-contract-schema.deliver
      gate_id: deliver
      worker_id: reviewer-ac006-w1-core
      claim_revision: 1
      contract_revision: 2
      candidate_tree: fd5d08cf88c980696437dd84afa30f36ac618ec3
      committed_oid: 5ce79bebda47d9fc34712157649b3d99186a1970
      accepted_at: 2026-11-27T14:30:54.082Z
      rollup:
        verified_invariants:
          applicable: []
          verified:
          - INV-SINGLE-OWNER
          - INV-CORE-LEAF
          - INV-SPEC-CRATE-BOUNDARY
        review_summary: Delivered spec-core-and-contract-schema after independent review pass across all 6 contours.
    spec-deltas:
      run_id: AC-006-universal-rep-20261127105323-954942-1
      node_cursor: root:spec-deltas.deliver
      gate_id: deliver
      worker_id: reviewer-w4-deltas-r3
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 8fd10a72aacfb70dfbd7f01fe3c0a080d93ea602
      committed_oid: 1d1455c93f5ba72e8462ae80aed358eced195363
      accepted_at: 2026-11-27T19:14:15.693Z
      rollup:
        verified_invariants:
          applicable: []
          verified: []
        review_summary: 'Delivered spec-deltas after Round 3 independent review PASS (#677 closed, AC-07 verified, +778 <= 800 #675).'
  rollup:
    verified_invariants:
      applicable: []
      verified:
      - INV-SINGLE-OWNER
      - INV-CORE-LEAF
      - INV-SPEC-CRATE-BOUNDARY
    decisions:
    - |-
      AC-006 Architectural Seam Decisions:
      1. Shared Confined Ref Resolver & External Lint Rules: Place the single confined `<name> | <path>[:<sha256_prefix>]` resolver in `crates/acdd-core/src/config/` for `specs_dirs:`, `workflows_dirs:`, and `lint_dirs:`. Load and lexicographically merge `.acdd/lint/*.yaml` into hydrated `Adapter.architecture` (failing closed on duplicate zones, rule IDs, or conflicting scalars) while removing `architecture` from `AdapterFile` (zero inline shim) and hashing merged architecture into `adapter_index_identity`. Delegate `workflow/inherit.rs` to this resolver and migrate test fixtures using inline `architecture:` to `.acdd/lint/`.
      2. DomainProvider & Lens Boundaries (`INV-STATE-NO-INDEX`, `ADR-0004`): Route dynamic `specs.<name>` and `paths.<name>` context lenses through `DocsDomainProvider` in `src/state_bridge/docs_domain.rs` over one shared 4-tier deterministic relevance ranker (`budget_scored_items`), supplying resolved workflow globs and owning-spec context via `DomainContextReq` so `DocsDomainProvider` never parses workflows or opens `state.sqlite`. Register `SpecDomainProvider` (`domain_id = "spec"`, aligned with `SubsystemId::Spec`) in `src/state_bridge/mod.rs` (default-enabled in `CompositeDomainProvider`) for `spec.deltas_applied` delegating pure checks to `acdd_spec::deltas`.
      3. Memory Spec Tracking & Blackboard Evidence (`INV-MEMORY-IS-A-SPEC`, `INV-STATE-SQLITE-CONFINED`): Track `docs/memories/` in Git for durable `spec: memory` rule specs (`specs.memories`), narrowing `.gitignore` from `/docs/memories/` to `/docs/memories/retrospectives/` so transient telemetry retrospectives stay untracked while durable memory specs enter Git candidate trees cleanly. Verify cited blackboard evidence IDs for changed `spec: memory` files inside `crates/acdd-state/src/usecases/verify.rs` through `&dyn StateBackend` (batching blackboard ID lookup once, compatible with `RunHarness::in_memory()`).
      4. Roadmap & MCP Surface Coherence: Keep `acdd_state` MCP strictly active-run-only (preserving `tests/mcp` boundary redirects to CLI `acdd roadmap`); implement `roadmap: true|false` defaults, filters, badges, and grouping in `crates/acdd-spec/src/roadmap/` (directory module, not flat `roadmap.rs`) + `crates/acdd-spec/src/types.rs`, with pure ANSI rendering in `src/format/roadmap.rs` (color/TTY/`NO_COLOR` resolved in `src/cli/dispatch_state.rs` per `ARCH-FORMAT-LEAF`).
      5. Multi-Spec Run Persistence & Autonomous Scope Extensions: Persist `attached_specs` via SQLite migration v3 (`runs.attached_specs_json`) on `RunRecord` and project budgeted `ClaimResp.contract.attached_specs[]`. Same-wave task scopes in Waves 1 and 4 are strictly disjoint; builders use autonomous `extend_scope` strictly within admitted crate/module roots where live directory modules or shared ports/error codes require co-evolution (`crates/acdd-spec/src/roadmap/`, `crates/acdd-core/src/{errors,ports}.rs`, `crates/acdd-state/src/workflow/inherit.rs`, `src/state_bridge/mod.rs`, and migrated test fixtures).
    - |-
      AC-006 Decision Addendum (amending #638, addressing Finding #639):
      1. Retrospective Path & `.acdd/workflows/default.yaml` Alignment: Track `docs/memories/` in Git for durable `spec: memory` rule specs (`specs.memories`), narrow `.gitignore` from `/docs/memories/` to `/docs/memories/retrospectives/`, and align transient milestone telemetry retrospectives to `docs/memories/retrospectives/<spec.name>.md`. Because `.acdd/workflows/**` is protected during task gates, record this path alignment in Wave 0 (`ADR 0014`) and `memory-schema-and-gate`, and update `.acdd/workflows/default.yaml` (`coordinator` / `repo.memory_synthesis` guidance) under `polish` (which admits `.acdd/workflows/**`) or explicit authorization before `handoff` so `handoff` never creates an untracked file inside tracked `docs/memories/`.
      2. Builder Seam Reminders: `spec-and-path-context-lenses` updates `crates/acdd-state/src/context/markdown.rs` lens rendering for `paths.*`/`specs.*`; `spec-deltas` updates `src/state_bridge/common.rs` (`build_composite_providers`) alongside `crates/acdd-core/src/ports.rs` to default-enable `SubsystemId::Spec`.
      3. Blueprint Key Encoding: Resubmitting `proof.tasks` via `.acdd/tmp/ac006-blueprint.json` with verified plain unquoted `task_ref` keys matching the milestone manifest exactly.
    - 'Polish requirement before handoff (operator instruction): tighten acdd exec usage across AGENTS.md (remove ambiguous ''-C .worktrees/<name> (or ...)'' contradiction at line 139 and specify acdd exec --worker <worker_id> -- ... consistently with bash -lc for pipelines), .acdd/workflows/default.yaml (add acdd exec guidance into ClaimResp guidance), and skills/acdd/SKILL.md (show concrete worktree worker examples).'
    - |-
      BUDGET_OVERRIDE: approved for task acdd-yaml-specs-and-refs (+939 measured net LOC across shared config/refs.rs, loader.rs, schema.rs, external .acdd/lint/*.yaml migration, and validate.rs).
      approved_budget: 1000
    - 'BUDGET_OVERRIDE request for task named-items-and-attached-specs: implementation and all verification gates are complete and green (verify passed, lint 0 errors, duplicates 0, dead_tails 0, acdd-core 49/49, acdd-spec 3/3, acdd-state 22/22, state_integration 52/52, contextunity-acdd --lib 42/42, tests/acdd 29/29, commitment_integrity 11/11, /tmp leak check BEFORE=17 == AFTER=17), with measured net LOC +1188 production (1552+ / 364- across acdd-core/acdd-spec/acdd-state/src; +1833 total including +645 tests) vs blueprint max_net_loc=620 due to co-evolving AC-04 (### <item>: <ref>, 3-step validation, DIAG_* codes/hints, cross-Spec #item_ref DAG, Wave 2 #650 effective_builtin_base) and AC-05 (SQLite v3 runs.attached_specs_json, budgeted ClaimResp.contract.attached_specs[], multi-spec start CLI/MCP/ID resolution, parameterized docs.specs). Requesting coordinator/architect approval for approved_budget: 1250 before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved for task named-items-and-attached-specs (+1188 measured net LOC across acdd-core/errors+schema, acdd-spec/{spec,deps,discovery,schema}, SQLite v3 attached_specs migration, context budget/markdown, start [SPEC ...], and docs.specs required check).
      approved_budget: 1250
    - 'BUDGET_OVERRIDE request (R3 closure for #655): all 4 R2 findings are fixed and all verification gates are green (acdd verify passed, lint --categories all 0 errors, duplicates 0, dead_tails 0 in touched crates, Clippy clean, acdd-core 49/49, acdd-spec 3/3, acdd-state 22/22, state_integration 52/52, tests/acdd 29/29, commitment_integrity 11/11, /tmp/contextunity-test-* BEFORE=17 == AFTER=17). Measured production net LOC is +1334 (+1861 / -527 vs baseline 792d2fbe, up from +1215 in R2 due to custom-Spec archive/tombstone/restore collection-aware parsing across spec.rs/post_save.rs/submit/archive.rs/lifecycle/specs.rs/sqlite/mod.rs and attached_specs priority/budget guards, partially offset by removing SpecConfig::effective_base/effective_roadmap_for and bare render_archive_markdown). Requesting coordinator/architect approval for approved_budget: 1350 before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved for task named-items-and-attached-specs R3 (+1334 measured net LOC after closing all #655 archive/spec_dirs/attached_specs/effective_roadmap findings).
      approved_budget: 1350
    - 'BUDGET_OVERRIDE request for spec-and-path-context-lenses.build: measured prod net LOC is +840 (+1216/-376 across src/ and crates/*/src/, ~+813 excluding in-file #[cfg(test)] edits) vs max_net_loc=460. Breakdown: workflow/validate.rs (+274 for paths: validation, legacy docs.instructions/adrs hints, EMPTY_GLOB_MATCH), docs_domain.rs (+253 for unified 4-tier ranker shared by paths.*/specs.* and collection-based docs.invariants), context/{markdown,tiers}.rs (+138), specs.rs (+72), workflow/{inherit,schema,mod,embedded}/validate/graph/ports (+103). All verification checks (clippy -D warnings, acdd-core, acdd-state, docs_domain, tests/acdd, tests/mcp, commitment_integrity, lint/duplicates/dead_tails, acdd verify) pass cleanly. Requesting coordinator/architect BUDGET_OVERRIDE approval before submitting spec-and-path-context-lenses.build.'
    - |-
      BUDGET_OVERRIDE: approved for task spec-and-path-context-lenses (+840 measured net LOC across workflow/validate.rs paths/specs checks & redirect hints, DocsDomainProvider 4-tier ranking, context/{markdown,tiers}.rs, workflow schema/embedded migration, and DomainContextReq).
      approved_budget: 900
    - 'BUDGET_OVERRIDE update request (closing review #660 for spec-and-path-context-lenses.build): all 4 review findings are fixed and all verification checks are green (acdd verify execution_digest=650bf050e6809b4963483a935494cea1add3f37534a20f53ae1e36ab5b55b132, code.lint 0 warnings / 65 rules, duplicates 0, dead_tails 0, Clippy clean, acdd-core 49/49, acdd-state 22+52, docs_domain 1/1, tests/acdd 32/32, tests/mcp 169/169, commitment_integrity 11/11, /tmp/contextunity-test-* 17==17). Adding named doc.items support in provide_specs_lens plus machine/sync.rs TaskGate preservation changed numstat across src/ & crates/*/src/ from +840 (#659 approved_budget=900) to +918 (+1326/-408, ~+888 excl. in-file #[cfg(test)]). Requesting approved_budget: 950 before calling acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved for task spec-and-path-context-lenses R2 (+918 measured raw net LOC after fixing #660 container_scope, doc.items projection, strict tier-major knapsack ranking, and workflow example migration).
      approved_budget: 950
    - |-
      AC-006 Architecture Cleanup & Instruction Improvement Plan (user-directed):
      1. Architecture & Bug Cleanup (executed across remaining waves + root:polish before integration_review/handoff; Wave 4 spec-deltas MUST register separate src/state_bridge/spec_domain.rs per #638 and never grow docs_domain.rs):
         - Split monolithic src/state_bridge/docs_domain.rs (2094 LOC) into cohesive directory module src/state_bridge/docs_domain/{mod,lenses,ranking,specs_check,invariants,walk}.rs.
         - Extract one canonical worktree-skip predicate (.git/.worktrees/target/node_modules/.venv + .acdd/{run,tmp,extraction-cache}) into crates/acdd-core/src/path_policy.rs and share it across crates/acdd-state/src/workflow/validate.rs and src/state_bridge/docs_domain/.
         - Eliminate remaining doc_type != "contract" branches in crates/acdd-index/src/db/lint/docs_lint.rs:317,320 (AC-02 closure).
         - Align .acdd/workflows/default.yaml repo.memory_synthesis retrospective path to docs/memories/retrospectives/<spec.name>.md (matching #638/#640).
      2. Instruction Overhaul (root:polish across AGENTS.md, .acdd/workflows/default.yaml, skills/acdd/SKILL.md, skills/contract-planning/SKILL.md):
         - Unambiguous acdd exec usage (remove "-C .worktrees/<name> (or acdd exec ...)" contradiction in AGENTS.md; mandate acdd exec --worker <id> -- <cmd> and bash -lc for compound pipelines).
         - Builder Round-1 completeness & modularity rules: mandatory workspace-wide caller/sibling sweep via code_map_impact/ast_grep_search before verify (archive, sync, restore, integration, workflow examples), proactive directory-module split trigger (~600-800 prod LOC), UTF-8 char-boundary & empty-scope fail-closed checks, zero duplicate walkers/parsers.
         - Blueprint synthesis calibration rules: architect MUST run code_map_impact on breaking struct/config/port/error changes to build realistic scope: (preventing 15-27 file extend_scope storms), avoid bundling 2 independent ACs/subsystems in one task, calibrate max_net_loc realistically so BUDGET_OVERRIDE stays rare, verify live directory modules vs flat paths, and match milestone invariant prose to ADRs.
    - 'BUDGET_OVERRIDE request for task roadmap-display-and-schema-filters: all verification gates are green (acdd verify exit_code=0, execution_digest=3a7bf4795c129b890a4758b576253c2be6d43bc4155e47a0db01939b998fee80, code.lint 0 warnings/65 rules, duplicates=0, dead_tails=0, clippy -D warnings clean, acdd-spec --lib 3/3, format::roadmap 1/1, cli::dispatch_state 1/1, tests/acdd spec_schemas::roadmap 1/1, state_integration roadmap_query_waves 14/14, tests/mcp mcp_boundaries pass, commitment_integrity 11/11, docs check pass, validate 10/10, /tmp leak check 17==17). Measured prod numstat across src/ and crates/*/src/ is +710 (+798/-88, ~+626 excl. in-file #[cfg(test)]) vs blueprint max_net_loc=380 due to co-evolving DTOs (+42 types.rs), roadmap filtering/badges/grouping (+305 crates/acdd-spec/src/roadmap/), pure ANSI+grouped formatter (+228 src/format/), and CLI flags+TTY/NO_COLOR precedence (+135 src/cli/). Requesting coordinator/architect approval for approved_budget: 750 before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved for task roadmap-display-and-schema-filters (+710 measured raw net LOC / ~+626 excl. in-file tests across acdd-spec/roadmap/{discovery,mod,render}, types.rs, format/roadmap.rs, cli/{args,dispatch_state}, implementing AC-08 badges, group_by, color/NO_COLOR/TTY, and schema/tag/field filters).
      approved_budget: 750
    - 'BUDGET_OVERRIDE request for spec-deltas.build: implementation and all verification gates are complete and green (acdd verify passed=true, exit_code=0, execution_digest=17e258c266650f8e2902fdb0e69736fb44585cf1df66228299aa56512d4dfe75, lint --categories all 0 errors / 0 warnings in touched scopes, duplicates=0, dead_tails=0, Clippy -D warnings exit 0, acdd-core 49/49, acdd-spec 3/3, acdd-state 22+52, docs_domain 1/1, tests/acdd 34/34, tests/mcp harness_contract 8/8, commitment_integrity 11/11, docs check pass, validate 10/10, /tmp/contextunity-test-* 17==17). Measured production net LOC is +685 (+847/-162 across src/ and crates/*/src/) vs max_net_loc=260, driven by separating cohesive SpecDomainProvider into src/state_bridge/spec_domain.rs (+265) per #638/#664, pure acyclic crates/acdd-spec/src/deltas.rs (+347 with DeltaSpecView & 5 target representations), spec.rs DeltaSpecView impl (+47 net), and deduplicating shared specs-digest/selector helpers out of docs_domain.rs (-120 net) into common.rs (+137 net). Requesting coordinator/architect approval for approved_budget: 720 before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved for task spec-deltas (+685 measured raw net LOC across crates/acdd-spec/src/{deltas,spec,lib}.rs and src/state_bridge/{spec_domain,common,docs_domain,mod}.rs implementing SpecDomainProvider cleanly without growing docs_domain.rs).
      approved_budget: 720
    - 'BUDGET_OVERRIDE bump request for task spec-deltas (Round 2 rework closing all 3 #673 defects): measured prod net LOC is +774 (+246/-162 tracked crates/+src/ + 690 untracked deltas.rs & spec_domain.rs; docs_domain.rs unchanged at -120 net LOC) vs #670 approved_budget: 720 (+54 LOC from WorkspaceDeltaSpec schema-backed empty declared collection resolution, core SpecItem/TaskSpec fields in delta_named_entries, empty base-matched collection & cancelled-spec filtering, and target validation). Requesting coordinator approval for approved_budget: 800 before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved update for task spec-deltas R2 (+774 measured prod net LOC after closing all #673 schema-backed empty collection removal, core item/task fields, split_string_entry ID vs text separation, and base selector edge cases; docs_domain.rs remains -120).
      approved_budget: 800
    - |-
      BUDGET_OVERRIDE: approved update for task roadmap-display-and-schema-filters R3 (+807 raw prod net LOC incl. inline #[cfg(test)] in format/roadmap.rs + cli/dispatch_state.rs after unifying effective_spec_meta across render.rs and fixing group_by trim + planned counter for #676).
      approved_budget: 820
    - |-
      Operator/coordinator resolution for #688 integration_review findings:
      1. AC-08 Surface Authority (#688 #3): Keep MCP acdd_state strictly active-run-only (AGENTS.md 'acdd_state is active-run-only', #638 #4, tests/mcp boundary) and reconcile AC-08 / W4 prose in docs/milestones/AC-006-universal-repository-memory-synthesis.md (lines 49, 269, 280, 285) with CLI acdd roadmap + acdd_spec::roadmap::build_roadmap.
      2. Workflow Role Models (#688 #7): Explicit operator direction during AC-006 authorized updating .acdd/workflows/default.yaml role models to builder=[gemini-4-neon-low, gemini-4-neon-medium], independent_reviewer=[gemini-4-neon-xhigh, gemini-4-neon-high], and integration_reviewer=[gemini-4-neon-xhigh, gpt-6.2-sol].
    deferred_findings:
    - 'Non-blocking Wave 2 reminder (owner: named-items-and-attached-specs): SpecConfig::effective_base() returns "contract" whenever base is None, including external ref: specs (spec_ref.is_some()) or specs keyed "memory" without explicit base: memory. Check spec_ref.is_some() / collection key when resolving built-in base inheritance in Wave 2.'
    - 'Non-blocking R3 follow-ups (owner: docs-and-acceptance / polish): (1) In tests/acdd/spec_schemas/paths.rs:1083,1107, change ''See xéclair guide'' to ''See xÉclair Guide'' so the UTF-8 char-boundary advance branch at src/state_bridge/docs_domain.rs:1370 actually executes during the regression test (contains_ascii_ci_word uses to_ascii_lowercase(), so lowercase ''é'' skips the uppercase ''É'' title match before hitting line 1370; the production fix at line 1370 itself is provably UTF-8-safe). (2) In multi-gate tasks where only an earlier gate declares gate-level scope: [specs.<name>], container_scope(snapshot, TaskGate) deliberately isolates sibling TaskGate scopes; workflows attaching scope: to a task''s build gate must also declare it on follow-up review/deliver gates (or evaluate inheriting predecessor task-gate scopes in future polish).'
    - 'Non-blocking follow-up (owner: polish, outside roadmap-display-and-schema-filters admitted scope): crates/acdd-spec/src/discovery.rs:110 (match_spec_collection) computes .max_by_key(|(_, cfg)| cfg.patterns().into_iter().map(str::len).max().unwrap_or(0)) across ALL patterns of cfg rather than only patterns matching norm. If collection A has paths: ["docs", "some/very/long/unrelated/path"] and collection B has path: "docs/memories", docs/memories/MEM-001.md will resolve to A instead of B. Filter patterns by matching norm before taking str::len during polish.'
  metrics_summary:
    decisions_count: 21
    deferred_findings_count: 3
    failed_traces: 4
    passed_traces: 48
    task_receipts_count: 9
    tasks_cancelled: 0
    tasks_completed: 9
    tasks_total: 9
    total_claims: 89
    total_review_cycles: 13
    total_traces: 55
---

# AC-006: Universal specs in `acdd.yaml`, named items, explicit spec/path lenses, multi-spec runs, roadmap customization, and repository memory

## Goal & Non-goals

**Goal.** Build on [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md) (`crates/acdd-spec`, `preset:`, `.acdd/workflows/`) to implement [ADR 0014](../../adr/0014-declarative-spec-schemas-and-items.md): universal schema-validated Specs in `acdd.yaml` `specs:` (inline, `base: contract|memory`, or `.acdd/specs/*.yaml` `ref:`), named `items:` collections, `0..N` attached input specs per run (`acdd start [SPEC ...]`) and `0..M` validated output specs (`docs.specs`), explicit `specs.<name>` and `paths.<name>` context/scope lenses with documented 4-tier deterministic relevance ranking, `spec_deltas` verified by `spec.deltas_applied`, customizable ANSI CLI `acdd roadmap`, and repository memory as the built-in `memory` Spec.

**Non-goals.**
- Crate extraction (`crates/acdd-spec`) and `profile`→`workflow` / `collection`→`spec` cutover (already completed in [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md)).
- Generalized `mode: swarm` / `with: spec.<collection>` execution over arbitrary item collections or extended `when:` field predicates (owned by [AC-005](../AC-005-unified-swarm-gates-and-peer-board-coordination.md)).
- Pause-and-call child runs (`parent_run_id` / `waiting_on_run_id`) across workflows (owned by [AC-008](../AC-008-always-on-reactive-triggers-and-deferred-approvals.md)).
- Vector stores, embeddings, fuzzy retrieval, model clients, or new `Cargo.toml` dependencies.

## Target architecture & decisions

### 1. Single `acdd.yaml` manifest, `specs:` registry, `lint_dirs:`, & `roadmap:` display config
- `acdd.yaml` stays a compact workspace manifest referencing the three symmetrical `.acdd/` directories (`workflows_dirs: [.acdd/workflows]`, `specs_dirs: [.acdd/specs]`, `lint_dirs: [.acdd/lint]`) via one shared confined `<name> | <path>[:<sha256_prefix>]` resolver in `crates/acdd-core/src/config/`.
- `acdd.yaml` `specs:` (`SpecConfig`) owns `path`/`paths`, `prefixes`, `digits`, `base: contract|memory`, external `ref:` (resolved in `specs_dirs:`), `workflow:`, `roadmap: true|false`, `fields`, and `items`.
- Architecture zones, complexity thresholds, `unused_public_api`, and lint rules live strictly in `.acdd/lint/*.yaml` (merged lexicographically with fail-closed rejection on duplicate zone names, duplicate `rule.id`, or conflicting scalar settings) plus existing document-embedded invariants; inline `architecture:` is removed from `acdd.yaml` (no compatibility shim — `#[serde(deny_unknown_fields)]` rejects it directly) and this repository's rules move into `.acdd/lint/`.
- `acdd.yaml` `roadmap:` (alongside `response:`) owns CLI presentation (`badges`, `group_by: wave|<field>|spec`, `color: auto|always|never` with `NO_COLOR`).
- Workflow files (`.acdd/workflows/*.yaml`) reject `specs:` and `roadmap:` keys fail-closed (`ACDD_WORKFLOW_SCHEMA_INVALID`).

### 2. Explicit `specs.<name>` & `paths.<name>` + 4-tier deterministic relevance ranking
- `scope: [specs.<name>]` and `context: [specs.<name>]` reference `acdd.yaml` `specs:` directly (zero hidden `specs → paths` magic).
- `paths:` inside workflows defines strictly non-spec doc globs (`paths.docs`, `paths.adrs`, `paths.instructions`), replacing hardcoded `docs.instructions` and `docs.adrs`.
- Tier 6 `ClaimResp.context_lenses` ranks candidate items/documents deterministically (documented in `ADR 0006`, `ADR 0014`, and `docs/reference/workflows.md`) and trims by `max_bytes`:
  1. Direct ID/title citation in task `target` or owning Spec body (highest priority).
  2. Explicit `scope`/`owners` overlap (`scopes_overlap` against task `admitted_scope`).
  3. Indexed `code-map.sqlite` edges (`documents`, `references_doc`, `cites_doc`, `governs`, `governed_by`) to symbols/paths in `admitted_scope`.
  4. Global fallback rules (`scope: ["**"]` / unlinked repo-wide docs — dropped first under budget pressure).

### 3. `0..N` attached input specs & `0..M` validated output specs
- `acdd start [SPEC ...] [--workflow <name>] [--goal "..."]` binds `0..N` registered Specs to one `Run` executing one resolved workflow; attached specs persist on the run and render in `ClaimResp.contract.attached_specs[]` within byte budgets.
- `checks: [{ check: docs.specs, with: { spec: <name>, required: true } }]` enforces that a gate creates/updates at least one valid document of that Spec type in its admitted scope; plain `checks: [docs.specs]` keeps zero-edit tolerance.

### 4. Repository memory as built-in `memory` Spec
- Built-in `memory` base (`roadmap: false`, `items.rules` → `### rule: <ref>` with `scope`, `statement`, `evidence`, `status`) lives in `crates/acdd-spec`.
- A single-role `memory_synthesis` gate template (`scope: [specs.memories]`, `checks: [docs.specs]`) verifies that every cited `evidence` blackboard ID exists in the run's SQLite blackboard; subsequent tasks consume relevant rules via `context: [specs.memories]`.

## Tasks in this milestone

### task: bootstrap-adr-and-seams

```yaml
task_ref: bootstrap-adr-and-seams
target: Admit ADR 0014, document the 4-tier context relevance policy in ADR 0006, record the docs/memories/ .gitignore decision, and register tests/acdd/spec_schemas/ harness modules
scope:
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/adr/0014-declarative-spec-schemas-and-items.md
- docs/adr/README.md
- tests/acdd/mod.rs
- tests/acdd/spec_schemas/
tags:
- docs
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: admit-adr0014-and-update-adr0006
  title: Move ADR 0014 to accepted in docs/adr/README.md, document the 4-tier deterministic relevance ranking in ADR 0006, and record docs/memories/ tracking decision
  status: completed
  evidence: 'Verified in accepted candidate tree aa6a9b7018d01f7c17e974bb0af936004e4ecda6: ADR 0014 accepted (date 2026-11-27) with Section 10 docs/memories/ tracking and retrospective path alignment decision, indexed in docs/adr/README.md, and 4-tier deterministic relevance ranking documented in ADR 0006 Tier 6.'
- subtask_ref: register-spec-schema-seams
  title: Register tests/acdd/spec_schemas/ submodules in tests/acdd/mod.rs compiling cleanly against existing crates/acdd-spec
  status: completed
  evidence: 'Verified in accepted candidate tree aa6a9b7018d01f7c17e974bb0af936004e4ecda6: mod spec_schemas registered in tests/acdd/mod.rs with 8 submodules under tests/acdd/spec_schemas/ (acceptance, config, core, deltas, items, memory, paths, roadmap) passing build and independent review.'
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:bootstrap-adr-and-seams.deliver
  gate_id: deliver
  worker_id: deliver-ac006-w0
  claim_revision: 1
  contract_revision: 1
  candidate_tree: aa6a9b7018d01f7c17e974bb0af936004e4ecda6
  committed_oid: 980268917cf3f12d7f72373cd3fae329b93419f8
  accepted_at: 2026-11-27T12:02:06.003Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
```

- **Wave**: `0` | **Budget**: `~220` net LOC.
- **Seam & mutation proof (`AC-01`)**: `acdd docs check` and `cargo test --test acdd spec_schemas` pass cleanly.

### task: spec-core-and-contract-schema

```yaml
task_ref: spec-core-and-contract-schema
target: 'Split SpecDocument into fixed core fields plus SchemaDef-validated fields, embed built-in contract and memory bases in crates/acdd-spec, move ephemeral --goal narrative from frontmatter spec: to goal:, and remove doc_type == contract branches'
scope:
- crates/acdd-core/src/schema.rs
- crates/acdd-spec/src/
- crates/acdd-state/src/usecases/lifecycle/
- tests/acdd/spec_schemas/core.rs
extended_scope:
- path: crates/acdd-state/src/context/budget.rs
  reason: Co-evolve SpecDocument struct literals and --goal context projection for goal/fields/spec_deltas split
- path: crates/acdd-state/src/context/tests.rs
  reason: Co-evolve SpecDocument struct literals and --goal context projection for goal/fields/spec_deltas split
- path: tests/acdd/claim_knapsack_and_sql_callers.rs
  reason: Co-evolve SpecDocument struct literals and --goal context projection for goal/fields/spec_deltas split
depends_on:
- bootstrap-adr-and-seams
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: fixed-core-and-embedded-bases
  title: 'Split SpecDocument into core fields + SchemaDef-validated extra fields in crates/acdd-spec, embed built-in contract and memory bases, and preserve default contract compatibility when spec: is omitted'
  status: completed
  evidence: SchemaDef::validate_spec_fields + edit-distance hints in crates/acdd-core/src/schema.rs; SpecDocument core/fields split + embedded contract/memory YAML bases in crates/acdd-spec/src/schema.rs & spec.rs; verified via cargo test --test acdd spec_schemas::core and acdd verify
- subtask_ref: goal-frontmatter-and-doctype-cleanup
  title: 'Move ephemeral --goal narrative text to frontmatter goal: so spec: selects Spec type cleanly, and eliminate remaining doc_type == contract branches'
  status: completed
  evidence: 'materialize_ephemeral_goal_spec_with_options writes goal: + spec: contract, draft_spec emits spec: contract, budget.rs projects goal, and crates/acdd-spec has zero doc_type == contract branching'
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:spec-core-and-contract-schema.deliver
  gate_id: deliver
  worker_id: reviewer-ac006-w1-core
  claim_revision: 1
  contract_revision: 2
  candidate_tree: fd5d08cf88c980696437dd84afa30f36ac618ec3
  committed_oid: 5ce79bebda47d9fc34712157649b3d99186a1970
  accepted_at: 2026-11-27T14:30:54.082Z
  rollup:
    verified_invariants:
      applicable: []
      verified:
      - INV-SINGLE-OWNER
      - INV-CORE-LEAF
      - INV-SPEC-CRATE-BOUNDARY
    review_summary: Delivered spec-core-and-contract-schema after independent review pass across all 6 contours.
```

- **Wave**: `1` | **Budget**: `~480` net LOC.
- **Seam & mutation proof (`AC-02`)**: `tests/acdd/spec_schemas/core.rs` proves existing milestones without `spec:` parse under built-in `contract`, `--goal` writes `goal:` rather than `spec:`, and unknown core/schema fields fail closed.

### task: acdd-yaml-specs-and-refs

```yaml
task_ref: acdd-yaml-specs-and-refs
target: 'Extend SpecConfig in acdd.yaml with base, ref, roadmap, fields, and items, add specs_dirs and lint_dirs (.acdd/lint/*.yaml, removing inline architecture: from acdd.yaml) over shared confined ref resolution in crates/acdd-core/src/config/, parse acdd.yaml roadmap display config, and reject specs/roadmap keys inside workflows'
scope:
- acdd.yaml
- .acdd/lint/
- crates/acdd-core/src/config/
- crates/acdd-index/src/docs.rs
- crates/acdd-state/src/workflow/schema.rs
- crates/acdd-state/src/workflow/validate.rs
- src/cli/validate.rs
- tests/acdd/spec_schemas/config.rs
extended_scope:
- path: crates/acdd-state/src/workflow/inherit.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-state/src/validate.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: src/state_bridge/code_domain/lint.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: src/state_bridge/docs_domain.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: tests/acdd/lint_gate_and_docs_profile.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: tests/mcp/lint_surface.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: tests/core/arch_lint.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-state/tests/state_integration/usecases_e2e.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-state/tests/state_integration/roadmap_query_waves.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-state/tests/state_integration/common.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-index/src/scanner/mod.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: tests/commitment_integrity/spec_boundaries.rs
  reason: Delegate workflow/inherit.rs to shared confined ref resolver, support lint_dirs/specs_dirs in validate.rs and lint freshness, and migrate inline architecture/SpecConfig test fixtures
- path: crates/acdd-core/src/path_policy.rs
  reason: Whitelist .acdd/lint/ in path_policy, .gitignore, and guide CANONICAL_GITIGNORE_BLOCK so externalized .acdd/lint/*.yaml files are tracked and committed on deliver
- path: .gitignore
  reason: Whitelist .acdd/lint/ in path_policy, .gitignore, and guide CANONICAL_GITIGNORE_BLOCK so externalized .acdd/lint/*.yaml files are tracked and committed on deliver
- path: src/engine/guide.rs
  reason: Whitelist .acdd/lint/ in path_policy, .gitignore, and guide CANONICAL_GITIGNORE_BLOCK so externalized .acdd/lint/*.yaml files are tracked and committed on deliver
depends_on:
- bootstrap-adr-and-seams
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: shared-ref-resolver-and-specs-config
  title: 'Add shared confined <name> | <path>[:<sha256_prefix>] resolver in crates/acdd-core/src/config/ for specs_dirs: [.acdd/specs], workflows_dirs:, and lint_dirs: [.acdd/lint], extend SpecConfig with base/ref/roadmap/fields/items (single-hop only), and add RoadmapDisplayConfig'
  status: completed
  evidence: Verified shared confined ref resolver in crates/acdd-core/src/config/refs.rs reused by workflow/inherit.rs, SpecConfig (base/ref/roadmap/fields/items, single-hop), and RoadmapDisplayConfig; tests/acdd spec_schemas::config 3/3 passed.
- subtask_ref: externalize-lint-rules-from-acdd-yaml
  title: 'Move architecture zones/rules/complexity/unused_public_api out of acdd.yaml into .acdd/lint/*.yaml (deterministic merge, fail-closed on duplicate zone/rule.id/scalar, no inline architecture: compatibility shim), validate lint_dirs in acdd validate, and migrate this repository''s rules to .acdd/lint/'
  status: completed
  evidence: 'Verified inline architecture: removed from AdapterFile (no shim), .acdd/lint/*.yaml lexicographical merge + collision fail-closed in loader.rs, lossless repo migration to .acdd/lint/{zones,rules}.yaml, acdd validate lint_dirs support, and path_policy/.gitignore/guide.rs whitelist; lint --categories all 0 errors across 65 rules.'
- subtask_ref: reject-duplicate-workflow-keys
  title: 'Ensure .acdd/workflows/*.yaml rejects duplicate specs: or roadmap: keys fail-closed with ACDD_WORKFLOW_SCHEMA_INVALID and acdd-index reads spec_paths cleanly without acdd-state coupling'
  status: completed
  evidence: 'Verified WorkflowDef rejects specs:/roadmap: with ACDD_WORKFLOW_SCHEMA_INVALID + hint in workflow/schema.rs and crates/acdd-index/src/docs.rs supports glob spec_paths via acdd-core::path_policy without acdd-state coupling; commitment_integrity 11/11 passed.'
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:acdd-yaml-specs-and-refs.deliver
  gate_id: deliver
  worker_id: reviewer-w1-config
  claim_revision: 1
  contract_revision: 3
  candidate_tree: 7eb53e7efca8c45364dad061d0d19908fce87f8b
  committed_oid: 2eea0d09cc61b69670e79203ce49a5c4a06dfe6f
  accepted_at: 2026-11-27T14:46:28.081Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Delivered acdd-yaml-specs-and-refs after independent review pass across all 6 contours (+939 production net LOC within approved_budget=1000).
```

- **Wave**: `1` | **Budget**: `~560` net LOC.
- **Seam & mutation proof (`AC-03`)**: `tests/acdd/spec_schemas/config.rs` proves inline, `base: contract|memory`, and `.acdd/specs/*.yaml` `ref:` entries plus `.acdd/lint/*.yaml` merge load deterministically, while inline `architecture:` in `acdd.yaml`, duplicate zone/rule IDs across lint files, path traversal, multi-hop `base:`, or `specs:`/`roadmap:` inside `.acdd/workflows/*.yaml` fail closed.

### task: named-items-and-attached-specs

```yaml
task_ref: named-items-and-attached-specs
target: 'Implement named items: collections (### <item>: <ref>), 3-step validation (SPEC_FIELD_UNKNOWN, SPEC_ITEM_UNKNOWN, ITEM_DEPENDENCY_UNKNOWN), multi-spec acdd start [SPEC ...] with ClaimResp.contract.attached_specs[], and docs.specs required-spec check parameters'
scope:
- crates/acdd-spec/src/
- crates/acdd-state/src/context/
- crates/acdd-state/src/store/
- crates/acdd-state/src/usecases/
- src/state_bridge/docs_domain.rs
- src/cli/
- src/mcp/
- tests/acdd/spec_schemas/items.rs
extended_scope:
- path: crates/acdd-core/src/errors.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-core/src/schema.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-core/src/config/schema.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/types.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/machine/inputs.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/machine/graph.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/machine/verify.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/harness.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/src/validate.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/tests/state_integration/common.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: crates/acdd-state/tests/state_integration/validate.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: src/format/lifecycle.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: tests/acdd/claim_knapsack_and_sql_callers.rs
  reason: 'Autonomous extend_scope per AC-006 blueprint: canonical SPEC_FIELD_UNKNOWN/SPEC_ITEM_UNKNOWN/ITEM_DEPENDENCY_UNKNOWN diagnostics in acdd-core, effective_builtin_base Wave 2 fix, RunRecord.attached_specs co-evolution across state machine/harness/validate/tests, and SpecDocument.items struct co-evolution'
- path: src/state_bridge/mod.rs
  reason: Update existing StartReq test initializers in src/state_bridge/mod.rs for new spec_paths field
- path: crates/acdd-state/tests/state_integration/sqlite.rs
  reason: Update sqlite migration integration test assertion for SQLite schema_version v3
- path: tests/commitment_integrity/state_boundaries.rs
  reason: Update schema_version assertion in tests/commitment_integrity/state_boundaries.rs for SQLite migration v3
- path: tests/acdd/spec_schemas/config.rs
  reason: 'Update 4 SpecConfig::effective_roadmap/effective_base test calls for unified SpecConfig::effective_roadmap(collection_key) eliminating dead_tails.phantom_tested (finding #655 #1)'
depends_on:
- spec-core-and-contract-schema
- acdd-yaml-specs-and-refs
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: named-items-and-three-step-validation
  title: 'Parse ### <item>: <ref> blocks across declared items: collections, enforce 3-step validation (core -> SchemaDef -> closed cross-Spec DAG RES-001#probe-a), and emit SPEC_FIELD_UNKNOWN (+ edit-distance hint), SPEC_ITEM_UNKNOWN, and ITEM_DEPENDENCY_UNKNOWN'
  status: completed
  evidence: 'Implemented ### <item>: <ref> block parsing across declared items collections, SpecConfig::effective_builtin_base(), strict 3-step validation order with SPEC_FIELD_UNKNOWN (+ edit-distance hint), SPEC_ITEM_UNKNOWN, ITEM_DEPENDENCY_UNKNOWN, and closed cross-Spec RES-001#probe-a DAG checks; verified in tests/acdd/spec_schemas/items.rs'
- subtask_ref: multi-spec-start-and-docs-specs-required
  title: 'Support acdd start [SPEC ...] (0..N specs) persisted as attached_specs and rendered in ClaimResp.contract.attached_specs[] within byte budgets, plus docs.specs with: { spec/specs, required } verification'
  status: completed
  evidence: Implemented 0..N acdd start [SPEC ...] (CLI/MCP/StartReq + Spec ID resolution + single workflow resolution), SQLite migration v3 runs.attached_specs_json, budgeted ClaimResp.contract.attached_specs[] projection + Markdown rendering, and parameterized docs.specs { spec/specs, required } verification; verified in tests/acdd/spec_schemas/items.rs
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:named-items-and-attached-specs.deliver
  gate_id: deliver
  worker_id: reviewer-w2-items-r3
  claim_revision: 1
  contract_revision: 6
  candidate_tree: 1f45ed1442e91d755b3389b844dc9cf4f064cf7c
  committed_oid: 500bb1d2c0d54a6918a7ad47c4b1eaa48b48a702
  accepted_at: 2026-11-27T16:36:38.712Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Delivered named-items-and-attached-specs after R3 independent review pass across all 6 contours.
```

- **Wave**: `2` | **Budget**: `~620` net LOC.
- **Seam & mutation proof (`AC-04`, `AC-05`)**: `tests/acdd/spec_schemas/items.rs` proves custom `### probe:` / `### rule:` items validate with cross-Spec DAG refs, `acdd start AUD-001 AC-006` surfaces both specs in `ClaimResp.contract.attached_specs[]`, and `docs.specs` with `required: true` fails closed when the target spec file is missing.

### task: spec-and-path-context-lenses

```yaml
task_ref: spec-and-path-context-lenses
target: 'Wire explicit specs.<name> and workflow paths.<name> into scope: and context: with 4-tier deterministic relevance ranking (direct citation -> scope/owners overlap -> code-map edges -> global **), replacing hardcoded docs.instructions and docs.adrs'
scope:
- crates/acdd-state/src/workflow/
- src/state_bridge/docs_domain.rs
- tests/acdd/spec_schemas/paths.rs
extended_scope:
- path: crates/acdd-core/src/ports.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/context/tiers.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/context/markdown.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/context/tests.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/validate.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/usecases/lifecycle/specs.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/usecases/lifecycle/workspace.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/usecases/lifecycle/runs.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/usecases/commands/start.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/usecases/commands/run_ops.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/src/machine/graph.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/tests/state_integration/profile.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/tests/state_integration/validate.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: crates/acdd-state/tests/state_integration/usecases_e2e.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: src/state_bridge/code_domain/mod.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: tests/acdd/claim_knapsack_and_sql_callers.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: tests/acdd/spec_schemas/items.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: tests/mcp/harness.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: tests/mcp/harness_contract.rs
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: skills/acdd/references/acdd-workflow-examples/spec_planning.yaml
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: skills/acdd/references/acdd-workflow-examples/audit.yaml
  reason: Co-evolve DomainContextReq, context lens forwarding/markdown, workflow validation/scope expansion, and legacy lens test fixtures per pre_task_slice
- path: tests/acdd/fixtures_matrix.rs
  reason: Update WorkflowValidationOptions literals for spec_collections field
- path: tests/acdd/profiles_and_inheritance.rs
  reason: Update WorkflowValidationOptions literals for spec_collections field
- path: src/state_bridge/common.rs
  reason: Remove dead budget_items wrapper after migrating docs_domain to budget_scored_items
- path: crates/acdd-state/src/machine/sync.rs
  reason: 'Close review #660 findings: TaskGate scope sync and migrate sibling workflow examples from docs.instructions/adrs'
- path: skills/acdd/references/acdd-workflow-examples/architecture-hygiene.workflow.yaml
  reason: 'Close review #660 findings: TaskGate scope sync and migrate sibling workflow examples from docs.instructions/adrs'
- path: skills/acdd/references/acdd-workflow-examples/pr-code-review.workflow.yaml
  reason: 'Close review #660 findings: TaskGate scope sync and migrate sibling workflow examples from docs.instructions/adrs'
depends_on:
- named-items-and-attached-specs
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: workflow-paths-and-specs-scope-expansion
  title: 'Add paths: to WorkflowDef (with defaults in embedded base), resolve specs.<name> and paths.<name> in gate scope: and context:, reject unknown names and legacy docs.instructions/docs.adrs with redirect hints, and warn on empty glob matches'
  status: completed
  evidence: 'Added WorkflowDef/ResolvedWorkflow paths: with embedded base defaults and inherit merge, migrated embedded code/documentation and shipped workflow examples to paths.instructions/paths.adrs, expanded specs.<name>/paths.<name> in gate scope: across container_scope/sync/collect_integration_inputs, rejected legacy/unknown/empty-scope refs with redirect hints, and emitted EMPTY_GLOB_MATCH warnings; verified via tests/acdd/spec_schemas/paths.rs (3/3), acdd validate (10/10), and acdd-state integration (52/52).'
- subtask_ref: four-tier-relevance-ranking
  title: Implement deterministic 4-tier relevance ranking in DocsDomainProvider (1. direct ID/title citation -> 2. scope/owners overlap -> 3. code-map doc edges -> 4. global ** fallback) bounded by max_bytes for both specs.<name> and paths.<name>
  status: completed
  evidence: Implemented single 4-tier deterministic relevance ranker in DocsDomainProvider (1. direct citation with UTF-8 char-boundary safety -> 2. scope/owners overlap -> 3. code-map doc edges -> 4. global ** fallback) with super-increasing u128-summed tier bases over budget_scored_items for both paths.<name> and specs.<name> (including named doc.items rules and Markdown rendering in context/markdown.rs); verified via docs_domain (5/5 state_bridge) and tests/acdd/spec_schemas/paths.rs (3/3).
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:spec-and-path-context-lenses.deliver
  gate_id: deliver
  worker_id: reviewer-w3-paths-r3
  claim_revision: 1
  contract_revision: 5
  candidate_tree: 17552d4c5348d9956cfdccec4814d13730c1dfd6
  committed_oid: c324184802fc1fa145ea90592e0335b0d2718ad2
  accepted_at: 2026-11-27T18:11:52.353Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: 'Delivered spec-and-path-context-lenses (AC-06) after Round 3 independent review PASS closing all #663 findings.'
```

- **Wave**: `3` | **Budget**: `~460` net LOC.
- **Seam & mutation proof (`AC-06`)**: `tests/acdd/spec_schemas/paths.rs` proves direct ID citations outrank `scope` overlaps, which outrank `code-map` edges, which outrank `**` globals under a tight byte budget; legacy `docs.instructions` / `docs.adrs` fail closed with redirect hints.

### task: spec-deltas

```yaml
task_ref: spec-deltas
target: Add spec_deltas core parsing in crates/acdd-spec and built-in spec.deltas_applied verification check confirming added, modified, and removed id-keyed entries in target Specs
scope:
- crates/acdd-spec/src/deltas.rs
- crates/acdd-spec/src/lib.rs
- src/state_bridge/docs_domain.rs
- tests/acdd/spec_schemas/deltas.rs
extended_scope:
- path: src/state_bridge/spec_domain.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
- path: src/state_bridge/mod.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
- path: src/state_bridge/common.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
- path: crates/acdd-spec/src/spec.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
- path: crates/acdd-core/src/ports.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
- path: crates/acdd-state/src/workflow/validate.rs
  reason: 'Register separate SpecDomainProvider module and wire SubsystemId::Spec across host bridge, CompositeDomainProvider default enablement, workflow domain enablement validation, and SpecDocument typed spec_deltas per #638 and #664'
depends_on:
- spec-and-path-context-lenses
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: deltas-parser-and-check
  title: Parse spec_deltas (target, field, added, modified, removed) and register spec.deltas_applied verifying target list membership and modified now text deterministically
  status: completed
  evidence: Pure spec_deltas parsing & verify_spec_deltas in crates/acdd-spec/src/deltas.rs + SpecDomainProvider (spec.deltas_applied) in src/state_bridge/spec_domain.rs verified by Round 3 independent review PASS (candidate tree 8fd10a72aacfb70dfbd7f01fe3c0a080d93ea602, tests/acdd spec_schemas 15/15, clippy/lint/duplicates/dead_tails/validate clean).
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:spec-deltas.deliver
  gate_id: deliver
  worker_id: reviewer-w4-deltas-r3
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 8fd10a72aacfb70dfbd7f01fe3c0a080d93ea602
  committed_oid: 1d1455c93f5ba72e8462ae80aed358eced195363
  accepted_at: 2026-11-27T19:14:15.693Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: 'Delivered spec-deltas after Round 3 independent review PASS (#677 closed, AC-07 verified, +778 <= 800 #675).'
```

- **Wave**: `4` | **Budget**: `~260` net LOC.
- **Seam & mutation proof (`AC-07`)**: `tests/acdd/spec_schemas/deltas.rs` proves `spec.deltas_applied` passes when all target edits match and fails closed if any `added` entry is missing, `removed` entry remains, or `modified.now` diverges.

### task: roadmap-display-and-schema-filters

```yaml
task_ref: roadmap-display-and-schema-filters
target: 'Wire acdd.yaml roadmap: display config (badges, group_by, color with NO_COLOR/--color), explicit roadmap: true|false default filtering, and --spec/--all/--tag/--field/--group-by filters in CLI (`acdd roadmap`) and `acdd_spec::roadmap::build_roadmap` (`acdd_state` MCP remains active-run-only)'
scope:
- crates/acdd-spec/src/roadmap.rs
- src/format/roadmap.rs
- src/cli/dispatch_state.rs
- src/cli/args.rs
- tests/acdd/spec_schemas/roadmap.rs
extended_scope:
- path: crates/acdd-spec/src/roadmap
  reason: AC-08 live roadmap module is crates/acdd-spec/src/roadmap/ with DTOs in crates/acdd-spec/src/types.rs; re-export RoadmapRenderOptions in src/format/mod.rs and pass stdout_is_tty from src/cli/mod.rs
- path: crates/acdd-spec/src/types.rs
  reason: AC-08 live roadmap module is crates/acdd-spec/src/roadmap/ with DTOs in crates/acdd-spec/src/types.rs; re-export RoadmapRenderOptions in src/format/mod.rs and pass stdout_is_tty from src/cli/mod.rs
- path: src/format/mod.rs
  reason: AC-08 live roadmap module is crates/acdd-spec/src/roadmap/ with DTOs in crates/acdd-spec/src/types.rs; re-export RoadmapRenderOptions in src/format/mod.rs and pass stdout_is_tty from src/cli/mod.rs
- path: src/cli/mod.rs
  reason: AC-08 live roadmap module is crates/acdd-spec/src/roadmap/ with DTOs in crates/acdd-spec/src/types.rs; re-export RoadmapRenderOptions in src/format/mod.rs and pass stdout_is_tty from src/cli/mod.rs
depends_on:
- spec-and-path-context-lenses
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: roadmap-filtering-and-badges
  title: 'Filter acdd roadmap by specs.<name>.roadmap: true by default, add --spec/--all/--tag/--field key=value/--group-by across CLI and MCP, and project configured badges'
  status: completed
  evidence: crates/acdd-spec/src/{types.rs,roadmap/{mod,discovery,render}.rs}, src/cli/{args,dispatch_state}.rs, tests/acdd/spec_schemas/roadmap.rs (acdd verify exit_code=0)
- subtask_ref: ansi-color-and-no-color
  title: Add ANSI color rendering in src/format/roadmap.rs honoring acdd.yaml roadmap.color, --color=auto|always|never, TTY detection, and NO_COLOR
  status: completed
  evidence: src/format/roadmap.rs, src/cli/{mod,dispatch_state}.rs, tests/acdd/spec_schemas/roadmap.rs (NO_COLOR/--color/TTY verified)
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:roadmap-display-and-schema-filters.deliver
  gate_id: deliver
  worker_id: reviewer-w4-roadmap-r3
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 71cfac3ff0bfefb6b57dbec63536f887ae87f649
  committed_oid: 3ab97f0df99c9c24ee19d6a89073671c428c05b6
  accepted_at: 2026-11-27T19:13:09.072Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Delivered roadmap-display-and-schema-filters after Round 3 independent review PASS
```

- **Wave**: `4` | **Budget**: `~380` net LOC.
- **Seam & mutation proof (`AC-08`)**: `tests/acdd/spec_schemas/roadmap.rs` proves `roadmap: false` specs hide by default unless `--spec` or `--all` is passed, `--field priority=high` and `--group-by` render configured badges, and `NO_COLOR=1` / `--color=never` emit zero ANSI escape sequences.

### task: memory-schema-and-gate

```yaml
task_ref: memory-schema-and-gate
target: Wire built-in memory Spec (docs/memories/), single-role memory_synthesis gate template with blackboard evidence verification, specs.memories delivery, and reusable spec examples (audit_brief, audit_report, proposal)
scope:
- acdd.yaml
- .gitignore
- crates/acdd-state/src/workflow/embedded/
- crates/acdd-state/src/usecases/verify.rs
- skills/acdd/references/acdd-spec-examples/
- tests/acdd/spec_schemas/memory.rs
depends_on:
- spec-deltas
tags:
- default
contract_revision: 1
status: completed
subtasks:
- subtask_ref: memory-synthesis-gate-and-evidence-check
  title: 'Add single-role memory_synthesis gate template (scope: [specs.memories], checks: [docs.specs]), verify every cited evidence blackboard ID exists in run SQLite blackboard, and update acdd.yaml/.gitignore per Wave 0 decision'
  status: completed
  evidence: Verified single-role gate_templates.memory_synthesis in code.yaml & documentation.yaml (base.yaml untouched), verify_memory_spec_evidence over &dyn StateBackend + canonical acdd-spec collection lookup & strict ASCII-digit ID parsing with MEMORY_EVIDENCE_UNKNOWN, acdd.yaml specs.memories, narrowed .gitignore /docs/memories/retrospectives/, and tests/acdd/spec_schemas/memory.rs (2/2 pass, spec_schemas 18/18, commitment_integrity 11/11).
- subtask_ref: reusable-spec-examples
  title: Add ready-to-copy spec templates (audit_brief.yaml, audit_report.yaml, proposal.yaml) under skills/acdd/references/acdd-spec-examples/ paired with acdd-workflow-examples/
  status: completed
  evidence: Verified skills/acdd/references/acdd-spec-examples/{audit_brief.yaml,audit_report.yaml,proposal.yaml} paired with acdd-workflow-examples/{audit.yaml,spec_planning.yaml} and passing validate_external_spec_yaml in reusable_spec_examples_validate_cleanly.
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:memory-schema-and-gate.deliver
  gate_id: deliver
  worker_id: reviewer-w5-memory-r2
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 9c5aac063790e67217b18518cf672930d16936a8
  committed_oid: f78a231c35427dcc7835ded112583d376b433920
  accepted_at: 2026-11-27T19:50:29.485Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Deliver memory-schema-and-gate (AC-09) after Round 2 independent review PASS
```

- **Wave**: `5` | **Budget**: `~340` net LOC.
- **Seam & mutation proof (`AC-09`)**: `tests/acdd/spec_schemas/memory.rs` proves a rejected review produces a linked `spec: memory` rule delivered to a later task via `context: [specs.memories]`, zero edits pass cleanly, and a fabricated blackboard `evidence` ID fails closed.

### task: docs-and-acceptance

```yaml
task_ref: docs-and-acceptance
target: Reconcile docs/reference/workflows.md (with complete specs.*/paths.*/code.* context and 4-tier relevance table), docs/architecture/, skills/acdd/, ROADMAP.md, and CHANGELOG.md, and execute end-to-end acceptance seam
scope:
- docs/reference/
- docs/architecture/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
- tests/acdd/spec_schemas/acceptance.rs
depends_on:
- memory-schema-and-gate
- roadmap-display-and-schema-filters
tags:
- docs
contract_revision: 1
status: completed
subtasks:
- subtask_ref: context-table-and-reference-docs
  title: Document complete specs.*, paths.*, and code.* context table + 4-tier relevance ranking in docs/reference/workflows.md, reconcile configuration/cli/tasks/acdd references, skills/acdd/, ROADMAP.md, and CHANGELOG.md
  status: completed
  evidence: Reconciled docs/reference/{workflows,configuration,cli,tasks,mcp-tools,acdd}.md, docs/architecture/domain-extensions.md, skills/acdd/, and CHANGELOG.md; acdd docs check + acdd validate + acdd lint pass.
- subtask_ref: end-to-end-acceptance
  title: Execute tests/acdd/spec_schemas/acceptance.rs across contract, external .acdd/specs/*.yaml ref, multi-spec start, specs.*/paths.* ranking, roadmap filters, spec_deltas, and memory Specs; pass acdd docs check, acdd validate, and acdd lint
  status: completed
  evidence: tests/acdd/spec_schemas/acceptance.rs exercises contract, external .acdd/specs/*.yaml, .acdd/lint/*.yaml, multi-spec start, memory_synthesis StateCommand::Verify, roadmap filters, paths.instructions + specs.memories 4-tier ranking, and spec.deltas_applied via SpecDomainProvider.
receipt:
  run_id: AC-006-universal-rep-20261127105323-954942-1
  node_cursor: root:docs-and-acceptance.deliver
  gate_id: deliver
  worker_id: reviewer-w6-docs-r3
  claim_revision: 1
  contract_revision: 1
  candidate_tree: 55540c8f0508ec805fa8695ee48eb85f9ce1a892
  committed_oid: 019e010fae71d77735f8731d3238015e20f529f6
  accepted_at: 2026-11-27T20:29:12.648Z
  rollup:
    verified_invariants:
      applicable: []
      verified: []
    review_summary: Delivered docs-and-acceptance (AC-10) after R3 independent review pass.
```

- **Wave**: `6` | **Budget**: `~400` net LOC.
- **Seam & mutation proof (`AC-10`)**: `tests/acdd/spec_schemas/acceptance.rs` + `acdd docs check` + `acdd validate` + `acdd lint --categories arch,docs` pass with zero errors.

## Negative boundaries

- No inline `architecture:` block inside `acdd.yaml` (architecture zones/rules live strictly in `.acdd/lint/*.yaml` via `lint_dirs:` and document-embedded invariants), no duplicate `specs:` or `roadmap:` keys inside `.acdd/workflows/*.yaml`, and no hidden `specs → paths` magic bridging.
- No separate `inputs:` workflow/milestone DSL and no nested `GateDef.workflow:` YAML merging (multi-workflow chaining uses pause-and-call child runs in [AC-008](../AC-008-always-on-reactive-triggers-and-deferred-approvals.md)).
- No memory-specific parser, registry, status axis, evidence collector, reviewer gate, context lens, CLI command, or MCP tool.
- No second schema validator or file resolver outside `SchemaDef` and the shared `acdd-core` confined reference resolver.
- No multi-level spec inheritance trees, no vector store/embeddings, and no `Cargo.toml` modifications in `AC-006`.

## Three-axis audit (Completeness, Correctness, Coherence)

- **Completeness**: Every criterion (`AC-01`..`AC-10`) maps 1:1 to a bounded task across Waves 0–6, dedicated seams in `tests/acdd/spec_schemas/`, and co-evolved ADRs/docs/examples.
- **Correctness**: Every `AC-*` names trigger, expected behavior, fail-closed rejection (`SPEC_FIELD_UNKNOWN`, `SPEC_ITEM_UNKNOWN`, `ITEM_DEPENDENCY_UNKNOWN`, `ACDD_WORKFLOW_SCHEMA_INVALID`), and seam mutation proof.
- **Coherence**: Builds cleanly on [AR-006](AR-006-acdd-spec-crate-and-workflow-cutover.md), reconciles [ADR 0006](../../adr/0006-budgeted-gate-context-and-dual-surface-parity.md) and [ADR 0014](../../adr/0014-declarative-spec-schemas-and-items.md), preserves `INV-SINGLE-OWNER`, `INV-CORE-LEAF`, `INV-SPEC-CRATE-BOUNDARY`, and `INV-STATE-NO-INDEX`, and keeps all concurrent same-wave `scope:` globs strictly disjoint (Wave 1: `spec-core-and-contract-schema` vs. `acdd-yaml-specs-and-refs`; Wave 4: `spec-deltas` vs. `roadmap-display-and-schema-filters`).
