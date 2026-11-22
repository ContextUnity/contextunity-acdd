# Workspace adapter defaults

Use the [adapter example](acdd.yaml.example) for a starter `acdd.yaml`. `acdd init` generates a starter file.

Each worktree's adapter lives at its repository root and is loaded from the selected MCP root. Linked workspace adapters live in sibling repositories listed by `linked_workspaces` and may supply their own roots and documentation paths.

Without an adapter file, ACDD scans `.` respecting `.gitignore`, excludes common build/cache directories (`target`, `node_modules`, `.git`, `.venv`, `__pycache__`), and writes `.acdd/code-map.sqlite`. The unified state database defaults to `.acdd/state.sqlite` (`retain_traces: 20`), `profiles_dirs` defaults to `[.acdd/profiles]`, and `specs:` defaults to `milestones` (`docs/milestones`, `prefixes: [AC]`) and `plans` (`docs/plans`, `prefixes: [PLAN]`), both resolving the workspace default profile (`.acdd/profile.yaml` when present, else embedded `code`).

For Git-linked worktrees, relative `state_db` paths resolve from the primary worktree root (`git worktree list --porcelain`), while locks and backups live under `<git_common_dir>/acdd/`. Standalone repositories remain independent unless explicitly configured to share a store. Worktree selector precedence for MCP is described in the [tool catalog](tool-catalog.md#discovery-and-search).

The starter adapter demonstrates roots, documentation paths, ignores, `specs:` collections, `state_db`, `retain_traces`, `modules:`, optional `linked_workspaces`, and response limits. Its sample linked workspace uses sibling path `../shared-core`, with `src` and `docs` roots.

Include both source roots when indexing a multi-crate workspace:

```yaml
roots:
  - src
  - crates # Include crates for multi-crate workspaces.
```
