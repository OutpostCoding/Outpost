// ── surface: backend ──────────────────────────────────
// Project management tools: read, write, edit, search, info, changes, restore.
// Exports: Tools::project_*, Tools::code_search, Tools::code_symbols, Tools::typecheck

use std::path::{Path, PathBuf};
use serde_json::Value;
use tauri::ipc::Channel;
use tokio_util::sync::CancellationToken;
use crate::provider::StreamEvent;
pub use crate::tools::context::number_lines_capped;
use super::*;

impl Tools {
    /// Snapshot a file before mutating it, or produce the refusal to return.
    ///
    /// Every mutating project tool goes through this. "Couldn't back it up" has
    /// to mean "didn't do it": for a folder with no version control,
    /// `project_restore` is the only undo, and it is what makes an unattended
    /// write acceptable in the first place.
    pub(crate) fn guard_snapshot(&self, full: &Path, action: &str) -> Option<String> {
        self.snapshots.capture(full).err().map(|e| {
            format!(
                "error: refusing to {action} — it couldn't be copied aside first ({e}), and \
                 project_restore is the only undo for this folder. Nothing was changed."
            )
        })
    }

    /// Resolve a model-supplied path within the project root, sandboxed
    /// — see [`crate::sandbox::contain`] for why the check is not lexical.
    /// Strips root prefix if an absolute path is inside the project, and trims leading
    /// slashes so paths copied from diagnostics or stack traces resolve seamlessly.
    pub(crate) fn project_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
        let trimmed = rel.trim().trim_matches('"').trim_matches('\'').trim();
        if trimmed.is_empty() || trimmed == "." {
            return Ok(root.to_path_buf());
        }
        let norm_path = if Path::new(trimmed).is_absolute() {
            if let Ok(canon_root) = root.canonicalize() {
                if let Ok(canon_p) = Path::new(trimmed).canonicalize() {
                    if let Ok(stripped) = canon_p.strip_prefix(&canon_root) {
                        stripped.to_string_lossy().to_string()
                    } else {
                        trimmed.to_string()
                    }
                } else {
                    let t_norm = trimmed.replace('\\', "/");
                    let r_norm = canon_root.to_string_lossy().replace('\\', "/");
                    let r_clean = r_norm.trim_start_matches("//?/").trim_start_matches(r"\\?\");
                    let r_clean_trimmed = r_clean.trim_end_matches('/');
                    let r_plain_raw = root.to_string_lossy().replace('\\', "/");
                    let r_plain = r_plain_raw.trim_end_matches('/');

                    let strip_prefix = |text: &str, prefix: &str| -> Option<String> {
                        if !prefix.is_empty()
                            && text.len() >= prefix.len()
                            && text.is_char_boundary(prefix.len())
                            && text[..prefix.len()].eq_ignore_ascii_case(prefix)
                            && (text.len() == prefix.len() || text.as_bytes()[prefix.len()] == b'/')
                        {
                            Some(text[prefix.len()..].trim_start_matches('/').to_string())
                        } else {
                            None
                        }
                    };

                    if let Some(rel) = strip_prefix(&t_norm, r_clean_trimmed) {
                        rel
                    } else if let Some(rel) = strip_prefix(&t_norm, r_plain) {
                        rel
                    } else {
                        trimmed.to_string()
                    }
                }
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        };

        // Strip leading slashes (e.g. "/src/App.tsx" or "\\src\\App.tsx")
        let clean = norm_path.trim_start_matches(['/', '\\']);
        if clean.is_empty() || clean == "." {
            return Ok(root.to_path_buf());
        }
        crate::sandbox::contain(root, clean)
            .map_err(|why| format!("invalid path '{}' — {why}", trimmed))
    }

    /// `godot_scene_tree` — read a Godot 4 .tscn scene file and return its node
    /// tree with resolved resource paths. Read-only and sandboxed like the other
    /// project_read-style tools; the parser itself is pure (`tscn.rs`), so it
    /// works against hand-written fixtures and real exported scenes alike.
    pub(crate) fn godot_scene_tree(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — godot_scene_tree needs an active coding session."
                .into();
        };
        let args = match crate::tscn::parse_godot_args(arguments) {
            Ok(a) => a,
            Err(e) => return format!("error: {e}"),
        };
        let full = match Self::project_path(root, &args.scene) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        if !full.is_file() {
            return format!("error: '{}' is not a file in the project.", args.scene);
        }
        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return format!("error: could not read '{}': {e}", args.scene),
        };
        match crate::tscn::render_scene(&text, &args) {
            Ok(out) => out,
            Err(e) => {
                format!(
                    "error: {e} — '{}' is not a well-formed Godot 4 scene?",
                    args.scene
                )
            }
        }
    }

    pub(crate) fn is_noise_dir(name: &str) -> bool {
        matches!(
            name,
            ".git"
                | ".godot"
                | "__pycache__"
                | ".venv"
                | "venv"
                | "env"
                | "node_modules"
                | ".mypy_cache"
                | ".pytest_cache"
                | ".tox"
                | ".fastembed_cache"
                | "target"
                | "dist"
                | "build"
                | ".next"
                | ".idea"
                | ".vscode"
                | "archive"
        )
    }

    /// Depth-first list of files under `dir`, as POSIX paths relative to `root`,
    /// skipping noise dirs and the `archive/` docs folder (history is read
    /// deliberately via `project_read`, never found by search), bounded by `cap`.
    pub(crate) fn walk_project(dir: &Path, root: &Path, out: &mut Vec<String>, cap: usize) {
        if out.len() >= cap {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            if out.len() >= cap {
                return;
            }
            // Skip symlinks and junctions to prevent escaping the project root or recursive cycles
            if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if Self::is_noise_dir(name) {
                    continue;
                }
                Self::walk_project(&p, root, out, cap);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// Breadth-first list of files and directories under `base`, as POSIX paths relative to `root`,
    /// skipping noise dirs and junctions/symlinks, bounded by `cap`.
    /// Subdirectories are formatted with a trailing `/`.
    /// When `max_depth` is `Some(d)`, only descends `d` levels deep (e.g. `d = 1` for immediate children).
    pub(crate) fn walk_project_bfs(
        base: &Path,
        root: &Path,
        max_depth: Option<usize>,
        cap: usize,
    ) -> (Vec<String>, bool) {
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((base.to_path_buf(), 0usize));
        let mut capped = false;

        while let Some((dir, current_depth)) = queue.pop_front() {
            if out.len() >= cap {
                capped = true;
                break;
            }

            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.path());

            for e in entries {
                if out.len() >= cap {
                    capped = true;
                    break;
                }
                if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if Self::is_noise_dir(name) {
                        continue;
                    }
                    if let Ok(rel) = p.strip_prefix(root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        out.push(format!("{rel_str}/"));
                    }
                    let next_depth = current_depth + 1;
                    let should_descend = match max_depth {
                        Some(md) => next_depth < md,
                        None => true,
                    };
                    if should_descend {
                        queue.push_back((p, next_depth));
                    }
                } else if p.is_file() {
                    if let Ok(rel) = p.strip_prefix(root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        out.push(rel_str);
                    }
                }
            }
        }

        out.sort();
        (out, capped)
    }

    /// `project_list` / `list_dir` — breadth-first listing of files and directories in the project.
    pub(crate) fn project_list(&self, arguments: &str, project_root: Option<&Path>) -> String {
        Self::run_project_list(arguments, project_root)
    }

    pub(crate) fn run_project_list(arguments: &str, project_root: Option<&Path>) -> String {
        const CAP: usize = 500;
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let subdir = parsed
            .as_ref()
            .and_then(|v| arg_str(v, &["path", "dir", "directory", "subdir", "file_path", "filepath"]))
            .filter(|s| !s.trim().is_empty());
        let depth = parsed
            .as_ref()
            .and_then(|v| arg_u64(v, &["depth", "max_depth"]))
            .map(|d| d.max(1));

        let base = match &subdir {
            Some(s) => match Self::project_path(root, s) {
                Ok(p) => p,
                Err(e) => return format!("error: {e}"),
            },
            None => root.to_path_buf(),
        };
        if !base.is_dir() {
            return format!(
                "error: '{}' is not a directory in the project.",
                subdir.unwrap_or_default()
            );
        }
        let (out, capped) = Self::walk_project_bfs(&base, root, depth, CAP);
        if out.is_empty() {
            return "(empty directory)".into();
        }
        let mut s = out.join("\n");
        if capped {
            s.push_str(&format!(
                "\n… (truncated at {CAP} entries; pass 'path' to list a specific directory e.g. list_dir(path: \"src\"), or pass 'depth' to limit depth)"
            ));
        }
        s
    }

    /// `project_read` — read one project file (sandboxed, truncated like read_note).
    pub(crate) fn project_read(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        Self::run_project_read(arguments, root)
    }

    pub(crate) fn run_project_read(arguments: &str, root: &Path) -> String {
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        if let Some(ref v) = parsed {
            if parse_line_num(
                v,
                &[
                    "start",
                    "start_line",
                    "from",
                    "line_start",
                    "offset",
                    "lines",
                    "limit",
                    "count",
                    "num_lines",
                    "max_lines",
                    "length",
                    "end",
                    "end_line",
                    "to",
                    "line_end",
                ],
            )
            .is_some()
            {
                return Self::run_project_read_lines(arguments, root);
            }
        }
        let path = parsed
            .as_ref()
            .and_then(|v| arg_str(v, PATH_ALIASES).map(str::to_string));
        let Some(path) = path else {
            return "error: project_read requires a 'path' string argument.".into();
        };
        let full = match Self::project_path(root, &path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        let max_chars = parse_max_chars(arguments, MAX_NOTE_CHARS);
        match read_text_capped(&full) {
            Ok(content) if content.trim().is_empty() => format!("'{path}' is empty."),
            Ok(content) => {
                // Number lines (1-based) so the model can reference line positions.
                // Truncate the numbered form at the requested cap.
                let (numbered, truncated) = number_lines_capped(&content, max_chars);
                if truncated {
                    let total_lines = content.lines().count();
                    let total_chars = content.chars().count();
                    let shown_lines = numbered.lines().count();
                    let approx_numbered_chars = total_chars.saturating_add(total_lines.saturating_mul(5));

                    if approx_numbered_chars <= MAX_CHARS_CEILING {
                        let suggest_cap = ((approx_numbered_chars + 999) / 1000) * 1000;
                        format!(
                            "{numbered}\n\n[content truncated at {max_chars} chars (showing lines 1–{shown_lines} of {total_lines}; {total_chars} total chars) — pass max_chars: {suggest_cap} to read the full file, or pass start and lines to read_file for specific ranges]"
                        )
                    } else {
                        format!(
                            "{numbered}\n\n[content truncated at {max_chars} chars (showing lines 1–{shown_lines} of {total_lines}; {total_chars} total chars) — file exceeds the {MAX_CHARS_CEILING} char ceiling. Pass start and lines to read_file to read targeted ranges or use find_symbols for an outline.]"
                        )
                    }
                } else {
                    numbered
                }
            }
            Err(e) => format!("error reading '{path}': {e}"),
        }
    }

    /// `project_read_lines` — read an inclusive 1-based line range of a project file.
    pub(crate) fn project_read_lines(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        Self::run_project_read_lines(arguments, root)
    }

    pub(crate) fn run_project_read_lines(arguments: &str, root: &Path) -> String {
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed
            .as_ref()
            .and_then(|v| arg_str(v, PATH_ALIASES).map(str::to_string));
        let Some(path) = path else {
            return "error: project_read_lines requires a 'path' string argument.".into();
        };
        let start = parsed
            .as_ref()
            .and_then(|v| parse_line_num(v, &["start", "start_line", "from", "line_start", "offset"]))
            .unwrap_or(1)
            .max(1);

        let lines_arg = parsed
            .as_ref()
            .and_then(|v| parse_line_num(v, &["lines", "limit", "count", "num_lines", "max_lines", "length"]));

        let end_arg = parsed
            .as_ref()
            .and_then(|v| parse_line_num(v, &["end", "end_line", "to", "line_end"]));

        // Resolve effective end line:
        // 1. Explicit lines/limit argument takes priority (chunk mode: start + lines - 1).
        // 2. Legacy end argument:
        //    - If end >= start: absolute end line (start..=end).
        //    - If end < start: auto-heal! Model passed a relative line count as 'end' (e.g. start: 145, end: 30 -> 174).
        let effective_end: Option<usize> = if let Some(n) = lines_arg {
            Some(start.saturating_add(n.max(1)).saturating_sub(1))
        } else if let Some(e) = end_arg {
            if e < start {
                Some(start.saturating_add(e.max(1)).saturating_sub(1))
            } else {
                Some(e)
            }
        } else {
            None
        };

        let max_chars = parse_max_chars(arguments, MAX_NOTE_CHARS);

        let full = match Self::project_path(root, &path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };

        let meta = match std::fs::metadata(&full) {
            Ok(m) => m,
            Err(e) => return format!("error reading '{path}': {e}"),
        };
        if meta.is_dir() {
            return format!("error: '{path}' is a directory — use list_dir to inspect folders.");
        }

        const MAX_DIRECT_BYTES: u64 = 8 * 1024 * 1024;
        if meta.len() <= MAX_DIRECT_BYTES {
            let content = match read_text_capped(&full) {
                Ok(c) => c,
                Err(e) => return format!("error reading '{path}': {e}"),
            };
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            if total == 0 {
                return format!("'{path}' is empty.");
            }
            if start > total {
                return format!("'{path}' has only {total} lines; start line {start} is past the end.");
            }
            let end = effective_end.unwrap_or(total).clamp(start, total);
            let mut numbered = String::new();
            let mut char_count = 0;
            let mut truncated = false;
            use std::fmt::Write;
            for (i, l) in lines[start - 1..end].iter().enumerate() {
                let cur_num = start + i;
                let before_len = numbered.len();
                if !numbered.is_empty() {
                    numbered.push('\n');
                    char_count += 1;
                }
                let _ = write!(numbered, "{cur_num}\t{l}");
                char_count += numbered[before_len + if before_len > 0 { 1 } else { 0 }..].chars().count();
                if char_count > max_chars {
                    let mut c = 0;
                    let mut b_idx = numbered.len();
                    for (b, _) in numbered.char_indices() {
                        if c == max_chars {
                            b_idx = b;
                            break;
                        }
                        c += 1;
                    }
                    numbered.truncate(b_idx);
                    truncated = true;
                    break;
                }
            }

            if truncated {
                let last = numbered.rfind('\n').map(|i| &numbered[..i]).unwrap_or(&numbered);
                return format!(
                    "'{path}' lines {start}\u{2013}{end} (of {total}):\n{last}\n\n[truncated at \
                     {max_chars} characters — pass max_chars to raise the cap (max {MAX_CHARS_CEILING}), or ask for a narrower line range]"
                );
            }
            return format!("'{path}' lines {start}\u{2013}{end} (of {total}):\n{numbered}");
        }

        // Streaming path for large files > 8 MB (e.g. database dumps, server logs, big data files).
        use std::io::{BufRead, BufReader};
        let file = match std::fs::File::open(&full) {
            Ok(f) => f,
            Err(e) => return format!("error reading '{path}': {e}"),
        };
        let reader = BufReader::new(file);
        let mut numbered = String::new();
        let mut char_count = 0;
        let mut current_line = 0usize;
        let mut truncated = false;
        let mut last_read_line = start;

        use std::fmt::Write;
        for line_res in reader.lines() {
            let mut line = match line_res {
                Ok(l) => l,
                Err(e) => return format!("error reading '{path}': {e}"),
            };
            current_line += 1;
            if current_line == 1 && line.starts_with('\u{feff}') {
                line.remove(0);
            }
            if current_line < start {
                continue;
            }
            if let Some(end) = effective_end {
                if current_line > end {
                    break;
                }
            }
            let before_len = numbered.len();
            if !numbered.is_empty() {
                numbered.push('\n');
                char_count += 1;
            }
            let _ = write!(numbered, "{current_line}\t{line}");
            last_read_line = current_line;
            char_count += numbered[before_len + if before_len > 0 { 1 } else { 0 }..].chars().count();
            if char_count > max_chars {
                let mut c = 0;
                let mut b_idx = numbered.len();
                for (b, _) in numbered.char_indices() {
                    if c == max_chars {
                        b_idx = b;
                        break;
                    }
                    c += 1;
                }
                numbered.truncate(b_idx);
                truncated = true;
                break;
            }
        }

        if current_line == 0 {
            return format!("'{path}' is empty.");
        }
        if start > current_line {
            return format!(
                "'{path}' has only {current_line} lines; start line {start} is past the end."
            );
        }

        let actual_end = if let Some(end) = end_arg {
            end.min(last_read_line)
        } else {
            last_read_line
        };

        if truncated {
            let last = numbered.rfind('\n').map(|i| &numbered[..i]).unwrap_or(&numbered);
            return format!(
                "'{path}' lines {start}\u{2013}{actual_end} (large file, {:.1} MB):\n{last}\n\n[truncated at \
                 {max_chars} characters — pass max_chars to raise the cap (max {MAX_CHARS_CEILING}), or ask for a narrower line range]",
                meta.len() as f64 / 1_048_576.0
            );
        }

        format!(
            "'{path}' lines {start}\u{2013}{actual_end} (large file, {:.1} MB):\n{numbered}",
            meta.len() as f64 / 1_048_576.0
        )
    }

    /// `project_read_image` — hand an image file in the project to the model as an
    /// actual picture. The tool RESULT is only a text receipt: the image itself is
    /// queued here and delivered by the agent loop as a synthetic user turn, because
    /// tool results are text-only on every provider (see agent.rs / to_anthropic).
    pub(crate) fn project_read_image(&self, arguments: &str, project_root: Option<&Path>) -> String {
        use base64::Engine as _;

        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        if let Some(msg) = empty_args_error(arguments, "project_read_image", "'path'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let Some(path) = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES)) else {
            return "error: project_read_image requires 'path' — the relative path of an image file in the project.".into();
        };
        let Some(media_type) = image_media_type(path) else {
            return format!(
                "error: '{path}' isn't a supported image — use .png, .jpg/.jpeg, .gif or .webp."
            );
        };
        let full = match Self::project_path(root, path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(_) => return format!("error: '{path}' doesn't exist or can't be read."),
        };
        if bytes.is_empty() {
            return format!("error: '{path}' is empty.");
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return format!(
                "error: '{path}' is {:.1} MB, over the {:.1} MB limit for a single image. Save a downscaled copy and read that instead.",
                bytes.len() as f64 / 1_000_000.0,
                MAX_IMAGE_BYTES as f64 / 1_000_000.0
            );
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        self.queue_image(format!("data:{media_type};base64,{encoded}"));
        format!(
            "Loaded '{path}' ({media_type}, {:.0} KB) — image attached for inspection this turn.",
            bytes.len() as f64 / 1000.0
        )
    }

    /// `browser_screenshot` — capture the perception window and hand the model the
    /// picture, via the same queue_image path as project_read_image. The COM work
    /// lives in crate::capture; everything here is packaging.
    #[cfg(windows)]

    /// `project_search` — search project file contents (or names, with a `name:`
    /// query prefix) and get back hits grouped by file, most hits first.
    pub(crate) fn project_search(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        Self::run_project_search(arguments, root)
    }

    /// `code_search` — hybrid relevance search (BM25 + bge-small vector, fused
    /// with RRF) over the current project's code and docs. The heavy work lives in
    /// `code_search::search` (pure, testable); this method only supplies the root
    /// and runs it on a blocking thread — the synchronous ONNX inference (bge-small)
    /// and a first-call index build over thousands of chunks must not sit on the
    /// async/UI thread (same reasoning as `extract_document`). The cancel token is
    /// CLONED into the blocking closure: dropping the JoinHandle below only
    /// detaches that thread, so the token is how a Stop actually halts the CPU burn.
    pub(crate) async fn code_search(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        cancel: &CancellationToken,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        let args = arguments.to_string();
        let root = root.to_path_buf();
        // The persisted index lives in the per-user app-data dir (same place as the
        // stores), keyed by repo root. `None` (unresolvable dir) degrades to the
        // in-process cache only — search still works, it just re-embeds on restart.
        let cache_dir = self.app.path().app_data_dir().ok();
        let cancel = cancel.clone();
        tauri::async_runtime::spawn_blocking(move || {
            code_search::search(&args, &root, cache_dir.as_deref(), &cancel)
        })
        .await
        .unwrap_or_else(|e| format!("error: search stopped unexpectedly: {e}"))
    }

    /// Classifies a project file for the search-result tags: `[src]` live code,
    /// `[doc]` prose, `[test]` tests, `[cfg]` config/markup/cosmetic, `[other]`
    /// everything else. Pure path heuristic, so it reports CATEGORY only — it
    /// never pretends to know a file's purpose (that's what reads and searches
    /// are for). Shared with `project_info`'s `{map: true}` computed map (design D)
    /// so one tag means the same thing everywhere it appears.
    pub(crate) fn file_kind(rel: &str) -> &'static str {
        let segs: Vec<&str> = rel.split(['/', '\\']).collect();
        let file_name = segs.last().copied().unwrap_or("").to_lowercase();
        let in_test_dir = segs.iter().any(|s| {
            let s = s.to_lowercase();
            s == "tests" || s == "test" || s == "spec" || s == "__tests__"
        });
        let test_named = file_name.ends_with("_test.rs")
            || file_name.starts_with("test_")
            || file_name.contains(".test.")
            || file_name.contains(".spec.");
        if in_test_dir || test_named {
            return "[test]";
        }
        match rel.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
            "md" | "txt" | "rst" | "adoc" | "license" => "[doc]",
            "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "kt" | "c" | "cc"
            | "cpp" | "h" | "hpp" | "cs" | "rb" | "php" | "swift" | "scala" | "sh" | "bat"
            | "ps1" | "sql" | "zig" | "hs" | "lua" | "r" | "dart" | "vue" | "svelte" => "[src]",
            "json" | "toml" | "yaml" | "yml" | "lock" | "env" | "ini" | "cfg" | "conf"
            | "gitignore" | "gitattributes" | "editorconfig" | "xml" | "css" | "scss" | "less"
            | "html" | "htm" => "[cfg]",
            _ => "[other]",
        }
    }

    /// Expands shell/glob brace alternatives like `*.{ts,tsx}` or `{Cargo.toml,tauri.conf.json}`
    /// into individual glob strings so `glob::Pattern` (which does not natively support `{a,b}`)
    /// matches correctly. Handles nested and multiple brace groups recursively.
    pub(crate) fn expand_braces(pattern: &str) -> Vec<String> {
        let mut depth = 0;
        let mut start_idx = None;
        let mut has_comma_at_depth_1 = false;
        let mut end_idx = None;

        for (i, c) in pattern.char_indices() {
            match c {
                '{' => {
                    if depth == 0 {
                        start_idx = Some(i);
                        has_comma_at_depth_1 = false;
                    }
                    depth += 1;
                }
                '}' => {
                    if depth > 0 {
                        depth -= 1;
                        if depth == 0 && start_idx.is_some() {
                            if has_comma_at_depth_1 {
                                end_idx = Some(i);
                                break;
                            } else {
                                start_idx = None;
                            }
                        }
                    }
                }
                ',' => {
                    if depth == 1 {
                        has_comma_at_depth_1 = true;
                    }
                }
                _ => {}
            }
        }

        if let (Some(start), Some(end)) = (start_idx, end_idx) {
            let prefix = &pattern[..start];
            let suffix = &pattern[end + 1..];
            let inner = &pattern[start + 1..end];

            let mut parts = Vec::new();
            let mut cur_part = String::new();
            let mut inner_depth = 0;
            for c in inner.chars() {
                if c == '{' {
                    inner_depth += 1;
                    cur_part.push(c);
                } else if c == '}' {
                    if inner_depth > 0 {
                        inner_depth -= 1;
                    }
                    cur_part.push(c);
                } else if c == ',' && inner_depth == 0 {
                    parts.push(cur_part);
                    cur_part = String::new();
                } else {
                    cur_part.push(c);
                }
            }
            parts.push(cur_part);

            return parts
                .into_iter()
                .flat_map(|part| Self::expand_braces(&format!("{prefix}{part}{suffix}")))
                .collect();
        }

        vec![pattern.to_string()]
    }

    /// Pure core of `project_search` — an associated fn (no host state) so the
    /// search logic is unit-testable without a Tauri AppHandle.
    pub(crate) fn run_project_search(arguments: &str, root: &Path) -> String {
        const FILE_CAP: usize = 2000; // files walked
        const HIT_CAP: usize = 80; // total output lines (headers + snippets)
        const PER_FILE: usize = 6; // hits kept per file
        const FILE_SHOW_CAP: usize = 12; // file groups shown before truncation
                                         // Design C is adaptive: context lines only when the result set is narrow.
                                         // Broad searches stay single-line (cheap breadth); a handful of files gets
                                         // ±1 context so the model can tell a hit from its neighbours. Narrow is
                                         // bounded by construction — 4 files × 6 hits × 3 lines + 4 headers = 76 <
                                         // HIT_CAP — so truncation never fires in context mode.
        const CONTEXT_GROUP_CAP: usize = 4;
        const SEARCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
        let start_time = std::time::Instant::now();
        let mut timed_out = false;
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let query_raw = parsed
            .as_ref()
            .and_then(|v| v["query"].as_str())
            .unwrap_or("")
            .trim();
        let path_filter = parsed
            .as_ref()
            .and_then(|v| v["path"].as_str())
            .map(str::to_string)
            .filter(|p| !p.trim().is_empty());
        let glob = parsed
            .as_ref()
            .and_then(|v| v["glob"].as_str())
            .map(str::to_string)
            .filter(|g| !g.trim().is_empty());
        let show_all = parsed
            .as_ref()
            .and_then(|v| v["all"].as_bool())
            .unwrap_or(false);
        let max_per_file_arg = parsed
            .as_ref()
            .and_then(|v| v["max_per_file"].as_u64())
            .map(|n| n as usize);

        if query_raw.is_empty() && path_filter.is_none() && glob.is_none() {
            return "error: project_search requires a 'query' string (or a 'path'/'glob' to list matching files).".into();
        }
        // Case-insensitive by default; exact-case on request — the case that needs
        // it is a query that is a substring of other identifiers.
        let case_sensitive = parsed
            .as_ref()
            .and_then(|v| v["case_sensitive"].as_bool())
            .unwrap_or(false);
        let use_regex = parsed
            .as_ref()
            .and_then(|v| v["regex"].as_bool())
            .unwrap_or(false);
        // `name:` (and aliases `path:`, `file:`, `filename:`) prefix switches to
        // file-NAME search: the term matches relative paths instead of contents.
        // Omitted query with path or glob also operates in filename/listing mode.
        //
        // Escaping / code syntax protection:
        // 1. A leading backslash (e.g. `\name:settings`) escapes prefix parsing for literal search.
        // 2. Prefixes followed by whitespace (e.g. `name: foo` in YAML or `path: &Path` in Rust) or
        //    with an empty suffix (`name:`) are treated as literal content searches, not filename searches.
        let is_escaped = query_raw.starts_with('\\');
        let unescaped_query = if is_escaped {
            &query_raw[1..]
        } else {
            query_raw
        };
        let name_prefix = if is_escaped {
            None
        } else {
            ["name:", "path:", "file:", "filename:"]
                .iter()
                .find(|&&p| {
                    if !query_raw.starts_with(p) {
                        return false;
                    }
                    let rest = &query_raw[p.len()..];
                    !rest.trim().is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t')
                })
        };
        let name_mode = unescaped_query.is_empty() || name_prefix.is_some();
        let term = if let Some(prefix) = name_prefix {
            query_raw[prefix.len()..].trim().to_string()
        } else {
            unescaped_query.to_string()
        };
        // Compiled matcher used for both content and filename searches.
        // If regex is false, term is escaped so it executes as a literal substring match,
        // UNLESS it contains obvious regex syntax (e.g. pipe alternations like `foo|bar`
        // or patterns like `.*`), in which case we auto-detect and attempt regex compilation
        // so models using grep patterns don't fail with 0 matches.
        let auto_regex = !use_regex && (term.contains('|') || term.contains(".*") || term.contains(".+"));
        let effective_regex = use_regex || auto_regex;
        let matcher = if term.is_empty() {
            regex::Regex::new(".*").unwrap()
        } else if effective_regex {
            match regex::RegexBuilder::new(&term)
                .case_insensitive(!case_sensitive)
                .multi_line(true)
                .build()
            {
                Ok(r) => r,
                Err(_) if auto_regex => {
                    // Auto-detected regex failed to compile cleanly (e.g. unmatched parens).
                    // Fall back safely to literal match rather than erroring out!
                    match regex::RegexBuilder::new(&regex::escape(&term))
                        .case_insensitive(!case_sensitive)
                        .multi_line(true)
                        .build()
                    {
                        Ok(r) => r,
                        Err(e) => return format!("error: invalid pattern '{term}': {e}"),
                    }
                }
                Err(e) => return format!("error: invalid regex '{term}': {e}"),
            }
        } else {
            match regex::RegexBuilder::new(&regex::escape(&term))
                .case_insensitive(!case_sensitive)
                .multi_line(true)
                .build()
            {
                Ok(r) => r,
                Err(e) => return format!("error: invalid pattern '{term}': {e}"),
            }
        };

        // Normalize model-supplied path using project_path so absolute paths, root prefixes,
        // Windows backslashes, and leading/trailing slashes resolve seamlessly to relative paths.
        let path_norm = path_filter.as_deref().and_then(|p| {
            let trimmed = p.trim();
            if trimmed.is_empty() || trimmed == "." {
                return None;
            }
            match Self::project_path(root, trimmed) {
                Ok(resolved) => {
                    if let Ok(rel) = resolved.strip_prefix(root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/").trim_matches('/').to_string();
                        if rel_str.is_empty() || rel_str == "." {
                            None
                        } else {
                            Some(rel_str)
                        }
                    } else {
                        Some(trimmed.replace('\\', "/").trim_matches('/').to_string())
                    }
                }
                Err(_) => Some(trimmed.replace('\\', "/").trim_matches('/').to_string()),
            }
        });
        let glob_norm = glob.as_deref().map(|g| g.replace('\\', "/"));
        let glob_pats: Vec<(glob::Pattern, String)> = match glob_norm.as_deref() {
            Some(g) => {
                let expanded = Self::expand_braces(g);
                let mut pats = Vec::with_capacity(expanded.len());
                for pat_str in expanded {
                    match glob::Pattern::new(&pat_str) {
                        Ok(p) => pats.push((p, pat_str)),
                        Err(e) => return format!("error: invalid glob '{g}': {e}"),
                    }
                }
                pats
            }
            None => Vec::new(),
        };

        // Adaptive caps:
        // Default HIT_CAP is 80 lines; when show_all is true, expand to 200 lines.
        let hit_cap = if show_all { 200 } else { HIT_CAP };
        // Default PER_FILE is 6. If max_per_file is specified, use it.
        // If show_all is true or path points to a single file (contains '.'), expand per_file_limit.
        let single_file_target = path_norm.as_ref().map_or(false, |p| p.contains('.'));
        let per_file_limit = if let Some(m) = max_per_file_arg {
            m.max(1)
        } else if show_all || single_file_target {
            hit_cap
        } else {
            PER_FILE
        };

        let mut files: Vec<String> = Vec::new();
        Self::walk_project(root, root, &mut files, FILE_CAP);
        // A kept hit plus its neighbours, captured during the scan. Whether the
        // neighbours are rendered is decided later (narrow vs broad) from the total
        // group count, so the scan stays a single pass with no file re-read.
        struct Hit {
            no: usize,            // 1-based line number of the hit
            text: String,         // trimmed + capped hit line
            prev: Option<String>, // trimmed + capped line above (None at file start)
            next: Option<String>, // trimmed + capped line below (None at file end)
        }
        // Groups are (relative path, honest hit count, kept hits). The count covers
        // the WHOLE file even though only the first PER_FILE hits are kept, so a
        // header never under-reports the file's true hit total.
        let mut groups: Vec<(String, usize, Vec<Hit>)> = Vec::new();
        let mut path_matched = 0usize;
        let mut glob_matched = 0usize;
        for rel in &files {
            if start_time.elapsed() >= SEARCH_DEADLINE {
                timed_out = true;
                break;
            }
            let rel_norm = rel.replace('\\', "/");
            if let Some(ref p) = path_norm {
                let matches_path = rel_norm == *p || rel_norm.starts_with(&format!("{p}/"));
                if !matches_path {
                    continue;
                }
            }
            path_matched += 1;
            if !glob_pats.is_empty() {
                let matched = glob_pats.iter().any(|(g, raw)| {
                    g.matches(rel)
                        || (!raw.contains('/')
                            && Path::new(rel)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| g.matches(n))
                                .unwrap_or(false))
                });
                if !matched {
                    continue;
                }
            }
            glob_matched += 1;
            if name_mode {
                if matcher.is_match(rel) {
                    groups.push((rel.clone(), 1, Vec::new()));
                }
                continue;
            }
            // Capped, and skipped rather than fatal: a search walks EVERY file in
            // the project, so one oversized binary must not take the search down.
            let Ok(content) = read_text_capped(&root.join(rel)) else {
                continue;
            };
            // Fast pre-filter: 99% of files don't contain the pattern.
            // A single SIMD-accelerated regex scan across raw file bytes skips them
            // without splitting lines or doing any heap allocations.
            if !matcher.is_match(&content) {
                continue;
            }
            let lines: Vec<&str> = content.lines().collect();
            let mut count = 0usize;
            let mut kept: Vec<Hit> = Vec::new(); // hits kept (capped at PER_FILE)
            for (i, &line) in lines.iter().enumerate() {
                if matcher.is_match(line) {
                    count += 1;
                    // Earliest matches first: in a file, the first occurrence is
                    // usually the definition, later ones the call sites. Each kept
                    // hit captures its neighbours too, so rendering can add ±1
                    // context later without re-reading the file.
                    if kept.len() >= per_file_limit {
                        continue;
                    }
                    kept.push(Hit {
                        no: i + 1,
                        text: Self::snippet_text(line),
                        prev: if i > 0 {
                            Some(Self::snippet_text(lines[i - 1]))
                        } else {
                            None
                        },
                        next: if i + 1 < lines.len() {
                            Some(Self::snippet_text(lines[i + 1]))
                        } else {
                            None
                        },
                    });
                }
            }
            if count > 0 {
                groups.push((rel.clone(), count, kept));
            }
        }
        // Auto-expand single matched file: if the entire search produced hits in only 1 file
        // and max_per_file was not explicitly set, expand kept hits up to hit_cap so the model
        // gets complete results without needing a second query or all:true.
        if !name_mode && groups.len() == 1 && max_per_file_arg.is_none() && !show_all && groups[0].1 > groups[0].2.len() {
            let rel = groups[0].0.clone();
            if let Ok(content) = read_text_capped(&root.join(&rel)) {
                let lines: Vec<&str> = content.lines().collect();
                let mut expanded_kept = Vec::new();
                for (i, &line) in lines.iter().enumerate() {
                    if start_time.elapsed() >= SEARCH_DEADLINE {
                        timed_out = true;
                        break;
                    }
                    if matcher.is_match(line) {
                        if expanded_kept.len() >= hit_cap {
                            break;
                        }
                        expanded_kept.push(Hit {
                            no: i + 1,
                            text: Self::snippet_text(line),
                            prev: if i > 0 {
                                Some(Self::snippet_text(lines[i - 1]))
                            } else {
                                None
                            },
                            next: if i + 1 < lines.len() {
                                Some(Self::snippet_text(lines[i + 1]))
                            } else {
                                None
                            },
                        });
                    }
                }
                groups[0].2 = expanded_kept;
            }
        }
        if name_mode {
            if groups.is_empty() {
                if timed_out {
                    return "Search timed out after 5s before finding matching files. Specify a path: or glob: to narrow.".into();
                }
                if path_norm.is_some() && path_matched == 0 {
                    return format!(
                        "Path '{}' matched no files in the project.",
                        path_filter.as_deref().unwrap_or("")
                    );
                }
                if !glob_pats.is_empty() && glob_matched == 0 {
                    return format!(
                        "Glob '{}' matched no files in the project.",
                        glob.as_deref().unwrap_or("")
                    );
                }
                return format!("No files match '{query_raw}'.");
            }
            let total = groups.len();
            let mut out: Vec<String> = groups
                .into_iter()
                .map(|(rel, _, _)| format!("{rel} {}", Self::file_kind(&rel)))
                .collect();
            out.truncate(FILE_SHOW_CAP);
            let mut s = out.join("\n");
            let file_str = if total == 1 { "file" } else { "files" };
            if total > FILE_SHOW_CAP {
                s.push_str(&format!(
                    "\n… ({total} {file_str} match in total; showing the top {FILE_SHOW_CAP})"
                ));
            } else {
                s.push_str(&format!("\n(all {total} matching {file_str} shown)"));
            }
            if timed_out {
                s.push_str("\n(search timed out after 5s; specify a path: or glob: to narrow)");
            }
            return s;
        }
        // Most hits first; ties broken alphabetically so output is deterministic.
        groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if groups.is_empty() {
            if timed_out {
                return "Search timed out after 5s before finding matches. Specify a path: or glob: to narrow.".into();
            }
            // Distinguish "the path/glob excluded everything" from "the query just
            // isn't there" — the first is a tooling mistake, not a search result.
            if path_norm.is_some() && path_matched == 0 {
                return format!(
                    "Path '{}' matched no files, so nothing could be searched. Check the path spelling or drop it.",
                    path_filter.as_deref().unwrap_or("")
                );
            }
            if !glob_pats.is_empty() && glob_matched == 0 {
                return format!(
                    "Glob '{}' matched no files, so nothing could be searched. Try a \
                     looser glob (e.g. '*.ts', 'src/**/*.rs') or drop it.",
                    glob.as_deref().unwrap_or("")
                );
            }
            let hint = if case_sensitive {
                " Hint: You can also try setting case_sensitive: false, or regex: true."
            } else if !use_regex {
                " Hint: You can also try setting regex: true."
            } else {
                ""
            };
            return format!(
                "No matches for '{query_raw}'. Try a different term, add a path/glob, or use \
                 'name:{query_raw}' to search file names instead.{hint}"
            );
        }
        let total_hits: usize = groups.iter().map(|(_, c, _)| c).sum();
        // Narrow vs broad is decided on the whole result set (not per file), so
        // context appears exactly where it helps and broad searches stay lean.
        let narrow = groups.len() <= CONTEXT_GROUP_CAP;
        let mut lines_out: Vec<String> = Vec::new();
        let mut shown = 0usize;
        for (rel, count, hits) in &groups {
            // Render this file's kept hits now that `narrow` is known. Broad mode
            // is a single line per hit; narrow adds the neighbouring lines and
            // marks the hit with a leading '>'.
            let mut snips: Vec<String> = Vec::with_capacity(hits.len() * 3);
            if narrow {
                // To avoid duplicate/confusing lines when hits are adjacent or within ±1 line of
                // each other, merge overlapping context windows into a deduplicated, ordered sequence.
                // A line is marked with '>' if it is an actual match; otherwise it is unmarked context.
                use std::collections::BTreeMap;
                struct DisplayLine {
                    is_match: bool,
                    text: String,
                }
                let mut merged: BTreeMap<usize, DisplayLine> = BTreeMap::new();
                for h in hits {
                    if let Some(prev) = &h.prev {
                        merged.entry(h.no - 1).or_insert_with(|| DisplayLine {
                            is_match: false,
                            text: prev.clone(),
                        });
                    }
                    let entry = merged.entry(h.no).or_insert_with(|| DisplayLine {
                        is_match: true,
                        text: h.text.clone(),
                    });
                    entry.is_match = true;
                    entry.text = h.text.clone();

                    if let Some(next) = &h.next {
                        merged.entry(h.no + 1).or_insert_with(|| DisplayLine {
                            is_match: false,
                            text: next.clone(),
                        });
                    }
                }

                for (line_no, line) in merged {
                    if line.is_match {
                        snips.push(format!(">{line_no}: {}", line.text));
                    } else {
                        snips.push(format!("{line_no}: {}", line.text));
                    }
                }
            } else {
                for h in hits {
                    snips.push(format!("{}: {}", h.no, h.text));
                }
            }
            if shown >= FILE_SHOW_CAP || lines_out.len() + 1 + snips.len() > hit_cap {
                break;
            }
            lines_out.push(format!(
                "{} ({} {}) {}",
                rel,
                count,
                if *count == 1 { "hit" } else { "hits" },
                Self::file_kind(rel)
            ));
            for s in snips {
                lines_out.push(format!("  {s}"));
            }
            shown += 1;
        }
        let mut s = lines_out.join("\n");
        let plural = if total_hits == 1 { "hit" } else { "hits" };
        let nfiles = groups.len();
        let file_str = if nfiles == 1 { "file" } else { "files" };
        if shown < nfiles {
            s.push_str(&format!(
                "\n… ({total_hits} {plural} across {nfiles} {file_str}; showing the top {shown})"
            ));
        } else {
            let any_per_file_capped = groups.iter().any(|(_, count, hits)| *count > hits.len());
            if any_per_file_capped {
                let max_shown = groups.iter().map(|(_, _, h)| h.len()).max().unwrap_or(per_file_limit);
                s.push_str(&format!(
                    "\n({total_hits} {plural} across {nfiles} {file_str}; showing up to {max_shown} hits per file)"
                ));
            } else {
                s.push_str(&format!(
                    "\n(all {total_hits} {plural} across {nfiles} {file_str} shown)"
                ));
            }
        }
        if timed_out {
            s.push_str("\n(search timed out after 5s; specify a path: or glob: to narrow)");
        }
        s
    }

    /// Shared snippet cleanup: trim and cap so context lines read exactly like
    /// the hit lines they flank.
    pub(crate) fn snippet_text(line: &str) -> String {
        let mut s = line.trim().to_string();
        if s.chars().count() > 160 {
            s = s.chars().take(160).collect::<String>();
        }
        s
    }

    /// `code_symbols` — extract public/exported symbols from a file, directory, or project.
    pub(crate) fn code_symbols(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — code_symbols needs an active coding session.".into();
        };
        let path = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["path"].as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty());
        let rel_target = match path {
            Some(ref p) => match Self::project_path(root, p) {
                Ok(full) => match full.strip_prefix(root) {
                    Ok(r) => Some(r.to_string_lossy().replace('\\', "/")),
                    Err(_) => return format!("error: path '{p}' escapes the project folder."),
                },
                Err(e) => return format!("error: {e}"),
            },
            None => None,
        };
        symbols::run_code_symbols(Some(root), rel_target.as_deref())
    }

    /// `project_info` — a read-only orientation snapshot of the coding environment.
    /// No side effects: reads a few marker/manifest files, detects entry points,
    /// probes live git status, and probes a handful of `--version` commands.
    pub(crate) async fn project_info(&self, _arguments: &str, project_root: Option<&Path>) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        Self::run_project_info(root).await
    }

    pub(crate) fn profile_dir_lightweight(dir: &Path, cap: usize) -> (usize, bool, Vec<String>) {
        let mut count = 0;
        let mut capped = false;
        let mut ext_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(dir.to_path_buf());

        while let Some(current) = queue.pop_front() {
            if count >= cap {
                capped = true;
                break;
            }
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for e in entries.flatten() {
                if count >= cap {
                    capped = true;
                    break;
                }
                if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if Self::is_noise_dir(name) {
                        continue;
                    }
                    queue.push_back(p);
                } else if p.is_file() {
                    count += 1;
                    if let Some(ext) = p.extension().and_then(|ext| ext.to_str()) {
                        let ext_clean = ext.to_ascii_lowercase();
                        if !ext_clean.is_empty() {
                            *ext_counts.entry(format!(".{ext_clean}")).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        let mut sorted_exts: Vec<(String, usize)> = ext_counts.into_iter().collect();
        sorted_exts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let top_exts = sorted_exts.into_iter().take(3).map(|(ext, _)| ext).collect();

        (count, capped, top_exts)
    }

    pub(crate) async fn run_project_info(root: &Path) -> String {
        let mut out = format!("Root: {}\n", root.display());

        // Likely language(s), from marker files.
        let markers: &[(&str, &str)] = &[
            ("Cargo.toml", "Rust"),
            ("package.json", "Node/JavaScript"),
            ("tsconfig.json", "TypeScript"),
            ("pyproject.toml", "Python"),
            ("requirements.txt", "Python"),
            ("setup.py", "Python"),
            ("go.mod", "Go"),
            ("Gemfile", "Ruby"),
            ("pom.xml", "Java"),
            ("build.gradle", "Java"),
        ];
        let mut langs: Vec<&str> = Vec::new();
        let mut hit: Vec<&str> = Vec::new();
        for &(file, lang) in markers {
            if root.join(file).exists() {
                if !langs.contains(&lang) {
                    langs.push(lang);
                }
                hit.push(file);
            }
        }
        if langs.is_empty() {
            out.push_str("Looks like: (no manifest found — inspect the files to tell)\n");
        } else {
            out.push_str(&format!(
                "Looks like: {} ({})\n",
                langs.join(" + "),
                hit.join(", ")
            ));
        }

        // Monorepo / workspace detection.
        let mut workspaces: Vec<String> = Vec::new();
        if root.join("Cargo.toml").is_file() {
            if let Ok(cargo_text) = std::fs::read_to_string(root.join("Cargo.toml")) {
                if cargo_text
                    .lines()
                    .any(|l| l.trim() == "[workspace]" || l.trim().starts_with("[workspace."))
                {
                    workspaces.push("Cargo workspace".into());
                }
            }
        }
        if root.join("pnpm-workspace.yaml").is_file() {
            workspaces.push("pnpm workspace".into());
        } else if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&pkg) {
                if v.get("workspaces").is_some() {
                    workspaces.push("npm/yarn workspace".into());
                }
            }
        }
        if root.join("turbo.json").is_file() {
            workspaces.push("Turborepo".into());
        }
        if root.join("lerna.json").is_file() {
            workspaces.push("Lerna monorepo".into());
        }
        if root.join("nx.json").is_file() {
            workspaces.push("Nx monorepo".into());
        }
        if !workspaces.is_empty() {
            out.push_str(&format!("Workspace: {}\n", workspaces.join(" · ")));
        }

        // Key entry points detection.
        let entry_candidates = &[
            "src/App.tsx",
            "src/main.tsx",
            "src/index.tsx",
            "src/App.jsx",
            "src/main.jsx",
            "src/index.jsx",
            "src/main.js",
            "src/index.js",
            "src/main.rs",
            "src/lib.rs",
            "src-tauri/src/main.rs",
            "src-tauri/src/lib.rs",
            "main.py",
            "app.py",
            "__main__.py",
            "main.go",
            "project.godot",
            "main.tscn",
        ];
        let mut detected_entries = Vec::new();
        for &entry in entry_candidates {
            if root.join(entry).is_file() {
                detected_entries.push(entry);
            }
        }
        if !detected_entries.is_empty() {
            out.push_str(&format!("Entry points: {}\n", detected_entries.join(", ")));
        }

        // Scale & top-level layout.
        let mut files: Vec<String> = Vec::new();
        Self::walk_project(root, root, &mut files, 5000);
        let scale = if files.len() >= 5000 {
            "5000+".to_string()
        } else {
            files.len().to_string()
        };
        out.push_str(&format!("Scale: {scale} files\n"));

        let mut top_dirs = Vec::new();
        let mut root_files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(root) {
            let mut es: Vec<_> = entries.flatten().collect();
            es.sort_by_key(|e| e.path());
            for e in es {
                if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                    continue;
                }
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if Self::is_noise_dir(&name) {
                    continue;
                }
                if p.is_dir() {
                    let (count, capped, exts) = Self::profile_dir_lightweight(&p, 1000);
                    let plus = if capped { "+" } else { "" };
                    let detail = if count == 0 {
                        "empty".to_string()
                    } else if exts.is_empty() {
                        format!("{count}{plus} files")
                    } else {
                        format!("{count}{plus} files: {}", exts.join(", "))
                    };
                    top_dirs.push(format!("  {name}/ ({detail})"));
                } else if p.is_file() {
                    root_files.push(name);
                }
            }
        }

        if !root_files.is_empty() {
            let display_files = if root_files.len() > 15 {
                format!(
                    "{} … ({} files total)",
                    root_files[..15].join(", "),
                    root_files.len()
                )
            } else {
                root_files.join(", ")
            };
            out.push_str(&format!("Root files: {display_files}\n"));
        }

        if !top_dirs.is_empty() {
            out.push_str("Layout:\n");
            for d in top_dirs {
                out.push_str(&d);
                out.push('\n');
            }
        }

        // Version control — live branch + status probe.
        if root.join(".git").exists() {
            if let Some(git_info) = probe_git_status(root).await {
                out.push_str(&format!("Version control: {git_info}\n"));
            } else {
                out.push_str("Version control: git repo present\n");
            }
        } else {
            out.push_str(
                "Version control: none (session snapshots active — use get_diff to inspect diffs and restore_file to undo)\n",
            );
        }

        // Toolchains — probe a curated set; report those present + a compact version.
        #[cfg(windows)]
        let py = "py";
        #[cfg(not(windows))]
        let py = "python3";
        // (label, program, version arg) — note `go` uses `version`, not `--version`.
        let probes: &[(&str, &str, &str)] = &[
            ("python", py, "--version"),
            ("node", "node", "--version"),
            ("npm", "npm", "--version"),
            ("cargo", "cargo", "--version"),
            ("rustfmt", "rustfmt", "--version"),
            ("go", "go", "version"),
            ("ruby", "ruby", "--version"),
            ("bash", "bash", "--version"),
        ];
        let mut present: Vec<String> = Vec::new();
        let mut have: Vec<&str> = Vec::new();
        for &(label, program, arg) in probes {
            if let Some(v) = probe_version(program, arg, std::time::Duration::from_secs(4)).await {
                have.push(label);
                present.push(match first_version(&v) {
                    Some(ver) => format!("{label} {ver}"),
                    None => label.to_string(),
                });
            }
        }
        out.push_str(&format!(
            "Toolchains: {}\n",
            if present.is_empty() {
                "(none of the common ones found)".into()
            } else {
                present.join(" · ")
            }
        ));

        // Which post-edit syntax checks are live here (derived from the probes above).
        let checks: Vec<&str> = [
            ("python", "Python"),
            ("node", "JavaScript"),
            ("ruby", "Ruby"),
            ("bash", "shell"),
            ("go", "Go"),
            ("rustfmt", "Rust"),
        ]
        .iter()
        .filter(|(tool, _)| have.contains(tool))
        .map(|(_, lang)| *lang)
        .collect();
        out.push_str(&format!(
            "Syntax-checked on edit here: {}\n",
            if checks.is_empty() {
                "(none — no matching toolchain)".into()
            } else {
                checks.join(", ")
            }
        ));
        // Say WHY the list stops where it does. The on-edit net only covers languages
        // with a CHEAP single-file parse check; compiled/typed languages have none
        // (rustc needs the whole crate), so their real check is a run_command away.
        // Without this line a model reads the gap as a defect rather than a boundary
        // — GPT-5.6 did exactly that ("only Python and JavaScript despite having Rust
        // … feels odd"). Naming the reason costs one line and removes the ambiguity.
        out.push_str(
            "  (only languages with a cheap single-file parse check get the on-edit net. \
TypeScript types, Java and C++ have none — there's no useful per-file check, so \
verify those by running the real tool yourself via run_command, e.g. `cargo check` or \
`tsc --noEmit`.)\n",
        );

        // How to run/test — scripts from package.json, targets from a Makefile.
        let mut run: Vec<String> = Vec::new();
        if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
            if let Some(scripts) = serde_json::from_str::<Value>(&pkg)
                .ok()
                .and_then(|v| v.get("scripts").and_then(|s| s.as_object()).cloned())
            {
                let items: Vec<String> = scripts
                    .iter()
                    .take(12)
                    .map(|(k, val)| format!("{k} → {}", val.as_str().unwrap_or("")))
                    .collect();
                if !items.is_empty() {
                    run.push(format!("package.json scripts: {}", items.join(" · ")));
                }
            }
        }
        if let Ok(mk) = std::fs::read_to_string(root.join("Makefile")) {
            let targets: Vec<String> = mk
                .lines()
                .filter_map(|l| {
                    l.split_once(':').and_then(|(name, rest)| {
                        let name = name.trim();
                        (!rest.starts_with('=') // skip `VAR := …` assignments
                            && !name.is_empty()
                            && !name.starts_with('.')
                            && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
                        .then(|| name.to_string())
                    })
                })
                .take(15)
                .collect();
            if !targets.is_empty() {
                run.push(format!("Makefile targets: {}", targets.join(" ")));
            }
        }
        if run.is_empty() {
            out.push_str(
                "How to run/test: (no package.json scripts or Makefile — check the README or the language's usual test command)\n",
            );
        } else {
            out.push_str(&format!("How to run/test: {}\n", run.join("  |  ")));
        }

        out.push_str("\nNext: Use `code_search` to discover where features and logic are implemented across the codebase, or `read_file` on key entry points.\n");

        out.trim_end().to_string()
    }

    /// `project_write` — create/overwrite a project file; diff-then-approve (the same
    /// collaborative review as note writes), sandboxed to the project root.
    pub(crate) async fn project_write(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        if let Some(msg) = empty_args_error(arguments, "project_write", "'path' and 'content'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let content = parsed.as_ref().and_then(|v| arg_str(v, CONTENT_ALIASES));
        let (Some(path), Some(content)) = (path, content) else {
            return "error: project_write requires 'path' and 'content' string arguments".into();
        };
        let full = match Self::project_path(root, path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        if full.is_dir() {
            return format!("error: '{path}' is a directory — project_write writes files only.");
        }
        let current = std::fs::read_to_string(&full).unwrap_or_default();
        let content_to_write = if current.contains("\r\n") {
            match_file_endings(&current, content)
        } else {
            content.to_string()
        };
        if current == content_to_write {
            return format!("'{path}' already has this exact content; nothing to write.");
        }
        // Auto-approve project writes if the policy grants it (they're sandboxed to
        // the project and recoverable via git); otherwise ask, as before.
        if !self.permissions.auto_project_writes() {
            let diff = unified_diff(&current, &content_to_write);
            let id = self.approvals.next_id();
            let rx = self.approvals.register(id);
            let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
            let warning = if symbols::is_supported_code_file(path) {
                symbols::check_syntax(&content_to_write, ext).map(|err| {
                    format!("Heads up: this code has a syntax error in {} ({}).", err.lang, err.format_diagnostic())
                })
            } else {
                None
            };
            if on_event
                .send(StreamEvent::ToolApproval {
                    id,
                    tool: "project_write".into(),
                    path: path.to_string(),
                    diff,
                    warning,
                    command: None,
                })
                .is_err()
            {
                return format!("could not request approval for '{path}' (UI channel closed)");
            }
            let decision = rx.await.unwrap_or_default();
            if decision != "approve" {
                return rejection_response(
                    &decision,
                    &format!("The user rejected the write to '{path}'. No changes were made."),
                );
            }
        }
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Some(refusal) = self.guard_snapshot(&full, &format!("write '{path}'")) {
            return refusal;
        }
        match std::fs::write(&full, &content_to_write) {
            Ok(()) => {
                append_syntax_warning(
                    format!("Wrote '{path}' ({} bytes).", content_to_write.len()),
                    &full,
                    path,
                )
                .await
            }
            Err(e) => format!("write error: {e}"),
        }
    }

    /// `project_edit` — replace one exact, unique snippet in a project file;
    /// diff-then-approve, sandboxed to the project root.
    pub(crate) async fn project_edit(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        if let Some(msg) = empty_args_error(
            arguments,
            "project_edit",
            "'path', 'old_string' (or 'symbol'), and 'new_string'",
        ) {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let old = parsed.as_ref().and_then(|v| arg_str(v, OLD_ALIASES));
        let new = parsed.as_ref().and_then(|v| arg_str(v, NEW_ALIASES));
        let symbol = parsed.as_ref().and_then(|v| arg_str(v, SYMBOL_ALIASES));
        let (Some(path), Some(new)) = (path, new) else {
            return "error: project_edit requires 'path' and 'new_string' string arguments".into();
        };
        if old.is_none() && symbol.is_none() {
            return "error: project_edit requires 'old_string' (or a 'symbol' name to replace)".into();
        }
        if let Some(o) = old {
            if o.is_empty() && symbol.is_none() {
                return "error: old_string must not be empty".into();
            }
        }
        let full = match Self::project_path(root, path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        if full.is_dir() {
            return format!("error: '{path}' is a directory — project_edit edits files only.");
        }
        let current = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => {
                return format!(
                "error: '{path}' doesn't exist or can't be read — use write_file to create it"
            )
            }
        };
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");

        let (updated, note) = if let Some(sym) = symbol {
            match resolve_symbol_edit(&current, ext, sym, old, new) {
                Ok(res) => res,
                Err(err) => return err,
            }
        } else {
            let old_str = match old {
                Some(o) if !o.is_empty() => o,
                _ => return "error: old_string must not be empty".into(),
            };
            let (u, matched) = resolve_snippet_edit(&current, old_str, new);
            let note = match matched {
                SnippetMatch::Exact => String::new(),
                SnippetMatch::Fuzzy { start_line, end_line } => {
                    if start_line == end_line {
                        format!(" (replaced line {start_line}; indentation automatically harmonized)")
                    } else {
                        format!(" (replaced lines {start_line}–{end_line}; indentation automatically harmonized)")
                    }
                }
                SnippetMatch::Approximate { start_line, end_line } => {
                    if start_line == end_line {
                        format!(
                            " (replaced line {start_line} via anchor/similarity fallback; indentation automatically harmonized — verify surrounding lines)"
                        )
                    } else {
                        format!(
                            " (replaced lines {start_line}–{end_line} via anchor/similarity fallback; indentation automatically harmonized — verify surrounding lines)"
                        )
                    }
                }
                SnippetMatch::Ambiguous(n) => {
                    return format!(
                        "error: old_string appears {n} times in '{path}' (counting whitespace-insensitive matches) — include 2–3 more surrounding lines before and after so it matches uniquely."
                    );
                }
                SnippetMatch::NotFound(hint) => {
                    let mut msg = format!(
                        "error: old_string was not found in '{path}' — not even ignoring whitespace (indentation, trailing spaces, line endings, blank lines). Call read_file to check '{path}' for its current text (an earlier edit may have modified it), and retry with the exact lines from the file."
                    );
                    if let Some(h) = hint {
                        msg.push_str(&format!("\n\nHint: {}", h));
                    }
                    return msg;
                }
            };
            (u, note)
        };
        if updated == current {
            return format!("'{path}' is unchanged; nothing to write.");
        }
        let prev_err = if symbols::is_supported_code_file(path) {
            symbols::check_syntax(&current, ext)
        } else {
            None
        };
        if !self.permissions.auto_project_writes() {
            let diff = unified_diff(&current, &updated);
            let id = self.approvals.next_id();
            let rx = self.approvals.register(id);
            let warning = if prev_err.is_none() && symbols::is_supported_code_file(path) {
                symbols::check_syntax(&updated, ext).map(|err| {
                    format!("Heads up: this edit introduces a syntax error in {} ({}).", err.lang, err.format_diagnostic())
                })
            } else {
                None
            };
            if on_event
                .send(StreamEvent::ToolApproval {
                    id,
                    tool: "project_edit".into(),
                    path: path.to_string(),
                    diff,
                    warning,
                    command: None,
                })
                .is_err()
            {
                return format!("could not request approval for '{path}' (UI channel closed)");
            }
            let decision = rx.await.unwrap_or_default();
            if decision != "approve" {
                return rejection_response(
                    &decision,
                    &format!("The user rejected the edit to '{path}'. No changes were made."),
                );
            }
        }
        if let Some(refusal) = self.guard_snapshot(&full, &format!("edit '{path}'")) {
            return refusal;
        }
        if let Err(e) = std::fs::write(&full, &updated) {
            return format!("write error: {e}");
        }
        if prev_err.is_none() {
            append_syntax_warning(format!("Edited '{path}'.{note}"), &full, path).await
        } else {
            format!("Edited '{path}'.{note}")
        }
    }

    /// `project_delete` — remove a project file. ALWAYS approval-gated (never
    /// auto-approved, unlike writes): a delete is irreversible here — these
    /// projects have no version control — so the user confirms every one.
    pub(crate) async fn project_delete(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        if let Some(msg) = empty_args_error(arguments, "project_delete", "'path'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let Some(path) = path else {
            return "error: project_delete requires a 'path' string argument.".into();
        };
        let full = match Self::project_path(root, &path) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        if full.is_dir() {
            return format!("error: '{path}' is a directory — project_delete removes files only.");
        }
        if !full.is_file() {
            return format!("error: '{path}' doesn't exist — nothing to delete.");
        }
        let (lines, bytes) = std::fs::read(&full)
            .map(|b| (String::from_utf8_lossy(&b).lines().count(), b.len()))
            .unwrap_or((0, 0));
        // Always ask — a delete can't be undone here.
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: "project_delete".into(),
                path: path.to_string(),
                diff: format!("Delete: {path}\n({lines} lines, {bytes} bytes)"),
                warning: Some(
                    "Deletes this file — Outpost's session snapshots can restore it via project_restore if needed."
                        .into(),
                ),
                command: None,
            })
            .is_err()
        {
            return format!("could not request approval to delete '{path}' (UI channel closed)");
        }
        let decision = rx.await.unwrap_or_default();
        if decision != "approve" {
            return rejection_response(
                &decision,
                &format!("The user rejected deleting '{path}'. Nothing was removed."),
            );
        }
        if let Some(refusal) = self.guard_snapshot(&full, &format!("delete '{path}'")) {
            return refusal;
        }
        match std::fs::remove_file(&full) {
            Ok(()) => format!("Deleted '{path}'."),
            Err(e) => format!("delete error: {e}"),
        }
    }

    /// `project_rename` — move/rename a project file. Both paths are sandboxed to
    /// the project root; refuses to overwrite an existing destination. ALWAYS
    /// approval-gated (it can break references and moves a file mid-work).
    pub(crate) async fn project_rename(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        if let Some(msg) = empty_args_error(arguments, "project_rename", "'from' and 'to'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let from = parsed
            .as_ref()
            .and_then(|v| v["from"].as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty());
        let to = parsed
            .as_ref()
            .and_then(|v| v["to"].as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty());
        let (Some(from), Some(to)) = (from, to) else {
            return "error: project_rename requires 'from' and 'to' string arguments.".into();
        };
        let from_full = match Self::project_path(root, &from) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        let to_full = match Self::project_path(root, &to) {
            Ok(p) => p,
            Err(e) => return format!("error: {e}"),
        };
        if from_full.is_dir() {
            return format!("error: '{from}' is a directory — project_rename moves files only.");
        }
        if !from_full.is_file() {
            return format!("error: '{from}' doesn't exist.");
        }
        if to_full.exists() {
            return format!(
                "error: '{to}' already exists — rename won't overwrite it. Pick a new name or delete the destination first."
            );
        }
        // Always ask — a rename can break imports and moves a file mid-work.
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: "project_rename".into(),
                path: format!("{from} → {to}"),
                diff: format!("Rename:\n  {from}\n→ {to}"),
                warning: Some(
                    "References to the old path (imports, etc.) aren't updated automatically."
                        .into(),
                ),
                command: None,
            })
            .is_err()
        {
            return format!("could not request approval to rename '{from}' (UI channel closed)");
        }
        let decision = rx.await.unwrap_or_default();
        if decision != "approve" {
            return rejection_response(
                &decision,
                &format!("The user rejected renaming '{from}'. Nothing was moved."),
            );
        }
        if let Some(parent) = to_full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // BOTH ends. Capturing only the source made restore-all recreate the
        // source and leave the moved file sitting there — while reporting a full
        // restore, so the folder quietly ended up holding both copies.
        if let Some(refusal) = self.guard_snapshot(&from_full, &format!("rename '{from}'")) {
            return refusal;
        }
        if let Some(refusal) = self.guard_snapshot(&to_full, &format!("rename '{from}' to '{to}'"))
        {
            return refusal;
        }
        match std::fs::rename(&from_full, &to_full) {
            Ok(()) => format!("Renamed '{from}' → '{to}'."),
            Err(e) => format!("rename error: {e}"),
        }
    }

    /// `project_restore` — revert one file (or all files touched this session) to
    /// its pre-session snapshot. Approval-gated: it discards the current content.
    pub(crate) async fn project_restore(
        &self,
        arguments: &str,
        project_root: Option<&Path>,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        let path = serde_json::from_str::<Value>(arguments)
            .ok()
            .as_ref()
            .and_then(|v| arg_str(v, PATH_ALIASES).map(str::to_string))
            .filter(|p| !p.trim().is_empty());

        // Resolve the target set: one file, or everything captured under this root.
        let targets: Vec<PathBuf> = match &path {
            Some(p) if p != "all" => match Self::project_path(root, p) {
                Ok(f) => vec![f],
                Err(e) => return format!("error: {e}"),
            },
            _ => self
                .snapshots
                .captured_paths()
                .into_iter()
                .filter(|f| f.starts_with(root))
                .collect(),
        };
        if targets.is_empty() {
            return "Nothing to restore — no files were changed this session.".into();
        }
        let rels: Vec<String> = targets
            .iter()
            .map(|f| {
                f.strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| f.display().to_string())
            })
            .collect();

        // Approve the revert — it throws away the current content of these files.
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        let preview = format!(
            "Revert {} file(s) to their pre-session state:\n{}",
            rels.len(),
            rels.join("\n")
        );
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: "project_restore".into(),
                path: root.display().to_string(),
                diff: preview,
                warning: Some("This discards the changes made to these files this session.".into()),
                command: None,
            })
            .is_err()
        {
            return "could not request approval to restore (UI channel closed)".into();
        }
        let decision = rx.await.unwrap_or_default();
        if decision != "approve" {
            return rejection_response(
                &decision,
                "The user declined the restore. Nothing was changed.",
            );
        }

        let mut msgs = Vec::new();
        for (f, rel) in targets.iter().zip(rels.iter()) {
            match self.snapshots.restore(f) {
                Ok(m) => msgs.push(format!("'{rel}' {m}")),
                Err(e) => msgs.push(format!("'{rel}' — {e}")),
            }
        }
        format!("Restored {} file(s):\n{}", msgs.len(), msgs.join("\n"))
    }

    /// `process_manage` — manage background processes started with `run_command(background: true)`.
    /// 'check' waits briefly (default 2s, up to 60s) and returns status + new output.
    /// 'stop' terminates the process tree.
    /// 'list' returns all managed processes in this session.

    /// Format the aggregate diff bodies across files, splitting the `max_chars`
    /// budget fairly: small diffs consume only what they need, freeing up budget
    /// so larger diffs are not prematurely truncated when the total output easily
    /// fits within `max_chars`.
    pub(crate) fn format_project_changes(bodies: &[(String, String)], max_chars: usize) -> String {
        let changed = bodies.len();
        if changed == 0 {
            return "No net changes this session — files were touched but match their original content.".into();
        }

        // Allocate budget via fair-share redistribution:
        let mut remaining_budget = max_chars;
        let mut remaining_files = changed;
        let mut sorted_indices: Vec<usize> = (0..changed).collect();
        sorted_indices.sort_by_key(|&i| bodies[i].1.chars().count());

        let mut caps_by_idx = vec![0; changed];
        for &idx in &sorted_indices {
            let len = bodies[idx].1.chars().count();
            let share = (remaining_budget / remaining_files).max(1_000);
            if len <= share {
                caps_by_idx[idx] = len;
                remaining_budget = remaining_budget.saturating_sub(len);
            } else {
                caps_by_idx[idx] = share;
                remaining_budget = remaining_budget.saturating_sub(share);
            }
            remaining_files -= 1;
        }

        let mut out = String::new();
        for (i, (rel, body)) in bodies.iter().enumerate() {
            let cap = caps_by_idx[i];
            let body_len = body.chars().count();
            let shown = if body_len > cap {
                format!(
                    "{}\n\n[content truncated at {cap} chars — pass a larger max_chars, or read '{rel}' for the full diff]",
                    body.chars().take(cap).collect::<String>()
                )
            } else {
                body.clone()
            };
            out.push_str(&format!("=== {rel} ===\n{shown}\n\n"));
        }
        if out.chars().count() > max_chars {
            out = out.chars().take(max_chars).collect();
            out.push_str("\n… (truncated — pass a larger max_chars, or read individual files for the rest)");
        }
        format!(
            "{changed} file(s) changed this session:\n\n{}",
            out.trim_end()
        )
    }

    /// `project_changes` — an aggregate diff of everything changed this session,
    /// reconstructed from the same snapshots project_restore uses (so it works with
    /// no version control). The review half of the snapshot net: restore undoes,
    /// this shows.
    ///
    /// Bounded by an optional `max_chars` (default [`DEFAULT_CHANGES_CHARS`], ceiling
    /// [`MAX_CHARS_CEILING`]). The budget is split across the changed files using
    /// fair-share redistribution: small files take only what they need, so larger
    /// diffs aren't prematurely truncated when the total diff fits in budget.
    pub(crate) fn project_changes(&self, arguments: &str, project_root: Option<&Path>) -> String {
        let max_chars = parse_max_chars(arguments, DEFAULT_CHANGES_CHARS);
        let Some(root) = project_root else {
            return "error: no project is open — project tools need an active coding session."
                .into();
        };
        let mut paths: Vec<PathBuf> = self
            .snapshots
            .captured_paths()
            .into_iter()
            .filter(|f| f.starts_with(root))
            .collect();
        if paths.is_empty() {
            return "No files have been changed this session.".into();
        }
        paths.sort();

        // First pass: the diff body for every file with a net change.
        let mut bodies: Vec<(String, String)> = Vec::new();
        for f in &paths {
            let rel = f
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| f.display().to_string());
            let current = std::fs::read_to_string(f).ok();
            let body: Option<String> = match (self.snapshots.original(f), current) {
                // Didn't exist before → created this session.
                (Some(crate::snapshots::Original::Absent), Some(cur)) => {
                    Some(format!("(new file)\n{}", unified_diff("", &cur)))
                }
                // Created and then removed again — no net change.
                (Some(crate::snapshots::Original::Absent), None) => None,
                (Some(crate::snapshots::Original::Content(orig)), Some(cur)) => {
                    if orig == cur {
                        None // touched, but back to its original content
                    } else {
                        Some(unified_diff(&orig, &cur))
                    }
                }
                (Some(crate::snapshots::Original::Content(orig)), None) => {
                    Some(format!("(deleted — was {} lines)", orig.lines().count()))
                }
                (None, _) => None,
            };
            if let Some(body) = body {
                bodies.push((rel, body));
            }
        }
        if bodies.is_empty() {
            return "No net changes this session — files were touched but match their original content.".into();
        }
        Self::format_project_changes(&bodies, max_chars)
    }

    /// `typecheck` — run the project's static typechecker or syntax validator once. Deliberately NOT
    /// approval-gated: the command is FIXED (no arbitrary shell injection), so the
    /// closed verb set is the safety — there is nothing to review, unlike
    /// run_command. The posture is reported in the head block (§3.1). Never
    /// interactive: `npx --no-install` fails fast instead of prompting to fetch
    /// typescript, and the 300s ceiling covers a first `cargo check`.
    ///
    /// Accepts an optional `target` in `arguments` (e.g. "ts" or "rust") to run only
    /// that specific toolchain. When omitted or empty, all detected toolchains run.
    pub(crate) async fn run_typecheck(arguments: &str, project_root: Option<&Path>, cancel: &CancellationToken) -> String {
        let Some(root) = project_root else {
            return "error: no project is open — typecheck needs an active coding session.".into();
        };

        let mut checks: Vec<TypeCheck> = Vec::new();

        // --- Rust ---
        // A root-level Cargo.toml takes priority; otherwise fall back to a
        // Tauri layout with its crate nested under src-tauri/.
        if root.join("Cargo.toml").exists() {
            checks.push(TypeCheck {
                command: "cargo check".into(),
                working_dir: root.to_path_buf(),
                lang: "rust",
                display_name: "Rust",
            });
        } else if root.join("src-tauri/Cargo.toml").exists() {
            checks.push(TypeCheck {
                command: "cargo check".into(),
                working_dir: root.join("src-tauri"),
                lang: "rust",
                display_name: "Rust",
            });
        }

        // --- TypeScript ---
        // Root-level tsconfig.json takes priority; otherwise probe common
        // frontend subdirectories for a nested TypeScript project.
        let ts_dir: Option<PathBuf> = if root.join("tsconfig.json").exists() {
            Some(root.to_path_buf())
        } else {
            ["frontend", "client", "web", "ui"]
                .iter()
                .find(|sub| root.join(sub).join("tsconfig.json").exists())
                .map(|sub| root.join(sub))
        };
        if let Some(dir) = ts_dir {
            checks.push(TypeCheck {
                command: "npx --no-install tsc --noEmit".into(),
                working_dir: dir,
                lang: "typescript",
                display_name: "TypeScript",
            });
        }

        // --- Go ---
        if root.join("go.mod").exists() {
            checks.push(TypeCheck {
                command: "go vet ./...".into(),
                working_dir: root.to_path_buf(),
                lang: "go",
                display_name: "Go",
            });
        }

        // --- C# / .NET ---
        // Any .sln or .csproj at the root level triggers a dotnet build.
        let has_dotnet = std::fs::read_dir(root)
            .map(|mut it| it.any(|e| e.ok().map_or(false, |entry| {
                entry.path().extension().map_or(false, |ext| ext == "sln" || ext == "csproj")
            })))
            .unwrap_or(false);
        if has_dotnet {
            checks.push(TypeCheck {
                command: "dotnet build".into(),
                working_dir: root.to_path_buf(),
                lang: "dotnet",
                display_name: "C#/.NET",
            });
        }

        // --- Python ---
        let has_python = root.join("pyproject.toml").exists()
            || root.join("requirements.txt").exists()
            || root.join("setup.py").exists()
            || root.join("setup.cfg").exists()
            || root.join("Pipfile").exists()
            || root.join("tox.ini").exists()
            || std::fs::read_dir(root)
                .map(|mut it| it.any(|e| e.ok().map_or(false, |entry| {
                    entry.path().extension().map_or(false, |ext| ext == "py")
                })))
                .unwrap_or(false)
            || (root.join("src").is_dir() && std::fs::read_dir(root.join("src"))
                .map(|mut it| it.any(|e| e.ok().map_or(false, |entry| {
                    entry.path().extension().map_or(false, |ext| ext == "py")
                })))
                .unwrap_or(false));

        if has_python {
            #[cfg(windows)]
            let py_cmd: &'static str = {
                static PY_CMD: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                *PY_CMD.get_or_init(|| {
                    if let Ok(path_var) = std::env::var("PATH") {
                        for p in std::env::split_paths(&path_var) {
                            if p.join("py.exe").is_file() {
                                return "py -m compileall -q .";
                            }
                        }
                    }
                    "python -m compileall -q ."
                })
            };
            #[cfg(not(windows))]
            let py_cmd: &'static str = {
                static PY_CMD: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                *PY_CMD.get_or_init(|| {
                    if let Ok(path_var) = std::env::var("PATH") {
                        for p in std::env::split_paths(&path_var) {
                            if p.join("python3").is_file() {
                                return "python3 -m compileall -q .";
                            }
                        }
                    }
                    "python -m compileall -q ."
                })
            };
            checks.push(TypeCheck {
                command: py_cmd.to_string(),
                working_dir: root.to_path_buf(),
                lang: "python",
                display_name: "Python",
            });
        }

        if checks.is_empty() {
            if root.join("package.json").exists() {
                return "no TypeScript config (tsconfig.json) — nothing to typecheck in this Node project. JS has no compiler-style check; run the tests (npm test) instead.".into();
            }
            return "no typechecker for this project — typecheck runs `tsc --noEmit` (TypeScript), `cargo check` (Rust), `go vet ./...` (Go), `dotnet build` (C#/.NET), or `compileall` (Python).".into();
        }

        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let target_raw = parsed
            .as_ref()
            .and_then(|v| {
                v.get("target")
                    .or_else(|| v.get("lang"))
                    .or_else(|| v.get("language"))
                    .or_else(|| v.get("area"))
                    .or_else(|| v.get("toolchain"))
                    .or_else(|| v.get("path"))
                    .and_then(|t| t.as_str())
                    .or_else(|| v.as_str())
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let trimmed = arguments.trim().trim_matches('"');
                if !trimmed.is_empty() && !trimmed.starts_with('{') {
                    Some(trimmed)
                } else {
                    None
                }
            });

        let checks_to_run: Vec<&TypeCheck> = if let Some(target) = target_raw {
            let matched: Vec<&TypeCheck> = checks
                .iter()
                .filter(|c| Self::check_matches_target(c, target))
                .collect();
            if matched.is_empty() {
                let available: Vec<&str> = checks.iter().map(|c| c.display_name).collect();
                return format!(
                    "target '{target}' not found in detected toolchains for this project. Available toolchains: {}.",
                    available.join(", ")
                );
            }
            matched
        } else {
            checks.iter().collect()
        };

        let mut results = Vec::new();
        for check in checks_to_run {
            let mut cmd = {
                #[cfg(windows)]
                {
                    let mut c = tokio::process::Command::new("powershell");
                    c.arg("-Command").arg(&check.command);
                    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, as elsewhere
                    c
                }
                #[cfg(not(windows))]
                {
                    let mut c = tokio::process::Command::new("sh");
                    c.arg("-c").arg(&check.command);
                    c
                }
            };
            cmd.current_dir(&check.working_dir);
            cmd.kill_on_drop(true);
            let outcome = crate::tools::process::run_captured(&mut cmd, cancel, 300).await;
            let output = match outcome {
                crate::processes::RunOutcome::Output(o) => o,
                crate::processes::RunOutcome::Cancelled => {
                    return format!("Stopped by the user while typechecking: {}", check.command);
                }
                crate::processes::RunOutcome::TimedOut => {
                    return format!("`{}` timed out after 300s", check.command);
                }
                crate::processes::RunOutcome::SpawnFailed(e) => {
                    return format!("error launching `{}`: {e}", check.command);
                }
            };
            results.push(format!("$ {}\n{}", check.command, format_captured(output)));
        }
        results.join("\n\n")
    }

    fn check_matches_target(check: &TypeCheck, target: &str) -> bool {
        let t = target.trim().to_ascii_lowercase();
        let t = t.trim_start_matches('.').trim_end_matches('/');

        match check.lang {
            "rust" => {
                matches!(t, "rust" | "rs" | "cargo" | "backend" | "src-tauri" | "tauri")
                    || check.working_dir.to_string_lossy().to_ascii_lowercase().ends_with(t)
                    || check.command.contains(t)
            }
            "typescript" => {
                matches!(t, "typescript" | "ts" | "tsx" | "js" | "javascript" | "frontend" | "client" | "web" | "ui" | "tsc")
                    || check.working_dir.to_string_lossy().to_ascii_lowercase().ends_with(t)
                    || check.command.contains(t)
            }
            "go" => {
                matches!(t, "go" | "golang" | "vet")
                    || check.command.contains(t)
            }
            "dotnet" => {
                matches!(t, "dotnet" | "c#" | "csharp" | "cs" | ".net")
                    || check.command.contains(t)
            }
            "python" => {
                matches!(t, "python" | "py" | "compileall")
                    || check.command.contains(t)
            }
            _ => check.lang.eq_ignore_ascii_case(t) || check.command.contains(t),
        }
    }

    pub(crate) async fn typecheck(&self, arguments: &str, project_root: Option<&Path>, cancel: &CancellationToken) -> String {
        Self::run_typecheck(arguments, project_root, cancel).await
    }


}

