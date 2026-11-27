use super::args::Command;
use super::paths;
use crate::{
    format::{self, OutputFormat},
    state_bridge::{
        build_domain_providers, execute_state_command_with_config, execute_state_query,
        resolve_gate_proof,
    },
};
use acdd_core::errors::{ErrorCode, AcddError};
use acdd_core::ids::{RunName, SubtaskRef, TaskRef, WorkerId};
use acdd_spec::SubtaskStatus;
use acdd_state::types::{
    BlackboardStatus, BlackboardTopic, RejectionFindings, ReviewEvidence, RunStatus, SubmitAction,
};
use acdd_state::usecases::{
    ArtifactSelector, BlackboardPostReq, BlackboardReadReq, CallPluginToolReq, CallPluginToolResp,
    ClaimReq, ClearReq, ContextReq, DraftReq, ExtendScopeReq, ListReq, NextReq, ReopenReq,
    RoadmapReq, RunReq, ShowReq, SnapshotAction, SnapshotReq, StartReq, StateCommand,
    StateCommandOutput, StateQuery, StatusReq, SubmitReq, SubtaskReq, SyncReq, VerifyReq,
};
use acdd_state::workflow::{ProofDef, ProofKind};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;
use std::str::FromStr;

pub(super) struct CliStateCtx<'a> {
    pub root: &'a Path,
    pub checkout_root: &'a Path,
    pub root_from_cli: bool,
    pub run_from_cli: bool,
    pub explicit_config: bool,
    pub state_db: Option<&'a Path>,
    pub code_db: Option<&'a Path>,
    pub adapter: &'a acdd_core::config::Adapter,
    pub output_format: OutputFormat,
    pub title: &'a str,
}

fn effective_run_selector(
    state_ctx: &CliStateCtx<'_>,
    run: Option<String>,
    has_explicit_worktree: bool,
) -> Option<String> {
    if state_ctx.run_from_cli || !(state_ctx.root_from_cli || has_explicit_worktree) {
        run
    } else {
        None
    }
}

impl CliStateCtx<'_> {
    fn command(&self, mut cmd: StateCommand) -> Result<(Value, Option<String>)> {
        if let Some((slot, has_wt)) = match &mut cmd {
            StateCommand::Claim(r) => Some((&mut r.run, r.worktree.is_some())),
            StateCommand::Verify(r) => Some((&mut r.run, r.worktree.is_some())),
            StateCommand::Submit(r) => Some((&mut r.run, r.worktree.is_some())),
            StateCommand::CallPluginTool(r) => Some((&mut r.run, r.worktree.is_some())),
            StateCommand::ExtendScope(r) => Some((&mut r.run, false)),
            StateCommand::Reopen(r) => Some((&mut r.run, false)),
            StateCommand::Sync(r) => Some((&mut r.run, false)),
            StateCommand::Subtask(r) => Some((&mut r.run, false)),
            StateCommand::BlackboardPost(r) => Some((&mut r.run, false)),
            StateCommand::Run(r) => Some((&mut r.run, false)),
            StateCommand::Start(_)
            | StateCommand::Draft(_)
            | StateCommand::Snapshot(_)
            | StateCommand::Clear(_) => None,
        } {
            *slot = effective_run_selector(self, slot.take(), has_wt);
        }
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

    fn query(&self, worktree: &Path, mut query: StateQuery) -> Result<(Value, Option<String>)> {
        if let Some(slot) = match &mut query {
            StateQuery::Status(r) => Some(&mut r.run),
            StateQuery::Show(r) => Some(&mut r.run),
            StateQuery::Context(r) => Some(&mut r.run),
            StateQuery::Next(r) => Some(&mut r.run),
            StateQuery::BlackboardRead(r) => Some(&mut r.run),
            StateQuery::List(_)
            | StateQuery::Observability(_)
            | StateQuery::Roadmap(_)
            | StateQuery::Validate(_) => None,
        } {
            *slot = effective_run_selector(self, slot.take(), false);
        }
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
        AcddError::new(
            ErrorCode::WorkflowSchemaInvalid,
            format!("invalid JSON payload: {err}"),
        )
        .into()
    })
}

fn state_err_invalid(msg: impl Into<String>) -> anyhow::Error {
    AcddError::new(ErrorCode::WorkflowSchemaInvalid, msg).into()
}

