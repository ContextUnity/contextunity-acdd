use super::args::{AstCommand, Command, DocsCommand, QueryCommand};
use super::paths;
use crate::{
    engine::{ast, pipeline as writer},
    format::OutputFormat,
};
use acdd_core::config::Adapter;
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::response::{
    CoverageOptions, Detail, QueryOptions, ResponsePolicy, SourceOptions, MAX_PAGE_SIZE,
};
use acdd_core::subsystems::SubsystemId;
use acdd_index::db::{lint as db_lint, reader, symbols, traversal};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(super) struct CliCodeCtx<'a> {
    pub root: &'a Path,
    pub db: &'a Path,
    pub state_db: Option<&'a Path>,
    pub adapter: &'a Adapter,
    pub explicit_config: Option<&'a Path>,
    pub has_explicit_config: bool,
    pub cache_dir: Option<&'a Path>,
    pub output_format: OutputFormat,
}

pub(super) fn looks_like_sql(value: &str) -> bool {
    value.split_whitespace().next().is_some_and(|keyword| {
        keyword.eq_ignore_ascii_case("select") || keyword.eq_ignore_ascii_case("with")
    })
}

pub(super) fn resolve_cli_adapter_path(
    root: &Path,
    adapter_arg: Option<&Path>,
    has_explicit_config: bool,
    resolved_config: Option<&Path>,
    fallback_to_resolved_config: bool,
) -> Result<Option<PathBuf>> {
    match adapter_arg {
        Some(adapter_arg) => {
            let confined = paths::confined_file(root, adapter_arg)?;
            if has_explicit_config {
                anyhow::ensure!(
                    resolved_config == Some(confined.as_path()),
                    "CLI_CONFIG_CONFLICT: --adapter and --config select different files"
                );
            }
            Ok(Some(confined))
        }
        None if fallback_to_resolved_config => Ok(resolved_config.map(Path::to_path_buf)),
        None => Ok(if has_explicit_config {
            resolved_config.map(Path::to_path_buf)
        } else {
            None
        }),
    }
}

