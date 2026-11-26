use super::args::Command;
use super::paths;
use crate::{
    format::{self, OutputFormat},
    state_bridge::{
        build_domain_providers, execute_state_command_with_config, execute_state_query,
    },
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_state::types::{
    BlackboardStatus, BlackboardTopic, RejectionFindings, ReviewEvidence, RunStatus, SubmitAction,
    SubtaskStatus,
};
use acdd_state::usecases::{
    BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, CallPluginToolResp, ClaimReq,
    ClearReq, ContextReq, DraftReq, ExtendScopeReq, ListReq, NextReq, ReopenReq, RoadmapReq,
    RunReq, ShowReq, SnapshotAction, SnapshotReq, StartReq, StateCommand, StateCommandOutput,
    StateQuery, StatusReq, SubmitReq, SubtaskReq, SyncReq, VerifyReq,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;
use std::str::FromStr;

pub(super) struct CliStateCtx<'a> {
    pub root: &'a Path,
    pub checkout_root: &'a Path,
    pub explicit_config: bool,
    pub state_db: Option<&'a Path>,
    pub code_db: Option<&'a Path>,
    pub adapter: &'a acdd_core::config::Adapter,
    pub output_format: OutputFormat,
    pub title: &'a str,
}

impl CliStateCtx<'_> {
    fn command(&self, cmd: StateCommand) -> Result<(Value, Option<String>)> {
        let worktree = match &cmd {
            StateCommand::Start(_) | StateCommand::Sync(_) => Some(self.checkout_root),
            StateCommand::Claim(req) => req.worktree.as_deref(),
            StateCommand::Verify(req) => req.worktree.as_deref(),
            StateCommand::Submit(req) => req.worktree.as_deref(),
            StateCommand::CallPluginTool(req) => req.worktree.as_deref(),
            StateCommand::Snapshot(req) => req.worktree.as_deref(),
            _ => None,
        }
        .map(Path::to_path_buf);
        let providers = build_domain_providers(self.root, self.adapter, self.code_db, Vec::new())?;
        let res = execute_state_command_with_config(
            self.root,
            worktree.as_deref().unwrap_or(self.root),
            self.state_db,
            self.code_db,
            self.adapter,
            providers,
            cmd,
            self.explicit_config,
        )?;
        render_cli_state_output(&res, self.output_format, self.adapter.debug, || {
            format::render_state_command_markdown(&res, self.title)
        })
    }

    fn query(&self, worktree: &Path, query: StateQuery) -> Result<(Value, Option<String>)> {
        let providers = build_domain_providers(self.root, self.adapter, self.code_db, Vec::new())?;
        let roadmap_detail = matches!(
            &query,
            StateQuery::Roadmap(req) if req.spec_id.is_some()
        );
        let res = execute_state_query(
            self.root,
            worktree,
            self.state_db,
            self.code_db,
            self.adapter,
            providers,
            query,
        )?;
        render_cli_state_output(&res, self.output_format, self.adapter.debug, || {
            format::render_state_query_markdown(&res, self.title, roadmap_detail)
        })
    }
}

fn render_cli_state_output<T: serde::Serialize>(
    res: &T,
    output_format: OutputFormat,
    debug: bool,
    render_md: impl FnOnce() -> String,
) -> Result<(Value, Option<String>)> {
    let md = (output_format == OutputFormat::Markdown).then(render_md);
    let val = if output_format == OutputFormat::Json || debug {
        serde_json::to_value(res)?
    } else {
        Value::Null
    };
    Ok((val, md))
}

fn parse_json_or_at_file(root: &Path, raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    let payload = if let Some(rel) = trimmed.strip_prefix('@') {
        let confined = paths::confined_file(root, Path::new(rel))?;
        std::fs::read_to_string(&confined)
            .with_context(|| format!("reading JSON payload '{}'", confined.display()))?
    } else {
        trimmed.to_owned()
    };
    serde_json::from_str(&payload).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("invalid JSON payload: {err}"),
        )
        .into()
    })
}

