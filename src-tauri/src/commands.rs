// ── surface: backend ──────────────────────────────────
// The bridge — every Tauri command (send_chat, compaction, notes, settings…),
// plus ENV_NOTES and the compaction prompt. Also owns AppState.
// Exports: AppState + ~50 commands (send_chat, compact_thread, preview_context, …) · Related: everything

//! Tauri commands — the bridge between the React webview and the Rust-side agent
//! loop / provider / tools / store. The webview invokes these; it never talks to
//! an inference backend or holds an API key directly.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{Manager, State};
use tokio_util::sync::CancellationToken;

use crate::agent;
use crate::approvals::Approvals;
use crate::coderun::CodeRuns;
use crate::grounding::{
    coding_orientation, display_datetime, grounding_message,
};
use crate::provider::{ChatRequest, Message, Provider, Role, StreamEvent};
use crate::store::{Store, ThreadInfo};
use crate::tools::Tools;

/// App-wide state. `cancel` holds the current turn's cancellation token so the
/// `stop_turn` command can interrupt it.
pub struct AppState {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<Tools>,
    pub approvals: Arc<Approvals>,
    pub code_runs: Arc<CodeRuns>,
    pub permissions: Arc<crate::permissions::PermissionStore>,
    pub snapshots: Arc<crate::snapshots::Snapshots>,
    /// Also held by `Tools`; kept here so the app-exit handler can stop_all().
    pub processes: Arc<crate::processes::Processes>,
    /// SQLite persistence for threads/messages — one file, WAL-backed, behind a
    /// non-poisoning mutex (store.rs).
    pub store: Arc<Store>,
    pub tz: Tz,
    /// The in-flight turn's cancellation token, or `None` between turns. Cleared
    /// by `ResetOnDrop` when a turn ends, so a late `stop_turn` can't cancel a
    /// dead token (or, once turns can overlap, the wrong live one).
    pub cancel: Mutex<Option<CancellationToken>>,
    /// Cancellation token for background repository indexing (/index), if running.
    pub index_cancel: Mutex<Option<CancellationToken>>,
}

#[tauri::command]
pub async fn list_models(
    state: State<'_, AppState>,
    provider_kind: Option<String>,
    base_url: Option<String>,
    key_id: Option<String>,
    mid_system_tail: Option<bool>,
) -> Result<Vec<String>, String> {
    let provider = match base_url.as_deref() {
        Some(base) if !base.is_empty() => {
            let kind = provider_kind.as_deref().unwrap_or("openai");
            let key = key_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .and_then(|id| crate::keystore::get_key(id));
            crate::provider::make_provider(kind, base, key, mid_system_tail.unwrap_or(true))
        }
        _ => state.provider.clone(),
    };
    provider.list_models().await.map_err(|e| e.to_string())
}

/// Curated facts about the coding environment, injected into every session so
/// local models stop rediscovering them the hard way (the `py` vs `python`,
/// `where` vs `which`, cmd-quoting, cp1252-encoding loop). Compiled per-OS.
/// Tool-semantics facts were dropped 2026-08-24 (run_command's two forms,
/// multi-line Python, the never-launch-a-server rule) — each is stated in its
/// tool's own definition, and a copy here would drift from it.
#[cfg(windows)]
const ENV_NOTES: &str = "\n\nEnvironment — Windows (commands run through PowerShell):\n\
     - PowerShell is used for shell commands. Use `;` to sequence commands, NOT `&&` (e.g. `cmd1; cmd2`).\n\
     - Use standard PowerShell aliases (`ls`, `rm`, `cat`, `mv`). Variables use `$env:VAR`.\n\
     - DO NOT use shell commands for file searching or reading. Use `code_search` as your primary discovery tool for finding features and logic, `grep_search` for exact symbols and call sites, `find_symbols` for structural outlines, and `read_file` to inspect files.\n\
     - Python: use `py` (the launcher), NOT `python`. Check with `py --version`; install with `py -m pip install X`.\n\
     - Both stdout and stderr are captured automatically — no `2>&1` needed.\n\
     - Python output is UTF-8 (PYTHONUTF8=1) and bytecode caching is disabled (PYTHONDONTWRITEBYTECODE=1).";

#[cfg(not(windows))]
const ENV_NOTES: &str = "";

fn format_byte_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        let kb = bytes as f64 / 1024.0;
        format!("{kb:.1} KB")
    } else {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        format!("{mb:.1} MB")
    }
}

