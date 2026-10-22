// ── surface: backend ──────────────────────────────────
// Selective auto-approval — project writes, command whitelist (default: fully gated).
// Exports: PermData, PermissionStore, command_auto_approved · Related: approvals.rs, tools/mod.rs

//! Selective auto-approval policy — the subtractive gate, but with standing
//! permission granted to bounded, named categories. Default is FULLY GATED
//! (everything asks, exactly as before). Two opt-in grants:
//!   • `auto_project_writes` — project_write / project_edit run without a prompt
//!     (they're sandboxed to the project folder and recoverable via git or snapshots).
//!   • `command_whitelist` — run_command / process_start auto-approve ONLY a
//!     command that matches a whitelisted pattern AND clears every safety guard
//!     below. Everything else still asks. There is deliberately no
//!     "auto-approve all commands": an approved command runs uncontained, so that
//!     surface stays default-deny.
//!
//! JSON-file-backed, mutex-guarded, atomic writes (like the index store).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct PermData {
    /// Auto-approve the model's writes/edits to files inside the project folder.
    #[serde(default)]
    pub auto_project_writes: bool,
    /// Master toggle: whether whitelisted commands may auto-approve.
    /// When false, every command prompts (audit mode) regardless of whitelist.
    #[serde(default = "default_auto_commands")]
    pub auto_commands: bool,
    /// Command patterns that may auto-approve — matched as a word-boundary prefix,
    /// and only when the command also clears the safety guards in
    /// [`command_auto_approved`]. Tight patterns (e.g. "pytest", "npm test").
    #[serde(default)]
    pub command_whitelist: Vec<String>,
}

fn default_auto_commands() -> bool {
    // Missing key in legacy JSON: default to true so existing whitelists stay active.
    // Fresh installs with no file start via Default (which has auto_commands: false).
    true
}

impl Default for PermData {
    fn default() -> Self {
        Self {
            auto_project_writes: false,
            auto_commands: false,
            command_whitelist: Vec::new(),
        }
    }
}

/// System-impacting programs that NEVER auto-approve, regardless of the whitelist —
/// a hard backstop against a mistaken whitelist entry. Shells (arbitrary execution),
/// destructive filesystem ops, system config, network fetch, code-eval. Dev tools
/// (npm, cargo, pip, py, node, pytest, git, tsc, …) are intentionally NOT here — the
/// tight whitelist is what controls those. Matched on the command's first token.
const BLACKLIST: &[&str] = &[
    "powershell", "pwsh", "cmd", "wsl", "bash", "sh", "zsh",
    "rm", "del", "erase", "rmdir", "rd", "format", "mkfs", "dd", "diskpart",
    "remove-item",
    "reg", "regedit", "netsh", "net", "sc", "schtasks", "wmic", "shutdown",
    "taskkill", "kill", "curl", "wget", "iex", "sudo", "runas",
    "invoke-expression", "invoke-webrequest", "invoke-restmethod", "iwr", "irm",
    "start-process",
];

/// A JSON-file-backed permission policy, guarded by a mutex.
pub struct PermissionStore {
    path: PathBuf,
    data: Mutex<PermData>,
}

