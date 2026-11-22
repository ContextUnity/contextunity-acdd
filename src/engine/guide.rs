//! Built-in CLI and MCP guidance topics and workspace initialization (`acdd init`).

use acdd_core::config::load_adapter;
use acdd_core::git::git_command;
use acdd_index::scanner;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Canonical `0.6.0` `acdd.yaml` template.
pub const TEMPLATE: &str = "\
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
profiles_dirs:
  - .acdd/profiles
state_db: .acdd/state.sqlite
retain_traces: 20
";

/// Canonical `.gitignore` block ensuring `.acdd/profile.yaml`, `profiles/**`, and `frameworks/**` remain tracked while runtime state/locks/traces stay ignored.
pub const CANONICAL_GITIGNORE_BLOCK: &str = "\
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
    let mut value = topic_value(&root, topic, adapter)?;
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

/// Initialize `acdd.yaml`, `.acdd/profile.yaml` (`inherit: code` or `inherit: base`), and canonical `.gitignore` block.
pub fn initialize(root: &Path, force: bool) -> Result<Value> {
    let acdd_yaml = root.join("acdd.yaml");
    let profile_yaml = root.join(".acdd/profile.yaml");

    if acdd_yaml.exists() && profile_yaml.exists() && !force {
        bail!("acdd.yaml and .acdd/profile.yaml already exist; pass --force to replace");
    }

    let mut wrote_config = false;
    if !acdd_yaml.exists() || force {
        acdd_core::fs::atomic_write(&acdd_yaml, TEMPLATE.as_bytes(), false)?;
        wrote_config = true;
    }

    let adapter = load_adapter(root, Some(&acdd_yaml))?;
    let code_enabled = adapter.modules.is_enabled("code");
    let inherit = if code_enabled { "code" } else { "base" };
    let profile_content = format!(
        "version: 2\ninherit: {inherit}\n\n# Bind abstract command slots (`test`, `build_check`, `verify_all`) for your project:\n# commands:\n#   test: \"cargo test --lib {{test_filter}}\"\n#   build_check: \"cargo clippy --workspace --all-targets -- -D warnings && cargo test --lib\"\n#   verify_all: \"cargo test --workspace\"\n"
    );

    let mut wrote_profile = false;
    if !profile_yaml.exists() || force {
        if let Some(parent) = profile_yaml.parent() {
            fs::create_dir_all(parent)?;
        }
        acdd_core::fs::atomic_write(&profile_yaml, profile_content.as_bytes(), false)?;
        wrote_profile = true;
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
        let ignored_profile = git_command(root, &["check-ignore", "-q", ".acdd/profile.yaml"])?
            .status
            .success();
        if !ignored_state {
            bail!(
                "GITIGNORE_VERIFICATION_FAILED: .gitignore did not ignore .acdd/state.sqlite; check repository ignore rules"
            );
        }
        if ignored_profile {
            bail!(
                "GITIGNORE_VERIFICATION_FAILED: .gitignore must not ignore .acdd/profile.yaml"
            );
        }
        git_check_ignore_verified = true;
    }

    Ok(json!({
        "root": root,
        "config_path": acdd_yaml,
        "profile_path": profile_yaml,
        "inherit": inherit,
        "created_config": wrote_config,
        "created_profile": wrote_profile,
        "gitignore_path": gitignore_path,
        "git_check_ignore_verified": git_check_ignore_verified,
    }))
}

