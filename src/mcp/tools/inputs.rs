use crate::format::OutputFormat;
use acdd_core::ids::{RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::response::{
    Detail, QueryOptions, ResponsePolicy, SourceOptions, SymbolClassification,
};
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

fn resolve_selector_alias<'a>(
    selector: Option<&'a str>,
    symbol: Option<&'a str>,
) -> anyhow::Result<&'a str> {
    Ok(aliased_value(selector, &[symbol], "selector")?.unwrap_or(""))
}

fn resolve_path_alias<'a>(
    path: Option<&'a str>,
    file_path: Option<&'a str>,
) -> anyhow::Result<Option<&'a str>> {
    aliased_value(path, &[file_path], "path")
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

fn analyze_mode_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), Value::String("string".into()));
    schema.insert(
        "enum".into(),
        serde_json::json!([
            "summary",
            "hotspots",
            "boundary",
            "path",
            "entrypoints",
            "diff_impact",
            "cochange"
        ]),
    );
    schema.into()
}

#[derive(Default, Deserialize, JsonSchema)]
/// Shared pagination, detail, worktree, and format parameters.
pub struct PageInput {
    /// Items per page (1..=100; default 12).
    pub limit: Option<usize>,
    #[serde(default)]
    /// Zero-based item offset.
    pub offset: usize,
    /// compact or full.
    pub detail: Option<Detail>,
    /// Include file:// URIs in Markdown.
    pub file_uris: Option<bool>,
    /// Generation token for offset > 0.
    pub generation: Option<String>,
    /// Worktree path or name.
    pub worktree: Option<String>,
    /// Output format: markdown or json.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}
#[derive(Clone, Copy, Default)]
pub(super) struct PageUriOptions {
    file_uris: Option<bool>,
    detail: Option<Detail>,
}

impl PageUriOptions {
    pub(super) fn enabled(self, policy: &ResponsePolicy) -> bool {
        self.file_uris.unwrap_or_else(|| {
            policy.file_uris || self.detail.unwrap_or(policy.detail) == Detail::Full
        })
    }
}

impl PageInput {
    pub(super) fn output_format(&self) -> OutputFormat {
        self.format.unwrap_or_default()
    }

