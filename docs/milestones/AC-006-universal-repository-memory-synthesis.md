---
id: AC-006-universal-repository-memory-synthesis
title: Universal repository memory synthesis and deterministic gate consumption
doc_type: contract
status: planned
depends_on:
- AC-005-unified-swarm-gates-and-peer-board-coordination
guidance:
  hitl: pro
owners:
- crates/acdd-core/src/models.rs
- crates/acdd-core/src/ports.rs
- crates/acdd-index/src/docs.rs
- crates/acdd-index/src/db/reader.rs
- crates/acdd-state/src/
- src/state_bridge/
- src/engine/pipeline.rs
- src/cli/
- src/mcp/
- docs/adr/
- docs/architecture/
- docs/reference/
- docs/runbooks/
- docs/workflows/
- skills/acdd/
- ROADMAP.md
- CHANGELOG.md
invariants:
- 'INV-MEMORY-SOURCE-AUTHORITY: Confined Git-trackable docs/memories/*.md files are the sole memory authority; SQLite contains only their rebuildable docs-index projection, never a second writable registry.'
- 'INV-CORE-LEAF: Memory DTOs and pure validation in acdd-core import no acdd-index, acdd-state, application bridge, CLI, or MCP code.'
- 'INV-MEMORY-EVIDENCE: Every admitted rule cites observed execution evidence and a source subject; empty evidence and ordinary diff summaries produce no memory.'
- 'INV-MEMORY-DETERMINISTIC-CONSUMPTION: Ephemeral, isolated, and in-place Run gate contexts use one explicit path/symbol/gate resolver (wired via DomainProvider context lenses), with stable ranking and identical bounded projections for identical inputs.'
- 'INV-MEMORY-BOUNDS: At most five compact memories and 4096 serialized UTF-8 bytes of memory context reach a gate; full documents are pulled through acdd_read.'
- 'INV-MEMORY-ANCHORS: Stale, superseded, malformed, ambiguous, unavailable, or digest-mismatched memories never supply active gate advice.'
- 'INV-MEMORY-LIFECYCLE: docs_sync retains live contract documentation; memory_synthesis supports both pre-binding per-run gates (alongside or after docs_sync) and off-hot-path batch "dreaming" passes (acdd memory synthesize --dream) over accumulated run tombstones and archived specs, without making synchronous synthesis mandatory on every small run or changing subject persistence or proof authority.'
- 'INV-MINIMAL-INTERVENTION: Reuse docs extraction, SQLite document nodes (doc_search FTS5/BM25), existing gate traces/blackboard evidence, gate graphs, confinement, and response budgets; introduce no dependency, embedded memory corpus, model client, vector DB, second BM25 engine, or parallel run registry.'
- 'INV-SCOPE-INTEGRITY: Cargo.toml, all member Cargo.toml files, build.rs, and .acdd/** are excluded from every task write scope.'
- 'INV-MEMORY-SEAM-HYGIENE: Register every new seam under tests/acdd/ in Task 0; use table-driven production seams, zero new root test binaries, and zero leaked temporary workspaces.'
- 'INV-DOCS-AND-EXAMPLES-COEVOLUTION: AC-006 ships in the same milestone with updated ADR-0004 and ADR-0006 (memory context lens, docs/memories/*.md authority, acdd_memory BM25 reuse), architecture/reference/runbook docs, executable docs/workflows/*.yaml and skills/acdd/references/acdd-profile-examples/*.yaml profiles, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md passing acdd docs check and acdd validate.'
acceptance_criteria:
- 'AC-01: Strict, bounded Markdown memory documents (docs/memories/*.md) parse cleanly while ordinary docs retain their existing contract.'
- 'AC-02: Cold, cache, and delta builds yield identical canonical memory projections and doc_search FTS5 entries; deletion removes obsolete advice.'
- 'AC-03: Rejections, decisions, and failures bind candidates; noisy or foreign evidence is rejected; empty synthesis is a clean no-op.'
- 'AC-04: Identical scope/symbol/gate inputs deliver stable K=5, <=4096-byte memory context via DomainProvider across linear, parallel, and swarm gates.'
- 'AC-05: Moved, deleted, or ambiguous anchors suppress advice cleanly without mutating tracked Markdown on read.'
- 'AC-06: acdd memory CLI and acdd_memory MCP share backend semantics, with query implemented as a thin façade over doc_search BM25 filtering stale/superseded by default.'
- 'AC-07: ADR-0004, ADR-0006, docs/architecture/, docs/reference/{profiles,tasks,cli,mcp-tools,configuration}.md, docs/runbooks/, docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md are updated and pass acdd docs check and acdd validate.'
required_skills:
- acdd
- contract-planning
- agent-instructions
---

# AC-006: Universal repository memory synthesis

## Goal and admission boundary

Curate reusable operational rules from execution evidence, keep them as small
repository documents, and deterministically deliver only applicable rules to
all unified `Run` / `GateProfile` execution modes (`ephemeral`/`save: none`,
`in_place`, and `worktree` milestone runs) under
[ADR 0003](../adr/0003-unified-profile-graph-and-state-machine.md) and
[ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md).
This contract authors the implementation queue; it does not seed fabricated
memories or implement the capability during milestone authoring.

`AC` is the lifecycle domain. The primary checkout contains archived AC-001 through
AC-004; [AC-005](AC-005-unified-swarm-gates-and-peer-board-coordination.md) is the
direct upstream prerequisite (`mode: linear | parallel | swarm` execution and peer
blackboard coordination), while
[AC-007](AC-007-unified-observability-board-otlp-and-metrics.md),
[AC-008](AC-008-always-on-reactive-triggers-and-deferred-approvals.md), and
[AC-009](AC-009-authenticated-ingress-interactive-webui-and-http-mcp.md) are
downstream planned capabilities in `docs/milestones/`. Execution targets the
unified `acdd-state` `GateProfile` engine (`crates/acdd-core`, `crates/acdd-state`,
`src/state_bridge/`, and `DomainProvider`), ensuring `mode: linear`, `mode: parallel`,
and `mode: swarm` nodes receive identical deterministic memory injection and
preserving `Run` persistence semantics. The AC-007 Web UI (`acdd serve --webui`) is outside this
milestone.