fn state_err_invalid(msg: impl Into<String>) -> anyhow::Error {
    StateError::new(ErrorCode::ProfileSchemaInvalid, msg).into()
}

pub(super) fn dispatch(
    state_ctx: &CliStateCtx<'_>,
    explicit_profile_ref: Option<String>,
    cli_profile: Option<&Path>,
    command: Command,
) -> Result<(Value, Option<String>)> {
    let root = state_ctx.root;
    match command {
        Command::Start {
            spec,
            goal,
            name,
            scope,
            target_base,
            worktree,
            in_place,
            worktree_path,
        } => {
            let run_name = name
                .as_deref()
                .map(RunName::new)
                .transpose()
                .map_err(anyhow::Error::from)?;
            let worktree_path = match worktree_path {
                Some(path) if path.is_relative() => Some(acdd_core::fs::checked_child(
                    &std::env::current_dir()?,
                    &path,
                )?),
                path => path,
            };
            state_ctx.command(StateCommand::Start(StartReq {
                spec_path: spec,
                goal,
                run_name,
                profile: explicit_profile_ref
                    .or_else(|| cli_profile.map(|p| p.to_string_lossy().into_owned())),
                scope,
                target_base,
                worktree,
                in_place,
                worktree_path,
            }))
        }
        Command::Draft {
            collection,
            slug,
            prefix,
            title: spec_title,
        } => state_ctx.command(StateCommand::Draft(DraftReq {
            collection,
            slug,
            prefix,
            title: spec_title,
            profile: explicit_profile_ref,
        })),
        Command::Status { run } => state_ctx.query(root, StateQuery::Status(StatusReq { run })),
        Command::List {
            workspace,
            status,
            include_tombstones,
            limit,
            offset,
            generation,
        } => {
            let status = status
                .as_deref()
                .map(RunStatus::from_str)
                .transpose()
                .map_err(anyhow::Error::from)?;
            state_ctx.query(
                root,
                StateQuery::List(ListReq {
                    workspace,
                    status,
                    include_tombstones,
                    limit,
                    offset,
                    generation,
                }),
            )
        }
        Command::Show { node, run } => {
            state_ctx.query(root, StateQuery::Show(ShowReq { run, node }))
        }
        Command::Context { node, run, worker } => state_ctx.query(
            root,
            StateQuery::Context(ContextReq {
                run,
                node,
                worker_id: worker
                    .map(|w| WorkerId::new(&w))
                    .transpose()
                    .map_err(anyhow::Error::from)?,
            }),
        ),
        Command::Next { run, limit } => {
            state_ctx.query(root, StateQuery::Next(NextReq { run, limit }))
        }
        Command::Roadmap {
            spec_id,
            collection,
            ready,
            active,
            tasks,
        } => state_ctx.query(
            root,
            StateQuery::Roadmap(RoadmapReq {
                spec_id,
                collection,
                ready_only: ready,
                active_only: active,
                include_tasks: tasks,
                ..Default::default()
            }),
        ),
        Command::Claim {
            node,
            run,
            worker,
            session_fingerprint,
            force,
            steal_remote,
            worktree,
            detail,
        } => {
            let worker_id = WorkerId::new(&worker)?;
            let parsed_detail =
                super::dispatch_code::parse_cli_detail(detail.as_deref())?.unwrap_or_default();
            state_ctx.command(StateCommand::Claim(ClaimReq {
                run,
                node,
                worker_id,
                session_fingerprint,
                force,
                steal_remote,
                worktree,
                detail: parsed_detail,
            }))
        }
        Command::Verify {
            node,
            run,
            worker,
            test,
            claim_revision,
            worktree,
        } => {
            let worker_id = worker.as_deref().map(WorkerId::new).transpose()?;
            state_ctx.command(StateCommand::Verify(VerifyReq {
                run,
                node,
                worker_id,
                test_target: test,
                claim_revision,
                worktree,
            }))
        }
        Command::Submit {
            node,
            action,
            run,
            worker,
            claim_revision,
            test,
            evidence,
            findings,
            invariants,
            summary,
            allow_main,
            worktree,
        } => {
            let submit_action = SubmitAction::from_str(&action)?;
            let worker_id = WorkerId::new(&worker)?;
            let mut review_evidence: Option<ReviewEvidence> = None;
            let mut custom_evidence: Option<Value> = None;
            if let Some(raw) = evidence.as_deref() {
                let parsed = parse_json_or_at_file(root, raw)?;
                if let Ok(re) = serde_json::from_value::<ReviewEvidence>(parsed.clone()) {
                    review_evidence = Some(re);
                } else {
                    custom_evidence = Some(parsed);
                }
            }
            let rejection_findings = findings
                .as_deref()
                .map(|raw| {
                    let parsed = parse_json_or_at_file(root, raw)?;
                    serde_json::from_value::<RejectionFindings>(parsed).map_err(|err| {
                        state_err_invalid(format!("invalid rejection findings: {err}"))
                    })
                })
                .transpose()?;
            state_ctx.command(StateCommand::Submit(Box::new(SubmitReq {
                run,
                node,
                action: submit_action,
                worker_id,
                claim_revision,
                test_target: test,
                review_evidence,
                rejection_findings,
                custom_evidence,
                verified_invariants: invariants,
                summary,
                allow_main,
                worktree,
            })))
        }
        Command::ExtendScope {
            node,
            paths,
            reason,
            run,
            worker,
        } => {
            let worker_id = worker.as_deref().map(WorkerId::new).transpose()?;
            state_ctx.command(StateCommand::ExtendScope(ExtendScopeReq {
                run,
                node,
                paths,
                reason,
                worker_id,
            }))
        }
        Command::Reopen {
            node,
            run,
            cascade,
            reason,
        } => state_ctx.command(StateCommand::Reopen(ReopenReq {
            run,
            node,
            cascade,
            reason,
        })),
        Command::Sync {
            run,
            rebind_profile,
        } => state_ctx.command(StateCommand::Sync(SyncReq {
            run,
            rebind_profile,
        })),
        Command::Subtask {
            task_ref,
            subtask_ref,
            status,
            evidence,
            run,
        } => {
            let task_ref = TaskRef::new(&task_ref)?;
            let subtask_ref = SubtaskRef::new(&subtask_ref)?;
            let status = SubtaskStatus::from_str(&status)?;
            state_ctx.command(StateCommand::Subtask(SubtaskReq {
                run,
                task_ref,
                subtask_ref,
                status,
                evidence,
            }))
        }
        Command::Blackboard {
            action,
            run,
            node,
            recipient_node,
            subtask_ref,
            gate,
            author,
            topic,
            payload,
            status,
            limit,
            offset,
            generation,
        } => match action.as_str() {
            "read" => {
                let topic = topic
                    .as_deref()
                    .map(BlackboardTopic::from_str)
                    .transpose()?;
                let status = status
                    .as_deref()
                    .map(BlackboardStatus::from_str)
                    .transpose()?;
                state_ctx.query(
                    root,
                    StateQuery::BlackboardRead(BlackboardReadReq {
                        run,
                        node,
                        recipient_node,
                        topic,
                        status,
                        limit,
                        offset,
                        generation,
                    }),
                )
            }
            "post" => {
                let topic_val = BlackboardTopic::from_str(topic.as_deref().ok_or_else(|| {
                    state_err_invalid("--topic is required for blackboard --action post")
                })?)?;
                let payload_val = payload.ok_or_else(|| {
                    state_err_invalid("--payload is required for blackboard --action post")
                })?;
                let author_val = author.unwrap_or_else(|| "cli".to_owned());
                let subtask_ref = subtask_ref.as_deref().map(SubtaskRef::new).transpose()?;
                state_ctx.command(StateCommand::BlackboardPost(BlackboardPostReq {
                    run,
                    node,
                    recipient_node,
                    subtask_ref,
                    gate,
                    author: author_val,
                    topic: topic_val,
                    payload: payload_val,
                }))
            }
            other => Err(state_err_invalid(format!(
                "blackboard --action must be 'read' or 'post', got '{other}'"
            ))),
        },
        Command::Snapshot {
            mut action,
            mut target,
            all,
            rev,
            run,
            delete,
            older_than,
            worktree,
        } => {
            if delete {
                if target.is_none() {
                    target = action.take();
                } else if !matches!(action.as_deref(), Some("clear" | "delete")) {
                    return Err(state_err_invalid("conflicting action with --delete"));
                }
                action = Some("clear".into());
            }
            let mapped = match action.as_deref() {
                Some("save") => SnapshotAction::Save,
                Some("restore") => SnapshotAction::Restore,
                Some("clear" | "delete") => SnapshotAction::Clear,
                Some("list") => SnapshotAction::List,
                _ => {
                    return Err(state_err_invalid(
                        "snapshot requires save, restore, list, or clear",
                    ))
                }
            };
            let (task_ref, name) = match (run.is_some(), target) {
                (true, Some(t)) => (
                    Some(TaskRef::new(&t).map_err(|err| state_err_invalid(err.to_string()))?),
                    None,
                ),
                (false, Some(t)) => (None, Some(t)),
                (_, None) => (None, None),
            };
            state_ctx.command(StateCommand::Snapshot(SnapshotReq {
                run,
                action: mapped,
                task_ref,
                all,
                revision: rev,
                name,
                worktree,
                older_than,
            }))
        }
        Command::Clear {
            run_id,
            run,
            reason,
            force,
        } => state_ctx.command(StateCommand::Clear(ClearReq {
            run: run.or(run_id),
            reason,
            force,
        })),
        Command::Run {
            run,
            jobs,
            allow_main,
            dry_run,
        } => state_ctx.command(StateCommand::Run(RunReq {
            run,
            jobs,
            allow_main,
            dry_run,
        })),
        Command::Tool {
            tool_name,
            args,
            run,
            node,
            worktree,
        } => {
            let parsed_args = parse_json_or_at_file(root, &args)?;
            if run.is_none() && node.is_none() {
                let providers =
                    build_domain_providers(root, state_ctx.adapter, state_ctx.code_db, Vec::new())?;
                let primary = acdd_core::git::primary_worktree_root(root)
                    .unwrap_or_else(|| root.to_path_buf());
                let wt = worktree.map(|p| acdd_core::fs::canonicalize_macos_var(&p));
                let tool_req = acdd_core::ports::DomainToolReq {
                    workspace_root: primary,
                    worktree: wt,
                    run_id: None,
                    node_cursor: None,
                };
                let output = providers
                    .call_tool(&tool_name, &parsed_args, &tool_req)
                    .map_err(|err| {
                        if let Some(se) = StateError::from_anyhow(&err) {
                            anyhow::Error::from(se.clone())
                        } else {
                            anyhow::Error::from(StateError::new(
                                ErrorCode::DomainCapabilityUnavailable,
                                format!("plugin tool '{tool_name}' failed: {err}"),
                            ))
                        }
                    })?;
                let wrapped =
                    StateCommandOutput::CallPluginTool(CallPluginToolResp { tool_name, output });
                render_cli_state_output(
                    &wrapped,
                    state_ctx.output_format,
                    state_ctx.adapter.debug,
                    || format::render_state_command_markdown(&wrapped, state_ctx.title),
                )
            } else {
                state_ctx.command(StateCommand::CallPluginTool(CallPluginToolReq {
                    tool_name,
                    args: parsed_args,
                    run,
                    node,
                    worktree,
                }))
            }
        }
        _ => unreachable!("non-state command routed to dispatch_state"),
    }
}
