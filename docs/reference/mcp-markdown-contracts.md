---
title: "Markdown and JSON presentation contracts"
doc_type: reference
---

# Markdown and JSON presentation contracts

[ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md)
owns the output decision.
[Milestone 054](../milestones/archive/054-markdown-first-dual-surface-and-agent-ergonomics.md)
owns implementation and acceptance.

## Shared format selection

MCP tools accept optional `format: "markdown" | "json"`. CLI commands accept
global `--format markdown|json`. Both default to Markdown and reject unknown
formats before running a query or mutation. Serve mode continues to use JSON-RPC
framing; this flag changes operation content, not the transport protocol.
Every registered MCP tool advertises the same optional `format` selector with
the exact string enum `markdown` and `json`; clients can omit it to receive the
default. The shared CLI/MCP seam tests compare omitted and explicit Markdown,
then compare parsed JSON for operations with CLI equivalents. `acdd_read`
shares its reader and bounded renderer with CLI `read`; core text/export tests
compare parsed JSON across both transports. See
[`stdio_compact_navigation_preserves_symbol_and_page_contracts`](../../tests/mcp/context.rs)
and [`mcp_exposes_and_executes_evolution_tools`](../../tests/mcp/tool_evolution/cli_and_relations.rs).
For every shared operation, route equivalent validated input and semantic data
through the same formatter. Compare Markdown content exactly and JSON after
parsing. Normalize only transport framing, trailing newline, transport-specific
worker identity, and values that actually changed between requests.
MCP graph operations may append a top-level server-owned `freshness` envelope
after the callback in [`read_with_adapter`](../../src/mcp/server.rs#L442-L479).
Dedicated discovery Markdown may omit inactive matched freshness display metadata
while JSON retains the actual envelope under the existing compaction policy.
Meaningful freshness notices remain visible. Parity checks separately assert the
exact emitted envelope or its permitted absence, then normalize only that exact
top-level block before comparing domain Markdown or parsed JSON. Preserve nested
and arbitrary domain freshness fields.

JSON retains structured domain values. Compaction applies only to explicitly
recognized operation envelopes. Preserve SQL rows and arbitrary checkpoint content
as opaque user data, including keys named `generation`, empty arrays, zero, and
false. Keep existing semantic error codes and validation behavior.

The first formatter task wires every public input/schema/argument allowlist and
response boundary, including inputs without PageInput (tasks, guide, checkpoint).
A generic readable rendering preserves fields for domains whose dedicated
presentation arrives in later tasks. Domain formatter tasks then replace that
presentation without changing the semantic result.

## Metadata and continuation

At final projection, omit inactive generation/freshness display hashes and empty
optional presentation sections. Preserve meaningful counts, empty-result outcomes,
false completeness flags, diagnostics, and requested detail. This is not a
recursive field-name filter.

Every active collection has a usable continuation hint containing its original
request scope, next offset, and accepted generation. Independent nested pages
remain independently recoverable. Source previews use `source_offset` and
`generation`, even when all graph collections are complete.
Generation is an opaque accepted token, not necessarily a 64-character string.
A Git-change generation binds the change view and must not be replaced with the
index generation.

Apply byte-bound trimming before deciding which tokens are inactive. If trimming
turns a complete page into a continuing page, emit its token and next offset.
Never skip a removed tail item. Preserve existing single-oversized-item errors,
source bounds, timeout recovery, and wire-size enforcement. Markdown escaping
and JSON-RPC escaping count toward the 96 KiB maximum response budget; a configured
lower response limit remains authoritative. If a `acdd_state` list tail is removed, update
`returned`, `byte_omitted`, `has_more`, and the actual `next_offset`, retaining the
generation and effective filter hint. Omit `next_offset` on the final page.

Durable gate/content/candidate hashes and receipts are evidence, not display
metadata. Run and gate state payloads retain all data admitted by their operation.

## Task lifecycle

`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, and `acdd_blackboard`
(and their flat CLI equivalents `status`/`list`/`show`/`next`, `claim`,
`verify`, `submit`, `blackboard`) share `src/format/lifecycle.rs` and
`src/format/coordination.rs` Markdown formatting:

- **Claims (`ClaimResp`)**: `render_claim_markdown` delegates directly to
  [`render_claim_prompt_markdown`](../../crates/acdd-state/src/context.rs) so
  MCP responses, CLI `claim`, and role-runner stdin share one canonical prompt
  renderer across all 6 budgeted tiers (`contract` + `guidance`,
  `unresolved_findings`, `pre_task_slice`, `dependency_receipts`, `blackboard`,
  and `context_lenses`), plus the context-reset warning when
  `context_reset_required = true`.
- **Verification, submission, state queries & blackboard**: `render_verify_markdown`,
  `render_submit_markdown`, `render_state_command_markdown`,
  `render_state_query_markdown`, and `render_blackboard_markdown` project
  canonical typed records through `src/format/mod.rs::render_markdown`,
  preserving run/node/claim IDs, claim revisions, findings, candidate trees,
  durable receipts, and Prometheus metrics text. `CallPluginTool` delegates to
  `render_plugin_tool_markdown` (Markdown verbatim or JSON rendered via
  `render_markdown`).
- **List paging**: `acdd_state(action: "list")` / `acdd list` pages active runs
  (and optionally `run_tombstones` via `include_tombstones: true`) with `limit`
  (1–100), `offset`, and `runs.generation`; nonzero offsets require the prior
  generation pin.

Do not substitute hardcoded gate lists, invented `--proof` flags, or hand-built
commit rules; follow the returned `ClaimResp.guidance` and declared command /
evidence schemas.

## Code discovery

### Inspect and source snippets

Render symbol name/kind, selected path and actual declaration line, signature,
container when verified, docstring when requested, governing invariants/ADRs,
verified documentation links, and available relation categories. Preserve the
distinction between verified documentation and ambiguous candidates. Empty
optional relation headings are omitted.

Render verified DocGraph governing rules for inspect and impact in one
`> [!IMPORTANT] Governing Invariants` callout, with a bullet for each supplied
`INV-*` rule and its source link. Explain uses the same presentation when verified
governing rules are supplied. Omit the callout when empty. Preserve direct versus
one-hop provenance, existing omitted counts/source references and recovery;
do not add presentation truncation or promote candidate documentation to rules.
These bounded navigation summaries never replace complete task authority.

Render requested source in a language fence with actual line numbers and the
producer's range/truncation state. Honor `show_source`, `show_doc`,
`max_body_lines`, `leading_lines`, `source_offset`, `detail`, and
`include_coverage`; do not rename `show_source` to `include_source`.
A source continuation includes the selected symbol and accepted generation.
Source-disabled output still includes the available signature.

### Search

Render a pattern/count heading and numbered symbol entries with name links,
actual coordinates, kind, visibility/container when supplied, signature,
production/test classification, first docstring sentence, exact-match reason,
and verified graph pulse (callers, covering tests, implementation relations).
Render linked invariants and ADRs only from verified metadata.

Search enrichment missing from compact baseline results is produced by the
discovery owner through bounded existing graph/query seams. Do not infer
production/test status, seam coverage, exact matches, or traits from display names.
Preserve existing exact-priority ranking, grouping, preview bounds, and pagination.
An empty result states the request and offers a supported narrower/wider search
step; it does not claim the workspace contains no such symbol beyond index scope.

### Overview

Render selected workspace root, actual worktree/branch when supplied, graph
counts, language counts, compiled profiles, freshness warnings, documentation
hubs, and active milestone progress. Use `state.sqlite` (`acdd_state`) and manifest authority for
milestone state and configured admitted documentation roots for hubs.

Resolution health is `resolved / (resolved + unresolved + ambiguous) * 100`.
Aggregate the matching coverage scope consistently; exclude external imports.
Use unavailable when the denominator is zero, not an invented 100%.
Do not describe all nodes as code symbols or combine an aggregate unresolved
count that already includes ambiguity with ambiguous counts a second time.

Add granular counts using the same scoped coverage records and canonical
`diagnostic_reason` classification as exact-file diagnostics. The four 053
reasons are `dynamic_callback`, `unresolved_receiver_type`, `framework_magic`,
and `external_unindexed`; preserve `ambiguous_candidates`, unknown reasons and
recorded status separately rather than forcing every record into those four.
Produce these aggregate reason counts with bounded full-scope aggregation, not
totals from a visible page.
If a count cannot be established within the selected scope/budget, render
`unavailable` with the cause. Emit zero only when the producer proves zero.
Keep coverage status and diagnostic reason as separate dimensions: each reason
retains its recorded `resolved`, `unresolved`, `ambiguous`, or `external` status.
Unknown reasons are counted only for diagnostic rows without a canonical reason;
external rows stay outside both the health denominator and non-external status
subtotals.

| Requested label | Supported meaning and unit |
| --- | --- |
| External crates | Show `external_imports` as external **references**; package/crate count is unavailable without manifest-backed identity aggregation |
| Dynamic callbacks | Records whose canonical reason is `dynamic_callback`; do not merge unresolved receivers |
| Framework magic | Records explicitly classified `framework_magic` by persisted evidence |
| Unindexed | Show `external_unindexed` diagnostic **references**, retaining their status; this is not a census of unindexed files/packages |

External-status references stay outside the health denominator and outside the
unresolved/ambiguous reason subtotal. An external-status record can itself carry
`external_unindexed`; its reason does not reclassify its status. Keep the
`unresolved_receiver_type` count visible as its own category.

Discover actual Cargo crates, Python project/Poetry packages, npm packages and
npm/pnpm/yarn workspaces, and Go modules from their manifests. Entries contain
declared name, ecosystem/unit, selected workspace-relative root and manifest
path, plus declared description and workspace members when present. Directory
components stay visibly distinct from manifest-backed packages. The adapter
adds absolute file URIs only after resolving the guarded paths against the
selected root. Respect configured roots, ignore policy, file size bounds,
linked-workspace ownership, and symlink guards; do not traverse outside
admitted roots. Malformed or unsupported manifest data produces an explicit
bounded diagnostic or unavailable inventory rather than a fabricated package.
Diagnostics are capped by the response limit and report exact totals only after
a complete bounded scan. Component pages retain continuation under the shared
response policy.

Documentation hubs link admitted documents and their indexed section titles
and actual lines. Active milestone rows come from the canonical `state.sqlite` / `acdd_state`
listing and retain id, title, status, and completion. Link a milestone document
only when its real coordinate is available; otherwise say that its document
coordinate is unavailable rather than inferring a path from its id.

### AST search

Render pattern/language/count, each actual file range, source snippet, captures,
and enclosing symbol when provided. Preserve exact/lower-bound totals,
`computation_truncated` warnings and active continuation; do not render
incomplete computation as exhaustive search.

CLI AST limits 1..100 use the same `matches` page object as MCP and share
Markdown/parsed-JSON parity for equivalent requests. CLI limits 101..10000 keep
the aggregate `matches` array, `limit`, and `truncated` result; this larger
CLI-only range is outside shared page parity.

Unsupported-pattern diagnostics retain the producer's code, pattern span when
available, language, reason, supported examples, and next step in a caution
callout. Use `AstSearchCapabilities.valid_examples` and
`diagnostic_hint` from the selected profile, not Rust examples for every
language. Unavailable source spans are not invented.

## Traversal and diagnostics

### Explain and impact

Explain renders the selected signature/docstring, governing rules and verified
documentation, inbound/outbound relation groups, available call sites and types,
coverage caveats, and source continuation. Architectural or public-boundary
badges require producer evidence; signatures alone do not establish a boundary.

Label an edge `[proven call]` only when its actual kind is `calls` with exact
evidence. Label `possible_calls` as `[heuristic / candidate]`, retaining kind,
confidence, ambiguity and incomplete-coverage information. Use the real edge
path/line for `file:///...#L...` call-site links when supplied; a declaration
coordinate is not a call-site coordinate. Dynamic/name/lexical matches and
structural relations never acquire a proven-call badge. Deeper paths with only
`via` IDs/edge kinds keep their available evidence without invented locations
or a claim that every edge on the path is exact.

Impact renders requested direction/depth/mode, target, depth-grouped reached
nodes, actual call/edge coordinates, available docstrings and classification,
governing invariants, affected documents, and incompleteness. An empty traversal
reports no indexed matching dependencies within the selected bounds; it does not
prove an unreferenced terminal leaf or safe removal.

Traversal and diagnostics Markdown omits sections and empty-page notices for
zero-item pages. It retains compact omission stubs and continuation evidence
when the producer reports a positive total, even if the current page has no
items.

Git-change impact supports existing `changes: staged|unstaged|head`, exclusive
with selector. Render `changes.files`, `changed_symbols`, `nodes`,
`tests`, `unmapped_changes`, and `complete`. Preserve unmapped reasons,
rename/deletion details, query bounds, and restart guidance when the Git view
changes. Affected tests are indexed evidence, not a claim that executing them
is sufficient proof. Keep the change fingerprint needed for each nested page.

### Tests

Render the selected symbol and direction, verified `test_role`, discovery
method, unresolved-reference caveats, and `connection.relation` /
`connection.call_path`. Lexical fallback remains explicitly unverified and
does not acquire a graph call path. Inbound results are verified test cases;
helpers can appear on the path. Outbound results are production dependencies.

Render verified `connection.call_path` in its producer order as a compact arrow
breadcrumb: test case -> intermediate helpers -> selected target. Link each
node using verified symbol coordinates, annotate `test_case`/`test_helper`/
`test_fixture` only when supplied by the role producer, and mark the selected
target explicitly. The baseline path carries IDs; the traversal task attaches
coordinates and roles only to returned path IDs from its bounded graph walk.
Missing roles or coordinates remain unavailable; names never establish either. Retain the
graph/lexical discovery method, unresolved caveat and every page continuation.

The baseline has no `positive_seam` / `negative_seam` intent producer.
By explicit user decision, intent semantics, producer and proof remain unresolved
for post-054 architecture / next planning. M054 renders verified roles and call
paths without adding intent inference or a declaration producer. Do not fabricate
intent badges from names, docstrings, assertions, or roles.

### Analyze, queries, and removal

Render existing analysis and syntax diagnostics with their category, actual
source/document coordinate, evidence, and supported remediation. Preserve
untested public-surface and documentation-drift result semantics, diagnostic
limits, candidate-versus-verified distinction, and selected path scope.
Use actual `target` arguments: an omitted target is not an admitted
`code_map_analyze()` example.

Generic graph queries, SQL results, and removal proof share core rendering and
parity. Preserve SQL values and removal uncertainty, including fail-closed
assessment, unresolved references and parse errors. Markdown changes no query
authorization or removal proof semantics.

## Coordination and documentation

Blackboard supports `read` and `post` (`acdd_blackboard` / `acdd blackboard`);
topics are `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred`,
with statuses `open`, `acknowledged`, `resolved`, and `carried_over`.
Preserve topic, message identity, source/recipient node cursor, subtask ref,
gate, author, timestamp, and payload. Render each `topic: blockers` message as
`> [!CAUTION] BLOCKER (#<id>) by <author>`, followed by scope, gate, timestamp,
and the supplied message body. Other topics remain visibly distinct and do not
acquire blocker severity. Preserve complete IDs/authors/gates and body content;
when a claim bundle bounds long bodies, mark the summary and retain
`acdd_blackboard(action: "read", ...)` full-body recovery. Preserve every
read-page `runs.generation` continuation.

Documentation search renders path/section/type and an excerpt only when supplied.
Get-document uses `path_or_id` and the actual section selector, renders source
Markdown or a compact outline according to detail, and retains the actual page
continuations for each returned collection. Link a document or symbol coordinate
only when the selected workspace adapter resolves its path; linked-workspace links
use that workspace's resolved URI. Keep verified citations and referenced symbols
separate from unverified ambiguous candidates. Render actual invariant metadata
without treating it as a `governs` edge; candidate ambiguity remains navigation,
not verification or governance. Compact output shows the documentary-candidate
omission hint once, while full detail retains candidate evidence. Each verified
referenced-symbol row retains its citing section as `doc_id` and citation edge
coordinate as `citation_line`; `line` identifies the target declaration and
drives its resolved URI anchor. Preserve both coordinates and their independent
pagination in JSON and Markdown. Omit empty
relation headings unless the page is an intentional compact omission stub.

## Producer and consumer bindings

| Surface | Live producer / adapter | Presentation consumer |
| --- | --- | --- |
| Format and compaction | Shared `src/format/` | `src/mcp/response.rs`, CLI stdout |
| Request validation | `src/mcp/tools/inputs.rs`, `src/mcp/tools.rs`, `src/cli/mod.rs` | All existing shared operations |
| Symbols and source | `crates/acdd-index/src/db/symbols.rs`, `crates/acdd-index/src/db/symbols/` | Inspect/search/snippet Markdown and JSON |
| Overview | `crates/acdd-index/src/db/reader.rs`, scanner/config roots and `src/cli/discovery` (`active_runs`) | Overview Markdown and JSON |
| Resolution reasons | `crates/acdd-index/src/db/reader.rs` canonical `diagnostic_reason`; discovery supplies same-scope bounded aggregate counts | Overview reason breakdown, with external reference counts separate |
| Ecosystem manifests | Discovery component producer | Overview component entries |
| AST | `src/cli/ast.rs`, `crates/acdd-index/src/ast/`, `crates/acdd-index/src/languages/` | AST results and language-specific alerts |
| Relations / Git impact | `crates/acdd-index/src/db/traversal.rs`, `crates/acdd-index/src/db/symbols/test_discovery.rs` | Explain/impact/tests/query/removal |
| Test breadcrumbs | `crates/acdd-index/src/db/symbols/test_discovery.rs` hydrates only returned `connection.call_path` IDs; `src/format/traversal.rs` renders them | Linked ordered test -> helper -> target breadcrumb |
| Diagnostics / docs | `crates/acdd-index/src/db/reader.rs`, `crates/acdd-index/src/docs.rs` | Analyze/search-doc/get-doc |
| State & Lifecycle | `crates/acdd-state/src/`, `src/state_bridge/`, `src/format/lifecycle.rs` | `acdd_state`/`acdd_claim`/`acdd_verify`/`acdd_submit`/`acdd_blackboard`, flat CLI, `acdd run` |
| Blocker alerts | Canonical `blockers` topic, stored blackboard rows and existing claim summaries | Standalone blackboard and embedded `ClaimResp` callouts |
| Guide & Init | `src/cli/guide.rs` | `acdd_guide` and `acdd guide` / `acdd init` |
| Test intent | Deferred (no baseline producer) | No inferred badges |

## Presentation examples

### Verified call-path fixture

This illustrative fixture is created by the acceptance test at a temporary root;
`/tmp/acdd-orders-fixture` is its example root. Coordinates below refer to these
exact fixture declarations, not declarations in the ACDD repository.
`src/orders.py` contains:

```python
def ProcessOrder():
    return "processed"
```

`tests/test_orders.py` contains:

```python
from src.orders import ProcessOrder

def setup_mock_driver():
    return ProcessOrder()

def test_order_processing():
    assert setup_mock_driver() == "processed"
```

After the producer verifies both calls and the test roles, render:

```markdown
Discovery: graph — verified exact calls
[test_order_processing](file:///tmp/acdd-orders-fixture/tests/test_orders.py#L6) (test_case) -> [setup_mock_driver](file:///tmp/acdd-orders-fixture/tests/test_orders.py#L3) (test_helper) -> [ProcessOrder](file:///tmp/acdd-orders-fixture/src/orders.py#L1) (target)
```

The test resolves each link against its actual temporary root and verifies the
line/name. A lexical fallback shows `Discovery: lexical_fallback (unverified)`
and has no fabricated breadcrumb. A node without a verified role has no role badge.

### Governing rules and edge evidence

This repository example uses the verified rule for
[`document_symbol_candidates`](../../crates/acdd-index/src/linker/resolution/mod.rs)
and its actual rule source:

```markdown
> [!IMPORTANT] Governing Invariants
> - INV-DOC-FIRST-MATCH-TIER: Candidate resolution returns the first non-empty eligible declaration tier. [Source](file:///home/oleksii/ContextUnity/projects/acdd/docs/AGENTS.md#L92)
```

For the live exact call to `diagnostic_reason`, the edge coordinate differs from
the caller's declaration coordinate:

```markdown
- [annotate_diagnostic_page](file:///home/oleksii/ContextUnity/projects/acdd/src/db/reader.rs#L2135) [proven call] — calls diagnostic_reason at [src/db/reader.rs:2145](file:///home/oleksii/ContextUnity/projects/acdd/src/db/reader.rs#L2145)
```

A `possible_calls` fixture renders `[heuristic / candidate]` and its actual
supplied call-site link; it never reuses this proven-call label.

### Resolution-health fixture

For one fully counted fixture scope with 80 resolved, 15 unresolved,
5 ambiguous and 12 external-status references, render:

```markdown
Resolution Health: 80.0% (80 / 100 non-external references)
Resolved: 80; unresolved: 15; ambiguous: 5
External references: 12 (excluded from health); external crate count: unavailable
Non-external diagnostic references:
- Dynamic callbacks (dynamic_callback): 6
- Unresolved receiver type (unresolved_receiver_type): 4
- Framework magic (framework_magic): 3
- Unindexed external references (external_unindexed, status=unresolved): 2
- Ambiguous candidates (ambiguous_candidates): 5
Unindexed package/file inventory: unavailable
```

The four named unresolved reasons sum to 15; ambiguity contributes five more
records to the denominator. The 12 external references are a separate status
population, not 12 crates or additional unresolved records. An absent reason
aggregate renders unavailable rather than the fixture's numbers.

### Blocker message fixture

The same illustrative message is rendered in standalone blackboard and embedded
claim/contract context; other topics remain in their own topic groups:

```markdown
> [!CAUTION] BLOCKER (#42) by codex-builder
> Scope: task fixture-task; gate: contract; timestamp: 2026-10-21T10:00:00Z
> The verified fixture call path is missing its helper node. Preserve its actual role before submitting the contract proof.
```

For a metadata-only read or an existing bounded summary, retain its exact message
ID and `acdd_blackboard(action: "read", ...)` full-body recovery. The alert
does not assert that a stored blocker has an independently tracked resolution state.

## Proof sensitivity

Use production CLI and stdio seams, real SQLite fixtures, and existing shared
test clients. Test omission/default, explicit Markdown, explicit JSON, invalid
format rejection before mutation, all active continuations, stale-generation
rejection, byte-limited tail trimming, source-only continuation, nested independent
pages, and opaque user content whose keys resemble metadata. Deliberately removing
a required token or operative rule must fail its seam assertion.

Discovery proof counts all fixture records in the selected workspace/path scope,
checks all four canonical 053 reasons plus ambiguity/unknown and separate external
status, and verifies the percentage on a nonzero and zero denominator. Adding
external records must not change health; substituting visible-page totals must
fail. Inspect/impact rule callout tests verify source links, omitted navigation
metadata and preservation of every supplied rule, with no empty callout.
Traversal proof checks the illustrative breadcrumb's order, role provenance and
actual coordinates, and exact `calls` versus `possible_calls`, lexical and
dynamic evidence; upgrading a candidate or dropping a helper must fail.
Lifecycle and coordination proof cover blockers and nonblockers in standalone
read/inspect plus embedded contract claim, including long IDs/authors, multiline
body, gate, existing summary recovery and page continuation. Relabeling a
nonblocker or dropping blocker body/recovery must fail.

Domain tests compare field meaning and real coordinates rather than frozen
invented symbol names or line numbers. Final parity tests cover every registered
tool and its existing CLI equivalent, including error outcomes, without adding
new transports. Run the full affected targets and strict Clippy under
[TESTS.md](../../TESTS.md).
