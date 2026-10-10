// ── surface: backend ──────────────────────────────────
// Note and document management tools: md_write, html_write, md_edit, NoteInfo.
// Exports: NoteInfo, open_in_default_app, Tools::md_write, etc.

use std::path::{Path, PathBuf};
use serde_json::Value;
use tauri::ipc::Channel;
use crate::provider::StreamEvent;
use super::*;

impl Tools {
    pub(crate) async fn md_write(
        &self,
        arguments: &str,
        context_files: &[String],
        on_event: &Channel<StreamEvent>,
    ) -> String {
        if let Some(msg) = empty_args_error(arguments, "md_write", "'path' and 'content'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let content = parsed.as_ref().and_then(|v| arg_str(v, CONTENT_ALIASES));
        let (Some(path), Some(content)) = (path, content) else {
            eprintln!(
                "[md_write] BAD ARGS ({} bytes): {}",
                arguments.len(),
                arguments.chars().take(200).collect::<String>()
            );
            return "error: md_write requires 'path' and 'content' string arguments".into();
        };
        eprintln!("[md_write] path={path:?}, content={} bytes", content.len());

        // Sandbox to the notes root (see `sandbox::contain` — a lexical
        // is_absolute/`..` check is not enough on Windows). `rel` below only ever
        // appends to the validated filename, so it inherits containment.
        let target = match crate::sandbox::contain(&self.notes_root, path) {
            Ok(p) => p,
            Err(why) => {
                return format!(
                "error: path must be relative and within the notes folder (got '{path}' — {why})"
            )
            }
        };

        // Collision protection: if the target exists but isn't a document the model
        // is aware of (in context), a same-name write is likely an accidental clash
        // — write to a numbered variant instead of clobbering the existing file.
        let rel = if target.exists() && !context_files.iter().any(|f| f == path) {
            next_available_name(&self.notes_root, path)
        } else {
            path.to_string()
        };
        let full = self.notes_root.join(&rel);

        let current = std::fs::read_to_string(&full).unwrap_or_default();
        if current == content {
            return format!("'{rel}' already has this exact content; nothing to write.");
        }
        let diff = unified_diff(&current, content);

        // Pause for the user's approval (spec §5 — diff-then-approve).
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        eprintln!(
            "[md_write] sending approval id={id} for '{rel}' (diff {} bytes); awaiting user…",
            diff.len()
        );
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: "md_write".into(),
                path: rel.clone(),
                diff,
                warning: None,
                command: None,
            })
            .is_err()
        {
            eprintln!("[md_write] approval send FAILED — UI channel closed");
            return format!("could not request approval for '{rel}' (UI channel closed)");
        }
        let decision = rx.await.unwrap_or_default();
        eprintln!("[md_write] id={id} decision={decision}");
        if decision != "approve" {
            return rejection_response(
                &decision,
                &format!("The user rejected the write to '{rel}'. No changes were made."),
            );
        }

        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&full, content) {
            Ok(()) if rel != path => format!(
                "Wrote '{rel}' ({} bytes). Renamed from '{path}' to avoid overwriting an existing file.",
                content.len()
            ),
            Ok(()) => format!("Wrote '{rel}' ({} bytes).", content.len()),
            Err(e) => format!("write error: {e}"),
        }
    }

    pub(crate) async fn md_edit(&self, arguments: &str, on_event: &Channel<StreamEvent>) -> String {
        self.apply_str_edit(arguments, "md_edit", on_event).await
    }

    pub(crate) async fn html_edit(&self, arguments: &str, on_event: &Channel<StreamEvent>) -> String {
        self.apply_str_edit(arguments, "html_edit", on_event).await
    }

    /// Shared targeted-edit logic for md_edit and html_edit: replace one unique
    /// snippet in an existing file — exact first, then whitespace-insensitive —
    /// diff-then-approve, write. The UI handles opening an edited page in the
    /// browser (deferred to end-of-turn).
    pub(crate) async fn apply_str_edit(
        &self,
        arguments: &str,
        tool: &str,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        if let Some(msg) =
            empty_args_error(arguments, tool, "'path', 'old_string', and 'new_string'")
        {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let old = parsed.as_ref().and_then(|v| arg_str(v, OLD_ALIASES));
        let new = parsed.as_ref().and_then(|v| arg_str(v, NEW_ALIASES));
        let (Some(path), Some(old), Some(new)) = (path, old, new) else {
            return format!(
                "error: {tool} requires 'path', 'old_string', and 'new_string' string arguments"
            );
        };
        if old.is_empty() {
            return "error: old_string must not be empty".into();
        }

        // Sandbox to the notes root (see `sandbox::contain`).
        let full = match crate::sandbox::contain(&self.notes_root, path) {
            Ok(p) => p,
            Err(why) => {
                return format!(
                "error: path must be relative and within the notes folder (got '{path}' — {why})"
            )
            }
        };

        let current = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => {
                return format!("error: '{path}' doesn't exist or can't be read — create it first")
            }
        };
        let (updated, matched) = resolve_snippet_edit(&current, &old, &new);
        let matched_fuzzy = match matched {
            SnippetMatch::Exact => false,
            SnippetMatch::Fuzzy => true,
            SnippetMatch::Ambiguous(n) => {
                return format!(
                    "error: old_string appears {n} times in '{path}' (counting whitespace-insensitive matches) — include more surrounding context so it matches exactly once, or use {} to rewrite the file.",
                    rewrite_tool_for(tool)
                );
            }
            SnippetMatch::NotFound(_) => {
                return format!(
                    "error: old_string was not found in '{path}' — not even ignoring whitespace (indentation, trailing spaces, line endings, blank lines). read_note the file and check its current text; if the block genuinely differs, use {} to rewrite the file.",
                    rewrite_tool_for(tool)
                );
            }
        };
        if updated == current {
            return format!("'{path}' is unchanged; nothing to write.");
        }
        let diff = unified_diff(&current, &updated);

        // Pause for the user's approval (spec §5 — diff-then-approve).
        // WHY: Notes vault writes (str_replace_note, md_write, html_write) unconditionally
        // require user approval rather than honoring `auto_project_writes`. Project files
        // have safety snapshots and rollback (guard_snapshot / project_restore), but notes
        // do not; user approval is the primary safety gate protecting notes from corruption.
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: tool.to_string(),
                path: path.to_string(),
                diff,
                warning: None,
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
        if let Err(e) = std::fs::write(&full, &updated) {
            return format!("write error: {e}");
        }
        let note = if matched_fuzzy {
            " (matched with whitespace-insensitive matching — indentation/spacing in the file may differ from your old_string)"
        } else {
            ""
        };
        format!("Edited '{path}'.{note}")
    }

    /// Create or overwrite an HTML file in the notes folder and open it in the
    /// user's default browser. Diff-then-approve like md_write, but with no
    /// collision rename — the approval gate covers intended overwrites, which
    /// keeps iterating on one page clean. Ensures an `.html` extension so the OS
    /// opens it in a browser.
    pub(crate) async fn html_write(
        &self,
        arguments: &str,
        seen_paths: &[String],
        on_event: &Channel<StreamEvent>,
    ) -> String {
        if let Some(msg) = empty_args_error(arguments, "html_write", "'path' and 'content'") {
            return msg;
        }
        let parsed = serde_json::from_str::<Value>(arguments).ok();
        let path = parsed.as_ref().and_then(|v| arg_str(v, PATH_ALIASES));
        let content = parsed.as_ref().and_then(|v| arg_str(v, CONTENT_ALIASES));
        let (Some(path), Some(content)) = (path, content) else {
            return "error: html_write requires 'path' and 'content' string arguments".into();
        };

        // Sandbox to the notes root (see `sandbox::contain`). `rel` below only
        // appends an extension to the validated name, so it stays contained.
        if let Err(why) = crate::sandbox::contain(&self.notes_root, path) {
            return format!(
                "error: path must be relative and within the notes folder (got '{path}' — {why})"
            );
        }

        // Ensure an .html extension so the OS opens it in a browser on launch.
        let lower = path.to_ascii_lowercase();
        let rel = if lower.ends_with(".html") || lower.ends_with(".htm") {
            path.to_string()
        } else {
            format!("{path}.html")
        };
        let full = self.notes_root.join(&rel);

        // Warn (don't silently rename) when overwriting a page the model hasn't
        // read or written this turn — likely an accidental clash with a different
        // page (models reuse generic names like 'index.html'). Renaming behind the
        // model's back desyncs its idea of the filename from the real one, so the
        // user decides via the approval and the model keeps a true record.
        let touched = seen_paths.iter().any(|p| p == &rel || p == path);
        let warning = if full.exists() && !touched {
            Some(format!(
                "'{rel}' already exists. Overwrite to replace it, or Save as new to keep it — the assistant will then save this page under a different name. Reject to cancel."
            ))
        } else {
            None
        };

        let current = std::fs::read_to_string(&full).unwrap_or_default();
        if current == content {
            return format!("'{rel}' already has this exact content; nothing to write.");
        }
        let diff = unified_diff(&current, content);

        // Pause for the user's approval (diff-then-approve, same as md_write).
        let id = self.approvals.next_id();
        let rx = self.approvals.register(id);
        if on_event
            .send(StreamEvent::ToolApproval {
                id,
                tool: "html_write".into(),
                path: rel.clone(),
                diff,
                warning,
                command: None,
            })
            .is_err()
        {
            return format!("could not request approval for '{rel}' (UI channel closed)");
        }
        let decision = rx.await.unwrap_or_default();
        match decision.as_str() {
            "approve" => {}
            "rename" => {
                return format!(
                    "'{rel}' already exists and the user chose to keep it. Save this page under a DIFFERENT filename — call html_write again with a new name (do not reuse '{rel}')."
                );
            }
            _ => {
                return rejection_response(
                    &decision,
                    &format!("The user rejected the write to '{rel}'. No changes were made."),
                );
            }
        }

        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&full, content) {
            return format!("write error: {e}");
        }
        format!("Wrote '{rel}' ({} bytes).", content.len())
    }

    pub(crate) fn search_notes(&self, arguments: &str) -> String {
        self.search_files(arguments, &["md"], "search_notes", "note")
    }

    pub(crate) fn search_pages(&self, arguments: &str) -> String {
        self.search_files(arguments, &["html", "htm"], "search_pages", "page")
    }

    /// Agentic library — search: BM25 + filename/title boosted search across the user's
    /// files of the given extensions, returning ranked matches with contextual windowed
    /// snippets so the model can pick one to read. Read-only. `tool`/`noun` shape the messages.
    pub(crate) fn search_files(&self, arguments: &str, exts: &[&str], tool: &str, noun: &str) -> String {
        let query = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["query"].as_str().map(str::to_string));
        let Some(query) = query else {
            return format!("error: {tool} requires a 'query' string argument");
        };
        let query = query.trim();
        if query.is_empty() {
            return format!("error: {tool} requires a non-empty 'query'");
        }

        let files = self.list_with_exts(exts);
        let total = files.len();

        let mut docs: Vec<(String, String)> = Vec::with_capacity(total);
        for f in &files {
            if let Ok(content) = self.read_note(&f.name) {
                docs.push((f.name.clone(), content));
            }
        }

        let hits = notes_search::rank_and_snippet(query, &docs);
        if hits.is_empty() {
            return format!("No {noun}s matched \"{query}\". (Searched {total}.)");
        }

        // Best matches first; cap so a big library can't flood a small context.
        let shown = hits.len().min(8);
        let mut out = format!(
            "Found {} {noun}(s) matching \"{query}\" (showing {shown}). Use read_note with a path to open one in full:\n",
            hits.len()
        );
        for hit in hits.iter().take(shown) {
            out.push_str(&format!("\n• {}\n", hit.path));
            for snippet in &hit.snippets {
                out.push_str(&format!("    {snippet}\n"));
            }
        }
        out.trim_end().to_string()
    }

    /// Agentic library — read: return a note's full text for the model (sandboxed,
    /// capped like a fetched page so an oversized note can't blow a small context).
    pub(crate) fn read_note_tool(&self, arguments: &str) -> String {
        let path = serde_json::from_str::<Value>(arguments)
            .ok()
            .as_ref()
            .and_then(|v| arg_str(v, PATH_ALIASES).map(str::to_string));
        let Some(path) = path else {
            return "error: read_note requires a 'path' string argument".into();
        };
        let max_chars = parse_max_chars(arguments, MAX_NOTE_CHARS);
        match self.read_note(&path) {
            Ok(content) if content.trim().is_empty() => format!("'{path}' is empty."),
            Ok(content) if content.chars().count() > max_chars => {
                let truncated: String = content.chars().take(max_chars).collect();
                format!(
                    "{truncated}\n\n[content truncated at {max_chars} characters — pass max_chars to raise the cap (max {MAX_CHARS_CEILING})]"
                )
            }
            Ok(content) => content,
            Err(e) => format!("error reading '{path}': {e}"),
        }
    }

    /// User-triggered export of assembled markdown into the notes folder. Unlike
    /// `md_write` there's no model/approval round-trip — the user asked for it
    /// directly — but it's sandboxed the same way (relative path, no traversal).
    /// Returns the absolute path written, for the UI confirmation.
    pub fn export_markdown(&self, name: &str, content: &str) -> Result<PathBuf, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a file name is required".into());
        }
        // Sandbox to the notes root (see `sandbox::contain`). The extension added
        // below only appends to the validated name, so it stays contained.
        if let Err(why) = crate::sandbox::contain(&self.notes_root, name) {
            return Err(format!(
                "name must be relative and within the notes folder (got '{name}' — {why})"
            ));
        }
        // Default to .md only when no extension was given; keep an explicit one
        // (e.g. .html) so saving a page from the viewer doesn't become .html.md.
        let name = if Path::new(name).extension().is_some() {
            name.to_string()
        } else {
            format!("{name}.md")
        };
        let full = self.notes_root.join(&name);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&full, content).map_err(|e| e.to_string())?;
        Ok(full)
    }

    /// List files under the notes folder matching any of `exts`, newest first.
    fn list_with_exts(&self, exts: &[&str]) -> Vec<NoteInfo> {
        let mut out = Vec::new();
        collect_files(&self.notes_root, &self.notes_root, exts, &mut out);
        out.sort_by(|a, b| b.modified.cmp(&a.modified));
        out
    }

    /// List the markdown notes under the notes folder, newest first.
    pub fn list_notes(&self) -> Vec<NoteInfo> {
        self.list_with_exts(&["md"])
    }

    /// List the HTML pages under the notes folder, newest first.
    pub fn list_pages(&self) -> Vec<NoteInfo> {
        self.list_with_exts(&["html", "htm"])
    }

    /// Open a saved file with the OS default handler (a browser, for pages).
    pub fn open_in_browser(&self, name: &str) -> Result<(), String> {
        let full = self.note_full_path(name)?;
        open_in_default_app(&full)
    }

    /// Resolve a note's relative path to its absolute path (sandboxed).
    pub fn note_full_path(&self, name: &str) -> Result<PathBuf, String> {
        let trimmed = name.trim();
        let clean = trimmed.trim_start_matches(['/', '\\']);
        crate::sandbox::contain(&self.notes_root, clean)
            .map_err(|why| format!("invalid note path '{}' — {why}", trimmed))
    }

    /// Delete a note from the notes folder (sandboxed).
    pub fn delete_note(&self, name: &str) -> Result<(), String> {
        let full = self.note_full_path(name)?;
        std::fs::remove_file(&full).map_err(|e| e.to_string())
    }

    /// Rename a note within the notes folder (sandboxed). Ensures a `.md`
    /// extension and fails if the target name already exists. Returns the final
    /// name written.
    pub fn rename_note(&self, old: &str, new: &str) -> Result<String, String> {
        let new = new.trim();
        // Keep the original file's extension when the new name omits one, so
        // renaming a page (.html) doesn't silently turn it into a .md file.
        let new = if Path::new(new).extension().is_some() {
            new.to_string()
        } else {
            let ext = Path::new(old)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("md");
            format!("{new}.{ext}")
        };
        let old_full = self.note_full_path(old)?;
        let new_full = self.note_full_path(&new)?;
        if new_full.exists() {
            return Err(format!("'{new}' already exists"));
        }
        if let Some(parent) = new_full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&old_full, &new_full).map_err(|e| e.to_string())?;
        Ok(new)
    }

    /// Read one markdown note by its relative path (sandboxed to the notes root).
    pub fn read_note(&self, name: &str) -> Result<String, String> {
        let full = self.note_full_path(name)?;
        read_text_capped(&full)
    }


}

