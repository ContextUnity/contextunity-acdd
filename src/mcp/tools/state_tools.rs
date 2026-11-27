use super::super::{
    response,
    server::{primary_workspace_root_for, Server},
};
use crate::format::{OperationKind, OutputFormat};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::response::ResponsePolicy;
use acdd_index::scanner;
use acdd_spec::SubtaskStatus;
use acdd_state::types::{BlackboardStatus, BlackboardTopic, RunStatus};
use acdd_state::usecases::{
    BlackboardPostReq, BlackboardReadReq, ClaimReq, ClearReq, ContextReq, ExtendScopeReq, ListReq,
    NextReq, ObservabilityReq, ReopenReq, RunReq, ShowReq, SnapshotReq, StartReq, StateCommand,
    StateCommandOutput, StateQuery, StateQueryOutput, StatusReq, SubmitReq, SubtaskReq, SyncReq,
    VerifyReq,
};
use rmcp::model::CallToolResult;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::inputs::{
    BlackboardInput, ClaimInput, McpBlackboardAction, McpStateAction, StateInput, SubmitInput,
    VerifyInput, WorktreeInput,
};

struct StateCallContext<'a> {
    server: &'a Server,
    workspace_root: &'a Path,
    target_root: &'a Path,
    state_db: Option<&'a Path>,
    code_db: Option<&'a Path>,
    adapter: &'a scanner::Adapter,
    providers: std::sync::Arc<acdd_core::ports::CompositeDomainProvider>,
}

impl StateCallContext<'_> {
    fn execute_query(&self, query: StateQuery) -> Result<StateQueryOutput, AcddError> {
        crate::state_bridge::execute_state_query(
            self.workspace_root,
            self.target_root,
            self.state_db,
            self.code_db,
            self.adapter,
            std::sync::Arc::clone(&self.providers),
            query,
        )
    }

    fn execute_command(&self, cmd: StateCommand) -> Result<StateCommandOutput, AcddError> {
        crate::state_bridge::execute_state_command(
            self.workspace_root,
            self.target_root,
            self.state_db,
            self.code_db,
            self.adapter,
            std::sync::Arc::clone(&self.providers),
            cmd,
        )
    }
}

fn require_present<T>(value: Option<T>, field: &str, action: &str) -> Result<T, AcddError> {
    value.ok_or_else(|| {
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("'{field}' is required for action '{action}'"),
        )
    })
}

fn require_field(value: Option<String>, field: &str, action: &str) -> Result<String, AcddError> {
    require_present(value.filter(|s| !s.trim().is_empty()), field, action)
}

