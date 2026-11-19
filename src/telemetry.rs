//! W3C Trace Context span correlation (`SpanGuard`), `traces.jsonl` persistence, and debug logging.

use crate::state_bridge::WorktreeSpanContext;
use acdd_core::fs::open_confined;
use acdd_core::git::primary_worktree_root;
use acdd_core::lock::acquire_exclusive;
use acdd_core::telemetry::{now_utc_iso_ms, SpanId, TraceId, TraceParent};
use acdd_state::git_ops::worktree::resolve_repo_layout;
use chrono::Local;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};
use walkdir::WalkDir;

/// Default maximum total size (in MiB) across retained `.acdd/run/*` trace directories.
pub const DEFAULT_MAX_RETAINED_TRACE_MIB: u64 = 128;

/// W3C Trace Context span guard recording bounded span events into `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`.
pub struct SpanGuard {
    root: PathBuf,
    context: Option<WorktreeSpanContext>,
    trace_id: TraceId,
    span_id: SpanId,
    parent_span_id: Option<String>,
    operation: String,
    retain_traces: usize,
    max_retained_trace_mib: u64,
    started_at: Instant,
    started_utc: String,
    status: String,
    attributes: Map<String, Value>,
    debug_log: Option<PendingDebugLog>,
    finished: bool,
}

impl SpanGuard {
    /// Start a new span guard from an optional [`WorktreeSpanContext`].
    pub fn from_worktree_context(
        root: &Path,
        ctx: Option<&WorktreeSpanContext>,
        operation: &str,
        retain_traces: usize,
    ) -> Self {
        let env_parent = std::env::var("TRACEPARENT")
            .ok()
            .and_then(|raw| TraceParent::parse(raw.trim()).ok());
        let (trace_id, parent_span_id) = match (ctx, env_parent) {
            (Some(c), Some(tp)) => {
                let ctx_trace_id = c.trace_id.parse::<TraceId>().ok();
                if ctx_trace_id == Some(tp.trace_id) {
                    (tp.trace_id, Some(tp.span_id.to_string()))
                } else {
                    (
                        ctx_trace_id.unwrap_or(tp.trace_id),
                        Some(c.span_id.clone()),
                    )
                }
            }
            (Some(c), None) => (
                c.trace_id.parse().unwrap_or_else(|_| TraceId::new()),
                Some(c.span_id.clone()),
            ),
            (None, Some(tp)) => (tp.trace_id, Some(tp.span_id.to_string())),
            (None, None) => (TraceId::new(), None),
        };
        let span_id = SpanId::new();
        Self {
            root: root.to_path_buf(),
            context: ctx.cloned(),
            trace_id,
            span_id,
            parent_span_id,
            operation: operation.to_owned(),
            retain_traces: retain_traces.max(1),
            max_retained_trace_mib: DEFAULT_MAX_RETAINED_TRACE_MIB,
            started_at: Instant::now(),
            started_utc: now_utc_iso_ms(),
            status: "ok".to_owned(),
            attributes: Map::new(),
            debug_log: None,
            finished: false,
        }
    }

    /// Override the maximum retained trace disk budget in MiB.
    pub fn with_max_retained_trace_mib(mut self, max_mib: u64) -> Self {
        self.max_retained_trace_mib = max_mib.max(1);
        self
    }

    /// Set the span status label (`"ok"`, `"error"`, etc.).
    pub fn set_status(&mut self, status: &str) {
        self.status = status.to_owned();
    }

    /// Attach a structured attribute to this span.
    pub fn set_attribute(&mut self, key: &str, value: Value) {
        self.attributes.insert(key.to_owned(), value);
    }

    /// Return the W3C `traceparent` header value (`00-<trace_id>-<span_id>-01`).
    pub fn traceparent(&self) -> String {
        TraceParent::new(self.trace_id, self.span_id).to_string()
    }

    /// Attach a pending MCP debug log entry to be awaited at [`SpanGuard::finish_async`].
    pub fn set_debug_mcp(
        &mut self,
        root: &Path,
        tool: &str,
        parameters: &str,
        output: &serde_json::Value,
    ) {
        self.debug_log = prepare_mcp_debug_log(root, tool, parameters, output);
    }

    /// Finish and flush this span explicitly.
    pub fn finish(mut self) {
        self.flush();
    }

