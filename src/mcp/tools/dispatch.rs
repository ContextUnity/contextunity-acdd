use super::super::{
    response,
    server::{primary_workspace_root_for, RequestAdapter, Server},
};
use super::state_tools::operation_kind;
use crate::format::{OperationKind, OutputFormat};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::NodeCursor;
use acdd_core::ports::{DomainToolDef, DomainToolOutput, DomainToolReq};
use acdd_core::response::ResponsePolicy;
use acdd_core::schema::AdditionalPropertiesDef;
use acdd_core::subsystems::SubsystemId;
use acdd_index::scanner;
use rmcp::model::{CallToolResult, ContentBlock, Tool, ToolAnnotations};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn allowed_fields_for_tool(tool_name: &str) -> &'static [String] {
    static CACHE: std::sync::OnceLock<std::collections::HashMap<String, Vec<String>>> =
        std::sync::OnceLock::new();
    let map = CACHE.get_or_init(|| {
        Server::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| {
                let mut fields: Vec<String> = tool
                    .input_schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|props| {
                        props
                            .keys()
                            .filter(|k| k.as_str() != "format")
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                fields.sort();
                (tool.name.into_owned(), fields)
            })
            .collect()
    });
    map.get(tool_name).map_or(&[], Vec::as_slice)
}

