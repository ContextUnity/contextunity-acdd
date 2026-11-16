---
id: AC-006-universal-repository-memory-synthesis
title: Universal repository memory synthesis and deterministic gate consumption
doc_type: contract
status: planned
depends_on: []
guidance:
  hitl: pro
owners:
  - crates/acdd-core/src/models.rs
  - crates/acdd-index/src/docs.rs
  - crates/acdd-tasks/src/
  - src/engine/pipeline.rs
  - src/cli/
  - src/mcp/
  - docs/reference/
  - skills/acdd/
invariants:
  - 'INV-MEMORY-SOURCE-AUTHORITY: Confined Git-trackable docs/memories/*.md files are the sole memory authority; SQLite contains only their rebuildable docs-index projection, never a second writable registry.'
  - 'INV-CORE-LEAF: Memory DTOs and pure validation in acdd-core import no acdd-index, acdd-tasks, application engine, CLI, or MCP code.'
  - 'INV-MEMORY-EVIDENCE: Every admitted rule cites observed execution evidence and a source subject; empty evidence and ordinary diff summaries produce no memory.'
  - 'INV-MEMORY-DETERMINISTIC-CONSUMPTION: Milestone, Plan, and taskless Workflow context use one explicit path/symbol/gate resolver, with stable ranking and identical bounded projections for identical inputs.'
  - 'INV-MEMORY-BOUNDS: At most five compact memories and 4096 serialized UTF-8 bytes of memory context reach a gate; full documents are pulled through acdd_read.'
  - 'INV-MEMORY-ANCHORS: Stale, superseded, malformed, ambiguous, unavailable, or digest-mismatched memories never supply active gate advice.'
  - 'INV-MEMORY-LIFECYCLE: docs_sync retains live contract documentation; memory_synthesis supports both pre-binding per-run gates (alongside or after docs_sync) and off-hot-path batch "dreaming" passes (synthesize --dream) over accumulated run tombstones and archived specs, without making synchronous synthesis mandatory on every small run or changing subject persistence or proof authority.'
  - 'INV-MINIMAL-INTERVENTION: Reuse docs extraction, SQLite document nodes, existing task evidence, gate graphs, confinement, and response budgets; introduce no dependency, embedded memory corpus, model client, or parallel run registry.'
  - 'INV-SCOPE-INTEGRITY: Cargo.toml, all member Cargo.toml files, build.rs, and .acdd/** are excluded from every task write scope.'
  - 'INV-MEMORY-SEAM-HYGIENE: Register every new seam under tests/acdd/ in Task 0; use table-driven production seams, zero new root test binaries, and zero leaked temporary workspaces.'
required_skills:
  - acdd
  - contract-planning
  - agent-instructions
related_plans: []
---

# AC-006: Universal repository memory synthesis

## Goal and admission boundary

Curate reusable operational rules from execution evidence, keep them as small
repository documents, and deterministically deliver only applicable rules to
Milestone, Plan, and Standalone Workflow gates under [ADR 0021](../adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md).
This contract authors the implementation queue; it does not seed fabricated
memories or implement the capability during milestone authoring.

`AC` is the lifecycle domain. The primary checkout contains archived AC-001 through
AC-003; AC-004 and AC-005 are reserved and active in separate worktrees. AC-006 is
the next unused number across that inventory. Neither active manifest is present
on `main`, so `depends_on: []` deliberately contains no unresolved links.
Execution assumes AC-004 Plan unification and specification traceability **if
already landed**. Task 0 re-inspects the execution base: use its canonical
TasksStore/task-runner Plan path when present; otherwise retain the current
source-owned Plan adapter described below. Do not copy AC-004 functionality or
create a second Plan registry. Preserve landed spec/acceptance-criteria context.
AC-005 inspector UI is outside this milestone.

The add-on names ADR 0024, but the primary checkout's ADR inventory ends at 0021.
There is no available ADR 0024 authority to infer. Reconcile any newly landed ADR
at Task 0; the present design preserves existing artifact ownership and clear
boundaries without depending on an unavailable decision.

## Architectural reconciliation and verified owners

[ADR 0018](../adr/0018-layer-dependency-boundaries-and-core-leaf-isolation.md),
[performance invariants](../architecture/performance.md), and the actual crate
layout require a leaf `acdd-core` and no new reverse dependency. Keep pure memory
data/validation in core, extraction in index, and evidence selection, resolution,
and explicit source maintenance in tasks. Application adapters perform transport
and scan wiring. No new crate dependencies or infrastructure exceptions are needed.

