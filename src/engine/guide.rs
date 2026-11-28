//! Built-in CLI and MCP guidance topics and workspace initialization (`acdd init`).

use acdd_core::config::load_adapter;
use acdd_core::git::git_command;
use acdd_index::scanner;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Canonical `acdd.yaml` template.
const TEMPLATE: &str = "\
preset: full
roots:
  - .
ignore:
  - target
  - node_modules
  - .venv
  - .git
specs:
  milestones:
    path: docs/milestones
    prefixes: [AC]
    digits: 3
  plans:
    path: docs/plans
    prefixes: [PLAN]
    digits: 3
workflows_dirs:
  - .acdd/workflows
state_db: .acdd/state.sqlite
retain_traces: 20
";

/// Canonical `.gitignore` block ensuring `.acdd/workflows/**`, `.acdd/specs/**`, `.acdd/lint/**`, and `frameworks/**` remain tracked while runtime state/locks/traces stay ignored.
const CANONICAL_GITIGNORE_BLOCK: &str = "\
/.worktrees/
!/.acdd/
!/.acdd/workflows/
!/.acdd/workflows/**
!/.acdd/specs/
!/.acdd/specs/**
!/.acdd/lint/
!/.acdd/lint/**
!/.acdd/frameworks/
!/.acdd/frameworks/**
.acdd/*.sqlite*
.acdd/*.lock
.acdd/*.log
.acdd/*.json
.acdd/*.jsonl
.acdd/reports/
.acdd/run/
.acdd/tmp/";

const TOPICS: &[(&str, Option<&str>)] = &[
    ("query", None),
    ("init", None),
    ("adapter", None),
    ("validate", None),
    ("code", Some("code")),
    ("ast", Some("code")),
    ("docs", Some("docs")),
    ("lifecycle", Some("state")),
    ("acdd", Some("state")),
    ("gates", Some("state")),
    ("subtasks", Some("state")),
];

/// Produce guide content using the invocation's selected configuration.
pub fn run(root: &Path, topic: &str, force: bool) -> Result<Value> {
    let root = scanner::canonical_root(root)?;
    if topic.trim() == "init" {
        return initialize(&root, force);
    }
    let explicit_config = std::env::var_os("ACDD_CONFIG").map(std::path::PathBuf::from);
    let adapter = scanner::load_adapter(&root, explicit_config.as_deref())?;
    run_with_adapter(&root, topic, force, &adapter)
}

/// Produce the same guide from an already resolved CLI or MCP adapter.
pub fn run_with_adapter(
    root: &Path,
    topic: &str,
    force: bool,
    adapter: &scanner::Adapter,
) -> Result<Value> {
    run_with_adapter_mode(root, topic, force, false, adapter)
}

/// Produce guide content using an already resolved adapter, optionally formatted for CLI-only callers (`cli_only`).
pub fn run_with_adapter_mode(
    root: &Path,
    topic: &str,
    force: bool,
    cli_only: bool,
    adapter: &scanner::Adapter,
) -> Result<Value> {
    let root = scanner::canonical_root(root)?;
    let topic = topic.trim();
    let topic = if topic.is_empty() { "query" } else { topic };
    if topic == "init" {
        return initialize(&root, force);
    }
    let enabled = |module: Option<&str>| module.is_none_or(|m| adapter.modules.is_enabled(m));
    if TOPICS
        .iter()
        .any(|(name, module)| *name == topic && !enabled(*module))
    {
        return Ok(json!({}));
    }
    let mut value = if cli_only {
        topic_value_cli(&root, topic, adapter)?
    } else {
        topic_value(&root, topic, adapter)?
    };
    if let Some(fields) = value.as_object_mut() {
        if topic == "query" {
            fields.insert(
                "topics".into(),
                json!(TOPICS
                    .iter()
                    .filter(|(_, module)| enabled(*module))
                    .map(|(topic, _)| *topic)
                    .collect::<Vec<_>>()),
            );
            if !adapter.modules.is_enabled("code") {
                for key in [
                    "start",
                    "sql_schema",
                    "symbols",
                    "snippets",
                    "tests",
                    "queries",
                    "diagnostics",
                    "recovery",
                ] {
                    fields.remove(key);
                }
            }
            if !adapter.modules.is_enabled("docs") {
                fields.remove("documents");
            }
            if !adapter.modules.is_enabled("code") && !adapter.modules.is_enabled("docs") {
                fields.remove("pages");
            }
        }
        if !adapter.modules.is_enabled("blackboard") {
            for key in [
                "blackboard_topics",
                "blackboard_gate_targeting",
                "blackboard_schema_version",
                "blackboard",
            ] {
                fields.remove(key);
            }
        }
    }
    Ok(value)
}

/// Initialize `acdd.yaml`, `.acdd/workflows/default.yaml` (`inherit: code` or `inherit: base`), and canonical `.gitignore` block.
pub fn initialize(root: &Path, force: bool) -> Result<Value> {
    let acdd_yaml = root.join("acdd.yaml");
    let workflow_yaml = root.join(".acdd/workflows/default.yaml");

    if acdd_yaml.exists() && workflow_yaml.exists() && !force {
        bail!("acdd.yaml and .acdd/workflows/default.yaml already exist; pass --force to replace");
    }

    let mut wrote_config = false;
    if !acdd_yaml.exists() || force {
        acdd_core::fs::atomic_write(&acdd_yaml, TEMPLATE.as_bytes(), false)?;
        wrote_config = true;
    }

    let adapter = load_adapter(root, Some(&acdd_yaml))?;
    let code_enabled = adapter.modules.is_enabled("code");
    let inherit = if code_enabled { "code" } else { "base" };
    let workflow_content = format!(
        "version: 2\ninherit: {inherit}\n\n# Bind abstract command slots (`test`, `build_check`, `verify_all`) for your project:\n# commands:\n#   test: \"cargo test --lib {{test_filter}}\"\n#   build_check: \"cargo clippy --workspace --all-targets -- -D warnings && cargo test --lib\"\n#   verify_all: \"cargo test --workspace\"\n"
    );

    let mut wrote_workflow = false;
    if !workflow_yaml.exists() || force {
        if let Some(parent) = workflow_yaml.parent() {
            fs::create_dir_all(parent)?;
        }
        acdd_core::fs::atomic_write(&workflow_yaml, workflow_content.as_bytes(), false)?;
        wrote_workflow = true;
    }

    let gitignore_path = root.join(".gitignore");
    let existing_gitignore = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };
    let updated_gitignore = upsert_canonical_gitignore(&existing_gitignore);
    if updated_gitignore != existing_gitignore || !gitignore_path.exists() {
        acdd_core::fs::atomic_write(&gitignore_path, updated_gitignore.as_bytes(), false)?;
    }

    let mut git_check_ignore_verified = false;
    if root.join(".git").exists() {
        let ignored_state = git_command(root, &["check-ignore", "-q", ".acdd/state.sqlite"])?
            .status
            .success();
        let ignored_workflow =
            git_command(root, &["check-ignore", "-q", ".acdd/workflows/default.yaml"])?
                .status
                .success();
        if !ignored_state {
            bail!(
                "GITIGNORE_VERIFICATION_FAILED: .gitignore did not ignore .acdd/state.sqlite; check repository ignore rules"
            );
        }
        if ignored_workflow {
            bail!("GITIGNORE_VERIFICATION_FAILED: .gitignore must not ignore .acdd/workflows/default.yaml");
        }
        git_check_ignore_verified = true;
    }

    Ok(json!({
        "root": root,
        "config_path": acdd_yaml,
        "workflow_path": workflow_yaml,
        "inherit": inherit,
        "created_config": wrote_config,
        "created_workflow": wrote_workflow,
        "gitignore_path": gitignore_path,
        "git_check_ignore_verified": git_check_ignore_verified,
    }))
}

/// Insert or replace the canonical `.acdd/` `.gitignore` block idempotently.
fn upsert_canonical_gitignore(existing: &str) -> String {
    let block_lines: Vec<&str> = CANONICAL_GITIGNORE_BLOCK.lines().collect();
    let legacy_or_managed = |line: &str| {
        let trimmed = line.trim();
        matches!(
            trimmed,
            ".acdd" | ".acdd/" | "/.acdd" | "/.acdd/" | "/.worktrees/" | ".worktrees/"
        ) || block_lines.contains(&trimmed)
    };

    let mut kept: Vec<&str> = Vec::new();
    for line in existing.lines() {
        if !legacy_or_managed(line) {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }

    let mut out = String::new();
    if !kept.is_empty() {
        out.push_str(&kept.join("\n"));
        out.push_str("\n\n");
    }
    out.push_str(CANONICAL_GITIGNORE_BLOCK);
    out.push('\n');
    out
}

fn topic_value(root: &Path, topic: &str, adapter: &scanner::Adapter) -> Result<Value> {
    match topic {
        "lifecycle" => Ok(json!({
            "purpose": "Advance admitted work through its active WorkflowDef (`version: 2`) and observed proof gates.",
            "authority": "The resolved WorkflowDef and `acdd_claim` contract + guidance are authoritative. Gate names and transitions are workflow data, not hardcoded stages.",
            "steps": [
                "Use CLI `acdd roadmap` for cross-spec planning (and CLI `acdd draft <spec> <slug>` to scaffold specs), and `acdd context [<node>]` / `acdd_state(action: \"context\")` to refresh budgeted gate contract/guidance over SQLITE_OPEN_READ_ONLY without mutating claims.",
                "Use `acdd next` / `acdd_state(action: \"next\")` or `acdd status` to discover ready cursors, then `acdd claim` / `acdd_claim` with `node`, `worker_id`, and `worktree`.",
                "Implement strictly inside admitted write scope (`in_write_scope: ALLOWED` in `acdd_read`). Call `acdd verify <cursor> [--test <target>]` / `acdd_verify` to observe fresh trace evidence.",
                "Submit via `acdd submit <cursor> --action pass|reject` / `acdd_submit`. Rejections require structured `findings` (`summary`, `items[]` with `severity`, `message`, `remediation`, and optional `kind`: `defect | architectural_drift`).",
                "For automated DAG execution, run `acdd run [--jobs N]` / `acdd_state(action: \"run\")` (requires role `runner:` commands; returns `WORKFLOW_RUNNER_REQUIRED` when roles use `mode: subagent` without `runner:`). Complete all subtasks with non-empty `evidence` before container receipt gates."
            ],
            "runners": "Role runners receive `ACDD_RUN_ID`, `ACDD_NODE_CURSOR`, `ACDD_CLAIM_REVISION`, `ACDD_WORKER_ID`, `ACDD_AGENT_SESSION_ID`, `ACDD_WORKTREE`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, and `TRACEPARENT`, with rendered ClaimResp delivered over stdin. Verification commands run sanitized without orchestrator env leakage.",
            "blackboard": "Use `acdd blackboard` / `acdd_blackboard` for `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred` entries.",
            "recovery": {
                "TASK_ALREADY_CLAIMED": "Node holds an active claim by another worker/session. Resume the owning worker or pass `--force` when taking over a dead local PID (or `--steal-remote`).",
                "STALE_CLAIM_REVISION": "Re-claim or fetch the current `claim_revision` via `acdd status` / `acdd show`, then re-run `acdd verify`.",
                "TASK_PROOF_UNOBSERVED": "Run `acdd verify <cursor> --test <target>` under the active claim and current tree before submitting.",
                "TASK_CANDIDATE_MISMATCH": "Working tree drifted from an immutable claim tree or predecessor `need:` tree. Inspect drift, restore via Git or `acdd reopen <node>`, then re-verify.",
                "TASK_NOT_INTEGRATED": "Completed task deltas are missing from the run worktree. Integrate completed task branches into the run worktree (or run `acdd run`) before post-task root gates."
            },
            "example": {
                "mcp": { "name": "acdd_state", "arguments": { "action": "next" } },
                "cli": "acdd next"
            },
            "references": ["AGENTS.md", "docs/reference/tasks.md", "docs/reference/cli.md"]
        })),
        "gates" => Ok(json!({
            "purpose": "Follow the active WorkflowDef's `when:` tag expressions, `pass:`/`fail:` routes, `save:` policy, `checks:`, `context:`, and proof contracts.",
            "steps": [
                "Inspect the active workflow and `acdd_claim` contract before running commands or reviews.",
                "Use `acdd verify` / `acdd_verify` to observe `command:`, `call:`, `checks:`, and scope barriers without mutating gate state.",
                "Submit only fresh, claim-bound observations via `acdd submit` / `acdd_submit`."
            ],
            "when": "Boolean expression over task tags (`tag`, `!tag`, `&`/`+`, `|`, parentheses); valid on `- tasks:` blocks and `task_gate`s, forbidden on `root_gate`s.",
            "transitions": "`pass:`, `fail:`, and `fail_drift:` select explicit next gate targets (`defect` routes via `fail:`, `architectural_drift` routes via `fail_drift:` and triggers builder context reset); `max_attempts` caps retries and root/container cycle counts before `escalated`. `independent_from` enforces `worker_id` inequality.",
            "persistence": "`git.isolation` (`worktree | in_place` via workflow or `--in-place`) and `git.worktrees` (`manual | managed`). Top-level `save` applies `none | commit`. Gate `save.post` applies `none | commit | snapshot` (`commit` writes an atomic plumbing commit with `Acdd-Claim`; `snapshot` is valid only on leaf task gates); `immutable: true` enforces `current_tree == claim_tree`; `save.report: <relpath>` writes a gate report strictly under `.acdd/reports/*.{md,json}` on pass.",
            "verification": "`verify` records `gate_traces` with RFC-8785 SHA-256 `execution_digest` bound to `contract_revision`, resolved commands, `test_target`, `checks:` salts (`index_generation` + `live_files_digest`), and `candidate_tree_before == candidate_tree_after`.",
            "example": {
                "mcp": { "name": "acdd_verify", "arguments": { "node": "t1.build", "test_target": "tests/seam.rs::case" } },
                "cli": "acdd verify t1.build --test tests/seam.rs::case"
            },
            "references": ["docs/reference/tasks.md", "docs/reference/workflows.md", "docs/reference/cli.md"]
        })),
        "subtasks" => Ok(json!({
            "purpose": "Track bounded task slices and mandatory terminal evidence before container `receipt: true` gates.",
            "steps": [
                "Declare concrete subtasks in the spec Markdown (`- [ ] <ref>: <title>`).",
                "Update subtask status via `acdd subtask <task_ref> <subtask_ref> --status in_progress|completed|cancelled [--evidence ...]` or `acdd_state(action: \"subtask\", ...)`.",
                "Every subtask must reach `completed` or `cancelled` with non-empty `evidence` before a `receipt: true` gate passes."
            ],
            "delivery": {
                "terminal_statuses": ["completed", "cancelled"],
                "evidence_required_for": ["completed", "cancelled"],
                "evidence_boundary": "Subtask evidence records what was verified; gate proof still requires observed `acdd verify` traces."
            },
            "example": {
                "mcp": { "name": "acdd_state", "arguments": { "action": "show", "node": "t1" } },
                "cli": "acdd show t1"
            },
            "references": ["docs/reference/tasks.md", "docs/reference/cli.md"]
        })),
        "code" => {
            let query = topic_value(root, "query", adapter)?;
            let mut steps = ["start", "symbols", "snippets", "queries", "tests", "recovery"]
                .into_iter()
                .map(|key| query[key].clone())
                .collect::<Vec<_>>();
            steps.push(json!("Use ast_grep_search for structural patterns; guide ast describes profile capabilities. Treat candidate relationships and lexical test suggestions as navigation, not verified calls or coverage."));
            Ok(json!({
                "purpose": "Navigate indexed code through the existing query guidance and verified relationships.",
                "steps": steps,
                "example": { "mcp": { "name": "code_map_overview", "arguments": {} }, "cli": "acdd query overview" },
                "references": ["docs/reference/mcp-tools.md", "docs/reference/cli.md"]
            }))
        }
        "adapter" => Ok(json!({
            "filename": "acdd.yaml",
            "template": TEMPLATE,
            "keys": [
                "preset",
                "roots",
                "eligible_roots",
                "docs",
                "ignore",
                "excluded_directory_names",
                "excluded_file_names",
                "specs",
                "workflows_dirs",
                "state_db",
                "retain_traces",
                "modules",
                "linked_workspaces",
                "owners",
                "aliases",
                "response",
                "limits",
                "sensitive",
                "DEBUG"
            ],
            "preset": "Three-layer subsystem preset (`full` default, `index`, `spec`, `state`, `minimal`); `spec` enables CLI `acdd validate`/`roadmap`/`draft` and MCP docs/core tools without creating `.acdd/state.sqlite`.",
            "specs": "Map of spec name -> { path, workflow, prefixes, digits } (defaults: milestones -> docs/milestones, plans -> docs/plans); paths must not overlap by components.",
            "modules": "Enable/disable subsystems (`code`, `docs`, `spec`, `state`, `blackboard`, `memory`, `domains.<id>`). Disabling `state` physically prevents `state.sqlite` creation and hides the 5 MCP state tools.",
            "linked_workspaces": "Array of { name, path, enabled?, roots, docs, ignore }; indexes sibling worktrees and resolves cross-repo `depends_on`.",
            "default": "Workspace root; `.git`, `.worktrees`, `.acdd` runtime artifacts, and common build directories excluded."
        })),
        "docs" => Ok(json!({
            "frontmatter": "---\ndoc_type: architecture\ntitle: Module contract\n---",
            "invariant": "> [!IMPORTANT] INV-MODULE-RULE: Document the rule and reference [`symbol.name`](../src/module.rs).",
            "types": ["architecture", "adr", "guide", "api", "plan"],
            "purpose": "Find and read documentation, then author valid metadata, links and invariants.",
            "steps": [
                "Use `search_docs(query, include_excerpt=true)` to find relevant sections; CLI: `acdd docs search TERM`.",
                "Use `get_doc(path_or_id, detail=compact)` for an outline, then `get_doc(path_or_id, section=...)` or default full `get_doc(path_or_id)` for exact content; CLI: `acdd docs get docs/guide.md`.",
                "Run `code_map_lint(categories=[\"docs\"])` / `acdd docs check` to verify frontmatter, relative links, and uniquely linked symbol/invariant claims."
            ],
            "example": { "mcp": { "name": "search_docs", "arguments": { "query": "guide" } }, "cli": "acdd docs search guide" },
            "references": ["docs/AGENTS.md", "docs/reference/mcp-tools.md", "docs/reference/cli.md"]
        })),
        "acdd" => Ok(json!({
            "purpose": "Execute specs, plans, workflows, or ephemeral `--goal` runs under one unified WorkflowDef engine (`gates_patch:`, `save.report`, `predecessor_proofs`) and 18 static + external DomainProvider plugin tools.",
            "steps": [
                "Initialize with `acdd init` (selects `inherit: code | base`, writes `.acdd/workflows/default.yaml` and canonical `.gitignore`; auxiliary templates live in `skills/acdd/references/acdd-workflow-examples/`).",
                "Validate workspace config, active workflow, `DomainProvider` capabilities, and active specs with `acdd validate`.",
                "Start a run via `acdd start <spec_path>` or `acdd start --goal \"...\" [--workflow <ref>]`, then claim/verify/submit with flat CLI commands (`claim`, `verify`, `submit`, `next`, `status`, `show`, `clear`, `run`, `tool`) or the 5 MCP state tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`)."
            ],
            "presets": {
                "full": "code + docs + spec + state + blackboard (all 18 static tools)",
                "index": "code + docs",
                "spec": "spec (CLI validate/roadmap/draft without state.sqlite) + docs (4 MCP tools)",
                "state": "state + blackboard + spec",
                "minimal": "core only (`acdd_read`, `acdd_guide`)"
            },
            "mcp_tools": {
                "code": ["code_map_overview", "code_map_search", "code_map_inspect", "code_map_impact", "code_map_analyze", "code_map_explain", "code_map_query", "code_map_lint", "ast_grep_search"],
                "docs": ["get_doc", "search_docs"],
                "core": ["acdd_read", "acdd_guide"],
                "state": ["acdd_state", "acdd_claim", "acdd_verify", "acdd_submit", "acdd_blackboard"],
                "plugins": "<domain>_<name> from active DomainProviders (also callable via `acdd tool <domain>_<name>`)"
            },
            "example": { "mcp": { "name": "acdd_state", "arguments": { "action": "status" } }, "cli": "acdd status" },
            "references": ["docs/adr/0003-unified-profile-graph-and-state-machine.md", "docs/adr/0004-domain-provider-spi-and-plugin-tools.md", "docs/reference/cli.md", "docs/reference/tasks.md"],
            "blackboard_topics": {
                "draft": "Exploratory drafts cleared on container receipt.",
                "notes": "Non-obvious technical context cleared on container receipt.",
                "findings": "Review findings and verified defects.",
                "blockers": "Issues preventing active work from proceeding.",
                "decisions": "Decisions retained in the delivery receipt.",
                "deferred": "Deferred items carried over to run closure."
            },
            "review_contours": ["paths", "claims", "architecture", "concurrency", "project_isolation", "administration"],
            "commit_authority": "Follow current user authorization and repository AGENTS.md for commits, integration, and cleanup. Remote publication requires explicit user approval."
        })),
        "query" => Ok(json!({
            "start": "Call code_map_overview to identify indexed paths and coverage.",
            "sql_schema": "nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id); path_dictionary(path_id, path); edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count); files(path, status, digest, size, language, is_test, generated, path_hash); doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant). Paths: FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id. Edge endpoints: FROM edges e JOIN nodes src ON e.src_hash=src.node_hash JOIN nodes dst ON e.dst_hash=dst.node_hash.",
            "symbols": "Use code_map_search with FTS terms or prefix* pattern. Compact inspect and explain include signature, docstring, receiver-aware container, inbound/outbound call counts, and up to five direct callers/callees.",
            "snippets": "Use acdd_read(selector=...) for digest-verified symbol bodies, or acdd_read(path=..., mode=lines|raw|search|delta|outline) for layered file reads. CLI: acdd read <path> --mode <mode>.",
            "tests": "Call code_map_query(operation=\"tests\", selector=...) on a narrow symbol or module; broad scopes are rejected before unbounded traversal.",
            "queries": "Use code_map_inspect and code_map_explain for symbols, code_map_analyze for diagnostics (mode=summary|hotspots|boundary|path|entrypoints|diff_impact|cochange), and code_map_impact for traversal. Use code_map_query for sql, untested, implementations, slice, unwired, duplicates, dead_tails, tests, and prove_removal; pages use limit, offset, and generation.",
            "diagnostics": "Call code_map_analyze with target='' for workspace totals (default mode='summary'); cycles require include_cycles=true, and mode='hotspots'|'boundary'|'path'|'entrypoints'|'diff_impact'|'cochange' run read-only graph/Git diagnostics.",
            "documents": "Use search_docs with include_excerpt=true to find sections, get_doc with detail='compact' for section outlines, and get_doc with section='...' to read a specific section.",
            "pages": "Start with compact detail and a small limit. Continue with the returned next_offset and generation for the same selector and filters.",
            "recovery": "On a computation budget error, narrow the selector or path and reduce depth."
        })),
        "ast" => Ok(json!({
            "capabilities": {
                "complete_syntax": "Patterns that form valid standalone syntax at the grammar root; every profile supports this capability.",
                "declaration_without_body": "Declaration patterns omit profile-declared body nodes, including function, type, class, interface, and implementation bodies where supported.",
                "fragment_probe": "A profile may wrap root-rejected expressions, calls, macros, or assignments in a probe container, then unwrap only the matched target node.",
                "attribute": "A profile may match its declared decorators or annotations layered over declarations."
            },
            "profile_matrix": {
                "rust": ["complete_syntax", "declaration_without_body", "fragment_probe", "attribute"],
                "python": ["complete_syntax", "declaration_without_body", "attribute"],
                "typescript_javascript": ["complete_syntax", "declaration_without_body", "attribute"],
                "html_vue": ["complete_syntax"]
            },
            "wildcards": {
                "$NAME": "Capture one syntax node.",
                "$$$SEQ": "Capture a sequence of zero or more syntax nodes."
            },
            "examples": ["fn $NAME($$$ARGS) {}", "client.fetch($ARG)", "@$DEC\ndef $NAME($$$ARGS):", "<div id=\"$ID\">$$$CHILDREN</div>"],
            "diagnostics": "Unsupported fragments and invalid syntax return a structured diagnostic with language, error span, actionable hint, and profile examples."
        })),
        "validate" => {
            let scan = scanner::scan_with_adapter(root, adapter)?;
            Ok(json!({
                "valid": true,
                "root": root,
                "roots": adapter.roots,
                "files": scan.files,
                "bytes": scan.bytes
            }))
        }
        _ => bail!(
            "unknown guide topic; expected init,adapter,docs,acdd,query,ast,validate,lifecycle,gates,subtasks,code"
        ),
    }
}

fn topic_value_cli(root: &Path, topic: &str, adapter: &scanner::Adapter) -> Result<Value> {
    match topic {
        "code" => Ok(json!({
            "purpose": "Navigate, inspect, lint, and verify indexed code and docs strictly through the `acdd` CLI.",
            "commands": {
                "read_selector": "acdd read <path> --selector <symbol> [--source-offset N] [--detail compact|full]",
                "read_modes": "acdd read <path> --mode auto|lines|raw|search|delta|snippet|outline [--start-line N] [--line-count K] [--pattern <pat>] [--baseline <ref>]",
                "overview": "acdd query overview [--aspects <list>] [--limit N]",
                "search": "acdd query search <pattern> [--kind <kind>] [--classification production|test|documentation] [--limit N]",
                "inspect": "acdd query inspect <selector> [--path <path>] [--show-source] [--show-doc true|false]",
                "impact": "acdd query impact [<selector>] [--changes staged|unstaged|head] [--path <path>] [--depth N]",
                "explain": "acdd query explain <selector> [--path <path>]",
                "analyze": "acdd query analyze [<target>] [--mode summary|hotspots|boundary|path|entrypoints|diff_impact|cochange] [--include-cycles true]",
                "query_run": "acdd query run duplicates|dead_tails|tests|prove_removal|slice|untested|implementations [<selector>] [--path <path>] [--min-lines N]",
                "ast_grep": "acdd ast grep '<pattern>' [--lang <lang>] [--path <path>] [--limit N]",
                "lint": "acdd lint [<target>] [--categories arch,duplicates,dead_tails,cycles,syntax,docs|all] [--severity warn|error]",
                "docs": "acdd docs search <query> [--doc-type <type>] | acdd docs get <path_or_id> [--section <heading>] | acdd docs check",
                "worktree_exec": "acdd -C <worktree> <command> | acdd exec [--run <id|name>] [--worker <id>] -- <cmd>"
            },
            "steps": [
                "Run `acdd query overview` to verify indexed paths and coverage (or `acdd build` to refresh `.acdd/code-map.sqlite`).",
                "Locate declarations via `acdd query search <pattern>` and inspect signatures, callers, and linked docs via `acdd query inspect|explain|impact`.",
                "Read digest-verified bodies via `acdd read <path> --selector <symbol>` and bounded slices via `acdd read <path> --mode lines|search|delta|outline`.",
                "Run structural pattern queries via `acdd ast grep '<pattern>' --lang <lang>` and graph hygiene via `acdd query run duplicates|dead_tails|tests|prove_removal|slice|untested|implementations` and `acdd lint --categories all`.",
                "Target isolated worktrees with `acdd -C .worktrees/<name> ...` or run build/test commands inside the run worktree with `acdd exec -- <cmd>`."
            ],
            "note": "Treat candidate relationships and lexical test suggestions as navigation, not verified calls or coverage.",
            "example": { "cli": "acdd query overview" },
            "references": ["docs/reference/cli.md"]
        })),
        "query" => Ok(json!({
            "start": "Run `acdd query overview` to identify indexed paths and coverage.",
            "sql_schema": "nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id); path_dictionary(path_id, path); edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count); files(path, status, digest, size, language, is_test, generated, path_hash); doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant). Paths: FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id. Edge endpoints: FROM edges e JOIN nodes src ON e.src_hash=src.node_hash JOIN nodes dst ON e.dst_hash=dst.node_hash.",
            "symbols": "Use `acdd query search <pattern>` (FTS or prefix*) and `acdd query inspect|explain <selector>` for signatures, docstrings, and direct callers/callees.",
            "snippets": "Use `acdd read <path> --selector <symbol>` for verified symbol bodies, or `acdd read <path> --mode auto|lines|raw|search|delta|snippet|outline` for layered file reads.",
            "tests": "Run `acdd query run tests <selector>` or `acdd query tests <selector>` on a narrow symbol or module.",
            "queries": "Use `acdd query impact <selector>`, `acdd query analyze [<target>] --mode summary|hotspots|boundary|path|entrypoints|diff_impact|cochange`, `acdd query run duplicates|dead_tails|tests|prove_removal|slice|untested|implementations`, `acdd ast grep '<pattern>'`, and `acdd lint --categories all`.",
            "diagnostics": "Run `acdd query analyze --mode summary --include-cycles true` and `acdd lint --categories all` for workspace diagnostics.",
            "documents": "Use `acdd docs search <query>` and `acdd docs get <path_or_id> [--section <heading>]`.",
            "pages": "Start with compact detail and a small `--limit`; continue with `--offset` and `--generation` for the same selector and filters.",
            "recovery": "On a computation budget error, narrow the selector or `--path` and reduce `--depth`; run inside worktrees via `-C <worktree>` or `acdd exec -- <cmd>`."
        })),
        "ast" => {
            let mut val = topic_value(root, "ast", adapter)?;
            if let Some(obj) = val.as_object_mut() {
                obj.insert(
                    "purpose".into(),
                    json!("Search workspace files with structural tree-sitter AST patterns via `acdd ast grep`."),
                );
                obj.insert(
                    "steps".into(),
                    json!([
                        "Run `acdd ast grep '<pattern>' [--lang <lang>] [--path <path>] [--limit N]`.",
                        "Use `$NAME` for single-node captures and `$$$SEQ` for sequence captures."
                    ]),
                );
                obj.insert(
                    "cli".into(),
                    json!("acdd ast grep '<pattern>' --lang <lang> [--path <path>] [--limit N]"),
                );
                obj.insert(
                    "example".into(),
                    json!({ "cli": "acdd ast grep 'fn $NAME($$$ARGS) {}' --lang rust" }),
                );
                obj.insert("references".into(), json!(["docs/reference/cli.md"]));
            }
            Ok(val)
        }
        "docs" => Ok(json!({
            "frontmatter": "---\ndoc_type: architecture\ntitle: Module contract\n---",
            "invariant": "> [!IMPORTANT] INV-MODULE-RULE: Document the rule and reference [`symbol.name`](../src/module.rs).",
            "types": ["architecture", "adr", "guide", "api", "plan"],
            "purpose": "Find and read documentation via CLI, then author valid metadata, links, and invariants.",
            "steps": [
                "Run `acdd docs search <query> [--doc-type <type>] [--component <prefix>]` to find relevant sections.",
                "Run `acdd docs get <path_or_id> --detail compact` for an outline, then `--section <heading>` or default full `acdd docs get <path_or_id>` for exact content.",
                "Run `acdd docs check` / `acdd lint --categories docs` to verify links and invariants."
            ],
            "example": { "cli": "acdd docs search guide" },
            "references": ["docs/AGENTS.md", "docs/reference/cli.md"]
        })),
        "lifecycle" => Ok(json!({
            "purpose": "Advance admitted work through its active WorkflowDef (`version: 2`) and observed proof gates via CLI.",
            "authority": "The resolved WorkflowDef and `acdd claim` contract + guidance are authoritative. Gate names and transitions are workflow data, not hardcoded stages.",
            "steps": [
                "Use `acdd roadmap` for cross-spec planning, `acdd draft` to scaffold specs, and `acdd context [<node>]` to inspect budgeted gate context read-only without mutating claims.",
                "Use `acdd next` or `acdd status` to discover ready cursors, then `acdd claim <cursor> --worker <id> --worktree <path>`.",
                "Implement strictly inside admitted write scope and run `acdd verify <cursor> [--test <target>]` and `acdd exec -- <cmd>` inside the worktree.",
                "Submit via `acdd submit <cursor> --action pass|reject` (with structured `--findings` JSON on reject), and complete subtasks via `acdd subtask` with non-empty `--evidence` before receipt gates.",
                "For automated DAG execution, run `acdd run [--jobs N]` (requires role `runner:` commands; returns `WORKFLOW_RUNNER_REQUIRED` when roles use `mode: subagent` without `runner:`)."
            ],
            "runners": "Role runners receive `ACDD_RUN_ID`, `ACDD_NODE_CURSOR`, `ACDD_CLAIM_REVISION`, `ACDD_WORKER_ID`, `ACDD_AGENT_SESSION_ID`, `ACDD_WORKTREE`, `ACDD_STATE_DB`, `ACDD_CODE_DB`, and `TRACEPARENT`, with rendered ClaimResp delivered over stdin. Verification commands run sanitized without orchestrator env leakage.",
            "blackboard": "Use `acdd blackboard --action read|post` for `draft`, `notes`, `findings`, `blockers`, `decisions`, and `deferred` entries.",
            "recovery": {
                "TASK_ALREADY_CLAIMED": "Node holds an active claim by another worker/session. Resume the owning worker or pass `--force` when taking over a dead local PID (or `--steal-remote`).",
                "STALE_CLAIM_REVISION": "Re-claim or fetch the current `claim_revision` via `acdd status` / `acdd show`, then re-run `acdd verify`.",
                "TASK_PROOF_UNOBSERVED": "Run `acdd verify <cursor> --test <target>` under the active claim and current tree before submitting.",
                "TASK_CANDIDATE_MISMATCH": "Working tree drifted from an immutable claim tree or predecessor `need:` tree. Inspect drift, restore via Git or `acdd reopen <node>`, then re-verify.",
                "TASK_NOT_INTEGRATED": "Completed task deltas are missing from the run worktree. Integrate completed task branches into the run worktree (or run `acdd run`) before post-task root gates."
            },
            "example": { "cli": "acdd next" },
            "references": ["AGENTS.md", "docs/reference/tasks.md", "docs/reference/cli.md"]
        })),
        "acdd" => Ok(json!({
            "purpose": "Execute specs, plans, workflows, or ephemeral `--goal` runs under one unified WorkflowDef engine and flat `acdd` CLI.",
            "steps": [
                "Initialize with `acdd init` (selects `inherit: code | base`, writes `.acdd/workflows/default.yaml` and canonical `.gitignore`; auxiliary templates live in `skills/acdd/references/acdd-workflow-examples/`).",
                "Validate workspace config, active workflow, `DomainProvider` capabilities, and active specs with `acdd validate`.",
                "Start a run via `acdd start <spec_path>` or `acdd start --goal \"...\" [--workflow <ref>]`, then claim/verify/submit with flat CLI commands (`claim`, `verify`, `submit`, `next`, `status`, `show`, `context`, `clear`, `run`, `tool`)."
            ],
            "cli_commands": {
                "code": ["acdd query overview", "acdd query search", "acdd query inspect", "acdd query impact", "acdd query analyze", "acdd query explain", "acdd query run", "acdd lint", "acdd ast grep"],
                "docs": ["acdd docs get", "acdd docs search", "acdd docs check"],
                "spec": ["acdd validate", "acdd roadmap", "acdd draft"],
                "core": ["acdd read", "acdd guide", "acdd exec"],
                "state": ["acdd start", "acdd status", "acdd list", "acdd show", "acdd context", "acdd next", "acdd claim", "acdd verify", "acdd submit", "acdd extend-scope", "acdd reopen", "acdd sync", "acdd subtask", "acdd blackboard", "acdd clear", "acdd run"],
                "plugins": "acdd tool <domain>_<name> --args '{...}' from active DomainProviders"
            },
            "example": { "cli": "acdd status" },
            "references": ["docs/adr/0003-unified-profile-graph-and-state-machine.md", "docs/adr/0004-domain-provider-spi-and-plugin-tools.md", "docs/reference/cli.md", "docs/reference/tasks.md"],
            "blackboard_topics": {
                "draft": "Exploratory drafts cleared on container receipt.",
                "notes": "Non-obvious technical context cleared on container receipt.",
                "findings": "Review findings and verified defects.",
                "blockers": "Issues preventing active work from proceeding.",
                "decisions": "Decisions retained in the delivery receipt.",
                "deferred": "Deferred items carried over to run closure."
            },
            "review_contours": ["paths", "claims", "architecture", "concurrency", "project_isolation", "administration"],
            "commit_authority": "Follow current user authorization and repository AGENTS.md for commits, integration, and cleanup. Remote publication requires explicit user approval."
        })),
        "gates" => {
            let mut val = topic_value(root, "gates", adapter)?;
            if let Some(obj) = val.as_object_mut() {
                obj.insert(
                    "steps".into(),
                    json!([
                        "Inspect the active workflow and `acdd claim` / `acdd context` output before running commands or reviews.",
                        "Use `acdd verify <cursor> [--test <target>]` to observe `command:`, `call:`, `checks:`, and scope barriers without mutating gate state.",
                        "Submit only fresh, claim-bound observations via `acdd submit <cursor> --action pass|reject`."
                    ]),
                );
                obj.insert(
                    "example".into(),
                    json!({ "cli": "acdd verify t1.build --test tests/seam.rs::case" }),
                );
            }
            Ok(val)
        }
        "subtasks" => {
            let mut val = topic_value(root, "subtasks", adapter)?;
            if let Some(obj) = val.as_object_mut() {
                obj.insert(
                    "steps".into(),
                    json!([
                        "Declare concrete subtasks in the spec Markdown (`- [ ] <ref>: <title>`).",
                        "Update subtask status via `acdd subtask <task_ref> <subtask_ref> --status in_progress|completed|cancelled [--evidence ...]`.",
                        "Every subtask must reach `completed` or `cancelled` with non-empty `evidence` before a `receipt: true` gate passes."
                    ]),
                );
                obj.insert("example".into(), json!({ "cli": "acdd show t1" }));
            }
            Ok(val)
        }
        "adapter" => {
            let mut val = topic_value(root, "adapter", adapter)?;
            if let Some(obj) = val.as_object_mut() {
                obj.insert(
                    "modules".into(),
                    json!("Enable/disable subsystems (`code`, `docs`, `spec`, `state`, `blackboard`, `domains.<id>`). Disabling `state` physically prevents `state.sqlite` creation and state commands."),
                );
            }
            Ok(val)
        }
        other => topic_value(root, other, adapter),
    }
}
