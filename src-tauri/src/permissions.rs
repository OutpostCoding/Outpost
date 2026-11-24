// ── surface: backend ──────────────────────────────────
// 3-Tier Execution Mode policy — Supervised · Standard · Turbo (default: Supervised).
// Exports: ExecutionMode, CommandDecision, PermData, PermissionStore, command_auto_approved · Related: approvals.rs, tools/mod.rs

//! 3-Tier Execution Mode policy (`Supervised` · `Standard` · `Turbo`):
//!   • `Supervised` — full review: file writes/edits/deletes/renames and all
//!     non-read-only commands pause for interactive user approval.
//!   • `Standard` — desk flow: `write_file` and `edit_file` apply automatically
//!     (backed by session snapshots), and commands matching `command_whitelist`
//!     auto-run when they clear every sandbox guard; everything else prompts.
//!   • `Turbo` — walk-away autonomy: project file mutations (backed by session
//!     snapshots) and in-project dev/test/local-git commands (including inline
//!     interpreter probes like `py -c` / `node -e`) execute automatically, while
//!     restricted, destructive, remote-push, or path-escaping commands fail-fast
//!     reject with constructive hints instead of hanging on an approval card.
//!
//! JSON-file-backed, mutex-guarded, atomic writes (like the index store).

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Execution posture for a coding session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    #[default]
    Supervised,
    Standard,
    Turbo,
}