pub(crate) fn open_in_default_app(path: &Path) -> Result<(), String> {
    // Belt and braces alongside the sandbox's own check: never hand the OS a
    // path for a file that isn't there. The injection above only reached the
    // shell because a failed write still produced a path to "open".
    if !path.exists() {
        return Err(format!("'{}' doesn't exist", path.display()));
    }
    tauri_plugin_opener::open_path(path.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Given a relative path that already exists, find a free numbered variant —
/// `recipe.md` → `recipe-2.md` → `recipe-3.md` … preserving directory + extension.
fn next_available_name(root: &Path, rel: &str) -> String {
    let p = Path::new(rel);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("md");
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let parent = p
        .parent()
        .filter(|par| !par.as_os_str().is_empty())
        .map(|par| par.to_string_lossy().replace('\\', "/"));
    for n in 2..1000 {
        let name = format!("{stem}-{n}.{ext}");
        let candidate = match &parent {
            Some(dir) => format!("{dir}/{name}"),
            None => name,
        };
        if !root.join(&candidate).exists() {
            return candidate;
        }
    }
    rel.to_string()
}

/// Metadata for one markdown note, for the Files panel.
#[derive(serde::Serialize)]
pub struct NoteInfo {
    /// Path relative to the notes root (forward-slashed).
    pub name: String,
    /// Last-modified time (epoch ms).
    pub modified: i64,
}

/// Recursively collect files whose extension matches any of `exts` under `dir`,
/// named relative to `root`.
fn collect_files(root: &Path, dir: &Path, exts: &[&str], out: &mut Vec<NoteInfo>) {
    collect_files_bounded(root, dir, exts, out, 0)
}

/// The only walker in the file without a bound, until now. `walk_project` caps by
/// result count; this one recursed freely, so a directory symlink or junction
/// pointing at one of its own ancestors — which a user can create by accident,
/// and which Windows profiles are full of — recursed until the stack blew and
/// took the whole app with it. Depth is the right bound here (the notes folder is
/// shallow by nature), and it costs nothing to carry.
fn collect_files_bounded(
    root: &Path,
    dir: &Path,
    exts: &[&str],
    out: &mut Vec<NoteInfo>,
    depth: usize,
) {
    const MAX_DEPTH: usize = 24;
    const MAX_FILES: usize = 5_000;
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        // Skip symlinks and junctions to prevent escaping notes root or recursive loops
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files_bounded(root, &path, exts, out, depth + 1);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
            .unwrap_or(false)
        {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(NoteInfo {
                    name: rel.to_string_lossy().replace('\\', "/"),
                    modified,
                });
            }
        }
    }
}