fn parse_u64_field(val: &Option<serde_json::Value>, keys: &[&str]) -> Option<u64> {
    let v = val.as_ref()?;
    for k in keys {
        if let Some(item) = v.get(*k) {
            if let Some(n) = item.as_u64() {
                return Some(n);
            }
            if let Some(s) = item.as_str() {
                if let Ok(n) = s.trim().parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// True when a tool result is a failure message rather than payload.
///
/// Tools signal failure with a short leading marker (`project.rs`, `web.rs`, `notes.rs`,
/// `symbols.rs`, `mod.rs`), or — for a bad line range — by naming the file and the overrun
/// without an `error:` prefix. A receipt must record failure honestly rather than summarize it as
/// content. Anchored to the START of the first line so file *content* that merely quotes an error
/// string (e.g. this module's own tests) is not mistaken for a failed call. If a tool in the prune
/// set ever adds a new error prefix, add it to this list.
fn is_error_result(text: &str) -> bool {
    const ERROR_PREFIXES: &[&str] = &[
        "error:",
        "error reading",
        "write error:",
        "search error:",
        "fetch error:",
        "page read error:",
        "rename error:",
        "delete error:",
    ];
    let first_line = text.trim_start().lines().next().unwrap_or("").trim();
    ERROR_PREFIXES.iter().any(|p| first_line.starts_with(p)) || is_range_error(text)
}

/// True for the `read_file_lines` "past the end" failure, whose first line names the file rather
/// than opening with `error:` — `'foo.txt' has only 7 lines; start line 999 is past the end.`
fn is_range_error(text: &str) -> bool {
    let first_line = text.trim_start().lines().next().unwrap_or("").trim();
    first_line.contains("has only") && first_line.contains("is past the end")
}

/// Receipt for a pruned call that FAILED, e.g.
/// `receipt — read_image("nope.png") returned an error in an earlier turn; the transcript's copy
/// has since been archived`. Names the subject (path/url/scene/query) when the args carry one, so
/// the model can see which call it was and address it if it still needs to. Deliberately neutral:
/// "returned an error" matches the register of the affirmative `returned …` receipts, and carries
/// no retry prompt — a charged verb or an action cue invites apology loops and needless re-reads.
///
/// A bad line RANGE adds a class marker — `(line past end)` — and drops the requested number.
/// Echoing the number would place it in the slot where a successful receipt shows a resolved
/// location, reading as though that line exists and the failure lay elsewhere; naming the class
/// says what went wrong without asserting anything false. A bad PATH carries no marker — there the
/// path is the whole cause.
fn failure_receipt(name: &str, parsed: &Option<serde_json::Value>, is_range: bool) -> String {
    let subject = parsed
        .as_ref()
        .and_then(|v| {
            ["path", "url", "scene", "query", "file"]
                .iter()
                .find_map(|k| v.get(k).and_then(|s| s.as_str()))
        })
        .map(|s| format!("(\"{s}\")"))
        .unwrap_or_default();
    let class = if is_range { " (line past end)" } else { "" };
    format!("receipt — {name}{subject} returned an error in an earlier turn{class}; the transcript's copy has since been archived")
}

/// Extracts the host domain name from a URL, stripping schemes, www prefixes, and ports/paths.
fn domain_of(raw_url: &str) -> String {
    let s = raw_url.trim();
    let without_scheme = if let Some(rest) = s.strip_prefix("https://") {
        rest
    } else if let Some(rest) = s.strip_prefix("http://") {
        rest
    } else if let Some(rest) = s.strip_prefix("//") {
        rest
    } else {
        s
    };
    let host_and_port = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host = host_and_port.split(':').next().unwrap_or(host_and_port);
    let host = host.trim_start_matches("www.").trim();
    if host.is_empty() {
        "web".to_string()
    } else {
        host.to_string()
    }
}

struct SearchIndexEntry {
    num: usize,
    title: String,
    domain: String,
}

/// Parses the structured `{i}. {title}\n   {url}\n   {snippet}` wire format emitted by `web_search`
/// into an ordered index of (number, title, domain) up to `max_rows`.
fn search_result_index(raw: &str, max_rows: usize) -> Vec<SearchIndexEntry> {
    let mut entries = Vec::new();
    let lines: Vec<&str> = raw.lines().collect();
    let mut i = 0;
    while i < lines.len() && entries.len() < max_rows {
        let line = lines[i].trim_start();
        if let Some((num_str, title)) = line.split_once(". ") {
            if let Ok(num) = num_str.parse::<usize>() {
                if num > 0 && !title.trim().is_empty() && i + 1 < lines.len() {
                    let next_line = lines[i + 1].trim();
                    if next_line.starts_with("http://")
                        || next_line.starts_with("https://")
                        || next_line.starts_with("www.")
                    {
                        let domain = domain_of(next_line);
                        let mut clean_title = title.trim().to_string();
                        if clean_title.chars().count() > 80 {
                            clean_title = format!("{}…", clean_title.chars().take(79).collect::<String>());
                        }
                        entries.push(SearchIndexEntry {
                            num,
                            title: clean_title,
                            domain,
                        });
                        i += 2;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    entries
}

struct OutlineEntry {
    level: usize,
    title: String,
}

/// Parses markdown heading lines (`# `, `## `, etc.) up to `max_rows`,
/// ignoring comments inside fenced code blocks.
/// Returns the list of parsed entries (up to `max_rows`) and the total count of headings found.
fn document_outline(raw: &str, max_rows: usize) -> (Vec<OutlineEntry>, usize) {
    let mut entries = Vec::new();
    let mut total_headings = 0;
    let mut in_code_block = false;

    for line in raw.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        let hash_count = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hash_count) {
            let after_hashes = &trimmed[hash_count..];
            if after_hashes.starts_with(' ') || after_hashes.starts_with('\t') {
                let title = after_hashes.trim_start().trim_end_matches('#').trim();
                if !title.is_empty() {
                    total_headings += 1;
                    if entries.len() < max_rows {
                        let mut clean_title = title.to_string();
                        if clean_title.chars().count() > 80 {
                            clean_title = format!("{}…", clean_title.chars().take(79).collect::<String>());
                        }
                        entries.push(OutlineEntry {
                            level: hash_count,
                            title: clean_title,
                        });
                    }
                }
            }
        }
    }

    (entries, total_headings)
}

/// Helper to construct an affirmative, tool-specific receipt for pruned tool outputs.
///
/// Rather than punitive stubs ("[project_read ... — output trimmed]") that trigger error-recovery
/// and anxiety loops in local models, these receipts clearly state what succeeded, provide
/// exact line counts and sizes, and give constructive instructions on how to re-inspect if needed.
///
/// A *failed* call is never summarized as a success — see the `is_error_result` guard below.
fn format_pruned_stub(name: &str, args: &str, raw_text: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(args).ok();

    // Every affirmative template below assumes the call returned content. When it didn't,
    // describe the failure instead of laundering it into a success receipt.
    if is_error_result(raw_text) {
        return failure_receipt(name, &parsed, is_range_error(raw_text));
    }

    let bytes = raw_text.len();
    let size_str = format_byte_size(bytes);

    // A tool output is truncated if and only if it carries the double-newline-delimited trailer:
    // "\n\n[content truncated at " (emitted by project_read, read_note, web_fetch)
    // or "\n\n[truncated at " (emitted by project_read_lines)
    // Anchoring to "\n\n[" and ensuring the trailer closes with ']' prevents false positives when
    // reading code or tests that quote truncation strings (e.g. commands.rs or tools/mod.rs).
    // Note: tscn.rs emits "\n\n[output truncated at " and project_list emits "\n… (truncated at … files)",
    // neither of which are in the prune set (should_prune below). If either tool ever joins the prune
    // list, add its trailer pattern here and add a tailored receipt arm in match name below.
    let (effective_text, is_truncated) = if let Some(idx) = raw_text.rfind("\n\n[content truncated at ") {
        if raw_text[idx..].trim_end().ends_with(']') {
            (&raw_text[..idx], true)
        } else {
            (raw_text, false)
        }
    } else if let Some(idx) = raw_text.rfind("\n\n[truncated at ") {
        if raw_text[idx..].trim_end().ends_with(']') {
            (&raw_text[..idx], true)
        } else {
            (raw_text, false)
        }
    } else if let Some(idx) = raw_text.rfind("\n\n[output truncated at ") {
        if raw_text[idx..].trim_end().ends_with(']') {
            (&raw_text[..idx], true)
        } else {
            (raw_text, false)
        }
    } else {
        (raw_text, false)
    };

    // For `project_read_lines`, the output includes a header line:
    //   `'path' lines start–end (of total):`
    // followed by lines formatted as `<lineno>\t<content>`.
    // Counting only numbered lines avoids the +1 off-by-one error on ranged reads.
    let line_count = if name == "project_read_lines" {
        let numbered_count = effective_text
            .lines()
            .filter(|l| {
                l.split_once('\t')
                    .map_or(false, |(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .count();
        if numbered_count > 0 {
            numbered_count
        } else if effective_text.lines().next().map_or(false, |l| l.starts_with('\'') && l.ends_with(':')) {
            effective_text.lines().count().saturating_sub(1).max(1)
        } else {
            effective_text.lines().count()
        }
    } else {
        effective_text.lines().count()
    };

    let lines_str = if line_count == 1 {
        "1 line".to_string()
    } else {
        format!("{line_count} lines")
    };

    match name {
        "web_fetch" => {
            let url = parsed.as_ref().and_then(|v| v.get("url").and_then(|u| u.as_str()));
            let cap_str = if is_truncated { " [capped]" } else { "" };
            let subject = if let Some(url) = url {
                format!("(\"{url}\")")
            } else {
                "".to_string()
            };

            let (entries, total_headings) = document_outline(effective_text, 8);
            if entries.is_empty() {
                format!("receipt — web_fetch{subject} returned {size_str}{cap_str} in an earlier turn; the transcript's copy has since been archived")
            } else {
                let mut lines = Vec::with_capacity(entries.len() + 3);
                lines.push(format!(
                    "receipt — web_fetch{subject} returned {size_str}{cap_str} in an earlier turn; the content was archived, but the document outline is kept:"
                ));
                for entry in &entries {
                    lines.push(format!("  • [H{}] {}", entry.level, entry.title));
                }
                if total_headings > entries.len() {
                    let remaining = total_headings - entries.len();
                    let word = if remaining == 1 { "section" } else { "sections" };
                    lines.push(format!("  … (and {remaining} more {word})"));
                }
                if is_truncated {
                    lines.push("(run web_fetch again to restore full content; pass max_chars to raise the limit)".to_string());
                } else {
                    lines.push("(run web_fetch again to restore full content)".to_string());
                }
                lines.join("\n")
            }
        }
        "web_search" => {
            let query = parsed.as_ref().and_then(|v| v.get("query").and_then(|q| q.as_str()));
            let entries = search_result_index(effective_text, 5);

            if entries.is_empty() {
                if let Some(query) = query {
                    format!("receipt — web_search(\"{query}\") returned output in an earlier turn; the transcript's copy has since been archived")
                } else {
                    format!("receipt — web_search returned output in an earlier turn; the transcript's copy has since been archived")
                }
            } else {
                let count = entries.len();
                let results_word = if count == 1 { "1 result" } else { &format!("{count} results") };
                let mut lines = Vec::with_capacity(count + 3);

                if let Some(query) = query {
                    lines.push(format!(
                        "receipt — web_search(\"{query}\") returned {results_word} in an earlier turn; the snippets were archived, but the result index is kept:"
                    ));
                } else {
                    lines.push(format!(
                        "receipt — web_search returned {results_word} in an earlier turn; the snippets were archived, but the result index is kept:"
                    ));
                }

                for entry in entries {
                    lines.push(format!("  {}. {} — {}", entry.num, entry.title, entry.domain));
                }

                if let Some(query) = query {
                    lines.push(format!("(run web_search again with \"{query}\" to restore the snippets)"));
                } else {
                    lines.push("(run web_search again to restore the snippets)".to_string());
                }

                lines.join("\n")
            }
        }
        "read_file" | "project_read" => {
            let path = parsed.as_ref().and_then(|v| v.get("path").and_then(|p| p.as_str()));
            let count_label = if is_truncated {
                let total_from_trailer = raw_text
                    .rfind("\n\n[content truncated at ")
                    .and_then(|t_idx| {
                        let trailer = &raw_text[t_idx..];
                        trailer.find(" of ").and_then(|idx| {
                            let rest = &trailer[idx + 4..];
                            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                            num_str.parse::<usize>().ok()
                        })
                    });

                if let Some(total) = total_from_trailer {
                    format!("lines 1–{line_count} (of {total}) [capped]")
                } else {
                    format!("first {lines_str} [capped]")
                }
            } else {
                lines_str
            };
            let lines_tool = if name == "read_file" { "read_file_lines" } else { "project_read_lines" };
            if let Some(path) = path {
                format!("receipt — {name}(\"{path}\") returned {count_label} in an earlier turn; the transcript's copy has since been archived (re-read with {lines_tool} before editing)")
            } else {
                format!("receipt — {name} returned {count_label} in an earlier turn; the transcript's copy has since been archived (re-read with {lines_tool} before editing)")
            }
        }
        "read_file_lines" | "project_read_lines" => {
            let path = parsed.as_ref().and_then(|v| v.get("path").and_then(|p| p.as_str()));

            // Determine start and end lines.
            // 1. First attempt: parse line numbers directly from numbered line prefixes ("<num>\t...")
            let first_line_num = effective_text
                .lines()
                .find_map(|l| l.split_once('\t').and_then(|(num, _)| num.trim().parse::<u64>().ok()));
            let last_line_num = effective_text
                .lines()
                .rev()
                .find_map(|l| l.split_once('\t').and_then(|(num, _)| num.trim().parse::<u64>().ok()));

            // 2. Fallback: check arguments
            let start = first_line_num
                .or_else(|| parse_u64_field(&parsed, &["start", "start_line", "from", "line_start"]));
            let end = last_line_num
                .or_else(|| parse_u64_field(&parsed, &["end", "end_line", "to", "line_end"]));

            let range_prefix = match (start, end) {
                (Some(s), Some(e)) if s == e => format!("line {s}"),
                (Some(s), Some(e)) => format!("lines {s}–{e}"),
                (Some(s), None) if line_count > 0 => {
                    let e = s + (line_count as u64).saturating_sub(1);
                    if s == e {
                        format!("line {s}")
                    } else {
                        format!("lines {s}–{e}")
                    }
                }
                _ => "lines".to_string(),
            };

            let count_label = if is_truncated {
                format!("{lines_str} [capped]")
            } else {
                lines_str
            };

            if let Some(path) = path {
                format!("receipt — {name}(\"{path}\", {range_prefix}) returned {count_label} in an earlier turn; the transcript's copy has since been archived (re-read with {name} before editing)")
            } else {
                format!("receipt — {name}({range_prefix}) returned {count_label} in an earlier turn; the transcript's copy has since been archived (re-read with {name} before editing)")
            }
        }
        "read_note" => {
            let path = parsed.as_ref().and_then(|v| v.get("path").and_then(|p| p.as_str()));
            let count_label = if is_truncated {
                format!("first {lines_str} [capped]")
            } else {
                lines_str
            };
            if let Some(path) = path {
                format!("receipt — read_note(\"{path}\") returned {count_label} in an earlier turn; the transcript's copy has since been archived")
            } else {
                format!("receipt — read_note returned {count_label} in an earlier turn; the transcript's copy has since been archived")
            }
        }
        "read_image" | "project_read_image" => {
            let path = parsed
                .as_ref()
                .and_then(|v| v.get("path").or_else(|| v.get("file")).and_then(|p| p.as_str()));
            if let Some(path) = path {
                format!("receipt — {name}(\"{path}\") was inspected in an earlier turn; the image payload has since been archived")
            } else {
                format!("receipt — {name} was inspected in an earlier turn; the image payload has since been archived")
            }
        }
        "browser_screenshot" => {
            "receipt — browser_screenshot was inspected in an earlier turn; the image payload has since been archived".to_string()
        }
        "godot_scene_tree" => {
            let scene = parsed
                .as_ref()
                .and_then(|v| v.get("scene").or_else(|| v.get("path")).and_then(|p| p.as_str()));
            let cap_str = if is_truncated { " [capped]" } else { "" };

            let node_count_label = effective_text
                .lines()
                .next()
                .and_then(|first_line| {
                    if let Some(idx) = first_line.find(" nodes") {
                        let before = &first_line[..idx];
                        before.rsplit_once(',').or_else(|| before.rsplit_once('(')).and_then(|(_, n)| {
                            let n = n.trim();
                            if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() {
                                let count: usize = n.parse().ok()?;
                                if count == 1 {
                                    Some("1 node".to_string())
                                } else {
                                    Some(format!("{count} nodes"))
                                }
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "scene tree".to_string());

            if let Some(scene) = scene {
                format!("receipt — godot_scene_tree(\"{scene}\") returned {node_count_label}{cap_str} in an earlier turn; the transcript's copy has since been archived")
            } else {
                format!("receipt — godot_scene_tree returned {node_count_label}{cap_str} in an earlier turn; the transcript's copy has since been archived")
            }
        }
        // Fallback for any future tool added to `should_prune` that hasn't received a tailored arm yet.
        _ => format!("receipt — {name} returned output in an earlier turn; the transcript's copy has since been archived"),
    }
}

/// Reconstruct multi-turn tool history from assistant `segments`.
///
/// Whenever an Assistant message has visual `segments` containing tool calls and results,
/// reconstruct the true chronological sequential interaction:
/// 1. Contiguous tool calls with preceding speech bubbles or reasoning are grouped into sequential steps:
///    Assistant step emitting the tool calls (with preceding text and preserved reasoning) ->
///    Subsequent `Role::Tool` messages carrying the tool results (with differentiated lifespan pruning).
/// 2. If the assistant produced final text or reasoning after tools, an Assistant message carrying that
///    synthesized response (`tool_calls: Vec::new()`).
///
/// Differentiated tool lifespan pruning policy:
/// - Web results (`web_search`, `web_fetch`) are one-shot scrapes; once synthesized, they are
///   trimmed immediately on subsequent user turns (`turns_ago >= 1`).
///// - File reads (`project_read`, `project_read_lines`, `read_note`) retain an N-turn working memory window
///   configured by `read_prune_turns` (default 2 turns). 0 or None means keep full / unlimited.
/// - Modifications/writes/edits are small acknowledgments and are never trimmed.
///
/// Reasoning stripping:
/// - All historical assistant turns have their reasoning stripped unconditionally across all
///   connections (local and cloud alike) to keep context clean and prevent provider errors.
///
/// Shared between `send_chat` and `preview_context` so the inspector and live API never drift.
pub(crate) fn expand_messages_with_segments(
    messages: Vec<Message>,
    read_prune_turns: Option<u32>,
) -> Vec<Message> {
    let mut expanded = Vec::new();
    let num_messages = messages.len();
    let prune_limit = read_prune_turns.unwrap_or(2);

    // Precalculate how many user turns occur after each message index.
    // In reverse: running count of Role::User messages seen so far from the tail.
    let mut user_turns_after = vec![0; num_messages];
    let mut user_count = 0;
    for i in (0..num_messages).rev() {
        user_turns_after[i] = user_count;
        if messages[i].role == Role::User {
            user_count += 1;
        }
    }

    for (i, mut msg) in messages.into_iter().enumerate() {
        if msg.role != Role::Assistant {
            expanded.push(msg);
            continue;
        }

        let turns_ago = user_turns_after[i];

        let has_tool_segment = msg
            .segments
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .map_or(false, |segs| {
                segs.iter().any(|s| s.get("kind").and_then(|k| k.as_str()) == Some("tool"))
            });

        if has_tool_segment {
            let segs = msg.segments.as_ref().and_then(serde_json::Value::as_array).unwrap();

            let mut pending_text = String::new();
            let mut pending_calls = Vec::new();
            let mut pending_results = Vec::new();
            let mut text_segments_seen = 0;
            let mut is_first_assistant_step = true;

            let flush_tool_step = |expanded: &mut Vec<Message>,
                                   pending_text: &mut String,
                                   pending_calls: &mut Vec<crate::provider::ToolCall>,
                                   pending_results: &mut Vec<Message>,
                                   _is_first_step: &mut bool,
                                   created_at: Option<i64>| {
                if pending_calls.is_empty() {
                    return;
                }

                expanded.push(Message {
                    role: Role::Assistant,
                    content: std::mem::take(pending_text).trim().to_string(),
                    images: Vec::new(),
                    tool_calls: std::mem::take(pending_calls),
                    tool_call_id: None,
                    created_at,
                    reasoning: None,
                    reasoning_signature: None,
                    sources: Vec::new(),
                    segments: None,
                });

                expanded.append(pending_results);
                *_is_first_step = false;
            };

            for seg in segs {
                let kind = seg.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                match kind {
                    "reasoning" => {
                        if !pending_calls.is_empty() {
                            flush_tool_step(
                                &mut expanded,
                                &mut pending_text,
                                &mut pending_calls,
                                &mut pending_results,
                                &mut is_first_assistant_step,
                                msg.created_at,
                            );
                        }
                    }
                    "text" => {
                        if !pending_calls.is_empty() {
                            flush_tool_step(
                                &mut expanded,
                                &mut pending_text,
                                &mut pending_calls,
                                &mut pending_results,
                                &mut is_first_assistant_step,
                                msg.created_at,
                            );
                        }
                        if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                            text_segments_seen += 1;
                            pending_text.push_str(t);
                        }
                    }
                    "tool" => {
                        let id = seg.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        let name = seg.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        let args = seg.get("args").and_then(|a| a.as_str()).unwrap_or("").to_string();
                        let raw_text = seg.get("output").and_then(|t| t.as_str()).unwrap_or("").to_string();

                        // Differentiated pruning: web and vision trimmed at 1+ turns; reads trimmed according to prune_limit (0 = unlimited).
                        let should_prune = (matches!(name.as_str(), "web_search" | "web_fetch" | "read_image" | "project_read_image" | "browser_screenshot") && turns_ago >= 1)
                            || (matches!(name.as_str(), "read_file" | "read_file_lines" | "project_read" | "project_read_lines" | "read_note" | "godot_scene_tree")
                                && prune_limit > 0
                                && turns_ago >= prune_limit as usize);

                        let text = if should_prune && (raw_text.len() > 120 || matches!(name.as_str(), "read_image" | "project_read_image" | "browser_screenshot")) {
                            format_pruned_stub(&name, &args, &raw_text)
                        } else {
                            raw_text
                        };

                        pending_calls.push(crate::provider::ToolCall {
                            id: id.clone(),
                            name,
                            arguments: args,
                        });

                        pending_results.push(Message {
                            role: Role::Tool,
                            content: text,
                            tool_call_id: Some(id),
                            images: Vec::new(),
                            tool_calls: Vec::new(),
                            created_at: msg.created_at,
                            reasoning: None,
                            reasoning_signature: None,
                            sources: Vec::new(),
                            segments: None,
                        });
                    }
                    _ => {
                        // "memory" or other non-wire segment kinds are ignored
                    }
                }
            }

            // If any tool calls are still pending, flush them now
            if !pending_calls.is_empty() {
                flush_tool_step(
                    &mut expanded,
                    &mut pending_text,
                    &mut pending_calls,
                    &mut pending_results,
                    &mut is_first_assistant_step,
                    msg.created_at,
                );
            }

            // Final synthesized answer step (if any text remains or was in msg.content)
            let final_text = if text_segments_seen > 0 {
                pending_text.trim().to_string()
            } else {
                msg.content.trim().to_string()
            };

            if !final_text.is_empty() {
                expanded.push(Message {
                    role: Role::Assistant,
                    content: final_text,
                    images: msg.images,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    created_at: msg.created_at,
                    reasoning: None,
                    reasoning_signature: None,
                    sources: msg.sources,
                    segments: None,
                });
            }
        } else {
            // Either msg.segments was None/empty or had no tool calls.
            if !msg.tool_calls.is_empty() {
                expanded.push(Message {
                    role: Role::Assistant,
                    content: String::new(),
                    images: Vec::new(),
                    tool_calls: msg.tool_calls,
                    tool_call_id: None,
                    created_at: msg.created_at,
                    reasoning: None,
                    reasoning_signature: None,
                    sources: Vec::new(),
                    segments: None,
                });
                if !msg.content.trim().is_empty() {
                    expanded.push(Message {
                        role: Role::Assistant,
                        content: msg.content.trim().to_string(),
                        images: msg.images,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        created_at: msg.created_at,
                        reasoning: None,
                        reasoning_signature: None,
                        sources: msg.sources,
                        segments: None,
                    });
                }
            } else {
                // Ordinary assistant message without tools
                msg.reasoning = None;
                msg.reasoning_signature = None;
                expanded.push(msg);
            }
        }
    }

    expanded
}

#[tauri::command]
pub async fn send_chat(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    mut request: ChatRequest,
    enabled_tools: Vec<String>,
    context_files: Vec<String>,
    project_root: Option<String>,
    system_prompt: String,
    profile: String,
    provider_kind: Option<String>,
    base_url: Option<String>,
    key_id: Option<String>,
    // OBSOLETE (2026-08-30): the tail folds into the one leading system block on
    // every adapter, so this capability is never consulted — accepted only for
    // the frontend's unchanged `midSystemMessages` contract. Retire with the UI flag.
    mid_system_tail: Option<bool>,
    // The session's Vision switch. The frontend gates the NATIVE image tools on
    // it in enabledToolList(); the backend gate that once covered MCP-advertised
    // tools went with the MCP removal (0.2.17). Forwarded to the tool layer as
    // `_vision_on` and never read — accepted only for the frontend's unchanged
    // contract. Retire with the UI flag.
    vision_on: bool,
    read_prune_turns: Option<u32>,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    // Fresh cancellation token for this turn; `stop_turn` cancels it.
    let token = CancellationToken::new();
    *state.cancel.lock().unwrap() = Some(token.clone());
    // Clear the slot when the turn ends (any outcome) so a late `stop_turn` is a
    // no-op rather than cancelling a dead token — or, once turns overlap, the
    // wrong live one.
    let _guard = ResetOnDrop {
        cancel: &state.cancel,
    };

    // Prepend one leading system message: the user's per-chat instructions (if
    // any), then grounding (§10e), then in-context document contents (§5), and
    // finally the user's profile — framed as reference and kept out of the lead
    // instruction slot so the model treats it as information it has, not behaviour
    // to adopt. One system message for chat-template compatibility; not persisted
    // — rebuilt fresh per turn.
    let mut system_content = String::new();
    if !system_prompt.trim().is_empty() {
        system_content.push_str(system_prompt.trim());
        system_content.push_str("\n\n");
    }
    // One merged grounding block for BOTH surfaces (2026-08-29): the environment
    // sentence and the model identity used to ship as separate head items
    // ("System · environment" + "System · session facts" in the inspector);
    // they now ship as one block — env on the first line, "Current model: …" on
    // its own line as data, never "You are …" (identity-as-assertion amnesia,
    // §5.3). Day-granular only: the date is appended by `grounding_message`
    // below (it changes at most once a day, so the cached prefix survives to the
    // rollover); no CLOCK time (a per-minute value in the cached prefix busts
    // everything after it) and no approval posture (it can change mid-session,
    // §7.4). Chat and coding share the identical head.
    let _is_coding_session = project_root
        .as_deref()
        .is_some_and(|r| !r.trim().is_empty());
    let connection_kind = crate::grounding::connection_kind(base_url.as_deref());
    let dialect = provider_kind.as_deref().unwrap_or("openai");
    system_content.push_str(&grounding_message(&request.model, connection_kind, dialect, state.tz));
    // Pinned documents are NOT baked into the head — they ride as tail messages
    // (Tools::tail_context) that the provider adapters fold in AFTER the head,
    // at the end of the one leading system block. Two reasons: their contents
    // change whenever the user edits or re-pins a file, and anything volatile
    // this early would invalidate the cached head prefix; and up here there was
    // no signal when the set changed, so a file pinned ten turns ago and one
    // pinned ten seconds ago looked identical to the model. Record the current
    // set once per turn so the tail can report what changed.
    state.tools.note_context_files(
        project_root
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .map(std::path::Path::new),
        &context_files,
    );
    if !profile.trim().is_empty() {
        system_content.push_str("\n\n");
        system_content.push_str(profile.trim());
    }
    // In a coding session, orient the model: where it works, a work contract
    // (read before acting, verify after), and the memory model — plus the
    // per-OS env notes. Built in one place (grounding::coding_orientation) so
    // the inspector's read-only mirror can't drift from the live path.
    if let Some(root) = project_root.as_deref().filter(|r| !r.trim().is_empty()) {
        if !std::path::Path::new(root).is_dir() {
            return Err(format!(
                "Project folder not found: {root}\nThe directory may have been moved, renamed, or deleted. Please relocate or choose a new folder before continuing."
            ));
        }
        // Pre-warm the search corpus in the background while the model plans or generates:
        let root_buf = std::path::PathBuf::from(root);
        let cache_dir = app.path().app_data_dir().ok();
        tauri::async_runtime::spawn_blocking(move || {
            crate::tools::code_search::warm_cache(&root_buf, cache_dir.as_deref());
        });

        // Separator from the block above (profile/head). Kept at the call site,
        // NOT inside coding_orientation: the inspector shows the builder's output
        // as a standalone entry, so a baked-in leading blank line would render as
        // two phantom blank lines at the top of "Session · orientation".
        let auto_writes = state.permissions.get().auto_project_writes;
        system_content.push_str("\n\n");
        system_content.push_str(&coding_orientation(root, auto_writes));
        system_content.push_str(ENV_NOTES);
    }
    request.messages.insert(
        0,
        Message {
            role: Role::System,
            content: system_content,
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: None,
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        },
    );
    // Trim old tool results (small-window mitigation) only if the policy asks.
    // Pick the provider for this turn: a hosted connection (base_url + key) when
    // the request named one, else the default local LM Studio (state.provider).
    let provider = match base_url.as_deref() {
        Some(base) if !base.is_empty() => {
            let kind = provider_kind.as_deref().unwrap_or("openai");
            let key = key_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .and_then(|id| crate::keystore::get_key(id));
            crate::provider::make_provider(kind, base, key, mid_system_tail.unwrap_or(true))
        }
        _ => state.provider.clone(),
    };
    request.messages = expand_messages_with_segments(request.messages, read_prune_turns);

    agent::run_turn(
        provider,
        state.tools.clone(),
        request,
        enabled_tools,
        vision_on,
        context_files,
        project_root.map(std::path::PathBuf::from),
        token,
        &on_event,
    )
    .await
    .map_err(|e| e.to_string())
}

/// What the compaction summary must contain: a handover note the model writes to
/// its future self. Deliberately MINIMAL — the model is asked to carry decisions
/// forward (a copying task, what transformers do well) and to write a fresh,
/// standalone note each time, never to compute a delta against the previous note
/// (what they do badly). Regenerating means errors self-heal on the next pass
/// instead of fossilising; the code's only job is to bound the input (see
/// `render_for_summary`).
///
/// The note deliberately has NO `Constraints` section. A constraint would be the
/// highest-authority content in the note — it would survive every future compaction
/// and read as a session-long directive — and letting the MODEL decide what counts
/// as one was a planted-instruction vector, because this harness reads untrusted
/// input (source files, dependency code, tool output). If the user wants something
/// to bind, they type it in the note viewer (or the system prompt); the model never
/// writes binding content.
const COMPACTION_PROMPT: &str = "You are writing your memory of this session for \
     your future self, who will continue the work with no access to the original \
     transcript. Write one complete, standalone note.\n\n\
     What matters most, in order:\n\
     1. Decisions and their reasons — a decision without its reason gets undone. \
     When a previous note is shown, carry its decisions and reasons forward.\n\
     2. Specifics: exact paths, names, commands, errors, versions.\n\
     3. Honesty: say plainly what is broken or unverified.\n\n\
     Use short headed sections — e.g. Goal, Decisions, Done, Outstanding, Key \
     facts, Files touched — and include a brief timeline of the session.";

/// Render a slice of the conversation as plain text for the summariser.
///
/// Per-message truncation matters: a single tool result can be tens of thousands of
/// characters, and the summarisation call has its own context limit — one giant
/// `project_read` would otherwise crowd out the entire rest of the session. The
/// summary needs the SHAPE of what happened, not every byte.
/// Returns the rendered transcript plus how many of the OLDEST messages had to be
/// left out to stay inside the budget.
///
/// `budget` of `None` means no clip — nothing is dropped, no matter how long the
/// history (the "empty cap = no cap" rule: no cap for sending means no cap for
/// summarising either). `Some(n)` renders newest-first and stops at `n` chars, so
/// what survives in full is the most recent material — the part with the most
/// bearing on what happens next. The dropped head is then covered by the previous
/// handover note (see `compact_thread`), which is what makes repeated compaction
/// bounded: the budget stays FIXED as a session grows, and the share carried by
/// the prior summary grows instead.
fn render_for_summary(messages: &[Message], budget: Option<usize>) -> (String, usize) {
    const PER_MESSAGE: usize = 2000;
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() > n {
            format!("{} …[truncated]", s.chars().take(n).collect::<String>())
        } else {
            s.to_string()
        }
    };

    let mut blocks: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut included = 0usize;
    for m in messages.iter().rev() {
        let who = match m.role {
            Role::System => continue, // rebuilt each turn; never part of the history
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL RESULT",
        };
        let mut piece = String::new();
        if !m.content.trim().is_empty() {
            piece.push_str(&format!(
                "### {who}\n{}\n\n",
                clip(m.content.trim(), PER_MESSAGE)
            ));
        }
        for tc in &m.tool_calls {
            piece.push_str(&format!(
                "### TOOL CALL: {}\n{}\n\n",
                tc.name,
                clip(&tc.arguments, 400)
            ));
        }
        if piece.is_empty() {
            included += 1; // nothing to render, but it isn't "dropped" either
            continue;
        }
        // Always take at least one message, or a single oversized turn would render
        // an empty transcript and the summary would be built from nothing.
        if !blocks.is_empty() && budget.is_some_and(|b| used + piece.chars().count() > b) {
            break;
        }
        used += piece.chars().count();
        blocks.push(piece);
        included += 1;
    }
    blocks.reverse();
    (blocks.concat(), messages.len().saturating_sub(included))
}

/// What a compaction returns: the handover note, plus how many of the OLDEST
/// messages `render_for_summary` dropped to stay inside the budget. `dropped > 0`
/// means the span since the last compaction overshot the budget and those turns
/// are not captured by the note — the UI surfaces it rather than letting it read
/// as a silent success. Always 0 when there is no budget (see `compact_thread`).
#[derive(serde::Serialize)]
pub struct CompactResult {
    pub summary: String,
    pub dropped: usize,
}

/// Summarise a run of earlier messages into a handover note (the `/compact`
/// command). Returns the note; the webview stores it and substitutes it for those
/// messages when sending. Nothing is deleted — the user keeps the full transcript.
///
/// `budget` bounds the summariser's own input: `None` means no clip (the caller has
/// declared no context cap), `Some(n)` keeps the newest `n` chars of transcript.
/// When a prior note exists it is always shown in full and carried forward, so the
/// model copies decisions across rather than re-deriving them — regeneration, not
/// delta. Nothing here preserves a `## Sequence` spine verbatim; the model rewrites
/// the whole note, including its timeline, every time.
#[tauri::command]
pub async fn compact_thread(
    state: State<'_, AppState>,
    messages: Vec<Message>,
    prior_summary: Option<String>,
    budget: Option<usize>,
    model: String,
    provider_kind: Option<String>,
    base_url: Option<String>,
    key_id: Option<String>,
) -> Result<CompactResult, String> {
    use futures_util::StreamExt;
    use tokio_util::sync::CancellationToken;

    // Fresh cancellation token for this compaction; `stop_turn` cancels it.
    let token = CancellationToken::new();
    *state.cancel.lock().unwrap() = Some(token.clone());
    let _guard = ResetOnDrop {
        cancel: &state.cancel,
    };

    // Floor the budget so a bad caller can't reduce the transcript to nothing.
    // `None` (no cap) stays `None` — no cap means no clip for summarising too.
    let budget = budget.map(|b| b.max(8_000));
    let (transcript, dropped) = render_for_summary(&messages, budget);
    if transcript.trim().is_empty() {
        return Err("there's nothing to compact yet.".into());
    }

    // Regenerating the note from source + prior note each time is deliberate —
    // building each note from the previous note would be a copy of a copy, and the
    // first thing lost to repeated summarising is exactly what matters most: the
    // reasons behind decisions. So the prior note is shown in full so its decisions
    // can be carried forward (a copying task, which is what models do reliably),
    // and the raw transcript fills in what has happened since. Nothing is lost
    // unless the transcript overshot the budget (see `dropped`).
    let prior = prior_summary
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let body = match prior.as_deref() {
        Some(prior) => format!(
            "Your previous handover note is below, followed by the conversation since \
             it was written.\n\n## PREVIOUS HANDOVER NOTE\n{prior}\n\n## TRANSCRIPT SINCE \
             THEN\n{transcript}\n\nWrite one complete, standalone note covering both."
        ),
        None => format!(
            "Here is the session to compact:\n\n{transcript}\n\nWrite the handover note now."
        ),
    };

    let provider = match base_url.as_deref() {
        Some(base) if !base.is_empty() => {
            let kind = provider_kind.as_deref().unwrap_or("openai");
            let key = key_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .and_then(crate::keystore::get_key);
            // Compaction has no per-turn tail — the flag is irrelevant here.
            crate::provider::make_provider(kind, base, key, true)
        }
        _ => state.provider.clone(),
    };

    let plain = |role: Role, content: String| Message {
        role,
        content,
        images: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        created_at: None,
        reasoning: None,
        reasoning_signature: None,
        sources: Vec::new(),
        segments: None,
    };
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![
            plain(Role::System, COMPACTION_PROMPT.to_string()),
            plain(Role::User, body),
        ],
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        max_tokens: None,
        thinking_effort: None,
        max_context_tokens: None,
        // Compaction is a single call, not a tool loop.
        max_tool_calls: None,
        reasoning_style: None,
        sequential_tool_calls: None,
    };

    let mut stream = tokio::select! {
        _ = token.cancelled() => return Err("Compaction stopped by user.".into()),
        res = provider.stream_chat(request) => res.map_err(|e| e.to_string())?,
    };
    let mut summary = String::new();
    loop {
        tokio::select! {
            _ = token.cancelled() => return Err("Compaction stopped by user.".into()),
            item = stream.next() => {
                match item {
                    Some(StreamEvent::Delta { text }) => summary.push_str(&text),
                    Some(StreamEvent::Error { message }) => return Err(message),
                    Some(StreamEvent::Done { .. }) => break,
                    None => break,
                    _ => {}
                }
            }
        }
    }
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err("the model returned an empty summary — try again, or switch model.".into());
    }
    Ok(CompactResult { summary, dropped })
}

#[cfg(test)]
mod compaction_tests {
    use super::render_for_summary;
    use crate::provider::{Message, Role, ToolCall};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: None,
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        }
    }

    #[test]
    fn labels_roles_and_includes_tool_calls() {
        let mut assistant = msg(Role::Assistant, "I'll read the file.");
        assistant.tool_calls = vec![ToolCall {
            id: "1".into(),
            name: "project_read".into(),
            arguments: r#"{"path":"src/main.rs"}"#.into(),
        }];
        let (out, _) = render_for_summary(
            &[
                msg(Role::User, "fix the bug"),
                assistant,
                msg(Role::Tool, "fn main() {}"),
            ],
            Some(100_000),
        );

        assert!(out.contains("### USER\nfix the bug"), "{out}");
        assert!(out.contains("### ASSISTANT\nI'll read the file."), "{out}");
        assert!(out.contains("### TOOL CALL: project_read"), "{out}");
        assert!(
            out.contains("src/main.rs"),
            "tool args should survive: {out}"
        );
        assert!(out.contains("### TOOL RESULT\nfn main() {}"), "{out}");
    }

    /// The system prompt is rebuilt every turn and is never part of the history —
    /// summarising it would waste the budget describing our own instructions.
    #[test]
    fn skips_system_messages() {
        let (out, _) = render_for_summary(
            &[
                msg(Role::System, "SYSTEM PROMPT TEXT"),
                msg(Role::User, "hello"),
            ],
            Some(100_000),
        );
        assert!(!out.contains("SYSTEM PROMPT TEXT"), "{out}");
        assert!(out.contains("hello"));
    }

    /// One huge tool result must not crowd the whole session out of the
    /// summarisation call's own context window.
    #[test]
    fn truncates_oversized_messages() {
        let huge = "x".repeat(9000);
        let (out, _) = render_for_summary(
            &[msg(Role::Tool, &huge), msg(Role::User, "still here")],
            Some(100_000),
        );

        assert!(out.contains("…[truncated]"), "should mark the cut");
        assert!(out.len() < 5000, "should be bounded, got {}", out.len());
        assert!(
            out.contains("still here"),
            "later messages must survive the truncation of an earlier one"
        );
    }

    /// The budget must bound the summariser's own input — and it must keep the
    /// NEWEST material, since that's what bears on what happens next. The dropped
    /// head is what the previous handover note stands in for.
    #[test]
    fn budget_keeps_the_newest_and_reports_the_dropped_head() {
        let msgs: Vec<Message> = (0..40)
            .map(|i| {
                msg(
                    Role::User,
                    &format!("message number {i} {}", "x".repeat(500)),
                )
            })
            .collect();

        let (out, dropped) = render_for_summary(&msgs, Some(5_000));

        assert!(
            out.chars().count() <= 6_000,
            "over budget: {}",
            out.chars().count()
        );
        assert!(dropped > 0, "should report a dropped head");
        assert!(out.contains("message number 39"), "newest must survive");
        assert!(
            !out.contains("message number 0"),
            "oldest should be dropped"
        );
        assert_eq!(dropped + out.matches("### USER").count(), msgs.len());
    }

    /// A single oversized turn must still render something — otherwise the summary
    /// would be built from an empty transcript.
    #[test]
    fn always_includes_at_least_one_message() {
        let huge = msg(Role::User, &"x".repeat(50_000));
        let (out, _) = render_for_summary(&[huge], Some(1_000));
        assert!(!out.trim().is_empty());
    }

    #[test]
    fn empty_or_blank_history_renders_nothing() {
        assert!(render_for_summary(&[], Some(100_000)).0.trim().is_empty());
        assert!(render_for_summary(&[msg(Role::User, "   ")], Some(100_000))
            .0
            .trim()
            .is_empty());
    }

    /// No budget means no clip: nothing is dropped, no matter how long the history.
    /// This is the "empty cap = no cap" rule — no cap for sending means no cap for
    /// summarising either.
    #[test]
    fn no_budget_drops_nothing() {
        let msgs: Vec<Message> = (0..50)
            .map(|i| {
                msg(
                    Role::User,
                    &format!("message number {i} {}", "x".repeat(100)),
                )
            })
            .collect();
        let (out, dropped) = render_for_summary(&msgs, None);
        assert_eq!(dropped, 0, "no budget must drop nothing");
        assert!(
            out.contains("message number 0"),
            "oldest must survive: {out}"
        );
        assert!(
            out.contains("message number 49"),
            "newest must survive: {out}"
        );
    }
}


/// Read the current auto-approval permission policy (for the Settings UI).
#[tauri::command]
pub async fn get_permissions(
    state: State<'_, AppState>,
) -> Result<crate::permissions::PermData, String> {
    Ok(state.permissions.get())
}

/// Replace the auto-approval permission policy from the Settings UI.
#[tauri::command]
pub async fn set_permissions(
    state: State<'_, AppState>,
    auto_project_writes: bool,
    auto_commands: bool,
    command_whitelist: Vec<String>,
) -> Result<(), String> {
    state.permissions.set(crate::permissions::PermData {
        auto_project_writes,
        auto_commands,
        command_whitelist,
    });
    Ok(())
}


// --- Session MD viewer: browse/read/edit the project's own markdown docs ------
// These serve the HUMAN viewer, so they act directly (the human is the authority);
// the model's project_write stays approval-gated. Paths are sandboxed to the
// project root the same way the tool layer guards them (reject absolute / `..`).

fn is_doc(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".md") || l.ends_with(".markdown")
}

fn project_doc_path(root: &str, rel: &str) -> Result<std::path::PathBuf, String> {
    crate::sandbox::contain(std::path::Path::new(root), rel)
        .map_err(|why| format!("invalid path '{}' — {why}", rel.trim()))
}

/// Depth-first list of markdown docs under `dir`, POSIX-relative to `root`,
/// skipping noise dirs, bounded by `cap`.
fn walk_docs(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>, cap: usize) {
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
        if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_noise_dir(name) {
                continue;
            }
            walk_docs(&p, root, out, cap);
        } else if is_doc(p.file_name().and_then(|n| n.to_str()).unwrap_or("")) {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn is_noise_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
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
            | ".godot"
    )
}

/// Depth-first list of all project files under `dir`, POSIX-relative to `root`,
/// skipping noise dirs, bounded by `cap`.
fn walk_files(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>, cap: usize) {
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
        if e.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_noise_dir(name) {
                continue;
            }
            walk_files(&p, root, out, cap);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Check if a directory exists on disk.
#[tauri::command]
pub fn check_directory_exists(path: String) -> bool {
    std::path::Path::new(&path).is_dir()
}

/// Batch check whether directories exist on disk.
#[tauri::command]
pub fn check_directories_exist(paths: Vec<String>) -> Vec<bool> {
    paths
        .into_iter()
        .map(|p| std::path::Path::new(&p).is_dir())
        .collect()
}

/// List all files in a project for the code file tree (POSIX-relative paths).
#[tauri::command]
pub async fn list_project_files(project_root: String) -> Result<Vec<String>, String> {
    let root = std::path::Path::new(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let mut out = Vec::new();
    walk_files(root, root, &mut out, 5000);
    out.sort();
    Ok(out)
}

/// Read one project file for the in-drawer code viewer (sandboxed to the project root).
#[tauri::command]
pub async fn read_project_file(project_root: String, path: String) -> Result<String, String> {
    let p = project_doc_path(&project_root, &path)?;
    crate::tools::read_text_capped(&p)
}

/// List the markdown docs in a project, for the session MD viewer.
#[tauri::command]
pub async fn list_project_docs(project_root: String) -> Result<Vec<String>, String> {
    let root = std::path::Path::new(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let mut out = Vec::new();
    walk_docs(root, root, &mut out, 500);
    Ok(out)
}

/// Read one project markdown doc (sandboxed to the project root).
#[tauri::command]
pub async fn read_project_doc(project_root: String, path: String) -> Result<String, String> {
    let p = project_doc_path(&project_root, &path)?;
    std::fs::read_to_string(&p).map_err(|e| format!("couldn't read {path}: {e}"))
}

/// Write one project markdown doc from the viewer (a human edit — direct).
#[tauri::command]
pub async fn write_project_doc(
    state: State<'_, AppState>,
    project_root: String,
    path: String,
    content: String,
) -> Result<(), String> {
    if !is_doc(&path) {
        return Err("only markdown docs can be edited here".into());
    }
    let p = project_doc_path(&project_root, &path)?;
    // Capture pre-edit snapshot so session diff and restore work for manual edits too
    let _ = state.snapshots.capture(&p);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&p, content).map_err(|e| format!("couldn't write {path}: {e}"))
}

/// Helper: compute list of files modified, created, or deleted relative to session snapshots.
pub fn session_modified_files(
    snapshots: &crate::snapshots::Snapshots,
    project_root: &Path,
) -> Vec<String> {
    let captured = snapshots.captured_paths();
    let mut modified = Vec::new();

    for path in captured {
        if !path.starts_with(project_root) {
            continue;
        }
        let rel = match path.strip_prefix(project_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let current = std::fs::read_to_string(&path).ok();
        let is_changed = match (snapshots.original(&path), current) {
            (Some(crate::snapshots::Original::Absent), Some(_)) => true,
            (Some(crate::snapshots::Original::Absent), None) => false,
            (Some(crate::snapshots::Original::Content(orig)), Some(cur)) => orig != cur,
            (Some(crate::snapshots::Original::Content(_)), None) => true,
            (None, _) => false,
        };
        if is_changed {
            modified.push(rel);
        }
    }
    modified.sort();
    modified
}

/// Helper: compute unified diff of a specific file against its pre-session snapshot.
pub fn file_diff(
    snapshots: &crate::snapshots::Snapshots,
    project_root: &Path,
    rel_path: &str,
) -> Result<Option<String>, String> {
    let root_str = project_root.to_string_lossy();
    let p = project_doc_path(&root_str, rel_path)?;
    let orig = snapshots.original(&p);
    let current = std::fs::read_to_string(&p).ok();

    match (orig, current) {
        (Some(crate::snapshots::Original::Absent), Some(cur)) => {
            let diff = similar::TextDiff::from_lines("", &cur);
            Ok(Some(format!("(new file)\n{}", diff.unified_diff())))
        }
        (Some(crate::snapshots::Original::Absent), None) => Ok(None),
        (Some(crate::snapshots::Original::Content(orig_text)), Some(cur)) => {
            if orig_text == cur {
                Ok(None)
            } else {
                let diff = similar::TextDiff::from_lines(&orig_text, &cur);
                Ok(Some(diff.unified_diff().to_string()))
            }
        }
        (Some(crate::snapshots::Original::Content(orig_text)), None) => {
            Ok(Some(format!(
                "(deleted — was {} lines)",
                orig_text.lines().count()
            )))
        }
        (None, _) => Ok(None),
    }
}

/// List relative paths of files modified, added, or deleted in the project this session.
#[tauri::command]
pub async fn get_session_modified_files(
    state: State<'_, AppState>,
    project_root: String,
) -> Result<Vec<String>, String> {
    let root = Path::new(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(session_modified_files(&state.snapshots, root))
}

/// Get unified diff of a specific project file against its pre-session snapshot.
#[tauri::command]
pub async fn get_file_diff(
    state: State<'_, AppState>,
    project_root: String,
    path: String,
) -> Result<Option<String>, String> {
    let root = Path::new(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    file_diff(&state.snapshots, root, &path)
}

/// Pre-warms the project search corpus in the background so code_search calls are instant.
#[tauri::command]
pub async fn warm_search_index(
    app: tauri::AppHandle,
    project_root: String,
) -> Result<(), String> {
    let trimmed = project_root.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let root = Path::new(trimmed);
    if !root.is_dir() {
        return Ok(());
    }
    let root_buf = root.to_path_buf();
    let cache_dir = app.path().app_data_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        crate::tools::code_search::warm_cache(&root_buf, cache_dir.as_deref());
    });
    Ok(())
}

/// Starts background vector indexing of the entire repository for /index.
#[tauri::command]
pub async fn start_repo_indexing(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_root: String,
    on_progress: Channel<crate::tools::code_search::IndexProgress>,
) -> Result<(), String> {
    let trimmed = project_root.trim();
    if trimmed.is_empty() {
        return Err("Project root is empty".into());
    }
    let root = Path::new(trimmed);
    if !root.is_dir() {
        return Err(format!("Project folder not found: {trimmed}"));
    }

    let token = CancellationToken::new();
    {
        let mut guard = state.index_cancel.lock().map_err(|e| e.to_string())?;
        if let Some(old) = guard.replace(token.clone()) {
            old.cancel();
        }
    }

    let root_buf = root.to_path_buf();
    let cache_dir = app.path().app_data_dir().ok();
    let token_clone = token.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let res = crate::tools::code_search::index_repository_full(
            &root_buf,
            cache_dir.as_deref(),
            &token_clone,
            |progress| {
                let _ = on_progress.send(progress);
            },
        );
        if let Err(e) = res {
            let _ = on_progress.send(crate::tools::code_search::IndexProgress {
                status: "error".into(),
                current: 0,
                total: 0,
                message: Some(format!("Indexing error: {e}")),
            });
        }
    });

    Ok(())
}

/// Stops active background repository indexing.
#[tauri::command]
pub fn stop_repo_indexing(state: State<'_, AppState>) -> bool {
    if let Ok(guard) = state.index_cancel.lock() {
        if let Some(token) = guard.as_ref() {
            token.cancel();
            return true;
        }
    }
    false
}

/// Returns the current readiness and disk usage of the local semantic embedding model.
#[tauri::command]
pub fn get_embedding_model_status(
    app: tauri::AppHandle,
) -> Result<crate::tools::code_search::EmbeddingModelStatus, String> {
    let cache_dir = app.path().app_data_dir().ok();
    Ok(crate::tools::code_search::embedding_model_status(cache_dir.as_deref()))
}

/// Downloads and initializes the bge-small model on demand.
#[tauri::command]
pub async fn download_embedding_model(
    app: tauri::AppHandle,
) -> Result<crate::tools::code_search::EmbeddingModelStatus, String> {
    let cache_dir = app.path().app_data_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        crate::tools::code_search::download_embedding_model(cache_dir.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Deletes the cached embedding model from disk to reclaim disk space.
#[tauri::command]
pub async fn delete_embedding_model(
    app: tauri::AppHandle,
) -> Result<crate::tools::code_search::EmbeddingModelStatus, String> {
    let cache_dir = app.path().app_data_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        crate::tools::code_search::delete_embedding_model(cache_dir.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reveals the models cache directory in the OS file manager.
#[tauri::command]
pub fn reveal_models_dir(app: tauri::AppHandle) -> Result<(), String> {
    let cache_dir = app.path().app_data_dir().ok();
    let dir = crate::tools::code_search::resolve_model_cache_dir(cache_dir.as_deref());
    let _ = std::fs::create_dir_all(&dir);

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(&dir);
        c
    };

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&dir);
        c
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&dir);
        c
    };

    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Clears the cancel slot when a turn completes, whatever the outcome, so a
/// `stop_turn` arriving after completion is a clean no-op instead of cancelling
/// a dead token (or, once turns can overlap, the wrong live one). Dropped at the
/// end of `send_chat` — i.e. exactly when the turn ends.
struct ResetOnDrop<'a> {
    cancel: &'a Mutex<Option<CancellationToken>>,
}
impl Drop for ResetOnDrop<'_> {
    fn drop(&mut self) {
        *self.cancel.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::ResetOnDrop;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn reset_on_drop_clears_the_slot() {
        let slot = Mutex::new(Some(CancellationToken::new()));
        {
            let _guard = ResetOnDrop { cancel: &slot };
            assert!(slot.lock().unwrap().is_some());
        }
        assert!(slot.lock().unwrap().is_none());
    }
}

/// Interrupt the running turn (Stop, spec §10). Keeps whatever already streamed.
/// Only the turn actually in flight is cancelled; a stop with no running turn is
/// a clean no-op — and reports `false` so the UI can distinguish "requested"
/// from "actually cancelling" (it shows the pending "Stopping…" state only for
/// the latter, otherwise a stop landing before `send_chat` registered the token
/// would latch the composer's Stop button into a disabled state for the whole
/// turn that follows).
#[tauri::command]
pub fn stop_turn(state: State<'_, AppState>) -> bool {
    match state.cancel.lock().unwrap().as_ref() {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

/// Current date/time in the configured timezone, for the UI readout.
#[tauri::command]
pub fn current_datetime(state: State<'_, AppState>) -> String {
    display_datetime(state.tz)
}

/// Answer a pending tool approval (e.g. an MD-write diff). Wakes the paused tool.
#[tauri::command]
pub fn resolve_edit(state: State<'_, AppState>, id: u64, decision: String) {
    state.approvals.resolve(id, decision);
}

/// Deliver a sandboxed run's captured output back to the waiting run_python tool.
#[tauri::command]
pub fn resolve_code(state: State<'_, AppState>, id: u64, output: String) {
    state.code_runs.resolve(id, output);
}

/// Store an API key for a connection in the OS keychain (backend-side only — the
/// key never returns to the webview).
#[tauri::command]
pub fn set_api_key(id: String, key: String) -> Result<(), String> {
    crate::keystore::set_key(&id, &key).map_err(|e| e.to_string())
}

/// Whether a connection has a key stored (the only key fact the UI ever learns).
#[tauri::command]
pub fn has_api_key(id: String) -> bool {
    crate::keystore::has_key(&id)
}

/// Remove a connection's stored key.
#[tauri::command]
pub fn delete_api_key(id: String) -> Result<(), String> {
    crate::keystore::delete_key(&id).map_err(|e| e.to_string())
}

/// Export assembled markdown to the notes folder (user-triggered; no approval).
/// Returns the absolute path written, for the UI confirmation.
#[tauri::command]
pub fn export_markdown(
    state: State<'_, AppState>,
    name: String,
    content: String,
) -> Result<String, String> {
    state
        .tools
        .export_markdown(&name, &content)
        .map(|p| p.display().to_string())
}

/// Write raw file content to an absolute path chosen by the user via file dialog.
#[tauri::command]
pub fn save_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content.as_bytes()).map_err(|e| e.to_string())
}

/// List the markdown files in the notes folder, for the Files panel.
#[tauri::command]
pub fn list_notes(state: State<'_, AppState>) -> Vec<crate::tools::NoteInfo> {
    state.tools.list_notes()
}

/// List the HTML pages in the notes folder, for the Files panel's Pages section.
#[tauri::command]
pub fn list_pages(state: State<'_, AppState>) -> Vec<crate::tools::NoteInfo> {
    state.tools.list_pages()
}

/// Open a saved HTML page with the OS default handler (the browser).
#[tauri::command]
pub fn open_page(state: State<'_, AppState>, name: String) -> Result<(), String> {
    state.tools.open_in_browser(&name)
}

/// Read one markdown note by relative path (sandboxed to the notes root).
#[tauri::command]
pub fn read_note(state: State<'_, AppState>, name: String) -> Result<String, String> {
    state.tools.read_note(&name)
}

/// Delete a note from the notes folder.
#[tauri::command]
pub fn delete_note(state: State<'_, AppState>, name: String) -> Result<(), String> {
    state.tools.delete_note(&name)
}

/// Rename a note within the notes folder; returns the final (sandboxed) name.
#[tauri::command]
pub fn rename_note(state: State<'_, AppState>, old: String, new: String) -> Result<String, String> {
    state.tools.rename_note(&old, &new)
}

/// Reveal a note in the OS file manager (Explorer / Finder / file manager).
#[tauri::command]
pub fn reveal_note(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let full = state.tools.note_full_path(&name)?;
    reveal_in_file_manager(&full)
}

/// Open a directory in the OS file manager (Explorer / Finder / file manager).
#[tauri::command]
pub fn reveal_folder(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.exists() {
        return Err(format!("'{}' does not exist", path));
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        let win_path = p.display().to_string().replace('/', "\\");
        c.arg(&win_path);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(p);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(p);
        c
    };
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}


/// Open the OS file manager with the given file selected/revealed.
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer");
        // raw_arg avoids Rust's quoting so explorer parses the `/select,` switch;
        // the path itself is quoted to survive spaces (e.g. "My Project"). Backslash-
        // normalize first: a subfolder note's path carries a forward slash (the model
        // writes `sub/foo.md`), and `explorer /select,` silently rejects forward
        // slashes — opening Documents instead of revealing the file.
        let win_path = path.display().to_string().replace('/', "\\");
        c.raw_arg(format!("/select,\"{win_path}\""));
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

// --- Persistence (spec §8/§9) ----------------------------------------------

#[tauri::command]
pub fn list_threads(state: State<'_, AppState>) -> Result<Vec<ThreadInfo>, String> {
    state.store.list_threads().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_thread(state: State<'_, AppState>) -> Result<i64, String> {
    state
        .store
        .create_thread("New chat")
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_thread(state: State<'_, AppState>, thread_id: i64) -> Result<Vec<Message>, String> {
    state
        .store
        .load_messages(thread_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_thread(
    state: State<'_, AppState>,
    thread_id: i64,
    title: String,
    messages: Vec<Message>,
) -> Result<(), String> {
    state
        .store
        .save_thread(thread_id, &title, &messages)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn duplicate_thread(
    state: State<'_, AppState>,
    thread_id: i64,
    up_to: i64,
) -> Result<i64, String> {
    state
        .store
        .duplicate_thread(thread_id, up_to)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_thread(state: State<'_, AppState>, thread_id: i64) -> Result<(), String> {
    state
        .store
        .delete_thread(thread_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn rename_thread(
    state: State<'_, AppState>,
    thread_id: i64,
    title: String,
) -> Result<(), String> {
    state
        .store
        .rename_thread(thread_id, &title)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_trashed(state: State<'_, AppState>) -> Result<Vec<ThreadInfo>, String> {
    state.store.list_trashed().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn restore_thread(state: State<'_, AppState>, thread_id: i64) -> Result<(), String> {
    state
        .store
        .restore_thread(thread_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn purge_thread(
    state: State<'_, AppState>,
    thread_id: i64,
) -> Result<(), String> {
    state
        .store
        .purge_thread(thread_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn purge_trashed(
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.store.purge_trashed().map_err(|e| e.to_string())
}

/// Load a thread's per-chat config blob (JSON string).
#[tauri::command]
pub fn load_thread_config(state: State<'_, AppState>, thread_id: i64) -> Result<String, String> {
    state
        .store
        .load_config(thread_id)
        .map_err(|e| e.to_string())
}

/// Persist a thread's per-chat config blob (JSON string).
#[tauri::command]
pub fn save_thread_config(
    state: State<'_, AppState>,
    thread_id: i64,
    config: String,
) -> Result<(), String> {
    state
        .store
        .save_config(thread_id, &config)
        .map_err(|e| e.to_string())
}

// --- Context inspector + token readout (spec §10) --------------------------

#[derive(Serialize)]
pub struct ContextSection {
    pub label: String,
    pub tokens: usize,
    pub content: String,
}

#[derive(Serialize)]
pub struct ContextPreview {
    pub sections: Vec<ContextSection>,
    pub total_tokens: usize,
}

/// Rough token estimate (~4 chars/token). Exact tokenization is model-specific;
/// this is for the attention-budget readout, not billing.
fn estimate_tokens(s: &str) -> usize {
    ((s.chars().count() as f64) / 4.0).ceil() as usize
}

/// Stop a managed background process by its id (user-initiated termination from terminal UI).
#[tauri::command]
pub fn stop_managed_process(state: State<'_, AppState>, id: u64) -> Result<String, String> {
    state.processes.stop(id)
}

fn summarize_tool_call(name: &str, raw_args: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_args) {
        if let Some(obj) = val.as_object() {
            for key in &["path", "file_path", "command", "query", "pattern", "url", "note_name", "scene"] {
                if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
                    let truncated = if v.len() > 60 {
                        format!("{}...", &v[..57])
                    } else {
                        v.to_string()
                    };
                    return format!("{}: {}", name, truncated);
                }
            }
        }
    }
    name.to_string()
}

fn format_tool_section_content(calls: &[String]) -> String {
    if calls.is_empty() {
        return String::new();
    }
    let count = calls.len();
    let header = if count == 1 {
        "[1 tool call]".to_string()
    } else {
        format!("[{} tool calls]", count)
    };
    let mut lines = vec![header];
    if count <= 40 {
        for c in calls {
            lines.push(format!("• {}", c));
        }
    } else {
        for c in &calls[..35] {
            lines.push(format!("• {}", c));
        }
        lines.push(format!("• ... and {} more tool calls", count - 35));
    }
    lines.join("\n")
}

/// Build the exact context that would be assembled for the next turn — the one
/// leading system block (head parts, then the tail parts folded in at its end),
/// the message history, and the offered tools — with per-section token
/// estimates. Mirrors what `send_chat` + the agent loop send.
#[tauri::command]
pub fn preview_context(
    state: State<'_, AppState>,
    messages: Vec<Message>,
    enabled_tools: Vec<String>,
    context_files: Vec<String>,
    system_prompt: String,
    profile: String,
    project_root: Option<String>,
    // Mirrors send_chat: the inspector must count what the live path would
    // actually send. Unread — see the note on send_chat's `vision_on`.
    _vision_on: bool,
    // The head block (§3.1) needs the same session facts send_chat has. None
    // when the composer has no model selected yet (e.g. mid connection switch) —
    // the block then omits the identity line.
    model: Option<String>,
    conn_kind: Option<String>,
    conn_base_url: Option<String>,
    // The send-cap (max_context_tokens) — mirrors send_chat so the
    // assembled-context tail line can show the budget the live path uses.
    _max_context_tokens: Option<u32>,
    read_prune_turns: Option<u32>,
) -> ContextPreview {
    let mut sections = Vec::new();

    if !system_prompt.trim().is_empty() {
        sections.push(ContextSection {
            label: "System · instructions".into(),
            tokens: estimate_tokens(system_prompt.trim()),
            content: system_prompt.trim().to_string(),
        });
    }

    // Mirror send_chat's merged head line (keep in sync): env + model identity
    // as ONE entry for both surfaces — the old two-item split ("System ·
    // environment" + "System · session facts") is gone (2026-08-29). Day-granular
    // date rides here (grounding_message); the tail carries no date or clock.
    let connection_kind = crate::grounding::connection_kind(conn_base_url.as_deref());
    let dialect = conn_kind.as_deref().unwrap_or("openai");
    let model_name = model.as_deref().unwrap_or("");
    let grounding = grounding_message(model_name, connection_kind, dialect, state.tz);
    sections.push(ContextSection {
        label: "System · environment & model".into(),
        tokens: estimate_tokens(&grounding),
        content: grounding,
    });

    if !profile.trim().is_empty() {
        sections.push(ContextSection {
            label: "System · about the user".into(),
            tokens: estimate_tokens(profile.trim()),
            content: profile.trim().to_string(),
        });
    }

    // Session HUD — the orientation + index that send_chat injects for a coding
    // session (the session's de-facto "Manifest"). Built by the same
    // grounding::coding_orientation as the live path, so the read-only inspector
    // shows the real, full context + its token cost without touching send_chat's
    // live model path.
    if let Some(root) = project_root.as_deref().filter(|r| !r.trim().is_empty()) {
        let auto_writes = state.permissions.get().auto_project_writes;
        let mut orientation = coding_orientation(root, auto_writes);
        orientation.push_str(ENV_NOTES);
        sections.push(ContextSection {
            label: "Session · orientation".into(),
            tokens: estimate_tokens(&orientation),
            content: orientation,
        });
    }

    let enabled = enabled_tools.clone();
    let tools: Vec<_> = state
        .tools
        .definitions()
        .into_iter()
        .filter(|t| enabled.iter().any(|e| e == &t.name))
        .collect();

    // The per-turn tail parts — the pin change-note, then pinned files. Each
    // rides as its own system message (agent.rs `append_tail`); the provider
    // adapters fold EVERY system message — the head sections above first, then
    // these parts, in order — into ONE leading system block before the
    // transcript (2026-08-30 — NOTES.md "single system block"). Grouped here
    // with the head sections so the inspector reads like the wire; built by the
    // same functions the live path uses, so it can't drift from reality. (The
    // approval posture, tool log and date/time lines that used to sit here were
    // removed rather than re-gated — NOTES.md "Head/tail discipline".)

    let messages = expand_messages_with_segments(messages, read_prune_turns);

    let total_user_turns = messages.iter().filter(|m| m.role == Role::User).count();
    let mut user_turn_idx = 0usize;
    let mut pending_tool_calls: Vec<String> = Vec::new();
    let mut current_tool_tokens = 0;

    for m in &messages {
        if m.role == Role::Tool {
            current_tool_tokens += estimate_tokens(&m.content);
            continue;
        }

        let text = m.content.clone();
        let has_displayable_assistant = m.role == Role::Assistant && !text.trim().is_empty();
        let is_user_or_system = m.role == Role::User || m.role == Role::System;

        if (has_displayable_assistant || is_user_or_system) && !pending_tool_calls.is_empty() {
            sections.push(ContextSection {
                label: "Tool Execution (Requests + Results)".to_string(),
                tokens: current_tool_tokens,
                content: format_tool_section_content(&pending_tool_calls),
            });
            pending_tool_calls.clear();
            current_tool_tokens = 0;
        }

        if m.role == Role::User {
            user_turn_idx += 1;
            let divider_label = if user_turn_idx == total_user_turns {
                format!("Turn {} (latest)", user_turn_idx)
            } else {
                format!("Turn {}", user_turn_idx)
            };
            sections.push(ContextSection {
                label: divider_label,
                tokens: 0,
                content: String::new(),
            });
        }

        if !m.tool_calls.is_empty() {
            let tc_json = serde_json::to_string(&m.tool_calls).unwrap_or_default();
            current_tool_tokens += estimate_tokens(&tc_json);
            for tc in &m.tool_calls {
                pending_tool_calls.push(summarize_tool_call(&tc.name, &tc.arguments));
            }
        }

        if m.role == Role::Assistant && !has_displayable_assistant {
            continue;
        }

        let label = match m.role {
            Role::System => "System · compaction summary".to_string(),
            Role::User => format!("Turn {} · user", user_turn_idx),
            Role::Assistant => "assistant".to_string(),
            Role::Tool => unreachable!(),
        };

        let tokens = estimate_tokens(text.trim());
        let display_content = text.trim().to_string();

        sections.push(ContextSection {
            label,
            tokens,
            content: display_content,
        });
    }

    if !pending_tool_calls.is_empty() {
        sections.push(ContextSection {
            label: "Tool Execution (Requests + Results)".to_string(),
            tokens: current_tool_tokens,
            content: format_tool_section_content(&pending_tool_calls),
        });
    }

    if project_root.is_some() || !context_files.is_empty() || !tools.is_empty() {
        sections.push(ContextSection {
            label: "Tail context & offered tools".to_string(),
            tokens: 0,
            content: String::new(),
        });
    }

    let root = project_root
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .map(std::path::Path::new);
    

    
    if let Some(note) = state.tools.context_change(root) {
        sections.push(ContextSection {
            label: "Tail · pin change note".into(),
            tokens: estimate_tokens(&note),
            content: note,
        });
    }
    
    if let Some(tail) = state.tools.tail_context(root, &context_files, true) {
        sections.push(ContextSection {
            label: "Tail · pinned files".into(),
            tokens: estimate_tokens(&tail),
            content: tail,
        });
    }

    // Count the WIRE, show the readable form — they are different strings. The
    // request carries minified OpenAI envelopes; this panel used to measure
    // pretty-printed internal structs instead, counting indentation and newlines
    // that never leave the machine, so the tool block has been reading high ever
    // since. `to_openai_tools` is the same function the sender uses, so the count
    // cannot drift from what is actually sent.
    let wire = serde_json::to_string(&crate::provider::lmstudio::to_openai_tools(&tools))
        .unwrap_or_default();
    let shown = serde_json::to_string_pretty(&tools).unwrap_or_default();
    sections.push(ContextSection {
        label: format!("Tools · {} offered", tools.len()),
        tokens: estimate_tokens(&wire),
        content: shown,
    });



    let total_tokens = sections.iter().map(|s| s.tokens).sum();
    ContextPreview {
        sections,
        total_tokens,
    }
}

#[cfg(test)]
mod project_files_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "outpost_proj_files_{name}_{}_{}",
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

    #[tokio::test]
    async fn test_list_and_read_project_files() {
        let temp = TestDir::new("list_read");
        let root = temp.path();

        // Create files in nested dirs
        std::fs::create_dir_all(root.join("src").join("nested")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();

        std::fs::write(root.join("README.md"), "# Hello").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src").join("nested").join("lib.rs"), "pub fn foo() {}").unwrap();

        // Ignored noise dir files
        std::fs::write(root.join(".git").join("config"), "git cfg").unwrap();
        std::fs::write(root.join("node_modules").join("foo.js"), "foo").unwrap();
        std::fs::write(root.join("target").join("build.log"), "log").unwrap();

        let files = list_project_files(root.to_string_lossy().to_string())
            .await
            .unwrap();

        assert_eq!(
            files,
            vec![
                "Cargo.toml",
                "README.md",
                "src/main.rs",
                "src/nested/lib.rs",
            ]
        );

        // Test reading code file
        let code = read_project_file(
            root.to_string_lossy().to_string(),
            "src/main.rs".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(code, "fn main() {}");

        // Test sandbox containment
        let err = read_project_file(
            root.to_string_lossy().to_string(),
            "../outside.txt".to_string(),
        )
        .await;
        assert!(err.is_err());
    }

    #[test]
    fn test_session_modified_files_and_diff() {
        let temp = TestDir::new("diff_tests");
        let root = temp.path();
        let snap_dir = temp.path().join("snaps");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let snapshots = crate::snapshots::Snapshots::open(snap_dir);

        // 1. Existing file untouched
        let untouched = root.join("untouched.rs");
        std::fs::write(&untouched, "fn foo() {}\n").unwrap();

        // 2. Existing file captured then modified
        let modified = root.join("src").join("mod.rs");
        std::fs::create_dir_all(modified.parent().unwrap()).unwrap();
        std::fs::write(&modified, "line 1\nline 2\n").unwrap();
        snapshots.capture(&modified).unwrap();
        std::fs::write(&modified, "line 1\nline 2 edited\nline 3 added\n").unwrap();

        // 3. New file created this session
        let created = root.join("new.txt");
        snapshots.capture(&created).unwrap();
        std::fs::write(&created, "brand new content\n").unwrap();

        // 4. File captured, edited, then restored back to original
        let reverted = root.join("reverted.txt");
        std::fs::write(&reverted, "original\n").unwrap();
        snapshots.capture(&reverted).unwrap();
        std::fs::write(&reverted, "different\n").unwrap();
        std::fs::write(&reverted, "original\n").unwrap();

        // Check modified files list
        let mod_list = session_modified_files(&snapshots, root);
        assert_eq!(mod_list, vec!["new.txt", "src/mod.rs"]);

        // Check diff for modified file
        let diff = file_diff(&snapshots, root, "src/mod.rs").unwrap();
        assert!(diff.is_some());
        let diff_str = diff.unwrap();
        assert!(diff_str.contains("+line 2 edited"));
        assert!(diff_str.contains("+line 3 added"));
        assert!(diff_str.contains("-line 2"));

        // Check diff for new file
        let new_diff = file_diff(&snapshots, root, "new.txt").unwrap();
        assert!(new_diff.is_some());
        let new_diff_str = new_diff.unwrap();
        assert!(new_diff_str.starts_with("(new file)"));
        assert!(new_diff_str.contains("+brand new content"));

        // Check diff for untouched file
        let untouched_diff = file_diff(&snapshots, root, "untouched.rs").unwrap();
        assert!(untouched_diff.is_none());

        // Check diff for reverted file
        let reverted_diff = file_diff(&snapshots, root, "reverted.txt").unwrap();
        assert!(reverted_diff.is_none());
    }

    #[test]
    fn test_expand_messages_with_segments_chronological_order() {
        let segs = serde_json::json!([
            {
                "kind": "tool",
                "id": "call_web_1",
                "name": "web_search",
                "args": "{\"query\":\"rust 2024\"}",
                "output": "Search result: Rust 2024 edition released"
            },
            {
                "kind": "tool",
                "id": "call_read_2",
                "name": "read_note",
                "args": "{\"path\":\"doc.md\"}",
                "output": "# Document contents"
            }
        ]);

        let input = vec![
            Message {
                role: Role::User,
                content: "Find rust info and read doc".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(100),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Here is the summary of rust and the doc.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(101),
                reasoning: Some("Thinking about searching...".into()),
                reasoning_signature: Some("sig123".into()),
                sources: Vec::new(),
                segments: Some(segs),
            },
        ];

        let expanded = expand_messages_with_segments(input, None);

        // Expect 4 messages:
        // 0: User ("Find rust info...")
        // 1: Assistant (tool calls: 2, empty content, reasoning preserved)
        // 2: Tool ("call_web_1" output)
        // 3: Tool ("call_read_2" output)
        // 4: Assistant (final content, 0 tool calls, reasoning None)
        assert_eq!(expanded.len(), 5);

        assert_eq!(expanded[0].role, Role::User);
        assert_eq!(expanded[0].content, "Find rust info and read doc");

        // 1: Assistant tool caller
        assert_eq!(expanded[1].role, Role::Assistant);
        assert_eq!(expanded[1].content, "");
        assert_eq!(expanded[1].tool_calls.len(), 2);
        assert_eq!(expanded[1].tool_calls[0].id, "call_web_1");
        assert_eq!(expanded[1].tool_calls[0].name, "web_search");
        assert_eq!(expanded[1].tool_calls[1].id, "call_read_2");
        assert!(expanded[1].reasoning.is_none());
        assert!(expanded[1].reasoning_signature.is_none());

        // 2: Tool result 1
        assert_eq!(expanded[2].role, Role::Tool);
        assert_eq!(expanded[2].tool_call_id.as_deref(), Some("call_web_1"));
        assert_eq!(expanded[2].content, "Search result: Rust 2024 edition released");

        // 3: Tool result 2
        assert_eq!(expanded[3].role, Role::Tool);
        assert_eq!(expanded[3].tool_call_id.as_deref(), Some("call_read_2"));
        assert_eq!(expanded[3].content, "# Document contents");

        // 4: Assistant final synthesized response
        assert_eq!(expanded[4].role, Role::Assistant);
        assert_eq!(expanded[4].content, "Here is the summary of rust and the doc.");
        assert!(expanded[4].tool_calls.is_empty());
        assert!(expanded[4].reasoning.is_none());
        assert!(expanded[4].reasoning_signature.is_none());
    }

    #[test]
    fn test_expand_messages_with_segments_differentiated_pruning() {
        let long_web = "<html><body>Long web page content that exceeds one hundred and twenty bytes for testing pruning functionality in Outpost</body></html>";
        let long_code = "fn main() {\n    println!(\"This is a long source code file content that exceeds one hundred and twenty bytes to test retention\");\n}";
        let short_ack = "{\"ok\":true}";

        let segs_turn1 = serde_json::json!([
            {
                "kind": "tool",
                "id": "fetch_1",
                "name": "web_fetch",
                "args": "{\"url\":\"https://example.com/docs\"}",
                "output": long_web
            },
            {
                "kind": "tool",
                "id": "read_1",
                "name": "project_read",
                "args": "{\"path\":\"src/main.rs\"}",
                "output": long_code
            },
            {
                "kind": "tool",
                "id": "edit_1",
                "name": "project_edit",
                "args": "{\"path\":\"src/main.rs\"}",
                "output": short_ack
            }
        ]);

        let segs_turn2 = serde_json::json!([
            {
                "kind": "tool",
                "id": "read_2",
                "name": "project_read",
                "args": "{\"path\":\"src/lib.rs\"}",
                "output": long_code
            }
        ]);

        // Case A: At Turn 2 (User 1 -> Assistant 1 -> User 2)
        // turns_ago for Assistant 1 is 1:
        // - web_fetch (turns_ago >= 1) -> PRUNED to stub
        // - project_read (turns_ago < 2) -> KEPT INTACT (2-turn window)
        // - project_edit -> KEPT INTACT
        let input_turn2 = vec![
            Message {
                role: Role::User,
                content: "Fetch web and read main".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(1),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Done fetching and reading.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(2),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: Some(segs_turn1.clone()),
            },
            Message {
                role: Role::User,
                content: "Now read lib".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(3),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
        ];

        let expanded_turn2 = expand_messages_with_segments(input_turn2.clone(), Some(2));
        // Messages:
        // 0: User 1
        // 1: Assistant 1 (calls)
        // 2: Tool fetch_1
        // 3: Tool read_1
        // 4: Tool edit_1
        // 5: Assistant 1 (text)
        // 6: User 2
        assert_eq!(expanded_turn2.len(), 7);

        // fetch_1 (1 turn old) must be stubbed:
        assert!(expanded_turn2[2].content.starts_with("receipt — web_fetch(\"https://example.com/docs\") returned"));
        assert!(expanded_turn2[2].content.ends_with("in an earlier turn; the transcript's copy has since been archived"));

        // read_1 (1 turn old) must be preserved in full when retention is 2:
        assert_eq!(expanded_turn2[3].content, long_code);

        // edit_1 (short ack) must remain untouched:
        assert_eq!(expanded_turn2[4].content, short_ack);

        // If aggressive retention is set (read_prune_turns = Some(1)), read_1 IS trimmed at turn 2:
        let expanded_turn2_aggressive = expand_messages_with_segments(input_turn2, Some(1));
        assert_eq!(
            expanded_turn2_aggressive[3].content,
            "receipt — project_read(\"src/main.rs\") returned 3 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // Case B: At Turn 3 (User 1 -> Assistant 1 -> User 2 -> Assistant 2 -> User 3)
        // turns_ago for Assistant 1 is 2:
        // - read_1 (turns_ago >= 2) -> PRUNED to stub with default/Some(2)!
        // turns_ago for Assistant 2 is 1:
        // - read_2 (turns_ago < 2) -> KEPT INTACT!
        let input_turn3 = vec![
            Message {
                role: Role::User,
                content: "Fetch web and read main".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(1),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Done fetching and reading.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(2),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: Some(segs_turn1),
            },
            Message {
                role: Role::User,
                content: "Now read lib".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(3),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Done reading lib.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(4),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: Some(segs_turn2),
            },
            Message {
                role: Role::User,
                content: "Now make the change".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(5),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
        ];

        let expanded_turn3 = expand_messages_with_segments(input_turn3.clone(), Some(2));
        // read_1 (now 2 turns old) must be stubbed:
        assert_eq!(
            expanded_turn3[3].content,
            "receipt — project_read(\"src/main.rs\") returned 3 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // read_2 (1 turn old) must still be full:
        // Indices in turn 3:
        // 0: User 1
        // 1: Assistant 1 calls
        // 2: Tool fetch_1 (stubbed)
        // 3: Tool read_1 (stubbed)
        // 4: Tool edit_1 (ack)
        // 5: Assistant 1 text
        // 6: User 2
        // 7: Assistant 2 calls
        // 8: Tool read_2 (full!)
        // 9: Assistant 2 text
        // 10: User 3
        assert_eq!(expanded_turn3.len(), 11);
        assert_eq!(expanded_turn3[8].content, long_code);

        // Case C: Unlimited / Keep full (read_prune_turns = Some(0))
        // Even at Turn 3 (turns_ago >= 2), read_1 and read_2 are BOTH preserved in full!
        let expanded_turn3_unlimited = expand_messages_with_segments(input_turn3, Some(0));
        assert_eq!(expanded_turn3_unlimited[3].content, long_code);
        assert_eq!(expanded_turn3_unlimited[8].content, long_code);
        // But web_fetch is STILL trimmed at turns_ago >= 1:
        assert!(expanded_turn3_unlimited[2].content.starts_with("receipt — web_fetch(\"https://example.com/docs\") returned"));
        assert!(expanded_turn3_unlimited[2].content.ends_with("in an earlier turn; the transcript's copy has since been archived"));
    }

    #[test]
    fn test_expand_messages_canonical_read_file_pruning() {
        let long_code = "fn important_function() {\n    println!(\"many lines of code here\");\n}\n".repeat(10);
        let segs = serde_json::json!([
            {
                "kind": "tool",
                "id": "call_rf",
                "name": "read_file",
                "args": "{\"path\":\"src/main.rs\"}",
                "output": long_code
            }
        ]);

        let input_turn2 = vec![
            Message {
                role: Role::User,
                content: "Read the file".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(1),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Read the file for you.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(2),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: Some(segs),
            },
            Message {
                role: Role::User,
                content: "Next instruction".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(3),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
        ];

        // 1. With default 2-turn window (read_prune_turns = Some(2)), 1 turn ago is preserved in full:
        let expanded_default = expand_messages_with_segments(input_turn2.clone(), Some(2));
        assert_eq!(expanded_default[2].content, long_code);

        // 2. With aggressive 1-turn window (read_prune_turns = Some(1)), 1 turn ago IS pruned to receipt:
        let expanded_aggressive = expand_messages_with_segments(input_turn2, Some(1));
        assert!(expanded_aggressive[2].content.starts_with("receipt — read_file(\"src/main.rs\") returned"));
        assert!(expanded_aggressive[2].content.contains("the transcript's copy has since been archived"));
    }

    #[test]
    fn test_expand_messages_sequential_trajectory_reconstruction() {
        let segs = serde_json::json!([
            { "kind": "reasoning", "text": "Thought 1: inspect codebase" },
            { "kind": "text", "text": "Let me read main.rs first." },
            {
                "kind": "tool",
                "id": "call_1",
                "name": "project_read",
                "args": "{\"path\":\"src/main.rs\"}",
                "output": "fn main() {}"
            },
            { "kind": "reasoning", "text": "Thought 2: now write helper" },
            { "kind": "text", "text": "Writing helper.rs now." },
            {
                "kind": "tool",
                "id": "call_2",
                "name": "project_write",
                "args": "{\"path\":\"src/helper.rs\"}",
                "output": "Wrote src/helper.rs"
            },
            { "kind": "text", "text": "All tasks completed successfully." }
        ]);

        let input = vec![
            Message {
                role: Role::User,
                content: "Refactor main and helper".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(10),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Let me read main.rs first.\n\nWriting helper.rs now.\n\nAll tasks completed successfully.".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(11),
                reasoning: Some("Thought 1: inspect codebase\nThought 2: now write helper".into()),
                reasoning_signature: None,
                sources: Vec::new(),
                segments: Some(segs),
            },
            Message {
                role: Role::User,
                content: "Next instruction".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(12),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
        ];

        // 1. Single turn (Assistant alone): reasoning stripped unconditionally
        let input_active = vec![input[0].clone(), input[1].clone()];
        let expanded_active = expand_messages_with_segments(input_active, Some(2));
        assert_eq!(expanded_active.len(), 6);
        assert!(expanded_active[1].reasoning.is_none());
        assert!(expanded_active[3].reasoning.is_none());

        // 2. Prior turn (turns_ago == 1 once User 2 arrives):
        // Reconstructed sequence should be:
        // 0: User ("Refactor main and helper")
        // 1: Assistant (step 1 speech: "Let me read main.rs first.", reasoning: None, tool_calls: [call_1])
        // 2: Tool (result for call_1)
        // 3: Assistant (step 2 speech: "Writing helper.rs now.", reasoning: None, tool_calls: [call_2])
        // 4: Tool (result for call_2)
        // 5: Assistant (final speech: "All tasks completed successfully.", tool_calls: [])
        // 6: User ("Next instruction")
        let expanded = expand_messages_with_segments(input.clone(), Some(2));
        assert_eq!(expanded.len(), 7);

        assert_eq!(expanded[0].role, Role::User);
        assert_eq!(expanded[0].content, "Refactor main and helper");

        // Step 1
        assert_eq!(expanded[1].role, Role::Assistant);
        assert_eq!(expanded[1].content, "Let me read main.rs first.");
        assert!(expanded[1].reasoning.is_none());
        assert_eq!(expanded[1].tool_calls.len(), 1);
        assert_eq!(expanded[1].tool_calls[0].id, "call_1");

        assert_eq!(expanded[2].role, Role::Tool);
        assert_eq!(expanded[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(expanded[2].content, "fn main() {}");

        // Step 2
        assert_eq!(expanded[3].role, Role::Assistant);
        assert_eq!(expanded[3].content, "Writing helper.rs now.");
        assert!(expanded[3].reasoning.is_none());
        assert_eq!(expanded[3].tool_calls.len(), 1);
        assert_eq!(expanded[3].tool_calls[0].id, "call_2");

        assert_eq!(expanded[4].role, Role::Tool);
        assert_eq!(expanded[4].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(expanded[4].content, "Wrote src/helper.rs");

        // Final response
        assert_eq!(expanded[5].role, Role::Assistant);
        assert_eq!(expanded[5].content, "All tasks completed successfully.");
        assert!(expanded[5].tool_calls.is_empty());

        assert_eq!(expanded[6].role, Role::User);
        assert_eq!(expanded[6].content, "Next instruction");
    }

    #[test]
    fn test_expand_messages_prior_turn_reasoning_stripped() {
        let segs = serde_json::json!([
            {
                "kind": "tool",
                "id": "call_1",
                "name": "project_read",
                "args": "{\"path\":\"src/main.rs\"}",
                "output": "fn main() {}"
            }
        ]);

        let input_turn1 = vec![
            Message {
                role: Role::User,
                content: "Turn 1 request".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(1),
                reasoning: None,
                reasoning_signature: None,
                sources: Vec::new(),
                segments: None,
            },
            Message {
                role: Role::Assistant,
                content: "Turn 1 answer".into(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                created_at: Some(2),
                reasoning: Some("Turn 1 thinking".into()),
                reasoning_signature: Some("sig1".into()),
                sources: Vec::new(),
                segments: Some(segs),
            },
        ];

        // 1. Single turn (turns_ago == 0): reasoning is stripped unconditionally
        let expanded_turn1 = expand_messages_with_segments(input_turn1.clone(), Some(2));
        assert!(expanded_turn1[1].reasoning.is_none());
        assert!(expanded_turn1[1].reasoning_signature.is_none());

        // 2. Once Turn 2 user message arrives (turns_ago == 1), reasoning is still stripped:
        let mut input_turn2 = input_turn1;
        input_turn2.push(Message {
            role: Role::User,
            content: "Turn 2 request".into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: Some(3),
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        });

        let expanded_turn2 = expand_messages_with_segments(input_turn2.clone(), Some(2));
        assert!(expanded_turn2[1].reasoning.is_none());
        assert!(expanded_turn2[1].reasoning_signature.is_none());

        // 3. At turn 3 (turns_ago == 2), reasoning continues to be stripped for all turns:
        let mut input_turn3 = input_turn2;
        input_turn3.push(Message {
            role: Role::Assistant,
            content: "Turn 2 answer".into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: Some(4),
            reasoning: Some("Turn 2 thinking".into()),
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        });
        input_turn3.push(Message {
            role: Role::User,
            content: "Turn 3 request".into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: Some(5),
            reasoning: None,
            reasoning_signature: None,
            sources: Vec::new(),
            segments: None,
        });

        let expanded_turn3 = expand_messages_with_segments(input_turn3, Some(2));
        assert!(expanded_turn3[1].reasoning.is_none());
        let asst2 = expanded_turn3.iter().find(|m| m.content == "Turn 2 answer").unwrap();
        assert!(asst2.reasoning.is_none());
    }

    #[test]
    fn test_format_pruned_stub_variants() {
        // 1. project_read
        let stub_read = format_pruned_stub(
            "project_read",
            r#"{"path":"src/app.rs"}"#,
            "line 1\nline 2\nline 3\nline 4\nline 5",
        );
        assert_eq!(
            stub_read,
            "receipt — project_read(\"src/app.rs\") returned 5 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 2. project_read_lines with numbered output
        let numbered_output = "10\tfn start() {\n11\t    init();\n12\t}\n";
        let stub_lines = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src/app.rs","start":10,"end":12}"#,
            numbered_output,
        );
        assert_eq!(
            stub_lines,
            "receipt — project_read_lines(\"src/app.rs\", lines 10–12) returned 3 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 3. project_read_lines single line
        let single_line = "42\tconst ANSWER: u32 = 42;\n";
        let stub_single = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src/consts.rs","start":42,"end":42}"#,
            single_line,
        );
        assert_eq!(
            stub_single,
            "receipt — project_read_lines(\"src/consts.rs\", line 42) returned 1 line in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 4. read_note
        let stub_note = format_pruned_stub(
            "read_note",
            r#"{"path":"notes/ideas.md"}"#,
            "# Ideas\n- Idea 1\n- Idea 2",
        );
        assert_eq!(
            stub_note,
            "receipt — read_note(\"notes/ideas.md\") returned 3 lines in an earlier turn; the transcript's copy has since been archived"
        );

        // 5. web_search
        let stub_search = format_pruned_stub(
            "web_search",
            r#"{"query":"rust axum streaming"}"#,
            "SearchResult 1: ...\nSearchResult 2: ...",
        );
        assert_eq!(
            stub_search,
            "receipt — web_search(\"rust axum streaming\") returned output in an earlier turn; the transcript's copy has since been archived"
        );

        // 6. project_read capped (legacy format fallback)
        let stub_capped = format_pruned_stub(
            "project_read",
            r#"{"path":"src/big.rs"}"#,
            "1\tfn big() {}\n2\tfn more() {}\n\n[content truncated at 32000 chars — pass max_chars to raise the cap (max 100000). Hint: Use project_read_lines to target and read specific sections of a large file.]",
        );
        assert_eq!(
            stub_capped,
            "receipt — project_read(\"src/big.rs\") returned first 2 lines [capped] in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 6b. project_read capped with total lines telemetry
        let stub_capped_telemetry = format_pruned_stub(
            "project_read",
            r#"{"path":"src/big.rs"}"#,
            "1\tfn big() {}\n2\tfn more() {}\n\n[content truncated at 32000 chars (showing lines 1–2 of 50; 1200 total chars) — pass max_chars: 2000 to read the full file, or use project_read_lines for specific ranges]",
        );
        assert_eq!(
            stub_capped_telemetry,
            "receipt — project_read(\"src/big.rs\") returned lines 1–2 (of 50) [capped] in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 7. project_read_lines capped
        let stub_lines_capped = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src/big.rs","start":1,"end":100}"#,
            "1\tline 1\n2\tline 2\n\n[truncated at 32000 characters — pass max_chars to raise the cap (max 50000), or ask for a narrower line range]",
        );
        assert_eq!(
            stub_lines_capped,
            "receipt — project_read_lines(\"src/big.rs\", lines 1–2) returned 2 lines [capped] in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 8. project_read_lines with standard output header (no +1 off-by-one)
        let header_output = "'src-tauri/src/commands.rs' lines 100–101 (of 2298):\n100\tfn first() {}\n101\tfn second() {}\n";
        let stub_header = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src-tauri/src/commands.rs","start":100,"end":101}"#,
            header_output,
        );
        assert_eq!(
            stub_header,
            "receipt — project_read_lines(\"src-tauri/src/commands.rs\", lines 100–101) returned 2 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 9. project_read_lines single line with standard header (verifying singular "1 line", not "2 lines")
        let single_header = "'src/consts.rs' lines 42–42 (of 100):\n42\tconst ANSWER: u32 = 42;\n";
        let stub_single_hdr = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src/consts.rs","start":42,"end":42}"#,
            single_header,
        );
        assert_eq!(
            stub_single_hdr,
            "receipt — project_read_lines(\"src/consts.rs\", line 42) returned 1 line in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 10. Reading code that quotes truncation literals must NOT trigger false [capped] (Defect 1)
        let code_quoting_literals = "'src-tauri/src/commands.rs' lines 140–142 (of 2298):\n140\tlet raw = text;\n141\tlet is_truncated = raw_text.contains(\"[content truncated at \");\n142\tlet is_other = raw_text.contains(\"[truncated at \");\n";
        let stub_literals = format_pruned_stub(
            "project_read_lines",
            r#"{"path":"src-tauri/src/commands.rs","start":140,"end":142}"#,
            code_quoting_literals,
        );
        assert_eq!(
            stub_literals,
            "receipt — project_read_lines(\"src-tauri/src/commands.rs\", lines 140–142) returned 3 lines in an earlier turn; the transcript's copy has since been archived (re-read with project_read_lines before editing)"
        );

        // 11. project_read_image
        let stub_img = format_pruned_stub(
            "project_read_image",
            r#"{"path":"03_corner.png"}"#,
            "Loaded '03_corner.png' (image/png, 45 KB) — image attached for inspection this turn.",
        );
        assert_eq!(
            stub_img,
            "receipt — project_read_image(\"03_corner.png\") was inspected in an earlier turn; the image payload has since been archived"
        );

        // 12. browser_screenshot
        let stub_shot = format_pruned_stub(
            "browser_screenshot",
            "{}",
            "Captured the browser view (120 KB) — screenshot attached for inspection this turn.",
        );
        assert_eq!(
            stub_shot,
            "receipt — browser_screenshot was inspected in an earlier turn; the image payload has since been archived"
        );

        // 13. godot_scene_tree uncapped
        let stub_godot = format_pruned_stub(
            "godot_scene_tree",
            r#"{"scene":"scenes/inn.tscn"}"#,
            "scenes/inn.tscn — Godot 4 scene (format 3, 14 nodes, 3 ext_resource, 1 sub_resource)\n├─ Root (Node2D)\n",
        );
        assert_eq!(
            stub_godot,
            "receipt — godot_scene_tree(\"scenes/inn.tscn\") returned 14 nodes in an earlier turn; the transcript's copy has since been archived"
        );

        // 14. godot_scene_tree capped
        let stub_godot_capped = format_pruned_stub(
            "godot_scene_tree",
            r#"{"scene":"scenes/world.tscn"}"#,
            "scenes/world.tscn — Godot 4 scene (format 3, 250 nodes, 10 ext_resource)\n├─ World (Node3D)\n\n[output truncated at 12000 chars (35000 total chars) — pass max_chars: 36000 to see the full scene tree, or use max_depth to inspect specific subtree depths]",
        );
        assert_eq!(
            stub_godot_capped,
            "receipt — godot_scene_tree(\"scenes/world.tscn\") returned 250 nodes [capped] in an earlier turn; the transcript's copy has since been archived"
        );

        // 15. Canonical tool names: read_file, read_file_lines, read_image
        let stub_canonical_read = format_pruned_stub(
            "read_file",
            r#"{"path":"src/app.rs"}"#,
            "line 1\nline 2\nline 3\nline 4\nline 5",
        );
        assert_eq!(
            stub_canonical_read,
            "receipt — read_file(\"src/app.rs\") returned 5 lines in an earlier turn; the transcript's copy has since been archived (re-read with read_file_lines before editing)"
        );

        let stub_canonical_lines = format_pruned_stub(
            "read_file_lines",
            r#"{"path":"src/app.rs","start":10,"end":12}"#,
            numbered_output,
        );
        assert_eq!(
            stub_canonical_lines,
            "receipt — read_file_lines(\"src/app.rs\", lines 10–12) returned 3 lines in an earlier turn; the transcript's copy has since been archived (re-read with read_file_lines before editing)"
        );

        let stub_canonical_img = format_pruned_stub(
            "read_image",
            r#"{"path":"03_corner.png"}"#,
            "Loaded '03_corner.png' (image/png, 45 KB) — image attached for inspection this turn.",
        );
        assert_eq!(
            stub_canonical_img,
            "receipt — read_image(\"03_corner.png\") was inspected in an earlier turn; the image payload has since been archived"
        );
    }

    #[test]
    fn test_web_search_pruned_index_variants() {
        // 1. Well-formed block with 2 search results
        let structured_web = "1. Axum SSE Guide\n   https://blog.devgenius.io/sse\n   Learn how to use SSE with Axum...\n\n2. Docs: axum::response::sse\n   https://docs.rs/axum/latest/axum/response/sse/index.html\n   Documentation for SSE in Axum.";
        let stub_search_structured = format_pruned_stub(
            "web_search",
            r#"{"query":"rust axum streaming"}"#,
            structured_web,
        );
        let expected = "receipt — web_search(\"rust axum streaming\") returned 2 results in an earlier turn; the snippets were archived, but the result index is kept:\n  1. Axum SSE Guide — blog.devgenius.io\n  2. Docs: axum::response::sse — docs.rs\n(run web_search again with \"rust axum streaming\" to restore the snippets)";
        assert_eq!(stub_search_structured, expected);

        // 2. Single result ("1 result")
        let single_web = "1. Tokio Tutorial\n   https://tokio.rs/tokio/tutorial\n   Async basics.";
        let stub_single = format_pruned_stub(
            "web_search",
            r#"{"query":"tokio tutorial"}"#,
            single_web,
        );
        let expected_single = "receipt — web_search(\"tokio tutorial\") returned 1 result in an earlier turn; the snippets were archived, but the result index is kept:\n  1. Tokio Tutorial — tokio.rs\n(run web_search again with \"tokio tutorial\" to restore the snippets)";
        assert_eq!(stub_single, expected_single);

        // 3. Title not followed by a URL (falls back to bare receipt)
        let malformed_web = "1. Invalid Result\n   Not a url at all\n   Snippet here.";
        let stub_malformed = format_pruned_stub(
            "web_search",
            r#"{"query":"broken query"}"#,
            malformed_web,
        );
        assert_eq!(
            stub_malformed,
            "receipt — web_search(\"broken query\") returned output in an earlier turn; the transcript's copy has since been archived"
        );

        // 4. Non-numeric ". " inside snippet should not trick parser
        let snippet_with_dot = "1. First Real Result\n   https://example.com/one\n   Step 1. Do something.\n   Step 2. Do more.\n\n2. Second Result\n   https://www.test.org/two\n   Another snippet.";
        let stub_snippet_dot = format_pruned_stub(
            "web_search",
            r#"{"query":"steps query"}"#,
            snippet_with_dot,
        );
        let expected_steps = "receipt — web_search(\"steps query\") returned 2 results in an earlier turn; the snippets were archived, but the result index is kept:\n  1. First Real Result — example.com\n  2. Second Result — test.org\n(run web_search again with \"steps query\" to restore the snippets)";
        assert_eq!(stub_snippet_dot, expected_steps);

        // 5. Empty / "No results found." falls back to bare receipt
        let stub_no_results = format_pruned_stub(
            "web_search",
            r#"{"query":"unfindable"}"#,
            "No results found.",
        );
        assert_eq!(
            stub_no_results,
            "receipt — web_search(\"unfindable\") returned output in an earlier turn; the transcript's copy has since been archived"
        );

        // 6. Missing query in arguments
        let stub_no_query = format_pruned_stub(
            "web_search",
            r#"{}"#,
            structured_web,
        );
        let expected_no_query = "receipt — web_search returned 2 results in an earlier turn; the snippets were archived, but the result index is kept:\n  1. Axum SSE Guide — blog.devgenius.io\n  2. Docs: axum::response::sse — docs.rs\n(run web_search again to restore the snippets)";
        assert_eq!(stub_no_query, expected_no_query);
    }

    #[test]
    fn test_web_fetch_pruned_outline_variants() {
        // 1. Standard markdown with H1 and H2
        let doc = "# Main Article Title\n\nIntro text...\n\n## Section One\n\nBody...\n\n## Section Two\n\nMore body.";
        let stub = format_pruned_stub(
            "web_fetch",
            r#"{"url":"https://example.com/article"}"#,
            doc,
        );
        let expected = "receipt — web_fetch(\"https://example.com/article\") returned 88 B in an earlier turn; the content was archived, but the document outline is kept:\n  • [H1] Main Article Title\n  • [H2] Section One\n  • [H2] Section Two\n(run web_fetch again to restore full content)";
        assert_eq!(stub, expected);

        // 2. Document with overflow (>8 headings)
        let mut many_headings = String::new();
        for i in 1..=10 {
            many_headings.push_str(&format!("## Heading {}\nContent\n\n", i));
        }
        let stub_overflow = format_pruned_stub(
            "web_fetch",
            r#"{"url":"https://example.com/big"}"#,
            &many_headings,
        );
        assert!(stub_overflow.contains("  • [H2] Heading 1\n"));
        assert!(stub_overflow.contains("  • [H2] Heading 8\n"));
        assert!(!stub_overflow.contains("  • [H2] Heading 9"));
        assert!(stub_overflow.contains("  … (and 2 more sections)"));
        assert!(stub_overflow.ends_with("(run web_fetch again to restore full content)"));

        // 3. Truncated document (hit character limit)
        let doc_capped = format!("{}\n\n[content truncated at 50000 chars — pass max_chars to raise the cap (max 100000)]", doc);
        let stub_capped = format_pruned_stub(
            "web_fetch",
            r#"{"url":"https://example.com/article"}"#,
            &doc_capped,
        );
        assert!(stub_capped.contains("[capped]"));
        assert!(stub_capped.ends_with("(run web_fetch again to restore full content; pass max_chars to raise the limit)"));

        // 4. Code block filtering (comments inside code fences are not headings)
        let doc_with_code = "# Real Header\n\n```python\n# This is a comment, not a header\ndef foo(): pass\n```\n\n## Second Real Header\n";
        let stub_code = format_pruned_stub(
            "web_fetch",
            r#"{"url":"https://example.com/code"}"#,
            doc_with_code,
        );
        assert!(stub_code.contains("  • [H1] Real Header\n"));
        assert!(stub_code.contains("  • [H2] Second Real Header\n"));
        assert!(!stub_code.contains("This is a comment"));

        // 5. Zero headings (fallback to byte receipt)
        let stub_plain = format_pruned_stub(
            "web_fetch",
            r#"{"url":"https://example.com/plain"}"#,
            "Just some plain text without any markdown hashes.\nAnother line here.",
        );
        assert_eq!(
            stub_plain,
            "receipt — web_fetch(\"https://example.com/plain\") returned 68 B in an earlier turn; the transcript's copy has since been archived"
        );
    }

    #[test]
    fn test_format_pruned_stub_error_results() {
        let expected = |name: &str, subject: &str| {
            format!("receipt — {name}{subject} returned an error in an earlier turn; the transcript's copy has since been archived")
        };
        let expected_range = |name: &str, subject: &str| {
            format!("receipt — {name}{subject} returned an error in an earlier turn (line past end); the transcript's copy has since been archived")
        };

        // A failed image read must NOT be summarized as "was inspected".
        assert_eq!(
            format_pruned_stub(
                "read_image",
                r#"{"path":"nope.png"}"#,
                "error: 'nope.png' doesn't exist or can't be read."
            ),
            expected("read_image", "(\"nope.png\")")
        );

        // A failed web_fetch reports failure, not a byte count of the error string.
        assert_eq!(
            format_pruned_stub(
                "web_fetch",
                r#"{"url":"https://example.com"}"#,
                "fetch error: error sending request for url (https://example.com): dns error"
            ),
            expected("web_fetch", "(\"https://example.com\")")
        );

        // A failed read (any length) reports failure, not "returned 1 line".
        assert_eq!(
            format_pruned_stub(
                "read_file",
                r#"{"path":"src/missing_module.rs"}"#,
                "error: reading 'src/missing_module.rs': The system cannot find the file specified. (os error 2)"
            ),
            expected("read_file", "(\"src/missing_module.rs\")")
        );

        // Legacy alias names work too.
        assert_eq!(
            format_pruned_stub(
                "project_read",
                r#"{"path":"src/missing.rs"}"#,
                "error: reading 'src/missing.rs': not found"
            ),
            expected("project_read", "(\"src/missing.rs\")")
        );

        // No subject in the args → still a truthful failure receipt.
        assert_eq!(
            format_pruned_stub("web_search", "", "search error: connection refused"),
            expected("web_search", "")
        );

        // A RANGE failure (file exists, range is past the end) is marked as such — but the
        // requested number is NOT echoed, so nothing reads as though that line exists.
        assert_eq!(
            format_pruned_stub(
                "read_file_lines",
                r#"{"path":"src-tauri/fixtures/engine_errors/unknown_predict_stream_wrapper.txt","start":999999999}"#,
                "'src-tauri/fixtures/engine_errors/unknown_predict_stream_wrapper.txt' has only 7 lines; start line 999999999 is past the end."
            ),
            expected_range(
                "read_file_lines",
                "(\"src-tauri/fixtures/engine_errors/unknown_predict_stream_wrapper.txt\")"
            )
        );

        // Same class, start–end range — the numbers still do not appear.
        assert_eq!(
            format_pruned_stub(
                "read_file_lines",
                r#"{"path":"notes/ideas.md","start":400,"end":450}"#,
                "'notes/ideas.md' has only 7 lines; start line 400 is past the end."
            ),
            expected_range("read_file_lines", "(\"notes/ideas.md\")")
        );

        // A PATH failure stays path-only even when start/end are present — the range is noise.
        assert_eq!(
            format_pruned_stub(
                "read_file_lines",
                r#"{"path":"src/gone.rs","start":10,"end":20}"#,
                "error reading 'src/gone.rs': The system cannot find the file specified. (os error 2)"
            ),
            expected("read_file_lines", "(\"src/gone.rs\")")
        );

        // A file whose CONTENT merely mentions an error is still a successful read.
        let stub_prose = format_pruned_stub(
            "read_file",
            r#"{"path":"docs/errors.md"}"#,
            "# Error handling\n\nThis document explains error: recovery.\n",
        );
        assert!(
            stub_prose.starts_with("receipt — read_file(\"docs/errors.md\") returned"),
            "{stub_prose}"
        );
    }

    #[test]
    fn test_coding_orientation_tool_directive() {
        let grounding = crate::grounding::grounding_message("gemma-27b", "local", "openai", chrono_tz::UTC);
        assert!(!grounding.contains("Tool execution protocol:"));

        let orientation_gated = crate::grounding::coding_orientation("/test/dir", false);
        assert!(!orientation_gated.contains("Tool execution protocol:"));
        assert!(!orientation_gated.contains("Permissions: Auto-approval is active"));
        assert!(orientation_gated.contains("Working memory: tool results from earlier turns are archived into compact receipts (`receipt — read_file(...) returned...`) to conserve context."));
        assert!(orientation_gated.contains("File safety: Outpost automatically snapshots files before editing or deleting."));

        let orientation_auto = crate::grounding::coding_orientation("/test/dir", true);
        assert!(orientation_auto.contains("Permissions: Auto-approval is active for `write_file` and `edit_file` — you can create, modify, and scaffold files directly without pausing for confirmation."));
    }

    #[test]
    fn test_preview_context_tool_formatting_and_summaries() {
        assert_eq!(
            summarize_tool_call("read_file", r#"{"path":"tetris/board.py"}"#),
            "read_file: tetris/board.py"
        );
        assert_eq!(
            summarize_tool_call("project_read", r#"{"path":"tetris/board.py"}"#),
            "project_read: tetris/board.py"
        );
        assert_eq!(
            summarize_tool_call("run_command", r#"{"command":"pytest"}"#),
            "run_command: pytest"
        );
        assert_eq!(
            summarize_tool_call("grep_search", r#"{"query":"rotate"}"#),
            "grep_search: rotate"
        );
        assert_eq!(
            summarize_tool_call("code_search", r#"{"pattern":"rotate"}"#),
            "code_search: rotate"
        );
        assert_eq!(
            summarize_tool_call("project_overview", r#"{}"#),
            "project_overview"
        );
        assert_eq!(
            summarize_tool_call("project_info", r#"{}"#),
            "project_info"
        );
        assert_eq!(
            summarize_tool_call("godot_scene_tree", r#"{"scene":"scenes/inn.tscn"}"#),
            "godot_scene_tree: scenes/inn.tscn"
        );

        let calls = vec![
            "project_read: tetris/board.py".to_string(),
            "project_read: tetris/pieces.py".to_string(),
        ];
        let content = format_tool_section_content(&calls);
        assert_eq!(
            content,
            "[2 tool calls]\n• project_read: tetris/board.py\n• project_read: tetris/pieces.py"
        );
    }
}