[ADR 0016](../adr/0016-lean-stage-tailored-task-context-bundles.md) and
[ADR 0011](../adr/0011-bounded-mcp-response-budgets.md) govern compact context.
[ADR 0005](../adr/0005-filesystem-root-guard-and-symlink-safety.md),
[ADR 0007](../adr/0007-fail-closed-selector-disambiguation.md), and
[ADR 0003](../adr/0003-two-phase-staged-publication.md) govern confinement,
unique anchor resolution, and index publication. Memory does not override
contracts, invariants, ADRs, rejection findings, independent review, or proof.

These declarations were inspected with `code_map_inspect` on the primary checkout;
the extraction and context paths were also traced with depth-one impact queries.
Line locations identify the inspected baseline and must be refreshed at Task 0.

| Existing declaration | Verified declaring container | Role in the design |
| --- | --- | --- |
| `DocSection` (563), `NavigationDetails` (288) | `crates/acdd-core/src/models.rs` | Existing docs DTO and metadata serialization boundary; adjacent pure memory DTO, no registry |
| `extract_with_details` (51) | `crates/acdd-index/src/docs.rs` | Returns document sections plus structured source metadata |
| `extract_typed_source_with_digest` (410) | `src/engine/pipeline.rs` | Copies selected document metadata into document-node details |
| `TasksStore` (impl at 467) | `crates/acdd-tasks/src/store/mod.rs` | Owns task findings, blackboard, and lifecycle evidence queries |
| `GateTransition` (49) | `crates/acdd-tasks/src/store/trace.rs` | Existing observed gate evidence; read without a second evidence ledger |
| `context_bundle` (727) | `crates/acdd-tasks/src/engine/context.rs` | Milestone claim/context integration; uses current scope and gate |
| `RoleObservations::execute` (`execute`, 84) | `crates/acdd-tasks/src/engine/orchestrator/role.rs` | Shared taskless role prompt construction, including current Plan roles |
| `run_plan_workflow` (286) | `crates/acdd-tasks/src/engine/orchestrator/plan.rs` | Current Plan scope, stage observations, source evidence and snapshots |
| `run_standalone_workflow` (109) | `crates/acdd-tasks/src/engine/orchestrator/workflow.rs` | Root gate observations with no task identity or persistence upgrade |
| `handoff_context` (373) | `crates/acdd-tasks/src/milestones/handoff/mod.rs` | Existing final-binding boundary; memory writes finish before invocation |
| `Command` (666) | `src/cli/mod.rs` | CLI dispatch extension; MCP uses the same backend |
| `WorkflowCommand` (10) | `src/cli/workflow.rs` | Explicit invocation scope reaches the taskless runner |

`search_docs` grounding covers core leaf isolation, minimal intervention and scope
integrity in the contract-planning add-on, stage-tailored context in ADR 0016,
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
and extraction-cache replay. No `memories` table, TasksStore mirror, SQLite-only
status authority, or sidecar registry is admitted. A missing directory means an
empty corpus; malformed memory files are diagnostics and never active advice.

### Evidence-grounded synthesis and source maintenance

The synthesis operation first returns a bounded evidence packet for an explicit
subject. Milestone evidence comes from `task_findings`, `gate_transitions`,
`task_blackboard` decisions/notes, and observed command failures. Use all admitted
subject tasks, not only the most recent successful claim. Plans use those same
owners after AC-004 if landed; the current adapter additionally reads canonical
Plan Evidence sections and its live gate observations. Workflows use live root
gate decisions and command observations; read native run logs only if a landed
owner already provides them. Never fabricate task rows for workflows.

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

Milestone claim/context calls this resolver in `engine/context.rs`. Plans call it
from their actual task/stage context owner; current source-owned Plan roles use
`role.rs`, while landed unified Plans reuse `context_bundle`. Standalone workflows
call the same resolver from `RoleObservations::execute` using explicit workflow
scope and cursor. Both configured and default prompt templates receive the compact
field once. `bundle: false` stays minimal. Memory-free repositories preserve the
existing context shape. Missing/unadmitted index or disabled docs/codemap yields
no injected advice plus a bounded availability diagnostic, not a new hard tool
requirement or a failed task claim. Memory is never a proof or authorization source.

