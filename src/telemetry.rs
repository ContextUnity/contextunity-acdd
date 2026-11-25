//! W3C Trace Context span correlation (`SpanGuard`), `traces.jsonl` persistence, and debug logging.

use acdd_core::fs::{ensure_confined_dir_all, open_confined};
use acdd_core::git::primary_worktree_root;
use acdd_core::lock::acquire_exclusive;
use acdd_core::telemetry::{now_utc_iso_ms, SpanId, TraceId, TraceParent, WorktreeSpanContext};
use acdd_state::git_ops::worktree::resolve_repo_layout;
use acdd_state::store::sqlite::SqliteStateReader;
use acdd_state::usecases::lifecycle::prune_old_run_trace_dirs;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Default maximum total size (in MiB) across retained `.acdd/run/*` trace directories.
const DEFAULT_MAX_RETAINED_TRACE_MIB: u64 = 128;

/// W3C Trace Context span guard recording bounded span events into `<primary_worktree>/.acdd/run/<run_id>/traces.jsonl`.
pub struct SpanGuard {
    root: PathBuf,
    state_db: Option<PathBuf>,
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
                    (ctx_trace_id.unwrap_or(tp.trace_id), Some(c.span_id.clone()))
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
            state_db: None,
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

    /// Override the `state.sqlite` path queried when enforcing active-run-safe trace retention.
    pub fn with_state_db(mut self, state_db: PathBuf) -> Self {
        self.state_db = Some(state_db);
        self
    }

    /// Override the maximum retained trace disk budget in MiB.
    #[cfg(test)]
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
    #[cfg(test)]
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
            state_db: self.state_db.clone(),
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

/// Prepared JSONL debug log line awaiting persistence.
struct PendingDebugLog {
    root: PathBuf,
    date: String,
    line: String,
}

struct SpanFlushPayload {
    root: PathBuf,
    state_db: Option<PathBuf>,
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
    let primary = primary.canonicalize().unwrap_or(primary);
    let lock_path = layout
        .as_ref()
        .map(|l| l.traces_lock_path.clone())
        .unwrap_or_else(|| primary.join(".acdd/traces.lock"));

    let Ok(_guard) = acquire_exclusive(&lock_path) else {
        return;
    };

    let run_rel_dir = Path::new(".acdd/run").join(payload.ctx.run_id.as_str());
    let rel = run_rel_dir.join("traces.jsonl");
    if ensure_confined_dir_all(&primary, &run_rel_dir).is_ok() {
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
            let mut open_opts = OpenOptions::new();
            open_opts.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                open_opts.mode(0o600);
            }
            if let Ok(mut file) = open_confined(&primary, &rel, &mut open_opts) {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }

