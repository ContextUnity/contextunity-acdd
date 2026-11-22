use super::super::{response, server::Server};
use super::inputs::{
    Analyze, Ast, Explain, GetDoc, Guide, Impact, Inspect, Lint, OverviewInput, Query, SearchDocs,
    SearchSymbols, Selector, Snippet, Tests,
};
use crate::{
    engine::{ast, guide},
    format::{
        attach_document_presentation_uris, attach_lint_presentation_uris,
        attach_presentation_uris, attach_traversal_presentation_uris, DiscoveryKind, OperationKind,
        OutputFormat,
    },
    state_bridge,
};
use acdd_core::response::{CoverageOptions, Detail, ResponsePolicy};
use acdd_core::subsystems::SubsystemId;
use acdd_index::{
    db::{reader, symbols, traversal},
    scanner,
};
use rmcp::model::CallToolResult;
use serde_json::Value;

pub(super) fn looks_like_sql(value: &str) -> bool {
    value.split_whitespace().next().is_some_and(|keyword| {
        keyword.eq_ignore_ascii_case("select") || keyword.eq_ignore_ascii_case("with")
    })
}

async fn responding_blocking(
    server: &Server,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &'static str,
    explain: Option<bool>,
    discovery: Option<DiscoveryKind>,
    run: impl FnOnce(&Server, &scanner::Adapter, &ResponsePolicy) -> anyhow::Result<Value>
        + Send
        + 'static,
) -> CallToolResult {
    let server = server.clone();
    match tokio::task::spawn_blocking(move || {
        responding_with_presentation(
            &server,
            output_format,
            operation,
            title,
            |adapter, policy| run(&server, adapter, policy),
            explain,
            discovery,
        )
    })
    .await
    {
        Ok(res) => res,
        Err(err) => response::result_for(
            Err(anyhow::anyhow!("MCP blocking task failed: {err}")),
            &ResponsePolicy::default(),
            output_format,
            operation,
            title,
            None,
        ),
    }
}

fn responding_with_presentation(
    server: &Server,
    output_format: OutputFormat,
    operation: OperationKind,
    title: &'static str,
    run: impl FnOnce(&scanner::Adapter, &ResponsePolicy) -> anyhow::Result<Value>,
    explain: Option<bool>,
    discovery: Option<DiscoveryKind>,
) -> CallToolResult {
    match server.adapter_snapshot() {
        Ok(adapter) => {
            let mut value = run(&adapter, &adapter.response);
            if output_format == OutputFormat::Markdown {
                if let (Some(kind), Ok(value)) = (discovery, &mut value) {
                    attach_presentation_uris(&server.root, &adapter, value, kind);
                } else if matches!(title, "search_docs" | "get_doc") {
                    if let Ok(value) = &mut value {
                        attach_document_presentation_uris(
                            &server.root,
                            &adapter,
                            value,
                            title,
                        );
                    }
                } else if title == "code_map_lint" {
                    if let Ok(value) = &mut value {
                        attach_lint_presentation_uris(&server.root, &adapter, value);
                    }
                }
                if operation == OperationKind::Navigation
                    && discovery.is_none()
                    && matches!(
                        title,
                        "code_map_impact"
                            | "code_map_explain"
                            | "code_map_tests"
                            | "code_map_analyze"
                            | "code_map_query"
                    )
                {
                    if let Ok(value) = &mut value {
                        attach_traversal_presentation_uris(
                            &server.root,
                            &adapter,
                            value,
                        );
                    }
                }
            }
            response::result_for_presented(
                value,
                &adapter.response,
                output_format,
                operation,
                title,
                explain,
                discovery,
            )
        }
        Err(error) => response::result_for_presented(
            Err(error.into()),
            &ResponsePolicy::default(),
            output_format,
            operation,
            title,
            explain,
            discovery,
        ),
    }
}

pub(super) async fn code_map_overview(server: &Server, p: OverviewInput) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_overview",
        None,
        Some(DiscoveryKind::Overview),
        move |server, adapter, policy| {
            let page = p.page.resolve(policy)?;
            let mut result = server.read_with_adapter(adapter, |c| {
                state_bridge::overview(c, &server.root, Some(adapter), p.aspects.as_deref(), &page)
            })?;
            if format == OutputFormat::Markdown {
                result["request_scope"] = serde_json::json!({
                    "workspace_root": server.root.display().to_string(),
                    "aspects": p.aspects,
                });
            }
            Ok(result)
        },
    )
    .await
}