### Anchor checks, stale status and closure placement

Every declared scope glob must match at least one currently admitted file; every
symbol must resolve uniquely in its owning workspace. A moved/deleted/ambiguous
anchor makes the **whole** memory effectively `stale`, even when a gate matches.
Gate-only memories need no filesystem anchor. Recheck anchors against the current
index at scan/check and before consumption; a scope-only code deletion invalidates
advice even when the Markdown did not change. Do not cache validity solely by the
memory file digest. Source status and effective status are reported separately.

`scan` reports effective stale status and leaves tracked Markdown unchanged.
`memory check --apply` persists `status: stale` for confirmed broken anchors with
an optimistic source-digest check. Unavailable/incomplete index coverage does not
prove deletion and cannot persist staleness. Repairing anchors does not silently
reactivate a source marked stale; an explicit checked maintenance update is needed.
`prune` defaults to preview and only removes explicitly selected stale/superseded
files with `--apply`; reject active targets and dangling supersedes references.
Neither clear nor handoff pruning deletes curated memory documents.

`memory_synthesis` is an ordinary configured gate/operation, alongside or after
`docs_sync`. There is no new gate DSL, mandatory synthesis on every task, or
replacement of live documentation. Milestones synthesize before task evidence is
pruned and before final handoff-context binding; changes afterward require renewed
binding, verification and review. Plans keep their existing tasks-only graph:
use an admitted final Plan step/stage or explicit closure synthesis without adding
milestone root gates. Workflows append a taskless synthesis gate before returning,
with memory paths explicitly admitted to the invocation's write scope; `save: none`
still creates no task rows, snapshots or commits. Capture evidence on failure so
an in-run repair/synthesis route can consume it. On the current main Plan adapter,
explicit closure synthesis consumes the returned live packet/canonical Evidence
instead of pretending there is a root-gate container. Profile examples exercise
each shape and preserve its existing persistence guarantees.

The current standalone runner derives only authoring directories or its own
workflow path; it has no explicit memory write-scope input. Admit one optional,
repeatable `workflow run --scope <relative-path>` input for this boundary, passed
through `run_standalone_workflow_with_scope(root, path, goal, scope)` and normalized
with existing confinement checks. Preserve the existing public
`run_standalone_workflow` as the default-scope wrapper. Explicit scope replaces
inferred scope; it never widens from goal prose, a memory match, or an artifact
name. The synthesis example explicitly supplies `--scope docs/memories/`.
This is invocation authority, not a new workflow registry or profile dialect.

### CLI and MCP contract

Provide `acdd memory list`, `show <id>`, `check [--apply]`,
`synthesize (--subject <subject> | --dream [--since <cursor>]) [--candidates <path>] [--apply]`,
and `prune <id>... [--apply]`. Synthesize without candidates returns the
evidence packet; candidates without `--apply` return the validated proposed
changes. Use a single MCP `memory_manage` action interface with the same strict
typed request/backend, workspace routing, pagination, errors and apply semantics.
Do not overload `task_manage` or create fake tasks. List/show/check expose source
path/digest, declared/effective status, anchors, diagnostics, and pull references.
Unknown IDs, ambiguous IDs, path escape/symlinks, bad requests, invalid candidates,
and write conflicts fail closed. List/show/check without apply are read-only.
No operation commits, pushes, force-deletes active memories, or edits configuration.

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
`MemoryRequest`, tasks `memory`, or MCP `memory_manage`. Their declaring files
are explicitly scoped. Existing container evidence is the inspected core
`models.rs`, tasks `lib.rs`/`engine/mod.rs`, and CLI/MCP input/handler modules.
Task 0 creates and indexes the shared new declarations before downstream scope closure
is checked. Do not declare a new symbol against an unrelated existing container.

Public additions admitted here are `RepositoryMemory` and its pure `parse`
constructor in core; `MemoryRequest` and the `memory` dispatch function in tasks;
the explicitly scoped workflow entry point; and the `memory_manage` MCP route.
`run_standalone_workflow_with_scope` is a proposed addition to the inspected
`engine/orchestrator/workflow.rs`, not an existing declaration. Its old wrapper
keeps all existing callers compatible. The shared resolver/evidence helpers remain
crate-internal. Keep existing public signatures unless explicitly listed. Module
wiring is pre-created in Task 0 rather than requiring concurrent parent edits.

