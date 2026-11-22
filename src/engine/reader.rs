//! Layered reader shared by the CLI and MCP transports.

use acdd_core::response::{QueryOptions, SourceOptions};
use acdd_core::{
    config::{resolve_file_path, Adapter},
    model::{ReadBounds, ReadMode, ReadResult},
    subsystems::{disabled_read_mode_error, filter_subsystem_tools},
};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

/// Input shared by both reader transports.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadRequest {
    /// Workspace-relative file, including configured linked-workspace paths.
    pub path: Option<String>,
    /// Optional batch file paths when reading multiple files.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Indexed symbol selector. When supplied, auto mode selects snippet.
    pub selector: Option<String>,
    /// auto, lines, raw, search, delta, snippet, or outline.
    #[serde(default)]
    #[schemars(with = "String")]
    pub mode: ReadMode,
    /// One-based first line.
    pub start_line: Option<usize>,
    /// Maximum lines, default 200 for text modes.
    pub line_count: Option<usize>,
    /// Search pattern, interpreted literally unless regex is true.
    pub pattern: Option<String>,
    /// Interpret the search pattern as a regular expression.
    #[serde(default)]
    pub regex: bool,
    /// Search context lines on each side, default 2.
    pub context: Option<usize>,
    /// Git revision for delta; defaults to the active task baseline or HEAD.
    pub baseline: Option<String>,
    /// Export the returned content to a confined workspace-relative file.
    pub output_file: Option<String>,
    /// Include Hints footer with follow-up MCP suggestions (default false).
    #[serde(default)]
    pub include_hints: bool,
}

/// Strips workspace root prefix from absolute paths, resolving to workspace-relative path.
pub fn strip_root_path(path: &str, root: &Path) -> Option<String> {
    let p = Path::new(path);
    if !p.is_absolute()
        || p.components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return None;
    }
    if p.ancestors().any(|component| {
        !acdd_core::fs::is_system_root_symlink(component)
            && std::fs::symlink_metadata(component)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
    }) {
        return None;
    }
    if let Ok(rel) = p.strip_prefix(root) {
        return Some(rel.to_string_lossy().into_owned());
    }
    if let (Ok(can_p), Ok(can_root)) = (p.canonicalize(), root.canonicalize()) {
        if let Ok(rel) = can_p.strip_prefix(&can_root) {
            return Some(rel.to_string_lossy().into_owned());
        }
    }
    let root_str = root.to_string_lossy();
    let root_trimmed = root_str.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix(root_trimmed) {
        if rest.is_empty() {
            return Some(String::new());
        }
        if rest.starts_with([std::path::MAIN_SEPARATOR, '/']) {
            return Some(rest[1..].to_owned());
        }
    }
    None
}

impl ReadRequest {
    /// Check subsystem admission before either transport opens the code index.
    pub fn requires_index(&self, adapter: &Adapter) -> Result<bool> {
        let mode = self.resolved_mode();
        let is_md_path = acdd_core::is_doc_path;
        let mut target_paths = Vec::new();
        if let Some(p) = self.path.as_deref() {
            target_paths.push(p);
        }
        if let Some(paths) = &self.paths {
            for p in paths {
                target_paths.push(p.as_str());
            }
        }
        if target_paths.is_empty() {
            if let Some(error) = disabled_read_mode_error(mode, false, &adapter.modules) {
                bail!("{error}");
            }
        } else {
            for p in &target_paths {
                let markdown = is_md_path(p);
                if let Some(error) = disabled_read_mode_error(mode, markdown, &adapter.modules) {
                    bail!("{error}");
                }
            }
        }
        Ok(matches!(mode, ReadMode::Snippet | ReadMode::Outline))
    }
    /// Resolve automatic mode without requiring an index.
    pub fn resolved_mode(&self) -> ReadMode {
        match self.mode {
            ReadMode::Auto if self.selector.is_some() => ReadMode::Snippet,
            ReadMode::Auto if self.pattern.is_some() => ReadMode::Search,
            ReadMode::Auto => ReadMode::Lines,
            mode => mode,
        }
    }
}

fn sensitive_with_case(adapter: &Adapter, path: &str, case_insensitive: bool) -> Result<()> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    builder.case_insensitive(case_insensitive)?;
    for pattern in &adapter.sensitive {
        builder.add_line(None, pattern)?;
    }
    ensure!(
        !builder
            .build()?
            .matched_path_or_any_parents(path, false)
            .is_ignore(),
        "SENSITIVE_FILE: blocked by sensitive policy"
    );
    Ok(())
}

fn sensitive(adapter: &Adapter, root: &Path, path: &str) -> Result<()> {
    sensitive_with_case(
        adapter,
        path,
        acdd_core::fs::is_case_insensitive_volume(root),
    )
}

