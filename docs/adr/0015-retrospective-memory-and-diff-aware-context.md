---
title: "ADR 0015: Retrospective Memory Pipeline and Diff-Aware Gate Context"
doc_type: adr
status: accepted
date: 2026-11-28
---

# ADR 0015: Retrospective Memory Pipeline and Diff-Aware Gate Context

## Status

Accepted.

## Context

[ADR 0014](0014-declarative-spec-schemas-and-items.md) establishes universal `specs:` collections (`memory` base), `gate_templates.memory_synthesis`, and 4-tier context ranking for `specs.<collection>` lenses. Repository learning requires a clean, low-ceremony pipeline separating:

1. **Universal run retrospectives** — structured observations captured per run (gate cycles, problem domains, code/blueprint quality, and agent-observed tool usage friction) without write-path overhead on read-only tool dispatch.
2. **Repository-specific consolidated memory** — domain-grouped rules (`docs/memories/<domain>.md`) used to evolve `AGENTS.md`, skills, and workflows without regressing prior fixes.
3. **Diff-aware gate context injection** — universal `specs.memories` ranking across any workflow (`code`, `documentation`, `planning`, `hotfix`) that uses changed files (and optional code symbols when `modules.code: true`) both on `claim` and via pre-submit self-checks.

## Decision

### 1. End-to-End Pipeline

```text
[1] Any workflow run
      │  Pre-handoff gate: 1 agent fills strict declarative `retrospectives` Spec
      ▼
[2] docs/memories/.retro/<workflow>/<spec_or_run>.md  (gitignored, validated via .acdd/specs/retrospective.yaml)
      │  Manual `memory_optimize` workflow (context: [specs.retrospectives, specs.memories])
      │  Updates docs/memories/<domain>.md (+ optional AGENTS.md / skills edits)
      │  Moves processed retrospectives -> .retro/<workflow>/.processed/
      ▼
[3] docs/memories/<domain>.md  (Git-tracked, indexed in `docs:` & validated by `specs.memories`)
      │  Universal `specs.memories` lens (citation -> hunk diff files/symbols -> scope -> graph -> global)
      ▼
[4] Gate context on `claim` + builder pre-submit `acdd context` / diff check
```

### 2. Declarative Spec Collection Filtering (`SpecConfig.ignore`) & Disjoint Subtrees

- `SpecConfig` exposes an optional `ignore: Option<Vec<String>>` field defaulting to `["archive", ".*", "README.md", "AGENTS.md", "CLAUDE.md"]`.
- `acdd-spec` workspace discovery (`collect_workspace_specs`) and `docs_domain` (`specs.*` lens, `docs.specs`) honor `SpecConfig.ignore` uniformly instead of hardcoding `"archive"`.
- `SpecConfig::ignores_subpattern` permits a child collection (`specs.retrospectives` at `docs/memories/.retro`) inside an ignored subdirectory of a parent collection (`specs.memories` at `docs/memories`) because their active file sets are provably disjoint, while still rejecting unignored overlapping patterns.
- Consequence: `.retro/` is automatically excluded from `specs.memories`, and `.retro/<workflow>/.processed/` is automatically excluded from `specs.retrospectives`, with zero engine hardcoding.

### 3. Agnostic Declarative `retrospective` Spec & Lifecycle (Zero Crate Hardcoding)

- Only `contract` and `memory` remain embedded engine bases (`crates/acdd-spec/src/embedded/`); `retrospective` is authored purely declaratively in `.acdd/specs/retrospective.yaml` (shipped alongside reusable examples in `skills/acdd/references/acdd-spec-examples/retrospective.yaml`) and bound via `acdd.yaml` `specs.retrospectives` (`ref: retrospective`).
- A pre-`handoff` gate (while run state is live in SQLite) invokes one agent to write `docs/memories/.retro/<workflow>/<spec_or_run>.md` (works identically for spec-backed runs and ephemeral `--goal` runs; gitignored so `claim_tree` and `save.immutable: true` remain intact).
- `run_tombstones` stays unchanged (`rejections_total` / `claims_by_outcome` remain aggregate-only; zero schema bloat).
- `.acdd/specs/retrospective.yaml` enforces closed minimal fields (`additionalProperties: false`):
  - Core frontmatter: `id`, `title`, `spec: retrospectives`, `status`, `workflow`, plus required extra fields `run_id`, `outcome` (`completed | cleared`), and optional `blueprint_quality` and `code_quality`.
  - Structured items:
    - `gate_stats`: per-gate `{ gate, attempts, rejections, cycles, top_reasons[] }`
    - `problems`: `{ domain, stage, count, summary, root_cause }`
    - `tool_usage`: `{ tool, pattern, issue, instruction_gap }` (agent-observed tool misuse, redundant calls, ignored rules — avoiding runtime telemetry overhead on read-only MCP/CLI dispatch).

### 4. Flat Repository Memory Layout (`docs/memories/`)

