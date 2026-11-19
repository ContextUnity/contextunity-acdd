//! Gate command execution, role runner spawning, and topological wave orchestration.

pub mod command;
pub mod spawner;
pub mod waves;

#[cfg(test)]
mod tests;

pub(crate) use command::render_invocation_command;
pub use command::{
    execute_gate_commands, sanitize_command_process, CommandExecOutcome, CommandExecReq,
};
pub use spawner::{
    spawn_role_runner_borrowed, verify_runner_submitted, RoleSpawnBorrow, RoleSpawnOutcome,
};
pub use waves::WaveExecutionReport;

use acdd_core::errors::{ErrorCode, StateError};
pub(super) use acdd_core::format::{utf8_floor_boundary, utf8_tail};
use acdd_core::fs::{ensure_confined_dir_all, open_confined};
use acdd_core::ids::{NodeCursor, RunId};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessRunOutcome {
    pub(super) exit_code: Option<i32>,
    pub(super) timed_out: bool,
}

pub(super) fn shell_command(command: &str) -> Command {
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

pub(super) fn node_slug(cursor: &NodeCursor) -> String {
    cursor
        .to_string()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub(super) fn open_run_log_file(
    primary_worktree: &Path,
    run_id: &RunId,
    filename: &str,
) -> Result<(PathBuf, String, File), StateError> {
    let run_rel = Path::new(".acdd").join("run").join(run_id.as_str());
    let logs_rel = run_rel.join("logs");
    ensure_confined_dir_all(primary_worktree, &logs_rel).map_err(|err| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "failed preparing confined log directory '{}': {err}",
                primary_worktree.join(&logs_rel).display()
            ),
        )
    })?;

    let run_dir = primary_worktree.join(&run_rel);
    let log_relpath = format!("logs/{filename}");
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    let file = open_confined(&run_dir, Path::new(&log_relpath), &mut opts).map_err(|err| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            format!(
                "failed to open confined log file '{}': {err}",
                run_dir.join(&log_relpath).display()
            ),
        )
    })?;
    Ok((run_dir.join(&log_relpath), log_relpath, file))
}

pub(super) struct BoundedLogWriter {
    file: File,
    max_bytes: usize,
    written: usize,
}

impl BoundedLogWriter {
    pub(super) fn new(file: File, max_bytes: usize) -> Self {
        Self {
            file,
            max_bytes,
            written: 0,
        }
    }

    pub(super) fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.written >= self.max_bytes {
            return Ok(());
        }
        let remaining = self.max_bytes - self.written;
        let bytes = line.as_bytes();
        if bytes.len() < remaining {
            self.file.write_all(bytes)?;
            self.file.write_all(b"\n")?;
            self.written += bytes.len() + 1;
        } else {
            let cut = utf8_floor_boundary(line, remaining);
            if cut > 0 {
                self.file.write_all(&bytes[..cut])?;
                self.written += cut;
            }
            self.written = self.max_bytes;
        }
        Ok(())
    }
}

struct LineAccumulator {
    buf: Vec<u8>,
    truncating: bool,
    max_line_bytes: usize,
}

impl LineAccumulator {
    fn new(max_line_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            truncating: false,
            max_line_bytes,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8], on_line: &mut dyn FnMut(&str)) {
        for &byte in chunk {
            if byte == b'\r' {
                continue;
            }
            if byte == b'\n' {
                self.emit_current_line(on_line);
            } else if !self.truncating {
                if self.buf.len() < self.max_line_bytes {
                    self.buf.push(byte);
                } else {
                    self.truncating = true;
                }
            }
        }
    }

    fn finish(&mut self, on_line: &mut dyn FnMut(&str)) {
        if !self.buf.is_empty() || self.truncating {
            self.emit_current_line(on_line);
        }
    }

    fn emit_current_line(&mut self, on_line: &mut dyn FnMut(&str)) {
        let lossy = String::from_utf8_lossy(&self.buf);
        let bound = utf8_floor_boundary(&lossy, self.max_line_bytes);
        on_line(&lossy[..bound]);
        self.buf.clear();
        self.truncating = false;
    }
}

pub(super) fn run_isolated_process(
    cmd: &mut Command,
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
    max_line_bytes: usize,
    on_line: &mut dyn FnMut(StreamKind, &str) -> Result<(), StateError>,
) -> Result<ProcessRunOutcome, StateError> {
    #[cfg(unix)]
    {
        run_isolated_process_unix(cmd, stdin_bytes, timeout, max_line_bytes, on_line)
    }
    #[cfg(not(unix))]
    {
        let _ = (cmd, stdin_bytes, timeout, max_line_bytes, on_line);
        Err(StateError::new(
            ErrorCode::GateVerificationFailed,
            "process group isolation requires a Unix host",
        ))
    }
}