fn rejected_selector_fields(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> Vec<String> {
    let Some(arguments) = arguments else {
        return Vec::new();
    };
    let allowed = allowed_fields_for_tool(tool_name);
    if allowed.is_empty() {
        return Vec::new();
    }
    arguments
        .keys()
        .filter(|field| field.as_str() != "format" && !allowed.iter().any(|a| a == field.as_str()))
        .take(16)
        .cloned()
        .collect()
}

fn rejected_plugin_fields(
    def: &DomainToolDef,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> Option<(Vec<String>, Vec<String>)> {
    if def.input_schema.schema_type != "object"
        || def.input_schema.additional_properties != Some(AdditionalPropertiesDef::Boolean(false))
    {
        return None;
    }
    let arguments = arguments?;
    let mut allowed: Vec<String> = def.input_schema.properties.keys().cloned().collect();
    if !allowed.iter().any(|k| k == "worktree") {
        allowed.push("worktree".to_owned());
    }
    if !allowed.iter().any(|k| k == "format") {
        allowed.push("format".to_owned());
    }
    let rejected: Vec<String> = arguments
        .keys()
        .filter(|field| !allowed.iter().any(|a| a == field.as_str()))
        .take(16)
        .cloned()
        .collect();
    (!rejected.is_empty()).then_some((rejected, allowed))
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut dp = vec![vec![0; b_bytes.len() + 1]; a_bytes.len() + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate() {
        *val = j;
    }
    for (i, &ca) in a_bytes.iter().enumerate() {
        for (j, &cb) in b_bytes.iter().enumerate() {
            if ca == cb {
                dp[i + 1][j + 1] = dp[i][j];
            } else {
                dp[i + 1][j + 1] = 1 + dp[i][j].min(dp[i + 1][j]).min(dp[i][j + 1]);
            }
        }
    }
    dp[a_bytes.len()][b_bytes.len()]
}

fn find_suggestions<'a>(rejected: &[String], allowed: &[&'a str]) -> Vec<(String, &'a str)> {
    let mut suggestions = Vec::new();
    for rej in rejected {
        let rej_lower = rej.to_lowercase();
        let mut best: Option<(&'a str, usize)> = None;
        for &cand in allowed {
            let cand_lower = cand.to_lowercase();
            if cand_lower.contains(&rej_lower) || rej_lower.contains(&cand_lower) {
                let dist = cand_lower.len().abs_diff(rej_lower.len());
                if best.as_ref().is_none_or(|(_, d)| dist < *d) {
                    best = Some((cand, dist));
                }
            } else {
                let dist = levenshtein_distance(&rej_lower, &cand_lower);
                let threshold = (cand.len() / 2).max(2);
                if dist <= threshold && best.as_ref().is_none_or(|(_, d)| dist < *d) {
                    best = Some((cand, dist));
                }
            }
        }
        if let Some((cand, _)) = best {
            suggestions.push((rej.clone(), cand));
        }
    }
    suggestions
}

const ARGUMENT_REDIRECTS: &[(&str, &str, &str)] = &[
    (
        "code_map_overview",
        "path",
        "acdd_read(path=..., mode=\"outline\") or code_map_analyze(target=...)",
    ),
    (
        "code_map_overview",
        "file_path",
        "acdd_read(path=..., mode=\"outline\") or code_map_analyze(target=...)",
    ),
    (
        "code_map_overview",
        "target",
        "code_map_analyze(target=...) or code_map_search(path=...)",
    ),
    (
        "code_map_search",
        "selector",
        "'pattern' (or code_map_inspect(selector=...))",
    ),
    (
        "code_map_search",
        "symbol",
        "'pattern' (or code_map_inspect(selector=...))",
    ),
    ("code_map_search", "target", "'path'"),
    ("code_map_tests", "target", "'selector'"),
    ("code_map_tests", "query", "'selector'"),
    ("code_map_tests", "name", "'selector'"),
    ("code_map_inspect", "target", "'selector' or 'path'"),
    (
        "code_map_inspect",
        "query",
        "'selector' (or code_map_search(pattern=...))",
    ),
    ("code_map_explain", "target", "'selector' or 'path'"),
    ("code_map_impact", "target", "'selector' or 'changes'"),
    ("code_map_prove_removal", "target", "'selector'"),
    (
        "code_map_analyze",
        "sql",
        "'target' (or code_map_query(sql=...))",
    ),
    (
        "code_map_analyze",
        "query",
        "'target' (or code_map_query(sql=...))",
    ),
    ("acdd_read", "target", "'path' or 'selector'"),
    ("acdd_read", "search", "'pattern' with mode=\"search\""),
    ("ast_grep_search", "target", "'path'"),
    ("search_docs", "target", "'component' or 'path'"),
    ("get_doc", "query", "search_docs(query=...)"),
    ("get_doc", "pattern", "search_docs(query=...)"),
    ("get_doc", "title", "'section' (or search_docs(query=...))"),
    ("get_doc", "heading", "'section'"),
    ("acdd_blackboard", "message", "'payload'"),
    ("acdd_blackboard", "body", "'payload'"),
    ("acdd_blackboard", "content", "'payload'"),
    ("acdd_blackboard", "text", "'payload'"),
];

fn resolve_cursor_redirect(allowed: &[&str]) -> Option<&'static str> {
    match (allowed.contains(&"node"), allowed.contains(&"offset")) {
        (true, true) => Some("'node' (or 'offset' and 'generation' for pagination)"),
        (true, false) => Some("'node'"),
        (false, true) => Some("'offset' and 'generation'"),
        (false, false) => None,
    }
}

fn resolve_scope_redirect(allowed: &[&str]) -> Option<&'static str> {
    if allowed.contains(&"path") {
        Some("'path'")
    } else if allowed.contains(&"target") {
        Some("'target'")
    } else {
        None
    }
}

fn find_argument_redirects(
    tool_name: &str,
    rejected: &[String],
    allowed: &[&str],
) -> Vec<(String, &'static str)> {
    rejected
        .iter()
        .filter_map(|field| {
            let hint = ARGUMENT_REDIRECTS
                .iter()
                .find(|(tool, arg, _)| *tool == tool_name && *arg == field.as_str())
                .map(|(_, _, hint)| *hint)
                .or_else(|| match field.as_str() {
                    "cursor" => resolve_cursor_redirect(allowed),
                    "scope" => resolve_scope_redirect(allowed),
                    _ => None,
                })?;
            Some((field.clone(), hint))
        })
        .collect()
}