fn admitted_file(root: &Path, adapter: &Adapter, path: &str) -> Result<std::path::PathBuf> {
    let stripped = strip_root_path(path, root);
    let path = stripped.as_deref().unwrap_or(path);
    sensitive(adapter, root, path)?;
    let file = resolve_file_path(root, adapter, path)?;
    ensure!(
        file.exists(),
        "FILE_NOT_FOUND: file '{path}' does not exist"
    );
    ensure!(file.is_file(), "reader path must name a regular file");
    let file_str = file.to_string_lossy();
    let file_ci = acdd_core::fs::is_case_insensitive_volume(&file);
    let relative = strip_root_path(&file_str, root).unwrap_or_else(|| {
        file.strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned()
    });
    sensitive_with_case(adapter, &relative, file_ci)?;
    for linked in &adapter.linked_workspaces {
        if let Some(relative) = strip_root_path(&file_str, &linked.path).or_else(|| {
            file.strip_prefix(&linked.path)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
        }) {
            let linked_ci = acdd_core::fs::is_case_insensitive_volume(&linked.path) || file_ci;
            sensitive_with_case(adapter, &relative, linked_ci)?;
        }
    }
    let mut prefix = [0; 8192];
    let count = File::open(&file)?.read(&mut prefix)?;
    ensure!(
        !prefix[..count].contains(&0),
        "BINARY_FILE: NUL byte in first 8 KiB"
    );
    Ok(file)
}

/// Active `in_progress` node context resolved read-only from `state.sqlite` for `acdd_read`.
pub struct ActiveNodeContext {
    /// Active run identifier.
    pub run_id: acdd_core::ids::RunId,
    /// Active node cursor (`<gate>` or `<task_ref>.<gate>`).
    pub node_cursor: String,
    /// Whether the active node is a root gate (unscoped root gates default to `**`).
    pub is_root_gate: bool,
    /// Whether the active node uses in-place isolation (`claim_tree` precedes `baseline_commit`).
    pub is_in_place: bool,
    /// Baseline commit OID, if present on the active node.
    pub baseline_commit: Option<String>,
    /// Claim tree OID, if present on the active node.
    pub claim_tree: Option<String>,
    /// Effective admitted write scope for the node's container.
    pub admitted_scope: Vec<String>,
    /// Declarative protected path globs from the resolved gate profile.
    pub protected_paths: Vec<String>,
    /// Canonical workspace path policy incorporating engine-managed specs and DB paths.
    pub path_policy: acdd_core::path_policy::WorkspacePathPolicy,
    /// Whether the active gate configures a registered command.
    pub has_command: bool,
}

/// Read a file or symbol using already admitted graph state and optional pre-resolved active-node scope context.
#[allow(clippy::too_many_arguments)]
pub fn acdd_read_tool(
    root: &Path,
    adapter: &Adapter,
    request: &ReadRequest,
    page: &QueryOptions,
    source: &SourceOptions,
    conn: Option<&Connection>,
    node_ctx: Option<&ActiveNodeContext>,
) -> Result<Value> {
    if request.resolved_mode() == ReadMode::Lines {
        line_coordinates(request.start_line.unwrap_or(1), request.line_count)?;
    }
    let normalized_req = with_stripped_root_path(request, root);
    let request = normalized_req.as_ref().unwrap_or(request);
    if let Some(paths) = &request.paths {
        if !paths.is_empty() {
            let normalized_paths: Vec<String> = paths
                .iter()
                .map(|p| strip_root_path(p, root).unwrap_or_else(|| p.clone()))
                .collect();
            let paths = &normalized_paths;
            if let Some(output) = &request.output_file {
                sensitive(adapter, root, output)?;
                let destination = acdd_core::fs::checked_child(root, Path::new(output))?;
                for p in paths {
                    if let Ok(input_res) = resolve_file_path(root, adapter, p) {
                        ensure!(
                            !acdd_core::fs::same_filesystem_path(&destination, &input_res),
                            "output_file must differ from any input file in paths"
                        );
                    }
                    if request.resolved_mode() == ReadMode::Search
                        && is_directory_or_glob_search(root, adapter, p)?
                    {
                        if let Some(scope) = resolve_search_scope(root, adapter, p)? {
                            ensure_export_not_in_search_scope(&scope, Some(&destination))?;
                        }
                    }
                }
            }
            let mut results = Vec::new();
            let mut markdown_sections = Vec::new();
            for p in paths {
                let mut single_req = request.clone();
                single_req.path = Some(p.clone());
                single_req.paths = None;
                single_req.output_file = None;
                match acdd_read_single(
                    root,
                    adapter,
                    &single_req,
                    page,
                    source,
                    conn,
                    node_ctx,
                ) {
                    Ok(mut val) => {
                        let content = exportable_content(&val).unwrap_or("");
                        markdown_sections.push(format!("### {p}\n\n{content}"));
                        if let Some(obj) = val.as_object_mut() {
                            obj.remove("content");
                        }
                        results.push(val);
                    }
                    Err(e) => {
                        markdown_sections.push(format!("### {p}\n\n*Error reading file: {e}*"));
                        results.push(json!({
                            "path": p,
                            "error": e.to_string(),
                        }));
                    }
                }
            }
            let aggregate_content = markdown_sections.join("\n\n---\n\n");
            let mut response = json!({
                "mode": "batch",
                "paths": paths,
                "batch": results,
                "content": aggregate_content,
            });
            if let Some(output) = &request.output_file {
                let destination = acdd_core::fs::checked_child(root, Path::new(output))?;
                acdd_core::fs::atomic_write(&destination, aggregate_content.as_bytes(), false)?;
                response["output_file"] = json!(output);
            }
            return Ok(response);
        }
    }
    acdd_read_single(root, adapter, request, page, source, conn, node_ctx)
}

