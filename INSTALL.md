---
title: "Installation, Workspace Setup, and Operational Guide"
doc_type: guide
---

# Installation, Workspace Setup, and Operational Guide

ContextUnity ACDD is a local-first, zero-daemon Rust engine for AI coding agents. It provides deep code graph navigation, documentation indexing, and an ACDD task lifecycle state machine — a system that injects tailored context into verified gates. The same binary serves both as a standard stdio [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server (20 tools) and as a command-line interface (CLI).

This guide walks you through installing ACDD MCP, configuring your workspace, selecting profiles, installing companion skills globally, establishing repository rules, authoring indexed documentation, and running milestones, plans, and workflows.

---

## 1. Prerequisites and Installation

### System Requirements

- **Git**: Git 2.30+ for worktree management and snapshot-bound lifecycle operations.
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

`acdd.yaml` located at the root of your project is the adapter configuration file. It declares which directories to index, where documentation lives, milestone and plan directories, task store paths, and output budgets.

### Quick Bootstrap

Generate a starter adapter file automatically:

```bash
acdd guide init
```

### Complete Configuration Schema

A fully featured `acdd.yaml` template:

```yaml
# Source directories to parse and index into .acdd/code-map.sqlite
roots:
  - src
  - crates

# Markdown documentation paths indexed into DocSection search chunks
docs:
  - README.md
  - docs

# Directory containing Markdown milestone specifications
milestones:
  - docs/milestones

# Directory containing Markdown plan documents
plans:
  - docs/plans

# Paths and directories excluded from code indexing (literal basenames)
ignore:
  - target
  - node_modules
  - .venv
  - .git

# Patterns blocked from reader operations and export (gitignore syntax)
sensitive:
  - '.env*'
  - 'secrets/**'

# Task lifecycle database and primary agent guidance routing file
task_repository: "my-project"
task_project: "my-project"
tasks_db: ".acdd/tasks.sqlite"
agents_guidance: "AGENTS.md"

# Path to ACDD gate profile (defaults to .acdd/profile.yaml if omitted)
acdd_profile: ".acdd/profile.yaml"

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
# 1. Validate configuration syntax, roots, and adapter paths
acdd guide validate

# 2. Build or refresh the code graph in .acdd/code-map.sqlite
acdd scan
# Or full build:
acdd build .

# 3. Query the graph overview to confirm language coverage and symbol counts
acdd query overview

# 4. Verify documentation indexing
acdd docs search "setup"
```

When connected to an AI agent via MCP, calling `code_map_overview` returns the same indexed workspace status, generation token, language breakdown, and document counts.

### Setting up `.gitignore`

ACDD uses `.acdd/` for runtime caches, indexes, and SQLite databases while persisting committed gate profiles and framework manifests. Configure your `.gitignore` using the canonical pattern:

```gitignore
# Allow committed configuration and profiles inside .acdd/
!/.acdd/
!/.acdd/profile.yaml
!/.acdd/frameworks/
!/.acdd/frameworks/**

# Exclude local runtime state, indexes, and session logs
.acdd/*.sqlite*
.acdd/*.lock
.acdd/*.log
.acdd/*.jsonl
.acdd/tasks/
.acdd/checkpoints.json
```

---

## 3. Profiles: Built-in vs Custom Universal Profile

Profiles govern task lifecycle stages, verification rules, review contours, roles, and contextual guidance injected into agent claims under ACDD (Augmented Context-Driven Delivery).

### Profile Resolution Order

When an agent claims a task gate, ACDD resolves the active profile in the following priority:
1. **Task pin**: Specific profile pinned directly on the task (`path:sha256-prefix`).
2. **Milestone pin**: Profile pinned on the containing milestone manifest.
3. **Workspace configuration**: `acdd_profile` path declared in `acdd.yaml`.
4. **Repository default**: `.acdd/profile.yaml` file if present.
5. **Compiled built-in fallback**: The embedded default profile.

If a profile is pinned with a SHA-256 hash prefix, ACDD verifies the file digest on every claim and fails with `TASK_PROFILE_TAMPERED` if any unauthorized modification occurs.

### Built-in Profile

The built-in profile provides a 4-stage sequential lifecycle:
- **`contract`**: Author writes a failing seam test proving the behavioral requirement (runtime exit code 101).
- **`build`**: Implementer writes code and verifies with a passing test command (exit code 0).
- **`review`**: Independent reviewer verifies changes against 5 review contours (`paths`, `claims`, `concurrency`, `project_isolation`, `administration`).
- **`deliver`**: Delivers changes, writes durable proof receipt into SQLite and milestone Markdown, and clears scratch blackboard messages.

### Authoring a Custom Universal Profile (`version: 2`)

Create `.acdd/profile.yaml` to tailor commands, roles, and review contours to your technology stack:

```yaml
version: 2
inherit: default

# Scoped command mappings referenced by inherited gates
commands:
  rust-check-and-test:
    by_pattern:
      "**/*.py": "pytest"
      "**/*.{ts,tsx,js,jsx,vue}": "npm test"
      "**/*.go": "go test ./..."
      "**/*.rs": "cargo clippy --all-targets --all-features -- -D warnings && cargo test"
    description: "Run strict checks and tests across project languages."
    timeout_seconds: 300
    output:
      counts: {passed: '(?m)\b(\d+) passed\b', failed: '(?m)\b(\d+) failed\b'}
      require_counts: true

  check_full_repo:
    by_pattern:
      "**/*.py": "pytest"
      "**/*.{ts,tsx,js,jsx,vue}": "npm test"
      "**/*.go": "go test ./..."
      "**/*.rs": "cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets"
    description: "Run full-repository verification before milestone handoff."
    timeout_seconds: 1800

# Overlay command bindings onto the embedded acdd.profile.yaml gate tree
gates:
  - tasks:
      gates:
        - id: build
          command: rust-check-and-test
        - id: seam
          when: seam
          gates:
            - id: integrate
              command: rust-check-and-test
  - id: verify
    command: check_full_repo
```

> [!TIP]
> Always validate custom profiles using `acdd validate --profile .acdd/profile.yaml`.
> See [ACDD Architecture & Default Profile Guide](docs/reference/acdd.md) and [ACDD Profiles Reference](docs/reference/profiles.md) for gate mechanics and inheritance details, or inspect [`skills/acdd/references/acdd_profile.yaml.example`](skills/acdd/references/acdd_profile.yaml.example) for a standalone `inherit: none` template.

---

## 4. Global Skill Installation

Companion skills equip coding agents with instructions and tool strategies for working with ACDD, authoring tests, refactoring, and closing review findings.

The repository includes a zero-config installer script (`./skills/install.sh`):

```bash
# Sync all bundled skills into ~/.agents/skills/ and symlink to all detected agent runtimes
./skills/install.sh

# Or install specific skills selectively:
./skills/install.sh acdd test-contract-seam close-review-findings
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
| [`test-contract-seam`](skills/test-contract-seam/SKILL.md) | Authoring compiling, failing seam tests for `contract` gates |
| [`close-review-findings`](skills/close-review-findings/SKILL.md) | Diagnosing review rejections and implementing evidence-closed fixes |
| [`test-suite-refactor`](skills/test-suite-refactor/SKILL.md) | Refactoring test suites while maintaining production coverage |
| [`agent-instructions`](skills/agent-instructions/SKILL.md) | Authoring and updating `AGENTS.md`, SKILL.md, and documentation |
| [`lean-ctx`](skills/lean-ctx/SKILL.md) | Compressed reading of source and command output |

---

## 5. Repository Rules Setup

Configure agent routing and instruction files so AI assistants automatically use ACDD MCP tools instead of raw filesystem operations.

### Configuring `AGENTS.md`

Place an `AGENTS.md` in your repository root based on [`skills/acdd/references/agents.md.example`](skills/acdd/references/agents.md.example):

1. **Mandatory MCP Navigation**: Instruct agents to use `code_map_*`, `ast_grep_search`, and `acdd_read` for code exploration.
2. **Direct Single-Shot Symbol Reads**: Direct agents to call `acdd_read(selector="SymbolName")` directly when a symbol name is known, avoiding multi-step search grep calls.
3. **Blackboard Context**: Mandate posting technical notes, findings, and blockers to `task_blackboard`.
4. **Verification Seams**: Point agents to the active task profile and `TESTS.md` for exact verification commands.

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
- Use ACDD MCP tools (`code_map_search`, `code_map_inspect`, `code_map_impact`, `ast_grep_search`) for symbols and code exploration.
- Use `acdd_read(selector="SymbolName")` for reading complete symbol implementations.
- Use `ctx_shell` or standard execution for running builds and tests.
```

#### Gemini / Antigravity (`.gemini/rules/acdd.md`)

```markdown
# Mandatory Code Navigation via ContextUnity ACDD

- ALWAYS use ACDD MCP tools (`code_map_search`, `code_map_inspect`, `code_map_impact`, `code_map_query`, `code_map_overview`) and `ast_grep_search` as primary tools for searching code and syntax patterns.
- Use `acdd_read` for reading files, outlines, and symbol implementations.
- Native grep/read is only permitted as a fallback if ACDD tools find no matches.
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

## 7. Working with Milestones, Plans, and Workflows

### Milestone Lifecycle

Milestones represent committed units of architectural progress containing structured task contracts.

#### 1. Creating and Drafting Milestones

```bash
# Draft a new milestone manifest
acdd milestone draft --num 045 --slug auth-refactor --title "Refactor Auth Pipeline"

# Or initialize an existing milestone manifest into the task store:
acdd milestone init docs/milestones/045-auth-refactor.md
```

#### 2. Listing and Inspecting Milestones

```bash
# List active milestones and completion progress
acdd milestone list --status active

# Show complete milestone contract and task definitions
acdd milestone show 045 --full
```

#### 3. Running Headless Milestone Execution

ACDD can orchestrate the entire milestone lifecycle through dependency waves:

```bash
acdd milestone run 045 --target-base main
```

#### 4. Milestone Handoff and Verification

When all tasks in a milestone are delivered:

```bash
# 1. Absorb uncommitted polish and generate exact content binding
acdd milestone handoff-context 045 --target-base main > /tmp/045-binding.json

# 2. Run repository verification (executes profile check commands)
acdd milestone verify 045 --target-base main

# 3. Complete handoff with independent review evidence file
acdd milestone handoff 045 --evidence-file .acdd/integration-evidence.json
```

`handoff` verifies content digests, reuses verified command traces, archives the milestone manifest to `docs/milestones/archive/`, and cleans up task state.

---

### Task Lifecycle Work Loop

Agents advance tasks through the state machine using atomic claims, proof verification, and evidence submission:

#### 1. Listing Available Tasks

```bash
# List ready tasks in the active milestone
acdd task list
```

#### 2. Claiming a Task Gate

```bash
acdd task claim <task-id> --stage contract --worker agent-1 --worktree .
```

The claim response returns:
- **`workflow_guidance`**: Role guidance, step-by-step instructions, and acceptance criteria.
- **Context bundle**: Task write scope, governed architectural decisions, in-scope symbols, covering tests, and recent blackboard messages.

#### 3. Verifying Gate Proof

For automated gates (`contract`, `build`), run `task verify` to execute the registered test/check command and cache the observation in SQLite:

```bash
# Verify failing contract seam test (exit code 101) or passing build (exit code 0)
acdd task verify <task-id> --test tests/auth_test.rs::test_token_expiry
```

#### 4. Submitting Gate Evidence

After a successful `task verify`, submitting the gate passes automatically using the cached observation:

```bash
# Submit passing transition (proof inferred from cached verification)
acdd task submit <task-id> --stage contract --action pass
```

Automated verification via `task verify` caches the execution trace in SQLite, allowing evidence-free submission. For manual or schema-driven gates that require explicit typed evidence payloads (such as independent `review`), see the [canonical evidence contract](docs/reference/tasks.md#gates-and-evidence).

#### 5. Collaboration via `task_blackboard`

Agents exchange notes, findings, and architectural decisions without cluttering Git history:

```bash
# Post technical note or review finding
acdd task blackboard post <task-id> --topic notes --payload "Token validation logic split into validator.rs"

# Read messages for a task
acdd task blackboard read <task-id>
```

#### 6. Autonomous Scope Extension

When implementation legitimately requires modifying an unlisted file inside admitted roots:

```bash
acdd task extend-scope <task-id> src/auth/utils.rs
```

> [!NOTE]
> Arguments are positional file paths. Paths must reside within the task's admitted `scope_roots` or frozen base roots. Scope extension fails closed if a path is exclusively locked by another active concurrent task or attempts to alter protected profile paths under `.acdd/**`.

---

### Structured Plans and Standalone Workflows

Beyond milestones, ACDD supports executing documentation plans and root-gate workflows:

#### Documentation Plans (`doc_type: plan`)

Execute structured multi-step plans without worktree branch isolation:

```bash
# List plans in docs/plans/
acdd plan list

# Inspect plan steps and dependencies
acdd plan show docs/plans/migration-plan.md

# Execute plan steps in dependency order
acdd plan run docs/plans/migration-plan.md
```

#### Standalone Root-Gate Workflows

Execute lightweight taskless workflows with ephemeral observations from `docs/workflows/`:

```bash
# List standalone workflows in docs/workflows/*.yaml
acdd workflow list

# Inspect workflow graph
acdd workflow show docs/workflows/ci-check.yaml

# Execute workflow in the current workspace
acdd workflow run docs/workflows/ci-check.yaml
```
