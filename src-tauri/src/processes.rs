// ── surface: backend ──────────────────────────────────
// Managed long-running processes — start/start_argv, wait, stop, logs; slip + shim interception.
// Exports: Processes · Related: tools/mod.rs (run_command), commands.rs

//! Managed long-running processes for coding work — a dev server, a file watcher,
//! a game. `run_command` BLOCKS on the child and kills console-UI programs, so a
//! server or game just hangs the turn. These tools instead spawn the process
//! DETACHED, stream its stdout/stderr into a capped buffer the model can poll, and
//! let it be stopped on demand. It's the foundation the browser-perception layer
//! (a hidden webview pointed at the dev server) will sit on top of.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};


use tokio::process::Child;

#[cfg(windows)]
use windows::Win32::System::JobObjects::*;
#[cfg(windows)]
use windows::Win32::System::Threading::*;
#[cfg(windows)]
use windows::Win32::Foundation::*;

/// Keep only the most recent N lines per process so a chatty server can't grow
/// the buffer without bound.
const MAX_LINES: usize = 800;

#[cfg(windows)]
pub struct JobGuard {
    handle: isize,
}

#[cfg(windows)]
impl JobGuard {
    pub fn new() -> Option<Self> {
        unsafe {
            let handle = CreateJobObjectW(None, None).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ).is_err() {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Self { handle: handle.0 as isize })
        }
    }

    pub fn assign(&self, pid: u32) -> bool {
        unsafe {
            if let Ok(proc_handle) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) {
                let res = AssignProcessToJobObject(HANDLE(self.handle as _), proc_handle).is_ok();
                let _ = CloseHandle(proc_handle);
                res
            } else {
                false
            }
        }
    }

    pub fn into_raw(mut self) -> isize {
        let h = self.handle;
        self.handle = 0;
        h
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(self.handle as _));
            }
            self.handle = 0;
        }
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Output(std::process::Output),
    Cancelled,
    TimedOut,
    SpawnFailed(std::io::Error),
}

const MAX_CAPTURED_BYTES: usize = 1_048_576; // 1 MB cap per stream