| Wave | Tasks | Incoming seam | Outgoing seam |
| --- | --- | --- | --- |
| 0 | upfront-contract-synthesis-and-dag-prioritization | Current ADRs and inspected execution base | Strict DTOs, API scaffolds, fixtures, registered seam modules, frozen matrix |
| 1 | memory-document-projection; evidence-grounded-synthesis | Shared DTOs and independent stub modules | Rebuildable document projection; bounded provenance/candidate validation |
| 2 | deterministic-resolver-and-memory-lifecycle | Both Wave 1 outputs | Shared resolver and source-first maintenance backend |
| 3 | universal-gate-context-and-closure-evidence | Resolver and evidence packet | Equal gate consumption and live failure capture on all three surfaces |
| 4 | cli-mcp-and-synthesis-workflow-assets | Stable backend and runtime context | Dual-surface maintenance, scan diagnostics, validated closure examples |
| 5 | cross-surface-memory-acceptance | All producers and consumers | End-to-end proof and documentation scan packet |

Only Wave 1 is parallel. Its scopes are disjoint, including test/fixture and docs
files. Repeated file ownership across later waves is sequential. No task may
edit a shared test index after Task 0. Scope extensions stay within admitted
roots and preserve these boundaries; protected infrastructure remains excluded.

### task: upfront-contract-synthesis-and-dag-prioritization

```yaml
task_ref: upfront-contract-synthesis-and-dag-prioritization
target: "Freeze the memory schema, diagnostic and producer-consumer matrix, scaffold shared APIs and all seam modules, reconcile the landed execution base, and prove strict pure memory validation without new dependencies"
proof_policy: seam-test-first
contract_revision: 1
depends_on: []
scope:
  - docs/milestones/AC-006-universal-repository-memory-synthesis.md
  - docs/adr/0016-lean-stage-tailored-task-context-bundles.md
  - docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md
  - docs/architecture/README.md
  - docs/architecture/repository-memory.md
  - crates/acdd-core/src/models.rs
  - crates/acdd-tasks/src/lib.rs
  - crates/acdd-tasks/src/memory/mod.rs
  - crates/acdd-tasks/src/memory/resolver.rs
  - crates/acdd-tasks/src/memory/synthesis.rs
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
symbols:
  - struct:RepositoryMemory
  - fn:RepositoryMemory.parse
  - struct:MemoryRequest
  - fn:memory
status: ready
subtasks:
  - subtask_ref: cross-task-data-contract-matrix
    title: "Record exact schema, public signatures, errors, budgets and evidence packet fields in the architecture page and milestone blackboard"
  - subtask_ref: deterministic-priority-waves-dag
    title: "Refresh AC-004 and ADR availability against the actual execution base and freeze the six acyclic waves without recreating Plan infrastructure"
  - subtask_ref: seam-test-harness-pre-registration
    title: "Register memory/mod.rs once in tests/acdd/mod.rs and pre-create every downstream module and minimal reusable fixture substrate"
  - subtask_ref: disjoint-scope-boundaries-freeze
    title: "Verify symbol-container closure and disjoint Wave 1 file, fixture and documentation ownership before downstream claims"
  - subtask_ref: shared-error-taxonomy-and-diagnostic-codes
    title: "Implement pure strict document validation and typed request scaffolds with shared diagnostics; keep backend stubs compiling and explicitly unavailable"
```

Proof slice: `tests/acdd/memory/schema.rs::strict_memory_document_contract`.
Use a single in-memory table of valid and malformed frontmatter/body/budget cases.
Contract scaffolds compile; runtime assertions fail until strict validation is
implemented. Sensitivity: remove unknown-field rejection or accept an oversized
rule and the same matrix fails. No DTO echo tests or unconditional panic stubs.
Incoming: existing leaf models.
Outgoing: `RepositoryMemory::parse(path: &str, source: &str) -> anyhow::Result<Self>`
and `memory(root: &std::path::Path, request: MemoryRequest) -> anyhow::Result<serde_json::Value>`.
The request covers action, subject/IDs, candidate input, explicit apply and bounded
paging; Task 0 records its exact field schema and internal live-evidence packet
type without adding a public parallel evidence model. Negative boundary: no
index/store access in core. The manifest coordinate `RepositoryMemory.parse`
uses the parser's dotted symbol spelling for the Rust associated function.

### task: memory-document-projection