fn parse_cli_lint_categories(categories: Option<Vec<String>>) -> Result<Option<Vec<LintCategory>>> {
    let Some(raw) = categories else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for item in raw {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "all" {
            out.extend_from_slice(LintCategory::ALL);
        } else {
            out.push(trimmed.parse::<LintCategory>()?);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn cli_page(limit: usize) -> Result<QueryOptions> {
    QueryOptions::resolve(&ResponsePolicy::default(), Some(limit), 0, None, None)
}

fn cli_page_with_offset(
    limit: usize,
    offset: usize,
    generation: Option<String>,
) -> Result<QueryOptions> {
    QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(limit),
        offset,
        None,
        generation,
    )
}

fn cli_source(enabled: bool) -> Result<SourceOptions> {
    SourceOptions::resolve(&ResponsePolicy::default(), Some(enabled), None, None, 0)
}

fn open_or_rebuild(
    db: &Path,
    root: &Path,
    explicit_config: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<reader::LockedConnection> {
    writer::validate_code_map_target(root, db)?;
    let rebuild = || match cache_dir {
        Some(cache_dir) => {
            crate::engine::pipeline::build_with_cache_dir(root, db, explicit_config, cache_dir)
        }
        None => writer::build(root, db, explicit_config),
    };
    match reader::open_locked(db, root) {
        Ok(conn) => {
            let selected = acdd_index::scanner::load_adapter(root, explicit_config)?;
            let persisted = reader::read_policy(root, &conn)?;
            if selected.digest != persisted.digest
                || selected.adapter_path != persisted.adapter_path
                || selected.linked_workspaces != persisted.linked_workspaces
            {
                drop(conn);
                rebuild()?;
                return reader::open_locked(db, root);
            }
            Ok(conn)
        }
        Err(err) => {
            let msg = err.to_string();
            let requires_rebuild = err.downcast_ref::<reader::IndexRebuildRequired>().is_some();
            if requires_rebuild || !db.exists() {
                eprintln!(
                    "ACDD index incompatible or missing ({msg}); auto-rebuilding for {}...",
                    root.display()
                );
                rebuild()?;
                reader::open_locked(db, root)
            } else {
                Err(err)
            }
        }
    }
}

fn require_subsystem(adapter: &Adapter, id: SubsystemId) -> Result<()> {
    if !adapter.modules.is_subsystem_enabled(id) {
        return Err(AcddError::new(
            ErrorCode::FeatureDisabled,
            format!(
                "FEATURE_DISABLED: '{}' module is disabled in modules:",
                id.as_str()
            ),
        )
        .into());
    }
    Ok(())
}

pub(super) fn parse_cli_detail(detail: Option<&str>) -> Result<Option<Detail>> {
    detail
        .map(|raw| match raw.trim() {
            "compact" => Ok(Detail::Compact),
            "full" => Ok(Detail::Full),
            other => Err(anyhow::anyhow!(
                "invalid --detail '{other}'; expected 'compact' or 'full'"
            )),
        })
        .transpose()
}

pub(super) fn dispatch(ctx: &CliCodeCtx<'_>, command: Command) -> Result<(Value, Option<String>)> {
    let val = match command {
        Command::Read {
            path,
            mode,
            selector,
            start_line,
            line_count,
            pattern,
            regex,
            context,
            baseline,
            output_file,
            include_hints,
            source_offset,
            generation,
            max_body_lines,
            leading_lines,
            detail,
        } => {
            let mode = serde_json::from_value::<acdd_core::model::ReadMode>(json!(mode))?;
            let request = crate::engine::reader::ReadRequest {
                path: Some(path),
                paths: None,
                mode,
                selector,
                start_line,
                line_count,
                pattern,
                regex,
                context,
                baseline,
                output_file,
                include_hints,
            };
            let source = SourceOptions::resolve_full_symbol(
                &ctx.adapter.response,
                leading_lines,
                max_body_lines,
                source_offset,
            )?;
            anyhow::ensure!(
                source.has_valid_paging_anchor(generation.as_deref()),
                "source continuation requires generation"
            );
            let parsed_detail = parse_cli_detail(detail.as_deref())?;
            let page =
                QueryOptions::resolve(&ctx.adapter.response, None, 0, parsed_detail, generation)?;
            let node_ctx = crate::state_bridge::active_node_context(
                ctx.root,
                ctx.adapter,
                ctx.state_db,
                Some(ctx.db),
            )
            .ok()
            .flatten();
            let run_read = |conn: Option<&rusqlite::Connection>| {
                crate::engine::reader::acdd_read_tool(
                    ctx.root,
                    ctx.adapter,
                    &request,
                    &page,
                    &source,
                    conn,
                    node_ctx.as_ref(),
                )
            };
            if request.requires_index(ctx.adapter)? {
                let conn = open_or_rebuild(ctx.db, ctx.root, ctx.explicit_config, ctx.cache_dir)?;
                run_read(Some(&conn))?
            } else if ctx.db.is_file()
                && (ctx.adapter.modules.is_subsystem_enabled(SubsystemId::Code)
                    || ctx.adapter.modules.is_subsystem_enabled(SubsystemId::Docs))
            {
                reader::open_locked(ctx.db, ctx.root)
                    .and_then(|conn| run_read(Some(&conn)))
                    .or_else(|_| run_read(None))?
            } else {
                run_read(None)?
            }
        }
        Command::Build {
            workspace_root: _,
            output,
            adapter: adapter_arg,
            verbose: _,
        } => {
            let adapter_path = resolve_cli_adapter_path(
                ctx.root,
                adapter_arg.as_deref(),
                ctx.has_explicit_config,
                ctx.explicit_config,
                true,
            )?;
            let output_db = output
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        ctx.root.join(path)
                    }
                })
                .unwrap_or_else(|| ctx.db.to_path_buf());
            writer::validate_code_map_target(ctx.root, &output_db)?;
            match ctx.cache_dir {
                Some(cache_dir) => crate::engine::pipeline::build_with_cache_dir(
                    ctx.root,
                    &output_db,
                    adapter_path.as_deref(),
                    cache_dir,
                )?,
                None => writer::build(ctx.root, &output_db, adapter_path.as_deref())?,
            }
        }
        Command::Scan {
            workspace_root: _,
            adapter: adapter_arg,
            pretty: _,
        } => {
            let adapter_path = resolve_cli_adapter_path(
                ctx.root,
                adapter_arg.as_deref(),
                ctx.has_explicit_config,
                ctx.explicit_config,
                true,
            )?;
            let scan_adapter = if adapter_path.as_deref() == ctx.explicit_config {
                ctx.adapter.clone()
            } else {
                acdd_core::config::load_adapter(ctx.root, adapter_path.as_deref())?
            };
            json!(acdd_index::scanner::scan_with_adapter(
                ctx.root,
                &scan_adapter
            )?)
        }
        Command::Delta {
            workspace_root: _,
            mut modified_files,
            modified,
        } => {
            modified_files.extend(modified);
            if modified_files.is_empty() {
                bail!("delta requires at least one modified file");
            }
            let adapter_path = resolve_cli_adapter_path(
                ctx.root,
                None,
                ctx.has_explicit_config,
                ctx.explicit_config,
                false,
            )?;
            crate::engine::pipeline::delta_with_selection(
                ctx.root,
                ctx.db,
                &modified_files,
                adapter_path.as_deref(),
                ctx.cache_dir,
            )?
        }
        Command::Ast {
            command:
                AstCommand::Grep {
                    pattern,
                    language,
                    path,
                    limit,
                },
        } => {
            require_subsystem(ctx.adapter, SubsystemId::Code)?;
            let conn = open_or_rebuild(ctx.db, ctx.root, ctx.explicit_config, ctx.cache_dir)?;
            let languages = ast::resolve_ast_languages(
                &conn,
                ctx.root,
                ctx.adapter,
                &pattern,
                language.as_deref(),
                path.as_deref(),
            )?;
            let lang_refs: Vec<&str> = languages.iter().map(String::as_str).collect();
            let mut result = if (1..=MAX_PAGE_SIZE).contains(&limit) {
                ast::search_paged_languages_with_adapter(
                    &conn,
                    ctx.root,
                    ctx.adapter,
                    &pattern,
                    &lang_refs,
                    path.as_deref(),
                    &cli_page(limit)?,
                )?
            } else {
                ast::search_languages_with_adapter(
                    &conn,
                    ctx.root,
                    ctx.adapter,
                    &pattern,
                    &lang_refs,
                    path.as_deref(),
                    limit,
                )?
            };
            let language_label = languages.join(", ");
            result["pattern"] = json!(pattern);
            result["language"] = json!(language_label);
            if ctx.output_format == OutputFormat::Markdown {
                result["request_scope"] = json!({
                    "workspace_root": ctx.root.display().to_string(),
                    "pattern": pattern,
                    "language": language_label,
                    "path": path,
                });
            }
            result
        }
        Command::Query { command } => {
            require_subsystem(ctx.adapter, SubsystemId::Code)?;
            let conn = open_or_rebuild(ctx.db, ctx.root, ctx.explicit_config, ctx.cache_dir)?;
            dispatch_query(ctx, &conn, command)?
        }
        Command::Docs { command } => {
            require_subsystem(ctx.adapter, SubsystemId::Docs)?;
            let conn = open_or_rebuild(ctx.db, ctx.root, ctx.explicit_config, ctx.cache_dir)?;
            match command {
                DocsCommand::Check => unreachable!("handled prior to dispatch_code"),
                DocsCommand::Search {
                    query,
                    doc_type,
                    component,
                    limit,
                } => reader::search_docs_with_options(
                    &conn,
                    &query,
                    &reader::DocSearchOptions {
                        doc_type: doc_type.as_deref(),
                        component: component.as_deref(),
                        include_excerpt: false,
                        page: &cli_page(limit)?,
                    },
                )?,
                DocsCommand::Get {
                    path_or_id,
                    section,
                    include_candidates,
                } => {
                    let mut options = cli_page(100)?;
                    options.detail = Detail::Full;
                    reader::get_doc_paged(
                        &conn,
                        &path_or_id,
                        section.as_deref(),
                        include_candidates,
                        &options,
                    )?
                }
            }
        }
        Command::Lint {
            target,
            categories,
            rule,
            severity,
            limit,
            offset,
            detail,
            generation,
        } => {
            require_subsystem(ctx.adapter, SubsystemId::Code)?;
            let parsed_categories = parse_cli_lint_categories(categories)?;
            let minimum_severity = severity
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::parse::<LintSeverity>)
                .transpose()?;
            let parsed_detail = parse_cli_detail(detail.as_deref())?;
            let page = QueryOptions::resolve(
                &ctx.adapter.response,
                Some(limit),
                offset,
                parsed_detail,
                generation,
            )?;
            let rule_ids: Option<Vec<String>> = rule.map(|list| {
                list.into_iter()
                    .map(|r| r.trim().to_owned())
                    .filter(|r| !r.is_empty())
                    .collect()
            });
            crate::state_bridge::code_domain::ensure_fresh_worktree_index(
                ctx.root,
                ctx.db,
                ctx.adapter,
                ctx.explicit_config,
                ctx.cache_dir,
                None,
            )?;
            let conn = reader::open_locked(ctx.db, ctx.root)?;
            db_lint::run_lint(
                &conn,
                &db_lint::LintRequest {
                    target: target.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                    paths: None,
                    categories: parsed_categories.as_deref(),
                    rule_ids: rule_ids.as_deref().filter(|r| !r.is_empty()),
                    minimum_severity,
                    architecture: Some(&ctx.adapter.architecture),
                    workspace_root: Some(ctx.root),
                    page: &page,
                },
            )?
        }
        _ => unreachable!("non-code command routed to dispatch_code"),
    };
    Ok((val, None))
}