fn line_coordinates(start: usize, count: Option<usize>) -> Result<(i64, Option<i64>)> {
    let first = i64::try_from(start)
        .context("invalid line bounds: start_line exceeds SQLite coordinates")?;
    let last = count
        .map(|count| {
            count
                .checked_sub(1)
                .and_then(|offset| start.checked_add(offset))
                .and_then(|end| i64::try_from(end).ok())
                .context("invalid line bounds: range exceeds SQLite coordinates")
        })
        .transpose()?;
    Ok((first, last))
}

fn with_stripped_root_path(request: &ReadRequest, root: &Path) -> Option<ReadRequest> {
    let stripped = request
        .path
        .as_deref()
        .and_then(|p| strip_root_path(p, root))?;
    let mut normalized = request.clone();
    normalized.path = Some(stripped);
    Some(normalized)
}

fn exportable_content(value: &Value) -> Option<&str> {
    value["source"].as_str().or(value["content"].as_str())
}

fn query_enclosing_symbol(
    conn: &Connection,
    clean_path: &str,
    start_line: i64,
    end_line: i64,
) -> Result<Option<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.kind, CASE WHEN (SELECT count(*) FROM nodes duplicate
             WHERE duplicate.path_id = n.path_id AND duplicate.name = n.name) > 1
             THEN CASE WHEN (SELECT count(*) FROM nodes qualified
                 WHERE qualified.path_id = n.path_id AND qualified.qualname = n.qualname) = 1
                 THEN n.qualname ELSE n.id END ELSE n.name END
         FROM path_dictionary p
         JOIN nodes n ON n.path_id = p.path_id
         WHERE (p.path = ?1 OR (substr(p.path, -length(?1) - 1) = '/' || ?1
                AND NOT EXISTS (SELECT 1 FROM path_dictionary exact WHERE exact.path = ?1)))
           AND n.line <= ?2
           AND n.end_line >= ?3
           AND n.kind IN ('function', 'method', 'struct', 'class', 'trait', 'interface', 'enum')
         ORDER BY (p.path = ?1) DESC, (n.end_line - n.line) ASC, n.id
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![clean_path, start_line, end_line])?;
    if let Some(row) = rows.next()? {
        return Ok(Some((row.get(0)?, row.get(1)?)));
    }
    Ok(None)
}