pub(super) fn dispatch(
    state_ctx: &CliStateCtx<'_>,
    explicit_workflow_ref: Option<String>,
    cli_workflow: Option<&Path>,
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
                workflow: explicit_workflow_ref
                    .or_else(|| cli_workflow.map(|p| p.to_string_lossy().into_owned())),
                scope,
                target_base,
                worktree,
                in_place,
                worktree_path,
            }))
        }
        Command::Draft {
            spec,
            slug,
            prefix,
            title: spec_title,
        } => {
            if !state_ctx
                .adapter
                .modules
                .is_subsystem_enabled(acdd_core::subsystems::SubsystemId::Spec)
            {
                return Err(AcddError::new(
                    ErrorCode::FeatureDisabled,
                    "FEATURE_DISABLED: 'spec' subsystem is disabled in modules:",
                )
                .into());
            }
            let req = DraftReq {
                spec,
                slug,
                prefix,
                title: spec_title,
                workflow: explicit_workflow_ref,
            };
            if state_ctx
                .adapter
                .modules
                .is_subsystem_enabled(acdd_core::subsystems::SubsystemId::State)
            {
                state_ctx.command(StateCommand::Draft(req))
            } else {
                let primary = acdd_core::git::primary_worktree_root(root)
                    .unwrap_or_else(|| root.to_path_buf());
                let cols = acdd_spec::effective_spec_collections(&state_ctx.adapter.specs);
                let resp = acdd_spec::draft_spec(
                    &primary,
                    &cols,
                    &req,
                    &acdd_spec::overlay::NoopRuntimeOverlay,
                )?;
                let wrapped = StateCommandOutput::Draft(resp);
                render_cli_state_output(
                    &wrapped,
                    state_ctx.output_format,
                    state_ctx.adapter.debug,
                    || format::render_state_command_markdown(&wrapped, state_ctx.title),
                )
            }
        }
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
        Command::Show {
            node,
            run,
            artifact,
            include_profile,
        } => {
            let artifact = artifact
                .as_deref()
                .map(ArtifactSelector::from_str)
                .transpose()
                .map_err(anyhow::Error::from)?;
            state_ctx.query(
                root,
                StateQuery::Show(ShowReq {
                    run,
                    node,
                    artifact,
                    include_profile,
                }),
            )
        }
        Command::Context {
            node,
            run,
            worker,
            detail,
        } => state_ctx.query(
            root,
            StateQuery::Context(ContextReq {
                run,
                node,
                worker_id: worker
                    .map(|w| WorkerId::new(&w))
                    .transpose()
                    .map_err(anyhow::Error::from)?,
                detail: super::dispatch_code::parse_cli_detail(detail.as_deref())?,
            }),
        ),
        Command::Next { run, limit } => {
            state_ctx.query(root, StateQuery::Next(NextReq { run, limit }))
        }
        Command::Roadmap {
            spec_id,
            spec,
            ready,
            active,
            tasks,
        } => {
            if !state_ctx
                .adapter
                .modules
                .is_subsystem_enabled(acdd_core::subsystems::SubsystemId::Spec)
            {
                return Err(AcddError::new(
                    ErrorCode::FeatureDisabled,
                    "FEATURE_DISABLED: 'spec' subsystem is disabled in modules:",
                )
                .into());
            }
            let req = RoadmapReq {
                spec_id,
                spec,
                ready_only: ready,
                active_only: active,
                include_tasks: tasks,
                ..Default::default()
            };
            if state_ctx
                .adapter
                .modules
                .is_subsystem_enabled(acdd_core::subsystems::SubsystemId::State)
            {
                state_ctx.query(root, StateQuery::Roadmap(req))
            } else {
                let primary = acdd_core::git::primary_worktree_root(root)
                    .unwrap_or_else(|| root.to_path_buf());
                let linked: std::collections::BTreeMap<String, std::path::PathBuf> = state_ctx
                    .adapter
                    .linked_workspaces
                    .iter()
                    .map(|lw| (lw.name.clone(), lw.path.clone()))
                    .collect();
                let roadmap_detail = req.spec_id.is_some();
                let cfg = acdd_spec::roadmap::RoadmapConfig {
                    workspace_root: &primary,
                    worktree: root,
                    spec_collections: &state_ctx.adapter.specs,
                    linked_workspaces: &linked,
                };
                let resp = acdd_spec::roadmap::build_roadmap(
                    &cfg,
                    &acdd_spec::overlay::NoopRuntimeOverlay,
                    req,
                )?;
                let wrapped = acdd_state::usecases::StateQueryOutput::Roadmap(Box::new(resp));
                render_cli_state_output(
                    &wrapped,
                    state_ctx.output_format,
                    state_ctx.adapter.debug,
                    || format::render_state_query_markdown(&wrapped, state_ctx.title, roadmap_detail),
                )
            }
        }
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
                let effective_run = effective_run_selector(state_ctx, run.clone(), worktree.is_some());
                let target_wt = worktree.as_deref().unwrap_or(root);
                let proof_def = resolve_gate_proof(
                    state_ctx.root,
                    target_wt,
                    state_ctx.state_db,
                    state_ctx.code_db,
                    state_ctx.adapter,
                    effective_run.as_deref(),
                    &node,
                )?;
                match proof_def {
                    ProofDef::Kind(ProofKind::Review) => {
                        let re = serde_json::from_value::<ReviewEvidence>(parsed).map_err(|err| {
                            state_err_invalid(format!("invalid review evidence: {err}"))
                        })?;
                        review_evidence = Some(re);
                    }
                    ProofDef::Scheme { .. }
                    | ProofDef::Named(_)
                    | ProofDef::ResolvedNamed { .. }
                    | ProofDef::Kind(_) => {
                        custom_evidence = Some(parsed);
                    }
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
            rebind_workflow,
        } => state_ctx.command(StateCommand::Sync(SyncReq {
            run,
            rebind_workflow,
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
            let run = effective_run_selector(state_ctx, run, worktree.is_some());
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
            purge,
        } => {
            let flag_or_env_run = effective_run_selector(state_ctx, run, false);
            let selected_run = if state_ctx.run_from_cli {
                flag_or_env_run.or(run_id)
            } else {
                run_id.or(flag_or_env_run)
            };
            state_ctx.command(StateCommand::Clear(ClearReq {
                run: selected_run,
                reason,
                force,
                purge,
            }))
        }
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
            let run = effective_run_selector(state_ctx, run, worktree.is_some());
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
                        if let Some(se) = AcddError::from_anyhow(&err) {
                            anyhow::Error::from(se.clone())
                        } else {
                            anyhow::Error::from(AcddError::new(
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
