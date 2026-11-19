//! Bounded gate command execution, process-group isolation, env sanitization, and output parsing.

use crate::profile::{
    render_command_with_scope_target, render_command_with_test_filter, resolve_command_for_scope,
    CommandDef, CommandOutputSpec, CompileErrorsSpec, ResolvedCommandInvocation, ResourceLimits,
};
use crate::runner::{
    node_slug, open_run_log_file, run_isolated_process, utf8_tail, BoundedLogWriter, StreamKind,
};
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, RunId};
use regex::Regex;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

static DEFAULT_PASSED_COUNT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+) passed\b").expect("valid default counts.passed regex"));
static DEFAULT_FAILED_COUNT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+) failed\b").expect("valid default counts.failed regex"));
static DEFAULT_IGNORED_COUNT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+) ignored\b").expect("valid default counts.ignored regex"));

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
    /// Optional `--test` target supplied by caller.
    pub test_target: Option<String>,
    /// Resource and timeout limits from `profile.limits`.
    pub limits: ResourceLimits,
    /// Optional W3C `TRACEPARENT` (note: stripped from project verification commands by `sanitize_command_process`).
    pub trace_parent: Option<String>,
}

/// Captured execution outcome and parsed test/compile diagnostics.
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
    /// Parsed number of passed tests.
    pub tests_passed: u32,
    /// Parsed number of failed tests.
    pub tests_failed: u32,
    /// Parsed number of ignored tests.
    pub tests_ignored: u32,
    /// Individual passed test names extracted from output.
    pub passed_tests: Vec<String>,
    /// Individual failed test names extracted from output.
    pub failed_tests: Vec<String>,
    /// Individual ignored test names extracted from output.
    pub ignored_tests: Vec<String>,
    /// Count of output lines matching `compile_errors`.
    pub compile_error_count: u32,
    /// Bounded tail of stderr for `expected_failure.stderr_contains` and diagnostics.
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

/// Render one resolved command invocation by expanding `{scope_target}` before `{test_filter}`.
pub(crate) fn render_invocation_command(
    inv: &ResolvedCommandInvocation,
    admitted_scope: &[String],
    test_target: Option<&str>,
) -> Result<String, StateError> {
    let match_paths = invocation_match_paths(inv, admitted_scope);
    let cmd_with_scope =
        render_command_with_scope_target(&inv.command, &inv.scope_args_by_pattern, match_paths)?;
    render_command_with_test_filter(&cmd_with_scope, inv.test_filter.as_ref(), test_target)
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
    let mut agg = OutputAccumulator::new(&req.worktree, req.limits.max_capture_bytes);

    for inv in &invocations {
        let rendered_cmd =
            render_invocation_command(inv, &req.scope, req.test_target.as_deref())?;
        rendered_commands.push(rendered_cmd.clone());

        let parsers = CompiledOutputParsers::compile(inv.output.as_ref())?;
        let timeout = timeout_override.unwrap_or_else(|| {
            let secs = inv
                .timeout_seconds
                .unwrap_or(req.limits.max_command_timeout_seconds)
                .min(req.limits.max_command_timeout_seconds)
                .max(1);
            Duration::from_secs(secs)
        });

        let mut cmd = shell_command(&rendered_cmd);
        cmd.current_dir(&req.worktree);
        sanitize_command_process(&mut cmd);

        let mut current_target = invocation_match_paths(inv, &req.scope)
            .first()
            .filter(|_| inv.output.as_ref().and_then(|o| o.tests.as_ref()).and_then(|t| t.target.as_ref()).is_some())
            .cloned();

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
                agg.observe_line(stream, line, &parsers, &mut current_target);
                Ok(())
            },
        )?;

        last_exit_code = outcome.exit_code;
        timed_out = outcome.timed_out;

        if parsers.explicit_counts {
            agg.used_explicit_counts = true;
        }

        if timed_out || last_exit_code != Some(0) {
            break;
        }
    }

    let (tests_passed, tests_failed, tests_ignored) = agg.finalized_counts();
    let stderr_tail = utf8_tail(&agg.stderr_retained, req.limits.diagnostic_inline_bytes).to_owned();

    Ok(CommandExecOutcome {
        command_line: rendered_commands.join(" && "),
        exit_code: last_exit_code,
        timed_out,
        aborted: false,
        tests_passed,
        tests_failed,
        tests_ignored,
        passed_tests: agg.passed_tests,
        failed_tests: agg.failed_tests,
        ignored_tests: agg.ignored_tests,
        compile_error_count: agg.compile_error_count,
        stderr_tail,
        log_relpath,
    })
}

fn shell_command(command: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }
}

struct CompiledTestsParser {
    target: Option<Regex>,
    passed: Regex,
    failed: Option<Regex>,
    ignored: Option<Regex>,
}