#[cfg(unix)]
fn run_isolated_process_unix(
    cmd: &mut Command,
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
    max_line_bytes: usize,
    on_line: &mut dyn FnMut(StreamKind, &str) -> Result<(), StateError>,
) -> Result<ProcessRunOutcome, StateError> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    cmd.process_group(0)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            format!("failed to spawn subprocess: {err}"),
        )
    })?;

    let stdin_handle = if let Some(payload) = stdin_bytes {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            StateError::new(
                ErrorCode::GateVerificationFailed,
                "child stdin pipe unavailable",
            )
        })?;
        let owned = payload.to_vec();
        Some(
            thread::Builder::new()
                .name("acdd-stdin-writer".to_owned())
                .spawn(move || {
                    let _ = stdin.write_all(&owned);
                    let _ = stdin.flush();
                })
                .map_err(|err| {
                    StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("failed to spawn stdin writer thread: {err}"),
                    )
                })?,
        )
    } else {
        None
    };

    enum PipeEvent {
        Data(StreamKind, Vec<u8>),
        End(io::Result<()>),
    }

    struct PipeReaders {
        cancel: ArcBool,
        handles: Vec<JoinHandle<()>>,
    }

    type ArcBool = std::sync::Arc<AtomicBool>;

    impl PipeReaders {
        fn spawn_reader<R: Read + AsRawFd + Send + 'static>(
            &mut self,
            mut pipe: R,
            kind: StreamKind,
            tx: mpsc::SyncSender<PipeEvent>,
        ) -> io::Result<()> {
            let fd = pipe.as_raw_fd();
            // SAFETY: fd belongs to the owned pipe and remains valid for both fcntl calls.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags == -1
                || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
            {
                return Err(io::Error::last_os_error());
            }
            let cancel = std::sync::Arc::clone(&self.cancel);
            let handle = thread::Builder::new()
                .name(format!("acdd-pipe-{kind:?}"))
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    while !cancel.load(Ordering::Acquire) {
                        match pipe.read(&mut buf) {
                            Ok(0) => {
                                let _ = send_event(&cancel, &tx, PipeEvent::End(Ok(())));
                                break;
                            }
                            Ok(n) => {
                                if !send_event(
                                    &cancel,
                                    &tx,
                                    PipeEvent::Data(kind, buf[..n].to_vec()),
                                ) {
                                    break;
                                }
                            }
                            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                                let mut pfd = libc::pollfd {
                                    fd: pipe.as_raw_fd(),
                                    events: libc::POLLIN,
                                    revents: 0,
                                };
                                // SAFETY: pfd points to a valid single pollfd struct.
                                unsafe {
                                    libc::poll(&mut pfd, 1, 10);
                                }
                            }
                            Err(err) => {
                                let _ = send_event(&cancel, &tx, PipeEvent::End(Err(err)));
                                break;
                            }
                        }
                    }
                })?;
            self.handles.push(handle);
            Ok(())
        }
    }

    fn send_event(
        cancel: &AtomicBool,
        tx: &mpsc::SyncSender<PipeEvent>,
        mut ev: PipeEvent,
    ) -> bool {
        loop {
            if cancel.load(Ordering::Acquire) {
                return false;
            }
            match tx.try_send(ev) {
                Ok(()) => return true,
                Err(mpsc::TrySendError::Disconnected(_)) => return false,
                Err(mpsc::TrySendError::Full(returned)) => {
                    ev = returned;
                    std::thread::yield_now();
                }
            }
        }
    }

    impl Drop for PipeReaders {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Release);
            for handle in self.handles.drain(..) {
                let _ = handle.join();
            }
        }
    }

    struct ManagedChild {
        child: Child,
        reaped: Option<ExitStatus>,
        cleaned_up: bool,
    }

    impl ManagedChild {
        fn new(child: Child) -> Self {
            Self {
                child,
                reaped: None,
                cleaned_up: false,
            }
        }

        fn has_exited_no_reap(&self) -> io::Result<bool> {
            loop {
                // SAFETY: zeroed siginfo_t is valid storage for waitid(WNOWAIT).
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let rc = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        self.child.id(),
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if rc == 0 {
                    // SAFETY: waitid populated siginfo_t on success.
                    return Ok(unsafe { info.si_pid() } != 0);
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }

        fn poll_exited(&mut self) -> io::Result<bool> {
            if self.reaped.is_some() {
                return Ok(true);
            }
            self.has_exited_no_reap()
        }

        fn is_harmless_kill_err(&self, err: &io::Error) -> bool {
            #[cfg(target_os = "macos")]
            {
                // XNU returns EPERM for kill(-pgid, SIGKILL) while the group leader is in P_WEXIT
                // before transitioning to SZOMB; terminate_group_and_reap immediately reaps via wait().
                matches!(err.raw_os_error(), Some(libc::ESRCH | libc::EPERM))
            }
            #[cfg(not(target_os = "macos"))]
            {
                err.raw_os_error() == Some(libc::ESRCH)
            }
        }

        fn terminate_group_and_reap(&mut self, force_kill: bool) -> io::Result<ExitStatus> {
            if self.cleaned_up {
                return if let Some(status) = self.reaped {
                    Ok(status)
                } else {
                    self.child.wait()
                };
            }
            let pid = self.child.id() as i32;
            if force_kill || !self.poll_exited().unwrap_or(false) {
                loop {
                    // SAFETY: `-pid` targets the dedicated process group created via `process_group(0)`.
                    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
                        break;
                    }
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if self.is_harmless_kill_err(&err) {
                        break;
                    }
                    return Err(err);
                }
                if self.reaped.is_none() && !self.poll_exited().unwrap_or(false) {
                    if let Err(err) = self.child.kill() {
                        if !self.is_harmless_kill_err(&err) {
                            return Err(err);
                        }
                    }
                }
            } else {
                // Even after leader exit, ensure any lingering group descendants are terminated.
                loop {
                    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
                        break;
                    }
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if self.is_harmless_kill_err(&err) {
                        break;
                    }
                    break;
                }
            }

            if let Some(status) = self.reaped {
                self.cleaned_up = true;
                return Ok(status);
            }
            let status = loop {
                match self.child.wait() {
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    res => break res?,
                }
            };
            self.reaped = Some(status);
            self.cleaned_up = true;
            Ok(status)
        }
    }

    impl Drop for ManagedChild {
        fn drop(&mut self) {
            if !self.cleaned_up {
                let _ = self.terminate_group_and_reap(true);
            }
        }
    }

    let stdout_pipe = child.stdout.take().ok_or_else(|| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            "child stdout pipe unavailable",
        )
    })?;
    let stderr_pipe = child.stderr.take().ok_or_else(|| {
        StateError::new(
            ErrorCode::GateVerificationFailed,
            "child stderr pipe unavailable",
        )
    })?;

    let mut managed = ManagedChild::new(child);
    let (tx, rx) = mpsc::sync_channel(16);
    let mut readers = PipeReaders {
        cancel: std::sync::Arc::new(AtomicBool::new(false)),
        handles: Vec::new(),
    };
    readers
        .spawn_reader(stdout_pipe, StreamKind::Stdout, tx.clone())
        .map_err(|err| {
            StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("failed to start stdout reader: {err}"),
            )
        })?;
    readers
        .spawn_reader(stderr_pipe, StreamKind::Stderr, tx)
        .map_err(|err| {
            StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("failed to start stderr reader: {err}"),
            )
        })?;

    let started = Instant::now();
    let mut stdout_acc = LineAccumulator::new(max_line_bytes);
    let mut stderr_acc = LineAccumulator::new(max_line_bytes);
    let mut ended_streams = 0u8;
    let mut child_exited = false;
    let mut timed_out = false;
    let mut callback_err: Option<StateError> = None;

    loop {
        if !child_exited {
            child_exited = managed.poll_exited().unwrap_or(false);
        }
        if child_exited && ended_streams >= 2 {
            break;
        }
        let Some(remaining) = timeout
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
        else {
            timed_out = true;
            break;
        };

        match rx.recv_timeout(remaining.min(Duration::from_millis(5))) {
            Ok(PipeEvent::Data(kind, bytes)) => {
                let acc = match kind {
                    StreamKind::Stdout => &mut stdout_acc,
                    StreamKind::Stderr => &mut stderr_acc,
                };
                acc.push_chunk(&bytes, &mut |line| {
                    if callback_err.is_none() {
                        if let Err(err) = on_line(kind, line) {
                            callback_err = Some(err);
                        }
                    }
                });
                if callback_err.is_some() {
                    break;
                }
            }
            Ok(PipeEvent::End(res)) => {
                if let Err(err) = res {
                    callback_err = Some(StateError::new(
                        ErrorCode::GateVerificationFailed,
                        format!("pipe read failed: {err}"),
                    ));
                    break;
                }
                ended_streams += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if !timed_out && callback_err.is_none() {
        stdout_acc.finish(&mut |line| {
            if callback_err.is_none() {
                if let Err(err) = on_line(StreamKind::Stdout, line) {
                    callback_err = Some(err);
                }
            }
        });
        stderr_acc.finish(&mut |line| {
            if callback_err.is_none() {
                if let Err(err) = on_line(StreamKind::Stderr, line) {
                    callback_err = Some(err);
                }
            }
        });
    }

    let status = managed
        .terminate_group_and_reap(timed_out || callback_err.is_some())
        .map_err(|err| {
            StateError::new(
                ErrorCode::GateVerificationFailed,
                format!("failed to cleanup child process group: {err}"),
            )
        })?;
    drop(readers);
    if let Some(handle) = stdin_handle {
        let _ = handle.join();
    }

    if let Some(err) = callback_err {
        return Err(err);
    }

    Ok(ProcessRunOutcome {
        exit_code: if timed_out { None } else { status.code() },
        timed_out,
    })
}