```yaml
task_ref: memory-document-projection
target: "Project strict memory documents through existing docs extraction, node metadata, cold/cache/delta persistence and removal without new tables, preserving source digests and ordinary documentation behavior"
proof_policy: seam-test-first
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
symbols:
  - fn:extract_with_details
  - struct:NavigationDetails
status: planned
subtasks:
  - subtask_ref: strict-memory-extraction
    title: "Use the shared parser only for the memory document domain and emit one canonical metadata owner plus searchable sections"
  - subtask_ref: projection-and-cache-preservation
    title: "Retain validated memory metadata through existing navigation details, staged storage and cache paths; invalidate extraction semantics when required"
  - subtask_ref: cold-delta-removal-parity
    title: "Prove edit, status change, malformed replacement and deletion remove obsolete advice candidates without a separate registry"
```

Proof slice: `tests/acdd/memory/projection.rs::memory_projection_round_trip`.
Use the real build/delta/cache pipeline with one base workspace outside the case
loop; inspect canonical stored nodes, sections and digests after batched mutations.
Sensitivity: strip `memory` metadata at document-node construction or navigation
serialization; cold/cache/delta parity assertions fail. Incoming: core parser.
Outgoing: canonical document-node metadata consumed by the resolver. No task-state
schema migration, duplicate memory table, global strict docs parser, or new crate.

### task: evidence-grounded-synthesis

```yaml
task_ref: evidence-grounded-synthesis
target: "Collect bounded subject evidence from existing task and live run observations and validate useful, provenance-bound, deduplicated or explicitly superseding memory candidates with a successful empty-result path"
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - upfront-contract-synthesis-and-dag-prioritization
scope:
  - crates/acdd-tasks/src/memory/synthesis.rs
  - crates/acdd-tasks/src/store/mod.rs
  - crates/acdd-tasks/src/store/trace.rs
  - docs/reference/repository-memory.md
  - tests/acdd/memory/synthesis.rs
  - tests/acdd/fixtures/memories/synthesis/
symbols: []
status: planned
subtasks:
  - subtask_ref: subject-evidence-selection
    title: "Read existing findings, transitions and decisions/notes by canonical subject and normalize live Plan/Workflow failure observations without task fabrication"
  - subtask_ref: candidate-admission-and-noise-rejection
    title: "Validate packet provenance, bounded rules/signals and explicit supersession; expose a reviewable zero-candidate no-op"
  - subtask_ref: deterministic-deduplication
    title: "Normalize fingerprints, preserve unchanged timestamps, and reject cyclic, dangling or conflicting candidate relationships"
```

Proof slice: `tests/acdd/memory/synthesis.rs::evidence_grounded_candidate_matrix`.
Use in-process TasksStore/TaskHarness transitions and typed live observations,
with one seeded store outside homogeneous case loops. Include rejected gates,
command failure, selected decision/notes, unrelated subjects, unavailable history,
forged evidence, repeated candidates and supersession cycles. Quality review uses
paired useful/noisy fixtures; mechanical checks bind evidence rather than claiming
semantic understanding. Sensitivity: admit a candidate with a foreign evidence ID
or drop rejected attempts and the matrix fails. No writes to source/index here;
the sequential lifecycle backend owns publication and cross-file retry behavior.

### task: deterministic-resolver-and-memory-lifecycle

```yaml
task_ref: deterministic-resolver-and-memory-lifecycle
target: "Implement the single deterministic bounded resolver and source-first list/show/check/synthesize/prune backend with anchor staleness, digest conflict protection and resumable supersession"
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - memory-document-projection
  - evidence-grounded-synthesis
scope:
  - crates/acdd-tasks/src/memory/mod.rs
  - crates/acdd-tasks/src/memory/resolver.rs
  - docs/reference/repository-memory.md
  - docs/architecture/repository-memory.md
  - tests/acdd/memory/resolver.rs
  - tests/acdd/fixtures/memories/resolver/
symbols:
  - fn:memory
status: planned
subtasks:
  - subtask_ref: explicit-intersection-and-ranking
    title: "Resolve confined paths, unique symbols and exact gate IDs through one stable ranker with top-K, serialized-byte and computation bounds"
  - subtask_ref: anchor-validity-and-effective-staleness
    title: "Suppress all broken-anchor or unavailable advice against the current inventory and distinguish declared status from effective stale status"
  - subtask_ref: source-first-maintenance
    title: "Implement dry-run/apply operations, optimistic digest checks, atomic per-file writes and deterministic interrupted-supersession retry without a SQLite authority"
```

