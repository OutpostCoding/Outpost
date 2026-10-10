// ── surface: backend ──────────────────────────────────
// Process execution, background process management.
// Exports: COMMAND_TIMEOUT_SECS, Tools::run_command, Tools::process_manage

use std::path::Path;
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio_util::sync::CancellationToken;

use crate::provider::{CommandApproval, StreamEvent};
use super::*;

#[derive(Debug)]
enum RunOutcome {
    Output(std::process::Output),
    Cancelled,
    TimedOut,
    SpawnFailed(std::io::Error),
}

/// Maximum execution duration (seconds) for synchronous command execution via `run_command`.
pub const COMMAND_TIMEOUT_SECS: u64 = 120;

/// One spawn attempt with the shared cancel/timeout policy: `timeout_secs` cap (clamped to 1..=600), killed on
/// user Stop. The caller formats the failure messages (it knows the display
/// form); this just classifies the outcome.
async fn run_captured(
    cmd: &mut tokio::process::Command,
    cancel: &CancellationToken,
    timeout_secs: u64,
) -> RunOutcome {
    let timeout_duration = std::time::Duration::from_secs(timeout_secs.clamp(1, 600));
    tokio::select! {
        _ = cancel.cancelled() => RunOutcome::Cancelled,
        res = tokio::time::timeout(timeout_duration, cmd.output()) => {
            match res {
                Ok(Ok(o)) => RunOutcome::Output(o),
                Ok(Err(e)) => RunOutcome::SpawnFailed(e),
                Err(_) => RunOutcome::TimedOut,
            }
        }
    }
}