pub(super) async fn code_map_inspect(server: &Server, p: Inspect) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_inspect",
        Some(false),
        Some(DiscoveryKind::Inspect),
        move |server, adapter, policy| {
            let selector = p.selector()?;
            let path = p.path()?;
            let page = p.page.resolve(policy)?;
            let source = p.source(policy, false)?;
            let coverage = CoverageOptions {
                include_coverage: p.include_coverage,
            };
            let mut result = server.read_with_adapter(adapter, |c| {
                let mut result = symbols::inspect_with_path_options(
                    c,
                    &server.root,
                    selector,
                    path,
                    &symbols::InspectOptions {
                        show_doc: p.show_doc,
                        source: &source,
                        coverage,
                        page: &page,
                    },
                )?;
                if format == OutputFormat::Markdown && p.show_doc && page.detail == Detail::Full {
                    symbols::attach_inspect_document_contents(c, &mut result)?;
                }
                Ok(result)
            })?;
            if format == OutputFormat::Markdown {
                result["request_scope"] = serde_json::json!({
                    "workspace_root": server.root.display().to_string(),
                    "selector": selector,
                    "path": path,
                });
            }
            Ok(result)
        },
    )
    .await
}

pub(super) async fn acdd_read(server: &Server, mut p: Snippet) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "acdd_read",
        None,
        None,
        move |server, adapter, policy| {
            p.normalize(&server.root)?;
            let page = p.page.resolve(policy)?;
            let source = p.source(policy)?;
            let node_ctx = crate::state_bridge::active_node_context(
                &server.root,
                adapter,
                server.state_db(),
                Some(&server.db),
            )
            .ok()
            .flatten();
            let read = |conn: Option<&rusqlite::Connection>| {
                crate::engine::reader::acdd_read_tool(
                    &server.root,
                    adapter,
                    &p.read,
                    &page,
                    &source,
                    conn,
                    node_ctx.as_ref(),
                )
            };
            let structural = p.read.requires_index(adapter)?;
            if structural {
                server.read_with_adapter(adapter, |conn| read(Some(conn)))
            } else if server.db.is_file()
                && (adapter.modules.is_subsystem_enabled(SubsystemId::Code)
                    || adapter.modules.is_subsystem_enabled(SubsystemId::Docs))
            {
                server
                    .read_with_adapter(adapter, |conn| read(Some(conn)))
                    .or_else(|_| read(None))
            } else {
                read(None)
            }
        },
    )
    .await
}

pub(super) async fn code_map_search(server: &Server, p: SearchSymbols) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_search",
        None,
        Some(DiscoveryKind::Search),
        move |server, adapter, policy| {
            server.read_with_adapter(adapter, |c| {
                let default_detail =
                    (p.page.limit.unwrap_or(policy.page_size) > 10).then_some(Detail::Compact);
                let page = p.page.resolve_with_default_detail(policy, default_detail)?;
                let res = symbols::search_with_options(
                    c,
                    p.pattern()?,
                    &symbols::SearchOptions {
                        kind: p.kind.as_deref(),
                        path: p.path.as_deref().filter(|path| !path.is_empty()),
                        include_docs: p.include_docs,
                        exact: p.exact,
                        page: &page,
                    },
                )?;
                let mut res = symbols::search_results_with_navigation(
                    c,
                    &server.root,
                    res,
                    p.include_preview,
                )?;
                if p.group_by_file {
                    if let Some(items) = res
                        .get_mut("nodes")
                        .and_then(|nodes| nodes.get_mut("items"))
                        .and_then(Value::as_array_mut)
                    {
                        let mut grouped: std::collections::BTreeMap<String, Vec<Value>> =
                            std::collections::BTreeMap::new();
                        for mut item in items.drain(..) {
                            let path = item["path"].as_str().unwrap_or("").to_string();
                            if let Some(object) = item.as_object_mut() {
                                object.remove("path");
                            }
                            grouped.entry(path).or_default().push(item);
                        }
                        res["nodes"]["grouped_by_file"] =
                            serde_json::to_value(grouped).unwrap_or_default();
                    }
                }
                if format == OutputFormat::Markdown {
                    res["request_scope"] = serde_json::json!({
                        "workspace_root": server.root.display().to_string(),
                        "pattern": p.pattern,
                        "kind": p.kind,
                        "path": p.path,
                        "exact": p.exact,
                        "include_docs": p.include_docs,
                    });
                }
                Ok(res)
            })
        },
    )
    .await
}

pub(super) async fn code_map_tests(server: &Server, p: Tests) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_tests",
        None,
        None,
        move |server, adapter, policy| {
            server.read_with_adapter(adapter, |c| {
                symbols::tests_paged_with_path(
                    c,
                    &p.selector,
                    p.path.as_deref(),
                    &p.direction,
                    &p.page.resolve(policy)?,
                )
            })
        },
    )
    .await
}

