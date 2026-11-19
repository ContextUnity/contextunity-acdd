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

Profiles use strict schema validation (`deny_unknown_fields`). Legacy `0.5.x`
profiles (without `version: 2`, or using removed `loop:`, `*_proof` wrappers,
`proof: seam_test` / `proof: delivery`, or `expected_failure` without `test_filter`)
fail closed with `ACDD_PROFILE_SCHEMA_INVALID`; upgrade them manually per the
[0.6.0 migration guide](../runbooks/migration-0.6.md).

> [!NOTE]
> Contract and command passes require claim-bound execution observations via
> `acdd verify` / `acdd_verify`. Seam/domain checks (`checks: [code.seam]`) and
> domain actions (`call: <domain>.<action>`) execute through `DomainProvider`
> adapters in `src/state_bridge/`. Headless automated execution is described in
> [CLI commands](cli.md#headless-run-execution).

---

## Profile Loading Precedence & Embedded Baselines

ACDD resolves the active profile reference (`--profile`, `ACDD_PROFILE`, spec
`profile:`, `specs.<collection>.profile`, or `"workspace"`) as follows:

1. **Built-in embedded names**:
   - `default` ([`default.yaml`](../../crates/acdd-state/src/profile/embedded/default.yaml)):
     domain-pure worktree-isolated lifecycle (`blueprint_synthesis` -> `tasks:` -> `verify`
     -> `polish` -> `integration_review` -> `handoff`) with `save: commit`,
     `git.isolation: worktree`, `expected_failure: {}`, six canonical review
     contours (`paths`, `claims`, `architecture`, `concurrency`, `project_isolation`,
     `administration`), anti-drift guidance (`REUSE & COMPLEXITY ESCALATION`,
     `REBUTTAL OVER BLIND REWRITE`, `BURDEN OF PROOF`), and zero language-specific
     exit codes or `.acdd/frameworks` assumptions.
   - `code` ([`code.yaml`](../../crates/acdd-state/src/profile/embedded/code.yaml)):
     inherits `default` (`gates_merge: patch`), adds `tags: [default, docs]` so
     `docs`-tagged tasks skip `code.seam`, binds `default_tools` to all 19
     built-in MCP tools, attaches `code.*` and `docs.*` `DomainProvider`
     `checks:` (`code.seam`) and `context:` lenses, and automatically strips
     `get_doc`, `search_docs`, and `docs.*` hooks when `modules.docs: false`.
   - `in_place` ([`in_place.yaml`](../../crates/acdd-state/src/profile/embedded/in_place.yaml)):
     inherits `workspace` (resolving `.acdd/profile.yaml` when present else
     `default`, `gates_merge: patch`), sets `git.isolation: in_place` and
     `save: none` (and `save.post: none` on `tasks.deliver` and `handoff`) for
     zero-worktree, zero-auto-commit feature runs.
2. **`"workspace"` selector**: Resolves `.acdd/profile.yaml` when present in the
   workspace root; otherwise falls back to embedded `default`.
3. **Explicit repository paths & `profiles_dirs`**: Any relative path confined to
   the workspace (optionally pinned with `:<sha256_prefix>`, min 7 lowercase hex
   chars) or named profile discovered in `acdd.yaml` `profiles_dirs`
   (default `[".acdd/profiles", "docs/workflows"]`).

> [!NOTE]
> Linked workspaces (`linked_workspaces`) do not override the parent ACDD
> profile. Only the active workspace root governs profile resolution.

---

## Top-Level `GateProfile` Schema (`version: 2`)

Every `v2` profile requires `version: 2` and supports these top-level fields:

```yaml
version: 2
inherit: code                  # Optional: default | code | in_place | none | <rel-path>[:<sha256_prefix>]
gates_merge: patch             # Optional: replace (default) | patch
git:
  isolation: worktree          # worktree | in_place | none
  worktrees: manual            # manual | managed
  commit_mode: plumbing        # plumbing | cli | manual
  integrate: manual            # manual | rebase-ff | ff-only | no-ff | squash
save: commit                   # Default post save action (none | commit) or { post?, immutable?, archive?, prune? }
tags: [default, docs_only, seam]
protected_paths: []
default_tools: [acdd_read, acdd_guide, acdd_state, acdd_claim, acdd_verify, acdd_submit, acdd_blackboard]
modules: {}                    # Optional SubsystemRegistryPatch
limits: {}                     # Optional ResourceLimitsPatch
git_commit_notes: {}           # Optional named GitCommitNotePolicy map
roles: {}                      # RoleDef / RolePatch map
commands: {}                   # CommandDef map (String | Script | Detailed | ByPattern | Unbound)
proofs: {}                     # Reusable ProofCatalogDef JSON Schema registry
guidance: {}                   # Keyed GuidanceEntry slots (string or { description, criteria })
contours: {}                   # Keyed ContourDef review lenses ({ description, criteria })
gate_templates: {}             # Reusable GateTemplate subgraphs expanded via `use:`
gates: []                      # Ordered RawGateItem list (root gates and `- tasks:` blocks)
```

---

## Inheritance and `gates_merge: replace | patch`

- `inherit: none`: Compiles a standalone profile from scratch with no inherited
  roles, commands, proofs, guidance, contours, or gates.
- `inherit: default | code | in_place | <path>[:<sha256_prefix>]`:
  - `roles`, `commands`, `proofs`, `guidance`, `contours`, `gate_templates`,
    `git_commit_notes`, `limits`, and `modules` merge field-wise / by key
    (supporting `PatchField` null-clearing on nullable fields).
  - `tags` and `protected_paths` extend or patch inherited sets. When inheriting
    `code` (whose `tasks.contract` and `tasks.contract_review` gates use
    `when: "!docs"`), any child profile that overrides `tags:` must retain or
    include `docs` (or override the inherited `when:` expressions) so all
    referenced tags remain declared.
  - **`gates_merge: replace`** (default when omitted): If the child declares
    `gates:`, it replaces the parent's `gates:` list completely.
  - **`gates_merge: patch`**: Patches individual inherited gates without
    rewriting the graph:
    - Update a gate by `id` (e.g. `id: polish`) or dotted cursor
      (`id: tasks.build`, `id: blueprint_synthesis.synthesize`), or match a
      `- tasks:` block by normalized `when:` expression.
    - Delete an inherited gate with `- <gate_id>: null`.
    - Insert a new gate at an explicit position with `after: <selector>` or
      `before: <selector>`.

---

## `git:` Policy, `save:` Policy, and Stateless Mode

- **`git:`**:
  - `isolation`: `worktree` (separate git worktree per task/run), `in_place`
    (active worktree, single-writer), or `none` (stateless/ephemeral run).
  - `worktrees`: `manual` or `managed` (engine creates/removes `.worktrees/*` automatically).
  - `commit_mode`: `plumbing` (git object/tree commit without hooks modifying
    verified candidate), `cli` (runs `git commit` with hooks and checks tree
    integrity), or `manual`.
  - `integrate`: `manual`, `rebase-ff`, `ff-only`, `no-ff`, or `squash`
    (controls `acdd integrate`).
- **`save:`**: Controls working-tree commits and SQLite persistence per gate (also settable
  at profile root as default or shorthand scalar `save: none` / `commit`):
  - `post`: `none` or `commit` (records candidate tree or creates a scoped commit on submit).
  - `immutable`: Require working tree at `submit` to match the claim tree (`TASK_CANDIDATE_MISMATCH` on drift).
  - `archive` / `prune`: Allowed on `root_gate` only (typically `handoff`); archives
    the completed spec or prunes terminal SQLite rows into `run_tombstones`.
  - `git_commit_note`: Key in `profile.git_commit_notes` attached as a Git note when `post: commit`.

---

## Commands, `test_filter`, and Output Rules

Each entry in `commands:` is one of:
- **`unbound: true`**: Declared placeholder slot that must be bound by a child
  overlay before any gate referencing it can pass validation (`COMMAND_UNBOUND`).
- **Simple string**: Shell command string.
- **`Script` / `Detailed`**: `{ command, description?, timeout_seconds?, test_filter?, output?, compile_errors? }`.
- **`Pattern` (`by_pattern`)**: `{ by_pattern: { "<glob>": ... }, description?, ... }`
  mapping changed/admitted scope globs to language commands via the canonical
  brace-aware glob matcher in `acdd_core::path_policy` (most specific glob wins;
  `compile_errors` can filter `include`/`exclude` regexes). Top-level
  `scope_args_by_pattern` propagates as fallback arguments into resolved commands
  (with `{file_stem}` and `{path}` expanded against concrete admitted paths).

**`test_filter` & `--test`**:
- `test_filter: false` (default on unbound/generic commands if omitted) rejects
  `acdd verify --test <target>` (`COMMAND_TEST_FILTER_UNSUPPORTED`).
- `test_filter: true` appends/substitutes `{test}` using single-argument command
  appending, or `test_filter: "-- {test}"` formats the filter explicitly.
- Chained commands (`&&`, `||`, `;`, `|`) require `{test_filter}` explicitly in
  the command string whenever `test_filter` is enabled
  (`COMMAND_TEST_FILTER_PLACEHOLDER_REQUIRED`).

**`output:`**: Optional `{ counts: { passed, failed }, require_counts, noise, errors, tests: { passed, failed?, ignored?, target? } }`
using regex capture group 1 to extract exact test counts and individual passed
test names from command output. `limits.diagnostic_inline_bytes` defaults to
`4096`; raw trace logs live under `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`.

---

## Gates, Conditional `when:`, `pass:` / `fail:` Routing, and `DomainProvider`

### Gate Fields & Routing (`pass`, `fail`, `fail_drift`, `need`, `independent_from`)
- `pass`: Explicit sibling gate ID on success (defaults to next sibling gate if omitted).
- `fail`: Target sibling gate ID on `defect` reject/failure (replaces removed `0.5.x` `loop:` blocks).
- `fail_drift`: Optional target sibling gate ID (e.g. `rearchitect`) on `architectural_drift` rejections, clearing cached builder context and rotating runner `ACDD_AGENT_SESSION_ID`.
- `when`: Boolean tag expression (`tag`, `!tag`, `a & b`, `a + b`, `a | b`, `( ... )`)
  parsed and evaluated solely by `acdd_state::profile::when::WhenExpr`,
  allowed on `- tasks:` blocks and `task_gate` nodes (forbidden on `root_gate`).
  All referenced tags must appear in profile `tags:` (or `"default"`).
- `max_attempts`: Failure/rejection budget before escalation (default `3`, `>= 1`).
- `independent_from`: Requires a `worker_id` distinct from the accepted worker of named predecessor gates (`TASK_REVIEW_NOT_INDEPENDENT`).
- `need`: Predecessor gate selectors whose accepted candidate `TreeOid` must match at submit.
- `receipt: true`: Marks a terminal gate that records a durable receipt and closes its container.

### Proofs (`proof:`)
Built-in `ProofKind` values are:
- `none`: Default when omitted.
- `contract`: Runtime-red proof requiring `command`, `expected_failure` (`exit_codes` / `stderr_contains`), and `test_filter`. An empty `exit_codes: []` (as in `default.yaml`) accepts any runtime failure in `1..=125` with `0` `compile_errors`; non-empty `exit_codes` rejects `0`, negative values, `126`, and `127`.
- `command`: Passing command verification (`exit_code == 0`, `tests_failed == 0`).
- `call`: Domain provider action (`call: <domain>.<action>` with optional `with:`).
- `review`: Independent review proof requiring `role` and non-empty `contours` (for example `paths`, `claims`, `architecture`, `concurrency`, `project_isolation`, `administration`).
- Named catalog proof (`proof: blueprint` referencing `proofs.blueprint`) or inline JSON Schema (`proof: { scheme: ... }`).

### `DomainProvider` Extensions (`call`, `checks`, `context`, `tools_add`, `tools_remove`)
Domain-agnostic `acdd-state` delegates domain behavior via `DomainProvider` (`crates/acdd-core/src/ports.rs`; see the [domain extensions guide](../architecture/domain-extensions.md)):
- **`call: <domain>.<action>`** (`with: { ... }`): Executes a registered domain action (with `lookup_action_result` recovery for mutating actions).
- **`checks: [<domain>.<check>]`** (or `{ check, with? }`): Automated domain verifiers executed during `acdd verify` / `acdd submit` (e.g., `code.seam`, `code.signature_scope`, `docs.links`).
- **`context: [<domain>.<lens>]`**: Domain context lenses assembled into `ClaimResp` tiers (`docs.instructions`, `docs.adrs`, `docs.invariants`, `code.symbols`, `code.tests`, `code.external_callers`).
- **`tools` / `tools_add` / `tools_remove`**: Per-gate MCP tool allowlists layered over `default_tools` (including built-in tools and `<domain>_<name>` plugin tools).

---

## Reference Examples

- See the canonical executable profile template in [`acdd_profile.yaml.example`](acdd_profile.yaml.example) (symlink to the full specification in `skills/acdd/`).
- Embedded profiles: [`default.yaml`](../../crates/acdd-state/src/profile/embedded/default.yaml), [`code.yaml`](../../crates/acdd-state/src/profile/embedded/code.yaml), and [`in_place.yaml`](../../crates/acdd-state/src/profile/embedded/in_place.yaml).
- See [Workspace configuration](configuration.md#declarative-acdd-profile-and-role-configuration) for loading precedence and `acdd.yaml` adapter settings.
