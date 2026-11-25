use crate::format::OutputFormat;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "acdd",
    version,
    about = "Native code graph, documentation engine, unified state engine, MCP server and CLI"
)]
/// Top-level CLI parser.
pub struct Cli {
    #[arg(long, global = true, env = "ACDD_WORKSPACE_ROOT")]
    /// Optional workspace root override.
    pub root: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_DB")]
    /// Optional code-map SQLite database path override.
    pub db: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_STATE_DB")]
    /// Optional state SQLite database path override.
    pub state_db: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_CONFIG")]
    /// Explicit root-confined ACDD configuration file.
    pub config: Option<PathBuf>,
    #[arg(long, global = true, env = "ACDD_PROFILE")]
    /// Explicit gate profile reference or root-confined profile path.
    pub profile: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value = "markdown")]
    /// Output format: markdown (default) or json.
    pub format: CliOutputFormat,
    #[command(subcommand)]
    /// Selected subcommand (defaults to `serve` when omitted).
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
/// Output representation selected for command results.
pub enum CliOutputFormat {
    #[default]
    /// Human-readable Markdown, selected when `--format` is omitted.
    Markdown,
    /// JSON for callers that parse stdout.
    Json,
}

impl From<CliOutputFormat> for OutputFormat {
    fn from(value: CliOutputFormat) -> Self {
        match value {
            CliOutputFormat::Markdown => Self::Markdown,
            CliOutputFormat::Json => Self::Json,
        }
    }
}

