mod args;
/// Implements ast support.
pub mod ast;
pub(crate) mod discovery;
mod dispatch_code;
mod dispatch_state;
pub(crate) mod docs_check;
/// Implements guide support.
pub mod guide;
mod paths;
/// Sends a reload signal to running MCP servers.
pub mod reload;
/// Fail-fast ACDD profile and spec validation.
pub mod validate;

pub use args::{AstCommand, Cli, CliOutputFormat, Command, DocsCommand, QueryCommand};

use crate::{
    engine::pipeline as writer,
    format::{self, OperationKind, OutputFormat},
    state_bridge::InvocationSettings,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::subsystems::SubsystemId;
use anyhow::{bail, Result};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// Default code-map SQLite path under `root`.
pub fn default_db(root: &Path) -> PathBuf {
    acdd_core::config::resolve_code_db_path(root, None)
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
        Command::Guide { .. } => (OperationKind::Guide, "acdd_guide", None),
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
            command: QueryCommand::Analyze { target },
        } => (
            if dispatch_code::looks_like_sql(target) {
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

fn check_explicit_config_migration(
    root: &Path,
    config_path: &Path,
) -> std::result::Result<(), StateError> {
    use std::io::Read;
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.read(true);
    let opened = if let Ok(rel) = config_path.strip_prefix(root) {
        acdd_core::fs::open_confined(root, rel, &mut open_opts)
    } else if let (Some(parent), Some(name)) = (config_path.parent(), config_path.file_name()) {
        acdd_core::fs::open_confined(parent, Path::new(name), &mut open_opts)
    } else {
        return Ok(());
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!(
                    "cannot inspect confined config '{}': {err}",
                    config_path.display()
                ),
            ));
        }
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Ok(());
    }
    let mut raw = String::new();
    if file.read_to_string(&mut raw).is_err() {
        return Ok(());
    }
    let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return Ok(());
    };
    if let Some(map) = yaml.as_mapping() {
        for legacy_key in acdd_core::config::LEGACY_05_KEYS {
            if map.contains_key(serde_yaml::Value::String((*legacy_key).to_owned())) {
                return Err(StateError::new(
                    ErrorCode::ConfigMigrationRequired,
                    format!(
                        "legacy 0.5.x key '{legacy_key}' found in '{}'",
                        config_path.display()
                    ),
                )
                .with_hint(
                    "Follow docs/runbooks/migration-0.6.md to update 0.5.x configuration keys",
                ));
            }
        }
    }
    Ok(())
}

