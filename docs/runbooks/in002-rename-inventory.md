# IN-002 One-Pass Rename Inventory

Authoritative inventory for [`IN-002-contextunity-acdd-one-pass-rename`](../milestones/IN-002-contextunity-acdd-one-pass-rename.md). Stored under `docs/runbooks/` because `is_milestone_manifest_name` (`crates/acdd-tasks/src/milestones/mod.rs`) treats uppercase `<TAG>-<NUM>-*.md` files inside `docs/milestones/` as milestone manifests.

## 1. Cargo, Crates, and Build Identities (Wave 1)

| Old identity | New identity | Notes |
| --- | --- | --- |
| Package `contextunity-forge-mcp` | `contextunity-acdd` | Keep version `0.5.2` |
| Library `contextunity_forge_mcp` | `contextunity_acdd` | Root `src/lib.rs` re-exports |
| Binary `contextunity-forge-mcp` | `cmd:acdd` (`[[bin]] name = "acdd"`) | Sole shipped CLI binary |
| `crates/forge-core` (`forge-core` / `forge_core`) | `crates/acdd-core` (`acdd-core` / `acdd_core`) | Preserve pure leaf (`INV-CORE-LEAF`) |
| `crates/forge-index` (`forge-index` / `forge_index`) | `crates/acdd-index` (`acdd-index` / `acdd_index`) | Preserve `acdd-core`-only dependency edge |
| `crates/forge-tasks` (`forge-tasks` / `forge_tasks`) | `crates/acdd-tasks` (`acdd-tasks` / `acdd_tasks`) | Preserve `acdd-core` dependency edge |
| `build.rs`, `crates/forge-index/build.rs`, `crates/forge-index/build_manifests.rs` | `crates/acdd-index/build*.rs`, `ACDD_LANGUAGE_PROFILE_DIGEST` | Update `cargo:rerun-if-changed` & env macro |

## 2. Config, State, Trace Paths, and Workspace Guidance (Waves 1 & 3)

| Old path / rule | New path / rule | Owner |
| --- | --- | --- |
| `forge-mcp.yaml` | `acdd.yaml` | `crates/acdd-core/src/config.rs`, `src/cli/paths.rs` |
| `.forge/acdd/profile.yaml` | `.acdd/profile.yaml` | `crates/acdd-tasks/src/tasks/profile/`, `src/cli/paths.rs` |
| `.forge/code-map.sqlite` | `.acdd/code-map.sqlite` | `crates/acdd-core/src/config.rs`, `crates/acdd-tasks/src/engine/mod.rs` |
| `.forge/tasks.sqlite` | `.acdd/tasks.sqlite` | `crates/acdd-tasks/src/engine/mod.rs` (`default_tasks_db`) |
| `.forge/task-traces` | `.acdd/task-traces` | `crates/acdd-tasks/src/milestones/command_gate.rs::persist_log` |
| `.forge/checkpoints.json`, `.forge/runs` | `.acdd/checkpoints.json`, `.acdd/runs` | `crates/acdd-tasks/src/` |
| `FORGE_SKILL_NAME`, `FORGE_SKILL_FILE`, `forge_gitignore_invalid` | `ACDD_SKILL_NAME="acdd"`, `.agents/skills/acdd/SKILL.md`, `acdd_gitignore_invalid` | `crates/acdd-tasks/src/engine/workspaces.rs` |

## 3. Environment Variables (Wave 1, Zero Fallbacks)

- `FORGE_WORKSPACE_ROOT` -> `ACDD_WORKSPACE_ROOT`
- `FORGE_DB` -> `ACDD_DB`
- `FORGE_CONFIG` -> `ACDD_CONFIG`
- `FORGE_PROFILE` -> `ACDD_PROFILE`
- `FORGE_ALLOW_BROAD_ROOT` -> `ACDD_ALLOW_BROAD_ROOT`
- `FORGE_MEMORY_BUDGET_MB` -> `ACDD_MEMORY_BUDGET_MB`
- `FORGE_DEBUG` -> `ACDD_DEBUG`
- `FORGE_AGENT_SESSION_ID` -> `ACDD_AGENT_SESSION_ID`
- `CONTEXTUNITY_FORGE_MCP_SESSION` -> `ACDD_MCP_SESSION_ID`
- `CONTEXTUNITY_FORGE_MCP_RELOAD_PARENT` -> `ACDD_MCP_RELOAD_PARENT`
- `FORGE_LANGUAGE_PROFILE_DIGEST` -> `ACDD_LANGUAGE_PROFILE_DIGEST`
- `FORGE_EXTRACTION_CACHE_DIR` -> `ACDD_EXTRACTION_CACHE_DIR`
- `FORGE_PROFILE_LINKER` -> `ACDD_PROFILE_LINKER`
- `FORGE_ORCHESTRATION_ENVS` -> `ACDD_ORCHESTRATION_ENVS`
- `FORGE_RELEASE_BASE_URL` -> `ACDD_RELEASE_BASE_URL` (installer/docs, Wave 2)
- `FORGE_RELEASE_CHANNEL` -> `ACDD_RELEASE_CHANNEL` (installer/docs, Wave 2)
- `FORGE_INSTALL_COMMAND` -> `ACDD_INSTALL_COMMAND` (installer/docs, Wave 2)

## 4. MCP Tools, Skills, Packaging, and Documentation (Waves 1 & 2)

- Reader tool: `Server::forge_read` / `forge_read_tool` -> `Server::acdd_read` (`route:acdd_read`) / `acdd_read_tool`
- Skill directory: `skills/contextunity-forge/` -> `skills/acdd/` (including `references/forge-mcp.yaml.example` -> `references/acdd.yaml.example`)
- Installer & release workflows: `install.sh`, `skills/install.sh`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `tests/release_packaging.rs` -> target `acdd` binary and `contextunity-acdd-<target>.tar.gz` archive with `SHA256SUMS`
- Active guidance & release notes: `AGENTS.md`, `src/AGENTS.md`, `tests/AGENTS.md`, `README.md`, `INSTALL.md`, `TESTS.md`, `docs/**` active pages, `CHANGELOG.md` `[Unreleased]` (keep version `0.5.2`)

## 5. Preserved Durable Contracts & Exclusions

- Completed MCP tool rename: `forge_guide` -> `acdd_guide`; `code_map_*`, `task_*`, `ast_grep_search`, and `session_checkpoint` remain unchanged.
- Completed default repository/project rename: `"forge-mcp"` -> `"acdd"`; commitment algorithm rename: `"forge-owner-sha256-v2"` -> `"acdd-owner-sha256-v2"`. Schema version constants remain unchanged.
- Completed Git ref namespace rename: `refs/forge/*` -> `refs/acdd/*` for runtime refs and `refs/notes/forge` -> `refs/notes/acdd` for existing handoff passport/preparation ownership notes.
- Immutable historical records: `docs/milestones/archive/**`, `docs/plans/archive/**`, prior `CHANGELOG.md` release entries, `benchmarks/` corpus data
- Trace evidence contract: `persist_log` and `record_trace` record execution trace locators consumed verbatim by `read_trace_log`; accepted historical SQLite trace rows are never unconditionally rewritten during offline transfer