Proof slice: `tests/acdd/memory/resolver.rs::deterministic_resolution_and_lifecycle`.
One indexed corpus exercises path/symbol/gate unions, empty anchors, ambiguous
symbols, moved/deleted anchors, gate-only rules, stable ties, Ukrainian/CJK/emoji,
JSON escaping, five-item and 4096-byte boundaries, overflow, unavailable coverage,
source drift, stale persistence, preview/apply and interrupted replacement retry.
Use distinct domain tests only for an orthogonal publication failure seam.
Sensitivity: bypass anchor checks, reverse the tie-break, ignore serialized
escaping, or reactivate stale advice; owning assertions fail. Incoming: projection
and evidence candidates. Outgoing: one facade and crate-internal resolver, no new
public library helper. No auto-commit, source mutation on read, or active prune.

### task: universal-gate-context-and-closure-evidence

```yaml
task_ref: universal-gate-context-and-closure-evidence
target: "Consume the same compact memories once in Milestone, Plan and Standalone Workflow gate contexts and capture their real failed attempts for explicit or configured closure synthesis without changing persistence, proof or contract documentation"
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - deterministic-resolver-and-memory-lifecycle
scope:
  - crates/acdd-tasks/src/engine/context.rs
  - crates/acdd-tasks/src/engine/orchestrator/role.rs
  - crates/acdd-tasks/src/engine/orchestrator/plan.rs
  - crates/acdd-tasks/src/engine/orchestrator/workflow.rs
  - crates/acdd-tasks/src/engine/orchestrator/task_runner.rs
  - crates/acdd-tasks/src/engine/orchestrator/spawner.rs
  - crates/acdd-tasks/src/engine/orchestrator/waves.rs
  - crates/acdd-tasks/src/engine/lifecycle/root_gates.rs
  - crates/acdd-tasks/src/memory/synthesis.rs
  - src/cli/workflow.rs
  - docs/adr/0016-lean-stage-tailored-task-context-bundles.md
  - docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md
  - docs/reference/acdd.md
  - docs/reference/tasks.md
  - docs/reference/cli.md
  - tests/acdd/memory/consumption.rs
  - tests/acdd/fixtures/memories/consumption/
symbols:
  - fn:run_plan_workflow
  - fn:run_standalone_workflow
  - fn:run_standalone_workflow_with_scope
  - enum:WorkflowCommand
status: planned
subtasks:
  - subtask_ref: equal-stage-context-consumption
    title: "Wire the shared resolver into claim/context and actual Plan/Workflow prompt construction, preserving minimal claims, stage context and landed spec projections"
  - subtask_ref: live-failure-evidence-bridge
    title: "Retain bounded rejected/failed observations before retries and terminal returns so in-run synthesis receives authentic evidence"
  - subtask_ref: closure-and-persistence-boundaries
    title: "Expose synthesis input before pruning/binding and add explicit confined Workflow invocation scope while preserving milestone commit, Plan snapshot and Workflow taskless semantics"
```

Proof slice: `tests/acdd/memory/consumption.rs::all_execution_surfaces_consume_memories`.
Compare equal normalized inputs through real claim/context, Plan stage projection,
and workflow runner prompt capture; test both default and configured templates.
Exercise command rejection/repair and terminal failure with existing runner
fixtures only where actual prompt/process behavior is under test. Assert memory
appears exactly once, does not displace contract/spec/findings, respects the entire
envelope budget, and disappears on anchor deletion. Inspect task rows and Git
effects for the three persistence modes. Sensitivity: remove any one caller's
resolver call or discard a failing observation and the corresponding row fails.
If AC-004 is landed, refresh the Plan call path inside this scope and use its
TasksStore/task runner; do not add a Plan-only fallback engine.

### task: cli-mcp-and-synthesis-workflow-assets