fn ensure_migrated_preflight(root: &Path, explicit_config: Option<&Path>) -> Result<()> {
    check_explicit_config_migration(root, &root.join(acdd_core::ACDD_CONFIG_FILE))?;
    if let Some(cfg) = explicit_config {
        let resolved = if cfg.is_absolute() {
            cfg.to_path_buf()
        } else {
            root.join(cfg)
        };
        if resolved != root.join(acdd_core::ACDD_CONFIG_FILE) {
            check_explicit_config_migration(root, &resolved)?;
        }
    }
    Ok(())
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

impl Cli {
    /// Run the parsed CLI command and terminate with canonical `StateError` exit codes (`2`/`3`/`4`).
    pub async fn run(self) -> Result<()> {
        match self.run_with_writer(&mut io::stdout()).await {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(state_err) = StateError::from_anyhow(&error) {
                    eprintln!("{error:#}");
                    std::process::exit(state_err.exit_code());
                }
                Err(error)
            }
        }
    }

    /// Runs CLI dispatch and formatting with a caller-provided output writer.
    pub async fn run_with_writer<W: io::Write>(self, output: &mut W) -> Result<()> {
        self.run_inner(output, None).await
    }

    /// Runs CLI dispatch with explicit output and extraction-cache locations.
    pub async fn run_with_writer_and_cache_dir<W: io::Write>(
        self,
        output: &mut W,
        cache_dir: &Path,
    ) -> Result<()> {
        self.run_inner(output, Some(cache_dir)).await
    }

    async fn run_inner(
        self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
    ) -> Result<()> {
        if matches!(self.command.as_ref(), Some(Command::Snapshot { .. })) {
            if let Some(root) = self.root.as_deref() {
                acdd_core::fs::check_ancestors_no_symlinks(root, Path::new(".git"))
                    .map_err(|e| anyhow::anyhow!(e))?;
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
        let root = match command_root(self.root.as_deref(), positional_root) {
            Ok(root) => root,
            Err(error) => {
                if matches!(
                    self.command.as_ref(),
                    Some(Command::Docs {
                        command: DocsCommand::Check
                    })
                ) {
                    return docs_check::check(
                        self.root.as_deref().unwrap_or_else(|| Path::new(".")),
                        OutputFormat::from(self.format),
                        output,
                    );
                }
                return Err(error);
            }
        };

        // Enforce CONFIG_MIGRATION_REQUIRED preflight on every command.
        ensure_migrated_preflight(&root, self.config.as_deref())?;

        // `init` and `guide init` must work even before `.acdd/profile.yaml` exists.
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
            return Ok(());
        }
        if let Some(Command::Guide { topic, force }) = self.command.as_ref() {
            if topic.trim() == "init" {
                let result = guide::run(&root, topic, *force)?;
                let formatted = format::render_for_operation(
                    &result,
                    OutputFormat::from(self.format),
                    "acdd_guide",
                    None,
                    OperationKind::Guide,
                );
                writeln!(output, "{formatted}")?;
                return Ok(());
            }
        }

        // `validate` handles potentially broken configs/profiles directly without ResolvedPaths pre-failing.
        if let Some(Command::Validate { path }) = self.command {
            let explicit_config = self.config.as_deref().map(|p| {
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                }
            });
            let explicit_profile = self
                .profile
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned());
            let result = validate::run_with_options(
                &root,
                path.as_deref(),
                explicit_config.as_deref(),
                explicit_profile.as_deref(),
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
                return Err(StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    "VALIDATION_FAILED: one or more documents failed validation",
                )
                .into());
            }
            return Ok(());
        }

        let profile_at_root = match &self.command {
            Some(Command::Start {
                spec: Some(_),
                worktree_path: Some(_),
                ..
            }) => None,
            _ => self.profile.as_deref(),
        };
        let settings =
            paths::ResolvedPaths::resolve(&root, self.config.as_deref(), profile_at_root)?;
        let database = acdd_core::config::resolve_code_db_path(&root, self.db.as_deref());
        let state_db = self.state_db.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });
        let profile_argument = self.profile.as_ref().map(|path| {
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
            settings,
            database,
            state_db,
            profile_argument,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_selected(
        self,
        output: &mut dyn io::Write,
        cache_dir: Option<&Path>,
        root: PathBuf,
        settings: paths::ResolvedPaths,
        db: PathBuf,
        state_db: Option<PathBuf>,
        profile_argument: Option<PathBuf>,
    ) -> Result<()> {
        let output_format = OutputFormat::from(self.format);
        let command = self.command.unwrap_or(Command::Serve);
        if matches!(
            &command,
            Command::Docs {
                command: DocsCommand::Check
            }
        ) {
            if !settings.adapter.modules.is_subsystem_enabled(SubsystemId::Docs) {
                return Err(StateError::new(
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
            profile_ref: explicit_profile_ref,
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
        if let Ok(db) =
            crate::state_bridge::resolve_effective_state_db(&root, state_db.as_deref(), &adapter)
        {
            span = span.with_state_db(db);
        }
        let checkout_root = self.root.as_deref().map(std::path::absolute).transpose()?;
        let state_ctx = dispatch_state::CliStateCtx {
            root: &root,
            checkout_root: checkout_root.as_deref().unwrap_or(&root),
            explicit_config: self.config.is_some(),
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
                Command::Init { .. } | Command::Validate { .. } => {
                    unreachable!("handled prior to run_selected")
                }
                Command::Serve => {
                    writer::validate_code_map_target(&root, &db)?;
                    let invocation = InvocationSettings {
                        root: root.clone(),
                        config: explicit_config.clone(),
                        profile: profile_argument,
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
                Command::Guide { topic, force } => {
                    Ok((guide::run_with_adapter(&root, &topic, force, &adapter)?, None))
                }
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
                    explicit_profile_ref,
                    self.profile.as_deref(),
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

        if output_format == OutputFormat::Markdown {
            if let Some(kind) = discovery {
                discovery::attach_presentation_uris(&root, &adapter, &mut result, kind);
            }
            if matches!(title, "search_docs" | "get_doc") {
                discovery::attach_document_presentation_uris(&root, &adapter, &mut result, title);
            }
            if title == "code_map_lint" {
                discovery::attach_lint_presentation_uris(&root, &adapter, &mut result);
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
                discovery::attach_traversal_presentation_uris(&root, &adapter, &mut result);
            }
        }
        let lint_errors = (title == "code_map_lint")
            .then(|| result.pointer("/summary/errors").and_then(Value::as_u64))
            .flatten()
            .filter(|&e| e > 0);
        let formatted = if title == "acdd_read" {
            let debug_snapshot = adapter.debug.then(|| result.clone());
            let bounded = crate::mcp::response::result_for_presented(
                Ok(result),
                &adapter.response,
                output_format,
                operation,
                title,
                explain,
                discovery,
            );
            let text = bounded
                .content
                .first()
                .and_then(rmcp::model::ContentBlock::as_text)
                .map(|block| block.text.as_str())
                .ok_or_else(|| anyhow::anyhow!("acdd_read response has no text content"))?;
            if bounded.is_error == Some(true) {
                bail!("{text}");
            }
            if let Some(debug_result) = debug_snapshot {
                crate::telemetry::log_cli(&root, &cli_command_line(), None, &debug_result);
            }
            text.to_owned()
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
    use acdd_core::errors::{ErrorCode, StateError};
    use acdd_core::fs::ScopedTempDir;
    use clap::Parser;
    use std::fs;

    #[test]
    fn cli_exit_codes_preflight_init_validate_and_flat_dispatch() {
        // 1. Exact ErrorCode -> exit code mapping (v7 §4.3: 2/3/4)
        let exit_cases = [
            (ErrorCode::ConfigMigrationRequired, 2),
            (ErrorCode::ProfileSchemaInvalid, 2),
            (ErrorCode::TaskAlreadyClaimed, 3),
            (ErrorCode::OutOfScopeChanges, 3),
            (ErrorCode::GateVerificationFailed, 4),
            (ErrorCode::BackendConflict, 4),
        ];
        for (code, expected) in exit_cases {
            assert_eq!(
                StateError::new(code, "test").exit_code(),
                expected,
                "{code:?}"
            );
        }

        // 2. Flat CLI subcommand parsing preserves Cli/Command AST & removes legacy trees
        for args in [
            vec!["acdd", "init"],
            vec!["acdd", "start", "--goal", "ship feature", "--name", "m1"],
            vec![
                "acdd",
                "start",
                "--goal",
                "ship feature",
                "--name",
                "m1",
                "--profile",
                "audit",
            ],
            vec!["acdd", "draft", "milestones", "my-slug"],
            vec![
                "acdd",
                "draft",
                "milestones",
                "my-slug",
                "--profile",
                "code",
            ],
            vec!["acdd", "status", "--run", "m1"],
            vec!["acdd", "list"],
            vec!["acdd", "show", "--run", "m1"],
            vec!["acdd", "next", "--run", "m1"],
            vec!["acdd", "roadmap"],
            vec!["acdd", "roadmap", "--format", "json"],
            vec!["acdd", "claim", "t1.build", "--run", "m1", "--worker", "w1"],
            vec!["acdd", "verify", "t1.build", "--run", "m1"],
            vec!["acdd", "submit", "t1.build", "--run", "m1", "--action", "pass"],
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
            assert!(Cli::try_parse_from(&args).is_ok(), "failed to parse {args:?}");
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
            let cli = Cli::try_parse_from(["acdd", "--root", root.to_str().unwrap(), "init"])
                .unwrap();
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(cli.run_with_writer(&mut out))
                .unwrap();

            let profile = fs::read_to_string(root.join(".acdd/profile.yaml")).unwrap();
            assert!(profile.contains(expected_inherit), "{profile}");
            let gi = fs::read_to_string(root.join(".gitignore")).unwrap();
            assert!(gi.contains(".acdd/*.sqlite*"), "{gi}");
            assert!(
                acdd_core::git::git_command(root, &["check-ignore", "-q", ".acdd/state.sqlite"])
                    .unwrap()
                    .status
                    .success()
            );

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
                assert!(fs::read_to_string(root.join(".acdd/profile.yaml"))
                    .unwrap()
                    .contains("inherit: code"));
            }
        }

        // 4. `CONFIG_MIGRATION_REQUIRED` preflight blocks legacy 0.5.x config keys
        let dir = ScopedTempDir::create("acdd-ut-cli-migrate").unwrap();
        let root = dir.path();
        fs::write(
            root.join("acdd.yaml"),
            "milestones:\n  path: docs/milestones\ntasks_db: .acdd/tasks.sqlite\n",
        )
        .unwrap();
        let err = super::ensure_migrated_preflight(root, None).unwrap_err();
        let state_err = StateError::from_anyhow(&err).expect("StateError");
        assert_eq!(state_err.code, ErrorCode::ConfigMigrationRequired);
        assert_eq!(state_err.exit_code(), 2);

        // 5. `acdd validate` succeeds on valid workspace with bound commands and fails on unbound/broken profile
        let vdir = ScopedTempDir::create("acdd-ut-cli-validate").unwrap();
        let vroot = vdir.path();
        fs::create_dir_all(vroot.join(".acdd")).unwrap();
        fs::write(
            vroot.join("acdd.yaml"),
            "modules:\n  code: true\n  docs: true\n  state: true\n",
        )
        .unwrap();
        fs::write(
            vroot.join(".acdd/profile.yaml"),
            "version: 2\ninherit: code\ncommands:\n  test: \"cargo test --lib {test_filter}\"\n  build_check: \"cargo check --lib\"\n  verify_all: \"cargo test --lib\"\n",
        )
        .unwrap();
        let val = super::validate::run(vroot, None).unwrap();
        assert_eq!(val["valid"], serde_json::json!(true), "{val}");

        fs::write(vroot.join(".acdd/profile.yaml"), "version: 2\ninherit: code\n").unwrap();
        let unbound = super::validate::run(vroot, None).unwrap();
        assert_eq!(unbound["valid"], serde_json::json!(false), "{unbound}");
        assert_eq!(unbound["diagnostics"][1]["code"], "COMMAND_UNBOUND");

        fs::write(vroot.join(".acdd/profile.yaml"), "version: 1\nbogus: true\n").unwrap();
        let bad = super::validate::run(vroot, None).unwrap();
        assert_eq!(bad["valid"], serde_json::json!(false), "{bad}");

        // 6. Default `.acdd/state.sqlite` keeps `state_db_override == None` (enabling `state.bak.sqlite`) even from linked worktrees,
        // and `code_db_path` is populated iff `code || docs` is enabled (#25).
        let cfg_default = crate::state_bridge::AcddConfig::default();
        let eng_default =
            crate::state_bridge::state_engine_config(vroot, vroot, None, None, &cfg_default)
                .unwrap();
        assert!(eng_default.state_db_override.is_none());
        assert_eq!(
            eng_default.code_db_path,
            Some(acdd_core::config::resolve_code_db_path(vroot, None))
        );
        let cfg_minimal = crate::state_bridge::AcddConfig {
            modules: acdd_core::subsystems::SubsystemRegistry {
                profile: acdd_core::subsystems::SubsystemPreset::Minimal,
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
        let tool_state_err = StateError::from_anyhow(&tool_err).expect("StateError");
        assert_eq!(tool_state_err.code, ErrorCode::DomainCapabilityUnavailable);
        assert!(!troot.join(".acdd/state.sqlite").exists());

        // 8. `resolve_cli_adapter_path` and `discovery::includes_discovery` contracts.
        let adir = ScopedTempDir::create("acdd-ut-cli-adapter").unwrap();
        let aroot = adir.path().canonicalize().unwrap();
        let cfg_a = aroot.join("a.yaml");
        let cfg_b = aroot.join("b.yaml");
        fs::write(&cfg_a, "modules:\n  code: true\n").unwrap();
        fs::write(&cfg_b, "modules:\n  code: true\n").unwrap();
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(&aroot, Some(&cfg_a), true, Some(&cfg_a), true).unwrap(),
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
        assert!(conflict.to_string().contains("CLI_CONFIG_CONFLICT"), "{conflict:#}");
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(&aroot, None, false, Some(&cfg_a), false).unwrap(),
            None
        );
        assert_eq!(
            super::dispatch_code::resolve_cli_adapter_path(&aroot, None, false, Some(&cfg_a), true).unwrap(),
            Some(cfg_a)
        );
        assert!(super::discovery::includes_discovery(None));
        assert!(super::discovery::includes_discovery(Some(&["COMPONENTS".into()])));
        assert!(super::discovery::includes_discovery(Some(&["milestones".into()])));
        assert!(!super::discovery::includes_discovery(Some(&["counts".into(), "cycles".into()])));
    }
}