fn rejected_arguments_error(
    tool_name: &str,
    fields: &[String],
    allowed: &[&str],
) -> CallToolResult {
    let truncated_fields = fields
        .iter()
        .take(16)
        .map(|field| field.chars().take(128).collect::<String>())
        .collect::<Vec<_>>();
    let redirects = find_argument_redirects(tool_name, &truncated_fields, allowed);
    let remaining_fields: Vec<String> = truncated_fields
        .iter()
        .filter(|field| !redirects.iter().any(|(orig, _)| orig == *field))
        .cloned()
        .collect();
    let suggestions = find_suggestions(&remaining_fields, allowed);
    let suggestion_text = if suggestions.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = suggestions
            .iter()
            .map(|(orig, sugg)| format!("'{sugg}' for '{orig}'"))
            .collect();
        format!(" Did you mean: {}?", items.join(", "))
    };
    let redirect_text = if redirects.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = redirects
            .iter()
            .map(|(orig, hint)| format!("for '{orig}', try {hint}"))
            .collect();
        format!(" Hint: {}.", items.join("; "))
    };
    let message = format!(
        "Unrecognized argument(s): {}. Valid supported parameters for '{tool_name}': {}.{suggestion_text}{redirect_text}",
        truncated_fields.join(", "),
        allowed.join(", ")
    );

    response::rejected_arguments(fields, Some(message), Some(allowed), &suggestions)
}

fn plugin_tool_to_rmcp(full_name: String, def: DomainToolDef) -> anyhow::Result<Tool> {
    let mut schema_obj = match serde_json::to_value(&def.input_schema)? {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if schema_obj.get("type").and_then(Value::as_str) == Some("object") {
        let props = schema_obj
            .entry("properties")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(map) = props.as_object_mut() {
            map.entry("worktree").or_insert_with(|| {
                serde_json::json!({
                    "type": "string",
                    "description": "Optional worktree path or unique name selector."
                })
            });
            map.entry("format").or_insert_with(|| {
                serde_json::json!({
                    "type": "string",
                    "enum": ["markdown", "json"],
                    "description": "Output format (markdown or json)."
                })
            });
        }
    }
    Ok(Tool::new(full_name, def.description, schema_obj)
        .with_annotations(ToolAnnotations::new().read_only(def.read_only)))
}

pub(super) fn validate_explicit_worktree_git_ancestors(
    server_root: &Path,
    requested_worktree: Option<&str>,
) -> anyhow::Result<()> {
    let Some(wt) = requested_worktree else {
        return Ok(());
    };
    let p = Path::new(wt);
    if p.is_absolute() || wt == "." || wt == ".." || wt.contains('/') || wt.contains('\\') {
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            server_root.join(p)
        };
        if candidate.is_dir() {
            acdd_core::fs::check_ancestors_no_symlinks(&candidate, Path::new(".git")).map_err(
                |e| anyhow::anyhow!(StateError::new(ErrorCode::ExternalGitDrift, e.to_string())),
            )?;
        }
    }
    Ok(())
}

fn invalid_format_policy(
    server: &Server,
    request: &rmcp::model::CallToolRequestParams,
) -> ResponsePolicy {
    let primary_adapter = server.load_adapter(&server.root);
    let fallback_policy = primary_adapter
        .as_ref()
        .map(|adapter| adapter.response.clone())
        .unwrap_or_default();
    let requested_worktree = request
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get("worktree"))
        .and_then(Value::as_str);
    let Ok((target_root, _)) = server.resolve_target_root_and_db_with_adapter(
        requested_worktree,
        primary_adapter.as_ref().ok(),
    ) else {
        return fallback_policy;
    };
    if target_root == server.root {
        return fallback_policy;
    }

    server
        .load_adapter(&target_root)
        .map(|adapter| adapter.response)
        .unwrap_or(fallback_policy)
}

fn active_tool_count(server: &Server) -> usize {
    let adapter = server.load_adapter(&server.root).unwrap_or_default();
    let static_tools = Server::tool_router().list_all();
    let active_static = acdd_core::subsystems::filter_subsystem_tools(
        static_tools.iter().map(|tool| tool.name.as_ref()),
        &adapter.modules,
    )
    .len();
    let active_plugins = server
        .domain_providers(&adapter)
        .map(|providers| providers.plugin_tools().len())
        .unwrap_or(0);
    active_static + active_plugins
}