- Active consolidated memories live directly in `docs/memories/<domain>.md` (Git-tracked, validated by `specs.memories`), grouped by problem domain (typically 5–8 files, including `instructions-and-skills.md` with `scope: ["AGENTS.md", "**/AGENTS.md", "skills/**"]` so edits to instructions automatically receive prior lessons through the same lens).
- Retired/superseded memories move to `docs/memories/archive/` (Git-tracked, consistent with `docs/milestones/archive/`).
- Raw retrospectives live in `docs/memories/.retro/<workflow>/` and move to `docs/memories/.retro/<workflow>/.processed/` after consolidation (both gitignored).

### 5. Unified `ContextRef` + `ContextCapability` SPI & Diff-Aware Ranking

- **Single `ContextRef` & `ContextCapability` SPI (`crates/acdd-core/src/ports.rs`)**:
  - Every `context:` entry (string shorthand or `{ context, with }` object symmetric with `checks:`) parses into `ContextRef::Paths(name) | ContextRef::Specs(name) | ContextRef::Domain { domain, name }`.
  - `DomainCapabilities` declares typed `ContextCapability { name, scope_mode: Admitted | NodeOnly, default_paths_key, input_schema }` plus `supports_collection_context: bool`, eliminating hardcoded `"code.symbols"`, `"code.tests"`, `"docs.invariants"`, and `paths`/`specs` domain-redirect hacks from `ports.rs` and `acdd-state/src/context/tiers.rs`.
  - `specs.<selector>` in `scope:` and `context:` resolves both literal collection keys (`specs.memories`) and built-in base selectors (`specs.memory`), keeping embedded `gate_templates.memory_synthesis` collection-name agnostic.
- **Universal diff-aware ranking (`DocsDomainProvider`)**:
  - Works with or without `modules.code` across any workflow (`code`, `documentation`, `planning`, `hotfix`).
- Ranking order (highest to lowest priority):
  1. **Direct citation:** rule `id`, `ref`, `title`, or path cited in `target` / `citation_text`.
  2. **Active diff match (when `git diff` is non-empty):**
     - *Universal (all file types):* changed file paths overlap rule `scope` / `owners`.
     - *Optional hunk-exact code enrichment (only when `modules.code: true` and `code.sqlite` exists):* hunk-overlapping `changed_symbols` (`collect_changed_symbol_seeds`) boost rules declaring matching optional `symbols:` or `doc_section` code-map edges (`documents`, `references_doc`, `cites_doc`, `governed_by`, `governs`) to the exact modified declarations, without boosting unrelated sibling symbols in the same file.
  3. **`admitted_scope` overlap:** module/directory match (primary signal when diff is empty, e.g. at `tasks.build` claim or `blueprint_synthesis`).
  4. **`code_map` doc-edges:** indexed edges connecting the memory document to files in `admitted_scope`.
  5. **Global fallback:** rules scoped to `**`.
- **Gate & command binding:**
  - Attach `specs.memories` to `context:` on `blueprint_synthesis.synthesize`, `tasks.build`, `tasks.review`, `polish`, and `integration_review`.
  - Before submitting `tasks.build` (alongside `duplicates` / `dead_tails` hygiene), builders run `acdd context <node_cursor>` (and/or `code_map_analyze(mode="diff")` for code runs) to evaluate `specs.memories` against their live diff.

### 6. Manual `memory_optimize` Workflow & Backfill

- A standalone `memory_optimize` workflow (shipped as a workflow template/profile, invoked manually via `acdd start --goal ... --workflow memory_optimize`) reads unprocessed `docs/memories/.retro/*/` files, consolidates domain rules into `docs/memories/<domain>.md`, proposes targeted `AGENTS.md`/skill refinements under independent review, and moves processed retrospectives into `.retro/<workflow>/.processed/`.
- Historical backfill runs as a separate step after engine implementation: legacy `docs/memories/*.md` notes are moved into `docs/memories/.retro/` first, then consolidated via `memory_optimize` alongside historical `run_tombstones` summaries.

## Consequences

- **Minimal engine footprint:** Reuses `acdd-spec`, `SpecConfig`, `docs_domain` ranking, `acdd-index` `changed_symbols`, and `acdd context` (~150–250 net production LOC total across `SpecConfig.ignore`, declarative `retrospective` spec, and diff-aware ranking).
- **Workflow-agnostic:** Non-code workflows (`documentation`, `planning`) get identical retrospective capture, file-diff memory matching, and instruction preservation without enabling `modules.code`.
- **Zero read-path telemetry overhead:** Tool-usage quality is synthesized by the retrospective agent rather than writing per-call logs on every read-only query.

## Non-Goals

- Automatic background triggers after every N runs (deferred to reactive trigger milestones such as `AC-008`).
- Runtime per-call MCP/CLI telemetry tables in `.acdd/state.sqlite`.
- Mandatory `symbols:` annotations on every memory rule (`symbols:` stays optional).
