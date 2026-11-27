# Repository onboarding to ACDD

Use this guide when the user asks to set up a repository for ACDD navigation or
ACDD delivery. It works with agents that can read and edit repository files and
use the installed ACDD MCP or CLI, including Antigravity, Codex, Claude, and
Cursor. Onboarding is agent work over companion assets; it requires no Rust
compilation, Rust CLI scaffolder, or embedded templates.

## Establish the requested result

Read the complete request, repository instructions, existing configuration,
architecture, and accepted decisions before proposing changes. Distinguish
navigation setup, ACDD setup, and delivery planning: complete the requested
parts without assuming permission to execute proposed specs. Preserve
tailored files and unrelated changes; merge relevant settings instead of
replacing files wholesale. Resolve a conflicting architectural decision with
its owner before committing to that decision.

Installation and MCP client registration are separate from repository
onboarding. Use the already installed tools. If unavailable, report the missing
prerequisite and continue preparing repository files where possible; do not
install tools, change client configuration, publish, or write to agent home
directories merely because this guide is loaded.

## Discover the repository

Start with `code_map_overview()` when ACDD is available for the target checkout.
Select its `worktree` explicitly when it differs from the server root. Check
coverage before treating an absent node as absent source. Use indexed symbol,
AST, and documentation queries for subsequent discovery. Until ACDD is
available, use local file inventory and manifest reads, then return to ACDD
after configuration and indexing.

Record only facts needed for setup:

- Repository root, Git worktrees, source/test/generated roots, languages, and
  package/workspace manifests. Inspect actual package scripts, Makefiles,
  build configuration, CI commands, and test targets.
- Existing `AGENTS.md`, verification rules, `acdd.yaml`, selected ACDD
  workflow, documentation, plans, milestones, and ignore rules.
- Sibling repositories explicitly relevant to the request or confirmed by
  dependency manifests. Verify their paths, purpose, roots, and ownership;
  adjacency alone does not establish a dependency.

## Configure the adapter

Read [adapter defaults](adapters.md) and adapt the
[canonical adapter example](acdd.yaml.example) into `acdd.yaml`.
`acdd init` can create a starter when no adapter exists;
do not use `--force` over a tailored adapter.

- Set `roots` to real repository-relative source roots, or `.` when appropriate.
  Include all relevant workspace source trees, such as both `src` and `crates`
  in a multi-crate Rust workspace. Keep generated outputs out of the graph.
- Set `ignore` from actual build/cache directory basenames, respecting existing
  `.gitignore`; do not use invented glob semantics for `ignore`. Use `sensitive`
  patterns for confidential paths that readers must block.
- Configure `preset: full` (or `index | spec | state | minimal`), `specs:` entries (`milestones` with `path: docs/milestones`,
  `prefixes: [AC]`; `plans` with `path: docs/plans`,
  `prefixes: [PLAN]`), `workflows_dirs` (`[.acdd/workflows]`),
  `state_db: .acdd/state.sqlite`, `retain_traces: 20`, and `modules:` (`code`, `docs`, `spec`, `state`, `blackboard`).
- Retain the default relative `state_db` unless the repository requires another
  location. Git-linked worktrees resolve `<primary_worktree>/.acdd/state.sqlite`
  and share state locks/backups under `<git_common_dir>/acdd/`.
- Add only verified `linked_workspaces`: unique `name`, existing relative
  sibling `path`, `enabled`, and its real `roots` (plus relevant `docs`/`ignore`).

Check `acdd guide adapter` when a field's semantics are uncertain. Keep
configuration inside the consuming repository. Do not add scanners or
template-rendering code to ACDD.

## Synthesize a project workflow

For ACDD setup, write a compact overlay in `.acdd/workflows/default.yaml` (`version: 2`,
`inherit: code` when `modules.code: true` or `inherit: base` / `inherit: documentation`;
when present, `.acdd/workflows/default.yaml` automatically becomes
the workspace default; use `gates:` to replace inherited gates or `gates_patch:` to
patch them), using the
[starter workflow overlay](acdd_workflow.yaml.example) and the complete
[workflow reference](workflow-reference.md) for all `WorkflowDef v2` fields. Use the
[workflow examples catalog](acdd-workflow-examples/README.md) for ready-to-copy
`audit`, `spec_planning`, `hotfix`, and `inherit: audit` workflows (`security-sast`, `pr-code-review`, `architecture-hygiene`);
examples are starting points, and the installed schema (`acdd validate`)
is the authority. Do not copy ACDD's Rust-specific guidance, models, protected
files, or commands into an unrelated project.