fn build_state_command_from_input(
    p: StateInput,
    explicit_worktree: Option<PathBuf>,
) -> std::result::Result<StateCommand, AcddError> {
    let run = p.run;
    match p.action {
        McpStateAction::Snapshot => Ok(StateCommand::Snapshot(SnapshotReq {
            run,
            action: if p.delete == Some(true) {
                acdd_state::usecases::SnapshotAction::Clear
            } else {
                require_present(p.snapshot_action, "snapshot_action", "snapshot")?
            },
            task_ref: p.task_ref,
            all: p.all.unwrap_or(false),
            revision: p.rev,
            name: p.name,
            worktree: explicit_worktree,
            older_than: p.older_than,
        })),
        McpStateAction::Start => {
            let create_wt = p.create_worktree.unwrap_or_else(|| {
                p.worktree
                    .as_ref()
                    .and_then(WorktreeInput::as_flag)
                    .unwrap_or(false)
            });
            let in_place = p.in_place.unwrap_or(false);
            let worktree_path = p
                .worktree_path
                .or(if in_place { None } else { explicit_worktree });
            Ok(StateCommand::Start(StartReq {
                spec_path: p.spec_path,
                goal: p.goal,
                run_name: p.run_name,
                workflow: p.workflow,
                scope: p.scope.unwrap_or_default(),
                target_base: p.target_base,
                worktree: create_wt,
                in_place,
                worktree_path,
            }))
        }
        McpStateAction::ExtendScope => {
            let node = require_field(p.node, "node", "extend_scope")?;
            let paths = p.paths.unwrap_or_default();
            if paths.is_empty() {
                return Err(AcddError::new(
                    ErrorCode::WorkflowSchemaInvalid,
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
            rebind_workflow: p.rebind_workflow.unwrap_or(false),
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

fn build_state_query_from_input(p: StateInput) -> std::result::Result<StateQuery, AcddError> {
    let run = p.run;
    match p.action {
        McpStateAction::Status => Ok(StateQuery::Status(StatusReq { run })),
        McpStateAction::List => {
            let status = p.status.as_deref().map(RunStatus::from_str).transpose()?;
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

async fn run_state_blocking(
    server: &Server,
    output_format: OutputFormat,
    title: &'static str,
    op: impl FnOnce(StateCallContext<'_>) -> anyhow::Result<(String, Value)> + Send + 'static,
) -> CallToolResult {
    let server = server.clone();
    match tokio::task::spawn_blocking(move || {
        let adapter = match server.adapter_snapshot() {
            Ok(adapter) => adapter,
            Err(err) => {
                return response::state_result(
                    Err(err),
                    &ResponsePolicy::default(),
                    output_format,
                    title,
                );
            }
        };
        if let Some(message) = acdd_core::subsystems::disabled_tool_error(title, &adapter.modules) {
            return response::state_result(
                Err(AcddError::new(ErrorCode::FeatureDisabled, message).into()),
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

pub(super) async fn acdd_state(server: &Server, p: StateInput) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    let explicit_wt_nonempty = p
        .worktree
        .as_ref()
        .and_then(WorktreeInput::as_selector)
        .is_some_and(|s| !s.trim().is_empty());
    run_state_blocking(server, format, "acdd_state", move |ctx| {
        if p.action.is_read_only() {
            let query = build_state_query_from_input(p)?;
            let out = ctx.execute_query(query)?;
            let md = crate::format::render_state_query_markdown(&out, "acdd_state", false);
            let val = out.into_untagged_json()?;
            Ok((md, val))
        } else {
            let explicit = explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf());
            let cmd = build_state_command_from_input(p, explicit)?;
            let out = ctx.execute_command(cmd)?;
            let md = crate::format::render_state_command_markdown(&out, "acdd_state");
            let val = out.into_untagged_json()?;
            Ok((md, val))
        }
    })
    .await
}

pub(super) async fn acdd_claim(server: &Server, p: ClaimInput) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
    run_state_blocking(server, format, "acdd_claim", move |ctx| {
        let req = ClaimReq {
            run: p.run,
            node: p.node,
            worker_id: p.worker_id,
            session_fingerprint: ctx.server.session_fingerprint.clone(),
            force: p.force,
            steal_remote: p.steal_remote,
            worktree: explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf()),
            detail: p.detail.unwrap_or_default(),
        };
        let out = ctx.execute_command(StateCommand::Claim(req))?;
        let StateCommandOutput::Claim(claim) = out else {
            unreachable!("Claim command always returns Claim output");
        };
        let md = crate::format::render_claim_markdown(&claim);
        let val = serde_json::to_value(&claim)?;
        Ok((md, val))
    })
    .await
}

pub(super) async fn acdd_verify(server: &Server, p: VerifyInput) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
    run_state_blocking(server, format, "acdd_verify", move |ctx| {
        let req = VerifyReq {
            run: p.run,
            node: p.node,
            worker_id: p.worker_id,
            test_target: p.test_target,
            claim_revision: p.claim_revision,
            worktree: explicit_wt_nonempty.then(|| ctx.target_root.to_path_buf()),
        };
        let out = ctx.execute_command(StateCommand::Verify(req))?;
        let StateCommandOutput::Verify(verify) = out else {
            unreachable!("Verify command always returns Verify output");
        };
        let md = crate::format::render_verify_markdown(&verify);
        let val = serde_json::to_value(&verify)?;
        Ok((md, val))
    })
    .await
}

pub(super) async fn acdd_submit(server: &Server, p: SubmitInput) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    let explicit_wt_nonempty = p.worktree.as_deref().is_some_and(|s| !s.trim().is_empty());
    run_state_blocking(server, format, "acdd_submit", move |ctx| {
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
        let out = ctx.execute_command(StateCommand::Submit(Box::new(req)))?;
        let StateCommandOutput::Submit(submit) = out else {
            unreachable!("Submit command always returns Submit output");
        };
        let md = crate::format::render_submit_markdown(&submit);
        let val = serde_json::to_value(&submit)?;
        Ok((md, val))
    })
    .await
}

pub(super) async fn acdd_blackboard(server: &Server, p: BlackboardInput) -> CallToolResult {
    let format = p.format.unwrap_or_default();
    run_state_blocking(server, format, "acdd_blackboard", move |ctx| {
        match p.action {
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
                let out = ctx.execute_query(StateQuery::BlackboardRead(req))?;
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
                let out = ctx.execute_command(StateCommand::BlackboardPost(req))?;
                let StateCommandOutput::BlackboardPost(resp) = out else {
                    unreachable!("BlackboardPost command always returns BlackboardPost output");
                };
                let val = serde_json::to_value(&resp)?;
                let md = crate::format::render_blackboard_markdown(&val, "acdd_blackboard");
                Ok((md, val))
            }
        }
    })
    .await
}

pub(super) fn operation_kind(
    name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> OperationKind {
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
                    || operation.is_some_and(super::code_tools::looks_like_sql)
                    || (operation.unwrap_or("").trim().is_empty()
                        && arguments
                            .get("query")
                            .and_then(Value::as_str)
                            .is_some_and(super::code_tools::looks_like_sql))
            }) =>
        {
            OperationKind::Sql
        }
        "code_map_analyze"
            if arguments.is_some_and(|arguments| {
                arguments.get("mode").is_none()
                    && arguments
                        .get("target")
                        .or(arguments.get("path"))
                        .or(arguments.get("selector"))
                        .and_then(Value::as_str)
                        .is_some_and(super::code_tools::looks_like_sql)
            }) =>
        {
            OperationKind::Sql
        }
        _ => OperationKind::Navigation,
    }
}
