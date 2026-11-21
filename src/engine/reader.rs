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

pub(crate) fn resolve_worktree_active_run_node(
    state_db: &Path,
    worktree: &Path,
    predicate: &dyn Fn(&acdd_state::types::GateNodeRecord) -> bool,
) -> std::result::Result<
    Option<(
        acdd_state::types::RunRecord,
        acdd_state::types::RunGraphSnapshot,
        acdd_state::types::GateNodeRecord,
    )>,
    acdd_core::errors::StateError,
> {
    if !std::fs::symlink_metadata(state_db).is_ok_and(|m| m.file_type().is_file()) {
        return Ok(None);
    }
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db.to_path_buf());
    let wt_canon = acdd_core::fs::canonicalize_macos_var(worktree);
    let wt_canon_str = wt_canon.to_string_lossy();
    let wt_raw_str = worktree.to_string_lossy();
    let run = match acdd_state::store::traits::StateReader::resolve_active_run_by_worktree(
        &reader,
        &wt_canon_str,
    )? {
        Some(r) => Some(r),
        None if wt_raw_str != wt_canon_str => {
            acdd_state::store::traits::StateReader::resolve_active_run_by_worktree(
                &reader,
                &wt_raw_str,
            )?
        }
        None => None,
    };
    let Some(run) = run else {
        return Ok(None);
    };
    let Some(graph) =
        acdd_state::store::traits::StateReader::load_run_graph(&reader, &run.run_id)?
    else {
        return Ok(None);
    };
    let matches_wt = |candidate: Option<&str>| {
        candidate.is_some_and(|w| {
            w == wt_canon_str.as_ref()
                || w == wt_raw_str.as_ref()
                || acdd_core::fs::canonicalize_macos_var(Path::new(w)) == wt_canon
        })
    };
    let active_node = graph
        .nodes
        .iter()
        .find(|n| predicate(n) && matches_wt(n.worktree.as_deref()))
        .or_else(|| {
            if matches_wt(run.worktree.as_deref()) {
                let mut candidates = graph.nodes.iter().filter(|n| predicate(n));
                let first = candidates.next();
                if candidates.next().is_none() {
                    first
                } else {
                    None
                }
            } else {
                None
            }
        })
        .cloned();
    Ok(active_node.map(|node| (run, graph, node)))
}

fn active_node_context(
    root: &Path,
    adapter: &Adapter,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
) -> Result<Option<ActiveNodeContext>> {
    if !adapter.modules.is_enabled("state") {
        return Ok(None);
    }
    let primary = acdd_core::git::primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    let state_db = acdd_core::config::resolve_effective_state_db_path(&primary, state_db, adapter);
    let Some((run, graph, node)) = resolve_worktree_active_run_node(&state_db, root, &|n| {
        n.status == acdd_state::types::NodeStatus::InProgress
            && matches!(
                n.kind,
                acdd_state::types::NodeKind::RootGate | acdd_state::types::NodeKind::TaskGate
            )
    })?
    else {
        return Ok(None);
    };
    let node_cursor = acdd_core::ids::NodeCursor::from_node_id(&node.node_id)?.to_string();
    let admitted_scope = acdd_state::machine::container_scope(&graph, &node);
    let profile =
        serde_json::from_str::<acdd_state::profile::ResolvedGateProfile>(&run.profile_json).ok();
    let protected_paths = profile
        .as_ref()
        .map(|p| p.protected_paths.clone())
        .unwrap_or_default();
    let has_command = profile
        .as_ref()
        .and_then(|profile| {
            let cursor = acdd_core::ids::NodeCursor::from_node_id(&node.node_id).ok()?;
            let gate_def = find_gate_in_profile(profile, &cursor)?;
            Some(
                gate_def
                    .command
                    .as_ref()
                    .is_some_and(|cmd| profile.commands.contains_key(cmd)),
            )
        })
        .unwrap_or(false);

    let default_state_db = primary.join(".acdd/state.sqlite");
    let state_db_override = (state_db != default_state_db).then(|| state_db.clone());
    let code_db_path = (adapter.modules.is_enabled("code") || adapter.modules.is_enabled("docs"))
        .then(|| acdd_core::config::resolve_code_db_path(&primary, code_db));
    let engine_cfg = acdd_state::usecases::StateEngineConfig {
        workspace_root: primary,
        worktree: root.to_path_buf(),
        state_db_override,
        code_db_path,
        spec_collections: adapter.specs.clone(),
        profiles_dirs: adapter.profiles_dirs.clone(),
        linked_workspaces: adapter
            .linked_workspaces
            .iter()
            .map(|lw| (lw.name.clone(), lw.path.clone()))
            .collect(),
        modules: adapter.modules.clone(),
        response_policy: adapter.response.clone(),
        retain_traces: adapter.retain_traces,
    };
    let reader = acdd_state::store::sqlite::SqliteStateReader::new(state_db);
    let path_policy = acdd_state::usecases::lifecycle::build_path_policy(
        &engine_cfg,
        &reader,
        &[],
        Some(&run),
        Some(&engine_cfg.worktree),
    )
    .unwrap_or_default();

    Ok(Some(ActiveNodeContext {
        run_id: run.run_id,
        node_cursor,
        node,
        admitted_scope,
        protected_paths,
        path_policy,
        has_command,
    }))
}