```yaml
task_ref: cli-mcp-and-synthesis-workflow-assets
target: "Expose identical CLI/MCP memory inspection and explicit maintenance, report effective staleness during scan, and document executable memory_synthesis composition after docs_sync for each subject shape using external assets"
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - universal-gate-context-and-closure-evidence
scope:
  - src/cli/mod.rs
  - src/cli/memory.rs
  - src/mcp/tools.rs
  - src/mcp/tools/inputs.rs
  - src/mcp/metadata.rs
  - src/core/format/mod.rs
  - src/core/format/lifecycle.rs
  - crates/acdd-core/src/subsystems.rs
  - docs/reference/cli.md
  - docs/reference/mcp-tools.md
  - docs/reference/configuration.md
  - docs/reference/profiles.md
  - docs/reference/README.md
  - docs/reference/repository-memory.md
  - docs/workflows/README.md
  - docs/workflows/memory-synthesis.workflow.yaml
  - skills/acdd/SKILL.md
  - skills/acdd/references/tool-catalog.md
  - skills/acdd/references/acdd_profile.yaml.example
  - skills/acdd/references/acdd-profile-examples/README.md
  - skills/acdd/references/acdd-profile-examples/profile-milestone.example.yaml
  - skills/acdd/references/acdd-profile-examples/profile-plan.example.yaml
  - skills/acdd/references/acdd-profile-examples/profile-workflow.example.yaml
  - skills/acdd/references/acdd-profile-examples/memory-synthesis.workflow.yaml
  - tests/acdd/memory/surfaces.rs
  - tests/acdd/fixtures/memories/surfaces/
symbols:
  - enum:Command
  - route:memory_manage
status: planned
subtasks:
  - subtask_ref: dual-surface-memory-operations
    title: "Route strict typed CLI/MCP requests through memory(root, request) with shared pagination, worktree resolution, Markdown/JSON output and apply guards"
  - subtask_ref: scan-and-anchor-diagnostics
    title: "Invoke the same read-only check from scan, report incomplete coverage explicitly and preserve ordinary scan and document retrieval behavior"
  - subtask_ref: external-closure-workflows-and-skills
    title: "Add validated examples for milestone pre-binding synthesis, tasks-only Plan closure, taskless Workflow synthesis after docs_sync, and batch memory-dream consolidation; keep runner configuration explicit and assets outside the binary"
```

Proof slice: `tests/acdd/memory/surfaces.rs::memory_cli_mcp_operation_parity`.
Exercise production command dispatch and in-process MCP on one materialized
workspace, with a table of operations, invalid inputs, disabled modules, alternate
worktree roots and dry-run/apply outcomes. Prove full memory content is retrievable
through `acdd_read`. Parse all changed examples with the real profile validator
and execute representative graphs using configured test runners. Explicit memory
source writes require admitted `docs/memories/` scope, supplied by the preceding
task's explicit Workflow invocation scope; no inference from free-text goals or
new hardcoded artifact-to-directory mapping is authorized.
Sensitivity: diverge an MCP action from its CLI backend or let scan mutate a source;
the matrix fails. No embedded template registration or `.acdd/profile.yaml` edits.

### task: cross-surface-memory-acceptance

```yaml
task_ref: cross-surface-memory-acceptance
target: "Prove the complete evidence-to-memory-to-next-gate path on Milestone, Plan and taskless Workflow surfaces, including retries, stale anchors, source authority and unchanged lifecycle proofs, then prepare the final documentation scan packet"
proof_policy: seam-test-first
contract_revision: 1
depends_on:
  - cli-mcp-and-synthesis-workflow-assets
scope:
  - tests/acdd/memory/integration.rs
  - tests/acdd/fixtures/memories/integration/
  - docs/milestones/AC-006-universal-repository-memory-synthesis.md
status: planned
symbols: []
subtasks:
  - subtask_ref: cross-surface-production-round-trip
    title: "Run observed rejection/decision through synthesis admission and source publication, rebuild the projection, and assert the next applicable gate consumes the same bounded rule on all three surfaces"
  - subtask_ref: lifecycle-and-recovery-acceptance
    title: "Prove empty synthesis, repeated synthesis, supersession retry, deleted anchors, index rebuild and clear/handoff behavior without new registries or Workflow task rows"
  - subtask_ref: documentation-scan-evidence-packet
    title: "Bind final diff, manifest and accepted task evidence for the coordinator-launched documentation scan and independent integration review"
```

