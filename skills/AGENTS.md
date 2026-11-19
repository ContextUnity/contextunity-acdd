# Skills & Examples Governance

Read the [root instructions](../AGENTS.md) and load `agent-instructions` before editing any skill, template, or profile example under `skills/`.

## Boundaries & Domain Agnosticism

- **Keep skills repository- and language-agnostic**: Reusable skills (`SKILL.md`, `references/`, `scripts/`) express generic procedures, tool orchestration, and invariants. Never hardcode domain business logic, single-repo directory trees, proprietary schemas, or single-language commands into a bundled `SKILL.md`.
- **Route local behavior to the consuming repository**: Express repository-specific commands, paths, test lanes, and policies in the target repo's `AGENTS.md`, `.acdd/profile.yaml`, or `.agents/skills/<skill>/ADDON.md`—never inside `~/.agents/skills/<name>/SKILL.md`. Do not shadow a bundled skill with a local `SKILL.md` of the same name.
- **Keep bundled skill trees self-contained on install**: [`./skills/install.sh`](install.sh) syncs each `skills/<name>/` directory into `~/.agents/skills/<name>/`. Every relative Markdown link inside `SKILL.md` or skill references must resolve strictly inside that skill's directory, except host-workspace template catalogs explicitly scoped under [`acdd/references/acdd-profile-examples/`](acdd/references/acdd-profile-examples/README.md).
- **Keep external prerequisites decoupled**: Document external skills (such as `review-authority` used by `close-review-findings`) in [`skills/README.md`](README.md) without copying their definitions into `skills/`.

## Synchronizing Skills and Profile Examples

1. **Co-evolve on every surface change**: Whenever CLI flags, MCP tools, profile fields (`deny_unknown_fields`, `inherit`, `save`, `guidance`, `proofs`), or ADR contracts change, update in the same change:
   - Affected `skills/<name>/SKILL.md` procedure sections;
   - Canonical templates in [`skills/acdd/references/`](acdd/references/acdd_profile.yaml.example) (`acdd_profile.yaml.example`, `profile-reference.md`, `acdd.yaml.example`, `agents.md.example`, `docs-agents.md.example`, `gitignore.example`, `spec_manifest.md.example`);
   - `inherit: audit` workflow scenario examples (`{scenario}.workflow.yaml`) and index under [`skills/acdd/references/acdd-profile-examples/`](acdd/references/acdd-profile-examples/README.md).
2. **Keep examples concise and distinct**: Avoid cloning overlapping examples. Keep [`acdd-profile-examples/`](acdd/references/acdd-profile-examples/README.md) focused on distinct `inherit: audit` specializations (`security-sast`, `pr-code-review`, `architecture-hygiene`) using `gates_patch:` and `save.report`.
3. **Preserve present-tense style**: Follow `agent-instructions`—state current rules cleanly in affirmative imperatives (`Use`, `Run`, `Keep`, `Verify`, `Do not`) with zero historical migration notes inside skill instructions.

## Validation & Host Distribution

- Run `acdd validate <path>` on modified YAML profile examples and `acdd docs check` on Markdown updates before committing.
- Run `git diff --check` across `skills/`.
- **Host sync guard**: Do not mutate `~/.agents/` automatically during automated `cargo` tests or sandboxed milestone task runs. Run `./skills/install.sh` (or `./skills/install.sh --dry-run` for inspection) only from explicit installer/user-requested sync workflows.
