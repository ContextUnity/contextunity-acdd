---
title: "Migrating from 0.5.x to 0.6.0"
doc_type: guide
---

# Migrating from 0.5.x to 0.6.0

ContextUnity ACDD `0.6.0` replaces the legacy `0.5.x` task engine (`crates/acdd-tasks` and `.acdd/tasks.sqlite`) with the unified `crates/acdd-state` engine (`.acdd/state.sqlite`), `GateProfile` (`version: 2`), flat CLI commands (`acdd start`, `claim`, `verify`, `submit`), and 19 static plus optional `DomainProvider` plugin MCP tools. Upgrade an existing `0.5.x` repository manually in six steps and verify everything fail-closed with `acdd validate`.

## 1. Complete or Archive Open `0.5.x` Tasks

Before switching configuration and deleting legacy runtime files:

1. Deliver or cancel any in-flight `0.5.x` milestones, plans, or workflows using your pre-`0.6.0` binary, or record their remaining state in Git-tracked Markdown.
2. Confirm no uncommitted worktree changes remain across `.worktrees/`.

## 2. Update `acdd.yaml`

Replace legacy `0.5.x` top-level keys (`milestones`, `plans`, `tasks_db`, `task_repository`, `task_project`, `acdd_profile`) with `0.6.0` `Adapter` fields (see [Configuration reference](../reference/configuration.md)):

```yaml
roots:
  - src
docs:
  - docs
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3
profiles_dirs:
  - .acdd/profiles
  - docs/workflows
state_db: .acdd/state.sqlite
retain_traces: 20
modules:
  code: true
  docs: true
  state: true
  blackboard: true
```

Note that `AdapterFile` enforces `#[serde(deny_unknown_fields)]` and rejects legacy `0.5.x` top-level keys (`milestones`, `plans`, `tasks_db`, `workflows_dir`, `milestone_prefix`, `plan_prefix`, `task_repository`, `task_project`, `acdd_profile`) fail-closed with `CONFIG_MIGRATION_REQUIRED`; remove all obsolete `0.5.x` keys completely.

## 3. Update `.gitignore`

Ensure `.acdd/profile.yaml`, `.acdd/profiles/**`, and `.acdd/frameworks/**` remain Git-tracked while ignoring all runtime SQLite, lock, log, trace, and worktree artifacts:

```gitignore
/.worktrees/
!/.acdd/
!/.acdd/profile.yaml
!/.acdd/profiles/
!/.acdd/profiles/**
!/.acdd/frameworks/
!/.acdd/frameworks/**
.acdd/*.sqlite*
.acdd/*.lock
.acdd/*.log
.acdd/*.json
.acdd/*.jsonl
.acdd/reports/
.acdd/run/
.acdd/tmp/
```

## 4. Upgrade `.acdd/profile.yaml` and Custom Workflows to `version: 2`

Convert every profile under `.acdd/profile.yaml`, `.acdd/profiles/`, and `docs/workflows/` to the strict `GateProfile` (`version: 2`) schema (see [ACDD Profiles Reference](../reference/profiles.md)):

1. Add `version: 2` at the top of the file (and `inherit: code` for standard software repositories, `inherit: audit` for read-only review/security scans, or `inherit: base` with explicit `gates:` for non-code workflows; use `gates_patch:` when patching specific inherited gates).
2. Rename legacy gate keys:
   - `persistence` $\to$ `save`
   - `agent` $\to$ `role`
   - `on_success` $\to$ `pass`
   - `review_sources` $\to$ `need`
   - `polish_scope` $\to$ `scope`
   - `steps` $\to$ `guidance` (inline `{ inline: "..." }` or key references into `guidance:`)
   - `proof: delivery` $\to$ `receipt: true` with `save.post: commit` (or `none`)
3. Replace `loop:` blocks with explicit `fail: <gate_id>` edges and `max_attempts:`.
4. Replace `proof: seam_test` (or legacy `proof: contract` / `expected_failure`) with `proof: command` plus `checks: [code.seam]` (or inherit `code` which attaches `code.seam` automatically), removing legacy `- id: seam` containers and `output:` regex scrapers.
5. Convert any `*_proof` custom wrappers to `proofs.<name>` JSON Schemas (`SchemaDef`) or inline `proof: { scheme: ... }`.
6. Rewrite legacy MCP tool names in `tools` / `default_tools` / `tools_add` (`task_*`, `session_checkpoint`, `forge_guide`) to `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`, `acdd_read`, and `acdd_guide`.
7. Recompute any local pinned `inherit: <path>:<sha256_prefix>` digests if the referenced profile changed.
8. When using `inherit: code`, if your profile overrides `tags:`, retain/include `docs` (for example `tags: [default, docs, ...]`) if downstream gates reference `!docs`. Remove obsolete `git.commit_mode`, `git.integrate`, `git_commit_notes`, and `save.git_commit_note` keys.

## 5. Clean Open `SpecDocument` Frontmatter and `### task:` Blocks

Open specs under `docs/milestones/` and `docs/plans/` are parsed with `#[serde(deny_unknown_fields)]`:

- Remove legacy frontmatter keys (`repository`, `project`, `proof_policy`, `symbols`, `extended_symbols`, `scope_roots`, `agent_type`, `concurrency`, and per-task `acdd_profile`).
- Ensure `doc_type` is `contract` (milestones) or `plan` (plans), `status` is one of `planned | active | completed | cancelled`, `tags` defaults to `[default]`, and `scope:` lists workspace-relative globs.

## 6. Remove Obsolete `0.5.x` Runtime Artifacts and Validate

Remove obsolete `0.5.x` runtime state from the primary worktree:

```sh
rm -f .acdd/tasks.sqlite .acdd/tasks.sqlite-wal .acdd/tasks.sqlite-shm .acdd/tasks.sqlite.backup
rm -f .acdd/checkpoints.json .acdd/tasks.receipt.lock .acdd/workflow-observations.jsonl .acdd/trace.log .acdd/integration-evidence.json
rm -rf .acdd/tasks .acdd/runs
```

Verify configuration, active profile, `DomainProvider` capabilities, open specs, and Markdown links fail-closed:

```sh
acdd validate
acdd docs check
```