struct CompiledOutputParsers {
    explicit_counts: bool,
    passed_count_re: Regex,
    failed_count_re: Regex,
    ignored_count_re: Regex,
    noise: Vec<Regex>,
    compile_include: Vec<Regex>,
    compile_exclude: Vec<Regex>,
    tests: Option<CompiledTestsParser>,
}

impl CompiledOutputParsers {
    fn compile(spec: Option<&CommandOutputSpec>) -> Result<Self, StateError> {
        let compile_re = |pat: &str, ctx: &str| -> Result<Regex, StateError> {
            Regex::new(pat).map_err(|err| {
                StateError::new(
                    ErrorCode::ProfileSchemaInvalid,
                    format!("invalid {ctx} regex '{pat}': {err}"),
                )
            })
        };

        let explicit_counts = spec.and_then(|s| s.counts.as_ref()).is_some();
        let passed_count_re = match spec.and_then(|s| s.counts.as_ref()) {
            Some(c) => compile_re(&c.passed, "output.counts.passed")?,
            None => DEFAULT_PASSED_COUNT_RE.clone(),
        };
        let failed_count_re = match spec.and_then(|s| s.counts.as_ref()) {
            Some(c) => compile_re(&c.failed, "output.counts.failed")?,
            None => DEFAULT_FAILED_COUNT_RE.clone(),
        };
        let ignored_count_re = DEFAULT_IGNORED_COUNT_RE.clone();

        let mut noise = Vec::new();
        let mut compile_include = Vec::new();
        let mut compile_exclude = Vec::new();
        let mut tests = None;

        if let Some(s) = spec {
            for pat in &s.noise {
                noise.push(compile_re(pat, "output.noise")?);
            }
            match &s.compile_errors {
                CompileErrorsSpec::Patterns(pats) => {
                    for pat in pats {
                        compile_include.push(compile_re(pat, "output.compile_errors")?);
                    }
                }
                CompileErrorsSpec::IncludeExclude(filter) => {
                    for pat in &filter.include {
                        compile_include.push(compile_re(pat, "output.compile_errors.include")?);
                    }
                    for pat in &filter.exclude {
                        compile_exclude.push(compile_re(pat, "output.compile_errors.exclude")?);
                    }
                }
            }
            if let Some(t) = &s.tests {
                tests = Some(CompiledTestsParser {
                    target: t
                        .target
                        .as_deref()
                        .map(|p| compile_re(p, "output.tests.target"))
                        .transpose()?,
                    passed: compile_re(&t.passed, "output.tests.passed")?,
                    failed: t
                        .failed
                        .as_deref()
                        .map(|p| compile_re(p, "output.tests.failed"))
                        .transpose()?,
                    ignored: t
                        .ignored
                        .as_deref()
                        .map(|p| compile_re(p, "output.tests.ignored"))
                        .transpose()?,
                });
            }
        }

        Ok(Self {
            explicit_counts,
            passed_count_re,
            failed_count_re,
            ignored_count_re,
            noise,
            compile_include,
            compile_exclude,
            tests,
        })
    }

    fn is_compile_error(&self, line: &str) -> bool {
        !self.compile_include.is_empty()
            && self.compile_include.iter().any(|re| re.is_match(line))
            && !self.compile_exclude.iter().any(|re| re.is_match(line))
    }

    fn is_noise(&self, line: &str) -> bool {
        self.noise.iter().any(|re| re.is_match(line))
    }
}

struct OutputAccumulator {
    worktree: PathBuf,
    max_capture_bytes: usize,
    captured_bytes: usize,
    stderr_retained: String,
    parsed_passed: u32,
    parsed_failed: u32,
    parsed_ignored: u32,
    used_explicit_counts: bool,
    passed_tests: Vec<String>,
    failed_tests: Vec<String>,
    ignored_tests: Vec<String>,
    seen_passed: BTreeSet<String>,
    seen_failed: BTreeSet<String>,
    seen_ignored: BTreeSet<String>,
    compile_error_count: u32,
}

impl OutputAccumulator {
    fn new(worktree: &Path, max_capture_bytes: usize) -> Self {
        Self {
            worktree: worktree.to_path_buf(),
            max_capture_bytes,
            captured_bytes: 0,
            stderr_retained: String::new(),
            parsed_passed: 0,
            parsed_failed: 0,
            parsed_ignored: 0,
            used_explicit_counts: false,
            passed_tests: Vec::new(),
            failed_tests: Vec::new(),
            ignored_tests: Vec::new(),
            seen_passed: BTreeSet::new(),
            seen_failed: BTreeSet::new(),
            seen_ignored: BTreeSet::new(),
            compile_error_count: 0,
        }
    }