fn find_gate_in_profile<'a>(
    profile: &'a acdd_state::profile::ResolvedGateProfile,
    cursor: &acdd_core::ids::NodeCursor,
) -> Option<&'a acdd_state::profile::GateDef> {
    match cursor {
        acdd_core::ids::NodeCursor::Root(gates) => {
            let mut list = &profile.gates;
            for (idx, g) in gates.iter().enumerate() {
                let found = list.iter().find(|def| def.id == g.as_str())?;
                if idx + 1 == gates.len() {
                    return Some(found);
                }
                list = &found.gates;
            }
            None
        }
        acdd_core::ids::NodeCursor::Task { gates, .. } => {
            let first = gates.first()?;
            for item in &profile.gates {
                if let Some(ref tasks_block) = item.tasks {
                    if let Some(found) = tasks_block.gates.iter().find(|def| def.id == first.as_str()) {
                        return Some(found);
                    }
                }
            }
            None
        }
    }
}

/// Active `in_progress` node context resolved read-only from `state.sqlite` for `acdd_read`.
pub struct ActiveNodeContext {
    /// Active run identifier.
    pub run_id: acdd_core::ids::RunId,
    /// Active node cursor (`<gate>` or `<task_ref>.<gate>`).
    pub node_cursor: String,
    /// Active `GateNodeRecord`.
    pub node: acdd_state::types::GateNodeRecord,
    /// Effective admitted write scope for the node's container.
    pub admitted_scope: Vec<String>,
    /// Declarative protected path globs from the resolved gate profile.
    pub protected_paths: Vec<String>,
    /// Canonical workspace path policy incorporating engine-managed specs and DB paths.
    pub path_policy: acdd_core::path_policy::WorkspacePathPolicy,
    /// Whether the active gate configures a registered command.
    pub has_command: bool,
}

