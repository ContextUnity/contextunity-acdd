mod args;
mod dispatch_code;
mod dispatch_state;
mod docs_check;
mod paths;
/// Sends a reload signal to running MCP servers.
pub mod reload;
/// Fail-fast ACDD profile and spec validation.
mod validate;

use args::Cli;
pub(crate) use args::{Command, DocsCommand, QueryCommand};

use crate::{
    engine::{guide, pipeline as writer},
    format::{self, OperationKind, OutputFormat},
    state_bridge::InvocationSettings,
};
use acdd_core::errors::{AcddError, ErrorCode};
use acdd_core::subsystems::SubsystemId;
use anyhow::{bail, Result};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

fn guide_title(mcp: bool) -> &'static str {
    if mcp {
        "acdd_guide"
    } else {
        "acdd guide"
    }
}

fn command_presentation(command: &Command) -> (OperationKind, &'static str, Option<bool>) {
    match command {
        Command::Init { .. } => (OperationKind::General, "acdd_init", None),
        Command::Start { .. }
        | Command::Draft { .. }
        | Command::Status { .. }
        | Command::List { .. }
        | Command::Show { .. }
        | Command::Next { .. }
        | Command::Context { .. }
        | Command::Roadmap { .. }
        | Command::ExtendScope { .. }
        | Command::Reopen { .. }
        | Command::Sync { .. }
        | Command::Subtask { .. }
        | Command::Clear { .. }
        | Command::Snapshot { .. }
        | Command::Run { .. }
        | Command::Tool { .. } => (OperationKind::Task, "acdd_state", None),
        Command::Claim { .. } => (OperationKind::Task, "acdd_claim", None),
        Command::Verify { .. } => (OperationKind::Task, "acdd_verify", None),
        Command::Submit { .. } => (OperationKind::Task, "acdd_submit", None),
        Command::Blackboard { .. } => (OperationKind::Task, "acdd_blackboard", None),
        Command::Read { .. } => (OperationKind::Navigation, "acdd_read", None),
        Command::Guide { mcp, .. } => (OperationKind::Guide, guide_title(*mcp), None),
        Command::Ast { .. } => (OperationKind::Navigation, "ast_grep_search", None),
        Command::Lint { .. } => (OperationKind::Navigation, "code_map_lint", None),
        Command::Query {
            command: QueryCommand::Sql { .. },
        } => (OperationKind::Sql, "code_map_query", None),
        Command::Query {
            command: QueryCommand::Run { operation, .. },
        } => (
            if operation == "sql" || dispatch_code::looks_like_sql(operation) {
                OperationKind::Sql
            } else {
                OperationKind::Navigation
            },
            "code_map_query",
            None,
        ),
        Command::Query {
            command: QueryCommand::Analyze { target, mode, .. },
        } => (
            if mode.is_none() && dispatch_code::looks_like_sql(target) {
                OperationKind::Sql
            } else {
                OperationKind::Navigation
            },
            "code_map_analyze",
            None,
        ),
        Command::Query { command } => match command {
            QueryCommand::Overview { .. } => (OperationKind::Navigation, "code_map_overview", None),
            QueryCommand::Inspect { .. } => {
                (OperationKind::Navigation, "code_map_inspect", Some(false))
            }
            QueryCommand::Search { .. } => (OperationKind::Navigation, "code_map_search", None),
            QueryCommand::Tests { .. } => (OperationKind::Navigation, "code_map_tests", None),
            QueryCommand::Impact { .. } => (OperationKind::Navigation, "code_map_impact", None),
            QueryCommand::Explain { .. } => {
                (OperationKind::Navigation, "code_map_explain", Some(true))
            }
            QueryCommand::Remove { .. } => {
                (OperationKind::Navigation, "code_map_prove_removal", None)
            }
            QueryCommand::Analyze { .. } | QueryCommand::Run { .. } | QueryCommand::Sql { .. } => {
                unreachable!()
            }
        },
        Command::Docs { command } => match command {
            DocsCommand::Check => (OperationKind::General, "docs check", None),
            DocsCommand::Search { .. } => (OperationKind::Navigation, "search_docs", None),
            DocsCommand::Get { .. } => (OperationKind::Navigation, "get_doc", None),
        },
        Command::Build { .. } => (OperationKind::General, "build result", None),
        Command::Scan { .. } => (OperationKind::General, "scan result", None),
        Command::Delta { .. } => (OperationKind::General, "delta result", None),
        Command::Validate { .. } => (OperationKind::Task, "validate result", None),
        Command::Reload => (OperationKind::General, "reload result", None),
        Command::Serve => (OperationKind::General, "server", None),
        Command::Exec { .. } => (OperationKind::Task, "acdd_exec", None),
    }
}

