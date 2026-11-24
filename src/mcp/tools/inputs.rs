use crate::format::OutputFormat;
use acdd_core::ids::{RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::response::{Detail, QueryOptions, ResponsePolicy, SourceOptions};
use acdd_state::types::{RejectionFindings, ReviewEvidence, SubmitAction};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

fn yes() -> bool {
    true
}
fn depth() -> u32 {
    2
}
fn impact_depth() -> u32 {
    1
}
fn inbound() -> String {
    "inbound".into()
}

fn aliased_value<'a>(
    canonical: Option<&'a str>,
    aliases: &[Option<&'a str>],
    name: &str,
) -> anyhow::Result<Option<&'a str>> {
    let mut values = canonical
        .into_iter()
        .chain(aliases.iter().copied().flatten());
    let value = values.next();
    anyhow::ensure!(values.next().is_none(), "duplicate field `{name}`");
    Ok(value)
}

fn aliased_string<'a>(
    canonical: &'a str,
    aliases: &[Option<&'a str>],
    name: &str,
) -> anyhow::Result<&'a str> {
    Ok(
        aliased_value((!canonical.is_empty()).then_some(canonical), aliases, name)?
            .unwrap_or(canonical),
    )
}

// An empty schema object accepts any JSON value and stays valid for MCP clients.
fn any_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::Map::new().into()
}

fn output_format_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), Value::String("string".into()));
    schema.insert("enum".into(), serde_json::json!(["markdown", "json"]));
    schema.into()
}

#[derive(Default, Deserialize, JsonSchema)]
/// Represents page input data.
pub struct PageInput {
    /// Items per collection, 1..=100; defaults to adapter response.page_size (30).
    pub limit: Option<usize>,
    #[serde(default)]
    /// The offset value.
    pub offset: usize,
    /// Compact omits heavy node details; full remains subject to the byte limit.
    pub detail: Option<Detail>,
    /// Use the generation returned by the previous page when offset is nonzero.
    pub generation: Option<String>,
    /// Worktree selector: omit to keep the canonical server root; `.` selects it explicitly. Absolute paths or values containing `/` or `\` are resolved explicitly (relative paths start at the server root) and must name a directory. Otherwise, canonical-path-deduplicated registered Git worktrees and immediate `.worktrees/` children are matched in order: exact name, literal prefix (or zero-normalized numeric milestone prefix), suffix, then substring. The first non-empty tier must be unique (`WORKTREE_AMBIGUOUS`); no match returns `WORKTREE_NOT_FOUND`.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}
impl PageInput {
    pub(super) fn output_format(&self) -> OutputFormat {
        self.format.unwrap_or_default()
    }

    pub(super) fn resolve(&self, policy: &ResponsePolicy) -> anyhow::Result<QueryOptions> {
        self.resolve_with_default_detail(policy, None)
    }

    pub(super) fn resolve_with_default_detail(
        &self,
        policy: &ResponsePolicy,
        default_detail: Option<Detail>,
    ) -> anyhow::Result<QueryOptions> {
        QueryOptions::resolve(
            policy,
            self.limit,
            self.offset,
            self.detail.or(default_detail),
            self.generation.clone(),
        )
    }
}
#[derive(Default, Deserialize, JsonSchema)]
/// Represents overview input data.
pub struct OverviewInput {
    /// Optional database aspects: 'counts', 'components', 'languages', 'coverage_by_workspace', 'cycles', 'entrypoints', 'entrypoints_with_tests', 'topology', 'compiled_profiles', or 'metadata'. The discovery adapter also accepts 'discovery', 'manifest_components', 'documentation_hubs', and 'milestones'. 'entrypoints' excludes tests; 'entrypoints_with_tests' includes them. An omitted or empty list selects every database aspect except 'cycles', 'topology', and both entrypoint aspects and includes discovery; unsupported values report all accepted names.
    pub aspects: Option<Vec<String>>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Deserialize, JsonSchema)]
/// Represents selector data.
pub struct Selector {
    /// Exact symbol ID or name; path forms include path:name, path:known_kind:name, path:line, and path#Lline.
    pub selector: String,
    /// Restrict selector resolution to a workspace-relative file or directory.
    pub path: Option<String>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Default, Deserialize, JsonSchema)]