impl Tools {
    /// event, awaits the UI's captured stdout/stderr, and returns it to the model.
    pub(crate) async fn run_python(&self, arguments: &str, on_event: &Channel<StreamEvent>) -> String {
        let code = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["code"].as_str().map(str::to_string));
        let Some(code) = code else {
            return "error: run_python requires a 'code' string argument".into();
        };
        let id = self.code_runs.next_id();
        let rx = self.code_runs.register(id);
        if on_event.send(StreamEvent::RunCode { id, code }).is_err() {
            return "error: could not reach the code sandbox (UI channel closed)".into();
        }
        let output = match rx.await {
            Ok(o) => o,
            Err(_) => return "error: code execution was interrupted".into(),
        };
        let output = output.trim_end();
        if output.is_empty() {
            return "(code ran with no output)".into();
        }
        // Cap so a runaway print can't blow the model's context, preserving head and tail.
        truncate_head_tail(output, MAX_FETCH_CHARS)
    }


    pub(crate) async fn process_manage(&self, arguments: &str, cancel: &CancellationToken) -> String {
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let Some(action) = parsed.as_ref().and_then(|v| v["action"].as_str()) else {
            return "error: process_manage requires an 'action' ('check', 'stop', or 'list').".into();
        };

        match action {
            "list" => self.processes.list(),
            "stop" => {
                let Some(id) = parsed.as_ref().and_then(|v| v["id"].as_u64()) else {
                    return "error: process_manage action 'stop' requires an integer 'id'.".into();
                };
                match self.processes.stop(id) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e}"),
                }
            }
            "check" => {
                let Some(id) = parsed.as_ref().and_then(|v| v["id"].as_u64()) else {
                    return "error: process_manage action 'check' requires an integer 'id'.".into();
                };
                let wait_secs = parsed
                    .as_ref()
                    .and_then(|v| v["wait_seconds"].as_u64())
                    .unwrap_or(2)
                    .clamp(0, 60);

                if wait_secs == 0 {
                    match self.processes.logs(id, 80) {
                        Ok(logs) => {
                            let status = self.processes.status_summary(id);
                            format!("{status}\n{logs}")
                        }
                        Err(e) => format!("error: {e}"),
                    }
                } else {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            format!("Stopped by the user while checking process #{id}.")
                        }
                        res = self.processes.wait_duration(id, std::time::Duration::from_secs(wait_secs)) => {
                            match res {
                                Ok((exited, out)) => {
                                    if exited {
                                        self.processes.notify_stopped(id);
                                    }
                                    out
                                }
                                Err(e) => format!("error: {e}"),
                            }
                        }
                    }
                }
            }
            other => format!("error: unknown action '{other}'. Expected 'check', 'stop', or 'list'."),
        }
    }

    /// Backward-compatibility alias for legacy `process_start` tool calls.
    pub(crate) async fn legacy_process_start(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        cancel: &CancellationToken,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let mut parsed = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("background".to_string(), json!(true));
        }
        let args_str = parsed.to_string();
        self.run_command(&args_str, project_root, cancel, on_event)
            .await
    }

    pub(crate) fn process_logs(&self, arguments: &str) -> String {
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let Some(id) = parsed.as_ref().and_then(|v| v["id"].as_u64()) else {
            return "error: process_logs requires an integer 'id'.".into();
        };
        let tail = parsed
            .as_ref()
            .and_then(|v| v["tail"].as_u64())
            .unwrap_or(80) as usize;
        match self.processes.logs(id, tail) {
            Ok(s) => s,
            Err(e) => format!("error: {e}"),
        }
    }

    pub(crate) async fn process_wait(&self, arguments: &str, cancel: &CancellationToken) -> String {
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let Some(id) = parsed.as_ref().and_then(|v| v["id"].as_u64()) else {
            return "error: process_wait requires an integer 'id'.".into();
        };
        let timeout = parsed
            .as_ref()
            .and_then(|v| v["timeout"].as_u64())
            .unwrap_or(120);
        let timeout = timeout.clamp(1, 600);

        tokio::select! {
            _ = cancel.cancelled() => {
                format!("Stopped by the user while waiting on process #{id}.")
            }
            res = self.processes.wait(id, timeout) => match res {
                Ok(s) => s,
                Err(e) => format!("error: {e}"),
            },
        }
    }

    pub(crate) fn process_stop(&self, arguments: &str) -> String {
        let Some(id) = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["id"].as_u64())
        else {
            return "error: process_stop requires an integer 'id'.".into();
        };
        match self.processes.stop(id) {
            Ok(s) => s,
            Err(e) => format!("error: {e}"),
        }
    }

    /// `browser_open` — point a single hidden "perception" webview at a LOCAL dev
    /// URL so browser_read can see what the frontend renders. Reuses the webview
    /// on subsequent calls (navigate). Restricted to localhost — this is for
    /// perceiving what you're building, not browsing the web.

    pub(crate) async fn run_command(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        cancel: &CancellationToken,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — run_command needs an active coding session."
                .into();
        };
        // Two forms, exactly one required: `command` (a shell string, legacy — for
        // shell features like &&, |, > or cmd builtins) or `argv` (program + args
        // array — spawned DIRECTLY with no shell, so nothing for cmd to mangle).
        let v = serde_json::from_str::<Value>(arguments).ok();
        let command = v
            .as_ref()
            .and_then(|v| v["command"].as_str().map(str::to_string))
            .filter(|c| !c.trim().is_empty());
        let argv: Option<Vec<String>> = v
            .as_ref()
            .and_then(|v| v["argv"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .filter(|s| !s.trim().is_empty())
                    .collect()
            })
            .filter(|a: &Vec<String>| !a.is_empty());
        let (mut command, mut argv) = match (command, argv) {
            (Some(c), None) => (c, None),
            (None, Some(a)) => (Self::display_argv(&a), Some(a)),
            (Some(_), Some(_)) => {
                return "error: run_command accepts exactly one of 'command' (shell string) or 'argv' (program + args array) — not both.".into();
            }
            (None, None) => {
                return "error: run_command requires a non-empty 'command' string or an 'argv' array.".into();
            }
        };

        // §7.1 translate slips — intercepted BEFORE the approval gate: a Hint
        // costs no click (nothing will run), and a Rewrite must update BOTH
        // argv[0] and the display string so the whitelist/preview show the
        // command that will actually execute (`py`, not `python`). Only the
        // program token is classified — `echo python` is not a slip. A bare
        // `python` (no script/module) would rewrite to a hanging REPL, so that
        // shape hints instead of running.
        #[cfg(windows)]
        {
            let program = argv
                .as_ref()
                .map(|a| a[0].as_str())
                .or_else(|| command.split_whitespace().next());
            if let Some(program) = program {
                if let Some(fix) = crate::processes::classify_slip(program) {
                    match fix {
                        crate::processes::SlipFix::Hint(hint) => return hint,
                        crate::processes::SlipFix::Rewrite(new_program) => {
                            let has_rest = match &argv {
                                Some(a) => a.len() > 1,
                                None => command.split_whitespace().count() > 1,
                            };
                            if !has_rest {
                                return "python with no arguments starts the interactive REPL, which never \
                                        exits — pass a script or `-m module` (use `py`)."
                                    .into();
                            }
                            if let Some(a) = argv.as_mut() {
                                a[0] = new_program;
                                command = Self::display_argv(a);
                            } else {
                                command = match command.split_once(char::is_whitespace) {
                                    Some((_, rest)) => format!("{new_program} {rest}"),
                                    None => new_program,
                                };
                            }
                        }
                    }
                }
            }
        }

        let parsed_args = serde_json::from_str::<Value>(arguments).ok();
        // Optional subdirectory to run in (sandboxed to the project); default = root.
        let work_dir = match parsed_args
            .as_ref()
            .and_then(|v| v["cwd"].as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
        {
            Some(sub) => match Self::project_path(root, &sub) {
                Ok(p) if p.is_dir() => p,
                Ok(_) => return format!("error: cwd '{sub}' is not a directory in the project."),
                Err(e) => return format!("error: {e}"),
            },
            None => root.to_path_buf(),
        };

        // Optional execution timeout in seconds (default 120, clamped to 1..=600).
        let timeout_secs = parsed_args
            .as_ref()
            .and_then(|v| v["timeout"].as_u64())
            .unwrap_or(COMMAND_TIMEOUT_SECS)
            .clamp(1, 600);

        let background = parsed_args
            .as_ref()
            .and_then(|v| v["background"].as_bool())
            .unwrap_or(false);
        let wait_ms = parsed_args
            .as_ref()
            .and_then(|v| v["wait_ms"].as_u64())
            .unwrap_or(2000)
            .clamp(100, 30_000);

        // Subtractive gate: approve THIS exact command in THIS folder before it runs.
        // A whitelisted command that clears the safety guards runs without a prompt;
        // everything else asks (default-deny on this uncontained surface). The argv
        // form is judged WITHOUT the shell guards: nothing is re-parsed by a shell —
        // the program and its arguments are spawned directly — so metacharacters in an
        // argument are inert data, not a smuggling vector.
        let auto_approved = match argv.as_deref() {
            Some(a) => self.permissions.argv_auto_approved(&command, a),
            None => self.permissions.command_auto_approved(&command),
        };
        if !auto_approved {
            let id = self.approvals.next_id();
            let rx = self.approvals.register(id);
            let preview = if background {
                format!(
                    "$ {command}\n\nStarts a background process in: {}",
                    work_dir.display()
                )
            } else {
                format!("$ {command}\n\nWorking directory: {}", work_dir.display())
            };
            let warning = if background {
                Some("This starts a real process that keeps running in the background until stopped — review it before approving.".into())
            } else {
                Some("This runs a real shell command on your machine — review it before approving.".into())
            };
            // The patterns an "Always allow" click would persist (the leading token of
            // every segment, or the program token for argv). Empty when the command can
            // never auto-approve — it fails a guard — so the UI disables the button
            // rather than offering one that silently no-ops. (Guards are re-checked at
            // auto-approve time; this is a hint, not a bypass.)
            let remember = self.permissions.remember_tokens(&command, argv.as_deref());
            if on_event
                .send(StreamEvent::ToolApproval {
                    id,
                    tool: "run_command".into(),
                    path: work_dir.display().to_string(),
                    diff: preview,
                    warning,
                    command: Some(CommandApproval {
                        text: command.clone(),
                        remember,
                    }),
                })
                .is_err()
            {
                return "could not request approval to run the command (UI channel closed)".into();
            }
            let decision = rx.await.unwrap_or_default();
            if decision != "approve" {
                return rejection_response(
                    &decision,
                    &format!("The user declined to run: {command}"),
                );
            }
        }

        if background {
            let old_note = if let Some(old_id) = self.processes.find_active_by_command(&command) {
                let _ = self.processes.stop(old_id);
                format!("(Stopped previous process #{old_id} running this command)\n")
            } else {
                String::new()
            };

            let label = work_dir
                .strip_prefix(root)
                .ok()
                .map(|r| {
                    let s = r.to_string_lossy().replace('\\', "/");
                    if s.is_empty() {
                        ".".to_string()
                    } else {
                        s
                    }
                })
                .unwrap_or_else(|| work_dir.display().to_string());

            let quiet = parsed_args
                .as_ref()
                .and_then(|val| val["quiet"].as_bool())
                .unwrap_or(false);
            let started = if let Some(ref a) = argv {
                self.processes
                    .start_argv(a, &command, &work_dir, &label, quiet)
            } else {
                self.processes.start(&command, &work_dir, &label, quiet)
            };

            let id = match started {
                Ok(id) => id,
                Err(e) => return format!("error starting background process: {e}"),
            };

            let wait_res = tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = self.processes.stop(id);
                    return format!("Stopped by the user while starting process #{id}.");
                }
                res = self.processes.wait_duration(id, std::time::Duration::from_millis(wait_ms)) => res,
            };

            return match wait_res {
                Ok((true, output)) => {
                    format!("{old_note}$ {command}\n{output}")
                }
                Ok((false, output)) => {
                    let pid_str = self
                        .processes
                        .get_pid(id)
                        .map(|p| format!(" (PID {p})"))
                        .unwrap_or_default();
                    format!(
                        "{old_note}Started background process #{id}{pid_str}: `{command}` — running detached.\n{output}\n\nUse process_manage(id: {id}, action: \"check\") to wait or check status, and process_manage(id: {id}, action: \"stop\") to stop it."
                    )
                }
                Err(e) => format!("error: {e}"),
            };
        }

        // Run it, scoped to the project folder, killed on timeout, output capped.
        // argv mode spawns DIRECTLY (no shell): args pass through verbatim, so
        // paths/messages with spaces or quotes just work. command mode keeps the
        // shell for features argv can't express (&&, |, >, cmd builtins) — with
        // cmd's quote-mangling as the known cost.
        let mut cmd = if let Some(argv) = &argv {
            let mut c = tokio::process::Command::new(&argv[0]);
            c.args(&argv[1..]);
            #[cfg(windows)]
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see note below)
            c
        } else {
            #[cfg(windows)]
            {
                let mut c = tokio::process::Command::new("powershell");
                c.arg("-Command").arg(&command);
                // CREATE_NO_WINDOW: run headless with no console, so a console-UI
                // program (e.g. a curses game) can't hijack the dev terminal or flash
                // a window in a packaged build — it just fails to find a console and
                // exits, rather than hanging. Normal commands write to piped
                // stdout/stderr regardless.
                c.creation_flags(0x0800_0000);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(&command);
                c
            }
        };
        cmd.current_dir(&work_dir);
        cmd.kill_on_drop(true);
        // Python UTF-8 Mode: make any child Python emit UTF-8 (and default open() to
        // UTF-8) instead of the Windows locale encoding (cp1252), so a script that
        // prints arrows/box-drawing/emoji doesn't die mid-run with UnicodeEncodeError.
        // Inherited by the `cmd /C …` child; non-Python programs ignore the var, and
        // we decode the captured output as UTF-8 (from_utf8_lossy) below.
        cmd.env("PYTHONUTF8", "1");
        cmd.env("PYTHONDONTWRITEBYTECODE", "1");

        // Race the command against Stop: if the user cancels (e.g. a command that
        // hangs on an interactive program), dropping `cmd` here kills the child
        // (kill_on_drop) so the turn doesn't stay stuck until the timeout.
        let mut outcome = run_captured(&mut cmd, cancel, timeout_secs).await;

        // §7.1: on Windows, an argv spawn that hits NotFound on a `.cmd`/`.bat`
        // shim (npm, npx, …) is retried through `cmd /C` — CreateProcess resolves
        // only real executables, so the argv form can never launch a shim. Two
        // guardrails keep this safe:
        //   • Only the CLASSIFIED miss retries (a genuine shim, per the existing
        //     `where` classification), never every NotFound.
        //   • Only join-safe args retry. `cmd /C` re-parses the joined line, so an
        //     arg that would need quoting (spaces, " & | < > ^ % !) gets mangled
        //     exactly the way the `command` form's known quote bug mangles it
        //     (a9289c7 — npx.cmd re-expanding %*). Unsafe args keep the hint
        //     below instead.
        // Approval already happened on the ORIGINAL argv display string above, so
        // the whitelist and the preview still match what the model sent — the
        // fallback is transparent to the gate (the joined retry string equals the
        // display string when every arg is join-safe).
        #[cfg(windows)]
        if let RunOutcome::SpawnFailed(e) = &outcome {
            if let Some(argv) = &argv {
                if let Some(retry) = crate::processes::shim_retry_command(&argv[0], e, argv) {
                    let mut c = tokio::process::Command::new("powershell");
                c.arg("-Command").arg(&retry);
                    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, as above
                    c.current_dir(&work_dir);
                    c.kill_on_drop(true);
                    c.env("PYTHONUTF8", "1");
                    c.env("PYTHONDONTWRITEBYTECODE", "1");
                    if let RunOutcome::Output(o) = run_captured(&mut c, cancel, timeout_secs).await {
                        outcome = RunOutcome::Output(o);
                    }
                    // The retry itself failing to spawn (cmd missing?) isn't
                    // actionable — fall through to the original error + hint.
                }
            }
        }

        let output = match outcome {
            RunOutcome::Output(o) => o,
            RunOutcome::Cancelled => {
                return format!("Stopped by the user while running: {command}")
            }
            RunOutcome::TimedOut => return format!("command timed out after {timeout_secs}s: {command}"),
            RunOutcome::SpawnFailed(e) => {
                let mut msg = format!("error launching command: {e}");
                if let Some(argv) = &argv {
                    if let Some(hint) = crate::processes::launch_failure_hint(&argv[0], &e) {
                        msg.push_str(&hint);
                    }
                }
                return msg;
            }
        };

        format_captured(output)
    }

    /// Display form of an argv list for the approval preview / permission check:
    /// args with spaces or shell metacharacters are double-quoted so the user sees
    /// exactly what runs. Display only — execution passes the raw arg vector, so
    /// nothing here is ever re-parsed.
    fn display_argv(argv: &[String]) -> String {
        argv.iter()
            .map(|a| {
                let needs_quotes = a.is_empty()
                    || a.chars()
                        .any(|c| c.is_whitespace() || "\"&|<>^%!".contains(c));
                if needs_quotes {
                    format!("\"{}\"", a.replace('"', "\\\""))
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The volatile TAIL — reference content that changes often, and therefore
    /// belongs at the END of the system block, after the stable head and
    /// directly before the transcript boundary (NOT after the transcript — see
    /// NOTES.md "single system block", 2026-08-30). Returns the pinned
    /// documents when `include_documents` is set; the pin/unpin change-note
    /// rides as its own message (`context_change`). Each part is one system
    /// message in the list; the provider adapters fold every system message —
    /// head first, then these parts in order — into ONE leading system block.
    ///
    /// Pinned files are EDITABLE by the model, so the caller gates them to
    /// turn-start (iteration 0) and the wrap-up, or a mid-loop self-edit
    /// re-injects itself in the highest-attention slot and loops.
    ///
    /// Prompt caching matches a PREFIX, so anything that changes must sit as
    /// late as possible. The index used to live in the system prompt, where
    /// Tail context methods retired: replaced by ephemeral turn-scoped reads
    /// dispatched as synthetic leading tool calls (project_read / read_note)
    /// that prune naturally into lightweight receipts after readPruneTurns.
    pub fn tail_context(
        &self,
        _project_root: Option<&Path>,
        _context_files: &[String],
        _include_documents: bool,
    ) -> Option<String> {
        None
    }


}

#[cfg(test)]
mod tests {
    use super::Tools;

    #[test]
    fn display_argv_leaves_plain_args_unquoted() {
        assert_eq!(
            Tools::display_argv(&["git".into(), "status".into()]),
            "git status"
        );
        assert_eq!(
            Tools::display_argv(&["py".into(), "script.py".into()]),
            "py script.py"
        );
    }

    #[test]
    fn display_argv_quotes_args_with_spaces_or_metachars() {
        assert_eq!(
            Tools::display_argv(&[
                "git".into(),
                "commit".into(),
                "-m".into(),
                "fix & ship".into()
            ]),
            "git commit -m \"fix & ship\""
        );
        assert_eq!(
            Tools::display_argv(&["npm".into(), "run".into(), "dev --port 3000".into()]),
            "npm run \"dev --port 3000\""
        );
    }
}