/// Decision returned by [`PermissionStore::evaluate_command`] for `run_command`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandDecision {
    /// Execute immediately without prompting.
    AutoApprove,
    /// Pause and display an interactive Approval Card (`Supervised` & `Standard` modes).
    AskUser,
    /// Fail-fast in `Turbo` mode: do NOT show an approval card; return this instructive
    /// error string directly to the model as the tool result.
    RejectTurbo(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermData {
    /// Default execution mode for newly created coding sessions (and legacy sessions
    /// without a saved per-thread `executionMode`).
    pub default_mode: ExecutionMode,
    /// Command patterns that may auto-approve in `Standard` mode — matched as a
    /// word-boundary prefix, and only when the command also clears the safety guards
    /// in [`command_auto_approved`]. Tight patterns (e.g. "pytest", "npm test").
    pub command_whitelist: Vec<String>,
}

impl Default for PermData {
    fn default() -> Self {
        Self {
            default_mode: ExecutionMode::Supervised,
            command_whitelist: Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for PermData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawPermData {
            default_mode: Option<ExecutionMode>,
            #[serde(default)]
            auto_project_writes: bool,
            auto_commands: Option<bool>,
            #[serde(default)]
            command_whitelist: Vec<String>,
        }

        let raw = RawPermData::deserialize(deserializer)?;
        let default_mode = match raw.default_mode {
            Some(mode) => mode,
            None => {
                // Seamless one-way migration from legacy boolean switches:
                // if `auto_project_writes` was enabled, `auto_commands` was explicitly true,
                // or a pre-`auto_commands` config had a non-empty whitelist, migrate to `Standard`.
                let legacy_active = raw.auto_project_writes
                    || match raw.auto_commands {
                        Some(v) => v,
                        None => !raw.command_whitelist.is_empty(),
                    };
                if legacy_active {
                    ExecutionMode::Standard
                } else {
                    ExecutionMode::Supervised
                }
            }
        };

        Ok(Self {
            default_mode,
            command_whitelist: raw.command_whitelist,
        })
    }
}

/// System-impacting programs, shells, file-destroying/mutating cmdlets, and network
/// fetchers that NEVER auto-approve in `Standard` or `Turbo` mode. Dev tools
/// (`npm`, `cargo`, `pip`, `py`, `node`, `pytest`, `git`, `tsc`, …) are intentionally
/// NOT here. Matched on the command's leading program token (extension-stripped).
const BLACKLIST: &[&str] = &[
    // Shells / subshells
    "powershell",
    "pwsh",
    "cmd",
    "wsl",
    "bash",
    "sh",
    "zsh",
    // Destructive filesystem & file-mutation cmdlets/aliases (use native file tools instead)
    "rm",
    "del",
    "erase",
    "rmdir",
    "rd",
    "ri",
    "remove-item",
    "mv",
    "move",
    "mi",
    "move-item",
    "ren",
    "rename",
    "rni",
    "rename-item",
    "clc",
    "clear-content",
    "set-content",
    "out-file",
    "format",
    "mkfs",
    "dd",
    "diskpart",
    // Windows registry, services, system config & power
    "reg",
    "regedit",
    "netsh",
    "net",
    "sc",
    "schtasks",
    "wmic",
    "shutdown",
    "stop-computer",
    "restart-computer",
    // Process termination / detached GUI launch
    "taskkill",
    "kill",
    "spps",
    "stop-process",
    "saps",
    "start",
    "start-process",
    // Network fetch / remote access / eval / privilege escalation / system package managers
    "curl",
    "wget",
    "iwr",
    "irm",
    "invoke-webrequest",
    "invoke-restmethod",
    "certutil",
    "bitsadmin",
    "ssh",
    "scp",
    "sftp",
    "iex",
    "invoke-expression",
    "sudo",
    "runas",
    "winget",
    "choco",
    "scoop",
];

/// A JSON-file-backed permission policy, guarded by mutexes, plus the live session's
/// in-memory `active_mode` so mid-turn mode switches take effect on the very next tool call.
pub struct PermissionStore {
    path: PathBuf,
    data: Mutex<PermData>,
    active_mode: Mutex<ExecutionMode>,
}

impl PermissionStore {
    /// Open (or start) the policy at `path`. A missing/unreadable file starts in
    /// `Supervised` mode (safe default). Legacy whitelist entries that are now intrinsically
    /// auto-approved read-only git commands (e.g. `git status`, `git diff`, `git log`)
    /// or legacy shell builtins (`pwd`, `ls`, `dir`) are pruned on load so they don't
    /// linger as redundant custom pills in Settings.
    pub fn open(path: PathBuf) -> Self {
        let mut data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<PermData>(&s).ok())
            .unwrap_or_default();
        data.command_whitelist
            .retain(|p| !is_retired_whitelist_pattern(p));
        let initial_mode = data.default_mode;
        Self {
            path,
            data: Mutex::new(data),
            active_mode: Mutex::new(initial_mode),
        }
    }

    /// The current persisted policy (for the settings UI).
    pub fn get(&self) -> PermData {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the persisted policy (from the settings UI) and write atomically.
    pub fn set(&self, next: PermData) {
        {
            let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
            *d = next.clone();
        }
        self.persist(&next);
    }

    /// The configured global default mode for new coding sessions.
    pub fn default_mode(&self) -> ExecutionMode {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_mode
    }

    /// The live execution mode of the active coding session.
    pub fn active_mode(&self) -> ExecutionMode {
        *self.active_mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Update the live execution mode (at turn start or mid-turn from the Composer pill).
    pub fn set_active_mode(&self, mode: ExecutionMode) {
        *self.active_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Evaluate a `run_command` invocation under the current `active_mode`.
    pub fn evaluate_command(
        &self,
        display: &str,
        argv: Option<&[String]>,
        project_root: &Path,
    ) -> CommandDecision {
        let mode = self.active_mode();
        match mode {
            ExecutionMode::Supervised => {
                let is_ro = match argv {
                    Some(a) => {
                        argv_passes_standard_guards(a, Some(project_root))
                            && is_readonly_git_tokens(a.iter().map(|s| s.as_str()))
                    }
                    None => shell_segments_parsed(display).is_some_and(|segs| {
                        segs.iter().all(|s| {
                            tokens_pass_standard_guards(&s.tokens, Some(project_root))
                                && is_readonly_git_tokens(s.tokens.iter().map(|t| t.as_str()))
                        })
                    }),
                };
                if is_ro {
                    CommandDecision::AutoApprove
                } else {
                    CommandDecision::AskUser
                }
            }
            ExecutionMode::Standard => {
                let whitelist = self
                    .data
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .command_whitelist
                    .clone();
                let approved = match argv {
                    Some(a) => argv_auto_approved_in(display, a, &whitelist, Some(project_root)),
                    None => command_auto_approved_in(display, &whitelist, Some(project_root)),
                };
                if approved {
                    CommandDecision::AutoApprove
                } else {
                    CommandDecision::AskUser
                }
            }
            ExecutionMode::Turbo => match argv {
                Some(a) => evaluate_turbo_tokens(a, project_root),
                None => match shell_segments_parsed(display) {
                    Some(segs) => {
                        for seg in &segs {
                            let decision = evaluate_turbo_tokens(&seg.tokens, project_root);
                            if decision != CommandDecision::AutoApprove {
                                return decision;
                            }
                        }
                        CommandDecision::AutoApprove
                    }
                    None => CommandDecision::RejectTurbo(
                        "error: Command blocked in Turbo mode: shell pipes ('|'), redirection ('>', '<'), backgrounding ('&'), subexpressions ('$(...)'), or '$' inside double quotes are not allowed in shell strings. Re-issue using 'argv' (e.g. {\"argv\": [\"py\", \"-c\", \"...\"]}) to bypass shell parsing, or use native file/search tools instead of pipes and redirection."
                            .into(),
                    ),
                },
            },
        }
    }

    /// Convenience helper for shell-string auto-approval under the store's active mode
    /// (without a specific `project_root` path exemption).
    #[allow(dead_code)]
    pub fn command_auto_approved(&self, command: &str) -> bool {
        match self.active_mode() {
            ExecutionMode::Supervised => command_auto_approved(command, &[]),
            ExecutionMode::Standard => {
                let wl = self
                    .data
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .command_whitelist
                    .clone();
                command_auto_approved(command, &wl)
            }
            ExecutionMode::Turbo => match shell_segments_parsed(command) {
                Some(segs) => segs
                    .iter()
                    .all(|s| evaluate_turbo_tokens_opt(&s.tokens, None) == CommandDecision::AutoApprove),
                None => false,
            },
        }
    }

    /// Convenience helper for `argv` auto-approval under the store's active mode.
    #[allow(dead_code)]
    pub fn argv_auto_approved(&self, display: &str, argv: &[String]) -> bool {
        match self.active_mode() {
            ExecutionMode::Supervised => argv_auto_approved(display, argv, &[]),
            ExecutionMode::Standard => {
                let wl = self
                    .data
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .command_whitelist
                    .clone();
                argv_auto_approved(display, argv, &wl)
            }
            ExecutionMode::Turbo => {
                evaluate_turbo_tokens_opt(argv, None) == CommandDecision::AutoApprove
            }
        }
    }

    /// The patterns to persist so `command` would auto-approve next time in `Standard` mode —
    /// the leading tight pattern of every segment (shell form) or program/subcommand (argv form).
    /// Empty when the command can never auto-approve in `Standard` mode, so the UI hides/disables
    /// the "Always allow" button.
    pub fn remember_tokens(&self, command: &str, argv: Option<&[String]>) -> Vec<String> {
        match argv {
            Some(a) => argv_remember_tokens(a),
            None => command_remember_tokens(command),
        }
    }

    fn persist(&self, d: &PermData) {
        let tmp = self.path.with_extension("tmp");
        if let Ok(json) = serde_json::to_string_pretty(d) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

fn strip_program_extension(prog: &str) -> &str {
    for ext in &[".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stripped) = prog.strip_suffix(ext) {
            return stripped;
        }
    }
    prog
}

const INTERPRETERS: &[&str] = &[
    "py", "python", "python3", "node", "bun", "deno", "ruby", "perl", "php",
];

fn has_eval_flag<'a>(mut tokens: impl Iterator<Item = &'a str>) -> bool {
    tokens.any(|tok| {
        let t = tok.to_ascii_lowercase();
        t == "-c"
            || t.starts_with("-c")
            || t == "-e"
            || t.starts_with("-e")
            || t == "--eval"
            || t.starts_with("--eval=")
            || t == "-p"
            || t.starts_with("-p")
            || t == "--print"
            || t.starts_with("--print=")
    })
}

/// Parsed representation of one top-level shell segment (`&&` / `;`).
struct ShellSegment {
    /// Quote-stripped flat text (used by `pattern_matches` against `command_whitelist`).
    flat: String,
    /// Quote-aware tokens in this segment (spaces inside quotes stay within one token).
    tokens: Vec<String>,
}

/// Split a shell command into its top-level segments (on `&&` and `;`), honouring
/// quotes, and tokenizing each segment while preserving spaces inside quoted literals.
///
/// Returns `None` — a hard refusal, never auto-approvable — for any construct we do
/// not try to reason about: a pipe or redirection, a PowerShell
/// subexpression/escape (`$(`, `${`, `(`, `)`, `{`, `}`, backtick), a bare background `&`, a
/// newline, or an unterminated quote. Quotes are honoured so a `&&`/`;`/metachar
/// INSIDE them stays literal data, and are stripped from the returned segments —
/// but `$` and backtick are still refused inside double quotes, where PowerShell
/// expands them. Empty segments (a stray or trailing separator) are dropped; an
/// all-empty result is `None`.
fn shell_segments_parsed(command: &str) -> Option<Vec<ShellSegment>> {
    #[derive(PartialEq)]
    enum Q {
        None,
        Single,
        Double,
    }
    let mut q = Q::None;
    let mut cur_flat = String::new();
    let mut cur_tok = String::new();
    let mut in_tok = false;
    let mut cur_tokens: Vec<String> = Vec::new();
    let mut raw_segs: Vec<(String, Vec<String>)> = Vec::new();
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        match q {
            Q::None => match c {
                '\'' => {
                    q = Q::Single;
                    in_tok = true;
                }
                '"' => {
                    q = Q::Double;
                    in_tok = true;
                }
                '&' => {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        if in_tok {
                            cur_tokens.push(std::mem::take(&mut cur_tok));
                            in_tok = false;
                        }
                        raw_segs.push((
                            std::mem::take(&mut cur_flat),
                            std::mem::take(&mut cur_tokens),
                        ));
                    } else {
                        return None; // lone `&` — backgrounding
                    }
                }
                ';' => {
                    if in_tok {
                        cur_tokens.push(std::mem::take(&mut cur_tok));
                        in_tok = false;
                    }
                    raw_segs.push((
                        std::mem::take(&mut cur_flat),
                        std::mem::take(&mut cur_tokens),
                    ));
                }
                '|' | '>' | '<' | '(' | ')' | '{' | '}' | '`' => return None,
                '$' => {
                    // `$(...)`/`${...}` are evaluated by PowerShell — refuse.
                    if matches!(chars.peek(), Some('(') | Some('{')) {
                        return None;
                    }
                    cur_flat.push(c);
                    cur_tok.push(c);
                    in_tok = true;
                }
                '\n' | '\r' => return None,
                w if w.is_whitespace() => {
                    cur_flat.push(w);
                    if in_tok {
                        cur_tokens.push(std::mem::take(&mut cur_tok));
                        in_tok = false;
                    }
                }
                _ => {
                    cur_flat.push(c);
                    cur_tok.push(c);
                    in_tok = true;
                }
            },
            Q::Single => {
                if c == '\'' {
                    q = Q::None;
                } else {
                    cur_flat.push(c);
                    cur_tok.push(c);
                }
            }
            Q::Double => {
                if c == '"' {
                    q = Q::None;
                } else if c == '$' || c == '`' {
                    return None; // still expanded inside double quotes
                } else {
                    cur_flat.push(c);
                    cur_tok.push(c);
                }
            }
        }
    }
    if q != Q::None {
        return None; // unterminated quote
    }
    if in_tok {
        cur_tokens.push(cur_tok);
    }
    raw_segs.push((cur_flat, cur_tokens));

    let segs: Vec<ShellSegment> = raw_segs
        .into_iter()
        .filter_map(|(flat, tokens)| {
            let trimmed = flat.trim().to_string();
            if trimmed.is_empty() || tokens.is_empty() {
                None
            } else {
                Some(ShellSegment {
                    flat: trimmed,
                    tokens,
                })
            }
        })
        .collect();
    if segs.is_empty() {
        None
    } else {
        Some(segs)
    }
}

fn has_git_config_injection(tokens: &[String]) -> bool {
    let mut saw_subcommand = false;
    let mut idx = 1;
    while idx < tokens.len() {
        let a = tokens[idx].as_str();
        if a == "--config-env" || a.starts_with("--config-env") {
            return true;
        }
        if !saw_subcommand {
            if a == "-c" || (a.starts_with("-c") && !a.starts_with("--")) {
                return true;
            }
            if matches!(
                a,
                "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
            ) {
                idx += 2;
                continue;
            }
            if a.starts_with('-') {
                idx += 1;
                continue;
            }
            saw_subcommand = true;
        }
        idx += 1;
    }
    false
}

/// Guard one tokenized command in `Standard` / `Supervised` mode:
/// non-empty, not `$` or `BLACKLIST`, no inline interpreter eval (`-c`/`-e`),
/// no `git -c` / `--config-env`, and no parent traversal or escaping path.
fn tokens_pass_standard_guards(tokens: &[String], project_root: Option<&Path>) -> bool {
    let Some(program) = tokens.first() else {
        return false;
    };
    let prog = program.trim().to_ascii_lowercase();
    let prog = strip_program_extension(&prog);
    if prog.is_empty() || prog.starts_with('$') || BLACKLIST.contains(&prog) {
        return false;
    }
    // Block inline code evaluation on script interpreters in Standard mode (-c, -e, --eval, -p, --print)
    if INTERPRETERS.contains(&prog) && has_eval_flag(tokens.iter().skip(1).map(|s| s.as_str())) {
        return false;
    }
    // Block git `-c` and `--config-env` which can smuggle arbitrary shell commands via aliases/hooks
    if prog == "git" && has_git_config_injection(tokens) {
        return false;
    }
    !tokens
        .iter()
        .any(|tok| has_parent_traversal(tok) || token_has_escaping_path(tok, project_root))
}

fn argv_passes_standard_guards(argv: &[String], project_root: Option<&Path>) -> bool {
    tokens_pass_standard_guards(argv, project_root)
}

/// Categorized fail-fast message when `prog` is in `BLACKLIST` or starts with `$`.
fn turbo_blacklist_rejection(prog: &str) -> String {
    if prog.starts_with('$') {
        return format!(
            "Shell variable/assignment '{prog}' is restricted in Turbo mode. Pass environment or values directly to a tool via 'argv' (e.g. {{\"argv\": [\"py\", \"-c\", \"...\"]}})."
        );
    }
    match prog {
        "powershell" | "pwsh" | "cmd" | "wsl" | "bash" | "sh" | "zsh" => format!(
            "Subshell '{prog}' is restricted in Turbo mode. Run the target tool directly via 'command' or 'argv' (e.g. 'pytest', 'cargo check', or {{\"argv\": [\"py\", \"-c\", \"...\"]}})."
        ),
        "rm" | "del" | "erase" | "rmdir" | "rd" | "ri" | "remove-item" => format!(
            "Command '{prog}' is restricted in Turbo mode. Use the native 'delete_file' tool to remove individual project files (recursive directory deletion via shell is disabled in Turbo mode)."
        ),
        "mv" | "move" | "mi" | "move-item" | "ren" | "rename" | "rni" | "rename-item" => format!(
            "Command '{prog}' is restricted in Turbo mode. Use the native 'rename_file' tool to move or rename project files safely."
        ),
        "clc" | "clear-content" | "set-content" | "out-file" => format!(
            "Command '{prog}' is restricted in Turbo mode. Use the native 'write_file' or 'edit_file' tools."
        ),
        "curl" | "wget" | "iwr" | "irm" | "invoke-webrequest" | "invoke-restmethod"
        | "certutil" | "bitsadmin" | "ssh" | "scp" | "sftp" => format!(
            "Network command '{prog}' is restricted in Turbo mode. Use local test clients (e.g. 'py -c' against localhost), 'browser_open'/'browser_read', or 'web_fetch' if enabled."
        ),
        _ => format!("Command '{prog}' is restricted by security policy in Turbo mode."),
    }
}

/// Enforce Turbo git safety: local inspection, staging, commits, stashes, and branch
/// switches run freely, while remote pushes (`git push`), worktree wipes (`git reset --hard`,
/// `git clean`, `git checkout --`/`.`, `git restore` without `--staged`), force-deleting
/// branches (`git branch -D`), and config/repo overrides fail-fast reject with guidance.
fn check_git_turbo_safety(tokens: &[String]) -> Result<(), String> {
    if has_git_config_injection(tokens) {
        return Err(
            "Git configuration overrides ('-c', '--config-env') are restricted in Turbo mode."
                .into(),
        );
    }
    for tok in tokens.iter().skip(1) {
        let lower = tok.to_ascii_lowercase();
        if lower == "--git-dir"
            || lower.starts_with("--git-dir=")
            || lower == "--work-tree"
            || lower.starts_with("--work-tree=")
            || lower == "--ext-diff"
            || lower == "--exec"
            || lower.starts_with("--exec=")
        {
            return Err(format!(
                "Git flag '{tok}' is restricted in Turbo mode."
            ));
        }
    }

    // Locate the git subcommand (skipping safe global flags and `-C <dir>`).
    let mut idx = 1;
    let mut sub_opt: Option<(String, usize)> = None;
    while idx < tokens.len() {
        let tok = tokens[idx].as_str();
        if tok == "-C" {
            idx += 2;
            continue;
        }
        if tok.starts_with("-C") && tok.len() > 2 {
            idx += 1;
            continue;
        }
        if tok.starts_with('-') {
            idx += 1;
            continue;
        }
        sub_opt = Some((tok.to_ascii_lowercase(), idx));
        break;
    }

    let Some((sub, sub_idx)) = sub_opt else {
        return Ok(());
    };
    let sub_args = &tokens[sub_idx + 1..];

    match sub.as_str() {
        "push" => Err(
            "Command 'git push' is restricted in Turbo mode — keep commits local so the user can review and push them."
                .into(),
        ),
        "reset" => {
            if sub_args
                .iter()
                .any(|a| matches!(a.as_str(), "--hard" | "--merge"))
            {
                Err(
                    "Command 'git reset --hard' is restricted in Turbo mode to protect uncommitted work. Use the native 'restore_file' tool to revert individual session files."
                        .into(),
                )
            } else {
                Ok(())
            }
        }
        "clean" => Err(
            "Command 'git clean' is restricted in Turbo mode to protect untracked files. Use 'delete_file' to remove specific files created this session."
                .into(),
        ),
        "checkout" => {
            if sub_args
                .iter()
                .any(|a| matches!(a.as_str(), "--" | "." | "-f" | "--force"))
            {
                Err(
                    "Discarding working-tree changes via 'git checkout' is restricted in Turbo mode. Use 'restore_file' to revert individual session files, or 'git switch' to change branches."
                        .into(),
                )
            } else {
                Ok(())
            }
        }
        "switch" => {
            if sub_args
                .iter()
                .any(|a| matches!(a.as_str(), "-f" | "--force" | "--discard-changes"))
            {
                Err(
                    "Discarding working-tree changes via 'git switch --discard-changes' / '--force' is restricted in Turbo mode."
                        .into(),
                )
            } else {
                Ok(())
            }
        }
        "restore" => {
            let has_staged = sub_args.iter().any(|a| a == "--staged" || a == "-S");
            let has_worktree = sub_args
                .iter()
                .any(|a| matches!(a.as_str(), "--worktree" | "-W" | "-SW" | "-WS"));
            if has_staged && !has_worktree {
                Ok(())
            } else {
                Err(
                    "Discarding working-tree changes via 'git restore' is restricted in Turbo mode (only 'git restore --staged' is allowed). Use 'restore_file' to revert individual session files."
                        .into(),
                )
            }
        }
        "branch" => {
            let force_del = sub_args.iter().any(|a| a == "-D")
                || (sub_args.iter().any(|a| a == "-d" || a == "--delete")
                    && sub_args.iter().any(|a| a == "-f" || a == "--force"));
            if force_del {
                Err("Force-deleting branches ('git branch -D') is restricted in Turbo mode.".into())
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Identify token indices that hold literal inline source code (e.g. `py -c "<code>"`,
/// `node -e "<code>"`, `node --eval="<code>"`) or literal commit/tag messages
/// (`git commit -m "<msg>"`) so syntax like `[...arr]`, `0..10`, or `/regex/g` inside
/// code/messages does not trigger path-traversal false positives in Turbo mode.
fn turbo_literal_arg_indices(prog: &str, tokens: &[String]) -> Vec<usize> {
    let mut exempt = Vec::new();
    if INTERPRETERS.contains(&prog) {
        let mut i = 1;
        while i < tokens.len() {
            let t = tokens[i].as_str();
            if matches!(t, "-c" | "-e" | "--eval" | "-p" | "--print") {
                exempt.push(i);
                if i + 1 < tokens.len() {
                    exempt.push(i + 1);
                }
                break;
            }
            if t.starts_with("--eval=")
                || t.starts_with("--print=")
                || (t.starts_with("-c") && t.len() > 2 && !t.starts_with("--"))
                || (t.starts_with("-e") && t.len() > 2 && !t.starts_with("--"))
                || (t.starts_with("-p") && t.len() > 2 && !t.starts_with("--"))
            {
                exempt.push(i);
                break;
            }
            i += 1;
        }
    } else if prog == "git" {
        let mut i = 1;
        while i < tokens.len() {
            let t = tokens[i].as_str();
            if matches!(t, "-m" | "--message") {
                exempt.push(i);
                if i + 1 < tokens.len() {
                    exempt.push(i + 1);
                }
                i += 2;
                continue;
            }
            if t.starts_with("--message=") || (t.starts_with("-m") && t.len() > 2 && !t.starts_with("--")) {
                exempt.push(i);
            }
            i += 1;
        }
    }
    exempt
}

fn evaluate_turbo_tokens(tokens: &[String], project_root: &Path) -> CommandDecision {
    evaluate_turbo_tokens_opt(tokens, Some(project_root))
}

fn evaluate_turbo_tokens_opt(tokens: &[String], project_root: Option<&Path>) -> CommandDecision {
    let Some(program) = tokens.first() else {
        return CommandDecision::RejectTurbo("error: Empty command cannot run in Turbo mode.".into());
    };
    let prog_raw = program.trim();
    if prog_raw.is_empty() {
        return CommandDecision::RejectTurbo("error: Empty command cannot run in Turbo mode.".into());
    }
    let prog_lower = prog_raw.to_ascii_lowercase();
    let prog = strip_program_extension(&prog_lower);

    if prog.starts_with('$') || BLACKLIST.contains(&prog) {
        return CommandDecision::RejectTurbo(format!(
            "error: {}",
            turbo_blacklist_rejection(prog)
        ));
    }

    if prog == "git" {
        if let Err(reason) = check_git_turbo_safety(tokens) {
            return CommandDecision::RejectTurbo(format!("error: {reason}"));
        }
    }

    let exempt = turbo_literal_arg_indices(prog, tokens);
    for (idx, tok) in tokens.iter().enumerate() {
        if exempt.contains(&idx) {
            continue;
        }
        if has_parent_traversal(tok) || token_has_escaping_path(tok, project_root) {
            return CommandDecision::RejectTurbo(
                "error: Command blocked in Turbo mode: arguments may not traverse above ('..') or reference paths outside the project root. Use the 'cwd' parameter for project subdirectories."
                    .into(),
            );
        }
    }

    CommandDecision::AutoApprove
}

/// Does `cmd` match whitelist `pattern` exactly, or as a prefix at a word boundary?
/// Separators are normalized first, so a pattern written `src-tauri\test.cmd`
/// matches a command the model spelled `src-tauri/test.cmd` (and vice versa).
fn pattern_matches(cmd: &str, pattern: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() {
        return false;
    }
    let cmd = cmd.replace('\\', "/");
    let p = p.replace('\\', "/");
    cmd == p || cmd.starts_with(&format!("{p} "))
}

/// Extract the tight pattern to whitelist for this command segment.
/// For tools with subcommands (git, npm, cargo, etc.), includes the first non-flag
/// subcommand (e.g. "npm test", "git status", "cargo check"). For npm/pnpm/yarn/bun
/// with "run", includes the script target (e.g. "npm run dev").
/// Interpreters (py, python, node, etc.) require script target or -m module and never
/// fall back to bare interpreter prefixes. Single-command tools (pytest, tsc, ruff)
/// extract just the program token.
fn extract_tight_pattern(cmd: &str) -> String {
    let mut tokens = cmd.split_whitespace();
    let Some(first) = tokens.next() else {
        return String::new();
    };
    let prog = first.to_ascii_lowercase();
    let prog = strip_program_extension(&prog);
    match prog {
        "py" | "python" | "python3" => {
            if let Some(second) = tokens.next() {
                if second == "-m" {
                    if let Some(third) = tokens.next() {
                        if !third.starts_with('-') {
                            return format!("{first} {second} {third}");
                        }
                    }
                } else if !second.starts_with('-') {
                    return format!("{first} {second}");
                }
            }
            String::new()
        }
        "node" | "ruby" | "perl" | "php" => {
            if let Some(second) = tokens.next() {
                if !second.starts_with('-') {
                    return format!("{first} {second}");
                }
            }
            String::new()
        }
        "npx" => {
            if let Some(second) = tokens.next() {
                if !second.starts_with('-') {
                    return format!("{first} {second}");
                }
            }
            String::new()
        }
        "git" | "npm" | "cargo" | "pnpm" | "yarn" | "bun" | "dotnet" | "pip" => {
            if let Some(second) = tokens.next() {
                if !second.starts_with('-') {
                    if matches!(prog, "npm" | "pnpm" | "yarn" | "bun") && second == "run" {
                        if let Some(third) = tokens.next() {
                            if !third.starts_with('-') {
                                return format!("{first} {second} {third}");
                            }
                        }
                    }
                    return format!("{first} {second}");
                }
            }
            first.to_string()
        }
        _ => first.to_string(),
    }
}

/// True if a legacy whitelist pattern is an intrinsically safe, read-only inspection
/// command (e.g. `git status`, `git diff`, `git log`, `pwd`, `ls`, `dir`) that has NO
/// mutating operations and is now natively handled/auto-approved. These legacy entries
/// are pruned on load so they don't linger as redundant custom pills in Settings.
///
/// NOTE: Subcommands like `git branch` (which mutates via `git branch new-feature`,
/// `git branch -D`, `git branch -m`) or `git checkout` / `git switch` / `git stash`
/// MUST NOT be pruned, because users explicitly whitelist them to auto-approve those
/// mutating operations.
fn is_retired_whitelist_pattern(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if matches!(trimmed, "pwd" | "ls" | "dir") {
        return true;
    }
    let mut tokens = trimmed.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    if strip_program_extension(&first.to_ascii_lowercase()) != "git" {
        return false;
    }
    let sub = loop {
        let Some(tok) = tokens.next() else {
            return false;
        };
        if matches!(tok, "--no-pager" | "-P" | "--no-optional-locks" | "--bare") {
            continue;
        }
        if tok.starts_with('-') {
            return false;
        }
        break tok.to_ascii_lowercase();
    };
    matches!(
        sub.as_str(),
        "status"
            | "log"
            | "diff"
            | "show"
            | "blame"
            | "shortlog"
            | "rev-parse"
            | "ls-files"
            | "ls-tree"
            | "cat-file"
            | "describe"
    )
}

/// True if a sequence of tokens (from a shell segment or `argv`)
/// is a strictly read-only `git` inspection command (e.g. `git status`, `git log -n 5`,
/// `git diff`, `git show`, `git blame`, `git rev-parse`, `git branch --show-current`).
///
/// Caller MUST still enforce [`tokens_pass_standard_guards`] / [`argv_passes_guards`] so path
/// escapes (`..`, absolute paths), shell metacharacters, and `git -c` / `--config-env`
/// remain blocked. This additionally blocks subcommand flags that can write files or
/// execute external helpers (`--output`, `-o`, `--ext-diff`, `--textconv`, `--exec`,
/// `--git-dir`, `--work-tree`).
fn is_readonly_git_tokens<'a>(mut tokens: impl Iterator<Item = &'a str>) -> bool {
    let Some(first) = tokens.next() else {
        return false;
    };
    let prog = first.trim().to_ascii_lowercase();
    if strip_program_extension(&prog) != "git" {
        return false;
    }

    // Consume safe read-only global flags before the subcommand.
    let sub = loop {
        let Some(tok) = tokens.next() else {
            return false; // bare `git` with no subcommand
        };
        if matches!(tok, "--no-pager" | "-P" | "--no-optional-locks" | "--bare") {
            continue;
        }
        if tok.starts_with('-') {
            return false; // reject global flags like -C, --git-dir, --work-tree, -c, --exec-path
        }
        break tok.to_ascii_lowercase();
    };

    let rest: Vec<&str> = tokens.collect();

    // Disallow flags that write output to disk, invoke external helpers, or redirect repo paths.
    let has_unsafe_flag = rest.iter().any(|raw| {
        let t = raw.trim_matches(['"', '\'']).to_ascii_lowercase();
        t == "-o"
            || t.starts_with("-o")
            || t == "--output"
            || t.starts_with("--output=")
            || t == "--ext-diff"
            || t == "--textconv"
            || t == "--exec"
            || t.starts_with("--exec=")
            || t == "--git-dir"
            || t.starts_with("--git-dir=")
            || t == "--work-tree"
            || t.starts_with("--work-tree=")
    });
    if has_unsafe_flag {
        return false;
    }

    match sub.as_str() {
        "status" | "log" | "diff" | "show" | "blame" | "shortlog" | "rev-parse" | "ls-files"
        | "ls-tree" | "cat-file" | "describe" => true,
        "reflog" => {
            // `git reflog` / `git reflog show` are read-only; `expire` and `delete` mutate.
            !rest.iter().any(|raw| {
                let t = raw.trim_matches(['"', '\'']).to_ascii_lowercase();
                matches!(t.as_str(), "expire" | "delete" | "exists")
            })
        }
        "branch" => {
            // Read-only only when listing/querying (no positional branch creation or -d/-D/-m/-M/-c/-C/-f).
            rest.iter().all(|raw| {
                let t = raw.trim_matches(['"', '\'']);
                matches!(
                    t,
                    "-a" | "--all"
                        | "-r"
                        | "--remotes"
                        | "-v"
                        | "-vv"
                        | "--verbose"
                        | "-l"
                        | "--list"
                        | "--show-current"
                        | "--merged"
                        | "--no-merged"
                        | "--color"
                        | "--no-color"
                        | "--abbrev"
                        | "--no-abbrev"
                        | "--column"
                        | "--no-column"
                ) || t.starts_with("--sort=")
                    || t.starts_with("--format=")
                    || t.starts_with("--points-at=")
                    || t.starts_with("--contains=")
                    || t.starts_with("--no-contains=")
                    || t.starts_with("--merged=")
                    || t.starts_with("--no-merged=")
            })
        }
        "remote" => {
            // Bare `git remote` or `git remote -v` / `--verbose` is read-only.
            rest.iter().all(|raw| {
                let t = raw.trim_matches(['"', '\'']);
                matches!(t, "-v" | "--verbose")
            })
        }
        "tag" => {
            // Bare `git tag` or `git tag -l` / `--list` (with no positional tag creation).
            rest.is_empty()
                || (rest.len() == 1
                    && matches!(rest[0].trim_matches(['"', '\'']), "-l" | "--list"))
        }
        _ => false,
    }
}

fn command_auto_approved_in(
    command: &str,
    whitelist: &[String],
    project_root: Option<&Path>,
) -> bool {
    match shell_segments_parsed(command) {
        Some(segs) => segs.iter().all(|s| {
            tokens_pass_standard_guards(&s.tokens, project_root)
                && (is_readonly_git_tokens(s.tokens.iter().map(|t| t.as_str()))
                    || whitelist.iter().any(|p| pattern_matches(&s.flat, p)))
        }),
        None => false,
    }
}

/// The security core for the shell form in `Standard` mode (pure, so it's easy to reason about).
/// A command auto-approves only if every segment clears the guards AND either is an
/// intrinsically safe read-only `git` inspection command or matches a whitelisted
/// prefix — so `git add -A && git commit -m "x"` can auto-approve under a
/// `git add` + `git commit` pattern, while `git status && rm -rf .` cannot (`rm` is blacklisted).
pub fn command_auto_approved(command: &str, whitelist: &[String]) -> bool {
    command_auto_approved_in(command, whitelist, None)
}

/// The leading tokens to add to the whitelist so THIS command's every segment would
/// auto-approve next time in `Standard` mode. Segments that are already intrinsically
/// auto-approved (read-only `git` inspection) are skipped so compound commands like
/// `git status && cargo test` only remember `cargo test`. Empty when the command can
/// never auto-approve (it fails a guard) or needs no new patterns.
pub fn command_remember_tokens(command: &str) -> Vec<String> {
    let Some(segs) = shell_segments_parsed(command) else {
        return Vec::new();
    };
    if !segs
        .iter()
        .all(|s| tokens_pass_standard_guards(&s.tokens, None))
    {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for s in &segs {
        if is_readonly_git_tokens(s.tokens.iter().map(|t| t.as_str())) {
            continue;
        }
        let pattern = extract_tight_pattern(&s.flat);
        if !pattern.is_empty() && !out.contains(&pattern) {
            out.push(pattern);
        }
    }
    out
}

/// Guards for the `argv` form of `run_command` in `Standard` mode. There is NO shell here —
/// the program runs directly with an argument vector, so metacharacters in an argument are inert
/// data, not a smuggling vector.
pub fn argv_passes_guards(argv: &[String]) -> bool {
    argv_passes_standard_guards(argv, None)
}

fn argv_auto_approved_in(
    display: &str,
    argv: &[String],
    whitelist: &[String],
    project_root: Option<&Path>,
) -> bool {
    argv_passes_standard_guards(argv, project_root)
        && (is_readonly_git_tokens(argv.iter().map(|s| s.as_str()))
            || whitelist.iter().any(|p| pattern_matches(display, p)))
}

/// Auto-approve an `argv` command in `Standard` mode: it clears the argv guards and either is an
/// intrinsically safe read-only `git` inspection command or its display string
/// matches a whitelisted prefix.
pub fn argv_auto_approved(display: &str, argv: &[String], whitelist: &[String]) -> bool {
    argv_auto_approved_in(display, argv, whitelist, None)
}

/// The program token to whitelist so this argv command would auto-approve next time in `Standard` mode.
/// Empty when it is already intrinsically auto-approved (read-only `git` inspection)
/// or can never auto-approve (blacklisted program / escaping argument).
pub fn argv_remember_tokens(argv: &[String]) -> Vec<String> {
    if !argv_passes_guards(argv) || is_readonly_git_tokens(argv.iter().map(|s| s.as_str())) {
        return Vec::new();
    }
    let Some(first) = argv.first() else {
        return Vec::new();
    };
    let prog = first.to_ascii_lowercase();
    let prog = strip_program_extension(&prog);
    match prog {
        "py" | "python" | "python3" => {
            if let Some(second) = argv.get(1) {
                if second == "-m" {
                    if let Some(third) = argv.get(2) {
                        if !third.starts_with('-') {
                            return vec![format!("{first} {second} {third}")];
                        }
                    }
                } else if !second.starts_with('-') {
                    return vec![format!("{first} {second}")];
                }
            }
            Vec::new()
        }
        "node" | "ruby" | "perl" | "php" => {
            if let Some(second) = argv.get(1) {
                if !second.starts_with('-') {
                    return vec![format!("{first} {second}")];
                }
            }
            Vec::new()
        }
        "npx" => {
            if let Some(second) = argv.get(1) {
                if !second.starts_with('-') {
                    return vec![format!("{first} {second}")];
                }
            }
            Vec::new()
        }
        "git" | "npm" | "cargo" | "pnpm" | "yarn" | "bun" | "dotnet" | "pip" => {
            if let Some(second) = argv.get(1) {
                if !second.starts_with('-') {
                    if matches!(prog, "npm" | "pnpm" | "yarn" | "bun") && second == "run" {
                        if let Some(third) = argv.get(2) {
                            if !third.starts_with('-') {
                                return vec![format!("{first} {second} {third}")];
                            }
                        }
                    }
                    return vec![format!("{first} {second}")];
                }
            }
            vec![first.clone()]
        }
        _ => vec![first.clone()],
    }
}

/// True if `tok` contains a `..` parent-directory path segment (`..`, `../foo`, `a\..\b`,
/// `--out=../x`), WITHOUT falsely matching JS spread (`[...arr]`), Python ellipsis (`...`),
/// Rust ranges (`0..10`), or git revision ranges (`main..HEAD`, `origin/main..HEAD`).
fn has_parent_traversal(tok: &str) -> bool {
    let trimmed = tok.trim().trim_matches(['"', '\'']);
    trimmed
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '\\' | '='))
        .any(|seg| seg.trim_matches(['"', '\'']) == "..")
}

/// True if `candidate` is a genuine absolute path (`C:\...` on Windows, `/...` on Unix)
/// whose canonical target (or deepest existing ancestor) resolves inside `root`, with
/// no `..` traversal or `.git` metadata access.
fn is_path_inside_root(candidate: &str, root: &Path) -> bool {
    let trimmed = candidate.trim().trim_matches(['"', '\'']);
    if trimmed.is_empty() {
        return false;
    }
    let p = Path::new(trimmed);
    if !p.is_absolute() {
        return false;
    }
    if p.components().any(|c| matches!(c, Component::ParentDir))
        || trimmed.split(['/', '\\']).any(|c| c == "..")
        || p.components()
            .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        return false;
    }
    let Ok(croot) = std::fs::canonicalize(root) else {
        return false;
    };
    let mut probe = p;
    let anchor = loop {
        if let Ok(c) = std::fs::canonicalize(probe) {
            break c;
        }
        if std::fs::symlink_metadata(probe).is_ok() {
            return false; // broken symlink or junction
        }
        match probe.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => probe = parent,
            _ => return false,
        }
    };
    anchor.starts_with(&croot)
}

fn part_has_escaping_path(part: &str, project_root: Option<&Path>) -> bool {
    let part = part.trim().trim_matches(['"', '\'']);
    if part.is_empty() || is_single_letter_flag(part) {
        return false;
    }
    let lower = part.to_ascii_lowercase();
    if lower.starts_with("file:") || lower.contains("file://") {
        return true;
    }
    if let Some(root) = project_root {
        if is_path_inside_root(part, root) {
            return false;
        }
    }
    // Single-letter Windows drive separator (`C:\` or `C:/`) anywhere in `part`,
    // without falsely matching multi-letter URL schemes (`http://`, `https://`, `ws://`).
    let bytes = part.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b':'
            && bytes[i - 1].is_ascii_alphabetic()
            && matches!(bytes.get(i + 1), Some(b'\\') | Some(b'/'))
        {
            let is_single_letter_drive = i == 1 || !bytes[i - 2].is_ascii_alphabetic();
            if is_single_letter_drive {
                return true;
            }
        }
    }
    // `/x` is unix-absolute; `\x` is Windows root-relative (`Path::is_absolute()` calls it
    // relative, yet it lands on the drive root); `C:x` is Windows drive-relative.
    matches!(bytes.first(), Some(b'/') | Some(b'\\'))
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// True if a single command argument contains a path that could leave `project_root`.
fn token_has_escaping_path(tok: &str, project_root: Option<&Path>) -> bool {
    let trimmed = tok.trim();
    if trimmed.is_empty() {
        return false;
    }
    // First allow a whole argument (including spaces in `project_root`, e.g. `"Test projects"`)
    // or a `--flag=VALUE` whose RHS resolves inside `project_root`.
    if let Some(root) = project_root {
        if is_path_inside_root(trimmed, root) {
            return false;
        }
        if let Some((lhs, rhs)) = trimmed.split_once('=') {
            if !part_has_escaping_path(lhs, project_root) && is_path_inside_root(rhs, root) {
                return false;
            }
        }
    }
    // Otherwise check whitespace- and `=`-delimited subparts so quoted strings like
    // `"foo /etc/passwd"` or `--out=C:\Windows\x` still trip the guard.
    trimmed
        .split_whitespace()
        .any(|w| w.split('=').any(|p| part_has_escaping_path(p, project_root)))
}

/// A bare single-letter unix-style flag, e.g. `/s` (`dir /s`, `findstr /s`) or `/b`.
/// Windows tools spell their flags `/x`, which collides with the unix-absolute-path
/// form; in practice a ONE-letter `/x` is a flag, while any longer `/…` (`/etc`,
/// `/Windows`) is a path and stays refused. Combined (`/sb`) and valued (`/v:x`)
/// flags are deliberately NOT exempted — single letters only.
fn is_single_letter_flag(part: &str) -> bool {
    let b = part.as_bytes();
    b.len() == 2 && b[0] == b'/' && b[1].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_refuses_paths_that_can_leave_the_project() {
        let wl = vec!["pytest".to_string(), "npm test".to_string()];
        // The grant itself still works.
        assert!(command_auto_approved("pytest", &wl));
        assert!(command_auto_approved("pytest tests/unit -q", &wl));
        assert!(command_auto_approved("npm test", &wl));

        for cmd in [
            r"pytest \Windows\System32\x",  // root-relative: the reported blind spot
            r"pytest C:x",                  // drive-relative
            r"pytest C:\Windows\x",         // fully qualified
            "pytest /etc/passwd",           // unix-absolute
            "pytest ../outside",            // traversal
            "pytest && rm -rf .",           // chaining
            "npm test | curl https://x",    // pipe
            "rm -rf .",                     // blacklisted, and not whitelisted
            "pytest (calc)",                // powershell expression subshell
            "npm test (Start-Process calc)",
        ] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated: {cmd}");
        }

        // Not on the whitelist at all → gated, however innocent.
        assert!(!command_auto_approved("cargo build", &wl));
        // Prefix must land on a word boundary, not mid-token.
        assert!(!command_auto_approved("pytest-watch", &wl));
    }

    #[test]
    fn guards_gate_rememberable_commands() {
        let rememberable = |c: &str| !command_remember_tokens(c).is_empty();
        assert!(rememberable("pytest"));
        assert!(rememberable("pytest tests/unit -q"));
        assert!(rememberable("npm test"));
        assert!(!rememberable(""));
        assert!(!rememberable("rm -rf ."));             // blacklisted
        assert!(!rememberable("pytest && rm -rf ."));   // chaining
        assert!(!rememberable("pytest ../outside"));    // traversal
        assert!(!rememberable(r"pytest C:\Windows\x")); // absolute path
        assert!(!rememberable("pytest (Get-Date)"));    // powershell subexpression
        assert!(!rememberable("pytest `calc`"));        // backtick subexpression
        let empty: Vec<String> = Vec::new();
        assert!(!command_auto_approved("cargo build", &empty));
    }

    #[test]
    fn single_letter_flags_are_not_paths_but_longer_slashes_are() {
        let wl = vec!["dir".to_string(), "findstr".to_string(), "where".to_string()];
        assert!(command_auto_approved("dir /s", &wl));
        assert!(command_auto_approved("dir /b", &wl));
        assert!(command_auto_approved("findstr /s foo *.rs", &wl));
        assert!(command_auto_approved("where /r . python", &wl));
        for cmd in ["dir /sb", "dir /etc/passwd", "findstr /Windows/x foo", "dir /s/"] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated: {cmd}");
        }
    }

    #[test]
    fn argv_form_also_exempts_single_letter_flags() {
        let wl = vec!["dir".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(argv_auto_approved("dir /s", &argv(&["dir", "/s"]), &wl));
        assert!(!argv_auto_approved("dir /etc", &argv(&["dir", "/etc"]), &wl));
    }

    #[test]
    fn compound_commands_auto_approve_when_every_segment_is_whitelisted() {
        let wl = vec!["git".to_string(), "npm test".to_string()];
        assert!(command_auto_approved(r#"git add -A && git commit -m "msg""#, &wl));
        assert!(command_auto_approved("git status; git diff", &wl));
        assert!(command_auto_approved("git add -A && npm test", &wl));
    }

    #[test]
    fn compound_commands_gate_when_any_segment_is_unwhitelisted_or_blacklisted() {
        let wl = vec!["git".to_string()];
        assert!(!command_auto_approved("git add -A && npm test", &wl));
        assert!(!command_auto_approved("git status && rm -rf .", &wl));
        assert!(!command_auto_approved("rm -rf . && git status", &wl));
    }

    #[test]
    fn pipes_redirection_and_substitution_never_auto_approve() {
        let wl = vec!["git".to_string()];
        for cmd in [
            "git status | rm -rf .",
            "git status > out.txt",
            "git status < in.txt",
            "git status $(evil)",
            "git status ${evil}",
            "git status `evil`",
            "git status &",
            "git status (calc)",
        ] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated: {cmd}");
        }
        assert!(!command_auto_approved(r#"git commit -m "unterminated"#, &wl));
    }

    #[test]
    fn quoted_metacharacters_stay_literal_in_the_shell_form() {
        let wl = vec!["git".to_string()];
        assert!(command_auto_approved(r#"git commit -m "fix: a & b""#, &wl));
        assert!(command_auto_approved(r#"git commit -m "a; b""#, &wl));
        assert!(!command_auto_approved(r#"git commit -m "hi $env:PATH""#, &wl));
        assert!(command_auto_approved(r#"git commit -m 'hi $env:PATH'"#, &wl));
    }

    #[test]
    fn argv_form_ignores_shell_metacharacters() {
        let wl = vec!["git".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(argv_auto_approved(
            r#"git commit -m "fix: a & b""#,
            &argv(&["git", "commit", "-m", "fix: a & b"]),
            &wl
        ));
        assert!(argv_auto_approved(
            "npm test -- --coverage",
            &argv(&["npm", "test", "--", "--coverage"]),
            &[("npm test").to_string()]
        ));
    }

    #[test]
    fn argv_form_still_gates_blacklisted_programs_and_escaping_paths() {
        let wl = vec!["git".to_string(), "rm".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!argv_auto_approved(
            "rm -rf .",
            &argv(&["rm", "-rf", "."]),
            &wl
        ));
        assert!(!argv_auto_approved(
            r"git add C:\Windows\x",
            &argv(&["git", "add", r"C:\Windows\x"]),
            &wl
        ));
        assert!(!argv_auto_approved(
            "git show ../outside",
            &argv(&["git", "show", "../outside"]),
            &wl
        ));
    }

    #[test]
    fn whitelist_matches_across_path_separators() {
        let bs = vec![r"src-tauri\test.cmd".to_string()];
        assert!(command_auto_approved("src-tauri/test.cmd", &bs));
        let fs = vec!["src-tauri/test.cmd".to_string()];
        assert!(command_auto_approved(r"src-tauri\test.cmd", &fs));
    }

    #[test]
    fn remember_tokens_cover_every_segment_without_duplicates() {
        assert_eq!(
            command_remember_tokens(r#"git add -A && git commit -m "msg""#),
            vec!["git add".to_string(), "git commit".to_string()]
        );
        assert_eq!(
            command_remember_tokens("git status && npm test"),
            vec!["npm test".to_string()]
        );
        assert!(command_remember_tokens("git status").is_empty());
        assert_eq!(
            command_remember_tokens("cargo check && pytest tests/"),
            vec!["cargo check".to_string(), "pytest".to_string()]
        );
        assert_eq!(
            command_remember_tokens("npm run dev"),
            vec!["npm run dev".to_string()]
        );
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            argv_remember_tokens(&argv(&["npm", "run", "test"])),
            vec!["npm run test".to_string()]
        );
        assert!(argv_remember_tokens(&argv(&["git", "status"])).is_empty());
        assert_eq!(
            argv_remember_tokens(&argv(&["git", "commit", "-m", "msg"])),
            vec!["git commit".to_string()]
        );
        assert!(command_remember_tokens("git status && rm -rf .").is_empty());
        assert!(command_remember_tokens("git status && remove-item .").is_empty());
        assert!(command_remember_tokens("git status | rm").is_empty());
        assert!(command_remember_tokens(r#"git -c alias.x=!calc x"#).is_empty());
        assert!(command_remember_tokens("invoke-expression 'echo hi'").is_empty());
        assert!(command_remember_tokens("iwr https://example.com").is_empty());
        assert!(command_remember_tokens("$env:FASTEMBED_CACHE_DIR").is_empty());
        assert!(command_remember_tokens("$x = 1").is_empty());
    }

    #[test]
    fn powershell_variables_and_expressions_never_auto_approve() {
        let wl = vec!["$env:FASTEMBED_CACHE_DIR".to_string(), "$x".to_string()];
        assert!(!command_auto_approved("$env:FASTEMBED_CACHE_DIR", &wl));
        assert!(!command_auto_approved("$x = 1", &wl));
        assert!(!command_auto_approved("$foo", &wl));
        assert!(command_remember_tokens("$env:FASTEMBED_CACHE_DIR").is_empty());
        assert!(command_remember_tokens("$x = 1").is_empty());

        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!argv_auto_approved("$foo", &argv(&["$foo"]), &wl));
        assert!(argv_remember_tokens(&argv(&["$env:VAR"])).is_empty());
    }

    #[test]
    fn git_config_injection_is_refused() {
        let wl = vec!["git".to_string(), "git status".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!command_auto_approved(r#"git -c alias.x=!calc x"#, &wl));
        assert!(!command_auto_approved(r#"git -c core.sshCommand=evil pull"#, &wl));
        assert!(!command_auto_approved(r#"git --config-env=foo=bar status"#, &wl));
        assert!(!argv_auto_approved(
            "git -c alias.x=!calc x",
            &argv(&["git", "-c", "alias.x=!calc", "x"]),
            &wl
        ));
    }

    #[test]
    fn policy_json_migration_and_roundtrip() {
        // Legacy policy with auto_project_writes: true migrates cleanly to Standard mode.
        let old_active = r#"{"auto_project_writes": true, "command_whitelist": ["pytest"], "auto_python_snippets": true}"#;
        let p: PermData = serde_json::from_str(old_active).unwrap();
        assert_eq!(p.default_mode, ExecutionMode::Standard);
        assert_eq!(p.command_whitelist, vec!["pytest".to_string()]);

        // Legacy policy with both booleans false migrates to Supervised mode.
        let old_gated = r#"{"auto_project_writes": false, "auto_commands": false, "command_whitelist": ["pytest"]}"#;
        let p_gated: PermData = serde_json::from_str(old_gated).unwrap();
        assert_eq!(p_gated.default_mode, ExecutionMode::Supervised);
        assert_eq!(p_gated.command_whitelist, vec!["pytest".to_string()]);

        // Empty legacy JSON `{}` starts Supervised.
        let empty_json: PermData = serde_json::from_str("{}").unwrap();
        assert_eq!(empty_json.default_mode, ExecutionMode::Supervised);

        // New 3-tier schema round-trips losslessly.
        let turbo_json = r#"{"default_mode": "turbo", "command_whitelist": ["cargo test"]}"#;
        let p_turbo: PermData = serde_json::from_str(turbo_json).unwrap();
        assert_eq!(p_turbo.default_mode, ExecutionMode::Turbo);
        let serialized = serde_json::to_string(&p_turbo).unwrap();
        assert!(serialized.contains(r#""default_mode":"turbo""#));
        assert!(!serialized.contains("auto_project_writes"));

        // Fresh install without file starts Supervised with empty whitelist.
        let fresh = PermData::default();
        assert_eq!(fresh.default_mode, ExecutionMode::Supervised);
        assert!(fresh.command_whitelist.is_empty());
    }

    #[test]
    fn active_mode_switches_between_supervised_standard_and_turbo() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "test_perm_modes_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let policy_path = tmp_dir.join("permissions.json");
        let store = PermissionStore::open(policy_path);
        store.set(PermData {
            default_mode: ExecutionMode::Supervised,
            command_whitelist: vec!["pytest".to_string()],
        });

        // Supervised: read-only git auto-approves, whitelisted `pytest` asks.
        store.set_active_mode(ExecutionMode::Supervised);
        assert_eq!(
            store.evaluate_command("git status", None, &tmp_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command("pytest", None, &tmp_dir),
            CommandDecision::AskUser
        );

        // Standard: whitelisted `pytest` auto-approves, unwhitelisted `cargo check` asks,
        // and `py -c` asks even if `py` were whitelisted.
        store.set_active_mode(ExecutionMode::Standard);
        assert_eq!(
            store.evaluate_command("pytest", None, &tmp_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command("cargo check", None, &tmp_dir),
            CommandDecision::AskUser
        );

        // Turbo: `cargo check` and `py -c` auto-approve; `rm` fail-fast rejects!
        store.set_active_mode(ExecutionMode::Turbo);
        assert_eq!(
            store.evaluate_command("cargo check", None, &tmp_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command(
                r#"py -c "print(1)""#,
                Some(&["py".into(), "-c".into(), "print(1)".into()]),
                &tmp_dir
            ),
            CommandDecision::AutoApprove
        );
        assert!(matches!(
            store.evaluate_command("rm -rf dist", None, &tmp_dir),
            CommandDecision::RejectTurbo(_)
        ));

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn turbo_mode_allows_inline_eval_spread_ranges_urls_and_in_project_paths() {
        let base_dir = std::env::temp_dir().join(format!(
            "test_turbo_proj with spaces_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base_dir.join("tests")).unwrap();
        let store = PermissionStore::open(base_dir.join("permissions.json"));
        store.set_active_mode(ExecutionMode::Turbo);
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Inline Python & Node with spread `[...a]`, ranges `0..10`, regexes `/a/g`, and URLs:
        assert_eq!(
            store.evaluate_command(
                r#"py -c "import os; print([x for x in range(10)])""#,
                None,
                &base_dir
            ),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command(
                r#"node -e "const a = [1, 2]; console.log([...a], 'ab'.replace(/a/g, 'b'))""#,
                Some(&argv(&[
                    "node",
                    "-e",
                    "const a = [1, 2]; console.log([...a], 'ab'.replace(/a/g, 'b'))"
                ])),
                &base_dir
            ),
            CommandDecision::AutoApprove
        );

        // Git revision ranges (`main..HEAD`), staging, and local commits (even mentioning `/api/users` in `-m`):
        assert_eq!(
            store.evaluate_command("git diff main..HEAD", None, &base_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command("git diff origin/main..HEAD", None, &base_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command(
                r#"git add -A; git commit -m "feat: add /api/users & handle ... spread""#,
                None,
                &base_dir
            ),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command("git switch -c feat/turbo", None, &base_dir),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command("git restore --staged src/lib.rs", None, &base_dir),
            CommandDecision::AutoApprove
        );

        // Localhost URLs (`http://` must NOT falsely match single-letter drive `p:/`):
        assert_eq!(
            store.evaluate_command("npx playwright test http://localhost:5173", None, &base_dir),
            CommandDecision::AutoApprove
        );

        // Explicit absolute path INSIDE `project_root` (even with spaces in folder name):
        let inside_file = base_dir.join("tests").join("unit_test.py");
        let inside_str = inside_file.display().to_string();
        assert_eq!(
            store.evaluate_command(
                &format!(r#"pytest "{inside_str}""#),
                None,
                &base_dir
            ),
            CommandDecision::AutoApprove
        );
        assert_eq!(
            store.evaluate_command(
                &format!("pytest {inside_str}"),
                Some(&argv(&["pytest", &inside_str])),
                &base_dir
            ),
            CommandDecision::AutoApprove
        );

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn turbo_mode_fail_fast_rejects_restricted_destructive_and_escaping_commands() {
        let base_dir = std::env::temp_dir().join(format!(
            "test_turbo_reject_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base_dir).unwrap();
        let store = PermissionStore::open(base_dir.join("permissions.json"));
        store.set_active_mode(ExecutionMode::Turbo);

        let expect_reject = |cmd: &str, needle: &str| {
            match store.evaluate_command(cmd, None, &base_dir) {
                CommandDecision::RejectTurbo(msg) => assert!(
                    msg.contains(needle),
                    "expected rejection for `{cmd}` to contain `{needle}`, got: {msg}"
                ),
                other => panic!("expected RejectTurbo for `{cmd}`, got: {other:?}"),
            }
        };

        // Destructive file / move / rename / content cmdlets:
        expect_reject("rm -rf dist", "delete_file");
        expect_reject("Remove-Item foo.txt", "delete_file");
        expect_reject("ri foo.txt", "delete_file");
        expect_reject("del foo.txt", "delete_file");
        expect_reject("mv a.rs b.rs", "rename_file");
        expect_reject("ren a.rs b.rs", "rename_file");
        expect_reject("Set-Content a.txt 'hi'", "write_file");

        // Subshells:
        expect_reject("powershell -Command Get-Date", "Subshell");
        expect_reject("cmd /c dir", "Subshell");
        expect_reject("bash -c 'ls'", "Subshell");

        // Network tools:
        expect_reject("curl.exe https://example.com", "Network command");
        expect_reject("iwr https://example.com", "Network command");

        // Git destructive / remote-push guards:
        expect_reject("git push origin main", "git push");
        expect_reject("git push --force", "git push");
        expect_reject("git reset --hard HEAD~1", "git reset --hard");
        expect_reject("git clean -fd", "git clean");
        expect_reject("git checkout -- .", "git checkout");
        expect_reject("git checkout .", "git checkout");
        expect_reject("git restore src/lib.rs", "git restore");
        expect_reject("git branch -D feature", "git branch -D");

        // Path escapes (`..`, external paths, `file://`):
        expect_reject("pytest ../other-repo", "outside the project root");
        expect_reject(r"py C:\Windows\System32\foo.py", "outside the project root");
        expect_reject("pytest /etc/passwd", "outside the project root");
        expect_reject("npx open file:///C:/Windows/System32", "outside the project root");
        // Trailing CLI arg after `py -c "..."` is STILL path-checked:
        expect_reject(r#"py -c "print(1)" ../outside"#, "outside the project root");

        // Shell pipes & redirection:
        expect_reject("npm test | tee out.txt", "argv");
        expect_reject("echo hi > a.txt", "argv");

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn extensions_are_stripped_for_blacklist_guards() {
        let wl = vec!["powershell".to_string(), "rm".to_string(), "del".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for cmd in [
            "powershell.exe -Command calc",
            "powershell.cmd -Command calc",
            "powershell.bat -Command calc",
            "powershell.ps1 -Command calc",
            "rm.exe -rf .",
            "del.bat /f /q *",
        ] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated: {cmd}");
            assert!(command_remember_tokens(cmd).is_empty(), "remember should be empty: {cmd}");
        }
        assert!(!argv_auto_approved("powershell.exe", &argv(&["powershell.exe"]), &wl));
        assert!(!argv_auto_approved("rm.cmd", &argv(&["rm.cmd", "-rf", "."]), &wl));
        assert!(argv_remember_tokens(&argv(&["powershell.bat"])).is_empty());
    }

    #[test]
    fn curly_braces_in_shell_segments_are_refused() {
        let wl = vec!["pytest".to_string(), "git".to_string()];
        assert!(!command_auto_approved("pytest { Start-Process calc }", &wl));
        assert!(!command_auto_approved("git status; { rm -rf . }", &wl));
        assert!(command_remember_tokens("pytest { calc }").is_empty());
        assert!(command_auto_approved("git commit -m '{fix}'", &wl));
    }

    #[test]
    fn interpreter_inline_eval_flags_are_refused_in_standard_mode() {
        let wl = vec![
            "python".to_string(),
            "py".to_string(),
            "node".to_string(),
            "ruby".to_string(),
        ];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for cmd in [
            r#"python -c "import os; os.system('calc')""#,
            r#"py -c "import os; os.system('calc')""#,
            r#"node -e "process.exit()""#,
            r#"node --eval "process.exit()""#,
            r#"ruby -e "exit""#,
            r#"python -p "print(1)""#,
            r#"python --print "1""#,
        ] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated eval flag: {cmd}");
            assert!(command_remember_tokens(cmd).is_empty(), "remember should be empty: {cmd}");
        }

        assert!(!argv_auto_approved(
            r#"python -c "exit()""#,
            &argv(&["python", "-c", "exit()"]),
            &wl
        ));
        assert!(!argv_auto_approved(
            r#"node --eval "exit()""#,
            &argv(&["node", "--eval", "exit()"]),
            &wl
        ));
        assert!(argv_remember_tokens(&argv(&["python", "-c", "exit()"])).is_empty());
    }

    #[test]
    fn interpreter_tight_patterns_require_script_or_module() {
        assert_eq!(
            command_remember_tokens("py -m pytest tests/"),
            vec!["py -m pytest".to_string()]
        );
        assert_eq!(
            command_remember_tokens("python3 -m unittest discover"),
            vec!["python3 -m unittest".to_string()]
        );
        assert_eq!(
            command_remember_tokens("python main.py --port 8080"),
            vec!["python main.py".to_string()]
        );
        assert_eq!(
            command_remember_tokens("node index.js --dev"),
            vec!["node index.js".to_string()]
        );
        assert_eq!(
            command_remember_tokens("npx vitest run"),
            vec!["npx vitest".to_string()]
        );

        assert!(command_remember_tokens("python").is_empty());
        assert!(command_remember_tokens("python.exe").is_empty());
        assert!(command_remember_tokens("py").is_empty());
        assert!(command_remember_tokens("node").is_empty());
        assert!(command_remember_tokens("npx").is_empty());

        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            argv_remember_tokens(&argv(&["py", "-m", "pytest", "tests/"])),
            vec!["py -m pytest".to_string()]
        );
        assert_eq!(
            argv_remember_tokens(&argv(&["node", "server.js"])),
            vec!["node server.js".to_string()]
        );
        assert_eq!(
            argv_remember_tokens(&argv(&["npx", "vitest"])),
            vec!["npx vitest".to_string()]
        );
        assert!(argv_remember_tokens(&argv(&["python"])).is_empty());
        assert!(argv_remember_tokens(&argv(&["node"])).is_empty());
    }

    #[test]
    fn readonly_git_inspection_auto_approves_without_whitelist() {
        let empty_wl: Vec<String> = Vec::new();
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        for cmd in [
            "git status",
            "git status --short -b",
            "git log -n 5 --stat",
            "git log --oneline -10",
            "git diff",
            "git diff main..HEAD",
            "git diff --staged src/main.rs",
            "git show HEAD:src/main.rs",
            "git blame -L 10,20 src/lib.rs",
            "git rev-parse HEAD",
            "git ls-files",
            "git branch",
            "git branch -a -v",
            "git branch --show-current",
            "git remote -v",
            "git tag -l",
            "git reflog -n 5",
            "git --no-pager log -n 3",
            "git status; git diff --stat",
        ] {
            assert!(
                command_auto_approved(cmd, &empty_wl),
                "expected read-only git command to auto-approve: {cmd}"
            );
        }

        assert!(argv_auto_approved(
            "git log -n 5 --stat",
            &argv(&["git", "log", "-n", "5", "--stat"]),
            &empty_wl
        ));
        assert!(argv_auto_approved(
            "git show HEAD:src/App.tsx",
            &argv(&["git", "show", "HEAD:src/App.tsx"]),
            &empty_wl
        ));

        // Fresh install (Supervised mode) still allows read-only git inspection.
        let tmp = std::env::temp_dir().join(format!(
            "test_git_ro_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = PermissionStore::open(tmp.clone());
        assert_eq!(store.active_mode(), ExecutionMode::Supervised);
        assert!(store.command_auto_approved("git log -n 5"));
        assert!(store.argv_auto_approved("git status", &argv(&["git", "status"])));
        assert!(!store.command_auto_approved("git commit -m 'msg'"));
        let _ = std::fs::remove_file(tmp);

        for gated in [
            "git",
            "git add .",
            "git commit -m 'x'",
            "git push",
            "git pull",
            "git checkout -b feat",
            "git switch main",
            "git reset --hard",
            "git clean -fd",
            "git stash",
            "git branch new-branch",
            "git branch -D old-branch",
            "git branch -m new-name",
            "git tag v1.0.0",
            "git remote add origin https://example.com",
            "git reflog expire --all",
            "git reflog delete HEAD@{1}",
            "git diff --output=out.patch",
            "git log -o out.txt",
            "git diff --ext-diff",
            "git show --textconv HEAD",
            "git -C ../other status",
            "git --git-dir=/tmp/repo status",
            "git -c core.pager=calc log",
            "git status && cargo test",
        ] {
            assert!(
                !command_auto_approved(gated, &empty_wl),
                "expected mutating/unsafe git command to gate: {gated}"
            );
        }
    }

    #[test]
    fn open_prunes_only_retired_read_only_commands_preserving_git_branch() {
        let tmp = std::env::temp_dir().join(format!(
            "test_perm_prune_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let json = r#"{
            "auto_project_writes": true,
            "auto_commands": true,
            "command_whitelist": [
                "git status",
                "git diff",
                "git log",
                "pwd",
                "ls",
                "dir",
                "git branch",
                "git add",
                "git commit",
                "git checkout",
                "git switch",
                "git stash",
                "cargo check"
            ]
        }"#;
        std::fs::write(&tmp, json).unwrap();
        let store = PermissionStore::open(tmp.clone());
        assert_eq!(store.get().default_mode, ExecutionMode::Standard);
        let wl = store.get().command_whitelist;
        assert_eq!(
            wl,
            vec![
                "git branch".to_string(),
                "git add".to_string(),
                "git commit".to_string(),
                "git checkout".to_string(),
                "git switch".to_string(),
                "git stash".to_string(),
                "cargo check".to_string(),
            ]
        );
        let _ = std::fs::remove_file(tmp);
    }
}