/// Shared source preview and continuation options.
pub struct SourceInput {
    /// Optional show source value.
    pub show_source: Option<bool>,
    /// Optional leading lines value.
    pub leading_lines: Option<usize>,
    /// Optional source body line limit; values clamp to 1..=500, omitted uses the configured default (200 by default).
    pub max_body_lines: Option<usize>,
    /// Body line offset returned by a previous source page. Nonzero offsets require that page's generation explicitly.
    #[serde(default)]
    pub source_offset: usize,
}

impl SourceInput {
    pub(super) fn resolve_with(
        &self,
        policy: &ResponsePolicy,
        generation: Option<&str>,
        required: Option<bool>,
    ) -> anyhow::Result<SourceOptions> {
        let source = match required {
            Some(req) => SourceOptions::resolve(
                policy,
                if req { Some(true) } else { self.show_source },
                self.leading_lines,
                self.max_body_lines,
                self.source_offset,
            )?,
            None => SourceOptions::resolve_full_symbol(
                policy,
                self.leading_lines,
                self.max_body_lines,
                self.source_offset,
            )?,
        };
        anyhow::ensure!(
            source.has_valid_paging_anchor(generation),
            "source continuation requires generation from the previous preview; pass the previous page's generation or restart at source_offset=0"
        );
        Ok(source)
    }
}

