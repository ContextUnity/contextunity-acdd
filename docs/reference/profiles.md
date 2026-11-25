---
title: "ACDD Profiles Reference"
doc_type: api
---

# ACDD Profiles Reference

An **ACDD `GateProfile` (`version: 2`)** is a single declarative YAML graph that
governs a run: its Git isolation & integration policy (`git:`), persistence mode
(`save:`), tags (`tags:`), roles, commands, domain hooks (`call`, `checks`,
`context`, `tools_add`, `tools_remove`), proofs, guidance slots, review contours,
and ordered verification gates with `pass` / `fail` routing edges.

Profiles use strict schema validation (`deny_unknown_fields`). Legacy or
non-`version: 2` profiles (or those using removed `loop:`, `*_proof` wrappers,
`proof: contract` / `seam_test` / `delivery`, `expected_failure`,
`git.commit_mode`, `git.integrate`, `git_commit_notes`, or regex `output:`
scrapers) fail closed with `ACDD_PROFILE_SCHEMA_INVALID`.

> [!NOTE]
> Contract and command passes require claim-bound execution observations via
> `acdd verify` / `acdd_verify`. Seam/domain checks (`checks: [code.seam]`) and
> domain actions (`call: <domain>.<action>`) execute through `DomainProvider`
> adapters in `src/state_bridge/`. Headless automated execution is described in
> [CLI commands](cli.md#headless-run-execution).

---

## Profile Loading Precedence & Embedded Baselines

ACDD resolves the active profile reference (`--profile`, `ACDD_PROFILE`, spec
`profile:`, `specs.<collection>.profile`, or the workspace default) as follows:

1. **Built-in embedded baselines** (5 embedded profiles):
   - `base` ([`base.yaml`](../../crates/acdd-state/src/profile/embedded/base.yaml)):
     domain-pure worktree-isolated baseline (`gates: []`) with `save: commit`,
     `git.isolation: worktree`, 7 core MCP tools (`acdd_read`, `acdd_guide`,
     `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`),
     universal guidance slots (`core.execution`, `core.review`, `hitl.pro`), and
     three domain-pure review contours (`paths`, `claims`, `administration`).
   - `code` ([`code.yaml`](../../crates/acdd-state/src/profile/embedded/code.yaml)):
     built-in code-delivery baseline inheriting `base`, adding the three-phase
     milestone lifecycle (`blueprint_synthesis` -> `tasks:` -> `run_tests` ->
     `polish` -> `integration_review` -> `handoff`), `tags: [default, docs]` so
     `docs`-tagged tasks skip `code.seam`, all 20 `default_tools` (including
     `code_map_lint`), additional contours (`architecture`, `concurrency`,
     `project_isolation`), 4-layer clean guidance (`milestone.*` / `task.*`,
     `core.*`, `code.principles`, `code.tools`, `hitl.pro`), `code.seam` +
     `code.lint` checks, `docs.*` / `code.*` context lenses, and automatic
     stripping of disabled `code.*` / `docs.*` hooks when `modules.code: false`
     or `modules.docs: false`.
   - `audit` ([`audit.yaml`](../../crates/acdd-state/src/profile/embedded/audit.yaml)):
     built-in read-only code review and security/taint scanning pipeline
     inheriting `base` with `git.isolation: in_place`, `git.worktrees: manual`,
     `save: none`, all 20 `default_tools` (automatically stripped when
     `modules.code: false` or `modules.docs: false`), roles (`auditor`,
     `independent_reviewer`), structured schemas (`recon_map`,
     `candidate_findings`, `audit_report`), review contours (`correctness`,
     `security`, `architecture`, `concurrency`, `tests`), and ordered root gates
     `recon -> analyze -> triage` (adversarial false-positive disproof in
     `triage` with `receipt: true`, `save.immutable: true`, and
     `save.report: ".acdd/reports/{run_name}.md"`).
   - `documentation` ([`documentation.yaml`](../../crates/acdd-state/src/profile/embedded/documentation.yaml)):
     compiler-free living-documentation and invariant authoring pipeline
     inheriting `base` with `modules: { required: [docs, code] }`,
     `tags: [default, docs]`, roles (`architect`, `builder`,
     `independent_reviewer`), `blueprint_synthesis` -> per-task
     `build -> review -> deliver` -> `verify_docs -> polish -> integration_review -> handoff`,
     enforcing `docs.links` and `code.lint` (`with: { categories: [arch, docs] }`)
     without code-compilation commands.
   - `spec_planning` ([`spec_planning.yaml`](../../crates/acdd-state/src/profile/embedded/spec_planning.yaml)):
     embedded specification, milestone, and implementation plan synthesis
     pipeline inheriting `base` with `git.isolation: in_place`,
     `git.worktrees: manual`, `save: none`, `synthesize -> review` (`proof: spec_plan`,
     `checks: [docs.specs, docs.links, code.lint]`, `docs.workspace_config` context
     lens, automatic `acdd.yaml` `specs.*`/`docs` planning scope derivation, and
     `save.report: ".acdd/reports/{run_name}.md"` on `review`).
2. **Automatic workspace default**: When no explicit profile is specified (`--profile`,
   `ACDD_PROFILE`, spec `profile:`, or `specs.<collection>.profile`), ACDD uses
   `.acdd/profile.yaml` automatically if present in the workspace root, and
   otherwise falls back to embedded `code` (with disabled domain hooks stripped).
   (`in_place` is selected as an execution mode via `--in-place` or
   `git.isolation: in_place` inside a profile, not as a separate profile name.)
3. **Explicit repository paths & `profiles_dirs`**: Any relative path confined to
   the workspace (optionally pinned with `:<sha256_prefix>`, min 7 lowercase hex
   chars) or named profile discovered in `acdd.yaml` `profiles_dirs`
   (default `[".acdd/profiles"]`).

> [!NOTE]
> Linked workspaces (`linked_workspaces`) do not override the parent ACDD
> profile. Only the active workspace root governs profile resolution.

---

## Top-Level `GateProfile` Schema (`version: 2`)

Every `v2` profile requires `version: 2` and supports these top-level fields:

```yaml
version: 2
inherit: code                  # Optional: base | code | audit | documentation | spec_planning | none | <rel-path>[:<sha256_prefix>]
git:
  isolation: worktree          # worktree | in_place
  worktrees: manual            # manual | managed
save: commit                   # Default post save action (none | commit)
tags: [default, docs]
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

- `inherit: none` (or omitted): Compiles a standalone profile from scratch with no
  inherited roles, commands, proofs, guidance, contours, or gates.
- `inherit: base | code | audit | documentation | spec_planning | <path>[:<sha256_prefix>]`:
  - `roles`, `commands`, `scope_args_by_pattern`, `proofs`, `guidance`, `contours`,
    `gate_templates`, `limits`, and `modules` merge field-wise / by key
    (supporting `PatchField` null-clearing on nullable fields).
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
    `git.isolation: in_place` in a profile) runs directly in the active checkout;
    passing `--in-place` sets `isolation: in_place`, `worktrees: manual`,
    `save: none`, and neutralizes gate `save.post: commit` to `none` while
    keeping `save.immutable: true`. Passing `--worktree` overrides profile
    `in_place` defaults to `isolation: worktree` and `worktrees: managed`.
  - `worktrees`: `manual` or `managed` (engine creates/removes `.worktrees/*` automatically).
- **`save:`**: Controls working-tree commits, report materialization, and SQLite
  persistence per gate (also settable at profile root as default or shorthand
  scalar `save: none` / `commit`):
  - `post`: `none`, `commit` (creates an atomic Git plumbing commit with `Acdd-Claim` on submit),
    or `snapshot` (writes `refs/acdd/snapshots/<run_id>/<task_ref>/r<claim_revision>` without
    moving `HEAD`; profile validation allows `snapshot` only on leaf task gates; checkpoint refs
    auto-clear on deliver, run `clear`, or `handoff`).
  - `immutable`: Require working tree at `submit` to match the claim tree (`TASK_CANDIDATE_MISMATCH` on drift).
  - `archive` / `prune`: Allowed on `root_gate` only (typically `handoff`); archives
    the completed spec or prunes terminal SQLite rows into `run_tombstones`.
  - `report`: Optional normalized workspace-relative template path strictly confined under `.acdd/reports/*.{md,json}` and materialized
    atomically (`checked_child` + `atomic_write`) on `--action pass`, returning
    the expanded path in `SubmitResp.report_path`:
    - Supported template variables: `{run_id}`, `{run_name}`, `{gate_id}`, `{spec_id}` (`"goal"` for ephemeral `--goal` runs). Paths outside `.acdd/reports/*.{md,json}`, paths with `..`, leading `/`, `\`, or unknown `{...}` placeholders fail profile validation fail-closed.
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
  parsed and evaluated solely by `acdd_state::profile::when::WhenExpr`,
  allowed on `- tasks:` blocks and `task_gate` nodes (forbidden on `root_gate`).
  All referenced tags must appear in profile `tags:` (or `"default"`).
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
- **`checks: [<domain>.<check>]`** (or `{ check, with? }`): Automated domain verifiers executed during `acdd verify` / `acdd_verify` (e.g., `code.seam`, `code.lint` supporting `with: { categories: [arch, syntax, docs, duplicates, dead_tails, cycles] }` and failing when `summary.errors > 0`, `code.signature_scope`, `docs.links`); `acdd submit` / `acdd_submit` re-validates live freshness against the recorded trace digest without re-running checks.
- **`context: [<domain>.<lens>]`**: Domain context lenses assembled into `ClaimResp` tiers (`docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`).
- **`tools` / `tools_add` / `tools_remove`**: Per-gate MCP tool allowlists layered over `default_tools` (including built-in tools and `<domain>_<name>` plugin tools).

---

## Reference Examples

- See the starter overlay template in [`acdd_profile.yaml.example`](acdd_profile.yaml.example) (symlink into `skills/acdd/references/`) and the complete field guide in [`skills/acdd/references/profile-reference.md`](../../skills/acdd/references/profile-reference.md).
- Embedded profiles: [`base.yaml`](../../crates/acdd-state/src/profile/embedded/base.yaml), [`code.yaml`](../../crates/acdd-state/src/profile/embedded/code.yaml), [`audit.yaml`](../../crates/acdd-state/src/profile/embedded/audit.yaml), [`documentation.yaml`](../../crates/acdd-state/src/profile/embedded/documentation.yaml), and [`spec_planning.yaml`](../../crates/acdd-state/src/profile/embedded/spec_planning.yaml).
- See [Workspace configuration](configuration.md#declarative-acdd-profile-and-role-configuration) for loading precedence and `acdd.yaml` adapter settings.