## Architectural reconciliation and verified owners

[ADR 0001](../adr/0001-crate-dag-and-layer-boundaries.md),
[ADR 0004](../adr/0004-domain-provider-spi-and-plugin-tools.md),
[performance invariants](../architecture/performance.md), and the actual crate
layout require a leaf `acdd-core` (hosting pure memory DTOs, validation, and
`DomainProvider` ports) and a domain-agnostic `acdd-state` connected through
`DomainProvider` in `src/state_bridge/`. Keep pure memory DTOs/validation and
SPI port/tool descriptors in `acdd-core`, document extraction and `doc_search`
BM25 querying in `acdd-index`,
lifecycle/gate evidence selection in `acdd-state`, and domain resolution, source
maintenance, and the `acdd_memory` plugin tool wiring in `src/state_bridge/`,
`src/cli/`, and `src/mcp/`. No new crate dependencies or infrastructure exceptions
are needed.

[ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md) governs
compact context.
[ADR 0002](../adr/0002-workspace-confinement-and-path-policy.md),
[ADR 0012](../adr/0012-fail-closed-selectors-and-readonly-sql-sandbox.md), and
[ADR 0007](../adr/0007-sqlite-code-map-concurrency-and-staged-publication.md) govern confinement,
unique anchor resolution, and index publication. Memory does not override
contracts, invariants, ADRs, rejection findings, independent review, or proof.

These declarations identify the `0.6.0` verified containers and should be
line-refreshed at Task 0:

| Existing declaration | Verified declaring container | Role in the design |
| --- | --- | --- |
| `BUILTIN_MCP_TOOLS`, `DomainProvider`, `DomainToolDef` | `crates/acdd-core/src/ports.rs` | `DomainProvider` plugin SPI exposing `acdd_memory` (`<domain>_<name>` or `memory` module tool), `context` lenses, and `checks` without coupling `acdd-state` to `acdd-index` |
| `DocSection`, `NavigationDetails` | `crates/acdd-index/src/models.rs` | Existing docs DTO and metadata serialization boundary; adjacent pure memory DTO in `acdd-core`, no registry |
| `ModulesConfig` | `crates/acdd-core/src/subsystems.rs` | `modules.memory` runtime toggle and module availability policy |
| `extract_with_details` | `crates/acdd-index/src/docs.rs` | Returns document sections plus structured source metadata |
| `search_docs_with_options`, `DocSearchOptions` | `crates/acdd-index/src/db/reader.rs` | Existing FTS5 `doc_search` `bm25(doc_search,3.0,1.0,1.0)` query path reused by `acdd_memory(action: "query")` |
| `extract_typed_source_with_digest` | `src/engine/pipeline.rs` | Copies selected document metadata into document-node details |
| `StateBackend`, `StateReader`, `StateTx` | `crates/acdd-state/src/store/traits.rs` | Owns `gate_traces`, `blackboard`, `gate_artifacts`, and `run_tombstones` evidence queries |
| `SqliteStateBackend`, `StateBackend` | `crates/acdd-state/src/store/sqlite/mod.rs` | Durable SQLite storage backend for persistent runs and run tombstones |
| `build_claim_context` | `crates/acdd-state/src/context.rs` | Bounded 6-tier `ClaimResp` assembly calling `DomainProvider::context_lens` for memory context |
| `DomainProviderBridge`, `DocsDomainProvider` | `src/state_bridge/mod.rs`, `src/state_bridge/docs_domain.rs` | Hosts domain context lenses, checks, and plugin tool dispatch (`acdd_memory`) |
| `Command` | `src/cli/mod.rs` | Flat `0.6.0` CLI dispatch (`acdd memory ...` and `acdd tool ...`); MCP shares the same backend |
| `BUILTIN_MCP_TOOLS` dispatch & handlers | `src/mcp/tools.rs`, `src/mcp/tools/inputs.rs` | 19 built-in static tools + dynamic `<domain>_<name>` plugin dispatch (hosts `acdd_memory`) |

`search_docs` grounding covers core leaf isolation, minimal intervention and scope
integrity in the contract-planning add-on, stage-tailored context in ADR 0006,
workspace boundaries in performance architecture, and the Task 0/scope-closure
rules in the milestone instructions. Source reads confirm that generic docs
frontmatter currently tolerates malformed/unknown metadata. The new **memory**
parser must reject it without making all existing documentation strict.

## Frozen architecture and producer-consumer contracts

### Source document and projection

One memory lives in `docs/memories/<slug>.md`. Its required frontmatter keys are
exactly `id`, `title`, `doc_type`, `kind`, `gates`, `scope_globs`, `symbols`,
`status`, `source_subject`, and `updated_at`. Use `doc_type: memory`,
`kind: rule | signal`, `status: active | stale | superseded`, a stable lowercase
slug `id` equal to the filename stem, and an RFC 3339 UTC timestamp. Unknown or
duplicate fields, invalid enums/timestamps, duplicate IDs, and missing required
values fail validation. Lists may be empty individually; at least one routing
anchor across paths, symbols, and gates is mandatory. Use strict serde DTOs with
`deny_unknown_fields` at structured input boundaries.

The canonical body has `## Rule`, `## Signal`, `## Evidence`, optional
`## Review checks`, and optional `## Supersedes`. Rule states the required action;
Signal states when it is useful. Evidence contains typed source references,
observed failure/decision, and the corrective action, not copied execution logs.
Supersedes contains existing memory IDs and supplies the relationship without an
extra frontmatter key or mutable relationship table. Both kinds retain an
actionable rule and signal. Unknown/duplicate body sections fail validation.

Bounds: 8192 bytes per complete file; 512 bytes each for rule and signal;
three review checks of at most 256 bytes each; eight evidence references;
eight supersedes references; sixteen entries per routing list; 256 bytes per
anchor, ID/source-subject value, or title. Reject oversize input before parsing
or allocating its full body. Limits apply to UTF-8 bytes; truncation, when used
for context display, preserves character boundaries. Do not truncate stored rules.