fn dispatch_query(
    ctx: &CliCodeCtx<'_>,
    conn: &reader::LockedConnection,
    command: QueryCommand,
) -> Result<Value> {
    match command {
        QueryCommand::Overview {
            aspects,
            limit,
            offset,
            generation,
        } => {
            let page = QueryOptions::resolve(
                &ResponsePolicy::default(),
                Some(limit),
                offset,
                None,
                generation,
            )?;
            crate::state_bridge::overview(
                conn,
                ctx.root,
                Some(ctx.adapter),
                aspects.as_deref(),
                None,
                &page,
            )
        }
        QueryCommand::Inspect {
            selector,
            path,
            show_doc,
            show_source,
            source_offset,
            generation,
        } => {
            let source = SourceOptions::resolve(
                &ResponsePolicy::default(),
                Some(show_source),
                None,
                None,
                source_offset,
            )?;
            anyhow::ensure!(
                source.has_valid_paging_anchor(generation.as_deref()),
                "source continuation requires restarting at source_offset=0 or providing the previous page's generation"
            );
            let page =
                QueryOptions::resolve(&ResponsePolicy::default(), Some(100), 0, None, generation)?;
            let mut result = symbols::inspect_with_path_options(
                conn,
                ctx.root,
                &selector,
                path.as_deref(),
                &symbols::InspectOptions {
                    show_doc,
                    source: &source,
                    coverage: CoverageOptions::default(),
                    page: &page,
                },
            )?;
            if ctx.output_format == OutputFormat::Markdown {
                result["request_scope"] = json!({
                    "workspace_root": ctx.root.display().to_string(),
                    "selector": selector,
                    "path": path,
                });
            }
            Ok(result)
        }
        QueryCommand::Search {
            pattern,
            kind,
            classification,
            limit,
        } => {
            let page = cli_page(limit)?;
            let exact = false;
            let include_docs = false;
            let parsed_classification = classification
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::parse::<acdd_core::response::SymbolClassification>)
                .transpose()?;
            let mut result = symbols::search_results_with_navigation(
                conn,
                ctx.root,
                &pattern,
                &symbols::SearchOptions {
                    kind: kind.as_deref(),
                    classification: parsed_classification,
                    path: None,
                    include_docs,
                    exact,
                    page: &page,
                },
                false,
                true,
            )?;
            if ctx.output_format == OutputFormat::Markdown {
                result["request_scope"] = json!({
                    "workspace_root": ctx.root.display().to_string(),
                    "pattern": pattern,
                    "kind": kind,
                    "classification": parsed_classification.map(acdd_core::response::SymbolClassification::as_str),
                    "exact": exact,
                    "include_docs": include_docs,
                });
            }
            Ok(result)
        }
        QueryCommand::Tests {
            selector,
            path,
            direction,
            limit,
        } => symbols::tests_paged_with_path(
            conn,
            &selector,
            path.as_deref(),
            &direction,
            &cli_page(limit)?,
        ),
        QueryCommand::Impact {
            selector,
            changes,
            path,
            depth,
            limit,
            offset,
            generation,
        } => traversal::impact_with_path_options(
            conn,
            ctx.root,
            selector.as_deref(),
            changes.as_deref(),
            path.as_deref(),
            &traversal::TraversalOptions {
                depth,
                direction: traversal::ImpactDirection::Inbound,
                mode: traversal::TraversalMode::Calls,
                edge_types: None,
                page: &QueryOptions::resolve(
                    &ResponsePolicy::default(),
                    Some(limit),
                    offset,
                    None,
                    generation,
                )?,
            },
        ),
        QueryCommand::Explain { selector, path } => symbols::explain_with_path_options(
            conn,
            ctx.root,
            &selector,
            path.as_deref(),
            &symbols::SymbolExplainOptions {
                explain: reader::ExplainOptions {
                    direction: reader::ExplainDirection::Both,
                    show_doc: true,
                    page: &cli_page(100)?,
                },
                source: &cli_source(false)?,
                coverage: CoverageOptions {
                    include_coverage: true,
                },
            },
        ),
        QueryCommand::Remove { selector, path } => {
            traversal::removal_paged_with_path(conn, &selector, path.as_deref(), &cli_page(100)?)
        }
        QueryCommand::Analyze {
            target,
            mode,
            to,
            base,
            include_cycles,
            lint,
            limit,
            offset,
            generation,
        } => {
            let page = cli_page_with_offset(limit, offset, generation)?;
            crate::state_bridge::dispatch_analyze(
                conn,
                ctx.root,
                ctx.adapter,
                ctx.state_db,
                Some(ctx.db),
                &crate::state_bridge::AnalyzeDispatchInput {
                    target: &target,
                    mode: mode.as_deref(),
                    to: to.as_deref(),
                    base: base.as_deref(),
                    include_cycles,
                    lint,
                    page: &page,
                },
            )
        }
        QueryCommand::Sql { sql, limit } => traversal::query_with_path_options(
            conn,
            "sql",
            Some(sql.as_str()),
            None,
            &traversal::GraphQueryOptions {
                depth: 2,
                direction: traversal::ImpactDirection::Inbound,
                coverage: CoverageOptions::default(),
                page: &cli_page(limit)?,
            },
        ),
        QueryCommand::Run {
            operation,
            selector,
            path,
            depth,
            limit,
            offset,
            generation,
            level,
            min_lines,
        } => {
            let (effective_op, effective_selector) =
                if selector.is_none() && looks_like_sql(&operation) {
                    ("sql", Some(operation.as_str()))
                } else {
                    (operation.as_str(), selector.as_deref())
                };
            let page = cli_page_with_offset(limit, offset, generation)?;
            if effective_op.trim() == "duplicates" {
                traversal::duplicates_paged(
                    conn,
                    effective_selector,
                    path.as_deref(),
                    level.as_deref(),
                    Some(min_lines),
                    &page,
                )
            } else {
                traversal::query_with_path_options(
                    conn,
                    effective_op,
                    effective_selector,
                    path.as_deref(),
                    &traversal::GraphQueryOptions {
                        depth,
                        direction: traversal::ImpactDirection::Inbound,
                        coverage: CoverageOptions::default(),
                        page: &page,
                    },
                )
            }
        }
    }
}
