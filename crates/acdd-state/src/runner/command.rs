//! Bounded gate command execution, process-group isolation, and env sanitization.

use crate::profile::{
    render_command_with_scope_target, render_command_with_test_filter, resolve_command_for_scope,
    CommandDef, ResolvedCommandInvocation, ResourceLimits,
};
use crate::runner::{
    node_slug, open_run_log_file, run_isolated_process, shell_command, utf8_tail, BoundedLogWriter,
    StreamKind,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Parameters for executing a resolved gate command slot.
#[derive(Debug, Clone)]
pub struct CommandExecReq {
    /// Owning run identifier.
    pub run_id: RunId,
    /// Target node cursor.
    pub node_cursor: NodeCursor,
    /// Active claim revision.
    pub claim_revision: u32,
    /// Working directory where the command executes.
    pub worktree: PathBuf,
    /// Primary workspace root holding `.acdd/run/<run_id>/logs/`.
    pub primary_worktree: PathBuf,
    /// Configured command slot name.
    pub command_id: String,
    /// Resolved command definition (`Script`, `Detailed`, or `Pattern`).
    pub command: CommandDef,
    /// Admitted scope globs/paths used for pattern matching and `{scope_target}`.
    pub scope: Vec<String>,
    /// Profile-level path-pattern `{scope_target}` argument fallbacks.
    pub profile_scope_args: BTreeMap<String, String>,
    /// Optional `--test` target supplied by caller.
    pub test_target: Option<String>,
    /// Resource and timeout limits from `profile.limits`.
    pub limits: ResourceLimits,
}

/// Captured execution outcome and bounded stderr diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecOutcome {
    /// Exact shell command line executed.
    pub command_line: String,
    /// Process exit code (`None` on signal/timeout).
    pub exit_code: Option<i32>,
    /// Whether execution exceeded timeout and was killed via `kill(-pgid, SIGKILL)`.
    pub timed_out: bool,
    /// Whether execution was aborted externally.
    pub aborted: bool,
    /// Bounded tail of stderr for diagnostics.
    pub stderr_tail: String,
    /// Log path relative to `.acdd/run/<run_id>/`.
    pub log_relpath: String,
}

/// Scrub ambient orchestrator and Git variables from project verification child processes.
pub fn sanitize_command_process(cmd: &mut Command) {
    for key in [
        "TRACEPARENT",
        "ACDD_CONFIG",
        "ACDD_PROFILE",
        "ACDD_STATE_DB",
        "ACDD_CODE_DB",
        "ACDD_WORKSPACE_ROOT",
        "ACDD_AGENT_SESSION_ID",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(key);
    }

    let git_env_keys: Vec<OsString> = std::env::vars_os()
        .filter_map(|(k, _)| k.to_string_lossy().starts_with("GIT_").then_some(k))
        .collect();
    for key in git_env_keys {
        cmd.env_remove(key);
    }

    let cmd_git_keys: Vec<OsString> = cmd
        .get_envs()
        .filter(|(k, _)| k.to_string_lossy().starts_with("GIT_"))
        .map(|(k, _)| k.to_os_string())
        .collect();
    for key in cmd_git_keys {
        cmd.env_remove(key);
    }
}

fn invocation_match_paths<'a>(
    inv: &'a ResolvedCommandInvocation,
    admitted_scope: &'a [String],
) -> &'a [String] {
    if inv.matched_paths.is_empty() {
        admitted_scope
    } else {
        &inv.matched_paths
    }
}

/// Render one resolved command invocation into one or more independent shell commands by
/// expanding `{scope_target}` per unique matched target before `{test_filter}`.
pub(crate) fn render_invocation_command(
    inv: &ResolvedCommandInvocation,
    admitted_scope: &[String],
    profile_scope_args: &BTreeMap<String, String>,
    test_target: Option<&str>,
) -> Result<Vec<String>, StateError> {
    let match_paths = invocation_match_paths(inv, admitted_scope);
    let effective_scope_args: Cow<'_, BTreeMap<String, String>> = if profile_scope_args.is_empty() {
        Cow::Borrowed(&inv.scope_args_by_pattern)
    } else if inv.scope_args_by_pattern.is_empty() {
        Cow::Borrowed(profile_scope_args)
    } else {
        let mut merged = profile_scope_args.clone();
        for (k, v) in &inv.scope_args_by_pattern {
            merged.insert(k.clone(), v.clone());
        }
        Cow::Owned(merged)
    };
    let cmds_with_scope =
        render_command_with_scope_target(&inv.command, &effective_scope_args, match_paths)?;
    cmds_with_scope
        .into_iter()
        .map(|cmd| render_command_with_test_filter(&cmd, inv.test_filter.as_ref(), test_target))
        .collect()
}