/// A single typecheck command paired with the directory it must run in.
/// Each check carries its own `working_dir` so nested projects (Tauri's
/// `src-tauri/`, monorepo frontends) get their checker invoked in the right
/// subdirectory rather than the session root.
struct TypeCheck {
    command: String,
    working_dir: PathBuf,
    lang: &'static str,
    display_name: &'static str,
}

// --- Post-edit syntax check --------------------------------------------------
// A line-range edit can miscount a boundary and silently corrupt a file (a stray
// paren, an unclosed block). Nothing catches it until the code is *run* several
// steps later and throws a traceback — by which point the model has to run, read
// the error, re-read the file, hunt, and fix. Parsing the file right after the
// edit turns that into an immediate, located signal on the edit's own result.
//
// Only languages with a CHEAP, PARSE-ONLY check are covered. The check runs
// un-gated after every edit, so it must NEVER execute the file — each of
// ast.parse / `node --check` / `ruby -c` / `bash -n` / `gofmt -e` / `rustfmt --emit stdout`
// only parses. TypeScript types, Java and C++ have no cheap single-file check
// (rustc needs the whole crate; tsc is whole-project and slow), so the model
// runs the real build (`cargo check`, `tsc`, …) itself via run_command.
// `rustfmt --emit stdout` is the exception: it parses ONE file in isolation
// (no crate context) and writes the formatted result to stdout, not the file,
// so it never executes or mutates — same contract as the other checkers here.
// It catches *syntax* (unclosed delimiters, missing tokens) but not *types*
// (missing `use`, wrong crate path) — the same line the Python/JS checks draw.
// A checker that isn't installed just skips silently.