fn command_discovery(command: &Command) -> Option<format::DiscoveryKind> {
    match command {
        Command::Ast { .. } => Some(format::DiscoveryKind::Ast),
        Command::Query { command } => match command {
            QueryCommand::Overview { .. } => Some(format::DiscoveryKind::Overview),
            QueryCommand::Inspect { .. } => Some(format::DiscoveryKind::Inspect),
            QueryCommand::Run { operation, .. } if operation.trim() == "inspect" => {
                Some(format::DiscoveryKind::Inspect)
            }
            QueryCommand::Search { .. } => Some(format::DiscoveryKind::Search),
            _ => None,
        },
        _ => None,
    }
}

fn command_root(global: Option<&Path>, positional: Option<&Path>) -> Result<PathBuf> {
    let global = global.map(Path::canonicalize).transpose()?;
    let positional = positional.map(Path::canonicalize).transpose()?;
    if let (Some(global), Some(positional)) = (&global, &positional) {
        if global != positional {
            bail!(
                "conflicting workspace roots: --root {} and positional {}",
                global.display(),
                positional.display()
            );
        }
    }
    Ok(positional
        .or(global)
        .unwrap_or(std::env::current_dir()?.canonicalize()?))
}

fn cli_command_line() -> String {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        "serve".to_string()
    } else {
        args.join(" ")
    }
}

fn write_output(output: &mut dyn io::Write, text: &str) -> Result<()> {
    if let Err(e) = writeln!(output, "{text}") {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e.into());
    }
    Ok(())
}

#[derive(Default)]
struct CliRunSources {
    root_from_cli: bool,
    run_from_cli: bool,
    stdout_is_tty: bool,
    env_workspace_root: Option<PathBuf>,
    env_worktree: Option<PathBuf>,
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn exit_on_cli_error(error: anyhow::Error) -> Result<()> {
    if let Some(acdd_err) = AcddError::from_anyhow(&error) {
        eprintln!("{error:#}");
        std::process::exit(acdd_err.exit_code());
    }
    Err(error)
}

/// Parses CLI arguments from the process environment and executes the selected command.
pub async fn run_from_env() -> Result<()> {
    use clap::{parser::ValueSource, CommandFactory, FromArgMatches};
    let matches = Cli::command().get_matches();
    let root_from_cli = matches.value_source("root") == Some(ValueSource::CommandLine);
    let run_from_cli = matches.subcommand().is_some_and(|(_, sub)| {
        sub.ids().any(|id| id == "run") && sub.value_source("run") == Some(ValueSource::CommandLine)
    });
    let cli = Cli::from_arg_matches(&matches)?;
    let sources = CliRunSources {
        root_from_cli,
        run_from_cli,
        stdout_is_tty: std::io::IsTerminal::is_terminal(&io::stdout()),
        env_workspace_root: (!root_from_cli)
            .then(|| non_empty_env_path("ACDD_WORKSPACE_ROOT"))
            .flatten(),
        env_worktree: (!root_from_cli)
            .then(|| non_empty_env_path("ACDD_WORKTREE"))
            .flatten(),
    };
    cli.run(sources).await
}

impl Cli {
    /// Run the parsed CLI command and terminate with canonical `AcddError` exit codes (`2`/`3`/`4`).
    async fn run(self, sources: CliRunSources) -> Result<()> {
        match self.run_inner(&mut io::stdout(), None, &sources).await {
            Ok(Some(code)) if code != 0 => std::process::exit(code),
            Ok(_) => Ok(()),
            Err(error) => exit_on_cli_error(error),
        }
    }

    /// Runs CLI dispatch and formatting with a caller-provided output writer.
    #[cfg(test)]
    async fn run_with_writer<W: io::Write>(self, output: &mut W) -> Result<()> {
        let sources = CliRunSources {
            root_from_cli: self.root.is_some(),
            run_from_cli: std::env::var_os("ACDD_RUN").is_none(),
            env_worktree: self
                .root
                .is_none()
                .then(|| non_empty_env_path("ACDD_WORKTREE"))
                .flatten(),
            ..CliRunSources::default()
        };
        let _ = self.run_inner(output, None, &sources).await?;
        Ok(())
    }