The existing docs extractor emits validated memory metadata on one canonical
document node per memory, together with ordinary searchable sections. Reuse
`nodes.details` and `doc_sections`, existing cold/delta publication, local facts,
and document source digests. Preserve metadata through navigation serialization
and extraction-cache replay. No `memories` table, `acdd-state` SQLite mirror,
SQLite-only status authority, or sidecar registry is admitted. A missing directory
means an empty corpus; malformed memory files are diagnostics and never active
advice.

### Evidence-grounded synthesis and source maintenance

The synthesis operation first returns a bounded evidence packet for an explicit
subject (`--subject <run_id|node_cursor>`). Under the unified `0.6.0` state
model, run evidence comes from `gate_traces` (pass/reject transitions and
summaries), `blackboard` (`findings`, `decision`, `note` messages),
`gate_artifacts`, and observed command failures. Use all admitted run nodes and
attempts, not only the most recent successful claim. Ephemeral (`save: none`)
and in-place runs use the same `StateBackend` / `StateReader` evidence queries;
ephemeral runs operate over their `MemoryStateBackend` within the live run before
teardown without creating persistent task rows in `state.sqlite`. Never fabricate
task rows for root-only profiles.

Capture rejection attempts and command/runner failures before retry routing or
ephemeral teardown, including terminal failed runs. Preserve the failure class,
subject, gate, revision/attempt or run identity, evidence digest, and observed
corrective decision. Keep at most 32 evidence items and 16384 serialized bytes,
ordered deterministically by subject, canonical gate, attempt and evidence ID;
report omitted counts and pull references. Missing historical evidence is
explicitly unavailable, never reconstructed from a diff or a success summary.

**Batch "Dreaming" Synthesis (`--dream`)**: Rather than forcing synchronous
memory synthesis at the end of every small plan or run,
`acdd memory synthesize --dream [--since <rfc3339|run_id>]` (paired with an
example `memory-dream` profile) executes an off-hot-path batch consolidation
pass. It aggregates bounded provenance across multiple completed or failed runs
from durable tombstone summaries (`run_tombstones.summary_json`, which retains
concise rejection findings, architectural decisions, deferred items, and gate
attempt counts across `prune: true`) and archived specs (`archive/*.md`),
clusters recurring review rejections and operational pitfalls, deduplicates
against existing `docs/memories/*.md`, and proposes new active rules or explicit
`superseded` transitions within the same bounded packet and review gates.

Existing role runners perform semantic synthesis; Rust selects evidence and
validates the result. No model client, embedding model, heuristic lesson generator,
or mandatory background process is added. A candidate qualifies only if its
evidence establishes a reusable failure prevention rule or non-obvious decision
beyond its original change. Zero candidates is a successful, observable no-op.
Diff paraphrases, routine passing tests, SHAs alone, generic maxims, and unsupported
claims fail admission. A reviewer evaluates semantic quality; engine validation
does not pretend to prove that prose is insightful.

Candidate evidence references must resolve to the selected packet and its digest.
For ephemeral workflow evidence, persist the bounded provenance excerpt/digest in
the accepted Markdown before the run ends; it remains verifiable as recorded
provenance without claiming a durable workflow log. Synthesis after a `save: none`
process exits cannot recover discarded failures and reports that limitation.

Deduplicate by canonical rule/signal/kind/routing fingerprint. Exact repeats are
no-ops preserving IDs and `updated_at`. The synthesizing role compares against
existing applicable rules; independent review rejects semantic duplicates that
normalization cannot decide. Replacements explicitly name superseded IDs;
validate existence, acyclicity and candidate/source digests before writing.
Write the replacement first, then mark predecessors `superseded`; exclude any
predecessor referenced by a valid replacement even if an interruption leaves its
source status active. Retry completes that deterministic operation. A conflicting
user edit fails with no overwrite. Stage writes with existing confined atomic
helpers; do not claim atomicity across Git files and SQLite. Rebuild the projection
after source writes; until refreshed, digest disagreement suppresses advice.

### One deterministic resolver for every gate

Normalize the owning workspace, subject scope paths, uniquely resolved code-map
symbol identities, and current canonical gate ID plus its explicit leaf ID.
The matching predicate is the union of three **same-domain** intersections:
path matches scope glob **OR** exact symbol identity matches **OR** gate ID matches.
Never compare a symbol string to a path or use semantic/fuzzy/recency similarity.
Empty routing lists are not wildcards. A gate-only memory is allowed only by its
explicit gate anchor. Do not invent a repository-wide scope for an unknown subject.

Use the existing normalized glob implementation and confinement helpers. Resolve
subject scope globs against the admitted index inventory once; literal future
file paths remain matchable as literals. If expansion or symbol resolution is
unavailable, report that dimension unavailable and use only independently proven
dimensions. Do not guess glob overlap from string prefixes. Cap subject expansion
at 4096 paths/symbols and memory candidates at 256 in stable ID order; a computation
overflow returns an explicit unavailable projection rather than a silently partial
ranking. Gate matching uses exact canonical or exact leaf IDs, without suffix or
substring guesses beyond that declared leaf normalization.

First exclude non-active source status, invalid anchors, source digest drift,
archive paths and superseded predecessors. Rank eligible results by the descending
tuple `(symbol_match, path_match, gate_match)`, then ascending `(id, path)`.
Return at most K=5 in that order. The whole serialized memory field, including
IDs, matched-reason flags, omission counts, diagnostics, pull references and JSON
escaping, fits 4096 UTF-8 bytes and the caller's remaining response/prompt budget.
Each entry carries only ID, concise rule/signal, and `acdd_read` path; include
review checks only for review gates when they fit. Add whole entries until the
next cannot fit, then stop. A byte cap is the enforceable token-independent
budget; do not advertise an inaccurate tokenizer count. No raw body, evidence
history, or duplicated root-level memory field enters a prompt.