pub(super) async fn code_map_impact(server: &Server, p: Impact) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_impact",
        None,
        None,
        move |server, adapter, policy| {
            let page = p.page.resolve(policy)?;
            server.read_with_adapter(adapter, |c| {
                traversal::impact_with_path_options(
                    c,
                    &server.root,
                    p.selector.as_deref(),
                    p.changes.as_deref(),
                    p.path.as_deref(),
                    &traversal::TraversalOptions {
                        depth: p.depth,
                        inbound: traversal::impact_inbound(Some(p.direction.as_str()))?,
                        mode: p.mode.as_deref(),
                        edge_types: p.edge_types.as_deref(),
                        page: &page,
                    },
                )
            })
        },
    )
    .await
}

pub(super) async fn code_map_explain(server: &Server, p: Explain) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_explain",
        Some(true),
        None,
        move |server, adapter, policy| {
            let page = p.page.resolve(policy)?;
            let source = p.source(policy, false)?;
            let coverage = CoverageOptions {
                include_coverage: p.include_coverage,
            };
            server.read_with_adapter(adapter, |c| {
                let direction = reader::ExplainDirection::parse_optional(p.direction.as_deref())?;
                symbols::explain_with_path_options(
                    c,
                    &server.root,
                    &p.selector,
                    p.path.as_deref(),
                    &symbols::ExplainOptions {
                        explain: reader::ExplainOptions {
                            direction,
                            show_doc: p.show_doc,
                            page: &page,
                        },
                        source: &source,
                        coverage,
                    },
                )
            })
        },
    )
    .await
}

pub(super) async fn code_map_query(server: &Server, p: Query) -> CallToolResult {
    let format = p.page.output_format();
    let implicit_sql = p.sql.is_some()
        || looks_like_sql(&p.operation)
        || (p.operation.trim().is_empty() && p.query.as_deref().is_some_and(looks_like_sql));
    let operation = if implicit_sql || p.operation.trim() == "sql" {
        OperationKind::Sql
    } else {
        OperationKind::Navigation
    };
    let is_inspect = !implicit_sql && p.operation.trim() == "inspect";
    let discovery = is_inspect.then_some(DiscoveryKind::Inspect);
    responding_blocking(
        server,
        format,
        operation,
        "code_map_query",
        None,
        discovery,
        move |server, adapter, policy| {
            let (effective_op, effective_selector) = if implicit_sql {
                (
                    "sql",
                    p.sql
                        .as_deref()
                        .or(p.query.as_deref())
                        .or(p.selector.as_deref())
                        .or_else(|| looks_like_sql(&p.operation).then_some(p.operation.as_str())),
                )
            } else {
                (
                    p.operation.as_str(),
                    p.target
                        .as_deref()
                        .or(p.selector.as_deref())
                        .or(p.query.as_deref()),
                )
            };
            server.read_with_adapter(adapter, |c| {
                let page = p.page.resolve(policy)?;
                if p.target.is_some()
                    && effective_op.trim() != "untested"
                    && effective_op.trim() != "duplicates"
                    && effective_op.trim() != "dead_tails"
                {
                    anyhow::bail!(
                        "target is supported only for untested, duplicates, and dead_tails"
                    );
                }
                if let Some(target) = p.target.as_deref() {
                    anyhow::ensure!(
                        p.selector.as_deref().is_none_or(|s| s == target)
                            && p.query.as_deref().is_none_or(|s| s == target)
                            && p.path.as_deref().is_none_or(|s| s == target),
                        "target conflicts with selector, query, or path"
                    );
                }
                let mut result = if effective_op.trim() == "duplicates" {
                    traversal::duplicates_paged(
                        c,
                        effective_selector,
                        p.path.as_deref(),
                        p.level.as_deref(),
                        p.min_lines,
                        &page,
                    )?
                } else {
                    traversal::query_with_path_options(
                        c,
                        effective_op,
                        effective_selector,
                        p.path.as_deref(),
                        &traversal::GraphQueryOptions {
                            depth: p.depth,
                            direction: p.direction.as_deref(),
                            coverage: CoverageOptions {
                                include_coverage: p.include_coverage,
                            },
                            page: &page,
                        },
                    )?
                };
                if is_inspect && format == OutputFormat::Markdown && page.detail == Detail::Full {
                    symbols::attach_inspect_document_contents(c, &mut result)?;
                }
                Ok(result)
            })
        },
    )
    .await
}

pub(super) async fn code_map_analyze(server: &Server, p: Analyze) -> CallToolResult {
    let format = p.page.output_format();
    let operation = if p.target().as_deref().is_ok_and(looks_like_sql) {
        OperationKind::Sql
    } else {
        OperationKind::Navigation
    };
    responding_blocking(
        server,
        format,
        operation,
        "code_map_analyze",
        None,
        None,
        move |server, adapter, policy| {
            let target = p.target()?;
            anyhow::ensure!(
                !p.lint || p.include_cycles != Some(true),
                "lint=true cannot be combined with include_cycles=true"
            );
            server.read_with_adapter(adapter, |c| {
                let options = p.page.resolve(policy)?;
                if p.lint {
                    acdd_index::db::lint::syntax_paged(c, target, &options)
                } else {
                    reader::analyze_paged(c, target, p.include_cycles, &options)
                }
            })
        },
    )
    .await
}

