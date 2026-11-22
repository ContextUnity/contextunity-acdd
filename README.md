# ContextUnity ACDD

[![Rust](https://img.shields.io/badge/rust-2021%20edition-blue.svg)](https://www.rust-lang.org)
[![MCP Protocol](https://img.shields.io/badge/MCP-Model%20Context%20Protocol-green.svg)](https://modelcontextprotocol.io)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#license)

ACDD stands for **Augmented Context & Durable Dispatch**. ContextUnity ACDD is a local-first, zero-hidden-daemon Rust engine for AI coding agents that combines deep code and documentation graph navigation with durable state, gate verification, and parallel swarm dispatch into a single high-performance binary.

The same binary acts as both a standard stdio [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server (20 built-in tools plus `DomainProvider` plugin tools) for coding assistants and an autonomous command-line interface (CLI) for headless workflows.

> [!NOTE]
> **Architecture & Lifecycle Methodology**: See [Specification, Design, and Verification Lifecycle](docs/architecture/specification-and-design-lifecycle.md) ([Architecture index](docs/architecture/README.md)), [ADR 0003](docs/adr/0003-unified-profile-graph-and-state-machine.md), and [ADR 0004](docs/adr/0004-domain-provider-spi-and-plugin-tools.md) for how `GateProfile` v2, `Run` execution, `DomainProvider` SPI (`crates/acdd-core`), and living C3–C4 graphs fit together. See [ROADMAP.md](ROADMAP.md) for release direction.

---

## The Three Core Pillars

ContextUnity ACDD is architected around three foundational pillars designed to eliminate LLM hallucinations, enforce architectural discipline, and enable reliable multi-agent software engineering:

```text
┌───────────────────────────────────────────────────────────────────────────┐
│                             CONTEXTUNITY ACDD                             │
└─────────────────────────────────────┬─────────────────────────────────────┘
                                      │
         ┌────────────────────────────┼────────────────────────────┐
         ▼                            ▼                            ▼
┌──────────────────┐        ┌──────────────────┐        ┌──────────────────┐
│ 1. CODE          │        │ 2. LIVING        │        │ 3. ACDD STATE    │
│    INTELLIGENCE  │        │    DOCUMENTATION │        │    MACHINE       │
│                  │        │                  │        │                  │
│ • Tree-sitter AST│        │ • SQLite Graph   │        │ • Flexible Gates │
│ • code_map_*     │        │ • BM25 Sections  │        │ • DAG Wave Concur│
│ • Direct Symbol  │        │ • Code Traceable │        │ • Feedback Loops │
│ • ast_grep_search│        │ • INV-* Invariant│        │ • Lean Guidance  │
│ • Blast Radius   │        │   Auto-Surfacing │        │ • Delivery Guard │
└──────────────────┘        └──────────────────┘        └──────────────────┘
```

---

### 1. Code Intelligence and Structural Navigation

ContextUnity ACDD parses repository source code into an incremental, queryable SQLite graph (`.acdd/code-map.sqlite`) using native Tree-sitter grammars. Rather than relying on fuzzy text grep, agents navigate code using high-level structural semantics:

- **Direct Single-Shot Symbol Reads (`acdd_read`)**: When an agent knows a declaration name, it reads the complete, digest-verified implementation in a single call via `acdd_read(selector="SymbolName")` (or `acdd_read(selector="path:SymbolName")`). This completely replaces wasteful two-step text grepping and partial line scanning.
- **Deep Relationship Graph**:
  - `code_map_search`: Find declarations by exact name, prefix pattern (`parse*`), or semantic query.
  - `code_map_inspect`: Inspect full signatures, docstrings, callers, callees, and linked documentation.
  - `code_map_explain` & `code_map_impact`: Calculate the blast radius of proposed changes across call and reference hierarchies.
  - `code_map_tests`: Identify covering tests reaching a symbol or discover production components exercised by a test.
  - **Graph Hygiene & Safe Deletion (`code_map_query`, `code_map_prove_removal`)**:
    - `code_map_query(operation="duplicates")`: Detect exact and near-duplicate function/method clones (`level="symbol"`) and identical files (`level="file"`).
    - `code_map_query(operation="dead_tails")`: Compute workspace-wide production liveness and isolate unreachable `phantom_tested`, `dead_tails`, and `dangling_calls` before delivery or review.
    - `code_map_query(operation="untested")`: Page production functions/methods lacking direct indexed test `calls` edges.
    - `code_map_prove_removal`: Statically evaluate incoming references and target-scoped diagnostics before removing symbols.
- **Syntactic Pattern Search (`ast_grep_search`)**: Match structural AST patterns (e.g. `fn $NAME($ARGS) -> Result<$T> { $$$ }`) across admitted source files.
- **Multi-Language & Multi-Repo Federation**: Built-in support for Python, Rust, TypeScript, JavaScript, Vue, HTML, Protocol Buffers, YAML, and TOML (extended with `--features all-languages` for Go, Java, C#, Kotlin, PHP, Ruby, C, C++).
- **Linked Workspaces & Cross-Repository Indexing**: ACDD natively supports multi-repository architectures through `linked_workspaces` in `acdd.yaml`. Sibling repositories are integrated into the unified code graph for cross-workspace symbol resolution, import linking, and cross-repo impact analysis:
  ```yaml
  # acdd.yaml: federate external repositories into the code map
  linked_workspaces:
    - name: shared-core
      path: ../shared-core
      roots: [src]
      docs: [docs]
      ignore: [target, node_modules]
  ```

---

### 2. Living Documentation Alongside Code

Documentation in ContextUnity ACDD is not an afterthought stored in disconnected wikis; it is indexed into the very same SQLite graph as the source code:

- **BM25 Searchable Section Chunks**: Markdown files under configured `docs` paths are parsed at `##` and `###` headings into distinct `DocSection` units. Agents search technical documentation using full-text BM25 queries (`search_docs`) filtered by `doc_type` (such as `architecture`, `guide`, `plan`, `adr`).
- **Bidirectional Symbol Traceability**: References to code declarations (e.g. `` `TokenValidator` `` or `` [`TokenValidator`](src/auth/token.rs) ``) are resolved against the AST code map. When unique matches occur, ContextUnity ACDD creates verified edges connecting documentation sections directly to code declarations.
- **Automatic Invariant Surfacing & Executable Rules (`INV-*` / `acdd-rule`)**:
  - Architectural invariants defined in Markdown GitHub alert callouts (`> [!IMPORTANT] Invariant: ...`) containing `INV-*` identifiers are indexed as governance rules.
  - Fenced `acdd-rule` YAML blocks alongside `INV-*` declarations (and `acdd.yaml` `architecture: { zones, rules }`) turn architectural boundaries into machine-enforced rules evaluated by `acdd lint` / `code_map_lint` and surfaced with `[ENFORCED]` in `ClaimResp`.
  - Whenever an agent inspects a symbol (`code_map_inspect`) or performs an impact analysis (`code_map_impact`), ContextUnity ACDD automatically surfaces the governing invariants directly in the agent's context. Agents are alerted to architectural constraints *before* writing code.

---

### 3. ACDD — Lifecycle State Machine and Orchestration

**Augmented Context & Durable Dispatch (ACDD)** replaces open-ended, unstructured coding loops with a deterministic verification state machine (`crates/acdd-state` + `crates/acdd-core`). It binds Git-tracked specifications (`docs/milestones/*.md`, `docs/plans/*.md`, or ephemeral `--goal` specs), persistent SQLite state (`.acdd/state.sqlite`), and declarative YAML `GateProfile` v2 topologies so every worker receives only the context relevant to its active gate:

- **Composable Gates & Universal Profiles**: Workflows are assembled from hierarchical role, command, check, action, and container gates (`blueprint_synthesis` $\to$ `tasks: build` $\to$ `review` $\to$ `deliver` $\to$ `run_tests` $\to$ `integration_review` $\to$ `handoff`, with bounded `rearchitect` / `polish` repair routes). Four embedded baselines exist under `crates/acdd-state/src/profile/embedded/` (`base`, `code`, read-only review/SAST `audit`, and compiler-free `documentation`); `.acdd/profile.yaml` automatically becomes the workspace default when present, overlaying commands and roles with `gates_patch:` (or replacing gates via `gates:` / `inherit: none`), and optional `save.report` templates write explicit Markdown/JSON reports on pass.
- **Topological DAG Waves & Declarative Git Policy**: Task dependencies (`depends_on`) resolve into execution waves governed by declarative `git:` (`isolation: worktree | in_place`, `worktrees: manual | managed`) and `save:` (`commit | none`) policies. Automated `acdd run` waves land completed task branches internally via `rebase-ff`, and terminal verification checks content equivalence by Git `TreeOid` (`TASK_NOT_INTEGRATED`).
- **Session-Preserving Repair Loops & Dynamic Context**: Every gate claim (`acdd_claim`) delivers tailored `guidance`, a 6-tier budgeted `ClaimResp` envelope (`contract` + `guidance`, `unresolved_findings`, `pre_task_slice`, `dependency_receipts`, `blackboard`, and dynamic `context_lenses` such as `code.external_callers`), and persistent `acdd_blackboard` notes; reviewer rejections resume the builder session with structured findings intact.
- **Unified `GateProfile` + `Run` Execution**: Ephemeral `--goal` runs, `in_place` plans, taskless root-gate workflows, and multi-task `worktree` specs all execute through one unified state machine and flat `30`-command CLI (`acdd start`, `acdd claim`, `acdd verify`, `acdd submit`, `acdd lint`, `acdd run`).

> [!TIP] Complete ACDD, Profile & Tool References
> - [**ACDD Architecture & Default Profile Guide (`docs/reference/acdd.md`)**](docs/reference/acdd.md) — full explanation of ACDD, how gates and universal profiles work, and a gate-by-gate walkthrough of the embedded `base`/`code`/`audit`/`documentation` profiles.
> - [**ACDD Profiles Reference (`docs/reference/profiles.md`)**](docs/reference/profiles.md) — `version: 2` profile schema, `git:` policy, command pattern rules, and validation constraints.
> - [**MCP Tools Reference (`docs/reference/mcp-tools.md`)**](docs/reference/mcp-tools.md) — full reference for all 20 built-in MCP tools and `DomainProvider` plugin tools exposed to AI agents.
> - [**CLI Reference (`docs/reference/cli.md`)**](docs/reference/cli.md) — complete reference for all flat command-line operations and terminal automation.
> - [**State Lifecycle Reference (`docs/reference/tasks.md`)**](docs/reference/tasks.md) — in-depth guide to `Run` / `GateNode` state machines, blackboard, and evidence envelopes.
> - [**Product Roadmap & Milestones (`ROADMAP.md`)**](ROADMAP.md) — release direction, active milestone commitments, and execution waves (`docs/milestones/`).

---

## Architectural Principles

- **Zero-Daemon, Local-First**: No background daemons, cloud dependencies, or idle RAM consumption. The server starts instantly over stdio or CLI, incrementally updates `.acdd/code-map.sqlite`, and releases resources immediately.
- **Strict Context Budgeting**: All tool outputs are formatted for high signal-to-noise ratio in Markdown or JSON. Output sizes are bounded by configurable limits (default 64–96 KiB) to prevent context window saturation.
- **Branch-Local State Isolation**: Each Git worktree maintains its own `.acdd/code-map.sqlite` and configuration, while linked worktrees share the task database to coordinate milestone execution safely.

```text
repository files
       │
       ▼
 acdd.yaml          which roots, docs, milestones, and linked repos
       │
       ├──► .acdd/code-map.sqlite     symbols, AST edges, docs sections, invariants
       │
       └──► .acdd/state.sqlite        runs, gate_nodes, claims, traces, blackboard
                   ▲
                   │
       specs & .acdd/profile.yaml      reviewed specs & GateProfile v2 committed in Git
```

---

## Dual-Surface Interface: MCP Tools and CLI

Every core operation is available both to AI agents over MCP and to developers/scripts via the CLI (detailed in [**`docs/reference/mcp-tools.md`**](docs/reference/mcp-tools.md) and [**`docs/reference/cli.md`**](docs/reference/cli.md)):

| Capability | MCP Tool (AI Agents) | CLI Command (Terminal / Automation) |
|---|---|---|
| **Build / Refresh Graph** | Automatic on first call & file change | `scan`, `build`, `delta` |
| **Search Symbols** | `code_map_search` | `query search` |
| **AST Pattern Match** | `ast_grep_search` | `ast grep` |
| **Inspect Declarations** | `code_map_inspect` | `query inspect` |
| **Read Symbol Source** | `acdd_read(selector="...")` | `read --selector "..."` |
| **Blast Radius & Impact** | `code_map_impact`, `code_map_explain` | `query impact`, `query explain` |
| **Test Coverage Seams** | `code_map_tests` | `query tests` |
| **Graph Hygiene & Queries** | `code_map_query` (`duplicates`, `dead_tails`, `untested`, `sql`) | `query run <op>`, `query sql` |
| **Unified Arch & Docs Lint** | `code_map_lint` | `lint` |
| **Safe Deletion Analysis** | `code_map_prove_removal` | `query remove` |
| **Documentation Search** | `search_docs`, `get_doc` | `docs search`, `docs get`, `docs check` |
| **Run Queue, Roadmap & Claims** | `acdd_state`, `acdd_claim`, `acdd_submit` | `init`, `draft`, `start`, `status`, `list`, `show`, `next`, `roadmap`, `claim`, `submit` |
| **Gate Verification** | `acdd_verify` | `verify` |
| **Blackboard Messaging** | `acdd_blackboard` | `blackboard` |
| **Subtask Tracking** | `acdd_state(action: "subtask")` | `subtask` |
| **Scope, Sync, Reopen & Clear** | `acdd_state(action: "extend_scope" \| "sync" \| "reopen" \| "clear")` | `extend-scope`, `sync`, `reopen`, `clear` |
| **Headless Run & Plugins** | `acdd_state(action: "run")`, `<domain>_<name>` | `run`, `tool <domain>_<name>`, `validate` |
| **Built-in Reference** | `acdd_guide` | `guide` |

---

## Installation

Install ContextUnity ACDD binary, companion skills, and project client configuration in one command:

### Quick Install (`install.sh`)

```bash
# Latest stable release (recommended)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh

# Nightly pre-release (active development & latest milestone features)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh -s -- --nightly
```

If you cloned the repository locally:
```bash
./install.sh             # Interactive prompt: choose between latest and nightly
./install.sh --latest    # Force latest stable release
./install.sh --nightly   # Force nightly pre-release
```

### What `install.sh` Does Automatically
1. **Auto-Detects Platform**: Identifies OS and architecture (Linux `x86_64`/`aarch64`, macOS `x86_64`/`arm64`). No Rust toolchain, Python, Node, or C compiler required.
2. **Cryptographic Verification**: Downloads pre-built stripped binaries and verifies SHA-256 integrity against `SHA256SUMS`.
3. **Atomic Binary Placement**: Installs the binary into `~/.local/bin/acdd`.
4. **Agent Skills Hub**: Syncs companion skills to `~/.agents/skills/` and symlinks them to detected agent directories (Antigravity/Gemini CLI, OpenAI Codex, Anthropic Claude, Cursor).
5. **Project MCP Configuration**: Atomically creates or updates `.mcp.json` in the current working directory.
6. **Live Reload**: If upgrading an existing installation, gracefully signals running MCP instances.

### Recommendations
- **Add to `PATH`**: Ensure `~/.local/bin` is in your shell `PATH`:
  ```bash
  export PATH="$HOME/.local/bin:$PATH"
  ```
- **Verify Installation**: Confirm by checking the version:
  ```bash
  acdd --version
  ```
- **Channel Selection**:
  - Use **`latest`** (default) for stable, production coding workflows.
  - Use **`--nightly`** when you need unreleased milestones, latest prompt optimizations, or recently merged bug fixes.

---

> [!TIP] Comprehensive Installation & Configuration Guide
> For detailed instructions, building from source (`cargo install`), multi-agent IDE configurations (Antigravity, Codex, Claude, Cursor), air-gapped/enterprise installations, and workspace initialization, see the dedicated [**Installation and Operational Guide (`INSTALL.md`)**](INSTALL.md).

---

## Companion Skills

Companion skills teach AI agents how to leverage ContextUnity ACDD MCP tools, structure tests, handle review rejections, and maintain documentation:

| Skill | Purpose |
|---|---|
| [`acdd`](skills/acdd/SKILL.md) | Primary operational skill: code navigation, single-shot symbol reads, task claims, blackboard, delivery |
| [`contract-planning`](skills/contract-planning/SKILL.md) | Architectural viability audit, ADR/invariant reconciliation, and contract synthesis |
| [`test-suite-rules`](skills/test-suite-rules/SKILL.md) | Production-seam tests, table-driven matrices, harness hygiene, and test-suite refactoring |
| [`close-review-findings`](skills/close-review-findings/SKILL.md) | Diagnosing review rejections and implementing evidence-closed remediations |
| [`agent-instructions`](skills/agent-instructions/SKILL.md) | Authoring and updating `AGENTS.md`, `SKILL.md`, and documentation |

---

## Contributing and Verification

We welcome contributions, bug fixes, documentation improvements, language profiles, and framework manifests.

> [!NOTE]
> - **Extended Language Scope**: While default languages are fully stable, the extended language profiles (`--features all-languages` covering Go, Java, C#, Kotlin, PHP, Ruby, C, C++) need further refinement and community contributions to improve language linkers and AST extractions.

Before submitting changes, ensure all verification gates pass:

```bash
# Code formatting
cargo fmt --check

# Strict Clippy analysis across workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Commitment integrity, workspace library unit tests, and integration tests
cargo test --test commitment_integrity
cargo test -p acdd-core --lib && cargo test -p acdd-state --lib
cargo test --workspace --lib
cargo test --workspace --all-targets

# Docs and configuration/profile validation
cargo run -- docs check
cargo run -- validate
```

Refer to [TESTS.md](TESTS.md) for domain-specific verification commands and count rules, [docs/AGENTS.md](docs/AGENTS.md) for documentation authoring governance, [ROADMAP.md](ROADMAP.md) for planned milestones, and [CHANGELOG.md](CHANGELOG.md) for release notes.

---

## License

ContextUnity ACDD is licensed under the [MIT License](LICENSE).
