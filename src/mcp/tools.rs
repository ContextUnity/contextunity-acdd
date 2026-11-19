use super::{
    response,
    server::{primary_workspace_root_for, Server},
};
use crate::{
    cli,
    format::{DiscoveryKind, OperationKind, OutputFormat},
};
use acdd_core::response::{CoverageOptions, Detail, ResponsePolicy};
use acdd_index::{
    db::{reader, symbols, traversal},
    scanner,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::NodeCursor;
use acdd_core::ports::{DomainToolDef, DomainToolOutput, DomainToolReq};
use acdd_core::schema::AdditionalPropertiesDef;
use acdd_state::types::{
    BlackboardStatus, BlackboardTopic, RunStatus, SubtaskStatus,
};
use acdd_state::usecases::{
    BlackboardPostReq, BlackboardReadReq, ClaimReq, ClearReq, DraftReq, ExtendScopeReq, ListReq,
    NextReq, ObservabilityReq, ReopenReq, RunReq, ShowReq, ContextReq, StartReq, StateCommand,
    StateCommandOutput, StateQuery, StateQueryOutput, StatusReq, SubmitReq, SubtaskReq, SyncReq,
    VerifyReq,
};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Tool, ToolAnnotations},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[path = "tools/inputs.rs"]
mod inputs;
pub use inputs::{
    Analyze, Ast, BlackboardInput, ClaimInput, Explain, GetDoc, Guide, Impact, Inspect,
    McpBlackboardAction, McpStateAction, OverviewInput, PageInput, Query, SearchDocs,
    SearchSymbols, Selector, Snippet, StateInput, SubmitInput, Tests, VerifyInput, WorktreeInput,
};

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
                dp[i + 1][j + 1] = 1 + dp[i][j].min(dp[i + 1][j]).min(dp[i + 1][j + 1]);
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
    let suggestions = find_suggestions(&truncated_fields, allowed);
    let suggestion_text = if suggestions.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = suggestions
            .iter()
            .map(|(orig, sugg)| format!("'{sugg}' for '{orig}'"))
            .collect();
        format!(" Did you mean: {}?", items.join(", "))
    };
    let message = format!(
        "Unrecognized argument(s): {}. Valid supported parameters for '{tool_name}': {}.{suggestion_text}",
        truncated_fields.join(", "),
        allowed.join(", ")
    );

    response::rejected_arguments(fields, Some(message), Some(allowed), &suggestions)
}

fn require_present<T>(value: Option<T>, field: &str, action: &str) -> Result<T, StateError> {
    value.ok_or_else(|| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("'{field}' is required for action '{action}'"),
        )
    })
}

fn require_field(value: Option<String>, field: &str, action: &str) -> Result<String, StateError> {
    require_present(value.filter(|s| !s.trim().is_empty()), field, action)
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

fn build_state_command_from_input(
    p: StateInput,
    explicit_worktree: Option<PathBuf>,
) -> std::result::Result<StateCommand, StateError> {
    let run = p.run;
    match p.action {
        McpStateAction::Start => {
            let create_wt = p
                .create_worktree
                .unwrap_or_else(|| p.worktree.as_ref().and_then(WorktreeInput::as_flag).unwrap_or(false));
            let in_place = p.in_place.unwrap_or(false);
            let worktree_path = p
                .worktree_path
                .or(if in_place { None } else { explicit_worktree });
            Ok(StateCommand::Start(StartReq {
                spec_path: p.spec_path,
                goal: p.goal,
                run_name: p.run_name,
                profile: p.profile,
                scope: p.scope.unwrap_or_default(),
                target_base: p.target_base,
                worktree: create_wt,
                in_place,
                worktree_path,
            }))
        }
        McpStateAction::Draft => {
            let collection = require_field(p.collection, "collection", "draft")?;
            let slug = require_field(p.slug, "slug", "draft")?;
            Ok(StateCommand::Draft(DraftReq {
                collection,
                slug,
                prefix: p.prefix,
                title: p.title,
                profile: p.profile,
            }))
        }
        McpStateAction::ExtendScope => {
            let node = require_field(p.node, "node", "extend_scope")?;
            let paths = p.paths.unwrap_or_default();
            if paths.is_empty() {
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "'paths' must be non-empty for action 'extend_scope'",
                ));
            }
            let reason = require_field(p.reason, "reason", "extend_scope")?;
            Ok(StateCommand::ExtendScope(ExtendScopeReq {
                run,
                node,
                paths,
                reason,
                worker_id: p.worker_id,
            }))
        }
        McpStateAction::Reopen => {
            let node = require_field(p.node, "node", "reopen")?;
            Ok(StateCommand::Reopen(ReopenReq {
                run,
                node,
                cascade: p.cascade.unwrap_or(false),
                reason: p.reason,
            }))
        }
        McpStateAction::Sync => Ok(StateCommand::Sync(SyncReq {
            run,
            rebind_profile: p.rebind_profile.unwrap_or(false),
        })),
        McpStateAction::Subtask => {
            let task_ref = require_present(p.task_ref, "task_ref", "subtask")?;
            let subtask_ref = require_present(p.subtask_ref, "subtask_ref", "subtask")?;
            let status_raw = require_field(p.status.or(p.subtask_status), "status", "subtask")?;
            let status = SubtaskStatus::from_str(&status_raw)?;
            Ok(StateCommand::Subtask(SubtaskReq {
                run,
                task_ref,
                subtask_ref,
                status,
                evidence: p.evidence,
            }))
        }
        McpStateAction::Clear => Ok(StateCommand::Clear(ClearReq {
            run,
            reason: p.reason,
            force: p.force.unwrap_or(false),
        })),
        McpStateAction::Run => Ok(StateCommand::Run(RunReq {
            run,
            jobs: p.jobs,
            allow_main: p.allow_main.unwrap_or(false),
            dry_run: p.dry_run.unwrap_or(false),
        })),
        McpStateAction::Status
        | McpStateAction::List
        | McpStateAction::Show
        | McpStateAction::Next
        | McpStateAction::Observability
        | McpStateAction::Context => unreachable!("read-only actions route via StateQuery"),
    }
}