/// Insert or replace the canonical `.acdd/` `.gitignore` block idempotently.
pub fn upsert_canonical_gitignore(existing: &str) -> String {
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
            "purpose": "Advance admitted work through its active GateProfile (`version: 2`) and observed proof gates.",
            "authority": "The resolved GateProfile and `acdd_claim` contract + guidance are authoritative. Gate names and transitions are profile data, not hardcoded stages.",
            "steps": [
                "Use `acdd roadmap` / `acdd_state(action: \"roadmap\")` for cross-collection planning, `acdd context [<node>]` / `acdd_state(action: \"context\")` to refresh budgeted gate contract/guidance over SQLITE_OPEN_READ_ONLY without mutating claims.",
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
                "TASK_NOT_INTEGRATED": "Completed task deltas are missing from the run worktree. Integrate completed task branches into the run worktree (or run `acdd run`) before post-task root gates.",
                "CONFIG_MIGRATION_REQUIRED": "Legacy 0.5.x keys found in acdd.yaml. Follow docs/runbooks/migration-0.6.md."
            },
            "example": {
                "mcp": { "name": "acdd_state", "arguments": { "action": "next" } },
                "cli": "acdd next"
            },
            "references": ["AGENTS.md", "docs/reference/tasks.md", "docs/reference/cli.md"]
        })),
        "gates" => Ok(json!({
            "purpose": "Follow the active GateProfile's `when:` tag expressions, `pass:`/`fail:` routes, `save:` policy, `checks:`, `context:`, and proof contracts.",
            "steps": [
                "Inspect the active profile and `acdd_claim` contract before running commands or reviews.",
                "Use `acdd verify` / `acdd_verify` to observe `command:`, `call:`, `checks:`, and scope barriers without mutating gate state.",
                "Submit only fresh, claim-bound observations via `acdd submit` / `acdd_submit`."
            ],
            "when": "Boolean expression over task tags (`tag`, `!tag`, `&`/`+`, `|`, parentheses); valid on `- tasks:` blocks and `task_gate`s, forbidden on `root_gate`s.",
            "transitions": "`pass:`, `fail:`, and `fail_drift:` select explicit next gate targets (`defect` routes via `fail:`, `architectural_drift` routes via `fail_drift:` and triggers builder context reset); `max_attempts` caps retries and root/container cycle counts before `escalated`. `independent_from` enforces `worker_id` inequality.",
            "persistence": "`git.isolation` (`worktree | in_place` via profile or `--in-place`) and `git.worktrees` (`manual | managed`). Top-level `save` applies `none | commit`. Gate `save.post` applies `none | commit | snapshot` (`commit` writes an atomic plumbing commit with `Acdd-Claim`; `snapshot` is valid only on leaf task gates); `immutable: true` enforces `current_tree == claim_tree`; `save.report: <relpath>` writes a gate report strictly under `.acdd/reports/*.{md,json}` on pass.",
            "verification": "`verify` records `gate_traces` with RFC-8785 SHA-256 `execution_digest` bound to `contract_revision`, resolved commands, `test_target`, `checks:` salts (`index_generation` + `live_files_digest`), and `candidate_tree_before == candidate_tree_after`.",
            "example": {
                "mcp": { "name": "acdd_verify", "arguments": { "node": "t1.build", "test_target": "tests/seam.rs::case" } },
                "cli": "acdd verify t1.build --test tests/seam.rs::case"
            },
            "references": ["docs/reference/tasks.md", "docs/reference/profiles.md", "docs/reference/cli.md"]
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
                "roots",
                "eligible_roots",
                "docs",
                "ignore",
                "excluded_directory_names",
                "excluded_file_names",
                "specs",
                "profiles_dirs",
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
            "specs": "Map of collection name -> { path, profile, prefixes, digits } (defaults: milestones -> docs/milestones, plans -> docs/plans); paths must not overlap by components.",
            "modules": "Enable/disable subsystems (`code`, `docs`, `state`, `blackboard`, `memory`, `domains.<id>`). Disabling `state` physically prevents `state.sqlite` creation and hides the 5 MCP state tools.",
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
                "Use `get_doc(path_or_id, detail=compact)` for an outline, then `section` for the exact content; CLI: `acdd docs get docs/guide.md`.",
                "Use `acdd_read(path, mode=lines)` for source text; preserve frontmatter, relative links, and uniquely linked symbol/invariant claims."
            ],
            "example": { "mcp": { "name": "search_docs", "arguments": { "query": "guide" } }, "cli": "acdd docs search guide" },
            "references": ["docs/AGENTS.md", "docs/reference/mcp-tools.md", "docs/reference/cli.md"]
        })),
        "acdd" => Ok(json!({
            "purpose": "Execute specs, plans, workflows, or ephemeral `--goal` runs under one unified `0.6.2` GateProfile engine (`gates_patch:`, `save.report`, `predecessor_proofs`) and 20 static + external DomainProvider plugin tools.",
            "steps": [
                "Initialize with `acdd init` (selects `inherit: code | base | audit | spec_planning`, writes canonical `.gitignore`).",
                "Validate workspace config, active profile, `DomainProvider` capabilities, and active specs with `acdd validate`.",
                "Start a run via `acdd start <spec_path>` or `acdd start --goal \"...\" [--profile spec_planning]`, then claim/verify/submit with flat CLI commands (`claim`, `verify`, `submit`, `next`, `status`, `show`, `clear`, `run`, `tool`) or the 5 MCP state tools (`acdd_state`, `acdd_claim`, `acdd_verify`, `acdd_submit`, `acdd_blackboard`)."
            ],
            "mcp_tools": {
                "code": ["code_map_overview", "code_map_search", "code_map_inspect", "code_map_impact", "code_map_analyze", "code_map_explain", "code_map_tests", "code_map_prove_removal", "code_map_query", "ast_grep_search"],
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
            "tests": "Call code_map_tests on a narrow symbol or module; broad scopes are rejected before unbounded traversal.",
            "queries": "Use code_map_inspect and code_map_explain for symbols, code_map_analyze for diagnostics, and code_map_impact for traversal. Use code_map_query for sql, untested, implementations, slice, unwired, duplicates, and dead_tails; pages use limit, offset, and generation.",
            "diagnostics": "Call code_map_analyze with target='' for workspace totals; cycles require include_cycles=true.",
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