Gate context assembly calls this resolver via a `DomainProvider` context lens
(e.g., `docs.memory` registered in `src/state_bridge/docs_domain.rs`) wired into
`build_claim_context` (`crates/acdd-state/src/context.rs`) and spawner prompt
construction (`crates/acdd-state/src/runner/spawner.rs`). All unified `Run` and
`GateMode` shapes — `worktree`, `in_place`, and `ephemeral` (`save: none`) runs
across `mode: linear`, `mode: parallel`, and `mode: swarm` shard cursors
(`poll[telegram]`, `implement[core-cleanup].build`) — invoke the exact same
`build_claim_context` / `DomainProvider::context_lens` contract using the node's
explicit `admitted_scope`, resolved targets, and canonical `gate_id` (plus leaf ID). Both configured and default prompt templates
receive the compact field once inside the `ClaimResp` 6-tier budget. Memory-free
repositories preserve the existing context shape. Missing/unadmitted index or
disabled `docs`/`memory` module yields no injected advice plus a bounded
availability diagnostic, not a new hard tool requirement or a failed claim.
Memory is never a proof or authorization source.

### Anchor checks, stale status and closure placement

Every declared scope glob must match at least one currently admitted file; every
symbol must resolve uniquely in its owning workspace. A moved/deleted/ambiguous
anchor makes the **whole** memory effectively `stale`, even when a gate matches.
Gate-only memories need no filesystem anchor. Recheck anchors against the current
index at scan/check and before consumption; a scope-only code deletion invalidates
advice even when the Markdown did not change. Do not cache validity solely by the
memory file digest. Source status and effective status are reported separately.

`scan` reports effective stale status and leaves tracked Markdown unchanged.
`acdd memory check --apply` persists `status: stale` for confirmed broken anchors
with an optimistic source-digest check. Unavailable/incomplete index coverage does
not prove deletion and cannot persist staleness. Repairing anchors does not silently
reactivate a source marked stale; an explicit checked maintenance update is needed.
`acdd memory prune` defaults to preview and only removes explicitly selected
stale/superseded files with `--apply`; reject active targets and dangling
supersedes references. Neither `acdd clear` nor terminal archive
pruning deletes curated memory documents.

`memory_synthesis` is an ordinary configured gate in `GateProfile` v2 (via
`call:` / `checks:` or role execution alongside/after `docs_sync`). There is no
new gate DSL, mandatory synthesis on every task, or replacement of live
documentation. Milestone/worktree runs synthesize before terminal verification /
`acdd clear` (and before final documentation verification); changes
afterward require renewed verification and review. In-place runs use an admitted
final gate/task without adding synthetic milestone rows. Ephemeral (`save: none`)
runs pass explicit `--scope docs/memories/` to `acdd start <spec|goal> --profile ... --scope docs/memories/`
with `git.isolation: none` / `save: none`, creating no SQLite task rows,
snapshots, or commits while capturing in-memory gate failures across retry edges
within that run so an in-run repair or closure gate can consume them. Explicit
`--scope` replaces inferred scope; it never widens from goal prose, a memory
match, or an artifact name.

### CLI and MCP contract

Provide:
- **CLI**: `acdd memory list`, `acdd memory show <id>`,
  `acdd memory query "<text>" [--include-stale]`,
  `acdd memory check [--apply]`,
  `acdd memory synthesize (--subject <subject> | --dream [--since <cursor>]) [--candidates <path>] [--apply]`,
  and `acdd memory prune <id>... [--apply]`.
- **MCP**: **`acdd_memory`** with action interface
  `list | show | query | check | synthesize | prune` (registered alongside the 19
  built-in tools or as a `DomainProvider` tool gated by `modules.memory`, keeping
  the exact same typed request/backend, workspace/worktree routing, pagination,
  diagnostics, and apply semantics).

Do not overload `acdd_state` or create fake tasks. List/show/query/check expose
source path/digest, declared/effective status, anchors, diagnostics, and
`acdd_read` pull references. Synthesize without candidates returns the bounded
evidence packet; candidates without `--apply` return the validated proposed
changes.

**Thin `query` Façade over `doc_search` BM25 (`include_stale: false` default)**:
`acdd_memory(action: "query", query: "<text>", include_stale?: bool)` and CLI
`acdd memory query "<text>" [--include-stale]` MUST be implemented as a **thin typed
alias/façade** over existing
`reader::search_docs_with_options(..., DocSearchOptions { doc_type: Some("memory"), ... })`
in `crates/acdd-index/src/db/reader.rs` (`doc_search` FTS5
`bm25(doc_search, 3.0, 1.0, 1.0)`). By default (`include_stale: false`), the façade
filters out records whose declared status or live anchor validation resolves to
`stale` or `superseded` (and surfaces explicit stale/superseded status markers when
`include_stale: true`), without building a second BM25 engine, custom FTS5 table,
or vector DB.

Unknown IDs, ambiguous IDs, path escape/symlinks, bad requests, invalid candidates,
and write conflicts fail closed. List/show/query/check without `--apply` are
read-only. No operation commits, pushes, force-deletes active memories, or edits
configuration.

Freeze diagnostic classes in Task 0: `MEMORY_SCHEMA_INVALID`,
`MEMORY_LIMIT_EXCEEDED`, `MEMORY_ANCHOR_STALE`, `MEMORY_INDEX_UNAVAILABLE`,
`MEMORY_EVIDENCE_UNAVAILABLE`, `MEMORY_EVIDENCE_INVALID`,
`MEMORY_SUPERSESSION_INVALID`, `MEMORY_SOURCE_CONFLICT`, and
`MEMORY_NOT_FOUND`. Reuse existing path/feature-disabled diagnostics where applicable.

## Tasks in this milestone

### Task 0 and wave ownership