/// If `full` is a source file whose language we can cheaply parse-check and it no
/// longer parses, return (language label, the checker's error). Best-effort: skips
/// silently for unsupported extensions, when the checker isn't on PATH, or on
/// timeout. NEVER executes the file — parse-only, so it's safe to run un-gated.
async fn syntax_warning(full: &Path) -> Option<(&'static str, String)> {
    let ext = full.extension().and_then(|e| e.to_str())?;
    let path_str = full.to_str().unwrap_or("");

    // 1. Fast in-memory Tree-Sitter AST syntax check (<1ms, zero PATH dependencies)
    // Supports Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, GDScript, and C#.
    if symbols::is_supported_code_file(path_str) {
        if let Ok(content) = std::fs::read_to_string(full) {
            if let Some(err) = symbols::check_syntax(&content, ext) {
                return Some((err.lang, err.format_diagnostic()));
            } else {
                return None;
            }
        }
    }

    // 2. CLI parse checks for remaining script languages (never executes code)
    let (lang, program, pre_args): (&str, &str, Vec<&str>) = match ext {
        "rb" => ("Ruby", "ruby", vec!["-c"]),
        "sh" | "bash" => ("shell", "bash", vec!["-n"]),
        _ => return None,
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    cmd.args(pre_args).arg(full);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    if output.status.success() {
        return None;
    }
    // We only read stderr (gofmt echoes the reformatted file to stdout on success).
    // Keep the signal tight: drop blank lines and JS-style "at …" stack frames
    // (node --check appends a trace + version footer), and cap it — the file:line
    // plus the error message is what the model actually needs to fix it.
    let raw = String::from_utf8_lossy(&output.stderr);
    let msg = raw
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("at "))
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    (!msg.is_empty()).then_some((lang, msg))
}