    enforce_trace_retention(
        &primary,
        payload.state_db.as_deref(),
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

fn resolve_retention_state_db(primary: &Path, explicit_state_db: Option<&Path>) -> Option<PathBuf> {
    if let Some(explicit) = acdd_core::config::resolve_custom_db_path(primary, explicit_state_db) {
        return Some(explicit);
    }
    let adapter = acdd_core::config::load_adapter(primary, None).ok()?;
    Some(adapter.resolve_state_db_path(primary))
}

fn enforce_trace_retention(
    primary: &Path,
    state_db: Option<&Path>,
    current_run_id: &str,
    retain_traces: usize,
    max_retained_trace_mib: u64,
) {
    let Some(resolved_state_db) = resolve_retention_state_db(primary, state_db) else {
        // Fail closed when workspace config cannot be read/parsed; never guess DB paths and delete run dirs.
        return;
    };
    // Fail closed if `state.sqlite` exists but cannot be opened/queried (corrupt/locked/symlink).
    let reader = SqliteStateReader::new(resolved_state_db);
    let max_total_bytes = max_retained_trace_mib.saturating_mul(1024 * 1024);
    let _ = prune_old_run_trace_dirs(
        primary,
        &reader,
        retain_traces,
        Some(max_total_bytes),
        Some(current_run_id),
    );
}

fn utc_date_and_timestamp() -> (String, String) {
    let timestamp = now_utc_iso_ms();
    let date = timestamp
        .split_once('T')
        .map_or_else(|| timestamp.clone(), |(d, _)| d.to_owned());
    (date, timestamp)
}

fn debug_log_root(root: &Path) -> PathBuf {
    let resolved = primary_worktree_root(root).unwrap_or_else(|| root.to_path_buf());
    resolved.canonicalize().unwrap_or(resolved)
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
    let target_root = debug_log_root(root);
    let destination = target_root
        .join(".acdd")
        .join(format!("command-log-debug--{date}.jsonl"));
    match serde_json::to_string(entry) {
        Ok(line) => Some(PendingDebugLog {
            root: target_root,
            date: date.to_owned(),
            line,
        }),
        Err(error) => {
            report_log_error("serialize", &destination, &error);
            None
        }
    }
}

/// Prepare an MCP debug log entry for persistence.
fn prepare_mcp_debug_log(
    root: &Path,
    tool: &str,
    parameters: &str,
    output: &serde_json::Value,
) -> Option<PendingDebugLog> {
    let (date_str, time_str) = utc_date_and_timestamp();

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

/// Persist a prepared debug log entry.
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
    let target_root = debug_log_root(root);
    let rel_dir = Path::new(".acdd");
    let rel_file = rel_dir.join(format!("command-log-debug--{date}.jsonl"));
    let directory = target_root.join(rel_dir);
    let destination = target_root.join(&rel_file);
    if let Err(error) = ensure_confined_dir_all(&target_root, rel_dir) {
        report_log_error("create directory", &directory, &error);
        return;
    }
    let mut open_opts = OpenOptions::new();
    open_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.mode(0o600);
    }
    let mut file = match open_confined(&target_root, &rel_file, &mut open_opts) {
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
/// Structured event payload written to daily JSONL telemetry logs.
enum DebugLogEntry<'a> {
    /// Entry emitted for a CLI command execution.
    Cli {
        /// UTC timestamp (`HH:MM:SS.mmmZ`) when the CLI event was recorded.
        timestamp: String,
        /// Top-level CLI command name.
        command: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// Redacted serialized CLI arguments when present.
        parameters: Option<serde_json::Value>,
        /// Redacted serialized command output or error summary.
        output: serde_json::Value,
    },
    /// Entry emitted for an MCP tool invocation.
    Mcp {
        /// UTC timestamp (`HH:MM:SS.mmmZ`) when the MCP event was recorded.
        timestamp: String,
        /// MCP tool name.
        tool: &'a str,
        /// Redacted JSON tool call arguments.
        parameters: serde_json::Value,
        /// Redacted tool result payload or error summary.
        output: serde_json::Value,
    },
}

/// Appends a redacted CLI execution record to today's debug log when `debug: true` is enabled.
pub fn log_cli(
    root: &Path,
    command: &str,
    parameters: Option<&serde_json::Value>,
    output: &serde_json::Value,
) {
    let (date_str, time_str) = utc_date_and_timestamp();

    let entry = DebugLogEntry::Cli {
        timestamp: time_str,
        command,
        parameters: parameters.cloned(),
        output: output.clone(),
    };

    write_entry(root, &date_str, &entry);
}

#[cfg(test)]
mod tests {
    use super::SpanGuard;
    use acdd_core::fs::ScopedTempDir;
    use acdd_core::ids::RunId;
    use acdd_core::telemetry::{SpanId, TraceId, TraceParent, WorktreeSpanContext};
    use std::fs;

    #[test]
    fn span_guard_writes_w3c_traces_and_enforces_retention() {
        let dir = ScopedTempDir::create("acdd-ut-telemetry").unwrap();
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

            let mut no_ctx_guard =
                SpanGuard::from_worktree_context(root, None, "code_map_search", 1);
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
        for line in debug_lines.lines() {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(entry["timestamp"].as_str().unwrap().ends_with('Z'));
        }
        // Symlinked `.acdd` directory must be rejected without escaping.
        #[cfg(unix)]
        {
            let outside = ScopedTempDir::create("acdd-ut-telemetry-outside").unwrap();
            let sym_ws = ScopedTempDir::create("acdd-ut-telemetry-symws").unwrap();
            std::os::unix::fs::symlink(outside.path(), sym_ws.path().join(".acdd")).unwrap();
            super::log_cli(sym_ws.path(), "scan", None, &serde_json::json!({"ok": 1}));
            assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
        }
    }

    #[test]
    fn debug_logger_writes_to_acdd_directory() {
        let dir = ScopedTempDir::create("acdd-ut-debug-logger").unwrap();
        let root = dir.path();
        let utc_now = acdd_core::telemetry::now_utc_iso_ms();
        let date_str = utc_now.split('T').next().unwrap();
        let expected_jsonl_file = root
            .join(".acdd")
            .join(format!("command-log-debug--{date_str}.jsonl"));

        assert!(!expected_jsonl_file.exists());

        super::log_cli(
            root,
            "query overview",
            None,
            &serde_json::json!({"status": "ok"}),
        );

        if let Some(pending) = super::prepare_mcp_debug_log(
            root,
            "code_map_inspect",
            "{\"selector\": \"foo::bar\"}",
            &serde_json::json!({"found": true}),
        ) {
            super::write_pending_debug_log(pending);
        }

        assert!(
            expected_jsonl_file.exists(),
            "JSONL log file must be created"
        );

        let content = fs::read_to_string(&expected_jsonl_file).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let cli_entry: serde_json::Value =
            serde_json::from_str(lines[0]).expect("valid jsonl line 1");
        assert_eq!(cli_entry["source"], "cli");
        assert_eq!(cli_entry["command"], "query overview");
        assert_eq!(cli_entry["output"]["status"], "ok");

        let mcp_entry: serde_json::Value =
            serde_json::from_str(lines[1]).expect("valid jsonl line 2");
        assert_eq!(mcp_entry["source"], "mcp");
        assert_eq!(mcp_entry["tool"], "code_map_inspect");
        assert_eq!(mcp_entry["parameters"]["selector"], "foo::bar");
        assert_eq!(mcp_entry["output"]["found"], true);
    }
}