pub(super) fn get_info(server: &Server) -> rmcp::model::ServerConfig {
    let active_count = active_tool_count(server);
    let instructions = format!(
        "Workspace code graph and task coordination, {active_count} tools. Responses have a 96 KiB maximum; lower configured byte limits remain authoritative. SQLite budget is 2 seconds. Start with code_map_overview and check coverage before an absence claim. Continue a page with offset and generation. Prefer detail=compact. Selector rules and gate proofs: acdd_guide topic=query or acdd."
    );
    rmcp::model::ServerConfig::new(
        rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build(),
    )
    .with_server_info(rmcp::model::Implementation::new(
        "contextunity-acdd",
        env!("CARGO_PKG_VERSION"),
    ))
    .with_instructions(instructions)
}

pub(super) fn get_tool(server: &Server, name: &str) -> Option<rmcp::model::Tool> {
    let adapter = server.load_adapter(&server.root).ok()?;
    if acdd_core::subsystems::disabled_tool_error(name, &adapter.modules).is_some() {
        return None;
    }
    if let Some(tool) = Server::tool_router().get(name).cloned() {
        return Some(tool);
    }
    let providers = server.domain_providers(&adapter).ok()?;
    providers
        .plugin_tools()
        .into_iter()
        .find(|(full_name, _)| full_name == name)
        .and_then(|(full_name, def)| plugin_tool_to_rmcp(full_name, def).ok())
}

pub(super) async fn list_tools(
    server: &Server,
    context: rmcp::service::RequestContext<rmcp::RoleServer>,
) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
    let server = server.clone();
    let tools = tokio::task::spawn_blocking(move || -> Result<Vec<Tool>, rmcp::ErrorData> {
        let adapter = server.load_adapter(&server.root).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("workspace module configuration is invalid: {error}"),
                None,
            )
        })?;
        let mut tools = Server::tool_router().list_all();
        let enabled = acdd_core::subsystems::filter_subsystem_tools(
            tools.iter().map(|tool| tool.name.as_ref()),
            &adapter.modules,
        )
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
        tools.retain(|tool| enabled.contains(tool.name.as_ref()));

        let providers = server.domain_providers(&adapter).map_err(|error| {
            rmcp::ErrorData::internal_error(
                format!("domain provider configuration is invalid: {error}"),
                None,
            )
        })?;
        for (full_name, def) in providers.plugin_tools() {
            let tool = plugin_tool_to_rmcp(full_name, def).map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("invalid plugin tool schema: {error}"),
                    None,
                )
            })?;
            tools.push(tool);
        }
        Ok(tools)
    })
    .await
    .map_err(|error| {
        rmcp::ErrorData::internal_error(
            format!("list_tools blocking task failed: {error}"),
            None,
        )
    })??;
    let supports_cache_hints = context
        .protocol_version()
        .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
    Ok(rmcp::model::ListToolsResult {
        result_type: Some(rmcp::model::ResultType::COMPLETE),
        tools,
        meta: None,
        next_cursor: None,
        ttl_ms: supports_cache_hints.then_some(0),
        cache_scope: supports_cache_hints.then_some(rmcp::model::CacheScope::Public),
    })
}

struct PreparedCall {
    target_root: PathBuf,
    target_db: PathBuf,
    adapter: RequestAdapter,
    loaded_adapter: Option<std::sync::Arc<scanner::Adapter>>,
    providers: Option<std::sync::Arc<acdd_core::ports::CompositeDomainProvider>>,
    span_ctx: Option<acdd_core::telemetry::WorktreeSpanContext>,
    early_response: Option<CallToolResult>,
}

impl PreparedCall {
    fn failed(
        server: &Server,
        error: anyhow::Error,
        output_format: OutputFormat,
        operation: OperationKind,
        tool_name: &str,
    ) -> Self {
        let code = acdd_core::errors::AcddError::from_anyhow(&error).map(|e| e.code);
        let message = error.to_string();
        Self {
            target_root: server.root.clone(),
            target_db: server.db.clone(),
            adapter: RequestAdapter::Failed { code, message },
            loaded_adapter: None,
            providers: None,
            span_ctx: None,
            early_response: Some(response::result_for(
                Err(error),
                &ResponsePolicy::default(),
                output_format,
                operation,
                tool_name,
                None,
            )),
        }
    }
}

