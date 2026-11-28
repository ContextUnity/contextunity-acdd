# ContextUnity ACDD

[![Documentation](https://img.shields.io/badge/docs-acdd.contextunity.com-00d2ff.svg)](https://acdd.contextunity.com)
[![Rust](https://img.shields.io/badge/rust-2021%20edition-blue.svg)](https://www.rust-lang.org)
[![MCP + CLI](https://img.shields.io/badge/surface-MCP%20%2B%20CLI-10b981.svg)](https://modelcontextprotocol.io)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Augmented Context & Dispatch Discipline (ACDD)** is a local-first, zero-daemon **meta-harness** for AI coding agents (Claude Code, Codex CLI, Antigravity, Cursor) and CI pipelines — packaged into a single pure-Rust binary.

One executable (`acdd`) serves both as a **stdio [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server** (18 built-in tools + plugin SPI) and as a **flat CLI**, giving agents an incremental Tree-sitter code & documentation graph, a machine-enforced architectural linter, and a deterministic gate state machine.

**[Website & Docs](https://acdd.contextunity.com)** · **[5-Minute Quickstart](https://acdd.contextunity.com/docs/getting-started/quickstart/)** · **[Install Guide](INSTALL.md)** · **[Architecture](docs/architecture/README.md)** · **[Roadmap](ROADMAP.md)**

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://acdd.contextunity.com/assets/acdd-architecture-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="https://acdd.contextunity.com/assets/acdd-architecture-light.svg">
    <img alt="ContextUnity ACDD — Meta-Harness Architecture &amp; Verified Gate Pipeline" src="https://acdd.contextunity.com/assets/acdd-architecture-dark.svg" width="100%">
  </picture>
</p>

---

## Why ContextUnity ACDD?

Unguided coding agents waste tokens in blind `grep` loops, erode module boundaries, and claim tasks are "done" without independent verification. ACDD sits above your agent harness and provides three local SQLite-backed engines with zero background daemons or cloud dependencies:

1. **AST Code & Living Docs Graph (`.acdd/code-map.sqlite`)**
   - Single-shot, SHA-256 verified symbol reads (`acdd_read(selector="...")`) instead of multi-step file scanning.
   - Multi-hop caller/callee blast radius (`code_map_impact`), covering-test and removal-safety queries (`code_map_query(operation="tests" | "prove_removal")`), read-only diagnostics (`code_map_analyze`), and Tree-sitter pattern search (`ast_grep_search`).
   - Bidirectional linking between code symbols, BM25-searchable Markdown sections (`search_docs`), and `INV-*` architectural invariants — across single repos or federated `linked_workspaces`.
2. **Architectural & Graph Linter (`acdd lint` / `code_map_lint`)**
   - Enforces zone dependency rules (`acdd.yaml`), executable `acdd-rule` blocks embedded directly inside Markdown docs, Tree-sitter syntax health, doc-link/symbol integrity, duplicate clones, dead code, and import/call cycles.
3. **Deterministic Gate State Machine (`.acdd/state.sqlite`)**
   - Turns ad-hoc goals (`--goal`) and versioned Markdown specs (`docs/milestones/*.md`) into verified DAG pipelines (`synthesize` → `build` → `independent review` → `deliver` → `handoff`).
   - Injects a budgeted 6-tier `ClaimResp` Markdown contract per gate and isolates parallel agent waves inside managed Git `.worktrees/`.

---

## Quick Start

### 1. Install binary & companion agent skills

Runs in POSIX `sh` on Linux (`x86_64`, `aarch64`) and macOS (`x86_64`, `arm64`) with SHA-256 verification — no Rust, Python, or Node required:

```bash
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh
```

`install.sh` installs `~/.local/bin/acdd`, syncs the 5 companion `SKILL.md` playbooks into `~/.agents/skills/` (auto-symlinked to Claude Code, Codex, Antigravity/Gemini, and Cursor), and configures `.mcp.json` (`--nightly` installs rolling pre-releases; see [`INSTALL.md`](INSTALL.md) for `cargo install`).

### 2. Initialize, index & lint any Git repository

```bash
acdd init                        # Generates starter acdd.yaml, .acdd/workflows/default.yaml & .gitignore rules
acdd build .                     # Builds .acdd/code-map.sqlite (MCP refreshes it automatically)
acdd query overview              # Summarizes languages, entrypoints, and indexed docs
acdd lint --categories all       # Checks architecture, docs, syntax, clones, dead code & cycles
```

### 3. Run a verified workflow

```bash
# Option A: Everyday bugfix or focused feature (no spec file needed)
acdd start --goal "Reject expired JWTs before DB lookup" --scope src/auth/ --scope tests/

# Option B: Multi-task specification (parallel waves in isolated Git worktrees)
acdd draft milestones jwt-rotation --title "JWT Key Rotation"
acdd start docs/milestones/AC-001-jwt-rotation.md --worktree
```

---

## Architectural & Graph Linter

`acdd lint` (MCP: `code_map_lint`, gate check: `code.lint`) evaluates rules from `.acdd/lint/*.yaml` (configured via `lint_dirs:` in `acdd.yaml`) and executable fenced `acdd-rule` blocks inside Markdown documentation in one pass over `.acdd/code-map.sqlite`:

| Category | Scope | What It Enforces |
| --- | --- | --- |
| **`arch`** | Default | 8 boundary primitives: `allow_only_dependencies`, `forbid_dependency`, `single_owner`, `boundary_entry`, `ast_pattern`, `forbid_cycles`, `must_implement`, `doc_integrity` |
| **`docs`** | Default | Frontmatter validity, relative Markdown links & `#heading` anchors, verified code-symbol claims, duplicate `INV-*` IDs, and orphan invariants |
| **`syntax`** | Default | Tree-sitter parse diagnostics across all admitted source files (`syntax.parse`) |
| **`duplicates`** | `--categories all` | Exact and structural function/method clones (`level: symbol`) and duplicate files (`level: file`) |
| **`dead_tails`** | `--categories all` | Unreachable production code (`unreachable`), test-only helpers (`phantom_tested`), and unresolved internal calls (`dangling_call`) |
| **`cycles`** | `--categories all` | Strongly connected call and import cycles across modules (`cycles.scc`) |

Pair prose invariants with machine-checked `acdd-rule` blocks so rules stay co-located with architectural docs and surface with `[ENFORCED]` inside agent gate prompts:

````markdown
> [!IMPORTANT]
> - INV-HTTP-SINGLE-OWNER: Outbound HTTP clients are confined to `src/http_client/`.

```acdd-rule
id: INV-HTTP-SINGLE-OWNER
invariant: INV-HTTP-SINGLE-OWNER
severity: error
kind: single_owner
from: { paths: ["src/**"] }
to: { external_symbols: ["reqwest::Client"] }
owner_paths: ["src/http_client/**"]
```
````

---

## Supported Languages & Frameworks

ACDD compiles native Tree-sitter grammars and static linkers directly into the binary (with Markdown/MDX documentation always indexed alongside code):

| Tier | Build Flag | Supported Languages |
| --- | --- | --- |
| **Default (13 languages)** | Included in prebuilt releases & `cargo build` | **Python** (`.py`, `.pyi`), **Rust** (`.rs`), **TypeScript** (`.ts`, `.tsx`, `.mts`, `.cts`), **JavaScript** (`.js`, `.jsx`, `.mjs`, `.cjs`), **Vue** (`.vue`), **HTML / Jinja2** (`.html`, `.htm`, `.j2`), **SQL** (`.sql`), **Protocol Buffers** (`.proto`), **Bash / Shell** (`.sh`, `.bash`, `.zsh`), **CSS** (`.css`), **JSON** (`.json`), **YAML** (`.yaml`, `.yml`), **TOML** (`.toml`) |
| **Extended (+8 languages, 21 total)** | `--features all-languages` | **Go** (`.go`), **Java** (`.java`), **C#** (`.cs`), **Kotlin** (`.kt`, `.kts`), **PHP** (`.php`), **Ruby** (`.rb`), **C** (`.c`, `.h`), **C++** (`.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`) |

- **Bundled Framework Packs**: FastAPI, Starlette, Django, SQLAlchemy, HTMX, Alpine.js, Vue / Pinia, Nuxt, Playwright, WebIDL/DOM, Rust `sqlx` / Cargo, Python `unittest`, and GridViewSpec.
- **Zero-Rebuild Project Overrides**: Extend or override route, DI, ORM-table, or template rules per repository under `.acdd/frameworks/*.{toml,yaml}` (see [`docs/reference/framework-manifests.md`](docs/reference/framework-manifests.md)).

---

## Dual-Surface Parity: MCP Tools & CLI

Every capability runs over the exact same Rust engine whether called by an AI agent over stdio MCP (`acdd serve`) or from terminals and CI (`acdd <command>`):

| Area | MCP Tools (`acdd serve`) | CLI Commands (`acdd ...`) |
| --- | --- | --- |
| **Index & Refresh** | Auto `delta` / `build` on file changes | `scan`, `build`, `delta` |
| **Symbol & Graph Navigation** | `code_map_overview`, `search`, `inspect`, `explain`, `impact` | `query overview \| search \| inspect \| explain \| impact` |
| **Hygiene, Diagnostics & AST** | `code_map_query` (`tests`, `prove_removal`, `duplicates`, `dead_tails`, `untested`, `slice`, `sql`), `code_map_analyze` (`summary`, `hotspots`, `boundary`, `path`, `entrypoints`, `diff_impact`, `cochange`), `ast_grep_search` | `query tests \| remove`, `query run <op>`, `query sql`, `query analyze [--mode]`, `ast grep` |
| **Lint & Living Docs** | `code_map_lint`, `search_docs`, `get_doc` | `lint`, `docs search \| get \| check` |
| **Layered File & Symbol Reader** | `acdd_read` (`snippet`, `lines`, `search`, `delta`, `outline`) | `read` |
| **Run Queue, Roadmap & Snapshots** | `acdd_state` (active runs: `start`, `status`, `list`, `show`, `next`, `context`, `snapshot`, `run`, ...) | `init`, `draft`, `start`, `status`, `list`, `show`, `next`, `roadmap`, `context`, `snapshot`, `run` |
| **Gate Execution & Blackboard** | `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard` | `claim`, `verify`, `submit`, `blackboard` |
| **Guide, Validation & Plugins** | `acdd_guide`, `<domain>_<name>` | `guide`, `validate`, `tool <domain>_<name>` |

Full schemas and flags: **[MCP Tools Reference](docs/reference/mcp-tools.md)** · **[CLI Reference](docs/reference/cli.md)** · **[Workflows Reference](docs/reference/workflows.md)**.

---

## Built-in Workflows & Companion Skills

- **3 Core Embedded `WorkflowDef` (`version: 2`) Workflows** (customizable in `.acdd/workflows/default.yaml` via `inherit:` and `gates_patch:`, with `audit`, `spec_planning`, and `hotfix` templates in [`skills/acdd/references/acdd-workflow-examples/`](skills/acdd/references/acdd-workflow-examples/README.md)):
  - **`code`**: Full engineering lifecycle (`blueprint_synthesis` → parallel `tasks.{build, review, deliver, rearchitect}` → `run_tests` → `integration_review` → `handoff`).
  - **`base`**: Minimal worktree + atomic commit foundation for custom gate pipelines.
  - **`documentation`**: Compiler-free living docs & `acdd-rule` verification workflow.
  - **Workflow Templates (`skills/acdd/references/acdd-workflow-examples/`)**: Copyable `audit` (read-only `recon` → `analyze` → `triage`), `spec_planning` (read-only spec synthesis & review), and `hotfix` (localized bugfix) overlays.
- **5 Bundled Companion Agent Skills (`skills/`)**:
  - [`acdd`](skills/acdd/SKILL.md) (MCP navigation & self-claimed gates), [`contract-planning`](skills/contract-planning/SKILL.md) (blueprint & ADR audit), [`test-suite-rules`](skills/test-suite-rules/SKILL.md) (production-seam testing), [`close-review-findings`](skills/close-review-findings/SKILL.md) (multi-surface root-cause fixes), and [`agent-instructions`](skills/agent-instructions/SKILL.md) (`AGENTS.md` & indexed docs).

---

## Roadmap

Strategic direction and active specifications are tracked in **[`ROADMAP.md`](ROADMAP.md)** and [`docs/milestones/`](docs/milestones/README.md) (for shipped releases through `v0.7.1`, see [`CHANGELOG.md`](CHANGELOG.md)):

- **`v0.8.0` (Next — Universal Specs, Swarm & Engine Changes)**:
  - **Universal Spec Schemas, Named Path Sets & Memory Synthesis (`AC-006`)**: Profile-declared `specs:` schemas, named `paths.*` sets, `spec_deltas` verification, and spec-driven `memory_synthesis` ([ADR 0014](docs/adr/0014-declarative-spec-schemas-and-items.md)).
  - **Unified Swarm Gates, Peer Board & Post-Delivery Engine Audit (`AC-005`)**: Unified `linear | parallel | swarm` gate execution, coordinator groups, `@mention` + zero-token `wait` blackboard threads, grouped `save: handoff`, and end-to-end terminal gate transition audit.
- **Later & Future Milestones**:
  - **Local Observability & Zero-Daemon Reactive Core (`AC-007`, `AC-008`)**: Read-only Live Gate Board Web UI, Prometheus/OTLP telemetry, `manual` deferred approvals, and zero-idle-RAM `acdd tick` timers & durable event inbox.
  - **Authenticated Ingress & Domain Crates (`AC-009`)**: Streamable HTTP MCP, interactive Web UI steering, HMAC webhook ingress, and modular `crates/acdd-domain-*` verification plugins.

---

## Contributing & Verification

Contributions to language profiles, framework manifests, state/index performance, and documentation are welcome. Run the workspace verification gates before opening a pull request:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --test commitment_integrity
cargo test --workspace --all-targets
cargo run -- docs check
cargo run -- validate
```

See [`TESTS.md`](TESTS.md) for scoped verification rules, [`docs/README.md`](docs/README.md) for full architecture and API documentation, and [`CHANGELOG.md`](CHANGELOG.md) for release history.

## License

Licensed under the [MIT License](LICENSE).