    async fn run_inner(
        mut self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
        sources: &CliRunSources,
    ) -> Result<Option<i32>> {
        let effective_root = self
            .root
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| sources.env_worktree.clone())
            .or_else(|| {
                self.root
                    .is_none()
                    .then(|| non_empty_env_path("ACDD_WORKTREE"))
                    .flatten()
            });
        if let Some(Command::Delta {
            workspace_root,
            modified_files,
            ..
        }) = &mut self.command
        {
            if let (Some(root_arg), Some(first)) =
                (effective_root.as_deref(), workspace_root.as_ref())
            {
                let is_explicit_dir = first == Path::new(".")
                    || first == Path::new("..")
                    || first.is_dir()
                    || root_arg.join(first).is_dir();
                if first.is_relative() && !is_explicit_dir {
                    if let Some(file) = workspace_root.take() {
                        modified_files.insert(0, file);
                    }
                }
            }
        }
        if let Some(Command::Exec {
            ref run,
            ref worker,
            ref command,
        }) = self.command
        {
            let cwd = std::env::current_dir()?;
            let explicit_cli_root = sources
                .root_from_cli
                .then_some(self.root.as_deref())
                .flatten();
            let target = crate::state_bridge::resolve_exec_target(
                &crate::state_bridge::ExecResolveInput {
                    explicit_cli_root,
                    env_workspace_root: sources.env_workspace_root.as_deref(),
                    env_worktree: sources.env_worktree.as_deref(),
                    explicit_config: self.config.as_deref(),
                    explicit_state_db: self.state_db.as_deref(),
                    worker: worker.as_deref(),
                    run_selector: run.as_deref(),
                    run_from_cli: sources.run_from_cli,
                    cwd: &cwd,
                },
            )?;
            let Some((program, args)) = command.split_first() else {
                bail!("exec requires a program and optional arguments after '--'");
            };
            let wt_str = target.worktree.to_string_lossy();
            let run_env = target.run_id.as_ref().map(|r| r.as_str());
            let code = acdd_core::sys::run_inherited_in_dir(
                program,
                args,
                &target.worktree,
                &[
                    ("ACDD_WORKSPACE_ROOT", Some(wt_str.as_ref())),
                    ("ACDD_WORKTREE", Some(wt_str.as_ref())),
                    ("ACDD_RUN", run_env),
                ],
            )
            .map_err(|err| anyhow::anyhow!("failed to execute '{program}': {err}"))?;
            return Ok(Some(code));
        }
        if matches!(self.command.as_ref(), Some(Command::Snapshot { .. })) {
            if let Some(root) = effective_root.as_deref() {
                acdd_core::fs::check_git_entry_no_symlinks(root).map_err(|e| anyhow::anyhow!(e))?;
            }
        }
        let positional_root = match self.command.as_ref() {
            Some(
                Command::Build { workspace_root, .. }
                | Command::Scan { workspace_root, .. }
                | Command::Delta { workspace_root, .. },
            ) => workspace_root.as_deref(),
            _ => None,
        };
        let root = match command_root(effective_root.as_deref(), positional_root) {
            Ok(root) => root,
            Err(error) => {
                if matches!(
                    self.command.as_ref(),
                    Some(Command::Docs {
                        command: DocsCommand::Check
                    })
                ) {
                    docs_check::check(
                        effective_root.as_deref().unwrap_or_else(|| Path::new(".")),
                        OutputFormat::from(self.format),
                        output,
                    )?;
                    return Ok(None);
                }
                return Err(error);
            }
        };

        // `init` and `guide init` must work even before `.acdd/workflows/default.yaml` exists.
        if matches!(self.command.as_ref(), Some(Command::Init { .. })) {
            let force = matches!(self.command.as_ref(), Some(Command::Init { force: true }));
            let result = guide::initialize(&root, force)?;
            let formatted = format::render_for_operation(
                &result,
                OutputFormat::from(self.format),
                "acdd_init",
                None,
                OperationKind::General,
            );
            writeln!(output, "{formatted}")?;
            return Ok(None);
        }
        if let Some(Command::Guide { topic, force, mcp }) = self.command.as_ref() {
            if topic.trim() == "init" {
                let result = guide::run(&root, topic, *force)?;
                let formatted = format::render_for_operation(
                    &result,
                    OutputFormat::from(self.format),
                    guide_title(*mcp),
                    None,
                    OperationKind::Guide,
                );
                writeln!(output, "{formatted}")?;
                return Ok(None);
            }
        }