Task 0 owns structural registrations and all shared test-module stubs. It freezes
the typed interfaces below and the per-task blueprint slices before downstream
claims. New symbols are explicit **proposals**, not claims that baseline code
already contains them: exact code-map searches returned no `RepositoryMemory`,
`MemoryRequest`, state/bridge `memory` backend, or `acdd_memory` tool. Their
declaring files are explicitly scoped. Existing container evidence is the
inspected core `models.rs`, `crates/acdd-core/src/ports.rs`,
`crates/acdd-index/src/db/reader.rs`, `crates/acdd-state/src/{context,store/traits}.rs`,
`src/state_bridge/docs_domain.rs`, and CLI/MCP input/handler modules.
Task 0 creates and indexes the shared new declarations before downstream scope
closure is checked. Do not declare a new symbol against an unrelated existing container.

Public additions admitted here are `RepositoryMemory` and its pure `parse`
constructor in `acdd-core`; `MemoryRequest` and the memory maintenance/synthesis
backend in `src/state_bridge/` (and read-only `StateReader` evidence queries in
`crates/acdd-state`); the `docs.memory` `DomainProvider` context lens; and the
`acdd memory` CLI subcommand + `acdd_memory` MCP route (`list | show | query | check | synthesize | prune`).
The shared resolver/evidence helpers remain crate-internal. Keep existing public
signatures unless explicitly listed. Module wiring is pre-created in Task 0 rather
than requiring concurrent parent edits.

| Wave | Tasks | Incoming seam | Outgoing seam |
| --- | --- | --- | --- |
| 0 | upfront-contract-synthesis-and-dag-prioritization | Current ADRs and inspected execution base | Strict DTOs, API scaffolds, fixtures, registered seam modules, frozen matrix |
| 1 | memory-document-projection; evidence-grounded-synthesis | Shared DTOs and independent stub modules | Rebuildable document projection; bounded provenance/candidate validation |
| 2 | deterministic-resolver-and-memory-lifecycle | Both Wave 1 outputs | Shared resolver, `query` BM25 façade (`include_stale`), and source-first maintenance backend |
| 3 | universal-gate-context-and-closure-evidence | Resolver and evidence packet | Equal `DomainProvider` gate consumption and live failure capture across `ephemeral`, `in_place`, and `worktree` runs |
| 4 | cli-mcp-and-synthesis-workflow-assets | Stable backend and runtime context | Dual-surface maintenance (`acdd memory` / `acdd_memory`), scan diagnostics, validated closure examples |
| 5 | cross-surface-memory-acceptance | All producers and consumers | End-to-end proof and documentation scan packet |

Only Wave 1 is parallel. Its scopes are disjoint, including test/fixture and docs
files. Repeated file ownership across later waves is sequential. No task may
edit a shared test index after Task 0. Scope extensions stay within admitted
roots and preserve these boundaries; protected infrastructure remains excluded.

### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: Freeze the memory schema, diagnostic and producer-consumer matrix, scaffold shared APIs and all seam modules, reconcile the landed 0.6.0 execution base, and prove strict pure memory validation without new dependencies
subtasks:
- subtask_ref: cross-task-data-contract-matrix
  title: Record exact schema, public signatures, errors, budgets and evidence packet fields in the architecture page and run blackboard
- subtask_ref: deterministic-priority-waves-dag
  title: Reconcile ADR 0003 / ADR 0004 and DomainProvider availability against the 0.6.0 execution base and freeze the six acyclic waves
- subtask_ref: seam-test-harness-pre-registration
  title: Register memory/mod.rs once in tests/acdd/mod.rs and pre-create every downstream module and minimal reusable fixture substrate
- subtask_ref: disjoint-scope-boundaries-freeze
  title: Verify symbol-container closure and disjoint Wave 1 file, fixture and documentation ownership before downstream claims
- subtask_ref: shared-error-taxonomy-and-diagnostic-codes
  title: Implement pure strict document validation and typed request scaffolds with shared diagnostics; keep backend stubs compiling and explicitly unavailable
contract_revision: 1
depends_on: []
scope:
- docs/milestones/AC-006-universal-repository-memory-synthesis.md
- docs/adr/0004-domain-provider-spi-and-plugin-tools.md
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/architecture/README.md
- docs/architecture/repository-memory.md
- crates/acdd-core/src/models.rs
- crates/acdd-core/src/ports.rs
- src/state_bridge/mod.rs
- src/state_bridge/docs_domain.rs
- src/cli/mod.rs
- src/cli/memory.rs
- tests/acdd/mod.rs
- tests/acdd/memory/mod.rs
- tests/acdd/memory/support.rs
- tests/acdd/memory/schema.rs
- tests/acdd/memory/projection.rs
- tests/acdd/memory/synthesis.rs
- tests/acdd/memory/resolver.rs
- tests/acdd/memory/consumption.rs
- tests/acdd/memory/surfaces.rs
- tests/acdd/memory/integration.rs
- tests/acdd/fixtures/memories/schema/
status: planned
```

Proof slice: `tests/acdd/memory/schema.rs::strict_memory_document_contract`.
Use a single in-memory table of valid and malformed frontmatter/body/budget cases.
Contract scaffolds compile; runtime assertions fail until strict validation is
implemented. Sensitivity: remove unknown-field rejection or accept an oversized
rule and the same matrix fails. No DTO echo tests or unconditional panic stubs.
Incoming: existing leaf models.
Outgoing: `RepositoryMemory::parse(path: &str, source: &str) -> anyhow::Result<Self>`
and `memory(root: &std::path::Path, request: MemoryRequest) -> anyhow::Result<serde_json::Value>`.
The request covers action (`list | show | query | check | synthesize | prune`),
subject/IDs, search text + `include_stale`, candidate input, explicit apply and
bounded paging; Task 0 records its exact field schema and internal live-evidence
packet type without adding a public parallel evidence model. Negative boundary:
no index/store access in core; no `acdd-index` dependency in `acdd-state`. The
manifest coordinate `RepositoryMemory.parse` uses the parser's dotted symbol
spelling for the Rust associated function.

### task: memory-document-projection

```yaml
task_ref: memory-document-projection
target: Project strict memory documents through existing docs extraction, node metadata, cold/cache/delta persistence and removal without new tables, preserving source digests and ordinary documentation behavior
subtasks:
- subtask_ref: strict-memory-extraction
  title: Use the shared parser only for the memory document domain and emit one canonical metadata owner plus searchable sections