fn find_enclosing_symbol(
    conn: &Connection,
    path: &str,
    start_line: i64,
    end_line: Option<i64>,
) -> Result<Option<(String, String)>> {
    let clean_path = path.strip_prefix("./").unwrap_or(path);
    let check_end = end_line.unwrap_or(start_line);
    if let Some(found) = query_enclosing_symbol(conn, clean_path, start_line, check_end)? {
        return Ok(Some(found));
    }
    if check_end != start_line {
        return query_enclosing_symbol(conn, clean_path, start_line, start_line);
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn acdd_read_single(
    root: &Path,
    adapter: &Adapter,
    request: &ReadRequest,
    page: &QueryOptions,
    source: &SourceOptions,
    conn: Option<&Connection>,
    node_ctx: Option<&ActiveNodeContext>,
) -> Result<Value> {
    let normalized_req = with_stripped_root_path(request, root);
    let request = normalized_req.as_ref().unwrap_or(request);
    let mode = request.resolved_mode();
    let markdown = request
        .path
        .as_deref()
        .is_some_and(|p| p.ends_with(".md") || p.ends_with(".markdown"));
    request.requires_index(adapter)?;
    let mut value = if mode == ReadMode::Snippet {
        let conn = conn.context("snippet requires the code index")?;
        let selector = request
            .selector
            .as_deref()
            .or(request.path.as_deref())
            .context("snippet requires selector")?;
        // Resolve metadata first; enforce policy before the snippet owner reads source.
        let inspection = acdd_index::db::reader::inspect_paged_with_path(
            conn,
            selector,
            request
                .path
                .as_deref()
                .filter(|_| request.selector.is_some()),
            false,
            page,
        )?;
        let selected = &inspection["node"];
        let path = selected["path"]
            .as_str()
            .context("selected symbol has no path")?;
        admitted_file(root, adapter, path)?;
        let mut value = acdd_index::db::symbols::snippet_paged_with_response_budget(
            conn,
            root,
            selector,
            request
                .path
                .as_deref()
                .filter(|_| request.selector.is_some()),
            source,
            page,
            adapter.response.max_output_bytes,
        )?;
        if let (Some(start), Some(end)) = (
            value["source_preview"]["start_line"].as_u64(),
            value["source_preview"]["end_line"].as_u64(),
        ) {
            value["header"] = json!(format!(
                "{} {} in {path}:{start}-{end}",
                value["node"]["kind"].as_str().unwrap_or("symbol"),
                value["node"]["qualname"]
                    .as_str()
                    .or(value["node"]["name"].as_str())
                    .unwrap_or(selector)
            ));
        }
        value
    } else {
        let path = request
            .path
            .as_deref()
            .or((mode == ReadMode::Search).then_some("."))
            .context("reader mode requires path")?;
        let bounds = ReadBounds {
            start_line: request.start_line,
            line_count: request.line_count,
        };
        let export_dest = request
            .output_file
            .as_deref()
            .map(|output| -> Result<std::path::PathBuf> {
                sensitive(adapter, root, output)?;
                Ok(acdd_core::fs::checked_child(root, Path::new(output))?)
            })
            .transpose()?;
        let (content, truncated, single_file) = if mode == ReadMode::Search
            && is_directory_or_glob_search(root, adapter, path)?
        {
            let (content, truncated) = search_directory_or_glob(
                root,
                adapter,
                path,
                &bounds,
                request,
                export_dest.as_deref(),
            )?;
            (content, truncated, false)
        } else {
            let file = admitted_file(root, adapter, path)?;
            let (content, truncated) = match mode {
                ReadMode::Delta => (
                    compute_file_delta(root, &file, request.baseline.as_deref(), node_ctx)?,
                    false,
                ),
                ReadMode::Outline => {
                    let parent = file.parent().context("outline file needs a parent")?;
                    let name = file.file_name().context("outline file needs a file name")?;
                    let mut opts = std::fs::OpenOptions::new();
                    opts.read(true);
                    let handle = acdd_core::fs::open_confined(parent, Path::new(name), &mut opts)?;
                    let metadata = handle.metadata()?;
                    ensure!(metadata.is_file(), "reader path must name a regular file");
                    ensure!(
                        metadata.len() <= adapter.limits.max_file_bytes,
                        "outline file exceeds max_file_bytes"
                    );
                    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(64 * 1024));
                    handle
                        .take(adapter.limits.max_file_bytes.saturating_add(1))
                        .read_to_end(&mut bytes)?;
                    ensure!(
                        bytes.len() as u64 <= adapter.limits.max_file_bytes,
                        "outline file exceeds max_file_bytes"
                    );
                    let text =
                        String::from_utf8(bytes).context("outline file is not valid UTF-8")?;
                    if markdown {
                        (
                            acdd_index::docs::render_outline(path, &text, &adapter.spec_paths())?,
                            false,
                        )
                    } else {
                        let profile = acdd_index::languages::for_path(&file)
                            .context("no compiled language profile for outline")?;
                        let facts = acdd_index::ast::extract(path, profile.id(), &text)?;
                        (
                            facts
                                .nodes
                                .iter()
                                .filter(|n| n.kind != "module")
                                .map(|n| {
                                    format!(
                                        "{}:{} {} {}",
                                        n.line,
                                        n.end_line,
                                        n.kind,
                                        n.details["signature"].as_str().unwrap_or(&n.qualname)
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            false,
                        )
                    }
                }
                _ => extract_file_slice(&file, mode, &bounds, request)?,
            };
            (content, truncated, true)
        };
        let mut val = serde_json::to_value(ReadResult {
            path: path.into(),
            mode,
            content,
            truncated,
        })?;
        if mode == ReadMode::Delta {
            if let Some(ctx) = node_ctx {
                if let Some(ref baseline) = ctx.baseline_commit {
                    val["baseline_commit"] = json!(baseline);
                }
                if let Some(ref claim_tree) = ctx.claim_tree {
                    val["claim_tree"] = json!(claim_tree);
                }
            }
        }
        if !single_file {
            if let (Some(output), Some(destination)) = (&request.output_file, export_dest) {
                let content = exportable_content(&val).context("read has no exportable content")?;
                acdd_core::fs::atomic_write(&destination, content.as_bytes(), false)?;
                val["output_file"] = json!(output);
            }
            return Ok(val);
        }
        val
    };
    let path = value["node"]["path"]
        .as_str()
        .or(request.path.as_deref())
        .unwrap_or("")
        .to_owned();
    if adapter.modules.is_enabled("codemap") {
        if let Some(conn) = conn {
            let pulse = graph_signals(conn, &path, value.get("node"), adapter)?;
            if pulse.as_object().is_some_and(|map| !map.is_empty()) {
                value["graph_pulse"] = pulse;
            }
        }
    }
    if let Some(ctx) = node_ctx {
        let root_unscoped = [String::from("**")];
        let scope = if ctx.admitted_scope.is_empty() && ctx.is_root_gate {
            &root_unscoped[..]
        } else {
            &ctx.admitted_scope[..]
        };
        let allowed = ctx
            .path_policy
            .is_agent_writable(&path, scope, &ctx.protected_paths);
        value["in_write_scope"] = json!(if allowed { "ALLOWED" } else { "OUT_OF_SCOPE" });
    }
    let footer_hint = if mode == ReadMode::Lines {
        if let (Some(conn), Some(start_line)) = (conn, request.start_line) {
            let (start, end_line) = line_coordinates(start_line, request.line_count)?;
            find_enclosing_symbol(conn, &path, start, end_line)?.map(|(kind, name)| {
                let kind_label = match kind.as_str() {
                    "function" | "method" => "fn",
                    "struct" => "struct",
                    "class" => "class",
                    "trait" => "trait",
                    "enum" => "enum",
                    "interface" => "interface",
                    other => other,
                };
                format!(
                    "💡 Enclosing symbol: '{kind_label} {name}'. To read this entire symbol safely with verified digest, use acdd_read(path=\"{path}\", selector=\"{name}\")"
                )
            })
        } else {
            None
        }
    } else {
        None
    };
    if let Some(ref footer) = footer_hint {
        value["footer"] = json!(footer);
    }
    if request.include_hints {
        let mut hints = render_read_hints(
            root,
            conn,
            adapter,
            &path,
            &value,
            node_ctx,
            value["in_write_scope"] == "OUT_OF_SCOPE",
        )?;
        if let Some(ref footer) = footer_hint {
            if hints.is_empty() {
                hints = footer.clone();
            } else {
                hints = format!("{footer}\n{hints}");
            }
        }
        if !hints.is_empty() {
            value["Hints"] = json!(hints);
        }
    }
    if let Some(output) = &request.output_file {
        sensitive(adapter, root, output)?;
        let destination = acdd_core::fs::checked_child(root, Path::new(output))?;
        let input_res = resolve_file_path(root, adapter, &path)?;
        ensure!(
            !acdd_core::fs::same_filesystem_path(&destination, &input_res),
            "output_file must differ from input"
        );
        let content = exportable_content(&value).context("read has no exportable content")?;
        acdd_core::fs::atomic_write(&destination, content.as_bytes(), false)?;
        value["output_file"] = json!(output);
    }
    Ok(value)
}

struct SearchMatcher<'a> {
    pattern: &'a str,
    regex: Option<regex::Regex>,
}

impl<'a> SearchMatcher<'a> {
    fn new(request: &'a ReadRequest) -> Result<Self> {
        let pattern = request
            .pattern
            .as_deref()
            .context("search requires pattern")?;
        ensure!(!pattern.is_empty(), "search pattern must not be empty");
        let regex = request
            .regex
            .then(|| regex::Regex::new(pattern))
            .transpose()?;
        Ok(Self { pattern, regex })
    }

    fn is_match(&self, line: &str) -> bool {
        self.regex
            .as_ref()
            .map_or_else(|| line.contains(self.pattern), |r| r.is_match(line))
    }
}

fn split_linked_workspace<'a>(
    root: &'a Path,
    adapter: &'a Adapter,
    raw_path: &'a str,
) -> Result<(&'a Path, &'a str, Option<String>)> {
    if raw_path.starts_with('[') {
        if let Some((prefix, rest)) = raw_path.split_once("]/") {
            let ws_name = prefix.trim_start_matches('[');
            if let Some(lw) = adapter.linked_workspaces.iter().find(|w| w.name == ws_name) {
                return Ok((
                    lw.path.as_path(),
                    rest.strip_prefix("./").unwrap_or(rest),
                    Some(format!("[{ws_name}]/")),
                ));
            }
        }
    }
    Ok((root, raw_path.strip_prefix("./").unwrap_or(raw_path), None))
}

fn has_glob_meta(path: &str) -> bool {
    path.contains(['*', '?', '[', ']'])
}

fn is_directory_or_glob_search(root: &Path, adapter: &Adapter, path: &str) -> Result<bool> {
    let (_, rel_spec, linked) = split_linked_workspace(root, adapter, path)?;
    if linked.is_none() && path.starts_with('[') && path.contains("]/") {
        return Ok(false);
    }
    if rel_spec.is_empty() || rel_spec == "." || has_glob_meta(rel_spec) {
        return Ok(true);
    }
    Ok(resolve_file_path(root, adapter, path)
        .map(|resolved| resolved.is_dir())
        .unwrap_or(false))
}

fn is_binary_prefix(file: &Path) -> Result<bool> {
    let mut prefix = [0; 8192];
    let count = File::open(file)?.read(&mut prefix)?;
    Ok(prefix[..count].contains(&0))
}

fn normalize_rel_display(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

struct SearchScope<'a> {
    base_root: &'a Path,
    walk_dir: std::path::PathBuf,
    glob_matcher: Option<ignore::overrides::Override>,
    linked_prefix: Option<String>,
}

fn resolve_search_scope<'a>(
    root: &'a Path,
    adapter: &'a Adapter,
    raw_path: &'a str,
) -> Result<Option<SearchScope<'a>>> {
    let (base_root, rel_spec, linked_prefix) = split_linked_workspace(root, adapter, raw_path)?;
    ensure!(
        !rel_spec.starts_with('/')
            && !rel_spec.contains('\\')
            && !rel_spec.contains('\0')
            && !Path::new(rel_spec)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "path must be a relative plain path without parent traversal"
    );
    let is_glob = has_glob_meta(rel_spec);
    let (walk_dir, glob_matcher) = if is_glob {
        let mut prefix_parts = Vec::new();
        for segment in rel_spec.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if has_glob_meta(segment) {
                break;
            }
            prefix_parts.push(segment);
        }
        let walk_dir = if prefix_parts.is_empty() {
            base_root.to_path_buf()
        } else {
            let prefix_rel = prefix_parts.join("/");
            sensitive(adapter, root, &prefix_rel)?;
            let dir = acdd_core::fs::checked_child(base_root, Path::new(&prefix_rel))?;
            if !dir.is_dir() {
                return Ok(None);
            }
            dir
        };
        let mut ov = ignore::overrides::OverrideBuilder::new(base_root);
        ov.add(rel_spec.trim_start_matches("./"))
            .with_context(|| format!("invalid search glob pattern '{raw_path}'"))?;
        (walk_dir, Some(ov.build()?))
    } else {
        if rel_spec != "." && !rel_spec.is_empty() {
            sensitive(adapter, root, raw_path)?;
        }
        let walk_dir = if rel_spec.is_empty() || rel_spec == "." {
            base_root.to_path_buf()
        } else {
            acdd_core::fs::checked_child(base_root, Path::new(rel_spec))?
        };
        ensure!(
            walk_dir.exists(),
            "FILE_NOT_FOUND: file '{raw_path}' does not exist"
        );
        (walk_dir, None)
    };
    Ok(Some(SearchScope {
        base_root,
        walk_dir,
        glob_matcher,
        linked_prefix,
    }))
}

