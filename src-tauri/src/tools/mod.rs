// ── surface: backend ──────────────────────────────────
// Tool registry + dispatch — every model-facing tool, gating refusals, approvals.
// Exports: Tools, definitions, execute, fetch, search, … · Related: agent.rs (filtering), commands.rs

//! Tool registry and dispatch (spec §5). Tools are owned by the Rust backend,
//! offered to the model via the provider, and executed here when the model calls
//! them. Each tool's backend sits behind its own thin interface (search, fetch —
//! §5.1) so it stays swappable. MD-write commits only after the user approves a
//! diff (§5).

pub(crate) mod code_search;
pub mod context;
pub mod definitions;
pub mod fetch;
pub mod matcher;
pub mod notes;
mod notes_search;
pub mod process;
pub mod project;
pub mod search;
pub mod symbols;
pub mod web;

pub use context::*;
pub use definitions::*;
pub use matcher::*;
pub use notes::*;
pub use process::*;
#[allow(unused_imports)]
pub use project::*;
#[allow(unused_imports)]
pub use web::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tauri::ipc::Channel;
use tauri::Manager;
use tokio_util::sync::CancellationToken;

use crate::approvals::Approvals;
use crate::coderun::CodeRuns;
use crate::permissions::PermissionStore;
use crate::provider::{StreamEvent, Tool, ToolCall};
use fetch::FetchProvider;
use search::SearchProvider;

pub(crate) const MAX_FETCH_CHARS: usize = 8000;
pub(crate) const MAX_NOTE_CHARS: usize = 32000;
pub(crate) const MAX_CHARS_CEILING: usize = 100_000;
pub(crate) const DEFAULT_CHANGES_CHARS: usize = 50_000;
pub(crate) const MAX_IMAGE_BYTES: usize = 3_500_000;

pub(crate) const PATH_ALIASES: &[&str] = &["path", "file", "filename", "filepath", "file_path"];
pub(crate) const CONTENT_ALIASES: &[&str] = &["content", "text", "contents", "body", "file_text"];
pub(crate) const OLD_ALIASES: &[&str] = &["old_string", "old_str", "old"];
pub(crate) const NEW_ALIASES: &[&str] = &["new_string", "new_str", "new", "replacement"];
pub(crate) const SYMBOL_ALIASES: &[&str] = &["symbol", "symbol_name", "target_symbol", "function", "method"];

pub(crate) fn rejection_response(decision: &str, default_msg: &str) -> String {
    if let Some(feedback) = decision.strip_prefix("reject:") {
        let trimmed = feedback.trim();
        if !trimmed.is_empty() {
            return format!("{default_msg} Feedback from user: \"{trimmed}\"");
        }
    }
    default_msg.to_string()
}

pub(crate) fn arg_str<'a>(v: &'a Value, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|n| v.get(*n).and_then(Value::as_str))
}