pub(super) async fn code_map_prove_removal(server: &Server, p: Selector) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_prove_removal",
        None,
        None,
        move |server, adapter, policy| {
            server.read_with_adapter(adapter, |c| {
                traversal::removal_paged_with_path(
                    c,
                    &p.selector,
                    p.path.as_deref(),
                    &p.page.resolve(policy)?,
                )
            })
        },
    )
    .await
}

pub(super) async fn code_map_lint(server: &Server, p: Lint) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "code_map_lint",
        None,
        None,
        move |server, adapter, policy| {
            let page = p.page.resolve(policy)?;
            server.read_with_adapter(adapter, |c| {
                let rule_ids: Option<Vec<String>> = p.rules.as_ref().map(|list| {
                    list.iter()
                        .map(|r| r.trim().to_owned())
                        .filter(|r| !r.is_empty())
                        .collect()
                });
                let req = acdd_index::db::lint::LintRequest {
                    target: p
                        .target
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty()),
                    paths: None,
                    categories: p.categories.as_deref(),
                    rule_ids: rule_ids.as_deref().filter(|r| !r.is_empty()),
                    minimum_severity: p.severity,
                    architecture: Some(&adapter.architecture),
                    workspace_root: Some(&server.root),
                    page: &page,
                };
                acdd_index::db::lint::run_lint(c, &req)
            })
        },
    )
    .await
}

pub(super) async fn ast_grep_search(server: &Server, p: Ast) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "ast_grep_search",
        None,
        Some(DiscoveryKind::Ast),
        move |server, adapter, policy| {
            server.read_with_adapter(adapter, |conn| {
                let pattern = p.pattern()?;
                let language = ast::resolve_ast_language(
                    conn,
                    &server.root,
                    adapter,
                    p.language.as_deref(),
                    p.path.as_deref(),
                )?;
                let mut result = ast::search_paged_with_adapter(
                    conn,
                    &server.root,
                    adapter,
                    pattern,
                    &language,
                    p.path.as_deref(),
                    &p.page.resolve(policy)?,
                )?;
                result["pattern"] = serde_json::json!(pattern);
                result["language"] = serde_json::json!(language);
                if format == OutputFormat::Markdown {
                    result["request_scope"] = serde_json::json!({
                        "workspace_root": server.root.display().to_string(),
                        "pattern": pattern,
                        "language": language,
                        "path": p.path,
                    });
                }
                Ok(result)
            })
        },
    )
    .await
}

pub(super) async fn search_docs(server: &Server, p: SearchDocs) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "search_docs",
        None,
        None,
        move |server, adapter, policy| {
            server.read_with_adapter(adapter, |c| {
                reader::search_docs_with_options(
                    c,
                    p.query()?,
                    &reader::DocSearchOptions {
                        doc_type: p.doc_type.as_deref(),
                        component: p.component()?,
                        include_excerpt: p.include_excerpt,
                        page: &p.page.resolve(policy)?,
                    },
                )
            })
        },
    )
    .await
}

pub(super) async fn get_doc(server: &Server, p: GetDoc) -> CallToolResult {
    let format = p.page.output_format();
    responding_blocking(
        server,
        format,
        OperationKind::Navigation,
        "get_doc",
        None,
        None,
        move |server, adapter, policy| {
            let mut page = p.page.resolve(policy)?;
            if p.page.detail.is_none() {
                page.detail = Detail::Full;
            }
            server.read_with_adapter(adapter, |c| {
                reader::get_doc_paged(c, p.path_or_id()?, p.section.as_deref(), &page)
            })
        },
    )
    .await
}

pub(super) async fn acdd_guide(server: &Server, p: Guide) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    let topic = p
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("query")
        .to_owned();
    if topic == "init" {
        let root = server.root.clone();
        let force = p.force;
        return match tokio::task::spawn_blocking(move || guide::run(&root, &topic, force)).await
        {
            Ok(result) => response::result_for(
                result,
                &ResponsePolicy::default(),
                format,
                OperationKind::Guide,
                "acdd_guide",
                None,
            ),
            Err(err) => response::result_for(
                Err(anyhow::anyhow!("MCP guide blocking task failed: {err}")),
                &ResponsePolicy::default(),
                format,
                OperationKind::Guide,
                "acdd_guide",
                None,
            ),
        };
    }
    responding_blocking(
        server,
        format,
        OperationKind::Guide,
        "acdd_guide",
        None,
        None,
        move |server, adapter, _| {
            guide::run_with_adapter(&server.root, &topic, p.force, adapter)
        },
    )
    .await
}
