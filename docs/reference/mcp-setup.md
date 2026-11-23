---
doc_type: guide
title: MCP setup
---

# MCP setup

ContextUnity ACDD runs as both a local stdio MCP server and a flat CLI in one binary:

- **Default / `serve` mode**: running `acdd` with no subcommand (or `acdd serve`) starts the stdio MCP server.
- **Workspace root**: uses the process working directory unless `--root <PATH>` or `ACDD_WORKSPACE_ROOT` selects another root.

## Install the binary & companion skills

Run the installer from the project directory whose `.mcp.json` you want configured:

```sh
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh
```

What `install.sh` requires and verifies:

- **Zero compiler dependencies**: uses POSIX `sh`, `tar` (gzip), `curl` or `wget`, and `sha256sum` or `shasum` (no Rust, Python, Node, or `jq` needed).
- **Supported platforms**: Linux (`x86_64`, `aarch64`) and macOS (`x86_64`, `arm64`).
- **SHA-256 verification**: verifies the archive against `SHA256SUMS` before extracting; unsupported platforms or checksum mismatches abort immediately.
- **Atomic binary placement**: stages and atomically renames the executable into `~/.local/bin/acdd` (prints a notice if `~/.local/bin` is not yet on `PATH`).
- **Companion agent skills**: runs bundled `skills/install.sh`, syncing the 5 companion skills into `~/.agents/skills` (overridable via `AGENTS_SKILLS_DIR`) and symlinking them into detected Claude, Codex, Gemini/Antigravity, and Cursor skill folders (`--copy` and `--dry-run` are also supported).
- **Safe `.mcp.json` update**: sets the ACDD `command` and `type` fields in `$PWD/.mcp.json` while preserving other servers and custom `args`, `env`, or `cwd` fields (rejects malformed JSON or symlinked config files without overwriting).
- **Live MCP reload**: when updating an existing install, runs `acdd reload` (`SIGHUP`) so active `acdd serve` sessions reload in place while keeping their stdio pipes open.

Select release channel or a pinned release mirror:

```sh
# Stable release (default)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh

# Nightly pre-release
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh | sh -s -- --nightly

# Interactive TTY channel prompt
sh install.sh

# Pinned release tag or internal mirror (must host SHA256SUMS + target tarball)
curl -fsSL https://raw.githubusercontent.com/ContextUnity/contextunity-acdd/main/install.sh -o /tmp/acdd-install.sh
ACDD_RELEASE_BASE_URL="https://github.com/ContextUnity/contextunity-acdd/releases/download/<tag>" sh /tmp/acdd-install.sh
```

### Build from source instead

```sh
cargo install --path . --locked
./skills/install.sh
command -v acdd
```

- **Toolchain**: requires stable Rust and a C compiler (`gcc` or `clang`) for bundled SQLite and Tree-sitter grammars.
- **Optional languages**: append `--features all-languages` to compile Go, Java, C#, Kotlin, PHP, Ruby, C, and C++ profiles alongside the default languages.

## Configure a client

For `.mcp.json` at the root of your repository:

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

If your client launches MCP servers outside the repository directory, set `cwd` explicitly:

```json
{
  "mcpServers": {
    "acdd": {
      "type": "stdio",
      "command": "/absolute/path/to/acdd",
      "cwd": "/absolute/path/to/repository"
    }
  }
}
```

For clients without a `cwd` field, pass `--root` in `args`:

```json
{
  "mcpServers": {
    "acdd": {
      "type": "stdio",
      "command": "/absolute/path/to/acdd",
      "args": ["--root", "/absolute/path/to/repository"]
    }
  }
}
```

- **Portable `.mcp.json` vs. VS Code `.vscode/mcp.json`**: `.mcp.json` uses top-level `mcpServers`, whereas `.vscode/mcp.json` uses top-level `servers` (see [VS Code's MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)).

## First request and subsequent reads

- **Automatic index creation**: reconnect your client and call `code_map_overview` — the first database-backed call builds `<root>/.acdd/code-map.sqlite` automatically (no manual `acdd build` required for MCP).
- **Automatic incremental refresh**: before subsequent MCP reads, ACDD checks `adapter.digest` and file inventory, running incremental `delta` or full rebuilds automatically when source files change.
- **Executable drift guard (`ExecutableIdentity`)**: before executing any cold rebuild or incremental reindex in `Server::admit`, `Server::rebuild`, or `Server::delta` (and under the `cold.rs` generation lock before replacing `.acdd/code-map.sqlite`), `acdd serve` verifies its startup running-image hash and bound launch pathname (`/proc/self/exe` + `argv[0]` on Linux, `PROC_PIDREGIONPATHINFO` vnode checks + `std::env::current_exe()` on macOS, fail-closed on unsupported platforms). If the on-disk binary changed, it emits `BINARY_DRIFT_RELOAD` and performs a live re-exec after staged cleanup and JSON-RPC flush (or fails closed with `BINARY_DRIFT_UNAVAILABLE` if startup binding failed or the binary is missing).
- **Optional `worktree` selector**: all 20 built-in MCP tools (and `<domain>_<name>` plugin tools) accept `worktree` to query a secondary Git worktree; omitting it always targets the server root (see [worktree selection](mcp-tools.md#worktree-selection)).
- **Shared state across worktrees**: each worktree keeps its own `.acdd/code-map.sqlite`, while all linked Git worktrees share `<primary_worktree>/.acdd/state.sqlite`.
- **Session lifetime**: the stdio server never times out on inactivity, survives `SIGHUP` reloads, and exits cleanly on Unix when its parent client process exits.
- **First queries to try**: run `code_map_search` with `{"pattern":"parse*"}`, inspect a returned symbol via `code_map_inspect`, and check callers via `code_map_impact` with `depth=1` (see [MCP tool reference](mcp-tools.md)).

## If the server does not appear

- **Executable not found**: run `command -v acdd` in a terminal and put that absolute path in `command`.
- **Wrong repository**: check the workspace path reported by `code_map_overview` and set `cwd` or `--root` explicitly.
- **Configuration error**: inspect the MCP client's server log and run `acdd validate` in the repository root.
- **CLI results look stale**: CLI commands read the on-disk index directly — run `acdd build .` or `acdd delta` after edits (MCP refreshes automatically).

See [Workspace configuration](configuration.md) for `acdd.yaml` options and [limits and freshness](../runbooks/limits-and-freshness.md) for scan and response budgets.