#[derive(Deserialize, JsonSchema)]
/// Represents inspect data.
pub struct Inspect {
    /// Exact symbol ID or name; path forms include path:name, path:known_kind:name, path:line, and path#Lline. Accepts 'symbol' as a unified alias.
    #[serde(default)]
    pub selector: Option<String>,
    /// Alternative to selector; provide exactly one of selector or symbol.
    pub symbol: Option<String>,
    /// Restrict selector resolution to a workspace-relative file or directory; accepts 'file_path' as a unified alias.
    pub path: Option<String>,
    /// Alternative to path; restrict selector resolution to a workspace-relative file or directory.
    pub file_path: Option<String>,
    #[serde(default = "yes")]
    /// Whether show doc applies.
    pub show_doc: bool,
    /// Include paged resolution coverage in addition to the compact summary.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(flatten)]
    /// Source preview and continuation options.
    pub source_input: SourceInput,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl Inspect {
    pub(super) fn selector(&self) -> anyhow::Result<&str> {
        Ok(aliased_value(
            self.selector.as_deref(),
            &[self.symbol.as_deref()],
            "selector",
        )?
        .unwrap_or(""))
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(self.path.as_deref(), &[self.file_path.as_deref()], "path")
    }

    pub(super) fn source(
        &self,
        policy: &ResponsePolicy,
        required: bool,
    ) -> anyhow::Result<SourceOptions> {
        self.source_input
            .resolve_with(policy, self.page.generation.as_deref(), Some(required))
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
/// Line range (e.g. "100-200") or integer count.
pub enum LinesInput {
    /// Line count limit as an integer.
    Integer(usize),
    /// Explicit inclusive line range string (`start-end`).
    String(String),
}

#[derive(Deserialize, JsonSchema)]
/// Layered reader options and source continuation bounds.
pub struct Snippet {
    #[serde(flatten)]
    /// Shared layered reader request.
    pub read: crate::engine::reader::ReadRequest,
    /// Indexed symbol name, ID, or qualified selector; alias for selector. Supply only one.
    pub symbol: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    /// One-based last line; computes line_count with start_line.
    pub end_line: Option<usize>,
    /// Line range (e.g. "100-200") or line count integer.
    pub lines: Option<LinesInput>,
    /// Alias for line_count.
    pub max_lines: Option<usize>,
    /// Alias for pattern search.
    pub query: Option<String>,
    #[serde(flatten)]
    /// Source preview and continuation options.
    pub source_input: SourceInput,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl Snippet {
    pub(super) fn normalize(&mut self, root: &std::path::Path) -> anyhow::Result<()> {
        self.read.selector = aliased_value(
            self.read.selector.as_deref(),
            &[self.symbol.as_deref()],
            "selector",
        )?
        .map(str::to_owned);
        self.read.path = aliased_value(
            self.read.path.as_deref(),
            &[self.file_path.as_deref()],
            "path",
        )?
        .map(str::to_owned);
        self.read.pattern = aliased_value(
            self.read.pattern.as_deref(),
            &[self.query.as_deref()],
            "pattern",
        )?
        .map(str::to_owned);
        if let Some(lines) = self.lines.take() {
            match lines {
                LinesInput::Integer(count) => {
                    if self.read.line_count.is_none() {
                        self.read.line_count = Some(count);
                    }
                }
                LinesInput::String(s) => {
                    let trimmed = s.trim();
                    if let Some((start_s, end_s)) = trimmed
                        .split_once('-')
                        .or_else(|| trimmed.split_once(".."))
                        .or_else(|| trimmed.split_once(':'))
                    {
                        let invalid = || {
                            anyhow::anyhow!("invalid lines range '{s}': expected positive START-END with END >= START")
                        };
                        let start = start_s.trim().parse::<usize>().map_err(|_| invalid())?;
                        let end = end_s.trim().parse::<usize>().map_err(|_| invalid())?;
                        anyhow::ensure!(start > 0 && end >= start, "{}", invalid());
                        {
                            if self.read.start_line.is_none() {
                                self.read.start_line = Some(start);
                            }
                            if self.read.line_count.is_none() {
                                self.read.line_count = Some(end.saturating_sub(start) + 1);
                            }
                        }
                    } else {
                        let count = trimmed.parse::<usize>().map_err(|_| {
                            anyhow::anyhow!(
                                "invalid lines '{s}': expected a positive count or START-END range"
                            )
                        })?;
                        anyhow::ensure!(count > 0, "invalid lines '{s}': count must be positive");
                        if self.read.line_count.is_none() {
                            self.read.line_count = Some(count);
                        }
                    }
                }
            }
        }
        if self.read.line_count.is_none() {
            self.read.line_count = self.max_lines.take();
        }
        if let Some(end) = self.end_line.take() {
            let start = self.read.start_line.unwrap_or(1);
            anyhow::ensure!(
                end > 0 && end >= start,
                "invalid end_line: expected a positive last line >= start_line"
            );
            if self.read.start_line.is_none() {
                self.read.start_line = Some(1);
            }
            if self.read.line_count.is_none() {
                self.read.line_count = Some(end.saturating_sub(start) + 1);
            }
        }
        if let Some(path) = self.read.path.take() {
            self.read.path =
                Some(crate::engine::reader::strip_root_path(&path, root).unwrap_or(path));
        }
        if let Some(paths) = self.read.paths.take() {
            self.read.paths = Some(
                paths
                    .into_iter()
                    .map(|p| crate::engine::reader::strip_root_path(&p, root).unwrap_or(p))
                    .collect(),
            );
        }
        Ok(())
    }

    pub(super) fn source(&self, policy: &ResponsePolicy) -> anyhow::Result<SourceOptions> {
        self.source_input
            .resolve_with(policy, self.page.generation.as_deref(), None)
    }
}

#[derive(Deserialize, JsonSchema)]
/// Represents explain data.
pub struct Explain {
    /// Exact symbol ID or name; path forms include path:name, path:known_kind:name, path:line, and path#Lline.
    pub selector: String,
    /// Restrict selector resolution to a workspace-relative file or directory.
    pub path: Option<String>,
    /// Optional direction value.
    pub direction: Option<String>,
    #[serde(default = "yes")]
    /// Whether show doc applies.
    pub show_doc: bool,
    /// Include paged resolution coverage in addition to the compact summary.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(flatten)]
    /// Source preview and continuation options.
    pub source_input: SourceInput,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl Explain {
    pub(super) fn source(
        &self,
        policy: &ResponsePolicy,
        required: bool,
    ) -> anyhow::Result<SourceOptions> {
        self.source_input
            .resolve_with(policy, self.page.generation.as_deref(), Some(required))
    }
}
#[derive(Deserialize, JsonSchema)]
/// Represents search symbols data.
pub struct SearchSymbols {
    /// Symbol pattern; query is accepted as an alias. Use '*' with a path or kind scope to list all symbols in that scope.
    #[serde(default)]
    pub pattern: String,
    /// Alternative to pattern; provide exactly one of pattern or query.
    pub query: Option<String>,
    /// Match only the complete symbol name or qualified name, without full-text search.
    #[serde(default)]
    pub exact: bool,
    /// Optional kind value.
    pub kind: Option<String>,
    /// Restrict results to a workspace-relative file or directory; an empty string leaves results unfiltered.
    pub path: Option<String>,
    /// Group results by file path.
    #[serde(default)]
    pub group_by_file: bool,
    /// Include Markdown documentation nodes in the code symbol search.
    #[serde(default)]
    pub include_docs: bool,
    /// Include source previews for up to three hits when the total exceeds three; totals at most three include previews automatically.
    #[serde(default)]
    pub include_preview: bool,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl SearchSymbols {
    pub(super) fn pattern(&self) -> anyhow::Result<&str> {
        aliased_string(&self.pattern, &[self.query.as_deref()], "pattern")
    }
}
#[derive(Deserialize, JsonSchema)]
/// Represents tests data.
pub struct Tests {
    /// Exact symbol ID or name; path forms include path:name, path:known_kind:name, path:line, and path#Lline.
    pub selector: String,
    /// Restrict selector resolution to a workspace-relative file or directory.
    pub path: Option<String>,
    #[serde(default = "inbound")]
    /// The direction value.
    pub direction: String,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Deserialize, JsonSchema)]
/// Represents impact data.
pub struct Impact {
    /// Optional exact symbol ID or name; required when changes is omitted. Provide exactly one of selector or changes.
    pub selector: Option<String>,
    /// Git change view: staged, unstaged, or head. Exactly one of selector or changes is required.
    pub changes: Option<String>,
    /// Restrict selector resolution to a workspace-relative file or directory.
    pub path: Option<String>,
    /// Impact direction: 'inbound' (default) or 'outbound'.
    #[serde(default = "inbound")]
    pub direction: String,
    #[serde(default = "impact_depth")]
    /// Maximum graph depth; omitted depth defaults to one edge.
    pub depth: u32,
    /// Traversal mode: 'calls' (default, invocation/call graph), 'data-flow' (parameter/assignment flow), 'all' (all dependency edges).
    pub mode: Option<String>,
    /// Optional specific edge kinds to follow (e.g. ['calls', 'mutates', 'inherits', 'implements', 'imports']).
    pub edge_types: Option<Vec<String>>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Deserialize, JsonSchema)]
/// Bounded graph or read-only SQL query. SQL uses the compact canonical tables;
/// join nodes to path_dictionary on path_id and edges to nodes on node_hash.
pub struct Query {
    /// Query operation: 'sql' (default when 'sql' is provided), 'untested', 'implementations', 'slice', 'unwired', 'duplicates', or 'dead_tails'.
    #[serde(default)]
    pub operation: String,
    /// Exact symbol ID or name (path:name, path:known_kind:name, path:line, path#Lline), or the read-only SELECT/WITH SQL statement when operation='sql'.
    pub selector: Option<String>,
    /// Read-only SELECT/WITH SQL statement; implies operation='sql' when operation is omitted.
    /// Schema: nodes(node_id, id, kind, name, qualname, path_id, line, end_line, is_test, language, generated, details, node_hash, owner_path_id); path_dictionary(path_id, path); edges(src_hash, kind, dst_hash, path_id, line, evidence_id, confidence_id, occurrence_count); files(path, status, digest, size, language, is_test, generated, path_hash); doc_sections(doc_id, path, section_title, doc_type, content, invariants, referenced_symbols, is_invariant).
    /// Paths: FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id.
    /// Endpoints: FROM edges e JOIN nodes src ON e.src_hash=src.node_hash JOIN nodes dst ON e.dst_hash=dst.node_hash.
    pub sql: Option<String>,
    /// Alias for selector (or SQL query when operation='sql').
    pub query: Option<String>,
    /// File or directory scope for untested, duplicates, and dead_tails operations; cannot conflict with selector or path.
    pub target: Option<String>,
    /// Duplicate search granularity for duplicates operation: 'symbol' (default) or 'file'.
    pub level: Option<String>,
    /// Minimum function/method statement count for duplicates operation (default: 5).
    pub min_lines: Option<usize>,
    /// Restrict selector resolution to a workspace-relative file or directory.
    pub path: Option<String>,
    /// Impact direction: 'inbound' (default) or 'outbound'.
    pub direction: Option<String>,
    /// Include paged resolution coverage for inspect and explain operations.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(default = "depth")]
    /// Maximum graph depth for generic query operations; impact defaults to two.
    pub depth: u32,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Deserialize, JsonSchema)]
/// Represents analyze data.
pub struct Analyze {
    /// File or directory target; accepts unified 'path' and 'selector' aliases. Omit target for workspace totals.
    #[serde(default)]
    pub target: String,
    /// Alternative to target for a file, directory, or read-only SQL statement.
    pub path: Option<String>,
    /// Alternative to target or path; omit all three for workspace totals.
    pub selector: Option<String>,
    /// Whether include cycles applies.
    pub include_cycles: Option<bool>,
    /// Read stored syntax diagnostics only; does not run a linter or reparse source.
    #[serde(default)]
    pub lint: bool,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl Analyze {
    pub(super) fn target(&self) -> anyhow::Result<&str> {
        Ok(aliased_value(
            (!self.target.is_empty()).then_some(self.target.as_str()),
            &[self.path.as_deref(), self.selector.as_deref()],
            "target",
        )?
        .unwrap_or(""))
    }
}
#[derive(Deserialize, JsonSchema)]
/// Represents ast data.
pub struct Ast {
    /// AST pattern; accepts 'query' as a unified alias.
    #[serde(default)]
    pub pattern: String,
    /// Alternative to pattern; provide exactly one of pattern or query.
    pub query: Option<String>,
    /// The language value (e.g. 'python', 'rust', 'typescript'). Optional when 'path' has a recognized file extension or resolves to an indexed single-language directory; required for multi-language scopes.
    pub language: Option<String>,
    /// Optional path value.
    pub path: Option<String>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl Ast {
    pub(super) fn pattern(&self) -> anyhow::Result<&str> {
        aliased_string(&self.pattern, &[self.query.as_deref()], "pattern")
    }
}
#[derive(Deserialize, JsonSchema)]
/// Represents search docs data.
pub struct SearchDocs {
    /// Documentation query; accepts 'pattern' as a unified alias.
    #[serde(default)]
    pub query: String,
    /// Alternative to query for documentation search terms.
    pub pattern: Option<String>,
    /// Optional doc type value.
    pub doc_type: Option<String>,
    /// Optional documentation component or directory prefix filter; accepts 'path' as a unified alias.
    pub component: Option<String>,
    /// Alternative to component for a documentation component or directory prefix.
    pub path: Option<String>,
    /// Include a short match-centered content excerpt in each result (recommended: true for zero-round-trip triage).
    #[serde(default)]
    pub include_excerpt: bool,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl SearchDocs {
    pub(super) fn query(&self) -> anyhow::Result<&str> {
        aliased_string(&self.query, &[self.pattern.as_deref()], "query")
    }

    pub(super) fn component(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(
            self.component.as_deref(),
            &[self.path.as_deref()],
            "component",
        )
    }
}
#[derive(Deserialize, JsonSchema)]
/// Represents get doc data.
pub struct GetDoc {
    /// Document path or ID; accepts unified 'path' and 'selector' aliases.
    #[serde(default)]
    pub path_or_id: String,
    /// Alternative to path_or_id for an indexed document path or ID.
    pub path: Option<String>,
    /// Alternative to path_or_id or path; provide exactly one document selector.
    pub selector: Option<String>,
    /// Optional exact section title. Omit with detail='compact' to inspect the document outline before reading a section.
    pub section: Option<String>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
impl GetDoc {
    pub(super) fn path_or_id(&self) -> anyhow::Result<&str> {
        aliased_string(
            &self.path_or_id,
            &[self.path.as_deref(), self.selector.as_deref()],
            "path_or_id",
        )
    }
}
#[derive(Default, Deserialize, JsonSchema)]
/// Unified architectural, graph hygiene, syntax, and documentation lint query.
pub struct Lint {
    /// Optional workspace-relative file, directory, or glob target; omit for full workspace.
    pub target: Option<String>,
    /// Optional lint categories (`arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`); omit for default `[arch, syntax, docs]`.
    pub categories: Option<Vec<LintCategory>>,
    /// Optional rule or invariant ID filters.
    pub rules: Option<Vec<String>>,
    /// Optional minimum severity threshold (`warn` or `error`).
    pub severity: Option<LintSeverity>,
    #[serde(flatten)]
    /// The page value.
    pub page: PageInput,
}
#[derive(Deserialize, JsonSchema)]
/// Represents guide data.
pub struct Guide {
    #[serde(default)]
    /// Optional topic: query (default), lifecycle, acdd, gates, subtasks, code, docs, ast, adapter, init, or validate. The query index advertises enabled topics; known disabled topics return empty guidance.
    pub topic: Option<String>,
    #[serde(default)]
    /// Whether force applies.
    pub force: bool,
    /// Worktree selector: omit to keep the canonical server root; `.` selects it explicitly. Absolute paths or values containing `/` or `\` are resolved explicitly (relative paths start at the server root) and must name a directory. Otherwise, canonical-path-deduplicated registered Git worktrees and immediate `.worktrees/` children are matched in order: exact name, literal prefix (or zero-normalized numeric milestone prefix), suffix, then substring. The first non-empty tier must be unique (`WORKTREE_AMBIGUOUS`); no match returns `WORKTREE_NOT_FOUND`.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}
/// Worktree routing selector or boolean `create_worktree` flag for `acdd_state(action: "start")`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum WorktreeInput {
    /// Boolean flag used by `acdd_state(action: "start")` (`worktree: true` creates a managed run worktree).
    Flag(bool),
    /// Explicit worktree path or unique name selector (empty selects the server root).
    Selector(String),
}

impl WorktreeInput {
    pub(super) fn as_selector(&self) -> Option<&str> {
        match self {
            Self::Flag(_) => None,
            Self::Selector(s) => Some(s.as_str()),
        }
    }

    pub(super) fn as_flag(&self) -> Option<bool> {
        match self {
            Self::Flag(b) => Some(*b),
            Self::Selector(_) => None,
        }
    }
}

/// Supported actions for the canonical `acdd_state` MCP tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpStateAction {
    /// Save, restore, or clear task checkpoints.
    Snapshot,
    /// Read-only status of the active run (`StateQuery::Status`).
    Status,
    /// Read-only paginated list of runs and tombstones (`StateQuery::List`).
    List,
    /// Read-only details for a run or specific node cursor (`StateQuery::Show`).
    Show,
    /// Read-only ready node cursors and topological execution waves (`StateQuery::Next`).
    Next,
    /// Read-only aggregate state observability metrics and Prometheus text (`StateQuery::Observability`).
    Observability,
    /// Read-only budgeted gate context refresh (`StateQuery::Context`).
    Context,
    /// Read-only cross-collection roadmap waves and drill-down (`StateQuery::Roadmap`).
    Roadmap,
    /// Start a new `Run` from a spec file or ephemeral `--goal` (`StateCommand::Start`).
    Start,
    /// Draft the next sequentially numbered spec document in a configured collection (`StateCommand::Draft`).
    Draft,
    /// Extend a task's admitted write scope (`StateCommand::ExtendScope`).
    ExtendScope,
    /// Reopen a gate or task container (`StateCommand::Reopen`).
    Reopen,
    /// Synchronize spec edits and optional profile rebind into an active `Run` (`StateCommand::Sync`).
    Sync,
    /// Update a subtask's status and mandatory terminal evidence (`StateCommand::Subtask`).
    Subtask,
    /// Clear/cancel an active `Run` into `run_tombstones` (`StateCommand::Clear`).
    Clear,
    /// Execute automated DAG waves (`StateCommand::Run`).
    Run,
}

impl McpStateAction {
    pub(super) fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::List
                | Self::Show
                | Self::Next
                | Self::Observability
                | Self::Context
                | Self::Roadmap
        )
    }
}

/// Canonical `acdd_state` MCP tool input covering 9 `StateCommand` actions and 7 read-only `StateQuery` actions.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StateInput {
    /// State action (`status`, `list`, `show`, `next`, `context`, `observability`, `roadmap`, `start`, `draft`, `extend_scope`, `reopen`, `sync`, `subtask`, `snapshot`, `clear`, `run`).
    pub action: McpStateAction,
    /// Snapshot operation: save, restore, or clear.
    pub snapshot_action: Option<acdd_state::usecases::SnapshotAction>,
    /// Clear all run checkpoints.
    pub all: Option<bool>,
    /// Snapshot revision.
    pub rev: Option<u32>,
    /// Standalone snapshot label.
    pub name: Option<String>,
    /// Delete refs older than `<N><s|m|h|d|w>`.
    pub older_than: Option<String>,
    /// When true, `snapshot` clears refs.
    pub delete: Option<bool>,
    /// Optional run ID or run name selector.
    pub run: Option<String>,
    /// Target node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`).
    pub node: Option<String>,
    /// Path to an existing spec Markdown file (`start`; mutually exclusive with `goal`).
    pub spec_path: Option<String>,
    /// Ephemeral goal text materializing `.acdd/run/<run_id>/spec.md` (`start`).
    pub goal: Option<String>,
    /// Optional explicit run name override (`start`).
    pub run_name: Option<RunName>,
    /// Optional profile override (`start`, `draft`).
    pub profile: Option<String>,
    /// Optional initial scope globs for ephemeral `goal` runs (`start`).
    pub scope: Option<Vec<String>>,
    /// Target integration base branch/ref for worktree isolation runs (`start`).
    pub target_base: Option<String>,
    /// Create a managed run worktree under `.worktrees/<run_name>` (`start`).
    pub create_worktree: Option<bool>,
    /// Force `in_place` isolation for this run (`start`).
    pub in_place: Option<bool>,
    /// Explicit worktree path override (`start`).
    pub worktree_path: Option<PathBuf>,
    /// Target spec collection key from `acdd.yaml` `specs:` (`draft`, e.g. `milestones`, `plans`).
    pub collection: Option<String>,
    /// URL/file slug for the drafted spec document (`draft`).
    pub slug: Option<String>,
    /// Optional ID prefix override within the collection (`draft`).
    pub prefix: Option<String>,
    /// Optional human-readable title (`draft`).
    pub title: Option<String>,
    /// Additional repository-relative paths or globs to admit (`extend_scope`).
    pub paths: Option<Vec<String>>,
    /// Justification/cancellation reason (`extend_scope`, `reopen`, `clear`).
    pub reason: Option<String>,
    /// Optional worker identity check (`extend_scope`).
    pub worker_id: Option<WorkerId>,
    /// Cascade-stale downstream dependent tasks (`reopen`).
    pub cascade: Option<bool>,
    /// Rebind non-completed gates against the latest resolved profile (`sync`).
    pub rebind_profile: Option<bool>,
    /// Target task reference (`subtask`, `snapshot`).
    pub task_ref: Option<TaskRef>,
    /// Target subtask reference (`subtask`).
    pub subtask_ref: Option<SubtaskRef>,
    /// Status filter (`list`: `active | completed | cancelled`) or new subtask status (`subtask`: `pending | in_progress | completed | cancelled`).
    pub status: Option<String>,
    /// Alias for `status` on `subtask`.
    pub subtask_status: Option<String>,
    /// Required non-empty evidence when `subtask` status is `completed` or `cancelled`.
    pub evidence: Option<String>,
    /// Explicitly permit committing/integrating onto `main` / `master` (`run`).
    pub allow_main: Option<bool>,
    /// Force cleanup of dirty managed worktrees (`clear`).
    pub force: Option<bool>,
    /// Maximum concurrent wave jobs (`run`).
    pub jobs: Option<usize>,
    /// Dry-run mode computing waves without mutating state (`run`).
    pub dry_run: Option<bool>,
    /// Optional spec ID filter for `roadmap` drill-down.
    pub spec_id: Option<String>,
    /// Limit `roadmap` output to Wave 0 ready specs.
    pub ready_only: Option<bool>,
    /// Limit `roadmap` output to in-progress specs.
    pub active_only: Option<bool>,
    /// Include intra-spec task drill-down in `roadmap` responses.
    pub include_tasks: Option<bool>,
    /// Optional workspace filter (`list`).
    pub workspace: Option<String>,
    /// Include closed `run_tombstones` (`list`).
    pub include_tombstones: Option<bool>,
    /// Page size (`1..=100`) (`list`, `next`).
    pub limit: Option<usize>,
    /// Zero-based page offset (`list`).
    pub offset: Option<usize>,
    /// Optional `runs.generation` pin for stable pagination (`list`).
    pub generation: Option<u64>,
    /// Include Prometheus 0.0.4 text output (`observability`).
    pub include_prometheus: Option<bool>,
    /// Worktree routing selector (or boolean `true` on `action="start"` to create a managed worktree).
    pub worktree: Option<WorktreeInput>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}

/// Canonical `acdd_claim` MCP tool input (`StateCommand::Claim`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimInput {
    /// Optional run ID or run name selector.
    pub run: Option<String>,
    /// Target node cursor (`<gate>`, `<task_ref>`, `<task_ref>.<gate>`), or omit to auto-select the next ready node.
    pub node: Option<String>,
    /// Claiming worker identity.
    pub worker_id: WorkerId,
    /// Force take-over of an existing active claim.
    #[serde(default)]
    pub force: bool,
    /// Explicitly authorize stealing an active claim from another host/remote session.
    #[serde(default)]
    pub steal_remote: bool,
    /// Worktree selector.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}

/// Canonical `acdd_verify` MCP tool input (`StateCommand::Verify`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyInput {
    /// Optional run ID or run name selector.
    pub run: Option<String>,
    /// Target node cursor (`<gate>`, `<task_ref>.<gate>`).
    pub node: String,
    /// Optional claiming worker identity check.
    pub worker_id: Option<WorkerId>,
    /// Optional `--test` filter target passed to `{test_filter}` and `code.seam`.
    pub test_target: Option<String>,
    /// Optional expected `claim_revision`.
    pub claim_revision: Option<u32>,
    /// Worktree selector.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}

/// Canonical `acdd_submit` MCP tool input (`StateCommand::Submit`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    /// Optional run ID or run name selector.
    pub run: Option<String>,
    /// Target node cursor (`<gate>`, `<task_ref>.<gate>`).
    pub node: String,
    /// Submission action (`pass` or `reject`).
    pub action: SubmitAction,
    /// Submitting worker identity.
    pub worker_id: WorkerId,
    /// Optional expected `claim_revision`.
    pub claim_revision: Option<u32>,
    /// Optional `--test` target forwarded if `submit` auto-triggers `verify`.
    pub test_target: Option<String>,
    /// Typed review evidence for `proof: review`.
    pub review_evidence: Option<ReviewEvidence>,
    /// Structured rejection findings required for `action: "reject"` (supports optional `kind`: `defect | architectural_drift`).
    pub rejection_findings: Option<RejectionFindings>,
    /// Custom schema JSON evidence (`proof: blueprint` or custom `proofs:` schema).
    #[schemars(schema_with = "any_json_schema")]
    pub custom_evidence: Option<Value>,
    /// Verified invariant identifiers attested by this submission.
    #[serde(default)]
    pub verified_invariants: Vec<String>,
    /// Optional human-readable summary for receipt rollups.
    pub summary: Option<String>,
    /// Explicitly permit committing onto `main` / `master`.
    #[serde(default)]
    pub allow_main: bool,
    /// Worktree selector.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}

/// Blackboard operation (`read` or `post`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpBlackboardAction {
    /// Read paginated blackboard entries over `StateQueryEngine` (`SQLITE_OPEN_READ_ONLY`).
    Read,
    /// Post a new message to the run blackboard (`StateCommand::BlackboardPost`).
    Post,
}

/// Canonical `acdd_blackboard` MCP tool input (`StateQuery::BlackboardRead` + `StateCommand::BlackboardPost`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlackboardInput {
    /// Blackboard action (`read` or `post`).
    pub action: McpBlackboardAction,
    /// Optional run ID or run name selector.
    pub run: Option<String>,
    /// Optional source node cursor filter (`read`) or source node (`post`).
    pub node: Option<String>,
    /// Optional target recipient node cursor (`read` filter or `post` target).
    pub recipient_node: Option<String>,
    /// Optional subtask reference (`post`).
    pub subtask_ref: Option<SubtaskRef>,
    /// Optional gate label (`post`).
    pub gate: Option<String>,
    /// Author identity (`post`; defaults to `"mcp"` when omitted).
    pub author: Option<String>,
    /// Topic (`draft | notes | findings | blockers | decisions | deferred`; required for `post`, optional filter for `read`).
    pub topic: Option<String>,
    /// Optional status filter for `read` (`open | acknowledged | resolved | carried_over`).
    pub status: Option<String>,
    /// Message text or JSON payload (required for `post`).
    pub payload: Option<String>,
    /// Page size (`1..=100`, default `50`) for `read`.
    pub limit: Option<usize>,
    /// Zero-based page offset for `read`.
    pub offset: Option<usize>,
    /// Optional `runs.generation` pin for stable `read` pagination.
    pub generation: Option<u64>,
    /// Worktree selector.
    pub worktree: Option<String>,
    /// Output format. Omit for readable Markdown; choose JSON for machine clients.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}