    /// Finish and await both trace and debug-log persistence without blocking a Tokio runtime thread.
    pub async fn finish_async(mut self) {
        let (span_payload, debug_payload) = self.take_flush_payloads();
        if span_payload.is_some() || debug_payload.is_some() {
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(payload) = span_payload {
                    flush_payload_sync(payload);
                }
                if let Some(debug) = debug_payload {
                    write_entry_sync(&debug.root, &debug.date, &debug.line);
                }
            })
            .await;
        }
    }

    fn take_flush_payloads(&mut self) -> (Option<SpanFlushPayload>, Option<PendingDebugLog>) {
        if self.finished {
            return (None, None);
        }
        self.finished = true;
        let span_payload = self.context.clone().map(|ctx| SpanFlushPayload {
            root: self.root.clone(),
            ctx,
            trace_id: self.trace_id.to_string(),
            span_id: self.span_id.to_string(),
            parent_span_id: self.parent_span_id.clone(),
            operation: self.operation.clone(),
            status: self.status.clone(),
            started_utc: self.started_utc.clone(),
            finished_utc: now_utc_iso_ms(),
            duration_ms: self.started_at.elapsed().as_millis() as u64,
            attributes: self.attributes.clone(),
            retain_traces: self.retain_traces,
            max_retained_trace_mib: self.max_retained_trace_mib,
        });
        let debug_payload = self.debug_log.take();
        (span_payload, debug_payload)
    }

    fn flush(&mut self) {
        let (span_payload, debug_payload) = self.take_flush_payloads();
        if span_payload.is_none() && debug_payload.is_none() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || {
                if let Some(payload) = span_payload {
                    flush_payload_sync(payload);
                }
                if let Some(debug) = debug_payload {
                    write_entry_sync(&debug.root, &debug.date, &debug.line);
                }
            });
        } else {
            if let Some(payload) = span_payload {
                flush_payload_sync(payload);
            }
            if let Some(debug) = debug_payload {
                write_entry_sync(&debug.root, &debug.date, &debug.line);
            }
        }
    }
}

struct PendingDebugLog {
    root: PathBuf,
    date: String,
    line: String,
}

struct SpanFlushPayload {
    root: PathBuf,
    ctx: WorktreeSpanContext,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    operation: String,
    status: String,
    started_utc: String,
    finished_utc: String,
    duration_ms: u64,
    attributes: Map<String, Value>,
    retain_traces: usize,
    max_retained_trace_mib: u64,
}

fn flush_payload_sync(payload: SpanFlushPayload) {
    let layout = resolve_repo_layout(&payload.root).ok();
    let primary = layout
        .as_ref()
        .map(|l| l.primary_worktree.clone())
        .or_else(|| primary_worktree_root(&payload.root))
        .unwrap_or_else(|| payload.root.clone());
    let lock_path = layout
        .as_ref()
        .map(|l| l.traces_lock_path.clone())
        .unwrap_or_else(|| primary.join(".acdd/traces.lock"));

    let Ok(_guard) = acquire_exclusive(&lock_path) else {
        return;
    };

    let rel = Path::new(".acdd/run")
        .join(payload.ctx.run_id.as_str())
        .join("traces.jsonl");
    if let Some(parent) = primary.join(&rel).parent() {
        let _ = fs::create_dir_all(parent);
    }

    let event = json!({
        "kind": "span",
        "trace_id": payload.trace_id,
        "span_id": payload.span_id,
        "parent_span_id": payload.parent_span_id,
        "run_id": payload.ctx.run_id.as_str(),
        "node_id": payload.ctx.node_id,
        "operation": payload.operation,
        "status": payload.status,
        "started_at": payload.started_utc,
        "finished_at": payload.finished_utc,
        "duration_ms": payload.duration_ms,
        "attributes": payload.attributes,
    });
    if let Ok(mut line) = serde_json::to_string(&event) {
        line.push('\n');
        if let Ok(mut file) = open_confined(
            &primary,
            &rel,
            OpenOptions::new().create(true).append(true),
        ) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    enforce_trace_retention(
        &primary,
        payload.ctx.run_id.as_str(),
        payload.retain_traces,
        payload.max_retained_trace_mib,
    );
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.flush();
    }
}