Bind registered `commands` (`test`, `rust-check-and-test`, `check_full_repo`, etc.)
to verified local tooling. For each command, establish its working directory,
prerequisites, failure exit behavior, timeout, and `{test_filter}` support when
targeting scoped tests. Use `scope_args_by_pattern` only with arguments the real command
accepts, and keep full repository verification distinct from focused task checks.

Keep `version: 2`, `git:`, `roles`, `guidance`, `contours`, `proofs`, `commands`,
and `gates` references consistent. Preserve independent review (`independent_from`)
and candidate immutability (`save.immutable: true`) boundaries. Optional
`roles.<name>.runner` commands must match tools actually available to the project.

Validate the workflow before claims. Follow the resulting `acdd_claim`
`guidance`, expected proof contract, and active workflow for actual execution;
do not prescribe a universal gate list or hardcoded model in `AGENTS.md`.

## Merge the agent router

Adapt the [AGENTS router example](agents.md.example) into the repository's
existing root `AGENTS.md`. Preserve local instructions and authorization rules.
Replace example document routes with real paths; omit ACDD repository naming
taxonomies and delivery conventions unless adopted by this project.

Require the shared `acdd` skill and ACDD MCP/CLI navigation when
available, including overview/coverage, indexed symbol and AST search, direct
`acdd_read(selector="KnownSymbol")`, state lifecycle tools (`acdd_state`,
`acdd_claim`, `acdd_verify`, `acdd_submit`), and `acdd_blackboard`
context. State the local fallback when ACDD is unavailable. Route verification,
test placement, architecture, and documentation to their owning files. Record
only established project invariants and Git permissions; setup does not grant
permission to commit on protected branches or publish changes.

## Bootstrap documentation and propose delivery

Use the [documentation layout](documentation-layout.md),
[docs router example](docs-readme.md.example), and
[documentation rules example](docs-agents.md.example). Reuse existing pages;
create directories only when they contain needed material. Document observed
topology in `docs/architecture/`, accepted decisions in `docs/adr/`, and current
interfaces and operations in their owner pages. Do not turn an unapproved idea
into an accepted ADR. State invariants with stable `INV-*` identifiers and a
precise uniquely resolvable code claim when a verified governance edge is
intended; otherwise distinguish a proposed constraint from implemented behavior.
Resolve all copied links from their destination files.

Propose a bounded first spec from the user's actual goal (`acdd draft milestones <slug>`
or `acdd start --goal "..."` for ephemeral runs). Give it an outcome, acceptance
criteria, owner paths, invariants, and independently scoped tasks (`task_ref`,
`target`, `scope`, `depends_on`, `tags`, `invariants`, `acceptance_criteria`,
`contract_revision: 1`, `subtasks`). Use the
[manifest example](spec_manifest.md.example)
for shape, removing unrelated skills, repositories, dependencies, and domains.

Keep speculative sequencing in `docs/plans/` (run with `--in-place` or `git.isolation: in_place` when zero worktree/commit isolation is needed). Leave
the spec unstarted when execution was not requested. When admission is
authorized, use `acdd start` / `acdd_state` rather than editing SQLite or
manufacturing status/receipts.

## Validate and hand back

Run installed commands from the consuming repository root:

```sh
acdd validate .acdd/workflows/default.yaml
acdd validate
```

Run the first command when a workflow was created or changed, and `acdd validate`
to validate the workspace adapter, active workflows, and open spec manifests.
Validate each proposed spec with `acdd validate <repository-relative-spec.md>`
and inspect diagnostics rather than accepting example YAML on appearance alone.

After indexing, inspect `code_map_overview` for the correct root, roots/coverage,
languages, and linked workspaces. Run `acdd lint` (or `code_map_lint`) when
`architecture:` zones/rules or `acdd-rule` blocks are configured, and check a
representative source symbol and documentation page. Review the diff for preserved
settings, valid links, and scope; run `git diff --check`. Adapt the [ignore example](gitignore.example) so
runtime state under `.acdd/` and `/.worktrees/` stays excluded while
`.acdd/workflows/**`, `.acdd/specs/**`, and `.acdd/frameworks/**` remain tracked.

Report changed files, observed verification, remaining prerequisites, and the
proposed next delivery step. Schema and link checks establish artifact validity;
they do not prove that configured project commands pass or that an agent made
correct domain decisions. Run meaningful project checks when authorized and
feasible, and state precisely what remains unverified.