        // `validate` handles potentially broken configs/workflows directly without ResolvedPaths pre-failing,
        // while still failing closed with `FEATURE_DISABLED` when `spec` is disabled on a valid config.
        if let Some(Command::Validate { path }) = self.command {
            let explicit_config = self.config.as_deref().map(|p| {
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                }
            });
            if let Ok(report) =
                acdd_core::config::load_adapter_report(&root, explicit_config.as_deref())
            {
                if !report.adapter.modules.is_subsystem_enabled(SubsystemId::Spec) {
                    return Err(AcddError::new(
                        ErrorCode::FeatureDisabled,
                        "FEATURE_DISABLED: 'spec' subsystem is disabled in modules:",
                    )
                    .into());
                }
            }
            let explicit_workflow = self
                .workflow
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned());
            let result = validate::run_with_options(
                &root,
                path.as_deref(),
                explicit_config.as_deref(),
                explicit_workflow.as_deref(),
            )?;
            let failed = result.get("valid").and_then(Value::as_bool) == Some(false);
            let formatted = format::render_for_operation(
                &result,
                OutputFormat::from(self.format),
                "validate result",
                None,
                OperationKind::Task,
            );
            writeln!(output, "{formatted}")?;
            if failed {
                let has_non_spec_err = result["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|d| {
                        d["status"] == "fail"
                            && d["code"]
                                .as_str()
                                .is_some_and(|c| c != ErrorCode::SpecSchemaInvalid.as_str())
                    });
                let code = if has_non_spec_err {
                    ErrorCode::WorkflowSchemaInvalid
                } else {
                    ErrorCode::SpecSchemaInvalid
                };
                return Err(AcddError::new(
                    code,
                    "VALIDATION_FAILED: one or more documents failed validation",
                )
                .into());
            }
            return Ok(None);
        }

        let workflow_at_root = match &self.command {
            Some(Command::Start {
                specs,
                worktree_path: Some(_),
                ..
            }) if !specs.is_empty() => None,
            _ => self.workflow.as_deref(),
        };
        let settings =
            paths::ResolvedPaths::resolve(&root, self.config.as_deref(), workflow_at_root)?;
        let database = acdd_core::config::resolve_code_db_path(&root, self.db.as_deref());
        let state_db = self.state_db.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });
        let workflow_argument = self.workflow.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });

        self.run_selected(
            output,
            cache_dir,
            root,
            effective_root.as_deref(),
            sources.root_from_cli,
            sources.run_from_cli,
            sources.stdout_is_tty,
            settings,
            database,
            state_db,
            workflow_argument,
        )
        .await?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_selected(
        self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
        root: PathBuf,
        effective_root: Option<&Path>,
        root_from_cli: bool,
        run_from_cli: bool,
        stdout_is_tty: bool,
        settings: paths::ResolvedPaths,
        db: PathBuf,
        state_db: Option<PathBuf>,
        workflow_argument: Option<PathBuf>,
    ) -> Result<()> {
        let output_format = OutputFormat::from(self.format);
        let command = self.command.unwrap_or(Command::Serve);
        if matches!(
            &command,
            Command::Docs {
                command: DocsCommand::Check
            }
        ) {
            if !settings
                .adapter
                .modules
                .is_subsystem_enabled(SubsystemId::Docs)
            {
                return Err(AcddError::new(
                    ErrorCode::FeatureDisabled,
                    "FEATURE_DISABLED: 'docs' module is disabled in modules:",
                )
                .into());
            }
            return docs_check::check(&root, output_format, output);
        }

        let (operation, title, explain) = command_presentation(&command);
        let discovery = command_discovery(&command);
        let paths::ResolvedPaths {
            adapter,
            config: explicit_config,
            workflow_ref: explicit_workflow_ref,
            ..
        } = settings;

        let span_worktree = match &command {
            Command::Claim { worktree, .. }
            | Command::Verify { worktree, .. }
            | Command::Submit { worktree, .. }
            | Command::Tool { worktree, .. } => worktree.as_deref().unwrap_or(&root),
            _ => &root,
        };
        let span_ctx = if adapter.modules.is_subsystem_enabled(SubsystemId::State) {
            crate::state_bridge::resolve_worktree_span_context(
                &root,
                span_worktree,
                state_db.as_deref(),
                &adapter,
            )
            .ok()
            .flatten()
        } else {
            None
        };
        let mut span = crate::telemetry::SpanGuard::from_worktree_context(
            &root,
            span_ctx.as_ref(),
            title,
            adapter.retain_traces,
        );
        if span_ctx.is_some() {
            if let Ok(db) = acdd_state::usecases::deps::try_resolve_effective_state_db(
                &root,
                state_db.as_deref(),
            ) {
                span = span.with_state_db(db);
            }
        }
        let checkout_root = effective_root.map(std::path::absolute).transpose()?;
        let state_ctx = dispatch_state::CliStateCtx {
            root: &root,
            checkout_root: checkout_root.as_deref().unwrap_or(&root),
            root_from_cli,
            run_from_cli,
            explicit_config: self.config.is_some(),
            stdout_is_tty,
            state_db: state_db.as_deref(),
            code_db: Some(db.as_path()),
            adapter: &adapter,
            output_format,
            title,
        };
        let code_ctx = dispatch_code::CliCodeCtx {
            root: &root,
            db: &db,
            state_db: state_db.as_deref(),
            adapter: &adapter,
            explicit_config: explicit_config.as_deref(),
            has_explicit_config: self.config.is_some(),
            cache_dir,
            output_format,
        };

        let exec_result: Result<(Value, Option<String>)> = (|| async {
            match command {
                Command::Init { .. } | Command::Validate { .. } | Command::Exec { .. } => {
                    unreachable!("handled prior to run_selected")
                }
                Command::Serve => {
                    writer::validate_code_map_target(&root, &db)?;
                    let invocation = InvocationSettings {
                        root: root.clone(),
                        config: explicit_config.clone(),
                        workflow: workflow_argument,
                        state_db: state_db.clone(),
                        code_db: db.clone(),
                    };
                    crate::mcp::server::serve_with_state_config(
                        root.clone(),
                        db.clone(),
                        cache_dir.map(Path::to_path_buf),
                        Some(invocation),
                    )
                    .await?;
                    Ok((Value::Null, Some(String::new())))
                }
                Command::Reload => Ok((reload::run()?, None)),
                Command::Guide { topic, force, mcp } => Ok((
                    guide::run_with_adapter_mode(&root, &topic, force, !mcp, &adapter)?,
                    None,
                )),
                cmd @ (Command::Read { .. }
                | Command::Build { .. }
                | Command::Scan { .. }
                | Command::Delta { .. }
                | Command::Ast { .. }
                | Command::Lint { .. }
                | Command::Query { .. }
                | Command::Docs { .. }) => dispatch_code::dispatch(&code_ctx, cmd),
                cmd => dispatch_state::dispatch(
                    &state_ctx,
                    explicit_workflow_ref,
                    self.workflow.as_deref(),
                    cmd,
                ),
            }
        })()
        .await;

        let (mut result, pre_rendered) = match exec_result {
            Ok(pair) => {
                span.set_status("ok");
                span.finish_async().await;
                pair
            }
            Err(err) => {
                span.set_status("error");
                span.finish_async().await;
                return Err(err);
            }
        };

        if let Some(text) = pre_rendered {
            if text.is_empty() && title == "server" {
                return Ok(());
            }
            if adapter.debug {
                crate::telemetry::log_cli(&root, &cli_command_line(), None, &result);
            }
            return write_output(output, &text);
        }

        if output_format == OutputFormat::Markdown
            && (adapter.response.file_uris
                || adapter.response.detail == acdd_core::response::Detail::Full)
        {
            if let Some(kind) = discovery {
                format::attach_presentation_uris(&root, &adapter, &mut result, kind);
            }
            if matches!(title, "search_docs" | "get_doc") {
                format::attach_document_presentation_uris(&root, &adapter, &mut result, title);
            }
            if title == "code_map_lint" {
                format::attach_lint_presentation_uris(&root, &adapter, &mut result);
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
                format::attach_traversal_presentation_uris(&root, &adapter, &mut result);
            }
        }
        let lint_errors = (title == "code_map_lint")
            .then(|| result.pointer("/summary/errors").and_then(Value::as_u64))
            .flatten()
            .filter(|&e| e > 0);
        let formatted = if title == "acdd_read" {
            let debug_snapshot = adapter.debug.then(|| result.clone());
            let budget = adapter
                .response
                .max_output_bytes
                .clamp(1024, acdd_core::response::MAX_OUTPUT_BYTES)
                .saturating_sub(256);
            let text = match format::present_value_bounded(
                result,
                budget,
                format::PresentationOptions {
                    output_format,
                    operation,
                    title,
                    explain,
                    discovery,
                },
                &str::len,
            ) {
                format::BoundedPresentation::Ok(text) => text,
                format::BoundedPresentation::AstDiagnostic(err)
                | format::BoundedPresentation::Oversized(err) => bail!("{err}"),
            };
            if let Some(debug_result) = debug_snapshot {
                crate::telemetry::log_cli(&root, &cli_command_line(), None, &debug_result);
            }
            text
        } else {
            format::compact(&mut result, operation, explain);
            let formatted =
                format::render_for_operation(&result, output_format, title, discovery, operation);
            if adapter.debug {
                crate::telemetry::log_cli(&root, &cli_command_line(), None, &result);
            }
            formatted
        };
        write_output(output, &formatted)?;
        if let Some(errors) = lint_errors {
            bail!("lint found {errors} error(s)");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use acdd_core::errors::{AcddError, ErrorCode};
    use acdd_core::fs::ScopedTempDir;
    use clap::Parser;
    use std::fs;

    #[test]
    fn cli_exit_codes_preflight_init_validate_and_flat_dispatch() {
        // 1. Exact ErrorCode -> exit code mapping (v7 §4.3: 2/3/4)
        let exit_cases = [
            (ErrorCode::ConfigMigrationRequired, 2),
            (ErrorCode::WorkflowSchemaInvalid, 2),
            (ErrorCode::SpecSchemaInvalid, 2),
            (ErrorCode::TaskAlreadyClaimed, 3),
            (ErrorCode::OutOfScopeChanges, 3),
            (ErrorCode::GateVerificationFailed, 4),
            (ErrorCode::BackendConflict, 4),
        ];
        for (code, expected) in exit_cases {
            assert_eq!(
                AcddError::new(code, "test").exit_code(),
                expected,
                "{code:?}"
            );
        }

        // 2. Flat CLI subcommand parsing preserves Cli/Command AST & removes legacy trees
        for args in [
            vec!["acdd", "init"],
            vec!["acdd", "start", "AUD-001", "AC-006"],
            vec!["acdd", "start", "--goal", "ship feature", "--name", "m1"],
            vec![
                "acdd",
                "start",
                "--goal",
                "ship feature",
                "--name",
                "m1",
                "--workflow",
                "code",
            ],
            vec!["acdd", "draft", "milestones", "my-slug"],
            vec![
                "acdd",
                "draft",
                "milestones",
                "my-slug",
                "--workflow",
                "code",
            ],
            vec!["acdd", "status", "--run", "m1"],
            vec!["acdd", "list"],
            vec!["acdd", "show", "--run", "m1"],
            vec!["acdd", "next", "--run", "m1"],
            vec!["acdd", "roadmap"],
            vec!["acdd", "roadmap", "--spec", "milestones", "--format", "json"],
            vec!["acdd", "claim", "t1.build", "--run", "m1", "--worker", "w1"],
            vec!["acdd", "verify", "t1.build", "--run", "m1"],
            vec![
                "acdd", "submit", "t1.build", "--run", "m1", "--action", "pass",
            ],
            vec![
                "acdd",
                "extend-scope",
                "t1.build",
                "--run",
                "m1",
                "--path",
                "src/a.rs",
                "--reason",
                "need helper",
            ],
            vec!["acdd", "reopen", "t1.build", "--run", "m1"],
            vec!["acdd", "sync", "--run", "m1"],
            vec!["acdd", "sync", "--run", "m1", "--rebind-workflow"],
            vec![
                "acdd",
                "subtask",
                "t1",
                "s1",
                "--run",
                "m1",
                "--status",
                "completed",
                "--evidence",
                "ok",
            ],
            vec!["acdd", "blackboard", "--run", "m1", "--action", "read"],
            vec!["acdd", "snapshot", "list"],
            vec!["acdd", "clear", "--run", "m1"],
            vec!["acdd", "run", "--run", "m1"],
            vec!["acdd", "tool", "code_extra"],
            vec!["acdd", "validate"],
        ] {
            assert!(
                Cli::try_parse_from(&args).is_ok(),
                "failed to parse {args:?}"
            );
        }
        for legacy in [
            vec!["acdd", "milestone", "list"],
            vec!["acdd", "plan", "list"],
            vec!["acdd", "workflow", "list"],
            vec!["acdd", "task", "list"],
            vec!["acdd", "checkpoint", "save"],
            vec!["acdd", "migrate"],
        ] {
            assert!(
                Cli::try_parse_from(&legacy).is_err(),
                "legacy cmd should be removed: {legacy:?}"
            );
        }
        let snap_legacy =
            Cli::try_parse_from(["acdd", "snapshot", "snap1", "--run", "m1"]).unwrap();
        let mut sink = Vec::new();
        assert!(tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(snap_legacy.run_with_writer(&mut sink))
            .is_err());

        // 3. `acdd init` picks inherit: code vs base and writes canonical .gitignore block
        for (acdd_yaml, expected_inherit) in [
            (
                "modules:\n  code: true\n  docs: true\n  state: true\n",
                "inherit: code",
            ),
            (
                "modules:\n  code: false\n  docs: true\n  state: true\n",
                "inherit: base",
            ),
        ] {
            let dir = ScopedTempDir::create("acdd-ut-cli-init").unwrap();
            let root = dir.path();
            assert!(acdd_core::git::git_command(root, &["init"])
                .unwrap()
                .status
                .success());
            fs::write(root.join("acdd.yaml"), acdd_yaml).unwrap();

            let mut out = Vec::new();
            let cli =
                Cli::try_parse_from(["acdd", "--root", root.to_str().unwrap(), "init"]).unwrap();
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(cli.run_with_writer(&mut out))
                .unwrap();

            let profile = fs::read_to_string(root.join(".acdd/workflows/default.yaml")).unwrap();
            assert!(profile.contains(expected_inherit), "{profile}");
            let gi = fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(gi.contains(".acdd/*.sqlite*"), "{gi}");
            assert!(acdd_core::git::git_command(
                root,
                &["check-ignore", "-q", ".acdd/state.sqlite"]
            )
            .unwrap()
            .status
            .success());

            if expected_inherit == "inherit: base" {
                let cli_force = Cli::try_parse_from([
                    "acdd",
                    "--root",
                    root.to_str().unwrap(),
                    "init",
                    "--force",
                ])
                .unwrap();
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(cli_force.run_with_writer(&mut out))
                    .unwrap();
                assert!(fs::read_to_string(root.join(".acdd/workflows/default.yaml"))
                    .unwrap()
                    .contains("inherit: code"));
            }
        }

        // 4. `acdd validate` succeeds on valid workspace with bound commands and fails on unbound/broken profile
        let vdir = ScopedTempDir::create("acdd-ut-cli-validate").unwrap();
        let vroot = vdir.path();
        fs::create_dir_all(vroot.join(".acdd/workflows")).unwrap();
        fs::write(
            vroot.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: true\n",
        )
        .unwrap();
        fs::write(
            vroot.join(".acdd/workflows/default.yaml"),
            "version: 2\ninherit: code\ncommands:\n  test: \"cargo test --lib {test_filter}\"\n  build_check: \"cargo check --lib\"\n  verify_all: \"cargo test --lib\"\n",
        )
        .unwrap();
        let val = super::validate::run_with_options(vroot, None, None, None).unwrap();
        assert_eq!(val["valid"], serde_json::json!(true), "{val}");

        fs::write(
            vroot.join(".acdd/workflows/default.yaml"),
            "version: 2\ninherit: code\n",
        )
        .unwrap();
        let unbound = super::validate::run_with_options(vroot, None, None, None).unwrap();
        assert_eq!(unbound["valid"], serde_json::json!(false), "{unbound}");
        assert_eq!(unbound["diagnostics"][1]["code"], "COMMAND_UNBOUND");

        fs::write(
            vroot.join(".acdd/workflows/default.yaml"),
            "version: 1\nbogus: true\n",
        )
        .unwrap();
        let bad = super::validate::run_with_options(vroot, None, None, None).unwrap();
        assert_eq!(bad["valid"], serde_json::json!(false), "{bad}");

        // 5. Default `.acdd/state.sqlite` keeps `state_db_override == None` (enabling `state.bak.sqlite`) even from linked worktrees,
        // and `code_db_path` is populated iff `code || docs` is enabled (#25).
        let cfg_default = acdd_core::config::Adapter::default();
        let eng_default =
            crate::state_bridge::state_engine_config(vroot, vroot, None, None, &cfg_default)
                .unwrap();
        assert!(eng_default.state_db_override.is_none());
        assert_eq!(
            eng_default.code_db_path,
            Some(acdd_core::config::resolve_code_db_path(vroot, None))
        );
        let cfg_minimal = acdd_core::config::Adapter {
            modules: acdd_core::subsystems::SubsystemRegistry {
                preset: acdd_core::subsystems::SubsystemPreset::Minimal,
                ..Default::default()
            },
            ..Default::default()
        };
        let eng_minimal =
            crate::state_bridge::state_engine_config(vroot, vroot, None, None, &cfg_minimal)
                .unwrap();
        assert_eq!(eng_minimal.code_db_path, None);
        let custom_db = std::path::Path::new(".acdd/custom.sqlite");
        let eng_custom = crate::state_bridge::state_engine_config(
            vroot,
            vroot,
            Some(custom_db),
            None,
            &cfg_default,
        )
        .unwrap();
        assert_eq!(eng_custom.state_db_override, Some(vroot.join(custom_db)));

        // 7. Stateless `acdd tool <name>` (no `--run`/`--node`) dispatches without opening `state.sqlite` even when `modules.state == false` (#30).
        let tdir = ScopedTempDir::create("acdd-ut-cli-tool-stateless").unwrap();
        let troot = tdir.path();
        fs::write(
            troot.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: false\n",
        )
        .unwrap();
        let mut tool_out = Vec::new();
        let tool_cli = Cli::try_parse_from([
            "acdd",
            "--root",
            troot.to_str().unwrap(),
            "tool",
            "missing_plugin",
        ])
        .unwrap();
        let tool_err = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool_cli.run_with_writer(&mut tool_out))
            .unwrap_err();
        let tool_state_err = AcddError::from_anyhow(&tool_err).expect("AcddError");
        assert_eq!(tool_state_err.code, ErrorCode::DomainCapabilityUnavailable);
        assert!(!troot.join(".acdd/state.sqlite").exists());

        // 8. `resolve_cli_adapter_path` contracts.
        let adir = ScopedTempDir::create("acdd-ut-cli-adapter").unwrap();
        let aroot = adir.path().canonicalize().unwrap();
        let cfg_a = aroot.join("a.yaml");
        let cfg_b = aroot.join("b.yaml");
        fs::write(&cfg_a, "modules:\n  code: true\n").unwrap();
        fs::write(&cfg_b, "modules:\n  code: true\n").unwrap();
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(
                &aroot,
                Some(&cfg_a),
                true,
                Some(&cfg_a),
                true
            )
            .unwrap(),
            Some(cfg_a.clone())
        );
        let conflict = super::dispatch_code::resolve_cli_adapter_path(
            &aroot,
            Some(&cfg_b),
            true,
            Some(&cfg_a),
            true,
        )
        .unwrap_err();
        assert!(
            conflict.to_string().contains("CLI_CONFIG_CONFLICT"),
            "{conflict:#}"
        );
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(
                &aroot,
                None,
                false,
                Some(&cfg_a),
                false
            )
            .unwrap(),
            None
        );
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(&aroot, None, false, Some(&cfg_a), true)
                .unwrap(),
            Some(cfg_a)
        );
        // 9. `preset: spec` enables CLI validate/roadmap/draft without state.sqlite; `preset: index` fails closed with FeatureDisabled.
        let pdir = ScopedTempDir::create("acdd-ut-cli-spec-preset").unwrap();
        let proot = pdir.path();
        fs::create_dir_all(proot.join("docs/milestones")).unwrap();
        fs::write(
            proot.join("docs/milestones/AC-001.md"),
            "---\nid: AC-001\ntitle: \"Spec preset\"\ndoc_type: contract\nstatus: planned\n---\n\n### task: t1\n\n```yaml\ntarget: \"Verify\"\nscope: [src/**]\n```\n",
        )
        .unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        fs::write(proot.join("acdd.yaml"), "preset: spec\n").unwrap();
        for argv in [
            vec!["acdd", "--root", proot.to_str().unwrap(), "validate"],
            vec!["acdd", "--root", proot.to_str().unwrap(), "roadmap"],
            vec![
                "acdd",
                "--root",
                proot.to_str().unwrap(),
                "draft",
                "milestones",
                "stateless-draft",
            ],
        ] {
            let mut buf = Vec::new();
            let cli = Cli::try_parse_from(&argv).unwrap();
            rt.block_on(cli.run_with_writer(&mut buf))
                .unwrap_or_else(|e| panic!("preset: spec failed for {argv:?}: {e:#}"));
        }
        assert!(
            !proot.join(".acdd/state.sqlite").exists(),
            "preset: spec must not materialize state.sqlite"
        );
        fs::write(proot.join("acdd.yaml"), "preset: index\n").unwrap();
        for argv in [
            vec!["acdd", "--root", proot.to_str().unwrap(), "validate"],
            vec!["acdd", "--root", proot.to_str().unwrap(), "roadmap"],
            vec![
                "acdd",
                "--root",
                proot.to_str().unwrap(),
                "draft",
                "milestones",
                "blocked-draft",
            ],
        ] {
            let mut buf = Vec::new();
            let cli = Cli::try_parse_from(&argv).unwrap();
            let err = rt
                .block_on(cli.run_with_writer(&mut buf))
                .expect_err("preset: index must reject spec CLI commands");
            assert_eq!(
                AcddError::from_anyhow(&err).expect("AcddError").code,
                ErrorCode::FeatureDisabled,
                "{argv:?}: {err:#}"
            );
        }
    }

    #[test]
    fn cli_writer_handles_broken_pipe_and_docs_check_write_panics() {
        use clap::Parser;
        use std::io::{self, Write};

        struct FailingWriter(io::ErrorKind);
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(self.0, "output unavailable"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let dir = ScopedTempDir::create("acdd-ut-cli-writer").unwrap();
        let root = dir.path();
        fs::write(root.join("acdd.yaml"), "preset: minimal\n").unwrap();
        fs::write(root.join("notes.txt"), "hello\n").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::Other] {
            let cli = Cli::try_parse_from([
                "fixture",
                "--root",
                root.to_str().unwrap(),
                "read",
                "notes.txt",
            ])
            .unwrap();
            let result = rt.block_on(cli.run_with_writer(&mut FailingWriter(kind)));
            if kind == io::ErrorKind::BrokenPipe {
                assert!(result.is_ok());
            } else {
                assert_eq!(
                    result
                        .unwrap_err()
                        .downcast_ref::<io::Error>()
                        .unwrap()
                        .kind(),
                    kind
                );
            }
        }

        for case in ["success", "broken-link", "missing-root"] {
            let ws = ScopedTempDir::create("acdd-ut-docs-writer").unwrap();
            fs::write(
                ws.path().join("README.md"),
                if case == "broken-link" {
                    "[bad](absent.md)\n"
                } else {
                    "# Valid\n"
                },
            )
            .unwrap();
            let target = if case == "missing-root" {
                ws.path().join("absent")
            } else {
                ws.path().to_path_buf()
            };
            let argv = [
                "fixture",
                "--root",
                target.to_str().unwrap(),
                "--format",
                "json",
                "docs",
                "check",
            ];
            let result = rt.block_on(
                Cli::try_parse_from(argv)
                    .unwrap()
                    .run_with_writer(&mut FailingWriter(io::ErrorKind::BrokenPipe)),
            );
            if case == "success" {
                assert!(result.is_ok(), "{case}: {result:?}");
            } else {
                let err = result.expect_err(case);
                assert!(
                    err.to_string().contains("documentation check failed"),
                    "{case}: {err}"
                );
            }
            let non_pipe = rt.block_on(
                Cli::try_parse_from(argv)
                    .unwrap()
                    .run_with_writer(&mut FailingWriter(io::ErrorKind::Other)),
            );
            assert_eq!(
                non_pipe
                    .unwrap_err()
                    .downcast_ref::<io::Error>()
                    .unwrap()
                    .kind(),
                io::ErrorKind::Other,
                "{case}"
            );
        }

        // Verify guide CLI examples and core subcommands parse cleanly via Clap.
        for args in [
            vec!["start", "--goal", "Test ephemeral run", "--in-place"],
            vec!["run", "--jobs", "2"],
            vec!["verify", "t1.build", "--test", "tests/seam.rs::case"],
            vec![
                "subtask",
                "t1",
                "slice",
                "--status",
                "completed",
                "--evidence",
                "observed-command",
            ],
            vec!["read", "--selector", "ExampleSym"],
            vec!["read", "--selector", "ExampleSym", "--path", "src/lib.rs"],
            vec!["read", "--mode", "search", "--query", "needle", "--path", "src"],
            vec!["query", "search", "parse*", "--path", "src/"],
            vec!["query", "run", "duplicates", "--level", "symbol", "--path", "src"],
            vec!["docs", "get", "README.md", "--detail", "compact"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("acdd").chain(args)).is_ok());
        }
        let dup_cli =
            Cli::try_parse_from(["acdd", "query", "run", "duplicates", "--path", "src"]).unwrap();
        match dup_cli.command {
            Some(super::Command::Query {
                command: super::QueryCommand::Run { min_lines, .. },
            }) => assert_eq!(min_lines, None),
            other => panic!("expected QueryCommand::Run, got {other:?}"),
        }
    }
}