fn ensure_export_not_in_search_scope(
    scope: &SearchScope<'_>,
    export_dest: Option<&Path>,
) -> Result<()> {
    let Some(dest) = export_dest else {
        return Ok(());
    };
    if let (Some(dest_name), Ok(can_parent), Ok(can_walk), Ok(can_base)) = (
        dest.file_name(),
        dest.parent()
            .context("export destination has no parent")
            .and_then(|p| Ok(p.canonicalize()?)),
        scope.walk_dir.canonicalize(),
        scope.base_root.canonicalize(),
    ) {
        let can_dest = can_parent.join(dest_name);
        if can_dest.starts_with(&can_walk) {
            let rel_match = can_dest
                .strip_prefix(&can_base)
                .ok()
                .map(normalize_rel_display);
            let in_scope = match (&scope.glob_matcher, rel_match) {
                (Some(ov), Some(rel)) => ov.matched(Path::new(&rel), false).is_whitelist(),
                (None, _) => true,
                _ => false,
            };
            ensure!(!in_scope, "output_file must differ from input");
        }
    }
    Ok(())
}

fn search_directory_or_glob(
    root: &Path,
    adapter: &Adapter,
    raw_path: &str,
    bounds: &ReadBounds,
    request: &ReadRequest,
    export_dest: Option<&Path>,
) -> Result<(String, bool)> {
    const MAX_SEARCH_BYTES: usize = 48 * 1024;
    let start = bounds.start_line.unwrap_or(1);
    let count = bounds.line_count.unwrap_or(200);
    ensure!(
        start > 0 && count > 0 && count <= 10000,
        "line bounds require start_line >= 1 and line_count 1..=10000"
    );
    let matcher = SearchMatcher::new(request)?;
    let context = request.context.unwrap_or(2).min(100);
    let Some(scope) = resolve_search_scope(root, adapter, raw_path)? else {
        return Ok((String::new(), false));
    };
    ensure_export_not_in_search_scope(&scope, export_dest)?;

    let SearchScope {
        base_root,
        walk_dir,
        glob_matcher,
        linked_prefix,
    } = scope;

    let base_ci = acdd_core::fs::is_case_insensitive_volume(base_root);
    let mut output = String::new();
    let mut kept = 0;
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(&walk_dir)
        .follow_links(false)
        .standard_filters(true)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != ".acdd"
        })
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(base_root) else {
            continue;
        };
        let rel_str = normalize_rel_display(rel);
        if let Some(ref ov) = glob_matcher {
            if !ov.matched(Path::new(&rel_str), false).is_whitelist() {
                continue;
            }
        }
        if let Some(dest) = export_dest {
            ensure!(
                !acdd_core::fs::same_filesystem_path(dest, path),
                "output_file must differ from input"
            );
        }
        let display_path = match &linked_prefix {
            Some(prefix) => format!("{prefix}{rel_str}"),
            None => rel_str.clone(),
        };
        if sensitive(adapter, root, &display_path).is_err()
            || (linked_prefix.is_some() && sensitive_with_case(adapter, &rel_str, base_ci).is_err())
        {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > adapter.limits.max_file_bytes {
            continue;
        }
        if is_binary_prefix(path).unwrap_or(true) {
            continue;
        }

        let file_truncated = scan_matching_lines(
            path,
            start,
            count,
            context,
            MAX_SEARCH_BYTES,
            Some(&display_path),
            &matcher,
            &mut output,
            &mut kept,
        )?;
        if file_truncated {
            truncated = true;
            break;
        }
    }
    Ok((output, truncated))
}

