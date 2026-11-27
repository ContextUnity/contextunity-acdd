//! Stdio MCP session lifecycle, parent watchdog, and SIGHUP/binary-drift re-exec.

use super::types::{ReloadSignal, Server};
use anyhow::Result;
use rmcp::ServiceExt;
use std::{path::PathBuf, sync::Arc};

const MCP_SESSION_ENV: &str = "ACDD_MCP_SESSION";
#[cfg(unix)]
const MCP_RELOAD_PARENT_ENV: &str = "ACDD_MCP_RELOAD_PARENT";

fn resumed_initialize_params(server: &Server) -> Option<rmcp::model::InitializeRequestParams> {
    let raw = std::env::var(MCP_SESSION_ENV).ok()?;
    std::env::remove_var(MCP_SESSION_ENV);
    let params = serde_json::from_str::<rmcp::model::InitializeRequestParams>(&raw).ok()?;
    let supported = rmcp::Service::supported_protocol_versions(server);
    supported
        .iter()
        .any(|version| version == &params.protocol_version)
        .then_some(params)
}

fn preserved_session(
    service: &rmcp::service::RunningService<rmcp::RoleServer, Server>,
) -> Option<String> {
    let info = service.peer_info()?;
    match serde_json::to_string(info.as_ref()) {
        Ok(json) => Some(json),
        Err(error) => {
            eprintln!("SIGHUP session preserve failed: {error}");
            None
        }
    }
}

#[cfg(unix)]
fn parent_is_alive(initial_parent_pid: u32) -> bool {
    initial_parent_pid <= 1 || std::os::unix::process::parent_id() == initial_parent_pid
}

#[cfg(unix)]
async fn parent_exited(initial_parent_pid: u32) {
    while parent_is_alive(initial_parent_pid) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Serves stdio retaining explicit 0.6.0 [`crate::state_bridge::InvocationSettings`] across requests.
pub async fn serve_with_state_config(
    root: PathBuf,
    db: PathBuf,
    cache_dir: Option<PathBuf>,
    settings: Option<crate::state_bridge::InvocationSettings>,
) -> Result<()> {
    #[cfg(unix)]
    {
        let reload_parent = std::env::var(MCP_RELOAD_PARENT_ENV).ok();
        std::env::remove_var(MCP_RELOAD_PARENT_ENV);
        let initial_parent_pid = reload_parent
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_else(std::os::unix::process::parent_id);
        tokio::select! {
            result = serve_session(root, db, cache_dir, settings, initial_parent_pid) => result,
            _ = parent_exited(initial_parent_pid) => {
                eprintln!(
                    "acdd: parent process ({initial_parent_pid}) exited; shutting down"
                );
                // Tokio's blocking stdin read cannot be cancelled; returning would hang
                // runtime shutdown while another process still holds the input open.
                std::process::exit(0)
            }
        }
    }
    #[cfg(not(unix))]
    {
        serve_session(root, db, cache_dir, settings).await
    }
}

async fn serve_session(
    root: PathBuf,
    db: PathBuf,
    cache_dir: Option<PathBuf>,
    settings: Option<crate::state_bridge::InvocationSettings>,
    #[cfg(unix)] initial_parent_pid: u32,
) -> Result<()> {
    let reload_signal = Arc::new(ReloadSignal::new());
    let reload_armed = Arc::clone(&reload_signal.armed);
    let reload_notify = Arc::clone(&reload_signal.notify);
    let mut server = Server::new(root, db).with_reload_signal(reload_signal);
    let startup_exe = server
        .startup_executable_path()
        .map(std::path::Path::to_path_buf);
    if let Some(settings) = &settings {
        server = server.with_invocation_settings(settings.clone());
    }
    if let Some(cache_dir) = cache_dir.as_ref() {
        server = server.with_cache_dir(cache_dir.clone());
    }
    let service = if let Some(params) = resumed_initialize_params(&server) {
        rmcp::service::serve_directly(
            server,
            super::super::response::stdio_with_flush_reload(
                reload_armed,
                Arc::clone(&reload_notify),
            ),
            Some(params),
        )
    } else {
        server
            .serve(super::super::response::stdio_with_flush_reload(
                reload_armed,
                Arc::clone(&reload_notify),
            ))
            .await?
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let preserved = preserved_session(&service);
        let waiting = service.waiting();
        tokio::pin!(waiting);
        let try_reexec = || -> std::io::Result<()> {
            let exe = startup_exe.clone().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cannot re-exec without verified startup executable launch binding",
                )
            })?;
            let args: Vec<_> = std::env::args_os().skip(1).collect();
            let mut command = std::process::Command::new(exe);
            command
                .args(args)
                .env(MCP_RELOAD_PARENT_ENV, initial_parent_pid.to_string());
            match &preserved {
                Some(json) => {
                    command.env(MCP_SESSION_ENV, json);
                }
                None => {
                    command.env_remove(MCP_SESSION_ENV);
                }
            }
            if let Some(settings) = &settings {
                if let Some(config) = &settings.config {
                    command.env("ACDD_CONFIG", config);
                }
                if let Some(workflow) = &settings.workflow {
                    command.env("ACDD_WORKFLOW", workflow);
                }
                if let Some(state_db) = &settings.state_db {
                    command.env("ACDD_STATE_DB", state_db);
                }
                command.env("ACDD_DB", &settings.code_db);
            }
            if let Some(cache_dir) = cache_dir.as_ref() {
                command.env("ACDD_EXTRACTION_CACHE_DIR", cache_dir);
            }
            Err(command.exec())
        };
        loop {
            tokio::select! {
                result = &mut waiting => {
                    result?;
                    break;
                }
                Some(()) = hangup.recv() => {
                    if let Err(error) = try_reexec() {
                        eprintln!("SIGHUP reload failed: {error}");
                    }
                }
                _ = reload_notify.notified() => {
                    if let Err(error) = try_reexec() {
                        eprintln!("binary-drift reload failed: {error}");
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (reload_armed, reload_notify);
        service.waiting().await?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[test]
fn parent_identity_contract() {
    let parent = std::os::unix::process::parent_id();
    let current = parent_is_alive(parent);
    let detached = [parent_is_alive(0), parent_is_alive(1)];
    let changed = parent_is_alive(u32::MAX);
    assert!(current);
    assert!(detached.into_iter().all(|alive| alive));
    assert!(!changed);
}