pub(crate) fn arg_u64(v: &Value, keys: &[&str]) -> Option<usize> {
    for &k in keys {
        if let Some(n) = v.get(k).and_then(|val| val.as_u64()) {
            return Some(n as usize);
        }
        if let Some(s) = v.get(k).and_then(|val| val.as_str()) {
            if let Ok(n) = s.trim().parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

pub(crate) fn parse_max_chars(arguments: &str, default: usize) -> usize {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .as_ref()
        .and_then(|v| arg_u64(v, &["max_chars", "max_length", "limit"]))
        .unwrap_or(default)
        .clamp(1_000, MAX_CHARS_CEILING)
}

pub(crate) fn parse_line_num(v: &Value, keys: &[&str]) -> Option<usize> {
    for &k in keys {
        if let Some(n) = v.get(k).and_then(|val| val.as_u64()) {
            return Some(n as usize);
        }
        if let Some(s) = v.get(k).and_then(|val| val.as_str()) {
            if let Ok(n) = s.trim().parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

pub(crate) fn empty_args_error(arguments: &str, tool: &str, fields: &str) -> Option<String> {
    arguments.trim().is_empty().then(|| {
        format!(
            "error: {tool} was called with empty arguments — a malformed tool call.              Re-issue it with {fields}."
        )
    })
}

pub struct Tools {
    pub(crate) search: Arc<dyn SearchProvider>,
    pub(crate) fetch: Arc<dyn FetchProvider>,
    pub(crate) notes_root: PathBuf,
    pub(crate) approvals: Arc<Approvals>,
    pub(crate) code_runs: Arc<CodeRuns>,
    pub(crate) permissions: Arc<PermissionStore>,
    pub(crate) snapshots: Arc<crate::snapshots::Snapshots>,
    pub(crate) processes: Arc<crate::processes::Processes>,
    pub(crate) app: tauri::AppHandle,
    pub(crate) pending_images: std::sync::Mutex<Vec<String>>,
}

impl Tools {
    pub fn new(
        search: Arc<dyn SearchProvider>,
        fetch: Arc<dyn FetchProvider>,
        notes_root: PathBuf,
        approvals: Arc<Approvals>,
        code_runs: Arc<CodeRuns>,
        permissions: Arc<PermissionStore>,
        snapshots: Arc<crate::snapshots::Snapshots>,
        processes: Arc<crate::processes::Processes>,
        app: tauri::AppHandle,
    ) -> Self {
        Self {
            search,
            fetch,
            notes_root,
            approvals,
            code_runs,
            permissions,
            snapshots,
            processes,
            app,
            pending_images: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn queue_image(&self, data_uri: String) {
        if let Ok(mut queued) = self.pending_images.lock() {
            queued.push(data_uri);
        }
    }

    pub fn take_pending_images(&self) -> Vec<String> {
        self.pending_images
            .lock()
            .map(|mut queued| std::mem::take(&mut *queued))
            .unwrap_or_default()
    }

    pub fn definitions(&self) -> Vec<Tool> {
        Self::tool_definitions()
    }

    pub fn tool_definitions() -> Vec<Tool> {
        tool_definitions()
    }

    pub async fn execute(
        &self,
        call: &ToolCall,
        enabled: &[String],
        _vision_on: bool,
        context_files: &[String],
        seen_paths: &[String],
        project_root: Option<&Path>,
        cancel: &CancellationToken,
        on_event: &Channel<StreamEvent>,
    ) -> String {
        // Refuse anything that wasn't offered. Until this existed, `enabled` only
        // filtered which tools were ADVERTISED — dispatch went on name alone, so any
        // implemented tool ran if a model simply guessed its name. Observed in the
        // wild: a chat-surface model invented a file, invented `read_note` to read
        // it, and the harness obliged.
        //
        // Worse, it then got back "the system cannot find the file specified" —
        // which says the TOOL is real and only the FILE is missing, so it tried
        // again. A hallucinated capability was confirmed by a plausible error.
        // Saying plainly that the tool isn't available ends that in one step.
        //
        // This is also what actually enforces a boundary that was previously only
        // a convention: notes tools staying out of coding sessions.
        let is_enabled = enabled.iter().any(|e| {
            e == &call.name
                || match (e.as_str(), call.name.as_str()) {
                    ("read_file", "project_read")
                    | ("project_read", "read_file")
                    | ("read_file", "read_file_lines")
                    | ("read_file", "project_read_lines") => true,
                    ("read_file_lines", "project_read_lines") | ("project_read_lines", "read_file_lines") => true,
                    ("read_image", "project_read_image") | ("project_read_image", "read_image") => true,
                    ("list_dir", "project_list") | ("project_list", "list_dir") => true,
                    ("grep_search", "project_search") | ("project_search", "grep_search") => true,
                    ("semantic_search", "code_search") | ("code_search", "semantic_search") => true,
                    ("project_overview", "project_info") | ("project_info", "project_overview") => true,
                    ("find_symbols", "code_symbols") | ("code_symbols", "find_symbols") => true,
                    ("write_file", "project_write") | ("project_write", "write_file") => true,
                    ("edit_file", "project_edit") | ("project_edit", "edit_file") => true,
                    ("delete_file", "project_delete") | ("project_delete", "delete_file") => true,
                    ("rename_file", "project_rename") | ("project_rename", "rename_file") => true,
                    ("restore_file", "project_restore") | ("project_restore", "restore_file") => true,
                    ("get_diff", "project_changes") | ("project_changes", "get_diff") => true,
                    _ => false,
                }
        });
        if !is_enabled {
            let is_legacy_process = matches!(
                call.name.as_str(),
                "process_start"
                    | "process_wait"
                    | "process_logs"
                    | "process_stop"
                    | "process_list"
                    | "run_background_process"
            ) && enabled
                .iter()
                .any(|e| e == "run_command" || e == "process_manage");
            if !is_legacy_process {
                return format!(
                    "error: the '{}' tool isn't available in this session. Use only the tools you were given.",
                    call.name
                );
            }
        }
        match call.name.as_str() {
            "web_search" => self.web_search(&call.arguments).await,
            "web_fetch" => self.web_fetch(&call.arguments).await,
            "run_python" => self.run_python(&call.arguments, on_event).await,
            "md_write" => {
                self.md_write(&call.arguments, context_files, seen_paths, on_event)
                    .await
            }
            "md_edit" => self.md_edit(&call.arguments, on_event).await,
            "html_write" => self.html_write(&call.arguments, seen_paths, on_event).await,
            "html_edit" => self.html_edit(&call.arguments, on_event).await,
            "search_notes" => self.search_notes(&call.arguments),
            "search_pages" => self.search_pages(&call.arguments),
            "read_note" => self.read_note_tool(&call.arguments),
            "list_dir" | "project_list" => self.project_list(&call.arguments, project_root),
            "read_file" | "project_read" => self.project_read(&call.arguments, project_root),
            "read_file_lines" | "project_read_lines" => self.project_read_lines(&call.arguments, project_root),
            "read_image" | "project_read_image" => self.project_read_image(&call.arguments, project_root),
            "grep_search" | "project_search" => self.project_search(&call.arguments, project_root),
            "semantic_search" | "code_search" => self.code_search(&call.arguments, project_root, cancel).await,
            "project_overview" | "project_info" => self.project_info(&call.arguments, project_root).await,
            "find_symbols" | "code_symbols" => self.code_symbols(&call.arguments, project_root),
            "godot_scene_tree" => self.godot_scene_tree(&call.arguments, project_root),
            "write_file" | "project_write" => {
                self.project_write(&call.arguments, project_root, on_event)
                    .await
            }
            "edit_file" | "project_edit" => {
                self.project_edit(&call.arguments, project_root, on_event)
                    .await
            }
            "project_edit_batch" => {
                "error: 'project_edit_batch' has been retired. Call 'edit_file' sequentially for each file modification so each change is individually reviewed and validated.".into()
            }
            "delete_file" | "project_delete" => {
                self.project_delete(&call.arguments, project_root, on_event)
                    .await
            }
            "rename_file" | "project_rename" => {
                self.project_rename(&call.arguments, project_root, on_event)
                    .await
            }
            "restore_file" | "project_restore" => {
                self.project_restore(&call.arguments, project_root, on_event)
                    .await
            }
            "get_diff" | "project_changes" => self.project_changes(&call.arguments, project_root),
            "run_command" => {
                self.run_command(&call.arguments, project_root, cancel, on_event)
                    .await
            }
            "typecheck" => self.typecheck(&call.arguments, project_root, cancel).await,
            "process_manage" => {
                self.process_manage(&call.arguments, cancel).await
            }
            "process_start" => {
                self.legacy_process_start(&call.arguments, project_root, cancel, on_event)
                    .await
            }
            "process_logs" => self.process_logs(&call.arguments),
            "process_wait" => self.process_wait(&call.arguments, cancel).await,
            "process_list" => self.processes.list(),
            "process_stop" => self.process_stop(&call.arguments),
            "browser_open" => self.browser_open(&call.arguments).await,
            "browser_act" => self.browser_act(&call.arguments).await,
            "browser_read" => self.browser_read().await,
            "browser_close" => self.browser_close(),
            "browser_reload" => self.browser_reload().await,
            "browser_screenshot" => self.browser_screenshot().await,
            other => {
                format!("error: unrecognized tool '{}'.", other)
            }
        }
    }
    pub fn note_context_files(&self, _project_root: Option<&Path>, _files: &[String]) {
        // No-op: legacy tail context retired.
    }

    pub fn context_change(&self, _project_root: Option<&Path>) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rejection_response_formatting() {
        assert_eq!(
            rejection_response("reject", "The user rejected the edit."),
            "The user rejected the edit."
        );
        assert_eq!(
            rejection_response("reject:", "The user rejected the edit."),
            "The user rejected the edit."
        );
        assert_eq!(
            rejection_response("reject:   ", "The user rejected the edit."),
            "The user rejected the edit."
        );
        assert_eq!(
            rejection_response(
                "reject:Mock this in test instead",
                "The user rejected the edit to 'src/main.py'. No changes were made."
            ),
            "The user rejected the edit to 'src/main.py'. No changes were made. Feedback from user: \"Mock this in test instead\""
        );
    }
}