Proof slice: `tests/acdd/memory/integration.rs::repository_memory_end_to_end`.
One table drives the three genuine execution surfaces over a shared indexed base;
use distinct mutable workspaces only where persistence semantics require it.
Observe rejected evidence, admitted Markdown, SQLite projection and a subsequent
gate's rendered context. Delete/move an anchor and observe suppression on the real
consumer path. Erase/rebuild the docs projection and prove identical results from
source. Clear or archive execution state and prove curated source remains. A
`save: none` run leaves zero task rows/checkpoints/commits. A missing projection
cannot silently serve stale advice. Sensitivity: bypass one production producer
or consumer, route a foreign subject, or preserve a stale candidate; the same
test fails. Source defects route to the owning task rather than being patched
under this verification-only scope.

## Acceptance and verification

| Criterion | Observable production proof | Owning task |
| --- | --- | --- |
| AC-01 | Strict, bounded Markdown parses; ordinary docs keep their contract | Task 0, projection |
| AC-02 | Cold, cache and delta yield the same canonical memory projection; deletion removes it | Projection |
| AC-03 | Rejections/decisions/failures bind candidates; noisy or foreign evidence is rejected; empty synthesis is a no-op | Synthesis |
| AC-04 | Identical scope/symbol/gate inputs give stable ranked K=5, <=4096-byte context on all three surfaces | Resolver, consumption |
| AC-05 | Moved/deleted/ambiguous anchors suppress advice; unavailable coverage never asserts deletion | Resolver, surfaces |
| AC-06 | CLI/MCP list/show/check/synthesize (`--subject` and `--dream`)/prune share semantics and explicit apply; acdd_read pulls full content | Surfaces |
| AC-07 | docs_sync remains live documentation; per-run synthesis precedes final binding and batch `memory-dream` consolidates tombstones off the hot path while retaining subject persistence and review/proof | Consumption, integration |
| AC-08 | Supersession/retry and index rebuild preserve source authority; clear/handoff preserve curated files | Resolver, integration |
| AC-09 | No new dependencies, protected infrastructure edits, duplicate registries, root test binaries or leaked test tempdirs | Task 0 and final diff review |

Each task's blueprint slice records implementation steps, exact signatures,
incoming/outgoing seams, test seam and negative boundaries. Task 0 posts the
matrix as a milestone Blackboard decision before downstream work. Each contract
gate compiles and fails at runtime on its named seam, then build proves green
through `acdd task verify <task_id> --test <test_path::test_fn>` and strict Clippy
as required by [TESTS.md](../../TESTS.md). Later consumers get new production-seam
contracts; review repair adds at most a missing row, with zero new test functions.

Use `PersistedSnapshot` for shared read-only indexes, in-process TasksStore for
state/evidence matrices, existing PlanHarness/WorkflowHarness for materialized
runtime shapes, and existing process harnesses only for actual runner contracts.
Create no top-level `tests/*.rs`, static Workspace/TempDir, per-row cold builds,
mock collaborator chain, manifest-substring test, or benchmark requirement.

Before implementation admission, run the read-only manifest validator and, at the
authoring workflow's preflight gate, the canonical initialization command:

```bash
acdd validate docs/milestones/AC-006-universal-repository-memory-synthesis.md
acdd milestone init docs/milestones/AC-006-universal-repository-memory-synthesis.md --in-place
```

Initialization belongs to preflight and synchronizes operational task state; it
does not authorize executing this planned milestone during authoring. Before
handoff, the coordinator launches the required documentation agent with the final
diff, manifest, accepted evidence and explicit documentation scope. Reconcile
architecture, ADRs, references, workflows, skills and examples against live code;
route source findings to owners, independently review updates, and repeat affected
scans after implementation changes. Run the final compound gate from TESTS.md and
the separate `cargo test -p acdd-tasks --lib`, recording actual counts. No benchmark
is authorized. Preserve accepted proofs, final content binding, independent review
and normal milestone archival/landing rules.

## Negative boundaries and deferred owners

No vector database, embeddings, fuzzy retrieval, automatic global advice,
user-level memory service, generated diff diary, hidden scheduling hook, or new
SQLite evidence ledger is admitted. No mandatory corpus is bundled in the binary.
No requirement depends on the inspector UI, a remote service, a model vendor, or
unmerged AC-004/AC-005 manifest paths. Existing adapters configure docs roots;
an unadmitted memory directory is reported rather than silently rewriting config.
Historical stateless workflow evidence that was never retained is unavailable;
recovering it belongs to a future explicitly admitted persistence capability.
Semantic rule conflicts are resolved through curated source/review, never by a
new authority hierarchy or a resolver guessing which architecture to enforce.