fn prepare_tool_call_blocking(
    server_prep: &Server,
    requested_worktree: Option<&str>,
    tool_name_prep: &str,
    args_prep: Option<&serde_json::Map<String, Value>>,
    output_format: OutputFormat,
    operation: OperationKind,
) -> PreparedCall {
    if let Err(error) =
        validate_explicit_worktree_git_ancestors(&server_prep.root, requested_worktree)
    {
        return PreparedCall::failed(server_prep, error, output_format, operation, tool_name_prep);
    }
    let primary_adapter = server_prep.load_adapter(&server_prep.root);
    let (target_root, target_db) = match server_prep
        .resolve_target_root_and_db_with_adapter(requested_worktree, primary_adapter.as_ref().ok())
    {
        Ok(pair) => pair,
        Err(error) => {
            return PreparedCall::failed(
                server_prep,
                error,
                output_format,
                operation,
                tool_name_prep,
            );
        }
    };
    let adapter_result =
        if requested_worktree.is_none() || target_root == server_prep.root {
            primary_adapter
        } else {
            server_prep.load_adapter(&target_root)
        };
    let (adapter, loaded_adapter) = match adapter_result {
        Ok(adapter) => {
            let arc = std::sync::Arc::new(adapter);
            (
                RequestAdapter::Loaded(std::sync::Arc::clone(&arc)),
                Some(arc),
            )
        }
        Err(error) => (
            RequestAdapter::Failed {
                code: acdd_core::errors::AcddError::from_anyhow(&error).map(|e| e.code),
                message: error.to_string(),
            },
            None,
        ),
    };
    if let Some(ref arc) = loaded_adapter {
        if let Some(message) =
            acdd_core::subsystems::disabled_tool_error(tool_name_prep, &arc.modules)
        {
            let early_response = Some(response::result_for(
                Err(StateError::new(ErrorCode::FeatureDisabled, message).into()),
                &arc.response,
                output_format,
                operation,
                tool_name_prep,
                None,
            ));
            return PreparedCall {
                target_root,
                target_db,
                adapter,
                loaded_adapter,
                providers: None,
                span_ctx: None,
                early_response,
            };
        }
    }
    let is_static_route = Server::tool_router().has_route(tool_name_prep);
    let ws_root = primary_workspace_root_for(&server_prep.root, &target_root);
    // Lazily build CompositeDomainProvider only for dynamic plugin tools.
    let providers = if !is_static_route {
        loaded_adapter.as_ref().and_then(|a| {
            server_prep
                .domain_providers_for(&ws_root, Some(&target_db), a)
                .ok()
        })
    } else {
        None
    };

    if let Some(providers) = providers.as_ref() {
        if let Some((_, def)) = providers
            .plugin_tools()
            .into_iter()
            .find(|(n, _)| n == tool_name_prep)
        {
            if let Some((rejected, allowed)) = rejected_plugin_fields(&def, args_prep) {
                let allowed_refs: Vec<&str> = allowed.iter().map(String::as_str).collect();
                return PreparedCall {
                    target_root,
                    target_db,
                    adapter,
                    loaded_adapter,
                    providers: Some(std::sync::Arc::clone(providers)),
                    span_ctx: None,
                    early_response: Some(rejected_arguments_error(
                        tool_name_prep,
                        &rejected,
                        &allowed_refs,
                    )),
                };
            }
        }
    }

    let span_ctx = loaded_adapter.as_ref().and_then(|a| {
        a.modules
            .is_subsystem_enabled(SubsystemId::State)
            .then(|| {
                crate::state_bridge::resolve_worktree_span_context(
                    &ws_root,
                    &target_root,
                    server_prep.state_db(),
                    a,
                )
                .ok()
                .flatten()
            })
            .flatten()
    });

    PreparedCall {
        target_root,
        target_db,
        adapter,
        loaded_adapter,
        providers,
        span_ctx,
        early_response: None,
    }
}

