// ── surface: backend ──────────────────────────────────
// Model-facing tool schemas and descriptions.
// Exports: tool_definitions

use serde_json::json;
use crate::provider::Tool;
use super::COMMAND_TIMEOUT_SECS;

pub fn tool_definitions() -> Vec<Tool> {
        vec![
            Tool {
                name: "web_search".into(),
                description: "Search the web for current information. Fast and shallow: returns a list of titles, URLs, and snippets. Use for recent events, specific facts, or to find pages worth reading in full.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "web_fetch".into(),
                description: "Fetch and extract the full text of a web page by URL. Slower and thorough: returns full page content rather than snippets. Use after web_search to read a promising result in depth, or when given a specific URL.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The full URL to fetch (including https://)." },
                        "max_chars": { "type": "integer", "description": "Optional, default 8000. Output cap in characters; raise it for very long pages (max 100000)." }
                    },
                    "required": ["url"]
                }),
            },
            Tool {
                name: "md_write".into(),
                description: "Write a markdown DOCUMENT — prose the user reads as text: a note, draft, summary, list, or write-up. Creates or overwrites a file in their notes folder. Call this whenever the user asks to save, write, note down, draft, or make a file, note, document, or markdown — that means producing the file itself, not just answering in chat. It always produces a FILE the user can open. The user reviews a diff and approves before anything is written, so use it freely. Provide a relative path and the full new file content. To make a small change to an existing file, prefer md_edit.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path within the notes folder, e.g. 'ideas.md' or 'research/tauri.md'." },
                        "content": { "type": "string", "description": "The full new content of the file." }
                    },
                    "required": ["path", "content"]
                }),
            },
            Tool {
                name: "md_edit".into(),
                description: "Make a targeted edit to an existing markdown note in the notes folder by replacing one unique snippet, far cheaper than rewriting the whole file with md_write. Call this whenever the user asks to change, update, fix, or tweak an existing note or document. Provide the relative path, the text to replace (old_string), and its replacement (new_string). old_string must appear only once (include surrounding context to disambiguate). Matching is exact first, then whitespace-insensitive: indentation shifts, trailing spaces, line endings and blank lines are tolerated, and the reply notes when that fallback matched — so a slightly-off old_string still lands instead of erroring. Read the note first with read_note if you don't already have its content. The user reviews a diff and approves.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path within the notes folder of the file to edit." },
                        "old_string": { "type": "string", "description": "The text to replace. Must appear only once (whitespace-insensitive); include surrounding context to disambiguate." },
                        "new_string": { "type": "string", "description": "The text to replace old_string with." }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
            Tool {
                name: "html_write".into(),
                description: "Build a rendered web PAGE — something the user opens in a browser, with layout, styling or interactivity. Creates or overwrites a self-contained .html file in their notes folder. Call this when the user asks to make, build, create, or write a web page, HTML page, or HTML file. When creating a NEW page, first choose a filename that isn't already used by another page — check existing pages with search_pages (and recall any you've made this session) and pick a fresh, descriptive name; don't reuse a generic name like 'index.html' or 'page.html' for a different page, as that collides with the existing one. Produce a complete standalone .html document with any CSS and JS inline — no external build step or extra files. To change an existing page, FIRST read its current content with read_note (use search_pages to find it if you don't know the path), then write the full updated document back to that same path — this edits it in place rather than duplicating it or losing detail. For a small, localized change, prefer html_edit over rewriting the whole page. The user reviews a diff and approves. An in-chat browser preview button is provided to the user when your turn ends — you never need to open, refresh, or re-run anything to show it, so do NOT re-write or re-edit the page just to display it. Provide a relative path ending in .html and the full file content.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path within the notes folder, ending in .html, e.g. 'page.html' or 'demos/clock.html'." },
                        "content": { "type": "string", "description": "The full HTML document content — a complete, self-contained page." }
                    },
                    "required": ["path", "content"]
                }),
            },
            Tool {
                name: "html_edit".into(),
                description: "Make a targeted edit to an existing HTML page in the notes folder by replacing one unique snippet — far cheaper than rewriting the whole page. Call this to change, update, fix, or tweak an existing page (for a full redesign, use html_write). Provide the relative path, the text to replace (old_string), and its replacement (new_string). old_string must appear only once (include surrounding context to disambiguate). Matching is exact first, then whitespace-insensitive: indentation shifts, trailing spaces, line endings and blank lines are tolerated, and the reply notes when that fallback matched — so a slightly-off old_string still lands instead of erroring. Read the page first with read_note if you don't already have its content. The user reviews a diff and approves. An in-chat browser preview button is provided to the user when your turn ends — you never need to open or refresh it yourself, so do NOT re-edit the page just to display it.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the .html page within the notes folder to edit." },
                        "old_string": { "type": "string", "description": "The text to replace. Must appear only once (whitespace-insensitive); include surrounding context to disambiguate." },
                        "new_string": { "type": "string", "description": "The text to replace old_string with." }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
            Tool {
                name: "search_notes".into(),
                // Distinguished by what it searches, not by naming the other tool. The old
// closing sentence ("to find an HTML page, use search_pages, not this")
// pointed at a tool that usually ISN'T loaded — html-write defaults off —
// and a description that names an absent tool is the defect this pass has
// been removing everywhere else.
description: "Search the user's markdown notes by keyword — their written material: notes, drafts, summaries, anything they saved as text. Use to look up, find, search, or recall a note, or something the user wrote down. Returns matching notes with a short snippet from each; follow up with read_note to read one in full. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Keywords to search for across note filenames and contents." }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "search_pages".into(),
                description: "Search the user's built HTML pages by keyword — the rendered pages they open in a browser, not their text notes. Use to find an existing page before editing it, or to check a name isn't already taken before creating a new one. Returns matching pages with a short snippet from each; follow up with read_note to open one. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Keywords to search for across page filenames and contents." }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "read_note".into(),
                description: "Read the full contents of one saved file (a markdown note or an HTML page) from the user's library, by its relative path (e.g. 'recipes/curry.md' or 'clock.html'). Use to open, view, or recall a specific file when you know its path, after a search has surfaced one, or to read an existing page before editing it. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the note within the notes folder, e.g. 'ideas/todo.md'." },
                        "max_chars": { "type": "integer", "description": "Optional, default 32000. Output cap in characters; raise it for very large notes (max 100000)." }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "run_python".into(),
                description: "Run a snippet of Python in a secure sandbox and get its output back. Use to calculate, compute, test an idea, check code, or work through a problem — whenever actually running code answers better than reasoning it out by hand. Runs in an isolated WASM Python: the standard library is available, but there is no internet or real file access. Returns whatever the code prints (stdout/stderr) plus the value of the last expression. Report the result to the user in your reply; you don't need to repeat the code itself, since it's shown to them separately.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "The Python source to execute. Use print() to show results." }
                    },
                    "required": ["code"]
                }),
            },
            Tool {
                name: "list_dir".into(),
                description: "List files and directories in the project. Pass 'path' to list a specific subdirectory, or omit to list the project root. Subdirectories are marked with a trailing '/'. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Optional relative subdirectory path to list; omit for the project root." },
                        "depth": { "type": "integer", "description": "Optional directory traversal depth (e.g. 1 for immediate children only, 2 for direct subdirectories). Omit for standard breadth-first listing up to the 500-item cap." }
                    }
                }),
            },
            Tool {
                name: "read_file".into(),
                description: "Read the full contents of a file in the project. Use to inspect source code, configuration, or documentation before planning edits. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the file within the project, e.g. 'src/main.rs'." },
                        "max_chars": { "type": "integer", "description": "Optional output cap in characters (default 32000, max 100000)." }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "read_file_lines".into(),
                description: "Read a specific line range of a file instead of the whole file. Lines are 1-based and inclusive. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the file within the project." },
                        "start": { "type": "integer", "description": "First line to read (1-based, alias 'start_line' or 'from'). Defaults to 1." },
                        "end": { "type": "integer", "description": "Last line to read (1-based, inclusive, alias 'end_line' or 'to'). Defaults to the end of the file." },
                        "max_chars": { "type": "integer", "description": "Optional output cap in characters (1000-50000, default 32000)." }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "read_image".into(),
                description: "Inspect an image file in the project (PNG, JPG, GIF, WebP). Returns the image visually as an attachment for multimodal models to inspect UI layouts, charts, or plots. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the image within the project, e.g. 'screenshots/ui.png'." }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "code_search".into(),
                description: "Primary codebase discovery and exploration tool. Use this by default to investigate where features, logic, and concepts live across the project (e.g. 'sandbox permission policy enforcement' or 'websocket reconnect backoff'). Returns relevant ranked code blocks with context. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Keywords, concept terms, and synonyms describing the functionality (e.g. 'sandbox permission policy check' or 'websocket reconnect exponential backoff'). Multi-term queries cast a wide net across lexical and semantic arms. For exact literals or single symbols, prefer grep_search." },
                        "path": { "type": "string", "description": "Optional directory prefix or file path to restrict the search, e.g. 'src-tauri' or 'src/components'." },
                        "glob": { "type": "string", "description": "Optional glob pattern to filter files, e.g. '*.rs' or 'src/**/*.ts'." },
                        "max_results": { "type": "integer", "description": "Optional number of top snippets to return (default 10, max 50)." }
                    },
                    "required": ["query"]
                }),
            },
            Tool {
                name: "grep_search".into(),
                description: "Targeted exact-string and regex search across project files. Use when you already know an exact identifier, constant, function name, or error string and need exhaustive call-site matching (e.g. finding every reference to 'resolve_snippet_edit'). For discovering features or exploring unfamiliar logic, prefer code_search. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The exact text or regular expression to search for. Can be omitted if 'path' or 'glob' is provided to list matching files. Use 'name:filename' to match file paths instead of content, or prefix with '\\' (e.g. '\\name:settings') for literal matching." },
                        "path": { "type": "string", "description": "Optional directory prefix or file path to restrict the search, e.g. 'src-tauri' or 'src/components'." },
                        "glob": { "type": "string", "description": "Optional glob pattern to restrict which files are searched, e.g. '*.py' or 'src/**/*.rs'." },
                        "regex": { "type": "boolean", "description": "Treat 'query' as a regular expression. Defaults to false." },
                        "all": { "type": "boolean", "description": "When true, disables per-file match caps to return all hits for audits and call-site enumeration." },
                        "max_per_file": { "type": "integer", "description": "Optional maximum number of hits to show per file (default 6)." },
                        "case_sensitive": { "type": "boolean", "description": "Match case exactly. Defaults to false." }
                    }
                }),
            },
            Tool {
                name: "project_overview".into(),
                description: "Orientation snapshot of the project — detected languages, key entry points, layout, Git status (branch, modified/untracked files), toolchains, and runnable scripts. Call early with no arguments to get your bearings. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            Tool {
                name: "find_symbols".into(),
                description: "Extract high-level code symbols (functions, types, methods, classes) across source files (.ts, .tsx, .rs, .py, .go, .cs, etc.) without reading entire file bodies. Pass 'path' to inspect a specific file or directory, or omit to scan top-level exported APIs. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Optional file or directory path relative to project root."
                        }
                    }
                }),
            },
            Tool {
                name: "typecheck".into(),
                description: "Run the project's static typechecker or syntax validator ONCE: `tsc --noEmit` for TypeScript, `cargo check` for Rust, or `compileall` for Python (runs all applicable). A cheap post-edit sanity pass — call it after a batch of edits to catch syntax and type errors before running the app. The command is fixed and takes no arguments, so it runs without prompting; results are capped like run_command. For languages with no compiler-style checker it says so instead of guessing. Takes no arguments.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            Tool {
                name: "write_file".into(),
                description: "Create or overwrite a file in the current project. The user reviews the diff and approves before anything is written. For a targeted change to an existing file, prefer edit_file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the file within the project, e.g. 'src/utils.py'." },
                        "content": { "type": "string", "description": "The full new contents of the file." }
                    },
                    "required": ["path", "content"]
                }),
            },
            Tool {
                name: "edit_file".into(),
                description: "Make a targeted edit to an existing file by replacing one unique snippet with new text, or replace a whole function/method/class by symbol. The user reviews the diff and approves before the change is written. Matching is exact first, then whitespace-insensitive, with automatic indentation harmonization. Pass 'symbol' to scope the snippet search inside a specific function/class, or pass 'symbol' without 'old_string' to replace the entire symbol cleanly.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the file within the project." },
                        "old_string": { "type": "string", "description": "The exact text to replace. Must be unique in the file (or unique inside 'symbol' if specified). Optional when replacing an entire symbol." },
                        "new_string": { "type": "string", "description": "The text to replace it with." },
                        "symbol": { "type": "string", "description": "Optional name of the function, method, class, or type (e.g. 'process_data' or 'Server.handle'). If old_string is provided, scopes the edit inside this symbol; if old_string is omitted, replaces the entire symbol." }
                    },
                    "required": ["path", "new_string"]
                }),
            },
            Tool {
                name: "delete_file".into(),
                description: "Delete a file in the current project. The user reviews the path and approves before anything is removed. The file is snapshotted first and can be brought back with restore_file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path of the file to delete, e.g. 'src/old_helper.py'." }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "rename_file".into(),
                description: "Rename or move a file within the current project. The user reviews the from → to paths and approves before anything moves. References to the old path (imports, etc.) are NOT updated automatically — fix those separately. Won't overwrite an existing file at the destination.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "Relative path of the existing file, e.g. 'src/util.py'." },
                        "to": { "type": "string", "description": "New relative path, e.g. 'src/helpers/util.py'. Must not already exist." }
                    },
                    "required": ["from", "to"]
                }),
            },
            Tool {
                name: "restore_file".into(),
                description: "Revert a project file (or everything changed this session) to its state from before editing. The harness snapshots each file the first time it's modified, so this is an undo even in folders with no version control. Pass a path to restore one file, or omit path (or use 'all') to restore every file changed this session. The user reviews and approves.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to restore, e.g. 'src/main.rs'. Omit or use 'all' to restore every file changed this session." }
                    }
                }),
            },
            Tool {
                name: "get_diff".into(),
                description: "Show an aggregate diff of every file written, edited, created, or deleted in this project during the current session against its initial state. Read-only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "max_chars": { "type": "integer", "description": "Optional output cap in characters (default 50000, max 100000)." }
                    }
                }),
            },

            Tool {
                name: "run_command".into(),
                description: format!("Execute build tools, test suites, package managers, or long-running services in the current project (e.g. 'cargo test', 'npm run build', 'python app.py'). Times out after {COMMAND_TIMEOUT_SECS}s by default; pass 'timeout' up to 600s for release builds. For long-running servers or watchers, pass 'background: true' — it spawns detached and waits up to 'wait_ms' (default 2000) so you see startup logs in 1 turn. Exclusively for compilation and execution — do not use for searching or reading code (native search and read tools are pre-indexed and structured). EXACTLY ONE of two forms: 'command' (a shell string) or 'argv' (program + args as an array). The user reviews the exact command and folder and approves before it runs. Returns the exit code plus stdout/stderr."),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command string, e.g. 'pytest -q', 'cargo test', or 'npm run dev'. Executed via PowerShell on Windows (so unix aliases like ls, rm, cat work)." },
                        "argv": { "type": "array", "items": { "type": "string" }, "description": "Program + args as an array, e.g. ['git', 'commit', '-m', 'message with spaces']. Spawns directly, no shell — spaces, quotes and special chars in args are safe." },
                        "cwd": { "type": "string", "description": "Optional relative subdirectory (within the project) to run in. Defaults to the project root." },
                        "timeout": { "type": "integer", "description": "Optional timeout in seconds for synchronous commands (default 120, max 600). Raise for large test suites, release builds, or compilations." },
                        "background": { "type": "boolean", "description": "Set to true for long-running servers (e.g. 'npm run dev'), watchers, or graphical games (e.g. 'godot') that run indefinitely. Defaults to false." },
                        "wait_ms": { "type": "integer", "description": "Milliseconds to wait for startup when background is true (default 2000, max 30000). If the process exits within this window, returns exit code and output directly. Otherwise returns startup output and assigned process ID." }
                    }
                }),
            },
            Tool {
                name: "process_manage".into(),
                description: "Manage background processes started with run_command(background: true). 'check' waits briefly and returns whether the process is still running or has exited, along with any output emitted; 'stop' shuts down the process; 'list' shows all active processes. Read/manage only.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["check", "stop", "list"],
                            "description": "'check' waits briefly and returns the process status and recent logs; 'stop' stops the process; 'list' lists all active processes."
                        },
                        "id": {
                            "type": "integer",
                            "description": "The process id to check or stop (returned by run_command when background: true). Optional for 'list'."
                        },
                        "wait_seconds": {
                            "type": "integer",
                            "description": "For 'check' action: seconds to wait before checking status and logs (default 2, max 60). Returns whether still running or exited, plus any new output."
                        }
                    },
                    "required": ["action"]
                }),
            },
            Tool {
                name: "browser_open".into(),
                description: "Open (or navigate) a hidden browser view to a LOCAL dev-server URL so you can see what your frontend actually renders. Use it after starting a dev server with run_command(background: true) — point it at the localhost URL the server prints (e.g. 'http://localhost:5173'). Then use browser_read to see the rendered page text and any console errors. Limited to localhost — this is for perceiving what you're building, not browsing the web (use web_fetch for that).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The local dev-server URL, e.g. 'http://localhost:5173'." }
                    },
                    "required": ["url"]
                }),
            },
            Tool {
                name: "browser_read".into(),
                description: "Read what the hidden browser view (opened with browser_open) is currently rendering: the page's visible text and its captured console messages/errors. This is your eyes on a running frontend — use it to confirm the UI came up, see what's on screen, and catch runtime/console errors after a change. Read-only. If it reports no text, the page may still be loading — wait briefly and retry, or re-open.".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            Tool {
                name: "browser_act".into(),
                description: "Interact with an element in the hidden browser view. Use browser_read first to get the numerical ID of the element from the 'Interactive Elements' list. Action can be 'click', 'type' (requires 'text'), 'clear', or 'select' (requires 'text' for the option value).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "description": "The numerical ID of the element from browser_read." },
                        "action": { "type": "string", "enum": ["click", "type", "clear", "select"] },
                        "text": { "type": "string", "description": "The text to type or select (only for 'type' or 'select')." }
                    },
                    "required": ["id", "action"]
                }),
            },
            Tool {
                name: "browser_close".into(),
                description: "Close the hidden browser view opened with browser_open, when you're done perceiving the frontend.".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            Tool {
                name: "browser_reload".into(),
                description: "Reload the page currently in the browser view and wait for the server to answer. Use this when the page looks broken, half-built, or shows a browser error — browser_read only reads what is currently rendered and does NOT reload it, so a bad load will otherwise persist. Also use it after changing a file the page depends on.".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            Tool {
                name: "browser_screenshot".into(),
                description: "SEE the page currently open in the browser view — returns an actual picture of it (attached to the message after the tool result), not a description. browser_read gives you the text and console errors; this shows you the LAYOUT: whether things are positioned, sized, coloured, overlapping or visible as intended, which text alone can't tell you. Call browser_open first, then screenshot. Use it after a UI change to check how it actually looks.".into(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            // Godot 4 scene inspection (archive/DESIGN_OLD.md Phase 1). Deliberately not in
            // CODE_CORE and not a CODE_GROUPS group either: it rides a standalone
            // Settings master switch (godotToolsOn) pushed by the frontend, so
            // non-game-dev sessions never see it and never pay its definition tokens.
            Tool {
                name: "godot_scene_tree".into(),
                description: "Read a Godot 4 scene file (.tscn) and return its node tree — names, types, hierarchy, instanced sub-scenes, scripts and key property values, with resource references resolved to real res:// paths. Use this BEFORE editing a scene or answering questions about where things are in a scene: the get_node() path to a node is often not what the node names suggest (e.g. an 'Innkeeper' sprite may live at 'Inn/Interior/Innkeeper'). Args: 'scene' (required) — the .tscn path relative to the project root, or a res:// path; 'max_depth' (optional) — show only this many levels of children (root = 0); 'props' (optional, default true) — include per-node property values; 'max_chars' (optional, default 12000) — raise the output cap for very large scenes (max 100000).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "scene": { "type": "string", "description": "Path to the .tscn file, relative to the project root (e.g. \"scenes/inn.tscn\"), or a res:// path." },
                        "max_depth": { "type": "integer", "description": "Optional. Only show nodes down to this depth (root = 0); deeper subtrees are collapsed to a count." },
                        "props": { "type": "boolean", "description": "Optional, default true. Include each node's property values (position, textures, shape refs, ...). Set false for just the skeleton." },
                        "max_chars": { "type": "integer", "description": "Optional, default 12000. Output cap; raise it for very large scenes (max 100000)." }
                    },
                    "required": ["scene"]
                }),
            },

        ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_include_run_command_background_and_process_manage() {
        let defs = tool_definitions();
        let run_cmd = defs.iter().find(|t| t.name == "run_command").expect("run_command exists");
        assert!(run_cmd.parameters["properties"]["background"].is_object(), "run_command should declare background");
        assert!(run_cmd.parameters["properties"]["wait_ms"].is_object(), "run_command should declare wait_ms");

        let proc_mgmt = defs.iter().find(|t| t.name == "process_manage").expect("process_manage exists");
        assert!(proc_mgmt.parameters["properties"]["action"].is_object());
        assert!(proc_mgmt.parameters["properties"]["id"].is_object());
        assert!(proc_mgmt.parameters["properties"]["wait_seconds"].is_object());

        // Verify retired 5 tools are no longer in definitions
        for retired in ["process_start", "process_wait", "process_logs", "process_list", "process_stop"] {
            assert!(
                !defs.iter().any(|t| t.name == retired),
                "retired tool '{retired}' should not be in model definitions"
            );
        }
    }


}
