//! Role runner subprocess spawning (`stdin` Markdown prompt, `ACDD_*` env, session reuse).

use crate::profile::RoleDef;
use super::process::{
    node_slug, open_run_log_file, run_isolated_process, shell_command, BoundedLogWriter,
};
use crate::types::ClaimOutcome;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::format::sha256_hex;
use acdd_core::git::scrub_git_env;
use acdd_core::ids::{NodeCursor, RunId, WorkerId};
use acdd_core::telemetry::unix_epoch_ms;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Borrowed parameters for spawning a configured `RoleDef.runner` process without cloning owned strings or paths.
#[derive(Debug, Clone, Copy)]
pub struct RoleSpawnBorrow<'a> {
    /// Owning run identifier.
    pub run_id: &'a RunId,
    /// Target node cursor.
    pub node_cursor: &'a NodeCursor,
    /// Owning container ID (`task:<t>` or `root:<c>`) for `reuse_on_reject` session scoping.
    pub node_container_id: &'a str,
    /// Active claim revision.
    pub claim_revision: u32,
    /// Role name (`architect`, `builder`, `independent_reviewer`, etc.).
    pub role_name: &'a str,
    /// Resolved role definition (`runner`, `reuse_on_reject`, `prompt_template`, etc.).
    pub role_def: &'a RoleDef,
    /// Assigned worker ID for this invocation.
    pub worker_id: &'a WorkerId,
    /// Node worktree path.
    pub worktree: &'a Path,
    /// Primary workspace root.
    pub primary_worktree: &'a Path,
    /// State SQLite path passed via `ACDD_STATE_DB`.
    pub state_db_path: &'a Path,
    /// Optional code-map SQLite path passed via `ACDD_CODE_DB`.
    pub code_db_path: Option<&'a Path>,
    /// W3C `TRACEPARENT` propagated to the agent runner.
    pub trace_parent: &'a str,
    /// Rendered Markdown prompt delivered via `stdin`.
    pub prompt_markdown: &'a str,
    /// Timeout ceiling in seconds.
    pub timeout_seconds: u64,
    /// Maximum log bytes retained in `.acdd/run/<run_id>/logs/`.
    pub max_log_bytes: usize,
    /// Maximum log line length in bytes.
    pub max_line_bytes: usize,
}

/// Result returned by `spawn_role_runner_borrowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpawnOutcome {
    /// Process exit code (`None` on timeout/signal).
    pub exit_code: Option<i32>,
    /// Whether the runner timed out.
    pub timed_out: bool,
    /// `ACDD_AGENT_SESSION_ID` assigned (or reused) for this invocation.
    pub agent_session_id: String,
    /// Log path relative to `.acdd/run/<run_id>/`.
    pub log_relpath: String,
}

/// Spawn a role runner with `stdin` prompt delivery, `ACDD_*` env, and process-group timeout enforcement.
pub fn spawn_role_runner_borrowed(
    req: &RoleSpawnBorrow<'_>,
    session_cache: &mut BTreeMap<(RunId, String, String), String>,
) -> Result<RoleSpawnOutcome, StateError> {
    spawn_role_runner_borrowed_inner(req, session_cache, None)
}