fn build_state_query_from_input(p: StateInput) -> std::result::Result<StateQuery, StateError> {
    let run = p.run;
    match p.action {
        McpStateAction::Status => Ok(StateQuery::Status(StatusReq { run })),
        McpStateAction::List => {
            let status = p
                .status
                .as_deref()
                .map(RunStatus::from_str)
                .transpose()?;
            Ok(StateQuery::List(ListReq {
                workspace: p.workspace,
                status,
                include_tombstones: p.include_tombstones.unwrap_or(false),
                limit: p.limit,
                offset: p.offset.unwrap_or(0),
                generation: p.generation,
            }))
        }
        McpStateAction::Show => Ok(StateQuery::Show(ShowReq { run, node: p.node })),
        McpStateAction::Next => Ok(StateQuery::Next(NextReq {
            run,
            limit: p.limit,
        })),
        McpStateAction::Observability => Ok(StateQuery::Observability(ObservabilityReq {
            include_prometheus: p.include_prometheus.unwrap_or(false),
        })),
        McpStateAction::Context => Ok(StateQuery::Context(ContextReq {
            run,
            node: p.node,
            worker_id: p.worker_id,
        })),
        _ => unreachable!("mutating actions route via StateCommand"),
    }
}

impl Server {
    async fn responding_blocking(
        &self,
        output_format: OutputFormat,
        operation: OperationKind,
        title: &'static str,
        explain: Option<bool>,
        discovery: Option<DiscoveryKind>,
        run: impl FnOnce(&Server, &scanner::Adapter, &ResponsePolicy) -> anyhow::Result<Value>
            + Send
            + 'static,
    ) -> CallToolResult {
        let server = self.clone();
        match tokio::task::spawn_blocking(move || {
            server.responding_with_presentation(
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
        &self,
        output_format: OutputFormat,
        operation: OperationKind,
        title: &'static str,
        run: impl FnOnce(&scanner::Adapter, &ResponsePolicy) -> anyhow::Result<Value>,
        explain: Option<bool>,
        discovery: Option<DiscoveryKind>,
    ) -> CallToolResult {
        match self.adapter_snapshot() {
            Ok(adapter) => {
                let mut value = run(&adapter, &adapter.response);
                if output_format == OutputFormat::Markdown {
                    if let (Some(kind), Ok(value)) = (discovery, &mut value) {
                        cli::discovery::attach_presentation_uris(&self.root, &adapter, value, kind);
                    } else if matches!(title, "search_docs" | "get_doc") {
                        if let Ok(value) = &mut value {
                            cli::discovery::attach_document_presentation_uris(
                                &self.root, &adapter, value, title,
                            );
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
                            cli::discovery::attach_traversal_presentation_uris(
                                &self.root, &adapter, value,
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
}

struct StateCallContext<'a> {
    server: &'a Server,
    workspace_root: &'a Path,
    target_root: &'a Path,
    state_db: Option<&'a Path>,
    code_db: Option<&'a Path>,
    adapter: &'a scanner::Adapter,
    providers: std::sync::Arc<acdd_core::ports::CompositeDomainProvider>,
}

impl Server {
    async fn run_state_blocking(
        &self,
        output_format: OutputFormat,
        title: &'static str,
        op: impl FnOnce(StateCallContext<'_>) -> anyhow::Result<(String, Value)> + Send + 'static,
    ) -> CallToolResult {
        let server = self.clone();
        match tokio::task::spawn_blocking(move || {
            let adapter = match server.adapter_snapshot() {
                Ok(adapter) => adapter,
                Err(err) => {
                    return response::state_result(
                        Err(err.into()),
                        &ResponsePolicy::default(),
                        output_format,
                        title,
                    );
                }
            };
            if let Some(message) =
                acdd_core::subsystems::disabled_tool_error(title, &adapter.modules)
            {
                return response::state_result(
                    Err(StateError::new(ErrorCode::FeatureDisabled, message).into()),
                    &adapter.response,
                    output_format,
                    title,
                );
            }
            let target_root = server.root.clone();
            let workspace_root = server
                .request_workspace_root()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| primary_workspace_root_for(&server.root, &target_root));
            let state_db = server.state_db().map(Path::to_path_buf);
            let code_db = Some(server.db.as_path());
            let providers = match server.domain_providers_for(&workspace_root, code_db, &adapter) {
                Ok(providers) => providers,
                Err(err) => {
                    return response::state_result(
                        Err(err.into()),
                        &adapter.response,
                        output_format,
                        title,
                    );
                }
            };
            let result = op(StateCallContext {
                server: &server,
                workspace_root: &workspace_root,
                target_root: &target_root,
                state_db: state_db.as_deref(),
                code_db,
                adapter: &adapter,
                providers,
            });
            response::state_result(result, &adapter.response, output_format, title)
        })
        .await
        {
            Ok(res) => res,
            Err(err) => response::state_result(
                Err(anyhow::anyhow!("MCP state blocking task failed: {err}")),
                &ResponsePolicy::default(),
                output_format,
                title,
            ),
        }
    }
}

#[tool_router]
impl Server {
    #[tool(
        description = "Execute a canonical state lifecycle command (`start`, `draft`, `extend_scope`, `reopen`, `sync`, `subtask`, `clear`, `run`) or read-only state query (`status`, `list`, `show`, `next`, `context`, `observability`). Read-only actions operate over `SQLITE_OPEN_READ_ONLY` with zero database creation."
    )]
    async fn acdd_state(&self, Parameters(p): Parameters<StateInput>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        let explicit_wt_nonempty = p
            .worktree
            .as_ref()
            .and_then(WorktreeInput::as_selector)
            .is_some_and(|s| !s.trim().is_empty());
        self.run_state_blocking(format, "acdd_state", move |ctx| {
            if p.action.is_read_only() {
                let query = build_state_query_from_input(p)?;
                let out = crate::state_bridge::execute_state_query(
                    ctx.workspace_root,
                    ctx.target_root,
                    ctx.state_db,
                    ctx.code_db,
                    ctx.adapter,
                    ctx.providers,
                    query,
                )?;
                let md = crate::format::render_state_query_markdown(&out, "acdd_state");
                let val = out.into_untagged_json()?;
                Ok((md, val))
            } else {
                let explicit = explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf());
                let cmd = build_state_command_from_input(p, explicit)?;
                let out = crate::state_bridge::execute_state_command(
                    ctx.workspace_root,
                    ctx.target_root,
                    ctx.state_db,
                    ctx.code_db,
                    ctx.adapter,
                    ctx.providers,
                    cmd,
                )?;
                let md = crate::format::render_state_command_markdown(&out, "acdd_state");
                let val = out.into_untagged_json()?;
                Ok((md, val))
            }
        })
        .await
    }

    #[tool(
        description = "Atomically claim or re-claim a ready/active gate node (or auto-select the next ready node when `node` is omitted). Returns budgeted `ClaimResp` with mandatory contract/guidance, unresolved findings, dependency receipts, blackboard messages, and context lenses."
    )]
    async fn acdd_claim(&self, Parameters(p): Parameters<ClaimInput>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
        self.run_state_blocking(format, "acdd_claim", move |ctx| {
            let req = ClaimReq {
                run: p.run,
                node: p.node,
                worker_id: p.worker_id,
                session_fingerprint: ctx.server.session_fingerprint.clone(),
                force: p.force,
                steal_remote: p.steal_remote,
                worktree: explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf()),
            };
            let out = crate::state_bridge::execute_state_command(
                ctx.workspace_root,
                ctx.target_root,
                ctx.state_db,
                ctx.code_db,
                ctx.adapter,
                ctx.providers,
                StateCommand::Claim(req),
            )?;
            let StateCommandOutput::Claim(claim) = out else {
                unreachable!("Claim command always returns Claim output");
            };
            let md = crate::format::render_claim_markdown(&claim);
            let val = serde_json::to_value(&claim)?;
            Ok((md, val))
        })
        .await
    }

    #[tool(
        description = "Execute gate verification (`command`, `call`, `checks`, and admitted scope barrier) for a claimed node, recording an execution-digest-bound trace for `acdd_submit`."
    )]
    async fn acdd_verify(&self, Parameters(p): Parameters<VerifyInput>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
        self.run_state_blocking(format, "acdd_verify", move |ctx| {
            let req = VerifyReq {
                run: p.run,
                node: p.node,
                worker_id: p.worker_id,
                test_target: p.test_target,
                claim_revision: p.claim_revision,
                worktree: explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf()),
            };
            let out = crate::state_bridge::execute_state_command(
                ctx.workspace_root,
                ctx.target_root,
                ctx.state_db,
                ctx.code_db,
                ctx.adapter,
                ctx.providers,
                StateCommand::Verify(req),
            )?;
            let StateCommandOutput::Verify(verify) = out else {
                unreachable!("Verify command always returns Verify output");
            };
            let md = crate::format::render_verify_markdown(&verify);
            let val = serde_json::to_value(&verify)?;
            Ok((md, val))
        })
        .await
    }

