---
title: "Installation, Workspace Setup, and Operational Guide"
doc_type: guide
---

# Installation, Workspace Setup, and Operational Guide

ContextUnity ACDD (`0.7.0`) is a local-first, zero-daemon Rust engine for AI coding agents. It provides deep code graph navigation, documentation indexing, and a unified `GateProfile` state machine — a system that injects tailored context into verified gates. The same binary serves both as a standard stdio [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server (20 built-in tools plus optional `DomainProvider` plugin tools) and as a command-line interface (CLI).

This guide walks you through installing ACDD MCP, configuring your workspace, selecting profiles, installing companion skills globally, establishing repository rules, authoring indexed documentation, and running milestones, plans, and workflows.

---

## 1. Prerequisites and Installation

### System Requirements

- **Git**: Git 2.30+ for worktree management and commit/tree-bound lifecycle operations.
- **Release installation**: POSIX `sh`, ordinary shell utilities, `tar` with gzip support, `curl` or `wget`, and `sha256sum` or `shasum`. No Rust toolchain or C compiler is required.
- **Building from source**: Stable Rust 2021 edition (`rustc` and `cargo` 1.88+) and a C compiler (`gcc` or `clang`) for bundled SQLite and native Tree-sitter parsers. Install Rust via [rustup.rs](https://rustup.rs).

### Installing a Release (`install.sh`)

The recommended installation method uses the zero-dependency installer script [`install.sh`](install.sh). It runs in standard POSIX `sh` without requiring Rust, Python, Node, or C compilers:

```sh
# Latest stable release (recommended)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh

# Nightly pre-release (active development & latest fixes)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh -s -- --nightly
```

If you have already cloned the repository:

```sh
./install.sh             # Interactive prompt: choose between latest (default) and nightly
./install.sh --latest    # Non-interactive: install latest stable release
./install.sh --nightly   # Non-interactive: install nightly pre-release
```

#### Command-Line Flags & Options

| Option | Shorthand | Description |
|---|---|---|
| `--latest` | `-l` | Install the latest stable release (default) |
| `--nightly` | `-n` | Install the rolling nightly pre-release |
| `--help` | `-h` | Display usage syntax and available options |

#### Environment Variables

| Variable | Description | Default |
|---|---|---|
| `ACDD_RELEASE_CHANNEL` | Release channel to install (`latest` or `nightly`) | Prompted in TTY, else `latest` |
| `ACDD_RELEASE_BASE_URL` | Custom base URL for downloading `SHA256SUMS` and archive tarball (useful for air-gapped mirrors or specific tags) | GitHub Releases |
| `AGENTS_SKILLS_DIR` | Canonical global skills hub directory | `~/.agents/skills` |

#### Automated Execution Lifecycle

The installer performs the following operations atomically:

1. **Platform Auto-Detection**: Determines host OS and architecture. Supported targets:
   - `x86_64-unknown-linux-gnu` (Linux 64-bit x86)
   - `aarch64-unknown-linux-gnu` (Linux 64-bit ARM)
   - `x86_64-apple-darwin` (macOS Intel)
   - `aarch64-apple-darwin` (macOS Apple Silicon M-series)
2. **Cryptographic Checksum Verification**: Downloads `SHA256SUMS` alongside the target archive, computing the actual SHA-256 digest using `sha256sum` or `shasum -a 256`. If the checksum does not match or the archive entry is missing, the installer halts immediately (fails closed).
3. **Atomic Binary Installation**: Extracts the pre-compiled, stripped binary into a temporary file in `~/.local/bin/` with `0755` permissions and atomically moves it to `~/.local/bin/acdd`.
4. **Agent Skills Hub Sync**: Executes the companion `skills/install.sh` script, copying companion skills to `~/.agents/skills/` and symlinking them into detected agent directories (`.gemini`, `.codex`, `.claude`, `.cursor`).
5. **Project MCP Configuration**: Updates or creates `$PWD/.mcp.json` using an embedded RFC 8259-compliant AWK parser, adding the `acdd` stdio server entry while preserving all other servers, formatting, and custom fields.
6. **Live Process Reload**: If updating an existing installation, runs `acdd reload` to notify active MCP server sessions without requiring manual restarts.

#### Installing a Specific Tag or Using a Mirror

To install a specific version tag (e.g. `<tag>`) or download from an internal company mirror:

```sh
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh -o /tmp/acdd-install.sh
ACDD_RELEASE_BASE_URL="https://github.com/ContextUnity/contextunity-acdd/releases/download/<tag>" sh /tmp/acdd-install.sh
```

#### Post-Installation & Verification

1. Ensure `~/.local/bin` is in your shell `PATH`:
   ```bash
   export PATH="$HOME/.local/bin:$PATH"
   ```
2. Verify binary execution:
   ```bash
   acdd --version
   ```
3. Verify MCP server response:
   ```bash
   acdd query overview
   ```

### Building from Source

Clone the repository and install the binary into your Cargo binary directory (`~/.cargo/bin`):

```bash
git clone https://github.com/ContextUnity/contextunity-acdd.git
cd contextunity-acdd

# Standard build with default languages (Python, Rust, TypeScript, JavaScript, Vue, HTML, Protobuf, YAML, TOML)
cargo install --path . --locked

# Extended build with all supported languages (+ Go, Java, C#, Kotlin, PHP, Ruby, C, C++)
cargo install --path . --locked --features all-languages
```

Ensure `~/.cargo/bin` is in your shell `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
acdd --version
```

### Connecting to AI Agent Clients (MCP stdio)

Add `acdd` to your agent runtime's MCP configuration:

#### Antigravity / Gemini CLI (`~/.gemini/antigravity/mcp_config.json` or project `.gemini/settings.json`)

```json
{
  "mcpServers": {
    "acdd": {
      "type": "stdio",
      "command": "acdd"
    }
  }
}
```

#### OpenAI Codex (`~/.codex/config.toml` or `codex mcp add`)

Add to `~/.codex/config.toml`:

```toml
[mcp_servers.acdd]
command = "acdd"
```

Or register via Codex CLI:

```bash
codex mcp add acdd -- acdd
```

#### Claude Desktop (`claude_desktop_config.json`)

```json
{
  "mcpServers": {
    "acdd": {
      "command": "acdd"
    }
  }
}
```

#### Cursor (`.cursor/mcp.json`)

```json
{
  "mcpServers": {
    "acdd": {
      "command": "acdd"
    }
  }
}
```

> [!NOTE]
> ACDD operates with the workspace directory as its working directory (`cwd`). If your client starts servers in a different directory, pass `--root /path/to/workspace` in `args` or set the environment variable `ACDD_WORKSPACE_ROOT`.
> For complete client connection options and troubleshooting, see the canonical [MCP setup guide](docs/reference/mcp-setup.md).

---

## 2. Workspace Initialization and `acdd.yaml`

`acdd.yaml` located at the root of your project is the adapter configuration file. It declares which directories to index, where documentation lives, `specs:` collections, profile directories, state store path, `modules:` flags, and output budgets.

### Quick Bootstrap

Generate a starter `acdd.yaml`, `.acdd/profile.yaml`, and canonical `.gitignore` rules automatically:

```bash
acdd init
```

Upgrading an existing `0.5.x` workspace? Follow the step-by-step [0.5.x to 0.6.0 migration runbook](docs/runbooks/migration-0.6.md) and verify with `acdd validate`.

### Complete Configuration Schema

A fully featured `0.6.2` `acdd.yaml` template:

```yaml
# Source directories to parse and index into .acdd/code-map.sqlite
roots:
  - src
  - crates

# Markdown documentation paths indexed into DocSection search chunks
docs:
  - README.md
  - docs

# Specification collections (mapped to profiles and sequential IDs)
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3

# Directories searched for standalone/named GateProfile YAML files
profiles_dirs:
  - .acdd/profiles
  - docs/workflows

# State database path and run trace retention
state_db: ".acdd/state.sqlite"
retain_traces: 20

# Subsystem activation flags
modules:
  code: true
  docs: true
  state: true
  blackboard: true

# Paths and directories excluded from code indexing (literal basenames)
ignore:
  - target
  - node_modules
  - .venv
  - .git
  - .acdd

# Patterns blocked from reader operations and export (gitignore syntax)
sensitive:
  - '.env*'
  - 'secrets/**'

# Multi-repository linking: federate external repositories into unified code graph
linked_workspaces:
  - name: shared-core
    path: ../shared-core
    enabled: true
    roots:
      - src
    docs:
      - docs
    ignore:
      - target
      - node_modules

# MCP response budget policy
response:
  detail: compact
  page_size: 30
  max_output_bytes: 65536
```

> [!NOTE]
> For the complete, exhaustive reference of all configuration fields (including `limits.memory_budget_mb`, `response.source_context`, `owners`, `aliases`, and `handoff_pruning`), see the canonical [Workspace configuration reference](docs/reference/configuration.md).

### Recommendations for Tailoring `acdd.yaml` to Your Repository

1. **Scope `roots` tightly**: Name only directories containing authored source code (e.g. `src`, `crates`, `frontend/src`). Avoid specifying the repository root `.` when possible to prevent indexing scratch scripts or unrelated files.
2. **List documentation explicitly in `docs`**: Include your `README.md`, architecture docs, ADRs, and guides. ACDD parses Markdown at `##` and `###` headings into indexed `DocSection` units for BM25 search and automatic symbol linkage.
3. **Configure `ignore`**: Default exclusions automatically skip `.git`, `.acdd`, `target`, `node_modules`, `.venv`, `dist`, `build`, and cache folders. Add any custom generated directories or build outputs. Remember that `ignore` matches literal directory basenames, not glob patterns.
4. **Protect secrets via `sensitive`**: Add patterns like `'.env*'`, `'secrets/**'`, or `'*.pem'`. Reader tools and export operations refuse access to matching paths.
5. **Configure `linked_workspaces` for Multi-Repo architectures**: If your project relies on sibling libraries, configure `linked_workspaces`. ACDD merges declarations and imports into the active code graph, allowing cross-repo symbol resolution and blast-radius impact analysis across project boundaries.

### How to Verify that `acdd.yaml` Works

Run the following checks from your repository root:

```bash
# 1. Validate configuration syntax, profiles, DomainProvider capabilities, and specs
acdd validate

# 2. Verify documentation links and anchors
acdd docs check

# 3. Build or refresh the code graph in .acdd/code-map.sqlite
acdd scan
# Or full build:
acdd build .

# 4. Query the graph overview to confirm language coverage and symbol counts
acdd query overview

# 5. Verify documentation indexing and unified architectural/doc lint
acdd docs search "setup"
acdd lint
```

When connected to an AI agent via MCP, calling `code_map_overview` returns the same indexed workspace status, generation token, language breakdown, and document counts.

### Setting up `.gitignore`

ACDD uses `.acdd/` for runtime caches, indexes, and SQLite databases while persisting committed gate profiles and framework manifests. Configure your `.gitignore` using the canonical pattern (written automatically by `acdd init`):

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

---

## 3. Profiles: Built-in vs Custom Universal Profile

Profiles (`GateProfile`, `version: 2`) govern lifecycle stages, `git:` isolation/integration policy, verification rules, review contours, roles, and contextual guidance injected into agent claims under ACDD.

### Profile Resolution Order

When starting a run (`acdd start`) or validating (`acdd validate`), ACDD resolves the active profile in the following priority:
1. **CLI `--profile` flag**: Explicit built-in name (`base`, `code`, `audit`, `documentation`), `profiles_dirs` entry, or workspace-relative path.
2. **Spec frontmatter `profile:`**: Profile declared in the specification Markdown frontmatter (e.g. `profile: audit` or a custom `.workflow.yaml`).
3. **Collection override**: Optional `specs.<collection>.profile` configured in `acdd.yaml`.
4. **Repository default**: `.acdd/profile.yaml` if present.
5. **Compiled built-in fallback**: Embedded `code` (automatically stripping disabled `code`/`docs` domain hooks).

If an `inherit:` reference includes a SHA-256 prefix (`<path>:<sha256_prefix>`), ACDD verifies the parent file digest on every load and fails closed if tampered.

### Built-in Profiles

ACDD embeds four `version: 2` profiles (`crates/acdd-state/src/profile/embedded/`):
- **`base`**: Domain-pure worktree baseline (`gates: []`, 7 core tools, universal guidance, 3 core contours) with `git.isolation: worktree` and `save: commit`.
- **`code`**: Built-in code-delivery baseline inheriting `base`, adding the three-phase milestone lifecycle (`blueprint_synthesis` -> per-task `build`, `review`, `deliver`, `rearchitect` -> `run_tests`, `polish`, `integration_review`, `handoff`), 20 built-in tools, `code.seam` + `code.lint` verification checks, and `docs.*`/`code.*` context lenses.
- **`audit`**: Read-only code review and security/taint scanning pipeline (`recon -> analyze -> triage`) inheriting `base` with `git.isolation: in_place`, `save: none`, structured `recon_map`/`candidate_findings`/`audit_report` schemas, adversarial false-positive disproof in `triage`, and `save.report: ".acdd/reports/{run_name}.md"`.
- **`documentation`**: Compiler-free living documentation and invariant authoring pipeline inheriting `base` (`modules: { required: [docs, code] }`), running `blueprint_synthesis`, per-task `build -> review -> deliver`, `verify_docs`, `polish`, `integration_review`, and `handoff` gated on `docs.links` and `code.lint` (`[arch, docs]`).

Single-checkout in-place execution is selected via `--in-place` or `git.isolation: in_place` in a profile (`--in-place` sets `isolation: in_place`, `worktrees: manual`, `save: none`, and neutralizes `save.post: commit` to `none` while keeping `save.immutable: true`).

### Authoring a Custom Universal Profile (`version: 2`)

Create `.acdd/profile.yaml` to tailor commands, roles, and review contours to your technology stack:

```yaml
version: 2
inherit: code

# Scoped command mappings referenced by patched gates
commands:
  rust-check-and-test:
    by_pattern:
      "**/*.py": "pytest {test_filter}"
      "**/*.{ts,tsx,js,jsx,vue}": "npm test -- {test_filter}"
      "**/*.go": "go test ./... {test_filter}"
      "**/*.rs": "cargo clippy --all-targets --all-features -- -D warnings && cargo test {test_filter}"
    test_filter: "{test}"
    description: "Run strict checks and tests across project languages."
    timeout_seconds: 300

  check_full_repo:
    command: "cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-targets"
    description: "Run full-repository verification before handoff."
    timeout_seconds: 1800

# Patch command bindings onto the inherited gate tree
gates_patch:
  - tasks:
      gates:
        - id: build
          command: rust-check-and-test
  - id: run_tests
    command: check_full_repo
  - id: polish
    command: check_full_repo
```

> [!TIP]
> Always validate custom profiles using `acdd validate .acdd/profile.yaml`.
> See [ACDD Architecture & Default Profile Guide](docs/reference/acdd.md) and [ACDD Profiles Reference](docs/reference/profiles.md) for gate mechanics and inheritance details, or inspect [`skills/acdd/references/acdd_profile.yaml.example`](skills/acdd/references/acdd_profile.yaml.example) for a standalone `inherit: none` template.

---

## 4. Global Skill Installation

Companion skills equip coding agents with instructions and tool strategies for working with ACDD, authoring tests, refactoring, and closing review findings.

The repository includes a zero-config installer script (`./skills/install.sh`):

```bash
# Sync all bundled skills into ~/.agents/skills/ and symlink to all detected agent runtimes
./skills/install.sh

# Or install specific skills selectively:
./skills/install.sh acdd test-suite-rules close-review-findings
```

### What `skills/install.sh` Does

1. Establishes `~/.agents/skills/` as the canonical global skill repository.
2. Detects installed AI agent environments and links skills into their directories; existing regular skill directories receive copies:
   - **Antigravity / Gemini CLI**: `~/.gemini/config/skills/`
   - **OpenAI Codex**: `~/.codex/skills/`
   - **Anthropic Claude**: `~/.claude/skills/`
   - **Cursor**: `~/.cursor/skills/`
3. Supports flags:
   - `--copy`: Perform standalone file copies instead of symlinks.
   - `--dry-run`: Preview changes without modifying files on disk.

### Bundled Companion Skills

| Skill | Purpose |
|---|---|
| [`acdd`](skills/acdd/SKILL.md) | Essential ACDD operations: code navigation, symbol reads, task claims, blackboard, delivery |
| [`contract-planning`](skills/contract-planning/SKILL.md) | Architectural viability audit, ADR/invariant reconciliation, and contract planning |
| [`test-suite-rules`](skills/test-suite-rules/SKILL.md) | Production-seam testing, table matrices, harness hygiene, and suite refactoring |
| [`close-review-findings`](skills/close-review-findings/SKILL.md) | Diagnosing review rejections and implementing evidence-closed fixes |
| [`agent-instructions`](skills/agent-instructions/SKILL.md) | Authoring and updating `AGENTS.md`, `SKILL.md`, and documentation |

---

## 5. Repository Rules Setup

Configure agent routing and instruction files so AI assistants automatically use ACDD MCP tools instead of raw filesystem operations.

### Configuring `AGENTS.md`

Place an `AGENTS.md` in your repository root based on [`skills/acdd/references/agents.md.example`](skills/acdd/references/agents.md.example):

1. **Mandatory MCP Navigation**: Instruct agents to use `code_map_*`, `ast_grep_search`, and `acdd_read` for code exploration.
2. **Direct Single-Shot Symbol Reads**: Direct agents to call `acdd_read(selector="SymbolName")` directly when a symbol name is known, avoiding multi-step search grep calls.
3. **State & Blackboard Context**: Route lifecycle state through `acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, and mandate posting technical notes, findings, and blockers to `acdd_blackboard`.
4. **Verification Seams**: Point agents to the active `.acdd/profile.yaml` and `TESTS.md` for exact verification commands.

### Configuring IDE / Agent Platform Rules

#### Cursor (`.cursor/rules/acdd.mdc`)

```markdown
---
description: Mandatory Code Navigation via ContextUnity ACDD
globs: *
alwaysApply: true
---

# Code Navigation Rules

When working in this repository:
- Use ACDD MCP tools (`code_map_search` with `detail: "compact"`, `code_map_inspect`, `code_map_impact`, `ast_grep_search`) for symbols, AST patterns, and graph exploration.
- Use `acdd_read(selector="SymbolName")` for reading complete symbol implementations.
- Use `rg -n` (or `acdd_read(mode="search", path=...)`) for directory substring/error/config searches, and native file reads for large file ranges (`>120` lines).
- Use `ctx_shell` or standard execution for running builds and tests.
```

#### Gemini / Antigravity (`.gemini/rules/acdd.md`)

```markdown
# Mandatory Code Navigation via ContextUnity ACDD

- Use ACDD MCP tools (`code_map_search` with `detail: "compact"`, `code_map_inspect`, `code_map_impact`, `code_map_query`, `code_map_overview`) and `ast_grep_search` for symbol lookup, structural AST patterns, and graph queries.
- Use `acdd_read(selector="SymbolName")` for digest-verified symbol bodies (with optional `path="..."`).
- Use `read_file` for reading large file slices (`>120` lines), and `rg -n` (or `acdd_read(mode="search", path=...)`) for searching substrings, error messages, or config files across directories.
```

---

## 6. Authoring Documentation for Indexing

ACDD indexes documentation into `.acdd/code-map.sqlite` alongside code symbols, enabling BM25 full-text search (`search_docs`) and automatic bidirectional linking between code declarations and documentation sections.

Follow these rules when writing documentation:

### 1. YAML Frontmatter at Byte 0

Every Markdown document intended for indexing must begin at byte 0 with valid YAML frontmatter:

```markdown
---
title: "Authentication and Session Management"
doc_type: architecture
---
```

Common `doc_type` values: `architecture`, `guide`, `reference`, `plan`, `adr`, `runbook`.

### 2. Heading Chunking (`##` and `###`)

ACDD splits documents at second-level (`##`) and third-level (`###`) headings into discrete `DocSection` records. Each section forms an independent BM25 search chunk. Keep headings descriptive and stable.

### 3. Symbol Linkage

Reference exact code declarations to establish verified edges in the SQLite graph:
- Use exact declaration names: `` `TokenValidator` `` or with explicit source paths: `` [`TokenValidator`](src/auth/token.rs) ``.
- When an indexed declaration resolves uniquely, ACDD creates a verified edge between the documentation section and the code entity.
- Code inspections (`code_map_inspect`, `code_map_explain`) automatically surface linked documentation sections when inspecting governed symbols.

### 4. Verified Architectural Invariants (`INV-*`)

Declare architectural invariants using GitHub alert callouts containing `Invariant:` and a unique `INV-*` identifier:

```markdown
> [!IMPORTANT]
> Invariant: Token validation must reject expired tokens before inspecting session storage.
> INV-AUTH-EXPIRATION: [`validate_token`](src/auth/validator.rs)
```

ACDD marks the section with `is_invariant: true` and connects it to the governed declaration. During blast-radius impact analysis (`code_map_impact`) and symbol inspection, ACDD highlights these architectural rules directly to agents.

---

## 7. Working with Runs, Specs, Plans, and Workflows

All stateful execution in ACDD `0.6.2` uses the unified flat CLI (`acdd draft`, `acdd start`, `acdd list`, `acdd show`, `acdd next`, `acdd claim`, `acdd verify`, `acdd submit`, `acdd run`) backed by `.acdd/state.sqlite` and `GateProfile` v2.

### Spec & Run Lifecycle

#### 1. Drafting and Starting a Run

```bash
# Draft the next sequentially numbered spec in a configured collection (e.g. milestones or plans)
acdd draft milestones auth-refactor --prefix AC --title "Refactor Auth Pipeline"

# Start a worktree-isolated run from an existing spec manifest:
acdd start docs/milestones/AC-045-auth-refactor.md --worktree --target-base main

# Or start an ephemeral goal run without writing a permanent spec file:
acdd start --goal "Fix parser edge case" --scope "src/parser/**" --in-place
```

#### 2. Listing, Inspecting, and Executing Automated Waves

```bash
# List active runs and optional closed tombstones
acdd list --status active

# Inspect full run or node status
acdd show
acdd show t1

# Execute automated DAG waves when roles configure shell runners:
acdd run --jobs 2
```

#### 3. Gate Execution Work Loop (`claim` -> `verify` -> `submit`)

Agents advance gates through atomic claims, observed verification, and claim-bound submissions:

```bash
# 1. Inspect ready node cursors
acdd next

# 2. Claim a ready node (or omit cursor to auto-select)
acdd claim t1.build --worker agent-1 --worktree .

# 3. Run gate verification without advancing state (records fresh trace)
acdd verify t1.build --test tests/auth_test.rs::test_token_expiry

# 4. Submit pass against the observed trace (or --action reject --findings '<JSON>')
acdd submit t1.build --action pass
```

#### 4. Collaboration via `acdd_blackboard` and Scope Extensions

```bash
# Post a technical note or review finding
acdd blackboard --action post --node t1 --topic notes --payload "Token validation logic split into validator.rs"

# Read messages for a run or node
acdd blackboard --action read --node t1

# Extend a task's admitted write scope with an explicit reason
acdd extend-scope t1 --path src/auth/utils.rs --reason "Share token helper across auth modules"
```

#### 5. Task Integration and Terminal Handoff

```bash
# Integrate completed task branches into the run worktree (automated `acdd run` lands via rebase-ff)
git merge --no-ff <task-branch>

# Run terminal verification and submit handoff (commits receipt, archives spec, prunes run)
acdd verify run_tests
acdd submit run_tests --action pass
acdd claim integration_review --worker integration-reviewer
acdd submit integration_review --action pass --evidence '{"decision":"pass","contours":{...},"summary":"..."}'
acdd submit handoff --action pass
```

### In-Place Plans, Standalone Workflows, and Plugin Tools

- **In-Place Plans (`docs/plans/*.md`)**: Start with `acdd start docs/plans/migration-plan.md --in-place` (or select a `git.isolation: in_place` profile via `profile:` / `specs.plans.profile`). `--in-place` neutralizes `save: commit` to `none` so work runs cleanly in the current checkout.
- **Audit & Standalone Workflows (`audit`, `docs/workflows/*.workflow.yaml`)**: Start read-only audits via `acdd start --goal "Audit auth paths" --profile audit` (writing reports to `.acdd/reports/<run_name>.md` on `triage` pass), or run custom workflow files with `--profile <name>`.
- **External `DomainProvider` Plugin Tools**: Invoke registered plugin tools from CLI via `acdd tool <domain>_<name> --args '{"key":"value"}'` or directly over MCP.