fn enforce_trace_retention(
    primary: &Path,
    current_run_id: &str,
    retain_traces: usize,
    max_retained_trace_mib: u64,
) {
    let run_root = primary.join(".acdd/run");
    if !run_root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&run_root) else {
        return;
    };

    let mut dirs: Vec<(String, SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let bytes = dir_size_bytes(&path);
        dirs.push((name.to_owned(), mtime, bytes, path));
    }

    // Sort newest first so we keep `retain_traces` newest entries and prune older ones.
    dirs.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let max_bytes = max_retained_trace_mib.saturating_mul(1024 * 1024);
    let mut kept_count = 0usize;
    let mut kept_bytes = 0u64;

    for (run_id, _, bytes, path) in dirs {
        if run_id == current_run_id {
            kept_count += 1;
            kept_bytes = kept_bytes.saturating_add(bytes);
            continue;
        }
        let within_count = kept_count < retain_traces;
        let within_bytes = kept_bytes.saturating_add(bytes) <= max_bytes;
        if within_count && within_bytes {
            kept_count += 1;
            kept_bytes = kept_bytes.saturating_add(bytes);
        } else {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn dir_size_bytes(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

fn report_log_error(operation: &str, destination: &Path, error: &dyn std::fmt::Display) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "debug log {operation} {}: {error}",
        destination.display()
    );
}

fn prepare_debug_log(
    root: &Path,
    date: &str,
    entry: &DebugLogEntry<'_>,
) -> Option<PendingDebugLog> {
    let destination = root
        .join(".acdd")
        .join(format!("command-log-debug--{date}.jsonl"));
    match serde_json::to_string(entry) {
        Ok(line) => Some(PendingDebugLog {
            root: root.to_path_buf(),
            date: date.to_owned(),
            line,
        }),
        Err(error) => {
            report_log_error("serialize", &destination, &error);
            None
        }
    }
}

fn prepare_mcp_debug_log(
    root: &Path,
    tool: &str,
    parameters: &str,
    output: &serde_json::Value,
) -> Option<PendingDebugLog> {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let time_str = now.format("%Y-%m-%dT%H:%M:%S%.3f").to_string();

    let params_val = serde_json::from_str::<serde_json::Value>(parameters)
        .unwrap_or_else(|_| serde_json::Value::String(parameters.to_owned()));

    let entry = DebugLogEntry::Mcp {
        timestamp: time_str,
        tool,
        parameters: params_val,
        output: output.clone(),
    };

    prepare_debug_log(root, &date_str, &entry)
}

fn write_pending_debug_log(pending: PendingDebugLog) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn_blocking(move || {
            write_entry_sync(&pending.root, &pending.date, &pending.line);
        });
    } else {
        write_entry_sync(&pending.root, &pending.date, &pending.line);
    }
}

fn write_entry(root: &Path, date: &str, entry: &DebugLogEntry<'_>) {
    if let Some(pending) = prepare_debug_log(root, date, entry) {
        write_pending_debug_log(pending);
    }
}

fn write_entry_sync(root: &Path, date: &str, line: &str) {
    let directory = root.join(".acdd");
    let destination = directory.join(format!("command-log-debug--{date}.jsonl"));
    if let Err(error) = fs::create_dir_all(&directory) {
        report_log_error("create directory", &directory, &error);
        return;
    }
    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&destination)
    {
        Ok(file) => file,
        Err(error) => {
            report_log_error("open", &destination, &error);
            return;
        }
    };
    if let Err(error) = writeln!(file, "{line}") {
        report_log_error("write", &destination, &error);
    }
}

#[derive(Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
/// Enumerates the supported debug log entry values.
pub enum DebugLogEntry<'a> {
    /// Represents the cli case.
    Cli {
        /// The timestamp value.
        timestamp: String,
        /// The command value.
        command: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// Optional parameters value.
        parameters: Option<serde_json::Value>,
        /// The output value.
        output: serde_json::Value,
    },
    /// Represents the mcp case.
    Mcp {
        /// The timestamp value.
        timestamp: String,
        /// The tool value.
        tool: &'a str,
        /// The parameters value.
        parameters: serde_json::Value,
        /// The output value.
        output: serde_json::Value,
    },
}

