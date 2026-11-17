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

- **Shared Engine & Formatter**: `src/cli/` and `src/mcp/` are thin protocol adapters over `acdd-index`, `acdd-state`, `src/engine/`, and `src/state_bridge/`. Presentation is owned by `src/format/` ([`lifecycle.rs`](../../src/format/lifecycle.rs), `coordination.rs`), defaulting to GitHub-flavored Markdown on both surfaces with explicit `--format json` / `format: "json"` parity.
- **Coordinates & Diagnostics**: File, symbol, and document coordinates render as workspace-confined `file:///` links with line fragments where known. Errors and notices render as `[!CAUTION]`, `[!WARNING]`, or `[!NOTE]` callouts preserving `ErrorCode`, cause, and recovery steps without mutating JSON semantics.
- **Scoped Compaction**: Omit inactive presentation metadata while preserving every continuation token, `ErrorCode`, candidate/content hash, receipt, and meaningful zero/false field.

### 2. `96 KiB` Response Budget Ceiling and Generational Paging

- [`ResponsePolicy`](../../crates/acdd-core/src/response.rs) enforces `max_output_bytes` in `1_024..=98_304` (`96 KiB` ceiling; smaller configured limits remain authoritative) and `page_size` in `1..=100` (default `30`).
- Serialized UTF-8 content, response metadata, and Markdown/JSON framing all count toward the byte budget.
- Code-map continuations bind `next_offset` / `source_offset` to the snapshot `generation`; `acdd list` and `acdd blackboard` bind continuations to `runs.generation` independently of the code index. Oversized indivisible records fail with explicit recovery guidance rather than silent truncation.

### 3. Six-Tier `ClaimResp` Context Budget Reservation

[`build_claim_context`](../../crates/acdd-state/src/context.rs) constructs `ClaimResp` within `ResponsePolicy.max_output_bytes` in strict priority order:

1. **Tier 1 (`contract` + `guidance`)**: Mandatory claimed specification excerpt and resolved gate guidance entries (reserved inside `max_guidance_bytes`, never silently truncated; fails closed with `GUIDANCE_BUDGET_EXCEEDED` on overflow).
2. **Tier 2 (`unresolved_findings` + failed trace tail)**: Structured `RejectionFindings` and tail of the latest failed trace log after a reject/fail rewind.
3. **Tier 3 (`pre_task_slice`)**: Per-task architectural slice (`proof.tasks[<task_ref>]`) from pre-task blueprint artifacts.
4. **Tier 4 (`dependency_receipts`)**: Accepted receipts of completed dependency tasks.
5. **Tier 5 (`blackboard`)**: Up to 15 relevant blackboard messages, each truncated safely on UTF-8 character boundaries to 500 characters (`MAX_BLACKBOARD_ITEMS = 15`, `MAX_BLACKBOARD_CHARS = 500`).
6. **Tier 6 (`context_lenses`)**: Dynamic `DomainProvider` lenses declared in `gate.context` (`docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`) evaluated in YAML declaration order and trimmed by section (`omitted_sections`) if the remaining byte budget is exhausted.

### Archive Exclusion from Documentation Search and Code-Documentation Graph

Documents under any `archive/` directory (`archive/...` or `.../archive/...`, including `docs/milestones/archive/` and `docs/plans/archive/` across primary and linked workspaces) remain indexed for explicit historical retrieval. Default documentation search and overview documentation hubs exclude them. A search `component` or `path` targeting `archive` opts into archive search; direct `get_doc` retrieval remains available.

Archived documents emit no `documents`, `references_doc`, `governs`, or `governed_by` edges. Their historical claims therefore contribute no live `documented_in`, `affected_docs`, or invariant governance. Document-to-document citations remain available. In `DocsDomainProvider` claim context (`docs.adrs`, `docs.invariants`), all indexed non-archive documentation across the repository with `doc_type != 'contract'` is intersected via `governs` and `documents` edges (while `doc_type = 'contract'` specs are delivered through `ClaimResp.contract`). The exclusion belongs to the documentation reader, linker, and domain context provider without SQL views or a separate archive registry.

## Consequences

- `acdd claim` / `acdd_claim` responses remain compact and deterministic within `ResponsePolicy.max_output_bytes` across multi-revision runs.
- UTF-8-safe truncation prevents slicing panics on multilingual text.
- Archived specs stay discoverable via explicit `get_doc` or `archive` filters without polluting live invariant governance or `documented_in` links.