    fn observe_line(
        &mut self,
        stream: StreamKind,
        line: &str,
        parsers: &CompiledOutputParsers,
        current_target: &mut Option<String>,
    ) {
        let is_compile_err = parsers.is_compile_error(line);
        if is_compile_err {
            self.compile_error_count = self.compile_error_count.saturating_add(1);
        }

        let is_noise = !is_compile_err && parsers.is_noise(line);

        if let Some(n) = extract_count(&parsers.passed_count_re, line) {
            self.parsed_passed = self.parsed_passed.saturating_add(n);
        }
        if let Some(n) = extract_count(&parsers.failed_count_re, line) {
            self.parsed_failed = self.parsed_failed.saturating_add(n);
        }
        if let Some(n) = extract_count(&parsers.ignored_count_re, line) {
            self.parsed_ignored = self.parsed_ignored.saturating_add(n);
        }

        if let Some(tests) = &parsers.tests {
            if let Some(target_re) = &tests.target {
                if let Some(caps) = target_re.captures(line) {
                    if let Some(m) = caps.get(1) {
                        let raw = m.as_str().trim();
                        let normalized = if let Ok(rel) = Path::new(raw).strip_prefix(&self.worktree)
                        {
                            rel.to_string_lossy().into_owned()
                        } else {
                            raw.trim_start_matches('/').to_owned()
                        };
                        if !normalized.is_empty() {
                            *current_target = Some(normalized);
                        }
                    }
                }
            }
            if let Some(caps) = tests.passed.captures(line) {
                if let Some(m) = caps.get(1) {
                    let q = qualify_test_id(m.as_str().trim(), current_target.as_deref());
                    if self.seen_passed.insert(q.clone()) {
                        self.passed_tests.push(q);
                    }
                }
            }
            if let Some(failed_re) = &tests.failed {
                if let Some(caps) = failed_re.captures(line) {
                    if let Some(m) = caps.get(1) {
                        let q = qualify_test_id(m.as_str().trim(), current_target.as_deref());
                        if self.seen_failed.insert(q.clone()) {
                            self.failed_tests.push(q);
                        }
                    }
                }
            }
            if let Some(ignored_re) = &tests.ignored {
                if let Some(caps) = ignored_re.captures(line) {
                    let raw = caps.get(1).map_or_else(|| line.trim(), |m| m.as_str().trim());
                    let q = qualify_test_id(raw, current_target.as_deref());
                    if self.seen_ignored.insert(q.clone()) {
                        self.ignored_tests.push(q);
                    }
                }
            }
        }

        if !is_noise {
            let line_len = line.len().saturating_add(1);
            if self.captured_bytes < self.max_capture_bytes {
                let avail = self.max_capture_bytes - self.captured_bytes;
                let take = line.len().min(avail);
                let cut = crate::runner::utf8_floor_boundary(line, take);
                self.captured_bytes = self.captured_bytes.saturating_add(cut + 1);
                if stream == StreamKind::Stderr {
                    if !self.stderr_retained.is_empty() {
                        self.stderr_retained.push('\n');
                    }
                    self.stderr_retained.push_str(&line[..cut]);
                }
            } else if stream == StreamKind::Stderr {
                // Even if capture limit is reached, preserve bounded stderr tail for diagnostics.
                if !self.stderr_retained.is_empty() {
                    self.stderr_retained.push('\n');
                }
                self.stderr_retained.push_str(line);
                if self.stderr_retained.len() > self.max_capture_bytes {
                    self.stderr_retained =
                        utf8_tail(&self.stderr_retained, self.max_capture_bytes).to_owned();
                }
                let _ = line_len;
            }
        }
    }

    fn finalized_counts(&self) -> (u32, u32, u32) {
        let passed = if self.used_explicit_counts || self.parsed_passed > 0 {
            self.parsed_passed
        } else {
            self.passed_tests.len() as u32
        };
        let failed = if self.used_explicit_counts || self.parsed_failed > 0 {
            self.parsed_failed
        } else {
            self.failed_tests.len() as u32
        };
        let ignored = self.parsed_ignored.max(self.ignored_tests.len() as u32);
        (passed, failed, ignored)
    }
}

fn extract_count(re: &Regex, line: &str) -> Option<u32> {
    let mut sum = 0u32;
    let mut matched = false;
    for caps in re.captures_iter(line) {
        if let Some(m) = caps.get(1) {
            if let Ok(val) = m.as_str().parse::<u32>() {
                sum = sum.saturating_add(val);
                matched = true;
            }
        }
    }
    matched.then_some(sum)
}

fn qualify_test_id(test_id: &str, target: Option<&str>) -> String {
    if test_id.contains('/') || test_id.contains('\\') {
        test_id.to_owned()
    } else if let Some(target) = target {
        if let Some(stripped) = test_id.strip_prefix(&format!("{target}::")) {
            format!("{target}::{stripped}")
        } else {
            format!("{target}::{test_id}")
        }
    } else {
        test_id.to_owned()
    }
}