#[derive(Subcommand, Debug)]
/// Flat `0.6.0` CLI subcommand surface.
pub enum Command {
    /// Save, restore, list, or delete checkpoints (`--delete`, `--older-than`, `--worktree`).
    Snapshot {
        /// `save`, `restore`, `clear`, `delete`, or `list`.
        action: Option<String>,
        /// Task ref or standalone snapshot name.
        target: Option<String>,
        /// Clear every ref under the resolved prefix.
        #[arg(long)]
        all: bool,
        /// Save, restore, or delete this revision.
        #[arg(long)]
        rev: Option<u32>,
        /// Active run id or name.
        #[arg(long)]
        run: Option<String>,
        /// Delete checkpoints.
        #[arg(long)]
        delete: bool,
        /// Delete refs older than `<N><s|m|h|d|w>`.
        #[arg(long)]
        older_than: Option<String>,
        /// Git worktree to read and restore.
        #[arg(long)]
        worktree: Option<std::path::PathBuf>,
    },
    /// Initialize `acdd.yaml`, `.acdd/profile.yaml`, and canonical `.gitignore` rules.
    Init {
        /// Overwrite existing `acdd.yaml` and `.acdd/profile.yaml`.
        #[arg(long)]
        force: bool,
    },
    /// Start a new `Run` from a spec file or ephemeral `--goal`.
    Start {
        /// Workspace-relative spec Markdown path (mutually exclusive with `--goal`).
        spec: Option<String>,
        /// Ephemeral goal text materializing `.acdd/run/<run_id>/spec.md`.
        #[arg(long)]
        goal: Option<String>,
        /// Optional explicit run name override.
        #[arg(long)]
        name: Option<String>,
        /// Initial scope globs (for `--goal` ephemeral runs).
        #[arg(long = "scope")]
        scope: Vec<String>,
        /// Target integration base branch/ref for worktree isolation runs.
        #[arg(long)]
        target_base: Option<String>,
        /// Create a managed run worktree under `.worktrees/<run_name>`.
        #[arg(long)]
        worktree: bool,
        /// Force `in_place` isolation for this run.
        #[arg(long)]
        in_place: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree_path: Option<PathBuf>,
    },
    /// Draft the next sequentially numbered spec document in a configured collection.
    Draft {
        /// Collection key from `acdd.yaml` `specs:` (e.g. `milestones`, `plans`).
        collection: String,
        /// URL/file slug for the drafted spec document.
        slug: String,
        /// Optional prefix override (must belong to `collection.prefixes`).
        #[arg(long)]
        prefix: Option<String>,
        /// Optional human-readable title.
        #[arg(long)]
        title: Option<String>,
    },
    /// Inspect status of an active `Run` (read-only).
    Status {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// List active runs and optional closed tombstones (read-only).
    List {
        /// Optional workspace filter.
        #[arg(long)]
        workspace: Option<String>,
        /// Optional status filter (`active | completed | cancelled`).
        #[arg(long)]
        status: Option<String>,
        /// Include closed `run_tombstones`.
        #[arg(long)]
        include_tombstones: bool,
        /// Maximum items per page (`1..=100`).
        #[arg(long)]
        limit: Option<usize>,
        /// Zero-based page offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Optional `runs.generation` pin for stable pagination.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// Show detailed run or node state, subtasks, and budgeted context preview (read-only).
    Show {
        /// Optional target node cursor (`<gate>`, `<task_ref>`, or `<task_ref>.<gate>`).
        node: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// Refresh budgeted claim context without mutating claim state (read-only).
    Context {
        /// Optional target node cursor (`<gate>`, `<task_ref>`, or `<task_ref>.<gate>`).
        node: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional worker filter when auto-selecting the active gate.
        #[arg(long, alias = "worker-id")]
        worker: Option<String>,
    },
    /// Show ready node cursors and topological execution waves (read-only).
    Next {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional maximum number of ready cursors.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Cross-collection roadmap: in-progress runs, dependency waves, and task drill-down (read-only).
    Roadmap {
        /// Optional spec ID filter / drill-down selector.
        spec_id: Option<String>,
        /// Optional spec collection display filter.
        #[arg(long)]
        collection: Option<String>,
        /// Show only Wave 0 ready specs.
        #[arg(long)]
        ready: bool,
        /// Show only in-progress specs (active runs).
        #[arg(long)]
        active: bool,
        /// Include intra-spec task drill-down.
        #[arg(long)]
        tasks: bool,
    },
    /// Claim or re-claim a ready/active node and materialize budgeted claim context.
    Claim {
        /// Optional node cursor (omitting auto-selects next ready node).
        node: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Claiming worker identity.
        #[arg(long, alias = "worker-id", default_value = "cli")]
        worker: String,
        /// Optional explicit session fingerprint override.
        #[arg(long)]
        session_fingerprint: Option<String>,
        /// Force take-over of an existing active claim.
        #[arg(long)]
        force: bool,
        /// Explicitly authorize stealing an active claim from another host/remote session.
        #[arg(long)]
        steal_remote: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Run gate verification (`command:`, `call:`, `checks:`, scope barrier) without advancing state.
    Verify {
        /// Target node cursor.
        node: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional claiming worker identity check.
        #[arg(long, alias = "worker-id")]
        worker: Option<String>,
        /// Optional `--test` filter target passed to `{test_filter}` and `code.seam`.
        #[arg(long, alias = "test-target")]
        test: Option<String>,
        /// Optional expected `claim_revision`.
        #[arg(long)]
        claim_revision: Option<u32>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Submit `pass` or `reject` for a claimed node.
    Submit {
        /// Target node cursor.
        node: String,
        /// Submission decision (`pass` or `reject`).
        #[arg(long)]
        action: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Submitting worker identity.
        #[arg(long, alias = "worker-id", default_value = "cli")]
        worker: String,
        /// Optional expected `claim_revision`.
        #[arg(long)]
        claim_revision: Option<u32>,
        /// Optional `--test` target forwarded if `submit` auto-triggers `verify`.
        #[arg(long, alias = "test-target")]
        test: Option<String>,
        /// Typed review or custom schema JSON evidence (or `@path` to a confined JSON file).
        #[arg(long)]
        evidence: Option<String>,
        /// Structured rejection findings JSON (or `@path` to a confined JSON file) required for `--action reject` (supports `kind: defect | architectural_drift`).
        #[arg(long)]
        findings: Option<String>,
        /// Verified invariant IDs attested by this submission.
        #[arg(long = "invariant")]
        invariants: Vec<String>,
        /// Optional human-readable summary for receipt rollups.
        #[arg(long)]
        summary: Option<String>,
        /// Explicitly permit committing onto `main` / `master`.
        #[arg(long)]
        allow_main: bool,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Extend a task's admitted write scope with a non-empty reason.
    ExtendScope {
        /// Target task or task-gate cursor.
        node: String,
        /// Additional workspace-relative paths or globs to admit.
        #[arg(long = "path", required = true)]
        paths: Vec<String>,
        /// Non-empty justification for the scope extension.
        #[arg(long)]
        reason: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional worker identity check.
        #[arg(long, alias = "worker-id")]
        worker: Option<String>,
    },
    /// Reopen a gate or task container (with optional `--cascade`).
    Reopen {
        /// Target gate or task container cursor.
        node: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Cascade-stale `in_progress` / `completed` downstream dependent tasks.
        #[arg(long)]
        cascade: bool,
        /// Optional operator reason recorded on the blackboard.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Synchronize spec edits (and optional `--rebind-profile`) into an active `Run`.
    Sync {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Rebind non-completed gates against the latest resolved profile.
        #[arg(long)]
        rebind_profile: bool,
    },
    /// Update a task's subtask status and mandatory terminal evidence.
    Subtask {
        /// Owning task reference.
        task_ref: String,
        /// Target subtask reference.
        subtask_ref: String,
        /// New subtask status (`pending | in_progress | completed | cancelled`).
        #[arg(long)]
        status: String,
        /// Required non-empty evidence when `--status` is `completed` or `cancelled`.
        #[arg(long)]
        evidence: Option<String>,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
    },
    /// Read from or post to the run blackboard (`read | post`).
    Blackboard {
        /// Blackboard action (`read` or `post`).
        #[arg(long, default_value = "read")]
        action: String,
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional source node cursor.
        #[arg(long)]
        node: Option<String>,
        /// Optional target recipient node cursor.
        #[arg(long)]
        recipient_node: Option<String>,
        /// Optional subtask reference.
        #[arg(long)]
        subtask_ref: Option<String>,
        /// Optional gate label.
        #[arg(long)]
        gate: Option<String>,
        /// Author identity (for `--action post`).
        #[arg(long)]
        author: Option<String>,
        /// Topic (`draft | notes | findings | blockers | decisions | deferred`).
        #[arg(long)]
        topic: Option<String>,
        /// Message body (for `--action post`).
        #[arg(long)]
        payload: Option<String>,
        /// Optional status filter (`open | acknowledged | resolved | carried_over`) for `read`.
        #[arg(long)]
        status: Option<String>,
        /// Page size (`1..=100`) for `read`.
        #[arg(long)]
        limit: Option<usize>,
        /// Zero-based page offset for `read`.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Optional `runs.generation` pin for `read`.
        #[arg(long)]
        generation: Option<u64>,
    },
    /// Clear/cancel an active `Run` into `run_tombstones(terminal_status = 'cleared')`.
    Clear {
        /// Optional run ID or name selector.
        run_id: Option<String>,
        /// Optional run ID or name selector flag.
        #[arg(long)]
        run: Option<String>,
        /// Optional cancellation reason recorded in the tombstone.
        #[arg(long)]
        reason: Option<String>,
        /// Force cleanup of dirty managed worktrees.
        #[arg(long)]
        force: bool,
    },
    /// Execute automated DAG waves (`--jobs N`, `--dry-run`, `--allow-main`).
    Run {
        /// Optional run ID or name selector.
        #[arg(long)]
        run: Option<String>,
        /// Maximum concurrent wave jobs (clamped to `1` for `in_place`).
        #[arg(long)]
        jobs: Option<usize>,
        /// Explicitly permit commits/integration onto `main` / `master`.
        #[arg(long)]
        allow_main: bool,
        /// Dry-run mode computing waves without mutating state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Invoke a plugin tool exposed by an active `DomainProvider` (`<domain>_<name>`).
    Tool {
        /// Full plugin tool name (`<domain>_<name>`).
        tool_name: String,
        /// JSON arguments validated against `DomainToolDef.input_schema`.
        #[arg(long, default_value = "{}")]
        args: String,
        /// Optional active run selector.
        #[arg(long)]
        run: Option<String>,
        /// Optional active node cursor.
        #[arg(long)]
        node: Option<String>,
        /// Explicit worktree path override.
        #[arg(long)]
        worktree: Option<PathBuf>,
    },
    /// Read text, symbols, outlines, or Git changes through the layered reader.
    Read {
        /// Workspace-relative file, or symbol selector with --mode snippet.
        path: String,
        /// auto, lines, raw, search, delta, snippet, or outline.
        #[arg(long, default_value = "auto")]
        mode: String,
        /// Indexed symbol selector, optionally confined by path.
        #[arg(long)]
        selector: Option<String>,
        /// One-based first line.
        #[arg(long)]
        start_line: Option<usize>,
        /// Maximum number of text lines.
        #[arg(long)]
        line_count: Option<usize>,
        /// Search pattern.
        #[arg(long)]
        pattern: Option<String>,
        /// Interpret pattern as a regular expression.
        #[arg(long)]
        regex: bool,
        /// Search context lines on each side.
        #[arg(long)]
        context: Option<usize>,
        /// Git baseline revision.
        #[arg(long)]
        baseline: Option<String>,
        /// Workspace-relative export destination.
        #[arg(long)]
        output_file: Option<String>,
        /// Include Hints footer with follow-up suggestions.
        #[arg(long)]
        include_hints: bool,
        /// Symbol source page offset.
        #[arg(long, default_value_t = 0)]
        source_offset: usize,
        /// Previous source page generation.
        #[arg(long)]
        generation: Option<String>,
        /// Smaller symbol body limit.
        #[arg(long)]
        max_body_lines: Option<usize>,
        /// Symbol leading context lines.
        #[arg(long)]
        leading_lines: Option<usize>,
    },
    /// Validate `acdd.yaml`, `.acdd/profile.yaml`, `DomainProvider` capabilities, and specs fail-closed.
    Validate {
        /// Optional workspace-relative file; when omitted, validates the full workspace.
        path: Option<PathBuf>,
    },
    /// Serve the MCP stdio server.
    Serve,
    /// Reload running MCP servers owned by this user.
    Reload,
    /// Build or rebuild `.acdd/code-map.sqlite`.
    Build {
        /// Workspace root to index (defaults to `--root` or current directory).
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        /// Optional output database path.
        output: Option<PathBuf>,
        #[arg(long)]
        /// Optional adapter config path.
        adapter: Option<PathBuf>,
        #[arg(long)]
        /// Emit per-phase build diagnostics.
        verbose: bool,
    },
    /// Scan workspace files without writing the code-map database.
    Scan {
        /// Workspace root to scan (defaults to `--root` or current directory).
        workspace_root: Option<PathBuf>,
        #[arg(long)]
        /// Optional adapter config path.
        adapter: Option<PathBuf>,
        #[arg(long)]
        /// Pretty-print JSON output.
        pretty: bool,
    },
    /// Incrementally update `.acdd/code-map.sqlite` for modified files.
    Delta {
        /// Workspace root whose index should be refreshed (defaults to `--root`).
        workspace_root: Option<PathBuf>,
        /// Modified workspace-relative files.
        modified_files: Vec<PathBuf>,
        #[arg(long = "modified", value_name = "FILE")]
        /// Additional modified files.
        modified: Vec<PathBuf>,
    },
    /// Query the indexed code graph.
    Query {
        #[command(subcommand)]
        /// Selected query operation.
        command: QueryCommand,
    },
    /// Check or query indexed documentation.
    Docs {
        #[command(subcommand)]
        /// Selected docs operation.
        command: DocsCommand,
    },
    /// Run structural AST pattern searches.
    Ast {
        #[command(subcommand)]
        /// Selected AST operation.
        command: AstCommand,
    },
    /// Run unified architectural, graph hygiene, syntax, and documentation lint rules.
    Lint {
        /// Optional workspace-relative file, directory, or glob target (omit for full workspace).
        target: Option<String>,
        /// Lint categories (`arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`, or `all`).
        #[arg(long, value_delimiter = ',')]
        categories: Option<Vec<String>>,
        /// Filter diagnostics to specific rule or invariant IDs.
        #[arg(long = "rule", alias = "rules", value_delimiter = ',')]
        rule: Option<Vec<String>>,
        /// Minimum diagnostic severity (`warn` or `error`).
        #[arg(long)]
        severity: Option<String>,
        /// Maximum violations per page (1..=100).
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Result page offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Response detail (`compact` or `full`).
        #[arg(long)]
        detail: Option<String>,
        /// Continuation generation from the preceding result page.
        #[arg(long)]
        generation: Option<String>,
    },
    /// Display built-in ACDD guidance topics (`init | adapter | docs | acdd | query | ast | validate | lifecycle | gates | subtasks | code`).
    Guide {
        /// Topic name.
        topic: String,
        #[arg(long)]
        /// Force overwrite when `topic == "init"`.
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
/// Subcommands for querying the indexed code graph and reading SQL/traversal outputs.
pub enum QueryCommand {
    /// Summarize codebase counts, components, languages, and coverage.
    Overview {
        /// Comma-separated overview aspects.
        #[arg(long, value_delimiter = ',')]
        aspects: Option<Vec<String>>,
        /// Maximum items per aspect.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Number of items to skip.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Generation from the previous page.
        #[arg(long)]
        generation: Option<String>,
    },
    /// Inspect a symbol's metadata, docs, resolution coverage, and optional source preview.
    Inspect {
        /// Symbol ID, name, `path:name`, `path:line`, or `path#Lline` selector.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long,default_value_t=true,action=clap::ArgAction::Set,num_args=0..=1,default_missing_value="true")]
        /// Include linked documentation sections.
        show_doc: bool,
        #[arg(long)]
        /// Include a bounded verified source preview.
        show_source: bool,
        #[arg(long, default_value_t = 0)]
        /// Source body offset returned by the prior preview.
        source_offset: usize,
        #[arg(long)]
        /// Generation returned by the prior source preview.
        generation: Option<String>,
    },
    /// Full-text and name-ranked symbol search across the indexed graph.
    Search {
        /// Symbol name, qualified name, or full-text search pattern (`*` lists all in scope).
        pattern: String,
        #[arg(long)]
        /// Optional node kind filter (for example `function`, `struct`, `class`).
        kind: Option<String>,
        #[arg(long)]
        /// Optional symbol classification filter (`production`, `test`, or `documentation`).
        classification: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of matching symbols to return.
        limit: usize,
    },
    /// Discover tests covering or called by a target symbol.
    Tests {
        /// Target symbol ID, name, or `path:symbol` selector.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value = "inbound")]
        /// Traversal direction (`inbound` callers/tests or `outbound` callees).
        direction: String,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of test entries to return.
        limit: usize,
    },
    /// Trace callers, callees, or Git-change blast radius across the dependency graph.
    Impact {
        /// Optional exact symbol selector; provide this or --changes.
        selector: Option<String>,
        #[arg(long)]
        /// Git change view: staged, unstaged, or head.
        changes: Option<String>,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 1)]
        /// Maximum impact depth; defaults to one edge.
        depth: u32,
        #[arg(long, default_value_t = 100)]
        /// Maximum result page size (1..=100).
        limit: usize,
        #[arg(long, default_value_t = 0)]
        /// Result page offset.
        offset: usize,
        #[arg(long)]
        /// Continuation generation from the preceding result page.
        generation: Option<String>,
    },
    /// Inspect incoming/outgoing edges and trait/inheritance relations for a symbol.
    Explain {
        /// Symbol ID, name, `path:name`, or `path:line` selector.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
    },
    /// Prove whether a symbol has remaining callers, tests, or references before deletion.
    Remove {
        /// Symbol ID, name, `path:name`, or `path:line` selector.
        selector: String,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
    },
    /// Analyze parse errors, unresolved references, untested surface, and cycles.
    Analyze {
        #[arg(default_value = "")]
        /// Workspace-relative file or directory scope (empty analyzes the whole workspace).
        target: String,
    },
    /// Execute a read-only SELECT/WITH SQL statement against the code graph.
    Sql {
        /// Read-only SELECT or WITH SQL statement.
        sql: String,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of result rows to return.
        limit: usize,
    },
    /// Execute a named graph query (`slice`, `duplicates`, `dead_tails`, etc.) or SQL query.
    Run {
        /// The operation value ('sql', 'untested', 'implementations', 'slice', 'unwired', 'duplicates', 'dead_tails', or a direct SELECT/WITH statement).
        operation: String,
        /// Optional symbol/path selector or SQL query when `operation == "sql"`.
        selector: Option<String>,
        /// Restrict selector resolution to a workspace-relative file or directory.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 2)]
        /// Maximum graph depth for generic query operations; impact defaults to two.
        depth: u32,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of items per result page (1..=100).
        limit: usize,
        #[arg(long, default_value_t = 0)]
        /// Result page offset.
        offset: usize,
        #[arg(long)]
        /// Continuation generation from the preceding result page.
        generation: Option<String>,
        #[arg(long)]
        /// Deduplication level: 'symbol' (functions/methods) or 'file' (identical files).
        level: Option<String>,
        #[arg(long, default_value_t = 10)]
        /// Minimum line count for duplicates detection (default 10).
        min_lines: usize,
    },
}

#[derive(Subcommand, Debug)]
/// Subcommands for validating, searching, and reading workspace Markdown documentation.
pub enum DocsCommand {
    /// Check Markdown links and anchors in the workspace.
    Check,
    /// Search indexed Markdown sections by terms, document type, or component.
    Search {
        /// Search query terms over indexed documentation titles and bodies.
        query: String,
        #[arg(long)]
        /// Filter results by frontmatter `doc_type` (for example `architecture`, `adr`, `reference`).
        doc_type: Option<String>,
        #[arg(long)]
        /// Restrict results to a component or documentation directory prefix.
        component: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of matching sections to return.
        limit: usize,
    },
    /// Read a document outline or a specific section from an indexed Markdown file.
    Get {
        /// Indexed document path or document section ID.
        path_or_id: String,
        #[arg(long)]
        /// Optional exact section heading to return; omit to inspect the outline.
        section: Option<String>,
        #[arg(long)]
        /// Include ambiguous documentary symbol reference candidates.
        include_candidates: bool,
    },
}

#[derive(Subcommand, Debug)]
/// Subcommands for structural tree-sitter AST pattern search.
pub enum AstCommand {
    /// Search workspace files with a structural tree-sitter AST pattern.
    Grep {
        /// Tree-sitter AST pattern using `$METAVAR` placeholders.
        pattern: String,
        #[arg(long = "lang", alias = "language")]
        /// Optional language filter (inferred from --path or pattern grammar when omitted).
        language: Option<String>,
        #[arg(long)]
        /// Optional workspace-relative file or directory scope.
        path: Option<String>,
        #[arg(long, default_value_t = 100)]
        /// Maximum number of AST pattern matches to return.
        limit: usize,
    },
}
