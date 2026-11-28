---
id: AR-003-unified-lint-engine-and-documentation-profile
title: Language-agnostic architectural & graph linter (acdd lint / code_map_lint), executable INV-* rules, embedded documentation profile, and AGENTS.md invariant consolidation
doc_type: contract
status: completed
depends_on:
- AR-002-modularize-db-reader-and-traversal
owners:
- AGENTS.md
- src/AGENTS.md
- crates/acdd-core/AGENTS.md
- crates/acdd-core/src/
- crates/acdd-index/AGENTS.md
- crates/acdd-index/src/docs.rs
- crates/acdd-index/src/db/
- crates/acdd-state/AGENTS.md
- crates/acdd-state/src/profile/
- crates/acdd-state/tests/
- src/cli/
- src/mcp/
- src/format/
- src/state_bridge/
- tests/AGENTS.md
- tests/core.rs
- tests/core/
- tests/mcp.rs
- tests/mcp/
- tests/acdd.rs
- tests/acdd/
- .acdd/profile.yaml
- docs/AGENTS.md
- docs/adr/
- docs/architecture/
- docs/reference/
- skills/acdd/
- README.md
- INSTALL.md
- ROADMAP.md
- docs/milestones/AR-003-unified-lint-engine-and-documentation-profile.md
invariants:
- 'INV-AR003-CORE-LEAF-LINT-TYPES: Pure lint categories, severities, architectural rule kinds, zone selectors, and acdd.yaml architecture schemas reside in leaf crates/acdd-core reusing WorkspacePathPolicy glob matching with zero dependencies on acdd-index, acdd-state, rusqlite, tree-sitter, clap, rmcp, or tokio.'
- 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Executable acdd-rule blocks live directly inside indexed Markdown sections alongside INV-* invariants and persist in Node.details["arch_rules"] (with schema/zone errors recorded fail-closed in errors), preserving deterministic cold/cache/delta parity under existing CommitmentDomain::Nodes and CommitmentDomain::Errors without adding shadow SQLite registries.'
- 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ is the sole canonical owner of all indexed verification categories (arch, duplicates, dead_tails, cycles, syntax, docs), evaluating internal edges (respecting EdgeKind, EdgeConfidence, and is_test) and segment-normalized external dependencies in batched O(V + E) passes with zero N+1 SQLite, filesystem, or Git queries.'
- 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint (CLI), code_map_lint (MCP), and CodeDomainProvider check code.lint share one execution and formatting contract, and code.lint observe_freshness binds the index generation/commitment digest plus live scoped workspace state so acdd submit rejects stale lint traces.'
- 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: crates/acdd-state embeds documentation (embedded/documentation.yaml) alongside base, code, and audit as a compiler-free documentation and invariant governance profile gated on docs.links and code.lint (categories: [arch, docs]) with independent review.'
- 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Architectural INV-* invariants and executable acdd-rule blocks reside exclusively in indexed canonical docs/architecture/*.md and docs/adr/*.md pages with verified symbol claims, while root and subtree AGENTS.md files act strictly as concise operational routers.'
acceptance_criteria:
- 'AC-1: crates/acdd-core defines closed LintCategory (arch, duplicates, dead_tails, cycles, syntax, docs), LintSeverity (error, warn), ArchRuleKind (forbid_dependency, allow_only_dependencies, single_owner, forbid_cycles, boundary_entry, must_implement, doc_integrity, ast_pattern), ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy, adds optional architecture.{zones,rules} to AcddConfig, and registers code_map_lint in BUILTIN_MCP_TOOLS (20 built-in tools).'
- 'AC-2: crates/acdd-index/src/docs.rs extracts fenced acdd-rule YAML blocks into section Node.details["arch_rules"] and records malformed rule YAML, duplicate rule_id definitions, or unknown zone references fail-closed in errors with identical cold, cached, and delta Merkle roots.'
- 'AC-3: crates/acdd-index/src/db/lint/ unifies arch, duplicates, dead_tails, cycles, syntax, and docs checks across all indexed languages and linked_workspaces, normalizes external dependency prefixes across ::, ., /, and : separators, attaches governing INV-* and source doc path:line citations to violations, and rewires code_map_query(duplicates | dead_tails) and code_map_analyze(lint=true | include_cycles=true) as thin delegates.'
- 'AC-4: crates/acdd-state embeds documentation.yaml (embedded_profile("documentation")) with compiler-free draft/build, docs.links + code.lint verification, independent review, and delivery/handoff gates, and adds code_map_lint + code.lint checks to embedded code.yaml and audit.yaml default toolsets.'
- 'AC-5: Host src/ exposes acdd lint CLI and code_map_lint MCP with dual-surface Markdown/JSON parity, registers CheckCapability("lint") (code.lint) with matching observe_freshness in CodeDomainProvider, marks rule-backed invariants as enforced in DocsDomainProvider (docs.invariants), and wires code.lint before/with tests in .acdd/profile.yaml.'
- 'AC-6: Scattered architectural invariants in AGENTS.md, src/AGENTS.md, crates/acdd-{core,index,state}/AGENTS.md, and tests/AGENTS.md are consolidated into canonical docs/architecture/*.md and docs/adr/*.md with INV-* IDs, verified symbol links, and live acdd-rule definitions enforcing ACDD crate/layer/single-owner boundaries; reference docs and skills/acdd/ document acdd lint, code_map_lint, acdd-rule, and --profile documentation; and acdd lint, acdd docs check, and acdd validate pass with zero violations across the workspace.'
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
  closed_at: 2026-11-22T03:30:34.286Z
  target_base: main
  target_base_commit: 2d946c4e8b9399ba5dc1b23b4f5a41a13c5effe5
  task_receipts:
    agents-md-invariant-consolidation-and-docs-alignment:
      run_id: AR-003-unified-lint--20261122000752-1979185-1
      node_cursor: root:agents-md-invariant-consolidation-and-docs-alignment.deliver
      gate_id: deliver
      worker_id: ar003-r5
      claim_revision: 1
      contract_revision: 2
      candidate_tree: d18af6a45d6b223b7cf38d674d58d68f232342ca
      committed_oid: b2fc19cb23591e62003faf0e55788c741764d7d6
      accepted_at: 2026-11-22T03:03:43.731Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant across contextunity-acdd has one canonical INV-* declaration in docs/architecture/*.md or docs/adr/*.md with verified symbol claims (and acdd-rule blocks for structural boundaries), while **/AGENTS.md files remain concise operational routers.'
          verified: []
    core-lint-contracts-and-seam-bootstrap:
      run_id: AR-003-unified-lint--20261122000752-1979185-1
      node_cursor: root:core-lint-contracts-and-seam-bootstrap.deliver
      gate_id: deliver
      worker_id: ar003-r1
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 26bd1fe00ae5e6f3553f709efa7c9f7b0cbea66c
      committed_oid: 092a349cf54ea38fa8b54597745d1d41432fc00d
      accepted_at: 2026-11-22T01:00:28.724Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR003-CORE-LEAF-LINT-TYPES: crates/acdd-core/src/lint.rs owns LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy glob helpers with zero I/O or non-leaf dependencies.'
          verified: []
        review_summary: Delivered Wave 0 task core-lint-contracts-and-seam-bootstrap after independent review pass.
    embedded-documentation-profile-and-profile-validation:
      run_id: AR-003-unified-lint--20261122000752-1979185-1
      node_cursor: root:embedded-documentation-profile-and-profile-validation.deliver
      gate_id: deliver
      worker_id: ar003-r3
      claim_revision: 1
      contract_revision: 2
      candidate_tree: d529db0146c1116ef5deb34733760e6ad01c35ee
      committed_oid: 181e632a3807bf1ba64aaaa2f3037d4400644a6a
      accepted_at: 2026-11-22T01:33:24.880Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: embedded_profile("documentation") resolves a self-contained v2 profile inheriting base, enforcing docs.links and code.lint (categories: [arch, docs]) with independent review and zero compiler command requirement.'
          verified:
          - INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE
        review_summary: Delivered Task 3 (embedded documentation profile, inherit.rs generalization, code/audit code_map_lint + code.lint wiring, markdown [ENFORCED] badge, and state_integration matrices).
    host-cli-mcp-lint-surface-and-domain-provider:
      run_id: AR-003-unified-lint--20261122000752-1979185-1
      node_cursor: root:host-cli-mcp-lint-surface-and-domain-provider.deliver
      gate_id: deliver
      worker_id: ar003-r4
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 3869ad599606f898c42a06cd9259fada1c5b9beb
      committed_oid: ecf1b8d204090460e8bee6a66176e5667fe9f81e
      accepted_at: 2026-11-22T02:41:41.476Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint, code_map_lint, and CodeDomainProvider::run_check("lint") share one request/response pipeline, exit non-zero / fail gate on error-severity violations, and bind observe_freshness to index commitments and live workspace paths.'
          verified: []
        review_summary: Deliver AR-003 task host-cli-mcp-lint-surface-and-domain-provider after round-2 independent review pass
    index-executable-invariants-and-unified-lint-engine:
      run_id: AR-003-unified-lint--20261122000752-1979185-1
      node_cursor: root:index-executable-invariants-and-unified-lint-engine.deliver
      gate_id: deliver
      worker_id: ar003-r2
      claim_revision: 1
      contract_revision: 2
      candidate_tree: 63ca5b5df24adb460fd5c6393b1dd0f227a18238
      committed_oid: 3d42cde6dffa3f58c70d07a1df03bfe45786316a
      accepted_at: 2026-11-22T01:54:17.474Z
      rollup:
        verified_invariants:
          applicable:
          - 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Fenced acdd-rule blocks persist in Node.details["arch_rules"] and malformed blocks emit deterministic errors rows with identical cold, cached, and delta Merkle commitments.'
          - 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ executes all 6 lint categories in batched passes without N+1 queries and serves existing duplicates, dead_tails, syntax, and cycles callers as the sole canonical owner.'
          verified:
          - INV-AR003-DOCS-SSOT-EXECUTABLE-RULES
          - INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT
        review_summary: Delivered AR-003 Wave 1 Task 2 (index-executable-invariants-and-unified-lint-engine) after independent review pass (round 2, candidate_tree 63ca5b5df2).
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-CORE-LEAF-LINT-TYPES: crates/acdd-core/src/lint.rs owns LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy glob helpers with zero I/O or non-leaf dependencies.'
      - 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Fenced acdd-rule blocks persist in Node.details["arch_rules"] and malformed blocks emit deterministic errors rows with identical cold, cached, and delta Merkle commitments.'
      - 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ executes all 6 lint categories in batched passes without N+1 queries and serves existing duplicates, dead_tails, syntax, and cycles callers as the sole canonical owner.'
      - 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: embedded_profile("documentation") resolves a self-contained v2 profile inheriting base, enforcing docs.links and code.lint (categories: [arch, docs]) with independent review and zero compiler command requirement.'
      - 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint, code_map_lint, and CodeDomainProvider::run_check("lint") share one request/response pipeline, exit non-zero / fail gate on error-severity violations, and bind observe_freshness to index commitments and live workspace paths.'
      - 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant across contextunity-acdd has one canonical INV-* declaration in docs/architecture/*.md or docs/adr/*.md with verified symbol claims (and acdd-rule blocks for structural boundaries), while **/AGENTS.md files remain concise operational routers.'
      verified:
      - INV-AR003-DOCS-SSOT-EXECUTABLE-RULES
      - INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT
      - INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE
    decisions:
    - |-
      AR-003 Architectural Blueprint Decision (reconciled against ADRs 0001-0004, 0006-0009, 0012 and INV-SINGLE-OWNER):

      1. Leaf Lint Contracts (`crates/acdd-core/src/{lint.rs, config/{mod,schema,loader}.rs, ports.rs}`):
         - Add `crates/acdd-core/src/lint.rs` owning closed `LintCategory` (`arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`; `ALL` = 6, `DEFAULT_VERIFY` = `[Arch, Syntax, Docs]`), `LintSeverity` (`error`, `warn`), `ArchRuleKind` (8 primitives), `LintEdgeKind`, `LintEdgeConfidence`, `ZoneSelector`, `ArchRuleSpec`, `ArchitectureConfig` (all `#[serde(deny_unknown_fields)]` + `schemars::JsonSchema`), pure `external_symbol_matches_prefix` (segment-boundary matching across `::`, `.`, `/`, `:`), and canonical `lint_check_input_schema() -> SchemaDef`.
         - Reuse `acdd_core::path_policy::{is_valid_scope_glob, glob_matches, expand_glob_braces, normalize_rel_path}` exclusively (zero duplicate glob engines).
         - Extend `AdapterFile` and `Adapter` in `crates/acdd-core/src/config/schema.rs` with `#[serde(default)] pub architecture: ArchitectureConfig` (exporting `pub type AcddConfig = Adapter;` in `config/mod.rs`), validate via `architecture.validate()` in `config/loader.rs`, and keep `architecture` INSIDE `loader.rs` `identity` hash so zone/rule edits invalidate `adapter.digest` and `effective_ruleset_hash` deterministically (while empty/omitted `architecture` preserves existing digests).
         - Register `"code_map_lint"` in `BUILTIN_MCP_TOOLS` (`[&str; 20]`) in `crates/acdd-core/src/ports.rs` (`SubsystemRegistry::subsystem_id_for_tool` already maps `code_map_*` to `SubsystemId::Code`).

      2. Executable `acdd-rule` Extraction & Merkle Parity (`crates/acdd-index/src/{docs.rs, models/details.rs}` + `src/engine/pipeline/{cold,hydration}.rs`):
         - Preserve `docs::extract_with_details` signature (`INV-SURFACE-COMPATIBILITY`) as thin wrapper over `extract_with_details_with_zones(..., known_zones: Option<&BTreeSet<String>>)`.
         - Parse fenced `acdd-rule` YAML blocks via `pulldown_cmark` `CodeBlockKind::Fenced`, track 1-based line offsets, validate against `ArchRuleSpec::validate(known_zones)`, store valid rules in section `metadata["arch_rules"]` (key `NodeDetailKey::ARCH_RULES = "arch_rules"`, omitted when empty), and emit fail-closed parse/schema/intra-file-duplicate/unknown-zone errors into `metadata["arch_rule_errors"]`.
         - SCOPE EXTENSION (Task 2, disjoint from Task 3): extend scope to `crates/acdd-index/src/models/details.rs` and `src/engine/pipeline/{cold,hydration}.rs` so `extract_typed_source_with_digest` passes `adapter.architecture.zone_names()`, copies non-empty `arch_rules` into `Node.details["arch_rules"]` (`CommitmentDomain::Nodes`), and pushes `arch_rule_errors` into `TypedFacts.errors` (`CommitmentDomain::Errors`) identically across cold, cached, and delta builds.

      3. Unified Language-Agnostic Lint Engine (`crates/acdd-index/src/db/lint/{mod,rules,arch,docs_lint,syntax,hygiene}.rs`):
         - Replace `crates/acdd-index/src/db/lint.rs` with directory module `db/lint/` exposing `run_lint(conn, &LintRequest)`, `enforced_invariant_ids(conn, config_rules)`, and backward-compatible `syntax_paged`.
         - Preload `path_dictionary`, `files`, test-line spans (`is_test=1` / `has_cfg_test` nodes), and only the `edges`/`resolution_coverage`/`dependencies` tables required by requested categories/rules in batched O(V+E) queries under `QueryBudget` (zero N+1 SQLite/FS/Git queries).
         - Evaluate external symbol rules against `resolution_coverage WHERE status = 'external'` (exact `path:line` + `#[cfg(test)]` line-span exclusion when `include_tests == false`) plus deduplicated fallback over `dependencies WHERE resolution = 'external'`.
         - `forbid_cycles` projects edges onto zones or depth buckets via `petgraph::algo::kosaraju_scc`; `ast_pattern` delegates to `ast::search::PreparedAstMatcher` using `db::symbols::verified_source` (zero raw `std::fs::read_to_string`).
         - Exclude `archive/%` and `%/archive/%` from live rules/docs lint (`ADR-0006`), and scope duplicate/orphan `INV-*` checks to non-contract docs (`doc_type != 'contract'`) so milestone specs repeating task `INV-*` do not false-positive.
         - Severity contract: `arch` (per-rule, default `error`), `syntax` (`error`), `docs` (stored doc errors, broken explicit-path `symbol_claims`, duplicate `rule_id`/non-contract `INV-*` -> `error`; orphan `INV-*` -> `warn` or rule severity in `doc_integrity`), `dead_tails.dangling_calls` (`error`), advisory `duplicates`/`dead_tails`/`phantom_tested`/`cycles` (`warn`). Default `run_lint` categories when omitted: `LintCategory::DEFAULT_VERIFY = [Arch, Syntax, Docs]` (`--categories all` runs all 6), preserving `<100ms` latency, 2s SQLite budget, and zero-violation self-hosting.
         - Rewire `db::traversal::{duplicates,dead_tails}`, `db::cycles`, and `db::reader::diagnostics` (`analyze_paged` `doc_diagnostics`/`include_cycles`/`syntax_paged`) to share canonical helpers with `db::lint` without duplicating logic.

      4. Embedded `documentation` Profile & `[ENFORCED]` Prompt Rendering (`crates/acdd-state/src/{profile/, context/markdown.rs}`):
         - Add `crates/acdd-state/src/profile/embedded/documentation.yaml` (inheriting `base`, compiler-free `blueprint_synthesis`, `tasks: [build, review, deliver]`, `verify_docs`, `polish`, `integration_review`, `handoff`, using `checks: [docs.links, { check: code.lint, with: { categories: [arch, docs] } }]`) and register `"documentation"` in `embedded_profile` (`inherit.rs`).
         - Add `"code_map_lint"` to `embedded/{code,audit}.yaml` `default_tools` (20 tools) and wire `code.lint` (`with: { categories: [arch, syntax, docs] }`) into `code.yaml` `tasks.build` and `run_tests` checks.
         - SCOPE EXTENSION (Task 3, disjoint from Task 2): extend scope to `crates/acdd-state/src/context/markdown.rs` (~6 LOC) to render `[ENFORCED]` in `render_context_lenses_markdown` when `docs.invariants` item has `"enforced": true`. Update `state_integration/{profile,validate}.rs` stub providers with `acdd_core::lint::lint_check_input_schema()`.

      5. Host Dual-Surface CLI/MCP, `CodeDomainProvider::code.lint`, Freshness & `protected_paths` (`src/{cli,mcp,format,state_bridge}/`, `.acdd/profile.yaml`):
         - Expose `acdd lint [TARGET] (--categories, --rule, --severity, --format markdown|json)` exiting non-zero on `summary.errors > 0` and MCP `code_map_lint` (`#[tool]` in `src/mcp/tools/mod.rs`, updating unit test count to 20+1=21) sharing `src/format/lint.rs` Markdown/JSON renderer with confined `file:///` links and 96 KiB response budgeting.
         - Register `CheckCapability("lint")` using `acdd_core::lint::lint_check_input_schema()` in `CodeDomainProvider`; `run_check("lint")` ensures up-to-date worktree index via canonical pipeline refresh when files exist (never mutating `candidate_tree`), scopes task checks to `admitted_scope`/`changed_paths` (workspace-wide on root gates), runs `db::lint::run_lint`, and returns `execution_digest_salt = "index_generation={gen};live_files_digest={digest}"`; `observe_freshness("lint")` runs strictly read-only (`open_code_db_readonly` + live scoped/doc/`acdd.yaml` SHA-256 digest) so post-verify mutations fail `acdd submit` closed.
         - `DocsDomainProvider::provide_invariants` queries `db::lint::enforced_invariant_ids` and stamps `"enforced": true` on rule-backed invariants.
         - PROTECTED PATHS AUTHORIZATION: `Cargo.toml`, `crates/*/Cargo.toml`, and `build.rs` remain STRICTLY OFF-LIMITS across all tasks. `.acdd/profile.yaml` (active `protected_paths: []`, explicitly admitted in AR-003 `owners:`, `AC-5`, and Task 4 `scope:`) is authorized EXCLUSIVELY in Task 4 solely to wire `code.lint` while preserving all existing commands, roles, and guidance (`acdd validate` clean).

      6. Invariant Consolidation & Live Self-Hosting Rules (`**/AGENTS.md`, `docs/{adr,architecture,reference}/`, `skills/acdd/`, `README.md`, `INSTALL.md`):
         - Consolidate scattered architectural invariants from `**/AGENTS.md` into `docs/adr/` and `docs/architecture/` with unique `INV-*` IDs (resolving existing duplicate `INV-DOC-FIRST-MATCH-TIER` between `docs/AGENTS.md` and `docs/reference/mcp-markdown-contracts.md`), verified symbol links, and live `acdd-rule` blocks enforcing: `INV-CORE-LEAF`, `INV-STATE-NO-INDEX`, `INV-STATE-PURE-MACHINE`, `INV-STATE-SQLITE-CONFINED`, `INV-GIT-SINGLE-OWNER` (`ast_pattern` `Command::new("git")` excluding `crates/acdd-core/src/git.rs` so `sys.rs` `ps` is unaffected), and `INV-STATE-BRIDGE-BOUNDARY`.
         - Update reference docs, `README.md`, `INSTALL.md`, and `skills/acdd/` for `acdd lint`, `code_map_lint`, `acdd-rule`, and `--profile documentation`, proving 0 violations on `acdd lint`, `acdd docs check`, and `acdd validate`.
    - |-
      AR-003 Architectural Blueprint Decision (Rev 2 — supersedes #282, closes all 4 findings in #284 + adjacent CLI embedded-profile sweep):

      Disposition of #284 findings:
      - Finding 1 (confirmed-fixed): `crates/acdd-core/src/config/loader.rs::policy()` (L287-319) and `read_policy_from_value()` (L372-431) MUST round-trip `"architecture": &adapter.architecture` and validate it via `architecture.validate()` on load (fail-closed on tampered persisted JSON) so `src/engine/pipeline/delta.rs:170,192,422,444` preserves `adapter.architecture.zone_names()` when `digest` matches on delta builds (`AC-2` cold/cached/delta Merkle parity).
      - Finding 2 (confirmed-fixed): Replace hardcoded `Some("base") | Some("code") | Some("audit")` in `crates/acdd-state/src/profile/inherit.rs:395` with `Some(builtin) if embedded_profile(builtin).is_some() =>` (and test `inherit: documentation` in `state_integration/profile.rs`), AND declare `modules: { required: [docs, code] }` in `crates/acdd-state/src/profile/embedded/documentation.yaml` so `validate_profile` (`validate.rs:85-92`) fails closed with `ErrorCode::DomainCapabilityUnavailable` when `docs` or `code` is disabled instead of `collect_disabled_domains` (`inherit.rs:478-526`) silently stripping checks/lenses/tools. Adjacent sweep also caught hardcoded `matches!(target_str, "base" | "code" | "audit")` in `src/cli/paths.rs:39` -> Task 4 replaces it with `acdd_state::profile::embedded_profile(target_str).is_some()`.
      - Finding 3 (confirmed-fixed):
        (a) `crates/acdd-index/src/docs.rs:228-237` MUST skip fenced `CodeBlock` byte/line spans (already emitted in the single `pulldown_cmark` pass) when scanning `declared_invariants` so fenced examples in `docs/reference/*` or `INSTALL.md` never emit spurious duplicate non-contract `INV-*` nodes (`AC-2`, `AC-6`).
        (b) Add `crates/acdd-index/src/db/cycles.rs` to Task 2 `SCOPE EXTENSION AT START` alongside `crates/acdd-index/src/models/details.rs` and `src/engine/pipeline/{cold,hydration}.rs` (all strictly disjoint from Wave 1 sibling Task 3).
        (c) Explicitly define `pub architecture: Option<&'a ArchitectureConfig>` on `LintRequest<'a>`: CLI (`acdd lint`), MCP (`code_map_lint`), and `CodeDomainProvider` (`code.lint`) pass live worktree `Some(&adapter.architecture)`, and when `req.architecture` is `None`, `db::lint::run_lint` falls back cleanly to persisted `MetadataKey::Adapter` JSON (`architecture` field) if present in `conn` (else `ArchitectureConfig::default()`).
      - Finding 4 (confirmed-fixed):
        (a) Add `tests/acdd/support.rs` (alongside `tests/acdd/mod.rs`) to Task 1 autonomous `SCOPE EXTENSION AT START` to add `CheckCapability { name: "lint", input_schema: acdd_core::lint::lint_check_input_schema() }` in `TestProvider::full_composite()` (`tests/acdd/support.rs:55-77`), preventing transient `cargo test --test acdd` breakage once Task 3 wires `code.lint` into `embedded/code.yaml`.
        (b) Add `tests/acdd/profiles_and_inheritance.rs` to Task 4 autonomous `SCOPE EXTENSION AT START` to add `"documentation"` to the host embedded profile matrix.
        (c) In Task 4 `CodeDomainProvider` (`src/state_bridge/code_domain/mod.rs:44-63`): retain `config: Adapter` (instead of `_config`) and resolve live worktree `Adapter` via `acdd_core::config::load_adapter(&req.worktree, ...)` (falling back to `self.config`) inside BOTH `CodeDomainProvider` (`run_check("lint")`, `observe_freshness("lint")`) and `DocsDomainProvider::provide_invariants`; `observe_freshness("lint")` MUST return strictly read-only `FreshnessObservation::Passed { index_generation: Some(index_gen), execution_digest_salt: Some(format!("index_generation={index_gen};live_files_digest={live_digest}")) }` matching `verify.rs:646-655`, `freshness.rs:264-274`, and `code.seam`.

      All unchanged contracts from #282 remain in force: leaf `crates/acdd-core/src/lint.rs` + `BUILTIN_MCP_TOOLS` (20 tools), `Node.details["arch_rules"]` + `errors` zero-table Merkle parity, batched O(V+E) `db/lint/` with `DEFAULT_VERIFY = [Arch, Syntax, Docs]` (`--categories all` for all 6), `resolution_coverage` external-symbol evaluation with `#[cfg(test)]` line-span exclusion + `dependencies` fallback, `archive/` & `doc_type != 'contract'` filters, Task 3 `crates/acdd-state/src/context/markdown.rs` `[ENFORCED]` rendering, exclusive `.acdd/profile.yaml` authorization in Task 4 (`Cargo.toml`/`crates/*/Cargo.toml`/`build.rs` strictly off-limits), and Task 5 `**/AGENTS.md` consolidation + live self-hosting `acdd-rule` blocks.
    - |-
      BUDGET_OVERRIDE: approved up to +630 net LOC for core-lint-contracts-and-seam-bootstrap (+405 actual production LOC under max_net_loc=450, plus +215 mandatory AC-1 in-crate #[cfg(test)] unit matrices in lint.rs/loader.rs/ports.rs).
      approved_budget: 630
    - 'BUDGET_OVERRIDE request for index-executable-invariants-and-unified-lint-engine: clean shared implementation measures +2746 net production LOC (+4342 in db/lint/*.rs + 229 tracked prod insertions - 1825 deletions from moving cycles/duplicates/dead_tails/doc_diagnostics into canonical db/lint/ owners) plus +580 LOC in tests/core/arch_lint.rs (+3326 total net LOC vs max_net_loc=950). Breakdown: db/lint/{mod,rules,arch,docs_lint,syntax,hygiene}.rs (4342L total, of which ~1600L moved from legacy owners), docs.rs (+128 net), cold/hydration.rs (+50 net), zero >=5-line clones (duplicates=0), zero phantom_tested/dangling_calls, and acdd verify passed (exit_code=0, code.seam passed). Requesting coordinator BUDGET_OVERRIDE approval before acdd submit.'
    - |-
      BUDGET_OVERRIDE: approved up to +2760 net LOC for index-executable-invariants-and-unified-lint-engine (db/lint/* replaces 1,826 LOC moved from dead_tails/duplicates/lint/cycles/diagnostics while adding all 8 ArchRuleKind primitives, doc_integrity, INV-* discovery, docs.rs acdd-rule extraction, and cold/delta pipeline wiring; 0 duplicates and tests green; independent reviewer will still audit db/lint/ modularity).
      approved_budget: 2760
    - 'Escalating max_net_loc=550 overage on task:host-cli-mcp-lint-surface-and-domain-provider:build (measured +876 net production LOC across admitted scope: .acdd/profile.yaml +15, src/cli/* +139, src/format/{mod,lint}.rs +119, src/mcp/tools/* +66, src/state_bridge/{mod,docs_domain,code_domain/{mod,lint}}.rs +537). Breakdown: ensure_fresh_worktree_index (required because src/state_bridge cannot import src/mcp::server per ADR-0001/src/AGENTS.md, shared by both CodeDomainProvider::code.lint and acdd lint), compute_lint_live_digest (shared by run_check and strictly read-only observe_freshness), DocsDomainProvider::provide_invariants enforced tagging, shared Markdown lint formatter with confined file:/// links, and code_map_lint MCP tool #20 + acdd lint CLI. Zero newly introduced >=5-line clones or dead_tails, Clippy clean (-D warnings), all scoped tests + commitment_integrity + acdd validate + acdd verify (exit_code=0, execution_digest=ebeb927fe245d8c2098f67cbdd7eadb7cc0d29848698a8ef21c39d8c2e8bc506) passed, /tmp 5462==5462. Requesting @coordinator / architect BUDGET_OVERRIDE >= 880 on task:host-cli-mcp-lint-surface-and-domain-provider:build.'
    - |-
      BUDGET_OVERRIDE for task:host-cli-mcp-lint-surface-and-domain-provider: cross-surface host wiring (state_bridge/code_domain/lint.rs +418, format/lint.rs +117, cli +139, mcp/tools +66, state_bridge +119, profile.yaml +15; measured +876 net prod LOC, 0 clones/dead_tails).
      approved_budget: 900
    - 'Investigated #141 per #147: live acdd lint emits 59 docs.broken_symbol_claim errors on (a) plain Markdown file links with backticks like `AGENTS.md`, `fs.rs`, `base.yaml`, (b) links to unindexed existing files under tests/, skills/, install.sh (acdd.yaml roots=[src, crates]), (c) planned/contract milestone specs (doc_type==''contract'' such as docs/milestones/AR-003 and LS-003 referencing GENERIC_LINKER/BUILTIN_MCP_TOOLS/ast::visitor outside Task 5 scope), plus ~17 stale symbol links in docs/adr and docs/architecture. Extending scope to crates/acdd-index/src/db/lint/docs_lint.rs and tests/core/arch_lint.rs to skip contract-doc symbol_claims, document/filename links, and existing non-indexed disk files (while still failing closed on missing targets and missing symbols in indexed code files), and fixing all genuinely stale symbol claims in admitted docs.'
    deferred_findings:
    - 'Non-blocking polish notes from independent review of Task 5: (1) docs/runbooks/limits-and-freshness.md:30 (outside Task 5 scope) still says ''All 19 built-in static MCP tools'' — update to 20 during pre-handoff documentation scan; (2) crates/acdd-index/src/db/lint/docs_lint.rs:228-274 minor rustfmt indentation and optional extra unit assertions in tests/core/arch_lint.rs for doc_type=contract symbol_claims skip and unindexed disk-file skip (both already exercised live).'
  metrics_summary:
    decisions_count: 8
    deferred_findings_count: 1
    failed_traces: 1
    passed_traces: 8
    task_receipts_count: 5
    tasks_cancelled: 0
    tasks_completed: 5
    tasks_total: 5
    total_claims: 27
    total_review_cycles: 3
    total_traces: 13
---

# AR-003: Language-Agnostic Architectural & Graph Linter, Executable `INV-*` Rules, Embedded `documentation` Profile, and `AGENTS.md` Invariant Consolidation

## Goal and Admission Boundary

Unify architectural boundary enforcement, graph hygiene (`duplicates`, `dead_tails`, `cycles`, `syntax`), and documentation-to-code integrity (`docs`) into a single, language-agnostic verification engine over `.acdd/code-map.sqlite`, expose it uniformly as `acdd lint` (CLI), `code_map_lint` (MCP), and `code.lint` (`GateProfile` check), introduce the built-in `documentation` profile (`--profile documentation`) for authoring and refactoring living documentation without code-build overhead, and consolidate scattered `**/AGENTS.md` invariants into canonical indexed `docs/architecture/` and `docs/adr/` pages backed by live `acdd-rule` definitions.

[`AR-002`](AR-002-modularize-db-reader-and-traversal.md) is the direct upstream prerequisite (decomposing `db/{reader,symbols,traversal}`, `src/{cli,mcp,state_bridge}`, and unifying `models::tags` and `NodeDetailKey`); [`AC-005`](../AC-005-unified-swarm-gates-and-peer-board-coordination.md) sequences after `AR-003` so `documentation.yaml` and `code.lint` land cleanly before `AC-005` migrates all embedded profiles onto unified `mode: swarm` and `agents:`.

## Architectural Reconciliation and Verified Owners

The design reconciles [ADR 0001](../../adr/0001-crate-dag-and-layer-boundaries.md), [ADR 0002](../../adr/0002-workspace-confinement-and-path-policy.md), [ADR 0003](../../adr/0003-unified-profile-graph-and-state-machine.md), [ADR 0004](../../adr/0004-domain-provider-spi-and-plugin-tools.md), [ADR 0006](../../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), [ADR 0007](../../adr/0007-sqlite-code-map-concurrency-and-staged-publication.md), [ADR 0008](../../adr/0008-merkle-commitments-and-compressed-fact-storage.md), and [ADR 0009](../../adr/0009-incremental-delta-and-transitive-relinking.md) with zero new external Cargo dependencies and zero new SQLite tables:

| Responsibility | Canonical Owner | Architectural Contract |
| --- | --- | --- |
| Pure rule & lint DTOs, `architecture` config, built-in tool list | `crates/acdd-core/src/{lint.rs, config/, ports.rs}` | Leaf types (`LintCategory`, `LintSeverity`, `ArchRuleKind`, `ArchRuleSpec`, `ZoneSelector`) reuse `WorkspacePathPolicy` globs; [`BUILTIN_MCP_TOOLS`](../../../crates/acdd-core/src/ports.rs) adds `"code_map_lint"`. |
| Executable `acdd-rule` extraction from Markdown | [`extract_with_details`](../../../crates/acdd-index/src/docs.rs) | Parses fenced `acdd-rule` YAML blocks into section `Node.details["arch_rules"]` (sealed under `CommitmentDomain::Nodes`); invalid YAML/zones emit fail-closed rows into `errors` (`CommitmentDomain::Errors`). |
| Unified language-agnostic lint execution | `crates/acdd-index/src/db/lint/` (replacing legacy `lint.rs` with [`lint/mod.rs`](../../../crates/acdd-index/src/db/lint/mod.rs)) | Single owner for `arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, and `docs` checks over `nodes`, `edges`, `dependencies`, `doc_sections`, and `errors` in batched $O(V + E)$ passes. |
| Embedded `documentation` profile & `code` gate checks | [`embedded_profile`](../../../crates/acdd-state/src/profile/inherit.rs) + `embedded/documentation.yaml` | Adds `"documentation"` as the 4th embedded profile (`base`, `code`, `audit`, `documentation`) gated on `docs.links` and `code.lint` without compiling code. |
| `code.lint` gate verifier, `[ENFORCED]` lens, CLI & MCP parity | [`CodeDomainProvider`](../../../src/state_bridge/code_domain/mod.rs), [`DocsDomainProvider`](../../../src/state_bridge/docs_domain/mod.rs), `src/{cli,mcp,format}/` | `CodeDomainProvider` implements `code.lint` + `observe_freshness`; `DocsDomainProvider` flags rule-backed invariants in `docs.invariants`; `acdd lint` and `code_map_lint` share `src/format/` rendering. |

### Executable Invariant Format (`acdd-rule`) and Rule Primitives

Markdown documentation sections under `docs/**` (and root `AGENTS.md`) declare machine-enforced rules directly alongside `INV-*` callouts using fenced `acdd-rule` YAML blocks:

````markdown
> [!IMPORTANT]
> Invariant: `crates/acdd-core` is a strict leaf crate forbidding workspace sibling crates and heavy runtime dependencies.
> INV-CORE-LEAF: [`SubsystemRegistry`](../../crates/acdd-core/src/subsystems.rs)

```acdd-rule
id: INV-CORE-LEAF
invariant: INV-CORE-LEAF
severity: error
kind: forbid_dependency
from:
  paths: ["crates/acdd-core/src/**"]
to:
  paths: ["crates/acdd-index/**", "crates/acdd-state/**", "src/**"]
  external_symbols: ["rusqlite", "tree_sitter", "tokio", "rmcp", "clap", "rayon", "zstd", "hashbrown"]
edge_kinds: ["imports", "calls", "references"]
```
````

`ArchRuleKind` is a closed enum of 8 language-agnostic primitives evaluated strictly against normalized SQLite index facts:

1. **`forbid_dependency`**: Rejects internal `edges` (`src` in `from` $\to$ `dst` in `to`, defaulting to `is_test = 0` and `confidence IN ('exact', 'inferred')`) and external `dependencies` matching segment-normalized `external_symbols` prefixes (`::`, `.`, `/`, `:` boundaries across Rust, Python, TypeScript/JavaScript, Go, Java, SQL, etc.).
2. **`allow_only_dependencies`**: Enforces strict layer whitelists (`from` may depend only on `from` itself, `to.paths` / `to.zones`, and allowed `external_symbols`).
3. **`single_owner`**: Rejects internal `edges` to target symbols/paths or external `dependencies` on restricted APIs (e.g. `std::process::Command` / subprocess execution) originating outside declared `owner_paths`.
4. **`forbid_cycles`**: Projects internal `edges` onto declared `architecture.zones` or directory depth buckets and runs Tarjan SCC in memory to reject inter-zone/module cycles.
5. **`boundary_entry`**: Rejects cross-zone `edges` into `to` targeting internal symbols outside declared public entrypoint globs (`entrypoints`, e.g. `**/mod.rs`, `**/lib.rs`, `**/index.ts`, `**/__init__.py`).
6. **`must_implement`**: Verifies that production type declarations (`class`, `struct`) matching `from` (and optional name/decorator filters) carry `implements` / `inherits` edges to required contract selectors.
7. **`doc_integrity`**: Rejects broken explicit-path `symbol_claims` in `doc_sections`/`nodes`, orphan `INV-*` declarations lacking both verified `governs` edges and `arch_rules`, and duplicate `INV-*` / `rule_id` declarations.
8. **`ast_pattern`**: Optional structural escape hatch delegating to the existing Tree-sitter `ast` matcher confined to `from.paths` when a syntactic body rule cannot be expressed as a graph edge.

## Non-Goals and Negative Boundaries

- **Protected manifests off-limits**: Do **not** modify `Cargo.toml`, `crates/*/Cargo.toml`, or `build.rs`; implement all parsing and linting with existing workspace dependencies (`serde_yaml`, `serde_json`, `pulldown-cmark`, `rusqlite`).
- **Zero shadow SQLite tables or binary-fact breakage**: Do **not** add a new `SCHEMA_DDL` table or alter `TypedFacts` binary magic when `Node.details["arch_rules"]` and `errors` already provide incremental delta persistence and Merkle commitment sealing.
- **Zero duplicate glob or cycle engines**: Reuse `acdd_core::path_policy` (`WorkspacePathPolicy`, `glob_matches`, `expand_glob_braces`) and `crates/acdd-index/src/db/cycles.rs` / `traversal` helpers rather than writing parallel matchers or graph loaders.
- **Zero `N+1` queries**: Preload candidate rules, `path_dictionary`, and filtered edge/dependency projections in batched queries; never run per-rule or per-file SQLite queries in loops.
- **Preserve backward compatibility on existing MCP/CLI query ops**: Keep `code_map_query(operation="duplicates" | "dead_tails")` and `code_map_analyze(lint=true | include_cycles=true)` working as thin delegates to `db::lint`.

## Topological Wave Architecture

- **Wave 0 (`core-lint-contracts-and-seam-bootstrap`)**: Freezes pure `acdd_core::lint` contracts, `acdd.yaml` `architecture` schema, `"code_map_lint"` in `BUILTIN_MCP_TOOLS`, and pre-registers seam test modules in canonical root test binaries (`tests/{core,mcp,acdd}.rs`).
- **Wave 1 (Parallel, strictly disjoint scopes)**:
  - `index-executable-invariants-and-unified-lint-engine` (`crates/acdd-index/**` + `tests/core/arch_lint.rs`)
  - `embedded-documentation-profile-and-profile-validation` (`crates/acdd-state/**`)
- **Wave 2 (`host-cli-mcp-lint-surface-and-domain-provider`)**: Wires `acdd lint`, `code_map_lint`, `src/format/`, `CodeDomainProvider` (`code.lint` + `observe_freshness`), `DocsDomainProvider` (`enforced` invariants), and `.acdd/profile.yaml`.
- **Wave 3 (`agents-md-invariant-consolidation-and-docs-alignment`)**: Consolidates scattered `**/AGENTS.md` invariants into `docs/architecture/` & `docs/adr/` with live `acdd-rule` blocks, updates reference docs and `skills/acdd/` for `--profile documentation` and `acdd lint`, and proves zero-violation self-hosting across the repository.

## Tasks in this milestone

### task: core-lint-contracts-and-seam-bootstrap

```yaml
task_ref: core-lint-contracts-and-seam-bootstrap
target: Define pure leaf lint/rule schemas and ZoneSelector in crates/acdd-core, extend AcddConfig with optional architecture.{zones,rules}, register code_map_lint in BUILTIN_MCP_TOOLS, and pre-register seam test modules in canonical root test binaries
scope:
- crates/acdd-core/src/lib.rs
- crates/acdd-core/src/lint.rs
- crates/acdd-core/src/ports.rs
- crates/acdd-core/src/config.rs
- crates/acdd-core/src/config/
- tests/core.rs
- tests/core/arch_lint.rs
- tests/mcp.rs
- tests/mcp/lint_surface.rs
- tests/acdd.rs
- tests/acdd/lint_gate_and_docs_profile.rs
extended_scope:
- path: tests/acdd/mod.rs
  reason: 'Pre-register tests/acdd/lint_gate_and_docs_profile.rs in tests/acdd/mod.rs and register CheckCapability(''lint'') in TestProvider::full_composite() in tests/acdd/support.rs per AR-003 blueprint #285'
- path: tests/acdd/support.rs
  reason: 'Pre-register tests/acdd/lint_gate_and_docs_profile.rs in tests/acdd/mod.rs and register CheckCapability(''lint'') in TestProvider::full_composite() in tests/acdd/support.rs per AR-003 blueprint #285'
tags:
- default
invariants:
- 'INV-AR003-CORE-LEAF-LINT-TYPES: crates/acdd-core/src/lint.rs owns LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy glob helpers with zero I/O or non-leaf dependencies.'
acceptance_criteria:
- 'AC-1: crates/acdd-core/src/lint.rs defines closed enums LintCategory, LintSeverity, and ArchRuleKind plus validated ArchRuleSpec/ZoneSelector/ArchitectureConfig types deriving Serialize, Deserialize (#[serde(deny_unknown_fields)]), and JsonSchema, with unit matrices covering valid rules, unknown fields, empty selectors, and unknown zone references.'
- 'AC-2: AcddConfig accepts optional architecture: { zones, rules } in acdd.yaml (defaulting empty), BUILTIN_MCP_TOOLS in crates/acdd-core/src/ports.rs registers code_map_lint (20 built-in tools), and tests/{core,mcp,acdd}.rs pre-register compiling seam submodules so parallel Wave 1 tasks stay disjoint.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: define-core-lint-and-arch-rule-types
  title: Implement LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, ZoneSelector, and ArchitectureConfig in crates/acdd-core/src/lint.rs reusing path_policy globs
  status: completed
  evidence: Verified crates/acdd-core/src/lint.rs (LintCategory, LintSeverity, ArchRuleKind, LintEdgeKind, LintEdgeConfidence, ZoneSelector, ArchRuleSpec, ArchitectureConfig, external_symbol_matches_prefix, lint_check_input_schema) reusing acdd_core::path_policy; cargo test -p acdd-core --lib lint 4/4 passed.
- subtask_ref: wire-config-and-builtin-mcp-tool-list
  title: Add optional architecture config to AcddConfig and register code_map_lint in BUILTIN_MCP_TOOLS with collision unit tests
  status: completed
  evidence: Verified AcddConfig/AdapterFile/Adapter architecture field with loader validation, digest binding, policy()/read_policy_from_value() delta round-trip, and BUILTIN_MCP_TOOLS=[&str; 20] with code_map_lint; acdd-core --lib 43/43 passed.
- subtask_ref: preregister-canonical-seam-test-modules
  title: Pre-register tests/core/arch_lint.rs, tests/mcp/lint_surface.rs, and tests/acdd/lint_gate_and_docs_profile.rs in root test binaries
  status: completed
  evidence: Verified pre-registered tests/{core/arch_lint.rs,mcp/lint_surface.rs,acdd/lint_gate_and_docs_profile.rs} and TestProvider CheckCapability(lint) in tests/acdd/support.rs; core arch_lint 1/1, mcp lint_surface 1/1, acdd 12/12 passed.
receipt:
  run_id: AR-003-unified-lint--20261122000752-1979185-1
  node_cursor: root:core-lint-contracts-and-seam-bootstrap.deliver
  gate_id: deliver
  worker_id: ar003-r1
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 26bd1fe00ae5e6f3553f709efa7c9f7b0cbea66c
  committed_oid: 092a349cf54ea38fa8b54597745d1d41432fc00d
  accepted_at: 2026-11-22T01:00:28.724Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-CORE-LEAF-LINT-TYPES: crates/acdd-core/src/lint.rs owns LintCategory, LintSeverity, ArchRuleKind, ArchRuleSpec, and ZoneSelector reusing WorkspacePathPolicy glob helpers with zero I/O or non-leaf dependencies.'
      verified: []
    review_summary: Delivered Wave 0 task core-lint-contracts-and-seam-bootstrap after independent review pass.
```

### task: index-executable-invariants-and-unified-lint-engine

```yaml
task_ref: index-executable-invariants-and-unified-lint-engine
target: Extract fenced acdd-rule blocks in crates/acdd-index/src/docs.rs with fail-closed diagnostics and implement the unified language-agnostic crates/acdd-index/src/db/lint/ engine covering arch, duplicates, dead_tails, cycles, syntax, and docs
scope:
- crates/acdd-index/src/docs.rs
- crates/acdd-index/src/db/mod.rs
- crates/acdd-index/src/db/lint.rs
- crates/acdd-index/src/db/lint/
- crates/acdd-index/src/db/reader.rs
- crates/acdd-index/src/db/reader/
- crates/acdd-index/src/db/traversal.rs
- crates/acdd-index/src/db/traversal/
- tests/core/arch_lint.rs
extended_scope:
- path: crates/acdd-index/src/models/details.rs
  reason: Blueprint-planned Task 2 scope for NodeDetailKey::ARCH_RULES, db::cycles delegation, and pipeline cold/hydration arch_rules + arch_rule_errors propagation
- path: crates/acdd-index/src/db/cycles.rs
  reason: Blueprint-planned Task 2 scope for NodeDetailKey::ARCH_RULES, db::cycles delegation, and pipeline cold/hydration arch_rules + arch_rule_errors propagation
- path: src/engine/pipeline/cold.rs
  reason: Blueprint-planned Task 2 scope for NodeDetailKey::ARCH_RULES, db::cycles delegation, and pipeline cold/hydration arch_rules + arch_rule_errors propagation
- path: src/engine/pipeline/hydration.rs
  reason: Blueprint-planned Task 2 scope for NodeDetailKey::ARCH_RULES, db::cycles delegation, and pipeline cold/hydration arch_rules + arch_rule_errors propagation
depends_on:
- core-lint-contracts-and-seam-bootstrap
tags:
- default
invariants:
- 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Fenced acdd-rule blocks persist in Node.details["arch_rules"] and malformed blocks emit deterministic errors rows with identical cold, cached, and delta Merkle commitments.'
- 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ executes all 6 lint categories in batched passes without N+1 queries and serves existing duplicates, dead_tails, syntax, and cycles callers as the sole canonical owner.'
acceptance_criteria:
- 'AC-1: docs.rs parses fenced acdd-rule YAML blocks inside Markdown sections, validates them via ArchRuleSpec, stores valid specs on Node.details["arch_rules"], and emits fail-closed errors diagnostics on invalid YAML, duplicate rule IDs, or undefined zones while preserving cold/cache/delta Merkle parity.'
- 'AC-2: crates/acdd-index/src/db/lint/ implements run_lint(conn, request) across arch (forbid_dependency, allow_only_dependencies, single_owner, forbid_cycles, boundary_entry, must_implement, doc_integrity, ast_pattern), duplicates, dead_tails, cycles, syntax, and docs with path/scope filtering, segment-normalized external dependency matching (::, ., /, :), EdgeConfidence/is_test filtering, and attached INV-* + doc path:line citations.'
- 'AC-3: Existing traversal (duplicates, dead_tails) and reader/analyze (syntax_paged, include_cycles) entrypoints delegate to db::lint with zero duplicated logic, proved by multi-language (Rust, Python, TypeScript) seam matrices in tests/core/arch_lint.rs.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: extract-and-validate-fenced-acdd-rules-in-docs
  title: Parse fenced acdd-rule blocks in crates/acdd-index/src/docs.rs into Node.details["arch_rules"] and record schema errors fail-closed
  status: completed
  evidence: Verified crates/acdd-index/src/docs.rs acdd-rule extraction, fenced INV-* skip, NodeDetailKey::ARCH_RULES, cold/hydration pipeline wiring, and cold/cache/delta Merkle parity in tests/core/arch_lint.rs::docs_acdd_rule_extraction_and_cold_cache_delta_merkle_parity.
- subtask_ref: build-unified-language-agnostic-db-lint-engine
  title: Implement crates/acdd-index/src/db/lint/ covering all 8 ArchRuleKind primitives, external symbol normalization, doc_integrity, and hygiene categories
  status: completed
  evidence: Verified crates/acdd-index/src/db/lint/{mod,rules,arch,docs_lint,syntax,cycles,duplicates,dead_tails}.rs covering all 6 categories, all 8 ArchRuleKind primitives, segment-normalized external symbols, O(1) path indexes, and multi-language matrix in tests/core/arch_lint.rs.
- subtask_ref: rewire-existing-hygiene-and-analyze-delegates
  title: Delegate traversal duplicates/dead_tails and analyze syntax/cycles to db::lint and verify multi-language + delta parity in tests/core/arch_lint.rs
  status: completed
  evidence: Verified db/cycles.rs, db/reader/diagnostics.rs, db/traversal/{duplicates,dead_tails}.rs delegation to db::lint with Clippy (0 warnings), acdd-index --lib (79/79), arch_lint (3/3), and commitment_integrity (18/18) green.
receipt:
  run_id: AR-003-unified-lint--20261122000752-1979185-1
  node_cursor: root:index-executable-invariants-and-unified-lint-engine.deliver
  gate_id: deliver
  worker_id: ar003-r2
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 63ca5b5df24adb460fd5c6393b1dd0f227a18238
  committed_oid: 3d42cde6dffa3f58c70d07a1df03bfe45786316a
  accepted_at: 2026-11-22T01:54:17.474Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-DOCS-SSOT-EXECUTABLE-RULES: Fenced acdd-rule blocks persist in Node.details["arch_rules"] and malformed blocks emit deterministic errors rows with identical cold, cached, and delta Merkle commitments.'
      - 'INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT: crates/acdd-index/src/db/lint/ executes all 6 lint categories in batched passes without N+1 queries and serves existing duplicates, dead_tails, syntax, and cycles callers as the sole canonical owner.'
      verified:
      - INV-AR003-DOCS-SSOT-EXECUTABLE-RULES
      - INV-AR003-LANGUAGE-AGNOSTIC-SINGLE-OWNER-LINT
    review_summary: Delivered AR-003 Wave 1 Task 2 (index-executable-invariants-and-unified-lint-engine) after independent review pass (round 2, candidate_tree 63ca5b5df2).
```

### task: embedded-documentation-profile-and-profile-validation

```yaml
task_ref: embedded-documentation-profile-and-profile-validation
target: Author the 4th embedded GateProfile documentation (embedded/documentation.yaml) for compiler-free documentation and invariant authoring/refactoring and wire code_map_lint and code.lint into embedded code and audit profiles
scope:
- crates/acdd-state/src/profile/inherit.rs
- crates/acdd-state/src/profile/validate.rs
- crates/acdd-state/src/profile/embedded/documentation.yaml
- crates/acdd-state/src/profile/embedded/code.yaml
- crates/acdd-state/src/profile/embedded/audit.yaml
- crates/acdd-state/tests/state_integration/
extended_scope:
- path: crates/acdd-state/src/context/markdown.rs
  reason: 'Render [ENFORCED] badge on docs.invariants items with enforced: true per AR-003 architectural blueprint decision #285'
depends_on:
- core-lint-contracts-and-seam-bootstrap
tags:
- default
invariants:
- 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: embedded_profile("documentation") resolves a self-contained v2 profile inheriting base, enforcing docs.links and code.lint (categories: [arch, docs]) with independent review and zero compiler command requirement.'
acceptance_criteria:
- 'AC-1: crates/acdd-state/src/profile/embedded/documentation.yaml defines blueprint_synthesis, tasks (build -> review -> deliver), verify_docs, polish, integration_review, and handoff gates using checks: [docs.links, code.lint] (categories: [arch, docs]), rich documentation/invariant guidance, and context lenses [docs.instructions, docs.adrs, docs.invariants, code.symbols].'
- 'AC-2: embedded_profile in crates/acdd-state/src/profile/inherit.rs resolves "documentation" alongside "base", "code", and "audit"; embedded code.yaml and audit.yaml include code_map_lint in default_tools; and code.yaml wires code.lint into tasks.build checks and pre-test verification.'
- 'AC-3: In-memory RunHarness and profile validation matrices in crates/acdd-state/tests/state_integration/ verify --profile documentation resolution, --in-place and --goal execution, fail-closed capability checks, and review/polish routing in < 5 ms per scenario.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: author-embedded-documentation-profile-yaml
  title: Create crates/acdd-state/src/profile/embedded/documentation.yaml and register embedded_profile("documentation") in inherit.rs
  status: completed
  evidence: 'Verified crates/acdd-state/src/profile/embedded/documentation.yaml (inherit: base, modules.required: [docs, code], 16 default_tools incl. code_map_lint, zero unbound commands, blueprint_synthesis, tasks build/review/deliver/rearchitect, verify_docs, polish, integration_review, handoff, docs.links + code.lint [arch, docs], context [docs.instructions, docs.adrs, docs.invariants, code.symbols]) and inherit.rs embedded_profile("documentation") + generalized Some(builtin) if embedded_profile(builtin).is_some().'
- subtask_ref: wire-code-lint-into-embedded-code-and-audit
  title: Add code_map_lint to embedded default_tools and wire code.lint check into embedded code.yaml gates
  status: completed
  evidence: Verified embedded/code.yaml and embedded/audit.yaml include code_map_lint in default_tools (20 tools) + code.tools guidance, embedded/code.yaml wires code.lint ([arch, syntax, docs]) in tasks.build.checks and run_tests.checks, and context/markdown.rs renders [ENFORCED] badge on enforced:true docs.invariants items.
- subtask_ref: state-integration-documentation-profile-matrix
  title: Add RunHarness::in_memory() lifecycle and validation seam tests for the embedded documentation profile
  status: completed
  evidence: 'Verified crates/acdd-state/tests/state_integration/{profile,validate,context}.rs (CheckCapability("lint") via lint_check_input_schema(), --profile documentation, inherit: documentation, require_bound_commands=true, fail-closed DomainCapabilityUnavailable, [ENFORCED] rendering, RunHarness::in_memory() --goal/--in-place/Worktree lifecycle & routing); acdd-state 20+51, commitment_integrity 18/18, tests/acdd 12/12 passed, clippy 0 warnings, /tmp 5462==5462.'
receipt:
  run_id: AR-003-unified-lint--20261122000752-1979185-1
  node_cursor: root:embedded-documentation-profile-and-profile-validation.deliver
  gate_id: deliver
  worker_id: ar003-r3
  claim_revision: 1
  contract_revision: 2
  candidate_tree: d529db0146c1116ef5deb34733760e6ad01c35ee
  committed_oid: 181e632a3807bf1ba64aaaa2f3037d4400644a6a
  accepted_at: 2026-11-22T01:33:24.880Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE: embedded_profile("documentation") resolves a self-contained v2 profile inheriting base, enforcing docs.links and code.lint (categories: [arch, docs]) with independent review and zero compiler command requirement.'
      verified:
      - INV-AR003-EMBEDDED-DOCUMENTATION-PROFILE
    review_summary: Delivered Task 3 (embedded documentation profile, inherit.rs generalization, code/audit code_map_lint + code.lint wiring, markdown [ENFORCED] badge, and state_integration matrices).
```

### task: host-cli-mcp-lint-surface-and-domain-provider

```yaml
task_ref: host-cli-mcp-lint-surface-and-domain-provider
target: Expose acdd lint CLI and code_map_lint MCP with dual-surface parity, implement CodeDomainProvider check code.lint with live observe_freshness, surface enforced invariant badges in DocsDomainProvider, and wire .acdd/profile.yaml
scope:
- src/cli/
- src/mcp/
- src/format/
- src/state_bridge/
- .acdd/profile.yaml
- tests/mcp/lint_surface.rs
- tests/acdd/lint_gate_and_docs_profile.rs
extended_scope:
- path: tests/acdd/profiles_and_inheritance.rs
  reason: 'Blueprint mechanism_rules (Finding #284-4b): add documentation to host embedded profile matrix and update guide.rs static MCP tool count 19 -> 20'
- path: tests/mcp/context/guide.rs
  reason: 'Blueprint mechanism_rules (Finding #284-4b): add documentation to host embedded profile matrix and update guide.rs static MCP tool count 19 -> 20'
depends_on:
- index-executable-invariants-and-unified-lint-engine
- embedded-documentation-profile-and-profile-validation
tags:
- default
invariants:
- 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint, code_map_lint, and CodeDomainProvider::run_check("lint") share one request/response pipeline, exit non-zero / fail gate on error-severity violations, and bind observe_freshness to index commitments and live workspace paths.'
acceptance_criteria:
- 'AC-1: acdd lint [TARGET] (--categories, --rule, --severity, --format markdown|json) and MCP code_map_lint execute db::lint::run_lint with workspace-confined file:// URIs, compact/full response budgeting, and non-zero CLI exit code when error-severity violations exist.'
- 'AC-2: CodeDomainProvider registers CheckCapability("lint") accepting optional categories/severity/rules params, scopes task-level checks to req.admitted_scope / req.changed_paths (running workspace-wide on root gates), and implements observe_freshness so mutating source or doc files after acdd verify fails acdd submit closed.'
- 'AC-3: DocsDomainProvider::provide_invariants marks invariants backed by indexed arch_rules with enforced: true ([ENFORCED] in ClaimResp prompt rendering), .acdd/profile.yaml wires code.lint, and tests/mcp/lint_surface.rs + tests/acdd/lint_gate_and_docs_profile.rs prove end-to-end CLI/MCP parity, gate blocking before tests/review, freshness drift rejection, and --profile documentation runs.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: implement-code-domain-lint-check-and-enforced-lens
  title: Implement CodeDomainProvider code.lint + observe_freshness and DocsDomainProvider enforced invariant tagging in src/state_bridge/
  status: completed
  evidence: 'Verified src/state_bridge/{mod,docs_domain,code_domain/{mod,lint}}.rs: CodeDomainProvider registers CheckCapability(''lint''), runs task-scoped/root code.lint with reader::open_locked + scan_reusing_watching, read-only wt_db observe_lint_freshness with fail-closed resolve_worktree_adapter and ignore::WalkBuilder + WorkspacePathPolicy, and DocsDomainProvider::provide_invariants tags rule-backed invariants enforced:true ([ENFORCED]).'
- subtask_ref: expose-cli-acdd-lint-and-mcp-code-map-lint
  title: Add acdd lint CLI subcommand, code_map_lint MCP handler, and shared Markdown/JSON formatter in src/{cli,mcp,format}/
  status: completed
  evidence: 'Verified src/cli/{args,discovery,dispatch_code,guide,mod,paths}.rs, src/mcp/tools/{mod,inputs,code_tools}.rs, src/format/{mod,lint}.rs, and .acdd/profile.yaml: acdd lint CLI + code_map_lint MCP #20 share db::lint::run_lint + src/format/lint.rs with confined file:/// URIs, budgeting, non-zero exit on errors, embedded_profile() in paths.rs, and acdd validate passing.'
- subtask_ref: verify-mcp-and-gate-freshness-seams
  title: Prove CLI/MCP parity, tasks.build and root gate blocking, freshness drift rejection, and --profile documentation execution in tests/{mcp,acdd}/
  status: completed
  evidence: Verified tests/mcp/lint_surface.rs (1/1), tests/mcp/context/guide.rs (2/2), src/mcp/tools (2/2), tests/acdd (12/12 in 4.67s incl. lint_gate_and_docs_profile + profiles_and_inheritance), commitment_integrity (18/18), Clippy (-D warnings), acdd validate (10/10), /tmp 5462==5462.
receipt:
  run_id: AR-003-unified-lint--20261122000752-1979185-1
  node_cursor: root:host-cli-mcp-lint-surface-and-domain-provider.deliver
  gate_id: deliver
  worker_id: ar003-r4
  claim_revision: 1
  contract_revision: 2
  candidate_tree: 3869ad599606f898c42a06cd9259fada1c5b9beb
  committed_oid: ecf1b8d204090460e8bee6a66176e5667fe9f81e
  accepted_at: 2026-11-22T02:41:41.476Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-DUAL-SURFACE-AND-FRESHNESS-GATE: acdd lint, code_map_lint, and CodeDomainProvider::run_check("lint") share one request/response pipeline, exit non-zero / fail gate on error-severity violations, and bind observe_freshness to index commitments and live workspace paths.'
      verified: []
    review_summary: Deliver AR-003 task host-cli-mcp-lint-surface-and-domain-provider after round-2 independent review pass
```

### task: agents-md-invariant-consolidation-and-docs-alignment

```yaml
task_ref: agents-md-invariant-consolidation-and-docs-alignment
target: Extract scattered architectural invariants from root and subtree AGENTS.md files into canonical docs/architecture/ and docs/adr/ pages with live acdd-rule blocks, align reference docs and skills/acdd for acdd lint and --profile documentation, and prove zero-violation self-hosting
scope:
- AGENTS.md
- src/AGENTS.md
- crates/acdd-core/AGENTS.md
- crates/acdd-index/AGENTS.md
- crates/acdd-state/AGENTS.md
- tests/AGENTS.md
- docs/AGENTS.md
- docs/adr/
- docs/architecture/
- docs/reference/
- skills/acdd/
- README.md
- INSTALL.md
extended_scope:
- path: crates/acdd-index/src/db/lint/docs_lint.rs
  reason: 'Fix docs.broken_symbol_claim false positives on plain Markdown file links, unindexed existing files (tests/, skills/, yaml), and doc_type=contract milestone specs per #141/#147'
- path: tests/core/arch_lint.rs
  reason: 'Fix docs.broken_symbol_claim false positives on plain Markdown file links, unindexed existing files (tests/, skills/, yaml), and doc_type=contract milestone specs per #141/#147'
depends_on:
- host-cli-mcp-lint-surface-and-domain-provider
tags:
- default
- docs
invariants:
- 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant across contextunity-acdd has one canonical INV-* declaration in docs/architecture/*.md or docs/adr/*.md with verified symbol claims (and acdd-rule blocks for structural boundaries), while **/AGENTS.md files remain concise operational routers.'
acceptance_criteria:
- 'AC-1: Architectural invariants scattered across AGENTS.md, src/AGENTS.md, crates/acdd-{core,index,state}/AGENTS.md, and tests/AGENTS.md are consolidated into canonical docs/architecture/*.md and docs/adr/*.md with unique INV-* IDs and verified symbol links, and **/AGENTS.md files retain only concise routing, commands, and local operational guards.'
- 'AC-2: Live acdd-rule blocks in docs/adr/ and docs/architecture/ machine-enforce ACDD own architecture (INV-CORE-LEAF, ADR-0001 3-crate DAG, acdd-state machine/ pure-core isolation, store/sqlite/ rusqlite confinement, acdd_core::git single-owner subprocess rule, and src/state_bridge import boundary), verified by acdd lint returning 0 errors across the live repository.'
- 'AC-3: docs/reference/{cli,mcp-tools,profiles,acdd,configuration}.md + README.md + INSTALL.md + skills/acdd/{SKILL.md,references/*} document acdd lint, code_map_lint, acdd-rule, and the built-in --profile documentation profile, and acdd docs check + acdd validate pass cleanly.'
contract_revision: 1
status: completed
subtasks:
- subtask_ref: consolidate-scattered-agents-invariants-and-author-acdd-rules
  title: Migrate invariants from **/AGENTS.md into docs/architecture/ and docs/adr/ with INV-* IDs, verified symbol links, and live self-hosting acdd-rule blocks
  status: completed
  evidence: Consolidated scattered invariants from AGENTS.md, src/AGENTS.md, crates/acdd-{core,index,state}/AGENTS.md, and tests/AGENTS.md into canonical docs/adr/{0001,0003}.md and docs/architecture/{modularity,test-harness}.md with verified symbol links, resolved duplicate INV-DOC-FIRST-MATCH-TIER between docs/AGENTS.md and docs/reference/mcp-markdown-contracts.md, authored 6 live acdd-rule blocks (INV-CORE-LEAF, INV-STATE-NO-INDEX, INV-STATE-BRIDGE-BOUNDARY, INV-STATE-PURE-MACHINE, INV-STATE-SQLITE-CONFINED, INV-GIT-SINGLE-OWNER), and trimmed **/AGENTS.md to concise routers.
- subtask_ref: update-references-and-acdd-skill-for-lint-and-docs-profile
  title: Update README.md, INSTALL.md, docs/reference/*, and skills/acdd/* to document acdd lint, code_map_lint, acdd-rule, and --profile documentation
  status: completed
  evidence: Updated README.md, INSTALL.md, docs/AGENTS.md, docs/reference/{README,acdd,cli,configuration,mcp-markdown-contracts,mcp-setup,mcp-tools,profiles,tasks}.md, docs/adr/0004, docs/architecture/{concurrency,domain-extensions,indexing,specification-and-design-lifecycle}.md, and skills/acdd/{SKILL.md,references/{onboarding,profile-reference,tool-catalog}.md} for 20 static MCP tools (code_map_lint), acdd lint, 4 embedded profiles (documentation), acdd-rule, architecture.{zones,rules}, and [ENFORCED] invariants; fixed all stale symbol links in docs/adr/0008 and docs/architecture/{cross-platform,domain-extensions,indexing,performance}.md.
- subtask_ref: verify-self-hosting-lint-docs-check-and-validate
  title: Rebuild workspace index and verify acdd lint, acdd docs check, and acdd validate pass with zero violations
  status: completed
  evidence: 'Extended scope (#301) to crates/acdd-index/src/db/lint/docs_lint.rs and tests/core/arch_lint.rs per #141/#147 to eliminate false-positive docs.broken_symbol_claim on file-name links, doc links, unindexed existing files, and doc_type=contract specs. Rebuilt index (output_root eaa5d6e2b1162001e39777f86f350ba3c02f0a9975c2caa12c8e092a4dfb866a), verified acdd lint (0 errors, 6/6 rules evaluated), acdd validate (10/10 passed), arch_lint:: (3/3), commitment_integrity (18/18), Clippy (-D warnings), acdd verify (exit_code=0), TMP 5462==5462. Note: acdd docs check passes every file except active engine-managed spec docs/milestones/AR-003-unified-lint-engine-and-documentation-profile.md:82 (stale link ../../crates/acdd-index/src/db/lint.rs outside Task 5 scope).'
receipt:
  run_id: AR-003-unified-lint--20261122000752-1979185-1
  node_cursor: root:agents-md-invariant-consolidation-and-docs-alignment.deliver
  gate_id: deliver
  worker_id: ar003-r5
  claim_revision: 1
  contract_revision: 2
  candidate_tree: d18af6a45d6b223b7cf38d674d58d68f232342ca
  committed_oid: b2fc19cb23591e62003faf0e55788c741764d7d6
  accepted_at: 2026-11-22T03:03:43.731Z
  rollup:
    verified_invariants:
      applicable:
      - 'INV-AR003-CANONICAL-INVARIANT-AUTHORITY: Every architectural invariant across contextunity-acdd has one canonical INV-* declaration in docs/architecture/*.md or docs/adr/*.md with verified symbol claims (and acdd-rule blocks for structural boundaries), while **/AGENTS.md files remain concise operational routers.'
      verified: []
```