#[allow(clippy::too_many_arguments)]
fn scan_matching_lines(
    file: &Path,
    start: usize,
    count: usize,
    context: usize,
    max_bytes: usize,
    path_prefix: Option<&str>,
    matcher: &SearchMatcher<'_>,
    output: &mut String,
    kept: &mut usize,
) -> Result<bool> {
    let mut prior: std::collections::VecDeque<(usize, String)> = std::collections::VecDeque::new();
    let mut after = 0;
    let mut last = 0;
    let mut reader = BufReader::new(File::open(file)?);
    let mut raw_line = Vec::new();
    let mut number = 0;
    let prefix_len = path_prefix.map_or(0, |p| p.len() + 1);
    let saved_len = output.len();
    let saved_kept = *kept;
    loop {
        raw_line.clear();
        let bytes = reader
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut raw_line)?;
        if bytes == 0 {
            return Ok(false);
        }
        if path_prefix.is_some() {
            if bytes > 1024 * 1024 {
                output.truncate(saved_len);
                *kept = saved_kept;
                return Ok(false);
            }
        } else {
            ensure!(bytes <= 1024 * 1024, "line exceeds reader byte limit");
        }
        let line = match std::str::from_utf8(&raw_line) {
            Ok(s) => s.to_owned(),
            Err(err) => {
                if path_prefix.is_some() {
                    output.truncate(saved_len);
                    *kept = saved_kept;
                    return Ok(false);
                }
                return Err(err.into());
            }
        };
        number += 1;
        if number < start {
            continue;
        }
        let matched = matcher.is_match(&line);
        if matched {
            after = context + 1;
        }
        let include = after > 0;
        if include {
            if matched {
                for (n, text) in &prior {
                    if *n > last
                        && *kept < count
                        && output.len() + prefix_len + text.len() + 32 <= max_bytes
                    {
                        if let Some(p) = path_prefix {
                            output.push_str(&format!("{p}:{n}: {text}"));
                        } else {
                            output.push_str(&format!("{n}: {text}"));
                        }
                        if !output.ends_with('\n') {
                            output.push('\n');
                        }
                        *kept += 1;
                        last = *n;
                    }
                }
            }
            if *kept >= count || output.len() + prefix_len + line.len() + 32 > max_bytes {
                return Ok(true);
            }
            if let Some(p) = path_prefix {
                output.push_str(&format!("{p}:{number}: {line}"));
            } else {
                output.push_str(&format!("{number}: {line}"));
            }
            if !output.ends_with('\n') {
                output.push('\n');
            }
            *kept += 1;
            last = number;
        }
        after = after.saturating_sub(1);
        prior.push_back((number, line.clone()));
        while prior.len() > context {
            prior.pop_front();
        }
    }
}