async fn execute_plugin_tool_route(
    server: &Server,
    prep: PreparedCall,
    tool_name: &str,
    raw_args: serde_json::Map<String, Value>,
    policy: &ResponsePolicy,
    output_format: OutputFormat,
    operation: OperationKind,
) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
    let providers = prep
        .providers
        .expect("providers checked in is_plugin_route");
    let server_root = server.root.clone();
    let target_root = prep.target_root;
    let span_ctx = prep.span_ctx;
    let tool_name_owned = tool_name.to_owned();
    let plugin_res = tokio::task::spawn_blocking(move || {
        let mut filtered_args = raw_args;
        let declared_props = providers
            .plugin_tools()
            .into_iter()
            .find(|(n, _)| n == &tool_name_owned)
            .map(|(_, def)| def.input_schema.properties);
        if !declared_props
            .as_ref()
            .is_some_and(|p| p.contains_key("format"))
        {
            filtered_args.remove("format");
        }
        if !declared_props
            .as_ref()
            .is_some_and(|p| p.contains_key("worktree"))
        {
            filtered_args.remove("worktree");
        }
        let workspace_root = primary_workspace_root_for(&server_root, &target_root);
        let req = DomainToolReq {
            workspace_root,
            worktree: Some(target_root),
            run_id: span_ctx.as_ref().map(|c| c.run_id.clone()),
            node_cursor: span_ctx
                .as_ref()
                .and_then(|c| NodeCursor::from_node_id(&c.node_id).ok()),
        };
        providers.call_tool(&tool_name_owned, &Value::Object(filtered_args), &req)
    })
    .await;
    match plugin_res {
        Ok(Ok(output)) => {
            let text = match output_format {
                OutputFormat::Markdown => {
                    crate::format::render_plugin_tool_markdown(tool_name, &output)
                }
                OutputFormat::Json => {
                    let val = match output {
                        DomainToolOutput::Json(v) => v,
                        DomainToolOutput::Markdown(m) => serde_json::json!({ "markdown": m }),
                    };
                    serde_json::to_string(&val).unwrap_or_else(|_| "null".to_owned())
                }
            };
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
        }
        Ok(Err(err)) => Ok(response::result_for(
            Err(err),
            policy,
            output_format,
            operation,
            tool_name,
            None,
        )
        .into()),
        Err(join_err) => Err(rmcp::ErrorData::internal_error(
            format!("plugin tool blocking task failed: {join_err}"),
            None,
        )),
    }
}