- subtask_ref: projection-and-cache-preservation
  title: Retain validated memory metadata through existing navigation details, staged storage and cache paths; invalidate extraction semantics when required
- subtask_ref: cold-delta-removal-parity
  title: Prove edit, status change, malformed replacement and deletion remove obsolete advice candidates without a separate registry
contract_revision: 1
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
scope:
- crates/acdd-index/src/docs.rs
- crates/acdd-core/src/models.rs
- crates/acdd-core/src/config.rs
- crates/acdd-index/src/db/ingest.rs
- crates/acdd-index/src/db/writer.rs
- src/engine/pipeline.rs
- docs/architecture/indexing.md
- tests/acdd/memory/projection.rs
- tests/acdd/fixtures/memories/projection/
status: planned
```

Proof slice: `tests/acdd/memory/projection.rs::memory_projection_round_trip`.
Use the real build/delta/cache pipeline with one base workspace outside the case
loop; inspect canonical stored nodes, sections and digests after batched mutations.
Sensitivity: strip `memory` metadata at document-node construction or navigation
serialization; cold/cache/delta parity assertions fail. Incoming: core parser.
Outgoing: canonical document-node metadata consumed by the resolver and `doc_search`
FTS5 index (`doc_type = "memory"`). No task-state schema migration, duplicate
memory table, global strict docs parser, or new crate.

### task: evidence-grounded-synthesis

```yaml
task_ref: evidence-grounded-synthesis
target: Collect bounded subject evidence from existing StateBackend traces, blackboard, and run tombstones and validate useful, provenance-bound, deduplicated or explicitly superseding memory candidates with a successful empty-result path
subtasks:
- subtask_ref: subject-evidence-selection
  title: Read existing gate_traces, blackboard findings/decisions/notes, and run_tombstones by canonical run/node subject across persistent and ephemeral backends
- subtask_ref: candidate-admission-and-noise-rejection
  title: Validate packet provenance, bounded rules/signals and explicit supersession; expose a reviewable zero-candidate no-op
- subtask_ref: deterministic-deduplication
  title: Normalize fingerprints, preserve unchanged timestamps, and reject cyclic, dangling or conflicting candidate relationships
contract_revision: 1
depends_on:
- upfront-contract-synthesis-and-dag-prioritization
scope:
- crates/acdd-state/src/store/traits.rs
- crates/acdd-state/src/store/sqlite/reader.rs
- crates/acdd-state/src/usecases/queries.rs
- src/state_bridge/docs_domain.rs
- docs/reference/repository-memory.md
- tests/acdd/memory/synthesis.rs
- tests/acdd/fixtures/memories/synthesis/
status: planned
```

Proof slice: `tests/acdd/memory/synthesis.rs::evidence_grounded_candidate_matrix`.
Use in-process `StateBackend` / `RunHarness` transitions and typed live observations,
with one seeded store outside homogeneous case loops. Include rejected gates,
command failure, selected decision/notes, `run_tombstones` dream inputs,
unrelated subjects, unavailable history, forged evidence, repeated candidates
and supersession cycles. Quality review uses paired useful/noisy fixtures;
mechanical checks bind evidence rather than claiming semantic understanding.
Sensitivity: admit a candidate with a foreign evidence ID or drop rejected
attempts and the matrix fails. No writes to source/index here; the sequential
lifecycle backend owns publication and cross-file retry behavior.

### task: deterministic-resolver-and-memory-lifecycle

```yaml
task_ref: deterministic-resolver-and-memory-lifecycle
target: Implement the single deterministic bounded resolver, thin doc_search BM25 query façade, and source-first list/show/query/check/synthesize/prune backend with anchor staleness, digest conflict protection and resumable supersession
subtasks:
- subtask_ref: explicit-intersection-and-ranking
  title: Resolve confined paths, unique symbols and exact gate IDs through one stable ranker with top-K, serialized-byte and computation bounds
- subtask_ref: bm25-query-facade-and-anchor-staleness
  title: Implement query as a thin typed façade over reader::search_docs_with_options(doc_type="memory") filtering stale/superseded by default, and suppress broken-anchor advice
- subtask_ref: source-first-maintenance
  title: Implement dry-run/apply operations, optimistic digest checks, atomic per-file writes and deterministic interrupted-supersession retry without a SQLite authority
contract_revision: 1
depends_on:
- memory-document-projection
- evidence-grounded-synthesis
scope:
- src/state_bridge/docs_domain.rs
- src/state_bridge/mod.rs
- crates/acdd-index/src/db/reader.rs
- docs/reference/repository-memory.md
- docs/architecture/repository-memory.md
- tests/acdd/memory/resolver.rs
- tests/acdd/fixtures/memories/resolver/
status: planned
```

Proof slice: `tests/acdd/memory/resolver.rs::deterministic_resolution_and_lifecycle`.
One indexed corpus exercises path/symbol/gate unions, empty anchors, ambiguous
symbols, moved/deleted anchors, gate-only rules, `query` BM25 façade with
`include_stale: false` vs `true`, stable ties, Ukrainian/CJK/emoji, JSON
escaping, five-item and 4096-byte boundaries, overflow, unavailable coverage,
source drift, stale persistence, preview/apply and interrupted replacement retry.
Use distinct domain tests only for an orthogonal publication failure seam.
Sensitivity: bypass anchor checks, return stale/superseded records on default
`query`, build a second BM25 engine, reverse the tie-break, ignore serialized
escaping, or reactivate stale advice; owning assertions fail. Incoming: projection
and evidence candidates. Outgoing: one bridge facade and crate-internal resolver.
No auto-commit, source mutation on read, or active prune.

### task: universal-gate-context-and-closure-evidence

```yaml
task_ref: universal-gate-context-and-closure-evidence
target: Consume the same compact memories once across worktree, in_place, and ephemeral Run gate contexts via DomainProvider and capture failed attempts for explicit or configured closure synthesis without changing persistence, proof or contract documentation
subtasks:
- subtask_ref: equal-stage-context-consumption
  title: Wire the docs.memory DomainProvider context lens into build_claim_context and spawner prompt construction across all Run modes
