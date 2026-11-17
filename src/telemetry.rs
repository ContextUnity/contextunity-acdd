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
            (Some(c), _) => (
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

    /// Finish and flush this span explicitly.
    pub fn finish(mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let Some(ref ctx) = self.context else {
            return;
        };

        let layout = resolve_repo_layout(&self.root).ok();
        let primary = layout
            .as_ref()
            .map(|l| l.primary_worktree.clone())
            .or_else(|| primary_worktree_root(&self.root))
            .unwrap_or_else(|| self.root.clone());
        let lock_path = layout
            .as_ref()
            .map(|l| l.traces_lock_path.clone())
            .unwrap_or_else(|| primary.join(".acdd/traces.lock"));

        let Ok(_guard) = acquire_exclusive(&lock_path) else {
            return;
        };

        let rel = Path::new(".acdd/run")
            .join(ctx.run_id.as_str())
            .join("traces.jsonl");
        if let Some(parent) = primary.join(&rel).parent() {
            let _ = fs::create_dir_all(parent);
        }

        let finished_utc = now_utc_iso_ms();
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        let event = json!({
            "kind": "span",
            "trace_id": self.trace_id.to_string(),
            "span_id": self.span_id.to_string(),
            "parent_span_id": self.parent_span_id,
            "run_id": ctx.run_id.as_str(),
            "node_id": ctx.node_id,
            "operation": self.operation,
            "status": self.status,
            "started_at": self.started_utc,
            "finished_at": finished_utc,
            "duration_ms": duration_ms,
            "attributes": self.attributes,
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
            ctx.run_id.as_str(),
            self.retain_traces,
            self.max_retained_trace_mib,
        );
    }
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

fn write_entry(root: &Path, date: &str, entry: &DebugLogEntry<'_>) {
    let directory = root.join(".acdd");
    if let Err(error) = fs::create_dir_all(&directory) {
        report_log_error("create directory", &directory, &error);
        return;
    }
    let destination = directory.join(format!("command-log-debug--{date}.jsonl"));
    let line = match serde_json::to_string(entry) {
        Ok(line) => line,
        Err(error) => {
            report_log_error("serialize", &destination, &error);
            return;
        }
    };
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

    write_entry(root, &date_str, &entry);
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
    }
}