/// Read a file or symbol using already admitted graph state when available,
/// honoring optional explicit `--state-db` and `--db` overrides for active-node context.
#[allow(clippy::too_many_arguments)]
pub fn acdd_read_tool(
    root: &Path,
    adapter: &Adapter,
    request: &ReadRequest,
    page: &QueryOptions,
    source: &SourceOptions,
    conn: Option<&Connection>,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
) -> Result<Value> {
    if request.resolved_mode() == ReadMode::Lines {
        line_coordinates(request.start_line.unwrap_or(1), request.line_count)?;
    }
    let mut normalized_req;
    let request = if let Some(stripped) = request
        .path
        .as_deref()
        .and_then(|p| strip_root_path(p, root))
    {
        normalized_req = request.clone();
        normalized_req.path = Some(stripped);
        &normalized_req
    } else {
        request
    };
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
                    state_db,
                    code_db,
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
    acdd_read_single(
        root, adapter, request, page, source, conn, state_db, code_db,
    )
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

fn exportable_content(value: &Value) -> Option<&str> {
    value["source"].as_str().or(value["content"].as_str())
}

fn find_enclosing_symbol(
    conn: &Connection,
    path: &str,
    start_line: i64,
    end_line: Option<i64>,
) -> Result<Option<(String, String)>> {
    let clean_path = path.strip_prefix("./").unwrap_or(path);
    let check_end = end_line.unwrap_or(start_line);
    let mut stmt = conn.prepare(
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
    let mut rows = stmt.query(params![clean_path, start_line, check_end])?;
    if let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        return Ok(Some((kind, name)));
    }
    let mut stmt = conn.prepare(
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
           AND n.end_line >= ?2
           AND n.kind IN ('function', 'method', 'struct', 'class', 'trait', 'interface', 'enum')
         ORDER BY (p.path = ?1) DESC, (n.end_line - n.line) ASC, n.id
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![clean_path, start_line])?;
    if let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        Ok(Some((kind, name)))
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_arguments)]
fn acdd_read_single(
    root: &Path,
    adapter: &Adapter,
    request: &ReadRequest,
    page: &QueryOptions,
    source: &SourceOptions,
    conn: Option<&Connection>,
    state_db: Option<&Path>,
    code_db: Option<&Path>,
) -> Result<Value> {
    let mut normalized_req;
    let request = if let Some(stripped) = request
        .path
        .as_deref()
        .and_then(|p| strip_root_path(p, root))
    {
        normalized_req = request.clone();
        normalized_req.path = Some(stripped);
        &normalized_req
    } else {
        request
    };
    let mode = request.resolved_mode();
    let markdown = request
        .path
        .as_deref()
        .is_some_and(|p| p.ends_with(".md") || p.ends_with(".markdown"));
    request.requires_index(adapter)?;
    let node_ctx = active_node_context(root, adapter, state_db, code_db)
        .ok()
        .flatten();
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
                    compute_file_delta(root, &file, request.baseline.as_deref(), node_ctx.as_ref())?,
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
            if let Some(ctx) = &node_ctx {
                if let Some(ref baseline) = ctx.node.baseline_commit {
                    val["baseline_commit"] = json!(baseline.as_str());
                }
                if let Some(ref claim_tree) = ctx.node.claim_tree {
                    val["claim_tree"] = json!(claim_tree.as_str());
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
    if let Some(ctx) = &node_ctx {
        let root_unscoped = [String::from("**")];
        let scope = if ctx.admitted_scope.is_empty()
            && ctx.node.kind == acdd_state::types::NodeKind::RootGate
        {
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
            node_ctx.as_ref(),
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
            if ctx.node.isolation == acdd_state::types::IsolationMode::InPlace {
                ctx.node
                    .claim_tree
                    .as_ref()
                    .map(|t| t.as_str().to_owned())
                    .or_else(|| ctx.node.baseline_commit.as_ref().map(|c| c.as_str().to_owned()))
            } else {
                ctx.node
                    .baseline_commit
                    .as_ref()
                    .map(|c| c.as_str().to_owned())
                    .or_else(|| ctx.node.claim_tree.as_ref().map(|t| t.as_str().to_owned()))
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
    use super::{acdd_read_tool, sensitive_with_case, ReadMode, ReadRequest};
    use acdd_core::config::Adapter;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::response::{QueryOptions, SourceOptions};
    use acdd_core::ids::WorkerId;
    use acdd_core::ports::CompositeDomainProvider;
    use acdd_state::usecases::{
        ClaimReq, StartReq, StateCommand, StateCommandOutput, StateEngine, StateEngineConfig,
    };
    use std::fs;
    use std::sync::Arc;

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

    #[test]
    fn read_with_state_module_resolves_active_node_scope_delta_and_hints() {
        let dir = ScopedTempDir::create("acdd-ut-reader-state").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let run_git = |args: &[&str]| {
            let out = acdd_core::git::git_command(&root, args).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".acdd")).unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  builder:\n    mode: subagent\ncommands:\n  build_check: \"cargo check --lib\"\ngates:\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n          command: build_check\n",
        )
        .unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"p\"\nversion = \"0.1.0\"\n").unwrap();
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 1 }\n").unwrap();
        fs::write(root.join("src/other.rs"), "pub fn b() -> i32 { 2 }\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);
        let base_commit = run_git(&["rev-parse", "HEAD"]);
        // Pre-claim in-place edit and pre-claim untracked file captured in `claim_tree`.
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 10 }\n").unwrap();
        fs::write(root.join("src/pre_claim_untracked.rs"), "pub fn pre_claim() {}\n").unwrap();

        let adapter = Adapter::default();
        let engine = StateEngine::open(
            StateEngineConfig {
                workspace_root: root.clone(),
                worktree: root.clone(),
                state_db_override: None,
                code_db_path: None,
                spec_collections: adapter.specs.clone(),
                profiles_dirs: adapter.profiles_dirs.clone(),
                linked_workspaces: std::collections::BTreeMap::new(),
                modules: adapter.modules.clone(),
                response_policy: adapter.response.clone(),
                retain_traces: adapter.retain_traces,
            },
            Arc::new(CompositeDomainProvider::new(vec![]).unwrap()),
        )
        .unwrap();
        let started = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                goal: Some("edit scoped".into()),
                run_name: None,
                profile: None,
                scope: vec!["src/scoped.rs".into()],
                target_base: None,
                worktree: false,
                in_place: true,
                worktree_path: None,
            }))
            .unwrap();
        let run_id = match started {
            StateCommandOutput::Start(resp) => resp.run.run_id.to_string(),
            other => panic!("unexpected start output: {other:?}"),
        };
        let claim_resp = engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(run_id.clone()),
                node: None,
                worker_id: WorkerId::new("w1").unwrap(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: Some(root.clone()),
            }))
            .unwrap();
        let claim_tree = match claim_resp {
            StateCommandOutput::Claim(c) => c.candidate_tree.to_string(),
            other => panic!("unexpected claim output: {other:?}"),
        };
        assert_ne!(claim_tree, run_git(&["rev-parse", "HEAD^{tree}"]));

        // Post-claim edit + untracked file.
        fs::write(root.join("src/scoped.rs"), "pub fn a() -> i32 { 42 }\n").unwrap();
        fs::write(root.join("src/untracked.rs"), "pub fn newly_added() {}\n").unwrap();
        fs::write(root.join("src/ leading.rs"), "pub fn spaced() {}\n").unwrap();
        fs::write(root.join("src/leading.rs"), "pub fn plain_name() {}\n").unwrap();

        let page = QueryOptions::resolve(&adapter.response, None, 0, None, None).unwrap();
        let source = SourceOptions::resolve(&adapter.response, None, None, None, 0).unwrap();
        let read = |req: &ReadRequest| {
            acdd_read_tool(&root, &adapter, req, &page, &source, None, None, None).unwrap()
        };
        let req_allowed = ReadRequest {
            path: Some("src/scoped.rs".into()),
            mode: ReadMode::Delta,
            include_hints: true,
            ..Default::default()
        };
        let allowed = read(&req_allowed);
        assert_eq!(allowed["in_write_scope"], "ALLOWED");
        assert_eq!(allowed["baseline_commit"], base_commit);
        assert_eq!(allowed["claim_tree"], claim_tree);
        let delta_text = allowed["content"].as_str().unwrap_or_default();
        assert!(delta_text.contains("-pub fn a() -> i32 { 10 }"), "{delta_text}");
        assert!(delta_text.contains("+pub fn a() -> i32 { 42 }"), "{delta_text}");
        let hints = allowed["Hints"].as_str().unwrap_or_default();
        assert!(hints.contains("acdd_blackboard"), "{hints}");

        let untracked_delta = read(&ReadRequest {
            path: Some("src/untracked.rs".into()),
            mode: ReadMode::Delta,
            ..Default::default()
        });
        let untracked_diff = untracked_delta["content"].as_str().unwrap_or_default();
        assert!(untracked_diff.contains("--- /dev/null"), "{untracked_diff}");
        assert!(untracked_diff.contains("+pub fn newly_added() {}"), "{untracked_diff}");
        for (rel, expected_fn, forbidden_fn) in [
            ("src/ leading.rs", "pub fn spaced()", "plain_name()"),
            ("src/leading.rs", "pub fn plain_name()", "spaced()"),
        ] {
            let delta = read(&ReadRequest {
                path: Some(rel.to_owned()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let diff = delta["content"].as_str().unwrap_or_default();
            assert!(diff.contains(expected_fn), "{rel}: {diff}");
            assert!(!diff.contains(forbidden_fn), "{rel}: {diff}");
        }

        // #12: Untracked file already identical in `claim_tree` must yield empty delta, NOT full `/dev/null` add.
        let pre_claim_untracked_delta = read(&ReadRequest {
            path: Some("src/pre_claim_untracked.rs".into()),
            mode: ReadMode::Delta,
            ..Default::default()
        });
        assert_eq!(
            pre_claim_untracked_delta["content"].as_str(),
            Some(""),
            "untracked file present in claim_tree must return empty delta: {pre_claim_untracked_delta}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let pre_abs = root.join("src/pre_claim_untracked.rs");
            let mut perms = fs::metadata(&pre_abs).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&pre_abs, perms).unwrap();
            let mode_delta = read(&ReadRequest {
                path: Some("src/pre_claim_untracked.rs".into()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let mode_diff = mode_delta["content"].as_str().unwrap_or_default();
            assert_eq!(mode_diff.matches("old mode 100644").count(), 1, "{mode_diff}");
            assert_eq!(mode_diff.matches("new mode 100755").count(), 1, "{mode_diff}");

            fs::write(&pre_abs, "pub fn pre_claim_untracked_v2() {}\n").unwrap();
            let mut perms2 = fs::metadata(&pre_abs).unwrap().permissions();
            perms2.set_mode(0o755);
            fs::set_permissions(&pre_abs, perms2).unwrap();
            let mode_content_delta = read(&ReadRequest {
                path: Some("src/pre_claim_untracked.rs".into()),
                mode: ReadMode::Delta,
                ..Default::default()
            });
            let mode_content_diff = mode_content_delta["content"].as_str().unwrap_or_default();
            assert_eq!(
                mode_content_diff.matches("old mode 100644").count(),
                1,
                "{mode_content_diff}"
            );
            assert_eq!(
                mode_content_diff.matches("new mode 100755").count(),
                1,
                "{mode_content_diff}"
            );
            assert!(mode_content_diff.contains("+pub fn pre_claim_untracked_v2()"), "{mode_content_diff}");
        }

        let protected_read = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(protected_read["in_write_scope"], "OUT_OF_SCOPE", "{protected_read}");

        let req_outside = ReadRequest {
            path: Some("src/other.rs".into()),
            mode: ReadMode::Lines,
            include_hints: true,
            ..Default::default()
        };
        let outside = read(&req_outside);
        assert_eq!(outside["in_write_scope"], "OUT_OF_SCOPE");
        let outside_hints = outside["Hints"].as_str().unwrap_or_default();
        assert!(outside_hints.contains("acdd_state"), "{outside_hints}");
        assert!(outside_hints.contains("acdd_blackboard"), "{outside_hints}");

        // Unscoped RootGate must also report OUT_OF_SCOPE for protected_paths (`Cargo.toml`).
        engine
            .execute(StateCommand::Clear(acdd_state::usecases::ClearReq {
                run: Some(run_id),
                reason: Some("reset for unscoped root gate check".into()),
                force: true,
            }))
            .unwrap();
        fs::write(
            root.join(".acdd/profile.yaml"),
            "version: 2\nprotected_paths:\n  - Cargo.toml\nroles:\n  architect:\n    mode: subagent\n  builder:\n    mode: subagent\ngates:\n  - id: verify\n    role: architect\n  - tasks:\n      gates:\n        - id: build\n          role: builder\n",
        )
        .unwrap();
        let root_start = engine
            .execute(StateCommand::Start(StartReq {
                spec_path: None,
                goal: Some("unscoped root check".into()),
                run_name: None,
                profile: None,
                scope: vec![],
                target_base: None,
                worktree: false,
                in_place: true,
                worktree_path: None,
            }))
            .unwrap();
        let root_run_id = match root_start {
            StateCommandOutput::Start(resp) => resp.run.run_id.to_string(),
            other => panic!("unexpected root start: {other:?}"),
        };
        engine
            .execute(StateCommand::Claim(ClaimReq {
                run: Some(root_run_id),
                node: Some("verify".into()),
                worker_id: WorkerId::new("w1").unwrap(),
                session_fingerprint: None,
                force: false,
                steal_remote: false,
                worktree: Some(root.clone()),
            }))
            .unwrap();
        let root_protected = read(&ReadRequest {
            path: Some("Cargo.toml".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(root_protected["in_write_scope"], "OUT_OF_SCOPE", "{root_protected}");
        let root_allowed = read(&ReadRequest {
            path: Some("src/other.rs".into()),
            mode: ReadMode::Lines,
            ..Default::default()
        });
        assert_eq!(root_allowed["in_write_scope"], "ALLOWED", "{root_allowed}");
    }
}