/// Append a syntax warning to a write/edit result if the edited file no longer
/// parses. A no-op for languages without a cheap parse-only check, or when the
/// checker isn't installed.
async fn append_syntax_warning(mut msg: String, full: &Path, path: &str) -> String {
    if let Some((lang, w)) = syntax_warning(full).await {
        msg.push_str(&format!(
            "\n\n⚠ Heads up: '{path}' no longer parses as {lang} — {w}. \
             read_file it and fix the syntax before running it."
        ));
    }
    msg
}

/// Run `git rev-parse` and `git status --porcelain` to report live branch and changed files.
async fn probe_git_status(root: &Path) -> Option<String> {
    let mut branch_cmd = tokio::process::Command::new("git");
    branch_cmd.stdin(std::process::Stdio::null());
    branch_cmd.kill_on_drop(true);
    branch_cmd.args(["rev-parse", "--abbrev-ref", "HEAD"]);
    branch_cmd.current_dir(root);
    #[cfg(windows)]
    branch_cmd.creation_flags(0x0800_0000);

    let branch_out = tokio::time::timeout(std::time::Duration::from_secs(2), branch_cmd.output())
        .await
        .ok()?
        .ok()?;

    if !branch_out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();
    if branch.is_empty() {
        return None;
    }

    let mut status_cmd = tokio::process::Command::new("git");
    status_cmd.stdin(std::process::Stdio::null());
    status_cmd.kill_on_drop(true);
    status_cmd.args(["status", "--porcelain"]);
    status_cmd.current_dir(root);
    #[cfg(windows)]
    status_cmd.creation_flags(0x0800_0000);

    let status_out = tokio::time::timeout(std::time::Duration::from_secs(2), status_cmd.output())
        .await
        .ok()?
        .ok()?;

    if !status_out.status.success() {
        return Some(format!("git on branch '{branch}'"));
    }

    let status_str = String::from_utf8_lossy(&status_out.stdout);
    let mut modified = 0usize;
    let mut untracked = 0usize;
    let mut changed_files = Vec::new();

    for line in status_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("??") {
            untracked += 1;
        } else {
            modified += 1;
        }
        if line.len() > 3 {
            let path = line[3..].trim();
            if changed_files.len() < 5 {
                changed_files.push(path.to_string());
            }
        }
    }

    if modified == 0 && untracked == 0 {
        Some(format!("git on branch '{branch}' (clean working tree)"))
    } else {
        let mut parts = Vec::new();
        if modified > 0 {
            parts.push(format!("{modified} modified"));
        }
        if untracked > 0 {
            parts.push(format!("{untracked} untracked"));
        }
        let detail = if !changed_files.is_empty() {
            let overflow = (modified + untracked).saturating_sub(changed_files.len());
            if overflow > 0 {
                format!(" ({}, +{overflow} more)", changed_files.join(", "))
            } else {
                format!(" ({})", changed_files.join(", "))
            }
        } else {
            String::new()
        };
        Some(format!("git on branch '{branch}' · {}{detail}", parts.join(", ")))
    }
}