/// Run a command bounded by timeout and cancel token.
/// Attaches a Windows Job Object with KILL_ON_JOB_CLOSE so when cancelled, timed out,
/// or dropped, top-level shell and all grandchildren are cleanly terminated.
/// Stdin is set to null, captured stdout/stderr streams are bounded to 1MB, and
/// a 500ms post-exit drain timeout prevents hanging if an inherited pipe handle lingers.
pub async fn run_captured_bounded(
    cmd: &mut tokio::process::Command,
    cancel: &tokio_util::sync::CancellationToken,
    timeout_secs: u64,
) -> RunOutcome {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::SpawnFailed(e),
    };

    #[cfg(windows)]
    let _job = if let Some(guard) = JobGuard::new() {
        if let Some(pid) = child.id() {
            let _ = guard.assign(pid);
        }
        Some(guard)
    } else {
        None
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));

    let stdout_buf_clone = stdout_buf.clone();
    let stdout_task = tokio::spawn(async move {
        if let Some(mut reader) = stdout {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 8192];
            loop {
                let current_len = stdout_buf_clone.lock().unwrap().len();
                if current_len >= MAX_CAPTURED_BYTES {
                    while let Ok(n) = reader.read(&mut chunk).await {
                        if n == 0 { break; }
                    }
                    break;
                }
                let to_read = std::cmp::min(chunk.len(), MAX_CAPTURED_BYTES - current_len);
                match reader.read(&mut chunk[..to_read]).await {
                    Ok(0) => break,
                    Ok(n) => {
                        stdout_buf_clone.lock().unwrap().extend_from_slice(&chunk[..n]);
                    }
                    Err(_) => break,
                }
            }
        }
    });

    let stderr_buf_clone = stderr_buf.clone();
    let stderr_task = tokio::spawn(async move {
        if let Some(mut reader) = stderr {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 8192];
            loop {
                let current_len = stderr_buf_clone.lock().unwrap().len();
                if current_len >= MAX_CAPTURED_BYTES {
                    while let Ok(n) = reader.read(&mut chunk).await {
                        if n == 0 { break; }
                    }
                    break;
                }
                let to_read = std::cmp::min(chunk.len(), MAX_CAPTURED_BYTES - current_len);
                match reader.read(&mut chunk[..to_read]).await {
                    Ok(0) => break,
                    Ok(n) => {
                        stderr_buf_clone.lock().unwrap().extend_from_slice(&chunk[..n]);
                    }
                    Err(_) => break,
                }
            }
        }
    });

    let timeout_duration = std::time::Duration::from_secs(timeout_secs.clamp(1, 600));

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            stdout_task.abort();
            stderr_task.abort();
            return RunOutcome::Cancelled;
        }
        _ = tokio::time::sleep(timeout_duration) => {
            let _ = child.start_kill();
            stdout_task.abort();
            stderr_task.abort();
            return RunOutcome::TimedOut;
        }
        res = child.wait() => {
            match res {
                Ok(s) => s,
                Err(e) => {
                    stdout_task.abort();
                    stderr_task.abort();
                    return RunOutcome::SpawnFailed(e);
                }
            }
        }
    };

    let mut stdout_task = stdout_task;
    let mut stderr_task = stderr_task;
    let drain_pipes = async {
        let _ = (&mut stdout_task).await;
        let _ = (&mut stderr_task).await;
    };

    if tokio::time::timeout(std::time::Duration::from_millis(500), drain_pipes).await.is_err() {
        stdout_task.abort();
        stderr_task.abort();
    }

    let stdout_bytes = std::mem::take(&mut *stdout_buf.lock().unwrap());
    let stderr_bytes = std::mem::take(&mut *stderr_buf.lock().unwrap());

    RunOutcome::Output(std::process::Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

struct Proc {
    command: String,
    cwd: String,
    root: String,
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
    crash_reported: bool,
    #[cfg(windows)]
    job_handle: Option<isize>,
}

impl Proc {
    #[cfg(windows)]
    fn kill_tree(&mut self) {
        if let Some(handle) = self.job_handle.take() {
            unsafe {
                let _ = CloseHandle(HANDLE(handle as _));
            }
        } else if let Some(pid) = self.child.id() {
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
        let _ = self.child.start_kill();
    }

    #[cfg(not(windows))]
    fn kill_tree(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

pub struct Processes {
    map: Mutex<HashMap<u64, Proc>>,
    counter: AtomicU64,
    app: Option<tauri::AppHandle>,
}

impl Processes {
    pub fn new(app: Option<tauri::AppHandle>) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            app,
        }
    }

    /// Spawn a detached process in `cwd` (already resolved/sandboxed by the caller),
    /// streaming its output into a capped buffer. When `quiet` is true, suppresses
    /// live `process_log` event emission so TerminalPane is not automatically opened.
    /// Returns the handle id.
    pub fn start(&self, command: &str, cwd: &Path, cwd_label: &str, quiet: bool, root: &str) -> Result<u64, String> {
        let mut cmd = build_command(command);
        cmd.current_dir(cwd);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // We own the lifetime explicitly (process_stop), so don't kill on drop —
        // the process should survive the tool call that started it.
        cmd.kill_on_drop(false);

        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut child = cmd.spawn().map_err(|e| format!("failed to start: {e}"))?;
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        if let Some(out) = child.stdout.take() {
            spawn_pump(out, logs.clone(), self.app.clone(), id, false, quiet);
        }
        if let Some(err) = child.stderr.take() {
            spawn_pump(err, logs.clone(), self.app.clone(), id, true, quiet);
        }

        #[cfg(windows)]
        let job_handle = if let Some(guard) = JobGuard::new() {
            if let Some(pid) = child.id() {
                if guard.assign(pid) {
                    Some(guard.into_raw())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        self.map.lock().unwrap().insert(
            id,
            Proc {
                command: command.to_string(),
                cwd: cwd_label.to_string(),
                root: root.to_string(),
                child,
                logs,
                crash_reported: false,
                #[cfg(windows)]
                job_handle,
            },
        );
        self.notify_started(id, command);
        Ok(id)
    }

    /// Same as [`start`], but spawns the program DIRECTLY from an argv list with
    /// no shell — the process_start sibling of run_command's argv mode, so spaces,
    /// quotes and special characters in arguments pass through verbatim (nothing
    /// for cmd to mangle). `display` is what process_logs / process_list / stop
    /// report (the joined command line the user approved).
    pub fn start_argv(
        &self,
        argv: &[String],
        display: &str,
        cwd: &Path,
        cwd_label: &str,
        quiet: bool,
        root: &str,
    ) -> Result<u64, String> {
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, same as build_command
        apply_child_env(&mut cmd);
        cmd.current_dir(cwd);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(false);

        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut child = cmd.spawn().map_err(|e| {
            let mut msg = format!("failed to start: {e}");
            if let Some(hint) = launch_failure_hint(&argv[0], &e) {
                msg.push_str(&hint);
            }
            msg
        })?;
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        if let Some(out) = child.stdout.take() {
            spawn_pump(out, logs.clone(), self.app.clone(), id, false, quiet);
        }
        if let Some(err) = child.stderr.take() {
            spawn_pump(err, logs.clone(), self.app.clone(), id, true, quiet);
        }

        #[cfg(windows)]
        let job_handle = if let Some(guard) = JobGuard::new() {
            if let Some(pid) = child.id() {
                if guard.assign(pid) {
                    Some(guard.into_raw())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        self.map.lock().unwrap().insert(
            id,
            Proc {
                command: display.to_string(),
                cwd: cwd_label.to_string(),
                root: root.to_string(),
                child,
                logs,
                crash_reported: false,
                #[cfg(windows)]
                job_handle,
            },
        );
        self.notify_started(id, display);
        Ok(id)
    }

    /// One line per managed process: id, running/exited, command, cwd.
    pub fn list(&self) -> String {
        let mut map = self.map.lock().unwrap();
        if map.is_empty() {
            return "No managed processes.".into();
        }
        let mut ids: Vec<u64> = map.keys().copied().collect();
        ids.sort_unstable();
        let mut out = String::new();
        for id in ids {
            let proc = map.get_mut(&id).unwrap();
            let status = status_of(proc);
            out.push_str(&format!(
                "#{id} [{status}] {} (in {})\n",
                proc.command, proc.cwd
            ));
        }
        out.trim_end().to_string()
    }

    /// The last `tail` log lines for one process, plus its running/exited status.
    pub fn logs(&self, id: u64, tail: usize) -> Result<String, String> {
        let mut map = self.map.lock().unwrap();
        let proc = map
            .get_mut(&id)
            .ok_or_else(|| format!("no managed process #{id} (already stopped?)"))?;
        let status = status_of(proc);
        let lines = proc.logs.lock().unwrap();
        let start = lines.len().saturating_sub(tail.max(1));
        let shown = lines[start..].join("\n");
        let head = format!(
            "#{id} `{}` [{status}] — {} log line(s)",
            proc.command,
            lines.len()
        );
        Ok(if shown.is_empty() {
            format!("{head}\n(no output yet)")
        } else {
            format!("{head}\n{shown}")
        })
    }

    /// Block until the process exits or `timeout_secs` elapses, whichever is
    /// first. Returns the exit status + current log buffer. This is the
    /// blocking primitive that lets the model wait for a build or test suite
    /// in one tool call instead of polling `process_logs` in a loop.
    ///
    /// If the process already exited before this is called, returns
    /// immediately. If the timeout elapses with the process still running,
    /// returns "still running after Ns" + current output — the model can call
    /// `wait` again or fall back to `process_logs` + `process_stop`.
    /// Wait for a managed process up to `duration`.
    /// Returns `(exited, formatted_output)` where `exited` indicates whether the
    /// process finished before the duration elapsed.
    pub async fn wait_duration(
        &self,
        id: u64,
        duration: std::time::Duration,
    ) -> Result<(bool, String), String> {
        // First, a non-blocking check: if the process already finished, return
        // its final output immediately without burning any wait time.
        {
            let mut map = self.map.lock().unwrap();
            let proc = map
                .get_mut(&id)
                .ok_or_else(|| format!("no managed process #{id} (already stopped?)"))?;
            if let Ok(Some(status)) = proc.child.try_wait() {
                proc.crash_reported = true;
                let code = status
                    .code()
                    .map(|c| format!("exited {c}"))
                    .unwrap_or_else(|| "terminated".to_string());
                // IMPORTANT: drop the lock before formatting. `format_wait_result`
                // re-locks `self.map`, and `std::sync::Mutex` is NOT re-entrant —
                // calling it here while holding the lock self-deadlocks forever.
                drop(map);
                return Ok((true, self.format_wait_result(id, &code, true)));
            }
        }

        let deadline = tokio::time::Instant::now() + duration;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                let dur_str = if duration.subsec_millis() == 0 {
                    format!("{}s", duration.as_secs())
                } else if duration.as_secs() > 0 {
                    format!("{:.1}s", duration.as_secs_f32())
                } else {
                    format!("{}ms", duration.as_millis())
                };
                return Ok((
                    false,
                    self.format_wait_result(id, &format!("still running after {dur_str}"), false),
                ));
            }
            let step = std::cmp::min(std::time::Duration::from_millis(100), deadline - now);
            tokio::time::sleep(step).await;
            let mut map = self.map.lock().unwrap();
            let proc = map
                .get_mut(&id)
                .ok_or_else(|| format!("no managed process #{id} (already stopped?)"))?;
            if let Ok(Some(status)) = proc.child.try_wait() {
                proc.crash_reported = true;
                let code = status
                    .code()
                    .map(|c| format!("exited {c}"))
                    .unwrap_or_else(|| "terminated".to_string());
                drop(map);
                return Ok((true, self.format_wait_result(id, &code, true)));
            }
        }
    }

    /// Block until a managed process exits or timeout_secs elapses.
    pub async fn wait(&self, id: u64, timeout_secs: u64) -> Result<String, String> {
        self.wait_duration(id, std::time::Duration::from_secs(timeout_secs))
            .await
            .map(|(_, out)| out)
    }

    /// Return the OS process ID (PID) if the child is still known.
    pub fn get_pid(&self, id: u64) -> Option<u32> {
        let map = self.map.lock().unwrap();
        map.get(&id).and_then(|p| p.child.id())
    }

    /// Find an active (non-exited) process running the exact same command.
    pub fn find_active_by_command(&self, cmd: &str) -> Option<u64> {
        let mut map = self.map.lock().unwrap();
        for (id, proc) in map.iter_mut() {
            if proc.command == cmd {
                if let Ok(None) = proc.child.try_wait() {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Fast one-line status summary for a process (e.g. `#1 (PID 4812) `npm run dev` [running]`).
    pub fn status_summary(&self, id: u64) -> String {
        let mut map = self.map.lock().unwrap();
        let proc = match map.get_mut(&id) {
            Some(p) => p,
            None => return format!("no managed process #{id}"),
        };
        let status = match proc.child.try_wait() {
            Ok(Some(s)) => s
                .code()
                .map(|c| format!("exited {c}"))
                .unwrap_or_else(|| "terminated".to_string()),
            Ok(None) => "running".to_string(),
            Err(e) => format!("error: {e}"),
        };
        let pid_str = proc.child.id().map(|p| format!(" (PID {p})")).unwrap_or_default();
        format!("#{id}{pid_str} `{}` [{status}]", proc.command)
    }

    /// Build the text result for a `wait` call: status header + full log buffer.
    fn format_wait_result(&self, id: u64, status: &str, exited: bool) -> String {
        let map = self.map.lock().unwrap();
        let proc = match map.get(&id) {
            Some(p) => p,
            None => return format!("no managed process #{id}"),
        };
        let lines = proc.logs.lock().unwrap();
        let body = lines.join("\n");
        let head = format!(
            "#{id} `{}` [{status}] — {} log line(s){}",
            proc.command,
            lines.len(),
            if exited { "" } else { " (so far)" },
        );
        if body.is_empty() {
            format!("{head}\n(no output)")
        } else {
            format!("{head}\n{body}")
        }
    }

    /// Emit `process_started` event to frontend so ProcessKillPill tracks it immediately.
    pub fn notify_started(&self, id: u64, command: &str) {
        if let Some(app) = &self.app {
            use tauri::Emitter;
            #[derive(serde::Serialize, Clone)]
            struct ProcessStarted {
                id: u64,
                command: String,
            }
            let _ = app.emit(
                "process_started",
                ProcessStarted {
                    id,
                    command: command.to_string(),
                },
            );
        }
    }

    /// Emit `process_stopped` event to frontend so ProcessKillPill updates immediately.
    pub fn notify_stopped(&self, id: u64) {
        if let Some(app) = &self.app {
            use tauri::Emitter;
            #[derive(serde::Serialize, Clone)]
            struct ProcessStopped {
                id: u64,
            }
            let _ = app.emit("process_stopped", ProcessStopped { id });
        }
    }

    /// Kill a process — and its child tree on Windows (a dev server spawns node
    /// children) — and forget it.
    pub fn stop(&self, id: u64) -> Result<String, String> {
        let mut proc = self
            .map
            .lock()
            .unwrap()
            .remove(&id)
            .ok_or_else(|| format!("no managed process #{id}"))?;
        proc.kill_tree();
        self.notify_stopped(id);
        Ok(format!("Stopped process #{id} (`{}`).", proc.command))
    }

    /// Kill every managed process. Called when the app exits.
    ///
    /// Managed processes are spawned with `kill_on_drop(false)` on purpose — a dev
    /// server has to outlive the tool call that started it. But nothing else reaps
    /// them: not the turn ending, not switching session, and on Windows not the app
    /// exiting either (a parent's death doesn't kill its grandchildren). Without
    /// this, quitting Outpost leaves an orphaned server holding its port until the
    /// user finds it in Task Manager — and the NEXT run then fails to bind, which
    /// surfaces as a mystery "the page won't load" (exactly what happened
    /// 2026-07-18). App exit is the right boundary: mid-session the server SHOULD
    /// stay up while you iterate.
    pub fn stop_all(&self) {
        let Ok(mut map) = self.map.lock() else { return };
        let stopped_ids: Vec<u64> = map.keys().copied().collect();
        for (_, mut proc) in map.drain() {
            proc.kill_tree();
        }
        for id in stopped_ids {
            self.notify_stopped(id);
        }
    }

    pub async fn monitor_crashes(&self) {
        let mut reported = std::collections::HashSet::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let mut map = self.map.lock().unwrap();
            let mut crashes = Vec::new();
            let mut stopped = Vec::new();
            for (&id, proc) in map.iter_mut() {
                if reported.contains(&id) { continue; }
                if let Ok(Some(status)) = proc.child.try_wait() {
                    reported.insert(id);
                    stopped.push(id);
                    if !status.success() && !proc.crash_reported {
                        proc.crash_reported = true;
                        crashes.push((id, status.code().unwrap_or(1), proc.root.clone()));
                    }
                }
            }
            drop(map);
            for &id in &stopped {
                self.notify_stopped(id);
            }
            if !crashes.is_empty() {
                if let Some(app) = &self.app {
                    for (id, code, root) in crashes {
                        let tail = {
                            if let Ok(logs) = self.logs(id, 40) {
                                logs
                            } else {
                                String::new()
                            }
                        };
                        use tauri::Emitter;
                        #[derive(serde::Serialize, Clone)]
                        struct ProcessCrash {
                            id: u64,
                            code: i32,
                            tail: String,
                            root: String,
                        }
                        let _ = app.emit("process_crash", ProcessCrash { id, code, tail, root });
                    }
                }
            }
            
            // Clean up reported list for processes that were removed
            let active_ids: std::collections::HashSet<u64> = self.map.lock().unwrap().keys().copied().collect();
            reported.retain(|id| active_ids.contains(id));
        }
    }
}

/// Running / exited status of a process (non-blocking).
fn status_of(proc: &mut Proc) -> String {
    match proc.child.try_wait() {
        Ok(Some(st)) => match st.code() {
            Some(c) => format!("exited {c}"),
            None => "terminated".to_string(),
        },
        Ok(None) => "running".to_string(),
        Err(e) => format!("unknown ({e})"),
    }
}

/// Read a child stream line-by-line into the shared capped buffer.
/// Splits on both `\n` and bare `\r` (so progress bars don't accumulate into
/// single megabyte lines), cleanly handles `\r\n` without emitting blank lines,
/// and truncates individual lines at 4,096 characters.
fn spawn_pump<R>(
    reader: R,
    logs: Arc<Mutex<Vec<String>>>,
    app: Option<tauri::AppHandle>,
    proc_id: u64,
    is_stderr: bool,
    quiet: bool,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        let mut current_line = Vec::new();
        let mut truncated = false;
        let mut last_was_cr = false;

        let push_line = |line_bytes: &mut Vec<u8>, is_trunc: &mut bool, logs: &Arc<Mutex<Vec<String>>>, app: &Option<tauri::AppHandle>| {
            let mut line = String::from_utf8_lossy(line_bytes).into_owned();
            if *is_trunc {
                line.push_str(" … [line truncated]");
            }
            line_bytes.clear();
            *is_trunc = false;

            let mut l = logs.lock().unwrap();
            l.push(line.clone());
            if !quiet {
                if let Some(app) = app {
                    use tauri::Emitter;
                    #[derive(serde::Serialize, Clone)]
                    struct ProcessLog {
                        id: u64,
                        line: String,
                        stderr: bool,
                    }
                    let _ = app.emit("process_log", ProcessLog {
                        id: proc_id,
                        line,
                        stderr: is_stderr,
                    });
                }
            }
            let over = l.len().saturating_sub(MAX_LINES);
            if over > 0 {
                l.drain(0..over);
            }
        };

        while let Ok(n) = reader.read(&mut buf).await {
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if last_was_cr {
                    last_was_cr = false;
                    if b == b'\n' {
                        continue;
                    }
                }

                if b == b'\n' || b == b'\r' {
                    if b == b'\r' {
                        last_was_cr = true;
                    }
                    push_line(&mut current_line, &mut truncated, &logs, &app);
                } else {
                    if current_line.len() < 4096 {
                        current_line.push(b);
                    } else {
                        truncated = true;
                    }
                }
            }
        }

        if !current_line.is_empty() || truncated {
            push_line(&mut current_line, &mut truncated, &logs, &app);
        }
    });
}

/// Shell wrapper for managed processes: `cmd /C` on Windows (headless, no console
/// window), `sh -c` elsewhere. Supports &&, |, > and standard shell syntax.
fn build_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (tokio inherent method)
        apply_child_env(&mut c);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        apply_child_env(&mut c);
        c
    }
}

/// Windows-only: the actionable part of an argv "program not found" launch
/// failure, decided from `where <program>` output (split from the I/O so it's
/// unit-testable). The argv form spawns via `CreateProcess`, which resolves only
/// real executables — a `.cmd`/`.bat`/`.ps1` shim like `npm`/`npx` is resolved by
/// `cmd.exe`, not by `CreateProcess` — so we classify the hit and say what to do.
#[cfg(windows)]
fn classify_where_hits(program: &str, hits: &str) -> String {
    let lower = hits
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();

    if shim_only(&lower) {
        format!("\n'{program}' is a `.cmd`/`.bat`/`.ps1` shim, not a real executable — the argv form spawns the program directly (no shell), so nothing resolves the shim. Use the `command` form (shell string) instead, e.g. `{program} ...`; the shell resolves it for you.")
    } else if !hits.trim().is_empty() {
        format!("\n`where {program}` found:\n{hits}\nbut spawning it directly still failed. Try the `command` form (shell string) if this needs a shell.")
    } else {
        format!("\n'{program}' was not found on PATH (checked with `where`). Check the program name/spelling.")
    }
}

/// §7.1 translate-slips: what to do when the model spawns a program that can't
/// be right on Windows. `Rewrite` replaces the program token (python → py) and
/// the run proceeds; `Hint` returns a one-line correction instead of spawning —
/// grep/rg/ls/which would only fail with "not recognized", and the correction
/// is cheaper than the lecture. Only the PROGRAM token is classified; a command
/// like `echo python` is not a slip.
#[cfg(windows)]
pub(crate) enum SlipFix {
    Rewrite(String),
    Hint(String),
}

#[cfg(windows)]
pub(crate) fn classify_slip(program: &str) -> Option<SlipFix> {
    match program.to_ascii_lowercase().as_str() {
        "python" => Some(SlipFix::Rewrite("py".to_string())),
        "grep" | "rg" | "findstr" | "ag" | "ack" => Some(SlipFix::Hint(
            "Do not use shell search commands — use the native 'code_search' tool (for concepts/phrases) \
             or 'grep_search' tool (for exact symbols/names) for instant, indexed results."
                .to_string(),
        )),
        "cat" | "type" | "head" | "tail" | "more" | "less" => Some(SlipFix::Hint(
            "Do not use shell commands to view files — use the native 'read_file' tool to inspect file contents."
                .to_string(),
        )),
        "ls" => Some(SlipFix::Hint(
            "ls isn't a cmd command — use the native 'list_dir' tool to inspect the project structure."
                .to_string(),
        )),
        "which" => Some(SlipFix::Hint(
            "use `where` (not `which`) to locate a program on Windows.".to_string(),
        )),
        _ => None,
    }
}

/// Whether `where` output names only shims (`.cmd`/`.bat`/`.ps1`) with no real
/// `.exe` — the case where CreateProcess can never launch the program and only
/// the shell (`cmd /C`) can. Shared by the hint and the §7.1 fallback so the
/// two can't disagree about what counts as a shim.
#[cfg(windows)]
fn shim_only(lower: &[String]) -> bool {
    let has_exe = lower.iter().any(|l| l.ends_with(".exe"));
    let shim = lower
        .iter()
        .any(|l| l.ends_with(".cmd") || l.ends_with(".bat") || l.ends_with(".ps1"));
    shim && !has_exe
}

/// The §7.1 guardrail: whether an argv list can be safely joined into a
/// `cmd /C` line WITHOUT quoting. `cmd` re-parses the joined line, so an arg
/// that would need quotes in display form (whitespace or a metacharacter) would
/// be re-mangled by the shell — exactly the a9289c7 bug class (`npx.cmd`
/// re-expanding `%*`). The char set mirrors `Tools::display_argv`: if that
/// function would quote an arg, this says no.
#[cfg(windows)]
pub(crate) fn argv_join_safe(argv: &[String]) -> bool {
    argv.iter().all(|a| {
        !a.is_empty()
            && !a.chars()
                .any(|c| c.is_whitespace() || "\"&|<>^%!".contains(c))
    })
}

/// §7.1: the argv→`cmd /C` fallback decision. On a NotFound spawn error, if
/// `where <program>` shows the program is a shim with no real .exe AND every
/// arg is join-safe, return the line to retry with (`cmd /C <line>`). Anything
/// else returns None and the caller keeps the existing hint path — a genuinely
/// missing program, a non-NotFound error, or args that would be mangled by the
/// shell must not be silently re-run.
#[cfg(windows)]
pub(crate) fn shim_retry_command(
    program: &str,
    err: &std::io::Error,
    argv: &[String],
) -> Option<String> {
    if err.kind() != std::io::ErrorKind::NotFound || !argv_join_safe(argv) {
        return None;
    }
    let out = std::process::Command::new("where").arg(program).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let lower: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .collect();
    shim_only(&lower).then(|| argv.join(" "))
}

/// Windows-only: turn a "program not found" spawn error into something the model
/// can act on. Only the `NotFound` kind is explained — anything else (permissions,
/// bad args) keeps its own message. On non-Windows the argv form resolves scripts
/// fine (shebang), so this is a no-op.
pub(crate) fn launch_failure_hint(program: &str, err: &std::io::Error) -> Option<String> {
    #[cfg(not(windows))]
    {
        let _ = (program, err);
        return None;
    }
    #[cfg(windows)]
    {
        if err.kind() != std::io::ErrorKind::NotFound {
            return None;
        }
        let out = std::process::Command::new("where")
            .arg(program)
            .output()
            .ok()?;
        if !out.status.success() {
            return Some(format!("\n'{program}' was not found on PATH (checked with `where`). Check the program name/spelling."));
        }
        Some(classify_where_hits(
            program,
            String::from_utf8_lossy(&out.stdout).as_ref(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The exact path that deadlocked before the fix: a process exits, and
    /// `wait()` calls `format_wait_result` after `try_wait()` succeeds. With
    /// the re-entrant-mutex bug this hung forever; now it must return.
    #[tokio::test]
    async fn wait_returns_when_process_already_exited() {
        let procs = Processes::new(None);
        // `cmd /C exit 0` exits immediately — by the time we call wait it's done.
        let id = procs
            .start("exit 0", Path::new("."), ".", false, ".")
            .expect("spawn");
        // Give it a moment to actually exit.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let result = procs.wait(id, 5).await.expect("wait must not hang");
        assert!(result.contains("exited 0"), "got: {result}");
    }

    /// The poll path: process is still running when wait starts, then exits
    /// mid-wait. This exercises the second deadlock site (inside the poll loop).
    #[tokio::test]
    async fn wait_returns_after_process_exits_mid_wait() {
        let procs = Processes::new(None);
        // Sleep ~600ms so the first try_wait in wait() sees it running, but the
        // ~3rd poll (at 200ms intervals) catches the exit.
        let id = procs
            .start("ping -n 2 127.0.0.1 >nul", Path::new("."), ".", false, ".")
            .expect("spawn");
        let result = procs.wait(id, 5).await.expect("wait must not hang");
        assert!(
            result.contains("exited"),
            "expected exit status, got: {result}"
        );
    }

    /// Timeout path: process runs longer than the timeout. Must return
    /// "still running" instead of hanging.
    #[tokio::test]
    async fn wait_times_out_when_process_runs_too_long() {
        let procs = Processes::new(None);
        // ping -n 30 runs ~30s; we time out after 1s.
        let id = procs
            .start("ping -n 30 127.0.0.1 >nul", Path::new("."), ".", false, ".")
            .expect("spawn");
        let start = tokio::time::Instant::now();
        let result = procs.wait(id, 1).await.expect("wait must not hang");
        let elapsed = start.elapsed();
        assert!(
            result.contains("still running after 1s"),
            "got: {result}"
        );
        // Should return shortly after the 1s timeout, not hang.
        assert!(
            elapsed < Duration::from_secs(3),
            "wait took {elapsed:?}, expected ~1s"
        );
        // Clean up so the test process doesn't linger.
        let _ = procs.stop(id);
    }

    /// argv mode spawns DIRECTLY with no shell — the process_start sibling of
    /// run_command's argv form. The display string is what the tools report.
    #[tokio::test]
    async fn start_argv_spawns_directly_and_is_waitable() {
        let procs = Processes::new(None);
        let id = procs
            .start_argv(
                &["ping".into(), "-n".into(), "1".into(), "127.0.0.1".into()],
                "ping -n 1 127.0.0.1",
                Path::new("."),
                ".",
                false,
                ".",
            )
            .expect("spawn");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let result = procs.wait(id, 5).await.expect("wait must not hang");
        assert!(result.contains("exited"), "got: {result}");
        assert!(
            procs.list().contains("ping -n 1 127.0.0.1"),
            "display string must be stored for the tools"
        );
    }

    #[tokio::test]
    async fn quiet_process_captures_logs_without_error() {
        let procs = Processes::new(None);
        let id = procs
            .start("exit 0", Path::new("."), ".", true, ".")
            .expect("spawn quiet");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let result = procs.wait(id, 5).await.expect("wait must not hang");
        assert!(result.contains("exited 0"), "got: {result}");
    }

    #[tokio::test]
    async fn wait_duration_returns_exit_state_and_output_on_fast_command() {
        let procs = Processes::new(None);
        let id = procs
            .start("exit 0", Path::new("."), ".", false, ".")
            .expect("spawn fast");
        let (exited, result) = procs
            .wait_duration(id, Duration::from_millis(1500))
            .await
            .expect("wait_duration should succeed");
        assert!(exited, "process should have exited within window");
        assert!(result.contains("exited 0"), "got: {result}");
    }

    #[tokio::test]
    async fn wait_duration_detects_still_running_and_status_helpers() {
        let procs = Processes::new(None);
        let cmd = "ping -n 10 127.0.0.1 >nul";
        let id = procs
            .start(cmd, Path::new("."), ".", false, ".")
            .expect("spawn long");
        let (exited, result) = procs
            .wait_duration(id, Duration::from_millis(300))
            .await
            .expect("wait_duration should return");
        assert!(!exited, "process should still be running");
        assert!(result.contains("still running after 300ms"), "got: {result}");

        // PID should be accessible
        let pid = procs.get_pid(id);
        assert!(pid.is_some(), "PID should exist for running process");

        // Duplicate finder
        let found = procs.find_active_by_command(cmd);
        assert_eq!(found, Some(id), "should find running process by command");

        // Status summary
        let summary = procs.status_summary(id);
        assert!(summary.contains(&format!("#{id}")), "summary: {summary}");
        assert!(summary.contains("[running]"), "summary: {summary}");

        // Stop cleanly
        let stop_res = procs.stop(id);
        assert!(stop_res.is_ok());

        // Process should no longer be found active
        assert_eq!(procs.find_active_by_command(cmd), None);
    }

    /// The shim classifier: a `.cmd`-only `where` hit must advise the command
    /// form (the npm/npx case), a real `.exe` must not be called a shim, and
    /// empty hits mean "not found".
    #[cfg(windows)]
    #[test]
    fn where_hits_classify_shim_vs_executable() {
        let shim = classify_where_hits(
            "npm",
            "C:\\Program Files\\nodejs\\npm.cmd\nC:\\Program Files\\nodejs\\npm\n",
        );
        assert!(
            shim.contains("shim") && shim.contains("command"),
            "shim hit should advise the command form, got: {shim}"
        );

        let exe = classify_where_hits("git", "C:\\Program Files\\Git\\cmd\\git.exe\n");
        assert!(
            !exe.contains("shim"),
            "a real .exe must not be called a shim, got: {exe}"
        );
        assert!(
            exe.contains("still failed"),
            "an .exe that failed to spawn should say 'still failed', got: {exe}"
        );

        let none = classify_where_hits("ghost", "");
        assert!(
            none.contains("not found on PATH"),
            "empty hits should say not found, got: {none}"
        );
    }

    /// The orchestrator only fires on NotFound, and reports genuinely-missing
    /// programs (where.exe exits non-zero).
    #[cfg(windows)]
    #[test]
    fn launch_failure_hint_fires_only_on_not_found() {
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let hint = launch_failure_hint("definitely_not_a_real_program_9x", &not_found);
        assert!(hint.is_some());
        assert!(
            hint.unwrap().contains("not found on PATH"),
            "a missing program should be reported as not on PATH"
        );

        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert!(
            launch_failure_hint("git", &denied).is_none(),
            "non-NotFound errors must not get a hint"
        );
    }

    /// Join-safety is the guardrail that keeps the cmd /C fallback from
    /// re-opening the quote-remangle bug (a9289c7): an arg that needs quoting in
    /// display form is not safe to join.
    #[cfg(windows)]
    #[test]
    fn argv_join_safe_requires_single_token_args() {
        use super::argv_join_safe;
        assert!(argv_join_safe(&["npm".into(), "run".into(), "dev".into()]));
        assert!(argv_join_safe(&["git".into(), "status".into()]));
        assert!(
            !argv_join_safe(&["echo".into(), "hello world".into()]),
            "an arg with a space must not be join-safe"
        );
        assert!(
            !argv_join_safe(&["git".into(), "commit".into(), "-m".into(), "has space".into()]),
            "a message with a space must not be join-safe"
        );
        assert!(!argv_join_safe(&["echo".into(), "a&b".into()]), "&");
        assert!(!argv_join_safe(&["echo".into(), "100%".into()]), "%");
        assert!(!argv_join_safe(&["echo".into(), "\"dev\"".into()]), "quote");
        assert!(!argv_join_safe(&["".into()]), "empty arg");
    }

    /// The shim-only classification: a .cmd/.bat/.ps1 with no real .exe is the
    /// fallback's trigger; anything else keeps the old hint path.
    #[cfg(windows)]
    #[test]
    fn shim_only_matches_only_shim_without_exe() {
        use super::shim_only;
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(shim_only(&v(&["C:\\users\\me\\npm.cmd"])));
        assert!(shim_only(&v(&["npm.cmd", "npm.ps1"])));
        assert!(!shim_only(&v(&["C:\\Program Files\\nodejs\\node.exe"])));
        assert!(
            !shim_only(&v(&["npm.cmd", "npm.exe"])),
            "a real exe exists — argv can run it"
        );
        assert!(!shim_only(&v(&[])));
    }

    /// The retry short-circuits on anything but a NotFound spawn error and on
    /// join-unsafe args (no `where` call needed for those). The happy path needs
    /// a real shim on PATH, so it skips when the machine has none.
    #[cfg(windows)]
    #[test]
    fn shim_retry_only_for_join_safe_not_found_shims() {
        use super::shim_retry_command;
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let argv = ["npm".to_string(), "run".to_string(), "dev".to_string()];

        assert_eq!(
            shim_retry_command("npm", &denied, &argv),
            None,
            "non-NotFound must not retry"
        );
        assert_eq!(
            shim_retry_command(
                "py",
                &not_found,
                &["py".to_string(), "-c".to_string(), "print(1)".to_string()]
            ),
            None,
            "join-unsafe args must not retry"
        );

        // Happy path: only meaningful when the machine actually has a `.cmd`
        // shim named `npm` (dev machines do). Skip otherwise.
        let out = std::process::Command::new("where").arg("npm").output().ok();
        let hits = out
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase())
            .unwrap_or_default();
        if !hits.lines().any(|l| l.trim().ends_with(".cmd")) {
            return;
        }
        assert_eq!(
            shim_retry_command("npm", &not_found, &argv).as_deref(),
            Some("npm run dev")
        );
    }

    /// Translate-slips: python rewrites to py (the run proceeds), the unix-isms
    /// hint instead (they'd only fail with "not recognized"), and a real program
    /// name passes through untouched.
    #[cfg(windows)]
    #[test]
    fn classify_slip_rewrites_python_and_hints_the_rest() {
        use super::{classify_slip, SlipFix};
        match classify_slip("python") {
            Some(SlipFix::Rewrite(p)) => assert_eq!(p, "py"),
            _ => panic!("python must rewrite to py"),
        }
        assert!(
            classify_slip("Python").is_some(),
            "the slip check must be case-insensitive"
        );
        for prog in [
            "grep", "rg", "findstr", "ag", "ack", "cat", "type", "head", "tail", "more", "less",
            "ls", "which",
        ] {
            assert!(
                matches!(classify_slip(prog), Some(SlipFix::Hint(_))),
                "{prog} should hint, not rewrite or run"
            );
        }
        for prog in ["py", "git", "node", "echo", "python3", "python.exe"] {
            assert!(classify_slip(prog).is_none(), "{prog} is not a slip");
        }
    }

    #[tokio::test]
    async fn spawn_pump_splits_bare_cr_and_handles_crlf_without_duplication() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let data = b"download 10%\rdownload 20%\r\ncompleted\n";
        let cursor = std::io::Cursor::new(data);
        spawn_pump(cursor, logs.clone(), None, 1, false, true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let lines = logs.lock().unwrap().clone();
        assert_eq!(
            lines,
            vec![
                "download 10%".to_string(),
                "download 20%".to_string(),
                "completed".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn spawn_pump_truncates_lines_exceeding_4096_bytes() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut long_line = vec![b'x'; 5000];
        long_line.push(b'\n');
        let cursor = std::io::Cursor::new(long_line);
        spawn_pump(cursor, logs.clone(), None, 1, false, true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let lines = logs.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with(" … [line truncated]"));
        assert!(lines[0].len() <= 4096 + " … [line truncated]".len());
    }
}

/// Environment for every managed process.
///
/// `PYTHONUNBUFFERED` is the important one: Python BLOCK-buffers stdout when it's a
/// pipe (rather than a tty), so a server's "Serving HTTP on port 8000" line can sit
/// unflushed in the buffer indefinitely and `process_logs` looks EMPTY even though
/// the process is healthy. That turns the log — the one diagnostic for a background
/// process — into a coin flip. Observed 2026-07-18 debugging a dev server.
/// `PYTHONUTF8` matches the one-shot runners so a server printing arrows or emoji
/// doesn't die on Windows cp1252. Both are ignored by non-Python programs.
fn apply_child_env(c: &mut tokio::process::Command) {
    c.env("PYTHONUNBUFFERED", "1");
    c.env("PYTHONUTF8", "1");
    c.env("GIT_PAGER", "cat");
    c.env("GIT_TERMINAL_PROMPT", "0");
}