/// Read bounded text windows or matching lines without loading the complete file.
pub fn extract_file_slice(
    file: &Path,
    mode: ReadMode,
    bounds: &ReadBounds,
    request: &ReadRequest,
) -> Result<(String, bool)> {
    const MAX_SLICE_BYTES: usize = 48 * 1024;
    let start = bounds.start_line.unwrap_or(1);
    let count = bounds.line_count.unwrap_or(200);
    ensure!(
        start > 0 && count > 0 && count <= 10000,
        "line bounds require start_line >= 1 and line_count 1..=10000"
    );
    let matcher = (mode == ReadMode::Search)
        .then(|| SearchMatcher::new(request))
        .transpose()?;
    let context = request.context.unwrap_or(2).min(100);
    let mut output = String::new();
    let mut kept = 0;
    if let Some(ref matcher) = matcher {
        let truncated = scan_matching_lines(
            file,
            start,
            count,
            context,
            MAX_SLICE_BYTES,
            None,
            matcher,
            &mut output,
            &mut kept,
        )?;
        return Ok((output, truncated));
    }
    let mut truncated = false;
    let mut reader = BufReader::new(File::open(file)?);
    let mut line = String::new();
    let mut number = 0;
    loop {
        line.clear();
        // Bound even a single physical line before allocating it in memory.
        let bytes = reader.by_ref().take(1024 * 1024 + 1).read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        ensure!(bytes <= 1024 * 1024, "line exceeds reader byte limit");
        number += 1;
        if number < start {
            continue;
        }
        if kept >= count || output.len() + line.len() + 32 > MAX_SLICE_BYTES {
            truncated = true;
            break;
        }
        if mode == ReadMode::Raw {
            output.push_str(&line);
        } else {
            output.push_str(&format!("{number}: {line}"));
        }
        kept += 1;
    }
    Ok((output, truncated))
}

/// Compute a path-confined unified diff against HEAD or the active node baseline/claim_tree.
pub fn compute_file_delta(
    _root: &Path,
    file: &Path,
    baseline: Option<&str>,
    node_ctx: Option<&ActiveNodeContext>,
) -> Result<String> {
    // Resolve Git from the admitted physical file, including linked repositories.
    let git_directory = file.parent().context("delta file has no parent")?;
    let path = file
        .file_name()
        .and_then(|name| name.to_str())
        .context("delta filename is not UTF-8")?;
    let node_baseline = if baseline.is_none() {
        node_ctx.and_then(|ctx| {
            if ctx.is_in_place {
                ctx.claim_tree
                    .clone()
                    .or_else(|| ctx.baseline_commit.clone())
            } else {
                ctx.baseline_commit
                    .clone()
                    .or_else(|| ctx.claim_tree.clone())
            }
        })
    } else {
        None
    };
    let revision = baseline.or(node_baseline.as_deref()).unwrap_or("HEAD");
    let diff = acdd_core::git::diff_worktree_file_against_rev(git_directory, revision, path, 3)?;
    ensure!(
        diff.len() <= 5 * 1024 * 1024,
        "delta exceeds reader byte limit"
    );
    Ok(diff)
}

