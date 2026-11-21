//! Filesystem confinement, deterministic directory sorting, PID liveness, and trace logging.

use super::runs::list_all_runs;
use crate::git_ops::worktree::RepoWorktreeLayout;
use crate::store::traits::StateReader;
use crate::types::RunStatus;
use acdd_core::config::ACDD_DIR;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::fs::{checked_child, open_confined};
use acdd_core::ids::RunId;
use acdd_core::lock::acquire_exclusive;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Read and lexicographically sort directory entries (returns empty `Vec` on unreadable/missing dir).
pub(crate) fn sorted_dir_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

/// Read a confined file inside `root` as UTF-8.
pub fn read_confined_utf8(root: &Path, relpath: &str) -> Result<String, StateError> {
    let mut file = open_confined(root, Path::new(relpath), OpenOptions::new().read(true)).map_err(
        |err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to read confined file '{relpath}': {err}"),
            )
        },
    )?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to decode UTF-8 in '{relpath}': {err}"),
        )
    })?;
    Ok(buf)
}

/// Build the current process session fingerprint `host_id:pid:start_time`.
pub fn current_session_fingerprint() -> String {
    let host = current_host_id();
    let pid = std::process::id();
    let start_time = acdd_core::sys::read_process_start_time(pid)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".to_owned());
    format!("{host}:{pid}:{start_time}")
}

pub(super) fn current_host_id() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end]) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_owned())
}

/// Probe whether the previous local claim PID in `host_id:pid:start_time` is alive (`Some(false)` = dead).
///
/// Returns `None` when the fingerprint belongs to a different host or cannot be parsed.
pub fn is_local_pid_alive(session_fingerprint: Option<&str>) -> Option<bool> {
    let fp = session_fingerprint?;
    let mut parts = fp.splitn(3, ':');
    let host = parts.next()?;
    let pid_str = parts.next()?;
    let recorded_start = parts.next()?;
    if host != current_host_id() {
        return None;
    }
    let pid = pid_str.parse::<u32>().ok()?;
    if pid == 0 {
        return Some(false);
    }

    #[cfg(target_os = "linux")]
    {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return Some(false);
        }
        if let Some(current_start) =
            acdd_core::sys::read_process_start_time(pid).map(|v| v.to_string())
        {
            if recorded_start != "0" && current_start != recorded_start {
                return Some(false);
            }
            return Some(true);
        }
        Some(false)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Ok(pid_t) = libc::pid_t::try_from(pid) else {
            return Some(false);
        };
        if pid_t <= 0 {
            return Some(false);
        }
        if recorded_start != "0" {
            if let Some(current_start) =
                acdd_core::sys::read_process_start_time(pid).map(|v| v.to_string())
            {
                if current_start != recorded_start {
                    return Some(false);
                }
            }
        }
        let rc = unsafe { libc::kill(pid_t, 0) };
        if rc == 0 {
            return Some(true);
        }
        let err = std::io::Error::last_os_error().raw_os_error();
        if err == Some(libc::ESRCH) {
            Some(false)
        } else {
            Some(true)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, recorded_start);
        None
    }
}

/// Join non-empty check observation salts or index generations deterministically (`None` if empty).
pub(crate) fn combine_check_observations(items: &[String]) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        Some(items.join("|"))
    }
}

fn dir_size_bytes_no_follow(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes_no_follow(&p));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn confined_run_traces_dir(primary_worktree: &Path) -> Option<PathBuf> {
    let run_rel = Path::new(ACDD_DIR).join("run");
    let run_dir = checked_child(primary_worktree, &run_rel).ok()?;
    fs::symlink_metadata(&run_dir)
        .is_ok_and(|m| m.file_type().is_dir())
        .then_some(run_dir)
}

/// Remove closed `.acdd/run/<run_id>/` directories beyond `retain_traces` and optional
/// `max_total_bytes` budget, ALWAYS preserving active runs queried from `reader` plus optional
/// `exempt_run_id`. Fails closed (deletes nothing) if querying active runs errors.
pub fn prune_old_run_trace_dirs(
    primary_worktree: &Path,
    reader: &dyn StateReader,
    retain_traces: usize,
    max_total_bytes: Option<u64>,
    exempt_run_id: Option<&str>,
) -> Result<(), StateError> {
    let Some(run_dir) = confined_run_traces_dir(primary_worktree) else {
        return Ok(());
    };
    let mut active_ids: BTreeSet<String> = list_all_runs(reader, Some(RunStatus::Active))?
        .into_iter()
        .map(|r| r.run_id.as_str().to_owned())
        .collect();
    if let Some(exempt) = exempt_run_id {
        active_ids.insert(exempt.to_owned());
    }
    let Ok(entries) = fs::read_dir(&run_dir) else {
        return Ok(());
    };

    let mut closed_dirs: Vec<(std::time::SystemTime, String, u64, PathBuf)> = Vec::new();
    let mut kept_count = 0usize;
    let mut kept_bytes: u64 = 0;

    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let bytes = if max_total_bytes.is_some() {
            dir_size_bytes_no_follow(&p)
        } else {
            0
        };
        if active_ids.contains(name) {
            kept_count = kept_count.saturating_add(1);
            kept_bytes = kept_bytes.saturating_add(bytes);
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        closed_dirs.push((mtime, name.to_owned(), bytes, p));
    }

    closed_dirs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    for (_, _, bytes, old_dir) in closed_dirs {
        let within_count = kept_count < retain_traces;
        let within_bytes =
            max_total_bytes.is_none_or(|limit| kept_bytes.saturating_add(bytes) <= limit);
        if within_count && within_bytes {
            kept_count = kept_count.saturating_add(1);
            kept_bytes = kept_bytes.saturating_add(bytes);
        } else {
            let _ = fs::remove_dir_all(old_dir);
        }
    }
    Ok(())
}

/// Append a single JSONL trace event to `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`
/// under `<git_common_dir>/acdd/traces.lock` (leaf lock, never inside `StateTx`).
pub fn append_trace_event(
    layout: Option<&RepoWorktreeLayout>,
    primary_worktree: &Path,
    run_id: &RunId,
    event: &Value,
) -> Result<(), StateError> {
    let lock_path = layout
        .map(|l| l.traces_lock_path.clone())
        .unwrap_or_else(|| primary_worktree.join(ACDD_DIR).join("traces.lock"));
    let _guard = acquire_exclusive(&lock_path).map_err(|err| {
        StateError::new(
            ErrorCode::BackendConflict,
            format!("failed to acquire traces.lock: {err}"),
        )
    })?;

    let rel = Path::new(ACDD_DIR)
        .join("run")
        .join(run_id.as_str())
        .join("traces.jsonl");
    if let Some(parent) = primary_worktree.join(&rel).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut line = serde_json::to_string(event).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to serialize trace event: {err}"),
        )
    })?;
    line.push('\n');

    let mut file = open_confined(
        primary_worktree,
        &rel,
        OpenOptions::new().create(true).append(true),
    )
    .map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to open traces.jsonl '{}': {err}", rel.display()),
        )
    })?;
    file.write_all(line.as_bytes()).map_err(|err| {
        StateError::new(
            ErrorCode::ProfileSchemaInvalid,
            format!("failed to append trace event: {err}"),
        )
    })?;
    Ok(())
}
