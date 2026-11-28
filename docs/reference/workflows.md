---
title: "ACDD Workflows Reference"
doc_type: api
---

# ACDD Workflows Reference

An **ACDD `WorkflowDef` (`version: 2`)** is a single declarative YAML graph that
governs a run: its Git isolation & integration policy (`git:`), persistence mode
(`save:`), tags (`tags:`), roles, commands, domain hooks (`call`, `checks`,
`context`, `tools_add`, `tools_remove`), proofs, guidance slots, review contours,
and ordered verification gates with `pass` / `fail` routing edges.

Workflows use strict schema validation (`deny_unknown_fields`). Legacy or
non-`version: 2` workflows (or those using removed `loop:`, `*_proof` wrappers,
`proof: contract` / `seam_test` / `delivery`, `expected_failure`,
`git.commit_mode`, `git.integrate`, `git_commit_notes`, regex `output:`
scrapers, top-level `specs:` / `roadmap:` keys that belong in `acdd.yaml`, or
legacy `docs.instructions` / `docs.adrs` context lenses — which emit migration
hints to `paths.instructions` / `paths.adrs`) fail closed with
`ACDD_WORKFLOW_SCHEMA_INVALID`.

> [!NOTE]
> Contract and command passes require claim-bound execution observations via
> `acdd verify` / `acdd_verify`. Seam/domain checks (`checks: [code.seam]`) and
> domain actions (`call: <domain>.<action>`) execute through `DomainProvider`
> adapters in `src/state_bridge/`. Headless automated execution is described in
> [CLI commands](cli.md#headless-run-execution).

---

## Workflow Loading Precedence & Embedded Baselines

ACDD resolves the active workflow reference (`--workflow`, `ACDD_WORKFLOW`, spec
`workflow:`, `specs.<name>.workflow`, or the workspace default) as follows:

1. **Built-in embedded baselines** (3 core embedded workflows):
   - `base` ([`base.yaml`](../../crates/acdd-state/src/workflow/embedded/base.yaml)):
     domain-pure worktree-isolated baseline (`gates: []`) with `save: commit`,
     `git.isolation: worktree`, 7 core MCP tools (`acdd_read`, `acdd_guide`,
     `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`),
     universal guidance slots (`core.execution`, `core.review`, `hitl.pro`), and
     three domain-pure review contours (`paths`, `claims`, `administration`).
   - `code` ([`code.yaml`](../../crates/acdd-state/src/workflow/embedded/code.yaml)):
     built-in code-delivery baseline inheriting `base`, adding the three-phase
     milestone lifecycle (`blueprint_synthesis` -> `tasks:` -> `run_tests` ->
     `polish` -> `integration_review` -> `handoff`), `tags: [default, docs]` so
     `docs`-tagged tasks skip `code.seam`, all 18 `default_tools` (including
     `code_map_lint`), additional contours (`architecture`, `concurrency`,
     `project_isolation`), 4-layer clean guidance (`milestone.*` / `task.*`,
     `core.*`, `code.principles`, `code.tools`, `hitl.pro`), `code.seam` +
     `code.lint` checks, `paths.*` / `docs.invariants` / `code.*` context lenses, and automatic
     stripping of disabled `code.*` / `docs.*` hooks when `modules.code: false`
     or `modules.docs: false`.
   - `documentation` ([`documentation.yaml`](../../crates/acdd-state/src/workflow/embedded/documentation.yaml)):
     compiler-free living-documentation and invariant authoring pipeline
     inheriting `base` with `modules: { required: [docs, code] }`,
     `tags: [default, docs]`, roles (`architect`, `builder`,
     `independent_reviewer`), `blueprint_synthesis` -> per-task
     `build -> review -> deliver` -> `verify_docs -> polish -> integration_review -> handoff`,
     enforcing `docs.links` and `code.lint` (`with: { categories: [arch, docs] }`)
     without code-compilation commands.
   - Auxiliary workflows (`audit.yaml`, `spec_planning.yaml`, `hotfix.yaml`, plus `inherit: audit` overlays) ship as ready-to-copy templates under [`skills/acdd/references/acdd-workflow-examples/`](../../skills/acdd/references/acdd-workflow-examples/README.md).
2. **Automatic workspace default**: When no explicit workflow is specified (`--workflow`,
   `ACDD_WORKFLOW`, spec `workflow:`, or `specs.<name>.workflow`), ACDD uses
   `.acdd/workflows/default.yaml` automatically if present in the workspace root, and
   otherwise falls back to embedded `code` (with disabled domain hooks stripped).
   (`in_place` is selected as an execution mode via `--in-place` or
   `git.isolation: in_place` inside a workflow, not as a separate workflow name.)
3. **Explicit repository paths & `workflows_dirs`**: Any relative path confined to
   the workspace (optionally pinned with `:<sha256_prefix>`, min 7 lowercase hex
   chars) or named workflow discovered in `acdd.yaml` `workflows_dirs`
   (default `[".acdd/workflows"]`).

> [!NOTE]
> Linked workspaces (`linked_workspaces`) do not override the parent ACDD
> workflow. Only the active workspace root governs workflow resolution.

---

## Top-Level `WorkflowDef` Schema (`version: 2`)

Every `v2` workflow requires `version: 2` and supports these top-level fields:

```yaml
version: 2
inherit: code                  # Optional: base | code | documentation | none | <rel-path>[:<sha256_prefix>]
git:
  isolation: worktree          # worktree | in_place
  worktrees: manual            # manual | managed
save: commit                   # Default post save action (none | commit)
tags: [default, docs]
paths: {}                      # Named non-spec path globs (e.g. instructions, adrs, docs) exposed as paths.<name>
protected_paths: []
default_tools: [acdd_read, acdd_guide, acdd_state, acdd_claim, acdd_verify, acdd_submit, acdd_blackboard]
modules: {}                    # Optional SubsystemRegistryPatch
limits: {}                     # Optional ResourceLimitsPatch
roles: {}                      # RoleDef / RolePatch map
commands: {}                   # CommandDef map (String | Script | Detailed | ByPattern | Unbound)
scope_args_by_pattern: {}      # Optional path-pattern to {scope_target} CLI arguments map
proofs: {}                     # Reusable ProofCatalogDef JSON Schema registry
guidance: {}                   # Keyed GuidanceEntry slots (string or { description, criteria })
contours: {}                   # Keyed ContourDef review lenses ({ description, criteria })
gate_templates: {}             # Reusable GateTemplate subgraphs expanded via `use:`
gates: []                      # Replace parent gates (mutually exclusive with gates_patch)
gates_patch: []                # Patch inherited gates by id/cursor/after/before/null (requires inherit != none)
```

---

## Inheritance (`inherit:`) and `gates:` vs `gates_patch:`

- `inherit: none` (or omitted): Compiles a standalone workflow from scratch with no
  inherited roles, commands, proofs, guidance, contours, or gates.
- `inherit: base | code | documentation | <path>[:<sha256_prefix>]`:
  - `paths`, `roles`, `commands`, `scope_args_by_pattern`, `proofs`, `guidance`,
    `contours`, `gate_templates`, `limits`, and `modules` merge field-wise / by key
    (supporting `PatchField` null-clearing on nullable fields; `base.yaml`
    provides default `paths` entries for `docs`, `adrs`, and `instructions`).
  - `tags` and `protected_paths` extend or patch inherited sets. When inheriting
    `code`, retain `docs` in `tags:` if `when: "!docs"` gates are used downstream.
  - **`gates:` (replace)**: Replaces the parent's `gates:` list completely when
    declared. When neither `gates:` nor `gates_patch:` is set, inherited gates
    are kept as-is.
  - **`gates_patch:` (patch)**: Mutually exclusive with `gates:` and requires an
    inherited parent (`inherit:` cannot be omitted or `none`). Patches individual
    inherited gates without rewriting the graph:
    - Update a gate by `id` (e.g. `id: polish`) or dotted cursor
      (`id: tasks.build`, `id: blueprint_synthesis.synthesize`), or match a
      `- tasks:` block by normalized `when:` expression.
    - Delete an inherited gate with `- <gate_id>: null`.
    - Insert a new gate at an explicit position with `after: <selector>` or
      `before: <selector>`.

---

## `git:`, `save:`, and `save.report` Policy

- **`git:`**:
  - `isolation`: `worktree` (separate git worktree per task/run) or `in_place`
    (active worktree, single-writer). Passing `--in-place` (or setting
    `git.isolation: in_place` in a workflow) runs directly in the active checkout;
    passing `--in-place` sets `isolation: in_place`, `worktrees: manual`,
    `save: none`, and neutralizes gate `save.post: commit` to `none` while
    keeping `save.immutable: true`. Passing `--worktree` overrides workflow
    `in_place` defaults to `isolation: worktree` and `worktrees: managed`.
  - `worktrees`: `manual` or `managed` (engine creates/removes `.worktrees/*` automatically).
- **`save:`**: Controls working-tree commits, report materialization, and SQLite
  persistence per gate (also settable at workflow root as default or shorthand
  scalar `save: none` / `commit`):
  - `post`: `none`, `commit` (creates an atomic Git plumbing commit with `Acdd-Claim` on submit),
    or `snapshot` (writes `refs/acdd/snapshots/<run_id>/<task_ref>/r<claim_revision>` without
    moving `HEAD`; workflow validation allows `snapshot` only on leaf task gates; checkpoint refs
    auto-clear on deliver, run `clear`, or `handoff`).
  - `immutable`: Require working tree at `submit` to match the claim tree (`TASK_CANDIDATE_MISMATCH` on drift).
  - `archive` / `prune`: Allowed on `root_gate` only (typically `handoff`); archives
    the completed spec or prunes terminal SQLite rows into `run_tombstones`.
  - `report`: Optional normalized workspace-relative template path strictly confined under `.acdd/reports/*.{md,json}` and materialized
    atomically (`checked_child` + `atomic_write`) on `--action pass`, returning
    the expanded path in `SubmitResp.report_path`:
    - Supported template variables: `{run_id}`, `{run_name}`, `{gate_id}`, `{spec_id}` (`"goal"` for ephemeral `--goal` runs). Paths outside `.acdd/reports/*.{md,json}`, paths with `..`, leading `/`, `\`, or unknown `{...}` placeholders fail workflow validation fail-closed.
    - Format: `.json` writes structured pretty JSON (`schema_version: 1`, run/spec/gate metadata, `summary`, submitted `proof`, `predecessor_proofs`, `receipt`, `verified_invariants`, `blackboard`); `.md` renders GitHub-Flavored Markdown with metadata, summary, proof payload, predecessor stage proofs, and blackboard highlights.
    - Destination: `.acdd/reports/` writes under the primary workspace `.acdd/` directory (gitignored by default via `.acdd/reports/`).

---

## Commands and `test_filter`

Each entry in `commands:` is one of:
- **`unbound: true`**: Declared placeholder slot that must be bound by a child
  overlay before any gate referencing it can pass validation (`COMMAND_UNBOUND`).
- **Simple string**: Shell command string.
- **`Script` / `Detailed`**: `{ command, description?, timeout_seconds?, test_filter?, scope_args_by_pattern? }`.
- **`Pattern` (`by_pattern`)**: `{ by_pattern: { "<glob>": ... }, description?, timeout_seconds?, test_filter?, scope_args_by_pattern? }`
  mapping changed/admitted scope globs to language commands via the canonical
  brace-aware glob matcher in `acdd_core::path_policy` (most specific glob wins).
  Top-level `scope_args_by_pattern` propagates as fallback arguments into resolved
  commands (non-placeholder entries match both concrete file globs and
  directory-style scopes such as `dir/`, `dir/**`, or extensionless directory
  paths; `{file_stem}` and `{path}` placeholders expand against concrete
  admitted file paths).

**`test_filter` & `--test`**:
- `test_filter: false` (default on unbound/generic commands if omitted) rejects
  `acdd verify --test <target>` (`COMMAND_TEST_FILTER_UNSUPPORTED`).
- `test_filter: true` appends/substitutes `{test}` using single-argument command
  appending, or `test_filter: "-- {test}"` formats the filter explicitly.
- Chained commands (`&&`, `||`, `;`, `|`) require `{test_filter}` explicitly in
  the command string whenever `test_filter` is enabled
  (`COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED`).

`limits.diagnostic_inline_bytes` defaults to `4096`; raw trace logs live under
`<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`.

---

## Gates, Conditional `when:`, `pass:` / `fail:` Routing, and `DomainProvider`

### Gate Fields & Routing (`pass`, `fail`, `fail_drift`, `need`, `independent_from`)
- `pass`: Explicit sibling gate ID on success (defaults to next sibling gate if omitted).
- `fail`: Target sibling gate ID on `defect` reject/failure.
- `fail_drift`: Optional target sibling gate ID (e.g. `rearchitect`) on `architectural_drift` rejections, clearing cached builder context and rotating runner `ACDD_AGENT_SESSION_ID`.
- `when`: Boolean tag expression (`tag`, `!tag`, `a & b`, `a + b`, `a | b`, `( ... )`)
  parsed and evaluated solely by `acdd_state::workflow::when::WhenExpr`,
  allowed on `- tasks:` blocks and `task_gate` nodes (forbidden on `root_gate`).
  All referenced tags must appear in workflow `tags:` (or `"default"`).
- `max_attempts`: Failure/rejection budget before escalation (default `3`, `>= 1`).
- `independent_from`: Requires a `worker_id` distinct from the accepted worker of named predecessor gates (`TASK_REVIEW_NOT_INDEPENDENT`).
- `need`: Predecessor gate selectors whose accepted candidate `TreeOid` must match at submit.
- `receipt: true`: Marks a terminal gate that records a durable receipt and closes its container.

### Predecessor Gate Proof Propagation (`predecessor_proofs`)
When sequential gates in the same container (e.g., `recon -> analyze -> triage` in
`audit`) emit typed `ArtifactKind::Proof` payloads (`proof: <schema>` or `proof: review`),
`GateContextAssembler` automatically injects completed predecessor sibling proofs into
downstream `ClaimResp.pre_task_slice["predecessor_proofs"]` keyed by `gate_id`
(ordered by `seq_order`, subject to Tier 3 response budgeting).

### Proofs (`proof:`)
Built-in `ProofKind` values are:
- `none`: Default when omitted.
- `command`: Passing command verification (`exit_code == 0`, plus any attached `checks:` such as `code.seam`).
- `call`: Domain provider action (`call: <domain>.<action>` with optional `with:`).
- `review`: Independent review proof requiring `role` and non-empty `contours` (for example `paths`, `claims`, `architecture`, `concurrency`, `project_isolation`, `administration`).
- Named catalog proof (`proof: blueprint` referencing `proofs.blueprint`) or inline JSON Schema (`proof: { scheme: ... }`).

### `DomainProvider` Extensions (`call`, `checks`, `context`, `tools_add`, `tools_remove`)
Domain-agnostic `acdd-state` delegates domain behavior via `DomainProvider` (`crates/acdd-core/src/ports.rs`; see the [domain extensions guide](../architecture/domain-extensions.md)):
- **`call: <domain>.<action>`** (`with: { ... }`): Executes a registered domain action (with `lookup_action_result` recovery for mutating actions).
- **`checks: [<domain>.<check>]`** (or `{ check, with? }`): Automated domain verifiers executed during `acdd verify` / `acdd_verify` (e.g., `code.seam`, `code.lint` supporting `with: { categories: [arch, syntax, docs, duplicates, dead_tails, cycles] }` and failing when `summary.errors > 0`, `code.signature_scope`, `docs.links`, `docs.specs`, `spec.deltas_applied`); `acdd submit` / `acdd_submit` re-validates live freshness against the recorded trace digest without re-running checks.
- **`context: [<ref> | { context: <ref>, with?: { ... } }]`**: Explicit domain context lenses assembled into `ClaimResp` Tier 6, symmetric with `checks:`. Providers declare typed `ContextCapability` entries (`name`, `scope_mode: admitted | node_only`, `default_paths_key`, `input_schema`) and `supports_collection_context: bool` in `DomainCapabilities` (see the complete table below).
- **`tools` / `tools_add` / `tools_remove`**: Per-gate MCP tool allowlists layered over `default_tools` (including built-in tools and `<domain>_<name>` plugin tools).

### Context Lenses (`specs.*`, `paths.*`, `docs.*`, `code.*`) & 5-Tier Diff-Aware Ranking

Every gate declares its exact context lenses in `context:` (either as a string shorthand or `{ context: <ref>, with: { ... } }`; `WorkflowDef.paths:` binds non-spec workspace directories/globs, while `acdd.yaml` `specs:` binds Spec collections):

| Lens | Owner / Source | What It Injects into `ClaimResp.context_lenses` |
|---|---|---|
| `specs.<name\|base>` | `acdd.yaml` `specs.<name>` or built-in base (`DocsDomainProvider`) | Specs and schema-rendered items from the matching Spec collection(s) (e.g. `specs.memories`, `specs.plans`, or base-agnostic `specs.memory`), ranked via the 5-tier diff-aware policy below and packed into the Tier 6 byte budget. |
| `paths.<name>` | `WorkflowDef.paths.<name>` (`DocsDomainProvider`) | Non-spec workspace files/directories declared in the workflow `paths:` map (e.g. `paths.instructions`, `paths.adrs`), ranked via the 5-tier diff-aware policy below. |
| `docs.invariants` | `DocsDomainProvider` | Non-archive `INV-*` invariants governing symbols or paths in `admitted_scope`, marking lint-backed rules with `[ENFORCED]`. |
| `docs.workspace_config` | `DocsDomainProvider` | Summary of active workspace config: `specs`, `docs_roots`, `workflows_dirs`, `available_workflows`, `architecture_zones`, `limits.max_metadata_bytes`, and `modules`. |
| `code.symbols` | `CodeDomainProvider` | Indexed production symbols inside `admitted_scope` (filtering derived/generated methods). |
| `code.tests` | `CodeDomainProvider` | Deduplicated covering test paths for `admitted_scope` symbols (capped at top 8). |
| `code.external_callers` | `CodeDomainProvider` | Inbound callers and SQL table consumers outside `admitted_scope` (capped at top 5 plus overflow count). |

**5-Tier Diff-Aware Relevance Ranking** ([ADR 0006](../adr/0006-budgeted-gate-context-and-dual-surface-parity.md), [ADR 0014](../adr/0014-declarative-spec-schemas-and-items.md), [ADR 0015](../adr/0015-retrospective-memory-and-diff-aware-context.md)):
`specs.<name|base>` and `paths.<name>` lenses rank candidate items/documents deterministically before bounded 0-1 knapsack / `max_bytes` packing (`pack_knapsack_indices`):
1. **Direct ID/title citation** in the current task `target` or owning Spec body (highest priority).
2. **Active `git diff` match** (when the worktree has uncommitted changes): hunk-exact `changed_symbols` / `doc_section` edges (when `modules.code: true` and `code-map.sqlite` exists) > file-diff `scope` / `owners` overlap > file-diff `code_map` edges.
3. **Explicit `admitted_scope` overlap** (`scopes_overlap` between item `scope` / spec `owners` and task `admitted_scope`).
4. **Indexed `code-map.sqlite` doc edges** (`documents`, `references_doc`, `cites_doc`, `governs`, `governed_by` linking the document to any path or symbol in `admitted_scope`).
5. **Global fallback rules** (`scope: ["**"]` or unlinked repo-wide documents — lowest priority, dropped first under budget pressure).

### Repository Memory, `memory_synthesis`, and Retrospective Pipeline

- **Embedded `memory_synthesis` template**: Built-in `code.yaml` and `documentation.yaml` ship an opt-in `gate_templates.memory_synthesis` single-role gate template (`use: memory_synthesis`, `scope: [specs.memory]`) that synthesizes durable lessons from run blackboard notes/findings/decisions into `spec: memory` documents across any collection using the `memory` base. Downstream gates receive relevant memory rules automatically via `context: [specs.memories]` (or `specs.memory`) ranked by the 5-tier diff-aware policy above.
- **Declarative retrospective pipeline (`retrospective.yaml` + `memory_optimize.yaml`)**: Repositories can capture structured per-run retrospectives without engine hardcoding by declaring `.acdd/specs/retrospective.yaml` (bound via `specs.retrospectives` at `docs/memories/.retro`, gitignored) and running the standalone `memory_optimize` workflow (`skills/acdd/references/acdd-workflow-examples/memory_optimize.yaml`). `memory_optimize` consolidates unprocessed `docs/memories/.retro/<workflow>/<spec_or_run>.md` files into flat Git-tracked `docs/memories/<domain>.md` (plus `docs/memories/instructions-and-skills.md` and `docs/memories/archive/` for retired rules) and moves processed retrospectives to `.retro/<workflow>/.processed/`.

---

## Reference Examples

- See the starter overlay template in [`acdd_workflow.yaml.example`](acdd_workflow.yaml.example) (symlink into `skills/acdd/references/`) and the complete field guide in [`skills/acdd/references/workflow-reference.md`](../../skills/acdd/references/workflow-reference.md).
- Embedded core workflows: [`base.yaml`](../../crates/acdd-state/src/workflow/embedded/base.yaml), [`code.yaml`](../../crates/acdd-state/src/workflow/embedded/code.yaml), and [`documentation.yaml`](../../crates/acdd-state/src/workflow/embedded/documentation.yaml). Auxiliary workflow templates (`audit.yaml`, `spec_planning.yaml`, `hotfix.yaml`, `memory_optimize.yaml`) live in [`skills/acdd/references/acdd-workflow-examples/`](../../skills/acdd/references/acdd-workflow-examples/README.md).
- See [Workspace configuration](configuration.md#declarative-acdd-workflow-and-role-configuration) for loading precedence and `acdd.yaml` adapter settings.