fn graph_signals(
    conn: &Connection,
    path: &str,
    node: Option<&Value>,
    adapter: &Adapter,
) -> Result<Value> {
    let id = node.and_then(|n| n["id"].as_str()).unwrap_or("");
    let (edges, callers, implementations, invariants): (u64,u64,u64,u64) = conn.query_row(
        "WITH selected(hash) AS (SELECT n.node_hash FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE (?1!='' AND n.id=?1) OR (?1='' AND p.path=?2)) SELECT count(*),count(DISTINCT CASE WHEN e.kind='calls' AND e.dst_hash IN (SELECT hash FROM selected) THEN e.src_hash END),coalesce(sum(e.kind IN ('implements','inherits') AND e.dst_hash IN (SELECT hash FROM selected)),0),coalesce(sum(e.kind='governs' AND e.dst_hash IN (SELECT hash FROM selected)),0) FROM edges e WHERE e.src_hash IN (SELECT hash FROM selected) OR e.dst_hash IN (SELECT hash FROM selected)",
        params![id,path], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let page = QueryOptions::resolve(&adapter.response, Some(1), 0, None, None)?;
    let tests = acdd_index::db::symbols::tests_paged_with_path(
        conn,
        if id.is_empty() { path } else { id },
        None,
        "inbound",
        &page,
    )
    .ok()
    .filter(|v| v["discovery_method"] == "graph")
    .and_then(|v| v["nodes"]["total"].as_u64())
    .unwrap_or(0);
    let mut pulse = serde_json::Map::new();
    if edges > 0 {
        pulse.insert("edges".into(), json!(edges));
    }
    if callers > 0 {
        pulse.insert("callers".into(), json!(callers));
    }
    if tests > 0 {
        pulse.insert("covering_tests".into(), json!(tests));
    }
    if implementations > 0 {
        pulse.insert("implementations".into(), json!(implementations));
    }
    if invariants > 0 {
        pulse.insert("invariants".into(), json!(invariants));
    }
    Ok(Value::Object(pulse))
}

/// Produce at most three lines of copy-ready calls, only for confirmed signals.
pub fn render_read_hints(
    root: &Path,
    conn: Option<&Connection>,
    adapter: &Adapter,
    path: &str,
    value: &Value,
    node_ctx: Option<&ActiveNodeContext>,
    out_of_scope: bool,
) -> Result<String> {
    let mut calls: Vec<(&str, String)> = Vec::new();
    if let Some(conn) = conn {
        if adapter.modules.is_enabled("codemap") {
            let selector = value["node"]["id"].as_str().unwrap_or(path);
            let signals = &value["graph_pulse"];
            if signals["edges"].as_u64().unwrap_or(0) > 0 {
                calls.push((
                    "code_map_explain",
                    format!("code_map_explain(selector: {})", json!(selector)),
                ));
                calls.push((
                    "code_map_impact",
                    format!("code_map_impact(selector: {})", json!(selector)),
                ));
            }
            if signals["covering_tests"].as_u64().unwrap_or(0) > 0 {
                calls.push((
                    "code_map_tests",
                    format!("code_map_tests(selector: {})", json!(selector)),
                ));
            }
            if value["node"]["id"].is_string()
                && signals["implementations"].as_u64().unwrap_or(0) > 0
            {
                calls.push((
                    "code_map_query",
                    format!(
                        "code_map_query(operation: \"implementations\", selector: {})",
                        json!(selector)
                    ),
                ));
            }
        }
        if adapter.modules.is_enabled("docs") {
            let id = value["node"]["id"].as_str().unwrap_or("");
            let mut stmt = conn.prepare("SELECT DISTINCT d.path,d.section_title FROM doc_sections d LEFT JOIN nodes dn ON dn.id=d.doc_id LEFT JOIN edges e ON e.src_hash=dn.node_hash AND e.kind IN ('documents','governs') LEFT JOIN nodes target ON target.node_hash=e.dst_hash LEFT JOIN path_dictionary p ON p.path_id=target.path_id WHERE d.path=?1 OR (?2!='' AND target.id=?2) OR (?2='' AND p.path=?1) ORDER BY d.path,d.section_title LIMIT 1")?;
            let section: Option<(String, String)> = stmt
                .query_map(params![path, id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .next()
                .transpose()?;
            if let Some((doc_path, section)) = section {
                calls.push((
                    "get_doc",
                    format!(
                        "get_doc(path: {}, section: {})",
                        json!(doc_path),
                        json!(section)
                    ),
                ));
                calls.push((
                    "search_docs",
                    format!(
                        "search_docs(query: {}, path: {})",
                        json!(section),
                        json!(doc_path)
                    ),
                ));
            }
        }
    }
    if let Some(ctx) = node_ctx {
        if out_of_scope {
            calls.push((
                "acdd_state",
                format!(
                    "acdd_state(action: \"extend_scope\", run: {}, node: {}, paths: [{}])",
                    json!(ctx.run_id.as_str()),
                    json!(ctx.node_cursor),
                    json!(path)
                ),
            ));
        }
        if ctx.has_command {
            calls.push((
                "acdd_verify",
                format!(
                    "acdd_verify(run: {}, node: {})",
                    json!(ctx.run_id.as_str()),
                    json!(ctx.node_cursor)
                ),
            ));
        }
        calls.push((
            "acdd_blackboard",
            format!(
                "acdd_blackboard(action: \"read\", run: {}, node: {})",
                json!(ctx.run_id.as_str()),
                json!(ctx.node_cursor)
            ),
        ));
    }
    let mut groups = [Vec::new(), Vec::new(), Vec::new()];
    for (tool, call) in calls
        .into_iter()
        .filter(|(tool, _)| !filter_subsystem_tools([*tool], &adapter.modules).is_empty())
    {
        let group = if tool.starts_with("code_map_") {
            0
        } else if matches!(tool, "get_doc" | "search_docs") {
            1
        } else {
            2
        };
        let call = format!(
            "{}, worktree: {})",
            call.trim_end_matches(')'),
            json!(root.to_string_lossy())
        );
        groups[group].push(call);
    }
    Ok(groups
        .into_iter()
        .filter(|group| !group.is_empty())
        .map(|group| group.join("; "))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::sensitive_with_case;
    use acdd_core::config::Adapter;

    #[test]
    fn sensitive_policy_honors_case_insensitive_volume_flag() {
        let adapter = Adapter {
            sensitive: vec![".env*".into(), "secrets/**".into()],
            ..Default::default()
        };

        assert!(sensitive_with_case(&adapter, ".env.local", false).is_err());
        assert!(sensitive_with_case(&adapter, ".ENV.local", false).is_ok());
        assert!(sensitive_with_case(&adapter, ".ENV.local", true).is_err());
        assert!(sensitive_with_case(&adapter, "SECRETS/token.txt", true).is_err());
        assert!(sensitive_with_case(&adapter, "src/lib.rs", true).is_ok());
    }
}