/// Run `program <arg>` and return its first non-empty output line if it exists and
/// exits cleanly — used to detect an installed toolchain for `project_info`.
/// Best-effort: None if the program isn't on PATH or times out. Read-only.
async fn probe_version(program: &str, arg: &str, timeout: std::time::Duration) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    cmd.arg(arg);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Most tools print the version to stdout; a few (e.g. `java -version`) use
    // stderr — prefer stdout, fall back to stderr.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Some(
        text.lines()
            .find(|l| !l.trim().is_empty())?
            .trim()
            .to_string(),
    )
}

/// The first dotted version number in a string (e.g. "5.2.15" out of bash's banner),
/// so toolchain lines stay compact. None if there's no `\d+.\d+…` run.

fn first_version(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            let mut saw_dot = false;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                saw_dot |= b[i] == b'.';
                i += 1;
            }
            if saw_dot {
                return Some(s[start..i].trim_end_matches('.').to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// A git-style unified diff between two file contents, for the approval card.

fn image_media_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod image_tests {
    use super::image_media_type;

    #[test]
    fn maps_supported_extensions_case_insensitively() {
        assert_eq!(image_media_type("shot.png"), Some("image/png"));
        assert_eq!(image_media_type("plots/PHOTO.JPG"), Some("image/jpeg"));
        assert_eq!(image_media_type("x.jpeg"), Some("image/jpeg"));
        assert_eq!(image_media_type("anim.gif"), Some("image/gif"));
        assert_eq!(image_media_type("m.WebP"), Some("image/webp"));
    }

    #[test]
    fn rejects_non_images_and_extensionless_paths() {
        assert_eq!(image_media_type("main.rs"), None);
        assert_eq!(image_media_type("notes.txt"), None);
        assert_eq!(image_media_type("Makefile"), None);
        // Only the LAST extension counts — a .png in the middle isn't an image.
        assert_eq!(image_media_type("archive.png.zip"), None);
    }
}

#[cfg(test)]
mod max_chars_tests {
    use super::parse_max_chars;

    #[test]
    fn defaults_when_absent_or_unparsable() {
        assert_eq!(parse_max_chars(r#"{"path":"a.rs"}"#, 32_000), 32_000);
        assert_eq!(parse_max_chars("not json", 8_000), 8_000);
    }

    #[test]
    fn honors_explicit_value() {
        assert_eq!(
            parse_max_chars(r#"{"path":"a.rs","max_chars":50000}"#, 8_000),
            50_000
        );
    }

    #[test]
    fn clamps_to_floor_and_ceiling() {
        assert_eq!(parse_max_chars(r#"{"max_chars":1}"#, 32_000), 1_000);
        assert_eq!(parse_max_chars(r#"{"max_chars":999999}"#, 32_000), 100_000);
    }

    #[test]
    fn parses_numeric_strings() {
        assert_eq!(
            parse_max_chars(r#"{"max_chars":"15000"}"#, 32_000),
            15_000
        );
    }
}

#[cfg(test)]
mod parse_line_num_tests {
    use super::parse_line_num;
    use serde_json::json;

    #[test]
    fn parses_integers_and_strings_with_aliases() {
        let v1 = json!({ "start_line": 42 });
        assert_eq!(parse_line_num(&v1, &["start", "start_line"]), Some(42));

        let v2 = json!({ "start": "100" });
        assert_eq!(parse_line_num(&v2, &["start", "start_line"]), Some(100));

        let v3 = json!({ "from": 0 });
        assert_eq!(parse_line_num(&v3, &["start", "from"]), Some(0));

        let v4 = json!({ "other": 10 });
        assert_eq!(parse_line_num(&v4, &["start", "start_line"]), None);

        let v5 = json!({ "start": "invalid" });
        assert_eq!(parse_line_num(&v5, &["start"]), None);
    }
}

#[cfg(test)]
mod number_lines_capped_tests {
    use super::{number_lines, number_lines_capped};

    #[test]
    fn numbers_lines_correctly() {
        let text = "alpha\nbeta\ngamma";
        let (numbered, truncated) = number_lines_capped(text, 10_000);
        assert!(!truncated);
        assert_eq!(numbered, "1\talpha\n2\tbeta\n3\tgamma\n");
        assert_eq!(number_lines(text), "1\talpha\n2\tbeta\n3\tgamma\n");
    }

    #[test]
    fn truncates_at_max_chars() {
        let text = "line 1\nline 2\nline 3\nline 4";
        let (numbered, truncated) = number_lines_capped(text, 15);
        assert!(truncated);
        assert_eq!(numbered.chars().count(), 15);
    }
}

#[cfg(test)]
mod read_text_capped_tests {
    use super::read_text_capped;
    use std::path::{Path, PathBuf};

    struct TestFile(PathBuf);
    impl TestFile {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "outpost_read_f_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_file(&p);
            TestFile(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn strips_utf8_bom() {
        let temp = TestFile::new("utf8_bom");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello world\nsecond line");
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello world\nsecond line");
        assert!(!read.starts_with('\u{feff}'));
    }

    #[test]
    fn decodes_utf16_le_with_bom() {
        let temp = TestFile::new("utf16_le");
        let s = "hello from utf16 le";
        let mut bytes = vec![0xFF, 0xFE];
        for b in s.encode_utf16() {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello from utf16 le");
    }

    #[test]
    fn decodes_utf16_be_with_bom() {
        let temp = TestFile::new("utf16_be");
        let s = "hello from utf16 be";
        let mut bytes = vec![0xFE, 0xFF];
        for b in s.encode_utf16() {
            bytes.extend_from_slice(&b.to_be_bytes());
        }
        std::fs::write(temp.path(), &bytes).unwrap();

        let read = read_text_capped(temp.path()).unwrap();
        assert_eq!(read, "hello from utf16 be");
    }
}

#[cfg(test)]
mod project_read_tools_tests {
    use super::Tools;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "outpost_read_d_{name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TestDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_path_handles_leading_slashes_and_absolute_paths() {
        let temp = TestDir::new("proj_path");
        let root = temp.path();
        let sub = root.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("app.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        // Relative path
        let p1 = Tools::project_path(root, "src/app.rs").unwrap();
        assert_eq!(p1, file);

        // Leading slash
        let p2 = Tools::project_path(root, "/src/app.rs").unwrap();
        assert_eq!(p2, file);

        // Leading backslash
        let p3 = Tools::project_path(root, r"\src\app.rs").unwrap();
        assert_eq!(p3, file);

        // Absolute path within project
        let abs_str = file.to_string_lossy().to_string();
        let p4 = Tools::project_path(root, &abs_str).unwrap();
        assert_eq!(p4, file);

        // Absolute path outside project errors
        let outside = std::env::temp_dir().join(format!(
            "outside_{}_{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::write(&outside, "outside");
        let outside_str = outside.to_string_lossy().to_string();
        assert!(Tools::project_path(root, &outside_str).is_err());
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn project_read_lines_aliases_and_reversed_bounds() {
        let temp = TestDir::new("read_lines_alias");
        let root = temp.path();
        let file = root.join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\nline4\nline5\n").unwrap();

        // Normal start and lines
        let out_lines = Tools::run_project_read_lines(&json!({ "path": "test.txt", "start": 2, "lines": 3 }).to_string(), root);
        assert!(out_lines.contains("2\tline2\n3\tline3\n4\tline4"), "{out_lines}");

        // Normal start and end (backward compatibility)
        let out = Tools::run_project_read_lines(&json!({ "path": "test.txt", "start": 2, "end": 4 }).to_string(), root);
        assert!(out.contains("2\tline2\n3\tline3\n4\tline4"), "{out}");

        // Aliases offset and limit as strings
        let out_alias = Tools::run_project_read_lines(&json!({ "path": "test.txt", "offset": "2", "limit": "2" }).to_string(), root);
        assert!(out_alias.contains("2\tline2\n3\tline3"), "{out_alias}");

        // Auto-heal when end < start (treats end as count): start 3, end 2 -> lines 3, 4
        let out_heal = Tools::run_project_read_lines(&json!({ "path": "test.txt", "start": 3, "end": 2 }).to_string(), root);
        assert!(out_heal.contains("3\tline3\n4\tline4"), "{out_heal}");
    }

    #[test]
    fn project_read_lines_streams_large_file() {
        let temp = TestDir::new("streaming");
        let root = temp.path();
        let file = root.join("large.log");

        // Create a file > 8 MB with numbered lines
        {
            use std::io::{BufWriter, Write};
            let f = std::fs::File::create(&file).unwrap();
            let mut writer = BufWriter::new(f);
            // 90,000 lines of ~100 chars each = ~9 MB
            for i in 1..=90_000 {
                writeln!(writer, "line {i:06} data padding abcdefghijklmnopqrstuvwxyz 1234567890 1234567890 1234567890").unwrap();
            }
            writer.flush().unwrap();
        }

        // Reading lines 10..12 of an 9MB file succeeds and streams without out-of-memory or 8MB refusal
        let out = Tools::run_project_read_lines(&json!({ "path": "large.log", "start": 10, "end": 12 }).to_string(), root);
        assert!(out.contains("lines 10–12"), "{out}");
        assert!(out.contains("10\tline 000010"), "{out}");
        assert!(out.contains("12\tline 000012"), "{out}");
        assert!(!out.contains("13\tline 000013"), "{out}");
    }

    #[test]
    fn project_read_telemetry_under_and_over_ceiling() {
        let temp = TestDir::new("read_telemetry");
        let root = temp.path();

        // 1. File under ceiling (300 lines of 10 chars = ~3000 chars)
        // With max_chars = 1000 (the minimum floor), it truncates but stays well under MAX_CHARS_CEILING (100,000)
        let file_under = root.join("under.txt");
        let lines_under: String = (1..=300).map(|i| format!("line {i:03}\n")).collect();
        std::fs::write(&file_under, &lines_under).unwrap();

        let out_under = Tools::run_project_read(
            &json!({ "path": "under.txt", "max_chars": 1000 }).to_string(),
            root,
        );
        assert!(out_under.contains("[content truncated at 1000 chars (showing lines 1–"), "{out_under}");
        assert!(out_under.contains("of 300;"), "{out_under}");
        assert!(out_under.contains("pass max_chars:"), "{out_under}");
        assert!(out_under.contains("to read the full file"), "{out_under}");

        // 2. File over ceiling (> 100,000 chars)
        let file_over = root.join("over.txt");
        // 2,000 lines of 60 chars = ~120,000 chars
        let lines_over: String = (1..=2000).map(|i| format!("line {i:04}: 1234567890 1234567890 1234567890 1234567890\n")).collect();
        std::fs::write(&file_over, &lines_over).unwrap();

        let out_over = Tools::run_project_read(
            &json!({ "path": "over.txt", "max_chars": 1000 }).to_string(),
            root,
        );
        assert!(out_over.contains("of 2000;"), "{out_over}");
        assert!(out_over.contains("file exceeds the 100000 char ceiling. Pass start and lines to read_file to read targeted ranges"), "{out_over}");

        // Delegating read_file with start/lines to run_project_read_lines
        let out_ranged = Tools::run_project_read(
            &json!({ "path": "over.txt", "start": 10, "lines": 5 }).to_string(),
            root,
        );
        assert!(out_ranged.contains("'over.txt' lines 10–14 (of 2000):"), "{out_ranged}");
        assert!(out_ranged.contains("10\tline 0010:"), "{out_ranged}");
        assert!(out_ranged.contains("14\tline 0014:"), "{out_ranged}");
    }
}

/// Read a text file, refusing one too large to hold in memory sensibly.
///
/// The read paths went straight to `read_to_string`, which allocates the WHOLE
/// file before any output cap applies — so a stray 2 GB log, database or media
/// file sitting in the project folder was an out-of-memory abort rather than an
/// error message, and the model has no way to know a path's size before asking
/// for it. 8 MB is far above any real source file. Strips UTF-8 BOM if present
/// and falls back to UTF-16 LE/BE decoding when standard UTF-8 fails.


#[cfg(test)]
mod project_search_tests {
    use super::Tools;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A throwaway project tree under the OS temp dir, removed on drop.
    struct Proj(PathBuf);

    impl Proj {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("outpost_search_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Proj(dir)
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
    }

    impl Drop for Proj {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn search(args: &str, root: &Path) -> String {
        Tools::run_project_search(args, root)
    }

    #[test]
    fn groups_by_file_and_ranks_most_hits_first() {
        let p = Proj::new("rank");
        p.write("src/a.ts", "fn alpha() {}\nfn beta() {}\nfn alpha() {}\n");
        p.write("src/b.ts", "fn alpha() {}\n");
        let out = search(r#"{"query":"alpha"}"#, p.0.as_path());
        // a.ts (2 hits) must sort before b.ts (1 hit), both with headers.
        let a = out.find("src/a.ts (2 hits)").expect("a.ts header");
        let b = out.find("src/b.ts (1 hit)").expect("b.ts header");
        assert!(a < b, "most-hit file first:\n{out}");
        // The matched line is marked with '>' and its neighbour shown as context.
        assert!(out.contains("  >1: fn alpha() {}"), "hit marked:\n{out}");
        assert!(out.contains("  2: fn beta() {}"), "context line:\n{out}");
    }

    #[test]
    fn adjacent_hits_merge_context_without_duplicate_lines() {
        let p = Proj::new("adjacent");
        p.write(
            "src/app.ts",
            "line 1\nif (codeOn) {\nt.push(\"run_python\");\n}\nline 5\n",
        );
        // Searching for pattern that matches both line 2 and line 3
        let out = search(r#"{"query":"codeOn|run_python","regex":true}"#, p.0.as_path());

        // Both hits must be present and marked with '>'
        assert!(out.contains("  >2: if (codeOn) {"), "hit 2 marked:\n{out}");
        assert!(out.contains("  >3: t.push(\"run_python\");"), "hit 3 marked:\n{out}");
        // Surrounding context lines 1 and 4 must be present
        assert!(out.contains("  1: line 1"), "prev context:\n{out}");
        assert!(out.contains("  4: }"), "next context:\n{out}");

        // Lines 2 and 3 must appear EXACTLY ONCE each, never duplicated!
        assert_eq!(
            out.matches("if (codeOn)").count(),
            1,
            "line 2 must appear exactly once:\n{out}"
        );
        assert_eq!(
            out.matches("run_python").count(),
            1,
            "line 3 must appear exactly once:\n{out}"
        );
    }

    #[test]
    fn headers_carry_file_kind_tags() {
        let p = Proj::new("kinds");
        p.write("src/a.rs", "needle\n");
        p.write("docs/NOTES.md", "needle\n");
        p.write("tests/a_test.rs", "needle\n");
        p.write("config/settings.json", "needle\n");
        p.write("assets/pic.png", "needle\n");
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(out.contains("src/a.rs (1 hit) [src]"), "src tag:\n{out}");
        assert!(
            out.contains("docs/NOTES.md (1 hit) [doc]"),
            "doc tag:\n{out}"
        );
        assert!(
            out.contains("tests/a_test.rs (1 hit) [test]"),
            "test tag:\n{out}"
        );
        assert!(
            out.contains("config/settings.json (1 hit) [cfg]"),
            "cfg tag:\n{out}"
        );
        assert!(
            out.contains("assets/pic.png (1 hit) [other]"),
            "other tag:\n{out}"
        );
    }

    #[test]
    fn archived_docs_are_excluded_from_search() {
        let p = Proj::new("archive_skip");
        p.write("NOTES.md", "needle\n");
        p.write("archive/NOTES_OLD.md", "needle\n");
        p.write("archive/CODE_REVIEW.md", "needle\n");
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(
            out.contains("NOTES.md (1 hit)"),
            "live doc must be found:\n{out}"
        );
        assert!(
            !out.contains("archive/"),
            "archive/ must never surface:\n{out}"
        );
    }

    #[test]
    fn name_mode_matches_paths_not_contents() {
        let p = Proj::new("name");
        p.write("src/settings.ts", "export const x = 1;\n");
        p.write("src/other.ts", "settings appears in body text only\n");
        let out = search(r#"{"query":"name:settings"}"#, p.0.as_path());
        assert!(out.contains("src/settings.ts"), "path match:\n{out}");
        assert!(
            !out.contains("other.ts"),
            "body text must not match:\n{out}"
        );
    }

    #[test]
    fn searching_for_literal_colon_does_not_break_into_filename_mode() {
        let p = Proj::new("name_colon");
        p.write("config.yaml", "name: Outpost\nversion: 1.0\n");
        p.write("other.rs", "pub name: String,\n");

        // Literal "name:" content search (the bug DeepSeek hit)
        let out = search(r#"{"query":"name:"}"#, p.0.as_path());
        assert!(out.contains("config.yaml"), "should match in config.yaml:\n{out}");
        assert!(out.contains("other.rs"), "should match in other.rs:\n{out}");

        // "name: Outpost" content search with space after colon
        let out_spaced = search(r#"{"query":"name: Outpost"}"#, p.0.as_path());
        assert!(out_spaced.contains("config.yaml"), "should match in config.yaml:\n{out_spaced}");
        assert!(!out_spaced.contains("other.rs"), "other.rs does not have Outpost:\n{out_spaced}");

        // Explicit escape \name:settings to search file contents
        p.write("escaped.txt", "name:settings in text\n");
        let out_escaped = search(r#"{"query":"\\name:settings"}"#, p.0.as_path());
        assert!(out_escaped.contains("escaped.txt"), "should match literal in escaped.txt:\n{out_escaped}");
    }

    #[test]
    fn a_glob_that_excludes_everything_is_called_out() {
        let p = Proj::new("glob");
        p.write("a.rs", "fn x() {}\n");
        let out = search(r#"{"query":"fn","glob":"*.py"}"#, p.0.as_path());
        assert!(out.contains("matched no files"), "honest glob miss:\n{out}");
    }

    #[test]
    fn filename_search_aliases_work() {
        let p = Proj::new("aliases");
        p.write("src/settings.ts", "export const x = 1;\n");
        for prefix in ["path:", "file:", "filename:"] {
            let out = search(&format!(r#"{{"query":"{prefix}settings"}}"#), p.0.as_path());
            assert!(out.contains("src/settings.ts"), "{prefix} should match file path:\n{out}");
        }
    }

    #[test]
    fn glob_with_no_slash_matches_files_in_subdirectories() {
        let p = Proj::new("subglob");
        p.write("src/deep/sub/file.rs", "fn target() {}\n");
        p.write("src/deep/sub/file.txt", "fn target() {}\n");
        let out = search(r#"{"query":"target","glob":"*.rs"}"#, p.0.as_path());
        assert!(out.contains("file.rs"), "glob without slash should match files in subdirectories:\n{out}");
        assert!(!out.contains("file.txt"), "non-matching extension excluded:\n{out}");
    }

    #[test]
    fn glob_with_windows_backslashes_normalizes_and_matches() {
        let p = Proj::new("winglob");
        p.write("src/target.rs", "fn target() {}\n");
        let out = search(r#"{"query":"target","glob":"src\\*.rs"}"#, p.0.as_path());
        assert!(out.contains("src/target.rs"), "windows backslash glob should match:\n{out}");
    }

    #[test]
    fn a_quiet_query_hints_at_alternatives() {
        let p = Proj::new("miss");
        p.write("a.rs", "fn x() {}\n");
        let out = search(r#"{"query":"zzz_not_here"}"#, p.0.as_path());
        assert!(out.starts_with("No matches"), "no-match path:\n{out}");
        assert!(out.contains("name:"), "hints at name: mode:\n{out}");
    }

    #[test]
    fn case_sensitive_obeys_the_flag() {
        let p = Proj::new("case");
        p.write("a.rs", "Alpha\n");
        assert!(search(r#"{"query":"alpha"}"#, p.0.as_path()).contains("a.rs"));
        assert!(
            search(r#"{"query":"alpha","case_sensitive":true}"#, p.0.as_path())
                .starts_with("No matches")
        );
    }

    #[test]
    fn context_lines_flank_each_hit() {
        let p = Proj::new("context");
        p.write(
            "src/a.rs",
            "fn before() {}\nfn needle() {}\nfn after() {}\n",
        );
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(out.contains("  >2: fn needle() {}"), "hit marked:\n{out}");
        assert!(out.contains("  1: fn before() {}"), "prev line:\n{out}");
        assert!(out.contains("  3: fn after() {}"), "next line:\n{out}");
    }

    #[test]
    fn context_omits_missing_neighbours_at_edges() {
        let p = Proj::new("edges");
        p.write("src/a.rs", "first\nneedle\n");
        p.write("src/b.rs", "needle\nlast\n");
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(
            out.contains(">2: needle"),
            "last-line hit keeps prev only:\n{out}"
        );
        assert!(
            out.contains(">1: needle"),
            "first-line hit keeps next only:\n{out}"
        );
        assert!(!out.contains("0:"), "no phantom line before line 1:\n{out}");
        assert!(
            !out.contains("3:"),
            "no phantom line after the last line:\n{out}"
        );
    }

    #[test]
    fn broad_results_are_single_line() {
        // Design C is adaptive: context only when the result set is narrow
        // (≤ CONTEXT_GROUP_CAP files). A 6-file match stays single-line — no '>'
        // marker and no neighbour lines — so broad searches keep their breadth.
        let p = Proj::new("broad_fmt");
        for i in 0..6 {
            p.write(
                &format!("src/f{i}.rs"),
                &format!("fn a{i}() {{}}\nneedle\nfn z{i}() {{}}\n"),
            );
        }
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(out.contains("  2: needle"), "single-line hit:\n{out}");
        assert!(
            !out.contains(">2: needle"),
            "no marker in broad mode:\n{out}"
        );
        assert!(
            !out.contains("  1: fn a0()"),
            "no context in broad mode:\n{out}"
        );
        assert!(
            !out.contains("  3: fn z0()"),
            "no next context in broad mode:\n{out}"
        );
    }

    #[test]
    fn broad_search_keeps_file_breadth() {
        // Adaptive keeps broad results single-line, so HIT_CAP stays at its
        // pre-context 80 and a dense search lists ~11 files rather than the ~4
        // that fixed ±1 context would force. 13 files × 6 hits = 7 lines each:
        // 11 fit under the cap (77) and the 12th does not (84).
        let p = Proj::new("breadth");
        for i in 0..13 {
            let mut body = String::from("fn pre() {}\n");
            for _ in 0..6 {
                body.push_str("needle\n");
            }
            body.push_str("fn post() {}\n");
            p.write(&format!("src/f{i:02}.rs"), &body);
        }
        let out = search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(
            out.contains("showing the top 11"),
            "breadth preserved:\n{out}"
        );
        assert!(
            out.contains("13 files"),
            "footer reports the true total:\n{out}"
        );
    }

    #[test]
    fn footer_reports_complete_when_all_shown() {
        let p = Proj::new("complete_footer");
        p.write("src/a.rs", "fn alpha() {}\n");
        p.write("src/b.rs", "fn alpha() {}\n");
        let out = search(r#"{"query":"alpha"}"#, p.0.as_path());
        assert!(
            out.contains("(all 2 hits across 2 files shown)"),
            "completeness footer for all hits:\n{out}"
        );
    }

    #[test]
    fn footer_reports_per_file_capping_when_individual_files_truncated() {
        let p = Proj::new("per_file_capping");
        let mut body = String::new();
        for _ in 0..10 {
            body.push_str("fn alpha() {}\n");
        }
        p.write("src/a.rs", &body);
        p.write("src/b.rs", "fn alpha() {}\n");
        let out = search(r#"{"query":"alpha"}"#, p.0.as_path());
        assert!(
            out.contains("(11 hits across 2 files; showing up to 6 hits per file)"),
            "footer for per-file capping:\n{out}"
        );
    }

    #[test]
    fn name_mode_reports_complete_when_all_files_shown() {
        let p = Proj::new("name_complete");
        p.write("src/config.rs", "// content\n");
        p.write("src/config_extra.rs", "// content\n");
        let out = search(r#"{"query":"name:config"}"#, p.0.as_path());
        assert!(
            out.contains("(all 2 matching files shown)"),
            "name mode completeness footer:\n{out}"
        );
    }

    #[test]
    fn multi_line_regex_matches_line_starts() {
        let p = Proj::new("multiline_regex");
        p.write("src/lib.rs", "// header\n// comment\npub async fn run_test() {\n    let x = 1;\n}\n");
        let out = search(r#"{"query":"^pub (async )?fn \\w+","regex":true}"#, p.0.as_path());
        assert!(out.contains("src/lib.rs (1 hit) [src]"), "hit found:\n{out}");
        assert!(out.contains(">3: pub async fn run_test() {"), "matched line 3:\n{out}");
    }

    #[test]
    fn path_filter_restricts_search() {
        let p = Proj::new("path_filter");
        p.write("src/tools/mod.rs", "fn target() {}\n");
        p.write("src/lib.rs", "fn target() {}\n");
        p.write("frontend/app.ts", "fn target() {}\n");

        // Restrict to directory
        let dir_out = search(r#"{"query":"target","path":"src/tools"}"#, p.0.as_path());
        assert!(dir_out.contains("src/tools/mod.rs"), "tools match:\n{dir_out}");
        assert!(!dir_out.contains("src/lib.rs"), "lib.rs excluded:\n{dir_out}");
        assert!(!dir_out.contains("frontend/app.ts"), "frontend excluded:\n{dir_out}");

        // Restrict to single file
        let file_out = search(r#"{"query":"target","path":"src/lib.rs"}"#, p.0.as_path());
        assert!(file_out.contains("src/lib.rs"), "lib match:\n{file_out}");
        assert!(!file_out.contains("src/tools/mod.rs"), "tools excluded:\n{file_out}");

        // Restrict using absolute path (model passing full project path)
        let abs_path = p.0.join("src").join("tools").to_string_lossy().to_string();
        let abs_arg = serde_json::json!({"query": "target", "path": abs_path}).to_string();
        let abs_out = search(&abs_arg, p.0.as_path());
        assert!(abs_out.contains("src/tools/mod.rs"), "abs path match:\n{abs_out}");

        // Auto-detect regex with pipe alternation (without regex: true)
        let auto_regex_out = search(r#"{"query":"target|nothing_here"}"#, p.0.as_path());
        assert!(auto_regex_out.contains("src/tools/mod.rs"), "auto-regex pipe match:\n{auto_regex_out}");
    }

    #[test]
    fn empty_query_with_path_or_glob_lists_files() {
        let p = Proj::new("empty_query_listing");
        p.write("src/tools/mod.rs", "content\n");
        p.write("src/lib.rs", "content\n");
        p.write("docs/readme.md", "content\n");

        let glob_out = search(r#"{"glob":"src/**/*.rs"}"#, p.0.as_path());
        assert!(glob_out.contains("src/tools/mod.rs [src]"), "glob list tools:\n{glob_out}");
        assert!(glob_out.contains("src/lib.rs [src]"), "glob list lib:\n{glob_out}");
        assert!(!glob_out.contains("docs/readme.md"), "docs excluded:\n{glob_out}");

        let path_out = search(r#"{"path":"docs"}"#, p.0.as_path());
        assert!(path_out.contains("docs/readme.md [doc]"), "path list docs:\n{path_out}");
        assert!(!path_out.contains("src/lib.rs"), "src excluded:\n{path_out}");

        // Empty query without path or glob gives a clear error
        let err_out = search(r#"{}"#, p.0.as_path());
        assert!(err_out.starts_with("error: project_search requires a 'query' string"), "clear error:\n{err_out}");
    }

    #[test]
    fn all_mode_and_single_file_target_bypass_six_hit_cap() {
        let p = Proj::new("enumeration");
        let mut body = String::new();
        for i in 1..=14 {
            body.push_str(&format!("pub fn gate_{i}() {{}}\n"));
        }
        p.write("src/gates.rs", &body);
        p.write("src/other.rs", "pub fn gate_other() {}\n");

        // 1. Without all or single-file path: multi-file search caps at 6 hits per file
        let default_out = search(r#"{"query":"gate_"}"#, p.0.as_path());
        assert!(default_out.contains("(15 hits across 2 files; showing up to 6 hits per file)"), "default caps at 6:\n{default_out}");

        // 2. With all: true: shows all 14 + 1 hits
        let all_out = search(r#"{"query":"gate_","all":true}"#, p.0.as_path());
        assert!(all_out.contains("(all 15 hits across 2 files shown)"), "all:true shows all 15:\n{all_out}");
        assert!(all_out.contains("gate_14"), "last hit present:\n{all_out}");

        // 3. With path to a single file: automatically shows all 14 hits without cap
        let file_target_out = search(r#"{"query":"gate_","path":"src/gates.rs"}"#, p.0.as_path());
        assert!(file_target_out.contains("(all 14 hits across 1 file shown)"), "single file target shows all 14:\n{file_target_out}");
        assert!(file_target_out.contains("gate_14"), "last hit present in single file target:\n{file_target_out}");
    }

    #[test]
    fn single_matched_file_in_multi_file_search_auto_expands() {
        let p = Proj::new("single_file_auto_expand");
        let mut body = String::new();
        for i in 1..=14 {
            body.push_str(&format!("pub fn gate_{i}() {{}}\n"));
        }
        p.write("src/gates.rs", &body);
        p.write("src/clean.rs", "pub fn clean_code() {}\n");

        // Broad glob matching both files, but only src/gates.rs has hits -> auto-expands to all 14 hits
        let out = search(r#"{"query":"gate_","glob":"src/**/*.rs"}"#, p.0.as_path());
        assert!(out.contains("(all 14 hits across 1 file shown)"), "single matched file auto-expands:\n{out}");
        assert!(out.contains("gate_14"), "all hits rendered:\n{out}");
    }
}



#[cfg(test)]
mod project_tests {
    use super::*;

    #[tokio::test]
    async fn project_info_reports_entry_points_and_git_truth() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_pinfo_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("src")).unwrap();
        std::fs::write(temp_dir.join("Cargo.toml"), "[package]\nname=\"foo\"\n").unwrap();
        std::fs::write(temp_dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(temp_dir.join("src").join("App.tsx"), "export function App() {}\n").unwrap();

        let out = Tools::run_project_info(&temp_dir).await;
        assert!(out.contains("Looks like: Rust (Cargo.toml)"), "out:\n{out}");
        assert!(out.contains("Entry points: src/App.tsx, src/main.rs"), "out:\n{out}");
        assert!(out.contains("Version control: none (session snapshots active — use get_diff to inspect diffs and restore_file to undo)"), "out:\n{out}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn project_info_profiles_layout_and_workspaces() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_pinfo_workspace_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("crates").join("core")).unwrap();
        std::fs::create_dir_all(temp_dir.join("frontend").join("src")).unwrap();
        std::fs::create_dir_all(temp_dir.join("tests")).unwrap();
        std::fs::write(temp_dir.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/*\"]\n").unwrap();
        std::fs::write(temp_dir.join("package.json"), "{\n  \"name\": \"root\",\n  \"workspaces\": [\"frontend\"]\n}\n").unwrap();
        std::fs::write(temp_dir.join("crates").join("core").join("lib.rs"), "pub fn run() {}\n").unwrap();
        std::fs::write(temp_dir.join("frontend").join("src").join("index.tsx"), "export const x = 1;\n").unwrap();
        std::fs::write(temp_dir.join("frontend").join("src").join("style.css"), "body {}\n").unwrap();
        std::fs::write(temp_dir.join("tests").join("test_basic.rs"), "fn t() {}\n").unwrap();

        let out = Tools::run_project_info(&temp_dir).await;
        assert!(out.contains("Workspace: Cargo workspace · npm/yarn workspace"), "workspaces detected:\n{out}");
        assert!(out.contains("Root files: Cargo.toml, package.json"), "root files listed:\n{out}");
        assert!(out.contains("crates/ (1 files: .rs)"), "crates profile:\n{out}");
        assert!(out.contains("frontend/ (2 files: ") && out.contains(".tsx") && out.contains(".css"), "frontend profile:\n{out}");
        assert!(out.contains("tests/ (1 files: .rs)"), "tests profile:\n{out}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn walk_project_bfs_depth_and_trailing_slashes() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_bfs_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("src").join("components")).unwrap();
        std::fs::create_dir_all(temp_dir.join("docs")).unwrap();
        std::fs::create_dir_all(temp_dir.join(".git")).unwrap();
        std::fs::write(temp_dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(temp_dir.join("README.md"), "").unwrap();
        std::fs::write(temp_dir.join("src").join("main.rs"), "").unwrap();
        std::fs::write(temp_dir.join("src").join("components").join("Button.tsx"), "").unwrap();
        std::fs::write(temp_dir.join("docs").join("guide.md"), "").unwrap();
        std::fs::write(temp_dir.join(".git").join("config"), "").unwrap();

        // Depth 1: immediate children only (files + directories with trailing slashes)
        let (d1, capped) = Tools::walk_project_bfs(&temp_dir, &temp_dir, Some(1), 500);
        assert!(!capped);
        assert_eq!(d1, vec![
            "Cargo.toml".to_string(),
            "README.md".to_string(),
            "docs/".to_string(),
            "src/".to_string(),
        ]);

        // Depth 2: down to depth 2
        let (d2, _) = Tools::walk_project_bfs(&temp_dir, &temp_dir, Some(2), 500);
        assert_eq!(d2, vec![
            "Cargo.toml".to_string(),
            "README.md".to_string(),
            "docs/".to_string(),
            "docs/guide.md".to_string(),
            "src/".to_string(),
            "src/components/".to_string(),
            "src/main.rs".to_string(),
        ]);

        // Unbounded depth: includes src/components/Button.tsx
        let (d_all, _) = Tools::walk_project_bfs(&temp_dir, &temp_dir, None, 500);
        assert!(d_all.contains(&"src/components/Button.tsx".to_string()));
        // .git must not be traversed or listed
        assert!(!d_all.iter().any(|p| p.starts_with(".git")));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn project_list_truncation_with_actionable_hint() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_plist_cap_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("bulk")).unwrap();
        for i in 0..10 {
            std::fs::write(temp_dir.join("bulk").join(format!("file_{i:02}.txt")), "").unwrap();
        }

        // Test walk_project_bfs cap
        let (capped_entries, capped) = Tools::walk_project_bfs(&temp_dir, &temp_dir, None, 5);
        assert!(capped);
        assert_eq!(capped_entries.len(), 5);

        // Test run_project_list with path parameter
        let sub_out = Tools::run_project_list(r#"{"path": "bulk", "depth": 1}"#, Some(&temp_dir));
        assert!(sub_out.contains("bulk/file_00.txt"), "sub_out:\n{sub_out}");
        assert!(!sub_out.contains("bulk/ ("), "must not add metadata to paths:\n{sub_out}");

        // Test run_project_list with empty directory
        let empty_dir = temp_dir.join("empty_folder");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let empty_out = Tools::run_project_list(r#"{"path": "empty_folder"}"#, Some(&temp_dir));
        assert_eq!(empty_out, "(empty directory)");

        // Test error handling
        let err_out = Tools::run_project_list(r#"{"path": "nonexistent"}"#, Some(&temp_dir));
        assert!(err_out.starts_with("error: 'nonexistent' is not a directory"), "err_out:\n{err_out}");

        let no_proj = Tools::run_project_list("{}", None);
        assert!(no_proj.starts_with("error: no project is open"), "no_proj:\n{no_proj}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_project_path_rejects_git_directory() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_git_guard_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join(".git").join("hooks")).unwrap();

        assert!(Tools::project_path(&temp_dir, ".git").is_err());
        assert!(Tools::project_path(&temp_dir, ".git/hooks/pre-commit").is_err());
        assert!(Tools::project_path(&temp_dir, r".git\config").is_err());
        assert!(Tools::project_path(&temp_dir, "sub/.git/config").is_err());

        // But .gitignore and .gitattributes must remain valid!
        assert!(Tools::project_path(&temp_dir, ".gitignore").is_ok());
        assert!(Tools::project_path(&temp_dir, ".gitattributes").is_ok());

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_walk_project_skips_symlinks_and_junctions() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_walk_sym_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let outside_dir = std::env::temp_dir().join(format!(
            "outpost_walk_outside_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("src")).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(temp_dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(outside_dir.join("secret.txt"), "secret\n").unwrap();

        // Create junction / symlink inside temp_dir pointing to outside_dir
        let link = temp_dir.join("ext");
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside_dir)
            .output();
        #[cfg(not(windows))]
        let _ = std::os::unix::fs::symlink(&outside_dir, &link);

        let mut files = Vec::new();
        Tools::walk_project(&temp_dir, &temp_dir, &mut files, 500);

        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(
            !files.iter().any(|f| f.contains("secret.txt")),
            "walk_project must not enter linked directory: {files:?}"
        );

        if link.exists() {
            let _ = std::fs::remove_dir_all(&link);
        }
        let _ = std::fs::remove_dir_all(temp_dir);
        let _ = std::fs::remove_dir_all(outside_dir);
    }

    #[tokio::test]
    async fn test_typecheck_detects_python_project() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_py_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("main.py"),
            "def add(a: int, b: int) -> int:\n    return a + b\n",
        )
        .unwrap();

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("compileall"),
            "typecheck should run compileall for Python projects, got:\n{out}"
        );
        assert!(
            out.contains("exit code: 0"),
            "clean python file should exit 0, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_catches_python_syntax_error() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_py_err_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("broken.py"),
            "def bad_syntax(:\n    pass\n",
        )
        .unwrap();

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("compileall"),
            "typecheck should run compileall, got:\n{out}"
        );
        assert!(
            out.contains("SyntaxError") || out.contains("exit code: 1"),
            "typecheck should report syntax error, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_detects_nested_tauri_crate() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_tauri_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("src-tauri")).unwrap();
        std::fs::write(
            temp_dir.join("src-tauri/Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        // No root-level Cargo.toml — the src-tauri/ layout must be detected.
        assert!(!temp_dir.join("Cargo.toml").exists());

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("cargo check"),
            "typecheck should detect nested src-tauri/Cargo.toml and run cargo check, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_detects_go_project() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_go_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("go.mod"),
            "module test\n\ngo 1.21\n",
        )
        .unwrap();

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("go vet"),
            "typecheck should detect go.mod and run go vet, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_detects_dotnet_project() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_dotnet_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("Test.sln"),
            "",
        )
        .unwrap();

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("dotnet build"),
            "typecheck should detect .sln and run dotnet build, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_detects_subdirectory_frontend() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_subdir_frontend_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("frontend")).unwrap();
        std::fs::write(
            temp_dir.join("frontend/tsconfig.json"),
            "{}",
        )
        .unwrap();
        // No root-level tsconfig.json — the frontend/ subdirectory must be detected.
        assert!(!temp_dir.join("tsconfig.json").exists());

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("tsc --noEmit"),
            "typecheck should detect frontend/tsconfig.json and run tsc, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn test_typecheck_polyglot_runs_all_matching() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_polyglot_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("src-tauri")).unwrap();
        std::fs::write(temp_dir.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(
            temp_dir.join("src-tauri/Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let cancel = CancellationToken::new();
        let out = Tools::run_typecheck("", Some(&temp_dir), &cancel).await;

        assert!(
            out.contains("cargo check"),
            "polyglot typecheck should run cargo check, got:\n{out}"
        );
        assert!(
            out.contains("tsc --noEmit"),
            "polyglot typecheck should run tsc --noEmit, got:\n{out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_expand_braces() {
        assert_eq!(
            Tools::expand_braces("{Cargo.toml,tauri.conf.json}"),
            vec!["Cargo.toml", "tauri.conf.json"]
        );
        assert_eq!(
            Tools::expand_braces("*.{ts,tsx}"),
            vec!["*.ts", "*.tsx"]
        );
        assert_eq!(
            Tools::expand_braces("src/{components,utils}/**/*.{ts,tsx}"),
            vec![
                "src/components/**/*.ts",
                "src/components/**/*.tsx",
                "src/utils/**/*.ts",
                "src/utils/**/*.tsx",
            ]
        );
        assert_eq!(
            Tools::expand_braces("src/*.rs"),
            vec!["src/*.rs"]
        );
        assert_eq!(
            Tools::expand_braces("{no_comma}"),
            vec!["{no_comma}"]
        );
        assert_eq!(
            Tools::expand_braces("{}"),
            vec!["{}"]
        );
    }

    #[tokio::test]
    async fn test_typecheck_targeted_filtering() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_typecheck_targeted_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("src-tauri")).unwrap();
        std::fs::write(temp_dir.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(
            temp_dir.join("src-tauri/Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let cancel = CancellationToken::new();

        // 1. Target TypeScript only
        let ts_out = Tools::run_typecheck(r#"{"target": "ts"}"#, Some(&temp_dir), &cancel).await;
        assert!(ts_out.contains("tsc --noEmit"), "should run tsc: {ts_out}");
        assert!(!ts_out.contains("cargo check"), "must not run cargo check: {ts_out}");

        // 2. Target Rust only
        let rust_out = Tools::run_typecheck(r#"{"target": "rust"}"#, Some(&temp_dir), &cancel).await;
        assert!(rust_out.contains("cargo check"), "should run cargo check: {rust_out}");
        assert!(!rust_out.contains("tsc --noEmit"), "must not run tsc: {rust_out}");

        // 3. Target unknown/unconfigured language (e.g. python in rust+ts project)
        let unk_out = Tools::run_typecheck(r#"{"target": "python"}"#, Some(&temp_dir), &cancel).await;
        assert!(
            unk_out.contains("target 'python' not found in detected toolchains"),
            "should report unknown target: {unk_out}"
        );
        assert!(
            unk_out.contains("Rust") && unk_out.contains("TypeScript"),
            "should list available toolchains: {unk_out}"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_project_search_brace_expansion() {
        let temp_dir = std::env::temp_dir().join(format!(
            "outpost_search_brace_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("Cargo.toml"), "[package]\nname = \"target_app\"\n").unwrap();
        std::fs::write(temp_dir.join("tauri.conf.json"), "{\n  \"productName\": \"target_app\"\n}\n").unwrap();
        std::fs::write(temp_dir.join("other.txt"), "target_app\n").unwrap();

        // Filename search with brace glob
        let name_res = Tools::run_project_search(
            r#"{"glob": "{Cargo.toml,tauri.conf.json}"}"#,
            &temp_dir,
        );
        assert!(name_res.contains("Cargo.toml"), "name_res should have Cargo.toml: {name_res}");
        assert!(name_res.contains("tauri.conf.json"), "name_res should have tauri.conf.json: {name_res}");
        assert!(!name_res.contains("other.txt"), "name_res should not have other.txt: {name_res}");

        // Content search with brace glob
        let content_res = Tools::run_project_search(
            r#"{"query": "target_app", "glob": "{Cargo.toml,tauri.conf.json}"}"#,
            &temp_dir,
        );
        assert!(content_res.contains("Cargo.toml"), "content_res should have Cargo.toml: {content_res}");
        assert!(content_res.contains("tauri.conf.json"), "content_res should have tauri.conf.json: {content_res}");
        assert!(!content_res.contains("other.txt"), "content_res should not have other.txt: {content_res}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn syntax_warning_detects_rust_syntax_error_in_memory() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_syntax_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file = temp_dir.join("main.rs");
        std::fs::write(&file, "fn main() {\n    let x = ;\n}\n").unwrap();

        let warn = syntax_warning(&file).await;
        assert!(warn.is_some(), "should catch syntax error via Tree-Sitter");
        let (lang, msg) = warn.unwrap();
        assert_eq!(lang, "Rust");
        assert!(msg.contains("line 2"));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn syntax_warning_passes_valid_code_cleanly() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_syntax_valid_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file = temp_dir.join("valid.ts");
        std::fs::write(&file, "export const add = (a: number, b: number): number => a + b;\n").unwrap();

        let warn = syntax_warning(&file).await;
        assert_eq!(warn, None);

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn append_syntax_warning_formats_model_notification() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_syntax_append_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file = temp_dir.join("script.py");
        std::fs::write(&file, "def foo(\n    pass\n").unwrap();

        let base_msg = "Edited 'script.py'.".to_string();
        let appended = append_syntax_warning(base_msg, &file, "script.py").await;
        assert!(appended.contains("Heads up: 'script.py' no longer parses as Python"));
        assert!(appended.contains("line 1"));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn format_project_changes_redistributes_unused_budget_to_larger_files() {
        let mut bodies = Vec::new();
        // 8 small files of 200 chars each = 1,600 chars
        for i in 1..=8 {
            bodies.push((format!("file{i}.rs"), "x".repeat(200)));
        }
        // 1 large file of 5,000 chars. Total = 6,600 chars <= 10,000 max_chars.
        bodies.push(("large.rs".to_string(), "y".repeat(5000)));

        let out = Tools::format_project_changes(&bodies, 10_000);
        // Under naive division (10,000 / 9 = 1,111), large.rs would be truncated at 1,111 chars.
        // With fair-share redistribution, large.rs gets the remaining budget and is NOT truncated.
        assert!(!out.contains("[content truncated"), "large file should not be truncated when total fits budget:\n{out}");
        assert!(out.contains("=== large.rs ==="));
        assert!(out.contains(&"y".repeat(5000)));
    }

    #[test]
    fn format_project_changes_truncates_fairly_when_over_budget() {
        let mut bodies = Vec::new();
        for i in 1..=5 {
            bodies.push((format!("file{i}.rs"), "z".repeat(5000)));
        }
        // Total 25,000 chars with max_chars = 10,000. Each file should get ~2,000 chars.
        let out = Tools::format_project_changes(&bodies, 10_000);
        assert!(out.contains("[content truncated at 2000 chars"), "files should be capped at fair share:\n{out}");
        assert!(out.contains("… (truncated — pass a larger max_chars"));
        assert!(out.chars().count() <= 10_000 + 300); // respects max_chars boundary + headers
    }

    #[test]
    fn project_read_and_read_lines_reject_directory() {
        let temp_dir = std::env::temp_dir().join(format!("outpost_dir_reject_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(temp_dir.join("subfolder")).unwrap();

        let read_out = Tools::run_project_read(&serde_json::json!({ "path": "subfolder" }).to_string(), &temp_dir);
        assert!(read_out.contains("is a directory"), "project_read must report directory: {read_out}");

        let lines_out = Tools::run_project_read_lines(&serde_json::json!({ "path": "subfolder" }).to_string(), &temp_dir);
        assert!(lines_out.contains("is a directory"), "project_read_lines must report directory: {lines_out}");

        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