impl PermissionStore {
    /// Open (or start) the policy at `path`. A missing/unreadable file starts fully
    /// gated (safe default). Legacy whitelist entries that are now intrinsically
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
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    /// The current policy (for the settings UI).
    pub fn get(&self) -> PermData {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the policy (from the settings UI) and persist it.
    pub fn set(&self, next: PermData) {
        {
            let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
            *d = next.clone();
        }
        self.persist(&next);
    }

    /// Whether project file writes/edits may run without a prompt.
    pub fn auto_project_writes(&self) -> bool {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_project_writes
    }

    /// Whether whitelisted commands may auto-approve (master toggle).
    #[allow(dead_code)]
    pub fn auto_commands(&self) -> bool {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_commands
    }

    /// Whether this exact command may run without a prompt.
    pub fn command_auto_approved(&self, command: &str) -> bool {
        let (auto_cmds, whitelist) = {
            let d = self.data.lock().unwrap_or_else(|e| e.into_inner());
            (d.auto_commands, d.command_whitelist.clone())
        };
        let active_wl: &[String] = if auto_cmds { &whitelist } else { &[] };
        command_auto_approved(command, active_wl)
    }

    /// Auto-approve an `argv`-form command (no shell; see [`argv_passes_guards`]).
    pub fn argv_auto_approved(&self, display: &str, argv: &[String]) -> bool {
        let (auto_cmds, whitelist) = {
            let d = self.data.lock().unwrap_or_else(|e| e.into_inner());
            (d.auto_commands, d.command_whitelist.clone())
        };
        let active_wl: &[String] = if auto_cmds { &whitelist } else { &[] };
        argv_auto_approved(display, argv, active_wl)
    }

    /// The patterns to persist so `command` would auto-approve next time — the leading
    /// token of every segment (shell form) or the program token (argv form). Empty
    /// when the command can never auto-approve, so the UI can show a disabled button.
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

/// Split a shell command into its top-level segments (on `&&` and `;`), honouring
/// quotes, so a compound command can be judged segment by segment (a whitelisted
/// `git add -A && git commit -m "x"` should auto-approve, while `git status && rm -rf .`
/// must not).
///
/// Returns `None` — a hard refusal, never auto-approvable — for any construct we do
/// not try to reason about: a pipe or redirection, a PowerShell
/// subexpression/escape (`$(`, `${`, `(`, `)`, `{`, `}`, backtick), a bare background `&`, a
/// newline, or an unterminated quote. Quotes are honoured so a `&&`/`;`/metachar
/// INSIDE them stays literal data, and are stripped from the returned segments —
/// but `$` and backtick are still refused inside double quotes, where PowerShell
/// expands them. Empty segments (a stray or trailing separator) are dropped; an
/// all-empty result is `None`.
fn shell_segments(command: &str) -> Option<Vec<String>> {
    #[derive(PartialEq)]
    enum Q {
        None,
        Single,
        Double,
    }
    let mut q = Q::None;
    let mut cur = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match q {
            Q::None => match c {
                '\'' => q = Q::Single,
                '"' => q = Q::Double,
                '&' => {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        out.push(std::mem::take(&mut cur));
                    } else {
                        return None; // lone `&` — backgrounding
                    }
                }
                ';' => out.push(std::mem::take(&mut cur)),
                '|' | '>' | '<' | '(' | ')' | '{' | '}' | '`' => return None,
                '$' => {
                    // `$(...)`/`${...}` are evaluated by PowerShell — refuse.
                    if matches!(chars.peek(), Some('(') | Some('{')) {
                        return None;
                    }
                    cur.push(c);
                }
                '\n' | '\r' => return None,
                _ => cur.push(c),
            },
            Q::Single => {
                if c == '\'' {
                    q = Q::None;
                } else {
                    cur.push(c);
                }
            }
            Q::Double => {
                if c == '"' {
                    q = Q::None;
                } else if c == '$' || c == '`' {
                    return None; // still expanded inside double quotes
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if q != Q::None {
        return None; // unterminated quote
    }
    out.push(cur);
    let segs: Vec<String> = out
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if segs.is_empty() {
        None
    } else {
        Some(segs)
    }
}

/// Guard one already-split shell segment: not empty, no path escape or absolute
/// path, and not a blacklisted program. The structural metacharacters were already
/// vetted by [`shell_segments`], so they are not re-scanned here.
fn segment_passes_guards(seg: &str) -> bool {
    let cmd = seg.trim();
    if cmd.is_empty() {
        return false;
    }
    if cmd.contains("..") || has_absolute_path(cmd) {
        return false;
    }
    let first = cmd.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let first = strip_program_extension(&first);
    // Refuse PowerShell variables, environment variables, or assignments ($env:VAR, $x = 1, etc.)
    if first.starts_with('$') || BLACKLIST.contains(&first) {
        return false;
    }
    // Block inline code evaluation on script interpreters (-c, -e, --eval, -p, --print)
    if INTERPRETERS.contains(&first) && has_eval_flag(cmd.split_whitespace().skip(1)) {
        return false;
    }
    // Block git `-c` and `--config-env` which can smuggle arbitrary shell commands via aliases/hooks
    if first == "git"
        && cmd
            .split_whitespace()
            .any(|tok| tok == "-c" || tok.starts_with("-c") || tok == "--config-env" || tok.starts_with("--config-env"))
    {
        return false;
    }
    true
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

/// True if a sequence of tokens (from a whitespace-split shell segment or `argv`)
/// is a strictly read-only `git` inspection command (e.g. `git status`, `git log -n 5`,
/// `git diff`, `git show`, `git blame`, `git rev-parse`, `git branch --show-current`).
///
/// Caller MUST still enforce [`segment_passes_guards`] / [`argv_passes_guards`] so path
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

/// The security core for the shell form (pure, so it's easy to reason about). A
/// command auto-approves only if every segment clears the guards AND either is an
/// intrinsically safe read-only `git` inspection command or matches a whitelisted
/// prefix — so `git add -A && git commit -m "x"` can auto-approve under a
/// `git add` + `git commit` pattern, while `git status && rm -rf .` cannot (`rm` is blacklisted).
pub fn command_auto_approved(command: &str, whitelist: &[String]) -> bool {
    match shell_segments(command) {
        Some(segs) => segs.iter().all(|s| {
            segment_passes_guards(s)
                && (is_readonly_git_tokens(s.split_whitespace())
                    || whitelist.iter().any(|p| pattern_matches(s, p)))
        }),
        None => false,
    }
}

/// The leading tokens to add to the whitelist so THIS command's every segment would
/// auto-approve next time. Segments that are already intrinsically auto-approved
/// (read-only `git` inspection) are skipped so compound commands like
/// `git status && cargo test` only remember `cargo test`. Empty when the command can
/// never auto-approve (it fails a guard) or needs no new patterns.
pub fn command_remember_tokens(command: &str) -> Vec<String> {
    let Some(segs) = shell_segments(command) else {
        return Vec::new();
    };
    if !segs.iter().all(|s| segment_passes_guards(s)) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for s in &segs {
        if is_readonly_git_tokens(s.split_whitespace()) {
            continue;
        }
        let pattern = extract_tight_pattern(s);
        if !pattern.is_empty() && !out.contains(&pattern) {
            out.push(pattern);
        }
    }
    out
}

/// Guards for the `argv` form of `run_command`. There is NO shell here — the program
/// runs directly with an argument vector, so metacharacters in an argument are inert
/// data, not a smuggling vector. Only the two guards that still mean something apply:
/// the program must not be blacklisted, and no argument may escape the project via an
/// absolute or parent-relative path. (This is why an argv command like
/// `git commit -m "fix: a & b"` — quoted for display and thus full of metacharacters —
/// can still auto-approve, where the same string in shell form could not.)
pub fn argv_passes_guards(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    let prog = program.trim().to_ascii_lowercase();
    let prog = strip_program_extension(&prog);
    if prog.is_empty() || prog.starts_with('$') || BLACKLIST.contains(&prog) {
        return false;
    }
    // Block inline code evaluation on script interpreters (-c, -e, --eval, -p, --print)
    if INTERPRETERS.contains(&prog) && has_eval_flag(argv.iter().skip(1).map(|s| s.as_str())) {
        return false;
    }
    if prog == "git"
        && argv
            .iter()
            .any(|a| a == "-c" || a.starts_with("-c") || a == "--config-env" || a.starts_with("--config-env"))
    {
        return false;
    }
    !argv.iter().any(|a| a.contains("..") || has_absolute_path(a))
}

/// Auto-approve an `argv` command: it clears the argv guards and either is an
/// intrinsically safe read-only `git` inspection command or its display string
/// matches a whitelisted prefix.
pub fn argv_auto_approved(display: &str, argv: &[String], whitelist: &[String]) -> bool {
    argv_passes_guards(argv)
        && (is_readonly_git_tokens(argv.iter().map(|s| s.as_str()))
            || whitelist.iter().any(|p| pattern_matches(display, p)))
}

/// The program token to whitelist so this argv command would auto-approve next time.
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

/// True if the command contains a path that could leave the project folder.
///
/// Windows has three spellings of "not relative to here" and only one of them is
/// what people picture, so all three are refused — the same blind spot that made
/// the tool sandbox bypassable (see [`crate::sandbox::contain`]). A bare
/// single-letter `/x` flag is carved out (see [`is_single_letter_flag`]).
fn has_absolute_path(cmd: &str) -> bool {
    // Anywhere in the string: a drive-or-scheme separator (`C:\`, `C:/`, and so
    // `https://` too — a whitelisted dev command has no business fetching a URL).
    let bytes = cmd.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b':'
            && bytes[i - 1].is_ascii_alphabetic()
            && matches!(bytes.get(i + 1), Some(b'\\') | Some(b'/'))
        {
            return true;
        }
    }
    cmd.split_whitespace().any(|t| {
        t.split('=').any(|part| {
            let part = part.trim_start_matches(['"', '\'']);
            // A bare single-letter flag is a Windows tool flag, not a path.
            if is_single_letter_flag(part) {
                return false;
            }
            let b = part.as_bytes();
            // `/x` is unix-absolute; `\x` is Windows root-relative — the spelling
            // `Path::is_absolute()` calls relative, which still lands on the drive
            // root, outside the project.
            matches!(b.first(), Some(b'/') | Some(b'\\'))
                // `C:x` is drive-RELATIVE: it resolves against that drive's own
                // working directory, which is not the project folder either.
                || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        })
    })
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
        // The guards are surfaced through `remember_tokens`: a command that clears
        // them yields the tokens to whitelist ("Always allow"); one that fails yields
        // none, so the button renders disabled.
        let rememberable = |c: &str| !command_remember_tokens(c).is_empty();
        assert!(rememberable("pytest"));
        assert!(rememberable("pytest tests/unit -q"));
        assert!(rememberable("npm test"));
        // …but anything the guards refuse can never auto-approve, however it's
        // remembered, so the UI must not offer to remember it.
        assert!(!rememberable(""));
        assert!(!rememberable("rm -rf ."));             // blacklisted
        assert!(!rememberable("pytest && rm -rf ."));   // chaining
        assert!(!rememberable("pytest ../outside"));    // traversal
        assert!(!rememberable(r"pytest C:\Windows\x")); // absolute path
        assert!(!rememberable("pytest (Get-Date)"));    // powershell subexpression
        assert!(!rememberable("pytest `calc`"));        // backtick subexpression
        // And the split stays consistent with the full check: a guarded command
        // that isn't whitelisted still doesn't auto-approve.
        let empty: Vec<String> = Vec::new();
        assert!(!command_auto_approved("cargo build", &empty));
    }

    #[test]
    fn single_letter_flags_are_not_paths_but_longer_slashes_are() {
        let wl = vec!["dir".to_string(), "findstr".to_string(), "where".to_string()];
        // Windows tool flags — the `/x` form collides with unix-absolute syntax.
        assert!(command_auto_approved("dir /s", &wl));
        assert!(command_auto_approved("dir /b", &wl));
        assert!(command_auto_approved("findstr /s foo *.rs", &wl));
        assert!(command_auto_approved("where /r . python", &wl));
        // …but anything longer than one letter is still an absolute path.
        for cmd in ["dir /sb", "dir /etc/passwd", "findstr /Windows/x foo", "dir /s/"] {
            assert!(!command_auto_approved(cmd, &wl), "should have gated: {cmd}");
        }
    }

    #[test]
    fn argv_form_also_exempts_single_letter_flags() {
        let wl = vec!["dir".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(argv_auto_approved("dir /s", &argv(&["dir", "/s"]), &wl));
        // A longer absolute arg still gates.
        assert!(!argv_auto_approved("dir /etc", &argv(&["dir", "/etc"]), &wl));
    }

    #[test]
    fn compound_commands_auto_approve_when_every_segment_is_whitelisted() {
        let wl = vec!["git".to_string(), "npm test".to_string()];
        // Both segments ride the single `git` pattern.
        assert!(command_auto_approved(r#"git add -A && git commit -m "msg""#, &wl));
        assert!(command_auto_approved("git status; git diff", &wl));
        // Distinct whitelisted segments also pass — every segment must match.
        assert!(command_auto_approved("git add -A && npm test", &wl));
    }

    #[test]
    fn compound_commands_gate_when_any_segment_is_unwhitelisted_or_blacklisted() {
        let wl = vec!["git".to_string()];
        // `npm` isn't whitelisted → the whole compound gates.
        assert!(!command_auto_approved("git add -A && npm test", &wl));
        // A blacklisted segment gates even when whitelisted-looking.
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
        // An unterminated quote is refused rather than guessed at.
        assert!(!command_auto_approved(r#"git commit -m "unterminated"#, &wl));
    }

    #[test]
    fn quoted_metacharacters_stay_literal_in_the_shell_form() {
        let wl = vec!["git".to_string()];
        // The `&` and `;` are inside quotes — PowerShell treats them as data.
        assert!(command_auto_approved(r#"git commit -m "fix: a & b""#, &wl));
        assert!(command_auto_approved(r#"git commit -m "a; b""#, &wl));
        // But a `$` inside double quotes is still expanded → refused.
        assert!(!command_auto_approved(r#"git commit -m "hi $env:PATH""#, &wl));
        // Single quotes are inert in PowerShell.
        assert!(command_auto_approved(r#"git commit -m 'hi $env:PATH'"#, &wl));
    }

    #[test]
    fn argv_form_ignores_shell_metacharacters() {
        let wl = vec!["git".to_string()];
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Spawned directly, no shell — the `&`/quotes are inert data.
        assert!(argv_auto_approved(
            r#"git commit -m "fix: a & b""#,
            &argv(&["git", "commit", "-m", "fix: a & b"]),
            &wl
        ));
        // The shell-form equivalent of a chained string is just one argv program.
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
        // Even whitelisted, a blacklisted program never auto-approves.
        assert!(!argv_auto_approved(
            "rm -rf .",
            &argv(&["rm", "-rf", "."]),
            &wl
        ));
        // Absolute or parent-relative arguments gate.
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
        // The model may spell the same path with either separator.
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
        // `git status` is intrinsically auto-approved, so only `npm test` needs remembering.
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
        // A command that can never auto-approve offers nothing to remember.
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
    fn old_policy_json_loads_and_ignores_retired_fields() {
        // Policy files predate today's schema in both directions: missing fields
        // default to safe values, and a retired `auto_python_snippets` key (from
        // the removed run_python_code tool) is ignored, not a parse error.
        let old = r#"{"auto_project_writes": true, "command_whitelist": ["pytest"], "auto_python_snippets": true}"#;
        let p: PermData = serde_json::from_str(old).unwrap();
        assert!(p.auto_project_writes);
        assert!(p.auto_commands);
        assert_eq!(p.command_whitelist, vec!["pytest".to_string()]);

        // Fresh install without file starts fully gated (all false)
        let fresh = PermData::default();
        assert!(!fresh.auto_project_writes);
        assert!(!fresh.auto_commands);
        assert!(fresh.command_whitelist.is_empty());
    }

    #[test]
    fn master_toggle_disables_all_command_auto_approvals() {
        let tmp = std::env::temp_dir().join(format!(
            "test_perm_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = PermissionStore::open(tmp.clone());
        store.set(PermData {
            auto_project_writes: true,
            auto_commands: false,
            command_whitelist: vec!["pytest".to_string()],
        });
        assert!(!store.auto_commands());
        assert!(!store.command_auto_approved("pytest"));
        assert!(!store.argv_auto_approved("pytest", &["pytest".to_string()]));

        // Enable master toggle
        store.set(PermData {
            auto_project_writes: true,
            auto_commands: true,
            command_whitelist: vec!["pytest".to_string()],
        });
        assert!(store.auto_commands());
        assert!(store.command_auto_approved("pytest"));
        assert!(store.argv_auto_approved("pytest", &["pytest".to_string()]));
        let _ = std::fs::remove_file(tmp);
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
        // Inside quotes, braces are literal data
        assert!(command_auto_approved("git commit -m '{fix}'", &wl));
    }

    #[test]
    fn interpreter_inline_eval_flags_are_refused() {
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
        // py -m <mod>
        assert_eq!(
            command_remember_tokens("py -m pytest tests/"),
            vec!["py -m pytest".to_string()]
        );
        assert_eq!(
            command_remember_tokens("python3 -m unittest discover"),
            vec!["python3 -m unittest".to_string()]
        );
        // python <script>
        assert_eq!(
            command_remember_tokens("python main.py --port 8080"),
            vec!["python main.py".to_string()]
        );
        // node <script>
        assert_eq!(
            command_remember_tokens("node index.js --dev"),
            vec!["node index.js".to_string()]
        );
        // npx <pkg>
        assert_eq!(
            command_remember_tokens("npx vitest run"),
            vec!["npx vitest".to_string()]
        );

        // Bare interpreter commands must NEVER fall back to bare prefix
        assert!(command_remember_tokens("python").is_empty());
        assert!(command_remember_tokens("python.exe").is_empty());
        assert!(command_remember_tokens("py").is_empty());
        assert!(command_remember_tokens("node").is_empty());
        assert!(command_remember_tokens("npx").is_empty());

        // Same for argv form
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

        // Read-only git commands auto-approve even with an empty whitelist.
        for cmd in [
            "git status",
            "git status --short -b",
            "git log -n 5 --stat",
            "git log --oneline -10",
            "git diff",
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

        // Store with auto_commands: false (default fresh install) still allows read-only git inspection.
        let tmp = std::env::temp_dir().join(format!(
            "test_git_ro_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = PermissionStore::open(tmp.clone());
        assert!(!store.auto_commands());
        assert!(store.command_auto_approved("git log -n 5"));
        assert!(store.argv_auto_approved("git status", &argv(&["git", "status"])));
        assert!(!store.command_auto_approved("git commit -m 'msg'"));
        let _ = std::fs::remove_file(tmp);

        // Mutating or unsafe git commands/flags must NOT auto-approve without explicit whitelist.
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