    #[tool(
        description = "Submit `pass` or `reject` for a claimed gate node. Passing verifies trace freshness (`execution_digest`, `candidate_tree`), `independent_from`, `immutable`, and `need:`; `reject` requires structured `rejection_findings` (optional `kind`: `defect | architectural_drift`)."
    )]
    async fn acdd_submit(&self, Parameters(p): Parameters<SubmitInput>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
        self.run_state_blocking(format, "acdd_submit", move |ctx| {
            let req = SubmitReq {
                run: p.run,
                node: p.node,
                action: p.action,
                worker_id: p.worker_id,
                claim_revision: p.claim_revision,
                test_target: p.test_target,
                review_evidence: p.review_evidence,
                rejection_findings: p.rejection_findings,
                custom_evidence: p.custom_evidence,
                verified_invariants: p.verified_invariants,
                summary: p.summary,
                allow_main: p.allow_main,
                worktree: explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf()),
            };
            let out = crate::state_bridge::execute_state_command(
                ctx.workspace_root,
                ctx.target_root,
                ctx.state_db,
                ctx.code_db,
                ctx.adapter,
                ctx.providers,
                StateCommand::Submit(Box::new(req)),
            )?;
            let StateCommandOutput::Submit(submit) = out else {
                unreachable!("Submit command always returns Submit output");
            };
            let md = crate::format::render_submit_markdown(&submit);
            let val = serde_json::to_value(&submit)?;
            Ok((md, val))
        })
        .await
    }

    #[tool(
        description = "Read (`StateQuery::BlackboardRead` over `SQLITE_OPEN_READ_ONLY`) or post (`StateCommand::BlackboardPost`) run-, node-, or recipient-scoped blackboard entries."
    )]
    async fn acdd_blackboard(&self, Parameters(p): Parameters<BlackboardInput>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        self.run_state_blocking(format, "acdd_blackboard", move |ctx| match p.action {
            McpBlackboardAction::Read => {
                let topic = p
                    .topic
                    .as_deref()
                    .map(BlackboardTopic::from_str)
                    .transpose()?;
                let status = p
                    .status
                    .as_deref()
                    .map(BlackboardStatus::from_str)
                    .transpose()?;
                let req = BlackboardReadReq {
                    run: p.run,
                    node: p.node,
                    recipient_node: p.recipient_node,
                    topic,
                    status,
                    limit: p.limit,
                    offset: p.offset.unwrap_or(0),
                    generation: p.generation,
                };
                let out = crate::state_bridge::execute_state_query(
                    ctx.workspace_root,
                    ctx.target_root,
                    ctx.state_db,
                    ctx.code_db,
                    ctx.adapter,
                    ctx.providers,
                    StateQuery::BlackboardRead(req),
                )?;
                let StateQueryOutput::BlackboardRead(resp) = out else {
                    unreachable!("BlackboardRead query always returns BlackboardRead output");
                };
                let val = serde_json::to_value(&resp)?;
                let md = crate::format::render_blackboard_markdown(&val, "acdd_blackboard");
                Ok((md, val))
            }
            McpBlackboardAction::Post => {
                let topic_str = require_field(p.topic, "topic", "post")?;
                let topic = BlackboardTopic::from_str(&topic_str)?;
                let payload = require_field(p.payload, "payload", "post")?;
                let author = p.author.unwrap_or_else(|| "mcp".to_owned());
                let req = BlackboardPostReq {
                    run: p.run,
                    node: p.node,
                    recipient_node: p.recipient_node,
                    subtask_ref: p.subtask_ref,
                    gate: p.gate,
                    author,
                    topic,
                    payload,
                };
                let out = crate::state_bridge::execute_state_command(
                    ctx.workspace_root,
                    ctx.target_root,
                    ctx.state_db,
                    ctx.code_db,
                    ctx.adapter,
                    ctx.providers,
                    StateCommand::BlackboardPost(req),
                )?;
                let StateCommandOutput::BlackboardPost(resp) = out else {
                    unreachable!("BlackboardPost command always returns BlackboardPost output");
                };
                let val = serde_json::to_value(&resp)?;
                let md = crate::format::render_blackboard_markdown(&val, "acdd_blackboard");
                Ok((md, val))
            }
        })
        .await
    }

    #[tool(
        description = "Workspace root, counts, languages, and resolution coverage, including paged coverage_by_workspace counts from the indexed primary and linked workspace snapshot. Uses server worktree routing; pass worktree for explicit selection."
    )]
    async fn code_map_overview(&self, Parameters(p): Parameters<OverviewInput>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
            format,
            OperationKind::Navigation,
            "code_map_overview",
            None,
            Some(DiscoveryKind::Overview),
            move |server, adapter, policy| {
                let page = p.page.resolve(policy)?;
                let mut result = server.read_with_adapter(adapter, |c| {
                    cli::discovery::overview(
                        c,
                        &server.root,
                        Some(adapter),
                        p.aspects.as_deref(),
                        &page,
                    )
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
    #[tool(
        description = "Compact symbol record: signature, docstring, container, call counts, and at most five direct call previews per direction. Accepts 'symbol' as an alias for 'selector' and 'file_path' as an alias for 'path'. include_coverage adds paged resolution evidence. Source stays off unless show_source is true. Source continuation requires the previous page's generation."
    )]
    async fn code_map_inspect(&self, Parameters(p): Parameters<Inspect>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
                    if format == OutputFormat::Markdown && p.show_doc && page.detail == Detail::Full
                    {
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
    #[tool(
        description = "Read workspace text by path: lines, raw, search (pattern, optional regex/context), or Git delta (optional baseline). To read a complete indexed function, struct, or class, pass symbol: \"SymbolName\" (or selector: \"SymbolName\"); optional path restricts lookup to a file. Auto selects snippet for symbol or selector, search for pattern, otherwise lines. Core reads work without an index. Outline uses docs for Markdown and codemap AST declarations for code; snippet requires codemap and preserves digest-verified complete-symbol paging with source_offset/generation. Optional output_file exports returned content to a confined path. Sensitive patterns, symlinks, traversal and NUL binary input are rejected. Enabled graph/task signals add scope badges and copy-ready Hints."
    )]
    async fn acdd_read(&self, Parameters(mut p): Parameters<Snippet>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
            format,
            OperationKind::Navigation,
            "acdd_read",
            None,
            None,
            move |server, adapter, policy| {
                p.normalize(&server.root)?;
                let page = p.page.resolve(policy)?;
                let source = p.source(policy)?;
                let read = |conn: Option<&rusqlite::Connection>| {
                    crate::engine::reader::acdd_read_tool(
                        &server.root,
                        adapter,
                        &p.read,
                        &page,
                        &source,
                        conn,
                        server.state_db(),
                        Some(&server.db),
                    )
                };
                let structural = p.read.requires_index(adapter)?;
                if structural {
                    server.read_with_adapter(adapter, |conn| read(Some(conn)))
                } else if server.db.is_file()
                    && (adapter.modules.is_enabled("codemap") || adapter.modules.is_enabled("docs"))
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
    #[tool(
        description = "Search names, qualified names, indexed parameter and return types, decorators, and tokens. Use takes:Type or returns:Type to filter by a parameter or return type. A trailing * is a name prefix. exact=true matches an indexed name or qualname without full text. Hits include indexed signatures and inspect_selector arguments for code_map_inspect; previews appear automatically for at most three total hits or on request, capped at three hits, 20 lines, and 12 KiB combined."
    )]
    async fn code_map_search(&self, Parameters(p): Parameters<SearchSymbols>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(
        description = "Test relationships for a narrow selector. Inbound returns verified test cases connected by exact calls paths and keeps the bounded lexical fallback when no call path exists; outbound returns production dependencies reached through exact calls. Results include test roles and shortest call paths where graph evidence exists. Possible calls do not establish coverage. Broad scopes are rejected."
    )]
    async fn code_map_tests(&self, Parameters(p): Parameters<Tests>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(
        description = "Transitive reachability from one selector or a Git change view (staged, unstaged, or head). Provide exactly one of selector or changes. inbound (default) lists dependents; outbound lists dependencies. Each reached depth-one node includes the persisted edge endpoints, kind, path, line, direction, confidence, and occurrence count; full detail adds edge evidence. Change views bind every page to the graph, selected diff, and verified postimages, and report unmapped changes explicitly."
    )]
    async fn code_map_impact(&self, Parameters(p): Parameters<Impact>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(
        description = "Symbol summary plus paged incoming and outgoing edges of every kind. The embedded call summary shows counts and at most five direct call previews per direction. direction defaults to both. Source continuation requires the previous page's generation."
    )]
    async fn code_map_explain(&self, Parameters(p): Parameters<Explain>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
                    let result = symbols::explain_with_path_options(
                        c,
                        &server.root,
                        &p.selector,
                        p.path.as_deref(),
                        &symbols::ExplainOptions {
                            direction: p.direction.as_deref(),
                            show_doc: p.show_doc,
                            source: &source,
                            coverage,
                            page: &page,
                        },
                    )?;
                    Ok(result)
                })
            },
        )
        .await
    }
    #[tool(
        description = "Bounded graph operation or one read-only SELECT/WITH. untested lists production functions without direct indexed test callers; implementations lists indexed subclasses and implementors; duplicates lists duplicate functions/methods or identical files; dead_tails detects phantom_tested prod functions (only reachable from tests), dead_tails (0 prod & 0 test reachability), and dangling_calls (unresolved internal crate/super/self references). unwired is not a dead-code proof."
    )]
    async fn code_map_query(&self, Parameters(p): Parameters<Query>) -> CallToolResult {
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
        self.responding_blocking(
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
                            .or_else(|| {
                                looks_like_sql(&p.operation).then_some(p.operation.as_str())
                            }),
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
                    if is_inspect && format == OutputFormat::Markdown && page.detail == Detail::Full
                    {
                        symbols::attach_inspect_document_contents(c, &mut result)?;
                    }
                    Ok(result)
                })
            },
        )
        .await
    }
    #[tool(
        description = "Diagnostics for the workspace or one path, declared-public production symbols not reached by an indexed test case, stored syntax diagnostics, or static cycles. Exact-file resolution and external-import rows include an evidence-based reason when known and a verified source-line snippet; full detail retains resolver evidence. lint=true cannot combine with include_cycles=true."
    )]
    async fn code_map_analyze(&self, Parameters(p): Parameters<Analyze>) -> CallToolResult {
        let format = p.page.output_format();
        let operation = if p.target().as_deref().is_ok_and(looks_like_sql) {
            OperationKind::Sql
        } else {
            OperationKind::Navigation
        };
        self.responding_blocking(
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
    #[tool(
        description = "Indexed removal check for one symbol or file. Returns incoming dependency edges, candidate-scoped unresolved references and parse errors, and safe_to_remove. That verdict excludes workspace-wide diagnostics and is not a runtime-safety proof. More than 10000 nodes is rejected."
    )]
    async fn code_map_prove_removal(&self, Parameters(p): Parameters<Selector>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(
        description = "Tree-sitter structural search over admitted source. Language profiles declare complete_syntax, declaration_without_body, fragment_probe, and attribute capabilities. $NAME captures one syntax node; $$$SEQ captures a syntax-node sequence. language must be a compiled profile id (e.g. 'python', 'rust', 'typescript') or omitted if path has a recognized file extension or resolves to a single-language directory. Call acdd_guide with topic=ast for the profile capability matrix and examples."
    )]
    async fn ast_grep_search(&self, Parameters(p): Parameters<Ast>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
            format,
            OperationKind::Navigation,
            "ast_grep_search",
            None,
            Some(DiscoveryKind::Ast),
            move |server, adapter, policy| {
                server.read_with_adapter(adapter, |conn| {
                    let pattern = p.pattern()?;
                    let language = cli::ast::resolve_ast_language(
                        conn,
                        &server.root,
                        adapter,
                        p.language.as_deref(),
                        p.path.as_deref(),
                    )?;
                    let mut result = cli::ast::search_paged_with_adapter(
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
    #[tool(
        description = "Search indexed Markdown sections with natural-language terms: common English stop words are ignored, any remaining term can match, and section-title matches receive three times the BM25 weight of body text. Returns paths and section anchors for get_doc. include_excerpt adds a short match-centered preview."
    )]
    async fn search_docs(&self, Parameters(p): Parameters<SearchDocs>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(
        description = "Read one indexed Markdown document. The default detail is full text. detail=compact returns a heading outline. A missing path or section is an error."
    )]
    async fn get_doc(&self, Parameters(p): Parameters<GetDoc>) -> CallToolResult {
        let format = p.page.output_format();
        self.responding_blocking(
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
    #[tool(description = "Built-in guide. The default topic is query.")]
    async fn acdd_guide(&self, Parameters(p): Parameters<Guide>) -> CallToolResult {
        let format = p.format.unwrap_or_default();
        let topic = p
            .topic
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("query")
            .to_owned();
        if topic == "init" {
            let root = self.root.clone();
            let force = p.force;
            return match tokio::task::spawn_blocking(move || cli::guide::run(&root, &topic, force))
                .await
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
        self.responding_blocking(
            format,
            OperationKind::Guide,
            "acdd_guide",
            None,
            None,
            move |server, adapter, _| {
                cli::guide::run_with_adapter(&server.root, &topic, p.force, adapter)
            },
        )
        .await
    }
}

impl Server {
    fn invalid_format_policy(
        &self,
        request: &rmcp::model::CallToolRequestParams,
    ) -> ResponsePolicy {
        let primary_adapter = self.load_adapter(&self.root);
        let fallback_policy = primary_adapter
            .as_ref()
            .map(|adapter| adapter.response.clone())
            .unwrap_or_default();
        let requested_worktree = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("worktree"))
            .and_then(Value::as_str);
        let Ok((target_root, _)) = self.resolve_target_root_and_db_with_adapter(
            requested_worktree,
            primary_adapter.as_ref().ok(),
        ) else {
            return fallback_policy;
        };
        if target_root == self.root {
            return fallback_policy;
        }

        self.load_adapter(&target_root)
            .map(|adapter| adapter.response)
            .unwrap_or(fallback_policy)
    }

    fn active_tool_count(&self) -> usize {
        let adapter = self.load_adapter(&self.root).unwrap_or_default();
        let static_tools = Self::tool_router().list_all();
        let active_static = acdd_core::subsystems::filter_subsystem_tools(
            static_tools.iter().map(|tool| tool.name.as_ref()),
            &adapter.modules,
        )
        .len();
        let active_plugins = self
            .domain_providers(&adapter)
            .map(|providers| providers.plugin_tools().len())
            .unwrap_or(0);
        active_static + active_plugins
    }
}

fn operation_kind(name: &str, arguments: Option<&serde_json::Map<String, Value>>) -> OperationKind {
    match name {
        "acdd_state" | "acdd_claim" | "acdd_verify" | "acdd_submit" | "acdd_blackboard" => {
            OperationKind::Task
        }
        "acdd_guide" => OperationKind::Guide,
        "code_map_query"
            if arguments.is_some_and(|arguments| {
                let operation = arguments.get("operation").and_then(Value::as_str);
                operation == Some("sql")
                    || arguments.get("sql").is_some()
                    || operation.is_some_and(looks_like_sql)
                    || (operation.unwrap_or("").trim().is_empty()
                        && arguments
                            .get("query")
                            .and_then(Value::as_str)
                            .is_some_and(looks_like_sql))
            }) =>
        {
            OperationKind::Sql
        }
        "code_map_analyze"
            if arguments
                .and_then(|arguments| {
                    arguments
                        .get("target")
                        .or(arguments.get("path"))
                        .or(arguments.get("selector"))
                })
                .and_then(Value::as_str)
                .is_some_and(looks_like_sql) =>
        {
            OperationKind::Sql
        }
        _ => OperationKind::Navigation,
    }
}

fn looks_like_sql(value: &str) -> bool {
    value.split_whitespace().next().is_some_and(|keyword| {
        keyword.eq_ignore_ascii_case("select") || keyword.eq_ignore_ascii_case("with")
    })
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        let active_count = self.active_tool_count();
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

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        let adapter = self.load_adapter(&self.root).ok()?;
        if acdd_core::subsystems::disabled_tool_error(name, &adapter.modules).is_some() {
            return None;
        }
        if let Some(tool) = Self::tool_router().get(name).cloned() {
            return Some(tool);
        }
        let providers = self.domain_providers(&adapter).ok()?;
        providers
            .plugin_tools()
            .into_iter()
            .find(|(full_name, _)| full_name == name)
            .and_then(|(full_name, def)| plugin_tool_to_rmcp(full_name, def).ok())
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let server = self.clone();
        let tools = tokio::task::spawn_blocking(move || -> Result<Vec<Tool>, rmcp::ErrorData> {
            let adapter = server.load_adapter(&server.root).map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("workspace module configuration is invalid: {error}"),
                    None,
                )
            })?;
            let mut tools = Self::tool_router().list_all();
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

    async fn call_tool(
        &self,
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
                    let server = self.clone();
                    let req_clone = request.clone();
                    let policy = tokio::task::spawn_blocking(move || {
                        server.invalid_format_policy(&req_clone)
                    })
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
        let server_prep = self.clone();
        let tool_name_prep = tool_name.to_string();
        let args_prep = request.arguments.clone();

        struct PreparedCall {
            target_root: PathBuf,
            target_db: PathBuf,
            adapter: super::server::RequestAdapter,
            loaded_adapter: Option<std::sync::Arc<scanner::Adapter>>,
            providers: Option<std::sync::Arc<acdd_core::ports::CompositeDomainProvider>>,
            span_ctx: Option<crate::state_bridge::WorktreeSpanContext>,
            early_response: Option<CallToolResult>,
        }

        let prep = tokio::task::spawn_blocking(move || {
            let primary_adapter = server_prep.load_adapter(&server_prep.root);
            let (target_root, target_db) = match server_prep
                .resolve_target_root_and_db_with_adapter(
                    requested_worktree.as_deref(),
                    primary_adapter.as_ref().ok(),
                ) {
                Ok(pair) => pair,
                Err(error) => {
                    return PreparedCall {
                        target_root: server_prep.root.clone(),
                        target_db: server_prep.db.clone(),
                        adapter: super::server::RequestAdapter::Failed {
                            kind: std::io::ErrorKind::Other,
                            message: error.to_string(),
                        },
                        loaded_adapter: None,
                        providers: None,
                        span_ctx: None,
                        early_response: Some(response::result_for(
                            Err(error),
                            &ResponsePolicy::default(),
                            output_format,
                            operation,
                            &tool_name_prep,
                            None,
                        )),
                    };
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
                        super::server::RequestAdapter::Loaded(std::sync::Arc::clone(&arc)),
                        Some(arc),
                    )
                }
                Err(error) => (
                    super::server::RequestAdapter::Failed {
                        kind: error.kind(),
                        message: error.to_string(),
                    },
                    None,
                ),
            };
            if let Some(ref arc) = loaded_adapter {
                if let Some(message) =
                    acdd_core::subsystems::disabled_tool_error(&tool_name_prep, &arc.modules)
                {
                    let early_response = Some(response::result_for(
                        Err(StateError::new(ErrorCode::FeatureDisabled, message).into()),
                        &arc.response,
                        output_format,
                        operation,
                        &tool_name_prep,
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
            let is_static_route = Self::tool_router().has_route(&tool_name_prep);
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
                    .find(|(n, _)| n == &tool_name_prep)
                {
                    if let Some((rejected, allowed)) =
                        rejected_plugin_fields(&def, args_prep.as_ref())
                    {
                        let allowed_refs: Vec<&str> =
                            allowed.iter().map(String::as_str).collect();
                        return PreparedCall {
                            target_root,
                            target_db,
                            adapter,
                            loaded_adapter,
                            providers: Some(std::sync::Arc::clone(providers)),
                            span_ctx: None,
                            early_response: Some(rejected_arguments_error(
                                &tool_name_prep,
                                &rejected,
                                &allowed_refs,
                            )),
                        };
                    }
                }
            }

            let span_ctx = loaded_adapter.as_ref().and_then(|a| {
                if a.modules.is_enabled("state") {
                    crate::state_bridge::resolve_worktree_span_context(
                        &ws_root,
                        &target_root,
                        server_prep.state_db(),
                        a,
                    )
                    .ok()
                    .flatten()
                } else {
                    None
                }
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
            .map_or(acdd_core::config::DEFAULT_RETAIN_TRACES, |a| a.retain_traces);
        let span_state_db = prep.loaded_adapter.as_ref().and_then(|a| {
            let ws_root = primary_workspace_root_for(&self.root, &prep.target_root);
            crate::state_bridge::resolve_effective_state_db(&ws_root, self.state_db(), a).ok()
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
        let policy = prep
            .loaded_adapter
            .as_ref()
            .map_or(&default_policy, |a| &a.response);

        let is_static_route = Self::tool_router().has_route(&tool_name);
        let is_plugin_route = !is_static_route
            && prep.providers.as_ref().is_some_and(|p| {
                p.has_enabled_plugin_tool(&tool_name)
                    || matches!(
                        p.validate_plugin_tool_name(&tool_name),
                        Err(ref err) if err.message.contains("disabled")
                    )
            });

        let result: Result<rmcp::model::CallToolResponse, rmcp::ErrorData> = if is_plugin_route {
            let providers = prep
                .providers
                .expect("providers checked in is_plugin_route");
            let server_root = self.root.clone();
            let target_root = prep.target_root.clone();
            let span_ctx = prep.span_ctx.clone();
            let tool_name_owned = tool_name.to_string();
            let raw_args = request.arguments.clone().unwrap_or_default();
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
                            crate::format::render_plugin_tool_markdown(&tool_name, &output)
                        }
                        OutputFormat::Json => {
                            let val = match output {
                                DomainToolOutput::Json(v) => v,
                                DomainToolOutput::Markdown(m) => {
                                    serde_json::json!({ "markdown": m })
                                }
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
                    &tool_name,
                    None,
                )
                .into()),
                Err(join_err) => Err(rmcp::ErrorData::internal_error(
                    format!("plugin tool blocking task failed: {join_err}"),
                    None,
                )),
            }
        } else {
            let request_server = self.with_root_and_db(prep.target_root, prep.target_db, prep.adapter);
            let call =
                rmcp::handler::server::tool::ToolCallContext::new(&request_server, request, context);
            Self::tool_router().call(call).await
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
                        serde_json::to_value(res)
                            .unwrap_or(serde_json::json!({"status": "complete"}))
                    })
                }
                Ok(resp) => serde_json::json!({ "raw": format!("{resp:?}") }),
                Err(err) => serde_json::json!({ "error": err.to_string() }),
            };
            span_guard.set_debug_mcp(&self.root, &tool_name, &tool_args, &output_val);
        }
        span_guard.finish_async().await;
        final_response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acdd_core::ports::{
        BUILTIN_MCP_TOOLS, DomainActionReq, DomainActionResult, DomainCapabilities,
        DomainCheckOutcome, DomainCheckReq, DomainContextReq, DomainProvider,
    };
    use acdd_core::schema::SchemaDef;
    use rmcp::model::{CallToolRequestParams, CallToolResponse, NumberOrString};
    use rmcp::service::{serve_directly, Peer, RequestContext, RoleServer};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    struct MockHttpProvider;
    impl DomainProvider for MockHttpProvider {
        fn domain_id(&self) -> &str {
            "http"
        }
        fn capabilities(&self) -> DomainCapabilities {
            let mut schema = SchemaDef::object();
            schema.properties.insert("url".into(), SchemaDef::string());
            schema
                .properties
                .insert("oversize".into(), SchemaDef::boolean());
            schema.required = vec!["url".into()];
            schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));
            DomainCapabilities {
                tools: vec![DomainToolDef {
                    name: "probe".into(),
                    description: "Probe an HTTP endpoint".into(),
                    input_schema: schema,
                    read_only: true,
                }],
                ..DomainCapabilities::default()
            }
        }
        fn execute_action(
            &self,
            _action: &str,
            _params: &Value,
            _idempotency_key: &str,
            _req: &DomainActionReq,
        ) -> anyhow::Result<DomainActionResult> {
            unreachable!()
        }
        fn run_check(
            &self,
            _check: &str,
            _params: &Value,
            _req: &DomainCheckReq,
        ) -> anyhow::Result<DomainCheckOutcome> {
            unreachable!()
        }
        fn provide_context(&self, _lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
            unreachable!()
        }
        fn call_tool(
            &self,
            tool: &str,
            args: &Value,
            _req: &DomainToolReq,
        ) -> anyhow::Result<DomainToolOutput> {
            anyhow::ensure!(tool == "probe");
            let url = args["url"].as_str().unwrap_or("");
            if args["oversize"].as_bool() == Some(true) {
                return Ok(DomainToolOutput::Markdown("x".repeat(200_000)));
            }
            if args["url"] == "json_out" {
                return Ok(DomainToolOutput::Json(
                    serde_json::json!({"status": 200, "url": url}),
                ));
            }
            Ok(DomainToolOutput::Markdown(format!("# Probe\n\nOK: {url}\n")))
        }
    }

    async fn make_peer_ctx(id: i64) -> (
        rmcp::service::RunningService<RoleServer, Server>,
        tokio::io::DuplexStream,
        RequestContext<RoleServer>,
    ) {
        let (client, transport) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(transport);
        let dummy = Server::new(PathBuf::new(), PathBuf::new());
        let running = serve_directly(dummy, (reader, response::BoundedWriter::new(writer)), None);
        let peer: Peer<RoleServer> = running.peer().clone();
        let ctx = RequestContext::new(NumberOrString::Number(id), peer);
        (running, client, ctx)
    }

    async fn call_raw(
        server: &Server,
        name: &str,
        args: Value,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let req = CallToolRequestParams::new(name.to_owned())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let CallToolResponse::Complete(res) = server.call_tool(req, ctx).await.unwrap() else {
            panic!("expected complete CallToolResponse");
        };
        res
    }

    #[tokio::test]
    async fn mcp_19_builtin_tools_and_plugin_tool_contract() {
        let ws = acdd_core::fs::ScopedTempDir::create("acdd-mcp-tools-19tools").unwrap();
        std::fs::write(
            ws.path().join("acdd.yaml"),
            "roots: [.]\nmodules:\n  domains:\n    http: true\n",
        )
        .unwrap();
        let server = Server::new(
            ws.path().to_path_buf(),
            ws.path().join(".acdd/code-map.sqlite"),
        )
        .with_domain_providers(vec![Arc::new(MockHttpProvider)]);

        let info = server.get_info();
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        let instructions = info.instructions.as_deref().unwrap_or("");
        assert!(
            instructions.contains("20 tools"),
            "expected dynamic 19+1=20 tools in instructions: {instructions}"
        );

        let (_running, _client, ctx) = make_peer_ctx(1).await;
        let listed = server.list_tools(None, ctx.clone()).await.unwrap();
        let names: BTreeSet<String> = listed
            .tools
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        let mut expected: BTreeSet<String> =
            BUILTIN_MCP_TOOLS.iter().map(|s| (*s).to_owned()).collect();
        expected.insert("http_probe".to_owned());
        assert_eq!(names, expected);
        for legacy in [
            "task_list",
            "task_claim",
            "task_submit",
            "task_manage",
            "task_blackboard",
            "session_checkpoint",
            "forge_guide",
        ] {
            assert!(!names.contains(legacy), "legacy tool {legacy} leaked");
        }

        let plugin_tool = listed
            .tools
            .iter()
            .find(|t| t.name.as_ref() == "http_probe")
            .unwrap();
        assert_eq!(
            plugin_tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            Some(true)
        );
        assert!(plugin_tool.input_schema["properties"].get("url").is_some());
        assert!(server.get_tool("http_probe").is_some());

        // Typo rejected with Levenshtein suggestion
        let (_, _, c1) = make_peer_ctx(2).await;
        let typo = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"ur": "https://example.com"}),
            c1,
        )
        .await;
        assert_eq!(typo.is_error, Some(true));
        let typo_text = typo.content[0].as_text().unwrap().text.as_str();
        assert!(typo_text.contains("rejected_arguments") && typo_text.contains("url"));

        // Invalid schema type fails closed
        let (_, _, c2) = make_peer_ctx(3).await;
        let bad_type = call_raw(&server, "http_probe", serde_json::json!({"url": 123}), c2).await;
        assert_eq!(bad_type.is_error, Some(true));

        // Markdown & JSON output parity
        let (_, _, c3) = make_peer_ctx(4).await;
        let md = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "https://example.com", "format": "markdown"}),
            c3,
        )
        .await;
        assert_ne!(md.is_error, Some(true));
        assert!(md.content[0].as_text().unwrap().text.contains("OK: https://example.com"));

        let (_, _, c4) = make_peer_ctx(5).await;
        let js = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "json_out", "format": "json"}),
            c4,
        )
        .await;
        assert_ne!(js.is_error, Some(true));
        let parsed: Value = serde_json::from_str(js.content[0].as_text().unwrap().text.as_str()).unwrap();
        assert_eq!(parsed["status"], 200);

        // Oversized plugin output clamped by response::enforce
        let (_, _, c5) = make_peer_ctx(6).await;
        let big = call_raw(
            &server,
            "http_probe",
            serde_json::json!({"url": "https://example.com", "oversize": true}),
            c5,
        )
        .await;
        assert_eq!(big.is_error, Some(true));
        assert!(big.content[0]
            .as_text()
            .unwrap()
            .text
            .contains("exceeds the response byte limit"));
    }

    #[tokio::test]
    async fn mcp_modules_isolation_and_readonly_state_queries() {
        let ws = acdd_core::fs::ScopedTempDir::create("acdd-mcp-tools-modules").unwrap();
        std::fs::write(
            ws.path().join("acdd.yaml"),
            "roots: [.]\nmodules:\n  domains:\n    http: true\n",
        )
        .unwrap();
        let server = Server::new(
            ws.path().to_path_buf(),
            ws.path().join(".acdd/code-map.sqlite"),
        );

        // Read-only state queries & blackboard read must NOT create state.sqlite
        for (idx, (tool, args)) in [
            ("acdd_state", serde_json::json!({"action": "status", "format": "json"})),
            ("acdd_state", serde_json::json!({"action": "list", "format": "json"})),
            ("acdd_state", serde_json::json!({"action": "next", "format": "json"})),
            ("acdd_state", serde_json::json!({"action": "observability", "format": "json"})),
            ("acdd_state", serde_json::json!({"action": "context", "format": "json"})),
            ("acdd_blackboard", serde_json::json!({"action": "read", "format": "json"})),
        ]
        .into_iter()
        .enumerate()
        {
            let (_, _, ctx) = make_peer_ctx(10 + idx as i64).await;
            let _ = call_raw(&server, tool, args, ctx).await;
            assert!(
                !ws.path().join(".acdd/state.sqlite").exists(),
                "read-only query {tool} must not create state.sqlite"
            );
        }

        // Physical module gating matrix: state=false, code=false, docs=false, domains.http=false
        struct ModuleCase {
            yaml: &'static str,
            hidden: &'static [&'static str],
            disabled_call: (&'static str, Value, &'static str),
        }
        let cases = [
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [state]\n",
                hidden: &[
                    "acdd_state",
                    "acdd_claim",
                    "acdd_verify",
                    "acdd_submit",
                    "acdd_blackboard",
                ],
                disabled_call: (
                    "acdd_state",
                    serde_json::json!({"action": "status"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [code]\n",
                hidden: &["code_map_overview", "code_map_search", "ast_grep_search"],
                disabled_call: (
                    "code_map_search",
                    serde_json::json!({"pattern": "main"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  disabled: [docs]\n",
                hidden: &["get_doc", "search_docs"],
                disabled_call: (
                    "search_docs",
                    serde_json::json!({"query": "readme"}),
                    "FEATURE_DISABLED",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\nmodules:\n  domains:\n    http: false\n",
                hidden: &["http_probe"],
                disabled_call: (
                    "http_probe",
                    serde_json::json!({"url": "https://example.com"}),
                    "DOMAIN_CAPABILITY_UNAVAILABLE",
                ),
            },
            ModuleCase {
                yaml: "roots: [.]\n",
                hidden: &["http_probe"],
                disabled_call: (
                    "http_probe",
                    serde_json::json!({"url": "https://example.com"}),
                    "DOMAIN_CAPABILITY_UNAVAILABLE",
                ),
            },
        ];

        for (idx, case) in cases.iter().enumerate() {
            let sub_ws = acdd_core::fs::ScopedTempDir::create(&format!("acdd-mcp-tools-mod-{idx}")).unwrap();
            std::fs::write(sub_ws.path().join("acdd.yaml"), case.yaml).unwrap();
            let s = Server::new(
                sub_ws.path().to_path_buf(),
                sub_ws.path().join(".acdd/code-map.sqlite"),
            )
            .with_domain_providers(vec![Arc::new(MockHttpProvider)]);
            let (_, _, ctx) = make_peer_ctx(100 + idx as i64).await;
            let listed = s.list_tools(None, ctx).await.unwrap();
            let names: BTreeSet<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();
            for tool in case.hidden {
                assert!(!names.contains(tool), "{tool} should be hidden for {}", case.yaml);
            }
            let (_, _, c2) = make_peer_ctx(200 + idx as i64).await;
            let res = call_raw(&s, case.disabled_call.0, case.disabled_call.1.clone(), c2).await;
            assert_eq!(res.is_error, Some(true));
            let text = res.content[0].as_text().unwrap().text.as_str();
            assert!(
                text.contains(case.disabled_call.2),
                "expected {} in {text}",
                case.disabled_call.2
            );
            assert!(!sub_ws.path().join(".acdd/state.sqlite").exists());
        }
    }
}