- subtask_ref: live-failure-evidence-bridge
  title: Retain bounded rejected/failed observations in StateBackend before retries and terminal returns so in-run synthesis receives authentic evidence
- subtask_ref: closure-and-persistence-boundaries
  title: Expose synthesis input before integration/clear and support explicit --scope confinement on ephemeral/in-place starts while preserving commit, snapshot, and save:none semantics
contract_revision: 1
depends_on:
- deterministic-resolver-and-memory-lifecycle
scope:
- crates/acdd-state/src/context.rs
- crates/acdd-state/src/machine/claim.rs
- crates/acdd-state/src/machine/submit.rs
- crates/acdd-state/src/runner/spawner.rs
- crates/acdd-state/src/runner/waves.rs
- crates/acdd-state/src/usecases/lifecycle.rs
- src/state_bridge/mod.rs
- src/state_bridge/docs_domain.rs
- docs/adr/0003-unified-profile-graph-and-state-machine.md
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/reference/acdd.md
- docs/reference/tasks.md
- docs/reference/cli.md
- tests/acdd/memory/consumption.rs
- tests/acdd/fixtures/memories/consumption/
status: planned
```

Proof slice: `tests/acdd/memory/consumption.rs::all_execution_surfaces_consume_memories`.
Compare equal normalized inputs through real `build_claim_context` (`worktree`,
`in_place`, and `ephemeral`/`save: none` runs) and spawner prompt capture; test
both default and configured templates. Exercise command rejection/repair and
terminal failure with `RunHarness` fixtures only where actual prompt/process
behavior is under test. Assert memory appears exactly once inside `ClaimResp`,
does not displace contract/spec/findings, respects the entire envelope budget,
and disappears on anchor deletion. Inspect SQLite rows and Git effects for all
three isolation/save modes. Sensitivity: remove any `DomainProvider` lens call
or discard a failing observation and the corresponding row fails.

### task: cli-mcp-and-synthesis-workflow-assets

```yaml
task_ref: cli-mcp-and-synthesis-workflow-assets
target: Expose identical CLI/MCP memory inspection, query, and explicit maintenance, report effective staleness during scan, and document executable memory_synthesis composition after docs_sync for each GateProfile shape
subtasks:
- subtask_ref: dual-surface-memory-operations
  title: Route strict typed acdd memory CLI and acdd_memory MCP requests through shared bridge backend with pagination, worktree resolution, Markdown/JSON output and apply guards
- subtask_ref: scan-and-anchor-diagnostics
  title: Invoke the same read-only check from scan, report incomplete coverage explicitly and preserve ordinary scan and document retrieval behavior