    pub(super) fn uri_options(&self, default_detail: Option<Detail>) -> PageUriOptions {
        PageUriOptions {
            file_uris: self.file_uris,
            detail: self.detail.or(default_detail),
        }
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
/// Workspace overview request selecting summary aspects, optional path scope, and pagination bounds.
pub struct OverviewInput {
    /// Optional database ('counts', 'components', 'languages', 'coverage_by_workspace', 'cycles', 'entrypoints', 'entrypoints_with_tests', 'topology', 'compiled_profiles', 'metadata') or discovery ('discovery', 'manifest_components', 'documentation_hubs', 'milestones') aspects.
    pub aspects: Option<Vec<String>>,
    /// Optional workspace-relative directory or file path to scope overview counts, languages, and files (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl OverviewInput {
    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Default, Deserialize, JsonSchema)]
/// Shared source preview and continuation options.
pub struct SourceInput {
    /// Override `response.source_context.enabled_by_default` to include or omit source text.
    pub show_source: Option<bool>,
    /// Leading context lines before the symbol declaration (`0..=20`).
    pub leading_lines: Option<usize>,
    /// Source body line limit (1..=500; default 100).
    pub max_body_lines: Option<usize>,
    /// Source line offset; nonzero requires previous generation.
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
/// Symbol inspection request covering metadata, linked docs, coverage, and source preview.
pub struct Inspect {
    /// Symbol ID/name or path:name, path:known_kind:name, path:line, path#Lline (alias: symbol).
    #[serde(default)]
    pub selector: Option<String>,
    /// Alias for selector.
    pub symbol: Option<String>,
    /// Relative file or directory scope (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    #[serde(default = "yes")]
    /// Include linked documentation sections in the inspection result.
    pub show_doc: bool,
    /// Include paged resolution coverage.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(flatten)]
    /// Source preview and continuation options.
    pub source_input: SourceInput,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Inspect {
    pub(super) fn selector(&self) -> anyhow::Result<&str> {
        resolve_selector_alias(self.selector.as_deref(), self.symbol.as_deref())
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
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
    /// Pagination, detail, worktree, and output format options.
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
/// Relationship explanation request listing callers, callees, and type hierarchy links.
pub struct Explain {
    /// Symbol ID/name or path:name, path:known_kind:name, path:line, path#Lline (alias: symbol).
    #[serde(default)]
    pub selector: Option<String>,
    /// Alias for selector.
    pub symbol: Option<String>,
    /// Relative file or directory scope (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    /// Edge direction filter (`incoming`, `outgoing`, or `both`).
    pub direction: Option<String>,
    #[serde(default = "yes")]
    /// Include linked documentation sections in the explain payload.
    pub show_doc: bool,
    /// Include paged resolution coverage.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(flatten)]
    /// Source preview and continuation options.
    pub source_input: SourceInput,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Explain {
    pub(super) fn selector(&self) -> anyhow::Result<&str> {
        resolve_selector_alias(self.selector.as_deref(), self.symbol.as_deref())
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Deserialize, JsonSchema)]
/// Full-text and name-ranked symbol search parameters.
pub struct SearchSymbols {
    /// Symbol pattern (alias: query). Use '*' with path or kind scope to list symbols.
    #[serde(default)]
    pub pattern: String,
    /// Alias for pattern.
    pub query: Option<String>,
    /// Match complete name or qualname only.
    #[serde(default)]
    pub exact: bool,
    /// Optional symbol kind filter (e.g. `function`, `struct`, `class`).
    pub kind: Option<String>,
    /// Optional classification filter ('production', 'test', or 'documentation').
    pub classification: Option<SymbolClassification>,
    /// Relative file or directory scope (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    /// Group results by file path.
    #[serde(default)]
    pub group_by_file: bool,
    /// Include Markdown documentation nodes.
    #[serde(default)]
    pub include_docs: bool,
    /// Include source previews for up to 3 hits (auto-included when total <= 3).
    #[serde(default)]
    pub include_preview: bool,
    /// Include or suppress graph_pulse on the top search hit.
    pub include_pulse: Option<bool>,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl SearchSymbols {
    pub(super) fn pattern(&self) -> anyhow::Result<&str> {
        aliased_string(&self.pattern, &[self.query.as_deref()], "pattern")
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Deserialize, JsonSchema)]
/// Impact traversal request for a symbol selector or Git change view.
pub struct Impact {
    /// Symbol ID or name (alias: symbol); provide one of selector or changes.
    pub selector: Option<String>,
    /// Alias for selector.
    pub symbol: Option<String>,
    /// Git change view: staged, unstaged, or head.
    pub changes: Option<String>,
    /// Relative file or directory scope (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
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
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Impact {
    pub(super) fn selector(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(
            self.selector.as_deref(),
            &[self.symbol.as_deref()],
            "selector",
        )
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Deserialize, JsonSchema)]
/// Bounded graph or read-only SQL query. SQL uses the compact canonical tables;
/// join nodes to path_dictionary on path_id and edges to nodes on node_hash.
pub struct Query {
    /// Query operation: 'sql' (default when 'sql' is provided), 'tests', 'prove_removal', 'untested', 'implementations', 'slice', 'unwired', 'duplicates', or 'dead_tails'.
    #[serde(default)]
    pub operation: String,
    /// Symbol selector or SQL statement when operation='sql' (aliases: symbol, query).
    pub selector: Option<String>,
    /// Alias for selector.
    pub symbol: Option<String>,
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
    /// Relative file or directory scope (alias: file_path).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    /// Impact direction: 'inbound' (default) or 'outbound'.
    pub direction: Option<String>,
    /// Include paged resolution coverage for inspect and explain operations.
    #[serde(default)]
    pub include_coverage: bool,
    #[serde(default = "depth")]
    /// Maximum graph depth for generic query operations; impact defaults to two.
    pub depth: u32,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Query {
    pub(super) fn selector(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(
            self.selector.as_deref(),
            &[self.symbol.as_deref(), self.query.as_deref()],
            "selector",
        )
    }

    pub(super) fn sql_statement(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(self.sql.as_deref(), &[self.selector()?], "sql")
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Deserialize, JsonSchema)]
/// Workspace or directory diagnostic analysis request covering summary, hotspots, boundary, path, entrypoints, diff_impact, and cochange.
pub struct Analyze {
    /// Relative file/directory target or SQL statement (aliases: path, file_path, selector); omit for workspace totals.
    #[serde(default)]
    pub target: String,
    /// Alias for target.
    pub path: Option<String>,
    /// Alias for target.
    pub file_path: Option<String>,
    /// Alias for target.
    pub selector: Option<String>,
    /// Analysis mode: 'summary' (default), 'hotspots', 'boundary', 'path', 'entrypoints', 'diff_impact', or 'cochange'.
    #[schemars(schema_with = "analyze_mode_schema")]
    pub mode: Option<String>,
    /// Destination symbol selector for mode='path'.
    pub to: Option<String>,
    /// Optional Git base ref for mode='diff_impact' or 'cochange'.
    pub base: Option<String>,
    /// Compute cross-module and cross-symbol dependency cycles in the selected scope.
    pub include_cycles: Option<bool>,
    /// Read stored syntax diagnostics only; does not run a linter or reparse source.
    #[serde(default)]
    pub lint: bool,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Analyze {
    pub(super) fn target(&self) -> anyhow::Result<&str> {
        Ok(aliased_value(
            (!self.target.is_empty()).then_some(self.target.as_str()),
            &[
                self.path.as_deref(),
                self.file_path.as_deref(),
                self.selector.as_deref(),
            ],
            "target",
        )?
        .unwrap_or(""))
    }
}
#[derive(Deserialize, JsonSchema)]
/// Structural tree-sitter AST pattern search parameters.
pub struct Ast {
    /// AST pattern (alias: query).
    #[serde(default)]
    pub pattern: String,
    /// Alias for pattern.
    pub query: Option<String>,
    /// Language profile ID (e.g. 'python', 'rust', 'typescript'); omitted infers from path or grammar.
    pub language: Option<String>,
    /// Restrict the structural AST search to a workspace-relative file or directory (alias: `file_path`).
    pub path: Option<String>,
    /// Alias for path.
    pub file_path: Option<String>,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Ast {
    pub(super) fn pattern(&self) -> anyhow::Result<&str> {
        aliased_string(&self.pattern, &[self.query.as_deref()], "pattern")
    }

    pub(super) fn path(&self) -> anyhow::Result<Option<&str>> {
        resolve_path_alias(self.path.as_deref(), self.file_path.as_deref())
    }
}
#[derive(Deserialize, JsonSchema)]
/// Indexed Markdown documentation search parameters.
pub struct SearchDocs {
    /// Documentation search query (alias: pattern).
    #[serde(default)]
    pub query: String,
    /// Alias for query.
    pub pattern: Option<String>,
    /// Filter results by frontmatter `doc_type` (for example `architecture`, `adr`, `reference`).
    pub doc_type: Option<String>,
    /// Documentation component or directory prefix (aliases: path, file_path).
    pub component: Option<String>,
    /// Alias for component.
    pub path: Option<String>,
    /// Alias for component.
    pub file_path: Option<String>,
    /// Include short match-centered excerpt.
    #[serde(default)]
    pub include_excerpt: bool,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl SearchDocs {
    pub(super) fn query(&self) -> anyhow::Result<&str> {
        aliased_string(&self.query, &[self.pattern.as_deref()], "query")
    }

    pub(super) fn component(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(
            self.component.as_deref(),
            &[self.path.as_deref(), self.file_path.as_deref()],
            "component",
        )
    }
}
#[derive(Deserialize, JsonSchema)]
/// Indexed Markdown document or section retrieval parameters.
pub struct GetDoc {
    /// Document path or ID (aliases: path, file_path, doc_id, selector).
    #[serde(default)]
    pub path_or_id: String,
    /// Alias for path_or_id.
    pub path: Option<String>,
    /// Alias for path_or_id.
    pub file_path: Option<String>,
    /// Alias for path_or_id.
    pub doc_id: Option<String>,
    /// Alias for path_or_id.
    pub selector: Option<String>,
    /// Exact section title; omit with detail='compact' for outline.
    pub section: Option<String>,
    /// Include ambiguous documentary symbol reference candidates (default false).
    #[serde(default)]
    pub include_candidates: bool,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl GetDoc {
    pub(super) fn path_or_id(&self) -> anyhow::Result<&str> {
        aliased_string(
            &self.path_or_id,
            &[
                self.path.as_deref(),
                self.file_path.as_deref(),
                self.doc_id.as_deref(),
                self.selector.as_deref(),
            ],
            "path_or_id",
        )
    }
}
#[derive(Default, Deserialize, JsonSchema)]
/// Unified architectural, graph hygiene, syntax, and documentation lint query.
pub struct Lint {
    /// Relative file, directory, or glob target (aliases: path, file_path); omit for full workspace.
    pub target: Option<String>,
    /// Alias for target.
    pub path: Option<String>,
    /// Alias for target.
    pub file_path: Option<String>,
    /// Optional lint categories (`arch`, `duplicates`, `dead_tails`, `cycles`, `syntax`, `docs`); omit for default `[arch, syntax, docs]`.
    pub categories: Option<Vec<LintCategory>>,
    /// Optional rule or invariant ID filters.
    pub rules: Option<Vec<String>>,
    /// Optional minimum severity threshold (`warn` or `error`).
    pub severity: Option<LintSeverity>,
    #[serde(flatten)]
    /// Pagination, detail, worktree, and output format options.
    pub page: PageInput,
}
impl Lint {
    pub(super) fn target(&self) -> anyhow::Result<Option<&str>> {
        aliased_value(
            self.target.as_deref(),
            &[self.path.as_deref(), self.file_path.as_deref()],
            "target",
        )
    }
}
#[derive(Deserialize, JsonSchema)]
/// Built-in ACDD topic guide and workspace config scaffolding parameters.
pub struct Guide {
    #[serde(default)]
    /// Optional topic: query (default), lifecycle, acdd, gates, subtasks, code, docs, ast, adapter, init, or validate. The query index advertises enabled topics; known disabled topics return empty guidance.
    pub topic: Option<String>,
    #[serde(default)]
    /// Overwrite existing scaffolded files when `topic == "init"`.
    pub force: bool,
    /// Worktree path or unique name/prefix selector; omit or '.' for server root.
    pub worktree: Option<String>,
    /// Output format: markdown (default) or json.
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

/// Supported active-run actions for the canonical `acdd_state` MCP tool.
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
    /// Start a new `Run` from a spec file or ephemeral `--goal` (`StateCommand::Start`).
    Start,
    /// Extend a task's admitted write scope (`StateCommand::ExtendScope`).
    ExtendScope,
    /// Reopen a gate or task container (`StateCommand::Reopen`).
    Reopen,
    /// Synchronize spec edits and optional workflow rebind into an active `Run` (`StateCommand::Sync`).
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
        )
    }
}

/// Canonical `acdd_state` MCP tool input covering 8 active-run `StateCommand` actions and 6 read-only `StateQuery` actions.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StateInput {
    /// State action (`status`, `list`, `show`, `next`, `context`, `observability`, `start`, `extend_scope`, `reopen`, `sync`, `subtask`, `snapshot`, `clear`, `run`).
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
    /// Optional workflow override (`start`).
    pub workflow: Option<String>,
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
    /// Additional repository-relative paths or globs to admit (`extend_scope`).
    pub paths: Option<Vec<String>>,
    /// Justification/cancellation reason (`extend_scope`, `reopen`, `clear`).
    pub reason: Option<String>,
    /// Optional worker identity check (`extend_scope`).
    pub worker_id: Option<WorkerId>,
    /// Cascade-stale downstream dependent tasks (`reopen`).
    pub cascade: Option<bool>,
    /// Rebind non-completed gates against the latest resolved workflow (`sync`).
    pub rebind_workflow: Option<bool>,
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
    /// Include `run.profile_json` on node-level `show` (`false` by default to omit ~25 KiB bloat).
    pub include_profile: Option<bool>,
    /// Include parsed JSON payloads for persisted gate artifacts (`proof | findings | receipt | all`) on `show`.
    pub artifact: Option<acdd_state::usecases::ArtifactSelector>,
    /// Context detail budget (`compact` default <= 10,000 bytes, or `full` up to 32 KiB) on `context`.
    pub detail: Option<acdd_core::response::Detail>,
    /// Worktree routing selector (or boolean `true` on `action="start"` to create a managed worktree).
    pub worktree: Option<WorktreeInput>,
    /// Output format: markdown (default) or json.
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
    /// Context detail budget (`compact` default <= 10,000 bytes, or `full` up to 32 KiB).
    #[serde(default)]
    pub detail: Option<Detail>,
    /// Output format: markdown (default) or json.
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
    /// Output format: markdown (default) or json.
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
    /// Optional `--test` target forwarded to scoped verification contracts (`submit` requires a prior fresh `verify` trace on `command:`/`call:`/`checks:` gates).
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
    /// Output format: markdown (default) or json.
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
    /// Output format: markdown (default) or json.
    #[serde(default)]
    #[schemars(schema_with = "output_format_schema")]
    pub format: Option<OutputFormat>,
}