pub(super) fn spawn_role_runner_borrowed_inner(
    req: &RoleSpawnBorrow<'_>,
    session_cache: &mut BTreeMap<(RunId, String, String), String>,
    timeout_override: Option<Duration>,
) -> Result<RoleSpawnOutcome, StateError> {
    let Some(runner_cmd) = req
        .role_def
        .runner
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(StateError::new(
            ErrorCode::WorkflowRunnerRequired,
            format!(
                "role '{}' has no configured runner command; configure roles.{}.runner before automated run",
                req.role_name, req.role_name
            ),
        ));
    };

    if !req.worktree.is_dir() {
        return Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "role runner worktree '{}' does not exist",
                req.worktree.display()
            ),
        ));
    }

    let cache_key = (
        req.run_id.clone(),
        req.node_container_id.to_owned(),
        req.role_name.to_owned(),
    );
    let agent_session_id = if req.role_def.reuse_on_reject {
        if let Some(existing) = session_cache.get(&cache_key) {
            existing.clone()
        } else {
            let fresh = generate_agent_session_id(req.run_id, req.role_name);
            session_cache.insert(cache_key, fresh.clone());
            fresh
        }
    } else {
        generate_agent_session_id(req.run_id, req.role_name)
    };

    let filename = format!(
        "{}-rev{}-role.log",
        node_slug(req.node_cursor),
        req.claim_revision
    );
    let (_log_path, log_relpath, file) =
        open_run_log_file(req.primary_worktree, req.run_id, &filename)?;
    let mut log_writer = BoundedLogWriter::new(file, req.max_log_bytes);

    let mut cmd = shell_command(runner_cmd);
    cmd.current_dir(req.worktree);
    scrub_git_env(&mut cmd);

    cmd.env("ACDD_RUN_ID", req.run_id.as_str())
        .env("ACDD_NODE_CURSOR", req.node_cursor.to_string())
        .env("ACDD_CLAIM_REVISION", req.claim_revision.to_string())
        .env("ACDD_WORKER_ID", req.worker_id.as_str())
        .env("ACDD_AGENT_SESSION_ID", &agent_session_id)
        .env("ACDD_WORKTREE", req.worktree)
        .env("ACDD_STATE_DB", req.state_db_path)
        .env("ACDD_ROLE_MODE", req.role_def.mode.to_string())
        .env("TRACEPARENT", req.trace_parent);

    if req.role_def.models.is_empty() {
        cmd.env_remove("ACDD_ROLE_MODELS")
            .env_remove("ACDD_MODEL")
            .env_remove("ACDD_REASONING");
    } else {
        let joined = req
            .role_def
            .models
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        cmd.env("ACDD_ROLE_MODELS", joined)
            .env("ACDD_MODEL", &req.role_def.models[0].model);
        match req.role_def.models[0].reasoning {
            Some(ref reasoning) => {
                cmd.env("ACDD_REASONING", reasoning);
            }
            None => {
                cmd.env_remove("ACDD_REASONING");
            }
        }
    }

    match req.code_db_path {
        Some(code_db) => {
            cmd.env("ACDD_CODE_DB", code_db);
        }
        None => {
            cmd.env_remove("ACDD_CODE_DB");
        }
    }

    let timeout = timeout_override.unwrap_or_else(|| Duration::from_secs(req.timeout_seconds.max(1)));
    let outcome = run_isolated_process(
        &mut cmd,
        Some(req.prompt_markdown.as_bytes()),
        timeout,
        req.max_line_bytes,
        &mut |_stream, line| {
            log_writer.write_line(line).map_err(|err| {
                StateError::new(
                    ErrorCode::GateVerificationFailed,
                    format!("failed writing role runner log: {err}"),
                )
            })
        },
    )?;

    Ok(RoleSpawnOutcome {
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        agent_session_id,
        log_relpath,
    })
}

fn generate_agent_session_id(run_id: &RunId, role_name: &str) -> String {
    let seq = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = unix_epoch_ms();
    let mut payload = Vec::with_capacity(run_id.as_str().len() + role_name.len() + 30);
    payload.extend_from_slice(run_id.as_str().as_bytes());
    payload.push(b':');
    payload.extend_from_slice(role_name.as_bytes());
    payload.push(b':');
    payload.extend_from_slice(&std::process::id().to_le_bytes());
    payload.extend_from_slice(&seq.to_le_bytes());
    payload.extend_from_slice(&millis.to_le_bytes());
    let digest = sha256_hex(&payload);
    format!("acdd-session-{}", &digest[..24])
}

/// Verify that the runner exited `0` AND closed the active claim (`Passed` or `Rejected`).
pub fn verify_runner_submitted(
    outcome: &RoleSpawnOutcome,
    claim_outcome_after: Option<ClaimOutcome>,
) -> Result<(), StateError> {
    if outcome.timed_out || outcome.exit_code != Some(0) {
        return Err(StateError::new(
            ErrorCode::RunnerNoSubmission,
            format!(
                "role runner failed without submission (exit_code={:?}, timed_out={})",
                outcome.exit_code, outcome.timed_out
            ),
        ));
    }
    match claim_outcome_after {
        Some(ClaimOutcome::Passed | ClaimOutcome::Rejected) => Ok(()),
        other => Err(StateError::new(
            ErrorCode::RunnerNoSubmission,
            format!("role runner exited 0 without submitting gate (claim_outcome={other:?})"),
        )),
    }
}