- subtask_ref: external-closure-profiles-and-skills
  title: Add validated v2 GateProfile examples (docs/workflows/*.yaml, skills/acdd/references/acdd-profile-examples/*.yaml) for pre-integration milestone synthesis, in_place closure, ephemeral synthesis after docs_sync, and batch memory-dream consolidation, and update ADR-0004/ADR-0006, architecture/reference/runbooks docs, skills/acdd/SKILL.md, ROADMAP.md, and CHANGELOG.md
contract_revision: 1
depends_on:
- universal-gate-context-and-closure-evidence
scope:
- src/cli/mod.rs
- src/cli/memory.rs
- src/mcp/tools.rs
- src/mcp/tools/inputs.rs
- src/mcp/metadata.rs
- src/format/mod.rs
- src/format/lifecycle.rs
- crates/acdd-core/src/subsystems.rs
- docs/adr/0004-domain-provider-spi-and-plugin-tools.md
- docs/adr/0006-budgeted-gate-context-and-dual-surface-parity.md
- docs/architecture/repository-memory.md
- docs/reference/cli.md
- docs/reference/mcp-tools.md
- docs/reference/configuration.md
- docs/reference/profiles.md
- docs/reference/tasks.md
- docs/reference/README.md
- docs/reference/repository-memory.md
- docs/runbooks/
- docs/workflows/
- skills/acdd/SKILL.md
- skills/acdd/references/tool-catalog.md
- skills/acdd/references/acdd_profile.yaml.example
- skills/acdd/references/acdd-profile-examples/
- ROADMAP.md
- CHANGELOG.md
- tests/acdd/memory/surfaces.rs
- tests/acdd/fixtures/memories/surfaces/
status: planned
```

Proof slice: `tests/acdd/memory/surfaces.rs::memory_cli_mcp_operation_parity`.
Exercise production `acdd memory ...` CLI dispatch, `acdd tool acdd_memory`, and
in-process MCP `acdd_memory` (`list | show | query | check | synthesize | prune`)
on one materialized workspace, with a table of operations, `query` with
`include_stale` false vs true, invalid inputs, disabled `modules.memory`,
alternate worktree roots and dry-run/apply outcomes. Prove full memory content is
retrievable through `acdd_read`. Parse all changed profile examples with the
real v2 profile validator. Explicit memory source writes require admitted
`docs/memories/` scope, supplied via `--scope docs/memories/` or declared task
scope; no inference from free-text goals or hardcoded artifact-to-directory
mapping is authorized. Sensitivity: diverge an MCP action from its CLI backend,
leak stale hits in default `query`, or let scan mutate a source; the matrix fails.
No embedded template registration or `.acdd/profile.yaml` edits.

### task: cross-surface-memory-acceptance

```yaml
task_ref: cross-surface-memory-acceptance
target: Prove the complete evidence-to-memory-to-next-gate path across worktree, in_place, and ephemeral Run modes, including retries, stale anchors, source authority and unchanged lifecycle proofs, then prepare the final documentation scan packet
subtasks:
- subtask_ref: cross-surface-production-round-trip
  title: Run observed rejection/decision through synthesis admission and source publication, rebuild the projection, and assert the next applicable gate consumes the same bounded rule across all three Run modes
- subtask_ref: lifecycle-and-recovery-acceptance
  title: Prove empty synthesis, repeated synthesis, default-clean query vs --include-stale, supersession retry, deleted anchors, index rebuild and clear/integrate behavior without new registries or ephemeral task rows
- subtask_ref: documentation-scan-evidence-packet
  title: Bind final diff, manifest and accepted task evidence for the documentation scan and independent integration review
contract_revision: 1
depends_on:
- cli-mcp-and-synthesis-workflow-assets
scope:
- tests/acdd/memory/integration.rs
- tests/acdd/fixtures/memories/integration/
- docs/milestones/AC-006-universal-repository-memory-synthesis.md
status: planned
```

Proof slice: `tests/acdd/memory/integration.rs::repository_memory_end_to_end`.
One table drives the three genuine `Run` isolation/save modes over a shared
indexed base; use distinct mutable workspaces only where persistence semantics
require it. Observe rejected evidence, admitted Markdown, SQLite projection and
a subsequent gate's rendered `ClaimResp` context. Delete/move an anchor and observe
suppression on both the real `DomainProvider` consumer path and default `query`.
Erase/rebuild the docs projection and prove identical results from source. Clear
or integrate execution state and prove curated source remains. A `save: none`
run leaves zero SQLite task rows/snapshots/commits. A missing projection cannot
silently serve stale advice. Sensitivity: bypass one production producer or
consumer, route a foreign subject, or preserve a stale candidate; the same test
fails. Source defects route to the owning task rather than being patched under
this verification-only scope.

## Acceptance and verification

| Criterion | Observable production proof | Owning task |
| --- | --- | --- |
| AC-01 | Strict, bounded Markdown parses; ordinary docs keep their contract | Task 0, projection |
| AC-02 | Cold, cache and delta yield the same canonical memory projection; deletion removes it | Projection |
| AC-03 | Rejections/decisions/failures bind candidates; noisy or foreign evidence is rejected; empty synthesis is a no-op | Synthesis |
| AC-04 | Identical scope/symbol/gate inputs give stable ranked K=5, <=4096-byte context across all Run modes | Resolver, consumption |
| AC-05 | Moved/deleted/ambiguous anchors suppress advice; unavailable coverage never asserts deletion | Resolver, surfaces |
| AC-06 | `acdd memory` CLI and `acdd_memory` MCP (`list \| show \| query \| check \| synthesize \| prune`) share semantics; `query` is a thin façade over `reader::search_docs_with_options(doc_type="memory")` (`doc_search` `bm25(doc_search, 3.0, 1.0, 1.0)`) filtering/marking `stale`/`superseded` by default (`include_stale: false`); `acdd_read` pulls full content | Resolver, surfaces |
| AC-07 | `docs_sync` remains live documentation; per-run synthesis precedes final integration and batch `--dream` consolidates `run_tombstones` off the hot path while retaining subject persistence and review/proof | Consumption, integration |
| AC-08 | Supersession/retry and index rebuild preserve source authority; `clear`/`integrate` preserve curated files | Resolver, integration |
| AC-09 | No new dependencies, protected infrastructure edits, duplicate registries, second BM25/vector engine, root test binaries or leaked test tempdirs | Task 0 and final diff review |
| AC-10 | `ADR-0004`, `ADR-0006`, `docs/architecture/`, `docs/reference/{profiles,tasks,cli,mcp-tools,configuration}.md`, `docs/runbooks/`, `docs/workflows/*.yaml`, `skills/acdd/references/acdd-profile-examples/*.yaml`, `skills/acdd/SKILL.md`, `ROADMAP.md`, and `CHANGELOG.md` pass `acdd docs check` and `acdd validate` | Surfaces, integration |

Each task's blueprint slice records implementation steps, exact signatures,
incoming/outgoing seams, test seam and negative boundaries. Task 0 posts the
matrix on the run `blackboard` before downstream work. Each contract gate
compiles and fails at runtime on its named seam, then build proves green through
`acdd verify <node> --test <test_path::test_fn>` and strict Clippy as required
by [TESTS.md](../../TESTS.md). Later consumers get new production-seam contracts;
review repair adds at most a missing row, with zero new test functions.

Use `PersistedSnapshot` for shared read-only indexes, in-process `StateBackend`
(`MemoryStateBackend` / `SqliteStateBackend`) for state/evidence matrices, existing
`RunHarness` for materialized runtime shapes, and existing process harnesses only
for actual runner contracts. Create no top-level `tests/*.rs`, static
`Workspace`/`TempDir`, per-row cold builds, mock collaborator chain,
manifest-substring test, or benchmark requirement.

Before implementation admission, run the read-only manifest validator and start
the run:

```bash
acdd validate docs/milestones/AC-006-universal-repository-memory-synthesis.md
acdd start docs/milestones/AC-006-universal-repository-memory-synthesis.md --in-place
```

Run startup belongs to preflight and synchronizes operational task state; it does
not authorize executing this planned milestone during authoring. Before
handoff, run a documentation scan with the final diff, manifest, accepted
evidence, and explicit documentation scope. Reconcile architecture, ADRs,
references, profiles, skills, and examples against live code; route source
findings to owners, independently review updates, and repeat affected scans
after implementation changes. Run the final compound gate from [TESTS.md](../../TESTS.md)
and `cargo test -p acdd-state --lib`, recording actual counts. No benchmark is
authorized. Preserve accepted proofs, final content binding, independent review,
and normal milestone archival/landing rules.

## Negative boundaries and deferred owners

No vector database, embeddings, fuzzy retrieval, automatic global advice,
user-level memory service, generated diff diary, hidden scheduling hook, or new
SQLite evidence ledger is admitted. No mandatory corpus is bundled in the binary.
No requirement depends on the AC-007 Web UI, a remote service, a model vendor, or
unmerged AC-007/AC-008/AC-009 manifest paths. Existing adapters configure docs roots;
an unadmitted memory directory is reported rather than silently rewriting config.
Historical stateless workflow evidence that was never retained is unavailable;
recovering it belongs to a future explicitly admitted persistence capability.
Semantic rule conflicts are resolved through curated source/review, never by a
new authority hierarchy or a resolver guessing which architecture to enforce.