pub(super) async fn call_tool(
    server: &Server,
    request: rmcp::model::CallToolRequestParams,
    context: rmcp::service::RequestContext<rmcp::RoleServer>,
) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
    let format_value = request
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get("format"));
    let output_format = match format_value {
        None => OutputFormat::default(),
        Some(value) => match value
            .as_str()
            .and_then(|requested| OutputFormat::parse(requested).ok())
        {
            Some(format) => format,
            None => {
                let server = server.clone();
                let req_clone = request.clone();
                let policy =
                    tokio::task::spawn_blocking(move || invalid_format_policy(&server, &req_clone))
                        .await
                        .unwrap_or_default();
                return Ok(response::invalid_format(value, &policy).into());
            }
        },
    };
    let tool_name = request.name.clone();
    let operation = operation_kind(&tool_name, request.arguments.as_ref());
    let rejected = rejected_selector_fields(&request.name, request.arguments.as_ref());
    if !rejected.is_empty() {
        let allowed: Vec<&str> = allowed_fields_for_tool(&request.name)
            .iter()
            .map(String::as_str)
            .collect();
        return Ok(rejected_arguments_error(&request.name, &rejected, &allowed).into());
    }

    let requested_worktree = request
        .arguments
        .as_ref()
        .and_then(|args| args.get("worktree"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let server_prep = server.clone();
    let tool_name_prep = tool_name.to_string();
    let args_prep = request.arguments.clone();

    let prep = tokio::task::spawn_blocking(move || {
        prepare_tool_call_blocking(
            &server_prep,
            requested_worktree.as_deref(),
            &tool_name_prep,
            args_prep.as_ref(),
            output_format,
            operation,
        )
    })
    .await
    .map_err(|error| {
        rmcp::ErrorData::internal_error(
            format!("call_tool preparation failed: {error}"),
            None,
        )
    })?;

    if let Some(early) = prep.early_response {
        return Ok(early.into());
    }

    let retain_traces = prep
        .loaded_adapter
        .as_ref()
        .map_or(acdd_core::config::DEFAULT_RETAIN_TRACES, |a| {
            a.retain_traces
        });
    let span_state_db = prep.loaded_adapter.as_ref().and_then(|_| {
        let ws_root = primary_workspace_root_for(&server.root, &prep.target_root);
        acdd_state::usecases::deps::try_resolve_effective_state_db(&ws_root, server.state_db()).ok()
    });
    let mut span_guard = crate::telemetry::SpanGuard::from_worktree_context(
        &prep.target_root,
        prep.span_ctx.as_ref(),
        &tool_name,
        retain_traces,
    );
    if let Some(state_db) = span_state_db {
        span_guard = span_guard.with_state_db(state_db);
    }
    span_guard.set_attribute("mcp.tool", serde_json::json!(tool_name.as_ref()));
    span_guard.set_attribute(
        "mcp.worktree",
        serde_json::json!(prep.target_root.display().to_string()),
    );

    let debug_request = prep
        .loaded_adapter
        .as_ref()
        .filter(|a| a.debug)
        .map(|_| {
            (
                request.name.clone(),
                serde_json::to_string_pretty(&request.arguments).unwrap_or_default(),
            )
        });

    let default_policy = ResponsePolicy::default();
    let loaded_adapter = prep.loaded_adapter.clone();
    let policy = loaded_adapter
        .as_ref()
        .map_or(&default_policy, |a| &a.response);

    let is_static_route = Server::tool_router().has_route(&tool_name);
    let is_plugin_route = !is_static_route
        && prep.providers.as_ref().is_some_and(|p| {
            p.has_enabled_plugin_tool(&tool_name)
                || matches!(
                    p.validate_plugin_tool_name(&tool_name),
                    Err(ref err) if err.message.contains("disabled")
                )
        });

    let result: Result<rmcp::model::CallToolResponse, rmcp::ErrorData> = if is_plugin_route {
        let raw_args = request.arguments.clone().unwrap_or_default();
        execute_plugin_tool_route(
            server,
            prep,
            &tool_name,
            raw_args,
            policy,
            output_format,
            operation,
        )
        .await
    } else {
        let request_server =
            server.with_root_and_db(prep.target_root, prep.target_db, prep.adapter);
        let call = rmcp::handler::server::tool::ToolCallContext::new(
            &request_server,
            request,
            context,
        );
        Server::tool_router().call(call).await
    };

    let final_response: Result<rmcp::model::CallToolResponse, rmcp::ErrorData> = match result {
        Ok(rmcp::model::CallToolResponse::Complete(result)) => {
            Ok(response::enforce(result, policy).into())
        }
        Ok(result) => Ok(result),
        Err(error) => {
            let err_msg = error.to_string();
            let enhanced = if tool_name == "acdd_read" {
                let allowed = allowed_fields_for_tool(&tool_name);
                format!(
                    "{err_msg}\nValid supported parameters for '{tool_name}': {}",
                    allowed.join(", ")
                )
            } else {
                err_msg
            };
            Ok(response::result_for(
                Err(anyhow::anyhow!(enhanced)),
                policy,
                output_format,
                operation,
                &tool_name,
                None,
            )
            .into())
        }
    };

    let is_err = match &final_response {
        Ok(rmcp::model::CallToolResponse::Complete(res)) => res.is_error == Some(true),
        Ok(_) => false,
        Err(_) => true,
    };
    span_guard.set_status(if is_err { "error" } else { "ok" });

    if let Some((tool_name, tool_args)) = debug_request {
        let output_val = match &final_response {
            Ok(rmcp::model::CallToolResponse::Complete(res)) => {
                let mut text_val = None;
                for item in &res.content {
                    if let Some(t) = item.as_text() {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&t.text) {
                            text_val = Some(val);
                            break;
                        }
                    }
                }
                text_val.unwrap_or_else(|| {
                    serde_json::to_value(res).unwrap_or(serde_json::json!({"status": "complete"}))
                })
            }
            Ok(resp) => serde_json::json!({ "raw": format!("{resp:?}") }),
            Err(err) => serde_json::json!({ "error": err.to_string() }),
        };
        span_guard.set_debug_mcp(&server.root, &tool_name, &tool_args, &output_val);
    }
    span_guard.finish_async().await;
    server.arm_pending_reload();
    final_response
}
