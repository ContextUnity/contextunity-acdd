---
title: "ADR 0006: Budgeted Gate Context and Dual-Surface Parity"
doc_type: adr
status: accepted
date: 2026-11-17
---

# ADR 0006: Budgeted Gate Context and Dual-Surface Parity

## Status

Accepted.

## Context

AI coding assistants and human/CI terminal operators must observe identical semantics across MCP JSON-RPC and the `acdd` CLI without saturating LLM context windows (`> 82 KiB` historical claim envelopes, unbounded blackboard dumps, or unpaged symbol results) or leaking archived/historical specifications into live documentation governance edges.

## Decision

### 1. Dual-Surface Parity and Markdown-First Presentation

- **Shared Engine & Formatter**: `src/cli/` and `src/mcp/` are thin protocol adapters over `acdd-index`, `acdd-state`, `src/engine/`, and `src/state_bridge/`. Presentation is owned by `src/format/` ([`mod.rs`](../../src/format/mod.rs), [`lifecycle.rs`](../../src/format/lifecycle.rs) delegating `ClaimResp` prompt rendering to [`render_claim_prompt_markdown`](../../crates/acdd-state/src/context/markdown.rs), [`coordination.rs`](../../src/format/coordination.rs)), defaulting to GitHub-flavored Markdown on both surfaces with explicit `--format json` / `format: "json"` parity.
- **Coordinates & Diagnostics**: File, symbol, and document coordinates render as concise repo-relative `path:line` / `path#Lline` labels in compact Markdown (`response.file_uris: false` default) and as workspace-confined `file:///` links when `detail: "full"` or `file_uris: true` is requested. Errors and notices render as `[!CAUTION]`, `[!WARNING]`, or `[!NOTE]` callouts preserving `ErrorCode`, cause, and recovery steps without mutating JSON semantics.
- **Scoped Compaction**: Omit inactive presentation metadata (including no-op `{status: "matched", refresh: "none"}` freshness envelopes in Markdown, zero-count diagnostic rows, and per-hop internal `rs:` IDs) while shortening continuing 64-char hex generation hashes to 8 chars (`>= 8`-char prefix validation) and preserving every continuation token, `ErrorCode`, candidate/content hash, receipt, JSON schema field, and meaningful zero/false field.

### 2. `96 KiB` Response Budget Ceiling and Generational Paging

- [`ResponsePolicy`](../../crates/acdd-core/src/response.rs) enforces `max_output_bytes` in `1_024..=98_304` (`96 KiB` ceiling; smaller configured limits remain authoritative), `page_size` in `1..=100` (default `12`), `source_context.max_body_lines` default `100`, and `file_uris` default `false` so routine compact Markdown navigation responses stay under the `< 4 KiB` inline threshold.
- Serialized UTF-8 content, response metadata, and Markdown/JSON framing all count toward the byte budget.
- Code-map continuations bind `next_offset` / `source_offset` to the snapshot `generation`; `acdd list` and `acdd blackboard` bind continuations to `runs.generation` independently of the code index. Oversized indivisible records fail with explicit recovery guidance rather than silent truncation.

### 3. Six-Tier `ClaimResp` Context Budget Reservation

[`GateContextAssembler`](../../crates/acdd-state/src/context/mod.rs) (backing `build_claim_context` across `claim`, `show`, and `run`) constructs `ClaimResp` within `ResponsePolicy.max_output_bytes` in strict priority order:

1. **Tier 1 (`contract` + `guidance`)**: Mandatory claimed contract (omitting empty `invariants` and default `tools` equal to `profile.default_tools`; sending full spec content on `blueprint_synthesis` or taskless specs and `spec_path` on post-task root gates) and resolved gate guidance entries (reserved inside `max_guidance_bytes`, never silently truncated; fails closed with `GUIDANCE_BUDGET_EXCEEDED` on overflow).
2. **Tier 2 (`unresolved_findings` + failed trace tail)**: Container-sibling (`parent_id == node.parent_id`) structured `RejectionFindings` and tail of the latest failed trace log across reject/fail rewinds, cleared automatically once the rejecting gate passes or the container's newest trace runs/passes.
3. **Tier 3 (`pre_task_slice`)**: Enriched per-task architectural slice (`proof.tasks[<task_ref>]`, including global negative boundaries and cited invariants) or global blueprint slice on post-task root gates, plus `predecessor_proofs` from completed same-container sibling gates emitting `ArtifactKind::Proof` (e.g., `recon` $\to$ `analyze` $\to$ `triage` in `audit`) (skipped alongside Tiers 4–5 on receipt-only `proof: none` gates).
4. **Tier 4 (`dependency_receipts`)**: Accepted receipts of completed dependency tasks, deduplicated to the latest `claim_revision` per dependency with compact coordinates (`node_cursor`, `candidate_tree`, `committed_oid?`, `accepted_at`).
5. **Tier 5 (`blackboard`)**: Up to 15 `Open` blackboard messages (`Findings` and `Deferred` excluded), prioritizing `Decisions` and `Blockers` before filling remaining slots with newest `Notes`, each truncated safely on UTF-8 character boundaries to 500 characters (`MAX_BLACKBOARD_ITEMS = 15`, `MAX_BLACKBOARD_CHARS = 500`).
6. **Tier 6 (`context_lenses`)**: Dynamic `DomainProvider` lenses declared in `gate.context` (`docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`) evaluated in YAML declaration order with proportional per-lens budget hints (`max(remaining / remaining_lenses, min(remaining, MIN_LENS_BUDGET_BYTES / 8))`). Built-in lenses pack relevance-scored candidates with bounded deterministic 0-1 knapsack selection (`pack_knapsack_indices`, `<= 65,536` DP cells with quantized weight scaling and original-rank tie-breaking) so oversized early items never drop smaller high-value fits. Empty `items: []` (`total: 0`) lenses are omitted cleanly; budget-exhausted lenses are recorded in `omitted_sections`, and oversized `items`-array payloads fall back to bounded knapsack packing before omission.

### Archive Exclusion from Documentation Search and Code-Documentation Graph

Documents under any `archive/` directory (`archive/...` or `.../archive/...`, including `docs/milestones/archive/` and `docs/plans/archive/` across primary and linked workspaces) remain indexed for explicit historical retrieval. Default documentation search and overview documentation hubs exclude them. A search `component` or `path` targeting `archive` opts into archive search; direct `get_doc` retrieval remains available.

Archived documents emit no `documents`, `references_doc`, `governs`, or `governed_by` edges. Their historical claims therefore contribute no live `documented_in`, `affected_docs`, or invariant governance. Document-to-document citations remain available. In `DocsDomainProvider` claim context (`docs.adrs`, `docs.invariants`), all indexed non-archive documentation across the repository with `doc_type != 'contract'` is intersected via `governs` and `documents` edges (while `doc_type = 'contract'` specs are delivered through `ClaimResp.contract`). The exclusion belongs to the documentation reader, linker, and domain context provider without SQL views or a separate archive registry.

## Consequences

- `acdd claim` / `acdd_claim` responses remain compact and deterministic within `ResponsePolicy.max_output_bytes` across multi-revision runs.
- UTF-8-safe truncation prevents slicing panics on multilingual text.
- Archived specs stay discoverable via explicit `get_doc` or `archive` filters without polluting live invariant governance or `documented_in` links.