/// Performs log cli.
pub fn log_cli(
    root: &Path,
    command: &str,
    parameters: Option<&serde_json::Value>,
    output: &serde_json::Value,
) {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let time_str = now.format("%Y-%m-%dT%H:%M:%S%.3f").to_string();

    let entry = DebugLogEntry::Cli {
        timestamp: time_str,
        command,
        parameters: parameters.cloned(),
        output: output.clone(),
    };

    write_entry(root, &date_str, &entry);
}

/// Performs log mcp.
pub fn log_mcp(root: &Path, tool: &str, parameters: &str, output: &serde_json::Value) {
    if let Some(pending) = prepare_mcp_debug_log(root, tool, parameters, output) {
        write_pending_debug_log(pending);
    }
}

/// Performs log command.
pub fn log_command(root: &Path, command: &str, input: Option<&str>, output: &str) {
    let output_val = serde_json::from_str::<serde_json::Value>(output)
        .unwrap_or_else(|_| serde_json::Value::String(output.to_string()));
    let input_val = input.map(|inp| {
        serde_json::from_str::<serde_json::Value>(inp)
            .unwrap_or_else(|_| serde_json::Value::String(inp.to_string()))
    });
    log_cli(root, command, input_val.as_ref(), &output_val);
}

#[cfg(test)]
mod tests {
    use super::SpanGuard;
    use crate::state_bridge::WorktreeSpanContext;
    use acdd_core::telemetry::{SpanId, TraceId, TraceParent};
    use acdd_core::ids::RunId;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "acdd-ut-{tag}-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn span_guard_writes_w3c_traces_and_enforces_retention() {
        let dir = TempDir::new("telemetry");
        let root = dir.path();
        let ok = acdd_core::git::git_command(root, &["init"])
            .unwrap()
            .status
            .success();
        assert!(ok);

        let parent = TraceParent::new(TraceId::new(), SpanId::new());
        let ctx = WorktreeSpanContext {
            run_id: RunId::new("run-new").unwrap(),
            node_id: "t1.build".into(),
            trace_id: parent.trace_id.to_string(),
            span_id: parent.span_id.to_string(),
        };
        let old_dir = root.join(".acdd/run/old-run");
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(old_dir.join("traces.jsonl"), "{\"old\":true}\n").unwrap();

        let mut guard = SpanGuard::from_worktree_context(root, Some(&ctx), "acdd_verify", 1)
            .with_max_retained_trace_mib(128);
        assert!(guard.traceparent().starts_with("00-"));
        guard.set_attribute("exit_code", serde_json::json!(0));
        guard.set_status("ok");
        guard.finish();

        let trace_file = root.join(".acdd/run/run-new/traces.jsonl");
        let content = fs::read_to_string(&trace_file).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(record["operation"], "acdd_verify");
        assert_eq!(record["status"], "ok");
        assert_eq!(record["run_id"], "run-new");
        assert_eq!(record["node_id"], "t1.build");
        assert_eq!(record["trace_id"], parent.trace_id.to_string());
        assert_eq!(record["parent_span_id"], parent.span_id.to_string());
        assert!(!old_dir.join("traces.jsonl").exists());

        // Verify Tokio async runtime flush offloads without blocking/panicking on current_thread.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut async_guard =
                SpanGuard::from_worktree_context(root, Some(&ctx), "acdd_async", 1);
            async_guard.set_status("ok");
            async_guard.set_debug_mcp(
                root,
                "acdd_async",
                "{\"mode\":\"with_ctx\"}",
                &serde_json::json!({ "ok": true }),
            );
            async_guard.finish_async().await;

            let mut no_ctx_guard = SpanGuard::from_worktree_context(root, None, "code_map_search", 1);
            no_ctx_guard.set_debug_mcp(
                root,
                "code_map_search",
                "{\"pattern\":\"foo\"}",
                &serde_json::json!({ "ok": true }),
            );
            no_ctx_guard.finish_async().await;
        });
        let after = fs::read_to_string(&trace_file).unwrap();
        assert_eq!(after.lines().count(), 2);
        let debug_files: Vec<_> = fs::read_dir(root.join(".acdd"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("command-log-debug--"))
            })
            .collect();
        assert_eq!(debug_files.len(), 1);
        let debug_lines = fs::read_to_string(&debug_files[0]).unwrap();
        assert_eq!(debug_lines.lines().count(), 2);
    }
}