/// Execute gate command(s) in an isolated process group with bounded line/log capture.
pub fn execute_gate_commands(req: &CommandExecReq) -> Result<CommandExecOutcome, StateError> {
    execute_gate_commands_inner(req, None)
}

pub(super) fn execute_gate_commands_inner(
    req: &CommandExecReq,
    timeout_override: Option<Duration>,
) -> Result<CommandExecOutcome, StateError> {
    if !req.worktree.is_dir() {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "command worktree '{}' does not exist",
                req.worktree.display()
            ),
        ));
    }

    let invocations = resolve_command_for_scope(&req.command, &req.scope)?;
    if invocations.is_empty() {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "command slot '{}' matched no paths in admitted scope",
                req.command_id
            ),
        ));
    }

    let filename = format!(
        "{}-rev{}.log",
        node_slug(&req.node_cursor),
        req.claim_revision
    );
    let (_log_path, log_relpath, file) =
        open_run_log_file(&req.primary_worktree, &req.run_id, &filename)?;
    let mut log_writer = BoundedLogWriter::new(file, req.limits.max_log_bytes);

    let mut rendered_commands = Vec::with_capacity(invocations.len());
    let mut last_exit_code = None;
    let mut timed_out = false;
    let max_capture_bytes = req.limits.max_capture_bytes;
    let mut captured_bytes = 0usize;
    let mut stderr_retained = String::new();

    for inv in &invocations {
        let rendered_cmds = render_invocation_command(
            inv,
            &req.scope,
            &req.profile_scope_args,
            req.test_target.as_deref(),
        )?;
        let timeout = timeout_override.unwrap_or_else(|| {
            let secs = inv
                .timeout_seconds
                .unwrap_or(req.limits.max_command_timeout_seconds)
                .min(req.limits.max_command_timeout_seconds)
                .max(1);
            Duration::from_secs(secs)
        });

        for rendered_cmd in rendered_cmds {
            rendered_commands.push(rendered_cmd.clone());

            let mut cmd = shell_command(&rendered_cmd);
            cmd.current_dir(&req.worktree);
            sanitize_command_process(&mut cmd);

            let outcome = run_isolated_process(
                &mut cmd,
                None,
                timeout,
                req.limits.max_line_bytes,
                &mut |stream, line| {
                    log_writer.write_line(line).map_err(|err| {
                        StateError::new(
                            ErrorCode::GateVerificationFailed,
                            format!("failed writing command log: {err}"),
                        )
                    })?;
                    observe_bounded_output(
                        stream,
                        line,
                        max_capture_bytes,
                        &mut captured_bytes,
                        &mut stderr_retained,
                    );
                    Ok(())
                },
            )?;

            last_exit_code = outcome.exit_code;
            timed_out = outcome.timed_out;

            if timed_out || last_exit_code != Some(0) {
                break;
            }
        }

        if timed_out || last_exit_code != Some(0) {
            break;
        }
    }

    let stderr_tail = utf8_tail(&stderr_retained, req.limits.diagnostic_inline_bytes).to_owned();

    Ok(CommandExecOutcome {
        command_line: rendered_commands.join(" && "),
        exit_code: last_exit_code,
        timed_out,
        aborted: false,
        stderr_tail,
        log_relpath,
    })
}

fn observe_bounded_output(
    stream: StreamKind,
    line: &str,
    max_capture_bytes: usize,
    captured_bytes: &mut usize,
    stderr_retained: &mut String,
) {
    if *captured_bytes < max_capture_bytes {
        let avail = max_capture_bytes - *captured_bytes;
        let take = line.len().min(avail);
        let cut = crate::runner::utf8_floor_boundary(line, take);
        *captured_bytes = captured_bytes.saturating_add(cut + 1);
        if stream == StreamKind::Stderr {
            if !stderr_retained.is_empty() {
                stderr_retained.push('\n');
            }
            stderr_retained.push_str(&line[..cut]);
        }
    } else if stream == StreamKind::Stderr {
        // Even if capture limit is reached, preserve bounded stderr tail for diagnostics.
        if !stderr_retained.is_empty() {
            stderr_retained.push('\n');
        }
        stderr_retained.push_str(line);
        if stderr_retained.len() > max_capture_bytes {
            *stderr_retained = utf8_tail(stderr_retained, max_capture_bytes).to_owned();
        }
    }
}
